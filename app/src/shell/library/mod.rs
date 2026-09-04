//! A zoomable canvas of live scenes from [`catalog`].
//!
//! Each node mounts a fixture, or a whole [`Stage`] on a session of its own,
//! in its own render pass. Entering a node flies the camera to it at 1:1 and
//! routes the keyboard and the pointer to that mount alone; everything else
//! on the canvas is a picture of the state it reached.
//!
//! The canvas names no app: it reads the catalogue, which asks the app list.
//!
//! This half is the state and the input: the mounts and how they boot, the
//! camera, what a click and a chord mean, and the canvas's own e2e bridge.
//! The paint — the render budget, the blocks, the mounts' textures — is in
//! `paint`.

mod paint;

use std::collections::HashMap;
use std::rc::Rc;

use kernel::caps::Screen;
use kernel::e2e::{self, Step};
use kernel::layout::Grid;
use kernel::scene::{self, Canvas, Metrics, Scene, TEXT_PT, TITLE_PT};
use kernel::spring::{Spring, SpringParams};
use kernel::store::Store;
use kernel::theme;
use makepad_widgets::makepad_platform::event::{
    MouseDownEvent, MouseMoveEvent, MouseUpEvent, ScrollEvent,
};
use makepad_widgets::*;

use super::boot::{self, Boot, RealScreen, FRAME_MS};
use super::catalog::{self, Open, Populate, Setup};
use super::draw::DrawFlat;
use super::dsl::OverlayProps;
use super::keys::ChordExec;
use super::stage::Stage;

/// What the Dev chord asks of the window: the library is the stage's
/// sibling, not its child, so the root acts on it.
#[derive(Debug, Clone)]
pub enum DevAction {
    /// Show the panels library over the workspace, or put it away.
    ToggleLibrary,
}

script_mod! {
    use mod.prelude.widgets.*
    use mod.widgets.*

    // A mount renders into its own pass; this quad shows that pass's
    // texture on the canvas, at whatever zoom.
    set_type_default() do #(DrawTex::script_shader(vm)){
        ..mod.draw.DrawQuad
        image: texture_2d(float)
        pixel: fn() {
            return self.image.sample_as_bgra(self.pos)
        }
    }

    // An arrowhead: a solid triangle pointing right, filling its quad.
    set_type_default() do #(DrawHead::script_shader(vm)){
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

    mod.widgets.Library = set_type_default() do #(Library::register_widget(vm)) {
        width: Fill
        height: Fill
        draw_mono +: {
            text_style: mod.widgets.SMonoStyle{}
            color: #141414ff
        }
    }
}

/// A quad that shows a mount's pass texture.
#[derive(Script, ScriptHook)]
#[repr(C)]
pub struct DrawTex {
    #[deref]
    draw_super: DrawQuad,
}

/// An arrowhead.
#[derive(Script, ScriptHook)]
#[repr(C)]
pub struct DrawHead {
    #[deref]
    draw_super: DrawQuad,
    #[live]
    pub color: Vec4f,
}

/// Zoom bounds, as log2.
const Z_MIN: f64 = -6.0;
const Z_MAX: f64 = 2.0;
/// One keyboard zoom step, log2.
const Z_STEP: f64 = 0.5;
/// One arrow-key pan, screen points.
const PAN_STEP: f64 = 240.0;
/// The dpi a stage is drawn at while it replays, as a fraction of the
/// window's. A step needs fresh hits, not pixels: layout and hits are in
/// logical points whatever the dpi, so a replay draws small.
const DPI_REPLAY_FRACTION: f64 = 0.25;
/// What one frame may spend rendering mounts that are not live: a time
/// slice windowed, a count under a headless build (whose frames are
/// virtual, and whose runs must stay reproducible).
const RENDER_MS: f64 = 8.0;
const RENDER_COUNT: u32 = 6;
/// Frames the zoom has to stand still before frozen mounts re-render at the
/// new level; until then they show their last texture, scaled.
const SETTLE_TICKS: u32 = 6;
/// Below this natural size (screen points) node names are left out: they are
/// clamped to a legible minimum, and far out that minimum no longer fits
/// between one node and the next.
const NAME_MIN_PT: f64 = 5.0;

/// The frame's render budget. One render is always allowed, so there is
/// progress even when a single one overruns.
struct Budget {
    started: std::time::Instant,
    spent: u32,
}

impl Budget {
    fn new() -> Budget {
        Budget {
            started: std::time::Instant::now(),
            spent: 0,
        }
    }

    fn ok(&self) -> bool {
        if self.spent == 0 {
            return true;
        }
        if cfg!(headless) {
            self.spent < RENDER_COUNT
        } else {
            self.started.elapsed().as_secs_f64() * 1000.0 < RENDER_MS
        }
    }

    fn spend(&mut self) {
        self.spent += 1;
    }
}

/// The camera: the canvas point at the viewport's top-left, and log2 zoom.
struct Camera {
    x: Spring,
    y: Spring,
    z: Spring,
}

impl Camera {
    fn at(x: f64, y: f64, z: f64) -> Camera {
        let p = SpringParams::movement();
        Camera {
            x: Spring::at_rest(x, p),
            y: Spring::at_rest(y, p),
            z: Spring::at_rest(z, p),
        }
    }

    fn zoom(&self) -> f64 {
        2f64.powf(self.z.value())
    }

    fn pos(&self) -> DVec2 {
        dvec2(self.x.value(), self.y.value())
    }

    fn advance(&mut self, dt: f64) -> bool {
        self.x.advance(dt);
        self.y.advance(dt);
        self.z.advance(dt);
        !(self.x.is_done() && self.y.is_done() && self.z.is_done())
    }
}

