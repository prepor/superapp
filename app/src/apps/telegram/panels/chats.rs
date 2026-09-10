//! The chat list: a rich table of chats, pinned ones first, and the batch
//! verbs over what is marked in it.
//!
//! One instance type for the active chats and the archive: the argument
//! picks the source and the one verb that differs, and nothing else about a
//! list of chats changes with which of the two it is over.
//!
//! It is also the forward picker. Lines selected in a transcript wait in
//! the store's runtime; *forward here* sends them to the chat under the cursor.

use std::any::Any;
use std::rc::Rc;

use kernel::layout::SlotId;
use kernel::nav::Nav;
use kernel::panel::{Opening, Panel, PanelId, PanelKind, Tag, Verb};
use kernel::richtable::{ListState, SqlSource};
use kernel::session::Session;
use kernel::store::Store;

use super::super::model::{self, ChatRow, PeerId, PAGE};
use super::super::{requests, runtime};
use super::Chat;
use super::{flip, told, Contacts, Messages};

/// A chat list: the chats, its cursor, and its marks.
pub struct Chats {
    id: PanelId,
    archive: bool,
    forums: bool,
    store: Rc<Store>,
    slot: SlotId,
    list: ListState<&'static SqlSource<ChatRow, String>>,
}

impl Chats {
    pub const TAG: Tag = Tag("chats");

    /// The active chats.
    #[must_use]
    pub fn id() -> PanelId {
        PanelId::bare(Self::TAG)
    }

    /// The archived ones.
    #[must_use]
    pub fn archive() -> PanelId {
        PanelId::new(Self::TAG, ["archive"])
    }

    pub fn forums() -> PanelId {
        PanelId::new(Self::TAG, ["topics"])
    }

    pub fn empty_line(&self, filter: &str) -> String {
        let runtime = runtime::of(&self.store);
        if let Some(error) = runtime.connection_error() {
            error
        } else if !filter.trim().is_empty() {
            "no chat under this filter".into()
        } else if runtime.list_syncing() {
            if self.forums {
                "loading groups with topics…".into()
            } else {
                "loading chats…".into()
            }
        } else if self.forums {
            "no groups with topics yet".into()
        } else if self.archive {
            "nothing archived".into()
        } else {
            "no chats yet".into()
        }
    }

    /// Whether a `chats` panel is the archive.
    #[must_use]
    pub fn is_archive(id: &PanelId) -> bool {
        id.tag == Self::TAG && id.arg(0) == Some("archive")
    }

    pub fn list_mut(&mut self) -> &mut ListState<&'static SqlSource<ChatRow, String>> {
        &mut self.list
    }

    /// Rows `lo..hi`, as far as the table has them.
    #[cfg(test)]
    #[must_use]
    pub fn rows(&self, lo: usize, hi: usize) -> Vec<ChatRow> {
        self.list.rows(&self.store, lo, hi)
    }

    /// Puts the cursor on row `i` — a click — and answers the preview.
    #[cfg(test)]
    pub fn go(&mut self, i: usize) -> Option<Nav> {
        let store = self.store.clone();
        let row = self.list.set_cursor(&store, i)?;
        Some(Nav::Preview { from: self.slot, id: Self::target(&row) })
    }

    /// Space: the mark on the cursor's row, toggled.
    #[cfg(test)]
    pub fn toggle_mark(&mut self) -> bool {
        let store = self.store.clone();
        self.list.toggle_mark(&store)
    }

    pub fn target(row: &ChatRow) -> PanelId {
        if row.is_forum { super::Topics::id(row.peer) }
        else { Chat::topic(row.peer, row.topic) }
    }
}

impl Panel for Chats {
    fn id(&self) -> &PanelId {
        &self.id
    }

    /// The list's word — and, while the engine is still bringing the list
    /// down page by page, *syncing…* beside it, so a list that is short is
    /// seen to be short for now.
    fn title(&self) -> String {
        let word = if self.forums { "topic groups" } else if self.archive { "archive" } else { "chats" };
        if runtime::of(&self.store).list_syncing() {
            format!("{word} · syncing…")
        } else {
            word.to_string()
        }
    }

    /// Four wide, six tall: a list wants the column.
    fn wish(&self, _cols: usize) -> (u32, u32) {
        (4, 6)
    }

    fn placed(&mut self, slot: SlotId) {
        self.slot = slot;
    }

