//! One reader per open file. A PDF is parsed once; only its current page is
//! rasterized and retained. Dropping the receiver discards an obsolete result.

use super::Preview;
use crate::reader::pdf::{Document, Page};
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
}

enum Loaded {
    Pdf(Document),
    Ready(Ready),
}

pub struct Worker {
    requests: Option<Sender<usize>>,
    replies: Option<Receiver<Result<Ready, String>>>,
    inline: Option<Document>,
    ready: Option<Result<Ready, String>>,
}

impl Worker {
    pub fn start(source: Preview) -> Result<Self, String> {
        let mut worker = Self {
            requests: None,
            replies: None,
            inline: None,
            ready: None,
        };
        if cfg!(headless) {
            match guarded(|| load(source))? {
                Loaded::Pdf(pdf) => {
                    worker.ready = Some(guarded(|| pdf.render(0).map(Ready::Pdf)));
                    worker.inline = Some(pdf);
                }
                Loaded::Ready(ready) => worker.ready = Some(Ok(ready)),
            }
            return Ok(worker);
        }
        let (ask, requests) = mpsc::channel();
        let (answer, replies) = mpsc::channel();
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
                        if !send(guarded(|| pdf.render(0).map(Ready::Pdf))) {
                            return;
                        }
                        while let Ok(page) = requests.recv() {
                            if !send(guarded(|| pdf.render(page).map(Ready::Pdf))) {
                                break;
                            }
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

    pub fn page(&mut self, page: usize) -> Result<(), String> {
        if let Some(pdf) = &self.inline {
            self.ready = Some(guarded(|| pdf.render(page).map(Ready::Pdf)));
            return Ok(());
        }
        self.requests
            .as_ref()
            .ok_or("The PDF viewer stopped; reopen the file")?
            .send(page)
            .map_err(|_| "The PDF viewer stopped; reopen the file".into())
    }

    pub fn poll(&mut self) -> Option<Result<Ready, String>> {
        if let Some(result) = self.ready.take() {
            return Some(result);
        }
        match self.replies.as_ref()?.try_recv() {
            Ok(result) => Some(result),
            Err(TryRecvError::Empty) => None,
            Err(TryRecvError::Disconnected) => Some(Err(
                "The file viewer stopped; reopen the file to try again".into(),
            )),
        }
    }
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

    #[test]
    fn worker_delivers_requested_pages_and_recovers_after_a_bad_page_number() {
        let mut worker = Worker::start(Preview::Pdf(kernel::caps::demo::PDF.to_vec())).unwrap();
        let receive = |worker: &mut Worker| {
            worker.ready.take().unwrap_or_else(|| {
                worker
                    .replies
                    .as_ref()
                    .unwrap()
                    .recv_timeout(std::time::Duration::from_secs(10))
                    .expect("worker answered")
            })
        };
        assert!(matches!(
            receive(&mut worker).unwrap(),
            Ready::Pdf(Page {
                number: 0,
                count: 2,
                ..
            })
        ));
        worker.page(1).unwrap();
        assert!(matches!(
            receive(&mut worker).unwrap(),
            Ready::Pdf(Page {
                number: 1,
                size: (595, 420),
                ..
            })
        ));
        worker.page(2).unwrap();
        assert!(receive(&mut worker).is_err());
        worker.page(0).unwrap();
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
        assert_eq!(pdf.render(1).unwrap().size, (595, 420));
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
