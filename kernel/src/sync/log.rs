//! The log: what a write meant, as cells, and what a peer's cells mean here.
//!
//! One [`Op`] is one cell of one row: the device that wrote it, that
//! device's own count, a hybrid logical clock, the table, the row's key, the
//! column, and the value as JSON. The empty column name is the row's
//! tombstone.
//!
//! Two ops compare by `(hlc, origin)`, so a tie has one answer on every
//! device. `sync_cell` remembers which op won each cell, so a merge is one
//! read rather than a walk of the log.

use std::collections::HashSet;

use base64::Engine;
use rusqlite::fallible_streaming_iterator::FallibleStreamingIterator;
use rusqlite::hooks::Action;
use rusqlite::session;
use rusqlite::types::{Value as SqlValue, ValueRef};
use rusqlite::Connection;
use serde_json::Value;

use super::{refused, Device, Replicated, Table, PEERS};
use crate::caps::ClockSource;

/// One cell, as it travels.
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Op {
    /// The device that made the change.
    pub origin: String,
    /// That device's own count, from 1, with no gaps.
    pub seq: i64,
    /// `(unix milliseconds << 16) | counter`.
    pub hlc: i64,
    pub tbl: String,
    /// A JSON array of the key values, in the declared order.
    pub key: String,
    /// The column, or `""` for the row's tombstone.
    pub col: String,
    /// JSON: a number, a string, `null` for SQL NULL, `{"b64": …}` for a
    /// blob.
    #[serde(default)]
    pub val: Value,
}

impl Op {
    /// The order every device merges in.
    fn after(&self, hlc: i64, origin: &str) -> bool {
        (self.hlc, self.origin.as_str()) > (hlc, origin)
    }
}

/// What one [`apply`](Log::apply) did.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Applied {
    /// Ops this store did not already hold.
    pub kept: u64,
    /// Cells written, tombstones included.
    pub applied: u64,
    /// Kept, and not applied: an older cell, an undeclared table or column,
    /// or a write a constraint refused.
    pub skipped: u64,
    /// Of those, the ones for a table or column this build does not
    /// declare. A newer build will know what to do with them.
    pub unknown: u64,
    /// Of those, the ones a constraint refused.
    pub refused: u64,
    /// The first such refusal, so a problem is reported once rather than
    /// once per op.
    pub refusal: Option<String>,
}

impl Applied {
    fn skip(&mut self) {
        self.skipped += 1;
    }
}

/// This device's half of the log: who it is, what time it stamps with, and
/// the tables it captures.
///
/// It lives on the writer thread, beside the one writable connection. Every
/// number it needs — the next sequence, the clock — is read out of
/// `sync_self` inside the transaction that moves it, so a rolled-back write
/// leaves nothing behind.
pub(crate) struct Log {
    device: String,
    name: String,
    clock: ClockSource,
    tables: Vec<Table>,
}

impl Log {
    /// Checks every declaration against the schema this store really has.
    /// The kernel's own roster is declared here whether or not the caller
    /// listed it.
    ///
    /// # Errors
    ///
    /// If a declaration does not fit the schema, in one line.
    pub(crate) fn open(conn: &Connection, device: Device) -> rusqlite::Result<Log> {
        let mut decls: Vec<Replicated> = device.tables.clone();
        if !decls.iter().any(|d| d.table.eq_ignore_ascii_case(PEERS.table)) {
            decls.insert(0, PEERS);
        }
        let mut tables: Vec<Table> = Vec::new();
        for decl in decls {
            if tables.iter().any(|t| t.decl.table.eq_ignore_ascii_case(decl.table)) {
                return Err(refused(decl.table, "it is declared twice"));
            }
            tables.push(Table::check(conn, decl)?);
        }
        Ok(Log {
            device: device.id,
            name: device.name,
            clock: device.clock,
            tables,
        })
    }

