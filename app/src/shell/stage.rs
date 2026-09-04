//! The stage: the widget that owns the session and draws it.
//!
//! ```text
//! Event::{Key,Mouse}* ──▶ Session ──▶ take_dirty ──▶ Anim::apply ──▶ redraw
//! Event::NextFrame ─────▶ Anim::advance(dt) ──▶ redraw while anything moves
//! ```
//!
//! The scene is recomputed after a state change, never on every frame, and
//! frames are only asked for while something is actually moving.

use std::collections::HashMap;

use kernel::caps::ClockSource;
use kernel::launcher;
use kernel::layout::{Grid, SlotId};
use kernel::session::{Action, Session};
use kernel::theme;
use makepad_widgets::*;

use super::anim::Anim;
use super::boot::{Boot, FRAME_MS};
use super::draw::{self, CellFont, DrawFlat, DrawPanel};
use super::dsl::LauncherOverlayWidgetRefExt;
use super::dsl::OverlayAction;
use super::e2e::E2E_TICK_MS;
use super::hits::Hits;
use super::hosted::{Hosted, OVERLAY_LAUNCHER};
use super::keys::CmdTap;
use super::menu::MenuSig;
use super::overlays::Overlay;
use super::touch::TouchNav;

/// A line the session said, and when — on the world's clock, so a toast
/// fades by the same amount on every run.
#[derive(Debug, Clone)]
pub struct Toast {
    pub msg: String,
    pub err: bool,
    pub at: f64,
}

/// Everything the shell keeps beside the session.
///
/// The session is the kernel's whole surface; this is the shell's own half:
/// what is being animated, what is hovered, which overlay is up, and what
/// the launcher has been asked.
pub struct Shell {
    pub session: Session,
    pub anim: Anim,
    pub viewport: DVec2,
    pub last_frame: Option<std::time::Instant>,
    pub hover: Option<super::hits::Act>,
    pub toasts: Vec<Toast>,
    pub overlay: Overlay,
    /// The overlay most recently up — what a close fade keeps drawing while
    /// the chassis' presence spring runs out.
    pub overlay_last: Overlay,
    pub launcher: launcher::Search,
    /// What time the app thinks it is. Virtual under a headless build, so
    /// every deadline moves with the script rather than with the machine.
    pub clock: ClockSource,
    pub virtual_time: bool,
    /// A forced grid (`--grid`).
    pub grid: Option<Grid>,
}

/// The safe area a cutout or a rounded corner leaves the workspace. Zero
/// everywhere but a phone.
#[derive(Debug, Clone, Copy, Default)]
pub struct Insets {
    pub top: f64,
    pub right: f64,
    pub bottom: f64,
    pub left: f64,
}

/// The grid a viewport is cut into.
///
/// Desktop is always 12×6. Android picks by width: 8×4 on the unfolded
/// screen and 4×3 on the cover display, the ~600 dp compact/medium
/// breakpoint a fold or unfold crosses. `--grid` overrides both, which is
/// how a desktop run previews a phone.
fn grid_for(vp: DVec2, forced: Option<Grid>) -> Grid {
    if let Some(g) = forced {
        return g;
    }
    if cfg!(target_os = "android") {
        if vp.x >= 600.0 {
            Grid { w: 8, h: 4 }
        } else {
            Grid { w: 4, h: 3 }
        }
    } else {
        Grid::default()
    }
}

/// Opens one panel, fresh, as the whole of a session — what a library mount
/// on a scene's node comes up as. Settled at once, so the slot it landed in
/// can be read back.
fn open_fresh(session: &mut Session, id: &kernel::panel::PanelId) {
    let label = format!("open “{}”", id.tag);
    let show = id.clone();
    let open = Action::new("open", label).moving(move |wm| {
        wm.open(show, None, false);
    });
    session.act(open);
    session.settle();
}

/// The first root the app list offers: where a store nobody has booted comes
/// up.
fn open_first_root(session: &mut Session) {
    let roots = session.roots();
    let Some(root) = roots.first() else {
        return;
    };
    let (id, label) = (root.id.clone(), root.label.clone());
    let open = Action::new("open", format!("open “{label}”")).moving(move |wm| {
        wm.open(id, None, false);
    });
    session.act(open);
}

/// The widget that owns the workspace and draws it.
#[derive(Script, Widget)]
pub struct Stage {
    #[uid]
    uid: WidgetUid,
    #[walk]
    walk: Walk,
    #[layout]
    layout: Layout,

