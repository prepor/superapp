//! Overview owns its own geometry and camera. No hosted panel content is
//! drawn into a tile, and no layout changes until a drag is released, or a
//! tile pulled down its column has gone.

use kernel::layout::{DropTarget, SlotId, Ws, WS_N};
use kernel::nav::Nav;
use kernel::session::Action;
use kernel::spring::{Spring, SpringParams};
use kernel::theme;
use makepad_widgets::*;

use super::draw::{rect, rgba_a, trunc};
use super::hits::{Act, Hit};
use super::overlays::Overlay;
use super::stage::{Shell, Stage};
use super::touch::Mode;

#[cfg(test)]
#[path = "overview_tests.rs"]
mod tests;

const PAD: f64 = 16.0;
const WORKSPACE_TOP: f64 = 56.0;
const WORKSPACE_W: f64 = 112.0;
const WORKSPACE_H: f64 = 68.0;
const WORKSPACE_STEP: f64 = 124.0;
const BODY_TOP: f64 = 152.0;
const TILE_TOP: f64 = 176.0;
const TILE_H: f64 = 140.0;
const ROW_STEP: f64 = 152.0;
const COL_GAP: f64 = 24.0;
const DWELL: f64 = 0.7;

#[derive(Debug, Default)]
pub struct OverviewState {
    workspace: Option<usize>,
    scroll: DVec2,
    workspace_scroll: f64,
    drag: Option<PanelDrag>,
    swipe: Option<TileSwipe>,
}

/// A panel tile pulled down its column: the phone's close.
///
/// The tile follows the finger and fades as it goes. Short of
/// [`SWIPE_CLOSE`] a lift springs it back; past it the tile carries on off
/// the bottom of the strip, and the panel closes when it has gone — so the
/// tile is out of sight before the column reflows under it. Like a row's
/// curtain, it outlives its finger.
#[derive(Debug)]
pub struct TileSwipe {
    pub slot: SlotId,
    /// The tile's rectangle where it stood.
    pub rect: Rect,
    /// How far down it has been taken.
    pub dy: Spring,
    /// Set on a committing lift: the close runs when the spring lands.
    pub commit: bool,
}

impl TileSwipe {
    /// Whether a lift now would close the panel.
    #[must_use]
    pub fn armed(&self) -> bool {
        self.dy.value() >= SWIPE_CLOSE
    }
}

/// How far down a tile must be taken for a lift to close its panel: half
/// the tile's own height.
pub const SWIPE_CLOSE: f64 = TILE_H / 2.0;

#[derive(Debug)]
struct PanelDrag {
    slot: SlotId,
    point: DVec2,
    offset: DVec2,
    size: DVec2,
    /// A workspace is only a destination after an uninterrupted dwell.
    hover: Option<(usize, f64)>,
    target: Option<(DropTarget, Rect)>,
}

#[derive(Debug)]
struct Tiles {
    body: Rect,
    width: f64,
    scroll: DVec2,
}

impl Tiles {
    fn new(vp: Rect, scroll: DVec2) -> Self {
        Self {
            body: rect(
                vp.pos.x,
                vp.pos.y + BODY_TOP,
                vp.size.x,
                (vp.size.y - BODY_TOP - PAD).max(0.0),
            ),
            // Narrow, so more of the workspace is on the glass at once: a
            // tile is a name and a word about it, not a likeness of the
            // panel, and three columns in view beat two half-shown ones.
            width: (vp.size.x * 0.32).clamp(132.0, 190.0),
            scroll,
        }
    }

    fn tile(&self, col: usize, row: usize) -> Rect {
        rect(
            self.body.pos.x + PAD + col as f64 * (self.width + COL_GAP) - self.scroll.x,
            self.body.pos.y + TILE_TOP - BODY_TOP + row as f64 * ROW_STEP - self.scroll.y,
            self.width,
            TILE_H,
        )
    }

    fn limit(&self, ws: &Ws) -> DVec2 {
        let columns = ws.columns.len();
        let rows = ws.columns.iter().map(|c| c.slots.len()).max().unwrap_or(0);
        dvec2(
            (PAD * 2.0 + columns as f64 * (self.width + COL_GAP) - COL_GAP - self.body.size.x)
                .max(0.0),
            (TILE_TOP - BODY_TOP + rows as f64 * ROW_STEP - 12.0 - self.body.size.y).max(0.0),
        )
    }

