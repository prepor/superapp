//! The people: the address book, and a group's members.
//!
//! One instance type under two tags. What differs is the source, the title,
//! and where a row opens — the contacts list previews the chat with a
//! person, which is how a new conversation starts, and the members list
//! previews the member's card.

use std::any::Any;
use std::rc::Rc;

use kernel::layout::SlotId;
use kernel::panel::{Opening, Panel, PanelId, PanelKind, Tag};
use kernel::richtable::{ListState, SqlSource};
use kernel::store::Store;

use super::super::model::{self, PeerId, Person, PAGE};

/// The address book's identity.
pub struct Contacts;

impl Contacts {
    pub const TAG: Tag = Tag("contacts");

    #[must_use]
    pub fn id() -> PanelId {
        PanelId::bare(Self::TAG)
    }
}

/// A group's members' identity.
pub struct Members;

impl Members {
    pub const TAG: Tag = Tag("members");

    /// The members of one group.
    #[must_use]
    pub fn id(group: PeerId) -> PanelId {
        PanelId::new(Self::TAG, [group.to_string()])
    }

    /// The group a `members` panel is of; `None` for any other tag.
    #[must_use]
    pub fn of(id: &PanelId) -> Option<PeerId> {
        (id.tag == Self::TAG)
            .then(|| id.arg(0)?.parse().ok())
            .flatten()
    }
}

/// A people panel: contacts, or one group's members.
pub struct People {
    id: PanelId,
    group: Option<PeerId>,
    store: Rc<Store>,
    slot: SlotId,
    list: ListState<&'static SqlSource<Person, i64>>,
}

impl People {
    #[must_use]
    pub fn group(&self) -> Option<PeerId> {
        self.group
    }

    pub fn list_mut(&mut self) -> &mut ListState<&'static SqlSource<Person, i64>> {
        &mut self.list
    }

    /// The group's name, where the panel is of one.
    #[must_use]
    pub fn group_name(&self) -> Option<String> {
        model::peer(&self.store, self.group?).map(|c| c.name)
    }

    /// The filter a members list comes up under: its group, in the list's
    /// own grammar.
    #[must_use]
    pub fn seed_filter(&self) -> String {
        match self.group_name() {
            Some(g) => format!("@group:{} ", model::filter_value(&g)),
            None => String::new(),
        }
    }

    /// Rows `lo..hi`, as far as the table has them.
    #[cfg(test)]
    #[must_use]
    pub fn rows(&self, lo: usize, hi: usize) -> Vec<Person> {
        self.list.table().rows(&self.store, lo, hi)
    }
}

impl Panel for People {
    fn id(&self) -> &PanelId {
        &self.id
    }

    /// `contacts`, or `members · stelaxis`.
    fn title(&self) -> String {
        match self.group_name() {
            Some(g) => format!("members · {g}"),
            None => "contacts".to_string(),
        }
    }

    fn wish(&self, _cols: usize) -> (u32, u32) {
        (4, 6)
    }

    fn placed(&mut self, slot: SlotId) {
        self.slot = slot;
    }

    fn as_any(&mut self) -> &mut dyn Any {
        self
    }
}

/// The address book's factory.
pub struct ContactsKind;

impl PanelKind for ContactsKind {
    fn tag(&self) -> Tag {
        Contacts::TAG
    }

    fn open(&self, id: &PanelId, cx: &mut Opening<'_>) -> Box<dyn Panel> {
        Box::new(People {
            id: id.clone(),
            group: None,
            store: cx.session().store().clone(),
            slot: 0,
            list: ListState::new(&model::CONTACTS, PAGE),
        })
    }
}

/// The members list's factory.
pub struct MembersKind;

impl PanelKind for MembersKind {
    fn tag(&self) -> Tag {
        Members::TAG
    }

    fn open(&self, id: &PanelId, cx: &mut Opening<'_>) -> Box<dyn Panel> {
        Box::new(People {
            id: id.clone(),
            group: Members::of(id),
            store: cx.session().store().clone(),
            slot: 0,
            list: ListState::new(&model::MEMBERS, PAGE),
        })
    }
}
