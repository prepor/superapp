//! One conversation: the transcript with its cursor and marks, the reply
//! line, the draft in the composer or the edit under way in it, what the
//! composer carries, and the one line playing.
//!
//! Opening marks the chat read locally on the layout action's undo node.
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
use kernel::session::Session;
use kernel::store::Store;

use crate::shell::widgets::media::PlayerState;

use super::super::draft_toast;
use super::super::model::{
    self, day_caption, same_day, Carried, Msg, MsgId, PeerCard, PeerId, Player, RUN_GAP,
};
use super::super::{requests, runtime, verbs};
use super::{wire, Attach, Chats, Line, Peer};

/// An edit under way in the composer: which of my lines, what it said, and
/// what the field says now.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Editing {
    pub msg: MsgId,
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
    store: Rc<Store>,
    slot: SlotId,
    cursor: Option<MsgId>,
    /// A line the transcript is asked to bring on screen — the original a
    /// reply jumps to — taken once by the widget, the way a caret wish is.
    follow_wish: Option<MsgId>,
    marks: BTreeSet<MsgId>,
    reply_to: Option<MsgId>,
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
    /// An edit under way: the field shows its text instead of the draft,
    /// which waits.
    editing: Option<Editing>,
    /// The first message not read when the panel opened, where there was
    /// one: the unread line stays above it for the panel's life.
    first_unread: Option<MsgId>,
    /// Retry unacknowledged views after a short delay; a queued command is
    /// not enough to dismiss a server notification.
    viewed_mentions: std::collections::BTreeMap<MsgId, f64>,
    /// What the composer will send with the text, in the order it will go.
    /// Edited from the attach panel, through the join.
    carrying: Vec<Carried>,
    /// A reply asked for the caret: the widget takes this once and puts the
    /// keyboard in the field, whatever had it.
    wants_field: bool,
    /// The one line playing, or paused: a chat plays one thing at a time.
    player: Option<Player>,
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
            .then(|| id.arg(1)?.parse().ok())
            .flatten()
    }

    #[must_use]
    pub fn peer(&self) -> PeerId {
        self.peer
    }

    fn blocked(&self) -> bool {
        model::peer(&self.store, self.peer).is_some_and(|c| c.blocked)
    }

    pub fn play_on_open(&self, id: MsgId) {
        runtime::of(&self.store).play_on_open(self.peer, id);
    }

    pub fn want_file(&self, remote_id: &str) {
        runtime::of(&self.store).want_file(remote_id);
    }

    /// Whether the transcript is still being filled from the wire — the
    /// history walk a chat starts as it opens, until its last page lands.
    #[must_use]
    pub fn loading(&self) -> bool {
        runtime::of(&self.store).loading(self.peer)
    }

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
        let card = model::peer(&self.store, self.peer);
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
        // nothing typed since the last flush — or nothing at all. Typing
        // writes the row too, so the row saying what the composer says is
        // no sign nobody typed (review, 2026-09-07: a phone's draft
        // overwrote unsent words).
        let untouched = self.draft.is_empty() || self.draft == self.sent_draft;
        self.seen_draft.clone_from(&row);
        if untouched {
            self.sent_draft.clone_from(&row);
            self.draft = row;
        }
    }

    /// The lines, oldest first.
    #[must_use]
    pub fn history(&self) -> Rc<Vec<Msg>> {
        model::history(&self.store, self.peer)
    }

    /// The widget supplies only messages visible in the focused transcript.
    pub fn view_mentions(&mut self, visible: &[MsgId], now: f64) {
        let ids: Vec<MsgId> = self.history().iter()
            .filter(|m| m.unread_mention && visible.contains(&m.id))
            .filter(|m| self.viewed_mentions.get(&m.id).is_none_or(|at| now - at >= 5.0))
            .map(|m| m.id).collect();
        if ids.is_empty() {
            return;
        }
        if wire(&self.store, &requests::view_messages(self.peer, &ids)) {
            for id in ids {
                self.viewed_mentions.insert(id, now);
            }
        } else if !super::super::Telegram::engine_store(self.store.dir())
            && super::super::schema::session(self.store.conn()).state == "closed"
        {
            let peer = self.peer;
            super::flip(&self.store, move |c| super::super::project::read_mentions(c, peer, &ids));
        }
    }

    /// Where the reading started, if anything was unread.
    #[cfg(test)]
    #[must_use]
    pub fn first_unread(&self) -> Option<MsgId> {
        self.first_unread
    }

    /// The transcript: the lines with the day captions, the unread line and
    /// the runs worked out. `now` is what the captions are spelled against.
    #[must_use]
    pub fn rows(&self, now: f64) -> Vec<Row> {
        rows_of(&self.history(), self.first_unread, now)
    }

    // -- the cursor and the marks -----------------------------------------------

    #[must_use]
    pub fn cursor(&self) -> Option<MsgId> {
        self.cursor
    }

    pub fn set_cursor(&mut self, id: MsgId) {
        self.cursor = Some(id);
    }

    /// Steps the cursor over the messages, `d` rows: from nothing, either
    /// way lands on the newest line, which is where a chat is read from.
    /// Answers what it landed on.
    pub fn walk(&mut self, d: isize) -> Option<MsgId> {
        let ids: Vec<MsgId> = self
            .history()
            .iter()
            .filter(|m| !m.service)
            .map(|m| m.id)
            .collect();
        if ids.is_empty() {
            return None;
        }
        let at = match self.cursor.and_then(|c| ids.iter().position(|i| *i == c)) {
            Some(i) => (i as isize + d).clamp(0, ids.len() as isize - 1) as usize,
            None => ids.len() - 1,
        };
        self.cursor = Some(ids[at]);
        self.cursor
    }

    #[must_use]
    pub fn marks(&self) -> &BTreeSet<MsgId> {
        &self.marks
    }

    /// Space: the mark on the cursor's line, toggled.
    pub fn toggle_mark(&mut self) {
        let Some(c) = self.cursor else { return };
        if !self.marks.remove(&c) {
            self.marks.insert(c);
        }
    }

    /// Shift with an arrow: the line left and the line landed on, both
    /// marked.
    pub fn mark_range(&mut self, d: isize) {
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
    pub fn reply_to(&self) -> Option<MsgId> {
        self.reply_to
    }

    /// Replies to a line — the bar's verb over the cursor, or the card of
    /// one asking this of the chat it hangs under. A reply is written, so
    /// the caret goes to the field with it. A blocked peer refuses the reply.
    pub fn reply(&mut self, msg: MsgId) -> bool {
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
    pub fn edit(&mut self, msg: MsgId) -> bool {
        if self.blocked() {
            return false;
        }
        let hist = self.history();
        let Some(m) = hist.iter().find(|m| m.id == msg && m.out && !m.service) else {
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

    /// The field changed: an edit keeps its text on the instance, a draft
    /// goes to the chat's row.
    pub fn typed(&mut self, text: &str) {
        match &mut self.editing {
            Some(e) => e.text = text.to_string(),
            None => self.set_draft(text),
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

    /// Deletes lines — the marks, or the cursor's own — as one undoable
    /// action, the cursor stepping off them first.
    pub fn delete(&mut self, s: &mut Session, ids: Vec<MsgId>) {
        if ids.is_empty() {
            return;
        }
        self.lines_gone(&ids);
        // Over the wire where the build is signed in: `deleteMessages` for
        // everyone, and TDLib's `updateDeleteMessages` strikes the rows. Off
        // the wire — the demo, or signed out — the local action deletes them
        // and undo puts them back. One or the other, never both, so a line is
        // not deleted twice.
        if !wire(
            &self.store,
            &requests::delete_messages(self.peer, &ids, true),
        ) && !super::live(&self.store)
        {
            verbs::delete_lines(s, self.peer, ids);
        }
        s.redraw();
    }

    /// Lines that are going: the cursor steps to the line before them — or
    /// after, at the top — the marks let go of them, and a reply to one or
    /// an edit of one is dropped. Called before the store hears of it, so
    /// the neighbours are still in the history.
    pub fn lines_gone(&mut self, ids: &[MsgId]) {
        if let Some(c) = self.cursor.filter(|c| ids.contains(c)) {
            let hist = self.history();
            let stays = |m: &&Msg| !m.service && !ids.contains(&m.id);
            self.cursor = hist.iter().position(|m| m.id == c).and_then(|i| {
                hist[..i]
                    .iter()
                    .rev()
                    .find(stays)
                    .or_else(|| hist[i + 1..].iter().find(stays))
                    .map(|m| m.id)
            });
        }
        for id in ids {
            self.marks.remove(id);
        }
        if self.editing.as_ref().is_some_and(|e| ids.contains(&e.msg)) {
            self.editing = None;
        }
        if self.reply_to.is_some_and(|r| ids.contains(&r)) {
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
    pub fn take_follow_wish(&mut self) -> Option<MsgId> {
        self.follow_wish.take()
    }

    /// Puts the cursor on the line the cursor's line answers — a reply's
    /// original — and asks the transcript to bring it on screen. An original
    /// the window does not hold (older than the ten thousand kept, or not
    /// yet backfilled) is said so rather than silently not jumped to.
    pub fn jump_to_original(&mut self, s: &mut Session) {
        let hist = self.history();
        let Some(target) = self
            .cursor
            .and_then(|c| hist.iter().find(|m| m.id == c))
            .and_then(|m| m.reply_to)
        else {
            return;
        };
        if hist.iter().any(|m| m.id == target) {
            self.cursor = Some(target);
            self.follow_wish = Some(target);
        } else {
            s.notify("the line it answers is not loaded".to_string(), false);
        }
        s.redraw();
    }


    /// What the line above the composer says while replying: *reply to
    /// Vera: the line*, shortened to one line.
    #[must_use]
    pub fn reply_line(&self, now: f64) -> Option<String> {
        let id = self.reply_to?;
        let hist = self.history();
        let m = hist.iter().find(|m| m.id == id)?;
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
        if self.draft == text {
            return;
        }
        self.draft = text.to_string();
        let (peer, d) = (self.peer, self.draft.clone());
        if let Err(e) = self.store.write(move |c| model::set_draft_tx(c, peer, &d)) {
            runtime::of(&self.store)
                .operations
                .report(&self.store, "saving draft", &e.to_string());
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
        let mut valid = Vec::new();
        for path in paths {
            let disk = kernel::caps::real_path(path);
            match std::fs::metadata(&disk) {
                Ok(m) if m.is_file() => valid.push(
                    std::fs::canonicalize(&disk)
                        .unwrap_or(disk)
                        .to_string_lossy()
                        .into_owned(),
                ),
                result => {
                    let error = match result {
                        Ok(_) => format!("{path} is not a regular file"),
                        Err(e) => format!("Cannot attach {path}: {e}"),
                    };
                    runtime::of(&self.store).operations.report(
                        &self.store,
                        "attaching file",
                        &error,
                    );
                    s.notify(error, true);
                }
            }
        }
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
                    .any(|m| m.id == e.msg && m.media.is_some());
                let request = if captioned {
                    requests::edit_message_caption(self.peer, e.msg, &text)
                } else {
                    requests::edit_message_text(self.peer, e.msg, &text)
                };
                if !wire(&self.store, &request) {
                    if super::live(&self.store) {
                        self.editing = Some(e);
                    } else {
                        verbs::edit_line(s, self.peer, e.msg, &e.original, e.was_edited, &text);
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
            self.send_live(s);
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
                && wire(&self.store, &requests::send_message(self.peer, &text, self.reply_to))
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

    /// Only queued files leave the composer. Each queued request owns its
    /// caption, reply and source path until Telegram confirms delivery.
    fn send_live(&mut self, s: &mut Session) {
        let text = self.draft.trim().to_string();
        let mut sent_first = false;
        if self.carrying.is_empty() {
            sent_first = wire(
                &self.store,
                &requests::send_message(self.peer, &text, self.reply_to),
            );
        } else {
            let files = std::mem::take(&mut self.carrying);
            let mut files = files.into_iter().enumerate();
            while let Some((i, file)) = files.next() {
                let request = requests::send_file(
                    self.peer,
                    if i == 0 { self.reply_to } else { None },
                    &file,
                    if i == 0 { &text } else { "" },
                );
                if !wire(&self.store, &request) {
                    self.carrying.push(file);
                    self.carrying.extend(files.map(|(_, file)| file));
                    break;
                }
                if i == 0 {
                    sent_first = true;
                }
            }
        }
        if sent_first {
            self.set_draft("");
            self.sent_draft.clear();
            self.seen_draft.clear();
            self.reply_to = None;
        } else {
            s.notify(
                "send failed: Telegram is not connected; your draft and files are kept",
                true,
            );
        }
        s.redraw();
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
            let reply = if first { self.reply_to } else { None };
            if wire(&self.store, &requests::send_file(self.peer, reply, file, caption)) {
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
        if self.draft == self.sent_draft {
            return;
        }
        let text = self.draft.trim();
        if wire(
            &self.store,
            &requests::set_chat_draft(self.peer, (!text.is_empty()).then_some(text)),
        ) {
            self.sent_draft.clone_from(&self.draft);
        }
    }

    // -- playing --------------------------------------------------------------------

    /// Where the player over a line stands, if that line is the one playing
    /// or paused; the line's own length at rest otherwise.
    #[must_use]
    pub fn player_state(&self, msg: &Msg, now: f64) -> Option<PlayerState> {
        let secs = msg.media.as_ref()?.secs?;
        Some(match self.player {
            Some(p) if p.msg == msg.id => p.state(now),
            _ => PlayerState {
                playing: false,
                position: 0.0,
                length: secs as f64,
            },
        })
    }

    /// Play or pause a line: the one playing pauses, any other takes over.
    pub fn toggle_play(&mut self, msg: &Msg, now: f64) {
        let Some(secs) = msg.media.as_ref().and_then(|m| m.secs) else {
            return;
        };
        let mut p = match self.player {
            Some(p) if p.msg == msg.id => p,
            _ => Player::over(msg.id, secs as f64),
        };
        p.toggle(now);
        self.player = Some(p);
    }

    /// Whether anything runs — what asks for the next frame.
    #[must_use]
    pub fn playing(&self, now: f64) -> bool {
        self.player.is_some_and(|p| p.state(now).playing)
    }
}

/// The transcript's rows, worked out from the lines: a day caption where
/// the day changes, the unread line above `first_unread`, a run where one
/// writer goes on within [`RUN_GAP`].
#[must_use]
pub fn rows_of(history: &[Msg], first_unread: Option<MsgId>, now: f64) -> Vec<Row> {
    let mut rows: Vec<Row> = Vec::with_capacity(history.len() + 8);
    let mut prev: Option<&Msg> = None;
    let mut run_with: Option<&Msg> = None;
    for m in history {
        if !prev.is_some_and(|p| same_day(p.date, m.date)) {
            rows.push(Row::Day(day_caption(m.date, now)));
            run_with = None;
        }
        if first_unread == Some(m.id) {
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
                r.out == m.out && r.sender == m.sender && m.date - r.date <= RUN_GAP && !stands_out
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
    let world = s.world().clone();
    let said = match world.run(&Clip {
        text: &text,
        what: "the line",
    }) {
        Ok(()) => ("copied".to_string(), false),
        Err(e) => (e, true),
    };
    s.notify(said.0, said.1);
}

impl Panel for Chat {
    fn id(&self) -> &PanelId {
        &self.id
    }

    /// The peer's name, read straight rather than through
    /// [`card`](Chat::card): a title is asked for from `&self`, and the card
    /// is the draw's reader, which reconciles.
    fn title(&self) -> String {
        model::peer(&self.store, self.peer).map_or_else(|| "chat".to_string(), |c| c.name)
    }

    /// Five wide, the whole height: a conversation is the one panel that
    /// wants to be wide.
    fn wish(&self, _cols: usize) -> (u32, u32) {
        (5, 6)
    }

    fn placed(&mut self, slot: SlotId) {
        self.slot = slot;
    }

    /// What a chat is for, over the line under the cursor: `reply`, `copy`,
    /// and on a line of mine `edit` and `delete` — none of them while there
    /// is no cursor, or while rows are marked and the batch has the bar. Then
    /// three links to the rest: `attach`, what goes with the next message
    /// and the ways to make more of it, saying how many files wait; `line`,
    /// the cursor's line as a card, which wears the verbs on one line;
    /// `about`, the peer's card, which wears the verbs about the chat and
    /// its search. With marks: `forward n`, `delete n` while every marked
    /// line is mine, and `clear`.
    fn verbs(&self) -> Vec<Verb> {
        let blocked = self.blocked();
        let n = self.marks.len();
        let k = self.carrying.len();
        let hist = self.history();
        let under = self.cursor.and_then(|c| hist.iter().find(|m| m.id == c && !m.service));
        let mut v = Vec::new();
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
            }
        }
        if blocked {
            if !runtime::of(&self.store).peer_action_pending(self.peer) {
                v.push(Verb::run("telegram.unblock", "unblock user", Some('b')));
            }
        } else {
            v.push(Verb::go(
                "telegram.attach",
                if k == 0 { "attach".to_string() } else { format!("attach {k}") },
                Some('h'),
                Nav::Open {
                    from: self.slot,
                    id: Attach::id(self.peer),
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
                    id: Line::id(self.peer, m.id),
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
                .all(|id| hist.iter().any(|m| m.id == *id && m.out));
            if all_mine {
                v.push(Verb::run("telegram.delete", format!("delete {n}"), Some('d')));
            }
            v.push(Verb::run("telegram.clear", "clear", None));
        }
        v
    }

    fn run(&mut self, verb: &str, s: &mut Session) {
        match verb {
            "telegram.unblock" => super::peer::perform(s, self.peer, requests::PeerAction::Unblock),
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
            // The marks, or the cursor's own line.
            "telegram.delete" => {
                let ids: Vec<MsgId> = if self.marks.is_empty() {
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
                if let Some(m) = self.cursor.and_then(|c| hist.iter().find(|m| m.id == c)) {
                    copy_line(s, m);
                }
            }
            // The lines are taken out of the transcript and the chat list
            // opens to be picked from, as the client's forward sheet is a
            // list of chats: the pick outlives this panel, so it is the
            // app's to hold. The marks have done their work here.
            "telegram.forward" => {
                let ids: Vec<MsgId> = if self.marks.is_empty() {
                    self.cursor.into_iter().collect()
                } else {
                    self.marks.iter().copied().collect()
                };
                if ids.is_empty() {
                    return;
                }
                runtime::of(&self.store).carry_forward(self.peer, ids);
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
    }
}

/// The read a chat claims when it opens, and how it is given back.
pub struct ReadClaim {
    pub peer: PeerId,
    pub unread: i64,
    pub last_read: Option<MsgId>,
}

impl Intent for ReadClaim {
    fn describe(&self) -> String {
        format!("chat:{} read", self.peer)
    }

    fn reverse(&self, w: &World) -> Result<(), String> {
        let (peer, unread, last_read) = (self.peer, self.unread, self.last_read);
        w.store()
            .write(move |c| {
                c.execute(
                    "UPDATE tg_chat SET unread = ?2, last_read = ?3 WHERE peer = ?1",
                    rusqlite::params![peer, unread, last_read],
                )
                .map(|_| ())
            })
            .map_err(|e| e.to_string())
    }

    fn reapply(&self, w: &World) -> Result<(), String> {
        let peer = self.peer;
        w.store()
            .write(move |c| model::mark_read_tx(c, peer))
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

/// The factory. Opening a chat reads it: the count goes to nought on the
/// opening action's node, and where the reading started is kept for the
/// unread line.
pub struct ChatKind;

impl PanelKind for ChatKind {
    fn tag(&self) -> Tag {
        Chat::TAG
    }

    fn open(&self, id: &PanelId, cx: &mut Opening<'_>) -> Box<dyn Panel> {
        let store = cx.session().store().clone();
        let peer = saved_messages(&store, Chat::of(id).unwrap_or_default());
        let at = Chat::msg_of(id);
        let card = model::peer(&store, peer);
        let (unread, last_read, draft) = card.map_or(
            (0, None, String::new()),
            |c| (c.unread, c.last_read, c.draft.unwrap_or_default()),
        );
        // Read before the claim below, which is what makes them all read.
        let first_unread = if unread > 0 {
            model::history(&store, peer)
                .iter()
                .find(|m| m.id > last_read.unwrap_or(0) && !m.out && !m.service)
                .map(|m| m.id)
        } else {
            None
        };
        if unread > 0 && at.is_none() {
            cx.claim(
                Box::new(move |tx: &rusqlite::Transaction| model::mark_read_tx(tx, peer)),
                vec![Box::new(ReadClaim {
                    peer,
                    unread,
                    last_read,
                }) as Box<dyn Intent>],
            );
        }
        // Where the build is signed in, carry that read through to Telegram
        // too, so the count clears at the server as it just did locally. The
        // newest ordinary line stands for the chat. If that line is an
        // unread reply or mention, wait until it is actually visible:
        // `viewMessages` would also acknowledge that notification.
        #[cfg(feature = "tdlib")]
        if unread > 0 && at.is_none() {
            if let Some(last) = model::history(&store, peer).iter().last()
                .filter(|m| !m.unread_mention).map(|m| m.id)
            {
                let _ = wire(&store, &requests::view_messages(peer, &[last]));
            }
        }
        // And fill the transcript's window from the wire: the newest page
        // first, to close whatever gap an absence left, then — the worker
        // walking on from each answer — the pages before the oldest line held,
        // until the window holds its ten thousand. This only starts the walk.
        // The account holder's own store asks; a fixture's would be putting a
        // real chat's history on the engine's queue for a scene.
        #[cfg(feature = "tdlib")]
        if super::super::Telegram::engine_store(store.dir()) {
            runtime::of(&store).want_history(peer);
        }
        // Opened at a line — from a messages list — the cursor starts on
        // it; from a row of the chat list, nowhere.
        Box::new(Chat {
            id: id.clone(),
            peer,
            store,
            slot: 0,
            cursor: at,
            follow_wish: at,
            marks: BTreeSet::new(),
            reply_to: None,
            sent_draft: draft.clone(),
            seen_draft: draft.clone(),
            draft,
            editing: None,
            first_unread,
            viewed_mentions: std::collections::BTreeMap::new(),
            carrying: Vec::new(),
            wants_field: false,
            player: None,
        })
    }
}
