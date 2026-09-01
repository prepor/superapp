//! The one store: a single SQLite file holding **all** durable data — mail
//! and UI state alike — plus the small reactive layer that derives panels
//! from it (CR-001).
//!
//! Shape (rel.systems' idioms, in-process):
//! - one connection, WAL, `synchronous=NORMAL`; every mutation goes through
//!   [`Store::write`], one transaction each;
//! - `update_hook` records which tables a commit touched; each touched
//!   table's **generation** bumps at commit;
//! - reads go through [`Store::rows`]: results are cached per
//!   `(query, params)` and stamped with the generations of the tables they
//!   read — a stale entry re-runs lazily on next access. Dependencies are
//!   captured **automatically** by SQLite's authorizer at prepare time, so
//!   provenance is complete by construction (that trace is the future panel
//!   context);
//! - the logical [`core::Wm`] state persists wholesale ([`Store::save_wm`])
//!   and boot restores it — ephemeral physics (springs, cameras) stay in
//!   memory.
//!
//! History is **not** here. CR-004 moved the action tree into memory
//! ([`crate::history`]): the store keeps current state, and what an action
//! claimed of the world is an `Intent`, not a changeset.
//!
//! This module is the generic substrate; the mail domain (schema content,
//! seed, typed queries) lives in [`crate::mail`].

use std::any::Any;
use std::cell::{Cell, RefCell};
use std::collections::{BTreeSet, HashMap, HashSet};
use std::path::Path;
use std::rc::Rc;
use std::sync::{Arc, Mutex};

use rusqlite::hooks::{AuthAction, AuthContext, Authorization};
use rusqlite::{Connection, Transaction};

use crate::core::{self, Kind, PanelId};

/// A registered query: identity, SQL, and the one-line purpose that panel
/// context will hand to an agent. Declared `static` at the call site.
#[derive(Debug)]
pub struct Q {
    /// Stable name.
    pub id: &'static str,
    /// The SQL, `?n` params.
    pub sql: &'static str,
    /// What this query is for, in one line.
    pub describe: &'static str,
}

/// A query parameter — small closed set, so cache keys are trivial.
#[derive(Debug, Clone, PartialEq)]
pub enum Val {
    /// An integer (ids, timestamps).
    I(i64),
    /// Text.
    S(String),
}

impl rusqlite::ToSql for Val {
    fn to_sql(&self) -> rusqlite::Result<rusqlite::types::ToSqlOutput<'_>> {
        Ok(match self {
            Val::I(i) => rusqlite::types::ToSqlOutput::from(*i),
            Val::S(s) => rusqlite::types::ToSqlOutput::from(s.as_str()),
        })
    }
}

struct Cached {
    deps: Rc<Vec<String>>,
    gens: Vec<u64>,
    rows: Rc<dyn Any>,
}

