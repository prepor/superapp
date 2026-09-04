//! One correspondent's card: who they are, how much they have written, and
//! the one link off it.
//!
//! The card owns nothing. Everything it shows is a cached query on the
//! address it carries, so a letter that arrives while it is open changes the
//! count on the next draw.

use std::any::Any;
use std::rc::Rc;

use kernel::layout::SlotId;
use kernel::nav::Nav;
use kernel::panel::{Opening, Panel, PanelId, PanelKind, Tag, Verb};
use kernel::store::Store;

use super::super::accounts;
use super::super::model::Role;

/// A sender's card.
pub struct Contact {
    id: PanelId,
    email: String,
    store: Rc<Store>,
    slot: SlotId,
}

impl Contact {
    pub const TAG: Tag = Tag("contact");

    /// The identity of one correspondent's card.
    #[must_use]
    pub fn id(email: &str) -> PanelId {
        PanelId::new(Self::TAG, [email])
    }

    /// The address a `contact` panel names; `None` for any other tag.
    #[must_use]
    pub fn of(id: &PanelId) -> Option<&str> {
        (id.tag == Self::TAG).then(|| id.arg(0)).flatten()
    }

    #[must_use]
    pub fn email(&self) -> &str {
        &self.email
    }

    #[must_use]
    pub fn slot(&self) -> SlotId {
        self.slot
    }

    /// Their name as of their latest letter, and how many they have sent.
    #[must_use]
    pub fn who(&self) -> (String, i64) {
        accounts::contact(&self.store, &self.email)
    }

    /// What the link is called: *messages from* their first name, lowercased
    /// like every other label in this language.
    #[must_use]
    pub fn link_label(&self) -> String {
        let (name, _) = self.who();
        let first = name.split(' ').next().unwrap_or(&name).to_lowercase();
        format!("messages from {first}")
    }
}

impl Panel for Contact {
    fn id(&self) -> &PanelId {
        &self.id
    }

    fn title(&self) -> String {
        self.who().0
    }

    /// A card, not a list: a name, an address, a count and a link.
    fn wish(&self, _cols: usize) -> (u32, u32) {
        (3, 2)
    }

    fn placed(&mut self, slot: SlotId) {
        self.slot = slot;
    }

    /// One link: the inbox, filtered to this address. It goes on the bar for
    /// the reason every navigation does — and the card draws it in its body
    /// too, where a person reading the card looks for it.
    fn verbs(&self) -> Vec<Verb> {
        vec![Verb::go(
            "mail.from",
            self.link_label(),
            Some('m'),
            Nav::Open {
                from: self.slot,
                id: Role::Inbox.filtered(&self.email),
                fresh: false,
            },
        )]
    }

    fn as_any(&mut self) -> &mut dyn Any {
        self
    }
}

/// Its factory.
pub struct ContactKind;

impl PanelKind for ContactKind {
    fn tag(&self) -> Tag {
        Contact::TAG
    }

    fn open(&self, id: &PanelId, cx: &mut Opening<'_>) -> Box<dyn Panel> {
        Box::new(Contact {
            email: Contact::of(id).unwrap_or_default().to_string(),
            id: id.clone(),
            store: cx.session().store().clone(),
            slot: 0,
        })
    }
}
