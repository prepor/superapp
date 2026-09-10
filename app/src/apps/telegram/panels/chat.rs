//! One conversation: the transcript with its cursor and marks, the reply
//! line, the draft in the composer or the edit under way in it, what the
//! composer carries, and the one line playing.
//!
//! Opening with an ordinary cached line marks the chat read on the undo node.
//! The live read receipt is separate: undo restores only the local count.
//! The panel remembers where reading started for its unread divider.
//!
//! The bar offers reply, edit and delete, and opens attachment, line and
//! peer cards. Live edits and deletes go to the worker; offline fixture
//! changes use local undoable intents.

use std::any::Any;
use std::collections::BTreeSet;
use std::rc::Rc;

use kernel::caps::Clip;
use kernel::effect::World;
use kernel::history::Intent;
use kernel::layout::SlotId;
use kernel::nav::Nav;
use kernel::panel::{Opening, Panel, PanelId, PanelKind, Tag, Verb};
use kernel::session::{Instance, Session};
use kernel::store::Store;

use crate::shell::widgets::media::PlayerState;

use super::super::draft_toast;
use super::super::model::{
    self, day_caption, same_day, Carried, Msg, MsgId, MsgKey, PeerCard, PeerId, RUN_GAP,
};
use super::super::{downloads, requests, runtime, verbs};
use super::reactions::{self, Reactions};
use super::playback::Playback;
use super::{wire, Attach, Chats, Line, Peer};

/// An edit under way in the composer: which of my lines, what it said, and
/// what the field says now.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Editing {
    pub msg: MsgKey,
    pub original: String,
    pub was_edited: bool,
    pub text: String,
}

/// One row of the transcript, as the widget draws it.
#[derive(Debug, Clone, PartialEq)]
pub enum Row {
    /// A day's caption over a hairline.
    Day(String),
    /// The line above the first message not read when the panel opened.
    Unread,
    /// A line about the chat, muted.
    Service(Msg),
    /// A message. `run` marks the second and later of a writer's run: no
    /// header, the way the client draws no second bubble tail.
    Message { msg: Msg, run: bool },
}

impl Row {
    /// The message a row is, where it is one.
    #[must_use]
    pub fn msg(&self) -> Option<&Msg> {
        match self {
            Row::Message { msg, .. } | Row::Service(msg) => Some(msg),
            Row::Day(_) | Row::Unread => None,
        }
    }
}

/// A chat panel.
pub struct Chat {
    id: PanelId,
    peer: PeerId,
    topic: i64,
    store: Rc<Store>,
    slot: SlotId,
    cursor: Option<MsgKey>,
    /// A line the transcript is asked to bring on screen — a reply's
    /// original or the way back — taken once by the widget.
    follow_wish: Option<MsgKey>,
    /// Replies left by successful jumps to their originals, newest last.
    reply_back: Vec<MsgKey>,
    marks: BTreeSet<MsgKey>,
    reply_to: Option<MsgKey>,
    /// The composer's text. Written to the chat's row behind it, so the list
    /// shows *draft: …*.
    draft: String,
    /// The draft as Telegram last heard it — what the row held when the panel
    /// opened, the server having put it there. A chat left with nothing typed
    /// in it since says nothing.
    sent_draft: String,
    /// The draft as the row last said it, so far as this panel has looked:
    /// what tells the composer's own words from a change made *under* it, by
    /// another device or by a send the engine refused.
    seen_draft: String,
    /// Keystrokes stay in memory until a brief pause or the chat is left.
    draft_pending: bool,
    draft_write: Option<kernel::store::PendingWrite<()>>,
    /// An edit under way: the field shows its text instead of the draft,
    /// which waits.
    editing: Option<Editing>,
    /// The first message not read when the panel opened, where there was
    /// one: the unread line stays above it for the panel's life.
    #[cfg(test)]
    first_unread: Option<MsgId>,
    /// Retry unacknowledged views after a short delay; a queued command is
    /// not enough to dismiss a server notification.
    viewed_messages: std::collections::BTreeMap<MsgKey, f64>,
    /// What the composer will send with the text, in the order it will go.
    /// Edited from the attach panel, through the join.
    carrying: Vec<Carried>,
    dropped_files: Vec<tokio::sync::oneshot::Receiver<Vec<Result<String, String>>>>,
    /// A reply asked for the caret: the widget takes this once and puts the
    /// keyboard in the field, whatever had it.
    wants_field: bool,
    /// The one line playing, or paused: a chat plays one thing at a time.
    player: Option<Playback>,
    reactions: Reactions,
    transcript: super::super::transcript::Transcript,
}

impl Chat {
    pub const TAG: Tag = model::CHAT_TAG;

    /// The identity of one chat.
    #[must_use]
    pub fn id(peer: PeerId) -> PanelId {
        model::chat_id(peer, None)
    }

    /// The same, opened at a message.
    #[must_use]
    pub fn at(peer: PeerId, msg: MsgId) -> PanelId {
        model::chat_id(peer, Some(msg))
    }

    pub fn topic(peer: PeerId, topic: i64) -> PanelId {
        if topic == 0 { return Self::id(peer); }
        PanelId::new(Self::TAG, [peer.to_string(), "topic".into(), topic.to_string()])
    }

    pub fn topic_at(peer: PeerId, topic: i64, msg: MsgId) -> PanelId {
        if topic == 0 { return Self::at(peer, msg); }
        PanelId::new(Self::TAG, [peer.to_string(), "topic".into(), topic.to_string(), msg.to_string()])
    }

    pub fn topic_of(id: &PanelId) -> i64 {
        if id.tag != Self::TAG || id.arg(1) != Some("topic") { return 0; }
        id.arg(2).and_then(|s| s.parse().ok()).filter(|id| *id > 0).unwrap_or(0)
    }

    /// The peer a `chat` panel names; `None` for any other tag.
    #[must_use]
    pub fn of(id: &PanelId) -> Option<PeerId> {
        (id.tag == Self::TAG)
            .then(|| id.arg(0)?.parse().ok())
            .flatten()
    }

    /// The message it was opened at, if any.
    #[must_use]
    pub fn msg_of(id: &PanelId) -> Option<MsgId> {
        (id.tag == Self::TAG)
            .then(|| id.arg(if Self::topic_of(id) == 0 { 1 } else { 3 })?.parse().ok())
            .flatten()
    }

    #[must_use]
    pub fn peer(&self) -> PeerId {
        self.peer
    }

    fn blocked(&self) -> bool {
        model::peer(&self.store, self.peer).is_some_and(|c| c.blocked)
    }

    pub fn topic_id(&self) -> i64 { self.topic }

    fn request(&self, request: String) -> String {
        requests::reply_in_chat(requests::in_topic(request, self.topic), self.peer, self.reply_to)
    }