/// The store. Single-threaded (the UI thread owns it); background workers
/// arrive in CR-001 phase 3 with their own connections.
pub struct Store {
    conn: Connection,
    /// Tables touched since the current transaction began (update_hook).
    dirty: Arc<Mutex<HashSet<String>>>,
    /// Per-table commit generation — the invalidation clock.
    generations: RefCell<HashMap<String, u64>>,
    /// Authorizer-captured read-set per SQL text.
    deps: RefCell<HashMap<&'static str, Rc<Vec<String>>>>,
    /// Result cache per `(sql, params)`.
    cache: RefCell<HashMap<(&'static str, String), Cached>>,
    redraw: Cell<bool>,
    /// Last seen `PRAGMA data_version` (foreign-commit detector).
    data_version: Cell<i64>,
    /// Per-panel query traces: which queries the panel's last draw touched
    /// — its data provenance, and the panel context an agent receives.
    traces: RefCell<HashMap<u64, Vec<TraceEntry>>>,
    active_trace: Cell<Option<u64>>,
}

/// One traced read: everything an agent needs to re-derive what a panel
/// showed.
#[derive(Debug, Clone)]
pub struct TraceEntry {
    pub id: &'static str,
    pub sql: &'static str,
    pub describe: &'static str,
    pub params: String,
    pub rows: usize,
}

/// Schema v1. UI tables are mutated only through actions (phase 2 formalizes
/// the wrapper); mail tables also by ingest (phase 3). Panel params are
/// typed columns (`p_int`, `p_txt`), not JSON — queryable and join-able,
/// which is the point of the whole exercise.
const SCHEMA_V1: &str = "
CREATE TABLE meta(key TEXT PRIMARY KEY, value ANY);

CREATE TABLE account(
  id    INTEGER PRIMARY KEY,
  label TEXT NOT NULL,
  email TEXT NOT NULL
);
CREATE TABLE folder(
  id      INTEGER PRIMARY KEY,
  account INTEGER NOT NULL REFERENCES account(id),
  name    TEXT NOT NULL,
  role    TEXT
);
CREATE TABLE message(
  id         INTEGER PRIMARY KEY,
  account    INTEGER NOT NULL REFERENCES account(id),
  folder     INTEGER NOT NULL REFERENCES folder(id),
  from_name  TEXT NOT NULL DEFAULT '',
  from_email TEXT NOT NULL DEFAULT '',
  subject    TEXT NOT NULL DEFAULT '',
  date       REAL NOT NULL,
  unread     INTEGER NOT NULL DEFAULT 0,
  body       TEXT NOT NULL DEFAULT '',
  status     TEXT,
  status_err INTEGER NOT NULL DEFAULT 0
);
CREATE INDEX idx_message_folder_date ON message(folder, date DESC);

CREATE TABLE workspace(
  k     INTEGER PRIMARY KEY,
  focus INTEGER
);
CREATE TABLE ws_col(
  ws     INTEGER NOT NULL,
  idx    INTEGER NOT NULL,
  tabbed INTEGER NOT NULL DEFAULT 0,
  active INTEGER NOT NULL DEFAULT 0,
  PRIMARY KEY(ws, idx)
);
CREATE TABLE panel(
  id        INTEGER PRIMARY KEY,
  ws        INTEGER NOT NULL,
  col       INTEGER NOT NULL,
  row       INTEGER NOT NULL,
  kind      TEXT NOT NULL,
  p_int     INTEGER,
  p_txt     TEXT,
  joined_to INTEGER
);
";

/// Schema v2 (CR-001 phase 2): the action log — the undo DAG — and the
/// `wm` row. `wm.active` moves out of `meta` because sessions must record
/// it (undo teleports you back to where the action happened) while `meta`
/// (the head pointer) must stay outside the recorded world.
const SCHEMA_V2: &str = "
CREATE TABLE action(
  id     INTEGER PRIMARY KEY,
  parent INTEGER NOT NULL DEFAULT 0,          -- 0 = root
  ts     REAL NOT NULL,
  kind   TEXT NOT NULL,
  label  TEXT NOT NULL,
  entity TEXT,
  fwd    BLOB NOT NULL,                       -- forward changeset
  state  TEXT NOT NULL DEFAULT 'applied'      -- applied | undone | expired
);
CREATE INDEX idx_action_parent ON action(parent);
CREATE TABLE wm(
  id     INTEGER PRIMARY KEY CHECK(id = 1),
  active INTEGER NOT NULL DEFAULT 0
);
INSERT INTO wm(id, active)
  SELECT 1, COALESCE((SELECT value FROM meta WHERE key='wm_active'), 0)
  WHERE EXISTS(SELECT 1 FROM meta WHERE key='wm_active');
DELETE FROM meta WHERE key='wm_active';
";

/// Schema v3 (CR-001 phase 3): real accounts. IMAP identity on folders and
/// messages, connection config and sync status on accounts, and the `dirty`
/// flag — a local change (read, archive) the server has not been told about
/// yet; reconciliation leaves dirty rows alone (phase 4's op executor
/// clears them).
const SCHEMA_V3: &str = "
ALTER TABLE account ADD COLUMN imap_host TEXT;
ALTER TABLE account ADD COLUMN smtp_host TEXT;
ALTER TABLE account ADD COLUMN status TEXT;
ALTER TABLE account ADD COLUMN synced REAL;
ALTER TABLE folder ADD COLUMN uidvalidity INTEGER;
ALTER TABLE folder ADD COLUMN uidnext INTEGER;
ALTER TABLE message ADD COLUMN uid INTEGER;
ALTER TABLE message ADD COLUMN raw BLOB;
ALTER TABLE message ADD COLUMN dirty INTEGER NOT NULL DEFAULT 0;
CREATE UNIQUE INDEX idx_message_folder_uid ON message(folder, uid)
  WHERE uid IS NOT NULL;
";

/// Schema v4 (CR-001 phase 4): the desired/actual split. `message` rows are
/// the user's **intent** (which folder, read or not); `server_msg` is what
/// the server actually holds. A row whose two sides disagree *is* the push
/// queue — no op table. Only the sync workers write `server_msg`, and it
/// stays outside the undo world, so undoing an already-pushed archive is
/// just intent flipping back: the next push pass moves it back on the
/// server. Compensation without compensation code.
const SCHEMA_V4: &str = "
CREATE TABLE server_msg(
  message INTEGER PRIMARY KEY,
  folder  INTEGER NOT NULL,
  uid     INTEGER,
  seen    INTEGER NOT NULL DEFAULT 0
);
CREATE UNIQUE INDEX idx_server_msg_uid ON server_msg(folder, uid)
  WHERE uid IS NOT NULL;
ALTER TABLE message ADD COLUMN message_id TEXT;
INSERT INTO server_msg(message, folder, uid, seen)
  SELECT id, folder, uid, NOT unread FROM message WHERE uid IS NOT NULL;
DROP INDEX idx_message_folder_uid;
ALTER TABLE message DROP COLUMN uid;
ALTER TABLE message DROP COLUMN dirty;
";

/// Schema v5 (CR-001 phase 5): drafts and the outbox. A draft belongs to
/// its compose **panel** (panel ids are stable and persisted), so
/// half-written text survives restarts; an outbox row shares the panel's
/// id — one pending send per compose, and the undo entity (`outbox:N`) is
/// known before the row exists.
const SCHEMA_V5: &str = "
CREATE TABLE draft(
  panel      INTEGER PRIMARY KEY,
  account    INTEGER,
  re_message INTEGER,
  to_addr    TEXT NOT NULL DEFAULT '',
  subject    TEXT NOT NULL DEFAULT '',
  body       TEXT NOT NULL DEFAULT '',
  updated    REAL NOT NULL DEFAULT 0
);
CREATE TABLE outbox(
  id         INTEGER PRIMARY KEY,
  account    INTEGER NOT NULL,
  send_after REAL NOT NULL,
  status     TEXT NOT NULL DEFAULT 'pending',
  error      TEXT
);
";

/// Schema v6 (main): the HTML half of a mail. `body` stays the plain-text
/// reading — it is what compose quotes, and what search will want — and
/// `html` holds the same letter narrowed to what the panel can draw (see
/// [`crate::html`]), or NULL when the sender sent text alone.
///
/// The column back-fills from `raw`, which every synced mail already
/// keeps, so mail that arrived before this migration gains its HTML
/// without a refetch. Narrowing at ingest rather than at draw leaves the
/// panel a plain `set_text`; `raw` stays the source to re-derive from when
/// the narrowing improves.
const SCHEMA_V6: &str = "
ALTER TABLE message ADD COLUMN html TEXT;
";

/// Schema v8 (CR-004): history moved into memory, so the durable action log
/// and its head pointer go. What an action claimed of the world is now an
/// `Intent` on an in-memory node; what it *wrote* is ordinary rows, which is
/// all the passes ever read. `ACTION_TABLES` went with it — nothing records
/// changesets any more.
const SCHEMA_V8: &str = "
DROP TABLE IF EXISTS action;
DELETE FROM meta WHERE key = 'head';
";

/// Schema v7 (CR-004): the effect table — one queue for every deferred
/// effect, whatever domain it came from. `payload` and `reply` are JSON
/// *text*, not JSONB: SQLite has no JSON type, and a BLOB encoding would
/// make `SELECT reply FROM effect` unreadable in a shell — inspectability
/// is why this lives in the store at all. `idempotent` is copied onto the
/// row at enqueue time so the crash sweep is pure SQL and never has to
/// decode a payload to know whether retrying is safe.
const SCHEMA_V7: &str = "
CREATE TABLE effect(
  id         INTEGER PRIMARY KEY,
  kind       TEXT NOT NULL,
  payload    TEXT NOT NULL CHECK (json_valid(payload)),
  entity     TEXT,
  status     TEXT NOT NULL DEFAULT 'pending',
  idempotent INTEGER NOT NULL DEFAULT 0,
  reply      TEXT CHECK (reply IS NULL OR json_valid(reply)),
  error      TEXT,
  attempts   INTEGER NOT NULL DEFAULT 0,
  not_before REAL NOT NULL DEFAULT 0,
  created    REAL NOT NULL,
  updated    REAL NOT NULL
);
CREATE INDEX idx_effect_due    ON effect(status, not_before);
CREATE INDEX idx_effect_entity ON effect(entity);
";

/// Fills `message.html` for mail that arrived before schema v6, reading
/// the `raw` blob each one already keeps. Messages without `raw` — the demo
/// seed — are left alone, and a mail whose sender wrote text only stays
/// NULL. Runs once, inside the migration.
pub(crate) fn backfill_html(conn: &Connection) -> rusqlite::Result<()> {
    let rows: Vec<(i64, Vec<u8>)> = conn
        .prepare("SELECT id, raw FROM message WHERE raw IS NOT NULL AND html IS NULL")?
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?
        .collect::<rusqlite::Result<_>>()?;
    for (id, raw) in rows {
        if let Some(html) = crate::sync::parse_mail(&raw).html {
            conn.execute(
                "UPDATE message SET html = ?2 WHERE id = ?1",
                rusqlite::params![id, html],
            )?;
        }
    }
    Ok(())
}

impl Store {
    /// Opens (and migrates) the store; `None` is in-memory, for tests.
    pub fn open(path: Option<&Path>) -> rusqlite::Result<Store> {
        let conn = match path {
            Some(p) => Connection::open(p)?,
            None => Connection::open_in_memory()?,
        };
        // `journal_mode` answers with a row, which pragma_update may reject.
        conn.pragma_update(None, "journal_mode", "WAL")
            .or_else(|_| conn.query_row("PRAGMA journal_mode=WAL", [], |_| Ok(())))?;
        conn.pragma_update(None, "synchronous", "NORMAL")?;
        conn.pragma_update(None, "foreign_keys", "ON")?;
        conn.busy_timeout(std::time::Duration::from_millis(5000))?;

        let version: i64 = conn.query_row("PRAGMA user_version", [], |r| r.get(0))?;
        if version < 1 {
            conn.execute_batch(SCHEMA_V1)?;
            conn.pragma_update(None, "user_version", 1)?;
        }
        if version < 2 {
            conn.execute_batch(SCHEMA_V2)?;
            conn.pragma_update(None, "user_version", 2)?;
        }
        if version < 3 {
            conn.execute_batch(SCHEMA_V3)?;
            conn.pragma_update(None, "user_version", 3)?;
        }
        if version < 4 {
            conn.execute_batch(SCHEMA_V4)?;
            conn.pragma_update(None, "user_version", 4)?;
        }
        if version < 5 {
            conn.execute_batch(SCHEMA_V5)?;
            conn.pragma_update(None, "user_version", 5)?;
        }
        if version < 6 {
            conn.execute_batch(SCHEMA_V6)?;
            backfill_html(&conn)?;
            conn.pragma_update(None, "user_version", 6)?;
        }
        if version < 7 {
            conn.execute_batch(SCHEMA_V7)?;
            conn.pragma_update(None, "user_version", 7)?;
        }
        if version < 8 {
            conn.execute_batch(SCHEMA_V8)?;
            conn.pragma_update(None, "user_version", 8)?;
        }
        sweep_effects(&conn)?;

        let dirty: Arc<Mutex<HashSet<String>>> = Arc::default();
        let d = dirty.clone();
        conn.update_hook(Some(
            move |_op, _db: &str, table: &str, _rowid: i64| {
                d.lock().expect("dirty set").insert(table.to_string());
            },
        ))?;

        Ok(Store {
            conn,
            dirty,
            generations: RefCell::default(),
            deps: RefCell::default(),
            cache: RefCell::default(),
            redraw: Cell::new(false),
            data_version: Cell::new(-1),
            traces: RefCell::default(),
            active_trace: Cell::new(None),
        })
    }

