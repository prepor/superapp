//! A file, drawn: the shell's own card over what the instance read.
//!
//! The card does no reading. What a file *is* — its size, its date, whether
//! there is a preview worth attempting and what those bytes are — the
//! [`Card`] instance worked out through the disk when it opened, and again
//! whenever a verb wrote one; this hands the answer to
//! [`card::fill`](crate::shell::widgets::card::fill).
//!
//! The bar is the instance's: *open*, *copy*, *move*, and the *delete* that
//! takes the card with the file.

use kernel::session::Session;
use makepad_widgets::*;

use crate::shell::hosted::PanelProps;
use crate::shell::widgets::card::{self, CardData, Preview};

use super::super::model::{fmt_size, FileKind};
use super::super::panels::Card;

/// The children the card's template adds to the shell's own.
const STATUS: &[LiveId] = ids!(status_lbl);

/// The selectable line under the three: the path, and the run the preview
/// is. Both are addressed by a script — the path by its own text, which is
/// the one thing two cards can never both be right about.
const DETAIL: &[LiveId] = ids!(detail_txt);
const PREVIEW: &[LiveId] = ids!(text_box.text_prev);

/// The widget: the card, and the line a refused verb leaves.
#[derive(Script, ScriptHook, Widget)]
pub struct CardPanel {
    #[source]
    source: ScriptObjectRef,
    #[deref]
    view: View,
}

impl Widget for CardPanel {
    fn handle_event(&mut self, cx: &mut Cx, event: &Event, scope: &mut Scope) {
        // Nothing watches a disk, so the card asks again once anybody has
        // written one — on an event as well as on a draw, since a verb's
        // write lands between the two.
        observe(scope);
        self.view.handle_event(cx, event, scope);
    }

    fn draw_walk(&mut self, cx: &mut Cx2d, scope: &mut Scope, walk: Walk) -> DrawStep {
        let Some(props) = scope.props.get::<PanelProps>().cloned() else {
            return self.view.draw_walk(cx, scope, walk);
        };
        observe(scope);
        // Cloned out of the instance: `fill` writes the whole tree, and
        // nothing may still be borrowing the panel by then.
        let Some((data, status)) = read(&props) else {
            return self.view.draw_walk(cx, scope, walk);
        };
        card::fill(cx, &self.view, &data);
        let lbl = self.view.label(cx, STATUS);
        lbl.set_text(cx, status.as_deref().unwrap_or(""));
        lbl.set_visible(cx, status.is_some());

        let step = self.view.draw_walk(cx, scope, walk);

        // The path line carries its own text as its label, the way a row
        // does, so a script can say which file this card is on.
        for (label, path) in [(data.detail.as_str(), DETAIL), ("preview", PREVIEW)] {
            let r = self.view.widget(cx, path).area().rect(cx);
            if r.size.x > 0.0 && !label.is_empty() {
                props.hits.add(label, r, MouseCursor::Text, props.slot);
            }
        }
        step
    }
}

/// What the card shows, off the instance: the name, what it is and how big,
/// when it changed, where it lives, and whatever preview there is.
///
/// A file that has gone says so in its own line rather than reading as a
/// nought-byte one, and the date line says the same thing again.
fn read(props: &PanelProps) -> Option<(CardData, Option<String>)> {
    let mut borrow = props.panel.borrow_mut();
    let c = borrow.as_any().downcast_mut::<Card>()?;
    let (kind_word, size) = if c.gone() {
        ("gone".to_string(), String::new())
    } else if c.kind() == FileKind::Dir {
        (c.kind_word().to_string(), String::new())
    } else {
        (c.kind_word().to_string(), fmt_size(c.size()))
    };
    Some((
        CardData {
            name: c.name(),
            kind_word,
            size,
            modified: c.when(),
            detail: c.path().to_string(),
            // The prototype draws no pictures: a card over one is the card
            // alone, and `open` shows it.
            preview: match c.text() {
                Some(t) => Preview::Text(t.to_string()),
                None => Preview::None,
            },
        },
        c.status().map(str::to_string),
    ))
}

/// Hands the instance the one fact it cannot ask for itself: that the disk
/// has moved under it. Read-only on the session, and both borrows end with
/// the call.
fn observe(scope: &mut Scope) {
    let Some(props) = scope.props.get::<PanelProps>().cloned() else {
        return;
    };
    let Some(session) = scope.data.get_mut::<Session>() else {
        return;
    };
    let session: &Session = session;
    let mut borrow = props.panel.borrow_mut();
    if let Some(c) = borrow.as_any().downcast_mut::<Card>() {
        c.observe(session);
    }
}