    /// One link always — the people, which is how a new conversation
    /// starts — then, while lines wait for a chat to go to, the pick that
    /// sends them here; and while there are marks, the batch verbs with
    /// their count and the two verbs about the set itself.
    ///
    /// *mark all* wears `k` because `a` is *archive* and `m` is *mute*;
    /// *forward here* wears `f`, which nothing else on this bar wants.
    /// *clear* wears none, `esc` being the table's own — and it stands for
    /// a waiting forward as well as for a marked set, being the way out of
    /// either.
    fn verbs(&self) -> Vec<Verb> {
        if self.forums {
            return vec![Verb::go("telegram.chats", "chats", Some('c'), Nav::Open {
                from: self.slot, id: Self::id(), fresh: false,
            })];
        }
        let mut v = vec![Verb::go(
            "telegram.new",
            "new message",
            Some('n'),
            Nav::Open {
                from: self.slot,
                id: Contacts::id(),
                fresh: false,
            },
        )];
        let replies = model::reply_count(&self.store);
        v.push(Verb::go(
            "telegram.replies",
            format!("replies & mentions {replies}"),
            Some('s'),
            Nav::Open { from: self.slot, id: Messages::replies(None), fresh: false },
        ));
        v.push(Verb::go("telegram.topics", "topics", None, Nav::Open {
            from: self.slot, id: Self::forums(), fresh: false,
        }));
        let forwarding = runtime::of(&self.store).pending_forward().is_some();
        if forwarding {
            v.push(Verb::run("telegram.forward_here", "forward here", Some('f')));
        }
        let n = self.list.marks().len();
        if n > 0 {
            v.push(Verb::run("telegram.read", format!("read {n}"), Some('r')));
            v.push(Verb::run("telegram.mute", format!("mute {n}"), Some('m')));
            v.push(Verb::run("telegram.pin", format!("pin {n}"), Some('p')));
            if self.archive {
                v.push(Verb::run(
                    "telegram.unarchive",
                    format!("unarchive {n}"),
                    Some('a'),
                ));
            } else {
                v.push(Verb::run("telegram.archive", format!("archive {n}"), Some('a')));
            }
            v.push(Verb::run("telegram.all", "mark all", Some('k')));
        }
        if n > 0 || forwarding {
            v.push(Verb::run("telegram.clear", "clear", None));
        }
        v
    }

    fn run(&mut self, verb: &str, s: &mut Session) {
        let store = self.store.clone();
        match verb {
            "telegram.all" => {
                self.list.mark_all(&store);
                s.redraw();
            }
            // The way out of both states this bar can be in: the marked set,
            // and a forward that has not found its chat. Letting the forward
            // go is not sending it anywhere.
            "telegram.clear" => {
                self.list.clear_marks();
                runtime::of(&self.store).take_forward();
                s.redraw();
            }
            "telegram.forward_here" => self.forward_here(s),
            "telegram.read" | "telegram.mute" | "telegram.pin" | "telegram.archive"
            | "telegram.unarchive" => self.batch(s, verb),
            _ => {}
        }
    }

    fn as_any(&mut self) -> &mut dyn Any {
        self
    }
}

// -- the batch verbs ---------------------------------------------------------