/// What lives in a mount: a component, or a stage.
#[derive(Clone)]
enum Live {
    /// A bare widget, populated once with its fixture.
    Widget(WidgetRef),
    /// A stage on a world of its own.
    Stage(WidgetRef),
}

/// One node's mount. A component is instantiated on the first draw; a stage
/// is booted the first time it is the one replaying, or entered, so opening
/// the canvas costs no stores at all and each frame boots at most one.
struct Mount {
    live: Option<Live>,
    scene: usize,
    node: usize,
    /// The node's viewport, points.
    size: DVec2,
    pass: Option<MountPass>,
    /// The dpi factor the pass was last rendered at; zero before the first.
    dpi: f64,
    /// The mount drew (or stepped) since the pass was last rendered. Held
    /// here rather than in makepad's redraw marks, which a draw event
    /// consumes whether or not the budget let this mount render.
    pending: bool,
}

struct MountPass {
    pass: DrawPass,
    tex: Texture,
    list: DrawList2d,
}

impl MountPass {
    fn new(cx: &mut Cx) -> MountPass {
        let pass = DrawPass::new(cx);
        let tex = Texture::new_with_format(
            cx,
            TextureFormat::RenderBGRAu8 {
                size: TextureSize::Auto,
                initial: true,
            },
        );
        pass.set_color_texture(
            cx,
            &tex,
            DrawPassClearColor::ClearWith(vec4(1.0, 1.0, 1.0, 1.0)),
        );
        MountPass {
            pass,
            tex,
            list: DrawList2d::new(cx),
        }
    }
}

/// What a click on the canvas does.
#[derive(Clone, Copy, PartialEq)]
enum HitAct {
    /// Enter this mount.
    Enter(usize),
    /// Fit this scene's block.
    Scene(usize),
}

struct Hit {
    label: String,
    rect: Rect,
    act: HitAct,
}

/// A drag-pan in progress: where it started, and where the camera was.
struct Drag {
    start: DVec2,
    cam: DVec2,
}

/// The canvas widget.
#[derive(Script, Widget)]
pub struct Library {
    #[uid]
    uid: WidgetUid,
    #[walk]
    walk: Walk,
    #[layout]
    layout: Layout,

    #[redraw]
    #[live]
    draw_flat: DrawFlat,
    #[live]
    draw_tex: DrawTex,
    #[live]
    draw_head: DrawHead,
    #[live]
    draw_mono: DrawText,

    /// The templates by DSL name: the component widgets' and the stage's,
    /// never auto-drawn, instantiated per node.
    #[rust]
    tpl: HashMap<LiveId, ScriptObjectRef>,
    /// Shared with the draw loop, which borrows the widget mutably.
    #[rust]
    scenes: Rc<Vec<Scene<Setup>>>,
    #[rust]
    canvas: Option<Canvas>,
    #[rust]
    mounts: Vec<Mount>,
    #[rust]
    cam: Option<Camera>,
    #[rust]
    entered: Option<usize>,
    #[rust]
    drag: Option<Drag>,
    /// Screen rects the canvas answers clicks (and the script) on.
    #[rust]
    hits: Vec<Hit>,
    /// The canvas's own script (`--library --e2e`).
    #[rust]
    e2e: Option<e2e::Runner>,
    #[rust]
    next_frame: NextFrame,
    #[rust]
    area: Area,
    #[rust]
    vp: Rect,
    #[rust]
    metrics: Option<Metrics>,
    #[rust]
    booted: bool,
    /// The draw list the canvas draws into — what a mount marks alongside
    /// its own, so its redraw reaches the compositor.
    #[rust]
    list_id: Option<DrawListId>,
    #[rust]
    last_frame: Option<std::time::Instant>,
    /// Where the pointer was last seen: deferred re-renders go nearest
    /// first.
    #[rust]
    pointer: Option<DVec2>,
    /// Frames since the zoom last changed, and the zoom it was checked at.
    #[rust]
    zoom_ticks: u32,
    #[rust]
    last_zoom: f64,
    /// Frames since boot, and whether the fill-in has been reported.
    #[rust]
    frames: u64,
    #[rust]
    filled: bool,
    /// The last draw left renders undone — over budget, or waiting for the
    /// zoom to settle — so keep the frames coming.
    #[rust]
    more_work: bool,
    /// Up over the workspace. Off, the canvas draws nothing and hears
    /// nothing; its mounts keep their state for the next time.
    #[rust]
    shown: bool,
    /// `SUPERAPP_FRAME_LOG`: what one frame cost, how much of it went into
    /// mount passes, and what came in since the last one.
    #[rust]
    mount_ms: f64,
    #[rust]
    renders: u32,
    #[rust]
    since_draw: Vec<(&'static str, u32)>,
    #[rust]
    last_draw: Option<std::time::Instant>,
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
        Event::Timer(_) => "timer",
        Event::Signal => "signal",
        Event::Actions(_) => "actions",
        _ => "other",
    }
}

