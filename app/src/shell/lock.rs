//! The locked screen: what a device that may not write shows instead.
//!
//! When a bucket is configured and this device does not hold the lease, a
//! full-window modal owns every hit and offers to take it. It is not an
//! overlay: an overlay is something the person raised and can dismiss, and
//! this one is a fact about the device — it goes when the lease turns over
//! and not before.
//!
//! Drawn under the toast, so an *acquiring…* line still shows.

use kernel::repl::Role;
use kernel::theme;
use makepad_widgets::*;

use super::draw::{rect, rgba_a, Style};
use super::hits::{Act, Hit};
use super::stage::{Shell, Stage};

/// How wide the card is, and how much of the viewport it will take.
const CARD_W: f64 = 460.0;

/// The card's height before any optional line is added.
const CARD_H: f64 = 156.0;

impl Stage {
    /// Draws the locked screen, if this device is locked. Answers whether it
    /// did — the caller stops drawing anything a click could reach.
    pub(super) fn draw_lock(&mut self, cx: &mut Cx2d, sh: &mut Shell, vp: Rect) -> bool {
        if sh.session.writable() {
            return false;
        }
        let (role, note, device) = match sh.session.lease() {
            Some(lease) => (
                // Admission closes before native retirement and the final
                // status arrive. Do not offer Acquire from a stale Holder.
                if lease.role.writable() { Role::Syncing } else { lease.role.clone() },
                lease.note.clone(), lease.device.clone(),
            ),
            None => (Role::Fault, Some("writer services are stopped".into()), sh.session.store().device()),
        };

        // The wash owns every hit: nothing under it is reachable, by a
        // pointer or by a script.
        self.hits.clear();
        self.draw_flat.new_draw_call(cx);
        self.draw_flat.color = rgba_a(theme::INK, 0.72);
        self.draw_flat.draw_abs(cx, vp);
        self.hits
            .push(Hit::act("locked", vp, MouseCursor::Default, Act::Noop));

        let (title, btn) = role.locked_screen();
        let cw = CARD_W.min(vp.size.x - 40.0);
        // One more line when the pass had a reason to give.
        let ch = CARD_H
            + if note.is_some() {
                self.cell.line_h + 6.0
            } else {
                0.0
            };
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
        self.set_text(Style::N, 1.0);
        self.draw_mono
            .draw_abs(cx, card.pos + dvec2(20.0, 20.0), title);
        self.set_text(Style::Muted, 1.0);
        let step = self.cell.line_h + 6.0;
        self.draw_mono
            .draw_abs(cx, card.pos + dvec2(20.0, 20.0 + step), &role.line());
        let mut line = 2.0;
        if let Some(note) = &note {
            self.draw_mono
                .draw_abs(cx, card.pos + dvec2(20.0, 20.0 + line * step), note);
            line += 1.0;
        }
        let short: String = device.chars().take(8).collect();
        self.draw_mono.draw_abs(
            cx,
            card.pos + dvec2(20.0, 20.0 + line * step),
            &format!("this device: {short}"),
        );

        // The one control. `Offline` has none: there is nothing to take from
        // a bucket that cannot be reached.
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
            let action = match role {
                Role::Waiting { .. } => Act::ForceAcquire,
                Role::Stranded { .. } => Act::RecoverSync,
                _ => Act::Acquire,
            };
            self.hits.push(Hit::act(btn, br, MouseCursor::Hand, action));
        }
        true
    }

    /// A normal takeover requests a handoff. Forcing and recovery are
    /// separate, explicitly labelled actions on their respective screens.
    pub(super) fn acquire_lease(&mut self, cx: &mut Cx, sh: &mut Shell, action: Act) {
        match action {
            Act::ForceAcquire => {
                sh.session.notify("forcing takeover — the other device may have unshared changes", false);
                sh.session.repl_override();
            }
            Act::RecoverSync => {
                sh.session.notify("saving a recovery backup and downloading shared data…", false);
                sh.session.repl_recover();
            }
            _ => {
                sh.session.notify("requesting the lease — waiting for shared data…", false);
                sh.session.repl_acquire();
            }
        }
        self.tick_repl(cx, sh);
    }
}
