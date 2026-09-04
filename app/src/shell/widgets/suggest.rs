//! A field's autocomplete: the offer under the caret, where the highlight
//! is, and a dismissal that holds until the caret moves on.
//!
//! Generic over a [`Completion`] — the part that differs between fields:
//! the filter's tag grammar, a recipient list — while the box, the keys and
//! the pick are this, once. The panel owns the box (a `#[live] suggest:
//! View` its template fills with `TblSuggest`) and lends it per call, and
//! it is drawn **last**, at an absolute rect hung under the field, so the
//! box covers what follows the field instead of pushing it.

use kernel::richtable::{Completion, Suggestion, MAX_SUGGESTIONS};
use kernel::store::Store;
use makepad_widgets::text::selection::Cursor;
use makepad_widgets::*;

/// The eight suggestion slots of a `TblSuggest`, by name.
const SLOTS: [LiveId; MAX_SUGGESTIONS] = [
    live_id!(s0),
    live_id!(s1),
    live_id!(s2),
    live_id!(s3),
    live_id!(s4),
    live_id!(s5),
    live_id!(s6),
    live_id!(s7),
];

/// One field's live offer.
pub struct Suggest<C: Completion> {
    ctx: Option<C::Ctx>,
    items: Vec<Suggestion>,
    sel: usize,
    dismissed: Option<C::Ctx>,
    /// Whether the field held the keyboard at the last event the panel saw
    /// ([`Suggest::track`]). The draw reads this rather than polling key
    /// focus: in the app the events never stop, so this is key focus with
    /// one event of lag, and a widget that hears no events at all keeps
    /// whatever it was last told.
    focused: bool,
}

impl<C: Completion> Default for Suggest<C> {
    fn default() -> Self {
        Suggest {
            ctx: None,
            items: Vec::new(),
            sel: 0,
            dismissed: None,
            focused: false,
        }
    }
}

impl<C: Completion> Suggest<C> {
    /// Notes whether the field holds the keyboard now. Call it on every
    /// event the panel handles, before anything else reads the box.
    pub fn track(&mut self, cx: &mut Cx, field: &TextInputRef) {
        self.focused = field.key_focus(cx);
    }

    /// Whether the box is up: a context with an offer, not put away.
    #[must_use]
    pub fn open(&self) -> bool {
        self.ctx.is_some() && self.dismissed != self.ctx && !self.items.is_empty()
    }

    /// The keys the box owns while it is open and its field holds the
    /// keyboard: the arrows walk the offer, enter and tab take it, esc puts
    /// it away. `true` when the key was one of them — the field must not
    /// see it (a swallowed enter is the point).
    pub fn key(&mut self, cx: &mut Cx, c: &C, field: &TextInputRef, k: &KeyEvent) -> bool {
        if !self.open() || !field.key_focus(cx) {
            return false;
        }
        match k.key_code {
            KeyCode::ArrowDown => self.sel = (self.sel + 1).min(self.items.len() - 1),
            KeyCode::ArrowUp => self.sel = self.sel.saturating_sub(1),
            KeyCode::ReturnKey | KeyCode::NumpadEnter | KeyCode::Tab => {
                self.pick(cx, c, field, self.sel);
            }
            KeyCode::Escape => self.dismissed = self.ctx.clone(),
            _ => return false,
        }
        true
    }

    /// Commits suggestion `i`: splices it over what the caret was typing,
    /// parks the caret after it and keeps the field's focus, so a picked
    /// `@from:` opens its values without another keystroke.
    pub fn pick(&mut self, cx: &mut Cx, c: &C, field: &TextInputRef, i: usize) {
        let (Some(ctx), Some(item)) = (self.ctx.as_ref(), self.items.get(i)) else {
            return;
        };
        let text = field.text();
        let (line, at) = c.splice(&text, field.cursor().index, ctx, item);
        field.set_text(cx, &line);
        field.set_cursor(
            cx,
            Cursor {
                index: at,
                prefer_next_row: false,
            },
            false,
        );
        field.set_key_focus(cx);
        self.dismissed = None;
    }

    /// Re-derives the offer from the caret — while the field holds the
    /// keyboard; a blurred field offers nothing — fills the slots and draws
    /// the box under the field. Call it after the rest of the panel has
    /// drawn, so the box lands in a draw call over what it covers.
    pub fn draw(
        &mut self,
        cx: &mut Cx2d,
        scope: &mut Scope,
        store: &Store,
        c: &C,
        field: &TextInputRef,
        view: &mut View,
    ) {
        let ctx = if self.focused {
            c.context(&field.text(), field.cursor().index)
        } else {
            None
        };
        if ctx != self.ctx {
            self.items = ctx.as_ref().map(|x| c.offer(store, x)).unwrap_or_default();
            self.ctx = ctx;
            self.sel = 0;
            if self.dismissed != self.ctx {
                self.dismissed = None;
            }
        }
        let open = self.open();
        view.set_visible(cx, open);
        if !open {
            return;
        }
        let (ink, bg, dim, dim_inv) = (
            vec4(0.078, 0.078, 0.078, 1.0),
            vec4(1.0, 1.0, 1.0, 1.0),
            vec4(0.565, 0.565, 0.565, 1.0),
            vec4(0.75, 0.75, 0.75, 1.0),
        );
        for (i, slot) in SLOTS.iter().enumerate() {
            let row = view.view(cx, &[*slot]);
            let Some(it) = self.items.get(i) else {
                row.set_visible(cx, false);
                continue;
            };
            row.set_visible(cx, true);
            let selected = i == self.sel;
            for (line, on, fg, fg_dim) in [
                (live_id!(line), !selected, ink, dim),
                (live_id!(line_sel), selected, bg, dim_inv),
            ] {
                view.view(cx, &[*slot, line]).set_visible(cx, on);
                if !on {
                    continue;
                }
                let lbl = view.label(cx, &[*slot, line, live_id!(lbl)]);
                lbl.set_text(cx, &it.label);
                lbl.set_text_color(cx, fg);
                let desc = view.label(cx, &[*slot, line, live_id!(desc)]);
                desc.set_text(cx, &it.describe);
                desc.set_visible(cx, !it.describe.is_empty());
                desc.set_text_color(cx, fg_dim);
            }
        }
        let fr = field.area().rect(cx);
        if fr.size.x <= 0.0 {
            return;
        }
        view.draw_walk_all(
            cx,
            scope,
            Walk {
                abs_pos: Some(dvec2(fr.pos.x, fr.pos.y + fr.size.y + 2.0)),
                width: Size::Fixed(fr.size.x),
                ..Walk::fit()
            },
        );
    }

    /// The open box's rows, `(label, rect)`, for the hit table — a press on
    /// one is a [`Suggest::pick`].
    #[must_use]
    pub fn hits(&self, cx: &mut Cx, view: &View) -> Vec<(String, Rect)> {
        if !self.open() {
            return Vec::new();
        }
        self.items
            .iter()
            .zip(SLOTS.iter())
            .map(|(it, slot)| (it.label.clone(), view.view(cx, &[*slot]).area().rect(cx)))
            .filter(|(_, r)| r.size.x > 0.0)
            .collect()
    }
}
