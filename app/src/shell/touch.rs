//! Touch: what the fingers on the glass add up to.
//!
//! One state machine, locked at the first move past the slop and held until
//! every finger lifts, so nothing flips mode mid-gesture:
//!
//! ```text
//! one finger   tap          → a click where it went down
//!              ↕ vertical   → the panel under it scrolls, 1:1
//!              ↔ on a row   → the curtain, and a verb past a third of it
//!              long press   → a row marks; a header picks the panel up
//! two fingers  ↔ horizontal → the strip pans, 1:1, and aligns on release
//!              ↕ vertical   → the workspaces overlay, down open, up closed
//! ```
//!
//! What a finger lands on comes from the same hit table a click resolves
//! through, so a gesture and a click can never disagree about what is there.
//! A row is the one thing the shell has to recognise
//! ([`Act::Row`](super::hits::Act::Row)); which row it is, and what a sweep
//! across it runs, stay with the widget that drew it
//! ([`Grab`](super::hosted::Grab)).
//!
//! There is no touch equivalent of `cmd+click`: a link on glass always
//! follows the join rule.

use std::collections::HashMap;

use kernel::layout::SlotId;
use kernel::session::Action;
use kernel::spring::{Spring, SpringParams};
use kernel::theme;
use makepad_widgets::makepad_platform::event::{
    ScrollEvent, ScrollPhase, TouchState, TouchUpdateEvent,
};
use makepad_widgets::*;

use super::draw::{rect, rgba_a};
use super::hits::{Act, Hit};
use super::hosted::{verb_word, Ask};
use super::overlays::Overlay;
use super::stage::{Shell, Stage};

/// How far a finger may wander and still be a tap, in points. The same
/// distance locks a gesture's axis.
pub const TOUCH_SLOP: f64 = 8.0;

/// How far across a row the curtain must be drawn for a lift to run its
/// verb: a third of the row.
pub const SWIPE_COMMIT: f64 = 1.0 / 3.0;

/// How fast the strip pans while a dragged panel is held against an edge,
/// in points per second.
const EDGE_PAN: f64 = 1000.0;

/// How wide the edge band that pans is, in points.
const EDGE_BAND: f64 = 60.0;

/// What the fingers are doing.
#[derive(Debug, Clone, Default)]
pub enum Mode {
    #[default]
    Idle,
    /// A finger down, undecided: a click on lift inside the slop.
    Tap { uid: u64, hit: Option<Hit> },
    /// One finger scrolling a panel's body, 1:1.
    Scroll { uid: u64 },
    /// Two fingers down. The first move past the slop locks the axis:
    /// horizontal pans the strip; a vertical swipe raises or dismisses the
    /// workspaces overlay and goes dead.
    Pan { horizontal: Option<bool> },
    /// A long-pressed header: the panel rides the finger, and the drop point
    /// picks its new place.
    Drag {
        uid: u64,
        slot: SlotId,
        offset: DVec2,
    },
    /// A sideways finger on a row: the curtain, whose physics live in
    /// [`RowSwipe`] on the stage, since a committed sweep keeps running
    /// after the finger is gone.
    Row { uid: u64 },
    /// A gesture that came to nothing: inert until every finger lifts.
    Dead,
}

/// Live touches and the gesture they add up to.
#[derive(Debug, Default)]
pub struct TouchNav {
    /// uid → (where it went down, where it is).
    pub pts: HashMap<u64, (DVec2, DVec2)>,
    pub mode: Mode,
}

/// A swept row and the curtain wiping across it.
///
/// The row itself never moves: an ink panel carrying the verb's word is
/// drawn in from the edge the finger travels *away* from. Below the commit
/// threshold it is a grey wash with ink lettering; past it the whole thing
/// inverts, the way a control under the pointer does, so it needs no colour.
///
/// It lives on the stage rather than in [`Mode`] because a committed sweep
/// outlives its finger: the curtain finishes covering the row, and only then
/// does the verb run.
#[derive(Debug)]
pub struct RowSwipe {
    /// The panel the row belongs to.
    pub slot: SlotId,
    /// Where the finger went down: what the widget resolves the row by.
    pub at: DVec2,
    /// The row's rectangle as last drawn — kept, so the curtain still has
    /// somewhere to be after the row has left the list.
    pub rect: Rect,
    /// The verb a leftward sweep runs and the verb a rightward one runs, by
    /// id. `None` where that way means nothing on this row.
    pub verbs: [Option<&'static str>; 2],
    /// How far the curtain is drawn, signed: negative wipes in from the
    /// right, positive from the left.
    pub x: Spring,
    /// Set on a committing lift: `true` swept left. The verb runs when the
    /// spring lands.
    pub commit: Option<bool>,
}

impl RowSwipe {
    /// The verb the curtain is promising, by id: `None` where the finger has
    /// gone nowhere, or has gone a way this row has no verb for.
    #[must_use]
    pub fn verb(&self) -> Option<&'static str> {
        match self.x.value() {
            x if x < 0.0 => self.verbs[0],
            x if x > 0.0 => self.verbs[1],
            _ => None,
        }
    }

