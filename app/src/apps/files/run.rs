//! The disk written off the UI thread: what a run is, how far it has got,
//! and how to stop it.
//!
//! A verb that writes the disk used to perform every one of its paths on the
//! frame of the click. That is fine for the file under the cursor and a
//! window that does not move for a directory of forty thousand — the frame
//! loop is the thread performing the copy, so nothing draws, nothing
//! scrolls, and the one thing a person wants (to stop it) cannot be pressed.
//!
//! So the four verbs that write — `new dir`, `copy here`, `move here` and
//! `delete` — hand a [`Run`] to this queue instead, and a
//! [`Worker`](kernel::app::Worker) performs it a path at a time. Four things
//! hold:
//!
//! * **The same disk.** A run performs the very effects the panel did
//!   ([`ops`]), through the [`Disk`](kernel::caps::Disk) its world was given
//!   — which the shell installs on the env, so the window's world and this
//!   thread's are handed the same one. Every refusal, the exclusive claim on
//!   a destination, the trash: one implementation, not a second copier that
//!   drifts from it.
//! * **It says where it is.** [`Files::running`](super::Files::running) is
//!   what the panels draw under their header while a run is on, and the
//!   worker's own thread wakes the window between steps.
//! * **It can be stopped.** [`Files::stop`](super::Files::stop) is read
//!   between steps: the path in hand finishes (a half-copied file is
//!   nobody's), the ones behind it are dropped, and what was done is kept.
//! * **Undo is unchanged.** Nothing here records anything. The worker
//!   collects [`Done`] exactly as the verb did and hands them back at the
//!   end; the node, its intents, the lease check and the toast are the UI
//!   thread's, in `panels::dir::land` — which is the same code the verb used
//!   to run inline, moved one frame later. A run that is stopped halfway
//!   lands what it managed, because a change with no node behind it is a
//!   change nobody can undo.
//!
//! What this is deliberately **not** is a [`Deferred`](kernel::effect::Deferred)
//! effect. The queue in the store is for work that is retried and outlives
//! the process; a copy is neither. Nobody may replay a trash on the next
//! boot because the machine went down mid-run, and there is nothing here
//! worth writing to a database — the disk is the state.

use std::sync::atomic::Ordering;
use std::time::Duration;

use kernel::app::{Wake, Worker};
use kernel::effect::{Job, World};
use kernel::layout::SlotId;
use kernel::session::Session;

use super::model::{basename, is_root};
use super::ops::{self, Done, Plan, Step};
use super::{Clipboard, Op, FILES};

/// What one run does to each of its paths.
///
/// The verb's own words, not the disk's: a `… here` carries the clipboard
/// as it was when the button was pressed and works out its destinations
/// when it starts, because the disk may have moved on while the run was
/// waiting behind another.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Task {
    /// `copy here` / `move here`: what was held, laid down in a directory.
    Here {
        verb: Op,
        clip: Clipboard,
        dir: String,
    },
    /// `delete`: to the trash, never an `rm`.
    Delete {
        paths: Vec<String>,
        /// Whether the panel that ran it is showing what went, and so
        /// closes with the action.
        own: bool,
    },
    /// `new dir`: one directory, where nothing is yet.
    MakeDir { path: String },
}

impl Task {
    /// What a run of this is doing, while it does it.
    #[must_use]
    pub fn doing(&self) -> &'static str {
        match self {
            Task::Here { verb: Op::Copy, .. } => "copying",
            Task::Here { verb: Op::Move, .. } => "moving",
            Task::Delete { .. } => "deleting",
            Task::MakeDir { .. } => "creating",
        }
    }
}

/// One run, as the verb that asked for it hands it over.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Run {
    /// Its number in this process, from one. A stop names the run it is
    /// meant for by this, so a *cancel* pressed on a bar drawn a moment too
    /// early — the run it was about having just finished — stops nothing at
    /// all rather than the next run somebody asks for.
    pub id: u64,
    pub task: Task,
    /// The panel that ran the verb: where the refusal goes, whose marks a
    /// delete took, and what a delete of its own directory closes. It may
    /// well have closed by the time the run lands, and then the run lands
    /// all the same — the node matters more than the line.
    pub by: SlotId,
    /// Whether the passes of this build run on the caller's thread. Where
    /// they do — virtual time, and every test — one pass is the whole run,
    /// so a scripted tick is followed by its consequences in the same tick,
    /// as everything else inline is.
    pub inline: bool,
    /// Which session asked, as the address of the world it holds.
    ///
    /// This app is a `static` and a process may be running more than one
    /// session — the window's, and one per mounted scene in the panels
    /// library — so a queue shared between them needs to know whose run it
    /// has. A node belongs in the history of the session whose verb it was,
    /// and a slot number means nothing anywhere else.
    pub world: usize,
}

