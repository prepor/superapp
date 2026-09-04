//! macOS: one FSEvents stream over every directory a panel is looking at.
//!
//! FSEvents is a *tree* instrument — it reports a change anywhere below
//! each path it was given — and a files panel shows one directory. So the
//! stream is created without `kFSEventStreamCreateFlagFileEvents`, which
//! makes every event a directory's own path, and a callback keeps the
//! events whose path is a directory somebody is looking at and drops the
//! rest. What happens deep under `~` costs the match and nothing more.
//!
//! The stream is scheduled on this thread's run loop, which is run a turn
//! at a time: between turns the wanted set is compared against what is
//! being watched, and the stream is rebuilt when they differ. A panel that
//! opens or closes stops the run loop rather than waiting for the turn to
//! end, so watching starts with the panel and not a second after it.

// CoreFoundation's mode is declared under the name it exports.
#![allow(non_upper_case_globals)]

use std::ffi::{c_char, c_void, CStr, CString};
use std::os::unix::ffi::OsStrExt;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::thread::JoinHandle;
use std::time::Duration;

use super::{resolve, Watching, TURN};

// -- what CoreServices calls it ------------------------------------------------

type CFAllocatorRef = *const c_void;
type CFStringRef = *const c_void;
type CFArrayRef = *const c_void;
type CFRunLoopRef = *mut c_void;
type CFIndex = isize;
type CFTimeInterval = f64;
type Boolean = u8;

type FSEventStreamRef = *mut c_void;
type FSEventStreamEventId = u64;
type FSEventStreamEventFlags = u32;
type FSEventStreamCreateFlags = u32;

const UTF8: u32 = 0x0800_0100;
/// Only what happens from now on: what is already there was listed by the
/// panel that asked for the watch.
const SINCE_NOW: FSEventStreamEventId = 0xFFFF_FFFF_FFFF_FFFF;
/// The first event of a burst arrives at once and the rest are gathered
/// into the latency below, so a single touch is prompt and a thousand-file
/// copy is still one delivery.
const NO_DEFER: FSEventStreamCreateFlags = 0x0000_0002;
/// Report what happens *to* a watched directory as well as in it: moved,
/// renamed, deleted, or an ancestor of it moved. Without this a panel
/// whose directory is taken out from under it — `~/src` renamed while a
/// listing of `~/src/ui` is open — hears nothing more, ever, and goes on
/// drawing a listing of a path that is not there. The event arrives on
/// the path as it was asked for, so it matches like any other and the
/// panel reads again and says what it finds.
const WATCH_ROOT: FSEventStreamCreateFlags = 0x0000_0004;
/// How long FSEvents gathers a burst before delivering it, in seconds. The
/// first grouping of the two; the callback's own is the second.
const LATENCY: CFTimeInterval = 0.25;

/// The three flags that mean *events were lost*: the queue overflowed in
/// the daemon or the kernel, or so much happened at once that FSEvents
/// will only say "look under here again". None of them carries what
/// changed, so what they are answered with is a reading of everything
/// being watched — the same thing inotify's own overflow gets.
const LOST: FSEventStreamEventFlags = 0x0000_0001 | 0x0000_0002 | 0x0000_0004;

#[repr(C)]
struct FSEventStreamContext {
    version: CFIndex,
    info: *mut c_void,
    retain: Option<extern "C" fn(*const c_void) -> *const c_void>,
    release: Option<extern "C" fn(*const c_void)>,
    copy_description: Option<extern "C" fn(*const c_void) -> CFStringRef>,
}

type FSEventStreamCallback = extern "C" fn(
    stream: FSEventStreamRef,
    info: *mut c_void,
    count: usize,
    paths: *mut c_void,
    flags: *const FSEventStreamEventFlags,
    ids: *const FSEventStreamEventId,
);

#[link(name = "CoreFoundation", kind = "framework")]
extern "C" {
    static kCFRunLoopDefaultMode: CFStringRef;

    fn CFStringCreateWithCString(
        alloc: CFAllocatorRef,
        text: *const c_char,
        encoding: u32,
    ) -> CFStringRef;
    fn CFArrayCreate(
        alloc: CFAllocatorRef,
        values: *const *const c_void,
        count: CFIndex,
        callbacks: *const c_void,
    ) -> CFArrayRef;
    fn CFRelease(cf: *const c_void);

    fn CFRunLoopGetCurrent() -> CFRunLoopRef;
    fn CFRunLoopRunInMode(mode: CFStringRef, seconds: CFTimeInterval, once: Boolean) -> i32;
    fn CFRunLoopStop(run_loop: CFRunLoopRef);
}