    /// Whether the transcript is still being filled from the wire — the
    /// history walk a chat starts as it opens, until its last page lands.
    #[must_use]
    pub fn loading(&self) -> bool {
        !self.transcript_ready() || runtime::of(&self.store).loading_in(self.peer, self.topic)
            || (self.topic == 0 && super::super::upgrades::original(&self.store, self.peer)
                .is_some_and(|old| runtime::of(&self.store).loading_in(old, 0)))
    }

    pub fn transcript_ready(&self) -> bool { self.transcript.get(&self.store).ready }

    /// Who the chat is with, and its flags. `None` for a peer the store does
    /// not have.
    ///
    /// Read on every draw, which is what makes it the place the draft is
    /// reconciled (`take_draft`, below): the row's draft moves under an open
    /// panel — a line half-typed on the phone arriving as
    /// `updateChatDraftMessage`, or the words of a send the engine refused
    /// put back — and a composer that read it once at construction would go
    /// on showing the stale string.
    #[must_use]
    pub fn card(&mut self) -> Option<PeerCard> {
        let card = super::super::topics::card(&self.store, self.peer, self.topic);
        if let Some(c) = &card {
            self.take_draft(c.draft.clone().unwrap_or_default());
        }
        card
    }

    /// The row's draft, taken into the composer where the composer has
    /// nothing of its own to lose: it still holds what the row last said, or
    /// nothing at all. Where somebody has typed since, the local words stand
    /// — what one is in the middle of writing is not another device's to
    /// overwrite, and the leaving will tell the server about it.
    ///
    /// The server is where this draft came from, so it is what Telegram has
    /// last heard too, and [`flush_draft`](Chat::flush_draft) has nothing to
    /// say about it.
    fn take_draft(&mut self, row: String) {
        if row == self.seen_draft {
            return;
        }
        // Untouched means the server already has what the composer holds —
        // nothing typed since the last flush — or nothing at all. A pending
        // save also protects a newly emptied field. Saving our own text to
        // the row does not make it a remote draft (review, 2026-09-07: a
        // phone's draft overwrote unsent words).
        let untouched = !self.draft_pending && self.draft_write.is_none()
            && (self.draft.is_empty() || self.draft == self.sent_draft);
        self.seen_draft.clone_from(&row);
        if untouched {
            self.sent_draft.clone_from(&row);
            self.draft = row;
        }
    }

    /// The lines, oldest first.
    #[must_use]
    pub fn history(&self) -> std::sync::Arc<Vec<Msg>> {
        self.transcript.get(&self.store).history.clone()
    }

    /// The widget supplies messages seen in a visible foreground transcript.
    /// Opening only read the cached history; newer visible lines need receipts
    /// too. Mentions keep their own acknowledgment behind the inbox cursor.
    /// Returns whether any visible messages still need a live acknowledgment,
    /// including attempts waiting for the retry delay.
    pub fn view_messages(&mut self, visible: &[MsgKey], now: f64) -> bool {
        if visible.is_empty() { return false; }
        let snapshot = self.transcript.get(&self.store);
        let keys = visible.iter().filter_map(|&key| snapshot.message(key))
            .filter(|m| m.id > 0 && !m.service && !matches!(m.state.as_deref(), Some("sending" | "failed")))
            .map(Msg::key);
        let mut pending = false;
        for (chat, mut ids) in model::message_groups(keys) {
            let topic = if chat == self.peer { self.topic } else { 0 };
            let last_read = super::super::topics::card(&self.store, chat, topic)
                .and_then(|card| card.last_read).unwrap_or(0);
            // The inbox cursor follows incoming messages; our own newer
            // lines never await a read acknowledgment.
            ids.retain(|&id| snapshot.message((chat, id))
                .is_some_and(|m| !m.out && (m.unread_mention || id > last_read)));
            if ids.is_empty() { continue; }
            let offline = !super::live(&self.store)
                && super::super::schema::session(self.store.conn()).state == "closed";
            pending |= !offline;
            ids.retain(|id| self.viewed_messages.get(&(chat, *id))
                .is_none_or(|at| now - at >= Self::VIEW_RETRY_DELAY));
            let Some(&through) = ids.last() else { continue; };
            if wire(&self.store, &requests::in_topic(requests::view_messages(chat, &ids), topic)) {
                for id in ids { self.viewed_messages.insert((chat, id), now); }
            } else if offline {
                super::flip(&self.store, move |c| {
                    super::super::topics::read_tx(c, chat, topic, through)?;
                    super::super::project::read_mentions(c, chat, &ids)
                });
            }
        }
        pending
    }

    pub const VIEW_RETRY_DELAY: f64 = 5.0;

    /// Where the reading started, if anything was unread.
    #[cfg(test)]
    #[must_use]
    pub fn first_unread(&self) -> Option<MsgId> {
        self.first_unread
    }

    /// The transcript: the lines with the day captions, the unread line and
    /// the runs worked out. `now` is what the captions are spelled against.
    #[cfg(test)]
    #[must_use]
    pub fn rows(&self, now: f64) -> std::sync::Arc<Vec<Row>> {
        self.snapshot(now).rows.clone()
    }

    /// The draw and its message lookups must use the same immutable reading,
    /// even when a background refresh finishes during the draw.
    pub fn snapshot(&self, now: f64) -> std::sync::Arc<super::super::transcript::Snapshot> {
        self.transcript.at(now);
        self.transcript.get(&self.store)
    }

    // -- the cursor and the marks -----------------------------------------------

    #[must_use]
    pub fn cursor(&self) -> Option<MsgKey> {
        self.cursor
    }

    pub fn set_cursor(&mut self, id: MsgKey) {
        if self.cursor != Some(id) {
            self.cancel_reactions();
        }
        self.cursor = Some(id);
    }

    /// Escape closes the reaction picker before touching the draft or marks.
    pub fn cancel_reactions(&mut self) -> bool {
        self.reactions.cancel()
    }

    pub fn poll(&mut self, s: &mut Session) -> bool {
        let mut changed = self.reactions.poll(s);
        if let Some(result) = self.draft_write.as_mut().and_then(|write| write.poll(&self.store)) {
            self.draft_write = None;
            changed = true;
            if let Err(error) = result {
                self.draft_pending = true;
                s.notify(format!("saving draft: {error}"), true);
            }
        }
        let mut results = Vec::new();
        self.dropped_files.retain_mut(|pending| match pending.try_recv() {
            Ok(files) => { results.extend(files); changed = true; false }
            Err(tokio::sync::oneshot::error::TryRecvError::Empty) => true,
            Err(tokio::sync::oneshot::error::TryRecvError::Closed) => {
                results.push(Err("Attachment validation stopped before completing".into()));
                changed = true;
                false
            }
        });
        if !results.is_empty() { self.attach_dropped_files(s, results); }
        changed
    }

