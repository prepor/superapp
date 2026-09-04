//! The one object a verb, an instance, or a widget acts on.
//!
//! [`Session`] is the kernel's: the shell holds one, lends it to widgets
//! through the scope (`&mut` during events, shared during draws), and after
//! every event reads its dirty flags to relayout or redraw. Nothing bubbles
//! up to the stage.
//!
//! There is no context bag, no hold, no list interface, no command type, and
//! no per-kind refresh: an instance holds its own context, a clipboard is an
//! app's, a list is a component inside a panel, and an app that changed the
//! world walks [`Session::panels`] and refreshes its own.

use std::cell::RefCell;
use std::collections::{HashMap, HashSet};
use std::path::Path;
use std::rc::Rc;
use std::sync::Arc;

use rusqlite::Transaction;

use crate::app::{Announced, Apps, Env, Mode, Problem, Workers};
use crate::effect::World;
use crate::history::{self, History, Intent, NodeId};
use crate::layout::{Grid, LayoutOpts, Scene, SlotId, Wm, WmSnap};
use crate::panel::{self, Open, Opening, Panel, PanelId};
use crate::repl;
use crate::store::{save_wm_tx, Store};

mod repl_mount;

pub use repl_mount::{ReplChange, ReplMount};
use repl_mount::Repl;

/// A live panel instance, as everything that reaches one holds it.
pub type Instance = Rc<RefCell<Box<dyn Panel>>>;

/// The data half of an action or a claim: a closure that runs on the
/// writer thread, inside the action's one transaction.
pub type Write = Box<dyn FnOnce(&Transaction) -> rusqlite::Result<()> + Send>;

/// The same, answering something — how an action learns a new row id.
pub type Data<R> = Box<dyn FnOnce(&Transaction) -> rusqlite::Result<R> + Send>;

/// What an opening panel claimed of the world: a write, and the intents
/// that reverse it.
pub type Claim = (Write, Vec<Box<dyn Intent>>);

/// One action as [`Session::act`] records it.
pub struct Action<R> {
    /// The history kind (`move`, `read`, `send`). Together with `entity`
    /// it decides coalescing.
    pub kind: &'static str,
    /// The node's label, as the history overlay shows it.
    pub label: String,
    /// What the action is about, as `noun:id` (`slot:7`, `outbox:9`). A
    /// new action with the same `kind` and `entity` as the head node,
    /// within a short window, amends that node instead of adding one: five
    /// moves of one panel are one undo, and a cursor walk that previews a
    /// row at a time is one undo that closes the whole walk. `None` never
    /// coalesces. The same spelling names an effect's row in the queue and
    /// a worker's kick address, so one id means one thing everywhere.
    pub entity: Option<String>,
    /// The layout half.
    pub layout: Box<dyn FnOnce(&mut Wm)>,
    /// The data half; runs on the writer thread.
    pub data: Data<R>,
    /// What the action claims of the world.
    pub intents: Vec<Box<dyn Intent>>,
}

impl Action<()> {
    /// An action that only moves the layout.
    #[must_use]
    pub fn new(kind: &'static str, label: impl Into<String>) -> Action<()> {
        Action {
            kind,
            label: label.into(),
            entity: None,
            layout: Box::new(|_| {}),
            data: Box::new(|_| Ok(())),
            intents: Vec::new(),
        }
    }
}

impl<R> Action<R> {
    /// An action that writes, and answers what its write returned — how an
    /// action learns a new row id.
    #[must_use]
    pub fn writing(
        kind: &'static str,
        label: impl Into<String>,
        data: impl FnOnce(&Transaction) -> rusqlite::Result<R> + Send + 'static,
    ) -> Action<R> {
        Action {
            kind,
            label: label.into(),
            entity: None,
            layout: Box::new(|_| {}),
            data: Box::new(data),
            intents: Vec::new(),
        }
    }

    /// What it is about, for coalescing and for kicks.
    #[must_use]
    pub fn about(mut self, entity: impl Into<String>) -> Action<R> {
        self.entity = Some(entity.into());
        self
    }

    /// The layout half.
    #[must_use]
    pub fn moving(mut self, f: impl FnOnce(&mut Wm) + 'static) -> Action<R> {
        self.layout = Box::new(f);
        self
    }

    /// What it claims of the world.
    #[must_use]
    pub fn claiming(mut self, intents: Vec<Box<dyn Intent>>) -> Action<R> {
        self.intents = intents;
        self
    }
}

/// A line for the person, as the shell drew it and a test reads it back.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Note {
    pub msg: String,
    /// Whether it is a failure — drawn in the one colour.
    pub err: bool,
}

/// What changed since the shell last looked.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Dirty {
    /// The scene moved: spring towards the new targets.
    pub layout: bool,
    /// Something on screen is stale.
    pub redraw: bool,
}

impl Dirty {
    #[must_use]
    pub fn any(self) -> bool {
        self.layout || self.redraw
    }
}

/// How wide a column is in characters when nobody has said. A panel's
/// [`Panel::wish`] is asked with this until the shell measures its face.
const DEFAULT_COLS: usize = 60;

/// The viewport a session lays out for until the shell says otherwise.
const DEFAULT_VIEWPORT: (f64, f64) = (1440.0, 900.0);

