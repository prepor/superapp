//! The files app: a directory list and a file card over the demo tree,
//! with a clipboard other apps may read.
//!
//! Two tags: `files` over a directory, `file` over a path. Nothing is
//! stored — the disk is the state — so the app has no schema, no seed and
//! no effects of its own; what it adds to the shell is two panel kinds, one
//! launcher root, the clipboard below, and the one worker that performs the
//! verbs that write ([`run`]) so a large tree does not stop the window.

use std::any::Any;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Mutex;

use kernel::app::{App, Root, Worker};
use kernel::effect::World;
use kernel::panel::PanelKind;
use kernel::session::Session;
use kernel::store::Store;

pub mod completion;
pub mod model;
pub mod ops;
pub mod panels;
pub mod run;
pub mod scenes;
pub mod ui;
pub mod widgets;

#[cfg(test)]
mod tests;

use model::{basename, plural, watched_at, HOME};
pub use panels::{Card, Dir};
pub use ui::UI;

/// The app.
pub struct Files {
    /// What `copy` and `move` are holding. A `Mutex` because an [`App`] is
    /// `Sync`; it is only ever taken on the UI thread, and never across a
    /// call that could take it again.
    clip: Mutex<Clipboard>,
    /// How many times the disk has been written through this app. A panel
    /// compares this against what it last listed at and lists again when
    /// they differ — which is how an undo, whose reversal is the history's
    /// and not a verb's, still reaches every open listing.
    ///
    /// It answers for this app's own writes only. What another program
    /// does is the watcher's to count, and [`Seen`] is the pair.
    writes: AtomicU64,
    /// The verbs that write, waiting, in flight, and finished: the one
    /// place the UI thread and the runner's thread meet. A `Mutex` for the
    /// obvious reason, and never held across a disk call.
    runs: Mutex<run::Runs>,
    /// Bumped whenever a run moves — a path performed, a run queued, a run
    /// over. A poll that finds this where it left it has nothing to redraw.
    moved: AtomicU64,
    /// What the last poll saw. Where a process runs more than one session
    /// this is whichever polled last, which is why it decides only the
    /// redraw: what a session *records* is its own landed runs, and those
    /// are taken by [`run`] under its own world.
    seen: AtomicU64,
}

/// The one in this build.
pub static FILES: Files = Files {
    clip: Mutex::new(Clipboard::empty()),
    writes: AtomicU64::new(0),
    runs: Mutex::new(run::Runs::new()),
    moved: AtomicU64::new(0),
    seen: AtomicU64::new(0),
};

static DIR_KIND: panels::dir::DirKind = panels::dir::DirKind;
static CARD_KIND: panels::card::CardKind = panels::card::CardKind;
static KINDS: &[&dyn PanelKind] = &[&DIR_KIND, &CARD_KIND];

impl App for Files {
    fn id(&self) -> &'static str {
        "files"
    }

    fn kinds(&self) -> &'static [&'static dyn PanelKind] {
        KINDS
    }

    /// One root: home. Everywhere else is a walk or a typed path away.
    fn roots(&self) -> Vec<Root> {
        vec![Root::new(Dir::id(HOME), "files", "disk home directory")]
    }

    /// One: the pass that performs the verbs that write, and only while
    /// there is something to perform. It is asked for here — rather than
    /// derived from a row, as a sync pass is — because this app stores
    /// nothing: what calls for the thread is the queue itself.
    fn workers(&self, store: &Store) -> Vec<Box<dyn Worker>> {
        if self.busy(run::whose(store)) {
            vec![Box::new(run::Runner::new())]
        } else {
            Vec::new()
        }
    }

    /// What a finished run owes the session: its history node, its intents,
    /// its toast and the listings that go stale behind it — everything the
    /// verb used to do on the frame of the click, one frame after the disk
    /// caught up.
    fn poll(&self, s: &mut Session) {
        // Its own runs, whatever anyone else's are doing; then whether
        // anything at all has moved since the last look, which is what a
        // quiet frame costs.
        let db = run::whose(s.store());
        let landed = self.take_landed(db);
        let moved = self.moved.load(Ordering::Relaxed);
        let quiet = self.seen.swap(moved, Ordering::Relaxed) == moved;
        if landed.is_empty() && quiet {
            return;
        }
        for l in landed {
            panels::dir::land(s, l);
        }
        // The worker is asked for again whenever the workers are kicked,
        // and it retires the moment its session has nothing to perform. An
        // action kicks them — but a run refused outright, one given back to
        // the lease, and a cancel that dropped what had not started all
        // record no action at all, and the thread would sit on a store
        // reader until something else happened to. So having nothing left
        // to do is a kick of its own.
        if !self.busy(db) {
            s.workers().kick_all();
        }
        // A run that is still going has moved the disk under every listing,
        // and its line has moved under every header.
        s.redraw();
    }

    fn as_any(&self) -> &dyn Any {
        self
    }
}