    /// Runs one mutation as one transaction; on commit, every touched
    /// table's generation bumps and dependent cached queries go stale.
    pub fn write<T>(
        &self,
        f: impl FnOnce(&Transaction) -> rusqlite::Result<T>,
    ) -> rusqlite::Result<T> {
        let tx = rusqlite::Transaction::new_unchecked(&self.conn, rusqlite::TransactionBehavior::Immediate)?;
        let out = match f(&tx) {
            Ok(v) => v,
            Err(e) => {
                drop(tx); // rollback: those writes never happened
                self.dirty.lock().expect("dirty set").clear();
                return Err(e);
            }
        };
        tx.commit()?;
        self.bump_dirty();
        Ok(out)
    }

    /// Bumps the generation of every table this commit touched, so cached
    /// queries that read them go stale. The invalidation clock, fed by
    /// `update_hook`.
    fn bump_dirty(&self) {
        let mut dirty = self.dirty.lock().expect("dirty set");
        if !dirty.is_empty() {
            let mut gens = self.generations.borrow_mut();
            for t in dirty.drain() {
                *gens.entry(t).or_insert(0) += 1;
            }
            self.redraw.set(true);
        }
    }

    /// Whether any commit landed since the last take — the shell's cue to
    /// redraw (its own writes redraw anyway; this catches future ingest).
    pub fn take_redraw(&self) -> bool {
        self.redraw.replace(false)
    }

