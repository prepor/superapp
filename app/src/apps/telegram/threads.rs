//! The comments under a post: where they are, how many, and how far I have
//! read.
//!
//! A channel keeps its comments in another chat — the **discussion group** a
//! channel is linked to. Telegram copies every post into that group, and a
//! comment is an ordinary message there answering the copy: the thread's
//! **root**. So one row here is the joint between the two chats: the post a
//! person points at, and the group and root the transcript is actually read
//! from.
//!
//! Which half is known depends on who last spoke. A post's own `reply_info`
//! says how many comments there are and how far Telegram has seen me read,
//! and rides every copy of the post; `getMessageThread` is what answers
//! *where*, and is asked once per post and cached here.

use kernel::store::{Q, Store, Val};
use rusqlite::Connection;

use super::model::{self, MsgId, PeerCard, PeerId};

/// One post's comments, as the store holds them.
#[derive(Debug, Clone, PartialEq)]
pub struct Thread {
    /// The channel, and the post the comments hang from.
    pub chat: PeerId,
    pub post: MsgId,
    /// The discussion group and the root, once the wire has said.
    pub group: Option<PeerId>,
    pub root: Option<MsgId>,
    pub count: i64,
    pub last: Option<MsgId>,
    pub last_read: Option<MsgId>,
    /// How many of the comments this store holds are past that cursor —
    /// what the unread line in the panel is drawn above. Not the wire's
    /// count of unread comments, which only a thread that has been asked
    /// about knows.
    pub unread: i64,
    pub draft: Option<String>,
}

impl Thread {
    /// Where the comments are read and written, once it is known.
    #[must_use]
    pub fn where_it_is(&self) -> Option<(PeerId, MsgId)> {
        Some((self.group?, self.root?))
    }
}

static Q_THREAD: Q = Q {
    id: "telegram thread",
    sql: "SELECT chat, post, NULLIF(COALESCE(group_id, 0), 0), NULLIF(COALESCE(root, 0), 0),
                 count, NULLIF(COALESCE(last, 0), 0), NULLIF(COALESCE(last_read, 0), 0),
                 (SELECT COUNT(*) FROM tg_message m
                   WHERE m.chat = tg_thread.group_id AND m.thread = tg_thread.root
                     AND m.out = 0 AND m.service = 0
                     AND m.id > COALESCE(tg_thread.last_read, 0)),
                 draft
          FROM tg_thread WHERE chat = ?1 AND post = ?2",
    describe: "one post's comments: where they are, how many, how far I have read",
};

// One thread can be named twice: by the channel's post, and by the post's
// own copy in the group, which carries the same reply info. Both rows say
// the same thing and are written together, so either answers — but the
// channel's is the one worth having, being the way the panel is named.
static Q_THREAD_OF_ROOT: Q = Q {
    id: "telegram thread of root",
    sql: "SELECT chat, post, NULLIF(COALESCE(group_id, 0), 0), NULLIF(COALESCE(root, 0), 0),
                 count, NULLIF(COALESCE(last, 0), 0), NULLIF(COALESCE(last_read, 0), 0),
                 (SELECT COUNT(*) FROM tg_message m
                   WHERE m.chat = tg_thread.group_id AND m.thread = tg_thread.root
                     AND m.out = 0 AND m.service = 0
                     AND m.id > COALESCE(tg_thread.last_read, 0)),
                 draft
          FROM tg_thread WHERE group_id = ?1 AND root = ?2
          ORDER BY (chat = ?1), chat LIMIT 1",
    describe: "the post whose comments a discussion group's thread holds",
};

fn thread_row(r: &rusqlite::Row) -> rusqlite::Result<Thread> {
    Ok(Thread {
        chat: r.get(0)?,
        post: r.get(1)?,
        group: r.get(2)?,
        root: r.get(3)?,
        count: r.get(4)?,
        last: r.get(5)?,
        last_read: r.get(6)?,
        unread: r.get(7)?,
        draft: r.get(8)?,
    })
}

/// What is known about one post's comments, by the post.
#[must_use]
pub fn get(store: &Store, chat: PeerId, post: MsgId) -> Option<Thread> {
    store.rows(&Q_THREAD, &[Val::I(chat), Val::I(post)], thread_row).first().cloned()
}

/// The same row reached from the other side: a thread in a discussion group,
/// by the root its comments answer.
#[must_use]
pub fn of_root(store: &Store, group: PeerId, root: MsgId) -> Option<Thread> {
    store.rows(&Q_THREAD_OF_ROOT, &[Val::I(group), Val::I(root)], thread_row).first().cloned()
}

