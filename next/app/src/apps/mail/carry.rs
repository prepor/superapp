//! What a letter will carry: the rows behind a compose sheet's `CARRIES`
//! line, and the two writes that put a file there and take it off again.
//!
//! A path on this machine and nothing else. The prototype has no MIME, so
//! nothing is read off the disk and a send leaves these behind — what the
//! sheet says it would carry is the whole of it, and the port brings the
//! parts.

use std::rc::Rc;

use kernel::store::{Q, Store, Val};

use super::model::Seed;

/// One file a draft carries.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DraftFile {
    pub path: String,
}

impl DraftFile {
    /// What the `CARRIES` line calls it: the last segment of the path.
    #[must_use]
    pub fn label(&self) -> String {
        file_name(&self.path).to_string()
    }
}

/// The last segment of a path — mail's own, because a compose has to name
/// what it carries whether or not the files app is in the build.
#[must_use]
pub fn file_name(path: &str) -> &str {
    path.trim_end_matches('/')
        .rsplit('/')
        .next()
        .filter(|s| !s.is_empty())
        .unwrap_or(path)
}

static Q_FILES: Q = Q {
    id: "draft files",
    sql: "SELECT f.path FROM draft_file f
          LEFT JOIN draft d ON d.panel = f.panel
          WHERE f.panel = ?1
            AND COALESCE(d.re_message, -1) = ?2
            AND COALESCE(d.fwd_message, -1) = ?3
          ORDER BY f.added, f.path",
    describe: "what one compose panel will carry out, in the order it was attached",
};

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
        |r| Ok(DraftFile { path: r.get(0)? }),
    )
}

/// Attaches paths to a draft and answers the ones it actually added — what
/// [`Attached`](super::effects::Attached) must take back off, and no more: a
/// path the draft already carried was not this action's to take away.
///
/// # Errors
///
/// If the store refuses the write.
pub fn attach_tx(
    c: &rusqlite::Connection,
    slot: i64,
    paths: &[String],
    now: f64,
) -> rusqlite::Result<Vec<String>> {
    let mut added = Vec::new();
    for path in paths {
        // The same file twice is one attachment, in the place it already
        // had.
        let n = c.execute(
            "INSERT OR IGNORE INTO draft_file(panel, path, added) VALUES(?1, ?2, ?3)",
            rusqlite::params![slot, path, now],
        )?;
        if n > 0 {
            added.push(path.clone());
        }
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
            "DELETE FROM draft_file WHERE panel = ?1 AND path = ?2",
            rusqlite::params![slot, path],
        )?;
    }
    Ok(())
}