    /// Detects commits from *other* connections (the sync workers):
    /// `data_version` moves only for foreign commits. Coarse on purpose —
    /// every table's generation bumps, every cached query re-runs; at this
    /// scale that costs microseconds. Returns whether anything changed.
    pub fn poll_external(&self) -> bool {
        let v: i64 = self
            .conn
            .query_row("PRAGMA data_version", [], |r| r.get(0))
            .unwrap_or(0);
        if v == self.data_version.replace(v) {
            return false;
        }
        let mut gens = self.generations.borrow_mut();
        for g in gens.values_mut() {
            *g += 1;
        }
        // Tables no query has touched yet have no entry — a fresh read
        // records the current (bumped) state, so nothing is missed.
        drop(gens);
        // Invalidate even never-bumped deps: wipe the cache wholesale.
        self.cache.borrow_mut().clear();
        self.redraw.set(true);
        true
    }

    /// The tables a query reads, captured by the authorizer at first
    /// prepare. This is dependency tracking *and* provenance in one.
    fn deps_for(&self, q: &'static Q) -> Rc<Vec<String>> {
        if let Some(d) = self.deps.borrow().get(q.sql) {
            return d.clone();
        }
        let seen: Arc<Mutex<BTreeSet<String>>> = Arc::default();
        let s = seen.clone();
        let _ = self.conn.authorizer(Some(move |ctx: AuthContext<'_>| {
            if let AuthAction::Read { table_name, .. } = ctx.action {
                s.lock().expect("dep set").insert(table_name.to_string());
            }
            Authorization::Allow
        }));
        let prepared = self.conn.prepare_cached(q.sql);
        let _ = self
            .conn
            .authorizer(None::<fn(AuthContext<'_>) -> Authorization>);
        if let Err(e) = prepared {
            eprintln!("store: preparing {} failed: {e}", q.id);
        }
        let tables: Vec<String> = seen.lock().expect("dep set").iter().cloned().collect();
        let rc = Rc::new(tables);
        self.deps.borrow_mut().insert(q.sql, rc.clone());
        rc
    }

    fn gen_of(&self, table: &str) -> u64 {
        self.generations.borrow().get(table).copied().unwrap_or(0)
    }

    /// Runs a registered query through the cache: a hit whose dependency
    /// generations still match returns the cached rows; anything else
    /// re-runs and re-stamps. Errors surface as an empty result (and a
    /// stderr note) — a draw pass has nowhere better to put them yet.
    /// While a trace is open, every read is recorded against it.
    pub fn rows<T: 'static>(
        &self,
        q: &'static Q,
        params: &[Val],
        map: fn(&rusqlite::Row) -> rusqlite::Result<T>,
    ) -> Rc<Vec<T>> {
        let pkey = fmt_params(params);
        let key = (q.sql, pkey.clone());
        let deps = self.deps_for(q);
        let cached: Option<Rc<Vec<T>>> = self.cache.borrow().get(&key).and_then(|c| {
            let fresh = c
                .deps
                .iter()
                .zip(&c.gens)
                .all(|(t, g)| self.gen_of(t) == *g);
            if fresh {
                c.rows.clone().downcast::<Vec<T>>().ok()
            } else {
                None
            }
        });
        let rows = cached.unwrap_or_else(|| {
            let run = || -> rusqlite::Result<Vec<T>> {
                let mut stmt = self.conn.prepare_cached(q.sql)?;
                let iter = stmt.query_map(rusqlite::params_from_iter(params.iter()), map)?;
                iter.collect()
            };
            let rows = Rc::new(run().unwrap_or_else(|e| {
                eprintln!("store: query {} failed: {e}", q.id);
                Vec::new()
            }));
            let gens = deps.iter().map(|t| self.gen_of(t)).collect();
            self.cache.borrow_mut().insert(
                key,
                Cached {
                    deps,
                    gens,
                    rows: rows.clone(),
                },
            );
            rows
        });
        if let Some(k) = self.active_trace.get() {
            let mut traces = self.traces.borrow_mut();
            if let Some(v) = traces.get_mut(&k) {
                if !v.iter().any(|e| e.id == q.id && e.params == pkey) {
                    v.push(TraceEntry {
                        id: q.id,
                        sql: q.sql,
                        describe: q.describe,
                        params: pkey,
                        rows: rows.len(),
                    });
                }
            }
        }
        rows
    }