    /// Whether the curtain is far enough across to run on lift.
    #[must_use]
    pub fn armed(&self) -> bool {
        self.verb().is_some()
            && self.rect.size.x > 0.0
            && self.x.value().abs() >= self.rect.size.x * SWIPE_COMMIT
    }
}

impl Stage {
    /// Every finger of one platform update.
    pub(super) fn touch_update(&mut self, cx: &mut Cx, sh: &mut Shell, e: &TouchUpdateEvent) {
        for t in &e.touches {
            match t.state {
                TouchState::Start => self.touch_start(t.uid, t.abs),
                TouchState::Move => self.touch_move(cx, sh, t.uid, t.abs),
                TouchState::Stop => self.touch_stop(cx, sh, t.uid, t.abs),
                TouchState::Stable => {}
            }
        }
    }

    /// A finger went down.
    pub(super) fn touch_start(&mut self, uid: u64, p: DVec2) {
        self.touch.pts.insert(uid, (p, p));
        match self.touch.mode {
            // A drag keeps its panel and a swept row keeps its curtain
            // whatever else lands: a second finger must not strand one
            // half-drawn with nothing left to settle it.
            Mode::Drag { .. } | Mode::Row { .. } => {}
            _ if self.touch.pts.len() >= 2 => {
                self.touch.mode = Mode::Pan { horizontal: None };
            }
            _ => {
                let hit = self.hits.at(p);
                self.touch.mode = Mode::Tap { uid, hit };
            }
        }
    }

    /// A finger moved. The first move past the slop decides what the gesture
    /// is; everything after it belongs to that mode alone.
    pub(super) fn touch_move(&mut self, cx: &mut Cx, sh: &mut Shell, uid: u64, p: DVec2) {
        let Some(&(start, last)) = self.touch.pts.get(&uid) else {
            return;
        };
        let d = p - last;
        self.touch.pts.insert(uid, (start, p));
        match self.touch.mode.clone() {
            Mode::Tap { uid: u, hit } if u == uid => {
                let t = p - start;
                if t.x.abs() < TOUCH_SLOP && t.y.abs() < TOUCH_SLOP {
                    return;
                }
                self.touch.mode = self.decide(cx, sh, uid, start, t, hit.as_ref());
                self.wake(cx, sh);
            }

            // The curtain tracks the finger 1:1 — no spring while it is
            // down, or the ink would lag the thumb.
            Mode::Row { uid: u } if u == uid => {
                if let Some(rs) = self.row_swipe.as_mut() {
                    rs.x.jump_to(p.x - start.x);
                }
                self.wake(cx, sh);
            }

            // Retained content scrolls itself: the drag becomes a scroll for
            // the widget under the finger, so its own list and scrollbars
            // clamp it.
            Mode::Scroll { uid: u } if u == uid => {
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
                self.forward_to_hosted(cx, sh, &ev);
                self.wake(cx, sh);
            }

            Mode::Pan { horizontal } => {
                if horizontal.is_none() {
                    let t = p - start;
                    if t.x.abs() < TOUCH_SLOP && t.y.abs() < TOUCH_SLOP {
                        return;
                    }
                    if t.x.abs() < t.y.abs() {
                        // A vertical two-finger swipe: down lists the
                        // workspaces, up puts whatever is up away. One shot
                        // — the rest of the gesture is inert.
                        sh.overlay = if t.y > 0.0 {
                            Overlay::Ws
                        } else {
                            Overlay::None
                        };
                        self.touch.mode = Mode::Dead;
                        sh.session.redraw();
                        self.wake(cx, sh);
                        return;
                    }
                    self.touch.mode = Mode::Pan {
                        horizontal: Some(true),
                    };
                }
                // Each finger reports its own move; dividing by the count is
                // what makes the strip track the gesture 1:1.
                let n = self.touch.pts.len().max(1) as f64;
                sh.session.pan(-d.x / n);
                let cam = sh.session.scene().camera_x;
                sh.anim.camera().jump_to(cam);
                self.wake(cx, sh);
            }

            Mode::Drag {
                uid: u,
                slot,
                offset,
            } if u == uid => {
                self.drag_to(sh, slot, offset, p);
                self.wake(cx, sh);
            }

            _ => {}
        }
    }

