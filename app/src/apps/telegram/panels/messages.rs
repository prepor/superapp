//! The messages a filter finds: everywhere, or in one chat.
//!
//! One source under both: a panel opened *about* a chat starts narrowed to
//! it, and the field shows `@chat:stelaxis` so the next edit is the
//! person's.

use std::any::Any;
use std::rc::Rc;

use kernel::layout::SlotId;
use kernel::panel::{Opening, Panel, PanelId, PanelKind, Tag};
use kernel::richtable::{ListState, SqlSource};
use kernel::store::Store;

use super::super::model::{self, MsgHit, PeerId, PAGE};

/// A messages panel.
pub struct Messages {
    id: PanelId,
    chat: Option<PeerId>,
    store: Rc<Store>,
    slot: SlotId,
    list: ListState<&'static SqlSource<MsgHit, i64>>,
}

impl Messages {
    pub const TAG: Tag = Tag("messages");

    /// Every chat's messages.
    #[must_use]
    pub fn id() -> PanelId {
        PanelId::bare(Self::TAG)
    }

    /// One chat's.
    #[must_use]
    pub fn in_chat(peer: PeerId) -> PanelId {
        PanelId::new(Self::TAG, [peer.to_string()])
    }

    /// The chat a `messages` panel is about, if one.
    #[must_use]
    pub fn chat_of(id: &PanelId) -> Option<PeerId> {
        (id.tag == Self::TAG)
            .then(|| id.arg(0)?.parse().ok())
            .flatten()
    }

    pub fn list_mut(&mut self) -> &mut ListState<&'static SqlSource<MsgHit, i64>> {
        &mut self.list
    }

    /// The chat's title, where the panel is about one.
    #[must_use]
    pub fn chat_title(&self) -> Option<String> {
        model::peer(&self.store, self.chat?).map(|c| c.name)
    }

    /// The filter the panel comes up under: the chat it is about, in the
    /// list's own grammar. The widget seeds its field with this once.
    #[must_use]
    pub fn seed_filter(&self) -> String {
        match self.chat_title() {
            Some(t) => format!("@chat:{} ", model::filter_value(&t)),
            None => String::new(),
        }
    }

    /// Rows `lo..hi`, as far as the table has them.
    #[cfg(test)]
    #[must_use]
    pub fn rows(&self, lo: usize, hi: usize) -> Vec<MsgHit> {
        self.list.table().rows(&self.store, lo, hi)
    }
}

impl Panel for Messages {
    fn id(&self) -> &PanelId {
        &self.id
    }

    /// `messages`, or `messages · stelaxis`.
    fn title(&self) -> String {
        match self.chat_title() {
            Some(t) => format!("messages · {t}"),
            None => "messages".to_string(),
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

/// The factory.
pub struct MessagesKind;

impl PanelKind for MessagesKind {
    fn tag(&self) -> Tag {
        Messages::TAG
    }

    fn open(&self, id: &PanelId, cx: &mut Opening<'_>) -> Box<dyn Panel> {
        Box::new(Messages {
            id: id.clone(),
            chat: Messages::chat_of(id),
            store: cx.session().store().clone(),
            slot: 0,
            list: ListState::new(&model::MESSAGES, PAGE),
        })
    }
}
