//! The makepad shell: window, shaders, drawing, events, animation.
//!
//! This is the only module that knows makepad exists. It owns no layout rules —
//! [`crate::core`] emits discrete targets and this module springs towards them
//! (mosaic's division of labour).
//!
//! # Frame loop
//!
//! ```text
//! Event::{Key,Mouse}* ──▶ mutate Ws ──▶ ws.scene() ──▶ Anim::apply ──▶ redraw
//! Event::NextFrame ─────▶ Anim::advance(dt) ──▶ redraw while anything moves
//! ```
//!
//! The scene is pulled only after a mutation, never per frame. Panel content is
//! laid out on a monospace character grid; bodies draw inside clipped turtles,
//! so scrolling is pixel-smooth and clipping is exact.

#![allow(missing_docs)]

use std::collections::HashMap;
use std::time::Instant;

use makepad_widgets::*;
// Touch types are not in the curated platform re-export list.
use makepad_widgets::makepad_platform::event::{TouchState, TouchUpdateEvent};
use makepad_widgets::makepad_platform::ime::TextInputConfig;

use crate::core::{self, Dir, Kind, MailId, PanelId, Wm, Ws, WS_N};
use crate::e2e;
use crate::launcher;
use crate::mail;
use crate::send;
use crate::panels::*;
use crate::store::Store;
use crate::sync;
use crate::spring::{Spring, SpringParams};
use crate::theme;
use crate::ui::{
    self, char_byte, kbd, pad_to, trunc, wrap, BtnAct, FieldId, Line, Seg, Style, TextField,
};

// ---------------------------------------------------------------------------
// Configuration
// ---------------------------------------------------------------------------

/// Command-line configuration.
#[derive(Debug, Default)]
struct Config {
    /// Path of an e2e script to replay.
    e2e: Option<String>,
    /// Screenshot directory for e2e runs.
    out: String,
    /// Keep the window frontmost even during an e2e run.
    front: bool,
    /// Force a grid (`--grid 4x3`): preview a phone layout on desktop.
    grid: Option<core::Grid>,
    /// Force the window size (`--window 380x840`): preview a phone screen.
    window: Option<(f64, f64)>,
    /// Override the store's path (`--db PATH`). E2e runs default to a fresh
    /// temp file; normal runs to the platform data dir.
    db: Option<String>,
    /// The send-undo window in seconds (`--send-delay 1` for e2e).
    send_delay: f64,
}

fn parse_wxh(s: &str) -> Option<(f64, f64)> {
    let (w, h) = s.split_once('x')?;
    Some((w.trim().parse().ok()?, h.trim().parse().ok()?))
}

fn config() -> &'static Config {
    static CONFIG: std::sync::OnceLock<Config> = std::sync::OnceLock::new();
    CONFIG.get_or_init(|| {
        let mut c = Config {
            out: "e2e/out".into(),
            send_delay: 10.0,
            ..Default::default()
        };
        let mut args = std::env::args().skip(1);
        while let Some(a) = args.next() {
            match a.as_str() {
                "--e2e" => c.e2e = args.next(),
                "--e2e-out" => {
                    if let Some(o) = args.next() {
                        c.out = o;
                    }
                }
                "--front" => c.front = true,
                "--grid" => {
                    c.grid = args.next().and_then(|s| {
                        parse_wxh(&s).map(|(w, h)| core::Grid {
                            w: w as u32,
                            h: h as u32,
                        })
                    });
                }
                "--window" => c.window = args.next().and_then(|s| parse_wxh(&s)),
                "--db" => c.db = args.next(),
                "--send-delay" => {
                    if let Some(d) = args.next().and_then(|s| s.parse().ok()) {
                        c.send_delay = d;
                    }
                }
                other => eprintln!("superapp: ignoring unknown argument {other:?}"),
            }
        }
        c
    })
}

/// An e2e run stays behind every normal window unless `--front` asks otherwise.
fn background_run() -> bool {
    config().e2e.is_some() && !config().front
}

/// Where the store lives: `--db` wins; an e2e run gets a fresh temp file
/// (deleted first, so every run seeds the same demo world); otherwise the
/// platform data dir. `None` (no resolvable home) falls back to in-memory.
fn db_path(cx: &Cx) -> Option<std::path::PathBuf> {
    if let Some(p) = &config().db {
        return Some(std::path::PathBuf::from(p));
    }
    if config().e2e.is_some() {
        let p = std::env::temp_dir().join(format!("superapp-e2e-{}.db", std::process::id()));
        for suffix in ["", "-wal", "-shm"] {
            let _ = std::fs::remove_file(p.with_file_name(format!(
                "{}{suffix}",
                p.file_name().unwrap_or_default().to_string_lossy()
            )));
        }
        return Some(p);
    }
    #[cfg(target_os = "android")]
    {
        return cx
            .os_type()
            .get_data_dir()
            .map(|d| std::path::Path::new(&d).join("superapp.db"));
    }
    #[cfg(not(target_os = "android"))]
    {
        let _ = cx;
        let home = std::env::var_os("HOME")?;
        let dir =
            std::path::PathBuf::from(home).join("Library/Application Support/superapp");
        let _ = std::fs::create_dir_all(&dir);
        Some(dir.join("superapp.db"))
    }
}

/// The desktop window frame: the display's visible frame, unless `--window`
/// shrinks it for a phone-screen preview.
#[cfg(target_os = "macos")]
fn desired_frame() -> (DVec2, DVec2) {
    let (pos, size) = crate::mac::visible_frame();
    match config().window {
        Some((w, h)) => (pos, dvec2(w.min(size.x), h.min(size.y))),
        None => (pos, size),
    }
}

// ---------------------------------------------------------------------------
// DSL
// ---------------------------------------------------------------------------