    /// Content templates by DSL name, and the live widget per slot — the
    /// `PortalList` pattern at panel scale.
    #[rust]
    pub tpl: HashMap<LiveId, ScriptObjectRef>,
    #[rust]
    pub hosted: Hosted,
    /// What each hosted widget was built for. A slot replaced in place
    /// keeps its number and so its widget; this is how the shell notices
    /// the identity under that widget changed.
    #[rust]
    pub hosted_for: HashMap<SlotId, (LiveId, kernel::panel::PanelId)>,
    /// What each hosted widget last said it keeps while one of its fields
    /// has the keyboard ([`Chord::field`](super::hosted::Chord::field)).
    /// Read by the bars, which are drawn before the bodies that report.
    #[rust]
    pub field_keeps: HashMap<SlotId, super::keys::Letters>,
    /// A widget that wants the keyboard on the next event tick, rather than
    /// during the draw that created it.
    #[rust]
    pub pending_focus: Option<SlotId>,

    #[redraw]
    #[live]
    pub draw_panel: DrawPanel,
    #[live]
    pub draw_flat: DrawFlat,
    #[live]
    pub draw_mono: DrawText,

    #[rust]
    pub area: Area,
    #[rust]
    pub next_frame: NextFrame,
    #[rust]
    pub origin: DVec2,
    #[rust]
    pub cell: CellFont,
    #[rust]
    pub hits: Hits,
    #[rust]
    pub cmd_tap: CmdTap,
    /// Every finger on the glass, and the gesture they add up to.
    #[rust]
    pub touch: TouchNav,
    /// A row mid-sweep and the curtain over it. It outlives the finger: a
    /// committed sweep keeps animating until the curtain has covered the row.
    #[rust]
    pub row_swipe: Option<super::touch::RowSwipe>,
    /// The insertion bar previewing where a dragged panel would land, in
    /// strip coordinates.
    #[rust]
    pub drag_hint: Option<kernel::layout::Rect>,
    /// The soft keyboard's bottom occlusion, in points. The workspace is
    /// shortened by it, so the panels make room themselves.
    #[rust]
    pub kb_h: f64,
    /// What the screen does not lend the workspace.
    #[rust]
    pub insets: Insets,
    #[rust]
    pub e2e: Option<kernel::e2e::Runner>,
    /// A `shot` step that has asked the rasterizer for its own frame and is
    /// waiting for it ([`PendingShot`](super::e2e::PendingShot)).
    #[rust]
    pub shot: Option<super::e2e::PendingShot>,
    /// A panels-library mount: booted by the canvas from a scene's node, it
    /// replays its steps and owns nothing outside its own pass. The
    /// window's own stage boots from argv and owns the keyboard.
    #[rust]
    mount: bool,
    /// Whether this stage may take the window's key focus — always for the
    /// window's own, for a mount only while the canvas has entered it.
    #[rust]
    active: bool,
    /// A mount's replay reached its last step: from here it is a picture.
    #[rust]
    arrived: bool,
    /// A mount's last step has not been drawn yet, so the next one waits:
    /// a step that resolves a label must see the state after the last.
    #[rust]
    stale_hits: bool,
    /// A panel node: the one slot this stage draws, at the whole viewport,
    /// in place of the workspace.
    #[rust]
    solo: Option<SlotId>,
    /// The panels library is up over this stage: it draws nothing and hears
    /// nothing, while its store and its workers keep turning.
    #[rust]
    suspended: bool,
    /// A mount's own draw list and the canvas's: what a scoped redraw marks,
    /// so one mount's keystroke does not re-lay-out a hundred others.
    #[rust]
    lists: Option<(DrawListId, DrawListId)>,
    #[rust]
    e2e_timer: Timer,
    #[rust]
    poll_timer: Timer,
    /// Nothing is rasterized (`--no-draw`), so a `shot` is logged.
    #[rust]
    pub no_draw: bool,
    #[rust]
    reported: bool,
    /// What the macOS menu bar was last built from ([`menu`](super::menu)).
    /// The bar is rebuilt when this changes and at no other time.
    #[rust]
    pub menu_sig: MenuSig,
    /// `SUPERAPP_FRAME_LOG`: how many events of each kind have gone by since
    /// the last draw, and when that draw was.
    #[rust]
    since_draw: Vec<(&'static str, u32)>,
    #[rust]
    last_draw: Option<std::time::Instant>,
    #[rust]
    shell: Option<Box<Shell>>,
}

/// One word for an event, for the frame log.
fn event_kind(e: &Event) -> &'static str {
    match e {
        Event::NextFrame(_) => "next-frame",
        Event::KeyDown(_) => "key-down",
        Event::KeyUp(_) => "key-up",
        Event::TextInput(_) => "text",
        Event::MouseDown(_) => "mouse-down",
        Event::MouseMove(_) => "mouse-move",
        Event::MouseUp(_) => "mouse-up",
        Event::Scroll(_) => "scroll",
        Event::TouchUpdate(_) => "touch",
        Event::LongPress(_) => "long-press",
        Event::Timer(_) => "timer",
        Event::Signal => "signal",
        Event::Actions(_) => "actions",
        _ => "other",
    }
}