/// The whole surface a verb, an instance, or a widget acts on.
pub struct Session {
    store: Rc<Store>,
    world: Rc<World>,
    apps: Apps,
    wm: Wm,
    history: History,
    /// The live instances, by slot.
    instances: HashMap<SlotId, Instance>,
    /// An instance built for a slot the running action is about to create,
    /// so the open is not run twice.
    pending: Vec<(PanelId, Instance)>,
    /// The layout moved and the instances have not caught up with it yet —
    /// what [`Session::settle`] answers to.
    unsettled: bool,
    notes: Vec<Note>,
    workers: Workers,
    announced: Announced,
    dirty: Dirty,
    scene: Scene,
    last_saved: Option<WmSnap>,
    viewport: (f64, f64),
    opts: LayoutOpts,
    cols: usize,
    /// A slot the camera should show once — what a preview asks for.
    show_once: Option<SlotId>,
    /// Device sync, when a bucket is configured. `None` means replication
    /// is off and the store is a plain local one.
    repl: Option<Repl>,
    /// How the driver is mounted, and what wakes the shell after a pass —
    /// kept so [`Session::connect_bucket`] can restart onto another bucket.
    repl_mount: Option<(ReplMount, Arc<dyn Fn() + Send + Sync>)>,
    /// The lease status the last pass reported — what the locked screen
    /// draws.
    lease: repl::Status,
    /// Whether the demo rows have gone in since this device began holding.
    /// Seeding is a holder-only act under replication: a would-be follower
    /// must not write a world it is about to replace with the holder's.
    seeded: bool,
    /// Which outside those rows are written for when it does — the same
    /// mode a boot seeds a plain store with.
    seed_mode: Mode,
}

impl Session {
    /// The session a boot builds: the world it was given, the apps it was
    /// listed with, the outside its demo rows are written for, and an empty
    /// layout.
    #[must_use]
    pub fn new(apps: Apps, world: Rc<World>, workers: Workers, seed_mode: Mode) -> Session {
        let store = world.store().clone();
        Session {
            store,
            world,
            apps,
            wm: Wm::new(),
            history: History::new(),
            instances: HashMap::new(),
            pending: Vec::new(),
            unsettled: false,
            notes: Vec::new(),
            workers,
            announced: Announced::new(),
            dirty: Dirty::default(),
            scene: Scene {
                camera_x: 0.0,
                slots: Vec::new(),
                bridges: Vec::new(),
                focus: None,
            },
            last_saved: None,
            viewport: DEFAULT_VIEWPORT,
            opts: LayoutOpts::default(),
            cols: DEFAULT_COLS,
            show_once: None,
            repl: None,
            repl_mount: None,
            lease: repl::Status::default(),
            seeded: false,
            seed_mode,
        }
    }