    /// The write that puts this device in the roster: its own row, written
    /// the first time the store opens and left alone after, because the
    /// name is the *device sync* panel's from then on. An ordinary captured
    /// write, so it replicates like any other.
    pub(crate) fn greeting(&self) -> impl FnOnce(&Connection) -> rusqlite::Result<()> + Send + 'static {
        let (device, name, added) = (self.device.clone(), self.name.clone(), self.clock.read());
        move |conn| {
            conn.execute(
                "INSERT OR IGNORE INTO sync_peer(device, name, added) VALUES(?1, ?2, ?3)",
                rusqlite::params![device, name, added],
            )?;
            Ok(())
        }
    }

    /// A session over the declared tables, for the length of one write.
    pub(crate) fn attach<'c>(&self, conn: &'c Connection) -> rusqlite::Result<session::Session<'c>> {
        let mut capture = session::Session::new(conn)?;
        for t in &self.tables {
            capture.attach(Some(t.decl.table))?;
        }
        Ok(capture)
    }

    fn table(&self, name: &str) -> Option<&Table> {
        self.tables.iter().find(|t| t.decl.table.eq_ignore_ascii_case(name))
    }

    // -- capture ------------------------------------------------------------

    /// Turns what the session recorded into ops, in the same transaction as
    /// the write that made them. Answers how many.
    ///
    /// An insert yields one op per replicated column; an update one per
    /// replicated column that moved, under the key read back inside this
    /// transaction; a delete one tombstone. A write that touched no
    /// replicated column emits nothing.
    ///
    /// # Errors
    ///
    /// If a key column changed — the write fails and nothing commits — or
    /// if the log cannot be written.
    pub(crate) fn emit(
        &self,
        conn: &Connection,
        capture: &mut session::Session<'_>,
    ) -> rusqlite::Result<u32> {
        let changeset = capture.changeset()?;
        let mut pending: Vec<(&Table, Vec<Value>, String, Value)> = Vec::new();
        let mut items = changeset.iter()?;
        while let Some(item) = items.next()? {
            let op = item.op()?;
            let Some(table) = self.table(op.table_name()) else {
                continue;
            };
            match op.code() {
                Action::SQLITE_INSERT => {
                    let key = read(&table.key, |i| item.new_value(i).ok());
                    for &i in &table.cells {
                        let val = item.new_value(i).ok().map(json).unwrap_or(Value::Null);
                        pending.push((table, key.clone(), table.columns[i].clone(), val));
                    }
                }
                Action::SQLITE_DELETE => {
                    let key = read(&table.key, |i| item.old_value(i).ok());
                    pending.push((table, key, String::new(), Value::Null));
                }
                Action::SQLITE_UPDATE => {
                    // A changeset's update record carries the primary key
                    // and the columns that moved, and nothing else — so a
                    // new value on a key column is a key that changed.
                    if table.key.iter().any(|&i| item.new_value(i).is_ok()) {
                        return Err(refused(
                            table.decl.table,
                            "a key column changed, and a row's key is the same on every device",
                        ));
                    }
                    let moved: Vec<(usize, Value)> = table
                        .cells
                        .iter()
                        .filter_map(|&i| item.new_value(i).ok().map(|v| (i, json(v))))
                        .collect();
                    if moved.is_empty() {
                        continue;
                    }
                    let pk: Vec<SqlValue> = table
                        .pk
                        .iter()
                        .filter_map(|name| table.columns.iter().position(|c| c == name))
                        .filter_map(|i| item.old_value(i).ok().map(sql))
                        .collect();
                    let Some(key) = read_back(conn, table, &pk)? else {
                        continue;
                    };
                    for (i, val) in moved {
                        pending.push((table, key.clone(), table.columns[i].clone(), val));
                    }
                }
                _ => {}
            }
        }
        if pending.is_empty() {
            return Ok(0);
        }
        let (mut seq, mut hlc) = mine(conn)?;
        let now = millis(&self.clock);
        let first = seq;
        for (table, key, col, val) in &pending {
            hlc = issue(hlc, now);
            let key = table.key_json(key);
            conn.execute(
                "INSERT INTO sync_op(origin, seq, hlc, tbl, key, col, val)
                 VALUES(?1, ?2, ?3, ?4, ?5, ?6, ?7)",
                rusqlite::params![self.device, seq, hlc, table.decl.table, key, col, text(val)],
            )?;
            // This device wins its own cells: it is the one writing them.
            conn.execute(
                "INSERT INTO sync_cell(tbl, key, col, hlc, origin) VALUES(?1, ?2, ?3, ?4, ?5)
                 ON CONFLICT(tbl, key, col) DO UPDATE SET hlc = excluded.hlc, origin = excluded.origin",
                rusqlite::params![table.decl.table, key, col, hlc, self.device],
            )?;
            seq += 1;
        }
        conn.execute(
            "UPDATE sync_self SET next_seq = ?1, hlc = ?2 WHERE id = 1",
            rusqlite::params![seq, hlc],
        )?;
        conn.execute(
            "INSERT INTO sync_have(origin, seq) VALUES(?1, ?2)
             ON CONFLICT(origin) DO UPDATE SET seq = excluded.seq",
            rusqlite::params![self.device, seq - 1],
        )?;
        Ok(u32::try_from(seq - first).unwrap_or(u32::MAX))
    }

    /// Ops for what was in the store before the log was.
    ///
    /// A store that has been through a migration holds rows no op ever
    /// described — notes typed before this build, a feed subscribed to, an
    /// article marked read — so a device paired with it afterwards would
    /// never hear of them. Every open walks the declared tables and files
    /// one op per cell `sync_cell` has no winner for, from this device, at
    /// **`hlc = 0`**: under every real op, so an edit made anywhere since
    /// still wins, and two devices backfilling one lineage tie by origin
    /// over values that are equal anyway.
    ///
    /// The second open emits nothing, because the first left a winner
    /// behind for every cell it touched. A column a later build adds to a
    /// declaration is backfilled on the open that declares it.
    ///
    /// # Errors
    ///
    /// If the log cannot be written.
    pub(crate) fn backfill(&self, conn: &Connection) -> rusqlite::Result<u32> {
        let (mut seq, _) = mine(conn)?;
        let first = seq;
        let mut file = conn.prepare_cached(
            "INSERT INTO sync_op(origin, seq, hlc, tbl, key, col, val)
             VALUES(?1, ?2, 0, ?3, ?4, ?5, ?6)",
        )?;
        let mut won = conn.prepare_cached(
            "INSERT INTO sync_cell(tbl, key, col, hlc, origin) VALUES(?1, ?2, ?3, 0, ?4)
             ON CONFLICT(tbl, key, col) DO NOTHING",
        )?;
        for table in &self.tables {
            // Who already speaks for a cell of this table, read once
            // rather than asked per row.
            let mut spoken: HashSet<(String, String)> = HashSet::new();
            let mut cells = conn.prepare("SELECT key, col FROM sync_cell WHERE tbl = ?1")?;
            let mut rows = cells.query([table.decl.table])?;
            while let Some(row) = rows.next()? {
                spoken.insert((row.get(0)?, row.get(1)?));
            }
            let names: Vec<String> = table
                .key
                .iter()
                .chain(table.cells.iter())
                .map(|&i| format!("\"{}\"", table.columns[i]))
                .collect();
            let sql = format!("SELECT {} FROM {}", names.join(", "), table.decl.table);
            let mut all = conn.prepare(&sql)?;
            let mut rows = all.query([])?;
            let width = table.key.len();
            while let Some(row) = rows.next()? {
                let mut values = Vec::with_capacity(width);
                for i in 0..width {
                    values.push(json(row.get_ref(i)?));
                }
                let key = table.key_json(&values);
                for (n, &i) in table.cells.iter().enumerate() {
                    let col = &table.columns[i];
                    if spoken.contains(&(key.clone(), col.clone())) {
                        continue;
                    }
                    let val = text(&json(row.get_ref(width + n)?));
                    file.execute(rusqlite::params![
                        self.device,
                        seq,
                        table.decl.table,
                        key,
                        col,
                        val
                    ])?;
                    won.execute(rusqlite::params![table.decl.table, key, col, self.device])?;
                    seq += 1;
                }
            }
        }
        if seq == first {
            return Ok(0);
        }
        // The clock stays where it was: a backfill says when nothing.
        conn.execute("UPDATE sync_self SET next_seq = ?1 WHERE id = 1", [seq])?;
        conn.execute(
            "INSERT INTO sync_have(origin, seq) VALUES(?1, ?2)
             ON CONFLICT(origin) DO UPDATE SET seq = excluded.seq",
            rusqlite::params![self.device, seq - 1],
        )?;
        Ok(u32::try_from(seq - first).unwrap_or(u32::MAX))
    }

    // -- apply --------------------------------------------------------------

    /// Takes a peer's ops, in `(origin, seq)` order, in one transaction and
    /// with no capture of its own — what arrives from another device is not
    /// this device's to log again.
    ///
    /// # Errors
    ///
    /// If the log itself cannot be written. A single op that cannot be
    /// applied is kept, skipped and counted; it never stalls the run.
    pub(crate) fn apply(&self, conn: &Connection, ops: &[Op]) -> rusqlite::Result<Applied> {
        let mut out = Applied::default();
        let mut ops: Vec<&Op> = ops.iter().collect();
        ops.sort_by(|a, b| (&a.origin, a.seq).cmp(&(&b.origin, b.seq)));
        let (_, mut hlc) = mine(conn)?;
        let mut origins: Vec<&str> = Vec::new();
        for op in ops {
            let fresh = conn.execute(
                "INSERT OR IGNORE INTO sync_op(origin, seq, hlc, tbl, key, col, val)
                 VALUES(?1, ?2, ?3, ?4, ?5, ?6, ?7)",
                rusqlite::params![op.origin, op.seq, op.hlc, op.tbl, op.key, op.col, text(&op.val)],
            )?;
            if fresh == 0 {
                continue;
            }
            out.kept += 1;
            hlc = hlc.max(op.hlc);
            if op.origin != self.device && !origins.contains(&op.origin.as_str()) {
                origins.push(&op.origin);
            }
            let Some(table) = self.table(&op.tbl) else {
                out.skip();
                out.unknown += 1;
                continue;
            };
            let known = op.col.is_empty()
                || table.cells.iter().any(|&i| table.columns[i].eq_ignore_ascii_case(&op.col));
            if !known {
                out.skip();
                out.unknown += 1;
                continue;
            }
            let Some(key) = keys(&op.key) else {
                out.skip();
                continue;
            };
            if !winner(conn, &op.tbl, &op.key, &op.col, op)? {
                out.skip();
                continue;
            }
            // A cell older than the row's tombstone is older than the
            // deletion that swept it away.
            if !op.col.is_empty() {
                if let Some((hlc, origin)) = cell(conn, &op.tbl, &op.key, "")? {
                    if !op.after(hlc, &origin) {
                        out.skip();
                        continue;
                    }
                }
            }
            let wrote = if op.col.is_empty() {
                tombstone(conn, table, &key, op)
            } else {
                put(conn, table, &key, &op.col, &value(&op.val))
            };
            match wrote {
                Ok(()) => {
                    out.applied += 1;
                    won(conn, &op.tbl, &op.key, &op.col, op)?;
                }
                Err(e) => {
                    out.skip();
                    out.refused += 1;
                    out.refusal.get_or_insert_with(|| format!("{}: {e}", op.tbl));
                }
            }
        }
        for origin in origins {
            advance(conn, origin)?;
        }
        conn.execute("UPDATE sync_self SET hlc = ?1 WHERE id = 1", [hlc])?;
        Ok(out)
    }
}