    /// The preview and the release share this exact target, including tabs.
    fn target(&self, ws: &Ws, slot: SlotId, p: DVec2) -> Option<(DropTarget, Rect)> {
        if !self.body.contains(p) {
            return None;
        }
        for (col, column) in ws.columns.iter().enumerate() {
            let r = self.tile(col, 0);
            if p.x < r.pos.x + self.width * 0.18 || p.x > r.pos.x + self.width * 0.82 {
                continue;
            }
            let others: Vec<_> = column
                .slots
                .iter()
                .enumerate()
                .filter(|(_, id)| **id != slot)
                .collect();
            if others.is_empty() {
                return None;
            }
            let row = others
                .iter()
                .filter(|(r, _)| {
                    let tile = self.tile(col, *r);
                    p.y > tile.pos.y + tile.size.y / 2.0
                })
                .count();
            let y = if row < others.len() {
                self.tile(col, others[row].0).pos.y - 6.0
            } else {
                let last = self.tile(col, others[others.len() - 1].0);
                last.pos.y + last.size.y + 6.0
            };
            let bar_y = (y - 2.0).clamp(
                self.body.pos.y,
                (self.body.pos.y + self.body.size.y - 4.0).max(self.body.pos.y),
            );
            return Some((
                DropTarget::Into { col, row },
                rect(r.pos.x, bar_y, self.width, 4.0),
            ));
        }
        let at = (0..=ws.columns.len())
            .min_by(|a, b| {
                let distance = |col| (self.tile(col, 0).pos.x - COL_GAP / 2.0 - p.x).abs();
                distance(*a).total_cmp(&distance(*b))
            })
            .unwrap_or(0);
        Some((
            DropTarget::Boundary { at },
            rect(
                self.tile(at, 0).pos.x - COL_GAP / 2.0 - 2.0,
                self.body.pos.y + 8.0,
                4.0,
                self.body.size.y - 16.0,
            ),
        ))
    }
}

fn clipped(r: Rect, clip: Rect) -> Option<Rect> {
    let lo = dvec2(r.pos.x.max(clip.pos.x), r.pos.y.max(clip.pos.y));
    let hi = dvec2(
        (r.pos.x + r.size.x).min(clip.pos.x + clip.size.x),
        (r.pos.y + r.size.y).min(clip.pos.y + clip.size.y),
    );
    (hi.x > lo.x && hi.y > lo.y).then_some(Rect {
        pos: lo,
        size: hi - lo,
    })
}

/// Where the ═ between a parent's tile and its joined child's runs: across
/// the gap, level with the child's title, as the stage draws it between the
/// panels themselves. Tiles in different rows share no height, so the rule
/// keeps to the child's title and points at the tile that hangs from the
/// column to its left. `None` when the tiles do not stand side by side.
fn bridge(parent: Rect, child: Rect, title_mid: f64) -> Option<Rect> {
    let x0 = parent.pos.x + parent.size.x;
    let w = child.pos.x - x0;
    (w > 0.0).then(|| rect(x0, child.pos.y + title_mid - 2.0, w, 4.0))
}

impl Stage {
    fn overview_vp(&self, sh: &Shell) -> Rect {
        Rect {
            pos: self.origin,
            size: sh.viewport,
        }
    }

    fn overview_ws(&self, sh: &Shell) -> usize {
        self.overview
            .workspace
            .unwrap_or(sh.session.ws().active)
            .min(WS_N - 1)
    }

