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
use std::collections::{HashMap, HashSet, VecDeque};
use std::path::Path;
use std::rc::Rc;

use rusqlite::Transaction;

use crate::app::{Announced, Apps, Env, Mode, Problem, Workers};
use crate::effect::World;
use crate::history::{self, History, Intent, UiIntent, NodeId};
use crate::layout::{Grid, LayoutOpts, Scene, SlotId, Wm, WmSnap};
use crate::nav::Nav;
use crate::panel::{self, Open, Opening, Panel, PanelId};
use crate::store::{save_wm_tx, Store};

mod edits;
mod shutdown;
mod sync_mount;
mod walks;
mod work;
pub use edits::Edit;

/// A live panel instance, as everything that reaches one holds it.
pub type Instance = Rc<RefCell<Box<dyn Panel>>>;

/// The data half of an action or a claim: a closure that runs on the
/// writer thread, inside the action's one transaction.
pub type Write = Box<dyn FnOnce(&Transaction) -> rusqlite::Result<()> + Send>;

/// The same, answering something — how an action learns a new row id.
pub type Data<R> = Box<dyn FnOnce(&Transaction) -> rusqlite::Result<R> + Send>;

/// An opening transaction derives its undo claims from the state it changes.
pub type Claim = Data<Vec<Box<dyn Intent>>>;

/// One action as [`Session::act`] records it.
pub struct Action<R> {
    /// Layout-only actions already know their result and need no data round trip.
    pub(crate) immediate: Option<R>,
    /// The history kind (`move`, `read`, `send`). Together with `entity`
    /// it decides coalescing.
    pub kind: &'static str,
    /// The node's label, as the history overlay shows it.
    pub label: String,
    /// What the action is about, as `noun:id` (`slot:7`, `outbox:9`). A
    /// new action with the same `kind` and `entity` as the head node,
    /// within a short window, amends that node instead of adding one: five
    /// moves of one panel are one undo. Navigation uses its originating slot
    /// by default, so rapid previews may share a node. A target kind can opt
    /// out through `PanelKind::coalesce_navigation`, leaving this as `None`
    /// to keep each visit separate. `None` never coalesces. The same spelling
    /// names an effect's row in the queue and a worker's kick address, so one
    /// id means one thing everywhere.
    pub entity: Option<String>,
    /// The layout half.
    pub layout: Box<dyn FnOnce(&mut Wm)>,
    /// The data half; runs on the writer thread.
    pub data: Data<R>,
    /// What the action claims of the world.
    pub intents: Vec<Box<dyn Intent>>,
    pub ui_intents: Vec<Box<dyn UiIntent>>,
}