// -- what a peer is owed -------------------------------------------------------

/// What this store holds, contiguously, per origin — the vector clock a
/// `Have` carries.
pub(crate) fn have(conn: &Connection) -> rusqlite::Result<Vec<(String, i64)>> {
    let mut stmt = conn.prepare("SELECT origin, seq FROM sync_have ORDER BY origin")?;
    let rows = stmt.query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?;
    rows.collect()
}

/// Every op past what the peer says it holds, from every origin this store
/// has — which is how a device carries a third device's history — in
/// `(origin, seq)` order, up to `limit`.
pub(crate) fn ops_since(
    conn: &Connection,
    theirs: &[(String, i64)],
    limit: usize,
) -> rusqlite::Result<Vec<Op>> {
    let mut stmt = conn.prepare(
        "SELECT origin, seq, hlc, tbl, key, col, val FROM sync_op ORDER BY origin, seq",
    )?;
    let rows = stmt.query_map([], |r| {
        let val: Option<String> = r.get(6)?;
        Ok(Op {
            origin: r.get(0)?,
            seq: r.get(1)?,
            hlc: r.get(2)?,
            tbl: r.get(3)?,
            key: r.get(4)?,
            col: r.get(5)?,
            val: val.and_then(|v| serde_json::from_str(&v).ok()).unwrap_or(Value::Null),
        })
    })?;
    let mut out = Vec::new();
    for op in rows {
        let op = op?;
        let held = theirs
            .iter()
            .find(|(origin, _)| *origin == op.origin)
            .map_or(0, |(_, seq)| *seq);
        if op.seq > held {
            out.push(op);
        }
        if out.len() >= limit {
            break;
        }
    }
    Ok(out)
}