impl ScriptHook for Stage {
    fn on_before_apply(
        &mut self,
        _vm: &mut ScriptVm,
        apply: &Apply,
        _scope: &mut Scope,
        _value: ScriptValue,
    ) {
        if apply.is_reload() {
            self.tpl.clear();
        }
    }

    fn on_after_apply(
        &mut self,
        vm: &mut ScriptVm,
        apply: &Apply,
        _scope: &mut Scope,
        value: ScriptValue,
    ) {
        // Named children of this custom-drawn widget are content templates,
        // never auto-drawn — collect them rooted, `PortalList`-style.
        if apply.is_eval() {
            return;
        }
        if let Some(obj) = value.as_object() {
            vm.vec_with(obj, |vm, vec| {
                for kv in vec {
                    if let (Some(id), Some(t)) = (kv.key.as_id(), kv.value.as_object()) {
                        self.tpl.insert(id, vm.bx.heap.new_object_ref(t));
                    }
                }
            });
        }
    }
}

impl Stage {
    /// Brings the stage up on a world: opens (or creates) its store, seeds
    /// the demo rows, restores the session, and arms a script if there is
    /// one.
    pub fn boot(&mut self, cx: &mut Cx, boot: Boot) {
        if self.shell.is_some() {
            return;
        }
        self.no_draw = boot.no_draw;
        self.mount = !boot.primary;
        self.active = boot.primary;
        // A mount's first step waits for its first draw; a mount with no
        // steps is its state from the start.
        self.stale_hits = self.mount;
        self.arrived = self.mount && boot.steps.is_none();
        let (session, clock) = boot.session();
        let mut sh = Box::new(Shell {
            session,
            anim: Anim::default(),
            viewport: dvec2(1440.0, 900.0),
            last_frame: None,
            hover: None,
            toasts: Vec::new(),
            overlay: Overlay::None,
            overlay_last: Overlay::None,
            launcher: launcher::Search::new(),
            clock,
            virtual_time: boot.virtual_time,
            grid: boot.grid,
        });
        sh.session.set_grid(grid_for(sh.viewport, sh.grid));
        match boot.open {
            // One panel, fresh, in place of the session — alone at the
            // viewport when solo, else the first column of the strip.
            Some(open) => {
                let id = open(sh.session.store());
                open_fresh(&mut sh.session, &id);
                if boot.solo {
                    self.solo = sh.session.showing(&id).first().copied();
                }
            }
            // Boot restores the last session; a store nobody has booted
            // comes up on the first root the app list offers.
            //
            // Under replication it waits: until the first pass has resolved
            // this device's role the gate is shut, and a would-be follower
            // must not write a layout into a store it is about to replace
            // with the holder's. `tick_repl` opens it when the role lands.
            None => {
                if !sh.session.restore() && sh.session.lease().is_none() {
                    open_first_root(&mut sh.session);
                }
            }
        }
        sh.session.relayout();
        self.settle(cx, &mut sh);
        self.shell = Some(sh);
        if let Some(steps) = boot.steps {
            let _ = std::fs::create_dir_all(&boot.out);
            let mut runner = kernel::e2e::Runner::new(steps, boot.out);
            runner.tag = boot.tag;
            self.e2e = Some(runner);
            // Windowed: a real timer paces the run. Virtual time: the draw
            // cycle does, so ask for the first frame and keep asking. A
            // mount is paced by the canvas, which hands it the frames.
            if !boot.virtual_time {
                self.e2e_timer = cx.start_interval(E2E_TICK_MS / 1000.0);
                self.poll_timer = cx.start_interval(2.0);
            }
        }
        self.next_frame = cx.new_next_frame();
        self.redraw_scoped(cx);
    }

    /// A mount is still on its way to its state.
    #[must_use]
    pub fn replaying(&self) -> bool {
        self.e2e.is_some()
    }

    /// Where a label sits in this stage's own coordinates: the centre of
    /// the rectangle the last draw registered for it. What the panels
    /// library resolves a click inside an entered mount with — the mount
    /// has its own hit table, and the canvas has none of its labels.
    #[must_use]
    pub fn hit_centre(&self, label: &str) -> Option<DVec2> {
        self.hits
            .by_label(label)
            .map(|h| h.rect.pos + h.rect.size / 2.0)
    }

    /// Whether this stage is a library mount rather than the window's own.
    #[must_use]
    pub(super) fn is_mount(&self) -> bool {
        self.mount
    }

