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
//! This module is the generic substrate; the mail domain (schema content,
//! seed, typed queries) lives in [`crate::mail`].

use std::any::Any;
use std::cell::{Cell, RefCell};
use std::collections::{BTreeSet, HashMap, HashSet};
use std::path::Path;
use std::rc::Rc;
use std::sync::{Arc, Mutex};

use rusqlite::hooks::{AuthAction, AuthContext, Authorization};
use rusqlite::session::{self, Changegroup, ConflictAction, Session};
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
    /// Answers "can this action no longer be undone?" (an expired send).
    /// Domain knowledge injected by the shell; the walk marks such nodes
    /// `expired` and skips them transparently.
    undo_guard: RefCell<Option<Box<dyn Fn(&Connection, &str, Option<&str>) -> bool>>>,
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

/// The tables sessions record — the undoable world. `action` and `meta`
/// (the head pointer) stay outside it — undo must never rewrite history's
/// own bookkeeping — and so does `server_msg`: what the server holds is
/// fact, not intent, and executor writes must never collide with undo.
/// `draft` and `outbox` are in — undoing a discard restores the text,
/// undoing a send cancels the pending row. (Typing itself never records:
/// keystrokes are plain writes outside `act`, the future editor's local
/// undo, not the system's.)
const ACTION_TABLES: [&str; 9] = [
    "account", "folder", "message", "workspace", "ws_col", "panel", "wm", "draft",
    "outbox",
];

/// Actions of the same kind on the same entity within this window amend the
/// head node instead of growing the tree (a burst of moves is one action).
const COALESCE_S: f64 = 2.5;

/// Unix seconds — the action log's clock.
#[must_use]
pub fn now() -> f64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs_f64())
        .unwrap_or(0.0)
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
            undo_guard: RefCell::new(None),
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

// -- the action log -----------------------------------------------------------

/// One node of the history DAG, as the overlay reads it.
#[derive(Debug, Clone)]
pub struct ActionNode {
    pub id: i64,
    pub parent: i64,
    pub ts: f64,
    pub kind: String,
    pub label: String,
    pub state: String,
}

impl Store {
    fn head(&self, c: &Connection) -> rusqlite::Result<i64> {
        Ok(c.query_row("SELECT value FROM meta WHERE key='head'", [], |r| r.get(0))
            .unwrap_or(0))
    }

    fn set_head(&self, c: &Connection, id: i64) -> rusqlite::Result<()> {
        c.execute(
            "INSERT INTO meta(key, value) VALUES('head', ?1)
             ON CONFLICT(key) DO UPDATE SET value=excluded.value",
            [id],
        )?;
        Ok(())
    }

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