/// The address of a session's world: what a run is stamped with, and what
/// the poll compares it against.
#[must_use]
fn whose(s: &Session) -> usize {
    std::rc::Rc::as_ptr(s.world()) as usize
}

/// How far the run in hand has got: what the panels draw under their header
/// while one is on.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Progress {
    /// [`Task::doing`].
    pub doing: &'static str,
    /// Paths attempted so far — done or refused, since both are behind us.
    pub at: usize,
    /// Paths this run planned. Zero until it has looked at the disk.
    pub total: usize,
    /// The name of the last path it took in hand, for the line.
    pub name: String,
}

impl Progress {
    /// The line, as a panel draws it: *copying 12 of 340 — “report.pdf”*,
    /// and *copying “notes.md”* where there is only one path to speak of.
    /// A run that has not looked at the disk yet is the verb alone.
    #[must_use]
    pub fn line(&self) -> String {
        let name = (!self.name.is_empty()).then(|| format!("“{}”", self.name));
        match (self.total > 1, name) {
            (true, Some(n)) => format!("{} {} of {} — {n}", self.doing, self.at, self.total),
            (true, None) => format!("{} {} of {}", self.doing, self.at, self.total),
            (false, Some(n)) => format!("{} {n}", self.doing),
            (false, None) => self.doing.to_string(),
        }
    }
}

/// A run that is over, and everything the UI thread needs to record it.
///
/// This is the whole handover: the worker decides nothing about history,
/// and the thread that owns the history performs no disk.
#[derive(Debug)]
pub struct Landed {
    pub run: Run,
    /// What was performed, in order — the records undo compares against
    /// before it takes anything away.
    pub done: Vec<Done>,
    /// One sentence per path that would not go.
    pub refused: Vec<String>,
    /// Paths the run never reached, because it was stopped.
    pub skipped: usize,
    /// Whether it was stopped rather than finished.
    pub stopped: bool,
    /// Runs that were waiting behind it and went with the stop.
    pub dropped: usize,
}

impl Landed {
    /// How many paths did not go: refused, or never reached.
    #[must_use]
    pub fn missed(&self) -> usize {
        self.refused.len() + self.skipped
    }
}

// -- the queue -----------------------------------------------------------------

/// Every run this app has in flight, waiting, or finished and not yet
/// recorded. One of these lives on [`Files`](super::Files), which is a
/// `static`, so this is the one place the UI thread and the worker meet.
#[derive(Debug, Default)]
pub struct Runs {
    /// Waiting, oldest first. A run is planned when it reaches the front,
    /// against the disk as it is then.
    pub(super) queue: Vec<Run>,
    /// The one in hand, and how far it has got.
    pub(super) now: Option<Progress>,
    /// Which run that is, by [`Run::id`]. Only meaningful while `now` is.
    pub(super) now_id: u64,
    /// How many runs this process has queued: the last [`Run::id`] given
    /// out.
    pub(super) minted: u64,
    /// Over, and waiting for a session to be recorded in.
    pub(super) landed: Vec<Landed>,
}

impl Runs {
    /// Nothing queued, nothing running, nothing to record.
    #[must_use]
    pub const fn new() -> Runs {
        Runs {
            queue: Vec::new(),
            now: None,
            now_id: 0,
            minted: 0,
            landed: Vec::new(),
        }
    }
}

// -- the worker ----------------------------------------------------------------

/// The pass that performs the queue: one run at a time, and — off the UI
/// thread — one path per pass, so the loop around it wakes the window
/// between them.
pub struct Runner {
    /// The run in hand. It lives on the worker rather than in [`Runs`]
    /// because the disk is worked without the lock: a run that is being
    /// performed is nobody else's to touch.
    work: Option<Working>,
}

impl Runner {
    #[must_use]
    pub fn new() -> Runner {
        Runner { work: None }
    }
}

impl Default for Runner {
    fn default() -> Runner {
        Runner::new()
    }
}

impl Worker for Runner {
    /// One, always: the disk is one thing, and two threads writing it would
    /// be two plans made against the same directory.
    fn name(&self) -> String {
        "files-run".to_string()
    }

    /// Nothing files a job, so nothing is claimed. What this worker does is
    /// its pass.
    fn claims(&self, _job: &Job) -> bool {
        false
    }