// -- the clipboard -------------------------------------------------------------

/// Which verb filled the clipboard, which is what a `… here` will do.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Op {
    Copy,
    Move,
}

impl Op {
    /// The verb, for a toast.
    #[must_use]
    pub fn verb(self) -> &'static str {
        match self {
            Op::Copy => "copy",
            Op::Move => "move",
        }
    }

    /// The button every files panel shows while this is held.
    #[must_use]
    pub fn here_label(self) -> &'static str {
        match self {
            Op::Copy => "copy here",
            Op::Move => "move here",
        }
    }

    /// What a `… here` did, past tense.
    #[must_use]
    pub fn done(self) -> &'static str {
        match self {
            Op::Copy => "copied",
            Op::Move => "moved",
        }
    }
}

/// Paths held for a copy or a move, until a `… here` performs them.
///
/// A card holds its own file, a list holds every marked row, and a `… here`
/// performs the set — refusing per path exactly as it does for one. Empty
/// when nothing is held.
///
/// This is the files app's public API: another app reaches it through
/// [`Apps::get_as`](kernel::app::Apps::get_as) and works when the answer is
/// `None`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Clipboard {
    /// What will happen to the paths where they land.
    pub verb: Op,
    pub paths: Vec<String>,
}

impl Default for Clipboard {
    fn default() -> Clipboard {
        Clipboard::empty()
    }
}

impl Clipboard {
    /// Nothing held.
    #[must_use]
    pub const fn empty() -> Clipboard {
        Clipboard {
            verb: Op::Copy,
            paths: Vec::new(),
        }
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.paths.is_empty()
    }

    /// What a toast calls it: the name where one thing is held, the count
    /// where a set is.
    #[must_use]
    pub fn what(&self) -> String {
        match self.paths.as_slice() {
            [one] => format!("“{}”", basename(one)),
            many => plural(many.len()),
        }
    }
}

impl Files {
    /// What `copy` and `move` are holding right now; empty when nothing is.
    ///
    /// Pulled, never subscribed to: bars are built on every draw, and a
    /// redraw is the one signal. A compose panel asks this when it builds
    /// its bar and offers *attach* while it says something.
    ///
    /// # Panics
    ///
    /// If a previous holder panicked while it had the clipboard.
    #[must_use]
    pub fn clipboard(&self) -> Clipboard {
        self.clip.lock().expect("the files clipboard").clone()
    }

    /// Holds a set of paths for a verb — what `copy` and `move` do.
    ///
    /// # Panics
    ///
    /// As [`Files::clipboard`].
    pub fn set(&self, verb: Op, paths: Vec<String>) {
        *self.clip.lock().expect("the files clipboard") = Clipboard { verb, paths };
    }

    /// Lets go — what a `move here` does once it has moved, and what a
    /// delete does to a path it took away.
    ///
    /// # Panics
    ///
    /// As [`Files::clipboard`].
    pub fn clear(&self) {
        *self.clip.lock().expect("the files clipboard") = Clipboard::empty();
    }

    /// The disk changed. Every write goes through
    /// [`ops`](crate::apps::files::ops), which says so here, so a panel
    /// that lists again on the next look covers an undo as well as a verb.
    pub fn touched(&self) {
        self.writes.fetch_add(1, Ordering::Relaxed);
    }

    /// What a panel compares its listing against, for this app's own
    /// writes. [`Files::seen`] is the whole of it.
    #[must_use]
    pub fn writes(&self) -> u64 {
        self.writes.load(Ordering::Relaxed)
    }

    /// What a reading of `dir` is taken at.
    ///
    /// A panel keeps the answer beside what it read and reads again when
    /// it differs, which is one comparison for the three ways a listing
    /// goes stale: a verb of this app's, an undo the history ran, and
    /// another program.
    #[must_use]
    pub fn seen(&self, world: &World, dir: &str) -> Seen {
        Seen {
            writes: self.writes(),
            outside: watched_at(world, dir),
        }
    }
}

/// When a panel last read a directory, counted from both sides: this app's
/// own writes, and the rounds of somebody else's the watcher had seen for
/// that directory.
///
/// Neither number means anything by itself — only that it moved.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Seen {
    writes: u64,
    outside: u64,
}
