//! A part of a letter, drawn: the shell's own card over bytes that came out
//! of a mail rather than off a disk.
//!
//! The card does no reading, and neither does this. The *description* is a
//! row, so it is there at once; the bytes are asked for through
//! [`pictures::want_part`] and come off the reader thread — an attachment is
//! exactly the megabyte-sized blob the rule about draws exists for. Until
//! they land the card is its description with the preview still coming, which
//! is not the same as saying there is none.
//!
//! The bar is the instance's, and it is one verb: `open`.

use std::rc::Rc;

use kernel::caps::{fmt_size, preview_of, FileKind, Preview as Read};
use kernel::panel::PanelId;
use kernel::store::Store;
use makepad_widgets::*;

use super::super::model::MailId;

use crate::shell::hosted::PanelProps;
use crate::shell::widgets::card::{self, CardData, Preview};

use super::super::panels::Card;
use super::pictures::{self, PartBytes};

/// The children mail's own template adds to the shell's card.
const STATUS: &[LiveId] = ids!(status_lbl);

/// The selectable line under the three: the media type, and the run the
/// preview is. Both are addressed by a script — the media type by its own
/// text, which is what tells a part's card from a disk file's (whose line is
/// a path).
const DETAIL: &[LiveId] = ids!(detail_txt);
const PREVIEW: &[LiveId] = ids!(text_box.text_prev);
/// The picture, when there is one: addressed by one word, since a card draws
/// at most one and its bytes are nothing a script can name.
const PICTURE: &[LiveId] = ids!(img_box.img_prev);

/// Which part is on the card, and whether its bytes had landed when it was
/// filled. A picture is decoded once per filling, so a second draw of the
/// same one writes nothing — unless it was still waiting, which is the one
/// reason to fill the same card twice.
#[derive(Clone, PartialEq, Eq)]
struct Shown {
    id: PanelId,
    waiting: bool,
}

/// The widget: the card, and the line a refused verb leaves.
#[derive(Script, ScriptHook, Widget)]
pub struct AttachmentPanel {
    #[source]
    source: ScriptObjectRef,
    #[deref]
    view: View,
    /// What the card was last filled for.
    #[rust]
    shown: Option<Shown>,
}

impl Widget for AttachmentPanel {
    fn handle_event(&mut self, cx: &mut Cx, event: &Event, scope: &mut Scope) {
        self.view.handle_event(cx, event, scope);
        // The bytes this card asked for come back off the reader thread.
        if let Event::Actions(actions) = event {
            if pictures::landed(cx, actions) {
                self.view.redraw(cx);
            }
        }
    }

    fn draw_walk(&mut self, cx: &mut Cx2d, scope: &mut Scope, walk: Walk) -> DrawStep {
        let Some(props) = scope.props.get::<PanelProps>().cloned() else {
            return self.view.draw_walk(cx, scope, walk);
        };
        let Some(r) = read(&props) else {
            return self.view.draw_walk(cx, scope, walk);
        };
        // Only a preview worth having is worth reading a whole letter for:
        // the kind decides whether to ask at all, so a card over a 4 MB PDF
        // costs nothing but its row.
        let mut waiting = false;
        let mut data = r.data;
        let (mail, at) = r.part;
        data.preview = match preview_of(r.kind, &r.name, r.size, |max| {
            match pictures::want_part(cx, &r.store, mail, at) {
                PartBytes::Here(b) => Some(b.iter().take(max).copied().collect()),
                PartBytes::Coming => {
                    waiting = true;
                    None
                }
                PartBytes::Gone => None,
            }
        }) {
            Read::Text(t) => Preview::Text(t),
            Read::Image(b) => Preview::Image(b),
            Read::None => Preview::None,
        };
        let (id, status) = (r.id, r.status);
        let shown = Shown { id, waiting };
        if self.shown.as_ref() != Some(&shown) {
            card::fill(cx, &self.view, &data);
            // Still coming is not the same as never: the line only claims
            // there is nothing to show once the answer is in.
            if waiting {
                self.view.label(cx, ids!(none_lbl)).set_visible(cx, false);
            }
            self.shown = Some(shown);
        }
        let lbl = self.view.label(cx, STATUS);
        lbl.set_text(cx, status.as_deref().unwrap_or(""));
        lbl.set_visible(cx, status.is_some());

        let step = self.view.draw_walk(cx, scope, walk);

        // The media-type line carries its own text as its label, the way a
        // disk card's path does, so a script can say which card this is.
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

/// What the card shows, off the instance — everything but the preview, which
/// this draw is about to ask for.
struct Reading {
    id: PanelId,
    /// The letter and the place in it, for the reader thread.
    part: (MailId, u32),
    name: String,
    kind: FileKind,
    size: u64,
    /// The store the bytes come back out of.
    store: Rc<Store>,
    data: CardData,
    status: Option<String>,
}

/// Everything but the bytes, off the instance. The borrow ends with the call:
/// `fill` writes the whole tree, and nothing may still be holding the panel
/// by then.
fn read(props: &PanelProps) -> Option<Reading> {
    let id = props.panel.borrow().id().clone();
    let mut borrow = props.panel.borrow_mut();
    let c = borrow.as_any().downcast_mut::<Card>()?;
    let (kind_word, size) = if c.gone() {
        ("gone".to_string(), String::new())
    } else {
        (c.kind().word().to_string(), fmt_size(c.size()))
    };
    Some(Reading {
        id,
        part: c.part(),
        name: c.name(),
        kind: c.kind(),
        size: c.size(),
        store: c.store().clone(),
        data: CardData {
            name: c.name(),
            kind_word,
            size,
            modified: c.when(),
            detail: c.detail(),
            preview: Preview::None,
        },
        status: c.status().map(str::to_string),
    })
}
