//! Transcript pictures, shared by rows and cards. Only texture installation
//! runs during a draw; reading files, decoding and drawing maps run in a
//! bounded worker pool. A recycled row never inherits another source's image.

use std::{collections::VecDeque, path::PathBuf, sync::mpsc, time::Instant};
use makepad_widgets::*;
use makepad_widgets::image_cache::decode_image_from_data;

use crate::shell::widgets::map::{self, FakeTiles};
use super::super::{model::Media, seed};

const MAX_ENTRIES: usize = 32;
const MAX_BYTES: usize = 64 * 1024 * 1024;

#[derive(Clone, PartialEq, Eq)]
enum Source {
    File(PathBuf),
    Demo(String),
    Map(u64, u64),
}

struct Pixels { width: usize, height: usize, data: Vec<u32> }

impl Source {
    fn read(&self) -> Option<Pixels> {
        if let Self::Map(lat, lon) = self {
            let snap = map::snapshot(&mut FakeTiles, f64::from_bits(*lat), f64::from_bits(*lon), map::ZOOM, 320, 160);
            return Some(Pixels { width: snap.width, height: snap.height, data: snap.pixels });
        }
        let bytes = match self {
            Self::File(path) => std::fs::read(path).ok()?,
            Self::Demo(reference) => seed::demo_bytes(reference)?.to_vec(),
            Self::Map(..) => unreachable!(),
        };
        let image = decode_image_from_data(&bytes).ok()?;
        Some(Pixels { width: image.width, height: image.height, data: image.data })
    }
}

enum State {
    Loading(mpsc::Receiver<Option<Pixels>>),
    Ready(Texture, usize),
    Missing(Instant),
}

struct Entry { source: Source, state: State }

#[derive(Default)]
struct Cache {
    entries: VecDeque<Entry>,
    pool: Option<TagThreadPool<Source>>,
    retry: Timer,
    retry_at: Option<Instant>,
}

#[derive(Debug)]
struct PictureReady;

/// A completion wakes even a stationary, unfocused preview. A missing local
/// blob retries after download without rereading it on every animation frame.
pub fn changed(cx: &mut Cx, event: &Event) -> bool {
    let retry = cx.has_global::<Cache>() && cx.get_global::<Cache>().retry.is_event(event).is_some();
    matches!(event, Event::Actions(actions) if actions.iter().any(|a| a.downcast_ref::<PictureReady>().is_some())) || retry
}

fn fill(cx: &mut Cx, picture: &WidgetRef, source: Option<Source>, fixture: bool) -> bool {
    let Some(source) = source else { picture.set_visible(cx, false); return false };
    let mut entry = {
        let cache = cx.global::<Cache>();
        cache.entries.iter().position(|e| e.source == source).and_then(|i| cache.entries.remove(i))
    };
    if entry.as_ref().is_some_and(|e| matches!(e.state, State::Missing(at) if at.elapsed().as_secs_f64() >= 1.0)) {
        entry = None;
    }
    let mut entry = entry.unwrap_or_else(|| {
        let (tx, rx) = mpsc::channel();
        if fixture || (cfg!(headless) && !matches!(source, Source::File(_))) {
            // Like other scripted passes, fixtures finish within virtual time.
            let _ = tx.send(source.read());
        } else {
            if cx.global::<Cache>().pool.is_none() {
                let pool = TagThreadPool::new(cx, 2);
                cx.global::<Cache>().pool = Some(pool);
            }
            cx.global::<Cache>().pool.as_ref().unwrap().execute_rev(source.clone(), move |source| {
                if tx.send(source.read()).is_ok() { Cx::post_action(PictureReady); }
            });
        }
        Entry { source, state: State::Loading(rx) }
    });
    if let State::Loading(rx) = &entry.state {
        if let Ok(result) = rx.try_recv() {
            entry.state = match result {
                Some(pixels) => {
                    let bytes = pixels.data.len() * std::mem::size_of::<u32>();
                    let texture = Texture::new_with_format(cx, TextureFormat::VecBGRAu8_32 {
                        width: pixels.width, height: pixels.height, data: Some(pixels.data),
                        updated: TextureUpdated::Full,
                    });
                    State::Ready(texture, bytes)
                }
                None => {
                    if cx.global::<Cache>().retry_at.is_none_or(|at| at.elapsed().as_secs_f64() >= 1.0) {
                        let retry = cx.start_timeout(1.0);
                        cx.global::<Cache>().retry = retry;
                        cx.global::<Cache>().retry_at = Some(Instant::now());
                    }
                    State::Missing(Instant::now())
                }
            };
        }
    }
    let shown = if let State::Ready(texture, _) = &entry.state {
        picture.widget(cx, ids!(img)).as_image().set_texture(cx, Some(texture.clone()));
        true
    } else { false };
    picture.set_visible(cx, shown);
    let cache = cx.global::<Cache>();
    cache.entries.push_back(entry);
    let mut bytes = cache.entries.iter().map(|e| match e.state { State::Ready(_, n) => n, _ => 0 }).sum::<usize>();
    while cache.entries.len() > MAX_ENTRIES || (bytes > MAX_BYTES && cache.entries.len() > 1) {
        if let Some(Entry { state: State::Ready(_, n), .. }) = cache.entries.pop_front() { bytes -= n; }
    }
    if let Some(pool) = &cache.pool {
        pool.retain_queued(|source| cache.entries.iter().any(|e| &e.source == source));
    }
    shown
}