/// Which post's comments a line opens: the channel's own, where this line
/// is a copy of one and the store knows it, else the line itself. Both name
/// the same thread, and the channel's is the one both ways in agree on.
#[must_use]
pub fn way_in(store: &Store, chat: PeerId, id: MsgId) -> (PeerId, MsgId) {
    of_root(store, chat, id)
        .filter(|t| t.chat != chat)
        .map_or((chat, id), |t| (t.chat, t.post))
}

/// The card a comments panel wears.
///
/// The conversation is the **group's** — its rights, its mute, its block,
/// and whether it wants joining before it takes a message — because that is
/// where a comment is written. Only the name is the post's: *comments ·
/// Rust Weekly*, the channel being what a person came from. Until the wire
/// has said where the comments are there is no group to read, and the
/// panel stands on the chat it was opened from.
#[must_use]
pub fn card(store: &Store, chat: PeerId, post: MsgId) -> Option<PeerCard> {
    let thread = get(store, chat, post);
    let named = model::peer(store, chat)?;
    let mut card = thread
        .as_ref()
        .and_then(|t| t.group)
        .filter(|group| *group != chat)
        .and_then(|group| model::peer(store, group))
        .unwrap_or(named.clone());
    card.name = format!("comments · {}", named.name);
    card.draft = thread.as_ref().and_then(|t| t.draft.clone());
    card.last_read = thread.as_ref().and_then(|t| t.last_read);
    card.unread = thread.as_ref().map_or(0, |t| t.unread);
    card.unread_mentions = 0;
    card.typing = None;
    Some(card)
}

/// The composer's words for this thread, written where the thread is known,
/// and dated: an answer about the thread carries a draft of its own, and of
/// the two the newer stands ([`project_thread`](super::project::project_thread)).
///
/// # Errors
///
/// If the store refuses the write.
pub fn draft_tx(
    c: &Connection,
    group: PeerId,
    root: MsgId,
    text: &str,
    now: f64,
) -> rusqlite::Result<()> {
    c.execute(
        "UPDATE tg_thread SET draft = ?3, draft_date = ?4 WHERE group_id = ?1 AND root = ?2",
        rusqlite::params![group, root, (!text.trim().is_empty()).then_some(text), now],
    )?;
    Ok(())
}

/// Reading a thread through one of its comments. Forward only: a read is
/// never taken back by a later, older claim.
///
/// # Errors
///
/// If the store refuses the write.
pub fn read_tx(c: &Connection, group: PeerId, root: MsgId, through: MsgId) -> rusqlite::Result<()> {
    c.execute(
        "UPDATE tg_thread SET last_read = ?3
         WHERE group_id = ?1 AND root = ?2 AND ?3 > COALESCE(last_read, 0)",
        rusqlite::params![group, root, through],
    )?;
    Ok(())
}

/// What a post's foot says about its comments: nothing at all where there is
/// no discussion group behind it, `leave a comment` where nobody has yet,
/// else how many — and whether one has arrived since I read them.
///
/// The count is the post's own (`tg_message.comments`, which is
/// `reply_info`'s); *new* is a word rather than a number because only a
/// thread this device has asked about knows how many of them are unread, and
/// a number that is sometimes a guess is worse than a word that is always
/// true.
#[must_use]
pub fn foot(count: Option<i64>, new: bool) -> Option<String> {
    let count = count?;
    if count <= 0 {
        return Some("leave a comment".to_string());
    }
    let comments = format!("{count} comment{}", if count == 1 { "" } else { "s" });
    Some(if new { format!("{comments} · new") } else { comments })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_posts_foot_says_what_is_under_it() {
        assert_eq!(foot(None, false), None);
        assert_eq!(foot(Some(0), false).as_deref(), Some("leave a comment"));
        assert_eq!(foot(Some(1), false).as_deref(), Some("1 comment"));
        assert_eq!(foot(Some(8), false).as_deref(), Some("8 comments"));
        assert_eq!(foot(Some(8), true).as_deref(), Some("8 comments · new"));
    }

    #[test]
    fn a_thread_is_somewhere_only_once_the_wire_has_said_where() {
        let mut thread = Thread {
            chat: -100,
            post: 5,
            group: Some(-200),
            root: Some(9),
            count: 2,
            last: Some(12),
            last_read: None,
            unread: 2,
            draft: None,
        };
        assert_eq!(thread.where_it_is(), Some((-200, 9)));
        thread.group = None;
        assert_eq!(thread.where_it_is(), None);
    }
}
