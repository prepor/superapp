//! The store's replication half: the local tables device sync keeps, the set
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
use tokio::sync::oneshot;

use rusqlite::{Connection, Transaction};

use super::{gone, Db, Erased, Job, RawFn, Store};

mod replay;
use replay::apply_changeset;

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

-- A bounded, device-local transition history. Rows and user/account content
-- never enter this journal. Keep changes to the state and its evidence atomic,
-- including raw protocol updates and snapshot installation.
CREATE TABLE IF NOT EXISTS repl_event(
  id      INTEGER PRIMARY KEY,
  ts      REAL NOT NULL,
  role    TEXT NOT NULL,
  epoch   INTEGER NOT NULL,
  holding INTEGER NOT NULL,
  seq     INTEGER NOT NULL,
  pending INTEGER NOT NULL,
  note    TEXT
);
CREATE TRIGGER IF NOT EXISTS repl_event_record
AFTER UPDATE OF epoch, holding, role, note ON repl
WHEN old.epoch IS NOT new.epoch OR old.holding IS NOT new.holding
  OR old.role IS NOT new.role OR old.note IS NOT new.note
BEGIN
  INSERT INTO repl_event(ts,role,epoch,holding,seq,pending,note)
    VALUES(unixepoch('subsec'),new.role,new.epoch,new.holding,new.materialized_seq,
      (SELECT count(*) FROM repl_log WHERE pub_seq IS NULL),new.note);
  DELETE FROM repl_event WHERE id NOT IN (
    SELECT id FROM repl_event ORDER BY id DESC LIMIT 256
  );
END;
";

/// Replication's local tables — never in a changeset, so a frame a
/// follower *applies* is never recaptured and never echoes back into its own
/// log.
const REPL_TABLES: [&str; 3] = ["repl", "repl_log", "repl_event"];

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

/// Restore parents before their children. Deferred foreign keys still scan
/// existing children when a missing parent arrives; without a child index,
/// restoring a message archive alphabetically makes that work quadratic.
/// Cycles fall back to deferred checks, which validate the whole transaction.
fn snapshot_table_order(conn: &Connection, tables: Vec<String>) -> rusqlite::Result<Vec<String>> {
    let mut pending = Vec::with_capacity(tables.len());
    for table in tables {
        let mut stmt = conn.prepare("SELECT \"table\" FROM pragma_foreign_key_list(?1)")?;
        let parents = stmt.query_map([&table], |r| r.get::<_, String>(0))?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        pending.push((table, parents));
    }
    let mut ordered = Vec::with_capacity(pending.len());
    while !pending.is_empty() {
        let next = pending.iter().position(|(table, parents)| {
            parents.iter().all(|parent| parent == table || !pending.iter().any(|(t, _)| t == parent))
        }).unwrap_or(0);
        ordered.push(pending.remove(next).0);
    }
    Ok(ordered)
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
    pub(crate) async fn raw_async<T: Send + 'static>(
        &self,
        f: impl FnOnce(&Connection) -> rusqlite::Result<T> + Send + 'static,
    ) -> rusqlite::Result<T> {
        let (reply, rx) = oneshot::channel();
        let run: RawFn = Box::new(move |c| f(c).map(|v| Box::new(v) as Erased));
        self.jobs.send(Job::Raw { run, reply }).map_err(|_| gone())?;
        let erased = rx.await.map_err(|_| gone())??;
        Ok(*erased.downcast::<T>().expect("raw result type"))
    }

    /// Applies a peer changeset. Private on purpose: a follower that applied
    /// frames through [`Db::write`] would recapture and republish each one,
    /// every applied frame echoing back into the log forever. This path
    /// records nothing. Conflicts `ABORT` — under a single writer a conflict
    /// means an invariant broke, and it should stop loudly rather than
    /// half-apply.
    pub(crate) async fn apply_async(&self, changeset: &[u8]) -> rusqlite::Result<()> {
        let (reply, rx) = oneshot::channel();
        self.jobs
            .send(Job::Apply {
                changeset: changeset.to_vec(),
                reply,
            })
            .map_err(|_| gone())?;
        rx.await.map_err(|_| gone())?
    }
}

