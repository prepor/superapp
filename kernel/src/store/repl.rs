//! The store's replication half: the two tables device sync keeps, the set
//! of tables a write records, and the paths that run on a follower.
//!
//! Two of those paths go round [`Store::write`] on purpose. A follower that
//! applied peer frames through the ordinary gate would recapture and
//! republish each one, every applied frame echoing back into its own log
//! forever; and its own bookkeeping — the epoch, the high-water mark, an
//! installed snapshot — has to run even while ordinary writes are closed.
//! Both go through the same single writer thread, so they are still
//! serialised against everything else.

use std::path::Path;
use std::sync::mpsc;

use rusqlite::session::ConflictAction;
use rusqlite::{Connection, Transaction};

use super::{gone, Db, Erased, Job, RawFn, Store};

/// The replication log and this install's local state.
///
/// `repl_log` is a **queue that drains and prunes**, not a durable changeset
/// table — the SQLite session extension records it, and nothing migrates
/// through it. `repl` is local-only, never replicated: it holds this
/// install's stable device id and its sequence counters, so two devices
/// never share an id and a follower never adopts the holder's.
///
/// `repl_log.seq` is fed from `repl.next_local_seq`, **not** a bare rowid — a
/// snapshot install clears `repl_log` while `repl` survives, and SQLite would
/// otherwise reassign rowids from 1 and make a fresh row look long published.
///
/// Applied by presence rather than by the counter, so a store that turns up
/// without them gains them at its next open.
pub(super) const SCHEMA_REPL: &str = "
CREATE TABLE IF NOT EXISTS repl_log(
  seq       INTEGER PRIMARY KEY,      -- local order, from repl.next_local_seq
  pub_seq   INTEGER,                  -- global seq at publish; NULL until then
  ts        REAL NOT NULL,
  changeset BLOB NOT NULL
);
CREATE INDEX IF NOT EXISTS idx_repl_log_pending ON repl_log(seq) WHERE pub_seq IS NULL;

CREATE TABLE IF NOT EXISTS repl(
  id      INTEGER PRIMARY KEY CHECK(id = 1),
  device  TEXT NOT NULL,              -- stable per install; survives snapshots
  epoch   INTEGER NOT NULL DEFAULT 0,
  next_local_seq   INTEGER NOT NULL DEFAULT 1,  -- monotone for the device's life
  materialized_seq INTEGER NOT NULL DEFAULT 0,  -- global seq contained through
  holding INTEGER NOT NULL DEFAULT 0,
  -- What the last pass made of this device, and why it failed if it did.
  -- Written here rather than kept in memory so the unreachable-bucket
  -- problem is what every other problem is: a row, derived.
  role    TEXT NOT NULL DEFAULT '',
  note    TEXT
);
";

/// Replication's own two tables — never in a changeset, so a frame a
/// follower *applies* is never recaptured and never echoes back into its own
/// log.
const REPL_TABLES: [&str; 2] = ["repl", "repl_log"];

/// The tables a write's session records: everything in `schema` a peer
/// device must be told about — every app's, whichever apps this build has —
/// less replication's own bookkeeping.
///
/// Discovered rather than listed: the kernel names no app's tables, and the
/// set cannot change after `migrate` has run, so it is read once per open.
/// `PRAGMA table_list` classifies views, virtual tables and the shadow
/// tables under them, none of which a changeset may carry.
pub(super) fn replicated_tables(conn: &Connection, schema: &str) -> rusqlite::Result<Vec<String>> {
    let mut stmt = conn.prepare("PRAGMA table_list")?;
    let rows = stmt.query_map([], |r| {
        Ok((
            r.get::<_, String>(0)?,
            r.get::<_, String>(1)?,
            r.get::<_, String>(2)?,
        ))
    })?;
    let mut out = Vec::new();
    for row in rows {
        let (sch, name, kind) = row?;
        if sch != schema || kind != "table" {
            continue;
        }
        if name.starts_with("sqlite_") || REPL_TABLES.contains(&name.as_str()) {
            continue;
        }
        out.push(name);
    }
    out.sort();
    Ok(out)
}

