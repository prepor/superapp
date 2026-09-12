//! The panel context menu: what a long press on a header offers.
//!
//! It hangs from the header that was pressed. The sheet's head *is* that
//! header — the panel's title in the chrome's own idiom, in the header's
//! own place — and the actions unfold beneath it, one row each, so the
//! menu reads as the panel's rather than as a palette hung over the
//! workspace. It rides the panel's own springs, so a camera pan under it
//! carries it along, and the chassis' presence spring in and out.
//!
//! The rows keep the panel they were opened for
//! ([`Stage::context_panel`](super::pointer)), so a focus change
//! underneath changes nothing; what each one does lives with the pointer,
//! beside the chords that do the same.

use kernel::layout::SlotId;
use kernel::theme;
use makepad_widgets::*;

use super::draw::{rect, rgba_a, solo_rect, trunc, Camera};
use super::hits::{Act, Hit};
use super::stage::{Shell, Stage};

/// A row: a finger's target, comfortably.
const ROW_H: f64 = 48.0;
/// A row with a second line under its label.
const ROW_2_H: f64 = 60.0;
/// A label's inset from the sheet's edge.
const PAD_X: f64 = 16.0;
/// The rows' type: the size the other sheets' rows are set in.
const TEXT: f64 = 13.0;
/// The sheet is as wide as the header it hangs from, within these: a
/// narrow panel's menu still holds its labels, and a panel across a
/// desktop does not make a menu across a desktop. The cap clears a
/// five-unit panel at desktop size, so a menu is cut short of its header
/// only under a wide one.
pub(super) const MIN_W: f64 = 240.0;
pub(super) const MAX_W: f64 = 600.0;

/// One action of the menu.
#[derive(Debug, Clone, PartialEq)]
pub(super) struct ContextRow {
    /// What the row says, and what a script addresses it by.
    pub label: String,
    /// A second line, where what the action does depends on the state it
    /// finds.
    pub detail: String,
    pub act: Act,
}

/// Where the menu's parts land.
#[derive(Debug, Clone, PartialEq)]
pub(super) struct ContextLayout {
    /// The whole sheet, border included.
    pub sheet: Rect,
    /// Its head: the panel's title strip.
    pub head: Rect,
    /// One rectangle per row, in order, inside the border.
    pub rows: Vec<Rect>,
}

/// The actions a panel's header offers, in order. Only a panel in a join
/// has one to break, and that row names the bridge that would go — the
/// touch way to what `cmd+shift+j` does, and, after the fact, to what
/// `cmd+click` does, which a finger cannot spell. The last one is the only
/// way to close a panel that a finger can be sure of: the header's own
/// close box is drawn for a pointer.
pub(super) fn context_rows(sh: &Shell, slot: SlotId) -> Vec<ContextRow> {
    let tabbed = sh
        .session
        .ws()
        .ws_of(slot)
        .and_then(|k| {
            let ws = &sh.session.ws().wss[k];
            ws.locate(slot).map(|(col, _)| ws.columns[col].tabbed)
        })
        .unwrap_or(false);
    let row = |label: &str, detail: &str, act: Act| ContextRow {
        label: label.into(),
        detail: detail.into(),
        act,
    };
    let mut rows = vec![
        row("start agent with panel context", "", Act::PanelAsk(slot)),
        row("copy panel context", "", Act::PanelCopyContext(slot)),
        row(
            "switch column tab mode",
            if tabbed {
                "show stacked panels"
            } else {
                "show panels as tabs"
            },
            Act::PanelToggleTabs(slot),
        ),
    ];
    if let Some((parent, child)) = sh.session.ws().bridge_of(slot) {
        let title = |s| super::keys::title_of(sh, s);
        rows.push(row(
            "unjoin panel",
            &format!("“{}” ═ “{}”", title(parent), title(child)),
            Act::PanelUnjoin(slot),
        ));
    }
    rows.push(row("close panel", "", Act::PanelClose(slot)));
    rows
}

fn row_h(row: &ContextRow) -> f64 {
    if row.detail.is_empty() {
        ROW_H
    } else {
        ROW_2_H
    }
}

/// Lays the sheet out from the header it hangs from — `anchor`, the
/// header's rectangle on screen — or centred, as the other sheets are,
/// where there is none to hang from. Lifted, or pulled in, as far as it
/// takes to stay on the screen.
pub(super) fn context_layout(vp: Rect, anchor: Option<Rect>, rows: &[ContextRow]) -> ContextLayout {
    let h = theme::HEAD_H + rows.iter().map(row_h).sum::<f64>() + 1.0;
    let (x, y, w) = match anchor {
        Some(a) => (a.pos.x, a.pos.y, a.size.x.clamp(MIN_W, MAX_W)),
        None => {
            let w = (vp.size.x - 4.0 * theme::GAP).min(MAX_W);
            (
                vp.pos.x + (vp.size.x - w) / 2.0,
                vp.pos.y + (vp.size.y * 0.14).max(2.0 * theme::GAP),
                w,
            )
        }
    };
    let w = w.min(vp.size.x);
    let x = x.min(vp.pos.x + vp.size.x - w).max(vp.pos.x);
    let y = y.min(vp.pos.y + vp.size.y - theme::GAP - h).max(vp.pos.y);
    let sheet = rect(x, y, w, h);
    let head = rect(x, y, w, theme::HEAD_H);
    let mut ry = y + theme::HEAD_H;
    let rows = rows
        .iter()
        .map(|r| {
            let rr = rect(x + 1.0, ry, w - 2.0, row_h(r));
            ry += row_h(r);
            rr
        })
        .collect();
    ContextLayout { sheet, head, rows }
}