    /// A mount reached the end of its script: it is its state from here.
    pub(super) fn arrive(&mut self, r: &kernel::e2e::Runner) {
        self.arrived = true;
        if r.failures > 0 {
            eprintln!(
                "library: {}reached its state with {} failed step(s)",
                r.tag, r.failures
            );
        }
    }

    /// A mount that reached its last step and is not entered: a picture. It
    /// gets no events and asks for no frames, which is what keeps a canvas
    /// of them free.
    #[must_use]
    pub fn frozen(&self) -> bool {
        self.mount && self.arrived && !self.active
    }

    /// Whether this stage has come up on a world.
    #[must_use]
    pub fn booted(&self) -> bool {
        self.shell.is_some()
    }

    /// The canvas entered (or left) this mount: it may (or may no longer)
    /// take the window's key focus, and its clock runs (or stands still).
    pub fn set_active(&mut self, cx: &mut Cx, active: bool) {
        if self.active == active {
            return;
        }
        self.active = active;
        if active {
            self.next_frame = cx.new_next_frame();
            self.redraw_scoped(cx);
        }
    }

    /// The panels library went up over this stage (or came down): while up,
    /// the stage neither draws nor hears input.
    pub fn set_suspended(&mut self, cx: &mut Cx, on: bool) {
        if self.suspended == on {
            return;
        }
        self.suspended = on;
        if !on && self.shell.is_some() {
            self.next_frame = cx.new_next_frame();
        }
        cx.redraw_all();
    }

    /// Where a mount draws: its own draw list and the canvas's, so its
    /// redraws stay scoped.
    pub fn set_lists(&mut self, own: DrawListId, canvas: DrawListId) {
        self.lists = Some((own, canvas));
    }

    /// Redraws what this stage draws into — the window for the window's own
    /// stage, a mount's own pass and the canvas that composites it for a
    /// mount, and nothing at all while the library is up over it.
    pub(super) fn redraw_scoped(&self, cx: &mut Cx) {
        if self.suspended {
            return;
        }
        match self.lists {
            Some((own, canvas)) => {
                cx.redraw_list_and_children(own);
                cx.redraw_list(canvas);
            }
            // A mount the canvas has not rendered yet is pending there
            // already; marking the whole window would make every other
            // mount pending too, and the budget would never get past the
            // first few.
            None if self.mount => {}
            None => cx.redraw_all(),
        }
    }

    /// One frame of a mount's replay: one step, the way the harness runs one
    /// per tick. A pending `wait` is consumed whole, together with the step
    /// after it, so a node needs as many frames as it has steps rather than
    /// as many as it has milliseconds. Answers the virtual milliseconds it
    /// advanced, for the springs.
    fn replay_step(&mut self, cx: &mut Cx, sh: &mut Shell) -> f64 {
        if self.stale_hits {
            return 0.0;
        }
        let Some(r) = &mut self.e2e else {
            return 0.0;
        };
        // A wait before a step that resolves a label gets a frame of its
        // own: the harness draws throughout a wait, so the click that
        // follows finds a panel where it settled.
        let mut dt = r.pending_wait().max(FRAME_MS);
        let settle = r.pending_wait() > 0.0 && r.next().is_some_and(kernel::e2e::Step::needs_hits);
        if settle {
            dt = r.take_wait();
        }
        sh.clock.advance(dt / 1000.0);
        sh.session.workers().tick();
        if !settle {
            self.e2e_tick(cx, sh, dt);
        }
        // Every replay frame ends with a draw before the next step.
        self.stale_hits = true;
        dt
    }

    /// What the shell owes the screen after every event: the instances the
    /// last action opened and closed, the widgets of the slots that went,
    /// the camera a preview asked for, the notes the session said, and the
    /// scene it moved.
    ///
    /// The session settles first, before anything reads it: a verb runs as
    /// `&mut self` on its own instance, so nothing may place or drop one
    /// until it has returned.
    pub fn settle(&mut self, cx: &mut Cx, sh: &mut Shell) {
        sh.session.settle();
        self.prune_hosted(sh);
        if let Some(slot) = sh.session.take_show_once() {
            sh.session.reveal(slot);
        }
        let now = sh.session.now();
        for n in sh.session.take_notes() {
            sh.toasts.push(Toast {
                msg: n.msg,
                err: n.err,
                at: now,
            });
        }
        let dirty = sh.session.take_dirty();
        if dirty.layout {
            let titles = draw::titles(&sh.session);
            let active = sh.session.ws().active;
            let scene = sh.session.scene().clone();
            sh.anim.apply(&scene, active, &titles);
            self.next_frame = cx.new_next_frame();
        }
        if dirty.any() || !sh.toasts.is_empty() {
            self.redraw_scoped(cx);
        }
        // The macOS menu bar is a picture of the roster and the problems, and
        // neither moves without something being marked dirty — a switch, an
        // action, or a poll that found new rows. Asking on a quiet frame would
        // run every app's problem sources sixty times a second for an answer
        // that cannot have changed.
        if dirty.any() {
            self.update_menu(cx, sh);
        }
    }

