//! A peer's card: who or what a chat is with, and the ways off it.
//!
//! Facts come from cached queries; a destructive action's confirmation stays
//! on this panel until it is confirmed or cancelled.

use std::any::Any;
use std::rc::Rc;

use kernel::layout::SlotId;
use kernel::nav::Nav;
use kernel::panel::{Opening, Panel, PanelId, PanelKind, Tag, Verb};
use kernel::session::Session;
use kernel::store::Store;

use super::super::model::{self, PeerCard, PeerId, PeerKind as Kind};
use super::super::requests::{self, PeerAction};
use super::super::runtime;
use super::{flip, told, Chat, Members, Messages};

/// A peer's card.
pub struct Peer {
    id: PanelId,
    peer: PeerId,
    store: Rc<Store>,
    slot: SlotId,
    confirmation: Option<PeerAction>,
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

    /// The consequence being confirmed, or progress while Telegram answers.
    pub fn prompt(&self) -> Option<String> {
        if runtime::of(&self.store).peer_action_pending(self.peer) {
            return Some("waiting for Telegram…".to_string());
        }
        let card = self.card()?;
        Some(match self.confirmation? {
            PeerAction::Block => format!("Block {}? They won't be able to message you or see your status and photo.", card.name),
            PeerAction::DeleteContact => format!("Delete {} from your contacts? Your conversation will stay.", card.name),
            PeerAction::DeleteChat => format!("Delete your chat with {}? This removes your messages and chat from your list. Their copy will stay.", card.name),
            PeerAction::Unblock => return None,
        })
    }
}

fn allowed(card: &PeerCard, action: PeerAction) -> bool {
    if card.kind != Kind::Person {
        return false;
    }
    match action {
        PeerAction::Block => !card.is_self && !card.blocked,
        PeerAction::Unblock => !card.is_self && card.blocked,
        PeerAction::DeleteContact => !card.is_self && card.is_contact,
        PeerAction::DeleteChat => card.in_main || card.archived,
    }
}

/// Used by the profile and the blocked conversation's unblock button.
pub(super) fn perform(s: &mut Session, peer: PeerId, action: PeerAction) {
    if !s.writable() {
        s.notify("read-only — acquire the lease to write", true);
        return;
    }
    let Some(card) = model::peer(s.store(), peer) else { return };
    let runtime = runtime::of(s.store());
    if !allowed(&card, action) || runtime.peer_action_pending(peer) {
        return;
    }
    if !runtime.send_peer_action(peer, action) {
        s.notify(super::super::draft_toast(action.word()), false);
    }
    s.redraw();
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

    /// Chat flags and navigation, followed by profile actions. Blocking and
    /// deleting ask for confirmation here, as in Telegram's user info view.
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
        if runtime::of(&self.store).peer_action_pending(self.peer) {
            return Vec::new();
        }
        if let Some(action) = self.confirmation {
            return vec![
                Verb::run("telegram.confirm", format!("confirm {}", action.word()), Some('f')),
                Verb::run("telegram.cancel", "cancel", Some('c')),
            ];
        }
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
                    id: if card.as_ref().is_some_and(|c| c.is_forum) {
                        super::Topics::id(self.peer)
                    } else {
                        Chat::id(self.peer)
                    },
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
        if card.as_ref().is_some_and(|c| c.is_forum) {
            v.push(Verb::go("telegram.topics", "topics", None, Nav::Open {
                from: self.slot, id: super::Topics::id(self.peer), fresh: false,
            }));
        }
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
        if let Some(c) = card.as_ref().filter(|c| c.kind == Kind::Person && !c.is_self) {
            v.push(Verb::run(
                if c.blocked { "telegram.unblock" } else { "telegram.block" },
                if c.blocked { "unblock user" } else { "block user" },
                Some('b'),
            ));
            if c.is_contact {
                v.push(Verb::run("telegram.delete_contact", "delete contact", Some('e')));
            }
        }
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
    /// still pointing at the name finds it. Profile actions wait for a server
    /// acknowledgement; a failed request leaves the local data intact.
    ///
    /// *join* writes nothing at all. What follows a join is a chat with a
    /// place in my list, and the engine says so itself a moment later
    /// (`updateChatAddedToList`); guessing it here would only be a flag to
    /// take back if the join were refused.
    fn run(&mut self, verb: &str, s: &mut Session) {
        let Some(card) = self.card() else { return };
        let peer = self.peer;
        if runtime::of(&self.store).peer_action_pending(peer) {
            return;
        }
        if verb == "telegram.cancel" {
            self.confirmation = None;
            s.redraw();
            return;
        }
        if verb == "telegram.confirm" {
            if let Some(action) = self.confirmation.take() {
                perform(s, peer, action);
            }
            s.redraw();
            return;
        }
        if self.confirmation.is_some() {
            return;
        }
        match verb {
            "telegram.block" | "telegram.delete_contact" | "telegram.delete" => {
                let action = match verb {
                    "telegram.block" => PeerAction::Block,
                    "telegram.delete_contact" => PeerAction::DeleteContact,
                    _ => PeerAction::DeleteChat,
                };
                if allowed(&card, action) {
                    self.confirmation = Some(action);
                }
            }
            "telegram.unblock" => perform(s, peer, PeerAction::Unblock),
            "telegram.mute" => {
                let on = !card.muted;
                let word = if on { "mute" } else { "unmute" };
                told(s, &requests::set_chat_muted(peer, on), word);
            }
            "telegram.pin" => {
                let on = card.pinned == 0;
                let word = if on { "pin" } else { "unpin" };
                told(s, &requests::toggle_chat_pinned(peer, on), word);
            }
            "telegram.archive" | "telegram.unarchive" => {
                let on = verb == "telegram.archive";
                let word = if on { "archive" } else { "unarchive" };
                told(s, &requests::add_chat_to_list(peer, on), word);
            }
            "telegram.join" => {
                told(s, &requests::join_chat(peer), "join");
            }
            "telegram.clear_history" => {
                told(s, &requests::clear_history(peer), "clear history");
                if !super::live(&self.store) {
                    flip(&self.store, move |c| model::clear_history_tx(c, peer));
                }
            }
            "telegram.leave" => {
                told(s, &requests::leave_chat(peer), "leave");
                if super::live(&self.store) {
                    s.redraw();
                    return;
                }
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
        let peer = Peer::of(id).unwrap_or_default();
        let store = cx.session().store().clone();
        if model::peer(&store, peer).is_some_and(|c| c.kind == Kind::Person && !c.is_self) {
            let _ = super::wire(&store, &requests::get_user_full_info(peer));
        }
        Box::new(Peer {
            peer,
            id: id.clone(),
            store,
            slot: 0,
            confirmation: None,
        })
    }
}
