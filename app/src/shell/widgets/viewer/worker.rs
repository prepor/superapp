//! One reader per open file. PDF geometry comes first, then requested pages.
//! Dropping the receiver discards an obsolete result.

use super::{Preview, selection::{append_copy, Span}};
use crate::reader::pdf::{Document, Page, TextPage};
use makepad_widgets::{image_cache::decode_image_from_data, SignalToUI};
use std::sync::mpsc::{self, Receiver, Sender, TryRecvError};

pub enum Ready {
    Text(String),
    Picture {
        width: usize,
        height: usize,
        pixels: Vec<u32>,
    },
    Pdf(Page),
    PdfInfo(Vec<(u32, u32)>),
    PdfText(usize, TextPage),
    Copied(Span, Result<String, String>),
}

pub(super) enum Request { Page(usize), Text(usize), Copy(Option<Span>) }

struct CopyJob { span: Span, next: usize, text: String }

impl CopyJob {
    fn new(span: Span) -> Self { Self { span, next: span.start.page, text: String::new() } }

    /// Yield between pages so a long copy does not starve visible page requests.
    fn step(&mut self, pdf: &Document) -> Option<Result<String, String>> {
        let result = guarded(|| {
            let page = pdf.text(self.next);
            if let Some(error) = page.error { return Err(error); }
            let range = self.span.range(self.next, page.text.len());
            append_copy(&mut self.text, page.text.get(range).ok_or("Could not read the selected text")?)
        });
        if let Err(error) = result { return Some(Err(error)); }
        if self.next == self.span.end.page { return Some(Ok(std::mem::take(&mut self.text))); }
        self.next += 1;
        None
    }
}

enum Loaded {
    Pdf(Document),
    Ready(Ready),
}

pub struct Worker {
    requests: Option<Sender<Request>>,
    replies: Option<Receiver<Result<Ready, String>>>,
    inline: Option<Document>,
    ready: Option<Result<Ready, String>>,
    copy: Option<CopyJob>,
}

impl Worker {
    pub fn start(source: Preview) -> Result<Self, String> {
        let mut worker = Self {
            requests: None,
            replies: None,
            inline: None,
            ready: None,
            copy: None,
        };
        if cfg!(headless) {
            match guarded(|| load(source))? {
                Loaded::Pdf(pdf) => {
                    let sizes = pdf.sizes();
                    worker.ready = Some(Ok(Ready::PdfInfo(sizes)));
                    worker.inline = Some(pdf);
                }
                Loaded::Ready(ready) => worker.ready = Some(Ok(ready)),
            }
            return Ok(worker);
        }
        let (ask, requests) = mpsc::channel();
        // A paused UI must not accumulate decoded pages or text in its inbox.
        let (answer, replies) = mpsc::sync_channel(1);
        std::thread::Builder::new()
            .name("file-viewer".into())
            .spawn(move || {
                let send = |result| {
                    let live = answer.send(result).is_ok();
                    SignalToUI::set_ui_signal();
                    live
                };
                match guarded(|| load(source)) {
                    Ok(Loaded::Pdf(pdf)) => {
                        let sizes = pdf.sizes();
                        if !send(Ok(Ready::PdfInfo(sizes))) {
                            return;
                        }
                        let mut copy: Option<CopyJob> = None;
                        loop {
                            let request = if copy.is_some() { requests.try_recv() }
                                else { requests.recv().map_err(|_| TryRecvError::Disconnected) };
                            let result = match request {
                                Ok(Request::Page(page)) => guarded(|| pdf.render(page).map(Ready::Pdf)),
                                Ok(Request::Text(page)) => Ok(text_page(&pdf, page)),
                                Ok(Request::Copy(span)) => { copy = span.map(CopyJob::new); continue; }
                                Err(TryRecvError::Empty) => {
                                    let job = copy.as_mut().unwrap();
                                    let Some(result) = job.step(&pdf) else { continue; };
                                    let span = job.span;
                                    copy = None;
                                    Ok(Ready::Copied(span, result))
                                }
                                Err(TryRecvError::Disconnected) => break,
                            };
                            if !send(result) { break; }
                        }
                    }
                    Ok(Loaded::Ready(ready)) => {
                        send(Ok(ready));
                    }
                    Err(error) => {
                        send(Err(error));
                    }
                }
            })
            .map_err(|e| format!("Could not start the file viewer: {e}"))?;
        worker.requests = Some(ask);
        worker.replies = Some(replies);
        Ok(worker)
    }