// -- the clock -----------------------------------------------------------------

/// How many bits of the hybrid clock are the counter.
const COUNTER: u32 = 16;

/// Unix milliseconds off the world's clock.
fn millis(clock: &ClockSource) -> i64 {
    (clock.read() * 1000.0) as i64
}

/// Issuing one takes `max(now << 16, last + 1)`, so two ops in one
/// millisecond still order.
fn issue(last: i64, now: i64) -> i64 {
    std::cmp::max(now << COUNTER, last + 1)
}

/// This device's next sequence and last clock.
fn mine(conn: &Connection) -> rusqlite::Result<(i64, i64)> {
    conn.query_row("SELECT next_seq, hlc FROM sync_self WHERE id = 1", [], |r| {
        Ok((r.get(0)?, r.get(1)?))
    })
}

// -- rows ----------------------------------------------------------------------

/// The winner of a cell, if one is recorded.
fn cell(
    conn: &Connection,
    tbl: &str,
    key: &str,
    col: &str,
) -> rusqlite::Result<Option<(i64, String)>> {
    let mut stmt = conn
        .prepare_cached("SELECT hlc, origin FROM sync_cell WHERE tbl = ?1 AND key = ?2 AND col = ?3")?;
    let mut rows = stmt.query(rusqlite::params![tbl, key, col])?;
    match rows.next()? {
        Some(r) => Ok(Some((r.get(0)?, r.get(1)?))),
        None => Ok(None),
    }
}