    /// Steps the cursor over the messages, `d` rows: from nothing, either
    /// way lands on the newest line, which is where a chat is read from.
    /// Answers what it landed on.
    pub fn walk(&mut self, d: isize) -> Option<MsgKey> {
        let ids: Vec<MsgKey> = self
            .history()
            .iter()
            .filter(|m| !m.service)
            .map(|m| m.key())
            .collect();
        if ids.is_empty() {
            return None;
        }
        let at = match self.cursor.and_then(|c| ids.iter().position(|i| *i == c)) {
            Some(i) => (i as isize + d).clamp(0, ids.len() as isize - 1) as usize,
            None => ids.len() - 1,
        };
        self.set_cursor(ids[at]);
        self.cursor
    }

    #[must_use]
    pub fn marks(&self) -> &BTreeSet<MsgKey> {
        &self.marks
    }

    /// Space: the mark on the cursor's line, toggled.
    pub fn toggle_mark(&mut self) {
        self.cancel_reactions();
        let Some(c) = self.cursor else { return };
        if !self.marks.remove(&c) {
            self.marks.insert(c);
        }
    }

    /// Shift with an arrow: the line left and the line landed on, both
    /// marked.
    pub fn mark_range(&mut self, d: isize) {
        self.cancel_reactions();
        if let Some(c) = self.cursor {
            self.marks.insert(c);
        }
        if let Some(c) = self.walk(d) {
            self.marks.insert(c);
        }
    }

    pub fn clear_marks(&mut self) {
        self.marks.clear();
    }

    // -- the reply line and the draft -------------------------------------------

    #[must_use]
    pub fn reply_key(&self) -> Option<MsgKey> { self.reply_to }

    pub fn reply_to(&self) -> Option<MsgId> {
        self.reply_to.map(|(_, id)| id)
    }

    /// Replies to a line — the bar's verb over the cursor, or the card of
    /// one asking this of the chat it hangs under. A reply is written, so
    /// the caret goes to the field with it. A blocked peer refuses the reply.
    pub fn reply(&mut self, msg: MsgKey) -> bool {
        if self.blocked() {
            return false;
        }
        self.reply_to = Some(msg);
        self.wants_field = true;
        true
    }

    // -- editing ------------------------------------------------------------------------

    #[must_use]
    pub fn editing(&self) -> Option<&Editing> {
        self.editing.as_ref()
    }

    /// Starts editing one of my lines: the field takes its text, the reply
    /// line goes, and the caret follows. Answers whether it is a line of
    /// mine and the peer is not blocked.
    pub fn edit(&mut self, msg: MsgKey) -> bool {
        if self.blocked() {
            return false;
        }
        let hist = self.history();
        let Some(m) = hist.iter().find(|m| m.key() == msg && m.out && !m.service) else {
            return false;
        };
        self.editing = Some(Editing {
            msg,
            original: m.text.clone(),
            was_edited: m.edited,
            text: m.text.clone(),
        });
        self.reply_to = None;
        self.wants_field = true;
        true
    }

    /// Lets the edit go; the draft comes back into the field.
    pub fn cancel_edit(&mut self) {
        self.editing = None;
    }

    /// What the field shows: the edit's text while one is under way, the
    /// draft otherwise.
    #[must_use]
    pub fn field_text(&self) -> &str {
        match &self.editing {
            Some(e) => &e.text,
            None => &self.draft,
        }
    }

    /// The field changes immediately. Persist after a pause, so a keystroke
    /// never waits behind the database writer or invalidates the chat list.
    pub fn typed(&mut self, text: &str) {
        match &mut self.editing {
            Some(e) => e.text = text.to_string(),
            None if self.draft != text => {
                self.draft = text.to_string();
                self.draft_pending = true;
            }
            None => {}
        }
    }

    pub fn save_pending_draft(&mut self) {
        if self.draft_pending {
            self.set_draft(&self.draft.clone());
        }
    }

    /// The line above the composer: *editing: …* while an edit is under
    /// way, *reply to …* while replying. Blocking hides it with the composer,
    /// keeping the pending reply or edit for when the user is unblocked.
    #[must_use]
    pub fn above_line(&self, now: f64) -> Option<String> {
        if self.blocked() {
            return None;
        }
        match &self.editing {
            Some(e) => Some(format!("editing: {}", model::one_line(&e.original))),
            None => self.reply_line(now),
        }
    }

    // -- deleting -----------------------------------------------------------------------

    /// Records deletion; live undo resends saved copies of supported outgoing
    /// messages. Only the offline fixture restores the original identities.
    pub fn delete(&mut self, s: &mut Session, ids: Vec<MsgKey>) {
        if ids.is_empty() {
            return;
        }
        for (chat, messages) in model::message_groups(ids) {
            let keys: Vec<_> = messages.iter().map(|&id| (chat, id)).collect();
            if super::live(&self.store) {
                let owner = s.panel(self.slot).map(|panel| (panel, self.id.clone()));
                Self::delete_live(s, chat, messages, owner);
            } else {
                self.lines_gone(&keys);
                verbs::delete_lines(s, chat, messages);
            }
        }
        s.redraw();
    }

    pub(super) fn delete_live(s: &mut Session, chat: PeerId, messages: Vec<MsgId>,
        owner: Option<(Instance, PanelId)>) {
        let keys = messages.iter().map(|&msg| (chat, msg)).collect::<Vec<_>>();
        super::super::history::delete(s, chat, messages, move |s, result| {
            if let Err(error) = result {
                error.notify(s, "delete");
                s.redraw();
                return;
            }
            // Fixtures can prepare immediately while the original panel is
            // still borrowed by its verb. All completions land between events.
            s.after_event(move |s| {
                if let Some((owner, id)) = owner {
                    let mut panel = owner.borrow_mut();
                    if let Some(chat) = panel.as_any().downcast_mut::<Chat>() {
                        if chat.id == id && s.panel(chat.slot).is_some_and(|current| Rc::ptr_eq(&current, &owner)) {
                            chat.lines_gone(&keys);
                        }
                    }
                }
                s.redraw();
            });
        });
    }

    /// Lines that are going: the cursor steps to the line before them — or
    /// after, at the top — the marks let go of them, and a reply to one or
    /// an edit of one is dropped. Called before the store hears of it, so
    /// the neighbours are still in the history.
    pub fn lines_gone(&mut self, ids: &[MsgKey]) {
        let gone = ids.iter().copied().collect::<BTreeSet<_>>();
        if let Some(c) = self.cursor.filter(|c| gone.contains(c)) {
            let hist = self.history();
            let stays = |m: &&Msg| !m.service && !gone.contains(&m.key());
            self.cursor = hist.iter().position(|m| m.key() == c).and_then(|i| {
                hist[..i]
                    .iter()
                    .rev()
                    .find(stays)
                    .or_else(|| hist[i + 1..].iter().find(stays))
                    .map(|m| m.key())
            });
        }
        for id in ids {
            self.marks.remove(id);
        }
        self.reply_back.retain(|id| !gone.contains(id));
        if self.editing.as_ref().is_some_and(|e| gone.contains(&e.msg)) {
            self.editing = None;
        }
        if self.reply_to.is_some_and(|r| gone.contains(&r)) {
            self.reply_to = None;
        }
    }