/// Says so, once at open, about any replicated table with no primary key.
///
/// The session extension records nothing for such a table — silently — so a
/// row written there would live on one device only. With a hand-written list
/// of tables that was somebody's job to remember; with a discovered one it
/// is worth a line, because the failure is otherwise invisible until two
/// devices disagree.
pub(super) fn warn_unkeyed(conn: &Connection, tables: &[String]) {
    for t in tables {
        let keyed = conn
            .prepare(&format!("PRAGMA table_info(\"{t}\")"))
            .and_then(|mut s| {
                s.query_map([], |r| r.get::<_, i64>(5))
                    .map(|rows| rows.filter_map(Result::ok).any(|pk| pk > 0))
            });
        if matches!(keyed, Ok(false)) {
            eprintln!(
                "store: {t} has no primary key, so device sync cannot record its rows"
            );
        }
    }
}

impl Db {
    /// A replication-internal operation on the raw connection — no session,
    /// no `writable` gate. The closure owns its own transactions. This is
    /// how the sync engine reads and advances `repl`, applies batches, and
    /// installs snapshots, work that must run even while ordinary writes are
    /// closed.
    ///
    /// # Errors
    ///
    /// Whatever the closure returned, or a dead writer thread.
    pub(crate) fn raw<T: Send + 'static>(
        &self,
        f: impl FnOnce(&Connection) -> rusqlite::Result<T> + Send + 'static,
    ) -> rusqlite::Result<T> {
        let (reply, rx) = mpsc::channel();
        let run: RawFn = Box::new(move |c| f(c).map(|v| Box::new(v) as Erased));
        self.jobs.send(Job::Raw { run, reply }).map_err(|_| gone())?;
        let erased = rx.recv().map_err(|_| gone())??;
        Ok(*erased.downcast::<T>().expect("raw result type"))
    }

    /// Applies a peer changeset. Private on purpose: a follower that applied
    /// frames through [`Db::write`] would recapture and republish each one,
    /// every applied frame echoing back into the log forever. This path
    /// records nothing. Conflicts `ABORT` — under a single writer a conflict
    /// means an invariant broke, and it should stop loudly rather than
    /// half-apply.
    pub(crate) fn apply(&self, changeset: &[u8]) -> rusqlite::Result<()> {
        let (reply, rx) = mpsc::channel();
        self.jobs
            .send(Job::Apply {
                changeset: changeset.to_vec(),
                reply,
            })
            .map_err(|_| gone())?;
        rx.recv().map_err(|_| gone())?
    }
}

/// One peer frame, on the writer thread: apply the changeset atomically with
/// no session (records nothing) and `ABORT` on conflict.
pub(super) fn do_apply(conn: &Connection, changeset: &[u8]) -> rusqlite::Result<()> {
    conn.apply_strm(
        &mut &changeset[..],
        None::<fn(&str) -> bool>,
        |_conflict, _item| ConflictAction::SQLITE_CHANGESET_ABORT,
    )
}

impl Store {
    /// Frames captured locally but not yet published — the drain's input.
    #[must_use]
    pub fn pending_frames(&self) -> Vec<(i64, Vec<u8>)> {
        let Ok(mut stmt) = self
            .conn
            .prepare("SELECT seq, changeset FROM repl_log WHERE pub_seq IS NULL ORDER BY seq")
        else {
            return Vec::new();
        };
        stmt.query_map([], |r| Ok((r.get(0)?, r.get(1)?)))
            .map(|it| it.filter_map(Result::ok).collect())
            .unwrap_or_default()
    }