/// Whether this op beats whatever holds the cell now.
fn winner(conn: &Connection, tbl: &str, key: &str, col: &str, op: &Op) -> rusqlite::Result<bool> {
    Ok(match cell(conn, tbl, key, col)? {
        Some((hlc, origin)) => op.after(hlc, &origin),
        None => true,
    })
}

/// Records that it did.
fn won(conn: &Connection, tbl: &str, key: &str, col: &str, op: &Op) -> rusqlite::Result<()> {
    conn.execute(
        "INSERT INTO sync_cell(tbl, key, col, hlc, origin) VALUES(?1, ?2, ?3, ?4, ?5)
         ON CONFLICT(tbl, key, col) DO UPDATE SET hlc = excluded.hlc, origin = excluded.origin",
        rusqlite::params![tbl, key, col, op.hlc, op.origin],
    )?;
    Ok(())
}

/// One cell, written where the key says. A row that is not there yet is
/// made with its defaults, which is why every other column must have one.
fn put(
    conn: &Connection,
    table: &Table,
    key: &[SqlValue],
    col: &str,
    val: &SqlValue,
) -> rusqlite::Result<()> {
    let names: Vec<&str> = table.key.iter().map(|&i| table.columns[i].as_str()).collect();
    let slots: Vec<String> = (1..=names.len() + 1).map(|n| format!("?{n}")).collect();
    let sql = format!(
        "INSERT INTO {t}({cols}, \"{col}\") VALUES({slots})
         ON CONFLICT({cols}) DO UPDATE SET \"{col}\" = excluded.\"{col}\"",
        t = table.decl.table,
        cols = names.join(", "),
        slots = slots.join(", "),
    );
    let mut values: Vec<&dyn rusqlite::ToSql> = key.iter().map(|v| v as &dyn rusqlite::ToSql).collect();
    values.push(val);
    conn.execute(&sql, values.as_slice())?;
    Ok(())
}

