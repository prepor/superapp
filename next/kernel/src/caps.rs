//! The capabilities the kernel defines, and the in-memory effects that wrap
//! them.
//!
//! A capability is a trait an effect reaches through [`Ctx::cap`]. The kernel
//! owns the five every build needs — the clock, secrets, the clipboard, the
//! screen and the disk — because the harness, attachments and a file browser
//! all use them; an app defines its own and supplies them in `App::outside`.
//!
//! In the prototype only the fakes exist: the clipboard and the screen are
//! the shell's to install for real, and the disk is always the demo tree, so
//! no test can reach a human's files.

use std::collections::BTreeMap;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use crate::app::{Capabilities, Env, Mode};
use crate::effect::{Ctx, Effect};
use crate::time::ts;

// -- the clock -----------------------------------------------------------------

/// What time it is. Every world has one, even a [`Mode::Deny`] world: an
/// effect that asks for anything else fails, but a deadline still has to be
/// readable.
pub trait Clock {
    /// Unix seconds.
    fn now(&self) -> f64;
}

/// The wall clock.
pub struct SystemClock;

impl Clock for SystemClock {
    fn now(&self) -> f64 {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs_f64())
            .unwrap_or(0.0)
    }
}

/// A clock that only moves when it is moved.
///
/// Shared on purpose: each worker builds its own world, and a deadline
/// written on the UI thread is read on a worker's. If the two disagreed
/// about what time it is, a worker would claim a job the script still
/// thinks is held back. One handle, cloned into every world.
#[derive(Clone, Debug)]
pub struct FakeClock(Arc<Mutex<f64>>);

impl FakeClock {
    /// A clock standing at `start` (unix seconds).
    #[must_use]
    pub fn at(start: f64) -> FakeClock {
        FakeClock(Arc::new(Mutex::new(start)))
    }

    /// Moves it on.
    pub fn advance(&self, secs: f64) {
        if let Ok(mut g) = self.0.lock() {
            *g += secs;
        }
    }

    /// Puts it at an instant.
    pub fn set(&self, at: f64) {
        if let Ok(mut g) = self.0.lock() {
            *g = at;
        }
    }
}

impl Default for FakeClock {
    /// The virtual epoch: where a scripted run believes it is.
    fn default() -> FakeClock {
        FakeClock::at(crate::time::virtual_epoch())
    }
}

impl Clock for FakeClock {
    fn now(&self) -> f64 {
        self.0.lock().map(|g| *g).unwrap_or(0.0)
    }
}

/// Which clock a build runs on, as a handle the shell can hold and every
/// world can be given a copy of.
///
/// This is the trait's other half: [`Clock`] is what an effect asks, and
/// this is what a caller with no world — a frame loop advancing virtual
/// time — holds.
#[derive(Clone, Debug)]
pub enum ClockSource {
    System,
    Virtual(FakeClock),
}

impl ClockSource {
    /// A virtual clock starting at `start`.
    #[must_use]
    pub fn virtual_from(start: f64) -> ClockSource {
        ClockSource::Virtual(FakeClock::at(start))
    }

    /// Unix seconds. Public because a thread that has no world still needs
    /// the app's clock rather than the wall's.
    #[must_use]
    pub fn read(&self) -> f64 {
        match self {
            ClockSource::System => SystemClock.now(),
            ClockSource::Virtual(c) => c.now(),
        }
    }

    /// Moves a virtual clock on; the system clock ignores this.
    pub fn advance(&self, secs: f64) {
        if let ClockSource::Virtual(c) = self {
            c.advance(secs);
        }
    }

    /// Whether time only moves when it is moved — what the workers read to
    /// decide between threads and inline passes.
    #[must_use]
    pub fn is_virtual(&self) -> bool {
        matches!(self, ClockSource::Virtual(_))
    }

    /// This source as a capability for one world.
    #[must_use]
    pub fn capability(&self) -> Box<dyn Clock> {
        match self {
            ClockSource::System => Box::new(SystemClock),
            ClockSource::Virtual(c) => Box::new(c.clone()),
        }
    }
}

impl Default for ClockSource {
    fn default() -> ClockSource {
        ClockSource::Virtual(FakeClock::default())
    }
}

// -- secrets -------------------------------------------------------------------

/// Where a password lives. Never the store: a secret is the one thing that
/// must not replicate.
pub trait Secrets {
    /// The secret filed under `key`, or `None`.
    fn get(&mut self, key: &str) -> Option<String>;
    /// Files one. Answers whether the backend took it.
    fn set(&mut self, key: &str, secret: &str) -> bool;
}

/// Secrets held in memory and shared across threads.
///
/// It has to be *shared*, not merely in-memory: each worker builds its own
/// world, so a password written by the UI thread is read by a worker. A
/// plain map on the instance would be empty on the reader's side.
///
/// The prototype has no other backend: a scripted run must never write to a
/// human's keychain, and there is nothing here worth keeping past the
/// process.
#[derive(Clone, Default, Debug)]
pub struct MemSecrets(Arc<Mutex<HashMap<String, String>>>);

impl MemSecrets {
    #[must_use]
    pub fn new() -> MemSecrets {
        MemSecrets::default()
    }

    /// Plants one directly, as a settings form would.
    pub fn plant(&self, key: &str, secret: &str) {
        if let Ok(mut g) = self.0.lock() {
            g.insert(key.to_string(), secret.to_string());
        }
    }
}

impl Secrets for MemSecrets {
    fn get(&mut self, key: &str) -> Option<String> {
        self.0.lock().ok()?.get(key).cloned()
    }

    fn set(&mut self, key: &str, secret: &str) -> bool {
        match self.0.lock() {
            Ok(mut g) => {
                g.insert(key.to_string(), secret.to_string());
                true
            }
            Err(_) => false,
        }
    }
}

// -- the clipboard -------------------------------------------------------------

/// The system clipboard, write only — nothing here reads a person's.
pub trait Clipboard {
    /// # Errors
    ///
    /// If the system refused the text.
    fn put(&mut self, text: &str) -> Result<(), String>;
}

/// A clipboard that keeps what it was given, so a test can read it back.
/// Shared, like the secrets, because the copy may happen in a worker.
#[derive(Clone, Default, Debug)]
pub struct FakeClipboard(Arc<Mutex<Vec<String>>>);

impl FakeClipboard {
    #[must_use]
    pub fn new() -> FakeClipboard {
        FakeClipboard::default()
    }

