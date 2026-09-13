//! The five tables the log lives in, and this device's own row.
//!
//! They are made by presence rather than by a schema number: a store from
//! any build this one reads gains them on its next open, and the kernel's
//! version says nothing about them.

use rusqlite::Connection;

/// The log, the vector clock, this device, the roster, and where each peer
/// was last reached.
///
/// The log is compacted as it is written: an op that a newer op of its cell
/// or a newer tombstone of its row has overtaken is dropped, so what the
/// table holds is one op per live cell — the winners — and the index over
/// `(tbl, key, col)` is how a merge finds the one it is up against.
pub(crate) const SYNC_TABLES: &str = "
CREATE TABLE IF NOT EXISTS sync_op(
  origin TEXT NOT NULL,      -- the device that made the change
  seq    INTEGER NOT NULL,   -- that device's own count, from 1
  hlc    INTEGER NOT NULL,   -- hybrid logical clock
  tbl    TEXT NOT NULL,
  key    TEXT NOT NULL,      -- JSON array of the key values, in declared order
  col    TEXT NOT NULL,      -- '' is the row's tombstone
  val    TEXT,               -- JSON; SQL NULL is NULL, a blob is {\"b64\":…}
  PRIMARY KEY(origin, seq)
);
CREATE INDEX IF NOT EXISTS sync_op_cell ON sync_op(tbl, key, col);
CREATE TABLE IF NOT EXISTS sync_have(
  origin TEXT PRIMARY KEY, seq INTEGER NOT NULL
);
CREATE TABLE IF NOT EXISTS sync_self(
  id       INTEGER PRIMARY KEY CHECK(id = 1),
  device   TEXT NOT NULL,    -- its iroh endpoint id
  next_seq INTEGER NOT NULL DEFAULT 1,
  hlc      INTEGER NOT NULL DEFAULT 0
);
CREATE TABLE IF NOT EXISTS sync_peer(
  device  TEXT PRIMARY KEY,
  name    TEXT NOT NULL DEFAULT '',
  added   REAL NOT NULL DEFAULT 0,  -- every column of a replicated table
  removed INTEGER NOT NULL DEFAULT 0  -- needs one: see below
);
CREATE TABLE IF NOT EXISTS sync_link(
  device TEXT PRIMARY KEY,    -- where this peer was last reached: an
  addr   TEXT NOT NULL        -- endpoint ticket, so a dial can go straight
);                            -- there. Device-local; it never replicates.
";

/// Makes the tables if they are not there, and writes down who this device
/// is.
///
/// A store carries the id it was first opened with. A store opened as
/// somebody else — a file restored beside another device's key — takes the
/// new id and counts its ops on from whatever that id already wrote here,
/// so no peer is ever shown a gap in a sequence. What it wrote here is what
/// the vector clock vouches for, or what the log still holds of it,
/// whichever is further: an op compacted away was still counted.
pub(crate) fn ensure(conn: &Connection, device: &str) -> rusqlite::Result<()> {
    conn.execute_batch(SYNC_TABLES)?;
    roster(conn)?;
    compacted(conn)?;
    let was: Option<String> = conn
        .query_row("SELECT device FROM sync_self WHERE id = 1", [], |r| r.get(0))
        .ok();
    if was.as_deref() == Some(device) {
        return Ok(());
    }
    let next: i64 = conn.query_row(
        "SELECT max(coalesce((SELECT seq FROM sync_have WHERE origin = ?1), 0),
                    coalesce((SELECT max(seq) FROM sync_op WHERE origin = ?1), 0)) + 1",
        [device],
        |r| r.get(0),
    )?;
    conn.execute(
        "INSERT INTO sync_self(id, device, next_seq) VALUES(1, ?1, ?2)
         ON CONFLICT(id) DO UPDATE SET device = excluded.device, next_seq = excluded.next_seq",
        rusqlite::params![device, next],
    )?;
    Ok(())
}

/// The roster, as the first build of this wrote it: `added` was required
/// and had no default, so no op could ever write a peer's row here. A
/// rename or a **forget** from another device arrives as one cell, and the
/// insert carrying it failed the NOT NULL check before it reached the row
/// it meant to update — a device that paired was never renamed and never
/// forgotten anywhere but where the gesture happened.
///
/// The shape is corrected by presence, like the tables themselves, because
/// the stores that ran that build are already stamped with this kernel's
/// number and a ladder would never reach them.
fn roster(conn: &Connection) -> rusqlite::Result<()> {
    let old: i64 = conn.query_row(
        "SELECT count(*) FROM pragma_table_info('sync_peer')
          WHERE name = 'added' AND \"notnull\" = 1 AND dflt_value IS NULL",
        [],
        |r| r.get(0),
    )?;
    if old == 0 {
        return Ok(());
    }
    conn.execute_batch(
        "BEGIN IMMEDIATE;
         CREATE TABLE sync_peer_rebuilt(
           device  TEXT PRIMARY KEY,
           name    TEXT NOT NULL DEFAULT '',
           added   REAL NOT NULL DEFAULT 0,
           removed INTEGER NOT NULL DEFAULT 0
         );
         INSERT INTO sync_peer_rebuilt(device, name, added, removed)
              SELECT device, name, added, removed FROM sync_peer;
         DROP TABLE sync_peer;
         ALTER TABLE sync_peer_rebuilt RENAME TO sync_peer;
         COMMIT;",
    )
}

/// The log as the first builds of this kept it: every op ever seen, whole,
/// and a `sync_cell` table beside it saying which one had won each cell.
/// The log is compacted now, and what it holds *is* the winners, so a store
/// that still has the table is swept once — every op overtaken by a newer
/// op of its cell or a newer tombstone of its row goes — and the table is
/// dropped. By presence, like the tables themselves.
///
/// Two passes, each one walk of the log: the newest op of every cell is
/// kept and the rest go, and then, with one tombstone left per row, every
/// cell older than its row's tombstone goes. Asking each op whether some
/// newer op of its cell exists would read a cell's whole history once per
/// version of it, and a note autosaved four thousand times would hold the
/// open for seconds.
fn compacted(conn: &Connection) -> rusqlite::Result<()> {
    let old: i64 = conn.query_row(
        "SELECT count(*) FROM sqlite_master WHERE type = 'table' AND name = 'sync_cell'",
        [],
        |r| r.get(0),
    )?;
    if old == 0 {
        return Ok(());
    }
    conn.execute_batch(
        "BEGIN IMMEDIATE;
         DELETE FROM sync_op WHERE rowid IN (
           SELECT rowid FROM (
             SELECT rowid, row_number() OVER (PARTITION BY tbl, key, col
                                              ORDER BY hlc DESC, origin DESC) AS rank
               FROM sync_op)
            WHERE rank > 1);
         DELETE FROM sync_op
          WHERE col <> ''
            AND EXISTS (SELECT 1 FROM sync_op t
                         WHERE t.tbl = sync_op.tbl AND t.key = sync_op.key AND t.col = ''
                           AND (t.hlc > sync_op.hlc
                                OR (t.hlc = sync_op.hlc AND t.origin > sync_op.origin)));
         DROP TABLE sync_cell;
         COMMIT;",
    )
}

/// This device's id. Empty before the first open has written one, which is
/// only true of a connection to a store that has never been opened.
#[must_use]
pub fn this_device(conn: &Connection) -> String {
    conn.query_row("SELECT device FROM sync_self WHERE id = 1", [], |r| r.get(0))
        .unwrap_or_default()
}