/// A tombstone sweeps the row away — unless some cell of it is newer than
/// the deletion, in which case the row is what those newer cells say and
/// nothing else. Both orders of arrival leave the same row.
fn tombstone(conn: &Connection, table: &Table, key: &[SqlValue], op: &Op) -> rusqlite::Result<()> {
    let sql = format!(
        "DELETE FROM {} WHERE {}",
        table.decl.table,
        table
            .key
            .iter()
            .enumerate()
            .map(|(n, &i)| format!("{} = ?{}", table.columns[i], n + 1))
            .collect::<Vec<_>>()
            .join(" AND ")
    );
    let values: Vec<&dyn rusqlite::ToSql> = key.iter().map(|v| v as &dyn rusqlite::ToSql).collect();
    conn.execute(&sql, values.as_slice())?;
    for (col, val) in newer(conn, &op.tbl, &op.key, op)? {
        if table.cells.iter().any(|&i| table.columns[i].eq_ignore_ascii_case(&col)) {
            put(conn, table, key, &col, &value(&val))?;
        }
    }
    Ok(())
}

/// The winning value of every cell of a row that is newer than this
/// tombstone, read back out of the log.
fn newer(conn: &Connection, tbl: &str, key: &str, op: &Op) -> rusqlite::Result<Vec<(String, Value)>> {
    let mut stmt = conn.prepare(
        "SELECT c.col, o.val FROM sync_cell c
         JOIN sync_op o ON o.tbl = c.tbl AND o.key = c.key AND o.col = c.col
                       AND o.hlc = c.hlc AND o.origin = c.origin
         WHERE c.tbl = ?1 AND c.key = ?2 AND c.col <> ''
           AND (c.hlc > ?3 OR (c.hlc = ?3 AND c.origin > ?4))",
    )?;
    let rows = stmt.query_map(rusqlite::params![tbl, key, op.hlc, op.origin], |r| {
        let val: Option<String> = r.get(1)?;
        Ok((
            r.get::<_, String>(0)?,
            val.and_then(|v| serde_json::from_str(&v).ok()).unwrap_or(Value::Null),
        ))
    })?;
    rows.collect()
}

/// Walks `sync_have` forward over the contiguous run this origin now has.
fn advance(conn: &Connection, origin: &str) -> rusqlite::Result<()> {
    let mut at: i64 = conn
        .query_row("SELECT seq FROM sync_have WHERE origin = ?1", [origin], |r| r.get(0))
        .unwrap_or(0);
    let mut next = conn.prepare_cached("SELECT 1 FROM sync_op WHERE origin = ?1 AND seq = ?2")?;
    while next.exists(rusqlite::params![origin, at + 1])? {
        at += 1;
    }
    conn.execute(
        "INSERT INTO sync_have(origin, seq) VALUES(?1, ?2)
         ON CONFLICT(origin) DO UPDATE SET seq = excluded.seq",
        rusqlite::params![origin, at],
    )?;
    Ok(())
}

