//! Device sync: the log, and what carries it between two devices.
//!
//! Each device owns its store. What travels is not the file but what a
//! person decided on one of them — a subscription, a read mark, a note, the
//! name of a device — as individual cells, last writer wins.
//!
//! An app declares what replicates ([`Replicated`]): the table, the key that
//! names a row on every device, and the columns that carry a decision. The
//! writer captures every transaction over those tables and turns it into
//! [`Op`]s in the same transaction; [`crate::store::Db::apply_ops`] takes a
//! peer's ops the other way, without a capture, so nothing echoes.
//!
//! [`self::protocol`] is the conversation two devices have over one byte
//! stream, and [`self::iroh`] is the transport underneath it — an endpoint,
//! a ticket, and a stream per peer. The protocol is written against
//! `AsyncRead + AsyncWrite` and tested over `tokio::io::duplex`;
//! [`self::iroh::Link`] is the other implementation of that stream.
//!
//! [`Service`] is what runs both on a device: one kernel task with the
//! endpoint, an accept loop and a dial loop, mounted by the session at boot
//! and read by the *device sync* panel through [`SyncStatus`].

use rusqlite::Connection;

use crate::caps::ClockSource;

pub mod iroh;
pub(crate) mod log;
pub mod protocol;
mod schema;
mod service;
mod status;
#[cfg(test)]
mod tests;

pub use iroh::{device_of, new_secret, secret_from_hex, secret_to_hex, Mode, ALPN, SECRET_KEY};
pub use log::{Applied, Op};
pub use schema::this_device;
pub use service::{Mount, Pairing, Service};
pub use status::{PeerStatus, SyncStatus};
pub(crate) use schema::ensure;

/// A table that replicates, as the app that owns it declares.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Replicated {
    /// The table.
    pub table: &'static str,
    /// The columns that identify a row on every device; a UNIQUE index
    /// over exactly these must exist. Never a rowid.
    pub key: &'static [&'static str],
    /// The columns whose values travel. At least one.
    pub columns: &'static [&'static str],
}

/// The kernel's own: the roster of devices replicates like anything else,
/// so pairing on one device is pairing on all of them.
pub const PEERS: Replicated = Replicated {
    table: "sync_peer",
    key: &["device"],
    columns: &["name", "added", "removed"],
};

/// This device, as the store needs to know it: the id it signs its ops
/// with, the name it introduces itself by, the clock those ops are stamped
/// from, and what replicates.
///
/// The id is an iroh endpoint id — sixty-four lowercase hex characters, the
/// public half of the key in the platform secret store under
/// [`SECRET_KEY`]. The shell reads that key at boot and hands the id here;
/// nothing invents one behind a caller's back.
#[derive(Clone, Debug)]
pub struct Device {
    id: String,
    name: String,
    clock: ClockSource,
    tables: Vec<Replicated>,
}

impl Device {
    /// A device by id, with no name, the default clock and nothing
    /// declared.
    #[must_use]
    pub fn new(id: impl Into<String>) -> Device {
        Device {
            id: id.into(),
            name: String::new(),
            clock: ClockSource::default(),
            tables: Vec::new(),
        }
    }

    /// The canned identity of a store no device syncs — every test's, and a
    /// library mount's. Sixty-four `a`s, so it is recognisable in a log.
    #[must_use]
    pub fn fake() -> Device {
        Device::new("a".repeat(64))
    }

    /// What this device calls itself. Its own `sync_peer` row is written
    /// with this the first time the store opens; after that the *device
    /// sync* panel owns the name.
    #[must_use]
    pub fn named(mut self, name: impl Into<String>) -> Device {
        self.name = name.into();
        self
    }

    /// The clock its ops are stamped from — the world's, not the wall's, so
    /// a scripted run stays deterministic.
    #[must_use]
    pub fn clocked(mut self, clock: ClockSource) -> Device {
        self.clock = clock;
        self
    }

    /// What replicates, usually [`crate::app::Apps::replicated`].
    #[must_use]
    pub fn replicating(mut self, tables: Vec<Replicated>) -> Device {
        self.tables = tables;
        self
    }

    /// This device's id.
    #[must_use]
    pub fn id(&self) -> &str {
        &self.id
    }

    /// The clock its ops are stamped from.
    #[must_use]
    pub fn clock(&self) -> &ClockSource {
        &self.clock
    }
}

/// A refusal, in one line, the way a kernel version mismatch is.
pub(crate) fn refused(table: &str, why: &str) -> rusqlite::Error {
    crate::store::store_err(&format!("{table} cannot replicate: {why}"))
}

/// One declaration resolved against the schema this store really has.
pub(crate) struct Table {
    pub(crate) decl: Replicated,
    /// Every column of the table, in the order a changeset reports them.
    pub(crate) columns: Vec<String>,
    /// Where the key columns sit in `columns`, in the declared order.
    pub(crate) key: Vec<usize>,
    /// Where the replicated columns sit, in the declared order.
    pub(crate) cells: Vec<usize>,
    /// The table's own primary key — how an update's row is found again,
    /// because a changeset's update record carries only that and what moved.
    pub(crate) pk: Vec<String>,
}