    pub(super) fn open_overview(&mut self, cx: &mut Cx, sh: &mut Shell) {
        self.cancel_overview_drag();
        self.settle_tile_swipe(sh);
        self.overview.workspace = Some(sh.session.ws().active);
        self.overview.scroll = DVec2::default();
        let tiles = Tiles::new(self.overview_vp(sh), self.overview.scroll);
        if let Some((col, _)) = sh.session.focus().and_then(|s| sh.session.ws().locate(s)) {
            self.overview.scroll.x = (col as f64 * (tiles.width + COL_GAP) + tiles.width
                - (sh.viewport.x - 2.0 * PAD))
                .max(0.0);
        }
        self.overview.workspace_scroll = (sh.session.ws().active as f64 * WORKSPACE_STEP
            + WORKSPACE_W
            - (sh.viewport.x - PAD * 2.0))
            .max(0.0);
        sh.overlay = Overlay::Overview;
        self.pending_focus = None;
        cx.set_key_focus(self.area);
        cx.hide_text_ime();
        sh.session.redraw();
    }

    pub(super) fn overview_focus(&mut self, cx: &mut Cx, sh: &mut Shell, slot: SlotId) {
        self.cancel_overview_drag();
        sh.overlay = Overlay::None;
        sh.session.nav(Nav::Focus(slot));
        cx.set_key_focus(self.area);
        sh.session.redraw();
    }

    pub(super) fn overview_workspace(&mut self, sh: &mut Shell, k: usize) {
        if k >= WS_N {
            return;
        }
        self.cancel_overview_drag();
        self.overview.workspace = Some(k);
        self.overview.scroll = DVec2::default();
        if sh.session.switch(k) {
            // The stack behind the tiles lands on the new space at once.
            // Overview covers it while it is up, but not while it comes
            // down: a slide still running would show through the fade the
            // moment a panel tile is tapped.
            let cam = sh.session.scene().camera_x;
            sh.anim.camera().jump_to(cam);
            sh.anim.slide().jump_to(k as f64);
        }
        sh.session.redraw();
    }

    pub(super) fn cancel_overview_drag(&mut self) -> bool {
        if self.overview.drag.take().is_none() {
            return false;
        }
        self.touch.mode = Mode::Dead;
        true
    }

    /// A finger past the slop, downward, on a panel tile: the tile comes
    /// with it from here. A tile still on its way off the strip from the
    /// last pull closes now rather than being forgotten under this one.
    pub(super) fn overview_swipe_start(&mut self, sh: &mut Shell, slot: SlotId, r: Rect, dy: f64) {
        self.abandon_tile_swipe(sh);
        let mut swipe = TileSwipe {
            slot,
            rect: r,
            dy: Spring::at_rest(0.0, SpringParams::movement()),
            commit: false,
        };
        swipe.dy.jump_to(dy.max(0.0));
        self.overview.swipe = Some(swipe);
        sh.session.redraw();
    }

    /// The tile tracks the finger 1:1 — no spring while it is down — and
    /// never rises above where it stood.
    pub(super) fn overview_swipe_to(&mut self, sh: &mut Shell, dy: f64) {
        if let Some(swipe) = self.overview.swipe.as_mut() {
            swipe.dy.jump_to(dy.max(0.0));
            sh.session.redraw();
        }
    }

    /// The finger lifted: past the threshold the tile runs on off the
    /// bottom of the strip and the close follows it; short of it, back.
    pub(super) fn overview_swipe_release(&mut self, sh: &mut Shell) {
        let bottom = self.overview_vp(sh).pos.y + self.overview_vp(sh).size.y;
        let Some(swipe) = self.overview.swipe.as_mut() else {
            return;
        };
        if swipe.armed() {
            swipe.commit = true;
            swipe.dy.retarget(bottom - swipe.rect.pos.y + 8.0);
        } else {
            swipe.dy.retarget(0.0);
        }
        sh.session.redraw();
    }

    /// Whether a tile is still under a finger.
    fn swiping(&self) -> bool {
        matches!(self.touch.mode, Mode::TileSwipe { .. })
    }

    /// Back with a tile in hand: it springs back where it stood and
    /// overview stays up. A tile already sent off is past cancelling.
    /// Answers whether there was one to let go of.
    pub(super) fn cancel_tile_swipe(&mut self, cx: &mut Cx, sh: &mut Shell) -> bool {
        let Some(swipe) = self.overview.swipe.as_mut().filter(|s| !s.commit) else {
            return false;
        };
        swipe.dy.retarget(0.0);
        if self.swiping() {
            self.touch.mode = Mode::Dead;
        }
        self.next_frame = cx.new_next_frame();
        sh.session.redraw();
        true
    }

