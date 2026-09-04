//! This machine's disk, watched: the changes no verb of ours made.
//!
//! The kernel's [`Watcher`] keeps the books — which directories somebody is
//! looking at, and how many rounds of change each has seen. What is added
//! here is the machine underneath them: a thread that asks the platform to
//! be told, and rings the shell's doorbell once a round has landed.
//!
//! macOS watches with FSEvents and android with inotify; anywhere else the
//! books stand alone and nothing ever changes, which is what a build with
//! no backend can honestly say. Both backends watch **one directory at a
//! time and never a tree**: a panel shows one directory's entries, so a
//! build running three levels down is not its business. FSEvents is a
//! recursive instrument, so its half filters; inotify is not, so its half
//! does not have to.
//!
//! Events are grouped twice over: the platform gathers a burst into one
//! delivery — FSEvents by the latency it is created with, inotify by
//! whatever has queued up behind one wait — and a delivery bumps a
//! directory once however many paths it carried. A copy of a thousand
//! files is a round, not a thousand.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use kernel::caps::{Watched, Watcher};

#[cfg(target_os = "macos")]
mod fsevents;
#[cfg(target_os = "macos")]
use fsevents as imp;

#[cfg(any(target_os = "android", target_os = "linux"))]
mod inotify;
#[cfg(any(target_os = "android", target_os = "linux"))]
use inotify as imp;

#[cfg(not(any(target_os = "macos", target_os = "android", target_os = "linux")))]
mod unwatched;
#[cfg(not(any(target_os = "macos", target_os = "android", target_os = "linux")))]
use unwatched as imp;

/// How long a backend gives one turn of its loop before looking at the
/// wanted set again. A panel that has just opened is already listed, so
/// this is only how long a brand-new listing can miss a change for — and
/// the backends are woken on a change to the set anyway, where the
/// platform lets them be.
const TURN: f64 = 1.0;

/// The machine's own watcher: the books, and the thread that fills them.
///
/// One per process — the window's world holds it, and a worker's world
/// keeps the fake, because a background pass has no panels to refresh.
pub struct RealWatcher {
    books: Watched,
    thread: imp::Thread,
}

impl RealWatcher {
    /// Starts watching nothing, and rings `notify` — the UI thread's own
    /// doorbell — once each round of change has been counted.
    ///
    /// A thread that will not start leaves the books alone: panels then
    /// refresh on their own writes exactly as they did before there was a
    /// watcher, which is a duller app and not a broken one.
    #[must_use]
    pub fn start(notify: impl Fn() + Send + Sync + 'static) -> RealWatcher {
        let books = Watched::new();
        let thread = imp::Thread::start(Watching {
            books: books.clone(),
            stop: Arc::new(AtomicBool::new(false)),
            notify: Arc::new(notify),
        });
        RealWatcher { books, thread }
    }
}

/// The capability is the books; what the thread adds is that they fill
/// themselves. A change to the wanted set knocks on the thread's door, so
/// a panel that just opened is watched now rather than at the next turn.
impl Watcher for RealWatcher {
    fn watch(&mut self, dir: &Path) {
        self.books.watch(dir);
        self.thread.wake();
    }

    fn unwatch(&mut self, dir: &Path) {
        self.books.unwatch(dir);
        self.thread.wake();
    }

    fn changed(&mut self, dir: &Path) {
        self.books.changed(dir);
    }

    fn revision(&mut self, dir: &Path) -> u64 {
        self.books.revision(dir)
    }

    fn moved(&mut self) -> bool {
        self.books.moved()
    }
}

impl Drop for RealWatcher {
    fn drop(&mut self) {
        self.thread.stop();
    }
}

/// What a watching thread was given: the books to report into, the flag
/// that ends it, and the doorbell.
///
/// Cloneable, because a platform callback needs its own handle on all
/// three — every part of it is a shared handle already.
#[derive(Clone)]
pub(crate) struct Watching {
    books: Watched,
    stop: Arc<AtomicBool>,
    notify: Arc<dyn Fn() + Send + Sync>,
}

impl Watching {
    /// Whether the handle has gone: the loop finishes its turn and returns.
    pub(crate) fn stopped(&self) -> bool {
        self.stop.load(Ordering::Relaxed)
    }