fn photo_source(media: Option<&Media>, dir: Option<&std::path::Path>) -> Option<Source> {
    media.filter(|m| m.has_picture()).and_then(|m| m.reference.as_deref()).and_then(|reference| {
        if reference.starts_with("tg:") {
            Some(Source::File(dir?.join("blobs").join(kernel::caps::file_name(reference))))
        } else if reference.starts_with("demo:") {
            Some(Source::Demo(reference.to_string()))
        } else { None }
    })
}

pub fn photo(cx: &mut Cx, picture: &WidgetRef, media: Option<&Media>, dir: Option<&std::path::Path>) -> bool {
    fill(cx, picture, photo_source(media, dir), dir.is_none())
}

pub fn missing(cx: &mut Cx, media: &Media, dir: Option<&std::path::Path>) -> bool {
    let Some(source) = photo_source(Some(media), dir) else { return false };
    cx.global::<Cache>().entries.iter().any(|e| e.source == source && matches!(e.state, State::Missing(_)))
}

pub fn place(cx: &mut Cx, picture: &WidgetRef, media: Option<&Media>, fixture: bool) {
    let source = media.filter(|m| matches!(m.kind.as_str(), "location" | "live"))
        .and_then(|m| Some(Source::Map(m.lat?.to_bits(), m.lon?.to_bits())));
    fill(cx, picture, source, fixture);
}

#[cfg(all(test, headless, unix))]
mod tests {
    use super::*;
    use std::{io::Write, time::Duration};

    #[test]
    fn a_blocked_image_read_does_not_block_the_ui_and_cached_images_need_no_file() {
        let dir = std::env::temp_dir().join(format!("superapp-picture-worker-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("slow-picture");
        assert!(std::process::Command::new("mkfifo").arg(&path).status().unwrap().success());
        let (go, started) = mpsc::channel();
        let destination = path.clone();
        // A FIFO makes the real read block until the UI call has returned.
        // The timeout also releases a regressed synchronous read before failing.
        let writer = std::thread::spawn(move || {
            let returned = started.recv_timeout(Duration::from_secs(3)).is_ok();
            std::fs::File::create(destination).unwrap().write_all(seed::demo_bytes("demo:garden").unwrap()).unwrap();
            returned
        });
        let mut cx = Cx::new(Box::new(|_, _| {}));
        let picture = WidgetRef::empty();
        let source = Source::File(path.clone());
        assert!(!fill(&mut cx, &picture, Some(source.clone()), false));
        go.send(()).unwrap();
        assert!(writer.join().unwrap(), "drawing waited for filesystem I/O");
        let deadline = Instant::now() + Duration::from_secs(5);
        while !fill(&mut cx, &picture, Some(source.clone()), false) {
            assert!(Instant::now() < deadline, "the decoded picture never arrived");
            std::thread::sleep(Duration::from_millis(1));
        }
        std::fs::remove_file(&path).unwrap();
        assert!(fill(&mut cx, &picture, Some(source), false), "a cached texture must not stat or read its file again");
        assert!(!fill(&mut cx, &picture, Some(Source::File(dir.join("different-picture"))), false),
            "a recycled picture must not show the previous source while loading");
        std::fs::remove_dir_all(dir).unwrap();
    }
}