    fn pass(&mut self, w: &World) -> Wake {
        loop {
            if self.work.is_none() {
                // Planned here rather than where it was asked for: the
                // clipboard may have waited behind another run, and nothing
                // watches the disk.
                let Some(run) = FILES.next_run() else {
                    return Wake::OnKick;
                };
                self.work = Some(Working::plan(w, run));
                FILES.moved();
            }
            let Some(work) = self.work.as_mut() else {
                return Wake::OnKick;
            };
            let inline = work.run.inline;
            let stopped = FILES.stopping(work.run.id);
            if stopped || work.left() == 0 {
                let work = self.work.take().expect("the run in hand");
                // A stop is a stop: what was waiting behind this goes too,
                // and the one line that lands says how much.
                let dropped = if stopped { FILES.drop_queued() } else { 0 };
                FILES.land(work.over(stopped, dropped));
                // Whatever is left in the queue is taken on the next turn
                // of this loop, planned then; an empty queue parks the
                // thread until something kicks it.
                continue;
            }
            work.step(w);
            FILES.showing(work.progress());
            if !inline {
                // One path a pass. The kernel's worker loop wakes the window
                // between passes, which is what a progress line is made of;
                // a zero wait means it comes straight back here.
                return Wake::After(Duration::ZERO);
            }
        }
    }
}

/// The run in hand: its plan, how far through it is, and what it has
/// collected.
struct Working {
    run: Run,
    /// What it will perform. A delete's destination is the trash's to
    /// choose, so its steps carry only where each path came from.
    steps: Vec<Step>,
    at: usize,
    done: Vec<Done>,
    refused: Vec<String>,
    /// The name of the last path taken in hand.
    name: String,
}

impl Working {
    /// The plan, made against the disk as it is right now.
    fn plan(w: &World, run: Run) -> Working {
        let (steps, refused) = match &run.task {
            Task::Here { clip, dir, .. } => {
                let Plan { steps, refused } = ops::plan_here(w, clip, dir);
                (steps, refused)
            }
            Task::Delete { paths, .. } => {
                let mut steps = Vec::new();
                let mut refused = Vec::new();
                for path in paths {
                    // A root is where the browser starts: nothing takes one
                    // away, and saying so here means no disk is ever asked
                    // to.
                    if is_root(path) {
                        refused.push(format!("“{path}” is a root"));
                    } else {
                        steps.push(Step {
                            from: path.clone(),
                            to: String::new(),
                        });
                    }
                }
                (steps, refused)
            }
            Task::MakeDir { path } => (
                vec![Step {
                    from: String::new(),
                    to: path.clone(),
                }],
                Vec::new(),
            ),
        };
        Working {
            run,
            steps,
            at: 0,
            done: Vec::new(),
            refused,
            name: String::new(),
        }
    }

    /// Paths still to attempt.
    fn left(&self) -> usize {
        self.steps.len() - self.at
    }

    /// One path, performed. A path the disk refuses at the last moment
    /// joins the refusals rather than failing the run: what is left is
    /// exactly what happened.
    fn step(&mut self, w: &World) {
        let Some(step) = self.steps.get(self.at).cloned() else {
            return;
        };
        self.at += 1;
        let r = match &self.run.task {
            // Read back the moment after the write: what undo will compare
            // against before it takes anything away.
            Task::Here { verb: Op::Copy, .. } => {
                self.name = basename(&step.from).to_string();
                ops::copy_in(w, &step.from, &step.to).map(|()| Done::of(w, &step.from, &step.to))
            }
            Task::Here { verb: Op::Move, .. } => {
                self.name = basename(&step.from).to_string();
                ops::move_in(w, &step.from, &step.to).map(|()| Done::of(w, &step.from, &step.to))
            }
            Task::Delete { .. } => {
                self.name = basename(&step.from).to_string();
                // `from` is where it was, `to` where the trash put it.
                ops::trash_in(w, &step.from).map(|landed| Done::of(w, &step.from, &landed))
            }
            Task::MakeDir { .. } => {
                self.name = format!("{}/", basename(&step.to));
                ops::make_dir_in(w, &step.to).map(|()| Done::of(w, &step.from, &step.to))
            }
        };
        match r {
            Ok(d) => self.done.push(d),
            Err(e) => self.refused.push(e),
        }
    }

    /// Where it has got to, for the panels' line.
    fn progress(&self) -> Progress {
        Progress {
            doing: self.run.task.doing(),
            at: self.at,
            total: self.steps.len(),
            name: self.name.clone(),
        }
    }

    /// Over: everything the UI thread needs to record it.
    fn over(self, stopped: bool, dropped: usize) -> Landed {
        Landed {
            skipped: self.left(),
            run: self.run,
            done: self.done,
            refused: self.refused,
            stopped,
            dropped,
        }
    }
}

// -- what the app holds of all this --------------------------------------------

impl super::Files {
    /// Queues a run and wakes whoever performs it.
    ///
    /// Where the passes run inline this *is* the run: the kick performs it
    /// on this very thread, and it is over by the time the call returns.
    pub(super) fn start(&self, s: &Session, task: Task, by: SlotId) {
        let inline = s.workers().is_inline();
        let world = whose(s);
        if let Ok(mut g) = self.runs.lock() {
            g.minted += 1;
            let id = g.minted;
            g.queue.push(Run {
                id,
                task,
                by,
                inline,
                world,
            });
        }
        self.moved();
        // The set of workers is re-asked here, so the thread that performs
        // this starts without waiting for the next action.
        s.workers().kick_all();
    }

