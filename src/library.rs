//! The panels library (CR-006): an infinite canvas of live stages.
//!
//! `--library` opens the window on a zoomable, pannable canvas instead of
//! the workspace. Every e2e script under `e2e/` is a **story** row; every
//! `shot` in it is a **node** — a whole [`Stage`] on a world of its own, an
//! in-memory store, a sealed outside and a virtual clock — that replayed
//! the story up to that shot and stopped there. The steps between two
//! shots label the arrow between their nodes; the script's comments are
//! the annotations. See [`crate::story`] for the reading, this module for
//! the mounting.
//!
//! A mount renders into its own pass at the canvas's zoom (crisp text at
//! every level: the pass's dpi factor is the zoom), and the canvas shows
//! the pass's texture. Entering a node — a click — brings it to 1:1 and
//! routes the keyboard and the pointer to it, remapped into its own
//! coordinates, so a flow can be continued by hand from any of its states.
//! Actions a mount's widgets raise are captured and handed straight back
//! to it, so a hundred stages never hear each other.

use makepad_widgets::makepad_platform::event::{
    LongPressEvent, MouseDownEvent, MouseMoveEvent, MouseUpEvent, ScrollEvent,
};
use makepad_widgets::*;

use crate::app::{self, rgba_a, Boot, BootOutside, DrawFlat, Stage, FRAME_MS};
use crate::e2e::{self, Step};
use crate::effect::{Clock, MemSecrets, Outside, Real, Secrets};
use crate::spring::{Spring, SpringParams};
use crate::story::{self, Canvas, Metrics, OutsideKind, Story, TEXT_PT, TITLE_PT};
use crate::theme;

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
/// The dpi a mount is drawn at while it replays out of view: the widget
/// pass still runs (a step needs fresh hits), the rasterizer barely does.
const DPI_OFFSCREEN: f64 = 0.1;

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

/// One node's live stage.
struct Mount {
    stage: WidgetRef,
    story: usize,
    node: usize,
    /// The viewport the story asked for, points.
    size: DVec2,
    pass: Option<MountPass>,
    /// The dpi factor the pass was last rendered at; zero before the first.
    dpi: f64,
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
    /// Fit this story's row.
    Story(usize),
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

    /// The stage template (the DSL's `stage_tpl`), one instance per node.
    #[rust]
    tpl: Option<ScriptObjectRef>,
    #[rust]
    stories: Vec<Story>,
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
            self.tpl = None;
        }
    }

    fn on_after_apply(
        &mut self,
        vm: &mut ScriptVm,
        apply: &Apply,
        _scope: &mut Scope,
        value: ScriptValue,
    ) {
        // The named child is a template, never auto-drawn (the Stage's own
        // pattern for its panels).
        if !apply.is_eval() {
            if let Some(obj) = value.as_object() {
                vm.vec_with(obj, |vm, vec| {
                    for kv in vec {
                        if kv.key.as_id() == Some(live_id!(stage_tpl)) {
                            if let Some(t) = kv.value.as_object() {
                                self.tpl = Some(vm.bx.heap.new_object_ref(t));
                            }
                        }
                    }
                });
            }
        }
    }
}

fn to_rect(r: crate::core::Rect) -> Rect {
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
/// octave, so a wheel gesture does not re-render every mount on every
/// notch and the texture is never more than a few percent off its
/// on-screen size.
fn render_zoom(zoom: f64) -> f64 {
    2f64.powf((zoom.log2() * 4.0).round() / 4.0)
}

/// A canvas chord, for the script and the keyboard alike.
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
    let key_code = match key? {
        "=" | "equals" | "plus" => KeyCode::Equals,
        "-" | "minus" => KeyCode::Minus,
        "0" => KeyCode::Key0,
        "esc" | "escape" => KeyCode::Escape,
        "left" => KeyCode::ArrowLeft,
        "right" => KeyCode::ArrowRight,
        "up" => KeyCode::ArrowUp,
        "down" => KeyCode::ArrowDown,
        "enter" | "return" => KeyCode::ReturnKey,
        _ => return None,
    };
    Some(KeyEvent {
        key_code,
        modifiers,
        is_repeat: false,
        time: 0.0,
    })
}