    /// Everything copied, oldest first.
    #[must_use]
    pub fn taken(&self) -> Vec<String> {
        self.0.lock().map(|g| g.clone()).unwrap_or_default()
    }

    /// The last thing copied.
    #[must_use]
    pub fn last(&self) -> Option<String> {
        self.0.lock().ok()?.last().cloned()
    }
}

impl Clipboard for FakeClipboard {
    fn put(&mut self, text: &str) -> Result<(), String> {
        self.0
            .lock()
            .map_err(|_| "the clipboard is poisoned".to_string())?
            .push(text.to_string());
        Ok(())
    }
}

// -- the screen ----------------------------------------------------------------

/// Photographing the window — what the e2e harness's `shot` reaches for.
/// The real one is the shell's: only it knows what a frame is.
pub trait Screen {
    /// # Errors
    ///
    /// If there was nothing to photograph, or the file could not be written.
    fn shot(&mut self, path: &Path) -> Result<(), String>;
}

/// A screen that notes where a shot was asked for and takes none.
#[derive(Clone, Default, Debug)]
pub struct FakeScreen(Arc<Mutex<Vec<PathBuf>>>);

impl FakeScreen {
    #[must_use]
    pub fn new() -> FakeScreen {
        FakeScreen::default()
    }

    /// Every path a shot was asked at, oldest first.
    #[must_use]
    pub fn shots(&self) -> Vec<PathBuf> {
        self.0.lock().map(|g| g.clone()).unwrap_or_default()
    }
}

impl Screen for FakeScreen {
    fn shot(&mut self, path: &Path) -> Result<(), String> {
        self.0
            .lock()
            .map_err(|_| "the screen is poisoned".to_string())?
            .push(path.to_path_buf());
        Ok(())
    }
}

// -- files ---------------------------------------------------------------------

/// One entry of a directory, as a file list draws it.
#[derive(Debug, Clone, PartialEq)]
pub struct Entry {
    /// The name alone, no slash — the row adds one to a directory's.
    pub name: String,
    pub is_dir: bool,
    pub size: u64,
    pub modified: f64,
}

impl Entry {
    /// A dot-file: out of a listing unless the filter asks.
    #[must_use]
    pub fn hidden(&self) -> bool {
        self.name.starts_with('.')
    }

    #[must_use]
    pub fn kind(&self) -> FileKind {
        if self.is_dir {
            FileKind::Dir
        } else {
            FileKind::of_name(&self.name)
        }
    }

    /// What the row shows: a directory wears a trailing `/`.
    #[must_use]
    pub fn label(&self) -> String {
        if self.is_dir {
            format!("{}/", self.name)
        } else {
            self.name.clone()
        }
    }
}

/// What the **disk** calls an object, as opposed to what a path calls it:
/// the device and inode `lstat` reports. Two paths with this id are the
/// same object; a path whose id has changed wears the same name over a
/// different thing — which is the one question a reversal has to ask
/// before it takes something away, because a name is cheap to reuse and
/// undo is not a suggestion.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FileId {
    pub dev: u64,
    pub ino: u64,
}

/// What a file is, off its extension.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FileKind {
    Dir,
    Image,
    Text,
    Pdf,
    Archive,
    Other,
}

impl FileKind {
    #[must_use]
    pub fn of_name(name: &str) -> FileKind {
        let ext = name.rsplit_once('.').map(|(_, e)| e.to_ascii_lowercase());
        match ext.as_deref() {
            Some("png" | "jpg" | "jpeg" | "gif" | "webp" | "heic" | "svg") => FileKind::Image,
            Some(
                "txt" | "md" | "rs" | "toml" | "json" | "tla" | "cfg" | "log" | "csv" | "html"
                | "xml" | "yaml" | "yml" | "sh",
            ) => FileKind::Text,
            Some("pdf") => FileKind::Pdf,
            Some("zip" | "gz" | "tgz" | "tar" | "dmg" | "7z" | "rar" | "xz") => FileKind::Archive,
            _ => FileKind::Other,
        }
    }

    /// The card's word for it.
    #[must_use]
    pub fn word(self) -> &'static str {
        match self {
            FileKind::Dir => "directory",
            FileKind::Image => "image",
            FileKind::Text => "text",
            FileKind::Pdf => "pdf",
            FileKind::Archive => "archive",
            FileKind::Other => "file",
        }
    }

    /// The filter value that finds it.
    #[must_use]
    pub fn tag(self) -> &'static str {
        match self {
            FileKind::Dir => "dir",
            FileKind::Image => "image",
            FileKind::Text => "text",
            FileKind::Pdf => "pdf",
            FileKind::Archive => "archive",
            FileKind::Other => "other",
        }
    }
}

/// The root a file browser opens on.
pub const HOME: &str = "~";
/// The other root: the whole disk.
pub const ROOT: &str = "/";

/// `~/Downloads` + `2026` → `~/Downloads/2026`; `/` + `tmp` → `/tmp`.
#[must_use]
pub fn join(dir: &str, name: &str) -> String {
    if dir.is_empty() {
        name.to_string()
    } else if dir == ROOT {
        format!("/{name}")
    } else {
        format!("{dir}/{name}")
    }
}

/// The directory a path sits in; `None` at a root (`~`, `/`).
#[must_use]
pub fn parent(path: &str) -> Option<&str> {
    if path == ROOT {
        return None;
    }
    path.rsplit_once('/')
        .map(|(p, _)| if p.is_empty() { ROOT } else { p })
}

/// One of the two roots. No verb takes a root away.
#[must_use]
pub fn is_root(path: &str) -> bool {
    path == HOME || path == ROOT
}

/// The last segment: a panel's title. A root is its own name.
#[must_use]
pub fn basename(path: &str) -> &str {
    if path == ROOT {
        return ROOT;
    }
    path.rsplit_once('/').map_or(path, |(_, n)| n)
}

/// Directories first, then names, case folded — the one order a listing
/// has, whichever disk produced it.
pub fn sort(entries: &mut [Entry]) {
    entries.sort_by(|a, b| {
        b.is_dir
            .cmp(&a.is_dir)
            .then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase()))
    });
}

/// Where home is on this machine, for the two spellings to meet.
fn home_dir() -> PathBuf {
    std::env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("/"))
}

