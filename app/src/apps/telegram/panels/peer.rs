//! A peer's card: who or what a chat is with, and the ways off it.
//!
//! The card owns nothing. Everything it shows is a cached query on the id it
//! carries, so a flag that changes under it changes on the next draw.

use std::any::Any;
use std::rc::Rc;

use kernel::layout::SlotId;
use kernel::nav::Nav;
use kernel::panel::{Opening, Panel, PanelId, PanelKind, Tag, Verb};
use kernel::session::Session;
use kernel::store::Store;

use super::super::model::{self, PeerCard, PeerId, PeerKind as Kind};
use super::super::sync;
use super::{flip, told, Chat, Members, Messages};

/// A peer's card.
pub struct Peer {
    id: PanelId,
    peer: PeerId,
    store: Rc<Store>,
    slot: SlotId,
}

impl Peer {
    pub const TAG: Tag = Tag("peer");

    /// The identity of one peer's card.
    #[must_use]
    pub fn id(peer: PeerId) -> PanelId {
        PanelId::new(Self::TAG, [peer.to_string()])
    }

    /// The peer a `peer` panel names; `None` for any other tag.
    #[must_use]
    pub fn of(id: &PanelId) -> Option<PeerId> {
        (id.tag == Self::TAG)
            .then(|| id.arg(0)?.parse().ok())
            .flatten()
    }

    /// The card, off the store. `None` for a peer it does not have.
    #[must_use]
    pub fn card(&self) -> Option<PeerCard> {
        model::peer(&self.store, self.peer)
    }
}

impl Panel for Peer {
    fn id(&self) -> &PanelId {
        &self.id
    }

    fn title(&self) -> String {
        self.card().map_or_else(|| "peer".to_string(), |c| c.name)
    }

    /// A card, not a list.
    fn wish(&self, _cols: usize) -> (u32, u32) {
        (4, 3)
    }

    fn placed(&mut self, slot: SlotId) {
        self.slot = slot;
    }

    /// Three buttons about the chat, and the links off the card: the chat,
    /// its messages, and, for a group, who is in it. Last, the one verb that
    /// ends or begins something, past the links: *leave* on a group or a
    /// channel I am in, *join* on one I merely know of, *delete chat* on a
    /// person I have a conversation with.
    ///
    /// A chat of mine is one with a place in a list — `in_main`, or the
    /// archive. A group the engine only learned of, through a forward or a
    /// mention, has neither, and there is nothing to leave: it offers the
    /// way in instead.
    ///
    /// *leave* wears `v`: `l` is the workspace's own chord (see
    /// [`keys`](crate::shell::keys)), `e` is *members* and `a` is *archive*.
    /// *join* wears `j` and *delete chat* `d`, both free on this bar.
    fn verbs(&self) -> Vec<Verb> {
        let card = self.card();
        let (muted, pinned, archived, group) = card.as_ref().map_or(
            (false, false, false, false),
            |c| (c.muted, c.pinned > 0, c.archived, c.kind == Kind::Group),
        );
        let mut v = vec![
            Verb::run(
                "telegram.mute",
                if muted { "unmute" } else { "mute" },
                Some('m'),
            ),
            Verb::run(
                "telegram.pin",
                if pinned { "unpin" } else { "pin" },
                Some('p'),
            ),
            Verb::run(
                if archived {
                    "telegram.unarchive"
                } else {
                    "telegram.archive"
                },
                if archived { "unarchive" } else { "archive" },
                Some('a'),
            ),
            Verb::go(
                "telegram.chat",
                "chat",
                Some('c'),
                Nav::Open {
                    from: self.slot,
                    id: Chat::id(self.peer),
                    fresh: false,
                },
            ),
            Verb::go(
                "telegram.search",
                "search",
                Some('s'),
                Nav::Open {
                    from: self.slot,
                    id: Messages::in_chat(self.peer),
                    fresh: false,
                },
            ),
        ];
        if group {
            v.push(Verb::go(
                "telegram.members",
                "members",
                Some('e'),
                Nav::Open {
                    from: self.slot,
                    id: Members::id(self.peer),
                    fresh: false,
                },
            ));
        }
        // The one that ends or begins something, where there is a peer to say
        // it of: a person one has a conversation with, or a group and whether
        // one is in it.
        let standing = card
            .as_ref()
            .map(|c| (c.kind == Kind::Person, c.in_main || c.archived));
        match standing {
            Some((false, true)) => v.push(Verb::run("telegram.leave", "leave", Some('v'))),
            Some((false, false)) => v.push(Verb::run("telegram.join", "join", Some('j'))),
            Some((true, true)) => {
                // Both of the client's, each my side only: the lines gone
                // and the chat kept, or the chat gone from the list too.
                v.push(Verb::run("telegram.clear_history", "clear history", Some('r')));
                v.push(Verb::run("telegram.delete", "delete chat", Some('d')));
            }
            _ => {}
        }
        v
    }

