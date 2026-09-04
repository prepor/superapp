//! The chrome: the strip of workspaces, the panels on it, and the sheet
//! over them.
//!
//! A panel is a bordered box with a header carrying its title and its close
//! button, a body the hosted widget draws into, and — this is the change
//! CR-010 makes — a **bar at the foot**, built on every draw from
//! [`Panel::verbs`](kernel::panel::Panel::verbs). Nothing here knows what a
//! panel is about; it knows what a panel wears.

use std::collections::HashMap;

use kernel::layout::{Rect as KRect, SlotId};
use kernel::theme;
use makepad_widgets::*;

use super::bar;
use super::hits::{Act, Hit};
use super::stage::Stage;

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

/// A theme colour at an alpha.
#[must_use]
pub fn rgba_a(c: theme::Rgba, alpha: f64) -> Vec4f {
    vec4(c[0], c[1], c[2], c[3] * alpha as f32)
}

#[must_use]
pub fn rect(x: f64, y: f64, w: f64, h: f64) -> Rect {
    Rect {
        pos: dvec2(x, y),
        size: dvec2(w, h),
    }
}

/// `s` cut to `max` characters, with an ellipsis when it did not fit.
#[must_use]
pub fn trunc(s: &str, max: usize) -> String {
    let n = s.chars().count();
    if n <= max {
        return s.to_string();
    }
    let mut out: String = s.chars().take(max.saturating_sub(1)).collect();
    out.push('…');
    out
}

/// Where a label carries its accelerator, if it carries it at all.
#[must_use]
pub fn accel_idx(label: &str, accel: char) -> Option<usize> {
    label.chars().position(|c| c.eq_ignore_ascii_case(&accel))
}

/// A label split around its accelerator: what comes before the letter, the
/// letter, and what follows. A label that does not carry its key is all
/// `pre`, and so draws no mark.
#[must_use]
pub fn split_accel(label: &str, accel: Option<char>) -> (String, String, String) {
    let Some(i) = accel.and_then(|c| accel_idx(label, c)) else {
        return (label.to_string(), String::new(), String::new());
    };
    let mut it = label.chars();
    let pre: String = it.by_ref().take(i).collect();
    let key: String = it.next().into_iter().collect();
    (pre, key, it.collect())
}

/// The type scale, as the char grid draws it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Style {
    N,
    Muted,
    Err,
}

impl Style {
    #[must_use]
    pub fn color(self) -> theme::Rgba {
        match self {
            Style::N => theme::INK,
            Style::Muted => theme::MUTED,
            Style::Err => theme::ERR,
        }
    }

    #[must_use]
    pub fn size(self) -> f64 {
        theme::FONT_SIZE
    }
}

/// The mono face as this display renders it: measured once per scale, so
/// the char grid and the layout agree about how wide a column is.
#[derive(Debug, Clone, Copy)]
pub struct CellFont {
    pub adv: f64,
    pub line_h: f64,
    pub natural: f64,
    pub dpi: f64,
}

impl Default for CellFont {
    fn default() -> CellFont {
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
    #[must_use]
    pub fn label_adv(&self) -> f64 {
        self.adv * (theme::LABEL_SIZE / theme::FONT_SIZE)
    }

    /// The same, tracking included.
    #[must_use]
    pub fn label_step(&self) -> f64 {
        self.label_adv() * (1.0 + theme::LABEL_TRACK)
    }

    /// Drawn width of a tracked label.
    #[must_use]
    pub fn label_w(&self, chars: usize) -> f64 {
        if chars == 0 {
            return 0.0;
        }
        chars as f64 * self.label_step() - self.label_adv() * theme::LABEL_TRACK
    }

    /// Natural line height at label size, for vertical centring.
    #[must_use]
    pub fn label_line(&self) -> f64 {
        self.natural * (theme::LABEL_SIZE / theme::FONT_SIZE)
    }
}

impl Stage {
    /// Measures the mono face once per display scale.
    pub(super) fn measure_cell(&mut self, cx: &mut Cx2d, dpi: f64) {
        if (self.cell.dpi - dpi).abs() < 1e-9 {
            return;
        }
        self.draw_mono.text_style.font_size = theme::FONT_SIZE as f32;
        let Some(run) = self
            .draw_mono
            .prepare_single_line_run(cx, "MMMMMMMMMMMMMMMM")
        else {
            return;
        };
        let width = f64::from(run.width_in_lpxs) / 16.0;
        let asc = f64::from(run.ascender_in_lpxs);
        let line = asc - f64::from(run.descender_in_lpxs);
        if width > 0.0 && line > 0.0 {
            self.cell = CellFont {
                adv: width,
                // Leave extra space between lines.
                line_h: (line * 1.28).ceil(),
                natural: line,
                dpi,
            };
        }
    }