impl Stage {
    /// The pressed panel's header, where it is drawn this frame.
    fn context_anchor(&self, sh: &mut Shell, vp: Rect, slot: SlotId) -> Option<Rect> {
        let r = if self.solo == Some(slot) {
            solo_rect(vp)
        } else {
            let (krect, ws) = sh.anim.panels.get(&slot).map(|pa| (pa.rect(), pa.ws))?;
            let cam = Camera {
                vp,
                cam_x: sh.anim.camera().value(),
                slide: sh.anim.slide().value(),
                step: vp.size.y + theme::GAP,
            };
            cam.to_screen(krect, ws)
        };
        Some(rect(r.pos.x, r.pos.y, r.size.x, theme::HEAD_H))
    }

    /// Draws the menu at presence `p`, and registers its rows as hits
    /// while it is `live`. The wash goes in first, so a tap that misses
    /// the sheet dismisses it, and the rows after, so one that lands on a
    /// row runs it.
    pub(super) fn draw_panel_context(
        &mut self,
        cx: &mut Cx2d,
        sh: &mut Shell,
        vp: Rect,
        p: f64,
        live: bool,
        slot: SlotId,
    ) {
        let p = p.clamp(0.0, 1.0);
        if live {
            self.hits.clear();
        }
        self.draw_flat.new_draw_call(cx);
        self.draw_flat.color = rgba_a(theme::INK, 0.30 * p);
        self.draw_flat.draw_abs(cx, vp);
        if live {
            self.hits.push(Hit::act(
                "panel context",
                vp,
                MouseCursor::Default,
                Act::OverlayClose,
            ));
        }

        let rows = context_rows(sh, slot);
        let anchor = self.context_anchor(sh, vp, slot);
        // A sheet folding back into a header that is gone — the panel was
        // closed from it — has nowhere to fold to.
        if !live && anchor.is_none() {
            return;
        }
        let lay = context_layout(vp, anchor, &rows);
        let title = self.title_of(sh, slot);
        let hover = sh.hover.clone();

        // The sheet unfolds from its head: the box grows down over the
        // panel as the spring runs, and the rows are clipped to it. Its
        // presence is the fold, not a fade — it is opaque throughout, so
        // the panel never shows through the rows on their way in.
        let body_h = lay.sheet.size.y - theme::HEAD_H;
        let shown = rect(
            lay.sheet.pos.x,
            lay.sheet.pos.y,
            lay.sheet.size.x,
            theme::HEAD_H + body_h * p,
        );
        // Its own draw call, or it would merge below the wash.
        self.draw_panel.new_draw_call(cx);
        self.draw_panel.color = rgba_a(theme::BG, 1.0);
        self.draw_panel.border_color = rgba_a(theme::INK, 1.0);
        self.draw_panel.border_size = 1.0;
        self.draw_panel.alpha = 1.0;
        self.draw_panel.draw_abs(cx, shown);

        // The head is the panel's header, in the header's place: the same
        // strip, the same title, without the close box a pointer aims at.
        self.draw_flat.new_draw_call(cx);
        self.draw_flat.color = rgba_a(theme::INK, 1.0);
        self.draw_flat.draw_abs(cx, lay.head);
        self.draw_mono.new_draw_call(cx);
        let cols = ((lay.head.size.x - 16.0) / self.cell.label_step()).max(4.0) as usize;
        let ty = lay.head.pos.y + (theme::HEAD_H - self.cell.label_line()) / 2.0;
        self.draw_label(cx, lay.head.pos.x + 8.0, ty, &trunc(&title, cols), theme::BG, 1.0);
        if live {
            self.hits
                .push(Hit::act(title, lay.head, MouseCursor::Default, Act::Noop));
        }

        let clip = rect(
            shown.pos.x,
            shown.pos.y + theme::HEAD_H,
            shown.size.x,
            (shown.size.y - theme::HEAD_H).max(0.0),
        );
        cx.begin_turtle(
            Walk::abs_rect(clip),
            Layout {
                clip_x: true,
                clip_y: true,
                ..Default::default()
            },
        );
        let line = self.cell.natural * (TEXT / theme::FONT_SIZE);
        for (i, (row, r)) in rows.iter().zip(&lay.rows).enumerate() {
            // A row under the pointer inverts, as every control does; the
            // hairline between two rows is what keeps them two.
            let hovered = hover.as_ref() == Some(&row.act);
            let (fg, dim) = if hovered {
                (theme::BG, theme::RULE)
            } else {
                (theme::INK, theme::MUTED)
            };
            if hovered {
                self.draw_flat.color = rgba_a(theme::INK, 1.0);
                self.draw_flat.draw_abs(cx, *r);
            } else if i > 0 {
                self.draw_flat.color = rgba_a(theme::RULE, 1.0);
                self.draw_flat
                    .draw_abs(cx, rect(r.pos.x, r.pos.y, r.size.x, 1.0));
            }
            let text_h = if row.detail.is_empty() {
                line
            } else {
                line + 3.0 + self.cell.label_line()
            };
            let y = r.pos.y + (r.size.y - text_h) / 2.0;
            self.draw_mono.text_style.font_size = TEXT as f32;
            self.draw_mono.color = rgba_a(fg, 1.0);
            self.draw_mono
                .draw_abs(cx, dvec2(r.pos.x + PAD_X, y), &row.label);
            if !row.detail.is_empty() {
                self.draw_label(cx, r.pos.x + PAD_X, y + line + 3.0, &row.detail, dim, 1.0);
            }
            if live {
                self.hits.push(Hit::act(
                    row.label.clone(),
                    *r,
                    MouseCursor::Hand,
                    row.act.clone(),
                ));
            }
        }
        cx.end_turtle();
    }
}