    pub fn request(&mut self, request: Request) -> Result<(), String> {
        if let Some(pdf) = &self.inline {
            match request {
                Request::Page(page) => self.ready = Some(guarded(|| pdf.render(page).map(Ready::Pdf))),
                Request::Text(page) => self.ready = Some(Ok(text_page(pdf, page))),
                Request::Copy(span) => self.copy = span.map(CopyJob::new),
            }
            return Ok(());
        }
        self.requests
            .as_ref()
            .ok_or("The PDF viewer stopped; reopen the file")?
            .send(request)
            .map_err(|_| "The PDF viewer stopped; reopen the file".into())
    }

    pub fn poll(&mut self) -> Option<Result<Ready, String>> {
        if let Some(result) = self.ready.take() {
            return Some(result);
        }
        if let (Some(pdf), Some(job)) = (&self.inline, &mut self.copy) {
            loop {
                if let Some(result) = job.step(pdf) {
                    let span = job.span;
                    self.copy = None;
                    return Some(Ok(Ready::Copied(span, result)));
                }
            }
        }
        match self.replies.as_ref()?.try_recv() {
            Ok(result) => Some(result),
            Err(TryRecvError::Empty) => None,
            Err(TryRecvError::Disconnected) => {
                self.replies = None;
                Some(Err("The file viewer stopped; reopen the file to try again".into()))
            }
        }
    }
}

fn text_page(pdf: &Document, page: usize) -> Ready {
    Ready::PdfText(page, guarded(|| Ok(pdf.text(page))).unwrap_or_else(TextPage::failed))
}

fn guarded<T>(work: impl FnOnce() -> Result<T, String>) -> Result<T, String> {
    std::panic::catch_unwind(std::panic::AssertUnwindSafe(work))
        .unwrap_or_else(|_| Err("Could not display this file; it may be damaged".into()))
}