#[link(name = "CoreServices", kind = "framework")]
extern "C" {
    fn FSEventStreamCreate(
        alloc: CFAllocatorRef,
        callback: FSEventStreamCallback,
        context: *mut FSEventStreamContext,
        paths: CFArrayRef,
        since: FSEventStreamEventId,
        latency: CFTimeInterval,
        flags: FSEventStreamCreateFlags,
    ) -> FSEventStreamRef;
    fn FSEventStreamScheduleWithRunLoop(
        stream: FSEventStreamRef,
        run_loop: CFRunLoopRef,
        mode: CFStringRef,
    );
    fn FSEventStreamStart(stream: FSEventStreamRef) -> Boolean;
    fn FSEventStreamStop(stream: FSEventStreamRef);
    fn FSEventStreamInvalidate(stream: FSEventStreamRef);
    fn FSEventStreamRelease(stream: FSEventStreamRef);
}

// -- the thread ----------------------------------------------------------------

/// The handle the watcher keeps: the run loop to knock on, and the thread
/// to join when the app is done with it.
pub struct Thread {
    run_loop: Arc<AtomicUsize>,
    idle: Arc<Park>,
    watching: Watching,
    join: Option<JoinHandle<()>>,
}

impl Thread {
    pub fn start(w: Watching) -> Thread {
        let run_loop = Arc::new(AtomicUsize::new(0));
        let idle = Arc::new(Park::new());
        let (rl, park, mine) = (run_loop.clone(), idle.clone(), w.clone());
        let join = std::thread::Builder::new()
            .name("disk-watch".to_string())
            .spawn(move || watch_loop(&mine, &rl, &park))
            .map_err(|e| eprintln!("watch: the disk is not being watched: {e}"))
            .ok();
        Thread {
            run_loop,
            idle,
            watching: w,
            join,
        }
    }

    /// The wanted set changed. Both ways the thread can be waiting are
    /// ended: the run loop is stopped, and a park is stepped out of.
    pub fn wake(&self) {
        let rl = self.run_loop.load(Ordering::SeqCst);
        if rl != 0 {
            // Safe while the thread is alive, which it is until `stop`
            // joins it — and `stop` is `&mut self`, so it cannot be
            // running beside this.
            unsafe { CFRunLoopStop(rl as CFRunLoopRef) };
        }
        self.idle.wake();
    }

    pub fn stop(&mut self) {
        self.watching.end();
        self.wake();
        if let Some(join) = self.join.take() {
            let _ = join.join();
        }
    }
}

/// Where the thread waits while there is nothing to watch: until somebody
/// opens a panel, or a turn, whichever comes first. A run loop with no
/// stream on it returns at once, so this is what keeps an app with no
/// files panel open from spinning.
pub(crate) struct Park {
    knocked: Mutex<bool>,
    bell: Condvar,
}

impl Park {
    fn new() -> Park {
        Park {
            knocked: Mutex::new(false),
            bell: Condvar::new(),
        }
    }

    fn wake(&self) {
        if let Ok(mut k) = self.knocked.lock() {
            *k = true;
            self.bell.notify_all();
        }
    }

    /// A knock that came before the wait is still a knock: it is read
    /// here rather than slept through, so the first panel of a run is
    /// watched at once and not a turn later.
    fn wait(&self, how_long: Duration) {
        let Ok(mut knocked) = self.knocked.lock() else {
            return;
        };
        if !*knocked {
            let Ok((k, _)) = self.bell.wait_timeout(knocked, how_long) else {
                return;
            };
            knocked = k;
        }
        *knocked = false;
    }
}

fn watch_loop(w: &Watching, run_loop: &Arc<AtomicUsize>, idle: &Park) {
    // The run loop of this thread, made by the asking. Published so the
    // handle can stop it, and taken back before the thread ends.
    let rl = unsafe { CFRunLoopGetCurrent() };
    run_loop.store(rl as usize, Ordering::SeqCst);

    // Each wanted directory as the panel asked for it and as it resolves
    // now — the pair the callback matches on, and the pair a rebuild is
    // decided by: a link repointed at another directory is a change to
    // the path a panel is showing, with nothing having happened in either
    // place for FSEvents to report.
    let mut have: Vec<(PathBuf, PathBuf)> = Vec::new();
    let mut stream: Option<Stream> = None;
    while !w.stopped() {
        let want: Vec<(PathBuf, PathBuf)> = w
            .want()
            .into_iter()
            .map(|dir| {
                let canon = resolve(&dir);
                (dir, canon)
            })
            .collect();
        if want != have {
            // What a path leads to now, where it led somewhere else
            // before: a reading is owed on that path, and the panel that
            // has just opened — not in `have` at all — is owed nothing,
            // since it read the directory itself a moment ago.
            let moved: Vec<PathBuf> = want
                .iter()
                .filter(|(asked, canon)| {
                    have.iter()
                        .any(|(was, before)| was == asked && before != canon)
                })
                .map(|(asked, _)| asked.clone())
                .collect();
            drop(stream.take()); // the old one goes before the new one is made
            stream = Stream::open(&want, w, rl);
            have = want;
            w.report(&moved);
        }
        if stream.is_some() {
            unsafe { CFRunLoopRunInMode(kCFRunLoopDefaultMode, TURN, 0) };
        } else {
            idle.wait(Duration::from_secs_f64(TURN));
        }
    }
    run_loop.store(0, Ordering::SeqCst);
}

