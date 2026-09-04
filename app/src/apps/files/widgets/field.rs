//! A field a panel raises: `go to`, `new dir`, `rename`.
//!
//! The text is the instance's and never the widget's — a verb that closes a
//! field takes what was typed with it — so the widget's whole share is what
//! is here: raise the row when the instance opens one, seed it the once,
//! hand it the keyboard when it has a rectangle to take it in, and give the
//! keyboard back to the panel when it goes.

use makepad_widgets::text::selection::Cursor;
use makepad_widgets::*;

/// One raised field's own memory: whether its row was up at the last look,
/// and whether it is still waiting for the keyboard.
#[derive(Debug, Clone, Copy, Default)]
pub struct Raised {
    /// Whether the row was up at the last look, so the field is seeded once
    /// — when it opens — and not written over as it is typed.
    up: bool,
    /// A field just raised wants the keyboard, once it has been drawn where
    /// it will stand: focus on a field with no rectangle lands nowhere.
    landing: bool,
}

impl Raised {
    /// Whether the row is up right now, as of the last [`Raised::sync`].
    #[must_use]
    pub fn up(self) -> bool {
        self.up
    }

    /// Raises or lowers the row with the instance's own state, seeding the
    /// field on the look it opens. Answers whether this was that look, which
    /// is where a caller starts anything of its own — an offer, say.
    ///
    /// Called from the draw as well as from the event, so a verb's field is
    /// up in the very frame it asked for.
    pub fn sync(
        &mut self,
        cx: &mut Cx,
        view: &View,
        row: &[LiveId],
        input: &TextInputRef,
        text: Option<&str>,
    ) -> bool {
        let up = text.is_some();
        if up == self.up {
            return false;
        }
        self.up = up;
        view.widget(cx, row).set_visible(cx, up);
        if up {
            input.set_text(cx, text.unwrap_or(""));
            self.landing = true;
        } else if input.key_focus(cx) {
            // The keyboard goes back to the panel, never to a field that is
            // no longer there.
            cx.set_key_focus(view.area());
        }
        up
    }

    /// The deferred focus: the field takes the keyboard once it has been
    /// drawn where it will stand. `whole` selects what is in it — a name is
    /// a value to type over, where a seeded path is one to type after.
    pub fn land(&mut self, cx: &mut Cx, input: &TextInputRef, whole: bool) {
        if !self.landing || !self.up || input.area().rect(cx).size.y <= 0.0 {
            return;
        }
        self.landing = false;
        input.set_key_focus(cx);
        if whole {
            if let Some(mut t) = input.borrow_mut() {
                t.select_all(cx);
            }
            return;
        }
        let end = input.text().len();
        input.set_cursor(
            cx,
            Cursor {
                index: end,
                prefer_next_row: false,
            },
            false,
        );
    }

    /// Whether the caret is in this field right now.
    ///
    /// A hidden field that has never been drawn reads makepad's "nothing has
    /// focus" as its own, so a field counts only while its row is up.
    #[must_use]
    pub fn live(self, cx: &Cx, input: &TextInputRef) -> bool {
        self.up && input.key_focus(cx)
    }
}
