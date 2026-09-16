//! Forum metadata and this app's persistent topic selection.

use kernel::store::{Store, Val, Q};
use rusqlite::Connection;

use super::model::{self, MsgId, PeerCard, PeerId, Scope};

#[derive(Debug, Clone, PartialEq)]
pub struct Topic {
    pub chat: PeerId,
    pub id: i64,
    pub name: String,
    pub selected: bool,
    pub closed: bool,
    pub hidden: bool,
    pub unread: i64,
    pub unread_mentions: i64,
    pub muted: bool,
    pub last_read: Option<MsgId>,
    pub draft: Option<String>,
    pub pinned: i64,
    pub archived: bool,
}

static Q_TOPICS: Q = Q {
    id: "telegram topics",
    sql: "SELECT t.chat, t.id, t.name, t.selected, t.closed, t.hidden, t.unread, t.mention,
                 CASE WHEN t.mute_default = 1 THEN COALESCE(c.muted, 0) ELSE t.muted END,
                 t.last_read, t.draft, t.pinned, t.archived
          FROM tg_topic t LEFT JOIN tg_chat c ON c.peer = t.chat
          WHERE t.chat = ?1 ORDER BY t.name COLLATE NOCASE, t.id",
    describe: "a forum's topics, including those hidden from the chat list",
};

pub fn list(store: &Store, chat: PeerId) -> std::rc::Rc<Vec<Topic>> {
    store.rows(&Q_TOPICS, &[Val::I(chat)], topic_row)
}

/// The topic picker can retain its previous snapshot while SQLite refreshes.
pub fn snapshot(store: &Store, chat: PeerId) -> std::rc::Rc<Vec<Topic>> {
    store.snapshot_rows_sql_deps(Q_TOPICS.id, Q_TOPICS.describe, Q_TOPICS.sql,
        &[Val::I(chat)], &[], topic_row)
}

fn topic_row(r: &rusqlite::Row) -> rusqlite::Result<Topic> {
        Ok(Topic {
            chat: r.get(0)?,
            id: r.get(1)?,
            name: r.get(2)?,
            selected: r.get(3)?,
            closed: r.get(4)?,
            hidden: r.get(5)?,
            unread: r.get(6)?,
            unread_mentions: r.get(7)?,
            muted: r.get(8)?,
            last_read: r.get(9)?,
            draft: r.get(10)?,
            pinned: r.get(11)?,
            archived: r.get(12)?,
        })
}

pub fn get(store: &Store, chat: PeerId, id: i64) -> Option<Topic> {
    let sql = Q_TOPICS.sql.replace("WHERE t.chat = ?1 ORDER BY t.name COLLATE NOCASE, t.id",
        "WHERE t.chat = ?1 AND t.id = ?2");
    store.rows_sql("telegram topic", "one forum topic", &sql,
        &[Val::I(chat), Val::I(id)], topic_row).first().cloned()
}

/// The card a panel standing in one part of a chat draws: the chat's own,
/// with the part's name, draft and reading over it. A thread's card is the
/// group's — the comments are written there — and
/// [`threads::card`](super::threads::card) puts the post's name on it.
pub fn card(store: &Store, chat: PeerId, scope: Scope) -> Option<PeerCard> {
    let mut card = model::peer(store, chat)?;
    match scope {
        Scope::Whole => {}
        Scope::Topic(topic) => {
            let t = get(store, chat, topic)?;
            card.name = format!("{} · {}", t.name, card.name);
            card.draft = t.draft;
            card.unread = t.unread;
            card.unread_mentions = t.unread_mentions;
            card.last_read = t.last_read;
            card.muted = t.muted;
            card.pinned = t.pinned;
            card.archived = t.archived;
            card.typing = None;
        }
        Scope::Thread(root) => {
            let t = super::threads::of_root(store, chat, root)?;
            card.draft = t.draft;
            card.last_read = t.last_read;
            card.unread = t.unread;
            card.unread_mentions = 0;
            card.typing = None;
        }
    }
    Some(card)
}

pub fn select_tx(
    c: &Connection,
    chat: PeerId,
    ids: &[i64],
    selected: bool,
) -> rusqlite::Result<()> {
    for id in ids {
        c.execute(
            "UPDATE tg_topic SET selected = ?3 WHERE chat = ?1 AND id = ?2",
            rusqlite::params![chat, id, selected],
        )?;
    }
    Ok(())
}

/// `now` dates a thread's draft, which is weighed against the one the wire
/// answers with; a chat's and a topic's are the wire's own to overwrite.
pub fn draft_tx(
    c: &Connection,
    chat: PeerId,
    scope: Scope,
    text: &str,
    now: f64,
) -> rusqlite::Result<()> {
    let topic = match scope {
        Scope::Whole => return model::set_draft_tx(c, chat, text),
        Scope::Thread(root) => return super::threads::draft_tx(c, chat, root, text, now),
        Scope::Topic(topic) => topic,
    };
    c.execute(
        "UPDATE tg_topic SET draft = ?3 WHERE chat = ?1 AND id = ?2",
        rusqlite::params![chat, topic, (!text.trim().is_empty()).then_some(text)],
    )?;
    Ok(())
}

pub fn read_tx(c: &Connection, chat: PeerId, scope: Scope, through: MsgId) -> rusqlite::Result<()> {
    let topic = match scope {
        Scope::Whole => return model::mark_read_tx(c, chat, through),
        Scope::Thread(root) => return super::threads::read_tx(c, chat, root, through),
        Scope::Topic(topic) => topic,
    };
    c.execute("UPDATE tg_topic SET unread = MAX(
        (SELECT COUNT(*) FROM tg_message
         WHERE chat = ?1 AND topic = ?2 AND id > ?3 AND out = 0 AND service = 0),
        unread - (SELECT COUNT(*) FROM tg_message
                  WHERE chat = ?1 AND topic = ?2 AND id > COALESCE(tg_topic.last_read, 0)
                    AND id <= ?3 AND out = 0 AND service = 0)),
        last_read = ?3
        WHERE chat = ?1 AND id = ?2 AND ?3 > COALESCE(last_read, 0)", [chat, topic, through])?;
    Ok(())
}

pub fn parse_key(key: &str) -> Option<(PeerId, i64)> {
    let (chat, topic) = key.split_once(':')?;
    Some((chat.parse().ok()?, topic.parse().ok()?))
}