impl Table {
    /// Reads the table's shape and checks the declaration against it: the
    /// table exists and has a primary key, the key has a unique index over
    /// exactly its columns, every named column exists, at least one column
    /// travels, no column is both key and cell, and every column but the
    /// key can be left out of an insert — because a row made on another
    /// device arrives one cell at a time, so the op that creates it names
    /// the key, one column, and nothing else.
    pub(crate) fn check(conn: &Connection, decl: Replicated) -> rusqlite::Result<Table> {
        let info = columns_of(conn, decl.table)?;
        if info.is_empty() {
            return Err(refused(decl.table, "this store has no such table"));
        }
        let at = |name: &str| info.iter().position(|c| c.name.eq_ignore_ascii_case(name));
        if decl.columns.is_empty() {
            return Err(refused(decl.table, "nothing is declared to travel"));
        }
        let mut key = Vec::new();
        for name in decl.key {
            let Some(i) = at(name) else {
                return Err(refused(decl.table, &format!("it has no column {name}")));
            };
            key.push(i);
        }
        let mut cells = Vec::new();
        for name in decl.columns {
            let Some(i) = at(name) else {
                return Err(refused(decl.table, &format!("it has no column {name}")));
            };
            if key.contains(&i) {
                return Err(refused(
                    decl.table,
                    &format!("{name} is both its key and a value that travels"),
                ));
            }
            cells.push(i);
        }
        if !info.iter().any(|c| c.pk) {
            return Err(refused(
                decl.table,
                "it has no primary key, so SQLite records none of its changes",
            ));
        }
        if !unique_over(conn, decl.table, decl.key)? {
            return Err(refused(
                decl.table,
                &format!(
                    "no unique index over exactly ({}) — a key names one row on every device",
                    decl.key.join(", ")
                ),
            ));
        }
        // Every column but the key, whether it travels or not. A row
        // arrives one cell at a time — an update's ops carry only what
        // moved — so the insert that writes the first of them names the
        // key and that one column and nothing else.
        for (i, c) in info.iter().enumerate() {
            if !key.contains(&i) && c.required {
                return Err(refused(
                    decl.table,
                    &format!(
                        "{} is required and has no default, so a row another device made \
                         could not be written here",
                        c.name
                    ),
                ));
            }
        }
        Ok(Table {
            decl,
            pk: info.iter().filter(|c| c.pk).map(|c| c.name.clone()).collect(),
            columns: info.into_iter().map(|c| c.name).collect(),
            key,
            cells,
        })
    }

    /// The key values of a row, in the declared order, as a JSON array —
    /// how a cell names its row on every device.
    pub(crate) fn key_json(&self, values: &[serde_json::Value]) -> String {
        serde_json::Value::Array(values.to_vec()).to_string()
    }
}

/// One column of a table, as far as the check cares.
struct Column {
    name: String,
    /// NOT NULL with no default, and not the rowid: an insert must name it.
    required: bool,
    /// Part of the table's own primary key.
    pk: bool,
}

fn columns_of(conn: &Connection, table: &str) -> rusqlite::Result<Vec<Column>> {
    let mut stmt = conn.prepare(
        "SELECT name, type, \"notnull\", dflt_value, pk FROM pragma_table_info(?1)",
    )?;
    let rows = stmt.query_map([table], |r| {
        let name: String = r.get(0)?;
        let kind: String = r.get(1)?;
        let notnull: bool = r.get(2)?;
        let default: Option<String> = r.get(3)?;
        let pk: i64 = r.get(4)?;
        // An INTEGER PRIMARY KEY is the rowid under another name: SQLite
        // fills it in, so an insert that leaves it out is fine.
        let rowid = pk == 1 && kind.eq_ignore_ascii_case("integer");
        Ok(Column {
            name,
            required: notnull && default.is_none() && !rowid,
            pk: pk > 0,
        })
    })?;
    rows.collect()
}

/// Whether some unique index — or the primary key itself — covers exactly
/// these columns and no others. A partial index does not count: it promises
/// nothing about the rows it leaves out.
fn unique_over(conn: &Connection, table: &str, key: &[&str]) -> rusqlite::Result<bool> {
    let want: Vec<String> = key.iter().map(|k| k.to_lowercase()).collect();
    let same = |mut cols: Vec<String>| {
        cols.sort();
        let mut want = want.clone();
        want.sort();
        cols == want
    };
    // The primary key of a rowid table is reported by `table_info` and, for
    // a single INTEGER column, by no index at all.
    let pk: Vec<String> = columns_of(conn, table)?
        .into_iter()
        .filter(|c| c.pk)
        .map(|c| c.name.to_lowercase())
        .collect();
    if same(pk) {
        return Ok(true);
    }
    let mut list = conn.prepare("SELECT name, \"unique\", partial FROM pragma_index_list(?1)")?;
    let indexes: Vec<String> = list
        .query_map([table], |r| {
            let name: String = r.get(0)?;
            let unique: bool = r.get(1)?;
            let partial: bool = r.get(2)?;
            Ok((name, unique && !partial))
        })?
        .filter_map(|r| r.ok().filter(|(_, keeps)| *keeps).map(|(n, _)| n))
        .collect();
    let mut info = conn.prepare("SELECT name FROM pragma_index_info(?1)")?;
    for index in indexes {
        let cols: Vec<String> = info
            .query_map([&index], |r| r.get::<_, Option<String>>(0))?
            .filter_map(|r| r.ok().flatten())
            .map(|c| c.to_lowercase())
            .collect();
        if same(cols) {
            return Ok(true);
        }
    }
    Ok(false)
}
