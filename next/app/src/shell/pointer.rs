//! The pointer: what is under it, and what a press on it means.
//!
//! Every rectangle the frame drew is in the hit table; a press resolves to
//! the last one registered that contains the point, so a control drawn over
//! another takes the click. Hits a hosted widget registered are its own to
//! answer — the shell only routes the pointer there.

use kernel::nav::Nav;
use makepad_widgets::*;

use super::hits::Act;
use super::overlays::Overlay;
use super::stage::{Shell, Stage};

impl Stage {
    /// What the pointer is over: the cursor shape, and the hover the chrome
    /// draws itself with.
    pub(super) fn handle_mouse_move(&mut self, cx: &mut Cx, sh: &mut Shell, p: DVec2) {
        let hit = self.hits.at(p);
        cx.set_cursor(hit.as_ref().map_or(MouseCursor::Default, |h| h.cursor));
        // A panel's own rectangle is not a control: hovering it highlights
        // nothing.
        let hover = hit
            .map(|h| h.act)
            .filter(|a| !matches!(a, Act::Focus(_) | Act::Widget));
        if hover != sh.hover {
            sh.hover = hover;
            sh.session.redraw();
        }
    }

    /// A press: the shell's own chrome resolves here, a hosted widget's
    /// element has already answered for itself.
    pub(super) fn handle_mouse_down(&mut self, cx: &mut Cx, sh: &mut Shell, e: &MouseDownEvent) {
        self.cmd_tap.other_input();
        let Some(hit) = self.hits.at(e.abs) else {
            return;
        };
        // A hosted field under the click already took key focus through the
        // forwarded event — stealing it back would kill the caret.
        if hit.act != Act::Widget {
            cx.set_key_focus(self.area);
        }
        // cmd (alt as a quiet alias): always a fresh, un-joined panel.
        let fresh = e.modifiers.logo || e.modifiers.alt;
        self.resolve(cx, sh, hit.act, fresh);
    }

    /// Runs what a hit means.
    pub(super) fn resolve(&mut self, cx: &mut Cx, sh: &mut Shell, act: Act, fresh: bool) {
        let _ = fresh;
        match act {
            Act::Widget => {}
            Act::Focus(slot) | Act::Tab(slot) => sh.session.nav(Nav::Focus(slot)),
            Act::Close(slot) => sh.session.nav(Nav::Close(slot)),
            Act::Verb(slot, id) => {
                // A click on a bar takes the panel with it: the chords that
                // bar answers to are the focused panel's from here on.
                sh.session.nav(Nav::Focus(slot));
                self.run_verb(sh, slot, id);
            }
            Act::WsRow(k) => self.switch_ws(sh, k),
            Act::LauncherOpen => self.open_launcher(cx, sh),
            Act::LauncherRow(i) => {
                let go = sh.launcher.hits().get(i).map(|h| h.go.clone());
                if let Some(go) = go {
                    self.launcher_go(sh, go);
                }
            }
            Act::HistoryRow(node) => self.travel(sh, node),
            Act::OverlayClose => {
                sh.overlay = Overlay::None;
                sh.session.redraw();
            }
        }
    }

    /// A press and a release at one point, down the same road a physical
    /// click takes: the hosted widgets first, then the hit table, then
    /// whatever the shell owes the screen after it. What a script's `click`
    /// synthesizes.
    ///
    /// It calls the stage's inner handler rather than `Widget::handle_event`
    /// — the outer one has the shell borrowed out of `self` already, and a
    /// re-entrant call would find nothing there.
    pub(super) fn synth_click(&mut self, cx: &mut Cx, sh: &mut Shell, p: DVec2, cmd: bool) {
        for ev in press_release(p, cmd) {
            self.handle_with(cx, sh, &ev);
            self.settle(cx, sh);
        }
    }

    /// A press-drag-release, the gesture text selection is made of.
    pub(super) fn synth_drag(&mut self, cx: &mut Cx, sh: &mut Shell, from: DVec2, to: DVec2) {
        let down = Event::MouseDown(MouseDownEvent {
            abs: from,
            button: MouseButton::PRIMARY,
            window_id: CxWindowPool::id_zero(),
            modifiers: KeyModifiers::default(),
            handled: std::cell::Cell::new(Area::Empty),
            time: 0.0,
        });
        self.forward_to_hosted(cx, sh, &down);
        for i in 1..=8 {
            let f = f64::from(i) / 8.0;
            let mv = Event::MouseMove(MouseMoveEvent {
                abs: from + (to - from) * f,
                window_id: CxWindowPool::id_zero(),
                modifiers: KeyModifiers::default(),
                time: f * 0.1,
                handled: std::cell::Cell::new(Area::Empty),
                lock_delta: DVec2::default(),
            });
            self.forward_to_hosted(cx, sh, &mv);
        }
        let up = Event::MouseUp(MouseUpEvent {
            abs: to,
            button: MouseButton::PRIMARY,
            window_id: CxWindowPool::id_zero(),
            modifiers: KeyModifiers::default(),
            time: 0.2,
        });
        self.forward_to_hosted(cx, sh, &up);
    }
}

/// One press and its release at a point: what a scripted `click`
/// synthesizes, here so the panels library can send the same pair into a
/// mount it has entered.
#[must_use]
pub(super) fn press_release(p: DVec2, cmd: bool) -> [Event; 2] {
    let modifiers = KeyModifiers {
        logo: cmd,
        ..Default::default()
    };
    [
        Event::MouseDown(MouseDownEvent {
            abs: p,
            button: MouseButton::PRIMARY,
            window_id: CxWindowPool::id_zero(),
            modifiers,
            handled: std::cell::Cell::new(Area::Empty),
            time: 0.0,
        }),
        Event::MouseUp(MouseUpEvent {
            abs: p,
            button: MouseButton::PRIMARY,
            window_id: CxWindowPool::id_zero(),
            modifiers,
            time: 0.1,
        }),
    ]
}