script_mod! {
    use mod.prelude.widgets.*

    // A panel quad: flat fill + hard 1 pt border, sharp corners.
    set_type_default() do #(DrawPanel::script_shader(vm)){
        ..mod.draw.DrawQuad
        color: vec4(1.0, 1.0, 1.0, 1.0)
        border_color: vec4(0.078, 0.078, 0.078, 1.0)
        border_size: 1.0
        alpha: 1.0
        pixel: fn() {
            let sdf = Sdf2d.viewport(self.pos * self.rect_size)
            sdf.box(0.0 0.0 self.rect_size.x self.rect_size.y 1.0)
            sdf.fill_keep(vec4(self.color.xyz, self.color.w * self.alpha))
            if self.border_size > 0.0 {
                sdf.stroke(vec4(self.border_color.xyz, self.border_color.w * self.alpha) self.border_size)
            }
            return sdf.result
        }
    }

    // Flat quad: rules, hovers, selections, underlines, carets, bridges.
    set_type_default() do #(DrawFlat::script_shader(vm)){
        ..mod.draw.DrawQuad
        color: vec4(0.078, 0.078, 0.078, 1.0)
        pixel: fn() {
            return vec4(self.color.xyz * self.color.w, self.color.w)
        }
    }

    let StageBase = #(Stage::register_widget(vm))
    let Stage = set_type_default() do StageBase{
        width: Fill
        height: Fill

        // The mono face. Menlo (always present on macOS) fronts the family;
        // makepad's own fonts fill in what it lacks.
        draw_mono +: {
            text_style: TextStyle{
                font_family: FontFamily{
                    latin := FontMember{res: file_resource("/System/Library/Fonts/Menlo.ttc") asc: 0.0 desc: 0.0}
                    fallback := FontMember{res: crate_resource("makepad_widgets:resources/LiberationMono-Regular.ttf") asc: 0.0 desc: 0.0}
                    symbols := FontMember{res: crate_resource("makepad_widgets:resources/NotoSans-Regular.ttf") asc: 0.0 desc: 0.0}
                }
                line_spacing: 1.0
            }
            color: #141414ff
        }
    }

    startup() do #(App::script_component(vm)){
        ui: Root{
            main_window := Window{
                show_caption_bar: false
                window.title: "superapp"
                window.inner_size: vec2(1440, 900)
                pass.clear_color: #ffffffff
                body +: {
                    stage := Stage{
                        // Retained content templates (CR-002): named children
                        // of a custom-drawn widget are never auto-drawn —
                        // they are collected as templates and instantiated
                        // per panel, PortalList-style.
                        settings_tpl := mod.widgets.SettingsPanel{}
                    }
                }
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Draw structs
// ---------------------------------------------------------------------------

/// A panel quad: flat fill, hard border, per-instance alpha for open/close.
#[derive(Script, ScriptHook)]
#[repr(C)]
pub struct DrawPanel {
    #[deref]
    draw_super: DrawQuad,
    #[live]
    pub color: Vec4f,
    #[live]
    pub border_color: Vec4f,
    #[live]
    pub border_size: f32,
    #[live]
    pub alpha: f32,
}

/// An unadorned filled rectangle.
#[derive(Script, ScriptHook)]
#[repr(C)]
pub struct DrawFlat {
    #[deref]
    draw_super: DrawQuad,
    #[live]
    pub color: Vec4f,
}

fn rgba_a(c: theme::Rgba, alpha: f64) -> Vec4f {
    vec4(c[0], c[1], c[2], c[3] * alpha as f32)
}

fn rect(x: f64, y: f64, w: f64, h: f64) -> Rect {
    Rect {
        pos: dvec2(x, y),
        size: dvec2(w, h),
    }
}


/// What a panel keeps between frames and loses on replacement.
#[derive(Debug, Clone)]
struct PanelUi {
    kind: Kind,
    sel: Option<MailId>,
    scroll: f64,
    max_scroll: f64,
    /// Visible height of the scrolling region, written during draw.
    view_h: f64,
    filter: TextField,
    to: TextField,
    subject: TextField,
    body: Vec<String>,
    caret: (usize, usize), // row, col in chars
    set_email: TextField,
    set_pass: TextField,
    set_imap: TextField,
    set_smtp: TextField,
    /// The draft as last persisted (compose panels; skip no-op saves).
    draft_saved: Option<mail::Draft>,
}

impl PanelUi {
    /// Every single-line field by id (`Body` is the multi-line exception).
    fn field_mut(&mut self, fid: FieldId) -> Option<&mut TextField> {
        Some(match fid {
            FieldId::Filter => &mut self.filter,
            FieldId::To => &mut self.to,
            FieldId::Subject => &mut self.subject,
            FieldId::SetEmail => &mut self.set_email,
            FieldId::SetPass => &mut self.set_pass,
            FieldId::SetImap => &mut self.set_imap,
            FieldId::SetSmtp => &mut self.set_smtp,
            FieldId::Body => return None,
        })
    }
}

impl PanelUi {
    fn for_kind(kind: &Kind, store: &Store, pid: PanelId) -> Self {
        let mut ui = PanelUi {
            kind: kind.clone(),
            sel: None,
            scroll: 0.0,
            max_scroll: 0.0,
            view_h: 0.0,
            filter: TextField::default(),
            to: TextField::default(),
            subject: TextField::default(),
            body: vec![String::new()],
            caret: (0, 0),
            set_email: TextField::default(),
            set_pass: TextField::default(),
            set_imap: TextField::default(),
            set_smtp: TextField::default(),
            draft_saved: None,
        };
        match kind {
            Kind::Inbox { filter } => {
                if let Some(f) = filter {
                    ui.filter.text = f.clone();
                    ui.filter.caret = ui.filter.text.chars().count();
                }
            }
            // Read flags are the *opening action's* business (its changeset
            // carries the flag flip, so undo restores unread) — not this
            // constructor's, which also runs on boot restore.
            Kind::Message { .. } => {}
            Kind::Compose { re } => {
                // A persisted draft (boot restore, undo) wins over the
                // reply prefill.
                if let Some(d) = mail::draft(store, pid as i64) {
                    ui.to.text = d.to;
                    ui.subject.text = d.subject;
                    ui.body = if d.body.is_empty() {
                        vec![String::new()]
                    } else {
                        d.body.split('\n').map(str::to_string).collect()
                    };
                    ui.caret = (ui.body.len() - 1, ui.body.last().map_or(0, |l| l.chars().count()));
                    ui.draft_saved = Some(mail::Draft {
                        to: ui.to.text.clone(),
                        subject: ui.subject.text.clone(),
                        body: ui.body.join("\n"),
                    });
                } else if let Some(m) = mail::mail(store, *re) {
                    ui.to.text = m.head.from_email;
                    ui.subject.text = format!("Re: {}", m.head.subject);
                }
                ui.to.caret = ui.to.text.chars().count();
                ui.subject.caret = ui.subject.text.chars().count();
            }
            _ => {}
        }
        ui
    }
}

// ---------------------------------------------------------------------------
// Hit testing
// ---------------------------------------------------------------------------

/// What a click means. Built during draw, resolved on MouseDown.
#[derive(Debug, Clone, PartialEq)]
enum Act {
    Focus(PanelId),
    Close(PanelId),
    Btn(PanelId, BtnAct),
    Open(PanelId, Kind),
    Replace(PanelId, Kind),
    Row(PanelId, MailId),
    Field(PanelId, FieldId),
    /// Activate this panel's tab in its tabbed column.
    Tab(PanelId),
    /// A row of the workspaces overlay: switch to workspace `k`.
    WsRow(usize),
    /// The workspaces overlay's search row (and the menu item): raise the
    /// launcher.
    LauncherOpen,
    /// The launcher's `i`-th visible hit: go to it / open it.
    LauncherRow(usize),
    /// A node of the history overlay: travel there (0 = the beginning).
    HistoryRow(i64),
    /// The overlay's backdrop: tapping outside the rows dismisses it.
    OverlayClose,
    /// A retained widget's interactive child (CR-002): the e2e bridge
    /// synthesizes pointer events at its rect; a real click just focuses.
    Pointer(PanelId),
    /// A retained widget's *button*, semantically (CR-002): e2e resolves it
    /// to the same PanelAction the button's own click emits.
    WidgetOp(PanelId, WidgetOp),
}

/// Semantic button operations on retained panels (the e2e bridge).
#[derive(Debug, Clone, Copy, PartialEq)]
enum WidgetOp {
    AddAccount,
    RemoveAccount(i64),
}

#[derive(Debug, Clone)]
struct HitR {
    rect: Rect,
    act: Act,
    cursor: MouseCursor,
    /// What an e2e script can address this element by.
    label: String,
}

/// The panel an act belongs to (overlay acts belong to none).
fn act_pid(act: &Act) -> Option<PanelId> {
    match act {
        Act::Focus(pid)
        | Act::Close(pid)
        | Act::Btn(pid, _)
        | Act::Open(pid, _)
        | Act::Replace(pid, _)
        | Act::Row(pid, _)
        | Act::Field(pid, _)
        | Act::Tab(pid) => Some(*pid),
        Act::WsRow(_) | Act::LauncherOpen | Act::LauncherRow(_) | Act::HistoryRow(_) | Act::OverlayClose | Act::Pointer(_) | Act::WidgetOp(..) => None,
    }
}

// ---------------------------------------------------------------------------
// Double-cmd
// ---------------------------------------------------------------------------

/// Max gap between the two taps, seconds.
const CMD_TAP_GAP: f64 = 0.35;
/// Max hold of each tap — longer means a chord was intended, not a tap.
const CMD_TAP_HOLD: f64 = 0.5;

/// The double-cmd detector (the JetBrains double-shift move, on the
/// workspace key). Bare modifier presses arrive as real KeyDown/KeyUp with
/// `KeyCode::Logo`. A tap only counts while *clean*: any other key, click or
/// scroll while cmd is down means a chord (cmd+w, cmd+click…) and dirties
/// it; a press held past [`CMD_TAP_HOLD`] is not a tap at all.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
enum CmdTap {
    #[default]
    Idle,
    /// First press down, so far clean.
    Down { t: f64, dirty: bool },
    /// One clean tap done; a second press within the gap arms the trigger.
    Up { t: f64 },
    /// Second press down; a clean release fires.
    Down2 { t: f64, dirty: bool },
}

impl CmdTap {
    fn press(&mut self, t: f64) {
        *self = match *self {
            CmdTap::Up { t: t0 } if t - t0 <= CMD_TAP_GAP => CmdTap::Down2 { t, dirty: false },
            _ => CmdTap::Down { t, dirty: false },
        };
    }

    /// Returns whether the double-tap fired.
    fn release(&mut self, t: f64) -> bool {
        let (next, fire) = match *self {
            CmdTap::Down { t: t0, dirty: false } if t - t0 <= CMD_TAP_HOLD => {
                (CmdTap::Up { t }, false)
            }
            CmdTap::Down2 { t: t0, dirty: false } if t - t0 <= CMD_TAP_HOLD => (CmdTap::Idle, true),
            _ => (CmdTap::Idle, false),
        };
        *self = next;
        fire
    }

    /// Any other input: a held press turns into a chord, a pending second
    /// tap is abandoned.
    fn other_input(&mut self) {
        *self = match *self {
            CmdTap::Down { t, .. } => CmdTap::Down { t, dirty: true },
            CmdTap::Down2 { t, .. } => CmdTap::Down2 { t, dirty: true },
            _ => CmdTap::Idle,
        };
    }
}

// ---------------------------------------------------------------------------
// Touch navigation
// ---------------------------------------------------------------------------

/// How far a finger may wander and still be a tap, in points.
const TOUCH_SLOP: f64 = 8.0;

/// What the active fingers are doing. One finger taps or scrolls the panel
/// under it; two fingers pan the workspace; a native long-press on a panel
/// header upgrades to a drag that re-places the panel on drop. There is no
/// touch equivalent of cmd+click — a solid link always follows join
/// semantics on touch.
#[derive(Debug, Clone, Default, PartialEq)]
enum TouchMode {
    #[default]
    Idle,
    /// A finger down, undecided; resolves to a click on lift inside the slop.
    Tap { uid: u64, act: Option<Act> },
    /// One finger scrolling a panel body, 1:1.
    Scroll { uid: u64, pid: PanelId },
    /// Two fingers down. The first move past the slop locks the axis:
    /// horizontal pans the strip 1:1; a vertical swipe toggles the
    /// workspaces overlay (down opens, up closes) and goes dead.
    Pan { horizontal: Option<bool> },
    /// A long-pressed header: the panel rides the finger; the drop point
    /// picks its new place ([`Ws::place_at`]).
    Drag { uid: u64, pid: PanelId, offset: DVec2 },
    /// A gesture that came to nothing (sideways one-finger move, a lifted
    /// pan finger): inert until every finger lifts.
    Dead,
}

/// Live touches and the gesture they add up to.
#[derive(Debug, Default)]
struct TouchNav {
    /// uid → (start, latest) positions.
    pts: HashMap<u64, (DVec2, DVec2)>,
    mode: TouchMode,
}

// ---------------------------------------------------------------------------
// Animation: springs towards the scene's targets
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
struct PanelAnim {
    x: Spring,
    y: Spring,
    w: Spring,
    h: Spring,
    alpha: Spring,
    title: String,
    /// Which workspace the panel lives on — its row in the vertical stack.
    ws: usize,
}

impl PanelAnim {
    fn spawn(target: core::Rect, title: String, visible: bool, ws: usize) -> Self {
        // Born slightly inset and transparent; springs carry it to place. A
        // panel born hidden (an inactive tab) just sits at its rect at rest.
        let inset = if visible { 12.0 } else { 0.0 };
        let mk = |v| Spring::at_rest(v, SpringParams::movement());
        let mut pa = PanelAnim {
            x: mk(target.x + inset),
            y: mk(target.y + inset),
            w: mk(target.w - 2.0 * inset),
            h: mk(target.h - 2.0 * inset),
            alpha: Spring::at_rest(0.0, SpringParams::fade()),
            title,
            ws,
        };
        pa.retarget(target);
        if visible {
            pa.alpha.retarget(1.0);
        }
        pa
    }
    fn retarget(&mut self, t: core::Rect) {
        self.x.retarget(t.x);
        self.y.retarget(t.y);
        self.w.retarget(t.w);
        self.h.retarget(t.h);
    }
    fn rect(&self) -> core::Rect {
        core::Rect {
            x: self.x.value(),
            y: self.y.value(),
            w: self.w.value(),
            h: self.h.value(),
        }
    }
    fn advance(&mut self, dt: f64) {
        self.x.advance(dt);
        self.y.advance(dt);
        self.w.advance(dt);
        self.h.advance(dt);
        self.alpha.advance(dt);
    }
    fn is_done(&self) -> bool {
        self.x.is_done()
            && self.y.is_done()
            && self.w.is_done()
            && self.h.is_done()
            && self.alpha.is_done()
    }
}

#[derive(Debug, Clone)]
struct Ghost {
    rect: core::Rect,
    alpha: Spring,
    title: String,
    /// The workspace row the panel died on.
    ws: usize,
}

/// Drawn state: springs keyed by panel, plus fading ghosts of closed panels.
#[derive(Debug, Default)]
struct Anim {
    camera: Option<Spring>,
    /// The camera's vertical position in the workspace stack, in workspace
    /// rows — springs between numbers on a switch (niri's slide).
    slide: Option<Spring>,
    panels: HashMap<PanelId, PanelAnim>,
    ghosts: Vec<Ghost>,
}

impl Anim {
    fn camera(&mut self) -> &mut Spring {
        self.camera
            .get_or_insert_with(|| Spring::at_rest(0.0, SpringParams::movement()))
    }

    fn slide(&mut self) -> &mut Spring {
        self.slide
            .get_or_insert_with(|| Spring::at_rest(0.0, SpringParams::movement()))
    }

    /// Applies every workspace's fresh scene: retarget the living, spawn the
    /// new, ghost the gone. Only the union across workspaces counts as
    /// alive — a switch must never ghost the space being left.
    fn apply(
        &mut self,
        scenes: &[(usize, core::Scene)],
        active: usize,
        titles: &HashMap<PanelId, String>,
    ) {
        if let Some((_, sc)) = scenes.iter().find(|(k, _)| *k == active) {
            self.camera().retarget(sc.camera_x);
        }
        self.slide().retarget(active as f64);
        let mut seen = std::collections::HashSet::new();
        for (k, scene) in scenes {
            for ps in &scene.panels {
                seen.insert(ps.id);
                let title = titles.get(&ps.id).cloned().unwrap_or_default();
                match self.panels.get_mut(&ps.id) {
                    Some(pa) => {
                        pa.retarget(ps.rect);
                        // A tab switch is a crossfade in place, never open/close.
                        pa.alpha.retarget(if ps.visible { 1.0 } else { 0.0 });
                        pa.title = title;
                        pa.ws = *k;
                    }
                    None => {
                        self.panels
                            .insert(ps.id, PanelAnim::spawn(ps.rect, title, ps.visible, *k));
                    }
                }
            }
        }
        let gone: Vec<PanelId> = self
            .panels
            .keys()
            .copied()
            .filter(|id| !seen.contains(id))
            .collect();
        for id in gone {
            let pa = self.panels.remove(&id).unwrap();
            let mut alpha = pa.alpha;
            alpha.retarget(0.0);
            self.ghosts.push(Ghost {
                rect: pa.rect(),
                alpha,
                title: pa.title,
                ws: pa.ws,
            });
        }
    }

    fn advance(&mut self, dt: f64) -> bool {
        let mut active = false;
        if let Some(c) = self.camera.as_mut() {
            c.advance(dt);
            active |= !c.is_done();
        }
        if let Some(s) = self.slide.as_mut() {
            s.advance(dt);
            active |= !s.is_done();
        }
        for pa in self.panels.values_mut() {
            pa.advance(dt);
            active |= !pa.is_done();
        }
        for g in &mut self.ghosts {
            g.alpha.advance(dt);
        }
        self.ghosts.retain(|g| !g.alpha.is_done());
        active |= !self.ghosts.is_empty();
        active
    }
}

// ---------------------------------------------------------------------------
// Shell state
// ---------------------------------------------------------------------------

/// Which modal surface is up. Both share the chassis (ink wash, rows, esc /
/// tap-outside closes); while one is up it owns every hit.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
enum Overlay {
    #[default]
    None,
    /// The workspaces list (two-finger swipe down on touch).
    Ws,
    /// The launcher: a query over everything (double-cmd on desktop, the
    /// search row of the workspaces overlay on touch).
    Launcher,
    /// The history tree: every action, walkable (cmd+u).
    History,
}

/// The launcher's editable state. Hits are recomputed on draw, and kept here
/// so a click resolves against exactly what was on screen.
#[derive(Debug, Default)]
struct LauncherUi {
    query: String,
    /// Caret, in chars.
    caret: usize,
    /// Selected row (enter activates it).
    sel: usize,
    hits: Vec<launcher::Hit>,
}

struct State {
    ws: Wm,
    store: std::rc::Rc<Store>,
    /// The store's file path — sync workers open their own connections to
    /// it (`None` = in-memory: no workers).
    db_path: Option<std::path::PathBuf>,
    /// One IMAP worker per configured account.
    workers: Vec<sync::Worker>,
    /// The outbox sender thread.
    sender: Option<send::Sender>,
    /// Failed outbox rows already toasted (new ones toast on signal).
    failed_seen: usize,
    /// The last persisted logical snapshot — [`State::sync`] only writes
    /// when the state actually changed.
    last_saved: Option<core::WmSnap>,
    ui: HashMap<PanelId, PanelUi>,
    anim: Anim,
    viewport: DVec2,
    last_frame: Option<Instant>,
    animating: bool,
    hover: Option<Act>,
    field: Option<(PanelId, FieldId)>,
    toast: Option<(String, bool, Instant)>,
    overlay: Overlay,
    launcher: LauncherUi,
}

/// A field's content as one string plus the caret in chars — the shape the
/// android IME editable mirrors (body lines joined with `\n`).
fn field_text_caret(state: &State, pid: PanelId, fid: FieldId) -> Option<(String, usize)> {
    let ui = state.ui.get(&pid)?;
    Some(match fid {
        FieldId::Filter => (ui.filter.text.clone(), ui.filter.caret),
        FieldId::To => (ui.to.text.clone(), ui.to.caret),
        FieldId::Subject => (ui.subject.text.clone(), ui.subject.caret),
        FieldId::SetEmail => (ui.set_email.text.clone(), ui.set_email.caret),
        FieldId::SetPass => (ui.set_pass.text.clone(), ui.set_pass.caret),
        FieldId::SetImap => (ui.set_imap.text.clone(), ui.set_imap.caret),
        FieldId::SetSmtp => (ui.set_smtp.text.clone(), ui.set_smtp.caret),
        FieldId::Body => {
            let (r, c) = ui.caret;
            let caret = ui
                .body
                .iter()
                .take(r)
                .map(|l| l.chars().count() + 1)
                .sum::<usize>()
                + c;
            (ui.body.join("\n"), caret)
        }
    })
}

/// The grid for a viewport. Desktop is always 12×6; android picks 8×4 on the
/// unfolded screen and 4×3 on the cover display (the ~600 dp compact/medium
/// breakpoint — a fold/unfold resize crosses it). `--grid` overrides for
/// desktop previews of the phone layouts.
fn grid_for(vp: DVec2) -> core::Grid {
    if let Some(g) = config().grid {
        return g;
    }
    if cfg!(target_os = "android") {
        if vp.x >= 600.0 {
            core::Grid { w: 8, h: 4 }
        } else {
            core::Grid { w: 4, h: 3 }
        }
    } else {
        core::Grid::default()
    }
}

impl State {
    fn new(store: Store, db_path: Option<std::path::PathBuf>) -> Self {
        let store = std::rc::Rc::new(store);
        // Boot restores the last session from the store; a store that never
        // booted gets the default layout (and persists it on first sync).
        let ws = match store.load_wm() {
            Ok(Some(snap)) => Wm::restore(snap),
            Ok(None) => {
                let mut ws = Wm::new();
                ws.open(Kind::Help, None, false);
                let inbox = ws.open(Kind::Inbox { filter: None }, None, false);
                ws.focus = Some(inbox);
                ws
            }
            Err(e) => {
                eprintln!("store: loading the session failed: {e}");
                Wm::new()
            }
        };
        State {
            ws,
            store,
            db_path,
            workers: Vec::new(),
            sender: None,
            failed_seen: 0,
            last_saved: None,
            ui: HashMap::new(),
            anim: Anim::default(),
            viewport: dvec2(1440.0, 900.0),
            last_frame: None,
            animating: false,
            hover: None,
            field: None,
            toast: None,
            overlay: Overlay::None,
            launcher: LauncherUi::default(),
        }
    }

    fn opts(&self) -> core::LayoutOpts {
        core::LayoutOpts { gap: theme::GAP }
    }

    fn vp(&self) -> (f64, f64) {
        (self.viewport.x, self.viewport.y)
    }

    fn panel_title(&self, kind: &Kind) -> String {
        mail::title(&self.store, kind)
    }

    /// Recomputes targets after a mutation and feeds the animator. The camera
    /// follows focus here — and only here, so trackpad pans stay free.
    fn sync(&mut self) {
        self.ws.set_grid(grid_for(self.viewport));
        let vp = self.vp();
        let opts = self.opts();
        self.ws.ensure_focus_visible(vp, opts);
        // Per-panel ui: create/reset entries, drop dead ones — across every
        // workspace, so panels on inactive spaces keep drafts and scrolls.
        let ids: Vec<(PanelId, Kind)> = self
            .ws
            .wss
            .iter()
            .flat_map(|w| w.panels.values().map(|p| (p.id, p.kind.clone())))
            .collect();
        for (pid, kind) in &ids {
            let fresh = match self.ui.get(pid) {
                Some(ui) => ui.kind != *kind,
                None => true,
            };
            if fresh {
                let ui = PanelUi::for_kind(kind, &self.store, *pid);
                if matches!(kind, Kind::Compose { .. }) {
                    self.field = Some((*pid, FieldId::Body));
                }
                self.ui.insert(*pid, ui);
            }
        }
        self.ui.retain(|pid, _| {
            let pid = *pid;
            ids.iter().any(|(id, _)| *id == pid)
        });
        // A field only makes sense on the visible workspace: switching away
        // (or moving the panel away) blurs it, which also parks the IME.
        if let Some((pid, _)) = self.field {
            if !self.ws.panels.contains_key(&pid) {
                self.field = None;
            }
        }

        // Every workspace computes its scene: the animator needs the union
        // (a switch retargets both spaces mid-slide, and must never ghost
        // the one being left).
        let active = self.ws.active;
        let scenes: Vec<(usize, core::Scene)> = self
            .ws
            .wss
            .iter_mut()
            .enumerate()
            .map(|(k, w)| (k, w.scene(vp, opts)))
            .collect();
        let titles: HashMap<PanelId, String> = self
            .ws
            .wss
            .iter()
            .flat_map(|w| w.panels.values())
            .map(|p| (p.id, self.panel_title(&p.kind)))
            .collect();
        self.anim.apply(&scenes, active, &titles);

        // Persist the logical state — every mutation path funnels through
        // sync, so a diff here catches them all. Springs and cameras are
        // deliberately not part of the snapshot.
        let snap = self.ws.snapshot();
        if self.last_saved.as_ref() != Some(&snap) {
            if let Err(e) = self.store.save_wm(&snap) {
                eprintln!("store: persisting the session failed: {e}");
            }
            self.last_saved = Some(snap);
        }
    }

    /// Trackpad pan: 1:1, no spring.
    fn pan(&mut self, dx: f64) {
        self.ws.pan(dx);
        let vp = self.vp();
        let opts = self.opts();
        let cam = self.ws.scene(vp, opts).camera_x;
        self.anim.camera().jump_to(cam);
    }

    fn toast(&mut self, msg: impl Into<String>, err: bool) {
        self.toast = Some((msg.into(), err, Instant::now()));
    }

    /// A panel's title, wherever it lives — for action labels.
    fn title_of(&self, pid: PanelId) -> String {
        self.ws
            .panel(pid)
            .map(|p| self.panel_title(&p.kind))
            .unwrap_or_else(|| "panel".into())
    }

    /// Runs one **undoable action**: mutates the in-memory `Wm`, then
    /// records the whole delta — the UI-table rewrite plus any data
    /// mutation — as one changeset node in the history DAG. The session
    /// consolidates per row, so an identical rewrite contributes nothing;
    /// an action that nets no change creates no node.
    fn act(
        &mut self,
        kind: &str,
        label: String,
        entity: Option<String>,
        mutate: impl FnOnce(&mut Wm),
        data: impl FnOnce(&rusqlite::Transaction) -> rusqlite::Result<()>,
    ) {
        mutate(&mut self.ws);
        let snap = self.ws.snapshot();
        let r = self
            .store
            .act(kind, &label, entity.as_deref(), crate::store::now(), |tx| {
                crate::store::save_wm_tx(tx, &snap)?;
                data(tx)
            });
        if let Err(e) = r {
            eprintln!("store: action “{label}” failed: {e}");
        }
        self.last_saved = Some(snap);
        // Push soon: whatever this action changed about mail intent, a
        // worker makes the server agree without waiting for the poll —
        // and the sender re-times its next deadline.
        for w in &self.workers {
            w.kick();
        }
        if let Some(s) = &self.sender {
            s.kick();
        }
    }

    /// An undoable action that only moves panels around.
    fn act_nav(
        &mut self,
        kind: &str,
        label: String,
        entity: Option<String>,
        mutate: impl FnOnce(&mut Wm),
    ) {
        self.act(kind, label, entity, mutate, |_| Ok(()));
    }

    /// Persists compose drafts that changed since their last save — typing
    /// upkeep, not actions (see the book on what undo deliberately isn't).
    fn persist_drafts(&mut self) {
        let composes: Vec<(PanelId, Option<MailId>)> = self
            .ws
            .wss
            .iter()
            .flat_map(|w| w.panels.values())
            .filter_map(|p| match p.kind {
                Kind::Compose { re } => Some((p.id, (re != 0).then_some(re))),
                _ => None,
            })
            .collect();
        for (pid, re) in composes {
            let Some(ui) = self.ui.get_mut(&pid) else { continue };
            let d = mail::Draft {
                to: ui.to.text.clone(),
                subject: ui.subject.text.clone(),
                body: ui.body.join("\n"),
            };
            if ui.draft_saved.as_ref() == Some(&d) {
                continue;
            }
            mail::save_draft(&self.store, pid as i64, re, &d);
            ui.draft_saved = Some(d);
        }
    }

    /// Spawns a sync worker for every configured account that lacks one.
    /// Idempotent — call after boot and after adding an account. Workers
    /// for removed accounts retire themselves.
    fn spawn_workers(&mut self) {
        let Some(db) = self.db_path.clone() else {
            return;
        };
        if self.sender.is_none() {
            self.sender = Some(send::spawn(db.clone(), || {
                SignalToUI::set_ui_signal();
            }));
        }
        self.workers.retain(|w| {
            mail::accounts(&self.store).iter().any(|a| a.id == w.account)
        });
        for a in mail::accounts(&self.store).iter() {
            if a.imap_host.as_deref().unwrap_or("").is_empty() {
                continue; // the local demo account
            }
            if self.workers.iter().any(|w| w.account == a.id) {
                continue;
            }
            self.workers.push(sync::spawn(db.clone(), a.id, || {
                SignalToUI::set_ui_signal();
            }));
        }
    }

    /// Rebuilds the in-memory `Wm` from the store — the tail of every
    /// undo/redo, whose changesets rewrote the UI tables underneath us.
    fn reload_wm(&mut self) {
        match self.store.load_wm() {
            Ok(Some(snap)) => {
                self.last_saved = Some(snap.clone());
                self.ws = Wm::restore(snap);
            }
            Ok(None) => {
                self.last_saved = None;
                self.ws = Wm::new();
            }
            Err(e) => eprintln!("store: reloading the session failed: {e}"),
        }
        self.field = None;
    }

    /// Rows the inbox panel currently shows.
    fn inbox_rows(&self, pid: PanelId) -> Vec<mail::MailHead> {
        let filter = self
            .ui
            .get(&pid)
            .map(|u| u.filter.text.clone())
            .unwrap_or_default();
        mail::inbox_filtered(&self.store, &filter)
    }
}

// ---------------------------------------------------------------------------
// Content builders

fn build_lines(state: &State, pid: PanelId, cols: usize) -> Vec<Line> {
    // Cross-workspace lookup: panels on inactive spaces draw mid-slide too.
    let Some(panel) = state.ws.panel(pid) else {
        return Vec::new();
    };
    let ui = state.ui.get(&pid);
    match &panel.kind {
        Kind::Help => help_lines(),
        Kind::About => about_lines(),
        Kind::Inbox { .. } => inbox_lines(state, pid, cols),
        Kind::Message { id } => message_lines(state, id, cols),
        Kind::Contact { email } => contact_lines(state, email),
        Kind::Compose { re } => compose_lines(ui, re),
        Kind::Settings => Vec::new(), // retained content (CR-002)
    }
}


fn help_lines() -> Vec<Line> {
    let about = Kind::About;
    let mut v = Vec::new();
    v.push(Line {
        left: vec![Seg::T("LEGEND".into(), Style::Label)],
        rule: true,
        ..Default::default()
    });
    v.push(Line {
        left: vec![
            Seg::Link {
                label: "solid underline".into(),
                target: about.clone(),
                dotted: false,
            },
            Seg::T(" — opens a panel to the right, joined".into(), Style::N),
        ],
        ..Default::default()
    });
    v.push(Line {
        left: vec![
            Seg::Link {
                label: "dotted underline".into(),
                target: about.clone(),
                dotted: true,
            },
            Seg::T(" — replaces this panel in place".into(), Style::N),
        ],
        ..Default::default()
    });
    v.push(Line {
        left: vec![
            Seg::Btn {
                label: "button".into(),
                act: BtnAct::TryIt,
            },
            Seg::T(" — side effect only, never navigation".into(), Style::N),
        ],
        ..Default::default()
    });
    v.push(Line {
        left: vec![
            kbd("cmd"),
            Seg::T("+click / ".into(), Style::N),
            kbd("cmd"),
            kbd("enter"),
            Seg::T(" — always a fresh,".into(), Style::N),
        ],
        ..Default::default()
    });
    v.push(Line::text("un-joined panel", Style::N));
    v.push(Line::text("a ═ bridge marks a joined pair: the next", Style::N));
    v.push(Line::text("solid link in the parent replaces the", Style::N));
    v.push(Line::text("joined panel; replacing a panel closes", Style::N));
    v.push(Line::text("its joined chain", Style::N));
    v.push(Line {
        left: vec![
            Seg::T("color is reserved for errors: ".into(), Style::N),
            Seg::T("like this".into(), Style::Err),
        ],
        ..Default::default()
    });
    v.push(Line::blank());
    v.push(Line {
        left: vec![Seg::T("KEYS".into(), Style::Label)],
        rule: true,
        ..Default::default()
    });
    v.push(Line {
        left: vec![kbd("cmd"), Seg::T("+arrows / hjkl — focus panels".into(), Style::N)],
        ..Default::default()
    });
    v.push(Line {
        left: vec![kbd("cmd"), kbd("shift"), Seg::T("+same — move the panel".into(), Style::N)],
        ..Default::default()
    });
    v.push(Line {
        left: vec![kbd("cmd"), kbd("w"), Seg::T(" — close the focused panel".into(), Style::N)],
        ..Default::default()
    });
    v.push(Line {
        left: vec![
            kbd("cmd"),
            kbd("z"),
            Seg::T(" — undo (open, close, move, archive…)".into(), Style::N),
        ],
        ..Default::default()
    });
    v.push(Line {
        left: vec![
            kbd("cmd"),
            kbd("shift"),
            kbd("z"),
            Seg::T(" — redo".into(), Style::N),
        ],
        ..Default::default()
    });
    v.push(Line {
        left: vec![
            kbd("cmd"),
            kbd("u"),
            Seg::T(" — history: the whole tree, walkable".into(), Style::N),
        ],
        ..Default::default()
    });
    v.push(Line {
        left: vec![
            kbd("cmd"),
            kbd("i"),
            Seg::T(" — copy the panel's context (its queries)".into(), Style::N),
        ],
        ..Default::default()
    });
    v.push(Line {
        left: vec![
            kbd("cmd"),
            kbd("["),
            kbd("]"),
            Seg::T(" — consume into / expel out of a column".into(), Style::N),
        ],
        ..Default::default()
    });
    v.push(Line {
        left: vec![
            kbd("cmd"),
            kbd(","),
            kbd("."),
            Seg::T(" — pull from the right / push bottom out".into(), Style::N),
        ],
        ..Default::default()
    });
    v.push(Line {
        left: vec![
            kbd("cmd"),
            kbd("t"),
            Seg::T(" — column tabs (click a tab or cmd+j/k)".into(), Style::N),
        ],
        ..Default::default()
    });
    v.push(Line::text("plain keys belong to the focused panel:", Style::N));
    v.push(Line {
        left: vec![
            Seg::T("  inbox ".into(), Style::N),
            kbd("j"),
            kbd("k"),
            kbd("enter"),
            kbd("/"),
            Seg::T("  message ".into(), Style::N),
            kbd("j"),
            kbd("k"),
            kbd("r"),
        ],
        ..Default::default()
    });
    v.push(Line {
        left: vec![kbd("esc"), Seg::T(" leaves a text field".into(), Style::N)],
        ..Default::default()
    });
    v.push(Line::text("trackpad: scroll the strip and the panels", Style::N));
    v.push(Line::blank());
    v.push(Line {
        left: vec![Seg::T("WORKSPACES".into(), Style::Label)],
        rule: true,
        ..Default::default()
    });
    v.push(Line {
        left: vec![
            kbd("cmd"),
            kbd("1"),
            Seg::T("…".into(), Style::N),
            kbd("9"),
            Seg::T(" — switch workspace".into(), Style::N),
        ],
        ..Default::default()
    });
    v.push(Line {
        left: vec![
            kbd("cmd"),
            kbd("shift"),
            Seg::T("+№ — move the panel there".into(), Style::N),
        ],
        ..Default::default()
    });
    if cfg!(target_os = "macos") {
        v.push(Line::text("the menu bar lists them; [n] is current", Style::N));
    }
    v.push(Line::blank());
    v.push(Line {
        left: vec![Seg::T("LAUNCHER".into(), Style::Label)],
        rule: true,
        ..Default::default()
    });
    if !cfg!(target_os = "android") {
        v.push(Line {
            left: vec![
                kbd("cmd"),
                kbd("cmd"),
                Seg::T(" — the launcher: search everything".into(), Style::N),
            ],
            ..Default::default()
        });
        v.push(Line::text("type to find panels, mail, people;", Style::N));
        v.push(Line::text("enter goes to it — or opens it fresh", Style::N));
    } else {
        v.push(Line::text("the overlay's search row opens it:", Style::N));
        v.push(Line::text("find open panels, mail, people", Style::N));
    }
    if cfg!(target_os = "android") {
        v.push(Line::blank());
        v.push(Line {
            left: vec![Seg::T("TOUCH".into(), Style::Label)],
            rule: true,
            ..Default::default()
        });
        v.push(Line::text("tap — follow links, press buttons", Style::N));
        v.push(Line::text("drag — scroll a panel's content", Style::N));
        v.push(Line::text("two fingers — scroll the workspace", Style::N));
        v.push(Line::text("two fingers down — workspaces overlay", Style::N));
        v.push(Line::text("hold a header — pick the panel up;", Style::N));
        v.push(Line::text("drop on a column to stack, between", Style::N));
        v.push(Line::text("columns for a fresh one", Style::N));
    }
    v.push(Line::blank());
    v.push(Line {
        left: vec![Seg::T("TRY".into(), Style::Label)],
        rule: true,
        ..Default::default()
    });
    v.push(Line::text("1. click a subject — a message opens,", Style::N));
    v.push(Line::text("   joined (bridge)", Style::N));
    v.push(Line::text("2. click another subject — it replaces", Style::N));
    v.push(Line::text("   the joined message", Style::N));
    v.push(Line::text("3. from → contact joins the chain; the", Style::N));
    v.push(Line::text("   next subject click closes the chain", Style::N));
    v.push(Line::text("4. cmd+shift+← the message — moved away,", Style::N));
    v.push(Line::text("   it un-joins", Style::N));
    v
}

fn about_lines() -> Vec<Line> {
    vec![
        Line::text("superapp — rust + makepad prototype.", Style::N),
        Line::text("no apps, no windows: specialized panels", Style::N),
        Line::text("on one scrolling gridded workspace.", Style::N),
        Line::blank(),
        Line {
            left: vec![Seg::Link {
                label: "back to help".into(),
                target: Kind::Help,
                dotted: true,
            }],
            ..Default::default()
        },
    ]
}

fn inbox_lines(state: &State, pid: PanelId, cols: usize) -> Vec<Line> {
    let mut v = Vec::new();
    v.push(Line {
        left: vec![Seg::Fld {
            id: FieldId::Filter,
            w: cols.saturating_sub(2).max(10),
        }],
        pin: true,
        ..Default::default()
    });
    let from_w = 11usize;
    let date_w = 12usize;
    let subj_w = cols.saturating_sub(from_w + date_w + 3).max(8);
    v.push(Line {
        left: vec![
            Seg::T(pad_to("FROM", from_w), Style::Label),
            Seg::Sp(1),
            Seg::T("SUBJECT".into(), Style::Label),
        ],
        right: vec![Seg::T("DATE".into(), Style::Label)],
        rule: true,
        rule_ink: true,
        pin: true,
        ..Default::default()
    });
    let rows = state.inbox_rows(pid);
    if rows.is_empty() {
        v.push(Line::text("no messages", Style::Muted));
    }
    for m in rows {
        let st = if m.unread { Style::Bold } else { Style::N };
        v.push(Line {
            left: vec![
                Seg::T(pad_to(&m.from_name, from_w), st),
                Seg::Sp(1),
                Seg::Link {
                    label: trunc(&m.subject, subj_w),
                    target: Kind::Message { id: m.id },
                    dotted: false,
                },
            ],
            right: vec![Seg::T(mail::fmt_date(m.date), Style::Muted)],
            row: Some(m.id),
            rule: true,
            ..Default::default()
        });
    }
    v
}

fn message_lines(state: &State, id: &MailId, cols: usize) -> Vec<Line> {
    let Some(m) = mail::mail(&state.store, *id) else {
        return vec![Line::text("message not found", Style::Muted)];
    };
    let (newer, older) = mail::neighbours(&state.store, *id);
    let mut v = Vec::new();
    v.push(Line {
        left: vec![
            Seg::T(pad_to("FROM", 6), Style::Label),
            Seg::Link {
                label: format!("{} <{}>", m.head.from_name, m.head.from_email),
                target: Kind::Contact {
                    email: m.head.from_email.clone(),
                },
                dotted: false,
            },
        ],
        ..Default::default()
    });
    v.push(Line {
        left: vec![
            Seg::T(pad_to("TO", 6), Style::Label),
            Seg::T(m.to.clone(), Style::Muted),
        ],
        ..Default::default()
    });
    v.push(Line {
        left: vec![
            Seg::T(pad_to("DATE", 6), Style::Label),
            Seg::T(mail::fmt_date(m.head.date), Style::N),
        ],
        rule: true,
        ..Default::default()
    });
    if let Some((s, err)) = &m.status {
        v.push(Line::text(s.clone(), if *err { Style::Err } else { Style::T2 }));
        v.push(Line::blank());
    }
    for para in m.body.split("\n\n") {
        for l in wrap(para, cols) {
            v.push(Line::text(l, Style::N));
        }
        v.push(Line::blank());
    }
    let mut nav = Line {
        rule: false,
        ..Default::default()
    };
    nav.left = vec![
        if let Some(n) = newer {
            Seg::Link {
                label: "← newer".into(),
                target: Kind::Message { id: n },
                dotted: true,
            }
        } else {
            Seg::T("← newer".into(), Style::Muted)
        },
        Seg::Sp(2),
        if let Some(o) = older {
            Seg::Link {
                label: "older →".into(),
                target: Kind::Message { id: o },
                dotted: true,
            }
        } else {
            Seg::T("older →".into(), Style::Muted)
        },
    ];
    nav.right = vec![Seg::Link {
        label: "reply".into(),
        target: Kind::Compose { re: m.head.id },
        dotted: false,
    }];
    v.push(nav);
    v
}

fn contact_lines(state: &State, email: &str) -> Vec<Line> {
    let (name, count) = mail::contact(&state.store, email);
    let first = name.split(' ').next().unwrap_or(&name).to_lowercase();
    vec![
        Line::text(name.clone(), Style::Big),
        Line::text(email, Style::Muted),
        Line::blank(),
        Line::text(format!("{count} message(s) in mail"), Style::N),
        Line::blank(),
        Line {
            left: vec![Seg::Link {
                label: format!("messages from {first}"),
                target: Kind::Inbox {
                    filter: Some(email.to_string()),
                },
                dotted: false,
            }],
            ..Default::default()
        },
    ]
}

fn compose_lines(ui: Option<&PanelUi>, _re: &MailId) -> Vec<Line> {
    let body_rows = ui.map(|u| u.body.len()).unwrap_or(1);
    let mut v = Vec::new();
    v.push(Line {
        left: vec![
            Seg::T(pad_to("TO", 8), Style::Label),
            Seg::Fld {
                id: FieldId::To,
                w: 30,
            },
        ],
        ..Default::default()
    });
    v.push(Line {
        left: vec![
            Seg::T(pad_to("SUBJECT", 8), Style::Label),
            Seg::Fld {
                id: FieldId::Subject,
                w: 30,
            },
        ],
        rule: true,
        ..Default::default()
    });
    // The body: free lines; the whole region is one Field hit (built in
    // draw). Send and discard live in the header, with the other side
    // effects — nothing floats below the text.
    for i in 0..body_rows {
        let text = ui.map(|u| u.body[i].clone()).unwrap_or_default();
        v.push(Line {
            left: vec![Seg::T(text, Style::N), Seg::Fld { id: FieldId::Body, w: 0 }],
            ..Default::default()
        });
    }
    v
}

// ---------------------------------------------------------------------------
// Stage
// ---------------------------------------------------------------------------

/// Measured mono metrics at [`theme::FONT_SIZE`], in points: advance per char,
/// the line grid, the ascender (underlines hang off it), and the natural
/// (ascender−descender) line for vertical centering.
#[derive(Debug, Clone, Copy)]
struct CellFont {
    adv: f64,
    line_h: f64,
    asc: f64,
    natural: f64,
    dpi: f64,
}

impl Default for CellFont {
    fn default() -> Self {
        CellFont {
            adv: theme::FONT_SIZE * 0.8,
            line_h: theme::FONT_SIZE * 2.0,
            asc: theme::FONT_SIZE * 1.24,
            natural: theme::FONT_SIZE * 1.55,
            dpi: 0.0,
        }
    }
}

impl CellFont {
    /// Advance of one label-size character, tracking excluded.
    fn label_adv(&self) -> f64 {
        self.adv * (theme::LABEL_SIZE / theme::FONT_SIZE)
    }
    /// Advance of one label-size character, tracking included.
    fn label_step(&self) -> f64 {
        self.label_adv() * (1.0 + theme::LABEL_TRACK)
    }
    /// Drawn width of a tracked label.
    fn label_w(&self, chars: usize) -> f64 {
        if chars == 0 {
            return 0.0;
        }
        chars as f64 * self.label_step() - self.label_adv() * theme::LABEL_TRACK
    }
    /// Natural line height at label size, for vertical centering.
    fn label_line(&self) -> f64 {
        self.natural * (theme::LABEL_SIZE / theme::FONT_SIZE)
    }
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

    /// Retained content templates by DSL name (CR-002) and the live
    /// instance per panel id — the PortalList pattern at panel scale.
    #[rust]
    tpl: HashMap<LiveId, ScriptObjectRef>,
    #[rust]
    hosted: HashMap<PanelId, WidgetRef>,

    #[redraw]
    #[live]
    draw_panel: DrawPanel,
    #[live]
    draw_flat: DrawFlat,
    #[live]
    draw_mono: DrawText,

    #[rust]
    area: Area,
    #[rust]
    next_frame: NextFrame,
    #[rust]
    origin: DVec2,
    #[rust]
    cell: CellFont,
    #[rust]
    hits: Vec<HitR>,
    #[rust]
    reported: bool,
    #[rust]
    ime_shown: bool,
    /// The field the IME is currently shown for (config + seeding follow it).
    #[rust]
    ime_field: Option<(PanelId, FieldId)>,
    /// Last `(text, caret)` pushed to the IME editable, to dedup syncs.
    #[rust]
    ime_sent: Option<(String, usize)>,
    /// The IME is mid-composition: hold app→IME syncs, they would clobber it.
    #[rust]
    ime_composing: bool,
    /// Soft-keyboard bottom occlusion (android), in points.
    #[rust]
    kb_h: f64,
    #[rust]
    touch: TouchNav,
    /// The drop-preview insertion bar while a panel is dragged, strip coords.
    #[rust]
    drag_hint: Option<core::Rect>,
    /// Safe-area insets (cutouts, rounded corners): top, right, bottom, left.
    #[rust]
    insets: (f64, f64, f64, f64),
    /// What the macOS menu bar currently shows: `(workspace, is_current)`
    /// per roster entry. Menus rebuild only when this changes.
    #[rust]
    menu_sig: Vec<(usize, bool)>,
    /// The double-cmd launcher trigger.
    #[rust]
    cmd_tap: CmdTap,
    #[rust]
    e2e: Option<e2e::Runner>,
    #[rust]
    e2e_timer: Timer,
    /// The fallback store poll (see [`Stage::poll_store`]).
    #[rust]
    poll_timer: Timer,
    #[rust]
    state: Option<Box<State>>,
}

/// Menu command id bases: workspace `k`'s items are `base + k`. Plain
/// numbers (not `live_id!` hashes) — the ranges cannot collide with the one
/// hashed command makepad special-cases, `quit`.
const WS_MENU_SWITCH: u64 = 0x5753_0100;
const WS_MENU_MOVE: u64 = 0x5753_0200;
const MENU_LAUNCHER: u64 = 0x5753_0300;
const MENU_UNDO: u64 = 0x5753_0400;
const MENU_REDO: u64 = 0x5753_0401;
const MENU_HISTORY: u64 = 0x5753_0500;

/// How a `key` chord executes: as a synthesized key event, as text (plain
/// letters reach panels the same way real typing does), or as a bare
/// modifier tap — a down/up pair (`key cmd 2` double-taps cmd).
enum ChordExec {
    Ev(KeyEvent),
    Text(String),
    Tap(KeyCode),
}

fn parse_chord(s: &str) -> Option<ChordExec> {
    if let "cmd" | "logo" | "super" = s {
        return Some(ChordExec::Tap(KeyCode::Logo));
    }
    let mut mods = KeyModifiers::default();
    let mut key: Option<&str> = None;
    for part in s.split('+') {
        match part {
            "cmd" | "logo" | "super" => mods.logo = true,
            "shift" => mods.shift = true,
            "alt" | "option" => mods.alt = true,
            "ctrl" | "control" => mods.control = true,
            k => key = Some(k),
        }
    }
    let key = key?;
    let code = match key {
        "left" => Some(KeyCode::ArrowLeft),
        "right" => Some(KeyCode::ArrowRight),
        "up" => Some(KeyCode::ArrowUp),
        "down" => Some(KeyCode::ArrowDown),
        "enter" | "return" => Some(KeyCode::ReturnKey),
        "esc" | "escape" => Some(KeyCode::Escape),
        "backspace" => Some(KeyCode::Backspace),
        "delete" => Some(KeyCode::Delete),
        "h" => Some(KeyCode::KeyH),
        "j" => Some(KeyCode::KeyJ),
        "k" => Some(KeyCode::KeyK),
        "l" => Some(KeyCode::KeyL),
        "w" => Some(KeyCode::KeyW),
        "t" => Some(KeyCode::KeyT),
        "z" => Some(KeyCode::KeyZ),
        "u" => Some(KeyCode::KeyU),
        "i" => Some(KeyCode::KeyI),
        "tab" => Some(KeyCode::Tab),
        "comma" | "," => Some(KeyCode::Comma),
        "period" | "." => Some(KeyCode::Period),
        "bracketleft" | "[" => Some(KeyCode::LBracket),
        "bracketright" | "]" => Some(KeyCode::RBracket),
        "1" => Some(KeyCode::Key1),
        "2" => Some(KeyCode::Key2),
        "3" => Some(KeyCode::Key3),
        "4" => Some(KeyCode::Key4),
        "5" => Some(KeyCode::Key5),
        "6" => Some(KeyCode::Key6),
        "7" => Some(KeyCode::Key7),
        "8" => Some(KeyCode::Key8),
        "9" => Some(KeyCode::Key9),
        _ => None,
    };
    let plain = !mods.logo && !mods.control && !mods.alt;
    match (code, plain, key.chars().count()) {
        // A modified chord, or a control key: a real key event.
        (Some(c), false, _)
        | (
            Some(c @ (KeyCode::ArrowLeft
            | KeyCode::ArrowRight
            | KeyCode::ArrowUp
            | KeyCode::ArrowDown
            | KeyCode::ReturnKey
            | KeyCode::Escape
            | KeyCode::Backspace
            | KeyCode::Delete
            | KeyCode::Tab)),
            true,
            _,
        ) => Some(ChordExec::Ev(KeyEvent {
            key_code: c,
            modifiers: mods,
            is_repeat: false,
            time: 0.0,
        })),
        // A plain letter: panels receive it as text, like real typing.
        (_, true, 1) => Some(ChordExec::Text(key.to_string())),
        _ => None,
    }
}

impl Stage {
    fn kick(&mut self, cx: &mut Cx) {
        // Compose drafts persist as they are typed (plain upkeep, not
        // actions): every edit path funnels through kick.
        if let Some(state) = self.state.as_deref_mut() {
            state.persist_drafts();
        }
        // makepad only routes keys to the system IME — which is what emits
        // TextInput events — while the IME is shown. On macOS letter keys
        // (j/k/r, "/", field typing) all arrive that way, so the IME stays on
        // whenever a panel has focus (mosaic's model: "typing only flows
        // after show_text_ime"). On android show_text_ime raises the
        // on-screen keyboard, so it is tied to a text field instead — glass
        // has no letter keys to lose. Every focus transition passes through
        // kick().
        // The launcher's query field wants the IME on both targets — on
        // android that is what raises the soft keyboard with the overlay.
        let launcher = self
            .state
            .as_deref()
            .is_some_and(|s| s.overlay == Overlay::Launcher);
        // A retained panel's TextInputs own the IME lifecycle and key
        // focus themselves — the char-grid machinery must not fight them.
        let hosted = self.hosted_focus();
        let want_ime = launcher
            || (!hosted
                && self.state.as_deref().is_some_and(|s| {
                    if cfg!(target_os = "android") {
                        s.field.is_some()
                    } else {
                        s.ws.focus.is_some()
                    }
                }));
        let want_field = if launcher {
            None
        } else {
            self.state.as_deref().and_then(|s| s.field)
        };
        if want_ime != self.ime_shown || (want_ime && self.ime_field != want_field) {
            self.ime_shown = want_ime;
            self.ime_field = want_field;
            self.ime_sent = None;
            self.ime_composing = false;
            if want_ime {
                cx.set_key_focus(self.area);
                cx.show_text_ime_with_config(
                    self.area,
                    rect(0.0, 0.0, 0.0, 0.0),
                    TextInputConfig {
                        is_multiline: matches!(want_field, Some((_, FieldId::Body))),
                        ..Default::default()
                    },
                );
            } else if !hosted {
                // Also resets makepad's "user dismissed the keyboard" latch,
                // without which the next show request is silently ignored.
                cx.hide_text_ime();
            }
        }
        // android's IME owns an editable mirroring the focused field; seed it
        // on focus and after every app-side edit — except mid-composition,
        // when a push would clobber what the keyboard is composing.
        if cfg!(target_os = "android") && self.ime_shown && !self.ime_composing {
            if let Some((text, caret)) = self.state.as_deref().and_then(|s| {
                if launcher {
                    Some((s.launcher.query.clone(), s.launcher.caret))
                } else {
                    s.field.and_then(|(pid, fid)| field_text_caret(s, pid, fid))
                }
            }) {
                let sent = self.ime_sent.as_ref();
                if sent.map(|(t, c)| (t.as_str(), *c)) != Some((text.as_str(), caret)) {
                    self.ime_sent = Some((text.clone(), caret));
                    cx.sync_ime_state(text, CharOffset(caret)..CharOffset(caret), None);
                }
            }
        }
        if let Some(state) = self.state.as_deref_mut() {
            if !state.animating {
                state.last_frame = Some(Instant::now());
                state.animating = true;
            }
        }
        self.next_frame = cx.new_next_frame();
        cx.redraw_all();
    }

    /// A mutation happened: recompute targets, animate, redraw.
    fn sync(&mut self, cx: &mut Cx) {
        if let Some(state) = self.state.as_deref_mut() {
            state.sync();
        }
        self.update_menu(cx);
        self.kick(cx);
    }

    /// The macOS menu bar mirrors the workspaces: one menu per roster entry,
    /// the current one bracketed. The bold app menu itself is
    /// AppKit-mandatory — it cannot be removed, so it keeps only Quit. The
    /// items carry no key equivalents (the KeyDown path owns cmd+№; the
    /// labels document it); rebuilds happen only when the signature changes,
    /// never per keystroke.
    fn update_menu(&mut self, cx: &mut Cx) {
        if !cfg!(target_os = "macos") {
            return;
        }
        let Some(state) = self.state.as_deref() else {
            return;
        };
        let sig: Vec<(usize, bool)> = state
            .ws
            .roster()
            .into_iter()
            .map(|k| (k, k == state.ws.active))
            .collect();
        if sig == self.menu_sig {
            return;
        }
        self.menu_sig = sig.clone();
        let mut items = vec![MacosMenu::Sub {
            name: "superapp".into(),
            items: vec![
                MacosMenu::Item {
                    command: LiveId(MENU_LAUNCHER),
                    key: KeyCode::Unknown,
                    shift: false,
                    enabled: true,
                    name: "Launcher — ⌘ ⌘".into(),
                },
                // Chords live in the KeyDown path (the shifted-digit menu-key
                // table is off by one upstream); labels carry the hint.
                MacosMenu::Item {
                    command: LiveId(MENU_UNDO),
                    key: KeyCode::Unknown,
                    shift: false,
                    enabled: true,
                    name: "Undo — ⌘Z".into(),
                },
                MacosMenu::Item {
                    command: LiveId(MENU_REDO),
                    key: KeyCode::Unknown,
                    shift: false,
                    enabled: true,
                    name: "Redo — ⇧⌘Z".into(),
                },
                MacosMenu::Item {
                    command: LiveId(MENU_HISTORY),
                    key: KeyCode::Unknown,
                    shift: false,
                    enabled: true,
                    name: "History — ⌘U".into(),
                },
                MacosMenu::Item {
                    command: live_id!(quit),
                    key: KeyCode::KeyQ,
                    shift: false,
                    enabled: true,
                    name: "Quit superapp".into(),
                },
            ],
        }];
        for (k, current) in sig {
            let name = if current {
                format!("[{}]", k + 1)
            } else {
                format!("{}", k + 1)
            };
            items.push(MacosMenu::Sub {
                name,
                items: vec![
                    MacosMenu::Item {
                        command: LiveId(WS_MENU_SWITCH + k as u64),
                        key: KeyCode::Unknown,
                        shift: false,
                        enabled: !current,
                        name: format!("Switch Here — ⌘{}", k + 1),
                    },
                    MacosMenu::Item {
                        command: LiveId(WS_MENU_MOVE + k as u64),
                        key: KeyCode::Unknown,
                        shift: true,
                        enabled: !current,
                        name: format!("Move Panel Here — ⇧⌘{}", k + 1),
                    },
                ],
            });
        }
        cx.update_macos_menu(MacosMenu::Main { items });
    }

    fn hit_at(&self, p: DVec2) -> Option<&HitR> {
        self.hits.iter().rev().find(|h| h.rect.contains(p))
    }

    /// Executes at most one e2e step per timer tick; waits pace the script.
    fn e2e_tick(&mut self, cx: &mut Cx) {
        let Some(mut runner) = self.e2e.take() else {
            return;
        };
        if let Some(step) = runner.next_step() {
            match step {
                e2e::Step::Wait(_) => {}
                e2e::Step::Shot(name) => {
                    let path = runner.out.join(format!("{name}.png"));
                    #[cfg(target_os = "macos")]
                    match crate::mac::screenshot(&path) {
                        Ok(()) => eprintln!("e2e: shot {}", path.display()),
                        Err(e) => {
                            eprintln!("e2e: FAIL shot {name}: {e}");
                            runner.failures += 1;
                        }
                    }
                    #[cfg(not(target_os = "macos"))]
                    {
                        eprintln!("e2e: FAIL shot {}: screenshots need macos", path.display());
                        runner.failures += 1;
                    }
                }
                e2e::Step::Click { label, fresh } => {
                    let needle = label.to_lowercase();
                    let hit = self
                        .hits
                        .iter()
                        .rev()
                        .find(|h| {
                            !matches!(h.act, Act::Focus(_))
                                && h.label.to_lowercase().contains(&needle)
                        })
                        .or_else(|| {
                            self.hits
                                .iter()
                                .rev()
                                .find(|h| h.label.to_lowercase().contains(&needle))
                        })
                        .map(|h| (h.act.clone(), h.rect));
                    match hit {
                        Some((act, r)) => {
                            eprintln!("e2e: click {label:?}{}", if fresh { " (cmd)" } else { "" });
                            if let Act::Pointer(_) = act {
                                // A retained widget: press it for real.
                                self.synth_click(cx, r.pos + r.size / 2.0);
                            }
                            self.resolve_click(cx, act, fresh);
                        }
                        None => {
                            eprintln!("e2e: FAIL click {label:?}: no matching element");
                            runner.failures += 1;
                        }
                    }
                }
                e2e::Step::Key { chord, times } => match parse_chord(&chord) {
                    Some(exec) => {
                        eprintln!("e2e: key {chord} ×{times}");
                        for _ in 0..times.max(1) {
                            match &exec {
                                ChordExec::Ev(ev) => self.handle_key_down(cx, ev),
                                ChordExec::Text(s) => self.handle_text(cx, s),
                                ChordExec::Tap(code) => {
                                    // A bare modifier press-release, the way
                                    // flagsChanged delivers it: the modifier
                                    // itself is set on the down, gone on the up.
                                    let down = KeyEvent {
                                        key_code: *code,
                                        modifiers: KeyModifiers {
                                            logo: *code == KeyCode::Logo,
                                            ..Default::default()
                                        },
                                        is_repeat: false,
                                        time: 0.0,
                                    };
                                    let mut up = down.clone();
                                    up.modifiers = KeyModifiers::default();
                                    self.handle_key_down(cx, &down);
                                    self.handle_key_up(cx, &up);
                                }
                            }
                        }
                    }
                    None => {
                        eprintln!("e2e: FAIL key {chord:?}: cannot parse chord");
                        runner.failures += 1;
                    }
                },
                e2e::Step::Type(s) => {
                    eprintln!("e2e: type {s:?}");
                    self.handle_text(cx, &s);
                }
                e2e::Step::Swipe { label, dx, dy } => {
                    let needle = label.to_lowercase();
                    let c = self
                        .hits
                        .iter()
                        .rev()
                        .find(|h| h.label.to_lowercase().contains(&needle))
                        .map(|h| h.rect.pos + h.rect.size / 2.0);
                    match c {
                        Some(c) => {
                            eprintln!("e2e: swipe {label:?} by ({dx}, {dy})");
                            self.touch_start(1, c);
                            for i in 1..=8 {
                                let f = f64::from(i) / 8.0;
                                self.touch_move(cx, 1, dvec2(c.x + dx * f, c.y + dy * f));
                            }
                            self.touch_stop(cx, 1, dvec2(c.x + dx, c.y + dy));
                        }
                        None => {
                            eprintln!("e2e: FAIL swipe {label:?}: no matching element");
                            runner.failures += 1;
                        }
                    }
                }
                e2e::Step::Pan2 { dx, dy } => {
                    eprintln!("e2e: pan2 by ({dx}, {dy})");
                    let vp = self
                        .state
                        .as_deref()
                        .map(|s| s.viewport)
                        .unwrap_or(dvec2(800.0, 600.0));
                    let mid = self.origin + dvec2(vp.x / 2.0, vp.y / 2.0);
                    let (a, b) = (mid - dvec2(40.0, 0.0), mid + dvec2(40.0, 0.0));
                    self.touch_start(1, a);
                    self.touch_start(2, b);
                    for i in 1..=8 {
                        let f = f64::from(i) / 8.0;
                        self.touch_move(cx, 1, dvec2(a.x + f * dx, a.y + f * dy));
                        self.touch_move(cx, 2, dvec2(b.x + f * dx, b.y + f * dy));
                    }
                    self.touch_stop(cx, 1, dvec2(a.x + dx, a.y + dy));
                    self.touch_stop(cx, 2, dvec2(b.x + dx, b.y + dy));
                }
                e2e::Step::Drop => {
                    if let TouchMode::Drag { uid, .. } = self.touch.mode {
                        let p = self
                            .touch
                            .pts
                            .get(&uid)
                            .map(|&(_, p)| p)
                            .unwrap_or(self.origin);
                        eprintln!("e2e: drop");
                        self.touch_stop(cx, uid, p);
                    } else {
                        eprintln!("e2e: FAIL drop: nothing is being dragged");
                        runner.failures += 1;
                    }
                }
                e2e::Step::HoldMove { label, dx, dy, hold } => {
                    let needle = label.to_lowercase();
                    // Press the panel's header: only Focus hits carry one.
                    let c = self
                        .hits
                        .iter()
                        .rev()
                        .find(|h| {
                            matches!(h.act, Act::Focus(_))
                                && h.label.to_lowercase().contains(&needle)
                        })
                        .map(|h| {
                            dvec2(
                                h.rect.pos.x + h.rect.size.x / 2.0,
                                h.rect.pos.y + theme::HEAD_H / 2.0,
                            )
                        });
                    match c {
                        Some(c) => {
                            eprintln!("e2e: holdmove {label:?} by ({dx}, {dy})");
                            self.touch_start(1, c);
                            self.long_press(cx, 1, c);
                            if !matches!(self.touch.mode, TouchMode::Drag { .. }) {
                                eprintln!("e2e: FAIL holdmove {label:?}: header did not grab");
                                runner.failures += 1;
                                self.touch_stop(cx, 1, c);
                            } else {
                                for i in 1..=8 {
                                    let f = f64::from(i) / 8.0;
                                    self.touch_move(cx, 1, dvec2(c.x + dx * f, c.y + dy * f));
                                }
                                if !hold {
                                    self.touch_stop(cx, 1, dvec2(c.x + dx, c.y + dy));
                                }
                            }
                        }
                        None => {
                            eprintln!("e2e: FAIL holdmove {label:?}: no matching panel");
                            runner.failures += 1;
                        }
                    }
                }
                e2e::Step::Quit => {
                    eprintln!(
                        "e2e: done — {} step(s), {} failure(s)",
                        runner.steps.len(),
                        runner.failures
                    );
                    if runner.failures > 0 {
                        std::process::exit(1);
                    }
                    cx.quit();
                }
            }
        }
        self.e2e = Some(runner);
    }

    fn handle_key_down(&mut self, cx: &mut Cx, k: &KeyEvent) {
        let hosted = self.hosted_focus();
        // A bare cmd press only feeds the double-tap detector (the launcher
        // trigger); the firing side lives in handle_key_up.
        if k.key_code == KeyCode::Logo {
            if !k.is_repeat {
                self.cmd_tap.press(k.time);
            }
            return;
        }
        self.cmd_tap.other_input();
        let Some(state) = self.state.as_deref_mut() else {
            return;
        };
        // The launcher owns the keyboard while it is up: arrows pick, enter
        // goes, esc closes; characters edit the query (they arrive as
        // TextInput, backspace and caret moves are handled here).
        if state.overlay == Overlay::Launcher {
            match k.key_code {
                KeyCode::Escape => {
                    state.overlay = Overlay::None;
                    self.kick(cx);
                }
                KeyCode::ReturnKey => {
                    let hit = state.launcher.hits.get(state.launcher.sel).cloned();
                    if let Some(hit) = hit {
                        self.launcher_go(cx, hit);
                    }
                }
                KeyCode::ArrowDown => {
                    state.launcher.sel += 1; // clamped against the hits on draw
                    self.kick(cx);
                }
                KeyCode::ArrowUp => {
                    state.launcher.sel = state.launcher.sel.saturating_sub(1);
                    self.kick(cx);
                }
                KeyCode::ArrowLeft => {
                    state.launcher.caret = state.launcher.caret.saturating_sub(1);
                    self.kick(cx);
                }
                KeyCode::ArrowRight => {
                    let len = state.launcher.query.chars().count();
                    state.launcher.caret = (state.launcher.caret + 1).min(len);
                    self.kick(cx);
                }
                KeyCode::Backspace => {
                    let c = state.launcher.caret;
                    if c > 0 {
                        let b0 = char_byte(&state.launcher.query, c - 1);
                        let b1 = char_byte(&state.launcher.query, c);
                        state.launcher.query.replace_range(b0..b1, "");
                        state.launcher.caret = c - 1;
                        state.launcher.sel = 0;
                    }
                    self.kick(cx);
                }
                _ => {}
            }
            return;
        }
        // The workspaces overlay dismisses on esc; cmd chords below still
        // work through it.
        if matches!(state.overlay, Overlay::Ws | Overlay::History)
            && k.key_code == KeyCode::Escape
        {
            state.overlay = Overlay::None;
            self.kick(cx);
            return;
        }
        // Cmd is the workspace modifier (niri's Mod; mosaic's choice too).
        if k.modifiers.logo {
            let num = match k.key_code {
                KeyCode::Key1 => Some(0),
                KeyCode::Key2 => Some(1),
                KeyCode::Key3 => Some(2),
                KeyCode::Key4 => Some(3),
                KeyCode::Key5 => Some(4),
                KeyCode::Key6 => Some(5),
                KeyCode::Key7 => Some(6),
                KeyCode::Key8 => Some(7),
                KeyCode::Key9 => Some(8),
                _ => None,
            };
            if let Some(n) = num {
                if k.modifiers.shift {
                    self.move_focused_to_ws(cx, n);
                } else {
                    self.switch_ws(cx, n);
                }
                return;
            }
            let dir = match k.key_code {
                KeyCode::ArrowLeft | KeyCode::KeyH => Some(Dir::Left),
                KeyCode::ArrowRight | KeyCode::KeyL => Some(Dir::Right),
                KeyCode::ArrowUp | KeyCode::KeyK => Some(Dir::Up),
                KeyCode::ArrowDown | KeyCode::KeyJ => Some(Dir::Down),
                _ => None,
            };
            if let Some(dir) = dir {
                state.field = None;
                if k.modifiers.shift {
                    if let Some(f) = state.ws.focus {
                        let label = format!("move “{}”", state.title_of(f));
                        state.act_nav("move", label, Some(format!("panel:{f}")), move |ws| {
                            ws.move_panel(f, dir);
                        });
                    }
                } else {
                    // Focus walks are context, not actions — never undo nodes.
                    let vp = state.vp();
                    let opts = state.opts();
                    state.ws.focus_dir(dir, vp, opts);
                }
                self.sync(cx);
                return;
            }
            if k.key_code == KeyCode::KeyZ {
                if k.modifiers.shift {
                    self.do_redo(cx);
                } else {
                    self.do_undo(cx);
                }
                return;
            }
            if k.key_code == KeyCode::KeyU {
                state.overlay = if state.overlay == Overlay::History {
                    Overlay::None
                } else {
                    Overlay::History
                };
                self.kick(cx);
                return;
            }
            if k.key_code == KeyCode::KeyI {
                self.copy_panel_context(cx);
                return;
            }
            if k.key_code == KeyCode::KeyW {
                if let Some(f) = state.ws.focus {
                    state.field = None;
                    let label = format!("close “{}”", state.title_of(f));
                    state.act_nav("close", label, None, move |ws| {
                        ws.close(f);
                    });
                    self.sync(cx);
                }
                return;
            }
            // niri's column operations.
            if let Some(f) = state.ws.focus {
                let col = |s: &mut State, label: &str, m: Box<dyn FnOnce(&mut Wm)>| {
                    s.act_nav("column", label.to_string(), Some(format!("panel:{f}")), m);
                };
                match k.key_code {
                    KeyCode::LBracket => {
                        col(state, "consume left", Box::new(move |ws| ws.consume_or_expel(f, Dir::Left)));
                        self.sync(cx);
                        return;
                    }
                    KeyCode::RBracket => {
                        col(state, "expel right", Box::new(move |ws| ws.consume_or_expel(f, Dir::Right)));
                        self.sync(cx);
                        return;
                    }
                    KeyCode::Comma => {
                        col(state, "pull from the right", Box::new(move |ws| ws.consume_from_right(f)));
                        self.sync(cx);
                        return;
                    }
                    KeyCode::Period => {
                        col(state, "push the bottom out", Box::new(move |ws| ws.expel_bottom(f)));
                        self.sync(cx);
                        return;
                    }
                    KeyCode::KeyT => {
                        col(state, "toggle tabs", Box::new(move |ws| ws.toggle_tabbed(f)));
                        self.sync(cx);
                        return;
                    }
                    _ => {}
                }
            }
            if k.key_code != KeyCode::ReturnKey {
                return;
            }
            // cmd+enter falls through: fresh un-joined open in the inbox.
        }

        // A focused text field owns the keyboard (chars arrive via TextInput).
        // A retained panel owns plain keys (its widgets gate on key focus)
        // — cmd chords and overlays were already handled above.
        if hosted && state.field.is_none() {
            self.forward_to_hosted(cx, &Event::KeyDown(k.clone()));
            self.kick(cx);
            return;
        }
        if let Some((pid, fid)) = state.field {
            let kind = state.ws.panels.get(&pid).map(|p| p.kind.clone());
            // Tab walks the form (shift reverses).
            if k.key_code == KeyCode::Tab {
                if let Some(kind) = &kind {
                    let dir = if k.modifiers.shift { -1 } else { 1 };
                    if let Some(next) = ui::next_field(kind, fid, dir) {
                        state.field = Some((pid, next));
                    }
                }
                self.kick(cx);
                return;
            }
            let Some(ui) = state.ui.get_mut(&pid) else {
                state.field = None;
                return;
            };
            match fid {
                FieldId::Body => match k.key_code {
                    KeyCode::Escape => state.field = None,
                    KeyCode::ReturnKey => {
                        let (r, c) = ui.caret;
                        let byte = char_byte(&ui.body[r], c);
                        let rest = ui.body[r].split_off(byte);
                        ui.body.insert(r + 1, rest);
                        ui.caret = (r + 1, 0);
                    }
                    KeyCode::Backspace => {
                        let (r, c) = ui.caret;
                        if c > 0 {
                            let b0 = char_byte(&ui.body[r], c - 1);
                            let b1 = char_byte(&ui.body[r], c);
                            ui.body[r].replace_range(b0..b1, "");
                            ui.caret = (r, c - 1);
                        } else if r > 0 {
                            let line = ui.body.remove(r);
                            let plen = ui.body[r - 1].chars().count();
                            ui.body[r - 1].push_str(&line);
                            ui.caret = (r - 1, plen);
                        }
                    }
                    KeyCode::ArrowLeft => {
                        let (r, c) = ui.caret;
                        ui.caret = if c > 0 {
                            (r, c - 1)
                        } else if r > 0 {
                            (r - 1, ui.body[r - 1].chars().count())
                        } else {
                            (r, c)
                        };
                    }
                    KeyCode::ArrowRight => {
                        let (r, c) = ui.caret;
                        let len = ui.body[r].chars().count();
                        ui.caret = if c < len {
                            (r, c + 1)
                        } else if r + 1 < ui.body.len() {
                            (r + 1, 0)
                        } else {
                            (r, c)
                        };
                    }
                    KeyCode::ArrowUp => {
                        let (r, c) = ui.caret;
                        if r > 0 {
                            ui.caret = (r - 1, c.min(ui.body[r - 1].chars().count()));
                        }
                    }
                    KeyCode::ArrowDown => {
                        let (r, c) = ui.caret;
                        if r + 1 < ui.body.len() {
                            ui.caret = (r + 1, c.min(ui.body[r + 1].chars().count()));
                        }
                    }
                    _ => return,
                },
                _ => {
                    let f = ui.field_mut(fid).expect("single-line field");
                    match k.key_code {
                        KeyCode::Escape => state.field = None,
                        KeyCode::ReturnKey => {
                            if fid == FieldId::Filter {
                                // Select the first visible row and leave the field.
                                let first = state.inbox_rows(pid).first().map(|m| m.id);
                                if let Some(ui) = state.ui.get_mut(&pid) {
                                    ui.sel = first;
                                }
                            }
                            state.field = None;
                        }
                        KeyCode::Backspace => f.backspace(),
                        KeyCode::Delete => f.delete(),
                        KeyCode::ArrowLeft => f.left(),
                        KeyCode::ArrowRight => f.right(),
                        _ => return,
                    }
                }
            }
            self.kick(cx);
            return;
        }

        // Plain keys to the focused panel: the non-character ones.
        let Some(f) = state.ws.focus else {
            return;
        };
        let kind = state.ws.panels.get(&f).map(|p| p.kind.clone());
        match kind {
            Some(Kind::Inbox { .. }) => match k.key_code {
                KeyCode::ArrowDown => self.inbox_move_sel(cx, f, 1),
                KeyCode::ArrowUp => self.inbox_move_sel(cx, f, -1),
                KeyCode::ReturnKey => {
                    let rows = state.inbox_rows(f);
                    let sel = state.ui.get(&f).and_then(|u| u.sel);
                    let target = sel
                        .filter(|s| rows.iter().any(|m| m.id == *s))
                        .or_else(|| rows.first().map(|m| m.id));
                    if let Some(id) = target {
                        let fresh = k.modifiers.logo || k.modifiers.alt;
                        let kind = Kind::Message { id };
                        let label = format!("open “{}”", state.panel_title(&kind));
                        state.act(
                            "open",
                            label,
                            None,
                            move |ws| {
                                ws.follow_open(f, kind, fresh);
                            },
                            move |tx| mail::mark_read_tx(tx, id),
                        );
                        self.sync(cx);
                    }
                }
                _ => {}
            },
            _ => match k.key_code {
                KeyCode::ArrowDown | KeyCode::ArrowUp => {
                    let d = if k.key_code == KeyCode::ArrowDown {
                        1.0
                    } else {
                        -1.0
                    };
                    if let Some(ui) = state.ui.get_mut(&f) {
                        ui.scroll = (ui.scroll + d * self.cell.line_h * 3.0)
                            .clamp(0.0, ui.max_scroll);
                    }
                    self.kick(cx);
                }
                _ => {}
            },
        }
    }

    fn inbox_move_sel(&mut self, cx: &mut Cx, pid: PanelId, d: isize) {
        let Some(state) = self.state.as_deref_mut() else {
            return;
        };
        let rows = state.inbox_rows(pid);
        if rows.is_empty() {
            return;
        }
        let Some(ui) = state.ui.get_mut(&pid) else {
            return;
        };
        let cur = ui.sel.and_then(|s| rows.iter().position(|m| m.id == s));
        let next = match cur {
            None => {
                if d > 0 {
                    0
                } else {
                    rows.len() - 1
                }
            }
            Some(i) => (i as isize + d).clamp(0, rows.len() as isize - 1) as usize,
        };
        ui.sel = Some(rows[next].id);
        // Keep the selection inside the scrolling region.
        let line_h = self.cell.line_h;
        let row_top = next as f64 * line_h;
        let row_bot = row_top + line_h;
        let view = ui.view_h.max(line_h);
        if row_top < ui.scroll {
            ui.scroll = row_top;
        } else if row_bot > ui.scroll + view {
            ui.scroll = row_bot - view;
        }
        self.kick(cx);
    }

    /// The android IME's authoritative field state (`full_state_sync`):
    /// replace the focused field's text and caret wholesale. Composition is
    /// tracked so app→IME syncs pause while the keyboard composes.
    /// Only the launcher trigger cares about key releases: a clean second
    /// cmd tap fires here.
    fn handle_key_up(&mut self, cx: &mut Cx, k: &KeyEvent) {
        if k.key_code == KeyCode::Logo && self.cmd_tap.release(k.time) {
            self.toggle_launcher(cx);
        }
        if self.hosted_focus() {
            self.forward_to_hosted(cx, &Event::KeyUp(k.clone()));
        }
    }

    /// Double-cmd: raise the launcher, or put it away if it is already up.
    fn toggle_launcher(&mut self, cx: &mut Cx) {
        let Some(state) = self.state.as_deref_mut() else {
            return;
        };
        if state.overlay == Overlay::Launcher {
            state.overlay = Overlay::None;
        } else {
            state.launcher = LauncherUi::default();
            state.overlay = Overlay::Launcher;
        }
        self.kick(cx);
    }

    /// Raise the launcher idempotently — tapping its own field (or the menu
    /// item twice) must not reset a typed query.
    fn open_launcher(&mut self, cx: &mut Cx) {
        let Some(state) = self.state.as_deref_mut() else {
            return;
        };
        if state.overlay != Overlay::Launcher {
            state.launcher = LauncherUi::default();
            state.overlay = Overlay::Launcher;
        }
        self.kick(cx);
    }

    /// Activate a hit: go to the panel wherever it lives, or open a fresh
    /// un-joined trailing column on the active workspace.
    /// Undoes the head action: the store applies the inverted changeset,
    /// then the in-memory world reloads from the rewritten tables. The
    /// action's own workspace/focus rows revert with it, so undo also puts
    /// you back where the action happened.
    fn do_undo(&mut self, cx: &mut Cx) {
        let Some(state) = self.state.as_deref_mut() else {
            return;
        };
        let was = state.ws.active;
        match state.store.undo() {
            Ok(Some(label)) => {
                state.reload_wm();
                state.sync();
                if state.ws.active != was {
                    let cam = state.ws.camera_x;
                    state.anim.camera().jump_to(cam);
                }
                for w in &state.workers {
                    w.kick(); // reverted intent pushes to the server too
                }
                state.toast(format!("undid — {label}"), false);
            }
            Ok(None) => state.toast("nothing to undo", false),
            Err(e) => state.toast(format!("undo failed: {e}"), true),
        }
        self.update_menu(cx);
        self.kick(cx);
    }

    /// Redoes the newest undone child of HEAD (the default branch).
    fn do_redo(&mut self, cx: &mut Cx) {
        let Some(state) = self.state.as_deref_mut() else {
            return;
        };
        let was = state.ws.active;
        match state.store.redo() {
            Ok(Some(label)) => {
                state.reload_wm();
                state.sync();
                if state.ws.active != was {
                    let cam = state.ws.camera_x;
                    state.anim.camera().jump_to(cam);
                }
                for w in &state.workers {
                    w.kick();
                }
                state.toast(format!("redid — {label}"), false);
            }
            Ok(None) => state.toast("nothing to redo", false),
            Err(e) => state.toast(format!("redo failed: {e}"), true),
        }
        self.update_menu(cx);
        self.kick(cx);
    }



    /// Notices foreign commits (sync workers, the sender): re-runs stale
    /// queries, surfaces fresh send failures, redraws. Ridden by the
    /// worker signal and by a coarse fallback timer — a lost wake must
    /// never strand the UI on cached rows.
    fn poll_store(&mut self, cx: &mut Cx) {
        let Some(state) = self.state.as_deref_mut() else {
            return;
        };
        if state.store.poll_external() {
            let failures = mail::outbox_failures(&state.store);
            if failures.len() > state.failed_seen {
                if let Some((_, err)) = failures.last() {
                    state.toast(format!("send failed: {err} — ⌘z reopens"), true);
                }
            }
            state.failed_seen = failures.len();
            cx.redraw_all();
        }
    }

    /// Serializes the focused panel's context — identity, params, and the
    /// query trace from its last draw (provenance by construction) — to
    /// the clipboard and a file beside the store. The agent handoff this
    /// feeds is future work; the surface is ready.
    fn copy_panel_context(&mut self, cx: &mut Cx) {
        let Some(state) = self.state.as_deref_mut() else {
            return;
        };
        let Some(pid) = state.ws.focus else {
            state.toast("no focused panel", false);
            return;
        };
        let Some(panel) = state.ws.panel(pid) else {
            return;
        };
        let kind = panel.kind.clone();
        let title = state.panel_title(&kind);
        let ws = state.ws.ws_of(pid).map_or(0, |k| k + 1);
        let (kname, p_int, p_txt) = crate::store::kind_cols(&kind);
        let entries = state.store.trace_of(pid);
        let mut md = String::new();
        md.push_str("# superapp panel context\n\n");
        md.push_str(&format!("panel: “{title}” — workspace {ws}\n"));
        md.push_str(&format!("kind: {kname}\n"));
        match (p_int, &p_txt) {
            (Some(i), _) => md.push_str(&format!("params: {i}\n")),
            (_, Some(s)) => md.push_str(&format!("params: '{s}'\n")),
            _ => {}
        }
        md.push_str(&format!(
            "\n## queries (last draw — {} of them)\n",
            entries.len()
        ));
        for e in &entries {
            md.push_str(&format!("\n### {} — {}\n", e.id, e.describe));
            if !e.params.is_empty() {
                md.push_str(&format!("params: {}\n", e.params));
            }
            md.push_str(&format!("rows: {}\n", e.rows));
            let sql: String = e.sql.split_whitespace().collect::<Vec<_>>().join(" ");
            md.push_str(&format!("```sql\n{sql}\n```\n"));
        }
        // Deliver: a file beside the store, and the clipboard on macOS.
        let mut where_to = String::new();
        if let Some(dir) = state.db_path.as_ref().and_then(|p| p.parent()) {
            let path = dir.join("panel-context.md");
            if std::fs::write(&path, &md).is_ok() {
                where_to = path.to_string_lossy().into_owned();
            }
        }
        #[cfg(target_os = "macos")]
        {
            use std::io::Write;
            if let Ok(mut child) = std::process::Command::new("/usr/bin/pbcopy")
                .stdin(std::process::Stdio::piped())
                .spawn()
            {
                if let Some(stdin) = child.stdin.as_mut() {
                    let _ = stdin.write_all(md.as_bytes());
                }
                let _ = child.wait();
            }
        }
        state.toast(
            format!(
                "panel context copied — {} queries{}",
                entries.len(),
                if where_to.is_empty() { String::new() } else { format!(" · {where_to}") }
            ),
            false,
        );
        self.kick(cx);
    }

    fn launcher_go(&mut self, cx: &mut Cx, hit: launcher::Hit) {
        let Some(state) = self.state.as_deref_mut() else {
            return;
        };
        state.overlay = Overlay::None;
        match hit.go {
            launcher::Go::Focus(pid) => {
                let was = state.ws.active;
                if let Some(k) = state.ws.focus_panel(pid) {
                    state.field = None;
                    state.sync();
                    if k != was {
                        // The same jump-under-the-slide as a cmd+№ switch.
                        let cam = state.ws.camera_x;
                        state.anim.camera().jump_to(cam);
                    }
                }
            }
            launcher::Go::Open(kind) => {
                let label = format!("open “{}”", state.panel_title(&kind));
                let mid = if let Kind::Message { id } = kind { Some(id) } else { None };
                state.act(
                    "open",
                    label,
                    None,
                    move |ws| {
                        ws.open(kind, None, false);
                    },
                    move |tx| mid.map_or(Ok(()), |id| mail::mark_read_tx(tx, id)),
                );
                state.field = None;
                state.sync();
            }
        }
        self.update_menu(cx);
        self.kick(cx);
    }

    fn handle_ime_state(&mut self, cx: &mut Cx, fs: &FullTextState) {
        let caret = fs.selection.end.0;
        self.ime_composing = fs.composition.is_some();
        self.ime_sent = Some((fs.text.clone(), caret));
        let Some(state) = self.state.as_deref_mut() else {
            return;
        };
        // The launcher's query mirrors the IME editable wholesale, like any
        // field (this is the android typing path).
        if state.overlay == Overlay::Launcher {
            let text = fs.text.replace('\n', "");
            state.launcher.caret = caret.min(text.chars().count());
            if state.launcher.query != text {
                state.launcher.query = text;
                state.launcher.sel = 0;
            }
            self.kick(cx);
            return;
        }
        let Some((pid, fid)) = state.field else {
            return;
        };
        let Some(ui) = state.ui.get_mut(&pid) else {
            return;
        };
        match fid {
            FieldId::Body => {
                ui.body = fs.text.split('\n').map(str::to_string).collect();
                if ui.body.is_empty() {
                    ui.body.push(String::new());
                }
                let mut rem = caret;
                let mut rc = (
                    ui.body.len() - 1,
                    ui.body.last().map(|l| l.chars().count()).unwrap_or(0),
                );
                for (i, line) in ui.body.iter().enumerate() {
                    let n = line.chars().count();
                    if rem <= n {
                        rc = (i, rem);
                        break;
                    }
                    rem -= n + 1;
                }
                ui.caret = rc;
            }
            _ => {
                let f = ui.field_mut(fid).expect("single-line field");
                f.text = fs.text.clone();
                f.caret = caret.min(f.text.chars().count());
                if fid == FieldId::Filter {
                    ui.sel = None;
                }
            }
        }
        self.kick(cx);
    }

    /// Character input: field typing, or the focused panel's letter keys.
    fn handle_text(&mut self, cx: &mut Cx, input: &str) {
        let hosted = self.hosted_focus();
        let Some(state) = self.state.as_deref_mut() else {
            return;
        };
        if input.is_empty() || input.chars().any(|c| c.is_control()) {
            return;
        }
        // A retained panel owns typing (its TextInputs gate on key focus);
        // overlays still win above it.
        if hosted && state.overlay == Overlay::None && state.field.is_none() {
            let ev = Event::TextInput(TextInputEvent {
                input: input.to_string(),
                ..Default::default()
            });
            self.forward_to_hosted(cx, &ev);
            self.kick(cx);
            return;
        }
        if state.overlay == Overlay::Launcher {
            let byte = char_byte(&state.launcher.query, state.launcher.caret);
            state.launcher.query.insert_str(byte, input);
            state.launcher.caret += input.chars().count();
            state.launcher.sel = 0;
            self.kick(cx);
            return;
        }
        if let Some((pid, fid)) = state.field {
            if let Some(ui) = state.ui.get_mut(&pid) {
                match fid {
                    FieldId::Filter => {
                        ui.filter.insert(input);
                        ui.sel = None;
                    }
                    FieldId::Body => {
                        let (r, c) = ui.caret;
                        let byte = char_byte(&ui.body[r], c);
                        ui.body[r].insert_str(byte, input);
                        ui.caret = (r, c + input.chars().count());
                    }
                    _ => {
                        if let Some(f) = ui.field_mut(fid) {
                            f.insert(input);
                        }
                    }
                }
                self.kick(cx);
            }
            return;
        }
        let Some(f) = state.ws.focus else {
            return;
        };
        let kind = state.ws.panels.get(&f).map(|p| p.kind.clone());
        match (kind, input) {
            (Some(Kind::Inbox { .. }), "j") => self.inbox_move_sel(cx, f, 1),
            (Some(Kind::Inbox { .. }), "k") => self.inbox_move_sel(cx, f, -1),
            (Some(Kind::Inbox { .. }), "/") => {
                state.field = Some((f, FieldId::Filter));
                self.kick(cx);
            }
            (Some(Kind::Message { id }), "j") | (Some(Kind::Message { id }), "k") => {
                let (newer, older) = mail::neighbours(&state.store, id);
                let t = if input == "j" { older } else { newer };
                if let Some(t) = t {
                    let kind = Kind::Message { id: t };
                    let label = format!("read “{}”", state.panel_title(&kind));
                    state.act(
                        "read",
                        label,
                        Some(format!("panel:{f}")),
                        move |ws| {
                            ws.follow_replace(f, kind, false);
                        },
                        move |tx| mail::mark_read_tx(tx, t),
                    );
                    self.sync(cx);
                }
            }
            (Some(Kind::Message { id }), "r") => {
                let kind = Kind::Compose { re: id };
                let label = format!("open “{}”", state.panel_title(&kind));
                state.act_nav("open", label, None, move |ws| {
                    ws.follow_open(f, kind, false);
                });
                self.sync(cx);
            }
            _ => {}
        }
    }

    /// Switches to workspace `k`: the slide spring carries the view there;
    /// the horizontal camera lands instantly on the target space's own
    /// position (a glide under the slide would read as drift).
    fn switch_ws(&mut self, cx: &mut Cx, k: usize) {
        let Some(state) = self.state.as_deref_mut() else {
            return;
        };
        state.overlay = Overlay::None;
        if state.ws.switch(k) {
            state.field = None;
            state.sync();
            let cam = state.ws.camera_x;
            state.anim.camera().jump_to(cam);
        }
        self.update_menu(cx);
        self.kick(cx);
    }

    /// Moves the focused panel to workspace `k` and follows it (niri's
    /// default): the whole viewport slides, the panel rides along.
    fn move_focused_to_ws(&mut self, cx: &mut Cx, k: usize) {
        let Some(state) = self.state.as_deref_mut() else {
            return;
        };
        state.overlay = Overlay::None;
        if let Some(f) = state.ws.focus {
            let label = format!("move “{}” to workspace {}", state.title_of(f), k + 1);
            let mut moved = false;
            state.act_nav("movews", label, None, |ws| {
                moved = ws.send_focused_to(k).is_some();
            });
            if moved {
                state.field = None;
                state.sync();
                let cam = state.ws.camera_x;
                state.anim.camera().jump_to(cam);
            }
        }
        self.update_menu(cx);
        self.kick(cx);
    }

    fn resolve_click(&mut self, cx: &mut Cx, act: Act, alt: bool) {
        let Some(state) = self.state.as_deref_mut() else {
            return;
        };
        match act {
            Act::WsRow(k) => {
                self.switch_ws(cx, k);
                return;
            }
            Act::LauncherOpen => {
                self.open_launcher(cx);
                return;
            }
            Act::LauncherRow(i) => {
                let hit = state.launcher.hits.get(i).cloned();
                if let Some(hit) = hit {
                    self.launcher_go(cx, hit);
                }
                return;
            }
            Act::HistoryRow(id) => {
                let was = state.ws.active;
                match state.store.travel(id) {
                    Ok(Some(label)) => {
                        state.reload_wm();
                        state.sync();
                        if state.ws.active != was {
                            let cam = state.ws.camera_x;
                            state.anim.camera().jump_to(cam);
                        }
                        for w in &state.workers {
                            w.kick();
                        }
                        if let Some(s) = &state.sender {
                            s.kick();
                        }
                        state.toast(format!("history — {label}"), false);
                    }
                    Ok(None) => {}
                    Err(e) => state.toast(format!("travel failed: {e}"), true),
                }
                // The overlay stays up: browsing history is the point.
                self.update_menu(cx);
                self.kick(cx);
                return;
            }
            Act::OverlayClose => {
                state.overlay = Overlay::None;
                self.kick(cx);
                return;
            }
            Act::Pointer(pid) => {
                // The widget under it got the real event via forwarding;
                // the shell's share is panel focus.
                state.ws.focus = Some(pid);
                state.field = None;
                self.sync(cx);
                return;
            }
            Act::WidgetOp(pid, op) => {
                match op {
                    WidgetOp::AddAccount => {
                        if let Some(w) = self.hosted.get(&pid) {
                            if let Some(mut sp) = w.as_settings_panel().borrow_mut() {
                                let (email, pass, imap, smtp) = sp.form_values(cx);
                                cx.action(crate::panels::PanelAction::AddAccount {
                                    pid, email, pass, imap, smtp,
                                });
                            }
                        }
                    }
                    WidgetOp::RemoveAccount(id) => {
                        cx.action(crate::panels::PanelAction::RemoveAccount(id));
                    }
                }
                return;
            }
            _ => {}
        }
        match act {
            Act::Focus(pid) => {
                state.ws.focus = Some(pid);
                state.field = None;
                self.sync(cx);
            }
            Act::Close(pid) => {
                let label = format!("close “{}”", state.title_of(pid));
                state.act_nav("close", label, None, move |ws| {
                    ws.close(pid);
                });
                self.sync(cx);
            }
            Act::Open(pid, kind) => {
                let label = format!("open “{}”", state.panel_title(&kind));
                let mid = if let Kind::Message { id } = kind { Some(id) } else { None };
                state.act(
                    "open",
                    label,
                    None,
                    move |ws| {
                        ws.follow_open(pid, kind, alt);
                    },
                    move |tx| mid.map_or(Ok(()), |id| mail::mark_read_tx(tx, id)),
                );
                self.sync(cx);
            }
            Act::Replace(pid, kind) => {
                // Replacing with another mail (the newer/older links) is the
                // same "read" walk as j/k — it coalesces per panel.
                let mid = if let Kind::Message { id } = kind { Some(id) } else { None };
                let (akind, entity, label) = match mid {
                    Some(_) => (
                        "read",
                        Some(format!("panel:{pid}")),
                        format!("read “{}”", state.panel_title(&kind)),
                    ),
                    None => ("open", None, format!("open “{}”", state.panel_title(&kind))),
                };
                state.act(
                    akind,
                    label,
                    entity,
                    move |ws| {
                        ws.follow_replace(pid, kind, alt);
                    },
                    move |tx| mid.map_or(Ok(()), |id| mail::mark_read_tx(tx, id)),
                );
                self.sync(cx);
            }
            Act::Row(pid, id) => {
                state.ws.focus = Some(pid);
                state.field = None;
                if let Some(ui) = state.ui.get_mut(&pid) {
                    ui.sel = Some(id);
                }
                self.sync(cx);
            }
            Act::Field(pid, fid) => {
                state.ws.focus = Some(pid);
                state.field = Some((pid, fid));
                self.sync(cx);
            }
            Act::Tab(pid) => {
                state.ws.focus = Some(pid);
                state.field = None;
                self.sync(cx);
            }
            Act::Btn(pid, b) => {
                match b {
                    BtnAct::TryIt => {
                        state.toast("side effect: nothing was opened or replaced", false);
                    }
                    BtnAct::Refresh => {
                        if state.workers.is_empty() {
                            state.toast("no accounts to sync — add one in settings", false);
                        } else {
                            for w in &state.workers {
                                w.kick();
                            }
                            state.toast("syncing…", false);
                        }
                    }
                    BtnAct::Archive => {
                        if let Some(Kind::Message { id }) =
                            state.ws.panels.get(&pid).map(|p| p.kind.clone())
                        {
                            let subject = mail::mail(&state.store, id)
                                .map(|m| m.head.subject)
                                .unwrap_or_default();
                            state.act(
                                "archive",
                                format!("archive “{subject}”"),
                                None,
                                move |ws| {
                                    ws.close(pid);
                                },
                                move |tx| mail::archive_tx(tx, id),
                            );
                            state.toast(format!("archived “{subject}” — ⌘z undoes"), false);
                        }
                    }
                    BtnAct::Send => {
                        let re = match state.ws.panels.get(&pid).map(|p| p.kind.clone()) {
                            Some(Kind::Compose { re }) => (re != 0).then_some(re),
                            _ => None,
                        };
                        let d = state
                            .ui
                            .get(&pid)
                            .map(|u| mail::Draft {
                                to: u.to.text.trim().to_string(),
                                subject: u.subject.text.clone(),
                                body: u.body.join("\n"),
                            })
                            .unwrap_or_default();
                        if d.to.is_empty() {
                            state.toast("no recipient", true);
                        } else {
                            let delay = config().send_delay;
                            let subject = if d.subject.is_empty() {
                                "(no subject)".into()
                            } else {
                                d.subject.clone()
                            };
                            state.act(
                                "send",
                                format!("send “{subject}”"),
                                Some(format!("outbox:{pid}")),
                                move |ws| {
                                    ws.close(pid);
                                },
                                move |tx| {
                                    mail::upsert_draft_tx(tx, pid as i64, re, &d)?;
                                    mail::file_send_tx(tx, pid as i64, delay)
                                },
                            );
                            state.toast(
                                format!("sending in {}s — ⌘z undoes", delay as u32),
                                false,
                            );
                        }
                    }
                    BtnAct::Discard => {
                        let label = format!("discard “{}”", state.title_of(pid));
                        state.act(
                            "close",
                            label,
                            None,
                            move |ws| {
                                ws.close(pid);
                            },
                            move |tx| mail::discard_draft_tx(tx, pid as i64),
                        );
                    }
                }
                self.sync(cx);
            }
            // Handled above — they return before reaching this match.
            Act::WsRow(_) | Act::LauncherOpen | Act::LauncherRow(_) | Act::HistoryRow(_) | Act::OverlayClose | Act::Pointer(_) | Act::WidgetOp(..) => {}
        }
    }

    // -- touch ---------------------------------------------------------------

    fn touch_update(&mut self, cx: &mut Cx, e: &TouchUpdateEvent) {
        for t in &e.touches {
            match t.state {
                TouchState::Start => self.touch_start(t.uid, t.abs),
                TouchState::Move => self.touch_move(cx, t.uid, t.abs),
                TouchState::Stop => self.touch_stop(cx, t.uid, t.abs),
                TouchState::Stable => {}
            }
        }
    }

    fn touch_start(&mut self, uid: u64, p: DVec2) {
        self.touch.pts.insert(uid, (p, p));
        match self.touch.mode {
            // A drag keeps the panel no matter what other fingers do.
            TouchMode::Drag { .. } => {}
            _ if self.touch.pts.len() >= 2 => {
                self.touch.mode = TouchMode::Pan { horizontal: None }
            }
            _ => {
                let act = self.hit_at(p).map(|h| h.act.clone());
                // logcat is the only window into a device run.
                if cfg!(target_os = "android") {
                    log!("touch start uid={uid} p=({:.0},{:.0}) act={act:?}", p.x, p.y);
                }
                self.touch.mode = TouchMode::Tap { uid, act };
            }
        }
    }

    fn touch_move(&mut self, cx: &mut Cx, uid: u64, p: DVec2) {
        let Some(&(start, last)) = self.touch.pts.get(&uid) else {
            return;
        };
        let d = p - last;
        self.touch.pts.insert(uid, (start, p));
        match &self.touch.mode {
            TouchMode::Tap { uid: u, act } if *u == uid => {
                let t = p - start;
                if t.x.abs() < TOUCH_SLOP && t.y.abs() < TOUCH_SLOP {
                    return;
                }
                // Vertical wins the panel's scroll; sideways one-finger
                // movement means nothing (the workspace pans on two).
                self.touch.mode = match act.as_ref().and_then(act_pid) {
                    Some(pid) if t.y.abs() >= t.x.abs() => TouchMode::Scroll { uid, pid },
                    _ => TouchMode::Dead,
                };
            }
            TouchMode::Scroll { uid: u, pid } if *u == uid => {
                let pid = *pid;
                if let Some(state) = self.state.as_deref_mut() {
                    if let Some(ui) = state.ui.get_mut(&pid) {
                        ui.scroll = (ui.scroll - d.y).clamp(0.0, ui.max_scroll);
                    }
                }
                self.kick(cx);
            }
            TouchMode::Pan { horizontal } => {
                // The first move past the slop locks the axis for the whole
                // gesture: no mode flips mid-pan.
                if horizontal.is_none() {
                    let t = p - start;
                    if t.x.abs() < TOUCH_SLOP && t.y.abs() < TOUCH_SLOP {
                        return;
                    }
                    if t.x.abs() >= t.y.abs() {
                        self.touch.mode = TouchMode::Pan { horizontal: Some(true) };
                    } else {
                        // A vertical two-finger swipe: down summons the
                        // workspaces overlay, up dismisses whichever overlay
                        // is up. One shot — the rest of the gesture is inert.
                        if let Some(state) = self.state.as_deref_mut() {
                            state.overlay = if t.y > 0.0 {
                                Overlay::Ws
                            } else {
                                Overlay::None
                            };
                        }
                        self.touch.mode = TouchMode::Dead;
                        self.kick(cx);
                        return;
                    }
                }
                // Each finger reports its own move; splitting by the finger
                // count makes the strip track the gesture 1:1.
                let n = self.touch.pts.len().max(1) as f64;
                if let Some(state) = self.state.as_deref_mut() {
                    state.pan(-d.x / n);
                }
                self.kick(cx);
            }
            TouchMode::Drag { uid: u, pid, offset } if *u == uid => {
                let (pid, off) = (*pid, *offset);
                let local = p - self.origin;
                let mut hint = None;
                if let Some(state) = self.state.as_deref_mut() {
                    let cam = state.anim.camera().value();
                    if let Some(pa) = state.anim.panels.get_mut(&pid) {
                        pa.x.retarget(local.x + off.x + cam);
                        pa.y.retarget(local.y + off.y);
                    }
                    // The preview is judged by the finger, not the panel.
                    let vp = state.vp();
                    let opts = state.opts();
                    hint = state
                        .ws
                        .drop_target(pid, local.x + cam, local.y, vp, opts)
                        .map(|(_, bar)| bar);
                }
                self.drag_hint = hint;
                self.kick(cx);
            }
            _ => {}
        }
    }

    fn touch_stop(&mut self, cx: &mut Cx, uid: u64, p: DVec2) {
        let start = self.touch.pts.remove(&uid).map(|(s, _)| s);
        match self.touch.mode.clone() {
            TouchMode::Tap { uid: u, act } if u == uid => {
                self.touch.mode = TouchMode::Idle;
                let within = start.is_some_and(|s| {
                    (p.x - s.x).abs() < TOUCH_SLOP && (p.y - s.y).abs() < TOUCH_SLOP
                });
                if cfg!(target_os = "android") {
                    log!("touch tap uid={uid} within={within} act={act:?}");
                }
                if let (true, Some(act)) = (within, act) {
                    // No modifiers on glass: never the fresh-panel variant.
                    self.resolve_click(cx, act, false);
                }
            }
            TouchMode::Scroll { uid: u, .. } if u == uid => {
                self.touch.mode = TouchMode::Idle;
            }
            TouchMode::Drag { uid: u, pid, .. } if u == uid => {
                self.touch.mode = TouchMode::Idle;
                self.drag_hint = None;
                let local = p - self.origin;
                if let Some(state) = self.state.as_deref_mut() {
                    let cam = state.anim.camera().value();
                    let vp = state.vp();
                    let opts = state.opts();
                    // The drop lands where the finger points — same judgement
                    // as the preview bar.
                    let label = format!("move “{}”", state.title_of(pid));
                    state.act_nav("move", label, Some(format!("panel:{pid}")), move |ws| {
                        ws.place_at(pid, local.x + cam, local.y, vp, opts);
                    });
                }
                self.sync(cx);
            }
            TouchMode::Drag { .. } => {} // a bystander finger lifted mid-drag
            TouchMode::Pan { horizontal } => {
                // The pan ends with the first lifted finger; the camera
                // magnetises to the nearest column alignment — a spring, so
                // it settles rather than jumps. A leftover finger is inert.
                if !self.touch.pts.is_empty() {
                    self.touch.mode = TouchMode::Dead;
                }
                if horizontal == Some(true) {
                    if let Some(state) = self.state.as_deref_mut() {
                        let vp = state.vp();
                        let opts = state.opts();
                        state.ws.snap_camera(vp, opts);
                        let cam = state.ws.camera_x;
                        state.anim.camera().retarget(cam);
                    }
                }
                self.kick(cx);
            }
            _ => {
                if !self.touch.pts.is_empty() {
                    self.touch.mode = TouchMode::Dead;
                }
            }
        }
        if self.touch.pts.is_empty() {
            self.touch.mode = TouchMode::Idle;
        }
    }

    /// The platform's long-press (android's GestureDetector; e2e on desktop):
    /// on a panel header it picks the panel up.
    fn long_press(&mut self, cx: &mut Cx, uid: u64, p: DVec2) {
        match self.touch.mode {
            TouchMode::Tap { uid: u, .. } if u == uid => {}
            TouchMode::Idle => {}
            _ => return,
        }
        let Some(pid) = self.hit_at(p).and_then(|h| act_pid(&h.act)) else {
            return;
        };
        // Only the header (or a tab riding above it) grabs.
        let Some(head) = self
            .hits
            .iter()
            .find(|h| matches!(h.act, Act::Focus(q) if q == pid))
            .map(|h| h.rect)
        else {
            return;
        };
        if p.y > head.pos.y + theme::HEAD_H {
            return;
        }
        let grab = p - self.origin;
        if let Some(state) = self.state.as_deref_mut() {
            let cam = state.anim.camera().value();
            let pa_pos = state
                .anim
                .panels
                .get(&pid)
                .map(|pa| dvec2(pa.x.value() - cam, pa.y.value()))
                .unwrap_or(grab);
            state.ws.focus = Some(pid);
            if cfg!(target_os = "android") {
                log!("touch drag grab uid={uid} pid={pid}");
            }
            self.touch.mode = TouchMode::Drag {
                uid,
                pid,
                offset: pa_pos - grab,
            };
        }
        self.kick(cx);
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
        // Named children of this custom-drawn widget are content templates
        // (never auto-drawn) — collect them rooted, PortalList-style.
        if !apply.is_eval() {
            if let Some(obj) = value.as_object() {
                vm.vec_with(obj, |vm, vec| {
                    for kv in vec {
                        if let Some(id) = kv.key.as_id() {
                            if let Some(t) = kv.value.as_object() {
                                self.tpl.insert(id, vm.bx.heap.new_object_ref(t));
                            }
                        }
                    }
                });
            }
        }
    }
}

/// Which kinds draw as retained widget trees (CR-002; grows per phase).
fn hosted_tpl(kind: &Kind) -> Option<LiveId> {
    match kind {
        Kind::Settings => Some(live_id!(settings_tpl)),
        _ => None,
    }
}

impl Stage {
    /// The live content widget for a panel, instantiated from its kind's
    /// template on first use (mirrors PortalList::item).
    fn hosted_widget(&mut self, cx: &mut Cx, pid: PanelId, tpl: LiveId) -> Option<WidgetRef> {
        if let Some(w) = self.hosted.get(&pid) {
            return Some(w.clone());
        }
        let template_ref = self.tpl.get(&tpl)?;
        let template_value: ScriptValue = template_ref.as_object().into();
        let vm_id = cx.script_ref_vm_id(template_ref)?;
        let widget =
            cx.with_script_vm_id(vm_id, |vm| WidgetRef::script_from_value(vm, template_value));
        self.hosted.insert(pid, widget.clone());
        Some(widget)
    }

    /// Turns bubbled widget intent ([`PanelAction`]) into store actions —
    /// the one place retained content meets the undo system.
    fn handle_panel_actions(&mut self, cx: &mut Cx, actions: &Actions) {
        let mut refresh = false;
        for a in actions {
            let Some(pa) = a.downcast_ref::<crate::panels::PanelAction>() else {
                continue;
            };
            match pa.clone() {
                crate::panels::PanelAction::AddAccount {
                    pid,
                    email,
                    pass,
                    imap,
                    smtp,
                } => {
                    let Some(state) = self.state.as_deref_mut() else {
                        continue;
                    };
                    if email.is_empty() || pass.is_empty() || imap.is_empty() {
                        state.toast("address, password and imap host are required", true);
                    } else if state.db_path.is_none() {
                        state.toast("no store file — accounts need one", true);
                    } else {
                        let dir = state
                            .db_path
                            .as_ref()
                            .and_then(|p| p.parent())
                            .map(std::path::Path::to_path_buf)
                            .unwrap_or_default();
                        if !crate::secret::set(&dir, &email, &pass) {
                            state.toast("storing the password failed", true);
                        } else {
                            state.act(
                                "account",
                                format!("add account {email}"),
                                None,
                                |_| {},
                                move |tx| {
                                    mail::add_account_tx(tx, &email, &imap, &smtp).map(|_| ())
                                },
                            );
                            state.spawn_workers();
                            state.toast("account added — syncing", false);
                            if let Some(w) = self.hosted.get(&pid) {
                                if let Some(mut sp) = w.as_settings_panel().borrow_mut() {
                                    sp.clear_form(cx);
                                }
                            }
                        }
                    }
                    refresh = true;
                }
                crate::panels::PanelAction::RemoveAccount(id) => {
                    let Some(state) = self.state.as_deref_mut() else {
                        continue;
                    };
                    let email = mail::accounts(&state.store)
                        .iter()
                        .find(|acc| acc.id == id)
                        .map(|acc| acc.email.clone())
                        .unwrap_or_default();
                    state.act(
                        "account",
                        format!("remove account {email}"),
                        None,
                        |_| {},
                        move |tx| mail::remove_account_tx(tx, id),
                    );
                    state.spawn_workers();
                    state.toast(format!("removed {email} — ⌘z undoes"), false);
                    refresh = true;
                }
            }
        }
        if refresh {
            self.sync(cx);
        }
    }

    /// Synthesizes a real pointer press+release at a point — the e2e
    /// bridge's way of clicking retained widgets through their own event
    /// system.
    fn synth_click(&mut self, cx: &mut Cx, p: DVec2) {
        // hits() converts mouse to finger hits geometrically, so plain
        // mouse events are the right synthesis level.
        let down = Event::MouseDown(MouseDownEvent {
            abs: p,
            button: MouseButton::PRIMARY,
            window_id: CxWindowPool::id_zero(),
            modifiers: KeyModifiers::default(),
            handled: std::cell::Cell::new(Area::Empty),
            time: 0.0,
        });
        let up = Event::MouseUp(MouseUpEvent {
            abs: p,
            button: MouseButton::PRIMARY,
            window_id: CxWindowPool::id_zero(),
            modifiers: KeyModifiers::default(),
            time: 0.1,
        });
        self.forward_to_hosted(cx, &down);
        self.forward_to_hosted(cx, &up);
    }

    /// Whether the focused panel's content is a retained widget tree —
    /// keys and text then belong to it, not to the char-grid machinery.
    fn hosted_focus(&self) -> bool {
        self.state
            .as_deref()
            .and_then(|s| s.ws.focus.and_then(|f| s.ws.panels.get(&f)))
            .is_some_and(|p| hosted_tpl(&p.kind).is_some())
    }

    /// Forwards an event to every live content widget with its panel's
    /// props on the scope. Widgets gate themselves (areas, key focus).
    fn forward_to_hosted(&mut self, cx: &mut Cx, event: &Event) {
        if self.hosted.is_empty() {
            return;
        }
        let Some(state) = self.state.as_deref() else {
            return;
        };
        for (pid, w) in &self.hosted {
            let props = crate::panels::PanelProps {
                store: state.store.clone(),
                pid: *pid,
            };
            let mut scope = Scope::with_props(&props);
            w.handle_event(cx, event, &mut scope);
        }
    }
}

impl Widget for Stage {
    fn handle_event(&mut self, cx: &mut Cx, event: &Event, _scope: &mut Scope) {
        // Retained content (CR-002): hosted widgets see every event through
        // their own system. Key/text events are forwarded by the inner
        // handlers instead (so the e2e paths share the exact route);
        // everything else — pointers, actions, frames — passes through here.
        if !matches!(
            event,
            Event::KeyDown(_) | Event::KeyUp(_) | Event::TextInput(_)
        ) {
            self.forward_to_hosted(cx, event);
        }
        if let Event::Actions(actions) = event {
            self.handle_panel_actions(cx, actions);
        }
        if matches!(event, Event::Startup) {
            if self.state.is_none() {
                let path = db_path(cx);
                let store = Store::open(path.as_deref()).unwrap_or_else(|e| {
                    panic!("store: opening {path:?} failed: {e}")
                });
                if let Err(e) = mail::seed_if_empty(&store) {
                    eprintln!("store: seeding demo mail failed: {e}");
                }
                // A delivered send can no longer be undone — the walk
                // marks it expired and steps past.
                store.set_undo_guard(mail::send_locked);
                let mut s = State::new(store, path);
                s.failed_seen = mail::outbox_failures(&s.store).len();
                s.spawn_workers();
                s.sync();
                self.state = Some(Box::new(s));
                // Belt and braces under the worker signal: a coarse poll so
                // a lost wake can never strand the UI on cached rows.
                self.poll_timer = cx.start_interval(2.0);
            }
            if let Some(path) = &config().e2e {
                match std::fs::read_to_string(path)
                    .map_err(|e| e.to_string())
                    .and_then(|s| e2e::parse(&s))
                {
                    Ok(steps) => {
                        let out = std::path::PathBuf::from(&config().out);
                        let _ = std::fs::create_dir_all(&out);
                        eprintln!("e2e: {} step(s) from {path}", steps.len());
                        self.e2e = Some(e2e::Runner::new(steps, out));
                        self.e2e_timer = cx.start_interval(0.03);
                    }
                    Err(e) => {
                        eprintln!("e2e: {path}: {e}");
                        std::process::exit(2);
                    }
                }
            }
            self.next_frame = cx.new_next_frame();
            cx.redraw_all();
        }
        if let Event::Timer(te) = event {
            if self.e2e_timer.0 != 0 && te.timer_id == self.e2e_timer.0 {
                self.e2e_tick(cx);
            }
            if self.poll_timer.0 != 0 && te.timer_id == self.poll_timer.0 {
                self.poll_store(cx);
            }
        }

        match event {
            Event::WindowGeomChange(e) => {
                // The viewport itself follows the drawn turtle (draw_walk);
                // here we only capture the safe-area insets a fold/notch
                // carves out. The next draw picks up both.
                let ins = e.new_geom.safe_area_insets;
                self.insets = (ins.top, ins.right, ins.bottom, ins.left);
                cx.redraw_all();
            }

            Event::TouchUpdate(e) => self.touch_update(cx, e),

            Event::LongPress(e) => self.long_press(cx, e.uid, e.abs),

            Event::KeyDown(k) => self.handle_key_down(cx, k),

            Event::KeyUp(k) => self.handle_key_up(cx, k),

            // A sync worker committed. The platform already consumed the
            // ui-signal flag before delivering this event (macos.rs checks
            // and clears it itself) — so never re-check it here, just poll.
            Event::Signal => self.poll_store(cx),

            // A menu item (macOS menu bar).
            Event::MacosMenuCommand(cmd) => {
                self.cmd_tap.other_input();
                let id = cmd.0;
                if (WS_MENU_SWITCH..WS_MENU_SWITCH + WS_N as u64).contains(&id) {
                    self.switch_ws(cx, (id - WS_MENU_SWITCH) as usize);
                } else if (WS_MENU_MOVE..WS_MENU_MOVE + WS_N as u64).contains(&id) {
                    self.move_focused_to_ws(cx, (id - WS_MENU_MOVE) as usize);
                } else if id == MENU_LAUNCHER {
                    self.open_launcher(cx);
                } else if id == MENU_UNDO {
                    self.do_undo(cx);
                } else if id == MENU_REDO {
                    self.do_redo(cx);
                } else if id == MENU_HISTORY {
                    if let Some(state) = self.state.as_deref_mut() {
                        state.overlay = Overlay::History;
                    }
                    self.kick(cx);
                }
            }

            Event::TextInput(e) => {
                // android's IME sends the authoritative full field state;
                // plain characters (macOS, hardware keys) come as `input`.
                if let Some(fs) = e.full_state_sync.clone() {
                    self.handle_ime_state(cx, &fs);
                } else {
                    let input = e.input.clone();
                    self.handle_text(cx, &input);
                }
            }

            Event::VirtualKeyboard(e) => {
                if cfg!(target_os = "android") {
                    log!("virtual keyboard: {e:?}");
                }
                // adjustNothing manifest: the app makes room itself. The
                // occlusion shrinks the viewport bottom; panels spring up.
                match e {
                    VirtualKeyboardEvent::WillShow { height, .. }
                    | VirtualKeyboardEvent::DidShow { height, .. } => self.kb_h = *height,
                    VirtualKeyboardEvent::WillHide { .. } => self.kb_h = 0.0,
                    VirtualKeyboardEvent::DidHide { .. } => {
                        self.kb_h = 0.0;
                        // The user dismissed the keyboard: that leaves the
                        // field, and kick()'s hide resets makepad's dismissed
                        // latch so the next field tap re-shows it.
                        if let Some(state) = self.state.as_deref_mut() {
                            state.field = None;
                        }
                    }
                }
                self.kick(cx);
            }

            Event::ImeAction(_) => {
                // The soft keyboard's action button ≈ Enter for single-line
                // fields (filter: select the first row and leave).
                let single_line = self
                    .state
                    .as_deref()
                    .is_some_and(|s| matches!(s.field, Some((_, f)) if f != FieldId::Body));
                if single_line {
                    let k = KeyEvent {
                        key_code: KeyCode::ReturnKey,
                        modifiers: KeyModifiers::default(),
                        is_repeat: false,
                        time: 0.0,
                    };
                    self.handle_key_down(cx, &k);
                }
            }

            Event::MouseMove(e) => {
                let p = e.abs;
                let act = self.hit_at(p).map(|h| (h.act.clone(), h.cursor));
                let Some(state) = self.state.as_deref_mut() else {
                    return;
                };
                let new_hover = act.as_ref().and_then(|(a, _)| match a {
                    Act::Focus(_) => None,
                    other => Some(other.clone()),
                });
                cx.set_cursor(act.map(|(_, c)| c).unwrap_or(MouseCursor::Default));
                if new_hover != state.hover {
                    state.hover = new_hover;
                    cx.redraw_all();
                }
            }

            Event::MouseDown(e) => {
                self.cmd_tap.other_input();
                cx.set_key_focus(self.area);
                let act = self.hit_at(e.abs).map(|h| h.act.clone());
                if let Some(act) = act {
                    // cmd+click (alt as a quiet alias): a fresh, un-joined panel.
                    let fresh = e.modifiers.logo || e.modifiers.alt;
                    self.resolve_click(cx, act, fresh);
                }
            }

            Event::Scroll(e) => {
                self.cmd_tap.other_input();
                let Some(state) = self.state.as_deref_mut() else {
                    return;
                };
                e.handled_x.set(true);
                e.handled_y.set(true);
                if e.scroll.x.abs() > e.scroll.y.abs() {
                    state.pan(e.scroll.x);
                } else if e.scroll.y != 0.0 {
                    // Vertical: scroll the panel body under the pointer.
                    let p = e.abs;
                    let pid = self
                        .hits
                        .iter()
                        .rev()
                        .find(|h| h.rect.contains(p))
                        .and_then(|h| act_pid(&h.act));
                    if let Some(pid) = pid {
                        if let Some(ui) = state.ui.get_mut(&pid) {
                            ui.scroll = (ui.scroll + e.scroll.y).clamp(0.0, ui.max_scroll);
                        }
                    }
                }
                self.kick(cx);
            }

            Event::NextFrame(ne) => {
                if !ne.set.contains(&self.next_frame) {
                    return;
                }
                let Some(state) = self.state.as_deref_mut() else {
                    return;
                };
                let now = Instant::now();
                let dt = state
                    .last_frame
                    .map(|t| (now - t).as_secs_f64())
                    .unwrap_or(1.0 / 60.0)
                    .clamp(0.0, 1.0 / 20.0);
                state.last_frame = Some(now);
                let springs_active = state.anim.advance(dt);
                // A held panel near a screen edge pans the camera — that is
                // how a drag reaches columns beyond the viewport. The grabbed
                // panel stays glued to the finger; the preview follows.
                let dragging = if let TouchMode::Drag { uid, pid, offset } = self.touch.mode {
                    if let Some(&(_, p)) = self.touch.pts.get(&uid) {
                        let local = p - self.origin;
                        let margin = 64.0;
                        let w = state.viewport.x;
                        let f = if local.x < margin {
                            -(margin - local.x) / margin
                        } else if local.x > w - margin {
                            (local.x - (w - margin)) / margin
                        } else {
                            0.0
                        };
                        if f != 0.0 {
                            state.pan(f.clamp(-1.0, 1.0) * 1000.0 * dt);
                            let cam = state.anim.camera().value();
                            if let Some(pa) = state.anim.panels.get_mut(&pid) {
                                pa.x.retarget(local.x + offset.x + cam);
                            }
                            let vp = state.vp();
                            let opts = state.opts();
                            self.drag_hint = state
                                .ws
                                .drop_target(pid, local.x + cam, local.y, vp, opts)
                                .map(|(_, bar)| bar);
                        }
                    }
                    true
                } else {
                    false
                };
                let toast_active = match state.toast {
                    Some((_, _, since)) => {
                        if since.elapsed().as_secs_f64() > 3.0 {
                            state.toast = None;
                            false
                        } else {
                            true
                        }
                    }
                    None => false,
                };
                state.animating = springs_active;
                if springs_active || toast_active || dragging {
                    self.next_frame = cx.new_next_frame();
                }
                cx.redraw_all();
            }

            _ => {}
        }
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
        // The workspace lives inside the safe area (zero on desktop). Android
        // additionally swallows touches in the notification-shade pull zone
        // at the very top of the window (~22 dp observed on gesture nav), so
        // panel headers — the drag grip, close, archive — stay below it.
        //
        // When the soft keyboard shows, makepad slides the whole pass up by
        // its height (the turtle's origin goes negative — android's
        // content-shift). Shifting back down by kb_h and shortening by it
        // makes the viewport exactly the visible region above the keyboard.
        let vp = {
            let r = cx.turtle().rect();
            let (t, rt, b, l) = self.insets;
            // 40 dp clears both the shade-pull zone and the punch-hole
            // camera (the Fold reports no cutout inset), with margin.
            let t = if cfg!(target_os = "android") {
                t.max(40.0)
            } else {
                t
            };
            rect(
                r.pos.x + l,
                r.pos.y + self.kb_h + t,
                (r.size.x - l - rt).max(40.0),
                (r.size.y - self.kb_h - t - b).max(40.0),
            )
        };
        self.origin = vp.pos;
        let dpi = cx.current_dpi_factor();

        if let Some(state) = self.state.as_deref_mut() {
            if (state.viewport - vp.size).length() > 1.0 {
                if cfg!(target_os = "android") {
                    log!(
                        "viewport: turtle {:?} insets {:?} kb {} -> vp {:?}",
                        cx.turtle().rect(),
                        self.insets,
                        self.kb_h,
                        vp
                    );
                }
                state.viewport = vp.size;
                state.sync();
            }
        }

        // Measure the mono face once per display scale.
        if (self.cell.dpi - dpi).abs() > 1e-9 {
            self.draw_mono.text_style.font_size = theme::FONT_SIZE as f32;
            if let Some(run) = self.draw_mono.prepare_single_line_run(cx, "MMMMMMMMMMMMMMMM") {
                let width = f64::from(run.width_in_lpxs) / 16.0;
                let asc = f64::from(run.ascender_in_lpxs);
                let line = asc - f64::from(run.descender_in_lpxs);
                if width > 0.0 && line > 0.0 {
                    self.cell = CellFont {
                        adv: width,
                        // The web prototype's 1.5 line-height, on this grid.
                        line_h: (line * 1.28).ceil(),
                        asc,
                        natural: line,
                        dpi,
                    };
                }
            }
        }

        self.hits.clear();
        let mut state = self.state.take();
        if let Some(state) = state.as_deref_mut() {
            self.draw_scene(cx, state, vp);
            if !self.reported {
                self.reported = true;
                eprintln!(
                    "superapp: first frame — {} panels, viewport {:.0}×{:.0}, cell {:.2}×{:.2}",
                    state.ws.panels.len(),
                    vp.size.x,
                    vp.size.y,
                    self.cell.adv,
                    self.cell.line_h,
                );
            }
        }
        self.state = state;

        cx.end_turtle_with_area(&mut self.area);
        DrawStep::done()
    }
}

// ---------------------------------------------------------------------------
// Drawing
// ---------------------------------------------------------------------------

impl Stage {
    fn draw_scene(&mut self, cx: &mut Cx2d, state: &mut State, vp: Rect) {
        // Workspaces stack vertically, one viewport (and a gap) apart; the
        // slide spring carries the view between rows on a switch. Each
        // workspace pans on its own x-camera — the active one live, the
        // rest parked at their stored targets. Anything a full row away
        // culls out, so a settled view draws exactly one workspace.
        let slide = state.anim.slide().value();
        let cam_a = state.anim.camera().value();
        let active = state.ws.active;
        let step = vp.size.y + theme::GAP;
        let cams: Vec<f64> = (0..WS_N)
            .map(|k| {
                if k == active {
                    cam_a
                } else {
                    state.ws.wss[k].camera_x
                }
            })
            .collect();
        let to_screen = move |r: core::Rect, k: usize| -> Rect {
            rect(
                r.x - cams[k] + vp.pos.x,
                r.y + (k as f64 - slide) * step + vp.pos.y,
                r.w,
                r.h,
            )
        };
        let off_screen = |r: &Rect| -> bool {
            r.pos.x > vp.pos.x + vp.size.x
                || r.pos.x + r.size.x < vp.pos.x
                || r.pos.y > vp.pos.y + vp.size.y
                || r.pos.y + r.size.y < vp.pos.y
        };

        // Ghosts first: chrome-only, fading out on their workspace row.
        let ghosts = state.anim.ghosts.clone();
        for g in &ghosts {
            let r = to_screen(g.rect, g.ws);
            if off_screen(&r) {
                continue;
            }
            let a = g.alpha.value();
            self.draw_chrome(cx, r, &g.title, false, a, None, None);
        }

        // Panels in column order per workspace; the active workspace's
        // focused panel last so it draws on top while overlapping
        // mid-animation.
        let mut order: Vec<PanelId> = state
            .ws
            .wss
            .iter()
            .flat_map(|w| w.columns.iter().flat_map(|c| c.panels.iter().copied()))
            .collect();
        if let Some(f) = state.ws.focus {
            if let Some(i) = order.iter().position(|&p| p == f) {
                let f = order.remove(i);
                order.push(f);
            }
        }
        // A panel is interactive only if it is on the active workspace and
        // its column actually shows it (the active tab, or any panel of a
        // normal column).
        let shown = |ws: &Ws, pid: PanelId| -> bool {
            ws.locate(pid).is_none_or(|(c, r)| {
                let col = &ws.columns[c];
                !col.tabbed || col.active.min(col.panels.len() - 1) == r
            })
        };
        for pid in order {
            let Some(pa) = state.anim.panels.get(&pid) else {
                continue;
            };
            let k = pa.ws;
            let r = to_screen(pa.rect(), k);
            if off_screen(&r) {
                continue;
            }
            let alpha = pa.alpha.value();
            let interactive = k == active && shown(&state.ws.wss[k], pid);
            if !interactive && alpha < 0.02 {
                continue; // a fully faded hidden tab
            }
            let hits_before = self.hits.len();
            self.draw_panel_full(cx, state, pid, r, alpha);
            state.store.trace_end();
            if !interactive {
                // Mid-crossfade or another workspace: visible, not hittable.
                self.hits.truncate(hits_before);
            }
        }

        // Tab strips above tabbed columns: one title segment per panel, the
        // active one inverted. They ride the active panel's animated rect.
        let hover = state.hover.clone();
        for k in 0..WS_N {
            let columns = state.ws.wss[k].columns.clone();
            for col in &columns {
                if !col.tabbed || col.panels.is_empty() {
                    continue;
                }
                let active_idx = col.active.min(col.panels.len() - 1);
                let Some(pa) = state.anim.panels.get(&col.panels[active_idx]) else {
                    continue;
                };
                let r = to_screen(pa.rect(), k);
                // The strip belongs to the column, not to one tab: during a
                // crossfade the outgoing+incoming alphas sum to ~1, so the
                // strip holds steady; when a column first appears it still
                // fades in.
                let alpha = col
                    .panels
                    .iter()
                    .filter_map(|pid| state.anim.panels.get(pid))
                    .map(|pa| pa.alpha.value())
                    .sum::<f64>()
                    .min(1.0);
                let strip = rect(
                    r.pos.x,
                    r.pos.y - theme::TAB_GAP - theme::TAB_H,
                    r.size.x,
                    theme::TAB_H,
                );
                if off_screen(&strip) {
                    continue;
                }
                let n = col.panels.len() as f64;
                let seg_gap = 2.0;
                let seg_w = ((strip.size.x - (n - 1.0) * seg_gap) / n).max(24.0);
                for (i, pid) in col.panels.iter().enumerate() {
                    let sx = strip.pos.x + i as f64 * (seg_w + seg_gap);
                    let sr = rect(sx, strip.pos.y, seg_w, theme::TAB_H);
                    let act = Act::Tab(*pid);
                    let tab_active = i == active_idx;
                    let hovered = hover.as_ref() == Some(&act);
                    let (bg, fg) = match (tab_active, hovered) {
                        (true, _) => (theme::INK, theme::BG),
                        (false, true) => (theme::HOVER, theme::INK),
                        (false, false) => (theme::BG, theme::INK),
                    };
                    self.draw_panel.color = rgba_a(bg, alpha);
                    self.draw_panel.border_color = rgba_a(theme::INK, alpha);
                    self.draw_panel.border_size = 1.0;
                    self.draw_panel.alpha = alpha as f32;
                    self.draw_panel.draw_abs(cx, sr);
                    let title = state
                        .ws
                        .panel(*pid)
                        .map(|p| state.panel_title(&p.kind))
                        .unwrap_or_default();
                    let title_cols =
                        (((seg_w - 12.0) / self.cell.label_step()).max(2.0)) as usize;
                    let t = trunc(&title, title_cols);
                    let tw = self.cell.label_w(t.chars().count());
                    let ty = sr.pos.y + (theme::TAB_H - self.cell.label_line()) / 2.0;
                    self.draw_label(cx, sx + ((seg_w - tw) / 2.0).max(6.0), ty, &t, fg, alpha);
                    if k == active {
                        self.hits.push(HitR {
                            rect: sr,
                            act,
                            cursor: MouseCursor::Hand,
                            label: title,
                        });
                    }
                }
            }
        }

        // Bridges above panels: the join indicator.
        self.draw_flat.new_draw_call(cx);
        let joins: Vec<(usize, PanelId, PanelId)> = state
            .ws
            .wss
            .iter()
            .enumerate()
            .flat_map(|(k, w)| w.joins.iter().map(move |(&a, &b)| (k, a, b)))
            .collect();
        for (k, a, b) in joins {
            let (Some(pa), Some(pb)) = (state.anim.panels.get(&a), state.anim.panels.get(&b))
            else {
                continue;
            };
            let ra = to_screen(pa.rect(), k);
            let rb = to_screen(pb.rect(), k);
            if off_screen(&ra) && off_screen(&rb) {
                continue;
            }
            let mut y = rb.pos.y + theme::HEAD_H / 2.0;
            if y < ra.pos.y || y > ra.pos.y + ra.size.y {
                y = ra.pos.y + theme::HEAD_H / 2.0;
                if y < rb.pos.y || y > rb.pos.y + rb.size.y {
                    let top = ra.pos.y.max(rb.pos.y);
                    let bot = (ra.pos.y + ra.size.y).min(rb.pos.y + rb.size.y);
                    y = if top < bot {
                        (top + bot) / 2.0
                    } else {
                        rb.pos.y + theme::HEAD_H / 2.0
                    };
                }
            }
            let x0 = ra.pos.x + ra.size.x;
            let w = rb.pos.x - x0;
            if w <= 0.0 || w > 60.0 {
                continue;
            }
            let a_min = pa.alpha.value().min(pb.alpha.value());
            self.draw_flat.color = rgba_a(theme::INK, a_min);
            self.draw_flat.draw_abs(cx, rect(x0, y - 2.0, w, 1.0));
            self.draw_flat.draw_abs(cx, rect(x0, y + 1.0, w, 1.0));
        }

        // The drop preview: an ink insertion bar where a dragged panel would
        // land — vertical in a gap (fresh column), horizontal across a
        // column (stack at that row).
        if let Some(h) = self.drag_hint {
            let r = to_screen(h, active);
            self.draw_flat.new_draw_call(cx);
            self.draw_flat.color = rgba_a(theme::INK, 0.85);
            self.draw_flat.draw_abs(cx, r);
        }

        // An empty active workspace names itself, so a switch onto a blank
        // screen reads as a place, not a bug.
        if state.ws.is_empty() && state.anim.ghosts.is_empty() && state.overlay == Overlay::None {
            let msg = if cfg!(target_os = "android") {
                format!("workspace {} is empty", active + 1)
            } else {
                format!("workspace {} — cmd+shift+№ brings a panel here", active + 1)
            };
            let w = msg.chars().count() as f64 * self.cell.adv;
            self.draw_mono.new_draw_call(cx);
            self.set_text(Style::Muted, 1.0);
            self.draw_mono.draw_abs(
                cx,
                dvec2(
                    vp.pos.x + (vp.size.x - w) / 2.0,
                    vp.pos.y + (vp.size.y - self.cell.line_h) / 2.0,
                ),
                &msg,
            );
        }

        // The modal overlays share a chassis: an ink wash that owns every
        // hit, a tap outside the rows dismisses. On top of it, either the
        // workspaces list or the launcher.
        if state.overlay != Overlay::None {
            self.hits.clear();
            self.draw_flat.new_draw_call(cx);
            self.draw_flat.color = rgba_a(theme::INK, 0.30);
            self.draw_flat.draw_abs(cx, vp);
            self.hits.push(HitR {
                rect: vp,
                act: Act::OverlayClose,
                cursor: MouseCursor::Default,
                label: match state.overlay {
                    Overlay::Ws => "workspaces",
                    Overlay::History => "history",
                    _ => "launcher",
                }
                .into(),
            });
        }

        // The workspaces overlay: a column of tappable rows — the current
        // space inverted, panel titles as the summary, the first empty slot
        // offered as a fresh space — under a search row, the launcher's
        // touch entry.
        if state.overlay == Overlay::Ws {
            let roster = state.ws.roster();
            let row_h: f64 = 54.0;
            let row_gap: f64 = 10.0;
            let w = (vp.size.x - 4.0 * theme::GAP).min(430.0);
            let total = (roster.len() + 1) as f64 * (row_h + row_gap) - row_gap;
            let x = vp.pos.x + (vp.size.x - w) / 2.0;
            let mut y = vp.pos.y + ((vp.size.y - total) / 2.0).max(2.0 * theme::GAP);
            self.draw_panel.new_draw_call(cx);
            self.draw_mono.new_draw_call(cx);
            let r = rect(x, y, w, row_h);
            self.draw_panel.color = rgba_a(theme::BG, 1.0);
            self.draw_panel.border_color = rgba_a(theme::INK, 1.0);
            self.draw_panel.border_size = 1.0;
            self.draw_panel.alpha = 1.0;
            self.draw_panel.draw_abs(cx, r);
            self.set_text(Style::Muted, 1.0);
            self.draw_mono.draw_abs(
                cx,
                dvec2(x + 16.0, y + (row_h - self.cell.natural) / 2.0),
                "search",
            );
            self.hits.push(HitR {
                rect: r,
                act: Act::LauncherOpen,
                cursor: MouseCursor::Hand,
                label: "search".into(),
            });
            y += row_h + row_gap;
            for k in roster {
                let r = rect(x, y, w, row_h);
                let current = k == state.ws.active;
                let (bg, fg) = if current {
                    (theme::INK, theme::BG)
                } else {
                    (theme::BG, theme::INK)
                };
                self.draw_panel.color = rgba_a(bg, 1.0);
                self.draw_panel.border_color = rgba_a(theme::INK, 1.0);
                self.draw_panel.border_size = 1.0;
                self.draw_panel.alpha = 1.0;
                self.draw_panel.draw_abs(cx, r);
                self.set_text(Style::Big, 1.0);
                self.draw_mono.color = rgba_a(fg, 1.0);
                self.draw_mono.draw_abs(
                    cx,
                    dvec2(x + 16.0, y + (row_h - self.cell.natural * 1.25) / 2.0),
                    &format!("{}", k + 1),
                );
                let ws = &state.ws.wss[k];
                let summary = if ws.is_empty() {
                    "new".to_string()
                } else {
                    let names: Vec<String> = ws
                        .columns
                        .iter()
                        .flat_map(|c| c.panels.iter())
                        .filter_map(|pid| ws.panels.get(pid).map(|p| state.panel_title(&p.kind)))
                        .collect();
                    names.join(" · ")
                };
                let cols = (((w - 56.0) / self.cell.adv).max(4.0)) as usize;
                let summary = trunc(&summary, cols);
                self.set_text(Style::N, 1.0);
                self.draw_mono.color = rgba_a(fg, 1.0);
                self.draw_mono.draw_abs(
                    cx,
                    dvec2(x + 48.0, y + (row_h - self.cell.natural) / 2.0),
                    &summary,
                );
                self.hits.push(HitR {
                    rect: r,
                    act: Act::WsRow(k),
                    cursor: MouseCursor::Hand,
                    label: format!("workspace {}", k + 1),
                });
                y += row_h + row_gap;
            }
        }

        // The launcher: a query field over the result rows, windowed around
        // the selection. Hits are recomputed here, on what is actually
        // drawn, so clicks and enter resolve against the visible list.
        // The history overlay: the action DAG as rows, newest first, indented
        // by branch depth. The row under HEAD is inverted; undone branches
        // are muted but clickable — travel goes anywhere, including the
        // beginning. Expired sends are physics: marked, never re-walked.
        if state.overlay == Overlay::History {
            let (nodes, head) = state.store.history().unwrap_or((Vec::new(), 0));
            let mut depth: HashMap<i64, usize> = HashMap::new();
            for n in &nodes {
                let d = depth.get(&n.parent).map_or(0, |d| d + 1);
                depth.insert(n.id, d);
            }
            struct HRow {
                id: i64,
                text: String,
                right: String,
                state: &'static str,
            }
            let mut rows: Vec<HRow> = nodes
                .iter()
                .rev()
                .map(|n| {
                    let ind = "  ".repeat((*depth.get(&n.id).unwrap_or(&0)).min(6));
                    HRow {
                        id: n.id,
                        text: format!("{ind}{}", n.label),
                        right: match n.state.as_str() {
                            "expired" => format!("{} · sent", mail::fmt_date(n.ts)),
                            _ => mail::fmt_date(n.ts),
                        },
                        state: match n.state.as_str() {
                            "applied" => "applied",
                            "expired" => "expired",
                            _ => "undone",
                        },
                    }
                })
                .collect();
            rows.push(HRow {
                id: 0,
                text: "the beginning".into(),
                right: String::new(),
                state: "applied",
            });

            let row_h: f64 = 40.0;
            let row_gap: f64 = 8.0;
            let w = (vp.size.x - 4.0 * theme::GAP).min(560.0);
            let x = vp.pos.x + (vp.size.x - w) / 2.0;
            let top = vp.pos.y + 2.0 * theme::GAP;
            let avail = (vp.pos.y + vp.size.y - 2.0 * theme::GAP - top).max(0.0);
            let max_rows = (((avail + row_gap) / (row_h + row_gap)).floor() as usize).max(1);
            let head_idx = rows.iter().position(|r| r.id == head).unwrap_or(0);
            let start = (head_idx + 1).saturating_sub(max_rows.max(3) - 2).min(
                rows.len().saturating_sub(max_rows),
            );
            let end = (start + max_rows).min(rows.len());

            self.draw_panel.new_draw_call(cx);
            let mut texts: Vec<(DVec2, String, theme::Rgba, f64)> = Vec::new();
            let mut y = top;
            for r in &rows[start..end] {
                let rr = rect(x, y, w, row_h);
                let is_head = r.id == head;
                let (bg, fg) = if is_head {
                    (theme::INK, theme::BG)
                } else {
                    (theme::BG, theme::INK)
                };
                let alpha = if r.state == "applied" || is_head { 1.0 } else { 0.45 };
                self.draw_panel.color = rgba_a(bg, 1.0);
                self.draw_panel.border_color = rgba_a(theme::INK, if is_head { 1.0 } else { alpha });
                self.draw_panel.border_size = 1.0;
                self.draw_panel.alpha = 1.0;
                self.draw_panel.draw_abs(cx, rr);
                let ty = y + (row_h - self.cell.natural) / 2.0;
                let rx = x + w - 16.0 - r.right.chars().count() as f64 * self.cell.adv;
                if !r.right.is_empty() {
                    texts.push((dvec2(rx, ty), r.right.clone(), fg, 0.5 * alpha + 0.05));
                }
                let cols = (((rx - 12.0 - (x + 16.0)) / self.cell.adv).max(4.0)) as usize;
                texts.push((dvec2(x + 16.0, ty), trunc(&r.text, cols), fg, alpha));
                self.hits.push(HitR {
                    rect: rr,
                    act: Act::HistoryRow(r.id),
                    cursor: MouseCursor::Hand,
                    label: r.text.trim().to_string(),
                });
                y += row_h + row_gap;
            }
            if end < rows.len() {
                texts.push((
                    dvec2(x + 16.0, y + 6.0),
                    format!("… {} earlier", rows.len() - end),
                    theme::INK,
                    0.45,
                ));
            }
            self.draw_mono.new_draw_call(cx);
            for (pos, s, color, alpha) in texts {
                self.set_text(Style::N, 1.0);
                self.draw_mono.color = rgba_a(color, alpha);
                self.draw_mono.draw_abs(cx, pos, &s);
            }
        }

        if state.overlay == Overlay::Launcher {
            state.launcher.hits =
                launcher::search(&state.ws, &state.store, &state.launcher.query);
            let n_hits = state.launcher.hits.len();
            state.launcher.sel = state.launcher.sel.min(n_hits.saturating_sub(1));
            let field_h: f64 = 54.0;
            let row_h: f64 = 40.0;
            let row_gap: f64 = 8.0;
            let w = (vp.size.x - 4.0 * theme::GAP).min(520.0);
            let x = vp.pos.x + (vp.size.x - w) / 2.0;
            let top = vp.pos.y + 2.0 * theme::GAP;
            self.draw_panel.new_draw_call(cx);
            let fr = rect(x, top, w, field_h);
            self.draw_panel.color = rgba_a(theme::BG, 1.0);
            self.draw_panel.border_color = rgba_a(theme::INK, 1.0);
            self.draw_panel.border_size = 1.0;
            self.draw_panel.alpha = 1.0;
            self.draw_panel.draw_abs(cx, fr);
            self.hits.push(HitR {
                rect: fr,
                act: Act::LauncherOpen,
                cursor: MouseCursor::Text,
                label: "search".into(),
            });

            // Result rows first (they share the panel draw call).
            let mut y = top + field_h + 12.0;
            let avail = (vp.pos.y + vp.size.y - 2.0 * theme::GAP - y).max(0.0);
            let max_rows = (((avail + row_gap) / (row_h + row_gap)).floor() as usize).max(1);
            let start = (state.launcher.sel + 1).saturating_sub(max_rows);
            let end = (start + max_rows).min(n_hits);
            let rows: Vec<launcher::Hit> = state.launcher.hits[start..end].to_vec();
            let sel = state.launcher.sel;
            let mut texts: Vec<(DVec2, String, theme::Rgba, f64)> = Vec::new();
            for (i, hit) in rows.iter().enumerate() {
                let idx = start + i;
                let r = rect(x, y, w, row_h);
                let (bg, fg) = if idx == sel {
                    (theme::INK, theme::BG)
                } else {
                    (theme::BG, theme::INK)
                };
                self.draw_panel.color = rgba_a(bg, 1.0);
                self.draw_panel.border_color = rgba_a(theme::INK, 1.0);
                self.draw_panel.border_size = 1.0;
                self.draw_panel.alpha = 1.0;
                self.draw_panel.draw_abs(cx, r);
                let ty = y + (row_h - self.cell.natural) / 2.0;
                let badge = match hit.ws {
                    Some(k) => format!("№{}", k + 1),
                    None => "new".to_string(),
                };
                let bx = x + w - 16.0 - badge.chars().count() as f64 * self.cell.adv;
                texts.push((dvec2(bx, ty), badge, fg, 0.55));
                let cols = (((bx - 12.0 - (x + 16.0)) / self.cell.adv).max(4.0)) as usize;
                let label = trunc(&hit.label, cols);
                let used = label.chars().count() + 2;
                texts.push((dvec2(x + 16.0, ty), label, fg, 1.0));
                if !hit.detail.is_empty() && hit.detail != hit.label && used < cols {
                    let detail = trunc(&hit.detail, cols - used);
                    texts.push((
                        dvec2(x + 16.0 + used as f64 * self.cell.adv, ty),
                        detail,
                        fg,
                        0.55,
                    ));
                }
                self.hits.push(HitR {
                    rect: r,
                    act: Act::LauncherRow(idx),
                    cursor: MouseCursor::Hand,
                    label: hit.label.clone(),
                });
                y += row_h + row_gap;
            }

            // The caret, above the field's rect.
            let q_chars = state.launcher.query.chars().count();
            let caret = state.launcher.caret.min(q_chars);
            self.draw_flat.new_draw_call(cx);
            self.draw_flat.color = rgba_a(theme::INK, 1.0);
            self.draw_flat.draw_abs(
                cx,
                rect(
                    x + 16.0 + caret as f64 * self.cell.adv,
                    top + (field_h - self.cell.line_h) / 2.0,
                    2.0,
                    self.cell.line_h,
                ),
            );

            // Text above everything: the query (or its ghost), the rows.
            self.draw_mono.new_draw_call(cx);
            let fy = top + (field_h - self.cell.natural) / 2.0;
            if state.launcher.query.is_empty() {
                self.set_text(Style::Muted, 1.0);
                self.draw_mono.draw_abs(
                    cx,
                    dvec2(x + 16.0, fy),
                    "search — panels, mail, people",
                );
            } else {
                let cols = (((w - 32.0) / self.cell.adv).max(4.0)) as usize;
                self.set_text(Style::N, 1.0);
                self.draw_mono
                    .draw_abs(cx, dvec2(x + 16.0, fy), &trunc(&state.launcher.query, cols));
            }
            for (pos, s, color, alpha) in texts {
                self.set_text(Style::N, 1.0);
                self.draw_mono.color = rgba_a(color, alpha);
                self.draw_mono.draw_abs(cx, pos, &s);
            }
            if n_hits == 0 && !state.launcher.query.is_empty() {
                self.set_text(Style::Muted, 1.0);
                self.draw_mono.draw_abs(cx, dvec2(x + 16.0, y + 6.0), "nothing matches");
            } else if end < n_hits {
                self.set_text(Style::Muted, 1.0);
                self.draw_mono.draw_abs(
                    cx,
                    dvec2(x + 16.0, y + 2.0),
                    &format!("… {} more", n_hits - end),
                );
            }
        }

        // The toast, above everything.
        if let Some((msg, err, since)) = state.toast.clone() {
            let age = since.elapsed().as_secs_f64();
            let a = (3.0 - age).clamp(0.0, 0.25) / 0.25;
            let wchars = msg.chars().count();
            let w = wchars as f64 * self.cell.adv + 20.0;
            let h = self.cell.line_h + 10.0;
            let r = rect(
                vp.pos.x + vp.size.x - w - 12.0,
                vp.pos.y + vp.size.y - h - 12.0,
                w,
                h,
            );
            let border = if err { theme::ERR } else { theme::INK };
            self.draw_panel.new_draw_call(cx);
            self.draw_panel.color = rgba_a(theme::BG, a);
            self.draw_panel.border_color = rgba_a(border, a);
            self.draw_panel.border_size = 1.0;
            self.draw_panel.alpha = a as f32;
            self.draw_panel.draw_abs(cx, r);
            self.draw_mono.new_draw_call(cx);
            self.set_text(if err { Style::Err } else { Style::N }, a);
            self.draw_mono
                .draw_abs(cx, r.pos + dvec2(10.0, 5.0), &msg);
        }
    }

    fn set_text(&mut self, st: Style, alpha: f64) {
        self.draw_mono.text_style.font_size = st.size() as f32;
        self.draw_mono.color = rgba_a(st.color(), alpha);
    }

    /// An uppercase tracked label (the stelaxis register style). Returns its
    /// drawn width.
    fn draw_label(
        &mut self,
        cx: &mut Cx2d,
        x: f64,
        y: f64,
        s: &str,
        color: theme::Rgba,
        alpha: f64,
    ) -> f64 {
        self.draw_mono.text_style.font_size = theme::LABEL_SIZE as f32;
        self.draw_mono.color = rgba_a(color, alpha);
        let step = self.cell.label_step();
        let up = s.to_uppercase();
        let mut dx = x;
        for ch in up.chars() {
            if ch != ' ' {
                let mut buf = [0u8; 4];
                self.draw_mono.draw_abs(cx, dvec2(dx, y), ch.encode_utf8(&mut buf));
            }
            dx += step;
        }
        dx - x
    }

    fn draw_text_at(&mut self, cx: &mut Cx2d, x: f64, y: f64, s: &str, st: Style, alpha: f64) {
        if st == Style::Label {
            // Labels are tracked and vertically centred in the line.
            let ly = y + (self.cell.natural - self.cell.label_line()) / 2.0;
            self.draw_label(cx, x, ly, s, st.color(), alpha);
            return;
        }
        self.set_text(st, alpha);
        self.draw_mono.draw_abs(cx, dvec2(x, y), s);
        if st == Style::Bold || st == Style::Big {
            // Fake bold: a second pass, nudged.
            self.draw_mono.draw_abs(cx, dvec2(x + 0.4, y), s);
        }
    }

    /// Chrome only: fill, border, header. Used for ghosts and as the first
    /// layer of live panels.
    #[allow(clippy::too_many_arguments)]
    fn draw_chrome(
        &mut self,
        cx: &mut Cx2d,
        r: Rect,
        title: &str,
        focused: bool,
        alpha: f64,
        close: Option<PanelId>,
        hover: Option<&Act>,
    ) {
        self.draw_panel.color = rgba_a(theme::BG, alpha);
        self.draw_panel.border_color = rgba_a(theme::INK, alpha);
        self.draw_panel.border_size = 1.0;
        self.draw_panel.alpha = alpha as f32;
        self.draw_panel.draw_abs(cx, r);

        let head = rect(r.pos.x, r.pos.y, r.size.x, theme::HEAD_H);
        if focused {
            self.draw_flat.color = rgba_a(theme::INK, alpha);
            self.draw_flat.draw_abs(cx, head);
        } else {
            self.draw_flat.color = rgba_a(theme::INK, alpha);
            self.draw_flat
                .draw_abs(cx, rect(r.pos.x, r.pos.y + theme::HEAD_H - 1.0, r.size.x, 1.0));
        }

        // Title: tracked uppercase, vertically centred, truncated to leave
        // room for the header buttons.
        let title_cols = (((r.size.x - 16.0 - 110.0) / self.cell.label_step()).max(4.0)) as usize;
        let t = trunc(title, title_cols);
        let ty = r.pos.y + (theme::HEAD_H - self.cell.label_line()) / 2.0;
        let color = if focused { theme::BG } else { theme::INK };
        self.draw_label(cx, r.pos.x + 8.0, ty, &t, color, alpha);

        // The close button.
        if let Some(pid) = close {
            let bw = theme::BTN_H;
            let br = rect(
                r.pos.x + r.size.x - bw - 4.0,
                r.pos.y + (theme::HEAD_H - bw) / 2.0,
                bw,
                bw,
            );
            self.draw_header_btn(cx, br, "×", "close", focused, alpha, Act::Close(pid), hover);
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn draw_header_btn(
        &mut self,
        cx: &mut Cx2d,
        r: Rect,
        label: &str,
        hit_label: &str,
        focused_head: bool,
        alpha: f64,
        act: Act,
        hover: Option<&Act>,
    ) {
        let hovered = hover == Some(&act);
        // On an inverted header the button inverts back when hovered.
        let (bg, fg) = match (focused_head, hovered) {
            (true, false) => (theme::INK, theme::BG),
            (true, true) => (theme::BG, theme::INK),
            (false, false) => (theme::BG, theme::INK),
            (false, true) => (theme::INK, theme::BG),
        };
        self.draw_panel.color = rgba_a(bg, alpha);
        self.draw_panel.border_color = rgba_a(if focused_head { theme::BG } else { theme::INK }, alpha);
        self.draw_panel.border_size = 1.0;
        self.draw_panel.alpha = alpha as f32;
        self.draw_panel.draw_abs(cx, r);
        let tw = self.cell.label_w(label.chars().count());
        let tx = r.pos.x + (r.size.x - tw) / 2.0;
        let ty = r.pos.y + (r.size.y - self.cell.label_line()) / 2.0;
        self.draw_label(cx, tx, ty, label, fg, alpha);
        self.hits.push(HitR {
            rect: r,
            act,
            cursor: MouseCursor::Hand,
            label: hit_label.to_string(),
        });
    }

    /// Draws a panel's retained content widget inside the body rect and
    /// registers its interactive children as e2e-addressable hits.
    fn draw_hosted(&mut self, cx: &mut Cx2d, state: &State, pid: PanelId, tpl: LiveId, body: Rect) {
        let Some(w) = self.hosted_widget(cx, pid, tpl) else {
            return;
        };
        let props = crate::panels::PanelProps {
            store: state.store.clone(),
            pid,
        };
        let mut scope = Scope::with_props(&props);
        cx.begin_turtle(
            Walk::abs_rect(body),
            Layout {
                clip_x: true,
                clip_y: true,
                ..Default::default()
            },
        );
        w.draw_all(cx, &mut scope);
        cx.end_turtle();

        // The e2e bridge: known interactive children become labelled hits;
        // a click on one synthesizes real pointer events at its centre.
        let mut reg: Vec<(String, Rect, Act)> = Vec::new();
        for (label, path) in [
            ("address", ids!(email_input)),
            ("password", ids!(pass_input)),
            ("imap", ids!(imap_input)),
            ("smtp", ids!(smtp_input)),
        ] {
            let r = w.widget(cx, path).area().rect(cx);
            if r.size.x > 0.0 {
                reg.push((label.to_string(), r, Act::Pointer(pid)));
            }
        }
        let add_r = w.widget(cx, ids!(add_btn)).area().rect(cx);
        if add_r.size.x > 0.0 {
            reg.push((
                "add account".to_string(),
                add_r,
                Act::WidgetOp(pid, WidgetOp::AddAccount),
            ));
        }
        let accounts = mail::accounts(&state.store);
        if let Some(list) = w.widget(cx, ids!(accounts_list)).as_portal_list().borrow() {
            for (idx, item) in list.items().iter() {
                let r = item.widget.button(cx, ids!(remove_btn)).area().rect(cx);
                if r.size.x > 0.0 {
                    if let Some(a) = accounts.get(*idx) {
                        reg.push((
                            "remove".to_string(),
                            r,
                            Act::WidgetOp(pid, WidgetOp::RemoveAccount(a.id)),
                        ));
                    }
                }
            }
        }
        for (label, r, act) in reg {
            self.hits.push(HitR {
                rect: r,
                act,
                cursor: MouseCursor::Hand,
                label,
            });
        }
    }

    fn draw_panel_full(&mut self, cx: &mut Cx2d, state: &mut State, pid: PanelId, r: Rect, alpha: f64) {
        // Cross-workspace lookup: inactive spaces draw during the slide.
        let Some(panel) = state.ws.panel(pid) else {
            return;
        };
        let kind = panel.kind.clone();
        let focused = state.ws.focus == Some(pid);
        // Everything this panel reads while drawing is its provenance —
        // the trace behind the panel context (cmd+i).
        state.store.trace_begin(pid);
        let title = state.panel_title(&kind);

        // Whole panel: focus on click (bottom-most hit).
        self.hits.push(HitR {
            rect: r,
            act: Act::Focus(pid),
            cursor: MouseCursor::Default,
            label: title.clone(),
        });

        let hover = state.hover.clone();
        self.draw_chrome(cx, r, &title, focused, alpha, Some(pid), hover.as_ref());

        // Extra header actions, right to left from the close button —
        // side effects live in the chrome, never floating in content.
        let head_btns: &[(&str, BtnAct)] = match kind {
            Kind::Inbox { .. } => &[("refresh", BtnAct::Refresh)],
            Kind::Message { .. } => &[("archive", BtnAct::Archive)],
            Kind::Compose { .. } => &[("send", BtnAct::Send), ("discard", BtnAct::Discard)],
            _ => &[],
        };
        let mut bx = r.pos.x + r.size.x - 18.0 - 4.0;
        for (label, act) in head_btns {
            let w = self.cell.label_w(label.chars().count()) + 12.0;
            bx -= w + 4.0;
            let br = rect(
                bx,
                r.pos.y + (theme::HEAD_H - theme::BTN_H) / 2.0,
                w,
                theme::BTN_H,
            );
            self.draw_header_btn(cx, br, label, label, focused, alpha, Act::Btn(pid, *act), hover.as_ref());
        }

        // The body: a clipped turtle, content on the char grid.
        let body = rect(
            r.pos.x + 1.0,
            r.pos.y + theme::HEAD_H,
            r.size.x - 2.0,
            (r.size.y - theme::HEAD_H - 1.0).max(0.0),
        );
        if body.size.y < 4.0 {
            return;
        }
        // Retained content (CR-002): kinds with a widget template draw a
        // widget tree instead of the char grid. Chrome above still fades;
        // the content pops — the pilot's accepted trade.
        if let Some(tpl) = hosted_tpl(&kind) {
            self.draw_hosted(cx, state, pid, tpl, body);
            return;
        }
        let pad = theme::PAD_X;
        let pad_y = theme::PAD_Y;
        let cols = (((body.size.x - 2.0 * pad) / self.cell.adv).max(8.0)) as usize;
        let lines = build_lines(state, pid, cols);
        let line_h = self.cell.line_h;
        // A leading run of pinned lines (the filter, table headers) stays put;
        // everything after it scrolls.
        let pin_count = lines.iter().take_while(|l| l.pin).count();
        let pinned_h = pin_count as f64 * line_h;
        let view_h = (body.size.y - 2.0 * pad_y - pinned_h).max(0.0);
        let content_h = (lines.len() - pin_count) as f64 * line_h;
        let max_scroll = (content_h - view_h).max(0.0);
        let (scroll, sel, caret_focus) = {
            let ui = state.ui.entry(pid).or_insert_with(|| {
                PanelUi::for_kind(&kind, &state.store, pid)
            });
            ui.max_scroll = max_scroll;
            ui.view_h = view_h;
            ui.scroll = ui.scroll.clamp(0.0, max_scroll);
            (ui.scroll, ui.sel, state.field)
        };

        cx.begin_turtle(
            Walk::abs_rect(body),
            Layout {
                clip_x: true,
                clip_y: true,
                ..self.layout
            },
        );

        let x0 = body.pos.x + pad;
        let body_top = body.pos.y;
        let body_bot = body.pos.y + body.size.y;
        let mut body_field_rows: Vec<(usize, f64)> = Vec::new(); // (row idx, y)

        // The scrolling region.
        for (li, line) in lines.iter().enumerate().skip(pin_count) {
            let y = body.pos.y + pad_y + pinned_h + (li - pin_count) as f64 * line_h - scroll;
            if y + line_h < body_top || y > body_bot {
                continue;
            }
            self.draw_line(
                cx, state, pid, line, li, x0, y, cols, body, alpha, sel, caret_focus,
                &mut body_field_rows,
            );
        }

        // Pinned lines: mask what scrolls underneath, then draw on top. The
        // fresh draw calls put the mask and the pinned text above the
        // already-batched content.
        if pin_count > 0 {
            self.draw_panel.new_draw_call(cx);
            self.draw_flat.new_draw_call(cx);
            self.draw_mono.new_draw_call(cx);
            self.draw_flat.color = rgba_a(theme::BG, alpha);
            self.draw_flat
                .draw_abs(cx, rect(body.pos.x, body.pos.y, body.size.x, pad_y + pinned_h));
            for (li, line) in lines.iter().enumerate().take(pin_count) {
                let y = body.pos.y + pad_y + li as f64 * line_h;
                self.draw_line(
                    cx, state, pid, line, li, x0, y, cols, body, alpha, sel, caret_focus,
                    &mut body_field_rows,
                );
            }
        }

        // A minimal thumb marks a scrollable body.
        if max_scroll > 0.0 {
            let track_y = body.pos.y + pad_y + pinned_h;
            let track_h = view_h;
            let thumb_h = (track_h * (view_h / content_h.max(1.0))).clamp(24.0, track_h);
            let thumb_y = track_y + (scroll / max_scroll) * (track_h - thumb_h);
            self.draw_flat.color = rgba_a(theme::MUTED, alpha);
            self.draw_flat
                .draw_abs(cx, rect(body.pos.x + body.size.x - 5.0, thumb_y, 3.0, thumb_h));
        }

        // Compose body region: one big field hit + caret.
        if let Kind::Compose { .. } = kind {
            if let Some((first_row_y, _)) = body_field_rows.first().map(|&(i, y)| (y, i)) {
                let region = rect(
                    body.pos.x,
                    first_row_y,
                    body.size.x,
                    (body_bot - first_row_y).max(0.0),
                );
                self.hits.push(HitR {
                    rect: region,
                    act: Act::Field(pid, FieldId::Body),
                    cursor: MouseCursor::Text,
                    label: "body".to_string(),
                });
            }
            if caret_focus == Some((pid, FieldId::Body)) {
                if let Some(ui) = state.ui.get(&pid) {
                    let (cr, cc) = ui.caret;
                    if let Some(&(_, cy)) = body_field_rows.iter().find(|&&(i, _)| i == cr) {
                        self.draw_flat.color = rgba_a(theme::INK, alpha);
                        self.draw_flat.draw_abs(
                            cx,
                            rect(x0 + cc as f64 * self.cell.adv, cy + 1.0, 1.5, line_h - 3.0),
                        );
                    }
                }
            }
        }

        cx.end_turtle();
    }

    /// One content line: row backing, left runs, right-aligned runs, rule.
    #[allow(clippy::too_many_arguments)]
    fn draw_line(
        &mut self,
        cx: &mut Cx2d,
        state: &State,
        pid: PanelId,
        line: &Line,
        li: usize,
        x0: f64,
        y: f64,
        cols: usize,
        body: Rect,
        alpha: f64,
        sel: Option<MailId>,
        caret_focus: Option<(PanelId, FieldId)>,
        body_field_rows: &mut Vec<(usize, f64)>,
    ) {
        let line_h = self.cell.line_h;
        let pad = theme::PAD_X;
        // Selected row backing.
        if let Some(mid) = line.row {
            let row_r = rect(body.pos.x, y - 1.0, body.size.x, line_h);
            let hovered = state.hover == Some(Act::Row(pid, mid));
            if sel == Some(mid) {
                self.draw_flat.color = rgba_a(theme::SEL, alpha);
                self.draw_flat.draw_abs(cx, row_r);
            } else if hovered {
                self.draw_flat.color = rgba_a(theme::HOVER, alpha);
                self.draw_flat.draw_abs(cx, row_r);
            }
            self.hits.push(HitR {
                rect: row_r,
                act: Act::Row(pid, mid),
                cursor: MouseCursor::Default,
                label: mail::mail(&state.store, mid)
                    .map(|m| m.head.subject)
                    .unwrap_or_default(),
            });
        }

        let mut cx_chars = 0usize;
        for seg in &line.left {
            cx_chars = self.draw_seg(
                cx, state, pid, seg, x0, y, cx_chars, alpha, caret_focus, li, body_field_rows,
            );
        }
        if !line.right.is_empty() {
            let rw: usize = line.right.iter().map(Seg::chars).sum();
            let mut rx = cols.saturating_sub(rw);
            for seg in &line.right {
                rx = self.draw_seg(
                    cx, state, pid, seg, x0, y, rx, alpha, caret_focus, li, body_field_rows,
                );
            }
        }
        if line.rule {
            let c = if line.rule_ink { theme::INK } else { theme::RULE };
            self.draw_flat.color = rgba_a(c, alpha);
            self.draw_flat
                .draw_abs(cx, rect(x0, y + line_h - 1.0, body.size.x - 2.0 * pad, 1.0));
        }
    }

    /// Draws one segment at char column `col`; returns the next char column.
    #[allow(clippy::too_many_arguments)]
    fn draw_seg(
        &mut self,
        cx: &mut Cx2d,
        state: &State,
        pid: PanelId,
        seg: &Seg,
        x0: f64,
        y: f64,
        col: usize,
        alpha: f64,
        field_focus: Option<(PanelId, FieldId)>,
        line_idx: usize,
        body_rows: &mut Vec<(usize, f64)>,
    ) -> usize {
        let adv = self.cell.adv;
        let line_h = self.cell.line_h;
        let x = x0 + col as f64 * adv;
        match seg {
            Seg::Sp(n) => col + n,
            Seg::T(s, st) => {
                self.draw_text_at(cx, x, y, s, *st, alpha);
                col + s.chars().count()
            }
            Seg::Link {
                label,
                target,
                dotted,
            } => {
                let n = label.chars().count();
                let w = n as f64 * adv;
                let act = if *dotted {
                    Act::Replace(pid, target.clone())
                } else {
                    Act::Open(pid, target.clone())
                };
                if state.hover.as_ref() == Some(&act) {
                    self.draw_flat.color = rgba_a(theme::HOVER, alpha);
                    self.draw_flat.draw_abs(cx, rect(x - 1.0, y - 1.0, w + 2.0, line_h));
                }
                self.draw_text_at(cx, x, y, label, Style::N, alpha);
                // Underline hangs 3 pt under the measured baseline.
                let uy = y + self.cell.asc + 3.0;
                self.draw_flat.color = rgba_a(theme::INK, alpha);
                if *dotted {
                    let mut dx = x;
                    while dx < x + w - 1.0 {
                        self.draw_flat.draw_abs(cx, rect(dx, uy, 1.6, 1.6));
                        dx += 4.5;
                    }
                } else {
                    self.draw_flat.draw_abs(cx, rect(x, uy, w, 1.0));
                }
                self.hits.push(HitR {
                    rect: rect(x, y, w, line_h),
                    act,
                    cursor: MouseCursor::Hand,
                    label: label.clone(),
                });
                col + n
            }
            Seg::Btn { label, act } => {
                let n = label.chars().count() + 2;
                let a = Act::Btn(pid, *act);
                let hovered = state.hover.as_ref() == Some(&a);
                let (bg, fg) = if hovered {
                    (theme::INK, theme::BG)
                } else {
                    (theme::BG, theme::INK)
                };
                let tw = self.cell.label_w(label.chars().count());
                let bw = tw + 14.0;
                let br = rect(x, y + (line_h - theme::BTN_H) / 2.0 - 1.0, bw, theme::BTN_H);
                self.draw_panel.color = rgba_a(bg, alpha);
                self.draw_panel.border_color = rgba_a(theme::INK, alpha);
                self.draw_panel.border_size = 1.0;
                self.draw_panel.alpha = alpha as f32;
                self.draw_panel.draw_abs(cx, br);
                let ty = br.pos.y + (theme::BTN_H - self.cell.label_line()) / 2.0;
                self.draw_label(cx, x + 7.0, ty, label, fg, alpha);
                self.hits.push(HitR {
                    rect: br,
                    act: a,
                    cursor: MouseCursor::Hand,
                    label: label.clone(),
                });
                col + n
            }
            Seg::Kbd(s) => {
                let n = s.chars().count() + 2;
                let tw = self.cell.label_w(s.chars().count());
                let bw = tw + 10.0;
                let kh = line_h - 5.0;
                let br = rect(x, y + (line_h - kh) / 2.0 - 1.0, bw, kh);
                self.draw_panel.color = rgba_a(theme::BG, alpha);
                self.draw_panel.border_color = rgba_a(theme::INK, alpha);
                self.draw_panel.border_size = 1.0;
                self.draw_panel.alpha = alpha as f32;
                self.draw_panel.draw_abs(cx, br);
                let ty = br.pos.y + (kh - self.cell.label_line()) / 2.0;
                self.draw_label(cx, x + 5.0, ty, s, theme::INK, alpha);
                col + n
            }
            Seg::Fld { id, w } => {
                if *id == FieldId::Body {
                    // Marker only: remember where this body row is drawn.
                    body_rows.push((line_idx.saturating_sub(2), y));
                    return col;
                }
                let focused = field_focus == Some((pid, *id));
                let fr = rect(x, y - 1.0, *w as f64 * adv + 8.0, line_h);
                self.draw_panel.color = rgba_a(theme::BG, alpha);
                self.draw_panel.border_color =
                    rgba_a(if focused { theme::INK } else { theme::RULE }, alpha);
                self.draw_panel.border_size = 1.0;
                self.draw_panel.alpha = alpha as f32;
                self.draw_panel.draw_abs(cx, fr);
                let ui = state.ui.get(&pid);
                let (text, caret) = match (id, ui) {
                    (FieldId::Filter, Some(u)) => (u.filter.text.clone(), u.filter.caret),
                    (FieldId::To, Some(u)) => (u.to.text.clone(), u.to.caret),
                    (FieldId::Subject, Some(u)) => (u.subject.text.clone(), u.subject.caret),
                    (FieldId::SetEmail, Some(u)) => (u.set_email.text.clone(), u.set_email.caret),
                    (FieldId::SetPass, Some(u)) => (u.set_pass.text.clone(), u.set_pass.caret),
                    (FieldId::SetImap, Some(u)) => (u.set_imap.text.clone(), u.set_imap.caret),
                    (FieldId::SetSmtp, Some(u)) => (u.set_smtp.text.clone(), u.set_smtp.caret),
                    _ => (String::new(), 0),
                };
                // A password draws as dots — same length, same caret.
                let text = if *id == FieldId::SetPass {
                    "•".repeat(text.chars().count())
                } else {
                    text
                };
                if text.is_empty() && *id == FieldId::Filter {
                    self.draw_text_at(cx, x + 4.0, y, "filter…  ( / )", Style::Muted, alpha);
                } else {
                    self.draw_text_at(cx, x + 4.0, y, &text, Style::N, alpha);
                }
                if focused {
                    self.draw_flat.color = rgba_a(theme::INK, alpha);
                    self.draw_flat.draw_abs(
                        cx,
                        rect(x + 4.0 + caret as f64 * adv, y + 1.0, 1.5, line_h - 4.0),
                    );
                }
                self.hits.push(HitR {
                    rect: fr,
                    act: Act::Field(pid, *id),
                    cursor: MouseCursor::Text,
                    label: match id {
                        FieldId::Filter => "filter",
                        FieldId::To => "to",
                        FieldId::Subject => "subject",
                        FieldId::Body => "body",
                        FieldId::SetEmail => "address",
                        FieldId::SetPass => "password",
                        FieldId::SetImap => "imap",
                        FieldId::SetSmtp => "smtp",
                    }
                    .to_string(),
                });
                col + w
            }
        }
    }
}

// ---------------------------------------------------------------------------
// App
// ---------------------------------------------------------------------------

/// The makepad application root.
#[derive(Script, ScriptHook)]
pub struct App {
    #[live]
    ui: WidgetRef,
    #[rust]
    shaped: bool,
    #[rust]
    shape_tries: u32,
}

impl MatchEvent for App {
    fn handle_startup(&mut self, cx: &mut Cx) {
        // A borderless window over the display's visible frame (menu bar and
        // Dock stay) — mosaic's shape, deliberately NOT a fullscreen Space.
        // Android has no window to shape: the surface is the screen.
        #[cfg(target_os = "macos")]
        {
            let win = self.ui.window(cx, ids!(main_window));
            win.configure_macos_window(
                cx,
                MacosWindowConfig {
                    chrome: MacosWindowChrome::Borderless,
                    resizable: false,
                    miniaturizable: false,
                    ..MacosWindowConfig::default()
                },
            );
            let (pos, size) = desired_frame();
            win.configure_window(cx, size, pos, false, "superapp".to_string());
            if !background_run() {
                crate::mac::activate();
            }
        }
        #[cfg(not(target_os = "macos"))]
        let _ = cx;
    }
}

impl AppMain for App {
    fn script_mod(vm: &mut ScriptVm) -> ScriptValue {
        makepad_widgets::script_mod(vm);
        crate::panels::script_mod(vm);
        self::script_mod(vm)
    }

    fn handle_event(&mut self, cx: &mut Cx, event: &Event) {
        self.match_event(cx, event);
        self.ui.handle_event(cx, event, &mut Scope::empty());

        // Enforce the window shape once the widget tree exists: at Startup the
        // script has not instantiated it, so the configure call above no-ops
        // (mosaic spike B, TASK 2 — same workaround).
        #[cfg(target_os = "macos")]
        if !self.shaped && self.shape_tries < 240 {
            if let Event::NextFrame(_) | Event::Draw(_) = event {
                self.shape_tries += 1;
                let win = self.ui.window(cx, ids!(main_window));
                if win.window_id().is_some() {
                    let (pos, size) = desired_frame();
                    let cur = win.get_inner_size(cx);
                    if (cur.x - size.x).abs() > 1.0 || (cur.y - size.y).abs() > 1.0 {
                        win.resize(cx, size);
                        win.reposition(cx, pos);
                    } else {
                        self.shaped = true;
                        if background_run() {
                            // Behind everything, click-through: an e2e run must
                            // not take the screen (patch 0003 keeps it
                            // presenting while occluded).
                            crate::mac::configure_background_window();
                        } else {
                            crate::mac::activate();
                        }
                    }
                }
            }
        }
    }
}

/// Entry point. `app_main!` generates the real `fn main`; this is the hook the
/// desktop binary calls. On android the same macro generates the JNI
/// `activityOnCreate` symbol instead and nothing calls `run`.
#[cfg(not(target_os = "android"))]
pub fn run() {
    let _ = config();
    if background_run() {
        // makepad skips presents for occluded windows; patch 0003 adds this
        // bypass so a background e2e run still draws what it screenshots.
        std::env::set_var("MAKEPAD_PRESENT_WHEN_OCCLUDED", "1");
    }
    main();
}

app_main!(App);
