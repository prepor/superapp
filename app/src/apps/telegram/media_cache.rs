//! Local media paths prepared outside drawing. Rows and viewers share a
//! bounded cache; a completed download invalidates only its own reference.

use std::collections::VecDeque;
use std::path::PathBuf;
use std::sync::{atomic::{AtomicBool, Ordering}, Arc, Mutex};
use std::time::{Duration, Instant};

use kernel::store::Store;
use kernel::{app::WorldFactory, caps::Blobs, effect::World};
use tokio::sync::{oneshot, Semaphore};

use super::model;

const CAPACITY: usize = 32;
const RECHECK: Duration = Duration::from_secs(1);

#[derive(Clone, Default)]
pub(super) struct Paths {
    pub file: Option<PathBuf>,
    pub playable: Option<PathBuf>,
}

#[derive(Clone)]
pub(super) enum Reading {
    Pending(Option<Paths>),
    Ready(Paths),
}

impl Reading {
    pub fn paths(self) -> Option<Paths> {
        match self { Self::Pending(paths) => paths, Self::Ready(paths) => Some(paths) }
    }
}

enum State {
    Pending(oneshot::Receiver<Paths>, Option<Paths>),
    Ready(Paths, Instant),
}

struct Entry {
    reference: String,
    kind: Kind,
    state: State,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Kind { File, Clip, Blob }

struct Cache {
    entries: Mutex<VecDeque<Entry>>,
    pool: Arc<Semaphore>,
    changed: AtomicBool,
}

impl Default for Cache {
    fn default() -> Self {
        Self { entries: Mutex::default(), pool: Arc::new(Semaphore::new(2)), changed: AtomicBool::new(false) }
    }
}

fn prepare(dir: Option<&std::path::Path>, reference: &str, clip: bool) -> Paths {
    Paths {
        file: model::media_path(dir, reference),
        playable: clip.then(|| model::playable_path(dir, reference)).flatten(),
    }
}

pub(super) fn read(store: &Store, reference: &str, clip: bool) -> Reading {
    if !reference.starts_with("tg:") { return Reading::Ready(Paths::default()); }
    // Scripted fixtures keep deterministic passes. A native UI reader always
    // registers with attach_ui before opening its panels.
    if !store.ui_attached() { return Reading::Ready(prepare(store.dir(), reference, clip)); }
    read_cached(store, reference, if clip { Kind::Clip } else { Kind::File }, None)
}

/// File viewers use the world's exact blob capability, including injected
/// caches. Its native get/stat runs on a reconstructed service world.
pub(super) fn read_blob(world: &World, reference: &str) -> Reading {
    if !world.store().ui_attached() {
        return Reading::Ready(Paths {
            file: world.with_cap::<dyn Blobs, _>(|blobs| blobs.get(reference)).ok().flatten(),
            playable: None,
        });
    }
    read_cached(world.store(), reference, Kind::Blob, world.factory())
}

fn read_cached(store: &Store, reference: &str, kind: Kind, factory: Option<WorldFactory>) -> Reading {
    let cache = store.local::<Cache>();
    let mut entries = cache.entries.lock().expect("media paths");
    let previous = entries.iter().position(|entry| entry.reference == reference && entry.kind == kind)
        .and_then(|at| entries.remove(at));
    let old = previous.as_ref().and_then(|entry| match &entry.state {
        State::Ready(paths, _) => Some(paths.clone()),
        State::Pending(_, old) => old.clone(),
    });
    let mut entry = previous.filter(|entry|
        !matches!(&entry.state, State::Ready(_, at) if at.elapsed() >= RECHECK));
    if entry.is_none() {
        let (mut send, receive) = oneshot::channel();
        entry = Some(Entry { reference: reference.into(), kind, state: State::Pending(receive, old) });
        let dir = store.dir().map(std::path::Path::to_path_buf);
        let reference = reference.to_string();
        let pool = cache.pool.clone();
        let owner = Arc::downgrade(&cache);
        let notify = store.ui_waker();
        kernel::runtime::spawn(async move {
            let permit = tokio::select! {
                permit = pool.acquire_owned() => permit.expect("media pool remains open"),
                _ = send.closed() => return,
            };
            let paths = kernel::runtime::spawn_blocking(move || {
                let _permit = permit;
                if kind == Kind::Blob {
                    Paths { file: factory.and_then(|factory| factory.build().ok())
                        .and_then(|world| world.with_cap::<dyn Blobs, _>(|blobs| blobs.get(&reference)).ok().flatten()),
                        playable: None }
                } else {
                    prepare(dir.as_deref(), &reference, kind == Kind::Clip)
                }
            }).await.unwrap_or_default();
            if send.send(paths).is_ok() {
                if let Some(cache) = owner.upgrade() { cache.changed.store(true, Ordering::Release); }
                if let Some(notify) = notify { notify(); }
            }
        });
    }
    let mut entry = entry.unwrap();
    if let State::Pending(receive, _) = &mut entry.state {
        match receive.try_recv() {
            Ok(paths) => entry.state = State::Ready(paths, Instant::now()),
            Err(oneshot::error::TryRecvError::Closed) => entry.state = State::Ready(Paths::default(), Instant::now()),
            Err(oneshot::error::TryRecvError::Empty) => {},
        }
    }
    let result = match &entry.state {
        State::Pending(_, previous) => Reading::Pending(previous.clone()),
        State::Ready(paths, _) => Reading::Ready(paths.clone()),
    };
    entries.push_back(entry);
    while entries.len() > CAPACITY { entries.pop_front(); }
    result
}

pub(super) fn invalidate(store: &Store, reference: &str) {
    let cache = store.local::<Cache>();
    // Dropping a pending receiver cancels its queued read. Keep a playable
    // path during revalidation so refreshing a file never resets its player.
    for entry in cache.entries.lock().expect("media paths").iter_mut()
        .filter(|entry| entry.reference == reference) {
        let previous = match &entry.state {
            State::Ready(paths, _) => paths.clone(),
            State::Pending(_, paths) => paths.clone().unwrap_or_default(),
        };
        entry.state = State::Ready(previous, Instant::now() - RECHECK);
    }
    cache.changed.store(true, Ordering::Release);
}

pub(super) fn take_changed(store: &Store) -> bool {
    store.local::<Cache>().changed.swap(false, Ordering::AcqRel)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ready(store: &Store, reference: &str) -> Paths {
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            if let Reading::Ready(paths) = read(store, reference, true) { return paths; }
            assert!(Instant::now() < deadline, "media paths prepared");
            std::thread::sleep(Duration::from_millis(1));
        }
    }