impl ScriptHook for Library {
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
        // Named children are templates, never auto-drawn — the stage's own
        // pattern for its panels.
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

fn to_rect(r: kernel::layout::Rect) -> Rect {
    Rect {
        pos: dvec2(r.x, r.y),
        size: dvec2(r.w, r.h),
    }
}

fn intersects(a: Rect, b: Rect) -> bool {
    a.pos.x < b.pos.x + b.size.x
        && b.pos.x < a.pos.x + a.size.x
        && a.pos.y < b.pos.y + b.size.y
        && b.pos.y < a.pos.y + a.size.y
}

/// The zoom a mount is rendered at: the canvas zoom snapped to a quarter
/// octave, so a wheel gesture does not re-render every mount on every notch
/// and the texture is never more than a few percent off its on-screen size.
fn render_zoom(zoom: f64) -> f64 {
    2f64.powf((zoom.log2() * 4.0).round() / 4.0)
}

/// The dpi a mount's pass renders at. A replaying stage draws for its hits,
/// small, whatever the canvas shows; anything else draws at the zoom it is
/// shown at, so its text is crisp there.
fn mount_dpi(win_dpi: f64, zoom: f64, replaying: bool) -> f64 {
    if replaying {
        win_dpi * DPI_REPLAY_FRACTION
    } else {
        win_dpi * render_zoom(zoom)
    }
}

/// The status strip's height, screen points. It carries the count of nodes
/// still on their way and nothing else — the controls are not written out,
/// as nothing else in this app writes its keys on a strip.
fn status_h() -> f64 {
    (theme::FONT_SIZE * 2.4).round()
}

/// The band a fit leaves at the top of the viewport. Scene titles and node
/// names are laid in screen space at a legible minimum, so far out they stop
/// shrinking with the canvas and would sit off the top of it; this is the
/// room they need. It is also what keeps an entered node's own name on
/// screen beside it.
fn caption_band() -> f64 {
    ((TITLE_PT + TEXT_PT) * 1.3 + 12.0).round()
}

/// A canvas chord, for the script and the keyboard alike. Anything this
/// cannot spell is offered to the entered mount through
/// [`keys::parse_chord`](super::keys::parse_chord), which knows the whole
/// grammar.
fn chord(s: &str) -> Option<KeyEvent> {
    let mut modifiers = KeyModifiers::default();
    let mut key = None;
    for part in s.split('+') {
        match part {
            "cmd" | "logo" | "super" => modifiers.logo = true,
            "shift" => modifiers.shift = true,
            "alt" | "option" => modifiers.alt = true,
            "ctrl" | "control" => modifiers.control = true,
            k => key = Some(k),
        }
    }
    if !modifiers.logo {
        // Only the canvas's own chords are cmd-modified; a plain key is the
        // entered mount's, and goes the other way.
        return None;
    }
    let key_code = match key? {
        "=" | "equals" | "plus" => KeyCode::Equals,
        "-" | "minus" => KeyCode::Minus,
        "0" => KeyCode::Key0,
        "esc" | "escape" => KeyCode::Escape,
        "l" if modifiers.shift => KeyCode::KeyL,
        _ => return None,
    };
    Some(KeyEvent {
        key_code,
        modifiers,
        is_repeat: false,
        time: 0.0,
    })
}

/// The same event, with its pointer position mapped from the window into a
/// mount's own coordinates — `None` for events that carry none.
fn remap(event: &Event, origin: DVec2, zoom: f64) -> Option<Event> {
    let f = |p: DVec2| (p - origin) / zoom;
    Some(match event {
        Event::MouseDown(e) => Event::MouseDown(MouseDownEvent {
            abs: f(e.abs),
            ..e.clone()
        }),
        Event::MouseMove(e) => Event::MouseMove(MouseMoveEvent {
            abs: f(e.abs),
            ..e.clone()
        }),
        Event::MouseUp(e) => Event::MouseUp(MouseUpEvent {
            abs: f(e.abs),
            ..e.clone()
        }),
        Event::Scroll(e) => Event::Scroll(ScrollEvent {
            abs: f(e.abs),
            ..e.clone()
        }),
        _ => return None,
    })
}

/// What a mount needs to come up, taken out of its node's setup so the
/// scenes are not borrowed while it boots.
enum Plan {
    Widget {
        tpl: LiveId,
        populate: Populate,
    },
    Stage {
        open: Option<Open>,
        solo: bool,
        steps: Option<Vec<Step>>,
        grid: Option<Grid>,
        mode: kernel::app::Mode,
    },
}

impl Library {
    // -- boot -------------------------------------------------------------------

    /// Puts the canvas up: the first time, it reads the catalogue and lays
    /// one mount per node.
    pub fn show(&mut self, cx: &mut Cx) {
        if !self.booted {
            self.boot(cx);
        }
        self.shown = true;
        self.next_frame = cx.new_next_frame();
        cx.set_key_focus(self.area);
        cx.redraw_all();
    }

    /// Puts the canvas away. An entered mount is left first, so it gives the
    /// keyboard back.
    pub fn hide(&mut self, cx: &mut Cx) {
        self.leave(cx);
        self.shown = false;
        cx.redraw_all();
    }

    /// Reads the catalogue and lays one mount per node.
    fn boot(&mut self, cx: &mut Cx) {
        let filter = boot::library_filter().unwrap_or(&[]);
        let mut scenes = catalog::scenes();
        if !filter.is_empty() {
            let wanted: Vec<String> = filter.iter().map(|f| f.to_lowercase()).collect();
            scenes.retain(|s| {
                let name = s.name.to_lowercase();
                wanted.iter().any(|w| name.contains(w))
            });
        }
        if scenes.is_empty() {
            let all: Vec<String> = catalog::scenes().iter().map(|s| s.name.clone()).collect();
            eprintln!("library: no scene named like {filter:?}; the catalogue has {all:?}");
            std::process::exit(2);
        }
        for s in &scenes {
            if let Err(e) = s.check() {
                eprintln!("library: {e}");
                std::process::exit(2);
            }
        }
        for (si, s) in scenes.iter().enumerate() {
            for (ni, n) in s.nodes.iter().enumerate() {
                self.mounts.push(Mount {
                    live: None,
                    scene: si,
                    node: ni,
                    size: dvec2(n.size.0, n.size.1),
                    pass: None,
                    dpi: 0.0,
                    pending: true,
                });
            }
        }
        eprintln!(
            "library: {} scene{} on the canvas, {} nodes",
            scenes.len(),
            if scenes.len() == 1 { "" } else { "s" },
            self.mounts.len()
        );
        self.scenes = Rc::new(scenes);
        // The canvas's own script — when the window opened on the canvas;
        // otherwise the script is the stage's suite.
        let (script, out) = boot::e2e_script();
        if let Some(path) = script.filter(|_| boot::library_filter().is_some()) {
            match std::fs::read_to_string(path)
                .map_err(|e| e.to_string())
                .and_then(|s| e2e::parse(&s))
            {
                Ok(steps) => {
                    let out = std::path::PathBuf::from(out);
                    let _ = std::fs::create_dir_all(&out);
                    eprintln!("e2e: {} step(s) from {path}", steps.len());
                    self.e2e = Some(e2e::Runner::new(steps, out));
                }
                Err(e) => {
                    eprintln!("e2e: {path}: {e}");
                    std::process::exit(2);
                }
            }
        }
        self.booted = true;
        self.next_frame = cx.new_next_frame();
        cx.redraw_all();
    }