    /// Applies a peer's changeset through the private, non-recording apply
    /// path, then invalidates caches like any foreign commit. The frame does
    /// **not** re-enter this store's own log.
    ///
    /// # Errors
    ///
    /// If the changeset conflicts (a broken invariant under a single
    /// writer).
    pub fn apply_frame(&self, changeset: &[u8]) -> rusqlite::Result<()> {
        self.db.apply(changeset)?;
        self.poll_external();
        Ok(())
    }

    /// Marks every unpublished frame through `seq` as published, so a second
    /// drain moves nothing. Locally, `pub_seq` uses the local sequence,
    /// which is enough to prevent draining the same frame twice.
    ///
    /// # Errors
    ///
    /// If the update fails.
    pub fn mark_published(&self, up_to_seq: i64) -> rusqlite::Result<()> {
        self.db.raw(move |c| {
            c.execute(
                "UPDATE repl_log SET pub_seq = seq WHERE seq <= ?1 AND pub_seq IS NULL",
                [up_to_seq],
            )
            .map(|_| ())
        })
    }

    /// How many captured frames are still unpublished — the risk an offline
    /// holder is accruing, surfaced as a problem.
    #[must_use]
    pub fn unpublished(&self) -> i64 {
        self.conn
            .query_row("SELECT COUNT(*) FROM repl_log WHERE pub_seq IS NULL", [], |r| {
                r.get(0)
            })
            .unwrap_or(0)
    }

    /// The global sequence this store *contains* through — whatever the
    /// origin, including its own published writes.
    #[must_use]
    pub fn materialized(&self) -> i64 {
        self.conn
            .query_row("SELECT materialized_seq FROM repl WHERE id=1", [], |r| {
                r.get(0)
            })
            .unwrap_or(0)
    }

    /// The epoch this store last recorded itself in the lineage at.
    #[must_use]
    pub fn epoch(&self) -> i64 {
        self.conn
            .query_row("SELECT epoch FROM repl WHERE id=1", [], |r| r.get(0))
            .unwrap_or(0)
    }

    /// Whether this store currently believes it holds the lease.
    #[must_use]
    pub fn holding(&self) -> bool {
        self.conn
            .query_row("SELECT holding FROM repl WHERE id=1", [], |r| {
                r.get::<_, i64>(0)
            })
            .map(|h| h != 0)
            .unwrap_or(false)
    }

    /// Records the epoch and the holding flag — replication's own local
    /// state, through the raw path so it runs even on a follower and is
    /// never captured as a frame.
    ///
    /// # Errors
    ///
    /// If the update fails.
    pub fn set_lease(&self, epoch: i64, holding: bool) -> rusqlite::Result<()> {
        let h = i64::from(holding);
        self.db.raw(move |c| {
            c.execute(
                "UPDATE repl SET epoch = ?1, holding = ?2 WHERE id = 1",
                rusqlite::params![epoch, h],
            )
            .map(|_| ())
        })
    }

    /// Records what the last pass made of this device, and why it failed if
    /// it did — the row the unreachable-bucket problem is derived from.
    ///
    /// # Errors
    ///
    /// If the update fails.
    pub fn set_status(&self, role: &str, note: Option<&str>) -> rusqlite::Result<()> {
        let (role, note) = (role.to_string(), note.map(str::to_string));
        self.db.raw(move |c| {
            c.execute(
                "UPDATE repl SET role = ?1, note = ?2 WHERE id = 1",
                rusqlite::params![role, note],
            )
            .map(|_| ())
        })
    }

    /// Advances the high-water mark this store contains through.
    ///
    /// # Errors
    ///
    /// If the update fails.
    pub fn set_materialized(&self, seq: i64) -> rusqlite::Result<()> {
        self.db.raw(move |c| {
            c.execute("UPDATE repl SET materialized_seq = ?1 WHERE id = 1", [seq])
                .map(|_| ())
        })
    }