    /// The verbs about the chat, done. Each is two things: the request that
    /// tells Telegram, where this build is signed in, and the flip on the
    /// store — written at once, so the bar says *unmute* on this draw rather
    /// than on the engine's answer, which arrives a moment later saying the
    /// same. Where nothing is signed in nothing is written and the toast says
    /// what would have left: the demo world stays what a store with no
    /// account shows.
    ///
    /// *leave* is the exception, and writes either way: the conversation goes
    /// from the store whether or not there was a wire to tell, and the card
    /// closes behind it, there being nothing left to say about a group one is
    /// no longer in. The peer stays, so the close leaves no hole — anything
    /// still pointing at the name finds it. *delete chat* is the same shape
    /// over a person: the same removal, the same close, the peer still known.
    ///
    /// *join* writes nothing at all. What follows a join is a chat with a
    /// place in my list, and the engine says so itself a moment later
    /// (`updateChatAddedToList`); guessing it here would only be a flag to
    /// take back if the join were refused.
    fn run(&mut self, verb: &str, s: &mut Session) {
        let Some(card) = self.card() else { return };
        let peer = self.peer;
        match verb {
            "telegram.mute" => {
                let on = !card.muted;
                let word = if on { "mute" } else { "unmute" };
                if told(s, &sync::set_chat_muted(peer, on), word) {
                    flip(&self.store, move |c| model::set_muted_tx(c, peer, on));
                }
            }
            "telegram.pin" => {
                let on = card.pinned == 0;
                let word = if on { "pin" } else { "unpin" };
                if told(s, &sync::toggle_chat_pinned(peer, on), word) {
                    flip(&self.store, move |c| model::set_pinned_tx(c, peer, on));
                }
            }
            "telegram.archive" | "telegram.unarchive" => {
                let on = verb == "telegram.archive";
                let word = if on { "archive" } else { "unarchive" };
                if told(s, &sync::add_chat_to_list(peer, on), word) {
                    flip(&self.store, move |c| model::set_archived_tx(c, peer, on));
                }
            }
            "telegram.join" => {
                told(s, &sync::join_chat(peer), "join");
            }
            "telegram.clear_history" => {
                told(s, &sync::clear_history(peer), "clear history");
                flip(&self.store, move |c| model::clear_history_tx(c, peer));
            }
            "telegram.leave" | "telegram.delete" => {
                let (request, word) = if verb == "telegram.leave" {
                    (sync::leave_chat(peer), "leave")
                } else {
                    (sync::delete_chat(peer), "delete chat")
                };
                told(s, &request, word);
                flip(&self.store, move |c| model::leave_chat_tx(c, peer));
                s.nav(Nav::Close {
                    slot: self.slot,
                    label: Some(card.name),
                });
            }
            _ => {}
        }
        s.redraw();
    }

    fn as_any(&mut self) -> &mut dyn Any {
        self
    }
}

/// Its factory.
pub struct PeerKind;

impl PanelKind for PeerKind {
    fn tag(&self) -> Tag {
        Peer::TAG
    }

    fn open(&self, id: &PanelId, cx: &mut Opening<'_>) -> Box<dyn Panel> {
        Box::new(Peer {
            peer: Peer::of(id).unwrap_or_default(),
            id: id.clone(),
            store: cx.session().store().clone(),
            slot: 0,
        })
    }
}