    fn is_stage(&self, i: usize) -> bool {
        let m = &self.mounts[i];
        matches!(
            self.scenes[m.scene].nodes[m.node].setup,
            Setup::Stage { .. }
        )
    }

    fn plan(&self, i: usize) -> Plan {
        let m = &self.mounts[i];
        match &self.scenes[m.scene].nodes[m.node].setup {
            Setup::Widget { tpl, populate, .. } => Plan::Widget {
                tpl: *tpl,
                populate: populate.clone(),
            },
            Setup::Stage {
                open,
                solo,
                steps,
                grid,
                mode,
            } => Plan::Stage {
                open: open.clone(),
                solo: *solo,
                steps: steps.clone(),
                grid: *grid,
                mode: *mode,
            },
        }
    }

    /// The sheet props a component mount draws and hears with, if it is one.
    fn overlay_props(&self, i: usize) -> Option<OverlayProps> {
        let m = &self.mounts[i];
        match &self.scenes[m.scene].nodes[m.node].setup {
            Setup::Widget { overlay, .. } => overlay.clone(),
            Setup::Stage { .. } => None,
        }
    }

    fn tag(&self, i: usize) -> String {
        let m = &self.mounts[i];
        format!(
            "{}/{}: ",
            self.scenes[m.scene].name, self.scenes[m.scene].nodes[m.node].name
        )
    }

    /// Brings a mount up if it is not yet: a component from its template,
    /// populated; a stage on its own world — one in-memory store with the
    /// demo rows, a few milliseconds paid when the mount's turn comes rather
    /// than a hundred times at open.
    fn ensure_booted(&mut self, cx: &mut Cx, i: usize) {
        if self.mounts[i].live.is_some() {
            return;
        }
        match self.plan(i) {
            Plan::Widget { tpl, populate } => {
                let Some(w) = self.instantiate(cx, tpl) else {
                    eprintln!("library: the DSL has no template {tpl:?} to mount");
                    std::process::exit(2);
                };
                populate(cx, &w);
                self.mounts[i].live = Some(Live::Widget(w));
            }
            Plan::Stage {
                open,
                solo,
                steps,
                grid,
                mode,
            } => {
                let Some(stage) = self.instantiate(cx, live_id!(stage_tpl)) else {
                    eprintln!("library: the DSL has no stage_tpl to mount");
                    std::process::exit(2);
                };
                let boot = Boot {
                    db: None,
                    grid,
                    virtual_time: true,
                    steps,
                    out: std::path::PathBuf::from(boot::e2e_script().1),
                    no_draw: boot::config().no_draw,
                    mode,
                    primary: false,
                    tag: self.tag(i),
                    open: open.map(|f| Box::new(move |s: &Store| f(s)) as super::boot::Opener),
                    solo,
                    // A mount never replicates: its world is its own, and
                    // two devices over one bucket is what the lease forbids.
                    bucket: None,
                };
                if let Some(mut st) = stage.borrow_mut::<Stage>() {
                    st.boot(cx, boot);
                }
                self.mounts[i].live = Some(Live::Stage(stage));
            }
        }
    }

    /// A stage mount that has not reached its state: waiting for its turn,
    /// or replaying. A component is its state from the start.
    fn mount_replaying(&self, i: usize) -> bool {
        match &self.mounts[i].live {
            None => self.is_stage(i),
            Some(Live::Stage(w)) => w.borrow::<Stage>().as_deref().is_some_and(Stage::replaying),
            Some(Live::Widget(_)) => false,
        }
    }

    fn instantiate(&self, cx: &mut Cx, tpl: LiveId) -> Option<WidgetRef> {
        let template_ref = self.tpl.get(&tpl)?;
        let template_value: ScriptValue = template_ref.as_object().into();
        let vm_id = cx.script_ref_vm_id(template_ref)?;
        Some(cx.with_script_vm_id(vm_id, |vm| WidgetRef::script_from_value(vm, template_value)))
    }

    // -- the camera -------------------------------------------------------------

    fn zoom(&self) -> f64 {
        self.cam.as_ref().map_or(1.0, Camera::zoom)
    }

    fn cam_pos(&self) -> DVec2 {
        self.cam.as_ref().map_or(dvec2(0.0, 0.0), Camera::pos)
    }

    /// Canvas → screen.
    fn to_screen(&self, p: DVec2) -> DVec2 {
        self.vp.pos + (p - self.cam_pos()) * self.zoom()
    }

    /// Screen → canvas.
    fn to_canvas(&self, p: DVec2) -> DVec2 {
        self.cam_pos() + (p - self.vp.pos) / self.zoom()
    }

    fn screen_rect(&self, r: Rect) -> Rect {
        Rect {
            pos: self.to_screen(r.pos),
            size: r.size * self.zoom(),
        }
    }

    /// A mount's canvas rect.
    fn mount_rect(&self, i: usize) -> Option<Rect> {
        let m = self.mounts.get(i)?;
        let block = self.canvas.as_ref()?.blocks.get(m.scene)?;
        block
            .nodes
            .iter()
            .find(|nb| nb.node == m.node)
            .map(|nb| to_rect(nb.rect))
    }