    /// A session over an in-memory store with fake capabilities and the
    /// passes running inline — what a test drives, and what a library mount
    /// gets.
    ///
    /// # Panics
    ///
    /// If SQLite cannot open an in-memory database, or an app's seed fails.
    #[must_use]
    pub fn fake(list: &'static [&'static dyn crate::app::App]) -> Session {
        Session::fake_with(list, &Env::default())
    }

    /// The same, over an environment a caller arranged — one clock shared
    /// with something else, a planted secret.
    ///
    /// # Panics
    ///
    /// If SQLite cannot open an in-memory database, or an app's seed fails.
    #[must_use]
    pub fn fake_with(list: &'static [&'static dyn crate::app::App], env: &Env) -> Session {
        Session::fake_mode(list, Mode::Fake, env)
    }

    /// The same, with the outside said: a library mount over a panel that
    /// reads nothing beyond its store takes [`Mode::Deny`], so an effect it
    /// files fails in words instead of quietly working.
    ///
    /// # Panics
    ///
    /// If SQLite cannot open an in-memory database, or an app's seed fails.
    #[must_use]
    pub fn fake_mode(
        list: &'static [&'static dyn crate::app::App],
        mode: Mode,
        env: &Env,
    ) -> Session {
        let apps = Apps::new(list);
        let store = Rc::new(Store::open(None, &apps.schemas()).expect("in-memory store"));
        apps.seed(&store, mode).expect("the apps' demo rows");
        let world = Rc::new(World::new(
            store,
            apps.capabilities(mode, env),
            apps.registry(),
        ));
        let workers = Workers::inline(list, world.clone());
        Session::new(apps, world, workers, mode)
    }

    // -- what everything reads ------------------------------------------------

    #[must_use]
    pub fn store(&self) -> &Rc<Store> {
        &self.store
    }

    #[must_use]
    pub fn world(&self) -> &Rc<World> {
        &self.world
    }

    #[must_use]
    pub fn apps(&self) -> &Apps {
        &self.apps
    }

    /// The world's clock.
    #[must_use]
    pub fn now(&self) -> f64 {
        self.world.now()
    }

    /// The directory beside the store; `None` in memory.
    #[must_use]
    pub fn db_dir(&self) -> Option<&Path> {
        self.store.dir()
    }

    /// Whether this device may write at all. A follower's store refuses,
    /// and a verb that touches the disk must ask *before* it acts — the
    /// disk would take the write even where the store will not.
    ///
    /// With no bucket there is no lease to lose, so the gate is whatever the
    /// store says and nothing shuts it.
    #[must_use]
    pub fn writable(&self) -> bool {
        self.repl.is_none() || self.store.is_writable()
    }

    /// The layout, for the shell to read. Every mutation of it goes through
    /// [`Session::act`] or [`Session::nav`].
    #[must_use]
    pub fn ws(&self) -> &Wm {
        &self.wm
    }

    /// The last computed scene — what the shell springs towards.
    #[must_use]
    pub fn scene(&self) -> &Scene {
        &self.scene
    }

    /// The instance in a slot. Apps downcast their own through `as_any`.
    #[must_use]
    pub fn panel(&self, slot: SlotId) -> Option<Instance> {
        self.instances.get(&slot).cloned()
    }

    /// Every slot on every workspace with its instance, by slot id.
    #[must_use]
    pub fn panels(&self) -> Vec<(SlotId, Instance)> {
        let mut v: Vec<(SlotId, Instance)> = self
            .instances
            .iter()
            .map(|(s, i)| (*s, i.clone()))
            .collect();
        v.sort_by_key(|(s, _)| *s);
        v
    }

    /// Every slot showing exactly this identity.
    #[must_use]
    pub fn showing(&self, id: &PanelId) -> Vec<SlotId> {
        self.wm.showing(id)
    }

    #[must_use]
    pub fn focus(&self) -> Option<SlotId> {
        self.wm.focus
    }

    #[must_use]
    pub fn joined_child(&self, slot: SlotId) -> Option<SlotId> {
        self.wm
            .ws_of(slot)
            .and_then(|k| self.wm.wss[k].joined_child(slot))
    }

    #[must_use]
    pub fn join_parent_of(&self, slot: SlotId) -> Option<SlotId> {
        self.wm
            .ws_of(slot)
            .and_then(|k| self.wm.wss[k].join_parent_of(slot))
    }

    /// `kick_all()` wakes every worker and re-asks the apps for the set;
    /// [`Session::act`] does this itself. `kick(entity)` wakes one.
    /// `any()` says whether anything is running at all.
    #[must_use]
    pub fn workers(&self) -> &Workers {
        &self.workers
    }

    /// What stands right now, every app's sources asked.
    #[must_use]
    pub fn problems(&self) -> Vec<Problem> {
        self.apps.problems(&self.store)
    }

    /// The instant half of the launcher: every open slot, the active
    /// workspace first, each under its instance's title. The session is the
    /// one thing that has both the layout and the instances.
    #[must_use]
    pub fn windows(&self) -> Vec<crate::launcher::Window> {
        let mut order: Vec<usize> = (0..crate::layout::WS_N).collect();
        order.sort_by_key(|&k| (k != self.wm.active, k));
        let mut out = Vec::new();
        for k in order {
            let ws = &self.wm.wss[k];
            for slot in ws.columns.iter().flat_map(|c| c.slots.iter()) {
                let Some(s) = ws.slots.get(slot) else {
                    continue;
                };
                let title = self
                    .instances
                    .get(slot)
                    .map(|i| i.borrow().title())
                    .unwrap_or_else(|| s.show.to_string());
                out.push(crate::launcher::Window {
                    slot: *slot,
                    ws: k,
                    id: s.show.clone(),
                    title,
                });
            }
        }
        out
    }

    /// The launcher's roots, apps in list order.
    #[must_use]
    pub fn roots(&self) -> Vec<crate::app::Root> {
        self.apps.roots()
    }

    /// The undo tree, for the overlay that draws it.
    #[must_use]
    pub fn history(&self) -> &History {
        &self.history
    }

    // -- what the shell drives ------------------------------------------------

    /// The viewport the layout is computed for. Answers whether it changed.
    pub fn set_viewport(&mut self, viewport: (f64, f64)) -> bool {
        if self.viewport == viewport {
            return false;
        }
        self.viewport = viewport;
        self.relayout();
        true
    }

    /// How wide a column is in characters, once the shell has measured its
    /// face. Answers whether it changed.
    pub fn set_cols(&mut self, cols: usize) -> bool {
        let cols = cols.max(1);
        if self.cols == cols {
            return false;
        }
        self.cols = cols;
        self.relayout();
        true
    }

    /// The unit grid the viewport is cut into, once the shell has read the
    /// screen (or argv). Ephemeral, like the camera: never snapshotted, so
    /// it is not an action. Answers whether it changed.
    pub fn set_grid(&mut self, grid: Grid) -> bool {
        if self.wm.grid == grid {
            return false;
        }
        self.wm.set_grid(grid);
        self.relayout();
        true
    }

    /// Brings a slot into view without focusing it — what a preview asks
    /// for through [`Session::take_show_once`]. The relayout that follows
    /// puts focus back on screen, so focus still wins where both cannot be
    /// shown at once.
    pub fn reveal(&mut self, slot: SlotId) {
        let (viewport, opts) = (self.viewport, self.opts);
        self.wm.ensure_visible(slot, viewport, opts);
        self.relayout();
    }

    /// Goes to a workspace. Not an action, for the same reason
    /// [`Nav::Focus`](crate::nav::Nav::Focus) is not one: nothing is
    /// claimed of the world, so there is nothing to give back. Answers
    /// whether anything moved.
    pub fn switch(&mut self, k: usize) -> bool {
        if !self.wm.switch(k) {
            return false;
        }
        self.save();
        self.relayout();
        true
    }

    /// Walks focus one panel in a direction. Context, like
    /// [`Session::switch`]: never an undo node.
    pub fn focus_dir(&mut self, dir: crate::layout::Dir) -> bool {
        let (viewport, opts) = (self.viewport, self.opts);
        let was = self.wm.focus;
        self.wm.focus_dir(dir, viewport, opts);
        if self.wm.focus == was {
            return false;
        }
        self.save();
        self.relayout();
        true
    }

    /// Pans the camera by `dx` points — a trackpad, 1:1 and un-sprung. Not
    /// a relayout: the person is dragging the strip, not asking to be taken
    /// anywhere, so nothing pulls the camera back onto focus.
    pub fn pan(&mut self, dx: f64) {
        self.wm.pan(dx);
        self.scene = self.wm.scene(self.viewport, self.opts);
        self.dirty.redraw = true;
    }

    /// Magnetises a freely panned camera to the nearest column alignment —
    /// what a two-finger pan asks for when the fingers lift. Ephemeral like
    /// the pan it ends, so it is not an action either; the shell springs
    /// towards the result.
    pub fn snap_camera(&mut self) {
        let (viewport, opts) = (self.viewport, self.opts);
        self.wm.snap_camera(viewport, opts);
        self.scene = self.wm.scene(viewport, opts);
        self.dirty.redraw = true;
    }

    #[must_use]
    pub fn viewport(&self) -> (f64, f64) {
        self.viewport
    }

    #[must_use]
    pub fn opts(&self) -> LayoutOpts {
        self.opts
    }

    /// Asks every instance what it wants, records the wishes, and recomputes
    /// the scene. The wishes are re-derived rather than kept, so a panel
    /// nothing shows any more drops out.
    pub fn relayout(&mut self) {
        let cols = self.cols;
        let mut wishes: HashMap<PanelId, (u32, u32)> = HashMap::new();
        for ws in &self.wm.wss {
            for slot in ws.slots.values() {
                let wish = self
                    .instances
                    .get(&slot.id)
                    .map(|i| i.borrow().wish(cols))
                    .unwrap_or(crate::layout::DEFAULT_WISH);
                wishes.insert(slot.show.clone(), wish);
            }
        }
        self.wm.set_wishes(wishes);
        self.wm.ensure_focus_visible(self.viewport, self.opts);
        self.scene = self.wm.scene(self.viewport, self.opts);
        self.dirty.layout = true;
        self.dirty.redraw = true;
    }

    /// What changed since the last look, taken.
    pub fn take_dirty(&mut self) -> Dirty {
        std::mem::take(&mut self.dirty)
    }

    /// Something on screen is stale.
    pub fn redraw(&mut self) {
        self.dirty.redraw = true;
    }

    /// A slot the camera should show once — what a preview asked for.
    pub fn take_show_once(&mut self) -> Option<SlotId> {
        self.show_once.take()
    }

    /// Asks the camera to show a slot once. A preview does this for its
    /// child, because focus stayed behind and nothing else would.
    pub(crate) fn show_camera_at(&mut self, slot: SlotId) {
        self.show_once = Some(slot);
    }

    /// The layout has moved and the instances have not caught up with it:
    /// what [`Session::settle`] answers to.
    pub(crate) fn unsettle(&mut self) {
        self.unsettled = true;
    }

    /// Focuses a slot wherever it lives, switching workspaces if needed.
    /// Answers whether anything moved. Not an action: nothing is claimed,
    /// so there is nothing to undo.
    pub(crate) fn focus_slot(&mut self, slot: SlotId) -> bool {
        if self.wm.focus == Some(slot) && self.wm.ws_of(slot) == Some(self.wm.active) {
            return false;
        }
        if self.wm.focus_slot(slot).is_none() {
            return false;
        }
        self.dirty.layout = true;
        self.dirty.redraw = true;
        true
    }

    /// A line for the person; `err` marks it as one. The shell draws it as
    /// a toast; a test reads it back.
    pub fn notify(&mut self, msg: impl Into<String>, err: bool) {
        self.notes.push(Note {
            msg: msg.into(),
            err,
        });
        self.dirty.redraw = true;
    }

    /// What has been said and not yet drawn.
    #[must_use]
    pub fn notes(&self) -> &[Note] {
        &self.notes
    }

    /// The same, drained — what the shell does once it has toasted them.
    pub fn take_notes(&mut self) -> Vec<Note> {
        std::mem::take(&mut self.notes)
    }

    /// Drops the instances of slots that closed and places the ones that
    /// opened. `act` and `nav` never touch instances themselves, so a verb
    /// may hold its own `&mut self` across them; the shell calls this after
    /// every event, and a test calls it before looking at the slots.
    pub fn settle(&mut self) {
        if !self.unsettled {
            return;
        }
        self.unsettled = false;
        self.sync_instances();
        // Whatever the action built and did not use — an open whose write
        // was refused — goes here, with nowhere to be placed.
        self.pending.clear();
        // The wishes and the saved session are read off the instances, so
        // both wait for this point too.
        self.relayout();
        self.save();
    }

    // -- actions ---------------------------------------------------------------

    /// One undoable action: mutates the layout, writes the session and
    /// `data` in one transaction on the writer thread, records a history
    /// node with the layout before and after plus the intents, then kicks
    /// the workers and replication. Refuses with a toast when not writable.
    /// Returns what `data` returned, which is how an action learns a new row
    /// id.
    ///
    /// It touches no instance: the slots it opened and closed are settled
    /// afterwards by [`Session::settle`], so the verb that ran it may hold
    /// its own `&mut self` across the call.
    pub fn act<R: Send + 'static>(&mut self, a: Action<R>) -> Option<R> {
        if !self.writable() {
            self.notify("another device holds the lease — nothing was written", true);
            return None;
        }
        self.act_done(a)
    }

    /// [`Session::act`] without the write gate, for a claim that has already
    /// happened on the disk: the caller checked [`Session::writable`] before
    /// acting and records the node whatever the lease did in between.
    pub fn act_done<R: Send + 'static>(&mut self, a: Action<R>) -> Option<R> {
        let Action {
            kind,
            label,
            entity,
            layout,
            data,
            intents,
        } = a;
        let before = self.wm.snapshot();
        layout(&mut self.wm);
        let after = self.wm.snapshot();
        // The layout as it stands, not as the instances would have it
        // saved: an instance whose `persist` differs is written by the
        // `save` that follows the settle.
        let snap = after.clone();
        let out = self.store.write(move |tx| {
            let r = data(tx)?;
            save_wm_tx(tx, &snap)?;
            Ok(r)
        });
        let out = match out {
            Ok(v) => v,
            Err(e) => {
                // The transaction rolled back, so the layout must go back
                // too: half an action is not an action.
                self.wm = Wm::restore(before);
                self.unsettle();
                self.notify(format!("the store refused: {e}"), true);
                return None;
            }
        };
        let ts = self.now();
        self.history.apply(history::Action {
            kind,
            label,
            entity,
            before,
            after: after.clone(),
            intents,
            ts,
        });
        self.last_saved = Some(after);
        self.unsettle();
        self.workers.kick_all();
        // And publish what was just captured to the other device promptly.
        self.repl_kick();
        self.announce_problems();
        Some(out)
    }

    /// The compensation when the lease turned over between a disk write and
    /// its node: reverses the intent and answers the sentence to toast, or
    /// `None` when the device is still writable.
    pub fn give_back(&mut self, intent: &dyn Intent) -> Option<String> {
        if self.writable() {
            return None;
        }
        Some(match intent.reverse(&self.world) {
            Ok(()) => format!(
                "{} was given back — another device holds the lease",
                intent.describe()
            ),
            Err(e) => format!("{} could not be given back: {e}", intent.describe()),
        })
    }

    /// Adds an intent to the head node after the fact, for an action whose
    /// claim needs the row id [`Session::act`] returned.
    pub fn claim(&mut self, intent: Box<dyn Intent>) {
        self.history.claim(intent);
    }

    /// Reconcile what has been announced now rather than at the next poll,
    /// so the next failure of the same key is news again.
    pub fn announce_problems(&mut self) {
        let now = self.problems();
        for said in self.announced.reconcile(&now) {
            self.notify(said, true);
        }
    }

    // -- undo ------------------------------------------------------------------

    /// Walks one node back. Answers whether anything moved.
    pub fn undo(&mut self) -> bool {
        let step = self.history.undo(&self.world);
        self.walked(step)
    }

    /// Walks one node forward.
    pub fn redo(&mut self) -> bool {
        let step = self.history.redo(&self.world);
        self.walked(step)
    }

    /// Walks to any node; `0` is the beginning.
    pub fn travel(&mut self, node: NodeId) -> bool {
        let step = self.history.travel(&self.world, node);
        self.walked(step)
    }

    fn walked(&mut self, step: Option<history::Step>) -> bool {
        let Some(step) = step else {
            return false;
        };
        self.wm = Wm::restore(step.snap);
        // A walk is nobody's `&mut self`: it comes from a chord or the
        // history overlay, so the instances settle within the call.
        self.unsettle();
        self.settle();
        let word = if step.undone { "undid" } else { "redid" };
        let said = format!("{word} {}{}", step.label, history::said(&step.failed));
        self.notify(said, !step.failed.is_empty());
        self.workers.kick_all();
        true
    }

    // -- the session on disk ----------------------------------------------------

    /// Restores the layout the store kept, opening every saved slot with
    /// [`Open::Restore`]. A tag no app in this build owns is kept, not
    /// dropped: it gets a [`Missing`](crate::panel::Missing) instance and
    /// persists back unchanged, because another build has the app and the
    /// session is shared.
    ///
    /// Answers whether there was a session to restore.
    pub fn restore(&mut self) -> bool {
        let Ok(Some(snap)) = self.store.load_wm() else {
            return false;
        };
        self.wm = Wm::restore(snap);
        self.sync_instances();
        self.pending.clear();
        self.last_saved = Some(self.persist_snapshot());
        self.relayout();
        true
    }

    /// Writes the layout when it has changed since the last write — the
    /// un-undoable upkeep a workspace switch or a focus move is. What each
    /// slot saves as is its instance's [`Panel::persist`].
    ///
    /// Answers whether anything was written.
    pub fn save(&mut self) -> bool {
        let snap = self.persist_snapshot();
        if self.last_saved.as_ref() == Some(&snap) {
            return false;
        }
        if let Err(e) = self.store.save_wm(&snap) {
            eprintln!("session: saving the layout failed: {e}");
            return false;
        }
        self.last_saved = Some(snap);
        true
    }

    /// The layout as the store keeps it: every slot under the identity its
    /// instance wants saved. A job panel on an in-memory effect saves as
    /// the effects list, because ring ids do not survive the process.
    fn persist_snapshot(&self) -> WmSnap {
        let mut snap = self.wm.snapshot();
        for ws in &mut snap.wss {
            for (sid, show) in &mut ws.slots {
                if let Some(i) = self.instances.get(sid) {
                    *show = i.borrow().persist();
                }
            }
        }
        snap
    }

    // -- instances --------------------------------------------------------------

    /// Builds one instance and collects what its open claimed. `None` for a
    /// tag no app owns, which gets a [`Missing`](crate::panel::Missing).
    pub(crate) fn open_instance(&self, id: &PanelId, how: Open) -> (Box<dyn Panel>, Vec<Claim>) {
        let mut cx = Opening::new(self, how);
        let instance = match self.apps.kind(id.tag) {
            Some(kind) => kind.open(id, &mut cx),
            None => panel::missing(id),
        };
        (instance, cx.claimed)
    }

    /// An instance the caller built, to be placed on the slot the running
    /// action creates.
    pub(crate) fn place(&mut self, id: PanelId, instance: Box<dyn Panel>) {
        self.pending.push((id, Rc::new(RefCell::new(instance))));
    }

    /// Every slot has exactly one instance of what it shows: closed slots
    /// give theirs up, new ones take the pending one if there is one and a
    /// restore otherwise.
    ///
    /// The session lets its handle go; a widget that is still holding one
    /// holds an [`Rc`] of its own, so the instance whose verb just closed
    /// its slot lives until that handle does.
    fn sync_instances(&mut self) {
        let live: HashSet<SlotId> = self
            .wm
            .wss
            .iter()
            .flat_map(|w| w.slots.keys().copied())
            .collect();
        let stale: Vec<SlotId> = self
            .instances
            .keys()
            .copied()
            .filter(|s| !live.contains(s))
            .collect();
        for s in stale {
            self.instances.remove(&s);
        }
        let want: Vec<(SlotId, PanelId)> = self
            .wm
            .wss
            .iter()
            .flat_map(|w| w.slots.values().map(|s| (s.id, s.show.clone())))
            .collect();
        for (sid, show) in want {
            if self
                .instances
                .get(&sid)
                .is_some_and(|i| i.borrow().id() == &show)
            {
                continue;
            }
            let instance = match self.pending.iter().position(|(id, _)| *id == show) {
                Some(at) => self.pending.remove(at).1,
                None => Rc::new(RefCell::new(self.open_instance(&show, Open::Restore).0)),
            };
            // Where it landed. The open could not say: the slot did not
            // exist until the layout half of this action placed it.
            instance.borrow_mut().placed(sid);
            self.instances.insert(sid, instance);
        }
    }
}