// -- one stream over every watched directory -----------------------------------

/// What the callback is handed: where to report, and which directories are
/// worth reporting — each as the panel asked for it and as the disk
/// answers, since `/tmp` is `/private/tmp` to an event and a home
/// directory may be a link anywhere.
struct Watch {
    w: Watching,
    dirs: Vec<(PathBuf, PathBuf)>,
}

/// One live FSEvents stream, with everything it borrows. Dropping it stops
/// the stream, unschedules it, and frees the callback's own box — in that
/// order, so nothing can be delivered into freed memory.
struct Stream {
    stream: FSEventStreamRef,
    array: CFArrayRef,
    strings: Vec<CFStringRef>,
    watch: *mut Watch,
}

impl Stream {
    /// A stream over these directories — each as the panel asked for it
    /// and as it resolves right now — or `None` for none to watch and for
    /// a stream the system would not start.
    fn open(dirs: &[(PathBuf, PathBuf)], w: &Watching, rl: CFRunLoopRef) -> Option<Stream> {
        let dirs = dirs.to_vec();
        if dirs.is_empty() {
            return None;
        }
        let mut strings: Vec<CFStringRef> = Vec::new();
        for (_, canon) in &dirs {
            let Ok(text) = CString::new(canon.as_os_str().as_bytes()) else {
                continue;
            };
            let s = unsafe { CFStringCreateWithCString(std::ptr::null(), text.as_ptr(), UTF8) };
            if !s.is_null() {
                strings.push(s);
            }
        }
        if strings.is_empty() {
            return None;
        }
        // Null callbacks: the array neither retains nor releases what it
        // holds, and the strings are released with it below.
        let array = unsafe {
            CFArrayCreate(
                std::ptr::null(),
                strings.as_ptr(),
                strings.len() as CFIndex,
                std::ptr::null(),
            )
        };
        let watch = Box::into_raw(Box::new(Watch { w: w.clone(), dirs }));
        let mut context = FSEventStreamContext {
            version: 0,
            info: watch.cast::<c_void>(),
            retain: None,
            release: None,
            copy_description: None,
        };
        let stream = unsafe {
            FSEventStreamCreate(
                std::ptr::null(),
                changed,
                &mut context,
                array,
                SINCE_NOW,
                LATENCY,
                NO_DEFER | WATCH_ROOT,
            )
        };
        let made = Stream {
            stream,
            array,
            strings,
            watch,
        };
        if stream.is_null() {
            return None; // dropping `made` gives back what was made
        }
        unsafe {
            FSEventStreamScheduleWithRunLoop(stream, rl, kCFRunLoopDefaultMode);
        }
        if unsafe { FSEventStreamStart(stream) } == 0 {
            eprintln!("watch: the disk is not being watched — FSEvents refused to start");
            return None;
        }
        Some(made)
    }
}

impl Drop for Stream {
    fn drop(&mut self) {
        unsafe {
            if !self.stream.is_null() {
                FSEventStreamStop(self.stream);
                FSEventStreamInvalidate(self.stream);
                FSEventStreamRelease(self.stream);
            }
            if !self.array.is_null() {
                CFRelease(self.array);
            }
            for s in self.strings.drain(..) {
                CFRelease(s);
            }
            // Last: the stream is invalidated by here, so no callback can
            // still be on its way to this box.
            drop(Box::from_raw(self.watch));
        }
    }
}

/// One delivery: every path it carried, matched against the directories
/// somebody is looking at, and reported as a single round.
///
/// An event that says events were lost is answered with all of them: a
/// listing that may be stale and a listing that is known to be stale are
/// worth the same one reading.
extern "C" fn changed(
    _stream: FSEventStreamRef,
    info: *mut c_void,
    count: usize,
    paths: *mut c_void,
    flags: *const FSEventStreamEventFlags,
    _ids: *const FSEventStreamEventId,
) {
    if info.is_null() || paths.is_null() || count == 0 {
        return;
    }
    // Set up by `Stream::open` and freed by its `Drop`, which cannot run
    // while the stream is still delivering.
    let watch = unsafe { &*info.cast::<Watch>() };
    let paths = paths.cast::<*const c_char>();
    let mut hit: Vec<PathBuf> = Vec::new();
    for i in 0..count {
        if !flags.is_null() && unsafe { *flags.add(i) } & LOST != 0 {
            let all: Vec<PathBuf> = watch.dirs.iter().map(|(asked, _)| asked.clone()).collect();
            watch.w.report(&all);
            return;
        }
        let p = unsafe { *paths.add(i) };
        if p.is_null() {
            continue;
        }
        let bytes = unsafe { CStr::from_ptr(p) }.to_bytes();
        let path = Path::new(std::ffi::OsStr::from_bytes(bytes));
        // Every spelling of the directory it happened in, not the first:
        // two panels may have reached one directory by two paths.
        for (asked, canon) in &watch.dirs {
            if canon == path && !hit.contains(asked) {
                hit.push(asked.clone());
            }
        }
    }
    watch.w.report(&hit);
}