impl Chats {
    /// A batch verb over the marks: one undo node, one request per chat. Acknowledgements
    /// and updates settle each local row, so a failed command keeps its data.
    ///
    /// A set is too many states to toggle one by one, so the bar's word is
    /// what happens: *mute 3* mutes three, and it is the archive's own bar
    /// that says *unarchive*. The set being one gesture, it is one toast
    /// where the build is not signed in — and then nothing is written and the
    /// marks stay, there being nothing done to have finished with.
    fn batch(&mut self, s: &mut Session, verb: &str) {
        let peers: Vec<(PeerId, i64)> = self.list.marks().keys().iter()
            .filter_map(|key| super::super::topics::parse_key(key)).collect();
        if peers.is_empty() {
            return;
        }
        let store = self.store.clone();
        let archiving = verb == "telegram.archive";
        let mut queued = Vec::new();
        let mut local = Vec::new();
        let mut remote = Vec::new();
        let mut requests = Vec::new();
        for &(peer, topic) in &peers {
            if topic != 0 && matches!(verb, "telegram.pin" | "telegram.archive" | "telegram.unarchive") {
                local.push((peer, topic));
                queued.push((peer, topic));
                continue;
            }
            let request = match verb {
                // Without an ordinary line, no receipt can advance this
                // conversation without acknowledging an unread mention.
                "telegram.read" => match model::newest_ordinary_line_in(&store, peer, topic) {
                    Some(last) => requests::view_messages(peer, &[last]),
                    None => continue,
                },
                "telegram.mute" if topic != 0 => requests::set_topic_muted(peer, topic, true),
                "telegram.mute" => requests::set_chat_muted(peer, true),
                "telegram.pin" => requests::toggle_chat_pinned(peer, true),
                _ => requests::add_chat_to_list(peer, archiving),
            };
            let request = if verb == "telegram.read" { requests::in_topic(request, topic) } else { request };
            requests.push(request);
            remote.push((peer, topic));
        }
        let word = verb.rsplit('.').next().unwrap_or(verb);
        let what = if peers.len() == 1 { "chat" } else { "chats" };
        let label = format!("{word} {} {what}", peers.len());
        if !requests.is_empty() {
            match super::super::history::batch(s, &requests, label.clone()) {
                Ok(_) => queued.extend(remote),
                Err(error) => { error.notify(s, &label); return; }
            }
        }
        if queued.is_empty() { return; }
        // Topic pinning and archiving are local preferences. Reads and
        // other server flags settle only when Telegram acknowledges them.
        let topic_column = if verb == "telegram.pin" { "pinned" } else { "archived" };
        let value = i64::from(verb != "telegram.unarchive");
        if !local.is_empty() {
            flip(&store, move |c| {
                for (peer, topic) in local {
                    c.execute(&format!("UPDATE tg_topic SET {topic_column} = ?3 WHERE chat = ?1 AND id = ?2"),
                        rusqlite::params![peer, topic, value])?;
                }
                Ok(())
            });
        }
        // A skipped conversation stays unread and marked for a later retry.
        for (peer, topic) in queued {
            self.list.marks_mut().remove(&format!("{peer}:{topic}"));
        }
        s.redraw();
    }

    /// The pick a waiting forward was opened for: the lines go to the chat
    /// under the cursor, in one `forwardMessages` per source chat. Telegram
    /// makes the copies and each comes back as its own `updateNewMessage`,
    /// so nothing local is written here.
    ///
    /// Accepted groups leave the waiting selection; refused live groups stay
    /// for retry. Offline selections end after reporting what would be sent.
    fn forward_here(&mut self, s: &mut Session) {
        let Some(f) = runtime::of(&self.store).pending_forward() else {
            return;
        };
        let Some((peer, topic)) = self.list.cursor_key().and_then(|k| super::super::topics::parse_key(k)) else {
            s.notify("put the cursor on a chat to forward to", true);
            return;
        };
        if model::peer(&self.store, peer).is_some_and(|c| c.blocked) {
            s.notify("unblock this user before sending a message", false);
            return;
        }
        let name =
            super::super::topics::card(&self.store, peer, topic).map_or_else(|| "the chat".to_string(), |c| c.name);
        let mut queued = 0;
        let mut remaining = Vec::new();
        let mut groups: Vec<_> = model::message_groups(f.messages.iter().copied()).into_iter().collect();
        groups.sort_by_key(|(chat, _)| f.messages.iter().position(|(source, _)| source == chat));
        for (from, ids) in groups {
            let n = ids.len();
            let what = if n == 1 { "line" } else { "lines" };
            let went = told(s, &requests::in_topic(requests::forward_messages(peer, from, &ids), topic),
                &format!("forward {n} {what} to {name}"));
            if went { queued += n; }
            else if super::live(&self.store) { remaining.extend(ids.into_iter().map(|id| (from, id))); }
        }
        if remaining.is_empty() { runtime::of(&self.store).take_forward(); }
        else { runtime::of(&self.store).carry_forward_messages(remaining); }
        if queued > 0 {
            let what = if queued == 1 { "line" } else { "lines" };
            s.notify(format!("forwarding {queued} {what} to {name}…"), false);
        }
        s.redraw();
    }
}

/// The factory.
pub struct ChatsKind;

impl PanelKind for ChatsKind {
    fn tag(&self) -> Tag {
        Chats::TAG
    }

    fn open(&self, id: &PanelId, cx: &mut Opening<'_>) -> Box<dyn Panel> {
        let archive = Chats::is_archive(id);
        let forums = id.arg(0) == Some("topics");
        Box::new(Chats {
            id: id.clone(),
            archive,
            forums,
            store: cx.session().store().clone(),
            slot: 0,
            list: ListState::new(if forums { &model::FORUMS } else { model::chats(archive) }, PAGE),
        })
    }
}