    /// One sync pass, reconciled: a role change is toasted, redraws the
    /// world (an install or a materialize may have replaced the very rows
    /// the layout is kept in) and raises or clears the locked screen; a new
    /// reason to be offline is worth saying even when the role stands, since
    /// a holder whose bucket refuses its key would otherwise accrue
    /// unpublished frames in silence.
    ///
    /// Called on every driver signal and, under virtual time, once a frame.
    pub(super) fn tick_repl(&mut self, cx: &mut Cx, sh: &mut Shell) {
        if sh.session.lease().is_none() {
            return;
        }
        let changed = sh.session.repl_poll();
        if changed.role {
            let (line, err) = match sh.session.lease() {
                Some(l) => (
                    l.role.line(),
                    matches!(l.role, kernel::repl::Role::Stranded { .. }),
                ),
                None => return,
            };
            sh.session.notify(line, err);
            // A device that has just become writable and has nothing open
            // is one whose store was never booted: this is its first root.
            if !sh.session.restore() && sh.session.writable() {
                open_first_root(&mut sh.session);
            }
        }
        if changed.note {
            if let Some(note) = sh.session.lease().and_then(|l| l.note.clone()) {
                sh.session.notify(note, true);
            }
        }
        // Rows a materialize brought in landed on the writer's connection,
        // which is foreign to this reader.
        if sh.session.store().poll_external() {
            sh.session.announce_problems();
        }
        sh.session.redraw();
        self.redraw_scoped(cx);
    }

    /// `SUPERAPP_FRAME_LOG`: what this event was and how long it took.
    /// Counted always, printed only past a millisecond — the interesting
    /// ones are the ones that cost something.
    fn log_event(&mut self, event: &Event, t0: std::time::Instant) {
        let kind = event_kind(event);
        match self.since_draw.iter_mut().find(|(k, _)| *k == kind) {
            Some((_, n)) => *n += 1,
            None => self.since_draw.push((kind, 1)),
        }
        let ms = t0.elapsed().as_secs_f64() * 1000.0;
        if ms > 1.0 {
            eprintln!(
                "{}: event {kind} took {ms:.2} ms",
                if self.mount { "mount" } else { "superapp" }
            );
        }
    }

    /// The rectangle the workspace lives in: the drawn turtle, less what the
    /// screen does not lend it.
    ///
    /// Zero everywhere but a phone. There, the safe area takes the cutout
    /// and the rounded corners, and android swallows touches in the
    /// notification-shade pull zone at the very top of the window — 40 dp
    /// clears both that and a punch-hole camera, which reports no inset of
    /// its own. When the soft keyboard shows, makepad may slide the whole
    /// pass up by as much as the focused caret needs; that shift is
    /// compensated, and the height shortened by the occlusion, which is
    /// there either way.
    fn workspace_rect(&self, cx: &Cx2d) -> Rect {
        let r = cx.turtle().rect();
        let shift = (-r.pos.y).max(0.0);
        let ins = self.insets;
        let top = if cfg!(target_os = "android") {
            ins.top.max(40.0)
        } else {
            ins.top
        };
        draw::rect(
            r.pos.x + ins.left,
            r.pos.y + shift + top,
            (r.size.x - ins.left - ins.right).max(40.0),
            (r.size.y - self.kb_h - top - ins.bottom).max(40.0),
        )
    }

    /// Android's authoritative text state, when no panel's field is
    /// listening for it.
    ///
    /// The launcher's query is the one field the shell owns, and it owns the
    /// whole protocol itself — so the state is handed over whole rather than
    /// read here. That is the guard: a state carrying a composition is a
    /// keyboard still making up its mind, and a shell that pulled characters
    /// out of one would type them a second time.
    fn handle_ime_state(&mut self, cx: &mut Cx, sh: &mut Shell, fs: &FullTextState) {
        if sh.overlay != Overlay::Launcher {
            return;
        }
        let ev = Event::TextInput(TextInputEvent {
            input: String::new(),
            full_state_sync: Some(fs.clone()),
            ..Default::default()
        });
        self.forward_to_overlay(cx, sh, &ev);
        sh.session.redraw();
    }

