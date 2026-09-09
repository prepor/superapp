//! Device-local diagnostic lines, separate from runtime coordination.

pub fn error(store_dir: Option<&std::path::Path>, line: &str) {
    eprintln!("telegram: {line}");
    note(store_dir, &format!("!! {line}"));
}

enum Entry {
    Note(std::path::PathBuf, String, u64),
    Flush(tokio::sync::oneshot::Sender<()>),
}

fn writer() -> &'static tokio::sync::mpsc::UnboundedSender<Entry> {
    static WRITER: std::sync::OnceLock<tokio::sync::mpsc::UnboundedSender<Entry>> = std::sync::OnceLock::new();
    WRITER.get_or_init(|| {
        let (send, mut receive) = tokio::sync::mpsc::unbounded_channel();
        kernel::runtime::spawn(async move {
            while let Some(entry) = receive.recv().await {
                match entry {
                    Entry::Note(path, line, secs) => {
                        let _ = kernel::runtime::spawn_blocking(move || {
                            use std::io::Write as _;
                            if let Ok(mut file) = std::fs::OpenOptions::new().create(true).append(true).open(path) {
                                let _ = writeln!(file, "{secs} {line}");
                            }
                        }).await;
                    }
                    Entry::Flush(done) => { let _ = done.send(()); }
                }
            }
        });
        send
    })
}

pub fn note(store_dir: Option<&std::path::Path>, line: &str) {
    let Some(dir) = store_dir else { return };
    let secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0);
    let _ = writer().send(Entry::Note(dir.join("tg-debug.log"), line.to_owned(), secs));
}

pub async fn flush() {
    let (done, complete) = tokio::sync::oneshot::channel();
    if writer().send(Entry::Flush(done)).is_ok() { let _ = complete.await; }
}

#[cfg(test)]
mod tests {
    #[test]
    fn errors_are_written_to_the_device_log() {
        let dir = std::env::temp_dir().join(format!(
            "superapp-telegram-error-log-{}",
            std::process::id()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        super::error(
            Some(&dir),
            "request 42 sendMessage chat=7: FILE_INVALID (400)",
        );
        kernel::runtime::block_on(super::flush());
        let log = std::fs::read_to_string(dir.join("tg-debug.log")).unwrap();
        assert!(log.contains("!! request 42 sendMessage chat=7: FILE_INVALID (400)"));
        std::fs::remove_dir_all(dir).unwrap();
    }
}