    /// `VACUUM INTO` a fresh file — a snapshot of the whole logical
    /// database, taken after pending replication frames have drained.
    /// Replication's own bookkeeping rides along and is dropped on install.
    ///
    /// # Errors
    ///
    /// If the vacuum fails.
    pub fn vacuum_into(&self, path: &Path) -> rusqlite::Result<()> {
        let path = path.to_string_lossy().to_string();
        self.db
            .raw(move |c| c.execute("VACUUM INTO ?1", [path]).map(|_| ()))
    }

    /// The genesis snapshot: `VACUUM INTO` a fresh file **and**, in the same
    /// writer-thread turn, bury every frame captured so far (they are
    /// already in the snapshot, so they must never also ship as a batch) and
    /// set the high-water to 0. Because the writer serves one job at a time,
    /// no write interleaves — a mutation is either before this turn (in the
    /// snapshot, buried) or after it (a future batch), never both. That is
    /// the "drained boundary" the snapshot needs.
    ///
    /// # Errors
    ///
    /// If the vacuum or the bury fails.
    pub fn snapshot_genesis(&self, path: &Path) -> rusqlite::Result<()> {
        let path = path.to_string_lossy().to_string();
        self.db.raw(move |c| {
            c.execute("VACUUM INTO ?1", [path])?;
            c.execute("UPDATE repl_log SET pub_seq = seq WHERE pub_seq IS NULL", [])?;
            c.execute("UPDATE repl SET materialized_seq = 0 WHERE id = 1", [])?;
            Ok(())
        })
    }

    /// Installs a snapshot into the live database: replace every replicated
    /// table's rows with the snapshot's, in one transaction, and set the
    /// high-water and epoch. `repl` is **preserved** — two devices must not
    /// share an id — and the snapshot's own `repl_log`/`repl` are not
    /// copied, so the sender's queue and identity do not come with it.
    ///
    /// Only the tables both sides have: a build without an app leaves that
    /// app's rows in the snapshot rather than failing the install.
    ///
    /// Not a file swap: a live connection keeps using the database it has
    /// open, so the rows are copied in with `ATTACH` rather than the file
    /// replaced under it.
    ///
    /// # Errors
    ///
    /// If the attach, copy or commit fails.
    pub fn install_snapshot(
        &self,
        path: &Path,
        materialized: i64,
        epoch: i64,
    ) -> rusqlite::Result<()> {
        let path = path.to_string_lossy().to_string();
        self.db.raw(move |c| {
            c.execute("ATTACH DATABASE ?1 AS snap", [&path])?;
            let result = (|| -> rusqlite::Result<()> {
                let here = replicated_tables(c, "main")?;
                let there = replicated_tables(c, "snap")?;
                c.execute_batch("PRAGMA defer_foreign_keys = ON")?;
                let tx = Transaction::new_unchecked(c, rusqlite::TransactionBehavior::Immediate)?;
                for t in here.iter().filter(|t| there.contains(t)) {
                    tx.execute(&format!("DELETE FROM main.\"{t}\""), [])?;
                    tx.execute(
                        &format!("INSERT INTO main.\"{t}\" SELECT * FROM snap.\"{t}\""),
                        [],
                    )?;
                }
                // The local pending queue is relative to the *old* baseline —
                // meaningless against the snapshot we just installed. Clear
                // it while `repl.next_local_seq` survives, so a re-drain
                // never resends a stale frame.
                tx.execute("DELETE FROM repl_log", [])?;
                tx.execute(
                    "UPDATE repl SET materialized_seq = ?1, epoch = ?2, holding = 0 WHERE id = 1",
                    rusqlite::params![materialized, epoch],
                )?;
                tx.commit()
            })();
            let _ = c.execute("DETACH DATABASE snap", []);
            result
        })?;
        self.poll_external();
        Ok(())
    }