    /// One frame of virtual or wall time. Answers the seconds it advanced.
    fn tick(&mut self, cx: &mut Cx, sh: &mut Shell) -> f64 {
        if !sh.virtual_time {
            let now = std::time::Instant::now();
            let dt = sh
                .last_frame
                .map(|t| (now - t).as_secs_f64())
                .unwrap_or(1.0 / 60.0)
                .clamp(0.0, 1.0 / 20.0);
            sh.last_frame = Some(now);
            return dt;
        }
        // A mount on its way to its state fast-forwards through its script;
        // the canvas hands it the frames, one step at a time.
        if self.mount && self.e2e.is_some() {
            return self.replay_step(cx, sh) / 1000.0;
        }
        // A mount that has arrived stands still until it is entered: that is
        // what makes a canvas of a hundred of them a canvas of pictures.
        if self.mount && !self.active {
            return 0.0;
        }
        // A `shot` is waiting for the rasterizer to write the frame its own
        // step drew. Nothing moves until it has: no clock, no pass, no next
        // step — the picture is the state at that step, and the frames it
        // waits are wall-clock time the virtual clock never hears about.
        if self.shot.is_some() {
            self.e2e_tick(cx, sh, 0.0);
            self.next_frame = cx.new_next_frame();
            return 0.0;
        }
        // Virtual time: one draw cycle is one tick of exactly FRAME_MS, and
        // a live run keeps the loop turning by asking for the next frame.
        let dt_ms = FRAME_MS;
        sh.clock.advance(dt_ms / 1000.0);
        // The background passes run inline from here, so what a pass filed
        // lands within the `wait` that expected it. Device sync's passes go
        // the same way, so a scripted `wait` advances a handoff exactly as
        // it advances a worker.
        sh.session.workers().tick();
        self.tick_repl(cx, sh);
        if self.e2e.is_some() {
            self.e2e_tick(cx, sh, dt_ms);
            if self.e2e.is_some() {
                self.next_frame = cx.new_next_frame();
            }
        }
        dt_ms / 1000.0
    }
}

impl Widget for Stage {
    fn handle_event(&mut self, cx: &mut Cx, event: &Event, _scope: &mut Scope) {
        // A frozen mount is a picture: nothing to hear, nothing to ask for.
        if self.frozen() {
            return;
        }
        // The window's own stage boots from argv — unless the window opened
        // on the library, which owns the screen until it is put away.
        if matches!(event, Event::Startup) {
            if super::boot::library_filter().is_none() {
                self.boot(cx, Boot::from_argv());
            }
            return;
        }
        // Under the library: the world keeps turning — timers, the store's
        // signals, and a running script, which is what lets a suite raise
        // the canvas and put it away again — but the window is not this
        // stage's, and nothing it draws is on screen. A menu command is the
        // exception: the Dev item that puts the library away again is on a
        // bar this stage owns.
        if self.suspended
            && !matches!(
                event,
                Event::Timer(_) | Event::Signal | Event::MacosMenuCommand(_)
            )
            && !(matches!(event, Event::NextFrame(_)) && self.e2e.is_some())
        {
            return;
        }
        let Some(mut sh) = self.shell.take() else {
            return;
        };
        let t0 = super::boot::frame_log().then(std::time::Instant::now);
        self.handle_with(cx, &mut sh, event);
        self.settle(cx, &mut sh);
        if let Some(t0) = t0 {
            self.log_event(event, t0);
        }
        self.shell = Some(sh);
    }