    /// What a one-finger move past the slop turns into.
    fn decide(
        &mut self,
        cx: &mut Cx,
        sh: &mut Shell,
        uid: u64,
        start: DVec2,
        t: DVec2,
        hit: Option<&Hit>,
    ) -> Mode {
        // Vertical keeps ties: a diagonal is a scroll, never half a sweep.
        let sideways = t.x.abs() > t.y.abs();
        let row = hit.filter(|h| matches!(h.act, Act::Row(_)));
        if let (true, Some(h)) = (sideways, row) {
            let Some(slot) = h.slot else {
                return Mode::Dead;
            };
            let verbs = self.ask_grab(cx, sh, slot, Ask::Verbs(start));
            // A row with nothing to run draws no curtain: the finger slides
            // and nothing is promised.
            if verbs.iter().all(Option::is_none) {
                return Mode::Dead;
            }
            self.row_swipe = Some(RowSwipe {
                slot,
                at: start,
                rect: h.rect,
                verbs,
                x: Spring::at_rest(0.0, SpringParams::movement()),
                commit: None,
            });
            return Mode::Row { uid };
        }
        // Anything else vertical scrolls the panel it is over. Sideways with
        // one finger means nothing: the strip pans on two.
        match (sideways, hit.and_then(|h| h.slot)) {
            (false, Some(_)) => Mode::Scroll { uid },
            _ => Mode::Dead,
        }
    }

    /// A finger lifted.
    pub(super) fn touch_stop(&mut self, cx: &mut Cx, sh: &mut Shell, uid: u64, p: DVec2) {
        let start = self.touch.pts.remove(&uid).map(|(s, _)| s);
        match self.touch.mode.clone() {
            Mode::Tap { uid: u, hit } if u == uid => {
                self.touch.mode = Mode::Idle;
                let within = start.is_some_and(|s| {
                    (p.x - s.x).abs() < TOUCH_SLOP && (p.y - s.y).abs() < TOUCH_SLOP
                });
                if let (true, Some(hit)) = (within, hit) {
                    // No modifiers on glass, so never the fresh variant. A
                    // widget's own element answers the press itself, exactly
                    // as it does under a mouse.
                    match hit.act {
                        Act::Widget | Act::Row(_) => self.synth_click(cx, sh, p, false),
                        act => self.resolve(cx, sh, act, false),
                    }
                }
            }

            Mode::Scroll { uid: u } if u == uid => self.touch.mode = Mode::Idle,

            Mode::Row { uid: u } if u == uid => {
                self.touch.mode = Mode::Idle;
                if let Some(rs) = self.row_swipe.as_mut() {
                    if rs.armed() {
                        // Committed: the curtain runs on to cover the row,
                        // and the verb fires when it lands — so the row is
                        // gone from view before it is gone from the list.
                        let w = rs.rect.size.x;
                        let left = rs.x.value() < 0.0;
                        rs.commit = Some(left);
                        rs.x.retarget(if left { -w } else { w });
                    } else {
                        rs.x.retarget(0.0);
                    }
                }
                self.wake(cx, sh);
            }

            Mode::Drag { uid: u, slot, .. } if u == uid => {
                self.touch.mode = Mode::Idle;
                self.drag_hint = None;
                let local = p - self.origin;
                let cam = sh.anim.camera().value();
                let (vp, opts) = (sh.session.viewport(), sh.session.opts());
                let label = format!("move “{}”", self.title_of(sh, slot));
                sh.session.act(
                    Action::new("move", label)
                        .about(kernel::panel::slot_entity(slot))
                        .moving(move |wm| {
                            wm.place_at(slot, local.x + cam, local.y, vp, opts);
                        }),
                );
                self.wake(cx, sh);
            }

            // A bystander finger lifted mid-drag.
            Mode::Drag { .. } => {}

            Mode::Pan { horizontal } => {
                // The pan ends with the first lifted finger; a leftover one
                // is inert. The camera then magnetises to the nearest column
                // alignment — a spring, so it settles rather than jumps.
                if !self.touch.pts.is_empty() {
                    self.touch.mode = Mode::Dead;
                }
                if horizontal == Some(true) {
                    sh.session.snap_camera();
                    let cam = sh.session.scene().camera_x;
                    sh.anim.camera().retarget(cam);
                }
                self.wake(cx, sh);
            }

            _ => {
                if !self.touch.pts.is_empty() {
                    self.touch.mode = Mode::Dead;
                }
            }
        }
        if self.touch.pts.is_empty() {
            self.touch.mode = Mode::Idle;
        }
    }