    /// The camera that shows `r` (canvas) centred at `zoom`.
    fn cam_for(&self, r: Rect, zoom: f64) -> (f64, f64, f64) {
        let z = zoom.log2().clamp(Z_MIN, Z_MAX);
        let zoom = 2f64.powf(z);
        let c = r.pos + r.size * 0.5;
        (
            c.x - self.vp.size.x / (2.0 * zoom),
            c.y - (caption_band() + self.vp.size.y - status_h()) / (2.0 * zoom),
            z,
        )
    }

    fn fly_to(&mut self, cx: &mut Cx, r: Rect, zoom: f64) {
        let (x, y, z) = self.cam_for(r, zoom);
        if let Some(cam) = &mut self.cam {
            cam.x.retarget(x);
            cam.y.retarget(y);
            cam.z.retarget(z);
        }
        self.kick(cx);
    }

    /// The zoom that shows all of `r` in the viewport, under the captions
    /// and above the strip.
    fn zoom_to_fit(&self, r: Rect) -> f64 {
        let zx = self.vp.size.x / r.size.x.max(1.0);
        let zy = (self.vp.size.y - status_h() - caption_band()) / r.size.y.max(1.0);
        zx.min(zy)
    }

    fn fit_all(&mut self, cx: &mut Cx) {
        let Some(c) = &self.canvas else {
            return;
        };
        let r = Rect {
            pos: dvec2(0.0, 0.0),
            size: dvec2(c.w, c.h),
        };
        let z = self.zoom_to_fit(r);
        self.fly_to(cx, r, z);
    }

    fn fit_scene(&mut self, cx: &mut Cx, si: usize) {
        let Some(b) = self.canvas.as_ref().and_then(|c| c.blocks.get(si)) else {
            return;
        };
        let r = Rect {
            pos: dvec2(
                b.bounds.x - scene::MARGIN / 2.0,
                b.bounds.y - scene::MARGIN / 2.0,
            ),
            size: dvec2(b.bounds.w + scene::MARGIN, b.bounds.h + scene::MARGIN),
        };
        let z = self.zoom_to_fit(r);
        self.fly_to(cx, r, z);
    }

    /// Zooms by `dz` (log2) keeping the canvas point under `anchor` (screen)
    /// where it is. Jumps — a wheel wants to feel attached.
    fn zoom_at(&mut self, cx: &mut Cx, anchor: DVec2, dz: f64) {
        let before = self.to_canvas(anchor);
        let Some(cam) = &mut self.cam else {
            return;
        };
        let z = (cam.z.target() + dz).clamp(Z_MIN, Z_MAX);
        cam.z.jump_to(z);
        let zoom = 2f64.powf(z);
        let pos = before - (anchor - self.vp.pos) / zoom;
        if let Some(cam) = &mut self.cam {
            cam.x.jump_to(pos.x);
            cam.y.jump_to(pos.y);
        }
        self.kick(cx);
    }

    /// Zooms by `dz` around the viewport's centre, on the springs.
    fn zoom_step(&mut self, cx: &mut Cx, dz: f64) {
        let centre = self.vp.pos + self.vp.size * 0.5;
        let at = self.to_canvas(centre);
        let Some(cam) = &mut self.cam else {
            return;
        };
        let z = (cam.z.target() + dz).clamp(Z_MIN, Z_MAX);
        let zoom = 2f64.powf(z);
        let pos = at - (centre - self.vp.pos) / zoom;
        cam.x.retarget(pos.x);
        cam.y.retarget(pos.y);
        cam.z.retarget(z);
        self.kick(cx);
    }

    fn pan_by(&mut self, cx: &mut Cx, d: DVec2) {
        let zoom = self.zoom();
        if let Some(cam) = &mut self.cam {
            cam.x.jump_to(cam.x.target() + d.x / zoom);
            cam.y.jump_to(cam.y.target() + d.y / zoom);
        }
        self.kick(cx);
    }

    fn kick(&mut self, cx: &mut Cx) {
        self.next_frame = cx.new_next_frame();
        self.redraw(cx);
    }

    // -- entering ---------------------------------------------------------------

    fn set_active(&mut self, cx: &mut Cx, i: usize, active: bool) {
        if let Some(Live::Stage(w)) = &self.mounts[i].live {
            if let Some(mut st) = w.borrow_mut::<Stage>() {
                st.set_active(cx, active);
            }
        }
    }

    fn enter(&mut self, cx: &mut Cx, i: usize) {
        if self.entered != Some(i) {
            self.leave(cx);
            self.ensure_booted(cx, i);
            self.set_active(cx, i, true);
            self.mounts[i].pending = true;
            self.entered = Some(i);
        }
        if let Some(r) = self.mount_rect(i) {
            self.fly_to(cx, r, 1.0);
        }
    }

    fn leave(&mut self, cx: &mut Cx) {
        if let Some(i) = self.entered.take() {
            self.set_active(cx, i, false);
            // Back to a picture: the texture is redrawn from the live state.
            self.mounts[i].pending = true;
            cx.set_key_focus(self.area);
            self.redraw(cx);
        }
    }

    /// The entered mount, if the pointer is over it: its index and screen
    /// rect.
    fn entered_under(&self, p: DVec2) -> Option<(usize, Rect)> {
        let i = self.entered?;
        let r = self.screen_rect(self.mount_rect(i)?);
        r.contains(p).then_some((i, r))
    }

    fn hit_at(&self, p: DVec2) -> Option<HitAct> {
        self.hits
            .iter()
            .rev()
            .find(|h| h.rect.contains(p))
            .map(|h| h.act)
    }

    // -- talking to mounts ------------------------------------------------------

