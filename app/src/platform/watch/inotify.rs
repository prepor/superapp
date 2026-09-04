//! Android (and any other linux): one inotify watch per directory a panel
//! is looking at.
//!
//! inotify is the instrument this app's model was already shaped like — a
//! watch is one directory, not a tree — so nothing here filters. Each
//! watch is added for what changes a *listing*: an entry appearing,
//! going, being renamed either way, being written or having its metadata
//! changed, and the directory itself going or moving.
//!
//! The thread waits on two things at once: the inotify descriptor, and a
//! pipe the handle writes a byte to when the wanted set changes. That is
//! what lets a panel that has just opened be watched now rather than at
//! the end of a turn, and what lets an app with no files panel open sleep
//! rather than spin.
//!
//! None of this has run on a device: there is no android SDK in this tree
//! (see the book's open questions). It is written to the syscalls' own
//! contract and compiles for the target, which is as far as this checkout
//! can take it.

#![allow(non_camel_case_types)]

use std::ffi::{c_char, c_int, c_void, CString};
use std::os::unix::ffi::OsStrExt;
use std::path::{Path, PathBuf};
use std::thread::JoinHandle;

use super::{Watching, TURN};

// -- what linux calls it -------------------------------------------------------

/// Everything that can change what a listing shows: entries in, out, moved
/// either way, written, or restamped — and the directory itself going or
/// being moved, which ends the watch.
const WATCHING: u32 = IN_CREATE
    | IN_DELETE
    | IN_MOVED_FROM
    | IN_MOVED_TO
    | IN_MODIFY
    | IN_ATTRIB
    | IN_CLOSE_WRITE
    | IN_DELETE_SELF
    | IN_MOVE_SELF
    | IN_ONLYDIR;

const IN_MODIFY: u32 = 0x0000_0002;
const IN_ATTRIB: u32 = 0x0000_0004;
const IN_CLOSE_WRITE: u32 = 0x0000_0008;
const IN_MOVED_FROM: u32 = 0x0000_0040;
const IN_MOVED_TO: u32 = 0x0000_0080;
const IN_CREATE: u32 = 0x0000_0100;
const IN_DELETE: u32 = 0x0000_0200;
const IN_DELETE_SELF: u32 = 0x0000_0400;
const IN_MOVE_SELF: u32 = 0x0000_0800;
const IN_ONLYDIR: u32 = 0x0100_0000;
/// The kernel's own queue overflowed and events were lost. What was missed
/// cannot be known, so every watched directory is reported.
const IN_Q_OVERFLOW: u32 = 0x0000_4000;
/// This watch is over: the directory went, or moved, or the watch was
/// removed. The kernel says it once and never speaks for that descriptor
/// again, so what it costs is the watch itself — the loop takes the
/// descriptor back and asks for the directory again on its next turn, in
/// case something has put it back.
const IN_IGNORED: u32 = 0x0000_8000;

const O_CLOEXEC: c_int = 0o200_0000;
const O_NONBLOCK: c_int = 0o000_4000;
const POLLIN: i16 = 0x001;

#[repr(C)]
struct pollfd {
    fd: c_int,
    events: i16,
    revents: i16,
}

/// The fixed head of one event; the name, when there is one, follows it
/// and is `len` bytes long. A listing does not care which name it was —
/// it reads the directory again either way — so the name is stepped over.
#[repr(C)]
#[derive(Clone, Copy)]
struct inotify_event {
    wd: c_int,
    mask: u32,
    cookie: u32,
    len: u32,
}

const HEAD: usize = std::mem::size_of::<inotify_event>();

extern "C" {
    fn inotify_init1(flags: c_int) -> c_int;
    fn inotify_add_watch(fd: c_int, path: *const c_char, mask: u32) -> c_int;
    fn inotify_rm_watch(fd: c_int, wd: c_int) -> c_int;
    fn pipe2(fds: *mut c_int, flags: c_int) -> c_int;
    fn poll(fds: *mut pollfd, count: usize, timeout_ms: c_int) -> c_int;
    fn read(fd: c_int, buf: *mut c_void, count: usize) -> isize;
    fn write(fd: c_int, buf: *const c_void, count: usize) -> isize;
    fn close(fd: c_int) -> c_int;
}

// -- the thread ----------------------------------------------------------------

/// The handle the watcher keeps: the pipe to knock on, and the thread to
/// join when the app is done with it. The pipe is closed here rather than
/// on the thread, so a knock can never land on a descriptor number
/// something else has since been given.
pub struct Thread {
    knock: c_int,
    read_end: c_int,
    watching: Watching,
    join: Option<JoinHandle<()>>,
}

impl Thread {
    pub fn start(w: Watching) -> Thread {
        let mut fds: [c_int; 2] = [-1, -1];
        if unsafe { pipe2(fds.as_mut_ptr(), O_CLOEXEC | O_NONBLOCK) } != 0 {
            // Not fatal: without the pipe there is nothing to knock on, so
            // a panel that has just opened is watched at the end of the
            // turn rather than at once.
            eprintln!("watch: no pipe — a new panel is watched at the turn");
            fds = [-1, -1];
        }
        let [read_end, knock] = fds;
        let mine = w.clone();
        let join = std::thread::Builder::new()
            .name("disk-watch".to_string())
            .spawn(move || watch_loop(&mine, read_end))
            .map_err(|e| eprintln!("watch: the disk is not being watched: {e}"))
            .ok();
        Thread {
            knock,
            read_end,
            watching: w,
            join,
        }
    }

    /// The wanted set changed: one byte down the pipe ends the wait.
    pub fn wake(&self) {
        if self.knock < 0 {
            return;
        }
        let byte = [0u8; 1];
        // A pipe nobody has drained is a pipe with a knock already in it,
        // which says the same thing; the write end is non-blocking, so a
        // full one fails rather than holding the UI thread.
        unsafe { write(self.knock, byte.as_ptr().cast::<c_void>(), 1) };
    }