/// The display spelling as the path a disk reads: `~` is home.
#[must_use]
pub fn real_path(display: &str) -> PathBuf {
    match display.strip_prefix('~') {
        Some(rest) => {
            let mut p = home_dir();
            for seg in rest.split('/').filter(|s| !s.is_empty()) {
                p.push(seg);
            }
            p
        }
        None => PathBuf::from(display),
    }
}

/// A real path as the panels spell it: home and below as `~/…`.
#[must_use]
pub fn display_path(path: &Path) -> String {
    let home = home_dir();
    match path.strip_prefix(&home) {
        Ok(rest) if rest.as_os_str().is_empty() => HOME.to_string(),
        Ok(rest) => format!("~/{}", rest.to_string_lossy()),
        Err(_) => path.to_string_lossy().into_owned(),
    }
}

/// The disk, read and written.
///
/// None of the writing verbs is `rm`: what a delete takes goes to the
/// trash, and undo moves it back out — so a backend that implements them
/// can never make a path unrecoverable.
pub trait Disk {
    /// One directory's entries, in the browser's order.
    ///
    /// # Errors
    ///
    /// If there is no such directory.
    fn list_dir(&mut self, dir: &Path) -> Result<Vec<Entry>, String>;

    /// One path's entry, `None` when there is nothing there.
    ///
    /// # Errors
    ///
    /// If the disk could not be asked.
    fn stat(&mut self, path: &Path) -> Result<Option<Entry>, String>;

    /// The first `max` bytes of a file.
    ///
    /// # Errors
    ///
    /// If there is no such file.
    fn read_file(&mut self, path: &Path, max: usize) -> Result<Vec<u8>, String>;

    /// A file written outside the store.
    ///
    /// # Errors
    ///
    /// If the write fails.
    fn write_file(&mut self, path: &Path, bytes: &[u8]) -> Result<(), String>;

    /// Hand a path to the OS — whatever opens that kind of file. Nothing is
    /// executed by us.
    ///
    /// # Errors
    ///
    /// If the OS refused it.
    fn open_path(&mut self, path: &Path) -> Result<(), String>;

    /// One directory, where nothing is yet. Refuses a taken name rather
    /// than adopting whatever is there.
    ///
    /// # Errors
    ///
    /// If something is already there, or its parent is not a directory.
    fn make_dir(&mut self, path: &Path) -> Result<(), String>;

    /// A file, or a directory with everything under it, copied. Refuses a
    /// taken destination — a copy never writes over anything.
    ///
    /// # Errors
    ///
    /// If the source is gone or the destination is taken.
    fn copy_path(&mut self, from: &Path, to: &Path) -> Result<(), String>;

    /// A path moved, and the same verb undo puts one back with. Refuses a
    /// taken destination, for the same reason.
    ///
    /// # Errors
    ///
    /// As [`Disk::copy_path`].
    fn move_path(&mut self, from: &Path, to: &Path) -> Result<(), String>;

    /// To the trash, answering where it landed — the trash picks the name,
    /// and undo needs the one it picked.
    ///
    /// # Errors
    ///
    /// If the path is a root, or the trash cannot be made.
    fn trash(&mut self, path: &Path) -> Result<PathBuf, String>;

    /// What the disk calls the object at this path, as opposed to what the
    /// path calls it — `None` where there is nothing. Never follows a link:
    /// the question is about the object the name is bound to.
    ///
    /// # Errors
    ///
    /// If the disk could not be asked.
    fn file_id(&mut self, path: &Path) -> Result<Option<FileId>, String>;
}

/// The demo tree as a disk: what every world in the prototype gets.
///
/// The one translation each of its verbs needs is the spelling: a [`Disk`]
/// is handed real paths, and the fixture is keyed by what the panels show.
/// Its own copy of the tree, which is what lets a verb that writes run in
/// any number of tests at once.
#[derive(Debug, Clone, Default)]
pub struct DemoDisk {
    pub tree: demo::Tree,
    /// What was written here, if anything was — a demo disk is writable,
    /// and a card reads back what a save just put down.
    pub written: BTreeMap<PathBuf, Vec<u8>>,
    /// What `open` handed to the OS.
    pub opened: Vec<PathBuf>,
    /// The clock the tree stamps a new directory with. Its own handle, so
    /// a `new dir` under virtual time is dated by the script.
    pub clock: ClockSource,
}

impl DemoDisk {
    #[must_use]
    pub fn new(clock: ClockSource) -> DemoDisk {
        DemoDisk {
            tree: demo::Tree::new(),
            written: BTreeMap::new(),
            opened: Vec::new(),
            clock,
        }
    }
}

fn shown(path: &Path) -> String {
    display_path(path)
}

impl Disk for DemoDisk {
    fn list_dir(&mut self, dir: &Path) -> Result<Vec<Entry>, String> {
        self.tree.list(&shown(dir))
    }

    fn stat(&mut self, path: &Path) -> Result<Option<Entry>, String> {
        Ok(self.tree.entry(&shown(path)))
    }

    fn read_file(&mut self, path: &Path, max: usize) -> Result<Vec<u8>, String> {
        match self.written.get(path) {
            Some(b) => Ok(b.iter().take(max).copied().collect()),
            None => self.tree.read(&shown(path), max),
        }
    }

    fn write_file(&mut self, path: &Path, bytes: &[u8]) -> Result<(), String> {
        self.written.insert(path.to_path_buf(), bytes.to_vec());
        Ok(())
    }

    fn open_path(&mut self, path: &Path) -> Result<(), String> {
        self.opened.push(path.to_path_buf());
        Ok(())
    }

    fn make_dir(&mut self, path: &Path) -> Result<(), String> {
        let now = self.clock.read();
        self.tree.make_dir(&shown(path), now)
    }

    fn copy_path(&mut self, from: &Path, to: &Path) -> Result<(), String> {
        self.tree.copy(&shown(from), &shown(to))
    }

    fn move_path(&mut self, from: &Path, to: &Path) -> Result<(), String> {
        self.tree.mv(&shown(from), &shown(to))
    }

    fn trash(&mut self, path: &Path) -> Result<PathBuf, String> {
        let now = self.clock.read();
        self.tree.trash(&shown(path), now).map(|p| real_path(&p))
    }

    fn file_id(&mut self, path: &Path) -> Result<Option<FileId>, String> {
        Ok(self.tree.id(&shown(path)))
    }
}

