use super::*;
use crate::apps::telegram::{downloads, operations::Status, requests};
use crate::platform::disk::RealDisk;
use kernel::caps::{self, BlobCache, Disk, DiskFactory, Entry, FileId};
use std::path::{Path, PathBuf};
use std::sync::{
    atomic::{AtomicBool, AtomicU64, Ordering},
    Arc,
};

/// Exercise the platform's exclusive copy in a temp directory. A test can
/// never write to the account holder's Downloads or open a downloaded file.
struct DownloadDisk {
    dir: PathBuf,
    denied: Arc<AtomicBool>,
}

impl DownloadDisk {
    fn destination(&self, path: &Path) -> PathBuf {
        self.dir.join(
            path.strip_prefix(caps::real_path("~/Downloads"))
                .expect("only Downloads is written"),
        )
    }
}

impl Disk for DownloadDisk {
    fn stat(&mut self, path: &Path) -> Result<Option<Entry>, String> {
        RealDisk::new().stat(&self.destination(path))
    }
    fn make_dir(&mut self, path: &Path) -> Result<(), String> {
        RealDisk::new().make_dir(&self.destination(path))
    }
    fn copy_path(&mut self, from: &Path, to: &Path) -> Result<(), String> {
        if self.denied.load(Ordering::Relaxed) {
            return Err("disk full".into());
        }
        RealDisk::new().copy_path(from, &self.destination(to))
    }
    fn list_dir(&mut self, _: &Path) -> Result<Vec<Entry>, String> {
        unreachable!()
    }
    fn read_file(&mut self, _: &Path, _: usize) -> Result<Vec<u8>, String> {
        unreachable!()
    }
    fn write_file(&mut self, _: &Path, _: &[u8]) -> Result<(), String> {
        unreachable!()
    }
    fn open_path(&mut self, _: &Path) -> Result<(), String> {
        panic!("downloads never execute files")
    }
    fn move_path(&mut self, _: &Path, _: &Path) -> Result<(), String> {
        unreachable!()
    }
    fn trash(&mut self, _: &Path) -> Result<PathBuf, String> {
        unreachable!()
    }
    fn file_id(&mut self, _: &Path) -> Result<Option<FileId>, String> {
        unreachable!()
    }
}

struct DownloadTest {
    w: World,
    td: FakeTd,
    acc: Account<FakeTd>,
    dir: PathBuf,
    denied: Arc<AtomicBool>,
}