    pub fn stop(&mut self) {
        self.watching.end();
        self.wake();
        if let Some(join) = self.join.take() {
            let _ = join.join();
        }
        for fd in [self.knock, self.read_end] {
            if fd >= 0 {
                unsafe { close(fd) };
            }
        }
        (self.knock, self.read_end) = (-1, -1);
    }
}

fn watch_loop(w: &Watching, knocks: c_int) {
    let fd = unsafe { inotify_init1(O_CLOEXEC | O_NONBLOCK) };
    if fd < 0 {
        eprintln!("watch: the disk is not being watched — inotify would not open");
        return;
    }
    // Every directory wanted, with the descriptor it is being watched
    // under — `NONE` for one that is wanted and not watched, which is a
    // directory that was not there when the panel opened, or that has
    // gone since. Those are asked for again on every turn: a watch lost
    // is a watch the kernel will not restore, and a path can come back.
    let mut have: Vec<(PathBuf, c_int)> = Vec::new();
    while !w.stopped() {
        let want = w.want();
        if !same(&have, &want) {
            // What is no longer wanted is let go of; what still is keeps
            // the watch it has, so an opening panel disturbs no other —
            // and no descriptor is freed only to be handed straight back,
            // which is how a queued `IN_IGNORED` would land on the wrong
            // watch.
            have.retain(|(dir, wd)| {
                let keep = want.binary_search(dir).is_ok();
                if !keep && *wd != NONE {
                    unsafe { inotify_rm_watch(fd, *wd) };
                }
                keep
            });
            for dir in want {
                if !have.iter().any(|(d, _)| *d == dir) {
                    have.push((dir, NONE));
                }
            }
            have.sort_by(|(a, _), (b, _)| a.cmp(b));
        }
        for (dir, wd) in have.iter_mut().filter(|(_, wd)| *wd == NONE) {
            *wd = add(fd, dir);
        }
        wait(fd, knocks, TURN);
        let hit = drain(fd, &mut have);
        w.report(&hit);
    }
    for (_, wd) in have.drain(..).filter(|(_, wd)| *wd != NONE) {
        unsafe { inotify_rm_watch(fd, wd) };
    }
    unsafe { close(fd) };
}

/// No descriptor: wanted, not watched.
const NONE: c_int = -1;

/// Whether the directories being kept are the ones wanted. Both are
/// sorted — the books answer in key order and the list is kept in it — so
/// this is the comparison and not a search.
fn same(have: &[(PathBuf, c_int)], want: &[PathBuf]) -> bool {
    have.len() == want.len() && have.iter().zip(want).all(|((d, _), w)| d == w)
}

/// One directory watched, or [`NONE`] where it cannot be: a directory that
/// has gone, or that is not a directory at all. Never fatal and never
/// final — the panel's own listing already says what it found, and the
/// loop asks again next turn.
fn add(fd: c_int, dir: &Path) -> c_int {
    let Ok(text) = CString::new(dir.as_os_str().as_bytes()) else {
        return NONE;
    };
    unsafe { inotify_add_watch(fd, text.as_ptr(), WATCHING) }
}

/// Sleeps until inotify has something, the handle knocks, or the turn ends.
fn wait(fd: c_int, knocks: c_int, turn: f64) {
    let mut fds = [
        pollfd {
            fd,
            events: POLLIN,
            revents: 0,
        },
        pollfd {
            fd: knocks,
            events: POLLIN,
            revents: 0,
        },
    ];
    let count = if knocks < 0 { 1 } else { 2 };
    unsafe { poll(fds.as_mut_ptr(), count, (turn * 1000.0) as c_int) };
    // Whatever was knocked with is read off, so the next wait waits.
    if knocks >= 0 {
        let mut sink = [0u8; 64];
        while unsafe { read(knocks, sink.as_mut_ptr().cast::<c_void>(), sink.len()) } > 0 {}
    }
}

/// Everything inotify has to say right now, as the set of directories it
/// happened in — one round however many events carried it.
///
/// A watch the kernel has closed is taken back here rather than left
/// standing dead, so the loop's next turn asks for that directory again.
fn drain(fd: c_int, have: &mut [(PathBuf, c_int)]) -> Vec<PathBuf> {
    // Aligned, because what is read into it is a run of C structs.
    #[repr(C, align(8))]
    struct Buf([u8; 4096]);
    let mut buf = Buf([0; 4096]);
    let mut hit: Vec<PathBuf> = Vec::new();
    loop {
        let n = unsafe { read(fd, buf.0.as_mut_ptr().cast::<c_void>(), buf.0.len()) };
        if n <= 0 {
            return hit; // nothing left, or nothing at all
        }
        let n = n as usize;
        let mut at = 0;
        while at + HEAD <= n {
            // Read out rather than borrowed: an event is only as aligned
            // as the one before it left it.
            let head = unsafe { buf.0.as_ptr().add(at) }.cast::<inotify_event>();
            let e = unsafe { head.read_unaligned() };
            if e.mask & IN_Q_OVERFLOW != 0 {
                // What was lost cannot be known: everything is stale.
                return have.iter().map(|(d, _)| d.clone()).collect();
            }
            // Every directory under that descriptor, not the first: one
            // inode reached by two paths is watched once, and both panels
            // are looking at what changed.
            for (dir, wd) in have.iter_mut().filter(|(_, wd)| *wd == e.wd) {
                if !hit.contains(dir) {
                    hit.push(dir.clone());
                }
                if e.mask & IN_IGNORED != 0 {
                    *wd = NONE;
                }
            }
            at += HEAD + e.len as usize;
        }
    }
}
