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
use std::path::PathBuf;
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
    // What was last asked for, and what is actually being watched: a
    // directory that has gone is asked for and not watched, and keeping
    // the two apart is what stops the loop rebuilding over it every turn.
    let mut asked: Vec<PathBuf> = Vec::new();
    let mut have: Vec<(PathBuf, c_int)> = Vec::new();
    while !w.stopped() {
        let want = w.want();
        if want != asked {
            for (_, wd) in have.drain(..) {
                unsafe { inotify_rm_watch(fd, wd) };
            }
            have = want.iter().cloned().filter_map(|d| add(fd, d)).collect();
            asked = want;
        }
        wait(fd, knocks, TURN);
        let hit = drain(fd, &have);
        w.report(&hit);
    }
    for (_, wd) in have.drain(..) {
        unsafe { inotify_rm_watch(fd, wd) };
    }
    unsafe { close(fd) };
}

/// One directory watched, with the descriptor it will be reported under.
/// A directory that has gone between the panel opening and this — or that
/// is not a directory at all — is simply not watched: the panel's own
/// listing already says so.
fn add(fd: c_int, dir: PathBuf) -> Option<(PathBuf, c_int)> {
    let text = CString::new(dir.as_os_str().as_bytes()).ok()?;
    let wd = unsafe { inotify_add_watch(fd, text.as_ptr(), WATCHING) };
    (wd >= 0).then_some((dir, wd))
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
fn drain(fd: c_int, have: &[(PathBuf, c_int)]) -> Vec<PathBuf> {
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
            if let Some((dir, _)) = have.iter().find(|(_, wd)| *wd == e.wd) {
                if !hit.contains(dir) {
                    hit.push(dir.clone());
                }
            }
            at += HEAD + e.len as usize;
        }
    }
}
