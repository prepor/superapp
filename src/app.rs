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

use crate::core::{self, Dir, Kind, PanelId, Ws};
use crate::data::{self, MailId, MailState};
use crate::e2e;
use crate::spring::{Spring, SpringParams};
use crate::theme;

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
}

fn config() -> &'static Config {
    static CONFIG: std::sync::OnceLock<Config> = std::sync::OnceLock::new();
    CONFIG.get_or_init(|| {
        let mut c = Config {
            out: "e2e/out".into(),
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
                    stage := Stage{}
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

// ---------------------------------------------------------------------------
// Content model: panel bodies as styled lines on a character grid
// ---------------------------------------------------------------------------

/// Text styles the content grammar needs. Everything monochrome except `Err`.
#[derive(Debug, Clone, Copy, PartialEq)]
enum Style {
    /// Body text.
    N,
    /// Fake-bold body text (unread rows).
    Bold,
    /// A bigger fake-bold heading (the contact name).
    Big,
    /// Secondary.
    T2,
    /// Muted.
    Muted,
    /// Uppercase small tracked label.
    Label,
    /// The one colour: errors.
    Err,
}

impl Style {
    fn color(self) -> theme::Rgba {
        match self {
            Style::N | Style::Bold | Style::Big => theme::INK,
            Style::T2 => theme::TEXT2,
            Style::Muted => theme::MUTED,
            Style::Label => theme::TEXT2,
            Style::Err => theme::ERR,
        }
    }
    fn size(self) -> f64 {
        match self {
            Style::Label => theme::LABEL_SIZE,
            Style::Big => theme::FONT_SIZE * 1.25,
            _ => theme::FONT_SIZE,
        }
    }
}

/// Side-effect buttons (never navigation).
#[derive(Debug, Clone, Copy, PartialEq)]
enum BtnAct {
    Refresh,
    Archive,
    Send,
    Discard,
    TryIt,
}

/// Text fields.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum FieldId {
    Filter,
    To,
    Subject,
    Body,
}

/// One run inside a line.
#[derive(Debug, Clone)]
enum Seg {
    /// Plain text.
    T(String, Style),
    /// A link: solid underline opens joined, dotted replaces in place.
    Link {
        label: String,
        target: Kind,
        dotted: bool,
    },
    /// A bordered side-effect button.
    Btn { label: String, act: BtnAct },
    /// A bordered, inert key-cap chip.
    Kbd(String),
    /// A single-line text field, `w` chars wide.
    Fld { id: FieldId, w: usize },
    /// Horizontal gap, in chars.
    Sp(usize),
}

impl Seg {
    fn chars(&self) -> usize {
        match self {
            Seg::T(s, _) => s.chars().count(),
            Seg::Link { label, .. } => label.chars().count(),
            Seg::Btn { label, .. } => label.chars().count() + 2,
            Seg::Kbd(s) => s.chars().count() + 2,
            Seg::Fld { w, .. } => *w,
            Seg::Sp(n) => *n,
        }
    }
}

/// One line of panel content: left-aligned runs, right-aligned runs, an
/// optional hairline under it, and an optional full-row selection identity.
#[derive(Debug, Clone, Default)]
struct Line {
    left: Vec<Seg>,
    right: Vec<Seg>,
    rule: bool,
    /// Draw the rule in ink (table headers) instead of the hairline grey.
    rule_ink: bool,
    /// This line is a selectable inbox row for the given mail.
    row: Option<MailId>,
    /// Pinned above the scrolling region (the filter, table headers). Only a
    /// leading run of pinned lines is honoured.
    pin: bool,
}

impl Line {
    fn text(s: impl Into<String>, st: Style) -> Self {
        Line {
            left: vec![Seg::T(s.into(), st)],
            ..Default::default()
        }
    }
    fn blank() -> Self {
        Line::default()
    }
}

fn trunc(s: &str, max: usize) -> String {
    let n = s.chars().count();
    if n <= max {
        return s.to_string();
    }
    let mut out: String = s.chars().take(max.saturating_sub(1)).collect();
    out.push('…');
    out
}

fn pad_to(s: &str, w: usize) -> String {
    let mut out = trunc(s, w);
    let n = out.chars().count();
    out.extend(std::iter::repeat(' ').take(w.saturating_sub(n)));
    out
}

fn wrap(s: &str, cols: usize) -> Vec<String> {
    let cols = cols.max(8);
    let mut lines = Vec::new();
    let mut cur = String::new();
    let mut cur_n = 0usize;
    for word in s.split(' ') {
        let wn = word.chars().count();
        if cur_n > 0 && cur_n + 1 + wn > cols {
            lines.push(std::mem::take(&mut cur));
            cur_n = 0;
        }
        if cur_n > 0 {
            cur.push(' ');
            cur_n += 1;
        }
        cur.push_str(word);
        cur_n += wn;
    }
    if !cur.is_empty() || lines.is_empty() {
        lines.push(cur);
    }
    lines
}

// ---------------------------------------------------------------------------
// Per-panel volatile UI state
// ---------------------------------------------------------------------------

/// A single-line text field.
#[derive(Debug, Clone, Default)]
struct TextField {
    text: String,
    caret: usize, // chars
}

impl TextField {
    fn insert(&mut self, s: &str) {
        let byte = char_byte(&self.text, self.caret);
        self.text.insert_str(byte, s);
        self.caret += s.chars().count();
    }
    fn backspace(&mut self) {
        if self.caret == 0 {
            return;
        }
        let b0 = char_byte(&self.text, self.caret - 1);
        let b1 = char_byte(&self.text, self.caret);
        self.text.replace_range(b0..b1, "");
        self.caret -= 1;
    }
    fn delete(&mut self) {
        if self.caret >= self.text.chars().count() {
            return;
        }
        let b0 = char_byte(&self.text, self.caret);
        let b1 = char_byte(&self.text, self.caret + 1);
        self.text.replace_range(b0..b1, "");
    }
    fn left(&mut self) {
        self.caret = self.caret.saturating_sub(1);
    }
    fn right(&mut self) {
        self.caret = (self.caret + 1).min(self.text.chars().count());
    }
}

fn char_byte(s: &str, ch: usize) -> usize {
    s.char_indices()
        .nth(ch)
        .map(|(b, _)| b)
        .unwrap_or(s.len())
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
}

impl PanelUi {
    fn for_kind(kind: &Kind, mail: &mut MailState) -> Self {
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
        };
        match kind {
            Kind::Inbox { filter } => {
                if let Some(f) = filter {
                    ui.filter.text = (*f).to_string();
                    ui.filter.caret = ui.filter.text.chars().count();
                }
            }
            Kind::Message { id } => mail.mark_read(id),
            Kind::Compose { re } => {
                if let Some(m) = data::mail(re) {
                    ui.to.text = m.from_email.to_string();
                    ui.to.caret = ui.to.text.chars().count();
                    ui.subject.text = format!("Re: {}", m.subject);
                    ui.subject.caret = ui.subject.text.chars().count();
                }
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
}

#[derive(Debug, Clone)]
struct HitR {
    rect: Rect,
    act: Act,
    cursor: MouseCursor,
    /// What an e2e script can address this element by.
    label: String,
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
}

impl PanelAnim {
    fn spawn(target: core::Rect, title: String) -> Self {
        // Born slightly inset and transparent; springs carry it to place.
        let inset = 12.0;
        let mk = |v| Spring::at_rest(v, SpringParams::movement());
        let mut pa = PanelAnim {
            x: mk(target.x + inset),
            y: mk(target.y + inset),
            w: mk(target.w - 2.0 * inset),
            h: mk(target.h - 2.0 * inset),
            alpha: Spring::at_rest(0.0, SpringParams::fade()),
            title,
        };
        pa.retarget(target);
        pa.alpha.retarget(1.0);
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
}

/// Drawn state: springs keyed by panel, plus fading ghosts of closed panels.
#[derive(Debug, Default)]
struct Anim {
    camera: Option<Spring>,
    panels: HashMap<PanelId, PanelAnim>,
    ghosts: Vec<Ghost>,
}

impl Anim {
    fn camera(&mut self) -> &mut Spring {
        self.camera
            .get_or_insert_with(|| Spring::at_rest(0.0, SpringParams::movement()))
    }

    /// Applies a fresh scene: retarget the living, spawn the new, ghost the gone.
    fn apply(&mut self, scene: &core::Scene, titles: &HashMap<PanelId, String>) {
        self.camera().retarget(scene.camera_x);
        let mut seen = std::collections::HashSet::new();
        for ps in &scene.panels {
            seen.insert(ps.id);
            let title = titles.get(&ps.id).cloned().unwrap_or_default();
            match self.panels.get_mut(&ps.id) {
                Some(pa) => {
                    pa.retarget(ps.rect);
                    pa.title = title;
                }
                None => {
                    self.panels.insert(ps.id, PanelAnim::spawn(ps.rect, title));
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
            });
        }
    }

    fn advance(&mut self, dt: f64) -> bool {
        let mut active = false;
        if let Some(c) = self.camera.as_mut() {
            c.advance(dt);
            active |= !c.is_done();
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

struct State {
    ws: Ws,
    mail: MailState,
    ui: HashMap<PanelId, PanelUi>,
    anim: Anim,
    viewport: DVec2,
    last_frame: Option<Instant>,
    animating: bool,
    hover: Option<Act>,
    field: Option<(PanelId, FieldId)>,
    toast: Option<(String, bool, Instant)>,
}

impl State {
    fn new() -> Self {
        let mut ws = Ws::new();
        ws.open(Kind::Help, None, false);
        let inbox = ws.open(Kind::Inbox { filter: None }, None, false);
        ws.focus = Some(inbox);
        State {
            ws,
            mail: MailState::new(),
            ui: HashMap::new(),
            anim: Anim::default(),
            viewport: dvec2(1440.0, 900.0),
            last_frame: None,
            animating: false,
            hover: None,
            field: None,
            toast: None,
        }
    }

    fn opts(&self) -> core::LayoutOpts {
        core::LayoutOpts { gap: theme::GAP }
    }

    fn vp(&self) -> (f64, f64) {
        (self.viewport.x, self.viewport.y)
    }

    fn panel_title(&self, kind: &Kind) -> String {
        match kind {
            Kind::Help => "help".into(),
            Kind::About => "about".into(),
            Kind::Inbox { filter: Some(f) } => format!("inbox · {f}"),
            Kind::Inbox { filter: None } => "inbox".into(),
            Kind::Message { id } => data::mail(id)
                .map(|m| m.subject.to_string())
                .unwrap_or_else(|| "message".into()),
            Kind::Contact { email } => data::mails()
                .iter()
                .find(|m| m.from_email == *email)
                .map(|m| m.from_name.to_string())
                .unwrap_or_else(|| (*email).to_string()),
            Kind::Compose { re } => data::mail(re)
                .map(|m| format!("re: {}", m.subject))
                .unwrap_or_else(|| "new mail".into()),
        }
    }

    /// Recomputes targets after a mutation and feeds the animator. The camera
    /// follows focus here — and only here, so trackpad pans stay free.
    fn sync(&mut self) {
        let vp = self.vp();
        let opts = self.opts();
        self.ws.ensure_focus_visible(vp, opts);
        // Per-panel ui: create/reset entries, drop dead ones.
        let ids: Vec<PanelId> = self.ws.panels.keys().copied().collect();
        for pid in &ids {
            let kind = self.ws.panels[pid].kind.clone();
            let fresh = match self.ui.get(pid) {
                Some(ui) => ui.kind != kind,
                None => true,
            };
            if fresh {
                let ui = PanelUi::for_kind(&kind, &mut self.mail);
                if matches!(kind, Kind::Compose { .. }) {
                    self.field = Some((*pid, FieldId::Body));
                }
                self.ui.insert(*pid, ui);
            }
        }
        self.ui.retain(|pid, _| self.ws.panels.contains_key(pid));
        if let Some((pid, _)) = self.field {
            if !self.ws.panels.contains_key(&pid) {
                self.field = None;
            }
        }

        let scene = self.ws.scene(self.vp(), self.opts());
        let titles: HashMap<PanelId, String> = self
            .ws
            .panels
            .values()
            .map(|p| (p.id, self.panel_title(&p.kind)))
            .collect();
        self.anim.apply(&scene, &titles);
    }

    /// Trackpad pan: 1:1, no spring.
    fn pan(&mut self, dx: f64) {
        self.ws.pan(dx);
        let cam = {
            let scene = self.ws.scene(self.vp(), self.opts());
            scene.camera_x
        };
        self.anim.camera().jump_to(cam);
    }

    fn toast(&mut self, msg: impl Into<String>, err: bool) {
        self.toast = Some((msg.into(), err, Instant::now()));
    }

    /// Rows the inbox panel currently shows.
    fn inbox_rows(&self, pid: PanelId) -> Vec<&'static data::Mail> {
        let filter = self
            .ui
            .get(&pid)
            .map(|u| u.filter.text.clone())
            .unwrap_or_default();
        self.mail.inbox_filtered(&filter)
    }
}

// ---------------------------------------------------------------------------
// Content builders
// ---------------------------------------------------------------------------

fn kbd(s: &str) -> Seg {
    Seg::Kbd(s.to_string())
}

fn build_lines(state: &State, pid: PanelId, cols: usize) -> Vec<Line> {
    let Some(panel) = state.ws.panels.get(&pid) else {
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
        Line::text("on one scrolling 12×6 workspace.", Style::N),
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
        let unread = state.mail.is_unread(m.id);
        let st = if unread { Style::Bold } else { Style::N };
        v.push(Line {
            left: vec![
                Seg::T(pad_to(m.from_name, from_w), st),
                Seg::Sp(1),
                Seg::Link {
                    label: trunc(m.subject, subj_w),
                    target: Kind::Message { id: m.id },
                    dotted: false,
                },
            ],
            right: vec![Seg::T(m.date.into(), Style::Muted)],
            row: Some(m.id),
            rule: true,
            ..Default::default()
        });
    }
    v
}

fn message_lines(state: &State, id: &MailId, cols: usize) -> Vec<Line> {
    let Some(m) = data::mail(id) else {
        return vec![Line::text("message not found", Style::Muted)];
    };
    let (newer, older) = state.mail.neighbours(id);
    let mut v = Vec::new();
    v.push(Line {
        left: vec![
            Seg::T(pad_to("FROM", 6), Style::Label),
            Seg::Link {
                label: format!("{} <{}>", m.from_name, m.from_email),
                target: Kind::Contact {
                    email: m.from_email,
                },
                dotted: false,
            },
        ],
        ..Default::default()
    });
    v.push(Line {
        left: vec![
            Seg::T(pad_to("TO", 6), Style::Label),
            Seg::T(data::ME.into(), Style::Muted),
        ],
        ..Default::default()
    });
    v.push(Line {
        left: vec![
            Seg::T(pad_to("DATE", 6), Style::Label),
            Seg::T(m.date.into(), Style::N),
        ],
        rule: true,
        ..Default::default()
    });
    if let Some((s, err)) = m.status {
        v.push(Line::text(s, if err { Style::Err } else { Style::T2 }));
        v.push(Line::blank());
    }
    for para in m.body {
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
        target: Kind::Compose { re: m.id },
        dotted: false,
    }];
    v.push(nav);
    v
}

fn contact_lines(state: &State, email: &&'static str) -> Vec<Line> {
    let name = data::mails()
        .iter()
        .find(|m| m.from_email == *email)
        .map(|m| m.from_name)
        .unwrap_or(email);
    let count = state.mail.count_from(email);
    let first = name.split(' ').next().unwrap_or(name).to_lowercase();
    vec![
        Line::text(name, Style::Big),
        Line::text(*email, Style::Muted),
        Line::blank(),
        Line::text(format!("{count} message(s) in mail"), Style::N),
        Line::blank(),
        Line {
            left: vec![Seg::Link {
                label: format!("messages from {first}"),
                target: Kind::Inbox {
                    filter: Some(email),
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
    // The body: free lines; the whole region is one Field hit (built in draw).
    for i in 0..body_rows {
        let text = ui.map(|u| u.body[i].clone()).unwrap_or_default();
        v.push(Line {
            left: vec![Seg::T(text, Style::N), Seg::Fld { id: FieldId::Body, w: 0 }],
            ..Default::default()
        });
    }
    v.push(Line::blank());
    v.push(Line {
        right: vec![
            Seg::Btn {
                label: "discard".into(),
                act: BtnAct::Discard,
            },
            Seg::Sp(1),
            Seg::Btn {
                label: "send".into(),
                act: BtnAct::Send,
            },
        ],
        ..Default::default()
    });
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
#[derive(Script, ScriptHook, Widget)]
pub struct Stage {
    #[uid]
    uid: WidgetUid,
    #[walk]
    walk: Walk,
    #[layout]
    layout: Layout,

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
    #[rust]
    e2e: Option<e2e::Runner>,
    #[rust]
    e2e_timer: Timer,
    #[rust]
    state: Option<Box<State>>,
}

/// How a `key` chord executes: as a synthesized key event, or as text (plain
/// letters reach panels the same way real typing does).
enum ChordExec {
    Ev(KeyEvent),
    Text(String),
}

fn parse_chord(s: &str) -> Option<ChordExec> {
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
        "comma" | "," => Some(KeyCode::Comma),
        "period" | "." => Some(KeyCode::Period),
        "bracketleft" | "[" => Some(KeyCode::LBracket),
        "bracketright" | "]" => Some(KeyCode::RBracket),
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
            | KeyCode::Delete)),
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
        // TextInput events — while the IME is shown. Letter keys (j/k/r, "/",
        // field typing) all arrive that way, so the IME stays on whenever a
        // panel has focus (mosaic's model: "typing only flows after
        // show_text_ime"). Every focus transition passes through kick().
        let want_ime = self
            .state
            .as_deref()
            .is_some_and(|s| s.ws.focus.is_some());
        if want_ime != self.ime_shown {
            self.ime_shown = want_ime;
            if want_ime {
                cx.set_key_focus(self.area);
                cx.show_text_ime(self.area, dvec2(0.0, 0.0));
            } else {
                cx.hide_text_ime();
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
        self.kick(cx);
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
                    match crate::mac::screenshot(&path) {
                        Ok(()) => eprintln!("e2e: shot {}", path.display()),
                        Err(e) => {
                            eprintln!("e2e: FAIL shot {name}: {e}");
                            runner.failures += 1;
                        }
                    }
                }
                e2e::Step::Click { label, fresh } => {
                    let needle = label.to_lowercase();
                    let act = self
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
                        .map(|h| h.act.clone());
                    match act {
                        Some(act) => {
                            eprintln!("e2e: click {label:?}{}", if fresh { " (cmd)" } else { "" });
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
        let Some(state) = self.state.as_deref_mut() else {
            return;
        };
        // Cmd is the workspace modifier (niri's Mod; mosaic's choice too).
        if k.modifiers.logo {
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
                        state.ws.move_panel(f, dir);
                    }
                } else {
                    let vp = state.vp();
                    let opts = state.opts();
                    state.ws.focus_dir(dir, vp, opts);
                }
                self.sync(cx);
                return;
            }
            if k.key_code == KeyCode::KeyW {
                if let Some(f) = state.ws.focus {
                    state.field = None;
                    state.ws.close(f);
                    self.sync(cx);
                }
                return;
            }
            // niri's column operations.
            if let Some(f) = state.ws.focus {
                match k.key_code {
                    KeyCode::LBracket => {
                        state.ws.consume_or_expel(f, Dir::Left);
                        self.sync(cx);
                        return;
                    }
                    KeyCode::RBracket => {
                        state.ws.consume_or_expel(f, Dir::Right);
                        self.sync(cx);
                        return;
                    }
                    KeyCode::Comma => {
                        state.ws.consume_from_right(f);
                        self.sync(cx);
                        return;
                    }
                    KeyCode::Period => {
                        state.ws.expel_bottom(f);
                        self.sync(cx);
                        return;
                    }
                    KeyCode::KeyT => {
                        state.ws.toggle_tabbed(f);
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
        if let Some((pid, fid)) = state.field {
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
                    let f = match fid {
                        FieldId::Filter => &mut ui.filter,
                        FieldId::To => &mut ui.to,
                        FieldId::Subject => &mut ui.subject,
                        FieldId::Body => unreachable!(),
                    };
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
                        state.ws.follow_open(f, Kind::Message { id }, fresh);
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

    /// Character input: field typing, or the focused panel's letter keys.
    fn handle_text(&mut self, cx: &mut Cx, input: &str) {
        let Some(state) = self.state.as_deref_mut() else {
            return;
        };
        if input.is_empty() || input.chars().any(|c| c.is_control()) {
            return;
        }
        if let Some((pid, fid)) = state.field {
            if let Some(ui) = state.ui.get_mut(&pid) {
                match fid {
                    FieldId::Filter => {
                        ui.filter.insert(input);
                        ui.sel = None;
                    }
                    FieldId::To => ui.to.insert(input),
                    FieldId::Subject => ui.subject.insert(input),
                    FieldId::Body => {
                        let (r, c) = ui.caret;
                        let byte = char_byte(&ui.body[r], c);
                        ui.body[r].insert_str(byte, input);
                        ui.caret = (r, c + input.chars().count());
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
                let (newer, older) = state.mail.neighbours(&id);
                let t = if input == "j" { older } else { newer };
                if let Some(t) = t {
                    state.ws.follow_replace(f, Kind::Message { id: t }, false);
                    self.sync(cx);
                }
            }
            (Some(Kind::Message { id }), "r") => {
                state.ws.follow_open(f, Kind::Compose { re: id }, false);
                self.sync(cx);
            }
            _ => {}
        }
    }

    fn resolve_click(&mut self, cx: &mut Cx, act: Act, alt: bool) {
        let Some(state) = self.state.as_deref_mut() else {
            return;
        };
        match act {
            Act::Focus(pid) => {
                state.ws.focus = Some(pid);
                state.field = None;
                self.sync(cx);
            }
            Act::Close(pid) => {
                state.ws.close(pid);
                self.sync(cx);
            }
            Act::Open(pid, kind) => {
                state.ws.follow_open(pid, kind, alt);
                self.sync(cx);
            }
            Act::Replace(pid, kind) => {
                state.ws.follow_replace(pid, kind, alt);
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
                    BtnAct::Refresh => state.toast("inbox refreshed (fake)", false),
                    BtnAct::Archive => {
                        if let Some(Kind::Message { id }) =
                            state.ws.panels.get(&pid).map(|p| p.kind.clone())
                        {
                            state.mail.archive(&id);
                            if let Some(m) = data::mail(&id) {
                                state.toast(format!("archived “{}” (fake)", m.subject), false);
                            }
                            state.ws.close(pid);
                        }
                    }
                    BtnAct::Send => {
                        let to = state
                            .ui
                            .get(&pid)
                            .map(|u| u.to.text.clone())
                            .unwrap_or_default();
                        state.toast(format!("sent to {to} (fake)"), false);
                        state.ws.close(pid);
                    }
                    BtnAct::Discard => {
                        state.ws.close(pid);
                    }
                }
                self.sync(cx);
            }
        }
    }
}

impl Widget for Stage {
    fn handle_event(&mut self, cx: &mut Cx, event: &Event, _scope: &mut Scope) {
        if matches!(event, Event::Startup) {
            if self.state.is_none() {
                let mut s = State::new();
                s.sync();
                self.state = Some(Box::new(s));
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
        }

        match event {
            Event::WindowGeomChange(e) => {
                if let Some(state) = self.state.as_deref_mut() {
                    state.viewport = e.new_geom.inner_size;
                    state.sync();
                }
                cx.redraw_all();
            }

            Event::KeyDown(k) => self.handle_key_down(cx, k),

            Event::TextInput(e) => {
                let input = e.input.clone();
                self.handle_text(cx, &input);
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
                cx.set_key_focus(self.area);
                let act = self.hit_at(e.abs).map(|h| h.act.clone());
                if let Some(act) = act {
                    // cmd+click (alt as a quiet alias): a fresh, un-joined panel.
                    let fresh = e.modifiers.logo || e.modifiers.alt;
                    self.resolve_click(cx, act, fresh);
                }
            }

            Event::Scroll(e) => {
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
                        .map(|h| match &h.act {
                            Act::Focus(pid)
                            | Act::Close(pid)
                            | Act::Btn(pid, _)
                            | Act::Open(pid, _)
                            | Act::Replace(pid, _)
                            | Act::Row(pid, _)
                            | Act::Field(pid, _)
                            | Act::Tab(pid) => *pid,
                        });
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
                if springs_active || toast_active {
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
        let vp = cx.turtle().rect();
        self.origin = vp.pos;
        let dpi = cx.current_dpi_factor();

        if let Some(state) = self.state.as_deref_mut() {
            if (state.viewport - vp.size).length() > 1.0 {
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
        let cam = state.anim.camera().value();
        let to_screen = |r: core::Rect| -> Rect {
            rect(r.x - cam + vp.pos.x, r.y + vp.pos.y, r.w, r.h)
        };

        // Ghosts first: chrome-only, fading out.
        let ghosts = state.anim.ghosts.clone();
        for g in &ghosts {
            let r = to_screen(g.rect);
            let a = g.alpha.value();
            self.draw_chrome(cx, r, &g.title, false, a, None, None);
        }

        // Panels in column order; the focused one last so it draws on top
        // while overlapping mid-animation.
        let mut order: Vec<PanelId> = state
            .ws
            .columns
            .iter()
            .flat_map(|c| c.panels.iter().copied())
            .collect();
        if let Some(f) = state.ws.focus {
            if let Some(i) = order.iter().position(|&p| p == f) {
                let f = order.remove(i);
                order.push(f);
            }
        }
        for pid in order {
            let Some(pa) = state.anim.panels.get(&pid) else {
                continue;
            };
            let r = to_screen(pa.rect());
            if r.pos.x > vp.pos.x + vp.size.x || r.pos.x + r.size.x < vp.pos.x {
                continue;
            }
            let alpha = pa.alpha.value();
            self.draw_panel_full(cx, state, pid, r, alpha);
        }

        // Tab strips above tabbed columns: one title segment per panel, the
        // active one inverted. They ride the active panel's animated rect.
        let hover = state.hover.clone();
        let columns = state.ws.columns.clone();
        for col in &columns {
            if !col.tabbed || col.panels.is_empty() {
                continue;
            }
            let active_idx = col.active.min(col.panels.len() - 1);
            let Some(pa) = state.anim.panels.get(&col.panels[active_idx]) else {
                continue;
            };
            let r = to_screen(pa.rect());
            let alpha = pa.alpha.value();
            let strip = rect(
                r.pos.x,
                r.pos.y - theme::TAB_GAP - theme::TAB_H,
                r.size.x,
                theme::TAB_H,
            );
            if strip.pos.x > vp.pos.x + vp.size.x || strip.pos.x + strip.size.x < vp.pos.x {
                continue;
            }
            let n = col.panels.len() as f64;
            let seg_gap = 2.0;
            let seg_w = ((strip.size.x - (n - 1.0) * seg_gap) / n).max(24.0);
            for (i, pid) in col.panels.iter().enumerate() {
                let sx = strip.pos.x + i as f64 * (seg_w + seg_gap);
                let sr = rect(sx, strip.pos.y, seg_w, theme::TAB_H);
                let act = Act::Tab(*pid);
                let active = i == active_idx;
                let hovered = hover.as_ref() == Some(&act);
                let (bg, fg) = match (active, hovered) {
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
                    .panels
                    .get(pid)
                    .map(|p| state.panel_title(&p.kind))
                    .unwrap_or_default();
                let title_cols = (((seg_w - 12.0) / self.cell.label_step()).max(2.0)) as usize;
                let t = trunc(&title, title_cols);
                let tw = self.cell.label_w(t.chars().count());
                let ty = sr.pos.y + (theme::TAB_H - self.cell.label_line()) / 2.0;
                self.draw_label(cx, sx + ((seg_w - tw) / 2.0).max(6.0), ty, &t, fg, alpha);
                self.hits.push(HitR {
                    rect: sr,
                    act,
                    cursor: MouseCursor::Hand,
                    label: title,
                });
            }
        }

        // Bridges above panels: the join indicator.
        self.draw_flat.new_draw_call(cx);
        for (&a, &b) in state.ws.joins.clone().iter() {
            let (Some(pa), Some(pb)) = (state.anim.panels.get(&a), state.anim.panels.get(&b))
            else {
                continue;
            };
            let ra = to_screen(pa.rect());
            let rb = to_screen(pb.rect());
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

    fn draw_panel_full(&mut self, cx: &mut Cx2d, state: &mut State, pid: PanelId, r: Rect, alpha: f64) {
        let Some(panel) = state.ws.panels.get(&pid) else {
            return;
        };
        let kind = panel.kind.clone();
        let focused = state.ws.focus == Some(pid);
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

        // Extra header actions.
        let head_btn = match kind {
            Kind::Inbox { .. } => Some(("refresh", BtnAct::Refresh)),
            Kind::Message { .. } => Some(("archive", BtnAct::Archive)),
            _ => None,
        };
        if let Some((label, act)) = head_btn {
            let w = self.cell.label_w(label.chars().count()) + 12.0;
            let br = rect(
                r.pos.x + r.size.x - 18.0 - 4.0 - w - 4.0,
                r.pos.y + (theme::HEAD_H - theme::BTN_H) / 2.0,
                w,
                theme::BTN_H,
            );
            self.draw_header_btn(cx, br, label, label, focused, alpha, Act::Btn(pid, act), hover.as_ref());
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
                PanelUi::for_kind(&kind, &mut state.mail)
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
                label: data::mail(&mid)
                    .map(|m| m.subject.to_string())
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
                    _ => (String::new(), 0),
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
        let (pos, size) = crate::mac::visible_frame();
        win.configure_window(cx, size, pos, false, "superapp".to_string());
        if !background_run() {
            crate::mac::activate();
        }
    }
}

impl AppMain for App {
    fn script_mod(vm: &mut ScriptVm) -> ScriptValue {
        makepad_widgets::script_mod(vm);
        self::script_mod(vm)
    }

    fn handle_event(&mut self, cx: &mut Cx, event: &Event) {
        self.match_event(cx, event);
        self.ui.handle_event(cx, event, &mut Scope::empty());

        // Enforce the window shape once the widget tree exists: at Startup the
        // script has not instantiated it, so the configure call above no-ops
        // (mosaic spike B, TASK 2 — same workaround).
        if !self.shaped && self.shape_tries < 240 {
            if let Event::NextFrame(_) | Event::Draw(_) = event {
                self.shape_tries += 1;
                let win = self.ui.window(cx, ids!(main_window));
                if win.window_id().is_some() {
                    let (pos, size) = crate::mac::visible_frame();
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
/// binary calls.
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
