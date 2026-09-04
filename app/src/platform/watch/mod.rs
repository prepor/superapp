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

/// What a path resolves to right now: the directory both instruments
/// actually end up watching, since FSEvents is given a resolved path and
/// inotify watches the inode a path led to.
///
/// Asked on every turn and not once, because a link is a name for
/// somewhere else and a link can be repointed: `~/latest` is a different
/// directory after a build, with nothing whatever having happened in the
/// one it named before. A path with nothing at the end of it answers as
/// itself, which is stable — a directory that is not there does not keep
/// looking as though it had just moved.
pub(crate) fn resolve(dir: &Path) -> PathBuf {
    std::fs::canonicalize(dir).unwrap_or_else(|_| dir.to_path_buf())
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

    /// A path of this run's own — the pid is in the name, because two
    /// checkouts of this tree run their suites side by side and a
    /// temporary directory is shared by every one of them.
    fn spot(name: &str) -> PathBuf {
        std::env::temp_dir().join(format!("superapp-watch-{}-{name}", std::process::id()))
    }

    /// One of those, made, and swept first if a run before this left it.
    fn scratch(name: &str) -> PathBuf {
        let dir = spot(name);
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

        // A directory taken out from under a panel is news about that
        // directory: without `WATCH_ROOT` this is the one change FSEvents
        // would never mention, and the panel would draw a listing of a
        // path that is not there for as long as it stayed open.
        w.watch(&watched);
        let gone = spot("gone");
        let _ = std::fs::remove_dir_all(&gone);
        std::thread::sleep(patience);
        std::fs::rename(&watched, &gone).expect("the directory moves away");
        let deadline = Instant::now() + Duration::from_secs(20);
        while w.revision(&watched) == 0 && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(100));
        }
        assert!(
            w.revision(&watched) > 0,
            "the watched directory itself moving is a change to it"
        );

        drop(w); // the thread stops with the watcher, and joins
        for dir in [next_door, gone] {
            let _ = std::fs::remove_dir_all(dir);
        }
    }

    /// A link is a name for somewhere else, and repointing it changes
    /// what a panel is showing without anything happening in either
    /// directory. FSEvents is given a resolved path, so it would go on
    /// reporting the place the name used to lead to and say nothing about
    /// the swap: what answers for it is asking what the path leads to on
    /// every turn.
    #[test]
    fn a_link_pointed_somewhere_else_is_a_change_to_the_path() {
        let (here, there) = (scratch("link-here"), scratch("link-there"));
        let link = spot("link");
        let _ = std::fs::remove_file(&link);
        std::os::unix::fs::symlink(&here, &link).expect("a link to watch through");

        let mut w = RealWatcher::start(|| {});
        w.watch(&link);
        // Watched where it leads now, which is proved by a write landing
        // through it before anything is repointed.
        let deadline = Instant::now() + Duration::from_secs(20);
        let mut n = 0;
        while w.revision(&link) == 0 && Instant::now() < deadline {
            n += 1;
            std::fs::write(here.join(format!("{n}.txt")), b"hello").expect("a file");
            std::thread::sleep(Duration::from_millis(100));
        }
        assert!(w.revision(&link) > 0, "a write through the link lands");

        let before = w.revision(&link);
        std::fs::remove_file(&link).expect("the link goes");
        std::os::unix::fs::symlink(&there, &link).expect("and leads somewhere else");
        let deadline = Instant::now() + Duration::from_secs(20);
        while w.revision(&link) == before && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(100));
        }
        assert!(
            w.revision(&link) > before,
            "the path leading somewhere else is a change to it"
        );

        // And it is the new place that is watched now.
        let before = w.revision(&link);
        let deadline = Instant::now() + Duration::from_secs(20);
        let mut n = 0;
        while w.revision(&link) == before && Instant::now() < deadline {
            n += 1;
            std::fs::write(there.join(format!("{n}.txt")), b"hello").expect("a file");
            std::thread::sleep(Duration::from_millis(100));
        }
        assert!(
            w.revision(&link) > before,
            "a write where the link leads now lands too"
        );

        drop(w);
        let _ = std::fs::remove_file(&link);
        for dir in [here, there] {
            let _ = std::fs::remove_dir_all(dir);
        }
    }
}