    /// Whether a reply, an edit or a press on the composer asked for the
    /// caret since the last look. Answered once: the widget that reads it
    /// moves the keyboard.
    pub fn take_field_wish(&mut self) -> bool {
        std::mem::take(&mut self.wants_field)
    }

    /// The line a verb asked the transcript to bring on screen, if one did
    /// since the last look. Answered once: the widget that reads it scrolls.
    pub fn take_follow_wish(&mut self) -> Option<MsgKey> {
        self.follow_wish.take()
    }

    /// Puts the cursor on the line the cursor's line answers — a reply's
    /// original — and asks the transcript to bring it on screen. An original
    /// the window does not hold (older than the ten thousand kept, or not
    /// yet backfilled) is said so rather than silently not jumped to. Only
    /// a successful jump remembers the reply for the way back.
    pub fn jump_to_original(&mut self, s: &mut Session) {
        let hist = self.history();
        let Some(reply) = self
            .cursor
            .and_then(|c| hist.iter().find(|m| m.key() == c))
        else {
            return;
        };
        let Some(target) = reply.reply_key().filter(|id| *id != reply.key()) else {
            return;
        };
        if hist.iter().any(|m| m.key() == target) {
            if self.reply_back.last() != Some(&reply.key()) {
                self.reply_back.push(reply.key());
            }
            self.cursor = Some(target);
            self.follow_wish = Some(target);
        } else {
            s.notify("the line it answers is not loaded".to_string(), false);
        }
        s.redraw();
    }

    /// Returns through the replies whose originals were followed, one at
    /// a time. Server deletions or retention may have removed a saved line
    /// without telling this panel, so skip those on the way back.
    fn jump_back(&mut self, s: &mut Session) {
        if self.reply_back.is_empty() {
            return;
        }
        let hist = self.history();
        while let Some(target) = self.reply_back.pop() {
            if hist.iter().any(|m| m.key() == target) {
                self.cursor = Some(target);
                self.follow_wish = Some(target);
                s.redraw();
                return;
            }
        }
        s.notify("the reply is no longer loaded", false);
    }

    /// What the line above the composer says while replying: *reply to
    /// Vera: the line*, shortened to one line.
    #[must_use]
    pub fn reply_line(&self, now: f64) -> Option<String> {
        let id = self.reply_to?;
        let snapshot = self.transcript.get(&self.store);
        let m = snapshot.message(id)?;
        Some(format!(
            "reply to {}: {}",
            m.writer(),
            model::media_or_text(m.media.as_ref(), &m.text, now)
        ))
    }

    pub fn cancel_reply(&mut self) {
        self.reply_to = None;
    }

    /// The composer's text, the draft alone: what the chat's row keeps.
    #[cfg(test)]
    #[must_use]
    pub fn draft(&self) -> &str {
        &self.draft
    }

    /// The composer changed: the row follows, straight through the store
    /// rather than as an action — a draft is not something one undoes.
    pub fn set_draft(&mut self, text: &str) {
        if let Err(e) = self.save_draft(text) {
            // A failed persistence write must not erase what was typed.
            self.draft = text.to_string();
            self.draft_pending = true;
            runtime::of(&self.store)
                .operations
                .report(&self.store, "saving draft", &e);
        }
    }

    fn save_draft(&mut self, text: &str) -> Result<(), String> {
        if !self.draft_pending && self.draft == text && self.seen_draft == text {
            return Ok(());
        }
        let (peer, topic, d) = (self.peer, self.topic, text.to_string());
        let write = move |c: &rusqlite::Transaction<'_>| super::super::topics::draft_tx(c, peer, topic, &d);
        if self.store.ui_attached() {
            let pending = self.store.submit_write(write).map_err(|e| e.to_string())?;
            if let Some(previous) = self.draft_write.replace(pending) {
                runtime::of(&self.store).track_write(previous, "saving draft");
            }
        } else {
            self.store.write(write).map_err(|e| e.to_string())?;
            self.seen_draft = text.to_string();
        }
        self.draft = text.to_string();
        self.draft_pending = false;
        Ok(())
    }

    /// An agent's explicit draft replacement uses the composer's own write.
    /// Unlike a remote update, it replaces local text after the tool checks
    /// for unsent work. Each open copy is updated, so none can restore stale
    /// words or attachments on its next focus or send.
    pub fn stage_draft(&mut self, text: &str, reply_to: Option<MsgId>, files: &[Carried], focus: bool) -> Result<(), String> {
        self.save_draft(text)?;
        self.reply_to = reply_to.map(|id| (self.peer, id));
        self.carrying = files.to_vec();
        self.wants_field = focus;
        Ok(())
    }

    /// Another open copy sent this exact draft. Its send already cleared
    /// the row; forget our copy without a second write or a focus change.
    fn forget_sent_draft(&mut self, text: &str, reply_to: Option<MsgKey>, files: &[Carried]) {
        if self.editing.is_none() && self.carrying == files
            && self.draft == text && self.reply_to == reply_to
        {
            self.draft.clear();
            self.sent_draft.clear();
            self.seen_draft.clear();
            self.draft_pending = false;
            self.reply_to = None;
            self.carrying.clear();
        }
    }

    // -- carrying ---------------------------------------------------------------------

    /// What the composer will send with the text, in the order it will go.
    #[must_use]
    pub fn carrying(&self) -> &[Carried] {
        &self.carrying
    }

    /// Takes paths into what the composer carries — what the files app
    /// holds, by `add` on the attach panel. A path already carried is
    /// passed over: it was not this add's to add. Answers how many came.
    pub fn carry(&mut self, paths: &[String]) -> usize {
        let mut added = 0;
        for p in paths {
            if !self.carrying.iter().any(|c| &c.path == p) {
                self.carrying.push(Carried { path: p.clone() });
                added += 1;
            }
        }
        added
    }

    /// A desktop drop stages readable, regular files for review in the composer.
    pub fn drop_files(&mut self, s: &mut Session, paths: &[String]) {
        if self.editing.is_some() || self.card().is_some_and(|c| !c.can_post()) {
            s.notify(
                "files can only be attached to a new message in a chat you can write to",
                true,
            );
            return;
        }
        let paths = paths.to_vec();
        let (done, pending) = tokio::sync::oneshot::channel();
        self.dropped_files.push(pending);
        kernel::runtime::spawn_blocking(move || {
            if done.is_closed() { return; }
            let files = paths.into_iter().map(|path| {
                let disk = kernel::caps::real_path(&path);
                match std::fs::metadata(&disk) {
                    Ok(m) if m.is_file() => Ok(
                        std::fs::canonicalize(&disk)
                            .unwrap_or(disk)
                            .to_string_lossy()
                            .into_owned(),
                    ),
                    result => {
                        Err(match result {
                            Ok(_) => format!("{path} is not a regular file"),
                            Err(e) => format!("Cannot attach {path}: {e}"),
                        })
                    }
                }
            }).collect();
            if done.send(files).is_ok() { makepad_widgets::SignalToUI::set_ui_signal(); }
        });
    }

