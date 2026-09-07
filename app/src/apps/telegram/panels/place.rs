//! A place to send: where the device says I am, on the map, with the two
//! ways to share it — once, or live for a while.
//!
//! Opened from the attach panel's bar and joined to it. The fourth phase
//! asks a `Location` capability; this round the answer is the trailhead.

use std::any::Any;
use std::rc::Rc;

use kernel::layout::SlotId;
use kernel::panel::{Opening, Panel, PanelId, PanelKind, Tag, Verb};
use kernel::session::Session;
use kernel::store::Store;

use super::super::draft_toast;
use super::super::model::{self, PeerId, HERE};
use super::super::sync;
use super::told;

/// The place panel.
pub struct Place {
    id: PanelId,
    chat: PeerId,
    store: Rc<Store>,
    slot: SlotId,
}

impl Place {
    pub const TAG: Tag = Tag("place");

    /// The identity of the place to send to one chat.
    #[must_use]
    pub fn id(chat: PeerId) -> PanelId {
        PanelId::new(Self::TAG, [chat.to_string()])
    }

    /// The chat a `place` panel is for; `None` for any other tag.
    #[must_use]
    pub fn of(id: &PanelId) -> Option<PeerId> {
        (id.tag == Self::TAG)
            .then(|| id.arg(0)?.parse().ok())
            .flatten()
    }

    /// Where I am.
    #[must_use]
    pub fn here(&self) -> (f64, f64) {
        HERE
    }

    /// The chat's title.
    #[must_use]
    pub fn chat_title(&self) -> String {
        model::peer(&self.store, self.chat).map_or_else(|| "chat".to_string(), |c| c.name)
    }
}

impl Panel for Place {
    fn id(&self) -> &PanelId {
        &self.id
    }

    fn title(&self) -> String {
        format!("place · {}", self.chat_title())
    }

    /// A card with a map.
    fn wish(&self, _cols: usize) -> (u32, u32) {
        (4, 3)
    }

    fn placed(&mut self, slot: SlotId) {
        self.slot = slot;
    }

    /// Once, or live for an hour.
    fn verbs(&self) -> Vec<Verb> {
        vec![
            Verb::run("telegram.send_place", "send", Some('s')),
            Verb::run("telegram.send_live", "live 1 h", Some('v')),
        ]
    }

    fn run(&mut self, verb: &str, s: &mut Session) {
        let (lat, lon) = self.here();
        match verb {
            // The one-off share, over the wire where the build is signed in:
            // the line comes back as its own echo, the way every send's does,
            // so there is nothing local to write. The place answers no
            // message — this panel is opened from the attach panel, which the
            // composer's reply line does not reach.
            "telegram.send_place" => {
                told(
                    s,
                    &sync::send_location(self.chat, None, lat, lon),
                    &format!("place {lat:.4}, {lon:.4}"),
                );
            }
            // A location that goes on moving is a live one, and that wants a
            // `Location` capability to keep it moving: it stays a toast.
            "telegram.send_live" => {
                s.notify(
                    draft_toast(&format!("live location {lat:.4}, {lon:.4} for an hour")),
                    false,
                );
            }
            _ => {}
        }
    }

    fn as_any(&mut self) -> &mut dyn Any {
        self
    }
}

/// Its factory.
pub struct PlaceKind;

impl PanelKind for PlaceKind {
    fn tag(&self) -> Tag {
        Place::TAG
    }

    fn open(&self, id: &PanelId, cx: &mut Opening<'_>) -> Box<dyn Panel> {
        Box::new(Place {
            chat: Place::of(id).unwrap_or_default(),
            id: id.clone(),
            store: cx.session().store().clone(),
            slot: 0,
        })
    }
}