    fn draw_walk(&mut self, cx: &mut Cx2d, _scope: &mut Scope, walk: Walk) -> DrawStep {
        cx.begin_turtle(
            walk,
            Layout {
                clip_x: false,
                clip_y: false,
                ..self.layout
            },
        );
        if self.suspended {
            cx.end_turtle_with_area(&mut self.area);
            return DrawStep::done();
        }
        let vp = self.workspace_rect(cx);
        self.origin = vp.pos;
        let dpi = cx.current_dpi_factor();
        self.measure_cell(cx, dpi);
        self.hits.clear();

        let mut shell = self.shell.take();
        if let Some(sh) = shell.as_deref_mut() {
            if (sh.viewport - vp.size).length() > 1.0 {
                sh.viewport = vp.size;
                sh.session.set_grid(grid_for(vp.size, sh.grid));
                sh.session.set_viewport((vp.size.x, vp.size.y));
            }
            // How wide a column reads, in characters: the width of a panel
            // at the default wish, less its padding, over one advance.
            let grid = sh.session.ws().grid;
            let unit = (vp.size.x - theme::GAP) / f64::from(grid.w.max(1));
            let text_w = unit * f64::from(grid.w.min(4)) - theme::GAP - 2.0 * theme::PAD_X;
            sh.session
                .set_cols((text_w / self.cell.adv).max(1.0) as usize);
            // A relayout the two calls above asked for lands here, before
            // anything is drawn against a stale scene.
            if sh.session.take_dirty().layout {
                let titles = draw::titles(&sh.session);
                let active = sh.session.ws().active;
                let scene = sh.session.scene().clone();
                sh.anim.apply(&scene, active, &titles);
            }
            let t0 = super::boot::frame_log().then(std::time::Instant::now);
            match self.solo {
                // A panel node: the one panel at the whole viewport, and the
                // sheet over it — so its toasts and its launcher still show.
                Some(slot) => self.draw_solo(cx, sh, vp, slot),
                None => self.draw_scene(cx, sh, vp),
            }
            if let Some(t0) = t0 {
                let since = self
                    .last_draw
                    .map_or(0.0, |t| (t0 - t).as_secs_f64() * 1000.0);
                self.last_draw = Some(t0);
                let after: Vec<String> = self
                    .since_draw
                    .drain(..)
                    .map(|(k, n)| format!("{k}×{n}"))
                    .collect();
                eprintln!(
                    "{}: frame (+{since:.0} ms): draw {:.2} ms, {} panels, {} hits, after {}",
                    if self.mount { "mount" } else { "superapp" },
                    t0.elapsed().as_secs_f64() * 1000.0,
                    sh.session.panels().len(),
                    self.hits.len(),
                    after.join(" ")
                );
            }
            if !self.reported && !self.mount {
                self.reported = true;
                eprintln!(
                    "superapp: first frame — {} panels, viewport {:.0}×{:.0}, cell {:.2}×{:.2}",
                    sh.session.panels().len(),
                    vp.size.x,
                    vp.size.y,
                    self.cell.adv,
                    self.cell.line_h,
                );
            }
        }
        self.shell = shell;
        self.track_row_rect();
        // Drawn: a mount's replay may take its next step.
        self.stale_hits = false;

        cx.end_turtle_with_area(&mut self.area);
        DrawStep::done()
    }
}

