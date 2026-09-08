//! The messages a filter finds: everywhere, or in one chat.
//!
//! One source under both: a panel opened *about* a chat starts narrowed to
//! it, and the field shows `@chat:stelaxis` so the next edit is the
//! person's.

use std::any::Any;
use std::rc::Rc;

use kernel::layout::SlotId;
use kernel::panel::{Opening, Panel, PanelId, PanelKind, Tag, Verb};
use kernel::richtable::ListState;
use kernel::session::Session;
use kernel::store::Store;

use super::super::model::{self, PeerId, PAGE};
#[cfg(test)]
use super::super::model::MsgHit;
use super::super::runtime;
use super::super::search_index::MessageSource;

/// A messages panel.
pub struct Messages {
    id: PanelId,
    chat: Option<PeerId>,
    replies: bool,
    store: Rc<Store>,
    slot: SlotId,
    list: ListState<&'static MessageSource>,
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

    pub fn replies(chat: Option<PeerId>) -> PanelId {
        PanelId::new(Self::TAG, std::iter::once("replies".to_string()).chain(chat.map(|p| p.to_string())))
    }

    pub fn is_replies(&self) -> bool {
        self.replies
    }

    fn refresh(&self) {
        for (chat, _) in model::reply_chats(&self.store).iter() {
            if self.chat.is_none_or(|peer| peer == *chat) {
                runtime::of(&self.store).want_mentions(*chat);
            }
        }
    }

    pub fn reply_status(&self) -> (bool, bool) {
        runtime::of(&self.store).mentions_status(self.chat)
    }

    pub fn pending_count(&self) -> i64 {
        model::reply_chats(&self.store).iter()
            .filter(|(chat, _)| self.chat.is_none_or(|peer| peer == *chat))
            .map(|(_, count)| count).sum()
    }

    /// The chat a `messages` panel is about, if one.
    #[must_use]
    pub fn chat_of(id: &PanelId) -> Option<PeerId> {
        (id.tag == Self::TAG)
            .then(|| id.arg(usize::from(id.arg(0) == Some("replies")))?.parse().ok())
            .flatten()
    }

    pub fn list_mut(&mut self) -> &mut ListState<&'static MessageSource> {
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
        let word = if self.replies { "replies & mentions" } else { "messages" };
        let status = match (self.replies, self.reply_status()) {
            (true, (true, _)) => " · loading…",
            (true, (_, true)) => " · could not finish loading",
            _ => "",
        };
        match self.chat_title() {
            Some(t) => format!("{word} · {t}{status}"),
            None => format!("{word}{status}"),
        }
    }

    fn verbs(&self) -> Vec<Verb> {
        if self.replies {
            vec![Verb::run("telegram.refresh_replies", "refresh", Some('r'))]
        } else {
            Vec::new()
        }
    }

    fn run(&mut self, verb: &str, s: &mut Session) {
        if verb == "telegram.refresh_replies" {
            self.refresh();
            s.redraw();
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
        let replies = id.arg(0) == Some("replies");
        let panel = Messages {
            id: id.clone(),
            chat: Messages::chat_of(id),
            replies,
            store: cx.session().store().clone(),
            slot: 0,
            list: ListState::new(if replies { &model::REPLIES } else { &model::MESSAGES }, PAGE),
        };
        if replies {
            panel.refresh();
        }
        Box::new(panel)
    }
}