    /// Hands an event to one mount and gives it back whatever actions its
    /// widgets raised, so nothing leaks to the others.
    fn send(&mut self, cx: &mut Cx, i: usize, event: &Event) {
        let Some(live) = self.mounts[i].live.clone() else {
            return;
        };
        let (w, props) = match live {
            Live::Stage(w) => (w, None),
            Live::Widget(w) => (w, self.overlay_props(i)),
        };
        let mut scope = match &props {
            Some(p) => Scope::with_props(p),
            None => Scope::empty(),
        };
        let mut acts = cx.capture_actions(|cx| w.handle_event(cx, event, &mut scope));
        for _ in 0..4 {
            if acts.is_empty() {
                break;
            }
            let ev = Event::Actions(acts);
            acts = cx.capture_actions(|cx| w.handle_event(cx, &ev, &mut scope));
        }
    }

    /// The one stage replaying right now: the first that has not arrived.
    /// Replays run one at a time because makepad has one key focus — a node
    /// that types into a field cannot share the keyboard with another
    /// replaying beside it. The rest wait their turn, in canvas order.
    fn current_replayer(&self) -> Option<usize> {
        (0..self.mounts.len()).find(|&i| self.mount_replaying(i))
    }

    /// Every mount that is awake — the entered one, and the one replaying. A
    /// frozen mount is a picture, a waiting one has not started; neither
    /// hears anything.
    fn broadcast(&mut self, cx: &mut Cx, event: &Event) {
        let current = self.current_replayer();
        if let Some(i) = current {
            self.ensure_booted(cx, i);
        }
        for i in 0..self.mounts.len() {
            if self.entered == Some(i) || current == Some(i) {
                self.send(cx, i, event);
            }
        }
    }

    fn send_entered(&mut self, cx: &mut Cx, event: &Event) {
        if let Some(i) = self.entered {
            self.send(cx, i, event);
        }
    }

    /// Forwards a pointer event to the entered mount, remapped, if the
    /// pointer is over it. Answers whether it was.
    fn forward_pointer(&mut self, cx: &mut Cx, event: &Event, p: DVec2) -> bool {
        let Some((i, r)) = self.entered_under(p) else {
            return false;
        };
        if self.inline(i) {
            // Drawn in the window: its hits are window coordinates already.
            self.send(cx, i, event);
            return true;
        }
        let zoom = self.zoom();
        if let Some(ev) = remap(event, r.pos, zoom) {
            self.send(cx, i, &ev);
        }
        true
    }

    fn replaying(&self) -> usize {
        (0..self.mounts.len())
            .filter(|&i| self.mount_replaying(i))
            .count()
    }

    // -- keys -------------------------------------------------------------------

    /// The canvas's own chords; anything else goes to the entered mount.
    fn key_down(&mut self, cx: &mut Cx, k: &KeyEvent) {
        let cmd = k.modifiers.logo;
        match k.key_code {
            KeyCode::Equals if cmd => self.zoom_step(cx, Z_STEP),
            KeyCode::Minus if cmd => self.zoom_step(cx, -Z_STEP),
            KeyCode::Key0 if cmd => {
                self.leave(cx);
                self.fit_all(cx);
            }
            KeyCode::Escape if cmd => self.leave(cx),
            // The Dev chord, from the canvas: the stage under the library is
            // suspended and hears no keys, so the library answers for it
            // while it has the window — and before an entered mount's own
            // stage could, or the toggle would fire twice.
            KeyCode::KeyL if cmd && k.modifiers.shift => cx.action(DevAction::ToggleLibrary),
            _ if self.entered.is_some() => self.send_entered(cx, &Event::KeyDown(*k)),
            KeyCode::ArrowLeft => self.pan_by(cx, dvec2(-PAN_STEP, 0.0)),
            KeyCode::ArrowRight => self.pan_by(cx, dvec2(PAN_STEP, 0.0)),
            KeyCode::ArrowUp => self.pan_by(cx, dvec2(0.0, -PAN_STEP)),
            KeyCode::ArrowDown => self.pan_by(cx, dvec2(0.0, PAN_STEP)),
            _ => {}
        }
    }

    /// Text into the entered mount — the one thing a script may type at.
    fn type_into(&mut self, cx: &mut Cx, s: &str) {
        let ev = Event::TextInput(TextInputEvent {
            input: s.to_string(),
            ..Default::default()
        });
        self.send_entered(cx, &ev);
    }

    /// A bare modifier press-release into the entered mount, the way
    /// `flagsChanged` delivers one: what double-cmd is made of.
    fn tap_into(&mut self, cx: &mut Cx, code: KeyCode) {
        let down = KeyEvent {
            key_code: code,
            modifiers: KeyModifiers {
                logo: code == KeyCode::Logo,
                ..Default::default()
            },
            is_repeat: false,
            time: 0.0,
        };
        let mut up = down;
        up.modifiers = KeyModifiers::default();
        self.send_entered(cx, &Event::KeyDown(down));
        self.send_entered(cx, &Event::KeyUp(up));
    }

    // -- the canvas's script ----------------------------------------------------

    fn e2e_tick(&mut self, cx: &mut Cx, dt_ms: f64) {
        let Some(mut runner) = self.e2e.take() else {
            return;
        };
        if let Some(step) = runner.next_step(dt_ms) {
            if self.e2e_step(cx, &mut runner, step) {
                return;
            }
        }
        self.e2e = Some(runner);
    }