    #[test]
    fn media_paths_prepare_off_ui_coalesce_and_preserve_playback_during_refresh() {
        let dir = std::env::temp_dir().join(format!("telegram-paths-{}-{}", std::process::id(),
            std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos()));
        std::fs::create_dir_all(dir.join("blobs")).unwrap();
        let reference = "tg:prepared-path-test";
        let blob = dir.join("blobs").join(kernel::caps::file_name(reference));
        std::fs::write(&blob, b"\0\0\0\x18ftypisom").unwrap();
        let store = Store::open(Some(&dir.join("store.sqlite")), &[]).unwrap();
        store.attach_ui(|| {});
        let cache = store.local::<Cache>();
        let held = kernel::runtime::block_on(cache.pool.clone().acquire_many_owned(2)).unwrap();
        for _ in 0..100 {
            assert!(matches!(read(&store, reference, true), Reading::Pending(None)));
        }
        assert_eq!(cache.entries.lock().unwrap().len(), 1, "rows share one pending reference");
        assert!(!dir.join("blobs-play").exists(), "drawing does no filesystem preparation");
        drop(held);
        let first = ready(&store, reference).playable.unwrap();
        assert_eq!(first.extension().unwrap(), "mp4");
        assert_eq!(std::fs::read(&first).unwrap(), b"\0\0\0\x18ftypisom");

        let held = kernel::runtime::block_on(cache.pool.clone().acquire_many_owned(2)).unwrap();
        invalidate(&store, reference);
        assert_eq!(read(&store, reference, true).paths().unwrap().playable, Some(first.clone()),
            "a pending refresh must not reset native playback");
        // Replace the reference while its old check is still queued. A late
        // result has no receiver and cannot become the replacement's snapshot.
        std::fs::write(&blob, [0x1a, 0x45, 0xdf, 0xa3, 0, 0, 0, 0]).unwrap();
        invalidate(&store, reference);
        assert_eq!(read(&store, reference, true).paths().unwrap().playable, Some(first));
        drop(held);
        assert_eq!(ready(&store, reference).playable.unwrap().extension().unwrap(), "webm");
        drop(store);
        std::fs::remove_dir_all(dir).unwrap();
    }
}