    /// The platform's long press (android's own detector; a script's
    /// `holdmove` on the desktop): a row marks, a header picks its panel up.
    pub(super) fn long_press(&mut self, cx: &mut Cx, sh: &mut Shell, uid: u64, p: DVec2) {
        match self.touch.mode {
            Mode::Tap { uid: u, .. } if u == uid => {}
            Mode::Idle => {}
            _ => return,
        }
        let hit = self.hits.at(p);
        // A row marks. The pointer has no way in — space and shift are the
        // keyboard's — so this is the phone's.
        if let Some(slot) = hit.as_ref().and_then(|h| match h.act {
            Act::Row(slot) => Some(slot),
            _ => None,
        }) {
            self.ask_grab(cx, sh, slot, Ask::Mark(p));
            self.touch.mode = Mode::Idle;
            sh.session.redraw();
            self.wake(cx, sh);
            return;
        }
        let Some(slot) = hit.and_then(|h| h.act.slot()) else {
            return;
        };
        // Only the header grabs: the body below it belongs to the panel.
        let Some(head) = self.hits.by_act(&Act::Focus(slot)).map(|h| h.rect) else {
            return;
        };
        if p.y > head.pos.y + theme::HEAD_H {
            return;
        }
        let grab = p - self.origin;
        let cam = sh.anim.camera().value();
        let corner = sh
            .anim
            .panels
            .get(&slot)
            .map(|pa| dvec2(pa.rect().x - cam, pa.rect().y))
            .unwrap_or(grab);
        sh.session.nav(kernel::nav::Nav::Focus(slot));
        self.touch.mode = Mode::Drag {
            uid,
            slot,
            offset: corner - grab,
        };
        self.wake(cx, sh);
    }

    /// The dragged panel follows the finger, and the insertion bar previews
    /// where a drop would put it — judged by the finger, not by the panel.
    fn drag_to(&mut self, sh: &mut Shell, slot: SlotId, offset: DVec2, p: DVec2) {
        let local = p - self.origin;
        let cam = sh.anim.camera().value();
        if let Some(pa) = sh.anim.panels.get_mut(&slot) {
            pa.retarget_pos(local.x + offset.x + cam, local.y + offset.y);
        }
        let (vp, opts) = (sh.session.viewport(), sh.session.opts());
        self.drag_hint = sh
            .session
            .ws()
            .drop_target(slot, local.x + cam, local.y, vp, opts)
            .map(|(_, bar)| bar);
    }

    /// One frame of whatever touch has left running: a held panel against an
    /// edge pans the strip, and a curtain springs. Answers whether either
    /// still wants frames.
    pub(super) fn touch_tick(&mut self, sh: &mut Shell, dt: f64) -> bool {
        let mut moving = false;
        if let Mode::Drag { uid, slot, offset } = self.touch.mode {
            moving = true;
            let p = self.touch.pts.get(&uid).map_or(self.origin, |&(_, p)| p);
            let vp = sh.session.viewport();
            let x = p.x - self.origin.x;
            let f = if x < EDGE_BAND {
                (x - EDGE_BAND) / EDGE_BAND
            } else if x > vp.0 - EDGE_BAND {
                (x - (vp.0 - EDGE_BAND)) / EDGE_BAND
            } else {
                0.0
            };
            if f != 0.0 {
                sh.session.pan(f.clamp(-1.0, 1.0) * EDGE_PAN * dt);
                let cam = sh.session.scene().camera_x;
                sh.anim.camera().jump_to(cam);
                self.drag_to(sh, slot, offset, p);
            }
        }
        // The curtain's spring lives outside `Anim`, so it asks for its own
        // frames or it would freeze after one.
        if matches!(self.touch.mode, Mode::Row { .. }) {
            moving = true;
        } else if let Some(rs) = self.row_swipe.as_mut() {
            rs.x.advance(dt);
            moving |= !rs.x.is_done();
        }
        moving
    }

