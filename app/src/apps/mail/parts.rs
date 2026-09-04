//! What a letter carries: the `attachment` rows derived from its `raw`, the
//! bytes read back out of it, and where a part lands when it is opened.
//!
//! The bytes are **not** stored twice. A letter's `raw` already holds every
//! part; a row here is the description a list and a card need — name, media
//! type, size, the Content-ID an inline part wears — plus `part`, the index
//! [`part_bytes`](super::sync::part_bytes) reads the bytes back by. A second
//! copy of every attachment in the mailbox is exactly the cost this design
//! refuses.
//!
//! The rows are derived, so they are versioned by the walk that made them
//! ([`ATTACH_VERSION`]) rather than by the schema counter, and
//! `attachment_scan` writes that version down **per mail**: a letter that
//! arrives through replication has a `raw` nobody has walked, and this is what
//! notices.

use std::path::PathBuf;
use std::rc::Rc;

use kernel::caps::{fmt_size, FileKind};
use kernel::store::{Store, Val, Q};

use super::model::{self, MailId};

/// Which walk over `raw` the stored rows came out of. Bump it and every
/// store re-derives every letter's parts on its next open.
pub const ATTACH_VERSION: i64 = 1;

/// One part of a letter, as the panels see it: the row
/// [`sync`](super::sync) derived from the mail's `raw`. The bytes are not
/// here — [`part`] reads them back out of the letter when a card asks.
#[derive(Debug, Clone, PartialEq)]
pub struct Attachment {
    pub message: MailId,
    /// Which part of the letter it is (see
    /// [`part_bytes`](super::sync::part_bytes)).
    pub at: u32,
    pub name: String,
    pub mime: String,
    pub size: u64,
    /// Its Content-ID, for a part the reading refers to; empty otherwise.
    pub cid: String,
}

impl Attachment {
    /// What the message row's link says: the name, and how big it is.
    #[must_use]
    pub fn label(&self) -> String {
        format!("{} · {}", self.name, fmt_size(self.size))
    }

    /// What it is, as the card's kind line words it.
    #[must_use]
    pub fn kind(&self) -> FileKind {
        FileKind::of_name(&self.name)
    }

    /// The identity of the card over it: `("attachment", [mail, at])`. The
    /// letter and its place in it, because a row's own id is derived and
    /// local to a device.
    #[must_use]
    pub fn panel(&self) -> kernel::panel::PanelId {
        super::panels::Card::id(self.message, self.at)
    }
}

static Q_ATTACHMENTS: Q = Q {
    id: "attachments",
    sql: "SELECT message, part, name, mime, size, cid
          FROM attachment WHERE message = ?1 ORDER BY part",
    describe: "the parts one letter carries, in the order they arrived",
};

static Q_ATTACHMENT: Q = Q {
    id: "attachment",
    sql: "SELECT message, part, name, mime, size, cid
          FROM attachment WHERE message = ?1 AND part = ?2",
    describe: "one part of a letter, by the letter and its place in it",
};

static Q_CARRIERS: Q = Q {
    id: "thread carriers",
    sql: "SELECT DISTINCT a.message FROM attachment a
          JOIN message m ON m.id = a.message
          WHERE m.thread = (SELECT thread FROM message WHERE id = ?1)",
    describe: "which mails of a conversation carry anything",
};

fn attachment_row(r: &rusqlite::Row) -> rusqlite::Result<Attachment> {
    Ok(Attachment {
        message: r.get(0)?,
        at: r.get::<_, i64>(1)? as u32,
        name: r.get(2)?,
        mime: r.get(3)?,
        size: r.get::<_, i64>(4)? as u64,
        cid: r.get(5)?,
    })
}

/// The parts one letter carries.
#[must_use]
pub fn attachments(store: &Store, id: MailId) -> Rc<Vec<Attachment>> {
    store.rows(&Q_ATTACHMENTS, &[Val::I(id)], attachment_row)
}

/// One part, by the letter and its place in it — the identity an
/// `attachment` panel persists.
#[must_use]
pub fn attachment(store: &Store, mail: MailId, at: u32) -> Option<Attachment> {
    store
        .rows(
            &Q_ATTACHMENT,
            &[Val::I(mail), Val::I(i64::from(at))],
            attachment_row,
        )
        .first()
        .cloned()
}

/// Which mails of a conversation carry anything — what the reader's height
/// wish adds a line for.
#[must_use]
pub fn thread_carriers(store: &Store, id: MailId) -> std::collections::BTreeSet<MailId> {
    store
        .rows(&Q_CARRIERS, &[Val::I(id)], |r| r.get::<_, MailId>(0))
        .iter()
        .copied()
        .collect()
}

/// One part's bytes, out of the letter that carries them. Reads the whole
/// `raw` and walks its MIME, so it belongs on a thread and never in a draw;
/// [`pictures`](super::widgets::pictures) asks for it the way it asks for a
/// letter's own images.
#[must_use]
pub fn part(store: &Store, a: &Attachment) -> Option<Vec<u8>> {
    super::sync::part_bytes(&model::raw(store, a.message)?, a.at)
}

