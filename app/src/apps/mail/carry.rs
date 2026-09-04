//! What a letter will carry: the rows behind a compose sheet's `CARRIES`
//! line, and the two writes that put a file there and take it off again.
//!
//! The **path**, not the bytes: an attach costs a `stat`, the file stays
//! where it is, and the send is what reads it — so a draft that sits for a
//! day carries the file as it is when it leaves, and a file that has moved
//! fails the send honestly rather than going out stale.
//!
//! A row also records **which install picked the file**, because
//! `~/Downloads/report-q3.pdf` is a different file on the other machine and
//! these rows replicate: the send refuses one attached elsewhere rather than
//! carrying out whatever happens to sit at that path here.

use std::rc::Rc;

use kernel::caps::fmt_size;
use kernel::store::{Store, Val, Q};

use super::model::Seed;

/// One file a draft will carry out.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DraftFile {
    /// The display path (`~/Downloads/report-q3.pdf`).
    pub path: String,
    pub name: String,
    pub size: u64,
    /// Which install attached it (`repl.device`).
    pub device: String,
}

impl DraftFile {
    /// What the `CARRIES` line says: the name, and how big it is.
    #[must_use]
    pub fn label(&self) -> String {
        format!("{} · {}", self.name, fmt_size(self.size))
    }
}

static Q_FILES: Q = Q {
    id: "draft files",
    sql: "SELECT f.path, f.name, f.size, f.device FROM draft_attachment f
          LEFT JOIN draft d ON d.panel = f.panel
          WHERE f.panel = ?1
            AND COALESCE(d.re_message, -1) = ?2
            AND COALESCE(d.fwd_message, -1) = ?3
          ORDER BY f.added, f.id",
    describe: "what one compose panel will carry out, in the order it was attached",
};

fn draft_file_row(r: &rusqlite::Row) -> rusqlite::Result<DraftFile> {
    Ok(DraftFile {
        path: r.get(0)?,
        name: r.get(1)?,
        size: r.get::<_, i64>(2)? as u64,
        device: r.get(3)?,
    })
}

/// What a compose panel will carry out, if the rows are `seed`'s own — the
/// rule [`draft_for`](super::model::draft_for) holds the text to. A compose
/// retargeted in place keeps its slot, so the files a reply left are not the
/// forward's; a panel with no draft row yet has nothing to disagree, which is
/// what lets *attach* land before the first keystroke.
#[must_use]
pub fn files(store: &Store, slot: i64, seed: Seed) -> Rc<Vec<DraftFile>> {
    store.rows(
        &Q_FILES,
        &[
            Val::I(slot),
            Val::I(seed.in_reply_to().unwrap_or(-1)),
            Val::I(seed.forwards().unwrap_or(-1)),
        ],
        draft_file_row,
    )
}

/// The same, whatever the panel is showing now — what a send reads, since
/// the outbox row's draft is the one that was filed.
///
/// # Errors
///
/// If the store refuses the read.
pub fn all(store: &rusqlite::Connection, slot: i64) -> rusqlite::Result<Vec<DraftFile>> {
    store
        .prepare(
            "SELECT path, name, size, device FROM draft_attachment
             WHERE panel = ?1 ORDER BY added, id",
        )?
        .query_map([slot], draft_file_row)?
        .collect()
}

/// Attaches files to a draft and answers the ones it actually added — what
/// [`Attached`](super::effects::Attached) must take back off, and no more: a
/// path the draft already carried was not this action's to take away.
///
/// # Errors
///
/// If the store refuses the write.
pub fn attach_tx(
    c: &rusqlite::Connection,
    slot: i64,
    files: &[DraftFile],
    now: f64,
) -> rusqlite::Result<Vec<DraftFile>> {
    let mut added = Vec::new();
    for f in files {
        // The same file twice is one attachment, in the place it already
        // had: a second *attach* of an overlapping set is not two copies in
        // the envelope, and not a reordering either.
        let held: bool = c
            .query_row(
                "SELECT 1 FROM draft_attachment WHERE panel = ?1 AND path = ?2",
                rusqlite::params![slot, f.path],
                |_| Ok(true),
            )
            .unwrap_or(false);
        if held {
            continue;
        }
        c.execute(
            "INSERT INTO draft_attachment(panel, path, name, size, added, device)
             VALUES(?1,?2,?3,?4,?5,?6)",
            rusqlite::params![slot, f.path, f.name, f.size as i64, now, f.device],
        )?;
        added.push(f.clone());
    }
    Ok(added)
}

/// Takes paths back off a draft — the reverse of an attach.
///
/// # Errors
///
/// If the store refuses the write.
pub fn detach_tx(c: &rusqlite::Connection, slot: i64, paths: &[String]) -> rusqlite::Result<()> {
    for path in paths {
        c.execute(
            "DELETE FROM draft_attachment WHERE panel = ?1 AND path = ?2",
            rusqlite::params![slot, path],
        )?;
    }
    Ok(())
}

/// A draft's files go with the draft: a discard, and a send once it has
/// left.
///
/// # Errors
///
/// If the store refuses the write.
pub fn discard_tx(c: &rusqlite::Connection, slot: i64) -> rusqlite::Result<()> {
    c.execute("DELETE FROM draft_attachment WHERE panel = ?1", [slot])?;
    Ok(())
}