/// The key of an update's row, read back inside the same transaction by the
/// primary key the changeset carried. `None` when the row is gone already.
fn read_back(
    conn: &Connection,
    table: &Table,
    pk: &[SqlValue],
) -> rusqlite::Result<Option<Vec<Value>>> {
    if pk.is_empty() || pk.len() != table.pk.len() {
        return Ok(None);
    }
    let cols: Vec<&str> = table.key.iter().map(|&i| table.columns[i].as_str()).collect();
    let sql = format!(
        "SELECT {} FROM {} WHERE {}",
        cols.join(", "),
        table.decl.table,
        table
            .pk
            .iter()
            .enumerate()
            .map(|(n, name)| format!("{name} = ?{}", n + 1))
            .collect::<Vec<_>>()
            .join(" AND ")
    );
    let values: Vec<&dyn rusqlite::ToSql> = pk.iter().map(|v| v as &dyn rusqlite::ToSql).collect();
    let mut stmt = conn.prepare_cached(&sql)?;
    let mut rows = stmt.query(values.as_slice())?;
    let Some(row) = rows.next()? else {
        return Ok(None);
    };
    let mut out = Vec::with_capacity(cols.len());
    for i in 0..cols.len() {
        out.push(json(row.get_ref(i)?));
    }
    Ok(Some(out))
}

// -- values --------------------------------------------------------------------

/// Reads a row's values at the given columns, missing ones as SQL NULL.
fn read<'a>(at: &[usize], mut of: impl FnMut(usize) -> Option<ValueRef<'a>>) -> Vec<Value> {
    at.iter().map(|&i| of(i).map(json).unwrap_or(Value::Null)).collect()
}

/// A SQLite value as JSON. A blob is the one shape that is not a JSON value
/// of its own.
fn json(v: ValueRef<'_>) -> Value {
    match v {
        ValueRef::Null => Value::Null,
        ValueRef::Integer(i) => Value::from(i),
        ValueRef::Real(f) => Value::from(f),
        ValueRef::Text(t) => Value::from(String::from_utf8_lossy(t).into_owned()),
        ValueRef::Blob(b) => {
            let mut map = serde_json::Map::new();
            map.insert("b64".into(), Value::from(base64::engine::general_purpose::STANDARD.encode(b)));
            Value::Object(map)
        }
    }
}

/// A SQLite value, owned.
fn sql(v: ValueRef<'_>) -> SqlValue {
    match v {
        ValueRef::Null => SqlValue::Null,
        ValueRef::Integer(i) => SqlValue::Integer(i),
        ValueRef::Real(f) => SqlValue::Real(f),
        ValueRef::Text(t) => SqlValue::Text(String::from_utf8_lossy(t).into_owned()),
        ValueRef::Blob(b) => SqlValue::Blob(b.to_vec()),
    }
}

/// The other direction.
fn value(v: &Value) -> SqlValue {
    match v {
        Value::Null => SqlValue::Null,
        Value::Bool(b) => SqlValue::Integer(i64::from(*b)),
        Value::Number(n) => n
            .as_i64()
            .map(SqlValue::Integer)
            .or_else(|| n.as_f64().map(SqlValue::Real))
            .unwrap_or(SqlValue::Null),
        Value::String(s) => SqlValue::Text(s.clone()),
        Value::Object(map) => match map.get("b64").and_then(Value::as_str) {
            Some(b64) => base64::engine::general_purpose::STANDARD
                .decode(b64)
                .map(SqlValue::Blob)
                .unwrap_or(SqlValue::Null),
            None => SqlValue::Text(v.to_string()),
        },
        Value::Array(_) => SqlValue::Text(v.to_string()),
    }
}

/// How a value is written into the log: JSON text, or SQL NULL for SQL
/// NULL.
fn text(v: &Value) -> Option<String> {
    match v {
        Value::Null => None,
        other => Some(other.to_string()),
    }
}

/// The key values a JSON array carries, as SQLite values.
fn keys(key: &str) -> Option<Vec<SqlValue>> {
    match serde_json::from_str::<Value>(key).ok()? {
        Value::Array(v) => Some(v.iter().map(value).collect()),
        _ => None,
    }
}
