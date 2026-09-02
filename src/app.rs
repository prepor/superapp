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

use std::collections::{BTreeSet, HashMap};
use std::time::Instant;

use makepad_widgets::*;
// Touch types are not in the curated platform re-export list.
use makepad_widgets::makepad_platform::event::{
    ScrollEvent, ScrollPhase, TouchState, TouchUpdateEvent,
};
use makepad_widgets::makepad_platform::ime::TextInputConfig;

use crate::core::{self, Dir, Kind, PanelId, Seed, Wm, Ws, WS_N};
use crate::e2e;
use crate::launcher;
use crate::mail;
use crate::panels::*;
use crate::store::Store;
use crate::sync;
use crate::spring::{Spring, SpringParams};
use crate::theme;
use crate::ui::{self, trunc, BtnAct, Style};

// ---------------------------------------------------------------------------
// Configuration
// ---------------------------------------------------------------------------

/// On **virtual time** — a headless build, and every panels-library mount —
/// one draw cycle is one frame of exactly this long, for both the springs
/// and the e2e runner. Nothing reads the wall clock, so a run is
/// reproducible whether the machine is idle or running a dozen other
/// suites.
pub(crate) const FRAME_MS: f64 = 1000.0 / 60.0;

/// How often the manual pump runs a sync/send round, in frames. Half a
/// second of frame time — often enough that a script's `wait` sees the
/// result, rare enough that a dead host is not dialled sixty times a second.
const PUMP_EVERY: u64 = 30;

/// The same cadence in seconds of virtual time, for a mount whose frames
/// jump by a whole `wait` at once (CR-006).
const PUMP_S: f64 = 0.5;

/// The windowed e2e runner is paced by a real timer at this interval; the
/// runner counts the same milliseconds either way.
const E2E_TICK_MS: f64 = 30.0;

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
    /// The device-sync bucket base URL (`--bucket http://127.0.0.1:9000`).
    /// When set, replication is on: this device joins the lineage, follows or
    /// holds the lease, and the locked screen appears when it does not write.
    bucket: Option<String>,
    /// Open the panels library instead of the workspace (`--library
    /// [NAME...]`): the catalogue's scenes whose names contain one of
    /// these, or every scene when none is given. CR-006.
    library: Option<Vec<String>>,
    /// The headless backend's `--no-draw`: the widget pass runs, nothing is
    /// rasterized. Read here so a `shot` knows there is nothing to keep.
    no_draw: bool,
}

/// Whether this run rasterizes nothing (`--no-draw`).
pub(crate) fn no_draw() -> bool {
    config().no_draw
}

/// `SUPERAPP_FRAME_LOG=1`: every frame's draw cost, and every event that
/// took over a millisecond, on stderr — for finding out where a window
/// spends its time. Read once: it is on the paint path.
pub(crate) fn frame_log() -> bool {
    static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ON.get_or_init(|| std::env::var_os("SUPERAPP_FRAME_LOG").is_some())
}

/// The scene names `--library` asked for (none: every scene), when it did.
pub(crate) fn library_filter() -> Option<&'static [String]> {
    config().library.as_deref()
}

/// The e2e script to replay, if any, and where its screenshots go.
pub(crate) fn e2e_script() -> (Option<&'static str>, &'static str) {
    (config().e2e.as_deref(), &config().out)
}

/// Everything a stage needs to come up (CR-006). The window's own stage
/// builds one from argv at startup; the panels library builds one per
/// mount from a scene's node.
pub struct Boot {
    /// The store's path; `None` is in memory.
    pub db: Option<std::path::PathBuf>,
    /// A forced unit grid.
    pub grid: Option<core::Grid>,
    /// The send-undo window, seconds.
    pub send_delay: f64,
    /// Run on the fixed frame clock. Always for a mount; for the primary
    /// stage exactly under a headless build.
    pub virtual_time: bool,
    /// The outside.
    pub outside: BootOutside,
    /// Keep passwords in memory (e2e runs and mounts) rather than the
    /// keychain.
    pub secrets_in_memory: bool,
    /// A script to replay. The primary stage runs it as a suite; a mount
    /// replays it up to its shot and stays there.
    pub steps: Option<Vec<e2e::Step>>,
    /// The window's own stage: owns the menu bar, the IME, the fallback
    /// store poll. A mount owns nothing outside its pass.
    pub primary: bool,
    /// A prefix for the script's messages — a mount's scene and node.
    pub tag: String,
    /// Solo: come up on this one panel alone, drawn at the whole viewport,
    /// chrome included — a panel node of the library. Otherwise the
    /// workspace is the restored session, or the default layout.
    pub open: Option<Opener>,
}

/// What a solo stage opens on: the kind, resolved against the seeded
/// store (a mail by its subject, a sender by name).
pub type Opener = Box<dyn FnOnce(&Store) -> Kind>;

/// Which [`crate::effect::Outside`] a booting stage gets.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BootOutside {
    /// The network, the keychain (or memory), the clipboard, the screen.
    Real,
    /// Every verb fails, loudly; the clock still runs.
    Deny,
    /// The in-memory mail world.
    Fake,
}