    /// A tile that has landed: gone off the strip, its panel closes now;
    /// sprung back, it is just a tile again. A committed swipe abandoned
    /// mid-flight — overview put away under it — closes at once rather than
    /// breaking what the lift promised.
    pub(super) fn settle_tile_swipe(&mut self, sh: &mut Shell) {
        let done = self.overview.swipe.as_ref().is_some_and(|s| {
            !self.swiping() && (s.dy.is_done() || sh.overlay != Overlay::Overview)
        });
        if done {
            self.abandon_tile_swipe(sh);
        }
    }

    /// Drops the swipe, wherever it is: a committed one closes its panel
    /// now, an uncommitted one is simply let go of.
    fn abandon_tile_swipe(&mut self, sh: &mut Shell) {
        let Some(swipe) = self.overview.swipe.take() else {
            return;
        };
        if swipe.commit {
            self.close_slot(sh, swipe.slot);
        }
        sh.session.redraw();
    }

    fn overview_workspace_rect(&self, vp: Rect, k: usize) -> Rect {
        rect(
            vp.pos.x + PAD + k as f64 * WORKSPACE_STEP - self.overview.workspace_scroll,
            vp.pos.y + WORKSPACE_TOP,
            WORKSPACE_W,
            WORKSPACE_H,
        )
    }

    fn workspace_at(&self, vp: Rect, p: DVec2) -> Option<usize> {
        if !vp.contains(p) {
            return None;
        }
        (0..WS_N).find(|&k| self.overview_workspace_rect(vp, k).contains(p))
    }

    fn clamp_overview(&mut self, sh: &Shell) {
        let vp = self.overview_vp(sh);
        let tiles = Tiles::new(vp, self.overview.scroll);
        let limit = tiles.limit(&sh.session.ws().wss[self.overview_ws(sh)]);
        self.overview.scroll.x = self.overview.scroll.x.clamp(0.0, limit.x);
        self.overview.scroll.y = self.overview.scroll.y.clamp(0.0, limit.y);
        let max = (PAD * 2.0 + WS_N as f64 * WORKSPACE_STEP - 12.0 - vp.size.x).max(0.0);
        self.overview.workspace_scroll = self.overview.workspace_scroll.clamp(0.0, max);
    }

    pub(super) fn overview_scroll(&mut self, sh: &mut Shell, start: DVec2, delta: DVec2) {
        if start.y - self.origin.y < BODY_TOP {
            self.overview.workspace_scroll += if delta.x.abs() > delta.y.abs() {
                delta.x
            } else {
                delta.y
            };
        } else {
            self.overview.scroll += delta;
        }
        self.clamp_overview(sh);
        sh.session.redraw();
    }

    /// Scrolls a strip the way a finger does, and answers whether the
    /// camera actually went anywhere. A coast asks: against the end of a
    /// strip the clamp eats every step, and a fling with nothing left to
    /// move is finished however much speed it has in hand.
    pub(super) fn overview_coast(&mut self, sh: &mut Shell, start: DVec2, delta: DVec2) -> bool {
        let before = (self.overview.scroll, self.overview.workspace_scroll);
        self.overview_scroll(sh, start, delta);
        (self.overview.scroll, self.overview.workspace_scroll) != before
    }

    pub(super) fn overview_pick(
        &mut self,
        sh: &mut Shell,
        uid: u64,
        slot: SlotId,
        p: DVec2,
        r: Rect,
    ) {
        self.overview.drag = Some(PanelDrag {
            slot,
            point: p,
            offset: r.pos - p,
            size: r.size,
            hover: None,
            target: None,
        });
        self.touch.mode = Mode::Drag { uid, slot };
        self.overview_drag_to(sh, p);
    }

    pub(super) fn overview_drag_to(&mut self, sh: &mut Shell, p: DVec2) {
        let vp = self.overview_vp(sh);
        let workspace = self.workspace_at(vp, p);
        let dest = self.overview_ws(sh);
        let Some(drag) = self.overview.drag.as_mut() else {
            return;
        };
        drag.point = p;
        if drag.hover.map(|(k, _)| k) != workspace {
            drag.hover = workspace.map(|k| (k, 0.0));
        }
        drag.target =
            Tiles::new(vp, self.overview.scroll).target(&sh.session.ws().wss[dest], drag.slot, p);
        sh.session.redraw();
    }