fn load(source: Preview) -> Result<Loaded, String> {
    match source {
        Preview::Bytes {
            bytes,
            name,
            kind,
            size,
        } => {
            let preview = kernel::caps::preview_of(kind, &name, size, |max| {
                Some(bytes[..bytes.len().min(max)].to_vec())
            });
            load(preview.into())
        }
        Preview::Text(text) => Ok(Loaded::Ready(Ready::Text(text))),
        Preview::Pdf(bytes) => Document::open(bytes).map(Loaded::Pdf),
        Preview::Image(bytes) => {
            // Refuse enormous decoded images before allocating their bitmap.
            if let Some((w, h)) = kernel::caps::image_size(&bytes) {
                if u64::from(w) * u64::from(h) > 40_000_000 {
                    return Err("Image is too large to preview".into());
                }
            }
            let image =
                decode_image_from_data(&bytes).map_err(|_| "Could not decode this image")?;
            Ok(Loaded::Ready(Ready::Picture {
                width: image.width,
                height: image.height,
                pixels: image.data,
            }))
        }
        Preview::Path {
            path,
            name,
            kind,
            size,
        } => {
            use std::io::Read;
            let preview = kernel::caps::preview_of(kind, &name, size, |max| {
                let mut bytes = Vec::new();
                std::fs::File::open(path)
                    .ok()?
                    .take(max as u64)
                    .read_to_end(&mut bytes)
                    .ok()?;
                Some(bytes)
            });
            load(preview.into())
        }
        Preview::Disk {
            factory,
            path,
            name,
            kind,
            size,
        } => {
            let preview = kernel::caps::preview_of(kind, &name, size, |max| {
                factory.make().read_file(&path, max).ok()
            });
            load(preview.into())
        }
        Preview::Error(error) => Err(error),
        _ => Err("No preview for this file; open it with the system viewer".into()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn receive(worker: &mut Worker) -> Result<Ready, String> {
        worker.poll().unwrap_or_else(|| worker.replies.as_ref().unwrap()
            .recv_timeout(std::time::Duration::from_secs(20)).expect("worker answered"))
    }

    #[test]
    fn text_is_requested_and_copy_includes_unvisited_pages_without_retaining_their_geometry() {
        use super::super::selection::{Position, Selection};
        let mut worker = Worker::start(Preview::Pdf(crate::reader::pdf::dense_fixture(200, 50, 80))).unwrap();
        assert!(matches!(receive(&mut worker).unwrap(), Ready::PdfInfo(sizes) if sizes.len() == 200));
        if let Some(replies) = &worker.replies {
            assert!(matches!(replies.recv_timeout(std::time::Duration::from_millis(30)), Err(mpsc::RecvTimeoutError::Timeout)),
                "opening a long document must not eagerly enqueue text");
        } else { assert!(worker.poll().is_none()); }
        worker.request(Request::Text(0)).unwrap();
        let Ready::PdfText(0, text) = receive(&mut worker).unwrap() else { panic!("requested text") };
        assert_eq!(text.glyphs.len(), 4000);
        let mut selection = Selection::default();
        selection.insert(0, text);
        selection.select_all();
        assert!(selection.copy(200).is_none());
        let span = selection.copy_request().unwrap();
        worker.request(Request::Copy(Some(span))).unwrap();
        worker.request(Request::Text(199)).unwrap();
        assert!(matches!(receive(&mut worker).unwrap(), Ready::PdfText(199, _)), "visible text takes priority over a long copy");
        let Ready::Copied(copied, text) = receive(&mut worker).unwrap() else { panic!("complete copy") };
        assert_eq!(copied, span);
        let text = text.unwrap();
        assert!(text.starts_with("p000 line000 ") && text.contains("p199 line049 "));
        assert_eq!(text.lines().filter(|line| line.starts_with('p')).count(), 10_000);
        assert_eq!(selection.copied(span, Ok(text)).unwrap().len(), 200 * (50 * 81 - 1) + 199 * 2);
        assert_eq!(selection.pages.len(), 1, "copy must not fill the geometry cache");

        let span = Span { start: Position { page: 2, byte: 13 }, end: Position { page: 4, byte: 25 } };
        worker.request(Request::Copy(Some(span))).unwrap();
        let Ready::Copied(_, text) = receive(&mut worker).unwrap() else { panic!("cross-page range") };
        let text = text.unwrap();
        assert!(text.starts_with("aaaa") && text.ends_with("p004 line000 aaaaaaaaaaaa"));
        assert!(text.contains("p003 line049 ") && !text.contains("p001"));
        worker.request(Request::Text(200)).unwrap();
        assert!(matches!(receive(&mut worker).unwrap(), Ready::PdfText(200, text) if text.error.is_some()));
    }

    #[test]
    fn worker_delivers_requested_pages_and_recovers_after_a_bad_page_number() {
        let mut worker = Worker::start(Preview::Pdf(kernel::caps::demo::PDF.to_vec())).unwrap();
        let receive = |worker: &mut Worker| {
            loop {
                let result = worker.ready.take().unwrap_or_else(|| {
                    worker
                    .replies
                    .as_ref()
                    .unwrap()
                    .recv_timeout(std::time::Duration::from_secs(10))
                        .expect("worker answered")
                });
                if !matches!(result, Ok(Ready::PdfText(..))) { break result; }
            }
        };
        let Ready::PdfInfo(sizes) = receive(&mut worker).unwrap() else { panic!("geometry before pixels") };
        assert_eq!(sizes, vec![(420, 595), (595, 420)]);
        worker.request(Request::Page(1)).unwrap();
        assert!(matches!(
            receive(&mut worker).unwrap(),
            Ready::Pdf(Page {
                number: 1,
                ..
            })
        ));
        worker.request(Request::Page(2)).unwrap();
        assert!(receive(&mut worker).is_err());
        worker.request(Request::Page(0)).unwrap();
        assert!(matches!(
            receive(&mut worker).unwrap(),
            Ready::Pdf(Page { number: 0, .. })
        ));
    }

    #[test]
    fn disk_and_cache_sources_use_the_same_decoders() {
        let source = Preview::Disk {
            factory: kernel::caps::DiskFactory::shared(kernel::caps::DemoDisk::new(
                Default::default(),
            )),
            path: kernel::caps::real_path("~/Downloads/report-q3.pdf"),
            name: "report-q3.pdf".into(),
            kind: kernel::caps::FileKind::Pdf,
            size: 0,
        };
        let Loaded::Pdf(pdf) = load(source).unwrap() else {
            panic!("PDF from disk")
        };
        assert_eq!(pdf.sizes()[1], (595, 420));
        let bytes = kernel::caps::demo::bytes_of("~/Downloads/2026/photo-lisbon.jpg").unwrap();
        assert!(matches!(
            load(Preview::Image(bytes)).unwrap(),
            Loaded::Ready(Ready::Picture {
                width: 32,
                height: 32,
                ..
            })
        ));
        let source = Preview::Path {
            path: std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("../kernel/resources/viewer-demo.pdf"),
            name: "document.PDF".into(),
            kind: kernel::caps::FileKind::Pdf,
            size: 0,
        };
        assert!(matches!(load(source).unwrap(), Loaded::Pdf(_)));
    }
}