    /// One step of the canvas's own script. Answers whether the run is over.
    fn e2e_step(&mut self, cx: &mut Cx, runner: &mut e2e::Runner, step: Step) -> bool {
        match step {
            Step::Wait(_) => {}
            Step::Shot(_) if boot::config().no_draw => {
                // Nothing was rasterized, so there is nothing to keep; the
                // labels and the replays are what this run checks.
            }
            Step::Shot(name) => {
                let path = runner.out.join(format!("{name}.png"));
                match RealScreen.shot(&path) {
                    Ok(()) => eprintln!("e2e: shot {}", path.display()),
                    Err(e) => {
                        eprintln!("e2e: FAIL shot {name}: {e}");
                        runner.failures += 1;
                    }
                }
            }
            // A whole-label match wins over a substring, so `mailbox/cursor`
            // is that node and not the scene it is in.
            Step::Click { label, .. } | Step::Mouse { label } => {
                let needle = label.to_lowercase();
                let hit = self
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
                    .map(|h| h.act);
                match hit {
                    Some(HitAct::Enter(i)) => {
                        eprintln!("e2e: click {label:?} — enter");
                        self.enter(cx, i);
                    }
                    Some(HitAct::Scene(s)) => {
                        eprintln!("e2e: click {label:?} — fit scene");
                        self.fit_scene(cx, s);
                    }
                    None => {
                        // Inside an entered node the click is the mount's:
                        // the canvas has no hit for it, the panel does.
                        if self.entered.is_some() {
                            eprintln!("e2e: click {label:?} — into the entered node");
                            self.click_entered(cx, &label, runner);
                        } else {
                            eprintln!(
                                "e2e: FAIL click {label:?}: no matching element — on offer: {}",
                                self.hits
                                    .iter()
                                    .map(|h| h.label.as_str())
                                    .collect::<Vec<_>>()
                                    .join(" · ")
                            );
                            runner.failures += 1;
                        }
                    }
                }
            }
            Step::Key { chord: c, times } => {
                if let Some(ev) = chord(&c) {
                    eprintln!("e2e: key {c} ×{times}");
                    for _ in 0..times.max(1) {
                        self.key_down(cx, &ev);
                    }
                } else if let Some(exec) = super::keys::parse_chord(&c) {
                    eprintln!("e2e: key {c} ×{times} — into the entered node");
                    for _ in 0..times.max(1) {
                        match &exec {
                            ChordExec::Ev(ev) => self.key_down(cx, ev),
                            ChordExec::Text(s) => self.type_into(cx, s),
                            ChordExec::Tap(code) => self.tap_into(cx, *code),
                        }
                    }
                } else {
                    eprintln!("e2e: FAIL key {c:?}: cannot parse chord");
                    runner.failures += 1;
                }
            }
            Step::Type(s) => {
                eprintln!("e2e: type {s:?}");
                self.type_into(cx, &s);
            }
            Step::Quit => {
                let replaying = self.replaying();
                eprintln!(
                    "e2e: done — {} step(s), {} failure(s), {} node(s) still replaying",
                    runner.steps.len(),
                    runner.failures,
                    replaying
                );
                if runner.failures > 0 || replaying > 0 {
                    std::process::exit(1);
                }
                cx.quit();
                return true;
            }
            other => {
                eprintln!("e2e: FAIL {other:?}: the canvas has no such step");
                runner.failures += 1;
            }
        }
        false
    }

    /// A click the canvas has no hit for, inside the entered node: resolved
    /// against that mount's own hit table, by its own stage.
    fn click_entered(&mut self, cx: &mut Cx, label: &str, runner: &mut e2e::Runner) {
        let Some(i) = self.entered else { return };
        let Some(r) = self.mount_rect(i).map(|r| self.screen_rect(r)) else {
            return;
        };
        let inline = self.inline(i);
        let zoom = self.zoom();
        let found = match &self.mounts[i].live {
            Some(Live::Stage(w)) => w
                .borrow::<Stage>()
                .and_then(|st| st.hit_centre(label))
                .map(|c| if inline { c } else { r.pos + c * zoom }),
            _ => None,
        };
        let Some(at) = found else {
            eprintln!("e2e: FAIL click {label:?}: the entered node has no such element");
            runner.failures += 1;
            return;
        };
        // Outside the inline path the mount's own coordinates are the
        // canvas's, scaled: the same remap a human's click takes.
        for ev in super::pointer::press_release(at, false) {
            match if inline {
                None
            } else {
                remap(&ev, r.pos, zoom)
            } {
                Some(mapped) => self.send(cx, i, &mapped),
                None => self.send(cx, i, &ev),
            }
        }
    }
}

impl Widget for Library {
    fn handle_event(&mut self, cx: &mut Cx, event: &Event, _scope: &mut Scope) {
        if !self.booted || !self.shown {
            return;
        }
        let t0 = super::boot::frame_log().then(std::time::Instant::now);
        self.handle(cx, event);
        if let Some(t0) = t0 {
            let kind = event_kind(event);
            match self.since_draw.iter_mut().find(|(k, _)| *k == kind) {
                Some((_, n)) => *n += 1,
                None => self.since_draw.push((kind, 1)),
            }
            let ms = t0.elapsed().as_secs_f64() * 1000.0;
            if ms > 1.0 {
                eprintln!(
                    "library: event {kind} took {ms:.2} ms (entered {:?})",
                    self.entered
                );
            }
        }
    }

    fn draw_walk(&mut self, cx: &mut Cx2d, _scope: &mut Scope, walk: Walk) -> DrawStep {
        let t0 = super::boot::frame_log().then(std::time::Instant::now);
        self.mount_ms = 0.0;
        self.renders = 0;
        let step = self.draw(cx, walk);
        if let Some(t0) = t0 {
            if self.booted && self.shown {
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
                    "library: frame {} (+{since:.0} ms): draw {:.2} ms, {:.2} ms in {} mount render(s), zoom {:.3}, entered {:?}, after {}",
                    self.frames,
                    t0.elapsed().as_secs_f64() * 1000.0,
                    self.mount_ms,
                    self.renders,
                    self.zoom(),
                    self.entered,
                    after.join(" ")
                );
            }
        }
        step
    }
}