/// The fixture: a machine-independent `~` a suite can address a row of by
/// name. Written as well as read, so the verbs act on it exactly as they
/// act on a filesystem and a suite proves them rather than a draft of them.
pub mod demo {
    use std::collections::BTreeMap;

    use super::{basename, is_root, join, parent, sort, Entry, FileId, FileKind, HOME, ROOT};
    use crate::time::ts;

    struct Fx {
        path: &'static str,
        dir: bool,
        size: u64,
        /// `(year, month, day, hour, minute)`.
        at: (i64, u32, u32, u32, u32),
    }

    const KB: u64 = 1024;
    const MB: u64 = 1024 * 1024;

    const TREE: &[Fx] = &[
        Fx {
            path: "~/Desktop",
            dir: true,
            size: 0,
            at: (2026, 8, 31, 18, 40),
        },
        Fx {
            path: "~/Documents",
            dir: true,
            size: 0,
            at: (2026, 8, 29, 11, 5),
        },
        Fx {
            path: "~/Downloads",
            dir: true,
            size: 0,
            at: (2026, 9, 1, 9, 12),
        },
        Fx {
            path: "~/Pictures",
            dir: true,
            size: 0,
            at: (2026, 8, 24, 20, 3),
        },
        Fx {
            path: "~/superapp",
            dir: true,
            size: 0,
            at: (2026, 9, 2, 7, 30),
        },
        Fx {
            path: "~/.config",
            dir: true,
            size: 0,
            at: (2026, 7, 14, 10, 0),
        },
        Fx {
            path: "~/notes.md",
            dir: false,
            size: 2 * KB + 130,
            at: (2026, 8, 30, 22, 47),
        },
        Fx {
            path: "~/.zshrc",
            dir: false,
            size: KB + 90,
            at: (2026, 6, 2, 9, 0),
        },
        Fx {
            path: "~/Desktop/todo.txt",
            dir: false,
            size: 300,
            at: (2026, 8, 31, 18, 40),
        },
        Fx {
            path: "~/Documents/panel-model.md",
            dir: false,
            size: 9 * KB,
            at: (2026, 8, 29, 11, 5),
        },
        Fx {
            path: "~/Documents/interaction-grammar.md",
            dir: false,
            size: 14 * KB,
            at: (2026, 9, 2, 6, 55),
        },
        Fx {
            path: "~/Documents/Lease.tla",
            dir: false,
            size: 5 * KB,
            at: (2026, 8, 28, 15, 20),
        },
        Fx {
            path: "~/Downloads/2026",
            dir: true,
            size: 0,
            at: (2026, 8, 17, 12, 0),
        },
        Fx {
            path: "~/Downloads/report-q3.pdf",
            dir: false,
            size: MB + 200 * KB,
            at: (2026, 8, 31, 9, 14),
        },
        Fx {
            path: "~/Downloads/budget-2026.xlsx",
            dir: false,
            size: 84 * KB,
            at: (2026, 8, 31, 9, 14),
        },
        Fx {
            path: "~/Downloads/screenshot-2026-08-30.png",
            dir: false,
            size: 412 * KB,
            at: (2026, 8, 30, 14, 2),
        },
        Fx {
            path: "~/Downloads/superapp-0.1.0.dmg",
            dir: false,
            size: 38 * MB,
            at: (2026, 9, 1, 9, 12),
        },
        Fx {
            path: "~/Downloads/logs.tar.gz",
            dir: false,
            size: 3 * MB + 400 * KB,
            at: (2026, 8, 30, 7, 30),
        },
        Fx {
            path: "~/Downloads/README.txt",
            dir: false,
            size: 640,
            at: (2026, 8, 12, 16, 45),
        },
        Fx {
            path: "~/Downloads/.DS_Store",
            dir: false,
            size: 6 * KB,
            at: (2026, 9, 1, 9, 12),
        },
        Fx {
            path: "~/Downloads/2026/invoice-0817.pdf",
            dir: false,
            size: 96 * KB,
            at: (2026, 8, 17, 12, 0),
        },
        Fx {
            path: "~/Downloads/2026/photo-lisbon.jpg",
            dir: false,
            size: 2 * MB + 800 * KB,
            at: (2026, 8, 3, 19, 21),
        },
        Fx {
            path: "~/Downloads/2026/notes.txt",
            dir: false,
            size: KB + 100,
            at: (2026, 8, 17, 12, 0),
        },
        Fx {
            path: "~/Pictures/lisbon",
            dir: true,
            size: 0,
            at: (2026, 8, 3, 19, 21),
        },
        Fx {
            path: "~/Pictures/fold-cover.png",
            dir: false,
            size: MB + 100 * KB,
            at: (2026, 8, 24, 20, 3),
        },
        Fx {
            path: "~/Pictures/lisbon/IMG_0417.jpg",
            dir: false,
            size: 3 * MB + 200 * KB,
            at: (2026, 8, 3, 19, 21),
        },
        Fx {
            path: "~/Pictures/lisbon/IMG_0418.jpg",
            dir: false,
            size: 3 * MB,
            at: (2026, 8, 3, 19, 24),
        },
        Fx {
            path: "~/superapp/files",
            dir: true,
            size: 0,
            at: (2026, 9, 2, 7, 30),
        },
        Fx {
            path: "~/superapp/superapp.db",
            dir: false,
            size: 24 * MB,
            at: (2026, 9, 2, 7, 30),
        },
        Fx {
            path: "~/superapp/panel-context.md",
            dir: false,
            size: 3 * KB,
            at: (2026, 9, 1, 23, 8),
        },
        // Beyond home: what a typed path reaches.
        Fx {
            path: "/Applications",
            dir: true,
            size: 0,
            at: (2026, 8, 20, 10, 0),
        },
        Fx {
            path: "/Users",
            dir: true,
            size: 0,
            at: (2026, 6, 1, 9, 0),
        },
        Fx {
            path: "/Users/andrey",
            dir: true,
            size: 0,
            at: (2026, 9, 2, 7, 30),
        },
        Fx {
            path: "/etc",
            dir: true,
            size: 0,
            at: (2026, 7, 14, 10, 0),
        },
        Fx {
            path: "/etc/hosts",
            dir: false,
            size: 213,
            at: (2026, 7, 14, 10, 0),
        },
        Fx {
            path: "/tmp",
            dir: true,
            size: 0,
            at: (2026, 9, 2, 12, 40),
        },
        Fx {
            path: "/tmp/superapp-e2e",
            dir: true,
            size: 0,
            at: (2026, 9, 2, 12, 40),
        },
        Fx {
            path: "/tmp/superapp-e2e/frames",
            dir: true,
            size: 0,
            at: (2026, 9, 2, 12, 41),
        },
        Fx {
            path: "/tmp/superapp-e2e/superapp.db",
            dir: false,
            size: 2 * MB,
            at: (2026, 9, 2, 12, 40),
        },
        Fx {
            path: "/tmp/notes.txt",
            dir: false,
            size: 380,
            at: (2026, 9, 1, 18, 5),
        },
        Fx {
            path: "/tmp/.keep",
            dir: false,
            size: 0,
            at: (2026, 9, 1, 18, 5),
        },
    ];