impl Stage {
    /// Every event but `Startup`, with the shell borrowed out.
    pub(super) fn handle_with(&mut self, cx: &mut Cx, sh: &mut Shell, event: &Event) {
        // Hosted widgets see every event through their own system. Keys and
        // text are forwarded by the inner handlers instead, so the e2e
        // paths share the exact route.
        if !matches!(
            event,
            Event::KeyDown(_) | Event::KeyUp(_) | Event::TextInput(_)
        ) {
            self.forward_to_hosted(cx, sh, event);
            // The overlay is hosted too, but keyed outside the slot
            // numbering — without this its field would never hear its own
            // Changed action, and the query would type but never search.
            self.forward_to_overlay(cx, sh, event);
        }
        if let Some(slot) = self.pending_focus.take() {
            if slot == OVERLAY_LAUNCHER {
                if let Some(w) = self.hosted.get(&slot).cloned() {
                    let q = sh.launcher.query().to_string();
                    w.as_launcher_overlay().focus_query(cx, &q);
                }
            }
        }
        match event {
            Event::Actions(actions) => {
                for a in actions {
                    if let Some(OverlayAction::Query(q)) = a.downcast_ref::<OverlayAction>() {
                        let q = q.clone();
                        self.launcher_ask(sh, &q);
                    }
                }
            }

            Event::Timer(te) => {
                if self.e2e_timer.0 != 0 && te.timer_id == self.e2e_timer.0 {
                    self.e2e_tick(cx, sh, E2E_TICK_MS);
                }
                if self.poll_timer.0 != 0 && te.timer_id == self.poll_timer.0 {
                    if sh.session.store().poll_external() {
                        sh.session.announce_problems();
                        sh.session.redraw();
                    }
                    self.tick_repl(cx, sh);
                }
            }

            // A worker committed. The platform already consumed the signal
            // flag before delivering this, so never re-check it — just poll.
            Event::Signal => {
                if sh.session.store().poll_external() {
                    sh.session.announce_problems();
                    sh.session.redraw();
                }
                self.tick_repl(cx, sh);
            }

            // The lease's lifecycle: hand it back when this device steps
            // away, so the other can take over without an override, and
            // re-poll when it returns. On close the release runs
            // synchronously — the driver may never get another turn.
            Event::Background | Event::Pause => sh.session.repl_release(),
            Event::Foreground | Event::Resume => sh.session.repl_kick(),
            // The last chance at both: the layout written, and then the
            // lease handed back. `settle` saves after every event that moved
            // anything, so in practice this writes nothing — but a shutdown
            // is the one moment where "in practice" is not good enough.
            Event::Shutdown => {
                sh.session.save();
                sh.session.repl_release_blocking();
            }

            // A menu item (macOS menu bar).
            Event::MacosMenuCommand(cmd) => self.menu_command(cx, sh, *cmd),

            Event::KeyDown(k) => self.handle_key_down(cx, sh, k),
            Event::KeyUp(k) => self.handle_key_up(cx, sh, k),

            Event::TextInput(e) => {
                // A hosted widget's own field owns the whole input protocol
                // — a plain character and android's authoritative full state
                // alike — so it gets the original event untouched. What is
                // left over splits by shape: a full state to
                // [`Stage::handle_ime_state`], characters to the shell's one
                // text door.
                if sh.overlay == Overlay::None && self.hosted_focus(sh) {
                    self.forward_to_focused(cx, sh, event);
                } else if let Some(fs) = e.full_state_sync.clone() {
                    self.handle_ime_state(cx, sh, &fs);
                } else {
                    let input = e.input.clone();
                    self.handle_text(cx, sh, &input);
                }
            }

            Event::MouseMove(e) => self.handle_mouse_move(cx, sh, e.abs),
            Event::MouseDown(e) => self.handle_mouse_down(cx, sh, e),

            // The fingers. Everything they mean is decided in `touch`; the
            // platform's own long press is the one gesture it detects for us.
            Event::TouchUpdate(e) => {
                self.cmd_tap.other_input();
                self.touch_update(cx, sh, e);
            }
            Event::LongPress(e) => self.long_press(cx, sh, e.uid, e.abs),

            // The viewport follows the drawn turtle; what is captured here is
            // what a cutout or a rounded corner carves out of it. The next
            // draw picks both up.
            Event::WindowGeomChange(e) => {
                let ins = e.new_geom.safe_area_insets;
                self.insets = Insets {
                    top: ins.top,
                    right: ins.right,
                    bottom: ins.bottom,
                    left: ins.left,
                };
                self.redraw_scoped(cx);
            }

            // An `adjustNothing` manifest: the app makes its own room. The
            // occlusion shortens the viewport, and the panels spring up.
            Event::VirtualKeyboard(e) => {
                self.kb_h = match e {
                    VirtualKeyboardEvent::WillShow { height, .. }
                    | VirtualKeyboardEvent::DidShow { height, .. } => *height,
                    VirtualKeyboardEvent::WillHide { .. }
                    | VirtualKeyboardEvent::DidHide { .. } => 0.0,
                };
                sh.session.redraw();
                self.next_frame = cx.new_next_frame();
                self.redraw_scoped(cx);
            }

            // The soft keyboard's action button. A field that has the caret
            // answers it itself, through its own hit path; when the keyboard
            // belongs to the shell it is this grammar's enter.
            Event::ImeAction(_) => {
                let field = self.field_letters(sh.session.focus()) != super::keys::Letters::NONE;
                if !field && sh.overlay != Overlay::Launcher {
                    let k = KeyEvent {
                        key_code: KeyCode::ReturnKey,
                        modifiers: KeyModifiers::default(),
                        is_repeat: false,
                        time: sh.session.now(),
                    };
                    self.handle_key_down(cx, sh, &k);
                }
            }

            Event::Scroll(e) => {
                self.cmd_tap.other_input();
                e.handled_x.set(true);
                e.handled_y.set(true);
                // Vertical scrolling belongs to the retained content, which
                // saw this event first.
                if e.scroll.x.abs() > e.scroll.y.abs() {
                    sh.session.pan(e.scroll.x);
                    let cam = sh.session.scene().camera_x;
                    sh.anim.camera().jump_to(cam);
                }
            }

            Event::NextFrame(ne) => {
                // A replaying mount ticks whenever the canvas hands it a
                // frame: the canvas decides which mount replays when, so the
                // frame it asked for may long since have gone by.
                let asked = ne.set.contains(&self.next_frame);
                if !(asked || (self.mount && self.e2e.is_some())) {
                    return;
                }
                let dt = self.tick(cx, sh);
                let moving = sh.anim.advance(dt);
                // A held panel against an edge and a curtain mid-wipe move
                // outside the scene's own springs, so they say for themselves
                // whether they still want frames.
                let gesturing = self.touch_tick(sh, dt);
                let now = sh.session.now();
                let toasting = sh.toasts.iter().any(|t| now - t.at <= 3.0);
                if moving || toasting || gesturing {
                    self.next_frame = cx.new_next_frame();
                }
                if super::boot::frame_log() && self.mount {
                    eprintln!(
                        "mount: tick dt {:.1} ms, moving {moving}, toast {toasting}",
                        dt * 1000.0
                    );
                }
                self.redraw_scoped(cx);
                // It changes the world, so it runs after the frame's own
                // bookkeeping rather than in the middle of it.
                self.settle_row_swipe(cx, sh);
            }

            _ => {}
        }
    }
}