    pub(super) fn overview_drop(&mut self, sh: &mut Shell, p: DVec2) {
        self.overview_drag_to(sh, p);
        let dest = self.overview_ws(sh);
        let Some(drag) = self.overview.drag.take() else {
            return;
        };
        let top = drag
            .hover
            .is_some_and(|(k, elapsed)| k == dest && elapsed >= DWELL);
        let target = drag.target.map(|(target, _)| target);
        if target.is_none() && !top {
            return;
        }
        let slot = drag.slot;
        let label = format!(
            "move “{}” to workspace {}",
            self.title_of(sh, slot),
            dest + 1
        );
        sh.session.act(
            Action::new("move", label)
                .about(kernel::panel::slot_entity(slot))
                .moving(move |wm| {
                    if wm.focus_slot(slot).is_none() {
                        return;
                    }
                    wm.send_focused_to(dest);
                    if let Some(target) = target {
                        wm.place(slot, target);
                    }
                }),
        );
        sh.session.redraw();
    }

    pub(super) fn overview_tick(&mut self, sh: &mut Shell, dt: f64) -> bool {
        if sh.overlay != Overlay::Overview {
            self.cancel_overview_drag();
            self.settle_tile_swipe(sh);
            return false;
        }
        // The swiped tile's spring lives outside `Anim`, so it asks for its
        // own frames: under a finger it moves as the finger does, and
        // after a lift it springs.
        let mut moving = false;
        if self.swiping() {
            moving = true;
        } else if let Some(swipe) = self.overview.swipe.as_mut() {
            swipe.dy.advance(dt);
            moving |= !swipe.dy.is_done();
        }
        let Some(drag) = self.overview.drag.as_mut() else {
            return moving;
        };
        let p = drag.point;
        let destination = drag.hover.as_mut().and_then(|(k, elapsed)| {
            *elapsed += dt;
            (*elapsed >= DWELL).then_some(*k)
        });
        if let Some(k) = destination {
            if self.overview.workspace != Some(k) {
                self.overview.workspace = Some(k);
                self.overview.scroll = DVec2::default();
            }
        }
        // Both strips can be reached beyond the screen while holding a tile.
        let x = p.x - self.origin.x;
        let edge = if x < 40.0 {
            (x - 40.0) / 40.0
        } else if x > sh.viewport.x - 40.0 {
            (x - sh.viewport.x + 40.0) / 40.0
        } else {
            0.0
        };
        let mut delta = dvec2(edge.clamp(-1.0, 1.0) * 600.0 * dt, 0.0);
        if p.y - self.origin.y >= BODY_TOP {
            let y = p.y - self.origin.y;
            let edge_y = if y < BODY_TOP + 32.0 {
                (y - BODY_TOP - 32.0) / 32.0
            } else if y > sh.viewport.y - 40.0 {
                (y - sh.viewport.y + 40.0) / 40.0
            } else {
                0.0
            };
            delta.y = edge_y.clamp(-1.0, 1.0) * 400.0 * dt;
        }
        self.overview_scroll(sh, p, delta);
        self.overview_drag_to(sh, p);
        true
    }

    fn overview_tile(
        &mut self,
        cx: &mut Cx2d,
        r: Rect,
        title: &str,
        detail: &str,
        selected: bool,
        alpha: f64,
    ) {
        self.draw_panel.new_draw_call(cx);
        self.draw_panel.color = rgba_a(if selected { theme::SEL } else { theme::BG }, 1.0);
        self.draw_panel.border_color = rgba_a(if selected { theme::INK } else { theme::RULE }, 1.0);
        self.draw_panel.border_size = if selected { 2.0 } else { 1.0 };
        self.draw_panel.alpha = alpha as f32;
        self.draw_panel.draw_abs(cx, r);
        let columns = ((r.size.x - 24.0) / self.cell.adv).max(1.0) as usize;
        self.draw_mono.new_draw_call(cx);
        self.draw_mono.color = rgba_a(theme::INK, alpha);
        self.draw_mono
            .draw_abs(cx, r.pos + dvec2(12.0, 14.0), &trunc(title, columns));
        if !detail.is_empty() {
            self.draw_label(
                cx,
                r.pos.x + 12.0,
                r.pos.y + r.size.y - 24.0,
                &trunc(detail, columns),
                theme::MUTED,
                alpha,
            );
        }
    }

