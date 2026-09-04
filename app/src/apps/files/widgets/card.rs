//! A file, drawn: the shell's own card over what the instance read.
//!
//! The card does no reading. What a file *is* — its size, its date, whether
//! there is a preview worth attempting and what those bytes are — the
//! [`Card`] instance worked out through the disk when it opened, and again
//! whenever a verb wrote one; this hands the answer to
//! [`card::fill`](crate::shell::widgets::card::fill).
//!
//! Filled when the reading changes and not once a frame: a picture is
//! decoded into a texture of its own, so the widget remembers which reading
//! of which file is on the card and writes again only when that moves.
//!
//! The bar is the instance's: *open*, *copy*, *move*, and the *delete* that
//! takes the card with the file.

use kernel::panel::PanelId;
use kernel::session::Session;
use makepad_widgets::*;

use crate::shell::hosted::PanelProps;
use crate::shell::widgets::card::{self, CardData, Preview};

use super::super::model::{fmt_size, FileKind, Preview as Read};
use super::super::panels::Card;

/// The children the card's template adds to the shell's own.
const STATUS: &[LiveId] = ids!(status_lbl);

/// The selectable line under the three: the path, and the run the preview
/// is. Both are addressed by a script — the path by its own text, which is
/// the one thing two cards can never both be right about.
const DETAIL: &[LiveId] = ids!(detail_txt);
const PREVIEW: &[LiveId] = ids!(text_box.text_prev);
/// The picture, when there is one: addressed by one word, since a card
/// draws at most one and its bytes are nothing a script can name.
const PICTURE: &[LiveId] = ids!(img_box.img_prev);

/// Which reading of which file is on the card. A picture is decoded once
/// per reading, so a second draw of the same one writes nothing.
#[derive(Clone, PartialEq, Eq)]
struct Shown {
    id: PanelId,
    /// The disk's write count when the instance last read: a verb that
    /// wrote is a new reading of the same path.
    at: u64,
}

/// The widget: the card, and the line a refused verb leaves.
#[derive(Script, ScriptHook, Widget)]
pub struct CardPanel {
    #[source]
    source: ScriptObjectRef,
    #[deref]
    view: View,
    /// What the card was last filled for.
    #[rust]
    shown: Option<Shown>,
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
        let Some(shown) = shown(&props) else {
            return self.view.draw_walk(cx, scope, walk);
        };
        let fresh = self.shown.as_ref() != Some(&shown);
        // Cloned out of the instance: `fill` writes the whole tree, and
        // nothing may still be borrowing the panel by then. The preview
        // comes along only when it is going to be written — a reading is up
        // to 64 KiB and a picture rather more.
        let Some((data, status)) = read(&props, fresh) else {
            return self.view.draw_walk(cx, scope, walk);
        };
        if fresh {
            card::fill(cx, &self.view, &data);
            self.shown = Some(shown);
        }
        let lbl = self.view.label(cx, STATUS);
        lbl.set_text(cx, status.as_deref().unwrap_or(""));
        lbl.set_visible(cx, status.is_some());

        let step = self.view.draw_walk(cx, scope, walk);

        // The path line carries its own text as its label, the way a row
        // does, so a script can say which file this card is on.
        for (label, path, cursor) in [
            (data.detail.as_str(), DETAIL, MouseCursor::Text),
            ("preview", PREVIEW, MouseCursor::Text),
            ("picture", PICTURE, MouseCursor::Default),
        ] {
            let r = self.view.widget(cx, path).area().rect(cx);
            if r.size.x > 0.0 && r.size.y > 0.0 && !label.is_empty() {
                props.hits.add(label, r, cursor, props.slot);
            }
        }
        step
    }
}

/// Which reading of which file the instance is holding right now.
fn shown(props: &PanelProps) -> Option<Shown> {
    // The identity off the instance as a panel, the reading off it as a
    // card: `Card::id` is the constructor of one, not the accessor.
    let id = props.panel.borrow().id().clone();
    let mut borrow = props.panel.borrow_mut();
    let c = borrow.as_any().downcast_mut::<Card>()?;
    Some(Shown {
        id,
        at: c.read_at(),
    })
}

/// What the card shows, off the instance: the name, what it is and how big,
/// when it changed, where it lives, and — when this draw is going to write
/// it — whatever preview there is.
///
/// A file that has gone says so in its own line rather than reading as a
/// nought-byte one, and the date line says the same thing again.
fn read(props: &PanelProps, preview: bool) -> Option<(CardData, Option<String>)> {
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
            // The instance's reading, in the card's own words. The two
            // enums are the same three cases on either side of the seam:
            // the app decides what is worth reading, the card decodes.
            preview: match (preview, c.preview()) {
                (true, Read::Text(t)) => Preview::Text(t.clone()),
                (true, Read::Image(b)) => Preview::Image(b.clone()),
                _ => Preview::None,
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