/// One peer frame, on the writer thread: apply the changeset atomically with
/// no session (records nothing) and `ABORT` on conflict.
pub(super) fn do_apply(conn: &Connection, changeset: &[u8]) -> rusqlite::Result<()> {
    let tx = Transaction::new_unchecked(conn, rusqlite::TransactionBehavior::Immediate)?;
    apply_changeset(&tx, changeset)?;
    tx.commit()
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
        crate::runtime::block_on(self.apply_frame_async(changeset))
    }

    pub async fn apply_frame_async(&self, changeset: &[u8]) -> rusqlite::Result<()> {
        self.db.apply_async(changeset).await?;
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
        crate::runtime::block_on(self.mark_published_async(up_to_seq))
    }

    pub async fn mark_published_async(&self, up_to_seq: i64) -> rusqlite::Result<()> {
        self.db.raw_async(move |c| {
            c.execute(
                "UPDATE repl_log SET pub_seq = seq WHERE seq <= ?1 AND pub_seq IS NULL",
                [up_to_seq],
            )
            .map(|_| ())
        }).await
    }

    /// A confirmed remote commit and our local replay watermark are one
    /// checkpoint. A crash must leave both old or both new.
    pub async fn acknowledge_publish_async(&self, local_seq: i64, global_seq: i64) -> rusqlite::Result<()> {
        self.db.raw_async(move |c| {
            let tx = Transaction::new_unchecked(c, rusqlite::TransactionBehavior::Immediate)?;
            tx.execute(
                "UPDATE repl_log SET pub_seq = seq WHERE seq <= ?1 AND pub_seq IS NULL",
                [local_seq],
            )?;
            tx.execute("UPDATE repl SET materialized_seq = ?1 WHERE id = 1", [global_seq])?;
            tx.commit()
        }).await
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
        crate::runtime::block_on(self.set_lease_async(epoch, holding))
    }

    pub async fn set_lease_async(&self, epoch: i64, holding: bool) -> rusqlite::Result<()> {
        let h = i64::from(holding);
        self.db.raw_async(move |c| {
            c.execute(
                "UPDATE repl SET epoch = ?1, holding = ?2 WHERE id = 1",
                rusqlite::params![epoch, h],
            )
            .map(|_| ())
        }).await
    }

    /// Records what the last pass made of this device, and why it failed if
    /// it did — the row the unreachable-bucket problem is derived from.
    ///
    /// # Errors
    ///
    /// If the update fails.
    pub fn set_status(&self, role: &str, note: Option<&str>) -> rusqlite::Result<()> {
        crate::runtime::block_on(self.set_status_async(role, note))
    }

    pub async fn set_status_async(&self, role: &str, note: Option<&str>) -> rusqlite::Result<()> {
        let (role, note) = (role.to_string(), note.map(str::to_string));
        self.db.raw_async(move |c| {
            c.execute(
                "UPDATE repl SET role = ?1, note = ?2 WHERE id = 1",
                rusqlite::params![role, note],
            )
            .map(|_| ())
        }).await
    }

    /// Advances the high-water mark this store contains through.
    ///
    /// # Errors
    ///
    /// If the update fails.
    pub fn set_materialized(&self, seq: i64) -> rusqlite::Result<()> {
        crate::runtime::block_on(self.set_materialized_async(seq))
    }

    pub async fn set_materialized_async(&self, seq: i64) -> rusqlite::Result<()> {
        self.db.raw_async(move |c| {
            c.execute("UPDATE repl SET materialized_seq = ?1 WHERE id = 1", [seq])
                .map(|_| ())
        }).await
    }

    /// `VACUUM INTO` a fresh file — a snapshot of the whole logical
    /// database, taken after pending replication frames have drained.
    /// Replication's own bookkeeping rides along and is dropped on install.
    ///
    /// # Errors
    ///
    /// If the vacuum fails.
    pub fn vacuum_into(&self, path: &Path) -> rusqlite::Result<()> {
        crate::runtime::block_on(self.vacuum_into_async(path))
    }

    pub async fn vacuum_into_async(&self, path: &Path) -> rusqlite::Result<()> {
        let path = path.to_string_lossy().to_string();
        self.db
            .raw_async(move |c| c.execute("VACUUM INTO ?1", [path]).map(|_| ())).await
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
        crate::runtime::block_on(self.snapshot_genesis_async(path))
    }

    pub async fn snapshot_genesis_async(&self, path: &Path) -> rusqlite::Result<()> {
        let path = path.to_string_lossy().to_string();
        self.db.raw_async(move |c| {
            c.execute("VACUUM INTO ?1", [path])?;
            c.execute("UPDATE repl_log SET pub_seq = seq WHERE pub_seq IS NULL", [])?;
            c.execute("UPDATE repl SET materialized_seq = 0 WHERE id = 1", [])?;
            Ok(())
        }).await
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
        crate::runtime::block_on(self.install_snapshot_async(path, materialized, epoch))
    }

    pub async fn install_snapshot_async(
        &self,
        path: &Path,
        materialized: i64,
        epoch: i64,
    ) -> rusqlite::Result<()> {
        self.install_snapshot_bound_async(path, materialized, epoch, None).await
    }

    /// A history binding becomes durable with its baseline and watermark.
    /// A failed download or install cannot associate old rows with a new
    /// history, even when the two histories happen to have equal counters.
    pub async fn install_snapshot_bound_async(
        &self,
        path: &Path,
        materialized: i64,
        epoch: i64,
        lineage: Option<String>,
    ) -> rusqlite::Result<()> {
        let path = path.to_string_lossy().to_string();
        self.db.raw_async(move |c| {
            c.execute("ATTACH DATABASE ?1 AS snap", [&path])?;
            let result = (|| -> rusqlite::Result<()> {
                let here = replicated_tables(c, "main")?;
                let there = replicated_tables(c, "snap")?;
                let common = snapshot_table_order(c,
                    here.into_iter().filter(|t| there.contains(t)).collect())?;
                for table in &common {
                    let columns = |schema: &str| -> rusqlite::Result<Vec<String>> {
                        c.prepare(&format!("PRAGMA {schema}.table_info(\"{table}\")"))?
                            .query_map([], |r| r.get(1))?.collect()
                    };
                    if columns("main")? != columns("snap")? {
                        return Err(rusqlite::Error::InvalidParameterName(format!(
                            "snapshot column order differs for {table}; migrate both devices before syncing")));
                    }
                }
                let tx = Transaction::new_unchecked(c, rusqlite::TransactionBehavior::Immediate)?;
                tx.execute_batch("PRAGMA defer_foreign_keys = ON")?;
                // Clear the old baseline completely before restoring rows:
                // deleting a parent later must not cascade into new children.
                for t in common.iter().rev() {
                    tx.execute(&format!("DELETE FROM main.\"{t}\""), [])?;
                }
                for t in &common {
                    // WHERE 1 prevents SQLite's bulk-transfer shortcut, which
                    // can leave deferred FK counters unresolved when a parent
                    // is copied after its children. Use ordinary row inserts.
                    tx.execute(
                        &format!("INSERT INTO main.\"{t}\" SELECT * FROM snap.\"{t}\" WHERE 1"),
                        [],
                    )?;
                }
                // The local pending queue is relative to the *old* baseline —
                // meaningless against the snapshot we just installed. Clear
                // it while `repl.next_local_seq` survives, so a re-drain
                // never resends a stale frame.
                tx.execute("DELETE FROM repl_log", [])?;
                // The divergent baseline is gone. Persist that transition
                // atomically so an interrupted replay cannot resurrect its
                // old Recover button on this run or after a restart.
                tx.execute(
                    "UPDATE repl SET materialized_seq = ?1, epoch = ?2, holding = 0, role = 'syncing', note = NULL WHERE id = 1",
                    rusqlite::params![materialized, epoch],
                )?;
                if let Some(lineage) = lineage {
                    tx.execute("UPDATE repl SET lineage=?1 WHERE id=1", [lineage])?;
                }
                tx.commit()
            })();
            let _ = c.execute("DETACH DATABASE snap", []);
            result
        }).await?;
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
        crate::runtime::block_on(self.apply_batch_async(frames, last_seq))
    }

    pub async fn apply_batch_async(&self, frames: &[(i64, Vec<u8>)], last_seq: i64) -> rusqlite::Result<()> {
        let frames: Vec<Vec<u8>> = frames.iter().map(|(_, cs)| cs.clone()).collect();
        self.db.raw_async(move |c| {
            let tx = Transaction::new_unchecked(c, rusqlite::TransactionBehavior::Immediate)?;
            for cs in &frames {
                apply_changeset(&tx, cs)?;
            }
            tx.execute("UPDATE repl SET materialized_seq = ?1 WHERE id = 1", [
                last_seq,
            ])?;
            tx.commit()
        }).await?;
        self.poll_external();
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::{Schema, Step};

    static RELATIONS: Schema = Schema {
        app: "replay-relations",
        steps: &[Step::Sql("
            CREATE TABLE parent(id INTEGER PRIMARY KEY, text TEXT NOT NULL);
            CREATE TABLE cascaded(id INTEGER PRIMARY KEY,
                parent INTEGER NOT NULL REFERENCES parent(id) ON DELETE CASCADE ON UPDATE CASCADE);
            CREATE TABLE nulled(id INTEGER PRIMARY KEY,
                parent INTEGER REFERENCES parent(id) ON DELETE SET NULL ON UPDATE CASCADE);
            CREATE TABLE triggered(id INTEGER PRIMARY KEY, parent INTEGER NOT NULL, payload TEXT NOT NULL);
            CREATE TRIGGER parent_delete AFTER DELETE ON parent BEGIN
                DELETE FROM triggered WHERE parent = old.id;
            END;
            CREATE VIRTUAL TABLE parent_fts USING fts5(text, content='parent', content_rowid='id');
            CREATE TRIGGER parent_fts_insert AFTER INSERT ON parent BEGIN
                INSERT INTO parent_fts(rowid,text) VALUES(new.id,new.text);
            END;
            CREATE TRIGGER parent_fts_delete AFTER DELETE ON parent BEGIN
                INSERT INTO parent_fts(parent_fts,rowid,text) VALUES('delete',old.id,old.text);
            END;
            CREATE TRIGGER parent_fts_update AFTER UPDATE ON parent BEGIN
                INSERT INTO parent_fts(parent_fts,rowid,text) VALUES('delete',old.id,old.text);
                INSERT INTO parent_fts(rowid,text) VALUES(new.id,new.text);
            END;
        ")],
    };

    fn related_pair() -> (Store, Store) {
        let a = Store::open(None, &[&RELATIONS]).unwrap();
        let b = Store::open(None, &[&RELATIONS]).unwrap();
        a.write(|tx| tx.execute_batch("
            INSERT INTO parent VALUES(1,'original');
            INSERT INTO cascaded VALUES(1,1);
            INSERT INTO nulled VALUES(1,1);
        ")).unwrap();
        crate::runtime::block_on(crate::repl::drain(&a, &b)).unwrap();
        (a, b)
    }

    fn relation_rows(s: &Store) -> Vec<(String, i64)> {
        ["parent", "cascaded", "nulled", "triggered"].into_iter().map(|table| {
            (table.to_string(), s.conn().query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |r| r.get(0)).unwrap())
        }).collect()
    }

    #[test]
    fn replay_applies_cascade_and_set_null_once_and_keeps_search_indexes() {
        let (a, b) = related_pair();
        a.write(|tx| tx.execute("DELETE FROM parent WHERE id=1", []).map(|_| ())).unwrap();
        crate::runtime::block_on(crate::repl::drain(&a, &b)).unwrap();
        assert_eq!(relation_rows(&a), relation_rows(&b));
        assert_eq!(b.conn().query_row("SELECT parent FROM nulled WHERE id=1", [], |r| r.get::<_, Option<i64>>(0)).unwrap(), None);
        assert_eq!(b.conn().query_row("SELECT COUNT(*) FROM parent_fts WHERE parent_fts MATCH 'original'", [], |r| r.get::<_, i64>(0)).unwrap(), 0);
        assert_eq!(b.conn().query_row("SELECT COUNT(*) FROM pragma_foreign_key_check", [], |r| r.get::<_, i64>(0)).unwrap(), 0);
        assert_eq!(b.unpublished(), 0);
    }

    #[test]
    fn replay_applies_triggered_deletions_once_and_keeps_search_indexes() {
        let (a, b) = related_pair();
        a.write(|tx| tx.execute("INSERT INTO triggered VALUES(1,1,'original')", []).map(|_| ())).unwrap();
        crate::runtime::block_on(crate::repl::drain(&a, &b)).unwrap();
        a.write(|tx| tx.execute("DELETE FROM parent WHERE id=1", []).map(|_| ())).unwrap();
        crate::runtime::block_on(crate::repl::drain(&a, &b)).unwrap();
        assert_eq!(relation_rows(&a), relation_rows(&b));
        assert_eq!(b.conn().query_row("SELECT COUNT(*) FROM parent_fts WHERE parent_fts MATCH 'original'", [], |r| r.get::<_, i64>(0)).unwrap(), 0);
        assert_eq!(b.unpublished(), 0);
    }

    #[test]
    fn replay_preserves_cascaded_parent_key_update() {
        let (a, b) = related_pair();
        a.write(|tx| tx.execute("UPDATE parent SET id=2,text='changed' WHERE id=1", []).map(|_| ())).unwrap();
        crate::runtime::block_on(crate::repl::drain(&a, &b)).unwrap();
        for table in ["cascaded", "nulled"] {
            assert_eq!(b.conn().query_row(&format!("SELECT parent FROM {table} WHERE id=1"), [], |r| r.get::<_, i64>(0)).unwrap(), 2);
        }
        assert_eq!(b.conn().query_row("SELECT rowid FROM parent_fts WHERE parent_fts MATCH 'changed'", [], |r| r.get::<_, i64>(0)).unwrap(), 2);
    }

    #[test]
    fn replay_conflict_rolls_back_prior_frames_and_the_watermark() {
        let (a, b) = related_pair();
        a.write(|tx| tx.execute("INSERT INTO parent VALUES(2,'new')", []).map(|_| ())).unwrap();
        a.write(|tx| tx.execute("UPDATE parent SET text='changed' WHERE id=1", []).map(|_| ())).unwrap();
        b.write(|tx| tx.execute("UPDATE parent SET text='private' WHERE id=1", []).map(|_| ())).unwrap();
        let frames = a.pending_frames();
        assert!(b.apply_batch(&frames, 7).is_err());
        assert_eq!(b.materialized(), 0);
        assert_eq!(b.conn().query_row("SELECT COUNT(*) FROM parent WHERE id=2", [], |r| r.get::<_, i64>(0)).unwrap(), 0);
        assert_eq!(b.conn().query_row("SELECT text FROM parent WHERE id=1", [], |r| r.get::<_, String>(0)).unwrap(), "private");
    }

    #[test]
    fn replay_must_not_hide_a_missing_or_changed_indirect_before_image() {
        for change in ["DELETE FROM triggered WHERE id=1", "UPDATE triggered SET payload='private' WHERE id=1"] {
            let (a, b) = related_pair();
            a.write(|tx| tx.execute("INSERT INTO triggered VALUES(1,1,'original')", []).map(|_| ())).unwrap();
            crate::runtime::block_on(crate::repl::drain(&a, &b)).unwrap();
            b.write(move |tx| tx.execute(change, []).map(|_| ())).unwrap();
            a.write(|tx| tx.execute("DELETE FROM parent WHERE id=1", []).map(|_| ())).unwrap();
            let frames = a.pending_frames();
            let error = b.apply_batch(&frames, 1).unwrap_err();
            assert!(error.to_string().contains("before-image differs"), "{error}");
            assert_eq!(b.materialized(), 0);
            assert_eq!(b.conn().query_row("SELECT COUNT(*) FROM parent", [], |r| r.get::<_, i64>(0)).unwrap(), 1);
        }
    }

    #[test]
    fn replay_must_not_hide_a_missing_direct_delete() {
        let (a, b) = related_pair();
        a.write(|tx| tx.execute("DELETE FROM parent WHERE id=1", []).map(|_| ())).unwrap();
        b.write(|tx| tx.execute("DELETE FROM parent WHERE id=1", []).map(|_| ())).unwrap();
        assert!(b.apply_batch(&a.pending_frames(), 1).is_err());
        assert_eq!(b.materialized(), 0);
    }

    #[test]
    fn replay_foreign_key_failure_does_not_commit_or_advance_the_watermark() {
        let (a, b) = related_pair();
        a.write(|tx| tx.execute("INSERT INTO cascaded VALUES(2,1)", []).map(|_| ())).unwrap();
        b.write(|tx| tx.execute("DELETE FROM parent WHERE id=1", []).map(|_| ())).unwrap();
        let error = b.apply_batch(&a.pending_frames(), 1).unwrap_err();
        assert!(error.to_string().contains("foreign key"), "{error}");
        assert_eq!(b.materialized(), 0);
        assert_eq!(b.conn().query_row("SELECT COUNT(*) FROM cascaded WHERE id=2", [], |r| r.get::<_, i64>(0)).unwrap(), 0);
    }

    #[test]
    fn replay_mail_parent_before_child_delete_converges() {
        static MAIL: Schema = Schema { app: "replay-mail", steps: &[Step::Sql("
            CREATE TABLE folder(id INTEGER PRIMARY KEY);
            CREATE TABLE message(id INTEGER PRIMARY KEY);
            CREATE TABLE trashed(message INTEGER PRIMARY KEY REFERENCES message(id) ON DELETE CASCADE,
                folder INTEGER NOT NULL REFERENCES folder(id) ON DELETE CASCADE);
        ")] };
        let a = Store::open(None, &[&MAIL]).unwrap();
        let b = Store::open(None, &[&MAIL]).unwrap();
        a.write(|tx| tx.execute_batch("INSERT INTO folder VALUES(1);INSERT INTO message VALUES(1);INSERT INTO trashed VALUES(1,1)")).unwrap();
        crate::runtime::block_on(crate::repl::drain(&a, &b)).unwrap();
        a.write(|tx| tx.execute("DELETE FROM message WHERE id=1", []).map(|_| ())).unwrap();
        b.apply_batch(&a.pending_frames(), 1).unwrap();
        assert_eq!(b.materialized(), 1);
        assert_eq!(b.conn().query_row("SELECT COUNT(*) FROM trashed", [], |r| r.get::<_, i64>(0)).unwrap(), 0);
        assert_eq!(b.conn().query_row("SELECT COUNT(*) FROM pragma_foreign_key_check", [], |r| r.get::<_, i64>(0)).unwrap(), 0);
    }

    #[test]
    fn replay_refuses_missing_tables_instead_of_silently_skipping_rows() {
        static EXTRA: Schema = Schema { app: "replay-extra", steps: &[Step::Sql("CREATE TABLE extra(id INTEGER PRIMARY KEY, value TEXT)")] };
        let a = Store::open(None, &[&EXTRA]).unwrap();
        let b = store();
        a.write(|tx| tx.execute("INSERT INTO extra VALUES(1,'must not vanish')", []).map(|_| ())).unwrap();
        let error = b.apply_batch(&a.pending_frames(), 1).unwrap_err();
        assert!(error.to_string().contains("incompatible table extra"), "{error}");
        assert_eq!(b.materialized(), 0);
    }

    fn store() -> Store {
        Store::open(None, &[]).expect("in-memory store")
    }

    #[test]
    fn transition_journal_records_changed_state_and_pending_boundary_only() {
        let s = store();
        s.set_lease(1, true).unwrap();
        s.set_status("holder", None).unwrap();
        let count = || s.conn().query_row("SELECT COUNT(*) FROM repl_event", [], |r| r.get::<_, i64>(0)).unwrap();
        assert_eq!(count(), 2);
        s.write(|tx| tx.execute("INSERT INTO meta VALUES('private-content','never journal this')", []).map(|_| ())).unwrap();
        s.set_materialized(7).unwrap();
        s.set_status("holder", None).unwrap();
        s.set_lease(1, true).unwrap();
        assert_eq!(count(), 2, "ordinary progress and unchanged polls do not crowd out transitions");
        s.set_status("offline", Some("transport unavailable")).unwrap();
        let event = s.conn().query_row("SELECT role,epoch,holding,seq,pending,note FROM repl_event ORDER BY id DESC LIMIT 1", [], |r|
            Ok((r.get::<_, String>(0)?,r.get::<_, i64>(1)?,r.get::<_, i64>(2)?,r.get::<_, i64>(3)?,r.get::<_, i64>(4)?,r.get::<_, String>(5)?))).unwrap();
        assert_eq!(event, ("offline".into(),1,1,7,1,"transport unavailable".into()));
        assert_eq!(s.unpublished(), 1, "journal writes are never captured as shared data");
        assert!(!replicated_tables(s.conn(), "main").unwrap().contains(&"repl_event".into()));
    }

    #[test]
    fn transition_journal_is_bounded_and_preserved_across_snapshot_install() {
        let source = store();
        let s = store();
        for epoch in 1..=300 { s.set_lease(epoch, true).unwrap(); }
        let range = s.conn().query_row("SELECT COUNT(*),MIN(epoch),MAX(epoch) FROM repl_event", [], |r|
            Ok((r.get::<_, i64>(0)?,r.get::<_, i64>(1)?,r.get::<_, i64>(2)?))).unwrap();
        assert_eq!(range, (256,45,300));
        let path = std::env::temp_dir().join(format!("superapp-journal-snapshot-{}.db", s.device()));
        source.set_lease(999, true).unwrap();
        source.set_status("foreign-device", None).unwrap();
        source.vacuum_into(&path).unwrap();
        s.install_snapshot(&path, 4, 301).unwrap();
        let imported: i64 = s.conn().query_row("SELECT COUNT(*) FROM repl_event WHERE role='foreign-device'", [], |r| r.get(0)).unwrap();
        assert_eq!(imported, 0, "a snapshot never replaces the receiver's incident history");
        let last = s.conn().query_row("SELECT epoch,holding,seq FROM repl_event ORDER BY id DESC LIMIT 1", [], |r|
            Ok((r.get::<_, i64>(0)?,r.get::<_, i64>(1)?,r.get::<_, i64>(2)?))).unwrap();
        assert_eq!(last, (301,0,4), "installation and its event share the same transaction");
        std::fs::remove_file(path).unwrap();
    }

    #[test]
    fn transition_journal_rolls_back_with_its_state_update() {
        let s = store();
        let result: rusqlite::Result<()> = crate::runtime::block_on(s.db().raw_async(|conn| {
            let tx = Transaction::new_unchecked(conn, rusqlite::TransactionBehavior::Immediate)?;
            tx.execute("UPDATE repl SET epoch=123,role='must roll back' WHERE id=1", [])?;
            assert_eq!(tx.query_row("SELECT COUNT(*) FROM repl_event", [], |r| r.get::<_, i64>(0))?, 1);
            Err(rusqlite::Error::QueryReturnedNoRows)
        }));
        assert!(result.is_err());
        assert_eq!(s.epoch(), 0);
        assert_eq!(s.conn().query_row("SELECT COUNT(*) FROM repl_event", [], |r| r.get::<_, i64>(0)).unwrap(), 0);
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

    #[test]
    fn snapshot_column_mismatch_preserves_the_receiving_store() {
        static A: Schema = Schema { app: "flags", steps: &[Step::Sql(
            "CREATE TABLE flags(id INTEGER PRIMARY KEY,blocked INTEGER,is_forum INTEGER)")] };
        static B: Schema = Schema { app: "flags", steps: &[Step::Sql(
            "CREATE TABLE flags(id INTEGER PRIMARY KEY,is_forum INTEGER,blocked INTEGER)")] };
        let source = Store::open(None, &[&A]).unwrap();
        let follower = Store::open(None, &[&B]).unwrap();
        source.write(|c| c.execute("INSERT INTO flags VALUES(1,0,1)", []).map(|_| ())).unwrap();
        follower.write(|c| c.execute("INSERT INTO flags VALUES(2,1,0)", []).map(|_| ())).unwrap();
        let path = std::env::temp_dir().join(format!("superapp-snapshot-columns-{}.db", std::process::id()));
        let _ = std::fs::remove_file(&path);
        source.vacuum_into(&path).unwrap();
        assert!(follower.install_snapshot(&path, 9, 2).is_err());
        let row: (i64,i64,i64) = follower.conn().query_row(
            "SELECT id,is_forum,blocked FROM flags", [], |r| Ok((r.get(0)?,r.get(1)?,r.get(2)?)),
        ).unwrap();
        assert_eq!(row, (2,1,0));
        assert_eq!(follower.epoch(), 0);
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn snapshot_import_does_not_rescan_all_children_for_each_parent() {
        static LADDER: Schema = Schema {
            app: "snapshot_volume",
            steps: &[Step::Sql(
                "CREATE TABLE z_sender(id INTEGER PRIMARY KEY);
                 CREATE TABLE a_message(id INTEGER PRIMARY KEY,
                     sender INTEGER NOT NULL REFERENCES z_sender(id));",
            )],
        };
        let source = Store::open(None, &[&LADDER]).unwrap();
        source.write(|tx| tx.execute_batch(
            "WITH RECURSIVE n(x) AS (VALUES(1) UNION ALL SELECT x+1 FROM n WHERE x<2000)
             INSERT INTO z_sender SELECT x FROM n;
             WITH RECURSIVE n(x) AS (VALUES(1) UNION ALL SELECT x+1 FROM n WHERE x<6000)
             INSERT INTO a_message SELECT x, 1+(x%2000) FROM n;",
        )).unwrap();
        let path = std::env::temp_dir().join(format!("superapp-snapshot-volume-{}.db", std::process::id()));
        let _ = std::fs::remove_file(&path);
        source.vacuum_into(&path).unwrap();
        let follower = Store::open(None, &[&LADDER]).unwrap();
        // Bound SQLite work rather than wall time: child-first restoration
        // scans all 6,000 messages once for each of the 2,000 senders.
        crate::runtime::block_on(follower.db.raw_async(|c| {
            let mut ticks = 0;
            c.progress_handler(1_000, Some(move || { ticks += 1; ticks > 2_000 }))?;
            Ok(())
        })).unwrap();
        let result = follower.install_snapshot(&path, 7, 3);
        crate::runtime::block_on(follower.db.raw_async(|c| {
            c.progress_handler(0, None::<fn() -> bool>)?;
            Ok(())
        })).unwrap();
        let _ = std::fs::remove_file(&path);
        result.expect("snapshot import must fit the linear-work budget");
        let messages: i64 = follower.conn().query_row(
            "SELECT COUNT(*) FROM a_message", [], |r| r.get(0),
        ).unwrap();
        assert_eq!(messages, 6_000);
    }

    #[test]
    fn snapshots_restore_deferred_foreign_keys_and_cascading_children() {
        static LADDER: Schema = Schema {
            app: "snapshot_fks",
            steps: &[Step::Sql(
                "CREATE TABLE z_parent(id INTEGER PRIMARY KEY,
                     child INTEGER REFERENCES a_child(id));
                 CREATE TABLE a_child(id INTEGER PRIMARY KEY REFERENCES z_parent(id));
                 CREATE TABLE b_cascade(id INTEGER PRIMARY KEY,
                     parent INTEGER NOT NULL REFERENCES z_parent(id) ON DELETE CASCADE);",
            )],
        };
        let source = Store::open(None, &[&LADDER]).unwrap();
        let follower = Store::open(None, &[&LADDER]).unwrap();
        for store in [&source, &follower] {
            store.write(|tx| tx.execute_batch(
                "PRAGMA defer_foreign_keys = ON;
                 INSERT INTO z_parent VALUES(1, 1);
                 INSERT INTO a_child VALUES(1);
                 INSERT INTO b_cascade VALUES(1, 1);",
            )).unwrap();
        }
        let device = follower.device();
        let path = std::env::temp_dir().join(format!("superapp-snapshot-fks-{}.db", std::process::id()));
        let _ = std::fs::remove_file(&path);
        source.vacuum_into(&path).unwrap();

        // Cyclic references cannot be ordered parent-first. Replacing the
        // baseline must preserve all children, including cascading ones.
        follower.install_snapshot(&path, 7, 3).unwrap();
        for table in ["z_parent", "a_child", "b_cascade"] {
            let count: i64 = follower.conn().query_row(
                &format!("SELECT COUNT(*) FROM {table}"), [], |r| r.get(0),
            ).unwrap();
            assert_eq!(count, 1, "the snapshot's {table} row survives");
        }
        assert_eq!(follower.device(), device);
        assert_eq!(follower.unpublished(), 0);

        // A corrupt snapshot must still fail and roll back both its rows
        // and replication bookkeeping. Ordinary writes still enforce FKs.
        follower.write(|tx| tx.execute(
            "INSERT INTO meta(key, value) VALUES('keep', 'local')", [],
        ).map(|_| ())).unwrap();
        let pending = follower.unpublished();
        let corrupt = Store::open(Some(&path), &[&LADDER]).unwrap();
        crate::runtime::block_on(corrupt.db.raw_async(|conn| conn.execute_batch(
            "PRAGMA foreign_keys = OFF; DELETE FROM z_parent; PRAGMA foreign_keys = ON;",
        ))).unwrap();
        drop(corrupt);
        assert!(follower.install_snapshot(&path, 8, 4).is_err());
        assert_eq!(follower.unpublished(), pending);
        let kept: String = follower.conn().query_row(
            "SELECT value FROM meta WHERE key = 'keep'", [], |r| r.get(0),
        ).unwrap();
        assert_eq!(kept, "local");
        let position: (i64, i64) = follower.conn().query_row(
            "SELECT materialized_seq, epoch FROM repl", [], |r| Ok((r.get(0)?, r.get(1)?)),
        ).unwrap();
        assert_eq!(position, (7, 3));
        assert!(follower.write(|tx| tx.execute("INSERT INTO a_child VALUES(2)", []).map(|_| ())).is_err());
        let _ = std::fs::remove_file(path);
    }
}
