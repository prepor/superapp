//! The search panel's telegram source: the chats and the people whose names
//! carry every word, then the messages whose text does.
//!
//! The names are sifted in memory — one row a peer — and the messages by
//! SQLite, round the cache: the parameter is the person's typing, and an
//! entry a keystroke nothing reads again is not worth keeping.

use kernel::search::{Abandoned, Hit, Provider};
use kernel::store::{Q, Store};

use super::model::{self, first_name, one_line, PeerId, PeerKind};
use super::panels::{Chat, Peer, Topics};

/// How many messages one question is worth showing.
const LIMIT: i64 = 100;

static Q_NAMES: Q = Q {
    id: "tg search names",
    sql: "SELECT p.id, p.kind, p.name, COALESCE(p.username, ''), COALESCE(p.status, ''),
                 p.is_self, p.is_contact, c.peer IS NOT NULL, p.blocked, p.is_forum
          FROM tg_peer p LEFT JOIN tg_chat c ON c.peer = p.id
          ORDER BY p.name",
    describe: "every peer's name, kind and presence, for the search panel",
};

struct Named {
    id: PeerId,
    kind: PeerKind,
    name: String,
    username: String,
    status: String,
    is_self: bool,
    is_contact: bool,
    has_chat: bool,
    blocked: bool,
    is_forum: bool,
}

fn named_row(r: &rusqlite::Row) -> rusqlite::Result<Named> {
    Ok(Named {
        id: r.get(0)?,
        kind: PeerKind::of(&r.get::<_, String>(1)?),
        name: r.get(2)?,
        username: r.get(3)?,
        status: r.get(4)?,
        is_self: r.get::<_, i64>(5)? != 0,
        is_contact: r.get::<_, i64>(6)? != 0,
        has_chat: r.get::<_, i64>(7)? != 0,
        blocked: r.get::<_, i64>(8)? != 0,
        is_forum: r.get(9)?,
    })
}

/// The messages a pattern reaches, latest first, service lines left out.
const MESSAGES_SQL: &str = "
    SELECT m.id, m.chat, COALESCE(t.name || ' · ', '') || p.name, COALESCE(s.name, ''), m.out, m.text, m.topic
    FROM tg_message m JOIN tg_peer p ON p.id = m.chat LEFT JOIN tg_peer s ON s.id = m.sender
    LEFT JOIN tg_topic t ON t.chat = m.chat AND t.id = m.topic
    WHERE m.service = 0 AND m.text LIKE ?1 ESCAPE '\\'
    ORDER BY m.date DESC, m.id DESC
    LIMIT ?2";

/// The telegram world as a search source.
pub struct TelegramSearch;

impl Provider for TelegramSearch {
    fn id(&self) -> &'static str {
        "telegram"
    }

    fn search(&self, store: &Store, query: &str, abandoned: &Abandoned) -> Vec<Hit> {
        let terms = kernel::search::terms(query);
        if terms.is_empty() {
            return Vec::new();
        }
        store.poll_external();
        let mut hits = matching_names(store, &terms);
        let rows = store.rows(&Q_TOPICS, &[], |r| Ok((r.get::<_, i64>(0)?, r.get::<_, i64>(1)?, r.get::<_, String>(2)?, r.get::<_, String>(3)?)));
        hits.extend(rows.iter().filter(|(_, _, name, group)| kernel::search::matches(&terms, &[name, group]))
            .map(|(chat, topic, name, group)| Hit::found(name, group, Chat::topic(*chat, *topic))));
        if abandoned.yes() {
            return hits;
        }
        hits.extend(matching_messages(store, query));
        hits
    }
}

/// The peers whose name or username carries every word. A person out of
/// the address book with no chat is offered only while blocked, so their
/// profile still provides a way to unblock them after deleting the chat.
fn matching_names(store: &Store, terms: &[String]) -> Vec<Hit> {
    store
        .rows(&Q_NAMES, &[], named_row)
        .iter()
        .filter(|n| n.is_self || n.is_contact || n.has_chat || n.blocked)
        .filter(|n| {
            kernel::search::matches(terms, &[&n.name, &n.username, n.kind.as_str()])
        })
        .map(|n| {
            let detail = if n.is_self {
                "saved messages".to_string()
            } else if n.blocked {
                "blocked".to_string()
            } else {
                match n.kind {
                    PeerKind::Person => model::presence(Some(n.status.as_str())),
                    PeerKind::Group => "group".to_string(),
                    PeerKind::Channel => "channel".to_string(),
                }
            };
            let target = if n.blocked && !n.has_chat { Peer::id(n.id) }
                else if n.is_forum { Topics::id(n.id) }
                else { Chat::id(n.id) };
            Hit::found(&n.name, detail, target)
        })
        .collect()
}

/// The messages whose text carries the query as typed, latest first. Each
/// opens its chat at that line.
fn matching_messages(store: &Store, query: &str) -> Vec<Hit> {
    let q = query.trim();
    if q.is_empty() {
        return Vec::new();
    }
    let pat = format!(
        "%{}%",
        q.replace('\\', "\\\\").replace('%', "\\%").replace('_', "\\_")
    );
    let mut stmt = match store.conn().prepare_cached(MESSAGES_SQL) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("search: preparing the telegram messages failed: {e}");
            return Vec::new();
        }
    };
    let rows = stmt.query_map(rusqlite::params![pat, LIMIT], |r| {
        Ok((
            r.get::<_, i64>(0)?,
            r.get::<_, PeerId>(1)?,
            r.get::<_, String>(2)?,
            r.get::<_, String>(3)?,
            r.get::<_, i64>(4)? != 0,
            r.get::<_, String>(5)?,
            r.get::<_, i64>(6)?,
        ))
    });
    let rows = match rows {
        Ok(rows) => rows,
        Err(e) => {
            eprintln!("search: the telegram messages refused {q:?}: {e}");
            return Vec::new();
        }
    };
    rows.filter_map(Result::ok)
        .map(|(id, chat, title, sender, out, text, topic)| {
            let who = if out { "me" } else { first_name(&sender) };
            let detail = if who.is_empty() || who == title {
                title.clone()
            } else {
                format!("{who} in {title}")
            };
            Hit::found(one_line(&text), detail, Chat::topic_at(chat, topic, id))
        })
        .collect()
}

static Q_TOPICS: Q = Q {
    id: "telegram search topics",
    sql: "SELECT t.chat, t.id, t.name, p.name FROM tg_topic t
        JOIN tg_peer p ON p.id = t.chat JOIN tg_chat c ON c.peer = t.chat
        WHERE t.selected = 1 AND p.is_forum = 1 AND (c.in_main = 1 OR c.archived = 1)",
    describe: "selected topics by name and group",
};