    /// A settled curtain: a committed one runs its verb, a cancelled one
    /// just clears. Called once the spring has landed, so the row is covered
    /// before it leaves the list rather than blinking out from under the
    /// finger.
    pub(super) fn settle_row_swipe(&mut self, cx: &mut Cx, sh: &mut Shell) {
        let done = self
            .row_swipe
            .as_ref()
            .is_some_and(|rs| rs.x.is_done() && !matches!(self.touch.mode, Mode::Row { .. }));
        if !done {
            return;
        }
        let Some(rs) = self.row_swipe.take() else {
            return;
        };
        if let Some(left) = rs.commit {
            self.ask_grab(cx, sh, rs.slot, Ask::Run { at: rs.at, left });
        }
        sh.session.redraw();
    }

    /// The curtain over a swept row, drawn inside the panel's own clipped
    /// turtle.
    ///
    /// It enters from the edge the finger travels *away* from. Below the
    /// commit threshold it is the selection grey with ink lettering and an
    /// ink hairline at its leading edge — without which a curtain over the
    /// row the cursor is standing on would be the same grey as the cursor's
    /// own wash. Past the threshold the whole thing inverts.
    pub(super) fn draw_row_curtain(
        &mut self,
        cx: &mut Cx2d,
        slot: SlotId,
        w: &WidgetRef,
        body: Rect,
    ) {
        let Some(rs) = self.row_swipe.as_ref().filter(|rs| rs.slot == slot) else {
            return;
        };
        let (dx, armed) = (rs.x.value(), rs.armed());
        let Some(verb) = rs.verb() else {
            return;
        };
        let row = rs.rect;
        if row.size.x <= 0.0 || dx.abs() < 0.5 {
            return;
        }
        // Clip to the rows: one scrolled half under the filter has a
        // rectangle that reaches above the list, and the curtain must not.
        let list = w.widget(cx, super::widgets::table::LIST).area().rect(cx);
        let clip = if list.size.y > 0.0 { list } else { body };
        let (top, bot) = (
            row.pos.y.max(clip.pos.y),
            (row.pos.y + row.size.y).min(clip.pos.y + clip.size.y),
        );
        if bot <= top {
            return;
        }
        let width = dx.abs().min(row.size.x);
        // A leftward sweep enters from the right.
        let x = if dx < 0.0 {
            row.pos.x + row.size.x - width
        } else {
            row.pos.x
        };
        let (bg, fg) = if armed {
            (theme::INK, theme::BG)
        } else {
            (theme::SEL, theme::INK)
        };
        // A draw call of its own, or these quads merge into the chrome's and
        // paint under the panel they belong to.
        self.draw_flat.new_draw_call(cx);
        self.draw_flat.color = rgba_a(bg, 1.0);
        self.draw_flat.draw_abs(cx, rect(x, top, width, bot - top));
        if !armed {
            let ex = if dx < 0.0 { x } else { x + width - 1.0 };
            self.draw_flat.color = rgba_a(theme::INK, 1.0);
            self.draw_flat.draw_abs(cx, rect(ex, top, 1.0, bot - top));
        }
        // The word is pinned to the entering edge, so it holds still while
        // the curtain grows past it — and stands down until there is room:
        // half a word reads as a glitch, not as a hint.
        const PAD: f64 = 10.0;
        let word = verb_word(verb);
        let tw = self.cell.label_step() * word.chars().count() as f64;
        if width >= tw + 2.0 * PAD {
            let tx = if dx < 0.0 {
                x + width - tw - PAD
            } else {
                x + PAD
            };
            let ty = row.pos.y + (row.size.y - self.cell.label_line()) / 2.0;
            self.draw_mono.new_draw_call(cx);
            self.draw_label(cx, tx, ty, &word, fg, 1.0);
        }
    }

    /// Keeps the swept row's rectangle current: the rows are rebuilt every
    /// draw, and the curtain is drawn against the newest one. What the row
    /// leaves behind when it is filed away is the last it had.
    pub(super) fn track_row_rect(&mut self) {
        let Some(rs) = self.row_swipe.as_mut() else {
            return;
        };
        if let Some(h) = self.hits.at(rs.at) {
            if h.slot == Some(rs.slot) && matches!(h.act, Act::Row(_)) {
                rs.rect = h.rect;
            }
        }
    }

    /// A gesture moved something: ask for the next frame and redraw.
    fn wake(&mut self, cx: &mut Cx, sh: &mut Shell) {
        sh.session.redraw();
        self.next_frame = cx.new_next_frame();
        self.redraw_scoped(cx);
    }

    /// A panel's title, as an action labels it.
    fn title_of(&self, sh: &Shell, slot: SlotId) -> String {
        sh.session
            .panel(slot)
            .map(|p| p.borrow().title())
            .unwrap_or_else(|| "panel".into())
    }
}