    fn attach_dropped_files(&mut self, s: &mut Session, files: Vec<Result<String, String>>) {
        // Validation can finish after an edit begins or permissions change.
        if self.editing.is_some() || self.card().is_some_and(|c| !c.can_post()) {
            s.notify("files can only be attached to a new message in a chat you can write to", true);
            return;
        }
        let valid: Vec<_> = files.into_iter().filter_map(|file| match file {
            Ok(path) => Some(path),
            Err(error) => {
                runtime::of(&self.store).operations.report(&self.store, "attaching file", &error);
                s.notify(error, true);
                None
            }
        }).collect();
        let added = self.carry(&valid);
        if added > 0 {
            self.wants_field = true;
            s.notify(
                format!(
                    "attached {added} file{} · enter to send",
                    if added == 1 { "" } else { "s" }
                ),
                false,
            );
        } else if !valid.is_empty() {
            s.notify("already carrying these files", false);
        }
        s.redraw();
    }

    /// Puts one down, by its place in the order.
    pub fn uncarry(&mut self, i: usize) -> Option<Carried> {
        (i < self.carrying.len()).then(|| self.carrying.remove(i))
    }

    /// Moves one a step in the order they will go, `d` places; answers
    /// where it landed, or `None` for a step off either end.
    pub fn move_carried(&mut self, i: usize, d: isize) -> Option<usize> {
        let n = self.carrying.len() as isize;
        let j = i as isize + d;
        if i as isize >= n || j < 0 || j >= n {
            return None;
        }
        self.carrying.swap(i, j as usize);
        Some(j as usize)
    }

    /// Enter in the composer. An edit under way is committed — over the wire
    /// where the build is signed in, and TDLib's echo re-projects it; else the
    /// local, undoable edit — unless nothing changed or nothing is left, and
    /// the draft comes back. Otherwise what the composer holds goes: the
    /// carried files where there are any, each its own line with the words
    /// under the first; the text alone where there are none. Over the wire
    /// where signed in, the transcript filling from the echo; off the wire the
    /// toast says what would leave. The composer is emptied the way a send
    /// always empties it, wire or no.
    pub fn send(&mut self, s: &mut Session) {
        if self.blocked() {
            s.notify("unblock this user before sending a message", false);
            return;
        }
        if self.card().is_some_and(|c| !c.can_post()) {
            s.notify("you can't post here", false);
            return;
        }
        if let Some(e) = self.editing.take() {
            let text = e.text.trim().to_string();
            if !text.is_empty() && text != e.original {
                // The wire edits it where signed in — the echo re-projects the
                // new text; off the wire the local action edits the row and
                // undo flips it back to the old text and the old edited flag.
                // A media line's text is its caption, which is its own
                // request; a text line's is the text.
                let captioned = self
                    .history()
                    .iter()
                    .any(|m| m.key() == e.msg && m.media.is_some());
                let request = if captioned {
                    requests::edit_message_caption(e.msg.0, e.msg.1, &text)
                } else {
                    requests::edit_message_text(e.msg.0, e.msg.1, &text)
                };
                if let Err(error) = super::super::history::command(s, &request) {
                    if super::live(&self.store) {
                        self.editing = Some(e);
                        error.notify(s, "edit");
                    } else {
                        verbs::edit_line(s, e.msg.0, e.msg.1, &e.original, e.was_edited, &text);
                    }
                }
            }
            s.redraw();
            return;
        }
        if self.draft.trim().is_empty() && self.carrying.is_empty() {
            return;
        }
        if super::live(&self.store) {
            self.send_draft(s);
            return;
        }
        // Over the wire where the build is signed in; the sent line rides its
        // own echo back into the transcript, so no optimistic local write is
        // ours to make. Carrying anything makes the files the message and the
        // words their caption — the way the client sends a picture with
        // something written under it — so there is no second, textless line.
        let text = self.draft.trim().to_string();
        let wired = if self.carrying.is_empty() {
            !text.is_empty()
                && wire(&self.store, &self.request(requests::send_message(self.peer, &text, self.reply_to())))
        } else {
            self.send_files(&text)
        };
        if !wired {
            let mut parts: Vec<String> = Vec::new();
            if self.reply_to.is_some() {
                parts.push("reply".to_string());
            } else if !text.is_empty() {
                parts.push("send".to_string());
            }
            if !self.carrying.is_empty() {
                let n = self.carrying.len();
                parts.push(format!("with {n} file{}", if n == 1 { "" } else { "s" }));
            }
            if !parts.is_empty() {
                s.notify(draft_toast(&parts.join(" ")), false);
            }
        }
        self.set_draft("");
        // A send clears the server's draft with it, so there is nothing left
        // for the leaving to tell it — and the composer stops being anybody's
        // own words, so what the row says next, even a moment later, is news
        // it takes: which is how a refused send comes back into it.
        self.sent_draft.clear();
        self.seen_draft.clear();
        self.carrying.clear();
        self.reply_to = None;
        s.redraw();
    }

    /// A live send, shared by Enter and the agent's approved send. Every
    /// attachment belongs to one history action; delivery is still tracked
    /// per message. A queue failure leaves the whole composer intact.
    pub fn send_draft(&mut self, s: &mut Session) -> Vec<u64> {
        // Read permission without reconciling a newer remote draft: the
        // caller has already read the composer, and the tool approved that
        // exact text. Nothing may substitute other words at this boundary.
        let card = super::super::topics::card(&self.store, self.peer, self.topic);
        if card.as_ref().is_some_and(|c| !c.can_post()) || self.editing.is_some()
            || (self.carrying.is_empty() && self.draft.trim().is_empty())
        {
            return Vec::new();
        }
        let text = self.draft.clone();
        let reply = self.reply_to;
        let carried = self.carrying.clone();
        let requests: Vec<_> = if carried.is_empty() {
            vec![self.request(requests::send_message(self.peer, text.trim(), self.reply_to()))]
        } else {
            carried.iter().enumerate().map(|(i, file)| self.request(requests::send_file(
                self.peer,
                if i == 0 { self.reply_to() } else { None },
                file,
                if i == 0 { text.trim() } else { "" },
            ))).collect()
        };
        let queued = if requests.len() == 1 {
            super::super::history::command(s, &requests[0]).map(|id| vec![id])
        } else {
            let label = format!("send {} attachments", carried.len());
            let label = card.map_or(label.clone(), |c| format!("{label} · {}", c.name));
            super::super::history::batch(s, &requests, label)
        };
        let operations = match queued {
            Ok(ids) => ids,
            Err(error) => { error.notify(s, "send"); Vec::new() }
        };
        if !operations.is_empty() {
            self.set_draft("");
            self.sent_draft.clear();
            self.seen_draft.clear();
            self.reply_to = None;
            self.carrying.clear();
            for (slot, panel) in s.panels() {
                if slot == self.slot { continue; }
                let mut p = panel.borrow_mut();
                if let Some(c) = p.as_any().downcast_mut::<Chat>()
                    .filter(|c| c.peer == self.peer && c.topic == self.topic)
                {
                    c.forget_sent_draft(&text, reply, &carried);
                }
            }
        }
        s.redraw();
        operations
    }