impl Boot {
    /// The primary stage's boot, from argv. A script that fails to parse
    /// ends the process here, before a window exists to be confused by it.
    fn primary(cx: &Cx) -> Boot {
        // Opened on the library, the script is the canvas's, not this
        // stage's.
        let steps = config().e2e.as_ref().filter(|_| library_filter().is_none()).map(|path| {
            match std::fs::read_to_string(path)
                .map_err(|e| e.to_string())
                .and_then(|s| e2e::parse(&s))
            {
                Ok(steps) => {
                    eprintln!("e2e: {} step(s) from {path}", steps.len());
                    steps
                }
                Err(e) => {
                    eprintln!("e2e: {path}: {e}");
                    std::process::exit(2);
                }
            }
        });
        Boot {
            db: db_path(cx),
            grid: config().grid,
            send_delay: config().send_delay,
            virtual_time: cfg!(headless),
            outside: BootOutside::Real,
            secrets_in_memory: config().e2e.is_some(),
            steps,
            primary: true,
            tag: String::new(),
            open: None,
        }
    }
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
        let mut args = std::env::args().skip(1).peekable();
        while let Some(a) = args.next() {
            match a.as_str() {
                // The headless backend's own flags: read (and, for the
                // budget, skipped) here so they are not reported as unknown.
                "--no-draw" => c.no_draw = true,
                "--draws" => {
                    args.next();
                }
                "--library" => {
                    let mut paths = Vec::new();
                    while let Some(p) = args.next_if(|p| !p.starts_with("--")) {
                        paths.push(p);
                    }
                    c.library = Some(paths);
                }
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
                "--bucket" => c.bucket = args.next(),
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
                    emoji := FontMember{res: crate_resource("makepad_widgets:resources/NotoColorEmoji.ttf") asc: 0.0 desc: 0.0}
                }
                line_spacing: 1.0
            }
            color: #141414ff
        }
    }

    // The panels library (CR-006). A mount renders into its own pass; this
    // quad shows that pass's texture on the canvas, at whatever zoom.
    set_type_default() do #(crate::library::DrawTex::script_shader(vm)){
        ..mod.draw.DrawQuad
        image: texture_2d(float)
        pixel: fn() {
            return self.image.sample_as_bgra(self.pos)
        }
    }

    // An arrowhead: a solid triangle pointing right, filling its quad.
    set_type_default() do #(crate::library::DrawHead::script_shader(vm)){
        ..mod.draw.DrawQuad
        color: vec4(0.078, 0.078, 0.078, 1.0)
        pixel: fn() {
            let sdf = Sdf2d.viewport(self.pos * self.rect_size)
            sdf.move_to(0.0, 0.0)
            sdf.line_to(self.rect_size.x, self.rect_size.y * 0.5)
            sdf.line_to(0.0, self.rect_size.y)
            sdf.close_path()
            sdf.fill(self.color)
            return sdf.result
        }
    }

    let LibraryBase = #(crate::library::Library::register_widget(vm))
    let Library = set_type_default() do LibraryBase{
        width: Fill
        height: Fill
        draw_mono +: {
            text_style: TextStyle{
                font_family: FontFamily{
                    latin := FontMember{res: file_resource("/System/Library/Fonts/Menlo.ttc") asc: 0.0 desc: 0.0}
                    fallback := FontMember{res: crate_resource("makepad_widgets:resources/LiberationMono-Regular.ttf") asc: 0.0 desc: 0.0}
                    symbols := FontMember{res: crate_resource("makepad_widgets:resources/NotoSans-Regular.ttf") asc: 0.0 desc: 0.0}
                    emoji := FontMember{res: crate_resource("makepad_widgets:resources/NotoColorEmoji.ttf") asc: 0.0 desc: 0.0}
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
                    // Both roots fill the window; argv decides which one
                    // boots and draws (`--library`, CR-006).
                    flow: Overlay
                    stage := Stage{
                        // Retained content templates (CR-002): named children
                        // of a custom-drawn widget are never auto-drawn —
                        // they are collected as templates and instantiated
                        // per panel, PortalList-style.
                        settings_tpl := mod.widgets.SettingsPanel{}
                        add_account_tpl := mod.widgets.AddAccountPanel{}
                        compose_tpl := mod.widgets.ComposePanel{}
                        inbox_tpl := mod.widgets.InboxPanel{}
                        message_tpl := mod.widgets.MessagePanel{}
                        contact_tpl := mod.widgets.ContactPanel{}
                        help_tpl := mod.widgets.HelpPanel{}
                        about_tpl := mod.widgets.AboutPanel{}
                        problems_tpl := mod.widgets.ProblemsPanel{}
                        effects_tpl := mod.widgets.EffectsPanel{}
                        job_tpl := mod.widgets.JobPanel{}
                        // The modal overlays are hosted the same way, keyed
                        // by a reserved id rather than a panel.
                        rows_overlay_tpl := mod.widgets.RowsOverlay{}
                        launcher_overlay_tpl := mod.widgets.LauncherOverlay{}
                    }
                    library := Library{
                        // Templates, never auto-drawn: a component node is
                        // instantiated from its widget's, a panel or
                        // workspace node from the stage's — exactly as
                        // panels are from theirs.
                        inbox_row_tpl := mod.widgets.InboxRow{}
                        thread_msg_tpl := mod.widgets.ThreadMsg{}
                        overlay_row_tpl := mod.widgets.OverlayRow{}
                        launcher_overlay_tpl := mod.widgets.LauncherOverlay{}
                        account_row_tpl := mod.widgets.AccountRow{}
                        effect_row_tpl := mod.widgets.EffectRow{}
                        link_tpl := mod.widgets.SLink{}
                        problem_row_tpl := mod.widgets.ProblemRow{}
                        stage_tpl := Stage{
                            settings_tpl := mod.widgets.SettingsPanel{}
                            add_account_tpl := mod.widgets.AddAccountPanel{}
                            compose_tpl := mod.widgets.ComposePanel{}
                            inbox_tpl := mod.widgets.InboxPanel{}
                            message_tpl := mod.widgets.MessagePanel{}
                            contact_tpl := mod.widgets.ContactPanel{}
                            help_tpl := mod.widgets.HelpPanel{}
                            about_tpl := mod.widgets.AboutPanel{}
                            problems_tpl := mod.widgets.ProblemsPanel{}
                            effects_tpl := mod.widgets.EffectsPanel{}
                            job_tpl := mod.widgets.JobPanel{}
                        job_tpl := mod.widgets.JobPanel{}
                            rows_overlay_tpl := mod.widgets.RowsOverlay{}
                            launcher_overlay_tpl := mod.widgets.LauncherOverlay{}
                        }
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

pub(crate) fn rgba_a(c: theme::Rgba, alpha: f64) -> Vec4f {
    vec4(c[0], c[1], c[2], c[3] * alpha as f32)
}

/// Redraws what this stage draws into. The window's own stage draws into
/// the window, so that is everything; a panels-library mount redraws only
/// its own pass — and the canvas that composites it — so one mount's
/// keystroke does not re-lay-out a hundred others.
fn redraw_scoped(cx: &mut Cx, lists: Option<(DrawListId, DrawListId)>, mount: bool) {
    match lists {
        Some((own, canvas)) => {
            cx.redraw_list_and_children(own);
            cx.redraw_list(canvas);
        }
        // A mount the canvas has not rendered yet is pending there already;
        // marking the whole window would make every other mount pending
        // too, and the budget would never get past the first few.
        None if mount => {}
        None => cx.redraw_all(),
    }
}

fn rect(x: f64, y: f64, w: f64, h: f64) -> Rect {
    Rect {
        pos: dvec2(x, y),
        size: dvec2(w, h),
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
    /// A list panel's cursor landed on a row: open its detail joined
    /// **without taking focus** (CR-005). The list keeps the keyboard, so
    /// the walk carries on. Carries the kind, not an id — the inbox previews
    /// a message and the effect log a job, through the one door.
    Preview(PanelId, Kind),
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
    /// The locked screen's button: take the device-sync lease (CR-005).
    Acquire,
    /// The problems mark in the toast's corner: go to the problems panel
    /// where it is open, or open it — the launcher's verb.
    Problems,
    /// The locked screen's backdrop: absorbs the click, does nothing.
    Noop,
}

/// Semantic button operations on retained panels (the e2e bridge).
#[derive(Debug, Clone, Copy, PartialEq)]
enum WidgetOp {
    AddAccount,
    RemoveAccount(i64),
    OpenMail(i64),
    /// The `i`-th row of a field's autocomplete (CR-006): the inbox
    /// filter's, or the compose TO field's.
    Suggest(usize),
    /// A thread row's header (CR-007): open the message, or close it.
    ToggleMail(i64),
    /// A message's quoted tail: unfold it, or fold it back.
    ToggleQuote(i64),
    /// A problems row's *sync* button: kick the account's worker.
    SyncAccount(i64),
    /// A problems row's *retry* button: file the failed send again.
    RetrySend(i64),
    /// A problems row's *reopen* link: the failed send back as a draft.
    ReopenSend(i64),
    /// A row of the effect log: preview the job it stands for, the way a
    /// click on an inbox row previews its mail.
    OpenJob(i64),
}

#[derive(Debug, Clone)]
struct HitR {
    rect: Rect,
    act: Act,
    cursor: MouseCursor,
    /// What an e2e script can address this element by.
    label: String,
}

/// The panel an act belongs to (overlay acts belong to none). A hosted
/// widget's own hits — rows, fields, selectable runs — name their panel too:
/// they sit *above* the panel-wide `Focus` hit, so a finger that lands on a
/// row would otherwise resolve to no panel at all and the gesture would die
/// before it could scroll (see [`Stage::touch_move`]).
fn act_pid(act: &Act) -> Option<PanelId> {
    match act {
        Act::Focus(pid)
        | Act::Close(pid)
        | Act::Btn(pid, _)
        | Act::Open(pid, _)
        | Act::Replace(pid, _)
        | Act::Tab(pid)
        | Act::Preview(pid, _)
        | Act::Pointer(pid)
        | Act::WidgetOp(pid, _) => Some(*pid),
        Act::WsRow(_) | Act::LauncherOpen | Act::LauncherRow(_) | Act::HistoryRow(_) | Act::OverlayClose | Act::Acquire | Act::Problems | Act::Noop => None,
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

/// The action kind a reading walk records — a preview, or an in-place
/// replace of one mail by another. Named because two rules key off it:
/// these coalesce per panel into one undo node, and they are the one kind
/// that does **not** wake the sync workers ([`State::act`]).
const READ: &str = "read";

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
    /// A sideways finger on a list row: triage. Left archives, right deletes
    /// — the curtain and its physics live in [`RowSwipe`] on the stage, since
    /// a committed swipe keeps animating after the finger is gone.
    RowSwipe { uid: u64 },
    /// A gesture that came to nothing (a sideways one-finger move with
    /// nothing to triage, a lifted pan finger): inert until every finger
    /// lifts.
    Dead,
}

/// Live touches and the gesture they add up to.
#[derive(Debug, Default)]
struct TouchNav {
    /// uid → (start, latest) positions.
    pts: HashMap<u64, (DVec2, DVec2)>,
    mode: TouchMode,
}

/// How far across a row the curtain must be drawn for a lift to commit.
const SWIPE_COMMIT: f64 = 0.35;

/// A swiped inbox row and the curtain wiping across it (CR-005). The row
/// itself never moves: an ink panel carrying the action's name is drawn in
/// from the edge the finger travels away from, which is also the edge that
/// action's button sits on in a message header. Past [`SWIPE_COMMIT`] the
/// curtain inverts — the same "this will fire" inversion a hovered header
/// button uses, and the same reason it needs no colour.
///
/// It lives on the [`Stage`] rather than in [`TouchMode`] because a committed
/// swipe outlives its finger: the curtain finishes covering the row, and only
/// then does the mail leave the inbox.
#[derive(Debug)]
struct RowSwipe {
    /// The inbox the row belongs to.
    pid: PanelId,
    /// The mail under the finger.
    id: core::MailId,
    /// The row's rect as last drawn — kept so the curtain still has somewhere
    /// to be after the mail leaves the query.
    slot: Rect,
    /// How far the curtain is drawn, signed: negative wipes in from the right
    /// (archive), positive from the left (delete).
    x: Spring,
    /// Set on a committing lift: `true` deletes, `false` archives. The action
    /// fires when the spring lands.
    commit: Option<bool>,
}

impl RowSwipe {
    /// Whether the curtain is far enough across to fire on lift.
    fn armed(&self) -> bool {
        self.slot.size.x > 0.0 && self.x.value().abs() >= self.slot.size.x * SWIPE_COMMIT
    }
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
    /// The overlay chassis' presence, 0 (away) → 1 (up): the wash, the
    /// sheet and its contents ride it together. Retargeted by `kick`, so
    /// every overlay change animates.
    overlay: Option<Spring>,
}

impl Anim {
    fn camera(&mut self) -> &mut Spring {
        self.camera
            .get_or_insert_with(|| Spring::at_rest(0.0, SpringParams::movement()))
    }

    fn overlay(&mut self) -> &mut Spring {
        self.overlay
            .get_or_insert_with(|| Spring::at_rest(0.0, SpringParams::overlay()))
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
        if let Some(o) = self.overlay.as_mut() {
            o.advance(dt);
            active |= !o.is_done();
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
    /// Selected row (enter activates it).
    sel: usize,
    hits: Vec<launcher::Hit>,
}

/// How device sync runs. Production spawns a worker thread; a headless run
/// drives the passes inline from the frame loop against the virtual clock, so
/// a scripted `wait` advances a handoff exactly the way it advances the mail
/// engine (mirroring [`sync::Pump::Manual`]).
#[allow(dead_code)]
enum ReplMode {
    /// A background worker thread (production).
    Threads(crate::repl::Worker),
    /// Inline passes on the UI thread, driven by the frame loop (headless).
    Manual {
        bucket: std::sync::Arc<dyn crate::object::Object>,
    },
}

struct State {
    ws: Wm,
    /// Everything that leaves the process, plus the clock (CR-004). Holds
    /// the same `Rc<Store>` as the field below, so the two cannot diverge —
    /// `store` stays for the hundred read sites that only want the store.
    world: std::rc::Rc<crate::effect::World>,
    /// The action tree (CR-004). In memory, so it dies with the process:
    /// a restart loses undo, but never loses work — the rows every action
    /// wrote are durable, and the passes read those, never this.
    history: crate::history::History,
    store: std::rc::Rc<Store>,
    /// The store's file path — sync workers open their own connections to
    /// it (`None` = in-memory: no workers).
    db_path: Option<std::path::PathBuf>,
    /// Who runs the passes (CR-004): threads in production, inline in a
    /// headless world.
    pump: sync::Pump,
    /// Where passwords live. An e2e run keeps them in memory, so a suite
    /// never writes to a human's keychain and two runs never collide.
    secrets: crate::effect::Secrets,
    /// What time the app thinks it is. Virtual under a headless build, so
    /// a send deadline moves with the script rather than with the machine.
    clock: crate::effect::Clock,
    /// Standing problems already announced, by key: a new one toasts on
    /// the signal that brings it, and the mark carries it from then on.
    seen_problems: BTreeSet<String>,
    /// The last persisted logical snapshot — [`State::sync`] only writes
    /// when the state actually changed.
    last_saved: Option<core::WmSnap>,
    anim: Anim,
    viewport: DVec2,
    last_frame: Option<Instant>,
    animating: bool,
    hover: Option<Act>,
    /// `(message, is_error, when)` — `when` on the world's clock, not the
    /// wall's, so a toast fades by the same amount on every run.
    toast: Option<(String, bool, f64)>,
    overlay: Overlay,
    /// The overlay most recently up — what a close fade keeps drawing
    /// while the chassis' presence spring runs out.
    overlay_last: Overlay,
    launcher: LauncherUi,
    /// A panel to reveal alongside focus on the next [`State::sync`], once.
    /// A preview opens without taking focus, so nothing else would pull the
    /// camera onto it — and this must stay one-shot: `sync` runs on every
    /// viewport change and worker poll, and a standing rule here would fight
    /// the user's own pans.
    show_also: Option<PanelId>,
    /// Which messages each message panel shows open (CR-007): seeded when
    /// the panel opens on a mail, toggled by touch, kept no further than
    /// the process. Context, like the inbox cursor — never history.
    expand: HashMap<PanelId, crate::panels::Expansion>,
    /// The device-sync driver (CR-005), when a `--bucket` is configured.
    /// `None` means replication is off and the store is a plain local one.
    repl: Option<ReplMode>,
    /// The lease status the worker last reported — drives the locked screen.
    repl_status: crate::repl::Status,
    /// Whether the canonical device has seeded the demo world since it began
    /// holding (seeding is a holder-only act under replication).
    seeded: bool,
    /// The fixed frame clock (CR-006): a headless build, and every
    /// panels-library mount. False only for a windowed primary stage — the
    /// one place the wall clock is read.
    virtual_time: bool,
    /// A forced grid: `--grid`, or a scene's.
    grid: Option<core::Grid>,
    /// The send-undo window, seconds.
    send_delay: f64,
}

/// Where a virtual clock starts: the instant a headless run and every
/// library mount believe it is. Fixed, so a run is reproducible down to the
/// dates it draws — and public because a fixture that plants a *deadline*
/// (the effect queue's `not_before`) has to place it against this, not
/// against the wall or the mail seed's own dates.
#[must_use]
pub fn virtual_epoch() -> f64 {
    mail::ts(2026, 9, 1, 12, 0)
}

/// The grid for a viewport. Desktop is always 12×6; android picks 8×4 on the
/// unfolded screen and 4×3 on the cover display (the ~600 dp compact/medium
/// breakpoint — a fold/unfold resize crosses it). `--grid` overrides for
/// desktop previews of the phone layouts.
fn grid_for(vp: DVec2, forced: Option<core::Grid>) -> core::Grid {
    if let Some(g) = forced {
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
    fn new(store: Store, boot: &Boot) -> Self {
        let store = std::rc::Rc::new(store);
        let db_path = boot.db.clone();
        let secrets = if boot.secrets_in_memory {
            crate::effect::Secrets::Memory(crate::effect::MemSecrets::new())
        } else {
            crate::effect::Secrets::Keychain(
                db_path
                    .as_ref()
                    .and_then(|p| p.parent())
                    .map(std::path::Path::to_path_buf)
                    .unwrap_or_default(),
            )
        };
        // Virtual time: one fixed frame clock for the springs, the e2e
        // runner and the app's own deadlines alike. It starts at a fixed
        // instant so even the dates in a screenshot are reproducible.
        let epoch = virtual_epoch();
        let clock = if boot.virtual_time {
            crate::effect::Clock::virtual_from(epoch)
        } else {
            crate::effect::Clock::System
        };
        let outside: Box<dyn crate::effect::Outside> = match boot.outside {
            BootOutside::Real => Box::new(crate::effect::Real::new(secrets.clone(), clock.clone())),
            BootOutside::Deny => Box::new(crate::effect::Deny::with_clock(clock.clone())),
            BootOutside::Fake => Box::new(crate::effect::Fake {
                clock: epoch,
                ..Default::default()
            }),
        };
        let world = std::rc::Rc::new(crate::effect::World::new(
            store.clone(),
            outside,
            mail::registry(),
        ));
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
        // Under replication, nothing is written until the first pass resolves
        // this device's role: a would-be follower must not seed demo mail or
        // persist a boot layout into a store it is about to replace with the
        // holder's snapshot. The gate opens (or stays shut) when the role is
        // known.
        // Only the window's own stage replicates; a mount's world is its own.
        let repl = if boot.primary {
            Self::start_repl(&store, db_path.as_deref())
        } else {
            None
        };
        if repl.is_some() {
            store.set_writable(false);
        }
        // What already stands at boot is old news: the mark shows it, the
        // toasts are for what arrives from here on.
        let seen_problems: BTreeSet<String> = crate::problems::list(&store, None)
            .iter()
            .map(crate::problems::Problem::key)
            .collect();
        State {
            ws,
            world,
            secrets,
            clock,
            history: crate::history::History::new(),
            store,
            db_path,
            repl,
            repl_status: crate::repl::Status {
                role: crate::repl::Role::Detached,
                epoch: 0,
                unpublished: 0,
                device: String::new(),
            },
            seeded: false,
            // Headless: no threads at all. The passes run inline from the
            // frame loop, so ingest, push and send land at frame
            // Virtual time: no threads at all. The passes run inline from
            // the frame loop, so ingest, push and send land at frame
            // boundaries instead of whenever a worker happens to wake —
            // the last thing standing between a run and reproducibility.
            pump: if boot.virtual_time {
                sync::Pump::Manual
            } else {
                sync::Pump::threads()
            },
            seen_problems,
            last_saved: None,
            anim: Anim::default(),
            viewport: dvec2(1440.0, 900.0),
            last_frame: None,
            animating: false,
            hover: None,
            toast: None,
            overlay: Overlay::None,
            overlay_last: Overlay::None,
            launcher: LauncherUi::default(),
            show_also: None,
            expand: HashMap::new(),
            virtual_time: boot.virtual_time,
            grid: boot.grid,
            send_delay: boot.send_delay,
        }
    }

    /// Moves virtual time on by `secs`. A [`crate::effect::Fake`] carries
    /// its own clock (a test's), so a fake-world mount moves that by hand.
    fn advance_clock(&self, secs: f64) {
        self.clock.advance(secs);
        self.world.outside(|o| {
            if let Some(f) = o.as_any().downcast_mut::<crate::effect::Fake>() {
                f.clock += secs;
            }
        });
    }

    /// One round of the manual pump: every account's sync pass, the
    /// outbox, then the effect queue.
    fn pump_round(&mut self) {
        let w = self.world.clone();
        sync::tick(&w);
        crate::send::outbox_pass(&w);
        w.run_effects();
        // No worker signal here to ride: what the round broke is announced
        // by the round itself.
        self.announce_problems();
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

    /// How many grid rows a letter asks for: enough that the whole of it is
    /// on screen, so a long mail opens tall instead of opening scrolled.
    /// The kind's three rows are the floor, the grid is the ceiling.
    ///
    /// The measuring belongs to the shell rather than to [`core`]: only here
    /// are the column's width in characters and the share of the panel the
    /// body does *not* get both known.
    fn message_rows(&self, id: core::MailId, open: &BTreeSet<core::MailId>) -> Option<u32> {
        /// Roughly how many lines of body the message panel spends on
        /// everything that is not the letters: its own header, the TO
        /// line and its rule, the reply link at the foot, and the padding
        /// around them. An estimate on purpose — the wish only has to land
        /// on the right row.
        const CHROME_LINES: f64 = 6.0;

        let msgs = mail::thread(&self.store, id);
        if msgs.is_empty() {
            return None;
        }
        let (vw, vh) = self.vp();
        let grid = self.ws.grid;
        let gap = theme::GAP;
        let (gw, floor) = Kind::Message { id }.grid();

        // How wide the letter reads, in characters: the column, less the
        // panel's padding, over one mono advance.
        let unit_w = (vw - gap) / f64::from(grid.w);
        let text_w = unit_w * f64::from(gw.min(grid.w)) - gap - 2.0 * theme::PAD_X;
        let cols = (text_w / (theme::FONT_SIZE * theme::MONO_ADV)).max(1.0) as usize;
        let need = mail::thread_lines(&msgs, open, cols) as f64;

        // ...against how many lines a panel of `rows` rows has room for.
        let line_h = theme::FONT_SIZE * theme::LINE_H;
        let row_h = (vh - 2.0 * gap - f64::from(grid.h - 1) * gap) / f64::from(grid.h);
        let holds = |rows: u32| {
            let h = f64::from(rows) * row_h + f64::from(rows - 1) * gap;
            h / line_h - CHROME_LINES
        };
        Some((floor..=grid.h).find(|&r| holds(r) >= need).unwrap_or(grid.h))
    }

    /// Measures a kind before a panel shows it. Placement consults the wish
    /// — a tall letter earns a column of its own instead of squeezing into
    /// a neighbour — and a panel about to be born has no id to hang one on,
    /// so the shell measures ahead of the mutation.
    fn wish_ahead(&mut self, kind: &Kind) {
        if let Kind::Message { id } = kind {
            let open = self.seed_for(*id);
            if let Some(h) = self.message_rows(*id, &open) {
                let (w, _) = kind.grid();
                self.ws.wish(kind, (w, h));
            }
        }
    }

    /// What a panel opening on `id` shows open (CR-007): the mail itself
    /// and every unread mail of its thread — read *before* the open marks
    /// them, which is why the shell seeds rather than the widget.
    fn seed_for(&self, id: core::MailId) -> BTreeSet<core::MailId> {
        let mut open: BTreeSet<core::MailId> =
            mail::thread_unread(&self.store, id).into_iter().collect();
        open.insert(id);
        open
    }

    /// Records `open` on every panel now showing `id` that was not seeded
    /// for it already — the one just opened or re-targeted; a panel on
    /// another workspace already reading the same mail keeps its own.
    fn seed_expansion(&mut self, id: core::MailId, open: &BTreeSet<core::MailId>) {
        for pid in self.ws.showing(&Kind::Message { id }) {
            if self.expand.get(&pid).is_some_and(|e| e.for_mail == id) {
                continue;
            }
            self.expand.insert(
                pid,
                crate::panels::Expansion {
                    for_mail: id,
                    open: open.clone(),
                    quotes: BTreeSet::new(),
                },
            );
        }
    }

    /// Recomputes targets after a mutation and feeds the animator. The camera
    /// follows focus here — and only here, so trackpad pans stay free.
    fn sync(&mut self) {
        self.ws.set_grid(grid_for(self.viewport, self.grid));
        // Wishes measured from content, re-taken from scratch: a letter that
        // arrived, changed or left changes what its panel asks for. Ephemeral
        // like the grid above — measured here, never snapshotted.
        let wishes = self
            .ws
            .wss
            .iter()
            .flat_map(|w| w.panels.values())
            .filter_map(|p| match &p.kind {
                Kind::Message { id } => {
                    let (w, _) = p.kind.grid();
                    let open =
                        crate::panels::Expansion::for_panel(self.expand.get(&p.id), *id).open;
                    Some((p.kind.clone(), (w, self.message_rows(*id, &open)?)))
                }
                _ => None,
            })
            .collect();
        self.ws.set_wishes(wishes);
        // Expansion state dies with its panel.
        let live: Vec<PanelId> = self.expand.keys().copied().collect();
        for pid in live {
            if self.ws.panel(pid).is_none() {
                self.expand.remove(&pid);
            }
        }
        let vp = self.vp();
        let opts = self.opts();
        // A preview asked to be seen: reveal it first, then focus, so focus
        // still wins when both cannot fit (a phone grid, where each of them
        // is the whole screen).
        if let Some(pid) = self.show_also.take() {
            self.ws.ensure_visible(pid, vp, opts);
        }
        self.ws.ensure_focus_visible(vp, opts);

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
        let now = self.world.now();
        self.toast = Some((msg.into(), err, now));
    }

    /// The standing problems, derived afresh: the store's rows plus the
    /// lease status the worker last reported (see [`crate::problems`]).
    fn problems(&self) -> std::rc::Rc<Vec<crate::problems::Problem>> {
        let repl = self.repl.as_ref().map(|_| &self.repl_status);
        std::rc::Rc::new(crate::problems::list(&self.store, repl))
    }

    /// The rows a kind's props carry: the list for the problems panel,
    /// nothing for anyone else.
    fn problems_for(&self, kind: &Kind) -> std::rc::Rc<Vec<crate::problems::Problem>> {
        if matches!(kind, Kind::Problems) {
            self.problems()
        } else {
            std::rc::Rc::default()
        }
    }

    /// Toasts each problem the first time it stands, and forgets the ones
    /// that cleared, so a relapse is announced again. Device sync's line is
    /// toasted by the role change that brings it ([`Stage::tick_repl`]).
    fn announce_problems(&mut self) {
        let now = self.problems();
        for p in now.iter() {
            if self.seen_problems.contains(&p.key()) {
                continue;
            }
            match &p.source {
                crate::problems::Source::Account { .. } => {
                    self.toast(format!("sync failed — {}: {}", p.label, p.line), true);
                }
                crate::problems::Source::Send { given_up: true, .. } => {
                    self.toast(format!("send failed: {} — ⌘z reopens", p.line), true);
                }
                crate::problems::Source::Send { .. } => {
                    self.toast(format!("send failed: {} — retrying", p.line), true);
                }
                crate::problems::Source::Sync => {}
            }
        }
        self.seen_problems = now.iter().map(crate::problems::Problem::key).collect();
    }

    /// A panel's title, wherever it lives — for action labels.
    fn title_of(&self, pid: PanelId) -> String {
        self.ws
            .panel(pid)
            .map(|p| self.panel_title(&p.kind))
            .unwrap_or_else(|| "panel".into())
    }

    /// Runs one **undoable action**: mutates the in-memory `Wm`, writes the
    /// whole thing through in one transaction, and records a node — the
    /// layout before and after, plus whatever the action claimed of the
    /// world. Nav-only actions carry no claims and undo for free off the
    /// snapshot.
    /// The `data` closure runs on the store's writer thread (CR-005 phase
    /// 0), so it must own what it touches — `Send + 'static`. Its value is
    /// returned, which is how an action learns a freshly minted row id
    /// without a shared cell.
    fn act<R: Send + 'static>(
        &mut self,
        kind: &str,
        label: String,
        entity: Option<String>,
        mutate: impl FnOnce(&mut Wm),
        data: impl FnOnce(&rusqlite::Transaction) -> rusqlite::Result<R> + Send + 'static,
        intents: Vec<Box<dyn crate::history::Intent>>,
    ) -> Option<R> {
        // Under replication, a follower does not write — the gate would refuse
        // it anyway, but returning here keeps the in-memory `Wm` from drifting
        // ahead of a store that never took the change (CR-005).
        if self.repl.is_some() && !self.store.is_writable() {
            self.toast("read-only — acquire the lease to write", true);
            return None;
        }
        let before = self.ws.snapshot();
        mutate(&mut self.ws);
        let snap = self.ws.snapshot();
        let for_write = snap.clone();
        let r = self.store.write(move |tx| {
            crate::store::save_wm_tx(tx, &for_write)?;
            data(tx)
        });
        let out = match r {
            Ok(v) => Some(v),
            Err(e) => {
                eprintln!("store: action “{label}” failed: {e}");
                None
            }
        };
        let now = self.world.now();
        self.history.apply(crate::history::Action {
            kind,
            label,
            entity,
            before,
            after: snap.clone(),
            intents,
            ts: now,
        });
        self.last_saved = Some(snap);
        // Push soon: whatever this action changed about mail intent, a
        // worker makes the server agree without waiting for the poll —
        // and the sender re-times its next deadline.
        self.pump.kick();
        // And publish what we just captured to the other device promptly.
        self.repl_kick();
        out
    }

    /// An undoable action that only moves panels around.
    fn act_nav(
        &mut self,
        kind: &str,
        label: String,
        entity: Option<String>,
        mutate: impl FnOnce(&mut Wm),
    ) {
        self.act(
            kind,
            label,
            entity,
            mutate,
            |_| Ok::<(), rusqlite::Error>(()),
            Vec::new(),
        );
    }

    /// Restores the layout a history walk landed on, writing it through.
    fn land(&mut self, step: crate::history::Step) -> String {
        if let Err(e) = self.store.save_wm(&step.snap) {
            eprintln!("store: persisting the walk failed: {e}");
        }
        self.last_saved = Some(step.snap.clone());
        self.ws = Wm::restore(step.snap);
        step.label
    }

    /// Spawns a sync worker for every configured account that lacks one.
    /// Idempotent — call after boot and after adding an account. Workers
    /// for removed accounts retire themselves.
    /// Starts the device-sync worker when a `--bucket` is configured; `None`
    /// leaves the store a plain local one. The worker polls the bucket, drives
    /// the lease, and reports status the UI reads each signal (CR-005).
    /// Resolves the device-sync bucket URL from the three sources that let
    /// each platform configure it: the `--bucket` flag (desktop), the
    /// `SUPERAPP_BUCKET` environment variable, and a `bucket` file beside the
    /// store (how android is pointed at `http://10.0.2.2:PORT` — `adb push` a
    /// one-line file into the app's files dir).
    fn resolve_bucket(db_path: Option<&std::path::Path>) -> Option<String> {
        if let Some(u) = config().bucket.clone() {
            return Some(u);
        }
        if let Ok(u) = std::env::var("SUPERAPP_BUCKET") {
            let u = u.trim().to_string();
            if !u.is_empty() {
                return Some(u);
            }
        }
        let dir = db_path.and_then(std::path::Path::parent)?;
        let u = std::fs::read_to_string(dir.join("bucket")).ok()?.trim().to_string();
        (!u.is_empty()).then_some(u)
    }

    fn start_repl(
        store: &std::rc::Rc<Store>,
        db_path: Option<&std::path::Path>,
    ) -> Option<ReplMode> {
        let url = Self::resolve_bucket(db_path)?;
        let bucket: std::sync::Arc<dyn crate::object::Object> =
            std::sync::Arc::new(crate::object::HttpBucket::new(&url));
        // Headless: inline passes driven by the frame loop's virtual clock, so
        // a scripted run is deterministic. Production: a background thread.
        #[cfg(headless)]
        {
            let _ = store;
            Some(ReplMode::Manual { bucket })
        }
        #[cfg(not(headless))]
        {
            Some(ReplMode::Threads(crate::repl::spawn(store.db(), bucket, || {
                SignalToUI::set_ui_signal();
            })))
        }
    }

    // -- device sync, mode-agnostic (CR-005) --------------------------------

    /// Reconciles the reported status: caches it, seeds the demo world the
    /// first time this device holds (a holder-only act), and answers whether
    /// the role changed.
    fn apply_repl(&mut self, status: crate::repl::Status) -> bool {
        if status == self.repl_status {
            return false;
        }
        let role_changed = status.role != self.repl_status.role;
        self.repl_status = status;
        if matches!(self.repl_status.role, crate::repl::Role::Holder) && !self.seeded {
            self.seeded = true;
            let empty = mail::inbox(&self.store).is_empty()
                && mail::accounts(&self.store).is_empty();
            if empty {
                if let Err(e) = mail::seed_if_empty(&self.store) {
                    eprintln!("store: seeding demo mail failed: {e}");
                }
                // The seed publishes on the next pass.
            }
        }
        role_changed
    }

    /// Runs (or reads) one sync pass and reconciles the result. Answers
    /// whether the role changed.
    fn repl_poll(&mut self) -> bool {
        let status = match &self.repl {
            Some(ReplMode::Threads(w)) => w.status(),
            Some(ReplMode::Manual { bucket }) => {
                let b = bucket.clone();
                crate::repl::poll(&self.store, &*b)
            }
            None => return false,
        };
        self.apply_repl(status)
    }

    /// Asks to take the lease.
    fn repl_acquire(&mut self) {
        match &self.repl {
            Some(ReplMode::Threads(w)) => w.acquire(),
            Some(ReplMode::Manual { bucket }) => {
                let b = bucket.clone();
                let s = crate::repl::acquire(&self.store, &*b)
                    .unwrap_or_else(|_| crate::repl::poll(&self.store, &*b));
                self.apply_repl(s);
            }
            None => {}
        }
    }

    /// Publishes promptly after an action (or nudges the worker to).
    fn repl_kick(&mut self) {
        match &self.repl {
            Some(ReplMode::Threads(w)) => w.kick(),
            Some(ReplMode::Manual { bucket }) => {
                let b = bucket.clone();
                let s = crate::repl::poll(&self.store, &*b);
                self.apply_repl(s);
            }
            None => {}
        }
    }

    /// Hands the lease back (best effort).
    fn repl_release(&mut self) {
        match &self.repl {
            Some(ReplMode::Threads(w)) => w.release(),
            Some(ReplMode::Manual { bucket }) => {
                let b = bucket.clone();
                let s = crate::repl::release(&self.store, &*b)
                    .unwrap_or_else(|_| crate::repl::poll(&self.store, &*b));
                self.apply_repl(s);
            }
            None => {}
        }
    }

    /// Releases synchronously on close — the last chance to hand back.
    fn repl_release_blocking(&self) {
        match &self.repl {
            Some(ReplMode::Threads(w)) => w.release_blocking(),
            Some(ReplMode::Manual { bucket }) => {
                let b = bucket.clone();
                let _ = crate::repl::release(&self.store, &*b);
            }
            None => {}
        }
    }

    fn spawn_workers(&mut self) {
        if self.db_path.is_none() {
            return; // in memory: production workers never spawn here
        }
        let world = self.world.clone();
        let db = self.store.db();
        let secrets = self.secrets.clone();
        let clock = self.clock.clone();
        self.pump.ensure(&world, &db, &secrets, &clock, || {
            SignalToUI::set_ui_signal();
        });
    }
}

// ---------------------------------------------------------------------------
// Content builders

/// Measured mono metrics at [`theme::FONT_SIZE`], in points: advance per char,
/// the line grid, and the natural (ascender−descender) line for vertical
/// centering.
#[derive(Debug, Clone, Copy)]
struct CellFont {
    adv: f64,
    line_h: f64,
    natural: f64,
    dpi: f64,
}

impl Default for CellFont {
    fn default() -> Self {
        CellFont {
            adv: theme::FONT_SIZE * theme::MONO_ADV,
            line_h: theme::FONT_SIZE * theme::LINE_H,
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
    /// How much of a header the chrome buttons eat: the close box plus every
    /// side-effect button the kind declares, gaps included. The title
    /// truncates against this rather than against a guessed constant — a
    /// message carrying both archive and delete needs half again what one
    /// button did.
    fn head_btns_w(&self, kind: &Kind) -> f64 {
        // Mirrors the walk in `draw_panel_full`: the close box, then each
        // button as its label plus padding, each preceded by a gap.
        theme::BTN_H
            + 4.0
            + ui::head_btns(kind)
                .iter()
                .map(|(label, _)| self.label_w(label.chars().count()) + 12.0 + 4.0)
                .sum::<f64>()
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
    /// What each hosted widget was built and last seeded for — its
    /// template and its kind. A panel replaced in place keeps its id and
    /// so its widget; this is how the shell notices the kind under that
    /// widget changed (a reply retargeted to a forward) and seeds it again.
    hosted_for: HashMap<PanelId, (LiveId, Kind)>,
    /// A freshly created compose wants its body focused — on the next
    /// event tick, not during the draw that created it.
    #[rust]
    pending_focus: Option<PanelId>,

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
    /// Soft-keyboard bottom occlusion (android), in points.
    #[rust]
    kb_h: f64,
    #[rust]
    touch: TouchNav,
    /// A row mid-swipe, and the curtain over it (CR-005). Outlives the finger:
    /// a committed swipe keeps animating until the curtain has covered the row.
    #[rust]
    row_swipe: Option<RowSwipe>,
    /// The drop-preview insertion bar while a panel is dragged, strip coords.
    #[rust]
    drag_hint: Option<core::Rect>,
    /// Safe-area insets (cutouts, rounded corners): top, right, bottom, left.
    #[rust]
    insets: (f64, f64, f64, f64),
    /// What the macOS menu bar currently shows: `(workspace, is_current)`
    /// per roster entry, and the problems' lines. Menus rebuild only when
    /// this changes.
    #[rust]
    menu_sig: (Vec<(usize, bool)>, Vec<String>),
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
    /// Frames drawn — the headless manual pump's cadence counter.
    #[rust]
    #[allow(dead_code)]
    frame: u64,
    /// e2e ticks — the cadence counter for the headless device-sync pass.
    #[rust]
    repl_ticks: u64,
    /// android: after a field-to-field focus move, the blurring TextInput
    /// hides the soft keyboard and the next field's one draw-time show can
    /// lose the race against the hide animation. The guard re-issues it —
    /// twice if it must (areas re-issue on redraw, so it checks the live
    /// focus, never a stored handle).
    #[rust]
    ime_guard_tries: u8,
    #[rust]
    ime_guard_timer: Timer,
    #[rust]
    state: Option<Box<State>>,
    /// A panels-library mount (CR-006): booted by the canvas from a scene's
    /// node, replays its steps, owns nothing outside its pass. The
    /// window's own stage boots from argv and owns the menu bar.
    #[rust]
    mount: bool,
    /// Whether this stage may touch the window's IME and key focus —
    /// always for the primary, for a mount only while the canvas has
    /// entered it.
    #[rust]
    active: bool,
    /// A mount's replay reached its shot.
    #[rust]
    arrived: bool,
    /// A mount's own draw list and the canvas's, for scoped redraws.
    #[rust]
    lists: Option<(DrawListId, DrawListId)>,
    /// The manual pump's next due time on a mount's virtual clock.
    #[rust]
    pump_due: f64,
    /// A mount's last step has not been drawn yet: the next step waits for
    /// the draw, which the canvas schedules within its frame budget.
    #[rust]
    stale_hits: bool,
    /// A panel node (CR-006): the one panel this stage draws, at the whole
    /// viewport, instead of the workspace.
    #[rust]
    solo: Option<PanelId>,
    /// The panels library is up over this stage: it draws nothing and
    /// hears no input, while its store, timers and script keep running.
    #[rust]
    suspended: bool,
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
const MENU_LIBRARY: u64 = 0x5753_0600;

/// The Dev menu's intents, raised by the stage (a menu item, a chord) for
/// the app root to act on: the library is the stage's sibling, not its
/// child.
#[derive(Debug, Clone)]
pub enum DevAction {
    /// Show the panels library over the workspace, or put it away.
    ToggleLibrary,
}

/// The problems menu: every item goes to the problems panel.
const MENU_PROBLEMS: u64 = 0x5753_0700;

/// A draft's subject for an action label, `(no subject)` when it has none.
fn draft_subject(store: &Store, panel: i64) -> String {
    mail::draft(store, panel)
        .map(|d| d.subject)
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "(no subject)".into())
}

/// The menu bar of a window opened on the library: the Dev menu alone,
/// until the workspace boots and the stage builds the full set.
fn dev_menu(cx: &mut Cx) {
    if !cfg!(target_os = "macos") {
        return;
    }
    cx.update_macos_menu(MacosMenu::Main {
        items: vec![
            // The app menu is AppKit's; replacing the bar without it would
            // take Quit — and ⌘Q — with it.
            MacosMenu::Sub {
                name: "superapp".into(),
                items: vec![MacosMenu::Item {
                    command: live_id!(quit),
                    key: KeyCode::KeyQ,
                    shift: false,
                    enabled: true,
                    name: "Quit superapp".into(),
                }],
            },
            MacosMenu::Sub {
                name: "Dev".into(),
                items: vec![MacosMenu::Item {
                    command: LiveId(MENU_LIBRARY),
                    key: KeyCode::Unknown,
                    shift: true,
                    enabled: true,
                    name: "Panels Library — ⇧⌘L".into(),
                }],
            },
        ],
    });
}

/// The overlays are hosted like panels, so they need keys in the same map.
/// Panel ids are workspace-tagged (`k << 32`) and allocated upward, so the
/// top of the range is free forever.
const OVERLAY_PID_R: PanelId = u64::MAX;
const OVERLAY_PID_L: PanelId = u64::MAX - 1;

/// How a `key` chord executes: as a synthesized key event, as text (plain
/// letters reach panels the same way real typing does), or as a bare
/// modifier tap — a down/up pair (`key cmd 2` double-taps cmd).
enum ChordExec {
    Ev(KeyEvent),
    Text(String),
    Tap(KeyCode),
}

/// The alphabet, both ways — read by the chord parser and by the
/// accelerator resolver, so a key's name and its code cannot drift.
const LETTERS: [(char, KeyCode); 26] = [
    ('a', KeyCode::KeyA),
    ('b', KeyCode::KeyB),
    ('c', KeyCode::KeyC),
    ('d', KeyCode::KeyD),
    ('e', KeyCode::KeyE),
    ('f', KeyCode::KeyF),
    ('g', KeyCode::KeyG),
    ('h', KeyCode::KeyH),
    ('i', KeyCode::KeyI),
    ('j', KeyCode::KeyJ),
    ('k', KeyCode::KeyK),
    ('l', KeyCode::KeyL),
    ('m', KeyCode::KeyM),
    ('n', KeyCode::KeyN),
    ('o', KeyCode::KeyO),
    ('p', KeyCode::KeyP),
    ('q', KeyCode::KeyQ),
    ('r', KeyCode::KeyR),
    ('s', KeyCode::KeyS),
    ('t', KeyCode::KeyT),
    ('u', KeyCode::KeyU),
    ('v', KeyCode::KeyV),
    ('w', KeyCode::KeyW),
    ('x', KeyCode::KeyX),
    ('y', KeyCode::KeyY),
    ('z', KeyCode::KeyZ),
];

/// The key code a letter presses.
fn letter_key(c: char) -> Option<KeyCode> {
    let lower = c.to_ascii_lowercase();
    LETTERS.iter().find(|(l, _)| *l == lower).map(|(_, k)| *k)
}

/// The letter a key code types, if it is one.
fn key_char(k: KeyCode) -> Option<char> {
    LETTERS.iter().find(|(_, c)| *c == k).map(|(l, _)| *l)
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
        // Every letter parses, so a script can drive any accelerator.
        k => k
            .chars()
            .next()
            .filter(|_| k.chars().count() == 1)
            .and_then(letter_key),
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
        // makepad only routes keys to the system IME — which is what emits
        // TextInput events — while the IME is shown. On macOS letter keys
        // (j/k/r, "/", field typing) all arrive that way, so the IME stays on
        // whenever a panel has focus (mosaic's model: "typing only flows
        // after show_text_ime"). On android show_text_ime raises the
        // on-screen keyboard, which every retained panel's TextInputs now do
        // for themselves — so the shell only asks for the launcher's field.
        // Every focus transition passes through kick().
        let launcher = self
            .state
            .as_deref()
            .is_some_and(|s| s.overlay == Overlay::Launcher);
        // A retained panel's TextInputs own their key focus and (on android)
        // the soft keyboard — the shell must not fight them. But on macOS the
        // IME must stay on for hosted panels too: letters only arrive as
        // TextInput events while it is shown, and the letter grammar is made
        // of them.
        let hosted = self.hosted_focus();
        let want_ime = launcher
            || (!cfg!(target_os = "android")
                && self.state.as_deref().is_some_and(|s| s.ws.focus.is_some()));
        // A mount the canvas has not entered keeps its hands off the
        // window's IME and key focus: a hundred of them would otherwise
        // fight over one keyboard.
        if self.active && want_ime != self.ime_shown {
            self.ime_shown = want_ime;
            if want_ime {
                // With a hosted panel — or the launcher's field — the key
                // focus belongs to whichever TextInput holds it.
                if !hosted && !launcher {
                    cx.set_key_focus(self.area);
                }
                cx.show_text_ime_with_config(
                    self.area,
                    rect(0.0, 0.0, 0.0, 0.0),
                    TextInputConfig::default(),
                );
            } else if !hosted {
                // Also resets makepad's "user dismissed the keyboard" latch,
                // without which the next show request is silently ignored.
                cx.hide_text_ime();
            }
        }
        if let Some(state) = self.state.as_deref_mut() {
            // Every overlay change passes through here too: point the
            // chassis' presence spring at where the overlay now is.
            let up = state.overlay != Overlay::None;
            state.anim.overlay().retarget(if up { 1.0 } else { 0.0 });
            if !state.animating {
                state.last_frame = Some(Instant::now());
                state.animating = true;
            }
        }
        self.next_frame = cx.new_next_frame();
        self.redraw_scoped(cx);
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
        if !cfg!(target_os = "macos") || self.mount {
            return;
        }
        let Some(state) = self.state.as_deref() else {
            return;
        };
        let roster: Vec<(usize, bool)> = state
            .ws
            .roster()
            .into_iter()
            .map(|k| (k, k == state.ws.active))
            .collect();
        let problems: Vec<String> = state
            .problems()
            .iter()
            .map(|p| format!("{} — {}", p.label, p.line))
            .collect();
        let sig = (roster, problems);
        if sig == self.menu_sig {
            return;
        }
        self.menu_sig = sig.clone();
        let (roster, problems) = sig;
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
        for (k, current) in roster {
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
        items.push(MacosMenu::Sub {
            name: "Dev".into(),
            items: vec![MacosMenu::Item {
                command: LiveId(MENU_LIBRARY),
                key: KeyCode::Unknown,
                shift: true,
                enabled: true,
                name: "Panels Library — ⇧⌘L".into(),
            }],
        });
        // The problems, mirrored the way the workspaces are: a menu that
        // exists only while something stands, one item per problem, each
        // opening the panel. Plain text — AppKit draws these titles itself,
        // so the colour lives in the window.
        if !problems.is_empty() {
            items.push(MacosMenu::Sub {
                name: format!("! {}", crate::problems::count_line(problems.len())),
                items: problems
                    .into_iter()
                    .map(|line| MacosMenu::Item {
                        command: LiveId(MENU_PROBLEMS),
                        key: KeyCode::Unknown,
                        shift: false,
                        enabled: true,
                        name: line,
                    })
                    .collect(),
            });
        }
        cx.update_macos_menu(MacosMenu::Main { items });
    }

    /// Whether an HTML link — a text link, or a picture carrying one — lies
    /// under `p` in the hosted widget of `pid`.
    fn link_under(&self, cx: &Cx, pid: u64, p: DVec2) -> bool {
        let Some(w) = self.hosted.get(&pid) else { return false };
        let mut hand = false;
        w.find_widgets_from_point(cx, p, &mut |x| {
            hand |= x.as_html_link().borrow().is_some()
                || x.as_html_image().borrow().is_some_and(|i| i.is_link());
        });
        hand
    }

    fn hit_at(&self, p: DVec2) -> Option<&HitR> {
        self.hits.iter().rev().find(|h| h.rect.contains(p))
    }

    /// Executes at most one e2e step per timer tick; waits pace the script.
    fn e2e_tick(&mut self, cx: &mut Cx, dt_ms: f64) {
        // Device sync advances with the run: `e2e_tick` is the one place every
        // headless path drives, so a scripted `wait` moves a handoff exactly
        // the way it moves the mail engine. Every ~20 ticks keeps the bucket
        // round trips gentle while staying live within a `wait`.
        self.repl_ticks = self.repl_ticks.wrapping_add(1);
        if self.repl_ticks.is_multiple_of(20) {
            self.tick_repl(cx);
        }
        let Some(mut runner) = self.e2e.take() else {
            return;
        };
        // Screenshots are effects; a run needs the world to take them.
        let world = self.state.as_ref().map(|s| s.world.clone());
        if let Some(step) = runner.next_step(dt_ms) {
            match step {
                e2e::Step::Wait(_) => {}
                // A mount's last step is where it stops: the state on the
                // canvas. The runner goes with it — nothing else to do, and
                // nothing to keep asking for frames.
                e2e::Step::Shot(_) | e2e::Step::Quit if self.mount => {
                    // Only the last shot is this mount's own; the ones on
                    // the way are earlier nodes', and nothing to do.
                    if runner.idx >= runner.steps.len() {
                        self.arrived = true;
                        if runner.failures > 0 {
                            eprintln!(
                                "library: {}reached its shot with {} failed step(s)",
                                runner.tag, runner.failures
                            );
                        }
                        return;
                    }
                }
                // The fast path rasterizes nothing: a shot is logged, not
                // failed, so a green `--no-draw` run means what it says.
                e2e::Step::Shot(name) if config().no_draw => {
                    eprintln!("e2e: shot {name} (skipped: --no-draw)");
                }
                e2e::Step::Shot(name) => {
                    let path = runner.out.join(format!("{name}.png"));
                    match world
                        .as_ref()
                        .ok_or_else(|| "no world yet".to_string())
                        .and_then(|w| w.run(&crate::effect::Shot(&path)))
                    {
                        Ok(()) => eprintln!("e2e: shot {}", path.display()),
                        Err(e) => {
                            eprintln!("{}e2e: FAIL shot {name}: {e}", runner.tag);
                            runner.failures += 1;
                        }
                    }
                    #[cfg(any())]
                    {
                        eprintln!("{}e2e: FAIL shot {}: screenshots need macos", runner.tag, path.display());
                        runner.failures += 1;
                    }
                }
                e2e::Step::Click { label, fresh } => {
                    let needle = label.to_lowercase();
                    // Whole-label matches win over substrings. Without that,
                    // `click "archive"` can land on a mail *subject* — the
                    // seed has sixty "archive digest #NN" rows — and which
                    // one it finds depends on draw order, so the script
                    // silently tests something else. An exact name is an
                    // exact target.
                    let exact = |h: &&HitR| h.label.eq_ignore_ascii_case(&label);
                    let loose = |h: &&HitR| h.label.to_lowercase().contains(&needle);
                    let named = |h: &&HitR| !matches!(h.act, Act::Focus(_));
                    let pick = |f: &dyn Fn(&&HitR) -> bool| {
                        self.hits
                            .iter()
                            .rev()
                            .find(|h| named(h) && f(h))
                            .or_else(|| self.hits.iter().rev().find(f))
                    };
                    let hit = pick(&exact)
                        .or_else(|| pick(&loose))
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
                            eprintln!("{}e2e: FAIL click {label:?}: no matching element", runner.tag);
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
                                    let mut up = down;
                                    up.modifiers = KeyModifiers::default();
                                    self.handle_key_down(cx, &down);
                                    self.handle_key_up(cx, &up);
                                }
                            }
                        }
                    }
                    None => {
                        eprintln!("{}e2e: FAIL key {chord:?}: cannot parse chord", runner.tag);
                        runner.failures += 1;
                    }
                },
                e2e::Step::Type(s) => {
                    eprintln!("e2e: type {s:?}");
                    self.handle_text(cx, &s);
                }
                e2e::Step::Drag { label, dx, dy } => {
                    let needle = label.to_lowercase();
                    // From the left edge, so a horizontal drag sweeps the
                    // run rather than starting halfway through it.
                    let c = self
                        .hits
                        .iter()
                        .rev()
                        .find(|h| h.label.to_lowercase().contains(&needle))
                        .map(|h| dvec2(h.rect.pos.x + 2.0, h.rect.pos.y + h.rect.size.y / 2.0));
                    match c {
                        Some(c) => {
                            eprintln!("e2e: drag {label:?} by ({dx}, {dy})");
                            self.synth_drag(cx, c, dvec2(c.x + dx, c.y + dy));
                        }
                        None => {
                            eprintln!("{}e2e: FAIL drag {label:?}: no matching element", runner.tag);
                            runner.failures += 1;
                        }
                    }
                }
                e2e::Step::SelectAll(label) => {
                    let needle = label.to_lowercase();
                    let hit = self
                        .hits
                        .iter()
                        .rev()
                        .find(|h| h.label.to_lowercase().contains(&needle))
                        .map(|h| (h.act.clone(), h.rect));
                    match hit {
                        Some((Act::Pointer(pid), r)) => {
                            eprintln!("e2e: selectall {label:?}");
                            // Just inside the run's top-left corner: the
                            // registered rect is unclipped, and the middle
                            // of a tall letter lies below the viewport,
                            // where nothing is hit.
                            let p = r.pos + dvec2(4.0, 4.0);
                            // Found while the tree is borrowed; acted on
                            // after, or the widget would be borrowed twice.
                            let mut runs = Vec::new();
                            if let Some(w) = self.hosted.get(&pid).cloned() {
                                w.find_widgets_from_point(cx, p, &mut |x| {
                                    if x.as_html().borrow().is_some() {
                                        runs.push(x.clone());
                                    }
                                });
                            }
                            if runs.is_empty() {
                                eprintln!("e2e: FAIL selectall {label:?}: no run under the point");
                                runner.failures += 1;
                            }
                            for run in runs {
                                run.selection_select_all();
                            }
                            cx.redraw_all();
                        }
                        _ => {
                            eprintln!("e2e: FAIL selectall {label:?}: no hosted run");
                            runner.failures += 1;
                        }
                    }
                }
                e2e::Step::Mouse { label } => {
                    // The physical path: the pair enters the stage's own
                    // handler, so the forwarding to hosted widgets, the
                    // hit lookup and the key-focus rule all run as they do
                    // for a real click. `click` resolves the action
                    // directly and proves less.
                    let needle = label.to_lowercase();
                    let r = self
                        .hits
                        .iter()
                        .rev()
                        .find(|h| h.label.eq_ignore_ascii_case(&label))
                        .or_else(|| {
                            self.hits
                                .iter()
                                .rev()
                                .find(|h| h.label.to_lowercase().contains(&needle))
                        })
                        .map(|h| h.rect);
                    match r {
                        Some(r) => {
                            eprintln!("e2e: mouse {label:?}");
                            let p = r.pos + r.size / 2.0;
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
                            let mut scope = Scope::empty();
                            <Self as Widget>::handle_event(self, cx, &down, &mut scope);
                            <Self as Widget>::handle_event(self, cx, &up, &mut scope);
                        }
                        None => {
                            eprintln!("e2e: FAIL mouse {label:?}: no matching element");
                            runner.failures += 1;
                        }
                    }
                }
                e2e::Step::Swipe { label, dx, dy, hold } => {
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
                            // A whole swipe runs inside one tick, so nothing
                            // is ever drawn mid-gesture: `hold` leaves the
                            // finger down long enough to photograph it.
                            if !hold {
                                self.touch_stop(cx, 1, dvec2(c.x + dx, c.y + dy));
                            }
                        }
                        None => {
                            eprintln!("{}e2e: FAIL swipe {label:?}: no matching element", runner.tag);
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
                    let held = match self.touch.mode {
                        TouchMode::Drag { uid, .. } | TouchMode::RowSwipe { uid } => Some(uid),
                        _ => None,
                    };
                    if let Some(uid) = held {
                        let p = self
                            .touch
                            .pts
                            .get(&uid)
                            .map(|&(_, p)| p)
                            .unwrap_or(self.origin);
                        eprintln!("e2e: drop");
                        self.touch_stop(cx, uid, p);
                    } else {
                        eprintln!("{}e2e: FAIL drop: no gesture is being held", runner.tag);
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
                                eprintln!("{}e2e: FAIL holdmove {label:?}: header did not grab", runner.tag);
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
                            eprintln!("{}e2e: FAIL holdmove {label:?}: no matching panel", runner.tag);
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
                    // Drop the runner rather than restoring it. Under a
                    // headless build the frame pump keeps asking for
                    // frames while a run is live, and a finished run that
                    // stays live spins the software rasterizer flat out
                    // until the whole --draws budget is gone.
                    return;
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
        // The launcher owns the keyboard while it is up: arrows pick the
        // hit, enter goes, esc closes. Everything else — the query's own
        // editing, caret, selection — belongs to its `SField` now, so it is
        // forwarded rather than re-implemented (CR-002 F).
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
                // The hits are a ring: past the last is the first. The
                // draw still clamps, against a list that shrank under
                // the query.
                KeyCode::ArrowDown => {
                    let n = state.launcher.hits.len();
                    state.launcher.sel = if n == 0 { 0 } else { (state.launcher.sel + 1) % n };
                    self.kick(cx);
                }
                KeyCode::ArrowUp => {
                    let n = state.launcher.hits.len();
                    state.launcher.sel = if n == 0 { 0 } else { (state.launcher.sel + n - 1) % n };
                    self.kick(cx);
                }
                _ => {
                    self.forward_to_overlay(cx, &Event::KeyDown(*k));
                    self.kick(cx);
                }
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
            // Arrows only — the vim walk went with CR-003, which also frees
            // h/j/k/l for panel accelerators.
            let dir = match k.key_code {
                KeyCode::ArrowLeft => Some(Dir::Left),
                KeyCode::ArrowRight => Some(Dir::Right),
                KeyCode::ArrowUp => Some(Dir::Up),
                KeyCode::ArrowDown => Some(Dir::Down),
                _ => None,
            };
            if let Some(dir) = dir {
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
            if k.key_code == KeyCode::KeyL && k.modifiers.shift {
                // The Dev menu's chord: the panels library, over the workspace.
                cx.action(DevAction::ToggleLibrary);
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
            // Past the reserved set the chord belongs to the focused panel
            // (CR-003). Chrome buttons resolve here, because the chrome is
            // the shell's; links resolve inside the panel widget, which
            // owns them — so an unclaimed chord falls through to it rather
            // than dying here.
            let accel = state.ws.focus.zip(key_char(k.key_code)).and_then(|(f, c)| {
                let kind = state.ws.panels.get(&f).map(|p| p.kind.clone())?;
                let act = ui::head_btns(&kind)
                    .iter()
                    .find(|(_, a)| ui::btn_accel(*a) == Some(c))
                    .map(|(_, a)| *a)?;
                Some((f, act))
            });
            if let Some((f, act)) = accel {
                // The same door a click uses, so undo and toasts are shared.
                self.resolve_click(cx, Act::Btn(f, act), false);
                return;
            }
            // Nothing on this panel wanted it. A panel that drives a preview
            // now **borrows** its preview's keys (CR-005): the pair reads as
            // one thing, so archive, delete and reply work from the list
            // without first walking focus into the mail. The borrowed mark is
            // never drawn here — it stays on the message's own chrome, one
            // column over and in plain sight.
            if let Some(child) = self.lender(cx) {
                let kind = self
                    .state
                    .as_deref()
                    .and_then(|s| s.ws.panel(child).map(|p| p.kind.clone()));
                let lent = kind.zip(key_char(k.key_code)).and_then(|(kind, c)| {
                    ui::head_btns(&kind)
                        .iter()
                        .find(|(_, a)| ui::btn_accel(*a) == Some(c))
                        .map(|(_, a)| *a)
                });
                if let Some(act) = lent {
                    self.resolve_click(cx, Act::Btn(child, act), false);
                    return;
                }
                // Its *links* answer inside the widget, so the chord has to
                // reach it. Only chords: the message scrolls its body on bare
                // arrows, which are the cursor walk out here.
                self.forward_to_panel(cx, child, &Event::KeyDown(*k));
            }
            // An unclaimed chord — cmd+enter included, which the inbox reads
            // as "open un-joined" — falls through to the panel below.
        }

        // A retained panel owns plain keys (its widgets gate on key focus)
        // — cmd chords and overlays were already handled above.
        if hosted {
            self.forward_to_focused(cx, &Event::KeyDown(*k));
            self.kick(cx);
        }
    }

    /// The panel the focused one may borrow accelerators from: its live
    /// preview child, if it drives one.
    ///
    /// The fifth letter rule's guard lives here — a borrowed chord stands
    /// down while the driver's own text field holds the keyboard, so `cmd+a`
    /// stays select-all in a live filter rather than silently archiving. The
    /// widget is asked directly: focus parks on `Area::Empty` after `esc` and
    /// on the stage's own area otherwise, and comparing against both here
    /// would re-encode two rules that already live elsewhere.
    fn lender(&mut self, cx: &mut Cx) -> Option<PanelId> {
        let state = self.state.as_deref()?;
        let f = state.ws.focus?;
        let kind = state.ws.panel(f).map(|p| p.kind.clone())?;
        ui::preview_kind(&kind)?;
        let child = state.ws.joined_child(f)?;
        if !state.ws.panels.contains_key(&child) {
            return None;
        }
        let editing = self
            .hosted
            .get(&f)
            .cloned()
            .is_some_and(|w| w.as_inbox_panel().filter_focused(cx));
        (!editing).then_some(child)
    }

    /// Only the launcher trigger cares about key releases: a clean second
    /// cmd tap fires here.
    fn handle_key_up(&mut self, cx: &mut Cx, k: &KeyEvent) {
        if k.key_code == KeyCode::Logo && self.cmd_tap.release(k.time) {
            self.toggle_launcher(cx);
        }
        if self.hosted_focus() {
            self.forward_to_focused(cx, &Event::KeyUp(*k));
        }
    }

    /// Double-cmd: raise the launcher, or put it away if it is already up.
    fn toggle_launcher(&mut self, cx: &mut Cx) {
        let opening = {
            let Some(state) = self.state.as_deref_mut() else {
                return;
            };
            if state.overlay == Overlay::Launcher {
                state.overlay = Overlay::None;
                false
            } else {
                state.launcher = LauncherUi::default();
                state.overlay = Overlay::Launcher;
                true
            }
        };
        if opening {
            self.refocus_launcher();
        }
        self.kick(cx);
    }

    /// Raise the launcher idempotently — tapping its own field (or the menu
    /// item twice) must not reset a typed query.
    fn open_launcher(&mut self, cx: &mut Cx) {
        let opening = {
            let Some(state) = self.state.as_deref_mut() else {
                return;
            };
            let opening = state.overlay != Overlay::Launcher;
            if opening {
                state.launcher = LauncherUi::default();
                state.overlay = Overlay::Launcher;
            }
            opening
        };
        if opening {
            self.refocus_launcher();
        }
        self.kick(cx);
    }

    /// A launcher summoned back while its close fade still runs finds its
    /// widget alive, so the draw that would seed the field and take the
    /// keyboard for a fresh one never fires. Ask for it here instead.
    fn refocus_launcher(&mut self) {
        if self.hosted.contains_key(&OVERLAY_PID_L) {
            self.pending_focus = Some(OVERLAY_PID_L);
        }
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
        let step = state.history.undo(&state.world);
        match step {
            Some(step) => {
                let label = state.land(step);
                state.sync();
                if state.ws.active != was {
                    let cam = state.ws.camera_x;
                    state.anim.camera().jump_to(cam);
                }
                state.pump.kick(); // reverted intent pushes to the server too
                state.toast(format!("undid — {label}"), false);
            }
            None => state.toast("nothing to undo", false),
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
        let step = state.history.redo(&state.world);
        match step {
            Some(step) => {
                let label = state.land(step);
                state.sync();
                if state.ws.active != was {
                    let cam = state.ws.camera_x;
                    state.anim.camera().jump_to(cam);
                }
                state.pump.kick();
                state.toast(format!("redid — {label}"), false);
            }
            None => state.toast("nothing to redo", false),
        }
        self.update_menu(cx);
        self.kick(cx);
    }



    /// Notices foreign commits (sync workers, the sender): re-runs stale
    /// queries, surfaces fresh send failures, redraws. Ridden by the
    /// worker signal and by a coarse fallback timer — a lost wake must
    /// never strand the UI on cached rows.
    fn poll_store(&mut self, cx: &mut Cx) {
        let changed = match self.state.as_deref_mut() {
            Some(state) => {
                if state.store.poll_external() {
                    state.announce_problems();
                    true
                } else {
                    false
                }
            }
            None => return,
        };
        if changed {
            self.redraw_scoped(cx);
            // The menu bar mirrors the problems; a pass that changed them
            // rebuilds it (a signature check keeps it cheap).
            self.update_menu(cx);
        }
        self.tick_repl(cx);
    }

    /// Runs (production: reads) one device-sync pass and reacts: on a role
    /// change, announce it, reload the layout (an install or materialize may
    /// have replaced rows), and redraw so the locked screen appears or clears.
    /// Called on every worker signal and, headless, from the frame loop.
    fn tick_repl(&mut self, cx: &mut Cx) {
        let Some(state) = self.state.as_deref_mut() else {
            return;
        };
        if state.repl.is_none() {
            return;
        }
        let role_changed = state.repl_poll();
        if role_changed {
            let line = state.repl_status.role.line();
            let err = matches!(state.repl_status.role, crate::repl::Role::Stranded { .. });
            state.toast(line, err);
            if let Ok(Some(snap)) = state.store.load_wm() {
                state.ws = Wm::restore(snap);
                state.last_saved = Some(state.ws.snapshot());
            }
            state.sync();
            self.update_menu(cx);
        }
        self.reseed_composes(cx);
        cx.redraw_all();
    }

    /// Re-seeds compose panels whose `draft` row has drifted from what their
    /// retained widget shows. A compose seeds its fields from the row exactly
    /// once, when its widget is built ([`Self::draw_hosted`]); a peer's
    /// materialized edit — or the canonical draft adopted when this device
    /// takes over or recovers — then rewrites that row underneath, but the live
    /// TextInputs keep their own buffers, so the panel would otherwise show a
    /// stale draft that no reopen can dislodge.
    ///
    /// The one thing never overwritten is an *active* edit on the holder: a
    /// compose with key focus may be mid-typing, and `save_draft` can lag the
    /// widget, so a writable device leaves a focused field alone. A read-only
    /// device cannot type at all — and its compose auto-focuses its body on
    /// open (`pending_focus`) yet sits behind the lock showing a peer's stale
    /// draft — so there the re-seed always runs. Idempotent: rows equal to the
    /// widget are left alone (single-writer, CR-005).
    fn reseed_composes(&mut self, cx: &mut Cx) {
        let Some(state) = self.state.as_deref() else {
            return;
        };
        if state.repl.is_none() {
            return;
        }
        if state.store.is_writable() {
            let focus = cx.key_focus();
            if focus != Area::Empty && focus != self.area {
                return;
            }
        }
        // Only a row that is this panel's seed's: one left by a kind the
        // panel had before says nothing about what it shows now.
        let drafts: Vec<(PanelId, mail::Draft)> = state
            .ws
            .wss
            .iter()
            .flat_map(|w| w.panels.iter())
            .filter_map(|(id, p)| match p.kind {
                Kind::Compose { seed } => {
                    mail::draft_for(&state.store, *id as i64, seed).map(|d| (*id, d))
                }
                _ => None,
            })
            .collect();
        for (id, want) in &drafts {
            if let Some(w) = self.hosted.get(id) {
                let c = w.as_compose_panel();
                let cur = c.values(cx);
                if cur.to.as_str() != want.to.trim()
                    || cur.subject != want.subject
                    || cur.body != want.body
                {
                    c.prefill(cx, &want.to, &want.subject, &want.body);
                }
            }
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
        // Deliver: a file beside the store, and the clipboard. Both are
        // effects, so a denied world refuses them loudly instead of
        // silently writing to a developer's machine.
        let mut where_to = String::new();
        if let Some(dir) = state.db_path.as_ref().and_then(|p| p.parent()) {
            let path = dir.join("panel-context.md");
            if state
                .world
                .run(&crate::effect::WriteFile {
                    path: &path,
                    bytes: md.as_bytes(),
                })
                .is_ok()
            {
                where_to = path.to_string_lossy().into_owned();
            }
        }
        state.world.try_run(&crate::effect::Clip {
            text: &md,
            what: "panel context",
        });
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
        self.go(cx, hit.go);
    }

    /// Reaches a root panel the way the launcher does: focus it where it is
    /// open, open it fresh otherwise. The problems mark and the menu bar's
    /// problems items come through here.
    fn go_to(&mut self, cx: &mut Cx, kind: Kind) {
        let Some(state) = self.state.as_deref() else {
            return;
        };
        let go = launcher::locate(&state.ws, &kind);
        self.go(cx, go);
    }

    fn go(&mut self, cx: &mut Cx, go: launcher::Go) {
        let Some(state) = self.state.as_deref_mut() else {
            return;
        };
        state.overlay = Overlay::None;
        match go {
            launcher::Go::Focus(pid) => {
                let was = state.ws.active;
                if let Some(k) = state.ws.focus_panel(pid) {
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
                // Opening a mail reads its whole thread (CR-007): every
                // unread mail of it is marked, one intent each, and the
                // panel opens with exactly those unfolded.
                let marks: Vec<core::MailId> =
                    mid.map(|id| mail::thread_unread(&state.store, id)).unwrap_or_default();
                let open: BTreeSet<core::MailId> =
                    marks.iter().copied().chain(mid).collect();
                let marks_tx = marks.clone();
                state.wish_ahead(&kind);
                state.act(
                    "open",
                    label,
                    None,
                    move |ws| {
                        ws.open(kind, None, false);
                    },
                    move |tx| {
                        for m in &marks_tx {
                            mail::mark_read_tx(tx, *m)?;
                        }
                        Ok(())
                    },
                    marks
                        .iter()
                        .map(|m| Box::new(mail::MarkRead { mail: *m }) as Box<dyn crate::history::Intent>)
                        .collect(),
                );
                if let Some(id) = mid {
                    state.seed_expansion(id, &open);
                }
                state.sync();
            }
        }
        self.update_menu(cx);
        self.kick(cx);
    }

    fn handle_ime_state(&mut self, cx: &mut Cx, fs: &FullTextState) {
        let Some(state) = self.state.as_deref() else {
            return;
        };
        // The launcher's query is an `SField`, and a TextInput owns the
        // whole android full-state protocol itself — hand it the event
        // untouched rather than mirroring it here.
        if state.overlay == Overlay::Launcher {
            let ev = Event::TextInput(TextInputEvent {
                input: String::new(),
                full_state_sync: Some(fs.clone()),
                ..Default::default()
            });
            self.forward_to_overlay(cx, &ev);
            self.kick(cx);
        }
    }

    /// Character input: the focused panel's typing, or the launcher's query.
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
        if hosted && state.overlay == Overlay::None {
            let ev = Event::TextInput(TextInputEvent {
                input: input.to_string(),
                ..Default::default()
            });
            self.forward_to_focused(cx, &ev);
            self.kick(cx);
            return;
        }
        // The launcher's query is an `SField` now: hand it the character
        // and let the widget's own editing take it (the shell hears the
        // result back as `OverlayAction::Query`).
        if state.overlay == Overlay::Launcher {
            let ev = Event::TextInput(TextInputEvent {
                input: input.to_string(),
                ..Default::default()
            });
            self.forward_to_overlay(cx, &ev);
            self.kick(cx);
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
                let step = state.history.travel(&state.world, id);
                if let Some(step) = step {
                    let label = state.land(step);
                    state.sync();
                    if state.ws.active != was {
                        let cam = state.ws.camera_x;
                        state.anim.camera().jump_to(cam);
                    }
                    state.pump.kick();
                    state.toast(format!("history — {label}"), false);
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
            Act::Acquire => {
                // The locked screen's button: ask the worker to take the
                // lease. Whether this is a plain acquire (from a free lease) or
                // an override (from a live holder) is the worker's to decide.
                if state.repl.is_some() {
                    let overriding = matches!(
                        state.repl_status.role,
                        crate::repl::Role::Follower { .. } | crate::repl::Role::Stranded { .. }
                    );
                    state.toast(
                        if overriding {
                            "taking over — the other device may hold unpublished work"
                        } else {
                            "acquiring the lease…"
                        },
                        false,
                    );
                    state.repl_acquire();
                }
                self.tick_repl(cx);
                self.kick(cx);
                return;
            }
            Act::Problems => {
                self.go_to(cx, Kind::Problems);
                return;
            }
            Act::Noop => {
                return; // the locked backdrop absorbs the click
            }
            Act::Pointer(pid) => {
                // The widget under it got the real event via forwarding;
                // the shell's share is panel focus. A no-op focus must not
                // redraw the world: a mid-gesture rebuild reissues areas
                // and breaks the widget's own down/up pairing.
                if state.ws.focus != Some(pid) {
                    state.ws.focus = Some(pid);
                    self.sync(cx);
                }
                return;
            }
            Act::WidgetOp(pid, op) => {
                match op {
                    WidgetOp::AddAccount => {
                        if let Some(w) = self.hosted.get(&pid) {
                            if let Some(mut ap) = w.as_add_account_panel().borrow_mut() {
                                let (email, pass, imap, smtp) = ap.form_values(cx);
                                cx.action(crate::panels::PanelAction::AddAccount {
                                    pid, email, pass, imap, smtp,
                                });
                            }
                        }
                    }
                    WidgetOp::RemoveAccount(id) => {
                        cx.action(crate::panels::PanelAction::RemoveAccount(id));
                    }
                    WidgetOp::OpenMail(id) => {
                        // A click inside a panel focuses it, as anywhere
                        // else. The preview below keeps *whoever* holds
                        // focus — right for the walk, which must not snap
                        // back — so the list has to take it here first.
                        if state.ws.focus != Some(pid) {
                            state.ws.focus = Some(pid);
                        }
                        // The cursor follows what you clicked, so the wash
                        // marks the mail on screen and the arrows carry on
                        // from there (panel-internal: the inbox listens).
                        cx.action(crate::panels::PanelAction::SelectMail { pid, id });
                        // Clicking a row is the same move as walking onto it:
                        // it previews, and the list keeps the keyboard.
                        // `enter` is what *goes*. Cmd+click still means what
                        // it means everywhere — a fresh, un-joined panel.
                        if alt {
                            self.resolve_click(cx, Act::Open(pid, Kind::Message { id }), true);
                        } else {
                            self.resolve_click(cx, Act::Preview(pid, Kind::Message { id }), false);
                        }
                    }
                    WidgetOp::Suggest(i) => {
                        // The pick splices into the field and keeps its
                        // focus; the shell's share is a redraw. Which
                        // field is the panel's kind to say.
                        if let Some(w) = self.hosted.get(&pid) {
                            match state.ws.panels.get(&pid).map(|p| &p.kind) {
                                Some(Kind::Compose { .. }) => {
                                    w.as_compose_panel().pick(cx, pid, i);
                                }
                                Some(Kind::Effects) => w.as_effects_panel().pick(cx, i),
                                _ => w.as_inbox_panel().pick(cx, i),
                            }
                        }
                        self.kick(cx);
                    }
                    WidgetOp::SyncAccount(id) => {
                        cx.action(crate::panels::PanelAction::SyncAccount(id));
                    }
                    WidgetOp::RetrySend(id) => {
                        cx.action(crate::panels::PanelAction::RetrySend(id));
                    }
                    WidgetOp::ReopenSend(id) => {
                        let seed = state
                            .problems()
                            .iter()
                            .find_map(|p| match &p.source {
                                crate::problems::Source::Send { outbox, seed, .. } if *outbox == id => {
                                    Some(*seed)
                                }
                                _ => None,
                            })
                            .unwrap_or(crate::core::Seed::Blank);
                        cx.action(crate::panels::PanelAction::ReopenSend {
                            pid,
                            outbox: id,
                            seed,
                            fresh: alt,
                        });
                    }
                    WidgetOp::ToggleMail(id) => self.toggle_msg(cx, pid, id, false),
                    WidgetOp::ToggleQuote(id) => self.toggle_msg(cx, pid, id, true),
                    WidgetOp::OpenJob(id) => {
                        // The inbox row's move, over the other table: the
                        // list takes focus, the cursor follows the click,
                        // and the job opens joined without stealing it back.
                        if state.ws.focus != Some(pid) {
                            state.ws.focus = Some(pid);
                        }
                        cx.action(crate::panels::PanelAction::SelectJob { pid, id });
                        if alt {
                            self.resolve_click(cx, Act::Open(pid, Kind::Job { id }), true);
                        } else {
                            self.resolve_click(cx, Act::Preview(pid, Kind::Job { id }), false);
                        }
                    }
                }
                return;
            }
            _ => {}
        }
        match act {
            Act::Focus(pid) => {
                if state.ws.focus != Some(pid) {
                    state.ws.focus = Some(pid);
                    self.sync(cx);
                }
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
                // Opening a mail reads its whole thread (CR-007): every
                // unread mail of it is marked, one intent each, and the
                // panel opens with exactly those unfolded.
                let marks: Vec<core::MailId> =
                    mid.map(|id| mail::thread_unread(&state.store, id)).unwrap_or_default();
                let open: BTreeSet<core::MailId> =
                    marks.iter().copied().chain(mid).collect();
                let marks_tx = marks.clone();
                state.wish_ahead(&kind);
                state.act(
                    "open",
                    label,
                    None,
                    move |ws| {
                        ws.follow_open(pid, kind, alt);
                    },
                    move |tx| {
                        for m in &marks_tx {
                            mail::mark_read_tx(tx, *m)?;
                        }
                        Ok(())
                    },
                    marks
                        .iter()
                        .map(|m| Box::new(mail::MarkRead { mail: *m }) as Box<dyn crate::history::Intent>)
                        .collect(),
                );
                if let Some(id) = mid {
                    state.seed_expansion(id, &open);
                }
                self.sync(cx);
            }
            Act::Replace(pid, kind) => {
                // Replacing with another mail is the same "read" walk as a
                // preview — it coalesces per panel.
                let mid = if let Kind::Message { id } = kind { Some(id) } else { None };
                // Opening a mail reads its whole thread (CR-007): every
                // unread mail of it is marked, one intent each, and the
                // panel opens with exactly those unfolded.
                let marks: Vec<core::MailId> =
                    mid.map(|id| mail::thread_unread(&state.store, id)).unwrap_or_default();
                let open: BTreeSet<core::MailId> =
                    marks.iter().copied().chain(mid).collect();
                let marks_tx = marks.clone();
                state.wish_ahead(&kind);
                let (akind, entity, label) = match mid {
                    Some(_) => (
                        READ,
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
                    move |tx| {
                        for m in &marks_tx {
                            mail::mark_read_tx(tx, *m)?;
                        }
                        Ok(())
                    },
                    marks
                        .iter()
                        .map(|m| Box::new(mail::MarkRead { mail: *m }) as Box<dyn crate::history::Intent>)
                        .collect(),
                );
                if let Some(id) = mid {
                    state.seed_expansion(id, &open);
                }
                self.sync(cx);
            }
            Act::Preview(pid, kind) => {
                // The cursor walk's own open (CR-005). Same door as a solid
                // link — join semantics, mark read, undoable — minus the one
                // thing that would end the walk: it never takes focus. So it
                // is a "read", coalescing per driver panel.
                let label = format!("read “{}”", state.panel_title(&kind));
                let (vp, opts) = (state.vp(), state.opts());
                // Reading a mail marks its thread; reading anything else
                // establishes nothing — a job is a record, and looking at
                // one leaves the world exactly as it was.
                let mid = if let Kind::Message { id } = kind { Some(id) } else { None };
                let marks: Vec<core::MailId> =
                    mid.map(|id| mail::thread_unread(&state.store, id)).unwrap_or_default();
                let open: BTreeSet<core::MailId> = marks.iter().copied().chain(mid).collect();
                let marks_tx = marks.clone();
                state.wish_ahead(&kind);
                state.act(
                    READ,
                    label,
                    Some(format!("panel:{pid}")),
                    move |ws| {
                        // Whoever holds focus keeps it — the driver normally,
                        // but not necessarily: this must leave focus exactly
                        // as it found it, or a walk that ends in cmd+→ would
                        // be snapped back to the list.
                        let held = ws.focus;
                        let child = ws.follow_open(pid, kind, false);
                        // follow_open focused the child; the child still has
                        // to be its column's shown tab either way, which
                        // normalize only does for the focused panel.
                        ws.activate(child);
                        // The exception: where the pair cannot share the
                        // screen — a phone grid, each panel the whole of it —
                        // a preview nobody can see would read as nothing
                        // having happened, so there the open simply goes.
                        if ws.fit_together(pid, child, vp, opts) {
                            ws.focus = held;
                        }
                    },
                    move |tx| {
                        for m in &marks_tx {
                            mail::mark_read_tx(tx, *m)?;
                        }
                        Ok(())
                    },
                    // Same claim on the world an ordinary open makes: the
                    // thread is read now, and undo un-reads what it read.
                    marks
                        .iter()
                        .map(|m| Box::new(mail::MarkRead { mail: *m }) as Box<dyn crate::history::Intent>)
                        .collect(),
                );
                if let Some(id) = mid {
                    state.seed_expansion(id, &open);
                }
                // The preview opened off to the right of a driver that never
                // moved, so nothing has pulled the camera onto it.
                state.show_also = state.ws.joined_child(pid);
                self.sync(cx);
            }
            Act::Tab(pid) => {
                state.ws.focus = Some(pid);
                self.sync(cx);
            }
            Act::Btn(pid, b) => {
                match b {
                    BtnAct::TryIt => {
                        state.toast("side effect: nothing was opened or replaced", false);
                    }
                    BtnAct::Refresh => {
                        if state.pump.idle() {
                            state.toast("no accounts to sync — add one in settings", false);
                        } else {
                            state.pump.kick();
                            state.toast("syncing…", false);
                        }
                    }
                    BtnAct::Archive | BtnAct::Delete => {
                        // The button acts on the mail its panel is showing;
                        // triage then closes every reader of it, this one
                        // included.
                        if let Some(Kind::Message { id }) =
                            state.ws.panels.get(&pid).map(|p| p.kind.clone())
                        {
                            self.triage(cx, id, b == BtnAct::Delete);
                        }
                        return;
                    }
                    BtnAct::Send => {
                        let seed = match state.ws.panels.get(&pid).map(|p| p.kind.clone()) {
                            Some(Kind::Compose { seed }) => seed,
                            _ => Seed::Blank,
                        };
                        let d = self
                            .hosted
                            .get(&pid)
                            .map(|w| w.as_compose_panel().values(cx))
                            .unwrap_or_default();
                        if d.to.is_empty() {
                            state.toast("no recipient", true);
                        } else {
                            let delay = state.send_delay;
                            let now = state.world.now();
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
                                    mail::upsert_draft_tx(tx, pid as i64, seed, &d, now)?;
                                    mail::file_send_tx(tx, pid as i64, now + delay)
                                },
                                vec![Box::new(mail::Sent { panel: pid as i64, delay }) as Box<dyn crate::history::Intent>],
                            );
                            state.toast(
                                format!("sending in {}s — ⌘z undoes", delay as u32),
                                false,
                            );
                        }
                    }
                    BtnAct::Discard => {
                        let label = format!("discard “{}”", state.title_of(pid));
                        let seed = match state.ws.panels.get(&pid).map(|p| p.kind.clone()) {
                            Some(Kind::Compose { seed }) => seed,
                            _ => Seed::Blank,
                        };
                        // The text goes with the panel, so undo has to carry
                        // it — the text the panel *shows*. The row can lag
                        // it (a keystroke), be missing (never typed in) or
                        // predate it (a compose retargeted in place, unedited
                        // since); what undo puts back is what was on screen.
                        let draft = self
                            .hosted
                            .get(&pid)
                            .map(|w| w.as_compose_panel().values(cx))
                            .or_else(|| mail::draft_for(&state.store, pid as i64, seed))
                            .unwrap_or_else(|| mail::seed_draft(&state.store, seed));
                        state.act(
                            "close",
                            label,
                            None,
                            move |ws| {
                                ws.close(pid);
                            },
                            move |tx| mail::discard_draft_tx(tx, pid as i64),
                            vec![Box::new(mail::Discarded {
                                panel: pid as i64,
                                draft,
                                seed,
                            }) as Box<dyn crate::history::Intent>],
                        );
                    }
                }
                self.sync(cx);
            }
            // Handled above — they return before reaching this match.
            Act::WsRow(_) | Act::LauncherOpen | Act::LauncherRow(_) | Act::HistoryRow(_) | Act::OverlayClose | Act::Pointer(_) | Act::WidgetOp(..) | Act::Acquire | Act::Problems | Act::Noop => {}
        }
    }

    /// Opens or closes one message of the thread a panel shows (CR-007),
    /// or unfolds and folds its quoted tail. Panel context, like the inbox
    /// cursor: no action, no history node — and a touch inside the panel
    /// focuses it, as anywhere else.
    fn toggle_msg(&mut self, cx: &mut Cx, pid: PanelId, id: core::MailId, quote: bool) {
        let Some(state) = self.state.as_deref_mut() else {
            return;
        };
        if state.ws.focus != Some(pid) {
            state.ws.focus = Some(pid);
        }
        let Some(Kind::Message { id: cur }) = state.ws.panel(pid).map(|p| p.kind.clone()) else {
            return;
        };
        let e = state
            .expand
            .entry(pid)
            .or_insert_with(|| crate::panels::Expansion::just(cur));
        if e.for_mail != cur {
            *e = crate::panels::Expansion::just(cur);
        }
        let set = if quote { &mut e.quotes } else { &mut e.open };
        if !set.remove(&id) {
            set.insert(id);
        }
        // The panel's wish changed with what it shows.
        self.sync(cx);
    }

    /// Files a thread out of the inbox — archive or delete — from wherever
    /// the intent came: a message panel's header button, the chord an inbox
    /// borrowed from its preview, or an android row swipe. One door, so the
    /// undo node, the toast and the closing of the thread's readers are the
    /// same story every time. The row is the thread (CR-007), so every
    /// inbox mail of the conversation goes together — one intent each, one
    /// node — and the mail itself with them if it sits elsewhere (a reader
    /// on an archived mail).
    fn triage(&mut self, cx: &mut Cx, id: core::MailId, delete: bool) {
        // Decided first, while the row is still in the list to have one.
        let next = self.successor_of(id);
        let Some(state) = self.state.as_deref_mut() else {
            return;
        };
        let (verb, done, role) = if delete {
            ("delete", "deleted", "trash")
        } else {
            ("archive", "archived", "archive")
        };
        // Ask before acting: without the folder the move is a no-op, and an
        // action that changes nothing records no node — so the user would
        // get silence rather than an answer.
        if !mail::can_file(&state.store, id, role) {
            state.toast(format!("this account has no {role} folder"), true);
            self.kick(cx);
            return;
        }
        let topic = mail::thread_topic(&state.store, id).unwrap_or_default();
        let mut ids = mail::thread_inbox(&state.store, id);
        if !ids.contains(&id) {
            ids.push(id);
        }
        // Where each lives now, so undo puts every one back exactly there
        // rather than guessing "the inbox".
        let from: Vec<(core::MailId, i64)> = ids
            .iter()
            .map(|&m| {
                let f: i64 = state
                    .store
                    .conn()
                    .query_row("SELECT folder FROM message WHERE id = ?1", [m], |r| r.get(0))
                    .unwrap_or(0);
                (m, f)
            })
            .collect();
        let mut readers: Vec<PanelId> = Vec::new();
        for m in &ids {
            for r in state.ws.showing(&Kind::Message { id: *m }) {
                if !readers.contains(&r) {
                    readers.push(r);
                }
            }
        }
        // The successor's preview is an open like any other: measured first,
        // so it is placed by the rows its thread actually wants — and it
        // reads its thread, exactly as the walk would.
        if let Some((_, nid)) = next {
            state.wish_ahead(&Kind::Message { id: nid });
        }
        let next_marks: Vec<core::MailId> = next
            .map(|(_, nid)| mail::thread_unread(&state.store, nid))
            .unwrap_or_default();
        let n = ids.len();
        let (ids_tx, marks_tx) = (ids.clone(), next_marks.clone());
        state.act(
            verb,
            format!("{verb} “{topic}”"),
            None,
            move |ws| {
                // The thread left the inbox, so its readers have nothing left
                // to read — on whichever workspace they were opened.
                for r in readers {
                    ws.close_anywhere(r);
                }
                // The walk survives triaging the row it stood on: the cursor
                // moves up one and its preview opens in the same breath. Same
                // action, so one ⌘z takes the whole thing back.
                if let Some((pid, nid)) = next {
                    let child = ws.follow_open(pid, Kind::Message { id: nid }, false);
                    ws.activate(child);
                    ws.focus = Some(pid);
                }
            },
            move |tx| {
                for m in &ids_tx {
                    if delete {
                        mail::delete_tx(tx, *m)?;
                    } else {
                        mail::archive_tx(tx, *m)?;
                    }
                }
                for m in &marks_tx {
                    mail::mark_read_tx(tx, *m)?;
                }
                Ok(())
            },
            // Both halves of the action claim something back: the filing, and
            // the read of whatever the cursor moved onto. One node, so one
            // ⌘z reverses the pair in step.
            next_marks
                .iter()
                .map(|m| Box::new(mail::MarkRead { mail: *m }) as Box<dyn crate::history::Intent>)
                .chain(from.iter().map(|(m, f)| {
                    Box::new(mail::Filed {
                        mail: *m,
                        from_folder: *f,
                        role,
                    }) as Box<dyn crate::history::Intent>
                }))
                .collect(),
        );
        let what = if n > 1 {
            format!("{done} “{topic}” ({n} mails) — ⌘z undoes")
        } else {
            format!("{done} “{topic}” — ⌘z undoes")
        };
        state.toast(what, false);
        if let Some((pid, nid)) = next {
            let open: BTreeSet<core::MailId> =
                next_marks.iter().copied().chain(std::iter::once(nid)).collect();
            state.seed_expansion(nid, &open);
            state.show_also = state.ws.joined_child(pid);
            cx.action(crate::panels::PanelAction::SelectMail { pid, id: nid });
        }
        self.sync(cx);
    }

    /// Where an inbox cursor standing on `id` should land once it is filed
    /// away: the next row down, or the one above if it was the last. `None`
    /// when no inbox is pointing at this mail — a header button pressed on a
    /// message nobody is walking towards moves no cursor.
    fn successor_of(&mut self, id: core::MailId) -> Option<(PanelId, core::MailId)> {
        let inboxes: Vec<PanelId> = self
            .state
            .as_deref()?
            .ws
            .panels
            .iter()
            .filter(|(_, p)| matches!(p.kind, Kind::Inbox { .. }))
            .map(|(pid, _)| *pid)
            .collect();
        let store = self.state.as_deref()?.store.clone();
        let th = mail::thread_of(&store, id)?;
        let pid = inboxes.into_iter().find(|pid| {
            self.hosted
                .get(pid)
                .and_then(|w| w.as_inbox_panel().selected_thread())
                == Some(th)
        })?;
        // The rows as that panel has them — its own filter included: the
        // panel's table answers, exactly as it does for hit registration.
        let w = self.hosted.get(&pid)?.clone();
        w.as_inbox_panel()
            .neighbour_of(&store, id)
            .map(|next| (pid, next))
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
            // A drag keeps the panel no matter what other fingers do, and a
            // row keeps its curtain: a second finger must not strand one
            // half-drawn with nothing left to settle it.
            TouchMode::Drag { .. } | TouchMode::RowSwipe { .. } => {}
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
                // Sideways on a mail row is triage (CR-005); sideways
                // anywhere else still means nothing (the workspace pans on
                // two fingers). Vertical is the panel's scroll, and keeps
                // ties — a diagonal is a scroll, never a half-swipe.
                let row = match act {
                    Some(Act::WidgetOp(pid, WidgetOp::OpenMail(id))) => Some((*pid, *id)),
                    _ => None,
                };
                self.touch.mode = match (row, act.as_ref().and_then(act_pid)) {
                    (Some((pid, id)), _) if t.x.abs() > t.y.abs() => {
                        self.row_swipe = Some(RowSwipe {
                            pid,
                            id,
                            slot: self.row_rect(pid, id).unwrap_or_default(),
                            x: Spring::at_rest(0.0, SpringParams::movement()),
                            commit: None,
                        });
                        TouchMode::RowSwipe { uid }
                    }
                    (_, Some(pid)) if t.y.abs() >= t.x.abs() => TouchMode::Scroll { uid, pid },
                    _ => TouchMode::Dead,
                };
            }
            TouchMode::RowSwipe { uid: u } if *u == uid => {
                // The curtain tracks the finger 1:1 — no spring while it is
                // down, or the ink would lag the thumb.
                if let Some(rs) = self.row_swipe.as_mut() {
                    rs.x.jump_to(p.x - start.x);
                }
                self.kick(cx);
            }
            TouchMode::Scroll { uid: u, pid: _ } if *u == uid => {
                // Retained content scrolls itself: the drag becomes a
                // Scroll event for the widget under the finger, so its own
                // PortalList / ScrollBars clamp it (CR-002 F — the char
                // grid's scroll offset no longer draws anything).
                let ev = Event::Scroll(ScrollEvent {
                    window_id: CxWindowPool::id_zero(),
                    scroll: dvec2(0.0, -d.y),
                    abs: p,
                    modifiers: KeyModifiers::default(),
                    handled_x: std::cell::Cell::new(false),
                    handled_y: std::cell::Cell::new(false),
                    is_mouse: false,
                    time: 0.0,
                    phase: ScrollPhase::None,
                });
                self.forward_to_hosted(cx, &ev);
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
                    let act = match act {
                        // Standalone widgets own their taps (the mouse
                        // rule): resolving the semantic op here too would
                        // double-fire.
                        Act::WidgetOp(pid, WidgetOp::AddAccount) => Act::Pointer(pid),
                        a => a,
                    };
                    self.resolve_click(cx, act, false);
                }
            }
            TouchMode::Scroll { uid: u, .. } if u == uid => {
                self.touch.mode = TouchMode::Idle;
            }
            TouchMode::RowSwipe { uid: u } if u == uid => {
                self.touch.mode = TouchMode::Idle;
                if let Some(rs) = self.row_swipe.as_mut() {
                    if rs.armed() {
                        // Committed: the curtain runs on to cover the row,
                        // and the mail is filed when it lands — so the row is
                        // gone from view before it is gone from the inbox.
                        let w = rs.slot.size.x;
                        rs.commit = Some(rs.x.value() > 0.0);
                        rs.x.retarget(if rs.x.value() > 0.0 { w } else { -w });
                    } else {
                        rs.x.retarget(0.0);
                    }
                }
                self.kick(cx);
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

    /// Runs a settled curtain: a committed one files its mail, a cancelled
    /// one just clears. Called once the spring has landed, so the row is
    /// covered before it leaves the inbox rather than blinking out from
    /// under the finger.
    fn settle_row_swipe(&mut self, cx: &mut Cx) {
        let done = self
            .row_swipe
            .as_ref()
            .is_some_and(|rs| rs.x.is_done() && !matches!(self.touch.mode, TouchMode::RowSwipe { .. }));
        if !done {
            return;
        }
        let Some(rs) = self.row_swipe.take() else {
            return;
        };
        if let Some(delete) = rs.commit {
            self.triage(cx, rs.id, delete);
        } else {
            self.kick(cx);
        }
    }

    /// The rect an inbox row was last drawn at, from the hits registered for
    /// that draw. The curtain needs somewhere to be, and this is the same
    /// geometry a tap on the row would resolve against.
    fn row_rect(&self, pid: PanelId, id: core::MailId) -> Option<Rect> {
        self.hits
            .iter()
            .find(|h| h.act == Act::WidgetOp(pid, WidgetOp::OpenMail(id)))
            .map(|h| h.rect)
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
        Kind::AddAccount => Some(live_id!(add_account_tpl)),
        Kind::Compose { .. } => Some(live_id!(compose_tpl)),
        Kind::Inbox { .. } => Some(live_id!(inbox_tpl)),
        Kind::Message { .. } => Some(live_id!(message_tpl)),
        Kind::Contact { .. } => Some(live_id!(contact_tpl)),
        Kind::Help => Some(live_id!(help_tpl)),
        Kind::About => Some(live_id!(about_tpl)),
        Kind::Problems => Some(live_id!(problems_tpl)),
        Kind::Effects => Some(live_id!(effects_tpl)),
        Kind::Job { .. } => Some(live_id!(job_tpl)),
    }
}

impl Stage {
    /// Brings the stage up on a world: opens (or creates) its store, seeds
    /// the demo mail, restores the session, starts the engine, and arms a
    /// script if there is one. The primary stage does this at startup from
    /// argv; the panels library does it per mount from a scene's node.
    pub fn boot(&mut self, cx: &mut Cx, boot: Boot) {
        if self.state.is_some() {
            return;
        }
        self.mount = !boot.primary;
        self.active = boot.primary;
        // A mount's first step waits for its first draw; a mount with no
        // steps is its state from the start.
        self.stale_hits = self.mount;
        self.arrived = self.mount && boot.steps.is_none();
        let store = Store::open(boot.db.as_deref())
            .unwrap_or_else(|e| panic!("store: opening {:?} failed: {e}", boot.db));
        // Seeding is a write, so under replication it waits for this device
        // to resolve as the holder (a follower installs the holder's
        // snapshot instead of seeding its own). Without a bucket — and on
        // every mount, whose world is its own — seed at boot as before.
        if !boot.primary || config().bucket.is_none() {
            if let Err(e) = mail::seed_if_empty(&store) {
                eprintln!("store: seeding demo mail failed: {e}");
            }
        }
        // A delivered send can no longer be undone — the walk marks it
        // expired and steps past.
        let mut s = State::new(store, &boot);
        if let Some(open) = boot.open {
            // Solo: one panel, fresh, in place of the session.
            let kind = open(&s.store);
            let mut ws = Wm::new();
            let pid = ws.open(kind.clone(), None, false);
            s.ws = ws;
            if let Kind::Message { id } = kind {
                let open = s.seed_for(id);
                s.seed_expansion(id, &open);
            }
            self.solo = Some(pid);
        }
        // The picture reader joins the one writer, as the other workers do
        // (CR-005 phase 0). Headless has no thread: a scripted run wants its
        // pictures in the frame that drew them, so the work stays inline —
        // the same bargain makepad's own decode strikes under `headless`.
        #[cfg(not(headless))]
        crate::panels::Pictures::serve(cx, s.store.db());
        s.spawn_workers();
        s.sync();
        let virtual_time = s.virtual_time;
        self.pump_due = s.world.now() + PUMP_S;
        self.state = Some(Box::new(s));
        // Belt and braces under the worker signal: a coarse poll so a lost
        // wake can never strand the UI on cached rows. Virtual time has no
        // worker threads, so no foreign commits to poll for — and a
        // wall-clock interval would be the last thing reading a clock the
        // script does not control.
        if boot.primary && !virtual_time {
            self.poll_timer = cx.start_interval(2.0);
        }
        if let Some(steps) = boot.steps {
            let out = std::path::PathBuf::from(&config().out);
            let _ = std::fs::create_dir_all(&out);
            let mut runner = e2e::Runner::new(steps, out);
            runner.tag = boot.tag;
            self.e2e = Some(runner);
            // Windowed: a real timer paces the run. Virtual time: the draw
            // cycle does, so ask for the first frame and keep asking.
            if !virtual_time {
                self.e2e_timer = cx.start_interval(E2E_TICK_MS / 1000.0);
            }
        }
        // The menu bar is this stage's from here: a window that opened on
        // the library carried only the Dev menu until now.
        self.update_menu(cx);
        self.next_frame = cx.new_next_frame();
        self.redraw_scoped(cx);
    }

    /// A mount is still replaying its steps.
    #[must_use]
    pub fn replaying(&self) -> bool {
        self.e2e.is_some()
    }

    /// Runs the manual pump for every half second of virtual time that has
    /// passed since it last ran — however far one frame jumped.
    fn pump_if_due(&mut self, state: &mut State) {
        let now = state.world.now();
        while now >= self.pump_due {
            state.pump_round();
            self.pump_due += PUMP_S;
        }
    }

    /// One frame of a mount's replay: one step, the way the harness runs
    /// one per tick. A pending `wait` is consumed whole, together with the
    /// step after it, so a node needs as many frames as it has steps rather
    /// than milliseconds. Anything a step did — the hits it changed, the
    /// hosted widgets it created, the actions its widgets raised (the
    /// canvas hands those back after this returns) — lands before the next
    /// step, which waits for the draw. Answers the virtual milliseconds
    /// advanced, for the springs.
    fn replay_step(&mut self, cx: &mut Cx) -> f64 {
        if self.stale_hits {
            return 0.0;
        }
        let Some(r) = &mut self.e2e else {
            return 0.0;
        };
        // A wait before a hit-resolving step gets a frame of its own: the
        // harness draws throughout a wait, so the click that follows finds
        // a panel where it settled, not where it was when the wait began.
        // Before a key, a shot or another wait it is consumed together
        // with that step.
        let mut dt = r.pending_wait().max(FRAME_MS);
        let settle = r.pending_wait() > 0.0 && r.next().is_some_and(e2e::Step::needs_hits);
        if settle {
            dt = r.take_wait();
        }
        if let Some(mut state) = self.state.take() {
            state.advance_clock(dt / 1000.0);
            self.pump_if_due(&mut state);
            self.state = Some(state);
        }
        if !settle {
            self.e2e_tick(cx, dt);
        }
        // Every replay frame ends with a draw before the next step: the
        // clock moved, so did the springs, and a hit-resolving step must
        // see the state after them, not whatever the budget last drew.
        self.stale_hits = true;
        dt
    }

    /// A mount reached its shot.
    #[must_use]
    pub fn arrived(&self) -> bool {
        self.arrived
    }

    /// A mount that reached its shot and is not entered: a picture. It
    /// gets no events, asks for no frames, and never re-runs its widget
    /// pass until the canvas enters it — that is what keeps a hundred of
    /// them free.
    #[must_use]
    pub fn frozen(&self) -> bool {
        self.mount && self.arrived && !self.active
    }

    /// The canvas entered (or left) this mount: it may (or may no longer)
    /// touch the window's IME and key focus, and its clock runs (or stands
    /// still).
    pub fn set_active(&mut self, cx: &mut Cx, active: bool) {
        if self.active == active {
            return;
        }
        self.active = active;
        if active {
            self.ime_shown = false;
            self.kick(cx);
        } else if self.ime_shown {
            self.ime_shown = false;
            cx.hide_text_ime();
        }
    }

    /// Redraws what this stage draws into — nothing while it is suspended
    /// under the library: a redraw of the whole window there would mark
    /// every mount pending on every tick of the script underneath, and the
    /// render budget would never get past the first few.
    fn redraw_scoped(&self, cx: &mut Cx) {
        if self.suspended {
            return;
        }
        redraw_scoped(cx, self.lists, self.mount);
    }

    /// Whether the stage has come up on a world.
    #[must_use]
    pub fn booted(&self) -> bool {
        self.state.is_some()
    }

    /// The panels library went up over this stage (or came down): while
    /// up, the stage neither draws nor hears input, and gives up the IME.
    pub fn set_suspended(&mut self, cx: &mut Cx, on: bool) {
        if self.suspended == on {
            return;
        }
        self.suspended = on;
        if on {
            if self.ime_shown {
                self.ime_shown = false;
                cx.hide_text_ime();
            }
        } else if self.state.is_some() {
            self.kick(cx);
        }
        cx.redraw_all();
    }

    /// Where a mount draws: its own draw list and the canvas's, so its
    /// redraws stay scoped.
    pub fn set_lists(&mut self, own: DrawListId, canvas: DrawListId) {
        self.lists = Some((own, canvas));
    }

    /// The live content widget for a panel, instantiated from its kind's
    /// template on first use (mirrors PortalList::item).
    fn hosted_widget(
        &mut self,
        cx: &mut Cx,
        pid: PanelId,
        tpl: LiveId,
    ) -> Option<(WidgetRef, bool)> {
        if let Some(w) = self.hosted.get(&pid) {
            return Some((w.clone(), false));
        }
        let template_ref = self.tpl.get(&tpl)?;
        let template_value: ScriptValue = template_ref.as_object().into();
        let vm_id = cx.script_ref_vm_id(template_ref)?;
        let widget =
            cx.with_script_vm_id(vm_id, |vm| WidgetRef::script_from_value(vm, template_value));
        self.hosted.insert(pid, widget.clone());
        Some((widget, true))
    }

    /// Turns bubbled widget intent ([`PanelAction`]) into store actions —
    /// the one place retained content meets the undo system.
    fn handle_panel_actions(&mut self, cx: &mut Cx, actions: &Actions) {
        let mut refresh = false;
        // A link in an HTML mail body leaves the app: panels are for this
        // app's own nouns — a mail, a contact — and the web is neither, so
        // the system browser takes it. The narrowing already vetted the
        // scheme (see [`crate::html`]), so nothing is left to check here.
        //
        // It lives at the app rather than on MessagePanel because every
        // open panel is handed the same action list: one click would
        // otherwise open as many browser windows as there are panels.
        for a in actions {
            if let Some(wa) = a.as_widget_action() {
                if let HtmlLinkAction::Clicked { url, .. } = wa.cast() {
                    cx.open_url(&url, OpenUrlInPlace::No);
                }
            }
        }
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
                        let stored = state
                            .world
                            .run(&crate::effect::SecretSet {
                                email: &email,
                                pass: &pass,
                            })
                            .is_ok();
                        if !stored {
                            state.toast("storing the password failed", true);
                        } else {
                            // The new row's id comes back from `act` (the
                            // write runs on the store's writer thread), so
                            // the claim needs no shared cell.
                            let (e, i, sm) = (email.clone(), imap.clone(), smtp.clone());
                            let added = state
                                .act(
                                    "account",
                                    format!("add account {email}"),
                                    None,
                                    |_| {},
                                    move |tx| mail::add_account_tx(tx, &e, &i, &sm),
                                    Vec::new(),
                                )
                                .unwrap_or(0);
                            state.history.claim(Box::new(mail::AccountAdded {
                                id: added,
                                email: email.clone(),
                                imap: imap.clone(),
                                smtp: smtp.clone(),
                            }));
                            state.spawn_workers();
                            state.toast("account added — syncing", false);
                            if let Some(w) = self.hosted.get(&pid) {
                                if let Some(mut ap) = w.as_add_account_panel().borrow_mut() {
                                    ap.clear_form(cx);
                                }
                            }
                        }
                    }
                    refresh = true;
                }
                crate::panels::PanelAction::DraftEdited {
                    pid,
                    to,
                    subject,
                    body,
                } => {
                    let Some(state) = self.state.as_deref_mut() else {
                        continue;
                    };
                    let seed = match state.ws.panel(pid).map(|p| p.kind.clone()) {
                        Some(Kind::Compose { seed }) => seed,
                        _ => Seed::Blank,
                    };
                    mail::save_draft(
                        &state.store,
                        pid as i64,
                        seed,
                        &mail::Draft { to, subject, body },
                        state.world.now(),
                    );
                }
                crate::panels::PanelAction::OpenMail { pid, id, fresh } => {
                    self.resolve_click(cx, Act::Open(pid, Kind::Message { id }), fresh);
                }
                crate::panels::PanelAction::PreviewMail { pid, id } => {
                    // Straight through, no pacing. A preview costs ~0.2 ms —
                    // one small transaction over a handful of UI rows, on a
                    // WAL store with `synchronous=NORMAL`, coalescing into the
                    // head node rather than appending — so even a held arrow
                    // spends well under a frame on them. Anything that queued
                    // them would only put a delay between the cursor and what
                    // it is pointing at, and could land a stale focus restore
                    // on top of a cmd+arrow the user has since pressed.
                    self.resolve_click(cx, Act::Preview(pid, Kind::Message { id }), false);
                }
                crate::panels::PanelAction::SelectMail { .. } => {}
                crate::panels::PanelAction::OpenJob { pid, id, fresh } => {
                    self.resolve_click(cx, Act::Open(pid, Kind::Job { id }), fresh);
                }
                crate::panels::PanelAction::PreviewJob { pid, id } => {
                    self.resolve_click(cx, Act::Preview(pid, Kind::Job { id }), false);
                }
                crate::panels::PanelAction::SelectJob { .. } => {}
                crate::panels::PanelAction::FollowLink {
                    pid,
                    target,
                    dotted,
                    fresh,
                } => {
                    // A dotted walk inside a preview re-aims the pair: the
                    // driver's cursor follows, so master and detail never
                    // disagree about which mail is open — whoever moved it.
                    if let (true, false, Kind::Message { id }) = (dotted, fresh, &target) {
                        let driver = self.state.as_deref().and_then(|s| {
                            let p = s.ws.join_parent_of(pid)?;
                            let k = s.ws.panel(p).map(|q| q.kind.clone())?;
                            ui::preview_kind(&k).map(|_| p)
                        });
                        if let Some(p) = driver {
                            cx.action(crate::panels::PanelAction::SelectMail { pid: p, id: *id });
                        }
                    }
                    let act = if dotted {
                        Act::Replace(pid, target)
                    } else {
                        Act::Open(pid, target)
                    };
                    self.resolve_click(cx, act, fresh);
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
                        vec![Box::new(mail::AccountRemoved {
                            email: email.clone(),
                        }) as Box<dyn crate::history::Intent>],
                    );
                    state.spawn_workers();
                    state.toast(format!("removed {email} — ⌘z undoes"), false);
                    state.announce_problems();
                    refresh = true;
                }
                crate::panels::PanelAction::SyncAccount(id) => {
                    let Some(state) = self.state.as_deref_mut() else {
                        continue;
                    };
                    let email = mail::accounts(&state.store)
                        .iter()
                        .find(|a| a.id == id)
                        .map(|a| a.email.clone())
                        .unwrap_or_default();
                    state.pump.kick_account(id);
                    state.toast(format!("syncing {email}…"), false);
                    refresh = true;
                }
                crate::panels::PanelAction::RetrySend(id) => {
                    let Some(state) = self.state.as_deref_mut() else {
                        continue;
                    };
                    // The send action again, window and all: the same row,
                    // re-filed, and the same claim to take it back.
                    let delay = config().send_delay;
                    let now = state.world.now();
                    let subject = draft_subject(&state.store, id);
                    let error = mail::outbox_failures(&state.store)
                        .iter()
                        .find(|(i, _)| *i == id)
                        .map(|(_, e)| e.clone())
                        .unwrap_or_else(|| "send failed".into());
                    // Not a `Sent`: undoing that deletes the row, and with
                    // no compose to reopen the draft would be stranded.
                    // Undoing a retry puts the *failure* back.
                    let filed = state.act(
                        "send",
                        format!("retry “{subject}”"),
                        Some(format!("outbox:{id}")),
                        |_| {},
                        move |tx| mail::file_send_tx(tx, id, now + delay),
                        vec![Box::new(mail::Retried {
                            outbox: id,
                            error,
                            delay,
                        }) as Box<dyn crate::history::Intent>],
                    );
                    if filed.is_some() {
                        state.toast(
                            format!("sending in {}s — ⌘z undoes", delay as u32),
                            false,
                        );
                    }
                    // The list changed under our own hand: reconcile what
                    // counts as announced now, not at the next poll, so the
                    // next failure of this send is news again.
                    state.announce_problems();
                    refresh = true;
                }
                crate::panels::PanelAction::ReopenSend {
                    pid,
                    outbox,
                    seed,
                    fresh,
                } => {
                    let Some(state) = self.state.as_deref_mut() else {
                        continue;
                    };
                    let subject = draft_subject(&state.store, outbox);
                    let error = mail::outbox_failures(&state.store)
                        .iter()
                        .find(|(i, _)| *i == outbox)
                        .map(|(_, e)| e.clone())
                        .unwrap_or_else(|| "send failed".into());
                    let now = state.world.now();
                    let kind = Kind::Compose { seed };
                    // The compose panel's id is minted by the layout change;
                    // the data half and the claim read it from here.
                    let minted = std::sync::Arc::new(std::sync::atomic::AtomicU64::new(0));
                    let (m_layout, m_data) = (minted.clone(), minted.clone());
                    let reopened = state.act(
                        "open",
                        format!("reopen “{subject}”"),
                        Some(format!("outbox:{outbox}")),
                        move |ws| {
                            let p = ws.follow_open(pid, kind, fresh);
                            m_layout.store(p, std::sync::atomic::Ordering::Relaxed);
                        },
                        move |tx| {
                            let new = m_data.load(std::sync::atomic::Ordering::Relaxed) as i64;
                            mail::reopen_send_tx(tx, outbox, new, now)
                        },
                        vec![Box::new(mail::Reopened {
                            old: outbox,
                            new: minted,
                            error,
                        }) as Box<dyn crate::history::Intent>],
                    );
                    if reopened.is_some() {
                        state.toast(format!("reopened “{subject}” — ⌘z undoes"), false);
                    }
                    state.announce_problems();
                    refresh = true;
                }
                crate::panels::PanelAction::TryIt { pid: _ } => {
                    if let Some(state) = self.state.as_deref_mut() {
                        state.toast("side effect: nothing was opened or replaced", false);
                    }
                    refresh = true;
                }
            }
        }
        // The launcher's field reports its own edits; the search re-runs on
        // the next draw from this query.
        for a in actions {
            if let Some(crate::panels::OverlayAction::Query(q)) =
                a.downcast_ref::<crate::panels::OverlayAction>()
            {
                if let Some(state) = self.state.as_deref_mut() {
                    if state.launcher.query != *q {
                        state.launcher.query = q.clone();
                        state.launcher.sel = 0;
                        refresh = true;
                    }
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

    /// A press-drag-release, the gesture text selection is made of. The
    /// moves carry PRIMARY down so the widget reads them as a drag.
    fn synth_drag(&mut self, cx: &mut Cx, from: DVec2, to: DVec2) {
        let down = Event::MouseDown(MouseDownEvent {
            abs: from,
            button: MouseButton::PRIMARY,
            window_id: CxWindowPool::id_zero(),
            modifiers: KeyModifiers::default(),
            handled: std::cell::Cell::new(Area::Empty),
            time: 0.0,
        });
        self.forward_to_hosted(cx, &down);
        for i in 1..=8 {
            let f = f64::from(i) / 8.0;
            let p = from + (to - from) * f;
            let mv = Event::MouseMove(MouseMoveEvent {
                abs: p,
                window_id: CxWindowPool::id_zero(),
                modifiers: KeyModifiers::default(),
                time: f * 0.1,
                handled: std::cell::Cell::new(Area::Empty),
                lock_delta: Default::default(),
            });
            self.forward_to_hosted(cx, &mv);
        }
        let up = Event::MouseUp(MouseUpEvent {
            abs: to,
            button: MouseButton::PRIMARY,
            window_id: CxWindowPool::id_zero(),
            modifiers: KeyModifiers::default(),
            time: 0.2,
        });
        self.forward_to_hosted(cx, &up);
    }

    /// Whether the focused panel's content is a retained widget tree — keys
    /// and text then belong to it rather than to the shell. Every kind is
    /// hosted since CR-002 F, so this is "something is focused" in practice.
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
            let Some(kind) = state.ws.panel(*pid).map(|p| p.kind.clone()) else {
                continue;
            };
            let props = crate::panels::PanelProps {
                store: state.store.clone(),
                registry: state.world.registry_rc(),
                pid: *pid,
                problems: state.problems_for(&kind),
                kind,
                expand: state.expand.get(pid).cloned(),
            };
            let mut scope = Scope::with_props(&props);
            w.handle_event(cx, event, &mut scope);
        }
    }

    /// Hands an event to whichever overlay widget is up. Its rows are
    /// presentation, so in practice this feeds the launcher's query field.
    fn forward_to_overlay(&mut self, cx: &mut Cx, event: &Event) {
        let Some(state) = self.state.as_deref() else {
            return;
        };
        let key = match state.overlay {
            Overlay::Launcher => OVERLAY_PID_L,
            Overlay::Ws | Overlay::History => OVERLAY_PID_R,
            Overlay::None => return,
        };
        let Some(w) = self.hosted.get(&key).cloned() else {
            return;
        };
        // Rows come from the shell each draw; event handling needs none.
        let props = crate::panels::OverlayProps::default();
        let mut scope = Scope::with_props(&props);
        w.handle_event(cx, event, &mut scope);
    }

    /// Keys and text go to the focused panel's widget alone — pointer
    /// events are positional, but the keyboard belongs to one panel, and
    /// a "j" typed into compose must not walk some other inbox.
    fn forward_to_focused(&mut self, cx: &mut Cx, event: &Event) {
        let Some(f) = self.state.as_deref().and_then(|s| s.ws.focus) else {
            return;
        };
        self.forward_to_panel(cx, f, event);
    }

    /// As [`Stage::forward_to_focused`], to a named panel: a borrowed chord
    /// has to reach the preview that owns it, and that panel needs *its own*
    /// props on the scope to know which mail it is looking at.
    fn forward_to_panel(&mut self, cx: &mut Cx, pid: PanelId, event: &Event) {
        let Some(state) = self.state.as_deref() else {
            return;
        };
        let Some(w) = self.hosted.get(&pid) else {
            return;
        };
        let Some(kind) = state.ws.panel(pid).map(|p| p.kind.clone()) else {
            return;
        };
        let props = crate::panels::PanelProps {
            store: state.store.clone(),
            registry: state.world.registry_rc(),
            pid,
            problems: state.problems_for(&kind),
            kind,
            expand: state.expand.get(&pid).cloned(),
        };
        let mut scope = Scope::with_props(&props);
        w.handle_event(cx, event, &mut scope);
    }
}

impl Widget for Stage {
    fn handle_event(&mut self, cx: &mut Cx, event: &Event, _scope: &mut Scope) {
        // A frozen mount is a picture: nothing to hear, nothing to ask for.
        if self.frozen() {
            return;
        }
        // Images the open letters asked for (see `Pictures`): filed as they
        // arrive, and everything redraws to place them. An HTTP reply, bytes
        // the picture reader took out of a letter's raw, a texture makepad's
        // decode pool finished — all three land in a `Cx` global that is no
        // stage's property, so they are filed above the suspend gate below:
        // each is delivered once, and a stage that let one past would strand
        // the item waiting on it. Before the hosted content sees the same
        // actions, so an item that asked finds its texture already cached.
        if let Event::NetworkResponses(responses) = event {
            if crate::panels::pictures_arrived(cx, responses) {
                cx.redraw_all();
            }
        }
        if let Event::Actions(actions) = event {
            if crate::panels::pictures_landed(cx, actions) {
                cx.redraw_all();
            }
        }
        // Under the library: the world keeps turning (timers, the store's
        // signals, a running script), the window is not this stage's.
        if self.suspended
            && !matches!(
                event,
                Event::Startup | Event::Timer(_) | Event::Signal | Event::MacosMenuCommand(_)
            )
            && !(matches!(event, Event::NextFrame(_)) && self.e2e.is_some())
        {
            return;
        }
        // Retained content (CR-002): hosted widgets see every event through
        // their own system. Key/text events are forwarded by the inner
        // handlers instead (so the e2e paths share the exact route);
        // everything else — pointers, actions, frames — passes through here.
        if !matches!(
            event,
            Event::KeyDown(_) | Event::KeyUp(_) | Event::TextInput(_)
        ) {
            self.forward_to_hosted(cx, event);
            // The overlay is hosted too, but keyed outside the panel map —
            // without this its field would never hear its own Changed
            // action, and the query would type but never search.
            self.forward_to_overlay(cx, event);
        }
        if let Some(pid) = self.pending_focus.take() {
            if let Some(w) = self.hosted.get(&pid).cloned() {
                if pid == OVERLAY_PID_L {
                    let q = self
                        .state
                        .as_deref()
                        .map(|s| s.launcher.query.clone())
                        .unwrap_or_default();
                    w.as_launcher_overlay().focus_query(cx, &q);
                } else {
                    // A forward has its letter and wants a recipient; the
                    // others have their recipient, or nothing, and start
                    // in the body.
                    let forward = matches!(
                        self.state
                            .as_deref()
                            .and_then(|s| s.ws.panel(pid))
                            .map(|p| &p.kind),
                        Some(Kind::Compose {
                            seed: Seed::Forward(_)
                        })
                    );
                    let c = w.as_compose_panel();
                    if forward {
                        c.focus_to(cx);
                    } else {
                        c.focus_body(cx);
                    }
                }
            }
        }
        // A blurring TextInput hides the platform IME (its own lifecycle);
        // if key focus went back to the shell — or nowhere — letters would
        // stop arriving as TextInput events. Re-show for the letter grammar.
        if let Event::KeyFocus(ke) = event {
            if !cfg!(target_os = "android")
                && self.hosted_focus()
                && (ke.focus == self.area || ke.focus == Area::Empty)
            {
                self.ime_shown = false;
                self.kick(cx);
            }
            // android, field to field ("next"): with the patched TextInput
            // (fork commit bdb23508) the blurring field no longer hides the
            // keyboard when focus moves to another widget, so the move is
            // seamless — no ops at all when the config matches. The timer
            // guard stays armed purely as a safety net.
            if cfg!(target_os = "android")
                && self.hosted_focus()
                && ke.prev != Area::Empty
                && ke.prev != self.area
                && ke.focus != Area::Empty
                && ke.focus != self.area
            {
                log!("ime guard: armed (field-to-field focus move)");
                self.ime_guard_tries = 2;
                self.ime_guard_timer = cx.start_timeout(0.4);
            }
        }
        if let Event::Actions(actions) = event {
            self.handle_panel_actions(cx, actions);
        }
        // The window's own stage boots from argv — unless the panels
        // library owns the window, in which case it boots mounts instead
        // and this stage stays empty.
        if matches!(event, Event::Startup) && !self.mount && config().library.is_none() {
            self.boot(cx, Boot::primary(cx));
        }
        if let Event::Timer(te) = event {
            if self.e2e_timer.0 != 0 && te.timer_id == self.e2e_timer.0 {
                self.e2e_tick(cx, E2E_TICK_MS);
            }
            if self.poll_timer.0 != 0 && te.timer_id == self.poll_timer.0 {
                self.poll_store(cx);
            }
            if self.ime_guard_timer.0 != 0 && te.timer_id == self.ime_guard_timer.0 {
                self.ime_guard_timer = Timer::default();
                // The keyboard lost the race (it is down while a widget —
                // in practice the next TextInput — holds key focus): reset
                // the platform's config dedup so the focused field's next
                // draw re-shows with its own config. Checked against the
                // LIVE focus (stored areas go stale across redraws). A
                // keyboard that made it up, or a user dismissal outside a
                // focus move, never gets here.
                let focus = cx.key_focus();
                let field_focused = focus != Area::Empty && focus != self.area;
                log!(
                    "ime guard: fire kb_h={} field_focused={} tries_left={}",
                    self.kb_h,
                    field_focused,
                    self.ime_guard_tries
                );
                if self.kb_h == 0.0 && field_focused && self.ime_guard_tries > 0 {
                    self.ime_guard_tries -= 1;
                    log!("ime guard: re-issuing keyboard show");
                    cx.hide_text_ime();
                    self.redraw_scoped(cx);
                    if self.ime_guard_tries > 0 {
                        self.ime_guard_timer = cx.start_timeout(0.5);
                    }
                }
            }
        }

        match event {
            Event::WindowGeomChange(e) => {
                // The viewport itself follows the drawn turtle (draw_walk);
                // here we only capture the safe-area insets a fold/notch
                // carves out. The next draw picks up both.
                let ins = e.new_geom.safe_area_insets;
                self.insets = (ins.top, ins.right, ins.bottom, ins.left);
                self.redraw_scoped(cx);
            }

            Event::TouchUpdate(e) => self.touch_update(cx, e),

            Event::LongPress(e) => self.long_press(cx, e.uid, e.abs),

            Event::KeyDown(k) => self.handle_key_down(cx, k),

            Event::KeyUp(k) => self.handle_key_up(cx, k),

            // A sync worker committed. The platform already consumed the
            // ui-signal flag before delivering this event (macos.rs checks
            // and clears it itself) — so never re-check it here, just poll.
            Event::Signal => self.poll_store(cx),

            // Device-sync lease lifecycle (CR-005): hand the lease back when
            // this device steps away, so the other can take over without an
            // override; re-poll when it returns. On android these are the
            // activity's stop/start; on macOS the app-terminate path below
            // (Shutdown) is the reliable release.
            Event::Background | Event::Pause => {
                if let Some(state) = self.state.as_deref_mut() {
                    state.repl_release();
                }
            }
            Event::Foreground | Event::Resume => {
                if let Some(state) = self.state.as_deref_mut() {
                    state.repl_kick();
                }
            }
            Event::Shutdown => {
                // The last chance to release: run it synchronously, since the
                // worker may never get another turn.
                if let Some(state) = self.state.as_deref() {
                    state.repl_release_blocking();
                }
            }

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
                } else if id == MENU_PROBLEMS {
                    self.go_to(cx, Kind::Problems);
                } else if id == MENU_HISTORY {
                    if let Some(state) = self.state.as_deref_mut() {
                        state.overlay = Overlay::History;
                    }
                    self.kick(cx);
                } else if id == MENU_LIBRARY {
                    cx.action(DevAction::ToggleLibrary);
                }
            }

            Event::TextInput(e) => {
                // A hosted panel's TextInputs own the whole IME protocol —
                // android's authoritative full-state syncs and plain chars
                // alike — so they get the original event first, untouched.
                // The e2e text path stays on handle_text, which synthesizes
                // the same event: one route, two doors. What is left over
                // (an overlay is up, or nothing is focused) splits by shape:
                // full state to handle_ime_state, chars to handle_text.
                if self.hosted_focus()
                    && self
                        .state
                        .as_deref()
                        .is_some_and(|s| s.overlay == Overlay::None)
                {
                    self.forward_to_focused(cx, event);
                    self.kick(cx);
                } else if let Some(fs) = e.full_state_sync.clone() {
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
                    VirtualKeyboardEvent::DidHide { .. } => self.kb_h = 0.0,
                }
                self.kick(cx);
            }

            Event::MouseMove(e) => {
                let p = e.abs;
                let act = self.hit_at(p).map(|h| (h.act.clone(), h.cursor));
                // A hosted reading is registered as one rect with the text
                // cursor, and this runs after the hosted widgets, so the
                // hand a link inside it set would be overruled here. Ask
                // the widget what lies under the point: a link, or a
                // picture that is one, wears the hand.
                let hand = matches!(&act, Some((Act::Pointer(pid), _)) if self.link_under(cx, *pid, p));
                let Some(state) = self.state.as_deref_mut() else {
                    return;
                };
                let new_hover = act.as_ref().and_then(|(a, _)| match a {
                    Act::Focus(_) => None,
                    other => Some(other.clone()),
                });
                cx.set_cursor(if hand {
                    MouseCursor::Hand
                } else {
                    act.map(|(_, c)| c).unwrap_or(MouseCursor::Default)
                });
                if new_hover != state.hover {
                    state.hover = new_hover;
                    self.redraw_scoped(cx);
                }
            }

            Event::MouseDown(e) => {
                self.cmd_tap.other_input();
                let act = self.hit_at(e.abs).map(|h| h.act.clone());
                // A hosted field under the click already took key focus via
                // the forwarded event — stealing it back would kill the
                // caret (and with it all typing).
                if !matches!(act, Some(Act::Pointer(_))) {
                    cx.set_key_focus(self.area);
                }
                if let Some(act) = act {
                    // cmd+click (alt as a quiet alias): a fresh, un-joined panel.
                    let fresh = e.modifiers.logo || e.modifiers.alt;
                    // PortalList items are rebuilt every draw, so their areas
                    // go stale the moment a mid-gesture redraw lands — a
                    // down/up pair inside one cannot be trusted. In-list
                    // controls (rows, remove) therefore resolve semantically
                    // for real clicks too. Standalone widgets (the add
                    // button) keep their native path — resolving those here
                    // as well would double-fire.
                    let act = match act {
                        Act::WidgetOp(pid, WidgetOp::AddAccount) => Act::Pointer(pid),
                        a => a,
                    };
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
                // Vertical scrolling belongs to the retained content, which
                // saw this event first (`forward_to_hosted`).
                if e.scroll.x.abs() > e.scroll.y.abs() {
                    state.pan(e.scroll.x);
                }
                self.kick(cx);
            }

            Event::NextFrame(ne) => {
                // A replaying mount ticks whenever the canvas hands it a
                // frame: the canvas decides which mount replays when, so
                // the frame it asked for may long since have gone by.
                let asked = ne.set.contains(&self.next_frame);
                let replaying_mount = self.mount && self.e2e.is_some();
                if !(asked || replaying_mount) {
                    return;
                }
                // Virtual time: one draw cycle is one e2e tick of exactly
                // FRAME_MS, and the run keeps the loop turning by asking
                // for the next frame every time. No wall clock anywhere.
                //
                // A mount replaying its steps fast-forwards: a pending
                // `wait` is consumed whole, so a node reaches its state in
                // as many frames as it has steps rather than as many as it
                // has milliseconds. An entered mount keeps its clock moving,
                // so its toasts fade and its deadlines pass; every other
                // mount stands still at its state.
                //
                // Entered in a window, a mount is worked by hand: it ticks
                // on the wall clock like the window's own stage, so a late
                // frame skips ahead instead of stretching every spring —
                // on frames, a slow frame is slow motion. Replays and
                // headless runs keep the fixed step: that is what makes
                // them reproducible.
                let virtual_time = self.state.as_deref().is_some_and(|s| s.virtual_time);
                let by_hand =
                    !cfg!(headless) && self.mount && self.active && self.e2e.is_none();
                let ticking =
                    virtual_time && (self.e2e.is_some() || (self.mount && self.active));
                let mut dt_ms = FRAME_MS;
                if by_hand {
                    if let Some(state) = self.state.as_deref_mut() {
                        let now = Instant::now();
                        dt_ms = state
                            .last_frame
                            .map(|t| (now - t).as_secs_f64())
                            .unwrap_or(1.0 / 60.0)
                            .clamp(0.0, 1.0 / 20.0)
                            * 1000.0;
                        state.last_frame = Some(now);
                    }
                }
                if ticking {
                    if self.mount && self.e2e.is_some() {
                        dt_ms = self.replay_step(cx);
                    } else {
                        if let Some(mut state) = self.state.take() {
                            state.advance_clock(dt_ms / 1000.0);
                            // The manual pump, on a fixed cadence: a sync
                            // and send round every half second of virtual
                            // time, so the engine advances with the script
                            // rather than beside it. The primary counts
                            // frames — the cadence every suite's
                            // screenshots were taken on; an entered mount
                            // counts seconds, like its replay did.
                            if self.mount {
                                self.pump_if_due(&mut state);
                            } else {
                                self.frame += 1;
                                if self.frame.is_multiple_of(PUMP_EVERY) {
                                    state.pump_round();
                                }
                            }
                            self.state = Some(state);
                        }
                        self.e2e_tick(cx, dt_ms);
                    }
                    if self.e2e.is_some() {
                        self.next_frame = cx.new_next_frame();
                    }
                }
                let Some(state) = self.state.as_deref_mut() else {
                    return;
                };
                let dt = if virtual_time {
                    dt_ms / 1000.0
                } else {
                    let now = Instant::now();
                    let dt = state
                        .last_frame
                        .map(|t| (now - t).as_secs_f64())
                        .unwrap_or(1.0 / 60.0)
                        .clamp(0.0, 1.0 / 20.0);
                    state.last_frame = Some(now);
                    dt
                };
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
                let toast_now = state.world.now();
                let toast_active = match state.toast {
                    Some((_, _, since)) => {
                        if toast_now - since > 3.0 {
                            state.toast = None;
                            false
                        } else {
                            true
                        }
                    }
                    None => false,
                };
                state.animating = springs_active;
                // The curtain's spring lives outside `Anim`, so it has to ask
                // for its own frames or it would freeze after one.
                let swiping = match self.touch.mode {
                    TouchMode::RowSwipe { .. } => true,
                    _ => match self.row_swipe.as_mut() {
                        Some(rs) => {
                            rs.x.advance(dt);
                            !rs.x.is_done()
                        }
                        None => false,
                    },
                };
                if springs_active || toast_active || dragging || swiping {
                    self.next_frame = cx.new_next_frame();
                }
                if frame_log() && self.mount {
                    eprintln!(
                        "mount: tick dt {:.1} ms, springs {}, toast {}, wants a frame {}",
                        dt * 1000.0,
                        springs_active,
                        toast_active,
                        springs_active || toast_active || dragging || swiping
                    );
                }
                self.redraw_scoped(cx);
                // Mutates the world, so it runs after the frame's own
                // bookkeeping rather than in the middle of it.
                self.settle_row_swipe(cx);
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
        if self.suspended {
            cx.end_turtle_with_area(&mut self.area);
            return DrawStep::done();
        }
        // The workspace lives inside the safe area (zero on desktop). Android
        // additionally swallows touches in the notification-shade pull zone
        // at the very top of the window (~22 dp observed on gesture nav), so
        // panel headers — the drag grip, close, archive — stay below it.
        //
        // When the soft keyboard shows, makepad may slide the whole pass up
        // (the turtle's origin goes negative — android's content-shift) by
        // however much the focused field's caret rect needs: the full
        // keyboard height for the old stage-level IME's zero rect, but as
        // little as nothing for a hosted TextInput already visible near the
        // top. Compensate by the ACTUAL shift — assuming kb_h shoved the
        // whole workspace down a keyboard-height when the shift was zero —
        // and shorten by the keyboard, which occludes that much regardless.
        let vp = {
            let r = cx.turtle().rect();
            let shift = (-r.pos.y).max(0.0);
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
                r.pos.y + shift + t,
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
                        natural: line,
                        dpi,
                    };
                }
            }
        }

        self.hits.clear();
        let mut state = self.state.take();
        if let Some(state) = state.as_deref_mut() {
            let t0 = frame_log().then(Instant::now);
            match self.solo {
                Some(pid) => self.draw_solo(cx, state, vp, pid),
                None => self.draw_scene(cx, state, vp),
            }
            if let Some(t0) = t0 {
                eprintln!(
                    "{}: draw {:.2} ms, {} panels, {} hits",
                    if self.mount { "mount" } else { "superapp" },
                    t0.elapsed().as_secs_f64() * 1000.0,
                    state.ws.panels.len(),
                    self.hits.len()
                );
            }
            if !self.reported && !self.mount {
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
        // Drawn: a mount's replay may take its next step.
        self.stale_hits = false;

        cx.end_turtle_with_area(&mut self.area);
        DrawStep::done()
    }
}

// ---------------------------------------------------------------------------
// Drawing
// ---------------------------------------------------------------------------

impl Stage {
    /// A panel node (CR-006): the one panel at the whole viewport, then
    /// the sheet over it — so the archive's toast and the launcher still
    /// show.
    fn draw_solo(&mut self, cx: &mut Cx2d, state: &mut State, vp: Rect, pid: PanelId) {
        let r = rect(
            vp.pos.x + theme::GAP,
            vp.pos.y + theme::GAP,
            (vp.size.x - 2.0 * theme::GAP).max(40.0),
            (vp.size.y - 2.0 * theme::GAP).max(40.0),
        );
        if state.ws.panel(pid).is_some() {
            self.draw_panel_full(cx, state, pid, r, 1.0);
            state.store.trace_end();
        }
        self.draw_sheet(cx, state, vp);
    }

    fn draw_scene(&mut self, cx: &mut Cx2d, state: &mut State, vp: Rect) {
        // Retained widgets otherwise outlive their panels: a closed panel drops
        // from the workspace, but its entry here would linger — a slow leak,
        // and a stale instance were its id ever reused. Keep only live panels
        // and the two overlay slots (managed in `draw_overlay`).
        self.hosted.retain(|pid, _| {
            *pid == OVERLAY_PID_L || *pid == OVERLAY_PID_R || state.ws.panel(*pid).is_some()
        });
        self.hosted_for.retain(|pid, _| state.ws.panel(*pid).is_some());

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
            // A ghost has no kind left to ask, and no buttons to clear.
            self.draw_chrome(cx, r, &g.title, false, a, None, None, 0.0);
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

        self.draw_sheet(cx, state, vp);
    }

    /// The modal overlays and the toast, over whatever the stage drew.
    fn draw_sheet(&mut self, cx: &mut Cx2d, state: &mut State, vp: Rect) {
        // The problems mark: what stands in the background, in the toast's
        // corner, in the one colour — and static. A toast announced it; this
        // stays until it clears. Drawn and registered *before* the overlays'
        // wash and the locked screen: they own every hit while they are up,
        // so the mark has to sit under them, not over.
        let problems = state.problems();
        let mut lift = 0.0;
        if !problems.is_empty() {
            let msg = crate::problems::count_line(problems.len());
            let w = msg.chars().count() as f64 * self.cell.adv + 20.0;
            let h = self.cell.line_h + 10.0;
            let r = rect(
                vp.pos.x + vp.size.x - w - 12.0,
                vp.pos.y + vp.size.y - h - 12.0,
                w,
                h,
            );
            let hovered = state.hover == Some(Act::Problems);
            self.draw_panel.new_draw_call(cx);
            self.draw_panel.color = rgba_a(if hovered { theme::HOVER } else { theme::BG }, 1.0);
            self.draw_panel.border_color = rgba_a(theme::ERR, 1.0);
            self.draw_panel.border_size = 1.0;
            self.draw_panel.alpha = 1.0;
            self.draw_panel.draw_abs(cx, r);
            self.draw_mono.new_draw_call(cx);
            self.set_text(Style::Err, 1.0);
            self.draw_mono.draw_abs(cx, r.pos + dvec2(10.0, 5.0), &msg);
            self.hits.push(HitR {
                rect: r,
                act: Act::Problems,
                cursor: MouseCursor::Hand,
                label: "problems".into(),
            });
            lift = h + 6.0;
        }

        // The modal overlays share a chassis: an ink wash that owns every
        // hit (a tap outside the sheet dismisses), and the sheet on it.
        // The wash rides the chassis' presence spring, so it fades in and
        // out with the sheet; only a live overlay takes the tap.
        let up = state.overlay != Overlay::None;
        let presence = state.anim.overlay().value();
        if up || presence > 0.0 {
            if up {
                self.hits.clear();
            }
            self.draw_flat.new_draw_call(cx);
            self.draw_flat.color = rgba_a(theme::INK, 0.30 * presence);
            self.draw_flat.draw_abs(cx, vp);
            if up {
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
        }

        // The overlays are retained widgets now (CR-002 F): the shell
        // supplies their rows and owns their clicks, exactly as it does for
        // a panel's in-list controls.
        self.draw_overlay(cx, state, vp);


        // The device-sync locked screen (CR-005): when a bucket is configured
        // and this device does not hold the lease, a full-window modal owns
        // every hit and offers to take the lease. Drawn under the toast so an
        // "acquiring…" message still shows.
        if state.repl.is_some() && !state.store.is_writable() {
            self.hits.clear();
            self.draw_flat.new_draw_call(cx);
            self.draw_flat.color = rgba_a(theme::INK, 0.72);
            self.draw_flat.draw_abs(cx, vp);
            self.hits.push(HitR {
                rect: vp,
                act: Act::Noop,
                cursor: MouseCursor::Default,
                label: "locked".into(),
            });

            let role = state.repl_status.role.clone();
            let (title, btn) = match &role {
                crate::repl::Role::Free => ("the lease is free", Some("acquire")),
                crate::repl::Role::Follower { .. } => ("another device is writing", Some("take over")),
                crate::repl::Role::Stranded { .. } => ("this device has diverged", Some("recover")),
                crate::repl::Role::Offline => ("offline — the bucket is unreachable", None),
                _ => ("read-only", Some("acquire")),
            };

            let cw = 460.0_f64.min(vp.size.x - 40.0);
            let ch = 156.0;
            let card = rect(
                vp.pos.x + (vp.size.x - cw) / 2.0,
                vp.pos.y + (vp.size.y - ch) / 2.0,
                cw,
                ch,
            );
            self.draw_panel.new_draw_call(cx);
            self.draw_panel.color = rgba_a(theme::BG, 1.0);
            self.draw_panel.border_color = rgba_a(theme::INK, 1.0);
            self.draw_panel.border_size = 1.0;
            self.draw_panel.alpha = 1.0;
            self.draw_panel.draw_abs(cx, card);

            self.draw_mono.new_draw_call(cx);
            self.set_text(Style::Bold, 1.0);
            self.draw_mono.draw_abs(cx, card.pos + dvec2(20.0, 20.0), title);
            self.set_text(Style::Muted, 1.0);
            self.draw_mono.draw_abs(
                cx,
                card.pos + dvec2(20.0, 20.0 + self.cell.line_h + 6.0),
                &role.line(),
            );
            let short: String = state.repl_status.device.chars().take(8).collect();
            let device = format!("this device: {short}");
            self.draw_mono.draw_abs(
                cx,
                card.pos + dvec2(20.0, 20.0 + 2.0 * (self.cell.line_h + 6.0)),
                &device,
            );

            if let Some(btn) = btn {
                let bw = btn.chars().count() as f64 * self.cell.adv + 26.0;
                let bh = self.cell.line_h + 12.0;
                let br = rect(card.pos.x + 20.0, card.pos.y + ch - bh - 18.0, bw, bh);
                self.draw_panel.new_draw_call(cx);
                self.draw_panel.color = rgba_a(theme::INK, 1.0);
                self.draw_panel.border_size = 0.0;
                self.draw_panel.alpha = 1.0;
                self.draw_panel.draw_abs(cx, br);
                self.draw_mono.new_draw_call(cx);
                self.set_text(Style::N, 1.0);
                self.draw_mono.color = rgba_a(theme::BG, 1.0);
                self.draw_mono.draw_abs(cx, br.pos + dvec2(13.0, 6.0), btn);
                self.hits.push(HitR {
                    rect: br,
                    act: Act::Acquire,
                    cursor: MouseCursor::Hand,
                    label: btn.into(),
                });
            }
        }

        // The toast, above everything.
        if let Some((msg, err, since)) = state.toast.clone() {
            let age = state.world.now() - since;
            let a = (3.0 - age).clamp(0.0, 0.25) / 0.25;
            let wchars = msg.chars().count();
            let w = wchars as f64 * self.cell.adv + 20.0;
            let h = self.cell.line_h + 10.0;
            // Above the mark, when one stands: the mark is a click target,
            // so it keeps its place and the toast takes the row above.
            let r = rect(
                vp.pos.x + vp.size.x - w - 12.0,
                vp.pos.y + vp.size.y - h - 12.0 - lift,
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
        self.draw_label_accel(cx, x, y, s, color, alpha, None)
    }

    /// As [`Self::draw_label`], but the character at `accel` is drawn twice,
    /// nudged — the accelerator mark (CR-003). It is the grid's own fake
    /// bold, narrowed from a run to a single glyph; the letter-tracking walk
    /// this label already does makes the position free.
    #[allow(clippy::too_many_arguments)]
    fn draw_label_accel(
        &mut self,
        cx: &mut Cx2d,
        x: f64,
        y: f64,
        s: &str,
        color: theme::Rgba,
        alpha: f64,
        accel: Option<usize>,
    ) -> f64 {
        self.draw_mono.text_style.font_size = theme::LABEL_SIZE as f32;
        self.draw_mono.color = rgba_a(color, alpha);
        let step = self.cell.label_step();
        let up = s.to_uppercase();
        let mut dx = x;
        for (i, ch) in up.chars().enumerate() {
            if ch != ' ' {
                let mut buf = [0u8; 4];
                let g = ch.encode_utf8(&mut buf);
                self.draw_mono.draw_abs(cx, dvec2(dx, y), g);
                if accel == Some(i) {
                    // Three passes: labels are 8.25 pt uppercase, where the
                    // single nudge that reads as bold in body text does not
                    // register at all.
                    self.draw_mono.draw_abs(cx, dvec2(dx + 0.35, y), g);
                    self.draw_mono.draw_abs(cx, dvec2(dx + 0.7, y), g);
                }
            }
            dx += step;
        }
        dx - x
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
        // Width the header buttons will occupy on the right — the title
        // truncates to clear them. Ghosts carry no kind, so they pass 0.
        btns_w: f64,
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
        let title_cols = (((r.size.x - 16.0 - btns_w) / self.cell.label_step()).max(4.0)) as usize;
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
        // A side-effect button wears its key (CR-003); × is cmd+w and needs
        // no mark of its own.
        let accel = match &act {
            Act::Btn(_, a) => ui::btn_accel(*a).and_then(|c| ui::accel_idx(label, c)),
            _ => None,
        };
        self.draw_label_accel(cx, tx, ty, label, fg, alpha, accel);
        self.hits.push(HitR {
            rect: r,
            act,
            cursor: MouseCursor::Hand,
            label: hit_label.to_string(),
        });
    }

    /// Draws whichever modal overlay is up as a retained widget, and
    /// registers its rows as hits.
    ///
    /// The rows live in a `PortalList`, so — like a panel's in-list
    /// controls — the shell owns their clicks: real presses and scripted
    /// ones resolve through the same `Act`s the char grid used, and the
    /// widget is presentation plus (for the launcher) a real text field.
    ///
    /// The chassis' presence — wash, sheet, contents — rides one spring,
    /// 0 (away) → 1 (up): an open rises in, a close fades out with the
    /// last overlay still drawn, hit-less, until the spring has run out.
    /// Only then does its widget go, so the next opening starts clean
    /// (the launcher's field seeds and takes focus on the frame its
    /// widget is created).
    fn draw_overlay(&mut self, cx: &mut Cx2d, state: &mut State, vp: Rect) {
        use crate::panels::{OverlayProps, OverlayRowData, OVERLAY_ROW_H};
        let live = state.overlay != Overlay::None;
        state.anim.overlay().retarget(if live { 1.0 } else { 0.0 });
        let p = state.anim.overlay().value();
        if !state.anim.overlay().is_done() {
            self.next_frame = cx.new_next_frame();
        }
        if live {
            state.overlay_last = state.overlay;
        } else if p <= 0.0 {
            self.hosted.remove(&OVERLAY_PID_R);
            self.hosted.remove(&OVERLAY_PID_L);
            return;
        }
        let kind = if live { state.overlay } else { state.overlay_last };
        let launcher = kind == Overlay::Launcher;
        let hover = state.hover.clone();
        // Rows, plus the Act each one resolves to.
        let mut acts: Vec<Act> = Vec::new();
        let mut labels: Vec<String> = Vec::new();
        let mut rows: Vec<OverlayRowData> = Vec::new();
        match kind {
            Overlay::Ws => {
                for k in state.ws.roster() {
                    let ws = &state.ws.wss[k];
                    let summary = if ws.is_empty() {
                        "new".to_string()
                    } else {
                        ws.columns
                            .iter()
                            .flat_map(|c| c.panels.iter())
                            .filter_map(|pid| {
                                ws.panels.get(pid).map(|p| state.panel_title(&p.kind))
                            })
                            .collect::<Vec<_>>()
                            .join(" · ")
                    };
                    rows.push(OverlayRowData {
                        num: format!("{}", k + 1),
                        main: summary,
                        current: k == state.ws.active,
                        hovered: hover == Some(Act::WsRow(k)),
                        ..Default::default()
                    });
                    acts.push(Act::WsRow(k));
                    labels.push(format!("workspace {}", k + 1));
                }
            }
            Overlay::History => {
                let (nodes, head) = state.history.rows();
                let mut depth: HashMap<i64, usize> = HashMap::new();
                for n in &nodes {
                    let d = depth.get(&n.parent).map_or(0, |d| d + 1);
                    depth.insert(n.id, d);
                }
                for n in nodes.iter().rev() {
                    let ind = "  ".repeat((*depth.get(&n.id).unwrap_or(&0)).min(6));
                    rows.push(OverlayRowData {
                        main: format!("{ind}{}", n.label),
                        right: match n.state.as_str() {
                            "expired" => format!("{} · sent", mail::fmt_date(n.ts)),
                            _ => mail::fmt_date(n.ts),
                        },
                        current: n.id == head,
                        muted: n.state != "applied" && n.state != "expired",
                        hovered: hover == Some(Act::HistoryRow(n.id)),
                        ..Default::default()
                    });
                    acts.push(Act::HistoryRow(n.id));
                    labels.push(n.label.clone());
                }
                rows.push(OverlayRowData {
                    main: "the beginning".into(),
                    current: head == 0,
                    hovered: hover == Some(Act::HistoryRow(0)),
                    ..Default::default()
                });
                acts.push(Act::HistoryRow(0));
                labels.push("the beginning".into());
            }
            Overlay::Launcher => {
                state.launcher.hits =
                    launcher::search(&state.ws, &state.store, &state.launcher.query);
                let n = state.launcher.hits.len();
                state.launcher.sel = state.launcher.sel.min(n.saturating_sub(1));
                for (i, hit) in state.launcher.hits.iter().enumerate() {
                    rows.push(OverlayRowData {
                        main: hit.label.clone(),
                        detail: if hit.detail == hit.label {
                            String::new()
                        } else {
                            hit.detail.clone()
                        },
                        right: match hit.ws {
                            Some(k) => format!("#{}", k + 1),
                            None => "new".into(),
                        },
                        current: i == state.launcher.sel,
                        hovered: hover == Some(Act::LauncherRow(i)),
                        ..Default::default()
                    });
                    acts.push(Act::LauncherRow(i));
                    labels.push(hit.label.clone());
                }
            }
            Overlay::None => return,
        }

        // A centred sheet, hung a little below the top edge — a palette,
        // not a toolbar — that rises its last few points into place as it
        // fades in, and is as tall as its rows, up to the viewport. The
        // workspaces overlay reserves a band above it for the search row:
        // the launcher's entry on glass, which the shell draws.
        let w = (vp.size.x - 4.0 * theme::GAP).min(560.0);
        let x = vp.pos.x + (vp.size.x - w) / 2.0;
        let rise = (1.0 - p) * -12.0;
        let top = vp.pos.y + (vp.size.y * 0.14).max(2.0 * theme::GAP) + rise;
        let search_h = if kind == Overlay::Ws { 48.0 } else { 0.0 };
        let bottom = vp.pos.y + vp.size.y - 2.0 * theme::GAP;

        let tpl = if launcher {
            live_id!(launcher_overlay_tpl)
        } else {
            live_id!(rows_overlay_tpl)
        };
        let key = if launcher { OVERLAY_PID_L } else { OVERLAY_PID_R };
        let Some((widget, created)) = self.hosted_widget(cx, key, tpl) else {
            return;
        };
        if created && launcher {
            // Typing lands in the query the moment the launcher opens —
            // but key focus set during a draw pass does not take, so the
            // next event tick does it (the compose panel's lesson).
            self.pending_focus = Some(OVERLAY_PID_L);
        }
        if launcher {
            widget
                .as_launcher_overlay()
                .scroll_to(cx, state.launcher.sel);
        }

        // Fit height: the field and its rule (measured — the field's own
        // Fit walk knows; a guess serves the frame it is born on, at an
        // alpha nobody sees), the rows or the launcher's empty-state row,
        // and the frame.
        let field_h = if launcher {
            let fh = widget.widget(cx, ids!(query_input)).area().rect(cx).size.y;
            if fh > 0.0 {
                fh + 1.0
            } else {
                50.0
            }
        } else {
            0.0
        };
        let n = rows.len().max(usize::from(launcher)) as f64;
        let h = (2.0 + field_h + n * OVERLAY_ROW_H).min((bottom - top - search_h).max(80.0));
        let r = rect(x, top + search_h, w, h);

        // The sheet: white, ink-framed, fading with the chassis. Its own
        // draw call — the shader is the panel chrome's too, and a merged
        // call would paint under the wash (CR-002's sixth defect).
        self.draw_panel.new_draw_call(cx);
        self.draw_panel.color = rgba_a(theme::BG, 1.0);
        self.draw_panel.border_color = rgba_a(theme::INK, 1.0);
        self.draw_panel.border_size = 1.0;
        self.draw_panel.alpha = p as f32;
        self.draw_panel.draw_abs(cx, r);
        // The workspaces overlay's search row: a card above the roster.
        let sr = rect(x, top, w, 40.0);
        if kind == Overlay::Ws {
            self.draw_panel.draw_abs(cx, sr);
            self.draw_mono.new_draw_call(cx);
            self.set_text(Style::Muted, p);
            self.draw_mono.draw_abs(
                cx,
                dvec2(sr.pos.x + 16.0, sr.pos.y + (40.0 - self.cell.natural) / 2.0),
                "search",
            );
        }

        // The widget, inside the frame, composited at the chassis' alpha.
        let props = OverlayProps {
            rows,
            query: state.launcher.query.clone(),
            alpha: p as f32,
        };
        let mut scope = Scope::with_props(&props);
        let inner = rect(r.pos.x + 1.0, r.pos.y + 1.0, r.size.x - 2.0, r.size.y - 2.0);
        cx.begin_turtle(
            Walk::abs_rect(inner),
            Layout {
                clip_x: true,
                clip_y: true,
                ..Default::default()
            },
        );
        widget.draw_all(cx, &mut scope);
        cx.end_turtle();

        // A closing overlay takes no clicks.
        if !live {
            return;
        }

        // The rows that actually drew become hits, above the backdrop's
        // close-everything rect.
        let list_path = ids!(list);
        if let Some(list) = widget.widget(cx, list_path).as_portal_list().borrow() {
            for (idx, item) in list.items().iter() {
                let ir = item.widget.area().rect(cx);
                if ir.size.x <= 0.0 {
                    continue;
                }
                if let (Some(act), Some(label)) = (acts.get(*idx), labels.get(*idx)) {
                    self.hits.push(HitR {
                        rect: ir,
                        act: act.clone(),
                        cursor: MouseCursor::Hand,
                        label: label.clone(),
                    });
                }
            }
        }
        if launcher {
            // The query field: a real TextInput, so a click just needs to
            // reach it (the widget owns focus and caret).
            let fr = widget.widget(cx, ids!(query_input)).area().rect(cx);
            if fr.size.x > 0.0 {
                self.hits.push(HitR {
                    rect: fr,
                    act: Act::LauncherOpen,
                    cursor: MouseCursor::Text,
                    label: "search".into(),
                });
            }
        } else if kind == Overlay::Ws {
            self.hits.push(HitR {
                rect: sr,
                act: Act::LauncherOpen,
                cursor: MouseCursor::Hand,
                label: "search".into(),
            });
        }
    }

    /// Draws a panel's retained content widget inside the body rect and
    /// registers its interactive children as e2e-addressable hits.
    fn draw_hosted(&mut self, cx: &mut Cx2d, state: &State, pid: PanelId, tpl: LiveId, body: Rect) {
        let Some((mut w, mut created)) = self.hosted_widget(cx, pid, tpl) else {
            return;
        };
        let kind = state.ws.panel(pid).map(|p| p.kind.clone());
        // A panel replaced in place keeps its id, and with it the widget
        // built for what it showed before. Another template means another
        // widget; the same template under another kind — a reply
        // retargeted to a forward by the next link — means seeding again,
        // exactly as a fresh instance would. A preview re-targeting its
        // message reads its props every draw and needs neither.
        let before = self.hosted_for.get(&pid).cloned();
        if !created && before.as_ref().is_some_and(|(t, _)| *t != tpl) {
            self.hosted.remove(&pid);
            let Some((fresh, _)) = self.hosted_widget(cx, pid, tpl) else {
                return;
            };
            w = fresh;
            created = true;
        }
        let reseed = created || before.is_none_or(|(_, k)| Some(&k) != kind.as_ref());
        if let Some(k) = &kind {
            self.hosted_for.insert(pid, (tpl, k.clone()));
        }
        if reseed {
            // A compose seeds from its persisted draft — the row that is
            // *this* seed's, never one left by the kind before it — or
            // from the seed itself: the reply header, the forwarded
            // letter. It starts in a field once an event tick comes
            // (`pending_focus`).
            if let Some(Kind::Compose { seed }) = &kind {
                let d = mail::draft_for(&state.store, pid as i64, *seed)
                    .unwrap_or_else(|| mail::seed_draft(&state.store, *seed));
                w.as_compose_panel().prefill(cx, &d.to, &d.subject, &d.body);
                self.pending_focus = Some(pid);
            }
            // An inbox with a baked filter param seeds its field.
            if let Some(Kind::Inbox { filter: Some(f) }) = &kind {
                w.widget(cx, ids!(filter_input))
                    .as_text_input()
                    .set_text(cx, f);
            }
        }
        let props = crate::panels::PanelProps {
            store: state.store.clone(),
            registry: state.world.registry_rc(),
            pid,
            kind: kind.clone().unwrap_or(Kind::About),
            expand: state.expand.get(&pid).cloned(),
            problems: kind
                .as_ref()
                .map_or_else(Default::default, |k| state.problems_for(k)),
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
        // Inside the panel's own clipped turtle, so a curtain over a row at
        // the edge of the list is cut off with everything else.
        if matches!(kind, Some(Kind::Inbox { .. })) {
            self.draw_row_swipe(cx, &w, pid);
        }
        cx.end_turtle();

        // The e2e bridge: known interactive children become labelled hits;
        // a click on one synthesizes real pointer events at its centre
        // (fields) or resolves semantically (buttons).
        let mut reg: Vec<(String, Rect, Act)> = Vec::new();
        match &kind {
            Some(Kind::Settings) => {
                let lr = w.widget(cx, ids!(add_link)).area().rect(cx);
                if lr.size.x > 0.0 {
                    reg.push((
                        "add account".to_string(),
                        lr,
                        Act::Open(pid, Kind::AddAccount),
                    ));
                }
                let accounts = mail::accounts(&state.store);
                if let Some(list) =
                    w.widget(cx, ids!(accounts_list)).as_portal_list().borrow()
                {
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
                        // The row's selectable runs (CR-003).
                        for (path, text) in [
                            (ids!(email_lbl), accounts.get(*idx).map(|a| a.email.clone())),
                            (
                                ids!(host_lbl),
                                accounts.get(*idx).map(|a| {
                                    a.imap_host
                                        .clone()
                                        .filter(|h| !h.is_empty())
                                        .unwrap_or_else(|| "local demo".into())
                                }),
                            ),
                        ] {
                            let rr = item.widget.widget(cx, path).area().rect(cx);
                            if rr.size.x > 0.0 {
                                if let Some(t) = text {
                                    reg.push((t, rr, Act::Pointer(pid)));
                                }
                            }
                        }
                    }
                }
            }
            Some(Kind::AddAccount) => {
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
            }
            Some(Kind::Compose { .. }) => {
                for (label, path) in [
                    ("to", ids!(to_input)),
                    ("subject", ids!(subject_input)),
                    ("body", ids!(body_input)),
                ] {
                    let r = w.widget(cx, path).area().rect(cx);
                    if r.size.x > 0.0 {
                        reg.push((label.to_string(), r, Act::Pointer(pid)));
                    }
                }
                // The TO field's autocomplete rows, registered after the
                // fields they cover: `hit_at` searches back to front, so
                // the box wins where they overlap.
                let hits = w.as_compose_panel().suggestion_hits(cx);
                for (i, (label, r)) in hits.into_iter().enumerate() {
                    reg.push((label, r, Act::WidgetOp(pid, WidgetOp::Suggest(i))));
                }
            }
            Some(Kind::Inbox { .. }) => {
                let fr = w.widget(cx, ids!(filter_input)).area().rect(cx);
                if fr.size.x > 0.0 {
                    reg.push(("filter".to_string(), fr, Act::Pointer(pid)));
                }
                // Visible rows. These rects are the REAL click path too, not
                // just the scripts' door: list items are rebuilt per draw,
                // so a down/up pair inside them dies on any mid-gesture
                // redraw. A row is ONE target, addressed by its subject:
                // anywhere on either line opens it. Splitting it (a select
                // band beside an open band) made the from name and the date
                // look dead — a row means its mail, whichever line you hit.
                let panel = w.as_inbox_panel();
                let swiping = self.row_swipe.as_ref().filter(|rs| rs.pid == pid).map(|rs| rs.id);
                if let Some(list) = w.widget(cx, ids!(list)).as_portal_list().borrow() {
                    for (idx, item) in list.items().iter() {
                        let r = item.widget.area().rect(cx);
                        if r.size.x > 0.0 {
                            if let Some(t) = panel.row_at(&state.store, *idx) {
                                // A row with a curtain over it answers to
                                // nothing: it is on its way out, and a tap
                                // landing on it would open the thread being
                                // filed. Its rect still counts — that is
                                // where the curtain is drawn.
                                if swiping == Some(t.target) {
                                    if let Some(rs) = self.row_swipe.as_mut() {
                                        rs.slot = r;
                                    }
                                    continue;
                                }
                                reg.push((
                                    t.topic.clone(),
                                    r,
                                    Act::WidgetOp(pid, WidgetOp::OpenMail(t.target)),
                                ));
                            }
                        }
                    }
                }
                // The autocomplete's rows, registered after the mail rows
                // they cover: `hit_at` searches back to front, so the box
                // wins where they overlap.
                for (i, (label, r)) in panel.suggestion_hits(cx).into_iter().enumerate() {
                    reg.push((label, r, Act::WidgetOp(pid, WidgetOp::Suggest(i))));
                }
            }
            Some(Kind::Message { id }) => {
                let id = *id;
                // The thread's rows (CR-007). A closed row is one target,
                // addressed by its sender and the line it previews:
                // touching it opens the message in place. An open one is
                // the same row, addressed by sender and date: touching it
                // closes the message — except on the contact link, which
                // is registered after it and so wins where they overlap.
                // The readings are selectable runs, registered like any
                // hosted field; `mail html` is the same run in its other
                // reading, and only one of the two is ever visible.
                let panel = w.as_message_panel();
                for h in panel.msg_hits(cx) {
                    let label = if h.open {
                        format!("{} · {}", h.name, h.date)
                    } else {
                        format!("{}: {}", h.name, h.preview)
                    };
                    reg.push((label, h.head, Act::WidgetOp(pid, WidgetOp::ToggleMail(h.id))));
                    if let Some(r) = h.link {
                        reg.push((
                            format!("{} <{}>", h.name, h.email),
                            r,
                            Act::Open(pid, Kind::Contact { email: h.email.clone() }),
                        ));
                    }
                    if let Some(r) = h.quote {
                        reg.push((
                            format!("quoted · {}", h.date),
                            r,
                            Act::WidgetOp(pid, WidgetOp::ToggleQuote(h.id)),
                        ));
                    }
                    if let Some(r) = h.text {
                        reg.push(("mail body".to_string(), r, Act::Pointer(pid)));
                    }
                    if let Some(r) = h.html {
                        reg.push(("mail html".to_string(), r, Act::Pointer(pid)));
                    }
                }
                let r = w.widget(cx, ids!(to_lbl)).area().rect(cx);
                if r.size.x > 0.0 {
                    reg.push(("mail to".to_string(), r, Act::Pointer(pid)));
                }
                let newest = mail::thread(&state.store, id)
                    .last()
                    .map_or(id, |t| t.mail.head.id);
                for (label, path, seed) in [
                    ("forward", ids!(forward_link), Seed::Forward(newest)),
                    ("reply", ids!(reply_link), Seed::Reply(newest)),
                ] {
                    let r = w.widget(cx, path).area().rect(cx);
                    if r.size.x > 0.0 {
                        reg.push((label.to_string(), r, Act::Open(pid, Kind::Compose { seed })));
                    }
                }
            }
            Some(Kind::Problems) => {
                // The rows' controls, by what they do: an account's *sync*
                // wears its address (the inbox has a *sync* too), and the
                // link to settings; a send's *retry* and *reopen*.
                let problems = state.problems();
                if let Some(list) = w.widget(cx, ids!(list)).as_portal_list().borrow() {
                    for (idx, item) in list.items().iter() {
                        let Some(p) = problems.get(*idx) else {
                            continue;
                        };
                        let lr = item.widget.widget(cx, ids!(label_lbl)).area().rect(cx);
                        if lr.size.x > 0.0 {
                            reg.push((p.label.clone(), lr, Act::Pointer(pid)));
                        }
                        match &p.source {
                            crate::problems::Source::Account { id, email } => {
                                let r = item.widget.button(cx, ids!(sync_btn)).area().rect(cx);
                                if r.size.x > 0.0 {
                                    reg.push((
                                        format!("sync {email}"),
                                        r,
                                        Act::WidgetOp(pid, WidgetOp::SyncAccount(*id)),
                                    ));
                                }
                                let r = item.widget.widget(cx, ids!(settings_link)).area().rect(cx);
                                if r.size.x > 0.0 {
                                    reg.push(("settings".to_string(), r, Act::Open(pid, Kind::Settings)));
                                }
                            }
                            crate::problems::Source::Send {
                                outbox,
                                given_up: true,
                                ..
                            } => {
                                let r = item.widget.button(cx, ids!(retry_btn)).area().rect(cx);
                                if r.size.x > 0.0 {
                                    reg.push((
                                        "retry".to_string(),
                                        r,
                                        Act::WidgetOp(pid, WidgetOp::RetrySend(*outbox)),
                                    ));
                                }
                                let r = item.widget.widget(cx, ids!(reopen_link)).area().rect(cx);
                                if r.size.x > 0.0 {
                                    reg.push((
                                        "reopen".to_string(),
                                        r,
                                        Act::WidgetOp(pid, WidgetOp::ReopenSend(*outbox)),
                                    ));
                                }
                            }
                            crate::problems::Source::Send { .. } | crate::problems::Source::Sync => {}
                        }
                    }
                }
            }
            Some(Kind::Effects) => {
                let fr = w.widget(cx, ids!(filter_input)).area().rect(cx);
                if fr.size.x > 0.0 {
                    reg.push(("filter".to_string(), fr, Act::Pointer(pid)));
                }
                // A row is ONE target, addressed by the sentence it shows —
                // the same string the row draws, so a script and a reader
                // name it the same way. Touching it previews the job.
                let panel = w.as_effects_panel();
                for h in panel.row_hits(cx) {
                    reg.push((h.label, h.rect, Act::WidgetOp(pid, WidgetOp::OpenJob(h.id))));
                }
                for (i, (label, r)) in panel.suggestion_hits(cx).into_iter().enumerate() {
                    reg.push((label, r, Act::WidgetOp(pid, WidgetOp::Suggest(i))));
                }
            }
            Some(Kind::Job { .. }) => {
                // The whole panel is selectable runs; nothing in it navigates.
                for (label, r) in w.as_job_panel().runs(cx) {
                    reg.push((label, r, Act::Pointer(pid)));
                }
            }
            Some(Kind::Contact { email }) => {
                let r = w.widget(cx, ids!(from_link)).area().rect(cx);
                if r.size.x > 0.0 {
                    let (name, _) = mail::contact(&state.store, email);
                    let first = name.split(' ').next().unwrap_or(&name).to_lowercase();
                    reg.push((
                        format!("messages from {first}"),
                        r,
                        Act::Open(pid, Kind::Inbox { filter: Some(email.clone()) }),
                    ));
                }
            }
            _ => {}
        }
        for (label, r, act) in reg {
            // The hand promises a click does something. Text keeps that
            // promise honest: the fields and the read-only selectable runs
            // (CR-003) answer to the drag, not the click, so they take the
            // beam — which is also the only hint that they can be copied.
            let cursor = match act {
                Act::Pointer(_) => MouseCursor::Text,
                _ => MouseCursor::Hand,
            };
            self.hits.push(HitR {
                rect: r,
                act,
                cursor,
                label,
            });
        }
    }

    /// The swipe curtain (CR-005): an ink panel wiping across the row under
    /// the finger, carrying the name of what a lift would do.
    ///
    /// It enters from the edge the finger travels *away* from, which is the
    /// edge that action's button occupies in a message header — swipe left
    /// and `archive` comes in from the right, exactly where the header draws
    /// it. Below the commit threshold it is a grey wash with ink lettering;
    /// past it the whole thing inverts, the same way a header button inverts
    /// under the pointer. No colour, and nothing to read but the word.
    fn draw_row_swipe(&mut self, cx: &mut Cx2d, w: &WidgetRef, pid: PanelId) {
        let Some(rs) = self.row_swipe.as_ref().filter(|rs| rs.pid == pid) else {
            return;
        };
        let (dx, armed, slot) = (rs.x.value(), rs.armed(), rs.slot);
        if slot.size.x <= 0.0 || dx.abs() < 0.5 {
            return;
        }
        // Clip to the list: a row scrolled half under the pinned header has a
        // rect that reaches above it, and the curtain must not.
        let list = w.widget(cx, ids!(list)).area().rect(cx);
        let (top, bot) = (
            slot.pos.y.max(list.pos.y),
            (slot.pos.y + slot.size.y).min(list.pos.y + list.size.y),
        );
        if bot <= top {
            return;
        }
        let width = dx.abs().min(slot.size.x);
        // Negative dx means the finger went left: archive, entering right.
        let (x, label) = if dx < 0.0 {
            (slot.pos.x + slot.size.x - width, "archive")
        } else {
            (slot.pos.x, "delete")
        };
        let (bg, fg) = if armed {
            (theme::INK, theme::BG)
        } else {
            (theme::SEL, theme::INK)
        };
        // A distinct draw call, or these quads merge into the chrome's and
        // paint under the panel they belong to (see `panels.rs`' portal-item
        // note — the same shader-merge trap).
        self.draw_flat.new_draw_call(cx);
        self.draw_flat.color = rgba_a(bg, 1.0);
        self.draw_flat.draw_abs(cx, rect(x, top, width, bot - top));
        // The leading edge, as a hairline. Without it a curtain that has not
        // armed yet is the *same* grey as the selection wash, so swiping the
        // row the cursor is standing on would show nothing at all until it
        // inverted. An ink edge being pulled across reads on any backing.
        if !armed {
            let ex = if dx < 0.0 { x } else { x + width - 1.0 };
            self.draw_flat.color = rgba_a(theme::INK, 1.0);
            self.draw_flat.draw_abs(cx, rect(ex, top, 1.0, bot - top));
        }
        // The word is pinned to the entering edge, so it holds still while
        // the curtain grows past it — and stands down until there is room:
        // half a word reads as a glitch, not as a hint.
        //
        // Measured by the step `draw_label` actually walks, not by
        // `label_w`, which trims the trailing tracking: aligning to a width
        // the drawing does not use pushes the run off its own edge.
        const SWIPE_PAD: f64 = 10.0;
        let tw = self.cell.label_step() * label.chars().count() as f64;
        if width >= tw + 2.0 * SWIPE_PAD {
            let tx = if dx < 0.0 {
                x + width - tw - SWIPE_PAD
            } else {
                x + SWIPE_PAD
            };
            let ty = slot.pos.y + (slot.size.y - self.cell.label_line()) / 2.0;
            self.draw_mono.new_draw_call(cx);
            self.draw_label(cx, tx, ty, label, fg, 1.0);
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
        let btns_w = self.cell.head_btns_w(&kind);
        self.draw_chrome(cx, r, &title, focused, alpha, Some(pid), hover.as_ref(), btns_w);

        // Extra header actions, right to left from the close button —
        // side effects live in the chrome, never floating in content.
        let head_btns = ui::head_btns(&kind);
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

        // The body: the rect the retained content draws into.
        let body = rect(
            r.pos.x + 1.0,
            r.pos.y + theme::HEAD_H,
            r.size.x - 2.0,
            (r.size.y - theme::HEAD_H - 1.0).max(0.0),
        );
        if body.size.y < 4.0 {
            return;
        }
        // All content is retained now (CR-002 F): every kind has a widget
        // template, so a panel body is a widget tree. Chrome above still
        // fades; the content pops — the pilot's accepted trade.
        if let Some(tpl) = hosted_tpl(&kind) {
            self.draw_hosted(cx, state, pid, tpl, body);
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
    /// The panels library is up over the workspace (Dev → Panels Library,
    /// ⇧⌘L; or `--library` from the start).
    #[rust]
    library_shown: bool,
}

impl App {
    /// Puts the panels library up over the workspace, or away again. The
    /// stage underneath is suspended rather than torn down — its store,
    /// sync and script keep running — and comes up on first need: opened
    /// on the library, the window has no workspace until asked.
    fn show_library(&mut self, cx: &mut Cx, on: bool) {
        self.library_shown = on;
        let stage = self.ui.widget(cx, ids!(stage));
        let library = self.ui.widget(cx, ids!(library));
        if on {
            if let Some(mut st) = stage.borrow_mut::<Stage>() {
                st.set_suspended(cx, true);
            }
            if let Some(mut lib) = library.borrow_mut::<crate::library::Library>() {
                lib.show(cx);
            }
        } else {
            if let Some(mut lib) = library.borrow_mut::<crate::library::Library>() {
                lib.hide(cx);
            }
            let boot = stage
                .borrow::<Stage>()
                .is_some_and(|st| !st.booted())
                .then(|| Boot::primary(cx));
            if let Some(mut st) = stage.borrow_mut::<Stage>() {
                st.set_suspended(cx, false);
                if let Some(boot) = boot {
                    st.boot(cx, boot);
                }
            }
        }
        cx.redraw_all();
    }
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
        match event {
            // Opened on the library: it is up from the first frame, and the
            // workspace stays unbooted until the toggle asks for it. The
            // menu bar gets the Dev menu now — the stage that usually builds
            // the menus has not booted, and without it the toggle would have
            // no item to live in.
            Event::Startup if library_filter().is_some() => {
                dev_menu(cx);
                self.show_library(cx, true);
            }
            Event::Actions(actions)
                if actions
                    .iter()
                    .any(|a| a.downcast_ref::<DevAction>().is_some()) =>
            {
                let on = !self.library_shown;
                self.show_library(cx, on);
            }
            _ => {}
        }
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
    main();
}

app_main!(App);