    /// Opens a trace: reads until [`Store::trace_end`] are recorded as this
    /// key's provenance (the shell traces each panel's draw).
    pub fn trace_begin(&self, key: u64) {
        self.traces.borrow_mut().insert(key, Vec::new());
        self.active_trace.set(Some(key));
    }

    pub fn trace_end(&self) {
        self.active_trace.set(None);
    }

    /// A key's provenance, as of its last trace.
    pub fn trace_of(&self, key: u64) -> Vec<TraceEntry> {
        self.traces.borrow().get(&key).cloned().unwrap_or_default()
    }

    /// Direct read access for one-shot, non-reactive reads (boot, tests).
    #[must_use]
    pub fn conn(&self) -> &Connection {
        &self.conn
    }

    // -- wm persistence ------------------------------------------------------

    /// Persists the whole logical workspace state — a wholesale rewrite of
    /// the (tiny) UI tables in one transaction. The caller diffs snapshots
    /// and only calls this when something actually changed. Un-undoable
    /// upkeep (workspace switches, boot); undoable mutations run
    /// [`save_wm_tx`] inside [`Store::act`] instead.
    pub fn save_wm(&self, snap: &core::WmSnap) -> rusqlite::Result<()> {
        self.write(|c| save_wm_tx(c, snap))
    }