impl Action<()> {
    /// An action that only moves the layout.
    #[must_use]
    pub fn new(kind: &'static str, label: impl Into<String>) -> Action<()> {
        Action {
            immediate: Some(()),
            kind,
            label: label.into(),
            entity: None,
            layout: Box::new(|_| {}),
            data: Box::new(|_| Ok(())),
            intents: Vec::new(),
            ui_intents: Vec::new(),
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
            immediate: None,
            kind,
            label: label.into(),
            entity: None,
            layout: Box::new(|_| {}),
            data: Box::new(data),
            intents: Vec::new(),
            ui_intents: Vec::new(),
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

    #[must_use]
    pub fn claiming_ui(mut self, intents: Vec<Box<dyn UiIntent>>) -> Action<R> {
        self.ui_intents = intents;
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
    /// The next action is a consequence of the one before it, and folds
    /// into its node — set for the length of one
    /// [`Session::nav_within`].
    merge_next: bool,
    edits: Vec<edits::PendingEdit>,
    walk: Option<walks::PendingWalk>,
    commands: VecDeque<walks::Command>,
    events: VecDeque<walks::Command>,
    ui_claims: HashMap<NodeId, Vec<Box<dyn UiIntent>>>,
    preparations: Vec<work::PendingWork>,
    effects: Vec<work::PendingWork>,
    shutdown: shutdown::Shutdown,
    /// Device sync's one task, on a run that has an endpoint. A library
    /// mount and a scripted run that asked for none have `None`, and the
    /// *device sync* panel says so.
    sync: Option<crate::sync::Service>,
}

impl Session {
    /// The session a boot builds: the world it was given, the apps it was
    /// listed with, and an empty layout.
    #[must_use]
    pub fn new(apps: Apps, world: Rc<World>, workers: Workers) -> Session {
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
            merge_next: false,
            edits: Vec::new(),
            walk: None,
            commands: VecDeque::new(),
            events: VecDeque::new(),
            ui_claims: HashMap::new(),
            preparations: Vec::new(),
            effects: Vec::new(),
            shutdown: shutdown::Shutdown::Running,
            sync: None,
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
        let store = Rc::new(Store::open(None, &apps.schemas(), crate::sync::Device::fake().replicating(apps.replicated())).expect("in-memory store"));
        apps.seed(&store, mode).expect("the apps' demo rows");
        let world = Rc::new(World::new(
            store,
            apps.capabilities(mode, env),
            apps.registry(),
        ));
        let workers = Workers::inline(list, world.clone());
        Session::new(apps, world, workers)
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

    /// What stands right now: every app's sources asked, and device sync's
    /// own — which is the kernel's, because the roster is.
    #[must_use]
    pub fn problems(&self) -> Vec<Problem> {
        let mut out = self.apps.problems(&self.store);
        if let Some(service) = &self.sync {
            out.extend(service.problems());
        }
        out
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
    pub fn history(&self) -> history::View {
        self.walk.as_ref().map_or_else(|| self.history.view(), |walk| walk.view.clone())
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
        self.relayout_restoring(None);
    }

    /// A history transition preserves the viewport when the workspace and
    /// geometry stay the same and restored focus is at least partly on-screen.
    /// Otherwise, bring focus into view so keyboard input has a visible target.
    fn relayout_restoring(&mut self, from_workspace: Option<usize>) {
        let cols = self.cols;
        let mut wishes: HashMap<PanelId, (u32, u32)> = HashMap::new();
        for ws in &mut self.wm.wss {
            ws.widths.clear();
            for slot in ws.slots.values() {
                if let Some(width) = self.instances.get(&slot.id).and_then(|i| i.borrow().width()) {
                    ws.widths.insert(slot.id, width);
                }
                let wish = self
                    .instances
                    .get(&slot.id)
                    .map(|i| i.borrow().wish(cols))
                    .unwrap_or(crate::layout::DEFAULT_WISH);
                wishes.insert(slot.show.clone(), wish);
            }
        }
        self.wm.set_wishes(wishes);
        let keep_camera = from_workspace == Some(self.wm.active) && {
            let scene = self.wm.scene(self.viewport, self.opts);
            scene.slots == self.scene.slots && scene.focus.is_none_or(|focus| {
                scene.slots.iter().any(|slot| slot.id == focus && slot.visible
                    && slot.rect.right() > scene.camera_x
                    && slot.rect.x < scene.camera_x + self.viewport.0)
            })
        };
        if !keep_camera {
            self.wm.ensure_focus_visible(self.viewport, self.opts);
        }
        self.scene = self.wm.scene(self.viewport, self.opts);
        self.dirty.layout = true;
        self.dirty.redraw = true;
    }

    /// The panel's bar plus controls it opted into through the panel contract.
    pub fn panel_verbs(&self, slot: SlotId) -> Vec<crate::panel::Verb> {
        use crate::panel::{PanelWidth, Verb};
        let Some(instance) = self.panel(slot) else { return Vec::new() };
        let panel = instance.borrow();
        let mut verbs = panel.verbs();
        if let Some(width) = panel.width() {
            let (id, label, next) = match width {
                PanelWidth::Half => ("panel.full_width", "full width", PanelWidth::Full),
                PanelWidth::Full => ("panel.half_width", "half width", PanelWidth::Half),
            };
            verbs.push(Verb::call(id, label, None, move |s| { s.set_panel_width(slot, next); }));
        }
        verbs
    }

    /// Resize an opted-in panel without replacing its live instance.
    pub fn set_panel_width(&mut self, slot: SlotId, width: crate::panel::PanelWidth) {
        let Some(instance) = self.panel(slot) else { return };
        {
            let mut panel = instance.borrow_mut();
            if panel.width().is_none() { return; }
            panel.set_width(width);
        }
        self.relayout();
        self.save();
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

    /// Activates a slot's tab and asks the camera to show it once. A preview does this for its
    /// child, because focus stayed behind and nothing else would; a *go to*
    /// that found the panel already focused does it for the same reason,
    /// there being no move for the layout to follow.
    pub(crate) fn show_camera_at(&mut self, slot: SlotId) {
        self.wm.activate(slot);
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
    ///
    /// The apps are polled first, before the check for whether anything
    /// moved: a background pass that has just finished *is* something
    /// moving, and what it claims must be settled in the same breath.
    pub fn settle(&mut self) {
        if !self.shutdown.accepts_completions() { return; }
        self.poll_events();
        self.poll_work();
        self.poll_edits();
        self.poll_walk();
        self.poll_commands();
        self.poll_apps();
        self.poll_events();
        self.settle_layout(None);
    }

    /// Reconcile and publish a layout without polling more commands. A
    /// history restore must finish here before queued work can observe it.
    fn settle_layout(&mut self, from_workspace: Option<usize>) {
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
        self.relayout_restoring(from_workspace);
        self.save();
    }

    /// Every app's [`App::poll`](crate::app::App::poll), in list order:
    /// what each has finished off this thread and now wants a session for.
    ///
    /// The list is `'static`, so nothing of this session is borrowed across
    /// the calls and an app may act, claim and close from inside one.
    fn poll_apps(&mut self) {
        for a in self.apps.list() {
            a.poll(self);
        }
    }

    // -- actions ---------------------------------------------------------------

    /// One undoable action: mutates the layout, writes the session and
    /// `data` in one transaction on the writer thread, records a history
    /// node with the layout before and after plus the intents, then kicks
    /// the workers. Returns what `data` returned, which is how an action
    /// learns a new row id.
    ///
    /// It touches no instance: the slots it opened and closed are settled
    /// afterwards by [`Session::settle`], so the verb that ran it may hold
    /// its own `&mut self` across the call.
    pub fn act<R: Send + 'static>(&mut self, mut a: Action<R>) -> Option<R> {
        if self.walk_pending() {
            if let Some(result) = a.immediate.take() {
                self.commands.push_back(Box::new(move |session| {
                    session.act(Action { immediate: Some(()), kind: a.kind, label: a.label,
                        entity: a.entity, layout: a.layout, data: Box::new(|_| Ok(())),
                        intents: a.intents, ui_intents: a.ui_intents });
                }));
                return Some(result);
            }
            self.notify("wait for the undo operation to finish", false);
            return None;
        }
        let Action {
            immediate,
            kind,
            label,
            entity,
            layout,
            data,
            intents,
            ui_intents,
        } = a;
        let before = self.wm.snapshot();
        layout(&mut self.wm);
        let after = self.wm.snapshot();
        // The layout as it stands, not as the instances would have it
        // saved: an instance whose `persist` differs is written by the
        // `save` that follows the settle.
        let snap = after.clone();
        let out = if let Some(result) = immediate.filter(|_| self.store.ui_attached()) {
            self.submit_layout(snap);
            result
        } else {
            let out = self.store.write(move |tx| {
            let r = data(tx)?;
            save_wm_tx(tx, &snap)?;
            Ok(r)
        });
        let out = match out {
            Ok(v) => v,
            Err(e) => {
                // The transaction rolled back, so the layout must go back
                // too, keeping the current display state.
                self.wm.apply_snapshot(before);
                self.unsettle();
                self.notify(format!("the store refused: {e}"), true);
                return None;
            }
        };
            out
        };
        let ts = self.now();
        let node = history::Action {
            kind,
            label,
            entity,
            before,
            after: after.clone(),
            intents,
            ts,
        };
        let id = if std::mem::take(&mut self.merge_next) {
            self.history.amend(node)
        } else {
            self.history.apply(node)
        };
        self.ui_claims.entry(id).or_default().extend(ui_intents);
        self.trim_ui_claims();
        self.last_saved = Some(after);
        self.unsettle();
        self.workers.kick_all();
        self.announce_problems();
        Some(out)
    }

    /// A navigation an action *caused*, folded into the node that caused
    /// it: the cursor walk a filing leaves behind is not a second gesture,
    /// and one gesture is one undo. Call it in place of [`Session::nav`],
    /// straight after the [`Session::act`] whose consequence it is.
    ///
    /// The walk still lands in the node's `after`, so redo replays it —
    /// and its own claims (the mail the new preview marks read) come back
    /// with the action's on the one press.
    pub fn nav_within(&mut self, n: Nav) {
        self.merge_next = true;
        self.nav(n);
        // A navigation that recorded nothing — a focus move — would
        // otherwise leave the flag standing over whatever came next.
        self.merge_next = false;
    }

    /// Adds an intent to the head node after the fact, for an action whose
    /// claim needs the row id [`Session::act`] returned.
    pub fn claim(&mut self, intent: Box<dyn Intent>) {
        if self.walk_pending() {
            self.commands.push_back(Box::new(move |session| session.claim(intent)));
            return;
        }
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
        self.begin_walk(walks::Direction::Undo)
    }

    /// Walks one node forward.
    pub fn redo(&mut self) -> bool {
        self.begin_walk(walks::Direction::Redo)
    }

    /// Walks to any node; `0` is the beginning.
    pub fn travel(&mut self, node: NodeId) -> bool {
        self.begin_walk(walks::Direction::Travel(node))
    }

    fn walked(&mut self, step: Option<history::Step>) -> bool {
        let Some(step) = step else {
            return false;
        };
        for (id, reverse) in &step.visits {
            if let Some(claims) = self.ui_claims.get(id) {
                for claim in claims {
                    if *reverse { claim.reverse(); } else { claim.reapply(); }
                }
            }
        }
        let from_workspace = self.wm.active;
        self.wm.apply_snapshot(step.snap);
        self.show_once = None;
        // A walk is nobody's `&mut self`: it comes from a chord or the
        // history overlay. Restore instances, geometry and focus together
        // before any queued command or app poll sees the restored layout.
        self.unsettle();
        self.settle_layout(Some(from_workspace));
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
    /// session is shared. The grid still belongs to this screen.
    ///
    /// Answers whether there was a session to restore.
    pub fn restore(&mut self) -> bool {
        let Ok(Some(snap)) = self.store.load_wm() else {
            return false;
        };
        let grid = self.wm.grid;
        self.wm = Wm::restore(snap);
        self.wm.set_grid(grid);
        self.sync_instances();
        self.pending.clear();
        self.last_saved = Some(self.persist_snapshot());
        self.relayout();
        // A restored layout needs no user action to start its services.
        self.workers.kick_all();
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
        if self.store.ui_attached() {
            self.submit_layout(snap.clone());
        } else if let Err(e) = self.store.save_wm(&snap) {
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
        // Placement consults the new panel's height before it has a slot.
        // In particular, a full-height panel must not be packed into the
        // spare half of another column using DEFAULT_WISH.
        let (w, h) = instance.wish(self.cols);
        let w = instance.width().map_or(w, |width| width.units(self.wm.grid));
        self.wm.wish(&id, (w, h));
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

    #[test]
    fn restoring_starts_background_work() {
        struct Background;
        #[async_trait::async_trait(?Send)]
        impl crate::app::Worker for Background {
            fn name(&self) -> String { "restored-background".into() }
            fn claims(&self, _: &crate::effect::Job) -> bool { false }
            async fn pass(&mut self, world: &crate::effect::World) -> crate::app::Wake {
                world.store().write_async(|tx| {
                    tx.execute("INSERT INTO meta(key,value) VALUES('background-started','yes')", [])?;
                    Ok(())
                }).await.unwrap();
                crate::app::Wake::OnKick
            }
        }
        impl App for Background {
            fn id(&self) -> &'static str { "restored-background" }
            fn kinds(&self) -> &'static [&'static dyn PanelKind] { &[] }
            fn workers(&self, _: &Store) -> Vec<Box<dyn crate::app::Worker>> {
                vec![Box::new(Background)]
            }
            fn as_any(&self) -> &dyn Any { self }
        }
        static BACKGROUND: Background = Background;
        static RESTORING_APPS: &[&dyn App] = &[&NOTES, &BACKGROUND];

        let mut saved = Session::fake(APPS);
        open(&mut saved, note("saved"));
        let apps = crate::app::Apps::new(RESTORING_APPS);
        let world = Rc::new(apps.world(Store::with_db(saved.store().db()).unwrap(), Mode::Fake, &Env::default()));
        let workers = Workers::inline(RESTORING_APPS, world.clone());
        let mut restored = Session::new(apps, world, workers);
        assert!(restored.restore());
        let ran: bool = restored.store().conn().query_row(
            "SELECT EXISTS(SELECT 1 FROM meta WHERE key='background-started')", [], |row| row.get(0),
        ).unwrap();
        assert!(ran, "restoring resumes background work without a user action");
    }

    /// A saved desktop layout can arrive after a phone's first frame or a
    /// fold/unfold. Restoring it must not turn that screen back into 12×6.
    #[test]
    fn a_restore_keeps_the_current_screens_grid_on_every_workspace() {
        let mut saved = Session::fake(APPS);
        for workspace in [0, 2] {
            saved.switch(workspace);
            for column in 0..3 {
                open(&mut saved, note(&format!("{workspace}/{column}")));
            }
        }
        let snapshot = saved.ws().snapshot();
        let mut restored = Session::new(
            crate::app::Apps::new(APPS),
            saved.world().clone(),
            Workers::none(saved.store().clone()),
        );

        for (grid, viewport, panel_width, visible_columns) in [
            (Grid { w: 4, h: 3 }, (380.0, 780.0), 364.0, 1),
            (Grid { w: 8, h: 4 }, (720.0, 780.0), 348.0, 2),
            (Grid { w: 4, h: 3 }, (380.0, 780.0), 364.0, 1),
        ] {
            restored.set_grid(grid);
            restored.set_viewport(viewport);
            // A role change may restore again without another resize.
            for _ in 0..2 {
                assert!(restored.restore());
                let scene = restored.scene();
                assert!(scene.slots.iter().all(|p| (p.rect.w - panel_width).abs() < 0.01),
                    "{grid:?} must keep its panel widths: {scene:?}");
                let on_screen = scene.slots.iter().filter(|p| p.visible
                    && p.rect.right() > scene.camera_x
                    && p.rect.x < scene.camera_x + viewport.0).count();
                assert_eq!(on_screen, visible_columns);
                assert_eq!(restored.viewport(), viewport);
                assert!(restored.ws().wss.iter().all(|ws| ws.grid == grid),
                    "inactive and empty workspaces must keep the screen's grid too");
                assert_eq!(restored.ws().snapshot(), snapshot,
                    "restoring still brings back the saved panels and focus");
            }
        }
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

    /// An action whose data half refuses leaves neither the layout nor the
    /// store half-moved.
    #[test]
    fn a_refused_write_puts_the_layout_back() {
        let mut s = Session::fake(APPS);
        let grid = Grid { w: 4, h: 3 };
        s.set_grid(grid);
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
        assert!(
            s.ws().wss.iter().all(|ws| ws.grid == grid),
            "rollback keeps the current screen's grid on every workspace"
        );
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
        );
        assert!(fresh.restore());
        assert_eq!(
            fresh.panel(slot).unwrap().borrow().title(),
            format!("Some({slot})")
        );
    }

    #[test]
    fn undo_and_redo_keep_the_current_screens_grid() {
        let mut s = Session::fake(APPS);
        open(&mut s, note("first"));
        open(&mut s, note("second"));
        // A phone or a folded screen can have a different grid from the
        // one on which the history nodes were recorded.
        let grid = Grid { w: 4, h: 3 };
        s.set_grid(grid);
        assert!(s.undo());
        assert_eq!(s.ws().grid, grid);
        assert!(s.showing(&note("second")).is_empty());
        assert!(s.redo());
        assert_eq!(s.ws().grid, grid);
        assert_eq!(s.showing(&note("second")).len(), 1);
    }

    #[test]
    fn content_undo_and_redo_preserve_cameras_and_panel_geometry() {
        let mut s = Session::fake(APPS);
        for i in 0..4 { open(&mut s, note(&format!("prefix {i}"))); }
        let list = open(&mut s, note("list"));
        let reader = open(&mut s, note("before"));
        s.switch(1);
        for i in 0..5 { open(&mut s, note(&format!("other {i}"))); }
        s.pan(-80.0);
        s.switch(0);
        s.nav(Nav::Focus(list));
        s.settle();
        s.reveal(reader);
        s.nav(Nav::Replace { slot: reader, id: note("after") });
        s.settle();

        // A content change must also respect a subsequent free camera pan.
        for pan in [0.0, -90.0] {
            s.pan(pan);
            let cameras: Vec<_> = s.ws().wss.iter().map(|ws| ws.camera_x).collect();
            assert!(cameras[0] > 0.0 && cameras[1] > 0.0);
            let slots = s.scene().slots.clone();
            for undo in [true, false] {
                assert!(if undo { s.undo() } else { s.redo() });
                assert_eq!(s.scene().slots, slots, "changing the reader preserves every panel rectangle");
                assert_eq!(s.ws().wss.iter().map(|ws| ws.camera_x).collect::<Vec<_>>(), cameras,
                    "history must not move any camera when the geometry is unchanged");
                assert_eq!(s.focus(), Some(if undo { list } else { reader }));
            }
        }
    }

    #[test]
    fn undo_and_redo_reveal_offscreen_focus_when_reader_geometry_is_unchanged() {
        let mut s = Session::fake(APPS);
        let left = open(&mut s, note("left"));
        for i in 0..6 { open(&mut s, note(&format!("spacer {i}"))); }
        let list = open(&mut s, note("list"));
        s.nav(Nav::Open { from: list, id: note("before"), fresh: false });
        s.settle();
        let reader = s.joined_child(list).unwrap();
        s.nav(Nav::Focus(left));
        s.settle();
        // Pan to the list without changing focus, then click another row.
        s.pan(10_000.0);
        s.nav(Nav::Select { from: list, id: note("after"), fresh: false });
        s.settle();
        if let Some(slot) = s.take_show_once() { s.reveal(slot); }
        let slots = s.scene().slots.clone();
        let left_rect = slots.iter().find(|slot| slot.id == left).unwrap().rect;
        let list_rect = slots.iter().find(|slot| slot.id == list).unwrap().rect;
        assert!(left_rect.right() < s.scene().camera_x, "previous focus is fully off-screen to the left");

        assert!(s.undo());
        assert_eq!(s.scene().slots, slots, "undo only replaces the reader's contents");
        assert_eq!(s.focus(), Some(left));
        assert_eq!(s.panel(reader).unwrap().borrow().id(), &note("before"));
        let camera = s.scene().camera_x;
        assert!(left_rect.x >= camera && left_rect.right() <= camera + s.viewport().0,
            "undo must reveal the restored focus instead of leaving it off-screen");
        assert!(list_rect.x > camera + s.viewport().0, "redo's focus is now fully off-screen to the right");

        assert!(s.redo());
        assert_eq!(s.scene().slots, slots, "redo also keeps every panel in place");
        assert_eq!(s.focus(), Some(list));
        assert_eq!(s.panel(reader).unwrap().borrow().id(), &note("after"));
        let camera = s.scene().camera_x;
        assert!(list_rect.x >= camera && list_rect.right() <= camera + s.viewport().0,
            "redo must reveal the restored focus on the other side of the strip");
    }

    #[test]
    fn undo_and_redo_follow_focus_when_panels_actually_close_or_reopen() {
        let mut s = Session::fake(APPS);
        for i in 0..5 { open(&mut s, note(&format!("prefix {i}"))); }
        let last = open(&mut s, note("last"));
        let open_scene = s.scene().clone();
        s.nav(Nav::Close { slot: last, label: None });
        s.settle();
        let closed_scene = s.scene().clone();
        assert!(closed_scene.camera_x < open_scene.camera_x);
        assert!(closed_scene.slots.iter().all(|slot| slot.id != last));
        assert!(s.undo());
        assert_eq!(s.focus(), Some(last));
        assert_eq!(s.scene(), &open_scene, "reopening reveals the restored panel");
        assert!(s.redo());
        assert_eq!(s.scene(), &closed_scene, "closing clamps the camera to the shorter strip");
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

}