    /// The carried files, each as its own message: the composer's words ride
    /// as the caption of the first and the line it answers with it, the rest
    /// going bare. That is the client's order too — a caption belongs to one
    /// picture, not to the handful behind it.
    ///
    /// Answers whether they left. Off the wire nothing does, and the caller's
    /// toast says what would have.
    fn send_files(&self, text: &str) -> bool {
        let mut went = false;
        for (i, file) in self.carrying.iter().enumerate() {
            let first = i == 0;
            let caption = if first { text } else { "" };
            let reply = if first { self.reply_to() } else { None };
            if wire(&self.store, &self.request(requests::send_file(self.peer, reply, file, caption))) {
                went = true;
            }
        }
        went
    }

    /// The draft to the server, where it has moved since the server last
    /// heard it: the chat is being left — closed, or the keyboard gone
    /// elsewhere — and a half-written line belongs on the phone too. An empty
    /// composer says so with `None`, which is how a draft is cleared.
    ///
    /// Off the wire there is nothing to tell, and the retry costs a comparison
    /// the next time the chat is left.
    pub fn flush_draft(&mut self) {
        self.save_pending_draft();
        if self.draft == self.sent_draft {
            return;
        }
        let text = self.draft.trim();
        if wire(
            &self.store,
            &self.request(requests::set_chat_draft(self.peer, (!text.is_empty()).then_some(text))),
        ) {
            self.sent_draft.clone_from(&self.draft);
        }
    }

    // -- playing --------------------------------------------------------------------

    /// Where the player over a line stands, if that line is the one playing
    /// or paused; the line's own length at rest otherwise.
    #[must_use]
    pub fn player_state(&self, msg: &Msg, now: f64) -> Option<PlayerState> {
        if let Some(player) = self.player.as_ref().filter(|p| p.msg == msg.key()) {
            return player.player_state(msg, now);
        }
        let md = msg.media.as_ref()?;
        let length = md.secs.or_else(|| super::playback::moving_picture_of_the_wire(msg).then_some(0))?;
        Some(PlayerState { length: length as f64, ..PlayerState::default() })
    }

    /// Play or pause a line: the one playing pauses, any other takes over.
    pub fn toggle_play(&mut self, msg: &Msg, now: f64) {
        self.select_playback(msg.key()).toggle_play(msg, now);
    }

    /// A progress-bar press can select a line before its first play.
    pub fn select_playback(&mut self, id: MsgKey) -> &mut Playback {
        if self.player.as_ref().is_none_or(|p| p.msg != id) {
            self.player = Some(Playback::new(self.store.clone(), id));
        }
        self.player.as_mut().unwrap()
    }

    pub fn playback(&mut self, id: MsgKey) -> Option<&mut Playback> {
        self.player.as_mut().filter(|p| p.msg == id)
    }

    pub fn active_media(&self) -> Option<MsgKey> {
        self.player.as_ref().map(|p| p.msg)
    }

    pub fn pause(&mut self, now: f64) {
        if let Some(player) = &mut self.player { player.pause(now); }
    }

    /// Whether anything runs — what asks for the next frame.
    #[must_use]
    pub fn playing(&self, now: f64) -> bool {
        self.player.as_ref().is_some_and(|p| p.playing(now))
    }
}

/// The transcript's rows, worked out from the lines: a day caption where
/// the day changes, the unread line above `first_unread`, a run where one
/// writer goes on within [`RUN_GAP`].
#[must_use]
pub fn rows_of(history: &[Msg], first_unread: Option<MsgKey>, now: f64) -> Vec<Row> {
    let mut rows: Vec<Row> = Vec::with_capacity(history.len() + 8);
    let mut prev: Option<&Msg> = None;
    let mut run_with: Option<&Msg> = None;
    for m in history {
        if !prev.is_some_and(|p| same_day(p.date, m.date)) {
            rows.push(Row::Day(day_caption(m.date, now)));
            run_with = None;
        }
        if first_unread == Some(m.key()) {
            rows.push(Row::Unread);
            run_with = None;
        }
        if m.service {
            rows.push(Row::Service(m.clone()));
            run_with = None;
        } else {
            // A line that never left, or has not yet, keeps its own header:
            // the header is where its state is said.
            let stands_out = m.fwd_from.is_some()
                || m.reply_to.is_some()
                || matches!(m.state.as_deref(), Some("failed" | "sending"));
            let run = run_with.is_some_and(|r| {
                r.chat == m.chat && r.out == m.out && r.sender == m.sender && m.date - r.date <= RUN_GAP && !stands_out
            });
            rows.push(Row::Message {
                msg: m.clone(),
                run,
            });
            run_with = Some(m);
        }
        prev = Some(m);
    }
    rows
}

/// One line's words onto the clipboard — the chat's *copy* over the cursor,
/// and the same verb on the line's own card. A line that is a picture, a
/// recording or a place has no words, so what goes is what the transcript
/// says it is: *photo 1600×1200*, *voice 0:14*.
///
/// The copy is an effect, so a world that may not touch a human's clipboard
/// refuses it out loud rather than quietly doing it — which is why the toast
/// is the effect's answer and not a foregone *copied*.
pub fn copy_line(s: &mut Session, m: &Msg) {
    let text = if m.text.trim().is_empty() {
        m.media.as_ref().map(|md| md.line(s.now())).unwrap_or_default()
    } else {
        m.text.clone()
    };
    if text.is_empty() {
        s.notify("nothing on that line to copy", true);
        return;
    }
    s.run_effect(Clip {
        text,
        what: "the line",
    }, |s, result| match result {
        Ok(()) => s.notify("copied", false),
        Err(error) => s.notify(error, true),
    });
}

impl Panel for Chat {
    fn flush(&mut self) { self.flush_draft(); }

    fn id(&self) -> &PanelId {
        &self.id
    }