    pub(super) fn set_text(&mut self, st: Style, alpha: f64) {
        self.draw_mono.text_style.font_size = st.size() as f32;
        self.draw_mono.color = rgba_a(st.color(), alpha);
    }

    /// Draws an uppercase label with extra letter spacing; answers its width.
    pub(super) fn draw_label(
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

    /// As [`Stage::draw_label`], but the character at `accel` is drawn
    /// three times, nudged — the grid's own fake bold, narrowed from a run
    /// to a single glyph.
    #[allow(clippy::too_many_arguments)]
    pub(super) fn draw_label_accel(
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
                    self.draw_mono.draw_abs(cx, dvec2(dx + 0.35, y), g);
                    self.draw_mono.draw_abs(cx, dvec2(dx + 0.7, y), g);
                }
            }
            dx += step;
        }
        dx - x
    }

    /// Chrome only: fill, border, header. Used for ghosts and as the first
    /// layer of a live panel.
    #[allow(clippy::too_many_arguments)]
    pub(super) fn draw_chrome(
        &mut self,
        cx: &mut Cx2d,
        r: Rect,
        title: &str,
        focused: bool,
        alpha: f64,
        close: Option<SlotId>,
        hover: Option<&Act>,
    ) {
        self.draw_panel.color = rgba_a(theme::BG, alpha);
        self.draw_panel.border_color = rgba_a(theme::INK, alpha);
        self.draw_panel.border_size = 1.0;
        self.draw_panel.alpha = alpha as f32;
        self.draw_panel.draw_abs(cx, r);

        let head = rect(r.pos.x, r.pos.y, r.size.x, theme::HEAD_H);
        self.draw_flat.color = rgba_a(theme::INK, alpha);
        if focused {
            self.draw_flat.draw_abs(cx, head);
        } else {
            self.draw_flat.draw_abs(
                cx,
                rect(r.pos.x, r.pos.y + theme::HEAD_H - 1.0, r.size.x, 1.0),
            );
        }

        // The header wears nothing but the title and the close button, so
        // the title only has to clear one box.
        let btns_w = theme::BTN_H + 8.0;
        let title_cols = (((r.size.x - 16.0 - btns_w) / self.cell.label_step()).max(4.0)) as usize;
        let t = trunc(title, title_cols);
        let ty = r.pos.y + (theme::HEAD_H - self.cell.label_line()) / 2.0;
        let color = if focused { theme::BG } else { theme::INK };
        self.draw_label(cx, r.pos.x + 8.0, ty, &t, color, alpha);

        if let Some(slot) = close {
            let bw = theme::BTN_H;
            let br = rect(
                r.pos.x + r.size.x - bw - 4.0,
                r.pos.y + (theme::HEAD_H - bw) / 2.0,
                bw,
                bw,
            );
            self.draw_box_btn(
                cx,
                br,
                "×",
                "close",
                focused,
                alpha,
                Act::Close(slot),
                hover,
                None,
            );
        }
    }

    /// A bordered label that fires: the close box, and every button of a
    /// bar. On an inverted header it inverts back when hovered.
    #[allow(clippy::too_many_arguments)]
    pub(super) fn draw_box_btn(
        &mut self,
        cx: &mut Cx2d,
        r: Rect,
        label: &str,
        hit_label: &str,
        inverted: bool,
        alpha: f64,
        act: Act,
        hover: Option<&Act>,
        accel: Option<usize>,
    ) {
        let hovered = hover == Some(&act);
        let (bg, fg) = match (inverted, hovered) {
            (true, false) => (theme::INK, theme::BG),
            (true, true) | (false, false) => (theme::BG, theme::INK),
            (false, true) => (theme::INK, theme::BG),
        };
        self.draw_panel.color = rgba_a(bg, alpha);
        self.draw_panel.border_color = rgba_a(if inverted { theme::BG } else { theme::INK }, alpha);
        self.draw_panel.border_size = 1.0;
        self.draw_panel.alpha = alpha as f32;
        self.draw_panel.draw_abs(cx, r);
        let tw = self.cell.label_w(label.chars().count());
        let tx = r.pos.x + (r.size.x - tw) / 2.0;
        let ty = r.pos.y + (r.size.y - self.cell.label_line()) / 2.0;
        self.draw_label_accel(cx, tx, ty, label, fg, alpha, accel);
        self.hits
            .push(Hit::act(hit_label, r, MouseCursor::Hand, act));
    }

    /// The bar at a panel's foot, built from its verbs: buttons for what
    /// acts on the panel, links for where it goes.
    ///
    /// `bold` is the set of letters a chord would reach here now
    /// ([`bar::bold`]); a verb whose letter is outside it is drawn without
    /// its mark, and still fires on click.
    #[allow(clippy::too_many_arguments)]
    pub(super) fn draw_bar(
        &mut self,
        cx: &mut Cx2d,
        slot: SlotId,
        verbs: &[kernel::panel::Verb],
        strip: Rect,
        alpha: f64,
        bold: super::keys::Letters,
        hover: Option<&Act>,
    ) {
        bar::check(verbs);
        self.draw_flat.color = rgba_a(theme::RULE, alpha);
        self.draw_flat
            .draw_abs(cx, rect(strip.pos.x, strip.pos.y, strip.size.x, 1.0));
        for e in bar::entries(verbs, &self.cell, strip) {
            let v = &verbs[e.at];
            let (label, id) = (v.label.clone(), v.id);
            let accel = v
                .accel
                .filter(|c| bold.has(*c))
                .and_then(|c| accel_idx(&label, c));
            let act = Act::Verb(slot, id);
            if e.button {
                self.draw_box_btn(cx, e.rect, &label, &label, false, alpha, act, hover, accel);
            } else {
                // A link, not a button: the three signals of the
                // grammar still hold at the foot of a panel.
                let hovered = hover == Some(&act);
                if hovered {
                    self.draw_flat.color = rgba_a(theme::HOVER, alpha);
                    self.draw_flat.draw_abs(cx, e.rect);
                }
                let ty = e.rect.pos.y + (e.rect.size.y - self.cell.label_line()) / 2.0;
                let w =
                    self.draw_label_accel(cx, e.rect.pos.x, ty, &label, theme::INK, alpha, accel);
                self.draw_flat.color = rgba_a(theme::INK, alpha);
                self.draw_flat.draw_abs(
                    cx,
                    rect(
                        e.rect.pos.x,
                        ty + self.cell.label_line() + 1.0,
                        w.max(4.0),
                        1.0,
                    ),
                );
                self.hits
                    .push(Hit::act(label, e.rect, MouseCursor::Hand, act));
            }
        }
    }
}