    /// The fixture materialised into one map, so `new dir`, a copy, a move
    /// and the trash act on it exactly as they act on a real disk.
    #[derive(Debug, Clone)]
    pub struct Tree {
        /// Display path → what is there. The two roots are in here as
        /// directories, so nothing special-cases them.
        nodes: BTreeMap<String, Node>,
        /// The next object number. A fixture needs identity for the same
        /// reason a disk does — a reversal asks whether the thing at a path
        /// is the thing it put there — so a node gets one when it is
        /// **made**: a move carries it (a rename keeps the inode) and a copy
        /// takes a fresh one (a copy is another object).
        next: u64,
    }

    #[derive(Debug, Clone)]
    struct Node {
        entry: Entry,
        bytes: Vec<u8>,
        id: u64,
    }

    impl Default for Tree {
        fn default() -> Tree {
            Tree::new()
        }
    }

    impl Tree {
        /// The fixture, as it stands before anything has written to it.
        #[must_use]
        pub fn new() -> Tree {
            let mut d = Tree {
                nodes: BTreeMap::new(),
                next: 1,
            };
            for root in [HOME, ROOT] {
                if let Some(e) = fixture_entry(root) {
                    d.put(root, e, Vec::new());
                }
            }
            for fx in TREE {
                d.put(fx.path, entry_of(fx), bytes_of(fx.path).unwrap_or_default());
            }
            d
        }

        /// A node with a fresh object number — what making something does.
        fn put(&mut self, path: &str, entry: Entry, bytes: Vec<u8>) {
            let id = self.next;
            self.next += 1;
            self.nodes
                .insert(path.to_string(), Node { entry, bytes, id });
        }

        fn dir_at(&self, path: &str) -> bool {
            self.nodes.get(path).is_some_and(|n| n.entry.is_dir)
        }

        /// The entry at a path, `None` for what the tree does not have.
        #[must_use]
        pub fn entry(&self, path: &str) -> Option<Entry> {
            self.nodes.get(path).map(|n| n.entry.clone())
        }

        /// The object at a path, as this tree numbers them — the fixture's
        /// answer to `lstat`'s device and inode.
        #[must_use]
        pub fn id(&self, path: &str) -> Option<FileId> {
            self.nodes.get(path).map(|n| FileId { dev: 1, ino: n.id })
        }

        /// One directory's listing, in the browser's order.
        ///
        /// # Errors
        ///
        /// If there is no such directory.
        pub fn list(&self, dir: &str) -> Result<Vec<Entry>, String> {
            if !self.dir_at(dir) {
                return Err(format!("{dir}: no such directory"));
            }
            let mut v: Vec<Entry> = self
                .nodes
                .iter()
                .filter(|(p, _)| parent(p) == Some(dir))
                .map(|(_, n)| n.entry.clone())
                .collect();
            sort(&mut v);
            Ok(v)
        }

        /// The first `max` bytes of a file.
        ///
        /// # Errors
        ///
        /// If there is no such file.
        pub fn read(&self, path: &str, max: usize) -> Result<Vec<u8>, String> {
            let node = self
                .nodes
                .get(path)
                .ok_or_else(|| format!("{path}: no such file"))?;
            let mut out = node.bytes.clone();
            out.truncate(max);
            Ok(out)
        }

        /// A path and everything under it — what a copy, a move and the
        /// trash carry as one.
        fn subtree(&self, path: &str) -> Vec<String> {
            let under = format!("{path}/");
            self.nodes
                .keys()
                .filter(|p| p.as_str() == path || p.starts_with(&under))
                .cloned()
                .collect()
        }

        /// What every verb that writes asks of a destination first: it is
        /// not a root, nothing is there, and its directory is.
        fn free(&self, path: &str) -> Result<(), String> {
            if is_root(path) {
                return Err(format!("{path} is a root"));
            }
            if self.nodes.contains_key(path) {
                return Err(format!("{path} is already there"));
            }
            match parent(path) {
                Some(d) if self.dir_at(d) => Ok(()),
                Some(d) => Err(format!("{d}: no such directory")),
                None => Err(format!("{path} is a root")),
            }
        }

        /// One directory, where nothing is yet.
        ///
        /// # Errors
        ///
        /// If something is already there, or its parent is not a directory.
        pub fn make_dir(&mut self, path: &str, now: f64) -> Result<(), String> {
            self.free(path)?;
            let e = Entry {
                name: basename(path).to_string(),
                is_dir: true,
                size: 0,
                modified: now,
            };
            self.put(path, e, Vec::new());
            Ok(())
        }

        /// A file, or a directory with everything under it. The times come
        /// along: a copy of a file is that file.
        ///
        /// # Errors
        ///
        /// If the source is gone, the destination is taken, or a directory
        /// is asked to copy into itself.
        pub fn copy(&mut self, from: &str, to: &str) -> Result<(), String> {
            if is_root(from) {
                return Err(format!("{from} is a root"));
            }
            if !self.nodes.contains_key(from) {
                return Err(format!("{from}: no such path"));
            }
            self.free(to)?;
            if to.starts_with(&format!("{from}/")) {
                return Err(format!("{from} is inside itself"));
            }
            for p in self.subtree(from) {
                let node = self.nodes[&p].clone();
                let dest = format!("{to}{}", &p[from.len()..]);
                let mut e = node.entry;
                e.name = basename(&dest).to_string();
                // A fresh number each: a copy is another object, however
                // alike its bytes.
                self.put(&dest, e, node.bytes);
            }
            Ok(())
        }