impl std::fmt::Debug for Session {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Session")
            .field("slots", &self.instances.len())
            .field("focus", &self.wm.focus)
            .field("workspace", &self.wm.active)
            .field("notes", &self.notes.len())
            .finish()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::{App, Root};
    use crate::panel::{Missing, Opening, Panel, PanelKind, Tag};
    use std::any::Any;

    const NOTE: Tag = Tag("note");

    fn note(text: &str) -> PanelId {
        PanelId::new(NOTE, [text])
    }

    struct NotePanel(PanelId);
    impl Panel for NotePanel {
        fn id(&self) -> &PanelId {
            &self.0
        }
        fn title(&self) -> String {
            self.0.arg(0).unwrap_or("note").to_string()
        }
        /// A note asks for the rows its text needs, measured against the
        /// column — the one thing a wish is for.
        fn wish(&self, cols: usize) -> (u32, u32) {
            let lines = self.title().len().div_ceil(cols.max(1));
            (4, (lines as u32).clamp(2, 6))
        }
        fn as_any(&mut self) -> &mut dyn Any {
            self
        }
    }

    struct NoteKind;
    impl PanelKind for NoteKind {
        fn tag(&self) -> Tag {
            NOTE
        }
        fn open(&self, id: &PanelId, _cx: &mut Opening<'_>) -> Box<dyn Panel> {
            Box::new(NotePanel(id.clone()))
        }
    }
    static NOTE_KIND: NoteKind = NoteKind;
    static KINDS: &[&dyn PanelKind] = &[&NOTE_KIND];