    /// A run queued the way a threaded build queues one: a pass performs a
    /// single path, so a test can stand between two of them and look at
    /// what a person would have seen.
    #[cfg(test)]
    pub(super) fn queue_by_hand(&self, s: &Session, task: Task, by: SlotId) {
        let world = whose(s);
        if let Ok(mut g) = self.runs.lock() {
            g.minted += 1;
            let id = g.minted;
            g.queue.push(Run {
                id,
                task,
                by,
                inline: false,
                world,
            });
        }
        self.moved();
    }

    /// The next run to perform, taken off the queue and marked as the one
    /// in hand. `None` when there is nothing waiting.
    pub(super) fn next_run(&self) -> Option<Run> {
        let mut g = self.runs.lock().ok()?;
        if g.queue.is_empty() {
            return None;
        }
        let run = g.queue.remove(0);
        // Marked as running before a single path is attempted: this is what
        // keeps the worker from being retired between two of its own passes,
        // and what a stop reaches for.
        g.now_id = run.id;
        g.now = Some(Progress {
            doing: run.task.doing(),
            at: 0,
            total: 0,
            name: String::new(),
        });
        Some(run)
    }

    /// Says where the run in hand has got to.
    pub(super) fn showing(&self, at: Progress) {
        if let Ok(mut g) = self.runs.lock() {
            g.now = Some(at);
        }
        self.moved();
    }

    /// Files a finished run for the UI thread to record.
    pub(super) fn land(&self, l: Landed) {
        if let Ok(mut g) = self.runs.lock() {
            g.now = None;
            g.landed.push(l);
        }
        self.moved();
    }

    /// Empties the queue, answering how many runs went — what a stop does
    /// to everything waiting behind the one it stopped.
    pub(super) fn drop_queued(&self) -> usize {
        let Ok(mut g) = self.runs.lock() else {
            return 0;
        };
        let n = g.queue.len();
        g.queue.clear();
        n
    }

    /// The runs that are over and belong to this session, taken for
    /// recording. Another session's are left where they are: its own poll
    /// is the one that may record them.
    pub(super) fn take_landed(&self, s: &Session) -> Vec<Landed> {
        let Ok(mut g) = self.runs.lock() else {
            return Vec::new();
        };
        let mine = whose(s);
        let (ours, theirs) = std::mem::take(&mut g.landed)
            .into_iter()
            .partition(|l| l.run.world == mine);
        g.landed = theirs;
        ours
    }

    /// Something moved: a path performed, a run queued, a run over. The
    /// poll on the UI thread compares this against what it last saw, so a
    /// quiet frame costs one atomic read.
    pub(super) fn moved(&self) {
        self.moved.fetch_add(1, Ordering::Relaxed);
    }

    /// How far the run in hand has got, or `None` when nothing is running.
    /// What every files panel draws under its header.
    ///
    /// # Panics
    ///
    /// If a previous holder panicked while it had the queue.
    #[must_use]
    pub fn running(&self) -> Option<Progress> {
        self.runs.lock().expect("the files runs").now.clone()
    }

    /// Whether anything is running or waiting to. The worker exists exactly
    /// while this is true.
    ///
    /// # Panics
    ///
    /// As [`Files::running`](super::Files::running).
    #[must_use]
    pub fn busy(&self) -> bool {
        let g = self.runs.lock().expect("the files runs");
        g.now.is_some() || !g.queue.is_empty()
    }

    /// Stop: the path in hand finishes, the ones behind it are dropped, and
    /// what was done is recorded. What the *cancel* button does.
    ///
    /// The stop names the run it is for, so it can never reach a later one.
    /// Where nothing has been taken in hand yet there is nothing to name and
    /// nothing to record: what was waiting simply never starts.
    pub fn stop(&self) {
        if let Ok(mut g) = self.runs.lock() {
            if g.now.is_some() {
                self.stopping.store(g.now_id, Ordering::Relaxed);
            } else {
                g.queue.clear();
            }
        }
        self.moved();
    }

    /// Whether *this* run has been told to stop.
    pub(super) fn stopping(&self, run: u64) -> bool {
        self.stopping.load(Ordering::Relaxed) == run
    }

    /// Everything forgotten — the queue, the run in hand's record, and the
    /// stop. The app is a `static`, so this is how one test starts where
    /// another left off.
    #[cfg(test)]
    pub fn forget_runs(&self) {
        if let Ok(mut g) = self.runs.lock() {
            *g = Runs::new();
        }
        self.stopping.store(0, Ordering::Relaxed);
        self.moved();
    }
}