        /// The copy, and then the source is gone.
        ///
        /// # Errors
        ///
        /// As [`Tree::copy`].
        pub fn mv(&mut self, from: &str, to: &str) -> Result<(), String> {
            self.copy(from, to)?;
            // The objects go with the names: a rename keeps the inode, so
            // the moved nodes carry the numbers they had rather than the
            // fresh ones the copy just minted.
            for p in self.subtree(from) {
                let Some(old) = self.nodes.remove(&p) else {
                    continue;
                };
                let dest = format!("{to}{}", &p[from.len()..]);
                if let Some(n) = self.nodes.get_mut(&dest) {
                    n.id = old.id;
                }
            }
            Ok(())
        }

        /// The trash: where a delete puts a path, answering where it landed
        /// so undo can move it back. `~/.Trash`, made if it is not there,
        /// and a name that does not clash — the real one's shape.
        ///
        /// # Errors
        ///
        /// If the path is a root, or the trash cannot be made.
        pub fn trash(&mut self, path: &str, now: f64) -> Result<String, String> {
            let dir = join(HOME, ".Trash");
            if !self.dir_at(&dir) {
                self.make_dir(&dir, now)?;
            }
            let name = basename(path);
            let mut to = join(&dir, name);
            let mut n = 1;
            while self.nodes.contains_key(&to) {
                n += 1;
                to = join(&dir, &format!("{name} {n}"));
            }
            self.mv(path, &to)?;
            Ok(to)
        }
    }

    fn entry_of(fx: &Fx) -> Entry {
        let (y, mo, d, h, min) = fx.at;
        Entry {
            name: basename(fx.path).to_string(),
            is_dir: fx.dir,
            size: fx.size,
            modified: ts(y, mo, d, h, min),
        }
    }

    /// Whether the fixture has this path — a directory or a file.
    #[must_use]
    pub fn exists(path: &str) -> bool {
        is_root(path) || TREE.iter().any(|f| f.path == path)
    }

    /// Whether the fixture has this path as a directory.
    #[must_use]
    pub fn is_dir(path: &str) -> bool {
        is_root(path) || TREE.iter().any(|f| f.path == path && f.dir)
    }

    /// The entry at a path, if the fixture has it; a root is a directory.
    #[must_use]
    pub fn fixture_entry(path: &str) -> Option<Entry> {
        if is_root(path) {
            return Some(Entry {
                name: path.to_string(),
                is_dir: true,
                size: 0,
                modified: ts(2026, 9, 2, 7, 30),
            });
        }
        TREE.iter().find(|f| f.path == path).map(entry_of)
    }

    /// A text file's reading, for a card's preview.
    #[must_use]
    pub fn text_of(path: &str) -> Option<String> {
        let e = fixture_entry(path)?;
        if e.kind() != FileKind::Text {
            return None;
        }
        Some(match e.name.as_str() {
            "hosts" => {
                "127.0.0.1\tlocalhost\n255.255.255.255\tbroadcasthost\n::1\tlocalhost".into()
            }
            "README.txt" => "superapp 0.1.0\n\nA personal user-space OS: one workspace, specialized panels, no windows.\n\nDrag the .app to Applications. First launch asks for nothing; add a mail account in settings.".into(),
            "todo.txt" => "- files: the card previews\n- files: move here / copy here\n- attachments: save a part where I choose\n- rename?".into(),
            "notes.txt" => "Lisbon, August.\n\nInvoice 0817 is for the flat; the photos are from the last evening.".into(),
            "notes.md" => "# notes\n\n- a directory is a list panel\n- a file is a card\n- enter goes, the cursor previews\n\nThe join is the only relation.".into(),
            "panel-context.md" => "# panel: files ~/Downloads\n\nfilter: @kind:image\nentries: 8 (1 shown)\nlisted: 0.4 s ago".into(),
            _ => format!("{}\n\n(the first 64 KB of the file, in the app's one face)", e.name),
        })
    }

    /// A file's bytes. Text files carry their reading; everything else is
    /// empty — the prototype draws no pictures, and a fixture that shipped
    /// megabytes to prove it could would be a fixture about nothing.
    #[must_use]
    pub fn bytes_of(path: &str) -> Option<Vec<u8>> {
        let e = fixture_entry(path)?;
        match e.kind() {
            FileKind::Text => text_of(path).map(String::into_bytes),
            _ => Some(Vec::new()),
        }
    }
}

// -- the capabilities of one world ---------------------------------------------

/// The capabilities the kernel itself supplies, installed before the apps'
/// so an app — or the shell — may replace one.
///
/// A [`Mode::Deny`] world gets the clock and nothing else: an effect that
/// asks for anything more fails with *this world has no …*, which is what a
/// library mount wants. The prototype's disk is the demo tree in every mode,
/// so no run can reach a human's files.
pub fn install(mode: Mode, env: &Env, caps: &mut Capabilities) {
    caps.insert::<dyn Clock>(env.clock.capability());
    if mode == Mode::Deny {
        return;
    }
    caps.insert::<dyn Secrets>(Box::new(env.secrets.clone()));
    caps.insert::<dyn Clipboard>(Box::new(FakeClipboard::new()));
    caps.insert::<dyn Screen>(Box::new(FakeScreen::new()));
    caps.insert::<dyn Disk>(Box::new(DemoDisk::new(env.clock.clone())));
}

// -- the in-memory effects that wrap them --------------------------------------
//
// Not `Deferred`: nobody retries a clipboard write or waits on a row for the
// time. They exist so that *everything* leaving the process goes through one
// door, and so a `Deny` world can refuse them.

/// What time it is.
pub struct Now;

impl Effect for Now {
    const KIND: &'static str = "now";
    type Reply = f64;
    fn describe(&self) -> String {
        "read the clock".into()
    }
    fn writes(&self) -> bool {
        false
    }
    fn perform(&self, cx: &mut Ctx<'_>) -> Result<f64, String> {
        Ok(cx.cap::<dyn Clock>()?.now())
    }
}

/// Recall a secret.
pub struct SecretGet<'a>(pub &'a str);

impl Effect for SecretGet<'_> {
    const KIND: &'static str = "secret_get";
    type Reply = Option<String>;
    fn describe(&self) -> String {
        format!("read the secret for {}", self.0)
    }
    fn writes(&self) -> bool {
        false
    }
    fn perform(&self, cx: &mut Ctx<'_>) -> Result<Self::Reply, String> {
        Ok(cx.cap::<dyn Secrets>()?.get(self.0))
    }
}

/// Store a secret. Never persisted, for the obvious reason.
pub struct SecretSet<'a> {
    pub key: &'a str,
    pub secret: &'a str,
}