    struct Notes;
    impl App for Notes {
        fn id(&self) -> &'static str {
            "notes"
        }
        fn kinds(&self) -> &'static [&'static dyn PanelKind] {
            KINDS
        }
        fn roots(&self) -> Vec<Root> {
            vec![Root::new(note("scratch"), "scratch", "jot")]
        }
        fn as_any(&self) -> &dyn Any {
            self
        }
    }
    static NOTES: Notes = Notes;
    static APPS: &[&dyn App] = &[&NOTES];

    fn open(s: &mut Session, id: PanelId) -> SlotId {
        s.act(Action::new("open", format!("open {id}")).moving(move |wm| {
            wm.open(id, None, false);
        }));
        s.settle();
        s.focus().expect("the new slot has focus")
    }

    /// A tag no app in this build owns is kept, not dropped: it opens as a
    /// `Missing` that says so, and it saves back exactly as it was — another
    /// build has the app, and the session is shared.
    #[test]
    fn an_unknown_tag_opens_as_missing_and_persists_unchanged() {
        let alien = PanelId::new(Tag("from_the_future"), ["7"]);
        let mut s = Session::fake(APPS);
        let slot = open(&mut s, alien.clone());

        let inst = s.panel(slot).expect("an instance all the same");
        assert_eq!(inst.borrow().title(), "from_the_future(7)");
        assert!(inst.borrow_mut().as_any().is::<Missing>());
        assert_eq!(Missing::line(), "no app for this panel in this build");
        assert_eq!(inst.borrow().wish(60), crate::layout::DEFAULT_WISH);

        // The action already wrote it; the row is the identity as given.
        let saved = s.store().load_wm().unwrap().expect("a session");
        assert_eq!(saved.wss[0].slots, vec![(slot, alien.clone())]);

        // And a restore off that row gives the same panel back.
        let mut fresh = Session::new(
            crate::app::Apps::new(APPS),
            s.world().clone(),
            Workers::none(s.store().clone()),
            Mode::Fake,
        );
        assert!(fresh.restore());
        assert_eq!(fresh.panels().len(), 1);
        assert_eq!(
            fresh.panel(slot).unwrap().borrow().title(),
            "from_the_future(7)"
        );
        assert!(!fresh.save(), "nothing to write: it came back as itself");
    }

    /// Restoring opens every saved slot, and a known tag comes back as its
    /// own instance.
    #[test]
    fn a_restore_opens_every_saved_slot() {
        let mut s = Session::fake(APPS);
        let a = open(&mut s, note("first"));
        let b = open(&mut s, note("second"));

        let mut fresh = Session::new(
            crate::app::Apps::new(APPS),
            s.world().clone(),
            Workers::none(s.store().clone()),
            Mode::Fake,
        );
        assert!(fresh.restore());
        assert_eq!(fresh.panels().len(), 2);
        assert_eq!(fresh.panel(a).unwrap().borrow().title(), "first");
        assert_eq!(fresh.panel(b).unwrap().borrow().title(), "second");
        assert_eq!(fresh.focus(), Some(b));

        // A store nobody has booted has no session to restore.
        let mut empty = Session::fake(APPS);
        assert!(!empty.restore());
    }

    /// The wishes come off the instances, and a wider column changes them.
    #[test]
    fn a_relayout_asks_every_instance_what_it_wants() {
        let mut s = Session::fake(APPS);
        let long = "a note long enough to need more than one line of any column";
        let slot = open(&mut s, note(long));
        s.set_cols(20);
        let tall = s.ws().wish_of(&note(long));
        s.set_cols(200);
        let short = s.ws().wish_of(&note(long));
        assert!(tall.1 > short.1, "{tall:?} vs {short:?}");
        assert_eq!(short, (4, 2), "a short note asks for its floor");

        // The scene follows.
        assert_eq!(s.scene().slots.len(), 1);
        assert_eq!(s.scene().slots[0].id, slot);
        // …and a panel nothing shows drops out of the wishes.
        s.nav(crate::nav::Nav::Close { slot, label: None });
        s.settle();
        assert!(s.ws().wishes.is_empty());
    }

    /// The dirty flags are what the shell reads after every event.
    #[test]
    fn the_dirty_flags_say_what_moved() {
        let mut s = Session::fake(APPS);
        s.take_dirty();
        assert_eq!(s.take_dirty(), Dirty::default());
        assert!(!Dirty::default().any());

        open(&mut s, note("one"));
        let d = s.take_dirty();
        assert!(d.layout && d.redraw && d.any());
        assert_eq!(s.take_dirty(), Dirty::default(), "taken once");

        s.redraw();
        let d = s.take_dirty();
        assert!(d.redraw && !d.layout);

        // The viewport is only a relayout when it changed.
        assert!(s.set_viewport((800.0, 600.0)));
        assert!(s.take_dirty().layout);
        assert!(!s.set_viewport((800.0, 600.0)));
        assert!(!s.set_cols(60), "the default, unchanged");
    }

    /// What the session says out loud is a queue a test reads back.
    #[test]
    fn notes_queue_up_and_drain() {
        let mut s = Session::fake(APPS);
        s.notify("saved", false);
        s.notify("could not", true);
        assert_eq!(s.notes().len(), 2);
        let taken = s.take_notes();
        assert_eq!(taken[0].msg, "saved");
        assert!(!taken[0].err);
        assert!(taken[1].err);
        assert!(s.notes().is_empty(), "drained");
    }

    /// A shut gate refuses an action with a line, and writes nothing.
    ///
    /// With a bucket configured the session refuses first, in the lease's
    /// words; `act_done` skips that gate — the caller has already touched
    /// the disk — and meets the store's own, which is shut too.
    #[test]
    fn a_closed_gate_refuses_an_action() {
        let mut s = Session::fake(APPS);
        s.mount_repl(ReplMount::Inline, || {});
        s.start_repl_with(Arc::new(crate::repl::object::MemBucket::new()));
        assert!(!s.writable(), "shut until the first pass answers");
        assert!(s
            .act(Action::new("open", "open").moving(|wm| {
                wm.open(PanelId::bare(NOTE), None, false);
            }))
            .is_none());
        assert!(s.panels().is_empty(), "nothing was opened");
        let said = s.take_notes();
        assert!(said[0].err);
        assert!(said[0].msg.contains("lease"), "{:?}", said[0].msg);

        assert!(s
            .act_done(Action::new("open", "open").moving(|wm| {
                wm.open(PanelId::bare(NOTE), None, false);
            }))
            .is_none());
        assert!(s.take_notes()[0].msg.starts_with("the store refused"));

        // With the lease taken, the same action lands.
        s.repl_poll();
        assert!(s.writable(), "the first pass made it the holder");
        assert!(s
            .act_done(Action::new("open", "open").moving(|wm| {
                wm.open(PanelId::bare(NOTE), None, false);
            }))
            .is_some());
        s.settle();
        assert_eq!(s.panels().len(), 1);
    }

    /// An action whose data half refuses leaves neither the layout nor the
    /// store half-moved.
    #[test]
    fn a_refused_write_puts_the_layout_back() {
        let mut s = Session::fake(APPS);
        let first = open(&mut s, note("one"));
        s.take_notes();
        let out: Option<()> = s.act(
            Action::writing("open", "open", |_| {
                Err(rusqlite::Error::QueryReturnedNoRows)
            })
            .moving(|wm| {
                wm.open(PanelId::bare(NOTE), None, false);
            }),
        );
        assert!(out.is_none());
        s.settle();
        assert_eq!(s.panels().len(), 1, "the layout went back");
        assert_eq!(s.focus(), Some(first));
        assert!(s.take_notes()[0].msg.starts_with("the store refused"));
    }

    /// An action learns what its write returned — how a new row id gets
    /// back to the caller.
    #[test]
    fn an_action_answers_what_it_wrote() {
        let mut s = Session::fake(APPS);
        let id = s.act(Action::writing("note", "write", |tx| {
            tx.execute("INSERT INTO meta(key, value) VALUES('x', 1)", [])?;
            Ok(tx.last_insert_rowid())
        }));
        assert!(id.is_some());
    }

    /// The launcher's instant half: open slots by their instance's title,
    /// the active workspace leading, plus the apps' roots.
    #[test]
    fn the_windows_are_the_open_slots_by_title() {
        let mut s = Session::fake(APPS);
        let a = open(&mut s, note("first"));
        s.act(Action::new("switch", "to 3").moving(|wm| {
            wm.switch(2);
        }));
        s.settle();
        let b = open(&mut s, note("second"));

        let windows = s.windows();
        assert_eq!(windows.len(), 2);
        assert_eq!(windows[0].slot, b, "the active workspace leads");
        assert_eq!(windows[0].ws, 2);
        assert_eq!(windows[0].title, "second");
        assert_eq!(windows[1].slot, a);
        assert_eq!(windows[1].ws, 0);

        assert_eq!(s.roots().len(), 1);
        assert_eq!(s.roots()[0].label, "scratch");
        assert!(s.apps().get("notes").is_some());
        assert!(s.db_dir().is_none(), "in memory");
        assert!(s.now() > 0.0);
        assert!(format!("{s:?}").contains("Session"));
    }

    /// A slot that closes keeps its instance until the settle, and the
    /// settle is what lets it go: the verb that closed it was running as
    /// `&mut self` on the very panel.
    #[test]
    fn a_closed_slot_drops_its_instance_at_the_settle() {
        let mut s = Session::fake(APPS);
        let slot = open(&mut s, note("one"));
        let held = s.panel(slot).expect("an instance");
        assert_eq!(Rc::strong_count(&held), 2, "the map and us");

        s.nav(crate::nav::Nav::Close { slot, label: None });
        assert!(s.panel(slot).is_some(), "nothing has settled yet");
        s.settle();
        assert!(s.panel(slot).is_none(), "the slot is gone");
        // The instance is still alive: a widget may be holding it, as we
        // are here.
        assert_eq!(held.borrow().title(), "one");
        assert_eq!(Rc::strong_count(&held), 1, "only us");
    }

    /// An instance is told where it landed, which is what lets its bar
    /// carry a link: every `Nav` names a slot, and an open cannot.
    #[test]
    fn an_instance_is_told_which_slot_it_landed_in() {
        struct Placed(PanelId, Option<SlotId>);
        impl Panel for Placed {
            fn id(&self) -> &PanelId {
                &self.0
            }
            fn title(&self) -> String {
                format!("{:?}", self.1)
            }
            fn placed(&mut self, slot: SlotId) {
                self.1 = Some(slot);
            }
            fn as_any(&mut self) -> &mut dyn Any {
                self
            }
        }
        struct Kind;
        impl PanelKind for Kind {
            fn tag(&self) -> Tag {
                Tag("placed")
            }
            fn open(&self, id: &PanelId, _cx: &mut Opening<'_>) -> Box<dyn Panel> {
                Box::new(Placed(id.clone(), None))
            }
        }
        static KIND: Kind = Kind;
        static KINDS: &[&dyn PanelKind] = &[&KIND];
        struct A;
        impl App for A {
            fn id(&self) -> &'static str {
                "placed"
            }
            fn kinds(&self) -> &'static [&'static dyn PanelKind] {
                KINDS
            }
            fn as_any(&self) -> &dyn Any {
                self
            }
        }
        static A_: A = A;
        static LIST: &[&dyn App] = &[&A_];

        let mut s = Session::fake(LIST);
        let slot = open(&mut s, PanelId::bare(Tag("placed")));
        assert_eq!(
            s.panel(slot).unwrap().borrow().title(),
            format!("Some({slot})")
        );

        // …and again on a restore, where the instance is built by the
        // session rather than by the action.
        let mut fresh = Session::new(
            crate::app::Apps::new(LIST),
            s.world().clone(),
            Workers::none(s.store().clone()),
            Mode::Fake,
        );
        assert!(fresh.restore());
        assert_eq!(
            fresh.panel(slot).unwrap().borrow().title(),
            format!("Some({slot})")
        );
    }

    /// The three knobs the shell turns that are not actions: the grid, the
    /// camera under a trackpad, and the reveal a preview asks for. None of
    /// them is snapshotted, so none of them is undoable.
    #[test]
    fn the_shell_moves_the_grid_and_the_camera_without_an_action() {
        let mut s = Session::fake(APPS);
        for i in 0..8 {
            open(&mut s, note(&format!("note {i}")));
        }
        let nodes = s.history().rows().0.len();

        assert!(s.set_grid(crate::layout::Grid { w: 4, h: 3 }));
        assert!(!s.set_grid(crate::layout::Grid { w: 4, h: 3 }), "unchanged");
        assert_eq!(s.ws().grid, crate::layout::Grid { w: 4, h: 3 });

        // A pan moves the camera and asks for a redraw, and nothing pulls
        // it back onto focus.
        s.take_dirty();
        let cam = s.scene().camera_x;
        s.pan(-200.0);
        let d = s.take_dirty();
        assert!(d.redraw && !d.layout, "a pan is not a relayout");
        assert!(s.scene().camera_x < cam, "the strip moved");

        // A reveal brings a slot on screen; a relayout follows, so focus is
        // visible too.
        let first = s.panels()[0].0;
        s.reveal(first);
        assert!(s.take_dirty().layout);

        // Going somewhere is attention, not an action, whether the
        // somewhere is a workspace or the panel next door.
        assert!(s.switch(2));
        assert!(!s.switch(2), "already there");
        assert_eq!(s.ws().active, 2);
        assert!(s.switch(0));
        assert!(s.focus_dir(crate::layout::Dir::Left));
        assert!(!s.focus_dir(crate::layout::Dir::Up), "nothing above");

        assert_eq!(s.history().rows().0.len(), nodes, "no node for any of it");
    }

    /// Problems are announced once and reconciled on demand.
    #[test]
    fn problems_are_announced_once() {
        let s = Session::fake(APPS);
        assert!(s.problems().is_empty(), "an app with no sources");
    }

    /// The lease reaches the shell through the session, and it is what
    /// closes the write gate: a follower's `act` is refused with a line, and
    /// taking the lease opens it again.
    #[test]
    fn the_lease_gates_the_session_and_is_readable_from_it() {
        let bucket = Arc::new(crate::repl::object::MemBucket::new());

        // The holder: another device, on the same bucket.
        let mut holder = Session::fake(APPS);
        holder.mount_repl(ReplMount::Inline, || {});
        holder.start_repl_with(bucket.clone());
        assert!(holder.repl_poll().role, "the first pass gave it a role");
        assert_eq!(
            holder.lease().map(|l| l.role.clone()),
            Some(repl::Role::Holder)
        );
        assert!(holder.writable());

        // This device follows it: read-only, and it says who has it.
        let mut s = Session::fake(APPS);
        assert!(s.lease().is_none(), "no bucket, no lease to lose");
        assert!(s.writable());
        s.mount_repl(ReplMount::Inline, || {});
        s.start_repl_with(bucket.clone());
        assert!(!s.writable(), "shut until the first pass answers");
        s.repl_poll();
        let lease = s.lease().expect("a lease status").clone();
        assert!(
            matches!(lease.role, repl::Role::Follower { .. }),
            "{:?}",
            lease.role
        );
        assert!(!s.writable());
        assert!(!lease.device.is_empty());
        assert_eq!(lease.role.locked_screen().1, Some("take over"));

        s.take_notes();
        assert!(s
            .act(Action::new("open", "open").moving(|wm| {
                wm.open(PanelId::bare(NOTE), None, false);
            }))
            .is_none());
        assert!(s.take_notes()[0].msg.contains("lease"));

        // Taking it over opens the gate, and the same action lands.
        s.repl_acquire();
        assert_eq!(s.lease().map(|l| l.role.clone()), Some(repl::Role::Holder));
        assert!(s.writable());
        assert!(s
            .act(Action::new("open", "open").moving(|wm| {
                wm.open(PanelId::bare(NOTE), None, false);
            }))
            .is_some());
        s.settle();
        assert_eq!(s.panels().len(), 1);

        // …and handing it back shuts it again.
        s.repl_release();
        assert_eq!(s.lease().map(|l| l.role.clone()), Some(repl::Role::Free));
        assert!(!s.writable());
        s.repl_release_blocking();
    }
}