    /// Says so. The backend's own knock follows, since a loop that is
    /// waiting will not read this until something ends the wait.
    pub(crate) fn end(&self) {
        self.stop.store(true, Ordering::Relaxed);
    }

    /// Every directory somebody is looking at right now, sorted — a
    /// backend compares this against what it is already watching, and
    /// rebuilds when the two differ.
    pub(crate) fn want(&self) -> Vec<PathBuf> {
        self.books.dirs()
    }

    /// One round of change, however many paths carried it: each directory
    /// counted once, and the doorbell rung once — and only when a
    /// directory somebody is looking at was among them.
    pub(crate) fn report(&self, dirs: &[PathBuf]) {
        if dirs.is_empty() {
            return;
        }
        let mut books = self.books.clone();
        for dir in dirs {
            books.changed(dir);
        }
        (self.notify)();
    }
}

#[cfg(all(test, target_os = "macos"))]
mod tests {
    //! The one thing no fake can answer for: that this machine actually
    //! tells us. Everything above the capability is tested against the
    //! kernel's books, which report what a test says; this reports what
    //! the disk did.

    use std::path::PathBuf;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Arc;
    use std::time::{Duration, Instant};

    use super::{RealWatcher, Watcher};

    /// A directory of this run's own, swept before it is used.
    fn scratch(name: &str) -> PathBuf {
        let mine = format!("superapp-watch-{}-{name}", std::process::id());
        let dir = std::env::temp_dir().join(mine);
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("a directory to watch");
        dir
    }

    /// Another program writes, and the panel that is looking at that
    /// directory has a reason to read it again — counted for the
    /// directory it happened in, with the UI thread told there is
    /// something to see. What happens in the directory next door, or a
    /// level below, is not that directory's news.
    ///
    /// Written to over and over until the first one lands: a stream is
    /// asked for and started on another thread, and *when* it is live is
    /// the platform's business, not this test's. What is asserted is that
    /// it does land.
    #[test]
    fn a_write_by_somebody_else_is_counted_where_it_happened() {
        let (watched, next_door) = (scratch("in"), scratch("out"));
        let below = watched.join("below");
        std::fs::create_dir_all(&below).expect("a directory under the watched one");
        let rung = Arc::new(AtomicUsize::new(0));
        let bell = rung.clone();
        let mut w = RealWatcher::start(move || {
            bell.fetch_add(1, Ordering::SeqCst);
        });
        w.watch(&watched);
        w.watch(&next_door);

        let deadline = Instant::now() + Duration::from_secs(20);
        let mut n = 0;
        while w.revision(&watched) == 0 && Instant::now() < deadline {
            n += 1;
            std::fs::write(watched.join(format!("{n}.txt")), b"hello").expect("a file");
            std::thread::sleep(Duration::from_millis(100));
        }
        assert!(w.revision(&watched) > 0, "FSEvents said what happened");
        assert!(w.moved(), "and the shell was told to look");
        assert!(rung.load(Ordering::SeqCst) > 0, "on the UI thread's bell");
        assert_eq!(
            w.revision(&next_door),
            0,
            "a directory that was not written to is not news"
        );

        // A tree is what FSEvents watches and a directory is what a panel
        // shows: what happens a level down belongs to nobody here.
        let (before, patience) = (w.revision(&watched), Duration::from_millis(800));
        std::fs::write(below.join("deep.txt"), b"hello").expect("a file");
        std::thread::sleep(patience);
        assert_eq!(
            w.revision(&watched),
            before,
            "a write below a watched directory is not a change to it"
        );

        // And it lets go: what a closed panel leaves is a directory that
        // is not watched, whatever happens in it afterwards.
        w.unwatch(&watched);
        std::fs::write(watched.join("after.txt"), b"hello").expect("a file");
        std::thread::sleep(patience);
        assert_eq!(
            w.revision(&watched),
            0,
            "the books forgot it with the watch"
        );

        drop(w); // the thread stops with the watcher, and joins
        for dir in [watched, next_door] {
            let _ = std::fs::remove_dir_all(dir);
        }
    }
}