impl Effect for SecretSet<'_> {
    const KIND: &'static str = "secret_set";
    type Reply = ();
    fn describe(&self) -> String {
        format!("store the secret for {}", self.key)
    }
    fn writes(&self) -> bool {
        true
    }
    fn perform(&self, cx: &mut Ctx<'_>) -> Result<(), String> {
        cx.cap::<dyn Secrets>()?
            .set(self.key, self.secret)
            .then_some(())
            .ok_or_else(|| "the keychain refused the secret".to_string())
    }
}

/// Put text on the system clipboard.
pub struct Clip<'a> {
    pub text: &'a str,
    /// What the text is, for the description — the text itself may be long.
    pub what: &'static str,
}

impl Effect for Clip<'_> {
    const KIND: &'static str = "clip";
    type Reply = ();
    fn describe(&self) -> String {
        format!("copy {} ({} bytes)", self.what, self.text.len())
    }
    fn writes(&self) -> bool {
        true
    }
    fn perform(&self, cx: &mut Ctx<'_>) -> Result<(), String> {
        cx.cap::<dyn Clipboard>()?.put(self.text)
    }
}

/// Hand a path to the OS: whatever opens that kind of file.
pub struct OpenPath<'a> {
    pub path: &'a Path,
}

impl Effect for OpenPath<'_> {
    const KIND: &'static str = "open";
    type Reply = ();
    fn describe(&self) -> String {
        format!("open {}", self.path.display())
    }
    /// Nothing of ours changes; something out there starts, which is more
    /// than a question.
    fn writes(&self) -> bool {
        true
    }
    fn perform(&self, cx: &mut Ctx<'_>) -> Result<(), String> {
        cx.cap::<dyn Disk>()?.open_path(self.path)
    }
}

/// Make one directory — a file list's `new dir`.
pub struct MakeDir<'a> {
    pub path: &'a Path,
}

impl Effect for MakeDir<'_> {
    const KIND: &'static str = "make_dir";
    type Reply = ();
    fn describe(&self) -> String {
        format!("make the directory {}", self.path.display())
    }
    fn writes(&self) -> bool {
        true
    }
    fn perform(&self, cx: &mut Ctx<'_>) -> Result<(), String> {
        cx.cap::<dyn Disk>()?.make_dir(self.path)
    }
}

/// `copy here`, one path of it.
pub struct CopyPath<'a> {
    pub from: &'a Path,
    pub to: &'a Path,
}

impl Effect for CopyPath<'_> {
    const KIND: &'static str = "copy_path";
    type Reply = ();
    fn describe(&self) -> String {
        format!("copy {} to {}", self.from.display(), self.to.display())
    }
    fn writes(&self) -> bool {
        true
    }
    fn perform(&self, cx: &mut Ctx<'_>) -> Result<(), String> {
        cx.cap::<dyn Disk>()?.copy_path(self.from, self.to)
    }
}

/// `move here`, one path of it — and what undo reverses every one of these
/// verbs with.
pub struct MovePath<'a> {
    pub from: &'a Path,
    pub to: &'a Path,
}

impl Effect for MovePath<'_> {
    const KIND: &'static str = "move_path";
    type Reply = ();
    fn describe(&self) -> String {
        format!("move {} to {}", self.from.display(), self.to.display())
    }
    fn writes(&self) -> bool {
        true
    }
    fn perform(&self, cx: &mut Ctx<'_>) -> Result<(), String> {
        cx.cap::<dyn Disk>()?.move_path(self.from, self.to)
    }
}

/// `delete`: to the trash, never `rm`, answering where it landed so undo
/// can take it back out.
pub struct Trash<'a> {
    pub path: &'a Path,
}

impl Effect for Trash<'_> {
    const KIND: &'static str = "trash";
    type Reply = PathBuf;
    fn describe(&self) -> String {
        format!("move {} to the trash", self.path.display())
    }
    fn writes(&self) -> bool {
        true
    }
    fn perform(&self, cx: &mut Ctx<'_>) -> Result<PathBuf, String> {
        cx.cap::<dyn Disk>()?.trash(self.path)
    }
}

/// Write a file outside the store.
pub struct WriteFile<'a> {
    pub path: &'a Path,
    pub bytes: &'a [u8],
}

impl Effect for WriteFile<'_> {
    const KIND: &'static str = "write_file";
    type Reply = ();
    fn describe(&self) -> String {
        format!("write {} ({} bytes)", self.path.display(), self.bytes.len())
    }
    fn writes(&self) -> bool {
        true
    }
    fn perform(&self, cx: &mut Ctx<'_>) -> Result<(), String> {
        cx.cap::<dyn Disk>()?.write_file(self.path, self.bytes)
    }
}

/// Photograph the window (e2e).
pub struct Shot<'a>(pub &'a Path);

impl Effect for Shot<'_> {
    const KIND: &'static str = "shot";
    type Reply = ();
    fn describe(&self) -> String {
        format!("capture {}", self.0.display())
    }
    fn writes(&self) -> bool {
        true
    }
    fn perform(&self, cx: &mut Ctx<'_>) -> Result<(), String> {
        cx.cap::<dyn Screen>()?.shot(self.0)
    }
}