impl Library {
    fn handle(&mut self, cx: &mut Cx, event: &Event) {
        match event {
            Event::NextFrame(ne) => {
                if ne.set.contains(&self.next_frame) {
                    let dt = if cfg!(headless) {
                        FRAME_MS / 1000.0
                    } else {
                        let now = std::time::Instant::now();
                        let dt = self
                            .last_frame
                            .map(|t| (now - t).as_secs_f64())
                            .unwrap_or(1.0 / 60.0)
                            .clamp(0.0, 1.0 / 20.0);
                        self.last_frame = Some(now);
                        dt
                    };
                    let moving = self.cam.as_mut().is_some_and(|c| c.advance(dt));
                    // Deferred re-renders wait for the zoom to stand still.
                    let z = self.zoom();
                    if (z - self.last_zoom).abs() > 1e-9 {
                        self.zoom_ticks = 0;
                        self.last_zoom = z;
                    } else {
                        self.zoom_ticks = self.zoom_ticks.saturating_add(1);
                    }
                    self.e2e_tick(cx, dt * 1000.0);
                    // While stages replay, the strip counts them down; while
                    // renders are owed, the draw pays them off a frame at a
                    // time.
                    self.frames += 1;
                    let replaying = self.replaying() > 0;
                    if !replaying && !self.filled {
                        self.filled = true;
                        eprintln!(
                            "library: all {} nodes arrived after {} frames",
                            self.mounts.len(),
                            self.frames
                        );
                    }
                    let work = self.more_work;
                    if moving || self.e2e.is_some() || replaying || work {
                        self.next_frame = cx.new_next_frame();
                    }
                    if moving || replaying || work {
                        self.redraw(cx);
                    }
                }
                self.broadcast(cx, event);
            }
            Event::Timer(_) | Event::Signal | Event::WindowGeomChange(_) => {
                self.broadcast(cx, event);
            }
            Event::KeyDown(k) => self.key_down(cx, k),
            Event::KeyUp(_) | Event::TextInput(_) | Event::KeyFocus(_) | Event::KeyFocusLost(_) => {
                self.send_entered(cx, event);
            }
            Event::MouseDown(e) => {
                let p = e.abs;
                if self.forward_pointer(cx, event, p) {
                    return;
                }
                match self.hit_at(p) {
                    Some(HitAct::Enter(i)) => self.enter(cx, i),
                    Some(HitAct::Scene(s)) => {
                        self.leave(cx);
                        self.fit_scene(cx, s);
                    }
                    None => {
                        self.leave(cx);
                        self.drag = Some(Drag {
                            start: p,
                            cam: self.cam_pos(),
                        });
                    }
                }
            }
            Event::MouseMove(e) => {
                self.pointer = Some(e.abs);
                if let Some(d) = &self.drag {
                    let zoom = self.zoom();
                    let pos = d.cam - (e.abs - d.start) / zoom;
                    if let Some(cam) = &mut self.cam {
                        cam.x.jump_to(pos.x);
                        cam.y.jump_to(pos.y);
                    }
                    self.kick(cx);
                    return;
                }
                if !self.forward_pointer(cx, event, e.abs) {
                    cx.set_cursor(if self.hit_at(e.abs).is_some() {
                        MouseCursor::Hand
                    } else {
                        MouseCursor::Default
                    });
                }
            }
            Event::MouseUp(e) => {
                if self.drag.take().is_some() {
                    return;
                }
                self.forward_pointer(cx, event, e.abs);
            }
            Event::Scroll(e) => {
                if e.modifiers.logo {
                    e.handled_x.set(true);
                    e.handled_y.set(true);
                    self.zoom_at(cx, e.abs, -e.scroll.y * 0.01);
                    return;
                }
                if self.forward_pointer(cx, event, e.abs) {
                    return;
                }
                e.handled_x.set(true);
                e.handled_y.set(true);
                self.pan_by(cx, e.scroll);
            }
            // A mount's actions are captured and handed back to it in
            // `send`; the window-wide batch is nobody's.
            _ => {}
        }
    }

    fn draw(&mut self, cx: &mut Cx2d, walk: Walk) -> DrawStep {
        cx.begin_turtle(walk, Layout::default());
        if !self.booted || !self.shown {
            cx.end_turtle_with_area(&mut self.area);
            return DrawStep::done();
        }
        self.vp = cx.turtle().rect();
        self.list_id = cx.get_current_draw_list_id();
        if self.metrics.is_none() {
            self.draw_mono.text_style.font_size = 10.0;
            if let Some(run) = self
                .draw_mono
                .prepare_single_line_run(cx, "MMMMMMMMMMMMMMMM")
            {
                let adv = f64::from(run.width_in_lpxs) / 16.0 / 10.0;
                let natural =
                    (f64::from(run.ascender_in_lpxs) - f64::from(run.descender_in_lpxs)) / 10.0;
                if adv > 0.0 && natural > 0.0 {
                    self.metrics = Some(Metrics {
                        adv,
                        line: natural * 1.3,
                    });
                }
            }
        }
        if self.canvas.is_none() {
            if let Some(m) = self.metrics {
                let c = scene::layout(&self.scenes[..], &m);
                let r = Rect {
                    pos: dvec2(0.0, 0.0),
                    size: dvec2(c.w, c.h),
                };
                self.canvas = Some(c);
                let z = self.zoom_to_fit(r);
                let (x, y, z) = self.cam_for(r, z);
                self.cam = Some(Camera::at(x, y, z));
            }
        }
        self.draw_canvas(cx);
        self.draw_status(cx);
        if self.more_work {
            self.next_frame = cx.new_next_frame();
        }
        cx.end_turtle_with_area(&mut self.area);
        DrawStep::done()
    }
}