/// Where a strip rectangle lands on the screen, given the camera and the
/// vertical slide between workspaces.
pub(super) struct Camera {
    pub vp: Rect,
    pub cam_x: f64,
    pub slide: f64,
    pub step: f64,
}

impl Camera {
    #[must_use]
    pub fn to_screen(&self, r: KRect, ws: usize) -> Rect {
        rect(
            r.x - self.cam_x + self.vp.pos.x,
            r.y + (ws as f64 - self.slide) * self.step + self.vp.pos.y,
            r.w,
            r.h,
        )
    }

    #[must_use]
    pub fn off_screen(&self, r: &Rect) -> bool {
        r.pos.x > self.vp.pos.x + self.vp.size.x
            || r.pos.x + r.size.x < self.vp.pos.x
            || r.pos.y > self.vp.pos.y + self.vp.size.y
            || r.pos.y + r.size.y < self.vp.pos.y
    }
}

/// Titles by slot, for the animator and the tab strips.
#[must_use]
pub(super) fn titles(session: &kernel::session::Session) -> HashMap<SlotId, String> {
    session
        .panels()
        .into_iter()
        .map(|(s, i)| (s, i.borrow().title()))
        .collect()
}

impl Stage {
    /// The workspace strip: ghosts, panels, tab strips, bridges — and then
    /// the sheet over all of it.
    pub(super) fn draw_scene(&mut self, cx: &mut Cx2d, sh: &mut super::stage::Shell, vp: Rect) {
        // Retained widgets otherwise outlive their panels: a closed slot
        // drops from the layout, but its entry here would linger.
        let live: std::collections::HashSet<SlotId> =
            sh.session.panels().iter().map(|(s, _)| *s).collect();
        self.hosted
            .retain(|slot, _| super::hosted::is_overlay(*slot) || live.contains(slot));
        self.hosted_for.retain(|slot, _| live.contains(slot));
        self.field_keeps.retain(|slot, _| live.contains(slot));
        sh.anim.retain(&live);

        let active = sh.session.ws().active;
        let cam = Camera {
            vp,
            cam_x: sh.anim.camera().value(),
            slide: sh.anim.slide().value(),
            step: vp.size.y + theme::GAP,
        };

        // Ghosts first: chrome-only, fading out where they stood.
        for g in sh.anim.ghosts.clone() {
            let r = cam.to_screen(g.rect, g.ws);
            if cam.off_screen(&r) {
                continue;
            }
            self.draw_chrome(cx, r, &g.title, false, g.alpha.value(), None, None);
        }

        // Panels left to right, the focused one last so it draws on top
        // while rectangles overlap mid-animation.
        let focus = sh.session.focus();
        let mut order: Vec<(SlotId, usize, KRect, f64)> = sh
            .anim
            .panels
            .iter()
            .map(|(s, pa)| (*s, pa.ws, pa.rect(), pa.alpha.value()))
            .collect();
        order.sort_by(|a, b| {
            let key = |t: &(SlotId, usize, KRect, f64)| (t.1, Some(t.0) == focus, t.2.x, t.0);
            key(a)
                .partial_cmp(&key(b))
                .unwrap_or(std::cmp::Ordering::Equal)
        });
        let shown = self.shown_slots(sh);
        for (slot, ws, krect, alpha) in order {
            let r = cam.to_screen(krect, ws);
            if cam.off_screen(&r) {
                continue;
            }
            let interactive = ws == active && shown.contains(&slot);
            if !interactive && alpha < 0.02 {
                continue; // a fully faded hidden tab
            }
            let before = self.hits.len();
            self.draw_panel_full(cx, sh, slot, r, alpha);
            if !interactive {
                // Mid-crossfade, or another workspace: visible, not hittable.
                self.hits.truncate(before);
            }
        }

        self.draw_tabs(cx, sh, &cam);
        self.draw_bridges(cx, sh, &cam);

        // An empty workspace names itself, so a switch onto a blank screen
        // reads as a place, not a bug.
        if sh.session.ws().is_empty() && sh.anim.ghosts.is_empty() {
            let msg = format!("workspace {} — cmd+shift+№ brings a panel here", active + 1);
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

        self.draw_sheet(cx, sh, vp);
    }

    /// Which slots their column actually shows: any slot of a normal
    /// column, and the active tab of a tabbed one.
    fn shown_slots(&self, sh: &super::stage::Shell) -> std::collections::HashSet<SlotId> {
        let ws = sh.session.ws();
        let mut out = std::collections::HashSet::new();
        for col in &ws.columns {
            for (i, slot) in col.slots.iter().enumerate() {
                if !col.tabbed || col.active.min(col.slots.len().saturating_sub(1)) == i {
                    out.insert(*slot);
                }
            }
        }
        out
    }

    /// A panel node of the panels library: the one panel at the whole
    /// viewport, then the sheet over it — so a toast and the launcher still
    /// show on a mount worked by hand.
    pub(super) fn draw_solo(
        &mut self,
        cx: &mut Cx2d,
        sh: &mut super::stage::Shell,
        vp: Rect,
        slot: SlotId,
    ) {
        let r = rect(
            vp.pos.x + theme::GAP,
            vp.pos.y + theme::GAP,
            (vp.size.x - 2.0 * theme::GAP).max(40.0),
            (vp.size.y - 2.0 * theme::GAP).max(40.0),
        );
        if sh.session.panel(slot).is_some() {
            self.draw_panel_full(cx, sh, slot, r, 1.0);
        }
        self.draw_sheet(cx, sh, vp);
    }

    /// One panel: chrome, bar, and the hosted widget between them.
    fn draw_panel_full(
        &mut self,
        cx: &mut Cx2d,
        sh: &mut super::stage::Shell,
        slot: SlotId,
        r: Rect,
        alpha: f64,
    ) {
        let Some(inst) = sh.session.panel(slot) else {
            return;
        };
        let (title, verbs) = {
            let p = inst.borrow();
            (p.title(), p.verbs())
        };
        let focused = sh.session.focus() == Some(slot);
        let hover = sh.hover.clone();

        // The whole panel: a click focuses it. Bottom-most, so anything
        // named wins over it.
        self.hits.push(Hit::act(
            title.clone(),
            r,
            MouseCursor::Default,
            Act::Focus(slot),
        ));
        self.draw_chrome(cx, r, &title, focused, alpha, Some(slot), hover.as_ref());

        let bar_h = if verbs.is_empty() { 0.0 } else { bar::BAR_H };
        if !verbs.is_empty() {
            // Where this bar stands in the chord routing order, and so what
            // it may promise: the focused panel's own letters less what its
            // widget keeps, the previewed panel's less what the focused bar
            // wears too, and nothing at all anywhere else.
            let focus = sh.session.focus();
            let kept = self.field_letters(focus);
            let driver = sh.session.join_parent_of(slot);
            let driving = driver.filter(|d| Some(*d) == focus && !focused);
            let driver_verbs = driving
                .and_then(|d| sh.session.panel(d))
                .map(|p| p.borrow().verbs())
                .unwrap_or_default();
            let reach = match (focused, driving.is_some()) {
                (true, _) => bar::Reach::Focused { kept },
                (false, true) => bar::Reach::Preview {
                    kept,
                    driver: &driver_verbs,
                },
                (false, false) => bar::Reach::Away,
            };
            let strip = bar::strip(r);
            let bold = bar::bold(&verbs, reach);
            self.draw_bar(cx, slot, &verbs, strip, alpha, bold, hover.as_ref());
        }

        let body = rect(
            r.pos.x + 1.0,
            r.pos.y + theme::HEAD_H,
            r.size.x - 2.0,
            (r.size.y - theme::HEAD_H - bar_h - 1.0).max(0.0),
        );
        if body.size.y < 4.0 {
            return;
        }
        self.draw_hosted(cx, sh, slot, body);
    }

    /// Tab strips above tabbed columns: one title segment per slot, the
    /// active one inverted. They ride the active slot's animated rect.
    fn draw_tabs(&mut self, cx: &mut Cx2d, sh: &mut super::stage::Shell, cam: &Camera) {
        let active = sh.session.ws().active;
        let columns = sh.session.ws().columns.clone();
        let hover = sh.hover.clone();
        for col in &columns {
            if !col.tabbed || col.slots.is_empty() {
                continue;
            }
            let at = col.active.min(col.slots.len() - 1);
            let Some(pa) = sh.anim.panels.get(&col.slots[at]) else {
                continue;
            };
            let r = cam.to_screen(pa.rect(), active);
            // The strip belongs to the column, not to one tab: during a
            // crossfade the two alphas sum to ~1, so it holds steady.
            let alpha = col
                .slots
                .iter()
                .filter_map(|s| sh.anim.panels.get(s))
                .map(|pa| pa.alpha.value())
                .sum::<f64>()
                .min(1.0);
            let strip = rect(
                r.pos.x,
                r.pos.y - theme::TAB_GAP - theme::TAB_H,
                r.size.x,
                theme::TAB_H,
            );
            if cam.off_screen(&strip) {
                continue;
            }
            let n = col.slots.len() as f64;
            let seg_gap = 2.0;
            let seg_w = ((strip.size.x - (n - 1.0) * seg_gap) / n).max(24.0);
            for (i, slot) in col.slots.iter().enumerate() {
                let sx = strip.pos.x + i as f64 * (seg_w + seg_gap);
                let sr = rect(sx, strip.pos.y, seg_w, theme::TAB_H);
                let act = Act::Tab(*slot);
                let (bg, fg) = match (i == at, hover.as_ref() == Some(&act)) {
                    (true, _) => (theme::INK, theme::BG),
                    (false, true) => (theme::HOVER, theme::INK),
                    (false, false) => (theme::BG, theme::INK),
                };
                self.draw_panel.color = rgba_a(bg, alpha);
                self.draw_panel.border_color = rgba_a(theme::INK, alpha);
                self.draw_panel.border_size = 1.0;
                self.draw_panel.alpha = alpha as f32;
                self.draw_panel.draw_abs(cx, sr);
                let title = sh
                    .session
                    .panel(*slot)
                    .map(|p| p.borrow().title())
                    .unwrap_or_default();
                let cols = ((seg_w - 12.0) / self.cell.label_step()).max(2.0) as usize;
                let t = trunc(&title, cols);
                let tw = self.cell.label_w(t.chars().count());
                let ty = sr.pos.y + (theme::TAB_H - self.cell.label_line()) / 2.0;
                self.draw_label(cx, sx + ((seg_w - tw) / 2.0).max(6.0), ty, &t, fg, alpha);
                self.hits.push(Hit::act(title, sr, MouseCursor::Hand, act));
            }
        }
    }

    /// The join indicator: a double rule between a parent and its child.
    fn draw_bridges(&mut self, cx: &mut Cx2d, sh: &mut super::stage::Shell, cam: &Camera) {
        let active = sh.session.ws().active;
        let bridges = sh.session.scene().bridges.clone();
        self.draw_flat.new_draw_call(cx);
        for (a, b) in bridges {
            let (Some(pa), Some(pb)) = (sh.anim.panels.get(&a), sh.anim.panels.get(&b)) else {
                continue;
            };
            let (aa, ab) = (pa.alpha.value(), pb.alpha.value());
            let (ra, rb) = (
                cam.to_screen(pa.rect(), active),
                cam.to_screen(pb.rect(), active),
            );
            if cam.off_screen(&ra) && cam.off_screen(&rb) {
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
            self.draw_flat.color = rgba_a(theme::INK, aa.min(ab));
            self.draw_flat.draw_abs(cx, rect(x0, y - 2.0, w, 1.0));
            self.draw_flat.draw_abs(cx, rect(x0, y + 1.0, w, 1.0));
        }
    }

    /// The modal overlays and the toasts, over whatever the stage drew.
    fn draw_sheet(&mut self, cx: &mut Cx2d, sh: &mut super::stage::Shell, vp: Rect) {
        self.draw_overlay(cx, sh, vp);

        // The toasts, above everything: newest at the bottom right, each
        // fading out three seconds after it was said. The world's clock, so
        // a run's pictures are the same every time.
        let now = sh.session.now();
        sh.toasts.retain(|t| now - t.at <= 3.0);
        let mut y = vp.pos.y + vp.size.y - 12.0;
        for t in sh.toasts.clone().iter().rev() {
            let age = now - t.at;
            let a = (3.0 - age).clamp(0.0, 0.25) / 0.25;
            let w = t.msg.chars().count() as f64 * self.cell.adv + 20.0;
            let h = self.cell.line_h + 10.0;
            y -= h;
            let r = rect(vp.pos.x + vp.size.x - w - 12.0, y, w, h);
            y -= 6.0;
            let border = if t.err { theme::ERR } else { theme::INK };
            self.draw_panel.new_draw_call(cx);
            self.draw_panel.color = rgba_a(theme::BG, a);
            self.draw_panel.border_color = rgba_a(border, a);
            self.draw_panel.border_size = 1.0;
            self.draw_panel.alpha = a as f32;
            self.draw_panel.draw_abs(cx, r);
            self.draw_mono.new_draw_call(cx);
            self.set_text(if t.err { Style::Err } else { Style::N }, a);
            self.draw_mono
                .draw_abs(cx, r.pos + dvec2(10.0, 5.0), &t.msg);
        }
    }
}
