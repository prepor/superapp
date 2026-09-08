//! The durable owner of message reactions. Message bodies only seed unknown
//! counts; live updates and reconciled server reads replace them. A known
//! empty row is a tombstone, distinct from metadata we haven't received.

use rusqlite::{Connection, OptionalExtension};

use super::model::{MsgId, PeerId};

#[derive(Default)]
pub(super) struct State {
    pub revision: i64,
    pub refresh: bool,
}

pub(super) fn state(c: &Connection, chat: PeerId, message: MsgId) -> rusqlite::Result<State> {
    c.query_row("SELECT revision, refresh FROM tg_message_reaction WHERE chat = ?1 AND message = ?2",
        (chat, message), |r| Ok(State { revision: r.get(0)?, refresh: r.get(1)? }))
        .optional().map(|r| r.unwrap_or(State { revision: 0, refresh: true }))
}

/// Cached history is useful on first load, but never newer than a reaction
/// update. In particular neither a missing field nor an old positive count
/// can overwrite a removal or revive a previous count.
pub(super) fn seed(c: &Connection, chat: PeerId, message: MsgId, counts: Option<&str>) -> rusqlite::Result<()> {
    let Some(counts) = counts else { return Ok(()); };
    c.execute("INSERT INTO tg_message_reaction(chat, message, counts, known, revision)
        VALUES(?1, ?2, ?3, 1, 1) ON CONFLICT(chat, message) DO UPDATE SET
        counts = excluded.counts, known = 1, revision = tg_message_reaction.revision + 1
        WHERE NOT tg_message_reaction.known", (chat, message, counts))?;
    Ok(())
}

pub(super) fn refresh(c: &Connection, chat: PeerId, message: MsgId) -> rusqlite::Result<()> {
    c.execute("INSERT INTO tg_message_reaction(chat, message) VALUES(?1, ?2)
        ON CONFLICT(chat, message) DO UPDATE SET refresh = 1 WHERE NOT refresh", (chat, message))?;
    Ok(())
}

/// Apply an explicit reaction list, including an explicitly empty list.
/// This also works before the chat or message body has been projected.
pub(super) fn set(c: &Connection, chat: PeerId, message: MsgId, counts: Option<&str>) -> rusqlite::Result<()> {
    c.execute("INSERT INTO tg_message_reaction(chat, message, counts, known, revision, refresh)
        VALUES(?1, ?2, ?3, 1, 1, 0) ON CONFLICT(chat, message) DO UPDATE SET
        counts = excluded.counts, known = 1, revision = tg_message_reaction.revision + 1, refresh = 0",
        (chat, message, counts))?;
    // Keep the legacy column readable to older app versions. Current readers
    // join the owner above, so an older process cannot overwrite these counts.
    c.execute("UPDATE tg_message SET reactions = ?3 WHERE chat = ?1 AND id = ?2", (chat, message, counts))?;
    Ok(())
}

/// A network reply may replace only the revision it was requested against.
/// Pending request identities live in the worker, never in replicated data.
pub(super) fn reconcile(c: &Connection, chat: PeerId, message: MsgId, revision: i64,
    counts: Option<&str>) -> rusqlite::Result<bool>
{
    if state(c, chat, message)?.revision != revision { return Ok(false); }
    set(c, chat, message, counts)?;
    Ok(true)
}