    /// Applies a peer's batch of changesets and advances the high-water to
    /// `last_seq`, all in **one** transaction — so a crash mid-batch rolls
    /// the whole thing back and re-applies from the unchanged watermark,
    /// never half-lands. Conflicts `ABORT`. Records nothing (no session).
    ///
    /// # Errors
    ///
    /// If any changeset conflicts, or the commit fails.
    pub fn apply_batch(&self, frames: &[(i64, Vec<u8>)], last_seq: i64) -> rusqlite::Result<()> {
        let frames: Vec<Vec<u8>> = frames.iter().map(|(_, cs)| cs.clone()).collect();
        self.db.raw(move |c| {
            let tx = Transaction::new_unchecked(c, rusqlite::TransactionBehavior::Immediate)?;
            for cs in &frames {
                tx.apply_strm(
                    &mut &cs[..],
                    None::<fn(&str) -> bool>,
                    |_conflict, _item| ConflictAction::SQLITE_CHANGESET_ABORT,
                )?;
            }
            tx.execute("UPDATE repl SET materialized_seq = ?1 WHERE id = 1", [
                last_seq,
            ])?;
            tx.commit()
        })?;
        self.poll_external();
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::{Schema, Step};

    fn store() -> Store {
        Store::open(None, &[]).expect("in-memory store")
    }

    /// A write is captured as a frame; a rolled-back write captures nothing,
    /// and neither does one that changed no row — the log carries real
    /// deltas, not every call to `write`.
    #[test]
    fn writes_are_captured_as_frames() {
        let s = store();
        assert_eq!(s.unpublished(), 0, "a fresh store has an empty log");
        s.write(|tx| {
            tx.execute("INSERT INTO meta(key, value) VALUES('a', 'x')", [])
                .map(|_| ())
        })
        .unwrap();
        assert_eq!(s.unpublished(), 1, "the insert was captured");

        let _ = s.write(|tx| -> rusqlite::Result<()> {
            tx.execute("INSERT INTO meta(key, value) VALUES('b', 'y')", [])?;
            Err(rusqlite::Error::QueryReturnedNoRows)
        });
        assert_eq!(s.unpublished(), 1, "a rolled-back write leaves no frame");

        s.write(|tx| {
            tx.execute("UPDATE meta SET value='z' WHERE key='absent'", [])
                .map(|_| ())
        })
        .unwrap();
        assert_eq!(s.unpublished(), 1, "no rows changed, no frame");

        // Published frames drop out of the pending set, once.
        let last = s.pending_frames().last().map(|(seq, _)| *seq).unwrap();
        s.mark_published(last).unwrap();
        assert_eq!(s.unpublished(), 0);
    }

    /// Replication's own two tables are never in a changeset — that is what
    /// keeps an applied frame from echoing back — and the kernel's and every
    /// app's are.
    #[test]
    fn the_replicated_set_is_every_table_but_replications_own() {
        static LADDER: Schema = Schema {
            app: "an_app",
            steps: &[Step::Sql(
                "CREATE TABLE an_app_thing(id INTEGER PRIMARY KEY, name TEXT)",
            )],
        };
        let s = Store::open(None, &[&LADDER]).expect("store");
        let tables = replicated_tables(s.conn(), "main").unwrap();
        for owned in ["meta", "workspace", "ws_col", "panel", "wm", "effect"] {
            assert!(tables.iter().any(|t| t == owned), "the kernel's {owned}");
        }
        assert!(
            tables.iter().any(|t| t == "an_app_thing"),
            "and an app's, without the kernel naming it"
        );
        for own in REPL_TABLES {
            assert!(!tables.iter().any(|t| t == own), "never {own}");
        }
    }

    /// The device id is `repl`'s, not `meta`'s: `meta` replicates, and a
    /// follower that adopted the holder's id would publish under its name.
    #[test]
    fn the_device_id_lives_outside_the_replicated_tables() {
        let s = store();
        assert!(!s.device().is_empty());
        let in_meta: i64 = s
            .conn()
            .query_row("SELECT COUNT(*) FROM meta WHERE key = 'device'", [], |r| {
                r.get(0)
            })
            .unwrap();
        assert_eq!(in_meta, 0, "nothing in the table that replicates");
    }
}