    /// Restores the logical workspace state; `None` means this store never
    /// booted (first run seeds the default layout). An empty-but-booted
    /// store restores as genuinely empty — closing everything is a state,
    /// not an accident.
    pub fn load_wm(&self) -> rusqlite::Result<Option<core::WmSnap>> {
        let active: Option<i64> = self
            .conn
            .query_row("SELECT active FROM wm WHERE id=1", [], |r| r.get(0))
            .ok();
        let Some(active) = active else {
            return Ok(None);
        };
        let mut wss = vec![core::WsSnap::default(); core::WS_N];
        {
            let mut stmt = self.conn.prepare("SELECT k, focus FROM workspace")?;
            let rows = stmt.query_map([], |r| {
                Ok((r.get::<_, i64>(0)?, r.get::<_, Option<i64>>(1)?))
            })?;
            for row in rows {
                let (k, focus) = row?;
                if let Some(ws) = wss.get_mut(k as usize) {
                    ws.focus = focus.map(|f| f as PanelId);
                }
            }
        }
        {
            let mut stmt = self
                .conn
                .prepare("SELECT ws, idx, tabbed, active FROM ws_col ORDER BY ws, idx")?;
            let rows = stmt.query_map([], |r| {
                Ok((
                    r.get::<_, i64>(0)?,
                    r.get::<_, bool>(2)?,
                    r.get::<_, i64>(3)?,
                ))
            })?;
            for row in rows {
                let (k, tabbed, active) = row?;
                if let Some(ws) = wss.get_mut(k as usize) {
                    ws.columns.push((Vec::new(), tabbed, active as usize));
                }
            }
        }
        {
            let mut stmt = self.conn.prepare(
                "SELECT id, ws, col, kind, p_int, p_txt, joined_to FROM panel
                 ORDER BY ws, col, row",
            )?;
            let rows = stmt.query_map([], |r| {
                Ok((
                    r.get::<_, i64>(0)? as PanelId,
                    r.get::<_, i64>(1)? as usize,
                    r.get::<_, i64>(2)? as usize,
                    r.get::<_, String>(3)?,
                    r.get::<_, Option<i64>>(4)?,
                    r.get::<_, Option<String>>(5)?,
                    r.get::<_, Option<i64>>(6)?,
                ))
            })?;
            for row in rows {
                let (id, k, col, kind, p_int, p_txt, joined_to) = row?;
                let Some(ws) = wss.get_mut(k) else { continue };
                let Some(kind) = kind_from(&kind, p_int, p_txt) else {
                    continue; // an unknown kind (downgrade?) is dropped, not fatal
                };
                ws.panels.push((id, kind));
                if let Some((panels, _, _)) = ws.columns.get_mut(col) {
                    panels.push(id);
                }
                if let Some(parent) = joined_to {
                    ws.joins.push((parent as PanelId, id));
                }
            }
        }
        for ws in &mut wss {
            ws.panels.sort_by_key(|(id, _)| *id);
            ws.joins.sort_unstable();
        }
        Ok(Some(core::WmSnap {
            active: (active as usize).min(core::WS_N - 1),
            wss,
        }))
    }
}

/// The wholesale UI-table rewrite behind [`Store::save_wm`] — also what an
/// undoable navigation action runs inside its recorded transaction (the
/// session consolidates per row, so an identical rewrite records nothing
/// and only real deltas reach the changeset).
pub fn save_wm_tx(c: &Connection, snap: &core::WmSnap) -> rusqlite::Result<()> {
    c.execute("DELETE FROM panel", [])?;
    c.execute("DELETE FROM ws_col", [])?;
    c.execute("DELETE FROM workspace", [])?;
    for (k, ws) in snap.wss.iter().enumerate() {
        c.execute(
            "INSERT INTO workspace(k, focus) VALUES(?1, ?2)",
            rusqlite::params![k as i64, ws.focus.map(|f| f as i64)],
        )?;
        let parent_of: HashMap<PanelId, PanelId> =
            ws.joins.iter().map(|&(a, b)| (b, a)).collect();
        let kind_of: HashMap<PanelId, &Kind> =
            ws.panels.iter().map(|(id, k)| (*id, k)).collect();
        for (ci, (panels, tabbed, active)) in ws.columns.iter().enumerate() {
            c.execute(
                "INSERT INTO ws_col(ws, idx, tabbed, active) VALUES(?1,?2,?3,?4)",
                rusqlite::params![k as i64, ci as i64, tabbed, *active as i64],
            )?;
            for (ri, pid) in panels.iter().enumerate() {
                let Some(kind) = kind_of.get(pid) else {
                    continue;
                };
                let (name, p_int, p_txt) = kind_cols(kind);
                c.execute(
                    "INSERT INTO panel(id, ws, col, row, kind, p_int, p_txt, joined_to)
                     VALUES(?1,?2,?3,?4,?5,?6,?7,?8)",
                    rusqlite::params![
                        *pid as i64,
                        k as i64,
                        ci as i64,
                        ri as i64,
                        name,
                        p_int,
                        p_txt,
                        parent_of.get(pid).map(|p| *p as i64),
                    ],
                )?;
            }
        }
    }
    c.execute(
        "INSERT INTO wm(id, active) VALUES(1, ?1)
         ON CONFLICT(id) DO UPDATE SET active=excluded.active",
        [snap.active as i64],
    )?;
    Ok(())
}

/// The crash sweep, run at every open: a job left `processing` was in
/// flight when the process died, and nobody knows whether it reached the
/// world. Idempotent ones are safe to run again; the rest must **not** be
/// guessed at — a second `submit` is a second mail — so they fail and wait
/// for a human. This is the whole reason `Deferred::idempotent` has no
/// default.
///
/// # Errors
///
/// If either update fails.
pub fn sweep_effects(conn: &Connection) -> rusqlite::Result<()> {
    conn.execute(
        "UPDATE effect SET status='pending' WHERE status='processing' AND idempotent=1",
        [],
    )?;
    conn.execute(
        "UPDATE effect SET status='failed', error='interrupted; outcome unknown'
         WHERE status='processing' AND idempotent=0",
        [],
    )?;
    Ok(())
}

/// Query params rendered for humans (and cache keys).
fn fmt_params(params: &[Val]) -> String {
    if params.is_empty() {
        return String::new();
    }
    params
        .iter()
        .map(|v| match v {
            Val::I(i) => i.to_string(),
            Val::S(s) => format!("'{s}'"),
        })
        .collect::<Vec<_>>()
        .join(", ")
}

/// [`Kind`] → its persisted row: `(kind, p_int, p_txt)`.
pub fn kind_cols(kind: &Kind) -> (&'static str, Option<i64>, Option<String>) {
    match kind {
        Kind::Help => ("help", None, None),
        Kind::About => ("about", None, None),
        Kind::Inbox { filter } => ("inbox", None, filter.clone()),
        Kind::Message { id } => ("message", Some(*id), None),
        Kind::Contact { email } => ("contact", None, Some(email.clone())),
        Kind::Compose { re } => ("compose", Some(*re), None),
        Kind::Settings => ("settings", None, None),
        Kind::AddAccount => ("add_account", None, None),
    }
}

/// The persisted row → [`Kind`]; `None` for rows this build cannot read.
fn kind_from(kind: &str, p_int: Option<i64>, p_txt: Option<String>) -> Option<Kind> {
    Some(match kind {
        "help" => Kind::Help,
        "about" => Kind::About,
        "inbox" => Kind::Inbox { filter: p_txt },
        "message" => Kind::Message { id: p_int? },
        "contact" => Kind::Contact { email: p_txt? },
        "compose" => Kind::Compose { re: p_int? },
        "settings" => Kind::Settings,
        "add_account" => Kind::AddAccount,
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::{Kind, Wm};

    fn store() -> Store {
        Store::open(None).expect("in-memory store")
    }

    static Q_META: Q = Q {
        id: "meta_probe",
        sql: "SELECT value FROM meta WHERE key='probe'",
        describe: "test probe",
    };

    fn probe(r: &rusqlite::Row) -> rusqlite::Result<i64> {
        r.get(0)
    }

    /// The reactive contract: cached until a commit touches a dependency,
    /// re-run after, and the authorizer captured the dependency itself.
    #[test]
    fn queries_cache_and_invalidate_by_table_generation() {
        let s = store();
        assert!(s.rows(&Q_META, &[], probe).is_empty());
        assert_eq!(*s.deps_for(&Q_META), vec!["meta".to_string()]);

        s.write(|c| {
            c.execute("INSERT INTO meta(key, value) VALUES('probe', 7)", [])
                .map(|_| ())
        })
        .unwrap();
        assert!(s.take_redraw(), "a commit wants a redraw");
        assert_eq!(*s.rows(&Q_META, &[], probe), vec![7]);

        // An unrelated table's commit must NOT re-run the query: same Rc.
        let before = s.rows(&Q_META, &[], probe);
        s.write(|c| {
            c.execute("INSERT INTO account(label, email) VALUES('x', 'x@x')", [])
                .map(|_| ())
        })
        .unwrap();
        let after = s.rows(&Q_META, &[], probe);
        assert!(Rc::ptr_eq(&before, &after), "cache survived foreign commit");

        // A rolled-back write invalidates nothing.
        let _ = s.write(|c| -> rusqlite::Result<()> {
            c.execute("UPDATE meta SET value=8 WHERE key='probe'", [])?;
            Err(rusqlite::Error::QueryReturnedNoRows)
        });
        assert_eq!(*s.rows(&Q_META, &[], probe), vec![7], "rollback kept 7");
    }

    /// Wm state survives the store: save → load → restore is the same
    /// logical state, and a never-booted store loads as None.
    #[test]
    fn wm_round_trips_through_the_store() {
        let s = store();
        assert!(s.load_wm().unwrap().is_none(), "fresh store: no session");

        let mut wm = Wm::new();
        let inbox = wm.open(Kind::Inbox { filter: None }, None, false);
        let _msg = wm.follow_open(inbox, Kind::Message { id: 3 }, false);
        wm.send_focused_to(4); // the message re-homes to ws 5
        wm.switch(0);
        wm.toggle_tabbed(inbox); // a surviving tabbed column
        wm.follow_open(inbox, Kind::Contact { email: "v@k.io".into() }, false);
        let snap = wm.snapshot();
        assert!(!snap.wss[0].joins.is_empty(), "a live join persists");

        s.save_wm(&snap).unwrap();
        let loaded = s.load_wm().unwrap().expect("booted store: a session");
        assert_eq!(loaded, snap);

        // Empty-but-booted restores as empty, not as a fresh boot.
        let empty = Wm::new().snapshot();
        s.save_wm(&empty).unwrap();
        assert_eq!(s.load_wm().unwrap(), Some(empty));
    }
}