    pub(super) fn draw_overview(
        &mut self,
        cx: &mut Cx2d,
        sh: &mut Shell,
        vp: Rect,
        alpha: f64,
        live: bool,
    ) {
        self.clamp_overview(sh);
        if live {
            self.hits.clear();
        }
        self.draw_flat.new_draw_call(cx);
        self.draw_flat.color = rgba_a(theme::HOVER, alpha);
        self.draw_flat.draw_abs(cx, vp);
        if live {
            self.hits
                .push(Hit::act("overview", vp, MouseCursor::Default, Act::Noop));
        }
        self.draw_label(
            cx,
            vp.pos.x + PAD,
            vp.pos.y + 20.0,
            "overview",
            theme::INK,
            alpha,
        );
        let close = rect(vp.pos.x + vp.size.x - 76.0, vp.pos.y + 4.0, 60.0, 44.0);
        self.draw_label(
            cx,
            close.pos.x + 12.0,
            close.pos.y + 16.0,
            "done",
            theme::INK,
            alpha,
        );
        if live {
            self.hits.push(Hit::act(
                "done",
                close,
                MouseCursor::Hand,
                Act::OverlayClose,
            ));
        }
        let dest = self.overview_ws(sh);
        let strip = rect(vp.pos.x, vp.pos.y + WORKSPACE_TOP, vp.size.x, WORKSPACE_H);
        cx.begin_turtle(
            Walk::abs_rect(strip),
            Layout {
                clip_x: true,
                clip_y: true,
                ..Default::default()
            },
        );
        for k in 0..WS_N {
            let r = self.overview_workspace_rect(vp, k);
            let Some(hit) = clipped(r, strip) else {
                continue;
            };
            let count = sh.session.ws().wss[k].slots.len();
            let title = format!("workspace {}", k + 1);
            let detail = if count == 0 {
                "empty".to_string()
            } else {
                format!("{count} panels")
            };
            self.overview_tile(
                cx,
                r,
                &format!("space {}", k + 1),
                &detail,
                k == dest,
                alpha,
            );
            if let Some((hover, elapsed)) = self.overview.drag.as_ref().and_then(|d| d.hover) {
                if hover == k {
                    self.draw_flat.new_draw_call(cx);
                    self.draw_flat.color = rgba_a(theme::INK, alpha);
                    self.draw_flat.draw_abs(
                        cx,
                        rect(
                            r.pos.x,
                            r.pos.y + r.size.y - 4.0,
                            r.size.x * (elapsed / DWELL).min(1.0),
                            4.0,
                        ),
                    );
                }
            }
            if live {
                self.hits.push(Hit::act(
                    title,
                    hit,
                    MouseCursor::Hand,
                    Act::OverviewWorkspace(k),
                ));
            }
        }
        cx.end_turtle();
        let tiles = Tiles::new(vp, self.overview.scroll);
        cx.begin_turtle(
            Walk::abs_rect(tiles.body),
            Layout {
                clip_x: true,
                clip_y: true,
                ..Default::default()
            },
        );
        let ws = &sh.session.ws().wss[dest];
        // Every tile where it stands this frame, and how far it has faded:
        // what the bridges are drawn between.
        let mut drawn: Vec<(SlotId, Rect, f64)> = Vec::new();
        for (col, column) in ws.columns.iter().enumerate() {
            let first = tiles.tile(col, 0);
            let caption = format!(
                "column {}{}",
                col + 1,
                if column.tabbed { " · tabs" } else { "" }
            );
            self.draw_label(
                cx,
                first.pos.x,
                first.pos.y - 20.0,
                &caption,
                theme::MUTED,
                alpha,
            );
            for (row, &slot) in column.slots.iter().enumerate() {
                let mut r = tiles.tile(col, row);
                // A tile being pulled down rides its finger and fades as it
                // goes, so the hand can see what a lift would do.
                let swipe = self.overview.swipe.as_ref().filter(|s| s.slot == slot);
                let (dy, armed) = swipe.map_or((0.0, false), |s| (s.dy.value(), s.armed()));
                r.pos.y += dy;
                let moving = self.overview.drag.as_ref().is_some_and(|d| d.slot == slot);
                let fade = 1.0 - (dy / (TILE_H * 1.5)).clamp(0.0, 0.85);
                let dim = if moving { 0.35 } else { fade };
                drawn.push((slot, r, dim));
                let Some(hit) = clipped(r, tiles.body) else {
                    continue;
                };
                let title = self.title_of(sh, slot);
                let detail = if moving {
                    "moving"
                } else if armed {
                    "release to close"
                } else if ws.focus == Some(slot) {
                    "focused"
                } else {
                    ""
                };
                self.overview_tile(cx, r, &title, detail, ws.focus == Some(slot), alpha * dim);
                if live {
                    let mut hit = Hit::act(title, hit, MouseCursor::Hand, Act::OverviewPanel(slot));
                    hit.unclipped = Some(r);
                    self.hits.push(hit);
                }
            }
        }
        // A live join shows as it does on the stage: the ═ spans the gap
        // between the parent's tile and its child's, rides with a tile on
        // its way down, and dims with one in hand.
        let title_mid = 14.0 + self.cell.natural / 2.0;
        let mut joins: Vec<(SlotId, SlotId)> = ws.joins.iter().map(|(&p, &c)| (p, c)).collect();
        joins.sort_unstable();
        self.draw_flat.new_draw_call(cx);
        for (parent, child) in joins {
            let find = |slot| {
                drawn
                    .iter()
                    .find(|(s, _, _)| *s == slot)
                    .map(|&(_, r, dim)| (r, dim))
            };
            let (Some((ra, da)), Some((rb, db))) = (find(parent), find(child)) else {
                continue;
            };
            let Some(bar) = bridge(ra, rb, title_mid) else {
                continue;
            };
            let Some(hit) = clipped(bar, tiles.body) else {
                continue;
            };
            self.draw_flat.color = rgba_a(theme::INK, alpha * da.min(db));
            self.draw_flat
                .draw_abs(cx, rect(bar.pos.x, bar.pos.y, bar.size.x, 1.0));
            self.draw_flat
                .draw_abs(cx, rect(bar.pos.x, bar.pos.y + 3.0, bar.size.x, 1.0));
            if live {
                self.hits
                    .push(Hit::act("joined", hit, MouseCursor::Default, Act::Noop));
            }
        }
        if ws.is_empty() {
            self.draw_label(
                cx,
                vp.pos.x + PAD,
                tiles.body.pos.y + 32.0,
                "empty workspace",
                theme::MUTED,
                alpha,
            );
        }
        if let Some((target, bar)) = self.overview.drag.as_ref().and_then(|d| d.target) {
            if live {
                let label = match target {
                    DropTarget::Into { col, row } => {
                        format!("place in column {} row {}", col + 1, row + 1)
                    }
                    DropTarget::Boundary { at } => format!("place in new column {}", at + 1),
                };
                if let Some(r) = clipped(bar, tiles.body) {
                    self.hits
                        .push(Hit::act(label, r, MouseCursor::Default, Act::Noop));
                }
            }
        }
        cx.end_turtle();
        if let Some(drag) = self.overview.drag.as_ref() {
            let r = Rect {
                pos: drag.point + drag.offset,
                size: drag.size,
            };
            let title = self.title_of(sh, drag.slot);
            self.overview_tile(cx, r, &title, "release to place", true, alpha * 0.9);
        }
        // Keep the promised insertion visible above the floating tile.
        if let Some(bar) = self
            .overview
            .drag
            .as_ref()
            .and_then(|d| d.target)
            .and_then(|(_, bar)| clipped(bar, tiles.body))
        {
            self.draw_flat.new_draw_call(cx);
            self.draw_flat.color = rgba_a(theme::INK, alpha);
            self.draw_flat.draw_abs(cx, bar);
        }
    }
}
