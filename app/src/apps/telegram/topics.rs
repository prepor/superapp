//! Forum metadata and this app's persistent topic selection.

use kernel::store::{Store, Val, Q};
use rusqlite::Connection;

use super::model::{self, MsgId, PeerCard, PeerId};

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
    store.rows(&Q_TOPICS, &[Val::I(chat)], |r| {
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
    })
}

pub fn get(store: &Store, chat: PeerId, id: i64) -> Option<Topic> {
    list(store, chat).iter().find(|t| t.id == id).cloned()
}

pub fn card(store: &Store, chat: PeerId, topic: i64) -> Option<PeerCard> {
    let mut card = model::peer(store, chat)?;
    if topic != 0 {
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

pub fn draft_tx(c: &Connection, chat: PeerId, topic: i64, text: &str) -> rusqlite::Result<()> {
    if topic == 0 {
        return model::set_draft_tx(c, chat, text);
    }
    c.execute(
        "UPDATE tg_topic SET draft = ?3 WHERE chat = ?1 AND id = ?2",
        rusqlite::params![chat, topic, (!text.trim().is_empty()).then_some(text)],
    )?;
    Ok(())
}

pub fn read_tx(c: &Connection, chat: PeerId, topic: i64, through: MsgId) -> rusqlite::Result<()> {
    if topic == 0 {
        return model::mark_read_tx(c, chat, through);
    }
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