    /// Runs one **undoable action**: `f`'s transaction is recorded by a
    /// session over the undoable tables, and the resulting changeset becomes
    /// a node of the history DAG (child of HEAD; acting mid-tree branches).
    /// A same-kind, same-entity action within [`COALESCE_S`] amends the head
    /// node instead. A transaction that nets no change creates no node.
    pub fn act<T>(
        &self,
        kind: &str,
        label: &str,
        entity: Option<&str>,
        now: f64,
        f: impl FnOnce(&Transaction) -> rusqlite::Result<T>,
    ) -> rusqlite::Result<T> {
        let mut sess = Session::new(&self.conn)?;
        for t in ACTION_TABLES {
            sess.attach(Some(t))?;
        }
        let tx = rusqlite::Transaction::new_unchecked(&self.conn, rusqlite::TransactionBehavior::Immediate)?;
        let out = match f(&tx) {
            Ok(v) => v,
            Err(e) => {
                drop(tx);
                drop(sess);
                self.dirty.lock().expect("dirty set").clear();
                return Err(e);
            }
        };
        let mut fwd: Vec<u8> = Vec::new();
        sess.changeset_strm(&mut fwd)?;
        drop(sess);
        if fwd.is_empty() {
            // Nothing actually changed: no node, nothing to undo.
            tx.commit()?;
            self.bump_dirty();
            return Ok(out);
        }
        let head = self.head(&tx)?;
        let mut coalesced = false;
        if let Some(ent) = entity {
            let head_row: Option<(String, Option<String>, f64, String, Vec<u8>)> = tx
                .query_row(
                    "SELECT kind, entity, ts, state, fwd FROM action WHERE id=?1",
                    [head],
                    |r| {
                        Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?))
                    },
                )
                .ok();
            if let Some((hkind, hent, hts, hstate, hfwd)) = head_row {
                if hkind == kind
                    && hent.as_deref() == Some(ent)
                    && hstate == "applied"
                    && (now - hts) < COALESCE_S
                {
                    let mut grp = Changegroup::new()?;
                    grp.add_stream(&mut hfwd.as_slice())?;
                    grp.add_stream(&mut fwd.as_slice())?;
                    let mut merged: Vec<u8> = Vec::new();
                    grp.output_strm(&mut merged)?;
                    tx.execute(
                        "UPDATE action SET fwd=?1, ts=?2 WHERE id=?3",
                        rusqlite::params![merged, now, head],
                    )?;
                    coalesced = true;
                }
            }
        }
        if !coalesced {
            tx.execute(
                "INSERT INTO action(parent, ts, kind, label, entity, fwd)
                 VALUES(?1,?2,?3,?4,?5,?6)",
                rusqlite::params![head, now, kind, label, entity, fwd],
            )?;
            let id = tx.last_insert_rowid();
            self.set_head(&tx, id)?;
        }
        tx.commit()?;
        self.bump_dirty();
        Ok(out)
    }

    /// Injects the domain judgement for irreversibility (see `undo_guard`).
    pub fn set_undo_guard(
        &self,
        guard: impl Fn(&Connection, &str, Option<&str>) -> bool + 'static,
    ) {
        *self.undo_guard.borrow_mut() = Some(Box::new(guard));
    }

    /// Undoes the head action: applies its inverted changeset (conflicting
    /// rows — changed since by ingest — are skipped, not forced), marks it
    /// undone, moves HEAD to its parent. An `expired` head (a send past its
    /// window) is skipped over transparently. Returns the undone action's
    /// label.
    pub fn undo(&self) -> rusqlite::Result<Option<String>> {
        let tx = rusqlite::Transaction::new_unchecked(&self.conn, rusqlite::TransactionBehavior::Immediate)?;
        let mut head = self.head(&tx)?;
        loop {
            if head == 0 {
                tx.commit()?;
                return Ok(None);
            }
            let (parent, label, state, kind, entity, fwd): (
                i64,
                String,
                String,
                String,
                Option<String>,
                Vec<u8>,
            ) = tx.query_row(
                "SELECT parent, label, state, kind, entity, fwd FROM action WHERE id=?1",
                [head],
                |r| {
                    Ok((
                        r.get(0)?,
                        r.get(1)?,
                        r.get(2)?,
                        r.get(3)?,
                        r.get(4)?,
                        r.get(5)?,
                    ))
                },
            )?;
            let now_expired = state != "expired"
                && self
                    .undo_guard
                    .borrow()
                    .as_ref()
                    .is_some_and(|g| g(&tx, &kind, entity.as_deref()));
            if now_expired {
                tx.execute("UPDATE action SET state='expired' WHERE id=?1", [head])?;
            }
            if state == "expired" || now_expired {
                // Transparent: the world can't roll it back; walk past it.
                self.set_head(&tx, parent)?;
                head = parent;
                continue;
            }
            let mut inv: Vec<u8> = Vec::new();
            session::invert_strm(&mut fwd.as_slice(), &mut inv)?;
            self.conn.apply_strm(
                &mut inv.as_slice(),
                None::<fn(&str) -> bool>,
                |_t, _item| ConflictAction::SQLITE_CHANGESET_OMIT,
            )?;
            tx.execute("UPDATE action SET state='undone' WHERE id=?1", [head])?;
            self.set_head(&tx, parent)?;
            tx.commit()?;
            self.bump_dirty();
            return Ok(Some(label));
        }
    }

    /// Redoes the most recent undone child of HEAD (the default branch) and
    /// moves HEAD onto it. Returns its label.
    pub fn redo(&self) -> rusqlite::Result<Option<String>> {
        let tx = rusqlite::Transaction::new_unchecked(&self.conn, rusqlite::TransactionBehavior::Immediate)?;
        let head = self.head(&tx)?;
        let child: Option<(i64, String, Vec<u8>)> = tx
            .query_row(
                "SELECT id, label, fwd FROM action
                 WHERE parent=?1 AND state='undone' ORDER BY id DESC LIMIT 1",
                [head],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )
            .ok();
        let Some((id, label, fwd)) = child else {
            tx.commit()?;
            return Ok(None);
        };
        self.conn.apply_strm(
            &mut fwd.as_slice(),
            None::<fn(&str) -> bool>,
            |_t, _item| ConflictAction::SQLITE_CHANGESET_OMIT,
        )?;
        tx.execute("UPDATE action SET state='applied' WHERE id=?1", [id])?;
        self.set_head(&tx, id)?;
        tx.commit()?;
        self.bump_dirty();
        Ok(Some(label))
    }

    /// Travels to any node of the DAG: undo up to the lowest common
    /// ancestor, then re-apply down the target's branch. Expired nodes are
    /// transparent in both directions (their effects are physics). `0` is
    /// the origin — undo everything. Returns the target's label.
    pub fn travel(&self, target: i64) -> rusqlite::Result<Option<String>> {
        let tx = rusqlite::Transaction::new_unchecked(&self.conn, rusqlite::TransactionBehavior::Immediate)?;
        let chain = |from: i64| -> rusqlite::Result<Vec<i64>> {
            let mut v = vec![from];
            let mut cur = from;
            while cur != 0 {
                cur = tx.query_row("SELECT parent FROM action WHERE id=?1", [cur], |r| {
                    r.get(0)
                })?;
                v.push(cur);
            }
            Ok(v)
        };
        let head = self.head(&tx)?;
        if target == head {
            tx.commit()?;
            return Ok(None);
        }
        let hc = chain(head)?;
        let tc = chain(target)?;
        let lca = *hc
            .iter()
            .find(|id| tc.contains(id))
            .expect("the root is always common");

        // Up: undo from head to the LCA.
        let mut cur = head;
        while cur != lca {
            let (parent, state, kind, entity, fwd): (
                i64,
                String,
                String,
                Option<String>,
                Vec<u8>,
            ) = tx.query_row(
                "SELECT parent, state, kind, entity, fwd FROM action WHERE id=?1",
                [cur],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?)),
            )?;
            let now_expired = state != "expired"
                && self
                    .undo_guard
                    .borrow()
                    .as_ref()
                    .is_some_and(|g| g(&tx, &kind, entity.as_deref()));
            if now_expired {
                tx.execute("UPDATE action SET state='expired' WHERE id=?1", [cur])?;
            }
            if state != "expired" && !now_expired {
                let mut inv: Vec<u8> = Vec::new();
                session::invert_strm(&mut fwd.as_slice(), &mut inv)?;
                self.conn.apply_strm(
                    &mut inv.as_slice(),
                    None::<fn(&str) -> bool>,
                    |_t, _i| ConflictAction::SQLITE_CHANGESET_OMIT,
                )?;
                tx.execute("UPDATE action SET state='undone' WHERE id=?1", [cur])?;
            }
            cur = parent;
        }

        // Down: re-apply from below the LCA to the target.
        let down: Vec<i64> = tc.iter().take_while(|&&id| id != lca).copied().collect();
        for &id in down.iter().rev() {
            let (state, fwd): (String, Vec<u8>) = tx.query_row(
                "SELECT state, fwd FROM action WHERE id=?1",
                [id],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )?;
            if state == "expired" {
                continue; // its effects never left
            }
            self.conn.apply_strm(
                &mut fwd.as_slice(),
                None::<fn(&str) -> bool>,
                |_t, _i| ConflictAction::SQLITE_CHANGESET_OMIT,
            )?;
            tx.execute("UPDATE action SET state='applied' WHERE id=?1", [id])?;
        }
        self.set_head(&tx, target)?;
        let label: Option<String> = if target == 0 {
            Some("the beginning".into())
        } else {
            tx.query_row("SELECT label FROM action WHERE id=?1", [target], |r| {
                r.get(0)
            })
            .ok()
        };
        tx.commit()?;
        self.bump_dirty();
        Ok(label)
    }

    /// The whole history DAG plus HEAD — what the overlay draws.
    pub fn history(&self) -> rusqlite::Result<(Vec<ActionNode>, i64)> {
        let head = self.head(&self.conn)?;
        let mut stmt = self
            .conn
            .prepare("SELECT id, parent, ts, kind, label, state FROM action ORDER BY id")?;
        let nodes = stmt
            .query_map([], |r| {
                Ok(ActionNode {
                    id: r.get(0)?,
                    parent: r.get(1)?,
                    ts: r.get(2)?,
                    kind: r.get(3)?,
                    label: r.get(4)?,
                    state: r.get(5)?,
                })
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok((nodes, head))
    }
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

    fn snap_open(kinds: &[Kind]) -> core::WmSnap {
        let mut wm = Wm::new();
        for k in kinds {
            wm.open(k.clone(), None, false);
        }
        wm.snapshot()
    }

    /// The undo DAG end to end: act records changesets, undo inverts them
    /// (back to a never-booted store at the root), redo re-applies, and
    /// acting mid-tree branches — redo then follows the newest branch.
    #[test]
    fn actions_undo_redo_and_branch() {
        let s = store();
        let one = snap_open(&[Kind::Help]);
        let two = snap_open(&[Kind::Help, Kind::About]);
        s.act("open", "open help", None, 1.0, |c| save_wm_tx(c, &one))
            .unwrap();
        s.act("open", "open about", None, 2.0, |c| save_wm_tx(c, &two))
            .unwrap();
        assert_eq!(s.load_wm().unwrap(), Some(two.clone()));

        assert_eq!(s.undo().unwrap().as_deref(), Some("open about"));
        assert_eq!(s.load_wm().unwrap(), Some(one.clone()));
        assert_eq!(s.undo().unwrap().as_deref(), Some("open help"));
        assert_eq!(s.load_wm().unwrap(), None, "root = never booted");
        assert_eq!(s.undo().unwrap(), None, "nothing left");

        assert_eq!(s.redo().unwrap().as_deref(), Some("open help"));
        assert_eq!(s.load_wm().unwrap(), Some(one.clone()));

        // A new action mid-tree branches; redo now prefers the new branch.
        let fork = snap_open(&[Kind::Help, Kind::Inbox { filter: None }]);
        s.act("open", "open inbox", None, 3.0, |c| save_wm_tx(c, &fork))
            .unwrap();
        s.undo().unwrap();
        assert_eq!(s.redo().unwrap().as_deref(), Some("open inbox"));
        assert_eq!(s.load_wm().unwrap(), Some(fork));

        let (nodes, head) = s.history().unwrap();
        assert_eq!(nodes.len(), 3);
        assert_eq!(head, 3);
        assert_eq!(nodes[1].parent, 1, "about and inbox share a parent");
        assert_eq!(nodes[2].parent, 1);
        assert_eq!(nodes[1].state, "undone", "the abandoned branch stays");
    }

    /// Same kind + entity within the window amends the head node instead of
    /// growing the tree; one undo reverts the whole burst.
    #[test]
    fn rapid_actions_coalesce() {
        let s = store();
        let a = snap_open(&[Kind::Help]);
        let b = snap_open(&[Kind::Help, Kind::About]);
        let c_ = snap_open(&[Kind::Help, Kind::About, Kind::Inbox { filter: None }]);
        s.act("move", "move", Some("panel:1"), 10.0, |c| save_wm_tx(c, &a))
            .unwrap();
        s.act("move", "move", Some("panel:1"), 11.0, |c| save_wm_tx(c, &b))
            .unwrap();
        assert_eq!(s.history().unwrap().0.len(), 1, "burst = one node");
        s.act("move", "move", Some("panel:1"), 99.0, |c| save_wm_tx(c, &c_))
            .unwrap();
        assert_eq!(s.history().unwrap().0.len(), 2, "window passed = new node");
        s.undo().unwrap();
        assert_eq!(s.load_wm().unwrap(), Some(b), "back to the burst's end");
        s.undo().unwrap();
        assert_eq!(s.load_wm().unwrap(), None, "one undo for the burst");
    }

    /// Rows the world changed since (ingest) are skipped on undo, not
    /// forced — the rest of the action still reverts.
    #[test]
    fn undo_skips_rows_changed_since() {
        let s = store();
        let mut wm = Wm::new();
        wm.open(Kind::Help, None, false);
        wm.focus = Some(1);
        let snap = wm.snapshot();
        s.act("open", "open help", None, 1.0, |c| save_wm_tx(c, &snap))
            .unwrap();
        // The world moves on outside the action log (ingest-style write).
        s.write(|c| {
            c.execute("UPDATE workspace SET focus=777 WHERE k=0", [])
                .map(|_| ())
        })
        .unwrap();
        s.undo().unwrap();
        let n: i64 = s
            .conn()
            .query_row("SELECT COUNT(*) FROM panel", [], |r| r.get(0))
            .unwrap();
        assert_eq!(n, 0, "the panel insert reverted");
        let focus: i64 = s
            .conn()
            .query_row("SELECT focus FROM workspace WHERE k=0", [], |r| r.get(0))
            .unwrap();
        assert_eq!(focus, 777, "the conflicting row was left alone");
    }

    /// Travel jumps anywhere in the DAG: across branches (undo to the LCA,
    /// redo down the other side) and to the origin.
    #[test]
    fn travel_walks_branches() {
        let s = store();
        let one = snap_open(&[Kind::Help]);
        let two = snap_open(&[Kind::Help, Kind::About]);
        s.act("open", "open help", None, 1.0, |c| save_wm_tx(c, &one))
            .unwrap();
        s.act("open", "open about", None, 2.0, |c| save_wm_tx(c, &two))
            .unwrap();
        // Branch: back to node 1, then a different second action.
        s.undo().unwrap();
        let fork = snap_open(&[Kind::Help, Kind::Inbox { filter: None }]);
        s.act("open", "open inbox", None, 3.0, |c| save_wm_tx(c, &fork))
            .unwrap();
        assert_eq!(s.load_wm().unwrap(), Some(fork.clone()));

        // Jump across the fork to the abandoned branch's tip.
        assert_eq!(s.travel(2).unwrap().as_deref(), Some("open about"));
        assert_eq!(s.load_wm().unwrap(), Some(two));
        // Jump to the origin.
        assert_eq!(s.travel(0).unwrap().as_deref(), Some("the beginning"));
        assert_eq!(s.load_wm().unwrap(), None);
        // And back out to the fork's tip again.
        assert_eq!(s.travel(3).unwrap().as_deref(), Some("open inbox"));
        assert_eq!(s.load_wm().unwrap(), Some(fork));
        // No-op travel to where we already stand.
        assert_eq!(s.travel(3).unwrap(), None);
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