impl DownloadTest {
    fn new() -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let dir = std::env::temp_dir().join(format!(
            "superapp-download-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir_all(dir.join("tdlib")).unwrap();
        let denied = Arc::new(AtomicBool::new(false));
        let disk_dir = dir.join("Downloads");
        let fail = denied.clone();
        let env = Env {
            blobs: BlobCache::at(dir.join("blobs"), caps::BLOB_BUDGET_DEFAULT),
            disk: Some(DiskFactory::new(move || {
                Box::new(DownloadDisk {
                    dir: disk_dir.clone(),
                    denied: fail.clone(),
                })
            })),
            ..Env::default()
        };
        let mut caps = Capabilities::default();
        kernel::caps::install(Mode::Fake, &env, &mut caps);
        let store = Store::open(None, &[&SCHEMA], kernel::sync::Device::fake()).unwrap();
        let w = World::new(Rc::new(store), caps, Registry::new());
        let td = FakeTd::new();
        let acc = Account::new(td.clone(), 17844, dir.join("tdlib"), None);
        acc.auth_ready.set(true);
        acc.drain(&w);
        Self {
            w,
            td,
            acc,
            dir,
            denied,
        }
    }

    fn start(&self) -> serde_json::Value {
        self.acc.send(&self.w, &requests::save_file(7, 42));
        last_request(&self.td, "getMessage")
    }

    fn source(&self, request: &serde_json::Value, name: &str) {
        self.acc.on_update(
            &self.w,
            &json!({"@type": "message", "chat_id": 7, "id": 42,
            "@extra": request["@extra"], "content": {"@type": "messageDocument",
                "document": {"file_name": name, "document": file()}}})
            .to_string(),
        );
    }

    fn complete(&self, request: &serde_json::Value) -> serde_json::Value {
        let path = self.dir.join("tdlib/source");
        std::fs::write(&path, b"original document").unwrap();
        let mut file = file();
        file["local"] =
            json!({"path": path, "is_downloading_completed": true, "downloaded_size": 17});
        file["@extra"] = request["@extra"].clone();
        file
    }

    fn op(&self, request: &serde_json::Value) -> crate::apps::telegram::operations::Operation {
        runtime::of(self.w.store())
            .operations
            .list()
            .into_iter()
            .find(|op| op.id == request["@extra"]["operation"].as_u64().unwrap())
            .unwrap()
    }
}

impl Drop for DownloadTest {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

fn file() -> serde_json::Value {
    json!({"@type": "file", "id": 77, "size": 17,
        "remote": {"id": "fresh-reference", "unique_id": "document"},
        "local": {"is_downloading_completed": false, "downloaded_size": 0}})
}

#[test]
fn an_agent_reads_a_remote_pdf_through_the_cache_without_exporting_or_marking_read() {
    use futures_util::FutureExt;
    let timer = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
    let _entered = timer.enter();
    let t = DownloadTest::new();
    // The message metadata exists on this device, but the file has never
    // been downloaded and no Telegram panel is open.
    t.acc.on_new_message(&t.w, &json!({"@type": "message", "chat_id": 7, "id": 42,
        "content": {"@type": "messageDocument", "document": {"file_name": "letter.pdf", "document": file()}}}));
    let tool = crate::apps::telegram::tools::all().into_iter().find(|t| t.name == "telegram.file").unwrap();
    assert!(!tool.writes && !tool.asks);
    let input = json!({"chat": 7, "message": 42});
    tool.check(&input).unwrap();
    let mut read = (tool.reader.unwrap())(&input)(&t.w);
    assert!(read.as_mut().now_or_never().is_none());
    assert!(read.as_mut().now_or_never().is_none(), "polling does not enqueue another request");
    t.acc.drain(&t.w);
    let source = last_request(&t.td, "getMessage");
    assert_eq!(source["@extra"]["context"], "cache:7:42");
    t.source(&source, "letter.pdf");
    let transfer = last_request(&t.td, "downloadFile");
    assert_eq!(transfer["@extra"], source["@extra"]);
    assert_eq!(transfer["file_id"], 77);
    assert!(read.as_mut().now_or_never().is_none());
    let completed = t.complete(&transfer);
    let bytes = crate::reader::document::test_pdf("A Telegram PDF for the agent.");
    std::fs::write(completed["local"]["path"].as_str().unwrap(), &bytes).unwrap();
    t.acc.on_update(&t.w, &completed.to_string());
    t.w.store().poll_external();
    assert_eq!(t.op(&source).status, Status::Done);
    let result = timer.block_on(read).expect("downloaded PDF must be readable");
    assert_eq!(result["name"], "letter.pdf");
    assert!(result["text"].as_str().unwrap().contains("A Telegram PDF for the agent."));
    assert!(!t.dir.join("Downloads").exists(), "agent reads leave Downloads alone");
    assert!(t.td.sent().iter().all(|r| !r.contains("viewMessages")));
    runtime::of(t.w.store()).disconnect();
    let cached = (tool.reader.unwrap())(&input)(&t.w);
    assert_eq!(timer.block_on(cached), Ok(result), "cached files work offline");
}

#[test]
fn agent_file_reads_report_download_errors_and_refuse_missing_or_oversized_sources() {
    use futures_util::FutureExt;
    let timer = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
    let _entered = timer.enter();
    let t = DownloadTest::new();
    t.acc.on_new_message(&t.w, &json!({"@type": "message", "chat_id": 7, "id": 42,
        "content": {"@type": "messageDocument", "document": {"file_name": "letter.pdf", "document": file()}}}));
    let tool = crate::apps::telegram::tools::all().into_iter().find(|t| t.name == "telegram.file").unwrap();
    let missing = (tool.reader.unwrap())(&json!({"chat": 8, "message": 42}))(&t.w);
    assert!(timer.block_on(missing).is_err(), "ids are scoped to their chat");
    let mut read = (tool.reader.unwrap())(&json!({"chat": 7, "message": 42}))(&t.w);
    assert!(read.as_mut().now_or_never().is_none());
    t.acc.drain(&t.w);
    let source = last_request(&t.td, "getMessage");
    t.acc.on_update(&t.w, &json!({"@type": "error", "code": 404,
        "message": "message was deleted", "@extra": source["@extra"]}).to_string());
    let error = timer.block_on(read).expect_err("download error must reach the agent");
    assert!(error.contains("deleted"));

    let mut retry = (tool.reader.unwrap())(&json!({"chat": 7, "message": 42}))(&t.w);
    assert!(retry.as_mut().now_or_never().is_none());
    t.acc.drain(&t.w);
    let source = last_request(&t.td, "getMessage");
    let mut large = file();
    large["size"] = json!(crate::reader::document::MAX_FILE + 1);
    t.acc.on_update(&t.w, &json!({"@type": "message", "chat_id": 7, "id": 42,
        "@extra": source["@extra"], "content": {"@type": "messageDocument",
            "document": {"file_name": "large.pdf", "document": large}}}).to_string());
    let error = timer.block_on(retry).expect_err("oversized source must fail");
    assert!(error.contains("32 MiB"));
    assert!(t.td.sent().iter().all(|r| !r.contains("downloadFile")));
}

#[test]
fn download_saves_the_original_bytes_and_name_only_after_completion() {
    let t = DownloadTest::new();
    let source = t.start();
    t.source(&source, "résumé.pdf");
    let transfer = last_request(&t.td, "downloadFile");
    assert_eq!(
        transfer["@extra"], source["@extra"],
        "one operation from source to saved copy"
    );
    assert_eq!(transfer["priority"], 32);
    assert_eq!(transfer["synchronous"], true);
    assert_eq!(transfer["file_id"], 77);
    let mut progress = file();
    progress["local"] = json!({"is_downloading_active": true, "downloaded_size": 8});
    t.acc.on_update(
        &t.w,
        &json!({"@type": "updateFile", "file": progress}).to_string(),
    );
    assert!(t.op(&source).line().contains("47%"));
    assert!(t.op(&source).foreground());
    assert!(!t.dir.join("Downloads/résumé.pdf").exists());

    let completed = t.complete(&transfer);
    // TDLib can announce cache arrival before answering downloadFile.
    t.acc.on_update(
        &t.w,
        &json!({"@type": "updateFile", "file": completed}).to_string(),
    );
    assert_eq!(t.op(&source).status, Status::Pending);
    t.acc.on_update(&t.w, &completed.to_string());
    assert_eq!(t.op(&source).status, Status::Done);
    assert!(t.op(&source).line().contains("~/Downloads/résumé.pdf"));
    assert_eq!(
        std::fs::read(t.dir.join("Downloads/résumé.pdf")).unwrap(),
        b"original document"
    );
    assert!(t
        .w
        .with_cap::<dyn Blobs, _>(|b| b.contains("tg:document"))
        .unwrap());
    t.acc.on_update(&t.w, &completed.to_string());
    assert_eq!(
        std::fs::read_dir(t.dir.join("Downloads")).unwrap().count(),
        1,
        "duplicate replies do not save twice"
    );
    t.w.with_cap::<dyn Blobs, _>(|b| b.remove("tg:document"))
        .unwrap();
    assert_eq!(
        std::fs::read(t.dir.join("Downloads/résumé.pdf")).unwrap(),
        b"original document",
        "the saved copy survives cache eviction"
    );
}

#[test]
fn cached_documents_save_offline_and_never_overwrite_an_existing_name() {
    let t = DownloadTest::new();
    let source = t.start();
    t.source(&source, "../../report.pdf");
    let transfer = last_request(&t.td, "downloadFile");
    t.acc.on_update(&t.w, &t.complete(&transfer).to_string());
    let saved = t.dir.join("Downloads/report.pdf");
    std::fs::write(&saved, b"my edited copy").unwrap();
    t.acc.auth_ready.set(false);
    let sent = t.td.sent().len();
    t.acc.send(&t.w, &requests::save_file(7, 42));
    assert_eq!(
        t.td.sent().len(),
        sent,
        "cached documents need no network or authorization"
    );
    assert_eq!(std::fs::read(&saved).unwrap(), b"my edited copy");
    assert_eq!(
        std::fs::read(t.dir.join("Downloads/report (1).pdf")).unwrap(),
        b"original document"
    );
    assert!(
        !t.dir.join("report.pdf").exists(),
        "sender paths cannot escape Downloads"
    );
}

#[test]
fn download_and_disk_failures_retry_the_source_and_keep_the_save_pending() {
    let t = DownloadTest::new();
    let source = t.start();
    t.source(&source, "report.pdf");
    let transfer = last_request(&t.td, "downloadFile");
    t.acc.on_update(
        &t.w,
        &json!({"@type": "error", "code": 400,
        "message": "download canceled", "@extra": transfer["@extra"]})
        .to_string(),
    );
    assert!(t.op(&source).retryable());
    let rt = runtime::of(t.w.store());
    let retry = rt.operations.retry(t.op(&source).id).unwrap();
    assert_eq!(
        serde_json::from_str::<serde_json::Value>(&retry).unwrap()["@type"],
        "getMessage"
    );
    t.acc.send(&t.w, &retry);
    let retried = last_request(&t.td, "getMessage");
    t.source(&retried, "report.pdf");
    assert!(t.op(&source).foreground(), "retry keeps progress visible");
    t.acc.on_update(
        &t.w,
        &json!({"@type": "error", "code": 400,
        "message": "late failure from the old transfer", "@extra": transfer["@extra"]})
        .to_string(),
    );
    assert_eq!(t.op(&source).status, Status::Pending);
    t.denied.store(true, Ordering::Relaxed);
    t.acc.on_update(&t.w, &t.complete(&retried).to_string());
    assert!(t.op(&source).line().contains("disk full"));
    assert!(t.op(&source).retryable());
    assert!(!t.dir.join("Downloads/report.pdf").exists());
    t.denied.store(false, Ordering::Relaxed);
    let sent = t.td.sent().len();
    t.acc
        .send(&t.w, &rt.operations.retry(t.op(&source).id).unwrap());
    assert_eq!(
        t.td.sent().len(),
        sent,
        "a disk retry uses the cached bytes"
    );
    assert_eq!(t.op(&source).status, Status::Done);
}

#[test]
fn missing_source_is_an_actionable_failure_and_never_creates_a_file() {
    let t = DownloadTest::new();
    let source = t.start();
    t.acc.on_update(&t.w, &json!({"@type": "message", "chat_id": 7, "id": 42,
        "@extra": source["@extra"], "content": {"@type": "messageText", "text": {"text": "changed"}}}).to_string());
    assert!(t.op(&source).retryable());
    assert!(!t.td.sent_types().contains(&"downloadFile".into()));
    assert!(!t.dir.join("Downloads").exists());
}

#[test]
fn download_button_retries_failed_save_without_leaving_stale_failures() {
    use crate::apps::telegram::{operations, panels};

    for disk_failure in [false, true] {
        let t = DownloadTest::new();
        let source = t.start();
        t.source(&source, "report.pdf");
        let transfer = last_request(&t.td, "downloadFile");
        if disk_failure {
            t.denied.store(true, Ordering::Relaxed);
            t.acc.on_update(&t.w, &t.complete(&transfer).to_string());
            t.denied.store(false, Ordering::Relaxed);
        } else {
            t.acc.on_update(
                &t.w,
                &json!({"@type": "error", "code": 400,
                "message": "download canceled", "@extra": transfer["@extra"]})
                .to_string(),
            );
        }
        let failed_id = t.op(&source).id;
        assert!(t.op(&source).retryable());
        let sent = t.td.sent().len();
        // This is the same queue boundary used by another press of download.
        let id = panels::queue(t.w.store(), &requests::save_file(7, 42)).unwrap();
        assert_eq!(
            id, failed_id,
            "download must retry the existing failed save"
        );
        assert_eq!(t.op(&source).status, Status::Pending);
        let rt = runtime::of(t.w.store());
        assert!(!rt
            .operations
            .list()
            .iter()
            .any(|op| matches!(op.status, Status::Failed { .. })));
        t.acc.drain(&t.w);
        if disk_failure {
            assert_eq!(t.td.sent().len(), sent, "retry uses the cached document");
        } else {
            let retry = last_request(&t.td, "getMessage");
            assert_eq!(retry["@extra"]["operation"], failed_id);
            assert_eq!(retry["@extra"]["attempt"], 1);
            t.source(&retry, "report.pdf");
            t.acc.on_update(&t.w, &t.complete(&retry).to_string());
        }
        assert_eq!(t.op(&source).status, Status::Done);
        assert!(!t.op(&source).retryable());
        // A click queued from the old feedback strip cannot write a second copy.
        operations::retry(t.w.store(), failed_id);
        t.acc.drain(&t.w);
        assert!(!rt
            .operations
            .list()
            .iter()
            .any(|op| matches!(op.status, Status::Failed { .. })));
        assert_eq!(
            std::fs::read(t.dir.join("Downloads/report.pdf")).unwrap(),
            b"original document"
        );
        assert_eq!(
            std::fs::read_dir(t.dir.join("Downloads")).unwrap().count(),
            1
        );
    }
}

#[test]
fn download_names_reject_unicode_format_controls() {
    // Overrides, embeddings, isolates and marks must not disguise an extension.
    // Cover other format controls too, including characters beyond the BMP.
    let controls = [
        '\u{061c}',
        '\u{200e}',
        '\u{200f}',
        '\u{202a}',
        '\u{202b}',
        '\u{202c}',
        '\u{202d}',
        '\u{202e}',
        '\u{2066}',
        '\u{2067}',
        '\u{2068}',
        '\u{2069}',
        '\u{00ad}',
        '\u{200b}',
        '\u{200c}',
        '\u{200d}',
        '\u{2060}',
        '\u{feff}',
        '\u{fff9}',
        '\u{fffb}',
        '\u{1d173}',
        '\u{e0001}',
        '\u{e007f}',
    ];
    for c in controls {
        assert_eq!(
            downloads::safe_name(&format!("invoice{c}gpj.exe")),
            "invoice_gpj.exe",
            "{c:?}"
        );
    }
    let ordinary = "résumé-עברית-العربية.pdf";
    assert_eq!(
        downloads::safe_name(ordinary),
        ordinary,
        "visible Unicode is retained"
    );

    let t = DownloadTest::new();
    let source = t.start();
    t.source(&source, "invoice\u{202e}gpj.exe");
    let transfer = last_request(&t.td, "downloadFile");
    t.acc.on_update(&t.w, &t.complete(&transfer).to_string());
    assert_eq!(
        std::fs::read(t.dir.join("Downloads/invoice_gpj.exe")).unwrap(),
        b"original document"
    );
    assert!(t.op(&source).line().contains("~/Downloads/invoice_gpj.exe"));
}

#[test]
fn download_names_strip_paths_and_control_characters() {
    for (input, expected) in [
        ("../../report.pdf", "report.pdf"),
        ("C:\\temp\\report.pdf", "report.pdf"),
        ("/tmp/", "telegram-file"),
        ("..", "telegram-file"),
        ("\0x\n.pdf", "_x_.pdf"),
        (".hidden", "hidden"),
    ] {
        assert_eq!(downloads::safe_name(input), expected);
    }
    let long = downloads::safe_name(&format!("{}.pdf", "é".repeat(250)));
    assert!(long.len() <= 240);
    assert!(long.ends_with(".pdf"));
}

#[test]
fn downloads_save_full_media_and_use_original_names_when_available() {
    let cases = [
        (
            json!({"@type": "messageVideo", "video": {"file_name": "holiday.mov",
            "thumbnail": {"file": {"id": 999}}, "video": file()}}),
            "holiday.mov",
        ),
        (
            json!({"@type": "messageAnimation", "animation": {"file_name": "loop.mp4",
            "thumbnail": {"file": {"id": 999}}, "animation": file()}}),
            "loop.mp4",
        ),
        (
            json!({"@type": "messageAudio", "audio": {"file_name": "song.flac", "audio": file()}}),
            "song.flac",
        ),
        (
            json!({"@type": "messageVoiceNote", "voice_note": {"voice": file()}}),
            "telegram-7-42.ogg",
        ),
        (
            json!({"@type": "messageVideoNote", "video_note": {"video": file()}}),
            "telegram-7-42.mp4",
        ),
        (
            json!({"@type": "messagePhoto", "photo": {"sizes": [
                {"width": 10, "height": 10, "photo": {"id": 999}},
                {"width": 100, "height": 100, "photo": file()}
            ]}}),
            "telegram-7-42.jpg",
        ),
    ];
    for (content, name) in cases {
        let t = DownloadTest::new();
        let source = t.start();
        t.acc.on_update(
            &t.w,
            &json!({"@type": "message", "chat_id": 7, "id": 42,
            "@extra": source["@extra"], "content": content})
            .to_string(),
        );
        let transfer = last_request(&t.td, "downloadFile");
        assert_eq!(
            transfer["file_id"], 77,
            "save the original, never a thumbnail"
        );
        assert_eq!(
            t.td.sent_types()
                .iter()
                .filter(|kind| *kind == "downloadFile")
                .count(),
            1
        );
        t.acc.on_update(&t.w, &t.complete(&transfer).to_string());
        assert_eq!(
            std::fs::read(t.dir.join("Downloads").join(name)).unwrap(),
            b"original document"
        );
        assert_eq!(t.op(&source).status, Status::Done);
    }
}