/// Records what a letter carries, and marks the mail walked at this build's
/// version. One transaction with the ingest that stored the letter, so no
/// draw ever sees a mail without its parts.
///
/// An **upsert on `(message, part)`**, not a delete and a re-insert: a part
/// is what an `attachment` panel persists, and a re-derive — the walk's
/// version changed, or a peer's snapshot landed — must not hand that place to
/// a different part of a different letter. Parts the letter no longer has go
/// afterwards, which is the only thing a re-derive may take away.
///
/// # Errors
///
/// If the store refuses the write.
pub fn attach_tx(
    c: &rusqlite::Connection,
    message: MailId,
    parts: &[super::sync::Part],
) -> rusqlite::Result<()> {
    for p in parts {
        c.execute(
            "INSERT INTO attachment(message, part, name, mime, size, cid)
             VALUES(?1,?2,?3,?4,?5,?6)
             ON CONFLICT(message, part) DO UPDATE SET
               name = excluded.name, mime = excluded.mime,
               size = excluded.size, cid = excluded.cid",
            rusqlite::params![
                message,
                i64::from(p.at),
                p.name,
                p.mime,
                p.size as i64,
                p.cid
            ],
        )?;
    }
    // Interpolated rather than bound: the values are part indices this build
    // just read off a MIME walk, and `NOT IN ()` is not a thing — a letter
    // that carries nothing has every row of its own to drop.
    let kept: Vec<String> = parts.iter().map(|p| p.at.to_string()).collect();
    let sql = if kept.is_empty() {
        "DELETE FROM attachment WHERE message = ?1".to_string()
    } else {
        format!(
            "DELETE FROM attachment WHERE message = ?1 AND part NOT IN ({})",
            kept.join(",")
        )
    };
    c.execute(&sql, [message])?;
    mark_scanned_tx(c, message)
}

/// Notes that this mail's `raw` has been walked at the current version —
/// including the answer "there was nothing to walk", which is why it is its
/// own function.
///
/// # Errors
///
/// If the store refuses the write.
pub fn mark_scanned_tx(c: &rusqlite::Connection, message: MailId) -> rusqlite::Result<()> {
    c.execute(
        "INSERT INTO attachment_scan(message, version) VALUES(?1, ?2)
         ON CONFLICT(message) DO UPDATE SET version = excluded.version",
        rusqlite::params![message, ATTACH_VERSION],
    )?;
    Ok(())
}

/// Derives the rows of every mail nobody has walked at this version.
///
/// The schema's [`Step::Derived`](kernel::app::Step) runs it when the version
/// moves; the sender pass runs it every turn, because a letter that arrives
/// through replication runs no ingest code and its `raw` is nobody's to walk
/// until somebody looks.
///
/// The anti-join is what makes running it every turn affordable. A
/// `WHERE raw IS NOT NULL` would decode every record as far as the letter to
/// answer, over the whole mailbox, every time; driving off `attachment_scan`
/// instead reads no letter at all once they have all been walked. A mail
/// *without* raw gets its scan row too — nothing to walk is an answer, and it
/// should be given once.
///
/// # Errors
///
/// If the store refuses a read or a write.
pub fn scan(conn: &rusqlite::Connection) -> rusqlite::Result<()> {
    // Rows whose letter is gone go first, and not only for tidiness: SQLite
    // hands a fresh `message` the lowest free rowid, so a stale
    // `attachment_scan` row would tell the walk below that a letter it has
    // never seen was already walked — and stale `attachment` rows would be
    // listed under it.
    for t in ["attachment", "attachment_scan"] {
        conn.execute(
            &format!(
                "DELETE FROM {t} WHERE NOT EXISTS
                   (SELECT 1 FROM message m WHERE m.id = {t}.message)"
            ),
            [],
        )?;
    }
    let rows: Vec<(MailId, Option<Vec<u8>>)> = conn
        .prepare(
            "SELECT m.id, m.raw FROM message m
             LEFT JOIN attachment_scan s ON s.message = m.id
             WHERE s.version IS NULL OR s.version != ?1",
        )?
        .query_map([ATTACH_VERSION], |r| Ok((r.get(0)?, r.get(1)?)))?
        .collect::<rusqlite::Result<_>>()?;
    for (id, raw) in rows {
        // No raw is not the same as no parts: a seeded letter writes its own
        // rows and this must not take them away, so only a mail there *is*
        // something to walk is rewritten.
        match raw {
            Some(raw) => attach_tx(conn, id, &super::sync::parse_mail(&raw).attachments)?,
            None => mark_scanned_tx(conn, id)?,
        }
    }
    Ok(())
}

// -- where a part lands ------------------------------------------------------

/// Where a letter's part lands when it is opened: the app's own scratch
/// directory, **a folder per part**, so nothing can be overwritten by
/// anything — not another letter's part, and not this letter's second
/// `image.png`, which is a shape mail actually arrives in. The folder carries
/// the disambiguation so the file keeps the name the sender gave it, which is
/// the name the viewer will put in its title bar. An ordinary directory
/// either way, so a files panel can walk to it afterwards.
#[must_use]
pub fn scratch(mail: MailId, at: u32, name: &str) -> PathBuf {
    std::env::temp_dir()
        .join("superapp-parts")
        .join(format!("mail-{mail}"))
        .join(format!("part-{at}"))
        // A part's filename comes off the wire: the last segment of it is
        // all that may reach the disk, and never `..`.
        .join(safe_name(name))
}

/// A filename from outside as a single, harmless segment: no separators, no
/// climbing, never empty.
#[must_use]
pub fn safe_name(name: &str) -> String {
    let last = name.rsplit(['/', '\\']).next().unwrap_or("").trim();
    if last.is_empty() || last == "." || last == ".." {
        "part".to_string()
    } else {
        last.to_string()
    }
}