/// The same event, with its pointer position mapped from the window into
/// a mount's own coordinates — `None` for events that carry none.
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
        Event::LongPress(e) => Event::LongPress(LongPressEvent {
            abs: f(e.abs),
            ..e.clone()
        }),
        _ => return None,
    })
}

impl Library {
    // -- boot ---------------------------------------------------------------

    /// Reads the stories and mounts one stage per node, each replaying its
    /// story prefix on a world of its own.
    fn boot(&mut self, cx: &mut Cx) {
        let Some(paths) = app::library_paths() else {
            return;
        };
        let stories = match story::load(paths) {
            Ok(s) if s.is_empty() => {
                eprintln!("library: no stories under {paths:?}");
                std::process::exit(2);
            }
            Ok(s) => s,
            Err(e) => {
                eprintln!("library: {e}");
                std::process::exit(2);
            }
        };
        for (si, s) in stories.iter().enumerate() {
            for (ni, n) in s.nodes.iter().enumerate() {
                let Some(stage) = self.instantiate(cx) else {
                    eprintln!("library: the DSL has no stage_tpl to mount");
                    std::process::exit(2);
                };
                let boot = Boot {
                    db: None,
                    grid: s.cfg.grid,
                    send_delay: s.cfg.send_delay,
                    virtual_time: true,
                    outside: match s.cfg.outside {
                        OutsideKind::Deny => BootOutside::Deny,
                        OutsideKind::Fake => BootOutside::Fake,
                        OutsideKind::Real => BootOutside::Real,
                    },
                    secrets_in_memory: true,
                    steps: Some(s.steps[..=n.until].to_vec()),
                    primary: false,
                };
                if let Some(mut st) = stage.borrow_mut::<Stage>() {
                    st.boot(cx, boot);
                }
                self.mounts.push(Mount {
                    stage,
                    story: si,
                    node: ni,
                    size: dvec2(s.cfg.window.0, s.cfg.window.1),
                    pass: None,
                    dpi: 0.0,
                });
            }
        }
        eprintln!(
            "library: {} stor{} on the canvas, {} nodes",
            stories.len(),
            if stories.len() == 1 { "y" } else { "ies" },
            self.mounts.len()
        );
        self.stories = stories;
        // The canvas's own script.
        let (script, out) = app::e2e_script();
        if let Some(path) = script {
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

    fn instantiate(&self, cx: &mut Cx) -> Option<WidgetRef> {
        let template_ref = self.tpl.as_ref()?;
        let template_value: ScriptValue = template_ref.as_object().into();
        let vm_id = cx.script_ref_vm_id(template_ref)?;
        Some(cx.with_script_vm_id(vm_id, |vm| {
            WidgetRef::script_from_value(vm, template_value)
        }))
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
        let row = self.canvas.as_ref()?.rows.get(m.story)?;
        row.nodes.get(m.node).map(|n| to_rect(n.rect))
    }

    /// The camera that shows `r` (canvas) centred at `zoom`.
    fn cam_for(&self, r: Rect, zoom: f64) -> (f64, f64, f64) {
        let z = zoom.log2().clamp(Z_MIN, Z_MAX);
        let zoom = 2f64.powf(z);
        let c = r.pos + r.size * 0.5;
        (
            c.x - self.vp.size.x / (2.0 * zoom),
            c.y - self.vp.size.y / (2.0 * zoom),
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

    /// The zoom that shows all of `r` in the viewport.
    fn zoom_to_fit(&self, r: Rect) -> f64 {
        let zx = self.vp.size.x / r.size.x.max(1.0);
        let zy = self.vp.size.y / r.size.y.max(1.0);
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

    fn fit_story(&mut self, cx: &mut Cx, si: usize) {
        let Some(row) = self.canvas.as_ref().and_then(|c| c.rows.get(si)) else {
            return;
        };
        let Some(first) = row.nodes.first() else {
            return;
        };
        let last = row.nodes.last().unwrap_or(first);
        let x0 = row.title.0;
        let y0 = row.title.1;
        let x1 = last.rect.x + last.rect.w;
        let y1 = first.rect.y + first.rect.h;
        let r = Rect {
            pos: dvec2(x0 - story::MARGIN / 2.0, y0 - story::MARGIN / 2.0),
            size: dvec2(x1 - x0 + story::MARGIN, y1 - y0 + story::MARGIN),
        };
        let z = self.zoom_to_fit(r);
        self.fly_to(cx, r, z);
    }

    /// Zooms by `dz` (log2) keeping the canvas point under `anchor`
    /// (screen) where it is. Jumps — a wheel wants to feel attached.
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

    fn enter(&mut self, cx: &mut Cx, i: usize) {
        if self.entered != Some(i) {
            self.leave(cx);
            if let Some(mut st) = self.mounts[i].stage.borrow_mut::<Stage>() {
                st.set_active(cx, true);
            }
            self.entered = Some(i);
        }
        if let Some(r) = self.mount_rect(i) {
            self.fly_to(cx, r, 1.0);
        }
    }

    fn leave(&mut self, cx: &mut Cx) {
        if let Some(i) = self.entered.take() {
            if let Some(mut st) = self.mounts[i].stage.borrow_mut::<Stage>() {
                st.set_active(cx, false);
            }
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
        self.hits.iter().rev().find(|h| h.rect.contains(p)).map(|h| h.act)
    }

    // -- talking to mounts ------------------------------------------------------

    /// Hands an event to one mount and gives it back whatever actions its
    /// widgets raised, so nothing leaks to the others (a `PanelAction`
    /// carries a panel id, and every mount numbers its panels from one).
    fn send(&mut self, cx: &mut Cx, i: usize, event: &Event) {
        let w = self.mounts[i].stage.clone();
        let mut acts = cx.capture_actions(|cx| w.handle_event(cx, event, &mut Scope::empty()));
        for _ in 0..4 {
            if acts.is_empty() {
                break;
            }
            let ev = Event::Actions(acts);
            acts = cx.capture_actions(|cx| w.handle_event(cx, &ev, &mut Scope::empty()));
        }
    }

    fn broadcast(&mut self, cx: &mut Cx, event: &Event) {
        for i in 0..self.mounts.len() {
            self.send(cx, i, event);
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
        let zoom = self.zoom();
        if let Some(ev) = remap(event, r.pos, zoom) {
            self.send(cx, i, &ev);
        }
        true
    }

    fn replaying(&self) -> usize {
        self.mounts
            .iter()
            .filter(|m| m.stage.borrow::<Stage>().is_some_and(|s| s.replaying()))
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
            _ if self.entered.is_some() => self.send_entered(cx, &Event::KeyDown(*k)),
            KeyCode::ArrowLeft => self.pan_by(cx, dvec2(-PAN_STEP, 0.0)),
            KeyCode::ArrowRight => self.pan_by(cx, dvec2(PAN_STEP, 0.0)),
            KeyCode::ArrowUp => self.pan_by(cx, dvec2(0.0, -PAN_STEP)),
            KeyCode::ArrowDown => self.pan_by(cx, dvec2(0.0, PAN_STEP)),
            _ => {}
        }
    }

    // -- the canvas's script ----------------------------------------------------

    fn e2e_tick(&mut self, cx: &mut Cx, dt_ms: f64) {
        let Some(mut runner) = self.e2e.take() else {
            return;
        };
        if let Some(step) = runner.next_step(dt_ms) {
            match step {
                Step::Wait(_) => {}
                Step::Shot(name) => {
                    let path = runner.out.join(format!("{name}.png"));
                    let mut real = Real::new(Secrets::Memory(MemSecrets::new()), Clock::System);
                    match real.shot(&path) {
                        Ok(()) => eprintln!("e2e: shot {}", path.display()),
                        Err(e) => {
                            eprintln!("e2e: FAIL shot {name}: {e}");
                            runner.failures += 1;
                        }
                    }
                }
                Step::Click { label, .. } => {
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
                        Some(HitAct::Story(s)) => {
                            eprintln!("e2e: click {label:?} — fit story");
                            self.fit_story(cx, s);
                        }
                        None => {
                            eprintln!("e2e: FAIL click {label:?}: no matching element");
                            runner.failures += 1;
                        }
                    }
                }
                Step::Key { chord: c, times } => match chord(&c) {
                    Some(ev) => {
                        eprintln!("e2e: key {c} ×{times}");
                        for _ in 0..times.max(1) {
                            self.key_down(cx, &ev);
                        }
                    }
                    None => {
                        eprintln!("e2e: FAIL key {c:?}: not a canvas chord");
                        runner.failures += 1;
                    }
                },
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
                    return;
                }
                other => {
                    eprintln!("e2e: FAIL {other:?}: the canvas has no such step");
                    runner.failures += 1;
                }
            }
        }
        self.e2e = Some(runner);
    }

    // -- drawing ----------------------------------------------------------------

    /// Canvas text: `pt` points at zoom 1, scaled with the camera. Below
    /// `min` screen points it is not drawn at all — unless `min` clamps it,
    /// which is how story and node names stay legible from any height.
    fn text(&mut self, cx: &mut Cx2d, pos: DVec2, pt: f64, color: theme::Rgba, s: &str) {
        self.text_min(cx, pos, pt, 2.0, false, color, s);
    }

    fn label(&mut self, cx: &mut Cx2d, pos: DVec2, pt: f64, color: theme::Rgba, s: &str) {
        self.text_min(cx, pos, pt, 10.0, true, color, s);
    }

    fn text_min(
        &mut self,
        cx: &mut Cx2d,
        pos: DVec2,
        pt: f64,
        min: f32,
        clamp: bool,
        color: theme::Rgba,
        s: &str,
    ) {
        let mut size = (pt * self.zoom()) as f32;
        if size < min {
            if !clamp {
                return;
            }
            size = min;
        }
        if s.is_empty() {
            return;
        }
        // Only what the window can show: the canvas is mostly off-screen.
        let est_w = s.chars().count() as f64 * f64::from(size) * 0.7;
        if pos.x > self.vp.pos.x + self.vp.size.x
            || pos.y > self.vp.pos.y + self.vp.size.y
            || pos.x + est_w < self.vp.pos.x
            || pos.y + f64::from(size) * 1.5 < self.vp.pos.y
        {
            return;
        }
        self.draw_mono.new_draw_call(cx);
        self.draw_mono.text_style.font_size = size;
        self.draw_mono.color = rgba_a(color, 1.0);
        self.draw_mono.draw_abs(cx, pos, s);
    }

    fn fill(&mut self, cx: &mut Cx2d, r: Rect, color: theme::Rgba) {
        self.draw_flat.color = rgba_a(color, 1.0);
        self.draw_flat.draw_abs(cx, r);
    }

    /// A one-pixel frame.
    fn frame(&mut self, cx: &mut Cx2d, r: Rect, color: theme::Rgba) {
        let (x, y, w, h) = (r.pos.x, r.pos.y, r.size.x, r.size.y);
        for (rx, ry, rw, rh) in [
            (x, y, w, 1.0),
            (x, y + h - 1.0, w, 1.0),
            (x, y, 1.0, h),
            (x + w - 1.0, y, 1.0, h),
        ] {
            self.fill(
                cx,
                Rect {
                    pos: dvec2(rx, ry),
                    size: dvec2(rw, rh),
                },
                color,
            );
        }
    }

    /// Renders (if needed) and shows one mount.
    fn draw_mount(&mut self, cx: &mut Cx2d, i: usize, screen: Rect) {
        let visible = intersects(screen, self.vp);
        let replaying = self.mounts[i]
            .stage
            .borrow::<Stage>()
            .is_some_and(|s| s.replaying());
        if !visible && !replaying {
            return;
        }
        let win_dpi = cx.current_dpi_factor();
        let dpi = if visible {
            win_dpi * render_zoom(self.zoom())
        } else {
            DPI_OFFSCREEN
        };
        let size = self.mounts[i].size;
        let stage = self.mounts[i].stage.clone();
        let mut mp = self.mounts[i]
            .pass
            .take()
            .unwrap_or_else(|| MountPass::new(cx));

        // The pass rect comes from an area of the parent: a transparent
        // quad the mount's logical size, so the texture is `size × dpi`
        // whatever the canvas shows it at.
        self.draw_flat.color = vec4(0.0, 0.0, 0.0, 0.0);
        self.draw_flat.draw_abs(
            cx,
            Rect {
                pos: screen.pos,
                size,
            },
        );
        let helper = self.draw_flat.area();

        let walk = Walk::abs_rect(Rect {
            pos: dvec2(0.0, 0.0),
            size,
        });
        let dpi_changed = (self.mounts[i].dpi - dpi).abs() > 1e-9;
        let redraw = dpi_changed || cx.will_redraw(&mut mp.list, walk);
        if redraw {
            self.mounts[i].dpi = dpi;
            if let (Some(mut st), Some(canvas)) = (stage.borrow_mut::<Stage>(), self.list_id) {
                st.set_lists(mp.list.id(), canvas);
            }
            cx.make_child_pass(&mp.pass);
            cx.begin_pass(&mp.pass, Some(dpi));
            mp.list.begin_always(cx);
            cx.begin_turtle(walk, Layout::default());
            stage.draw_all(cx, &mut Scope::empty());
            cx.end_turtle();
            mp.list.end(cx);
            cx.end_pass(&mp.pass);
        }
        cx.set_pass_area_with_origin(&mp.pass, helper, dvec2(0.0, 0.0));
        if visible {
            self.draw_tex.draw_vars.set_texture(0, &mp.tex);
            self.draw_tex.draw_abs(cx, screen);
        }
        self.mounts[i].pass = Some(mp);
    }

    fn draw_canvas(&mut self, cx: &mut Cx2d) {
        let Some(canvas) = self.canvas.clone() else {
            return;
        };
        let zoom = self.zoom();
        let line = self.metrics.map_or(20.0, |m| m.line * TEXT_PT);
        self.hits.clear();
        let entered = self.entered;
        // Out of `self` for the loop: drawing borrows the widget mutably.
        let stories = std::mem::take(&mut self.stories);
        // Mounts by (story, node), for the hits.
        let index: std::collections::HashMap<(usize, usize), usize> = self
            .mounts
            .iter()
            .enumerate()
            .map(|(i, m)| ((m.story, m.node), i))
            .collect();

        for row in &canvas.rows {
            let story = &stories[row.story];
            // Names are clamped to a legible size, so far out they would
            // sit on the frames they belong to: the row's labels are laid
            // in screen space instead — the node names just above the
            // mounts, the title just above those.
            let line_px = self.metrics.map_or(1.3, |m| m.line);
            let name_px = (TEXT_PT * zoom).max(10.0) * line_px;
            let first_top = row
                .nodes
                .first()
                .map_or(0.0, |nb| self.to_screen(dvec2(nb.rect.x, nb.rect.y)).y);
            let names_y = row.nodes.first().map_or(0.0, |nb| {
                self.to_screen(dvec2(nb.caption.0, nb.caption.1))
                    .y
                    .min(first_top - name_px - 4.0)
            });
            let title_px = (TITLE_PT * zoom).max(10.0) * line_px;
            let title_canvas = self.to_screen(dvec2(row.title.0, row.title.1));
            let title_at = dvec2(title_canvas.x, title_canvas.y.min(names_y - title_px - 6.0));
            let title_h = title_px;
            self.label(cx, title_at, TITLE_PT, theme::INK, &story.name);
            let cfg = format!(
                "{}×{} · {} · {}",
                story.cfg.window.0,
                story.cfg.window.1,
                story
                    .cfg
                    .grid
                    .map_or("default grid".to_string(), |g| format!("grid {}×{}", g.w, g.h)),
                match story.cfg.outside {
                    OutsideKind::Deny => "deny",
                    OutsideKind::Fake => "fake",
                    OutsideKind::Real => "real",
                }
            );
            let name_w = story.name.chars().count() as f64
                * self.metrics.map_or(0.6, |m| m.adv)
                * (TITLE_PT * zoom).max(10.0);
            self.text(
                cx,
                title_at + dvec2(name_w + 24.0 * zoom, title_h - line * zoom),
                TEXT_PT,
                theme::MUTED,
                &cfg,
            );
            self.hits.push(Hit {
                label: story.name.clone(),
                rect: Rect {
                    pos: title_at,
                    size: dvec2(name_w, title_h),
                },
                act: HitAct::Story(row.story),
            });
            let mut y = self.to_screen(dvec2(row.intro.0, row.intro.1)).y;
            for l in &story.intro {
                self.text(cx, dvec2(title_at.x, y), TEXT_PT, theme::TEXT2, l);
                y += line * zoom;
            }
            for a in &row.arrows {
                let from = self.to_screen(dvec2(a.from.0, a.from.1));
                let to = self.to_screen(dvec2(a.to.0, a.to.1));
                let head = (14.0 * zoom).max(4.0);
                self.fill(
                    cx,
                    Rect {
                        pos: dvec2(from.x, from.y - 0.5),
                        size: dvec2((to.x - from.x - head).max(0.0), 1.0),
                    },
                    theme::INK,
                );
                self.draw_head.color = rgba_a(theme::INK, 1.0);
                self.draw_head.draw_abs(
                    cx,
                    Rect {
                        pos: dvec2(to.x - head, to.y - head / 2.0),
                        size: dvec2(head, head),
                    },
                );
                let mut ly = self.to_screen(dvec2(a.labels_at.0, a.labels_at.1)).y;
                let lx = self.to_screen(dvec2(a.labels_at.0, 0.0)).x;
                for l in &a.labels {
                    self.text(cx, dvec2(lx, ly), TEXT_PT, theme::INK, l);
                    ly += line * zoom;
                }
            }
            for nb in &row.nodes {
                let node = &story.nodes[nb.node];
                let screen = self.screen_rect(to_rect(nb.rect));
                let i = index.get(&(row.story, nb.node)).copied();
                let is_entered = i.is_some() && i == entered;
                // The caption: the shot's name (inverted while entered,
                // the way a focused panel's header is), then the note.
                let cap_canvas = self.to_screen(dvec2(nb.caption.0, nb.caption.1));
                let cap = dvec2(cap_canvas.x, cap_canvas.y.min(names_y));
                let name_w = node.name.chars().count() as f64
                    * self.metrics.map_or(0.6, |m| m.adv)
                    * (TEXT_PT * zoom).max(10.0);
                if is_entered {
                    self.fill(
                        cx,
                        Rect {
                            pos: cap - dvec2(4.0, 2.0),
                            size: dvec2(name_w + 8.0, name_px + 4.0),
                        },
                        theme::INK,
                    );
                }
                // The name stays legible from any height; far out, it is
                // cut to what fits over its own mount.
                let adv_px = self.metrics.map_or(0.6, |m| m.adv) * (TEXT_PT * zoom).max(10.0);
                let fit = (screen.size.x / adv_px).floor() as usize;
                let shown = if name_w <= screen.size.x || zoom * TEXT_PT >= 10.0 {
                    node.name.clone()
                } else if fit >= 4 {
                    crate::ui::trunc(&node.name, fit)
                } else {
                    String::new()
                };
                self.label(
                    cx,
                    cap,
                    TEXT_PT,
                    if is_entered { theme::BG } else { theme::INK },
                    &shown,
                );
                let mut ny = cap_canvas.y + line * zoom;
                for l in &node.note {
                    self.text(cx, dvec2(cap.x, ny), TEXT_PT, theme::TEXT2, l);
                    ny += line * zoom;
                }
                if let Some(i) = i {
                    self.draw_mount(cx, i, screen);
                    let replaying = self.mounts[i]
                        .stage
                        .borrow::<Stage>()
                        .is_some_and(|s| s.replaying());
                    self.frame(
                        cx,
                        Rect {
                            pos: screen.pos - dvec2(1.0, 1.0),
                            size: screen.size + dvec2(2.0, 2.0),
                        },
                        if replaying { theme::MUTED } else { theme::INK },
                    );
                    if replaying {
                        // A node still on its way: a wash, so it is not
                        // mistaken for a state.
                        self.draw_flat.color = rgba_a(theme::BG, 0.6);
                        self.draw_flat.draw_abs(cx, screen);
                    }
                    self.hits.push(Hit {
                        label: node.name.clone(),
                        rect: Rect {
                            pos: cap,
                            size: dvec2(name_w, name_px),
                        },
                        act: HitAct::Enter(i),
                    });
                    self.hits.push(Hit {
                        label: node.name.clone(),
                        rect: screen,
                        act: HitAct::Enter(i),
                    });
                }
            }
        }
        self.stories = stories;
    }

    /// The legend, screen-fixed at the bottom: every control, spelled out.
    fn draw_legend(&mut self, cx: &mut Cx2d) {
        let replaying = self.replaying();
        let status = if replaying > 0 {
            format!("{replaying} of {} nodes replaying · ", self.mounts.len())
        } else {
            String::new()
        };
        let legend = format!(
            "{status}drag · scroll  pan   ⌘scroll · ⌘= · ⌘-  zoom   ⌘0  fit   click  enter   click outside · ⌘esc  leave"
        );
        let size = theme::FONT_SIZE as f32;
        let h = (f64::from(size) * 2.4).round();
        self.fill(
            cx,
            Rect {
                pos: dvec2(self.vp.pos.x, self.vp.pos.y + self.vp.size.y - h),
                size: dvec2(self.vp.size.x, h),
            },
            theme::BG,
        );
        self.fill(
            cx,
            Rect {
                pos: dvec2(self.vp.pos.x, (self.vp.pos.y + self.vp.size.y - h).round()),
                size: dvec2(self.vp.size.x, 1.0),
            },
            theme::RULE,
        );
        self.draw_mono.new_draw_call(cx);
        self.draw_mono.text_style.font_size = size;
        self.draw_mono.color = rgba_a(theme::TEXT2, 1.0);
        self.draw_mono.draw_abs(
            cx,
            dvec2(
                self.vp.pos.x + theme::PAD_X,
                self.vp.pos.y + self.vp.size.y - h + f64::from(size) * 0.6,
            ),
            &legend,
        );
    }
}

impl Widget for Library {
    fn handle_event(&mut self, cx: &mut Cx, event: &Event, _scope: &mut Scope) {
        if matches!(event, Event::Startup) {
            self.boot(cx);
        }
        if !self.booted {
            return;
        }
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
                    self.e2e_tick(cx, dt * 1000.0);
                    // While nodes replay, the legend counts them down.
                    let replaying = self.replaying() > 0;
                    if moving || self.e2e.is_some() || replaying {
                        self.next_frame = cx.new_next_frame();
                    }
                    if moving || replaying {
                        self.redraw(cx);
                    }
                }
                self.broadcast(cx, event);
            }
            Event::Timer(_)
            | Event::Signal
            | Event::WindowGeomChange(_)
            | Event::VirtualKeyboard(_) => self.broadcast(cx, event),
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
                    Some(HitAct::Story(s)) => {
                        self.leave(cx);
                        self.fit_story(cx, s);
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
            Event::LongPress(e) => {
                self.forward_pointer(cx, event, e.abs);
            }
            // A mount's actions are captured and handed back to it in
            // `send`; the window-wide batch is nobody's.
            _ => {}
        }
    }

    fn draw_walk(&mut self, cx: &mut Cx2d, _scope: &mut Scope, walk: Walk) -> DrawStep {
        cx.begin_turtle(walk, Layout::default());
        if !self.booted {
            cx.end_turtle_with_area(&mut self.area);
            return DrawStep::done();
        }
        self.vp = cx.turtle().rect();
        self.list_id = cx.get_current_draw_list_id();
        if self.metrics.is_none() {
            self.draw_mono.text_style.font_size = 10.0;
            if let Some(run) = self.draw_mono.prepare_single_line_run(cx, "MMMMMMMMMMMMMMMM") {
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
                let c = story::layout(&self.stories, &m);
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
        self.draw_legend(cx);
        cx.end_turtle_with_area(&mut self.area);
        DrawStep::done()
    }
}