    /// The peer's name, read straight rather than through
    /// [`card`](Chat::card): a title is asked for from `&self`, and the card
    /// is the draw's reader, which reconciles.
    fn title(&self) -> String {
        super::super::topics::card(&self.store, self.peer, self.topic).map_or_else(|| "chat".to_string(), |c| c.name)
    }

    /// Five wide, the whole height: a conversation is the one panel that
    /// wants to be wide.
    fn wish(&self, _cols: usize) -> (u32, u32) {
        (5, 6)
    }

    fn placed(&mut self, slot: SlotId) {
        self.slot = slot;
    }

    /// What a chat is for, over the line under the cursor: `reply`, `copy`, `react`,
    /// and on a line of mine `edit` and `delete` — none of them while there
    /// is no cursor, or while rows are marked and the batch has the bar. Then
    /// three links to the rest: `attach`, what goes with the next message
    /// and the ways to make more of it, saying how many files wait; `line`,
    /// the cursor's line as a card, which wears the verbs on one line;
    /// `about`, the peer's card, which wears the verbs about the chat and
    /// its search. With marks: `forward n`, `delete n` while every marked
    /// line is mine, and `clear`. A jump to an original offers `back` until
    /// the saved replies have been retraced, even while rows are marked.
    fn verbs(&self) -> Vec<Verb> {
        let blocked = self.blocked();
        if let Some(verbs) = self.reactions.verbs() {
            return verbs;
        }
        let n = self.marks.len();
        let k = self.carrying.len();
        let snapshot = self.transcript.get(&self.store);
        let under = self.cursor.and_then(|id| snapshot.message(id)).filter(|m| !m.service);
        let mut v = Vec::new();
        if !blocked && model::peer(&self.store, self.peer).is_none_or(|card| card.can_post())
            && (self.editing.is_some() || !self.draft.trim().is_empty() || k > 0)
        {
            v.push(Verb::run("telegram.submit", if self.editing.is_some() { "save" } else { "send" }, None));
        }
        if !self.reply_back.is_empty() {
            v.push(Verb::run("telegram.back", "back", Some('b')));
        }
        if let Some(card) = model::peer(&self.store, self.peer)
            .filter(|c| c.kind == model::PeerKind::Group && c.unread_mentions > 0)
        {
            v.push(Verb::go(
                "telegram.replies",
                format!("replies & mentions {}", card.unread_mentions),
                Some('s'),
                Nav::Open {
                    from: self.slot,
                    id: super::Messages::replies(Some(self.peer)),
                    fresh: false,
                },
            ));
        }
        if model::peer(&self.store, self.peer).is_some_and(|c| c.is_forum) {
            v.push(Verb::go("telegram.topics", "topics", None, Nav::Open {
                from: self.slot, id: super::Topics::id(self.peer), fresh: false,
            }));
        }
        if n == 0 {
            if let Some(m) = under {
                if !blocked {
                    v.push(Verb::run("telegram.reply", "reply", Some('r')));
                }
                if m.out {
                    if !blocked {
                        v.push(Verb::run("telegram.edit", "edit", Some('e')));
                    }
                    v.push(Verb::run("telegram.delete", "delete", Some('d')));
                }
                // A reply's original, to jump to — the way a press on the
                // quoted line does on the client.
                if m.reply_to.is_some() {
                    v.push(Verb::run("telegram.original", "original", Some('o')));
                }
                v.push(Verb::run("telegram.copy", "copy", Some('c')));
                v.extend(downloads::verb(m));
                if reactions::can_react(m) {
                    v.push(Verb::run("telegram.react", "react(j)", Some('j')));
                }
            }
        }
        if blocked {
            if !runtime::of(&self.store).peer_action_pending(self.peer) {
                // Back keeps cmd+b even after the last return point, so
                // another press cannot unexpectedly unblock the person.
                v.push(Verb::run("telegram.unblock", "unblock user", Some('k')));
            }
        } else {
            v.push(Verb::go(
                "telegram.attach",
                if k == 0 { "attach".to_string() } else { format!("attach {k}") },
                Some('h'),
                Nav::Open {
                    from: self.slot,
                    id: Attach::in_topic(self.peer, self.topic),
                    fresh: false,
                },
            ));
        }
        if let Some(m) = under {
            v.push(Verb::go(
                "telegram.line",
                "line",
                Some('n'),
                Nav::Open {
                    from: self.slot,
                    id: Line::id(m.chat, m.id),
                    fresh: false,
                },
            ));
        }
        v.push(Verb::go(
            "telegram.about",
            "about",
            Some('a'),
            Nav::Open {
                from: self.slot,
                id: Peer::id(self.peer),
                fresh: false,
            },
        ));
        if n > 0 {
            v.push(Verb::run("telegram.forward", format!("forward {n}"), Some('f')));
            let all_mine = self
                .marks
                .iter()
                .all(|id| snapshot.message(*id).is_some_and(|m| m.out));
            if all_mine {
                v.push(Verb::run("telegram.delete", format!("delete {n}"), Some('d')));
            }
            v.push(Verb::run("telegram.clear", "clear", None));
        }
        v
    }

    fn run(&mut self, verb: &str, s: &mut Session) {
        if self.reactions.run(&self.store, verb, s) {
            return;
        }
        match verb {
            "telegram.submit" => self.send(s),
            "telegram.unblock" => super::peer::perform(s, self.peer, requests::PeerAction::Unblock),
            "telegram.react" if self.marks.is_empty() => {
                if let Some(m) = self.cursor.and_then(|id| model::line(&self.store, id.0, id.1)) {
                    self.reactions.open(s, &m);
                    s.redraw();
                }
            }
            "telegram.reply" => {
                if let Some(c) = self.cursor {
                    if self.reply(c) {
                        s.redraw();
                    }
                }
            }
            "telegram.edit" => {
                if let Some(c) = self.cursor {
                    if self.edit(c) {
                        s.redraw();
                    }
                }
            }
            "telegram.original" => self.jump_to_original(s),
            "telegram.back" => self.jump_back(s),
            // The marks, or the cursor's own line.
            "telegram.delete" => {
                let ids: Vec<MsgKey> = if self.marks.is_empty() {
                    self.cursor.into_iter().collect()
                } else {
                    self.marks.iter().copied().collect()
                };
                self.delete(s, ids);
            }
            "telegram.clear" => {
                self.marks.clear();
                s.redraw();
            }
            "telegram.copy" => {
                let hist = self.history();
                if let Some(m) = self.cursor.and_then(|c| hist.iter().find(|m| m.key() == c)) {
                    copy_line(s, m);
                }
            }
            "telegram.download" if self.marks.is_empty() => {
                let hist = self.history();
                if let Some(m) = self.cursor.and_then(|c| hist.iter().find(|m| m.key() == c)) {
                    downloads::request(s, m);
                }
            }
            // The lines are taken out of the transcript and the chat list
            // opens to be picked from, as the client's forward sheet is a
            // list of chats: the pick outlives this panel, so it is the
            // app's to hold. The marks have done their work here.
            "telegram.forward" => {
                let ids: Vec<MsgKey> = if self.marks.is_empty() {
                    self.cursor.into_iter().collect()
                } else {
                    self.marks.iter().copied().collect()
                };
                if ids.is_empty() {
                    return;
                }
                let selected: BTreeSet<_> = ids.into_iter().collect();
                let ordered = self.history().iter().map(Msg::key).filter(|key| selected.contains(key)).collect();
                runtime::of(&self.store).carry_forward_messages(ordered);
                self.marks.clear();
                s.nav(Nav::Open {
                    from: self.slot,
                    id: Chats::id(),
                    fresh: false,
                });
                s.notify("pick a chat to forward to", false);
                s.redraw();
            }
            _ => {}
        }
    }