/// One day, as the fixture dates things — kept public so a suite can place
/// a file against the same calendar the tree uses.
#[must_use]
pub fn demo_day(y: i64, mo: u32, d: u32) -> f64 {
    ts(y, mo, d, 0, 0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::Capabilities;

    fn disk() -> DemoDisk {
        DemoDisk::new(ClockSource::default())
    }

    #[test]
    fn the_fake_clock_only_moves_when_moved() {
        let c = FakeClock::at(100.0);
        assert_eq!(c.now(), 100.0);
        // Every world holds a clone of the one handle.
        let other = c.clone();
        c.advance(5.0);
        assert_eq!(other.now(), 105.0);
        other.set(7.0);
        assert_eq!(c.now(), 7.0);

        let src = ClockSource::virtual_from(3.0);
        assert!(src.is_virtual());
        src.advance(2.0);
        assert_eq!(src.read(), 5.0);
        assert!(!ClockSource::System.is_virtual());
        assert!(
            ClockSource::System.read() > 1.7e9,
            "the wall clock moved on"
        );
    }

    #[test]
    fn secrets_are_shared_between_worlds() {
        let s = MemSecrets::new();
        let mut a = s.clone();
        let mut b = s.clone();
        assert_eq!(a.get("v@k.io"), None);
        assert!(a.set("v@k.io", "hunter2"));
        assert_eq!(b.get("v@k.io"), Some("hunter2".into()));
        s.plant("other", "x");
        assert_eq!(b.get("other"), Some("x".into()));
    }

    #[test]
    fn the_fake_clipboard_and_screen_record() {
        let c = FakeClipboard::new();
        let mut w = c.clone();
        w.put("one").unwrap();
        w.put("two").unwrap();
        assert_eq!(c.taken(), vec!["one".to_string(), "two".to_string()]);
        assert_eq!(c.last(), Some("two".into()));

        let s = FakeScreen::new();
        let mut w = s.clone();
        w.shot(Path::new("/tmp/a.png")).unwrap();
        assert_eq!(s.shots(), vec![PathBuf::from("/tmp/a.png")]);
    }

    #[test]
    fn the_demo_tree_lists_reads_and_writes() {
        let mut d = disk();
        let downloads = real_path("~/Downloads");
        let names: Vec<String> = d
            .list_dir(&downloads)
            .unwrap()
            .into_iter()
            .map(|e| e.label())
            .collect();
        assert_eq!(names[0], "2026/", "directories first");
        assert!(names.contains(&".DS_Store".to_string()), "hidden ones too");

        // Reading a text file gives its reading; a listing of nothing errs.
        let notes = real_path("~/notes.md");
        let text = String::from_utf8(d.read_file(&notes, 1 << 20).unwrap()).unwrap();
        assert!(text.starts_with("# notes"));
        assert!(d.list_dir(Path::new("/nowhere")).is_err());

        // The write verbs act on the fixture.
        let fresh = real_path("~/Downloads/fresh");
        d.make_dir(&fresh).unwrap();
        assert!(d.stat(&fresh).unwrap().is_some_and(|e| e.is_dir));
        assert!(d.make_dir(&fresh).is_err(), "a taken name is refused");

        let id_before = d.file_id(&notes).unwrap().expect("an object");
        let moved = real_path("~/Downloads/fresh/notes.md");
        d.move_path(&notes, &moved).unwrap();
        assert_eq!(
            d.file_id(&moved).unwrap(),
            Some(id_before),
            "a rename keeps the object"
        );
        assert_eq!(d.stat(&notes).unwrap(), None);

        let copied = real_path("~/Downloads/fresh/notes-2.md");
        d.copy_path(&moved, &copied).unwrap();
        assert_ne!(
            d.file_id(&copied).unwrap(),
            Some(id_before),
            "a copy is another object"
        );

        let landed = d.trash(&copied).unwrap();
        assert_eq!(display_path(&landed), "~/.Trash/notes-2.md");
        assert_eq!(d.stat(&copied).unwrap(), None);
        // A second delete of the same name does not clash.
        d.copy_path(&moved, &copied).unwrap();
        let again = d.trash(&copied).unwrap();
        assert_eq!(display_path(&again), "~/.Trash/notes-2.md 2");

        // No verb takes a root.
        assert!(d.trash(&real_path(HOME)).is_err());
        assert!(d.copy_path(&real_path(HOME), &real_path("~/x")).is_err());

        // What was written here reads back, fixture or no fixture.
        let scratch = PathBuf::from("/tmp/scratch.txt");
        d.write_file(&scratch, b"hello").unwrap();
        assert_eq!(d.read_file(&scratch, 3).unwrap(), b"hel");
        d.open_path(&scratch).unwrap();
        assert_eq!(d.opened, vec![scratch]);
    }

    #[test]
    fn a_directory_copied_carries_everything_under_it() {
        let mut d = disk();
        let from = real_path("~/Downloads/2026");
        let to = real_path("~/Desktop/2026");
        d.copy_path(&from, &to).unwrap();
        assert_eq!(d.list_dir(&to).unwrap().len(), 3);
        assert_eq!(d.list_dir(&from).unwrap().len(), 3, "the source stayed");
        // Into itself is refused before anything is written.
        assert!(d
            .copy_path(&from, &real_path("~/Downloads/2026/again"))
            .is_err());
    }

    #[test]
    fn the_path_spellings_meet() {
        assert_eq!(join("~/Downloads", "2026"), "~/Downloads/2026");
        assert_eq!(join(ROOT, "tmp"), "/tmp");
        assert_eq!(parent("~/Downloads/2026"), Some("~/Downloads"));
        assert_eq!(parent("/tmp"), Some(ROOT));
        assert_eq!(parent(ROOT), None);
        assert_eq!(basename("~/notes.md"), "notes.md");
        assert_eq!(basename(ROOT), ROOT);
        assert!(is_root(HOME) && is_root(ROOT) && !is_root("~/x"));
        assert_eq!(display_path(&real_path("~/Downloads")), "~/Downloads");
        assert_eq!(display_path(Path::new("/etc/hosts")), "/etc/hosts");
    }

    #[test]
    fn file_kinds_come_off_the_name() {
        assert_eq!(FileKind::of_name("a.PNG"), FileKind::Image);
        assert_eq!(FileKind::of_name("a.md"), FileKind::Text);
        assert_eq!(FileKind::of_name("a.pdf"), FileKind::Pdf);
        assert_eq!(FileKind::of_name("a.tar"), FileKind::Archive);
        assert_eq!(FileKind::of_name("Makefile"), FileKind::Other);
        assert_eq!(FileKind::Dir.word(), "directory");
        assert_eq!(FileKind::Image.tag(), "image");
        assert!(demo::exists("~/notes.md") && demo::is_dir("~/Downloads"));
        assert!(!demo::exists("~/nothing"));
    }

    /// A denied world answers the clock and refuses the rest, in words.
    #[test]
    fn a_denied_world_has_only_the_clock() {
        let env = Env::default();
        let mut caps = Capabilities::default();
        install(Mode::Deny, &env, &mut caps);
        assert!(caps.get::<dyn Clock>().is_some());
        assert!(caps.get::<dyn Disk>().is_none());
        assert!(caps.get::<dyn Clipboard>().is_none());

        let mut caps = Capabilities::default();
        install(Mode::Fake, &env, &mut caps);
        assert!(caps.get::<dyn Disk>().is_some());
        assert!(caps.get::<dyn Secrets>().is_some());
        assert!(caps.get::<dyn Screen>().is_some());
    }
}