    fn as_any(&mut self) -> &mut dyn Any {
        self
    }
}

/// A chat closing is the moment its draft goes to Telegram: one client's
/// composer is every client's, and until now the half-written line was this
/// device's alone. The panel is the only thing that knows the draft moved, so
/// the going is where it says so.
impl Drop for Chat {
    fn drop(&mut self) {
        self.flush_draft();
        if let Some(write) = self.draft_write.take() {
            runtime::of(&self.store).track_write(write, "saving draft");
        }
    }
}

/// The read a chat claims when it opens, and how it is given back.
pub struct ReadClaim {
    pub peer: PeerId,
    pub topic: i64,
    pub unread: i64,
    pub last_read: Option<MsgId>,
    pub through: MsgId,
}

impl Intent for ReadClaim {
    fn describe(&self) -> String {
        format!("chat:{} read", self.peer)
    }

    fn reverse(&self, w: &World) -> Result<(), String> {
        let (peer, topic, unread, last_read) = (self.peer, self.topic, self.unread, self.last_read);
        w.store()
            .write(move |c| {
                if topic != 0 {
                    return c.execute("UPDATE tg_topic SET unread = ?3, last_read = ?4
                        WHERE chat = ?1 AND id = ?2", rusqlite::params![peer, topic, unread, last_read]).map(|_| ());
                }
                c.execute(
                    "UPDATE tg_chat SET unread = ?2, last_read = ?3 WHERE peer = ?1",
                    rusqlite::params![peer, unread, last_read],
                )
                .map(|_| ())
            })
            .map_err(|e| e.to_string())
    }

    fn reapply(&self, w: &World) -> Result<(), String> {
        let (peer, topic, through) = (self.peer, self.topic, self.through);
        w.store()
            .write(move |c| super::super::topics::read_tx(c, peer, topic, through))
            .map_err(|e| e.to_string())
    }
}

/// The chat *saved messages* really means. The launcher's root names the
/// demo world's own stand-in self ([`seed::SELF`](super::super::seed::SELF)),
/// because until an account signs in that is the only self there is; once one
/// has, the engine says who the account holder is
/// ([`model::self_peer`]) and the notes-to-self open on their chat instead.
///
/// Resolved here rather than in the root, so the launcher keeps one identity
/// for the entry however many accounts pass through the store, and a fixture
/// — which has no signed-in self — opens exactly the conversation it drew
/// before.
fn saved_messages(store: &Store, peer: PeerId) -> PeerId {
    if peer != super::super::seed::SELF {
        return peer;
    }
    model::self_peer(store).unwrap_or(peer)
}

/// The factory. Opening a chat with an ordinary cached line reads it on the
/// opening action's node. Where reading started is kept for the unread line.
pub struct ChatKind;

impl PanelKind for ChatKind {
    fn tag(&self) -> Tag {
        Chat::TAG
    }

    fn open(&self, id: &PanelId, cx: &mut Opening<'_>) -> Box<dyn Panel> {
        let store = cx.session().store().clone();
        let peer = saved_messages(&store, Chat::of(id).unwrap_or_default());
        let topic = Chat::topic_of(id);
        let at = Chat::msg_of(id);
        let topic = if topic == 0 { at.map_or(0, |msg| model::message_topic(&store, peer, msg)) } else { topic };
        let card = super::super::topics::card(&store, peer, topic);
        let (unread, last_read, draft) = card.map_or(
            (0, None, String::new()),
            |c| (c.unread, c.last_read, c.draft.unwrap_or_default()),
        );
        // Capture the unread divider before advancing the read position.
        let first_unread = if unread > 0 {
            model::first_unread_in(&store, peer, topic, last_read.unwrap_or(0))
        } else {
            None
        };
        // A newly discovered group's cache may contain only unread mentions.
        // Claim no local read unless an ordinary line can carry it to Telegram.
        // A restored panel must not send a new receipt beyond a redo's
        // original boundary; the recorded claim reapplies that exact read.
        let read_target = if unread > 0 && at.is_none() && cx.how().claims() {
            model::newest_ordinary_line_in(&store, peer, topic)
                .filter(|through| *through > last_read.unwrap_or(0))
        } else {
            None
        };
        if let Some(through) = read_target {
            cx.claim(
                Box::new(move |tx: &rusqlite::Transaction| super::super::topics::read_tx(tx, peer, topic, through)),
                vec![Box::new(ReadClaim {
                    peer,
                    topic,
                    unread,
                    last_read,
                    through,
                }) as Box<dyn Intent>],
            );
        }
        // Carry the ordinary read through to Telegram even when the newest
        // lines are unread replies or mentions. Name the preceding ordinary
        // line: `viewMessages` would acknowledge a named notification too,
        // which waits until it is visible in the transcript.
        #[cfg(feature = "tdlib")]
        if let Some(last) = read_target {
            let _ = wire(&store, &requests::in_topic(requests::view_messages(peer, &[last]), topic));
        }
        // The widget requests history only after its viewport settles. Merely
        // traversing this chat, or restoring a hidden panel, needs no refresh.
        // Opened at a line — from a messages list — the cursor starts on
        // it; from a row of the chat list, nowhere.
        Box::new(Chat {
            id: id.clone(),
            peer,
            topic,
            store,
            slot: 0,
            cursor: at.map(|id| (peer, id)),
            follow_wish: at.map(|id| (peer, id)),
            reply_back: Vec::new(),
            marks: BTreeSet::new(),
            reply_to: None,
            sent_draft: draft.clone(),
            seen_draft: draft.clone(),
            draft_pending: false,
            draft_write: None,
            draft,
            editing: None,
            #[cfg(test)]
            first_unread,
            viewed_messages: std::collections::BTreeMap::new(),
            carrying: Vec::new(),
            dropped_files: Vec::new(),
            wants_field: false,
            player: None,
            reactions: Reactions::default(),
            transcript: super::super::transcript::Transcript::new(peer, topic, first_unread, cx.session().now()),
        })
    }
}
