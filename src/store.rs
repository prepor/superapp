//! The one store: a single SQLite file holding **all** durable data — mail
//! and UI state alike — plus the small reactive layer that derives panels
//! from it (CR-001).
//!
//! Shape (rel.systems' idioms, in-process):
//! - **one writable connection**, private to a dedicated writer thread (the
//!   [`Db`] gate, CR-005 phase 0); every mutation is a `Send` closure
//!   submitted to it and awaited, one transaction each. Every other
//!   connection — the UI's, each worker's — is a `query_only` reader, so a
//!   stray write fails loudly instead of racing. WAL, `synchronous=NORMAL`;
//! - inside the gate, a **session** over the durable tables records what each
//!   transaction wrote into `repl_log` — the changeset a peer device applies
//!   (CR-005). Applying a peer's frame records nothing, so it never echoes;
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
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc;
use std::sync::{Arc, Mutex};

use rusqlite::hooks::{AuthAction, AuthContext, Authorization};
use rusqlite::session::{ConflictAction, Session};
use rusqlite::{Connection, OpenFlags, Transaction};

use crate::core::{self, Kind, PanelId, Seed};

/// The durable tables a write's session records — everything a peer device
/// must be told about (CR-005). `repl_log` and `repl` are replication's own
/// bookkeeping and are deliberately absent, so a frame a follower *applies*
/// is never recaptured and never echoes back into the log.
pub(crate) const REPLICATED: &[&str] = &[
    "meta", "account", "folder", "message", "workspace", "ws_col", "panel", "wm",
    "server_msg", "draft", "outbox", "effect",
];

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
    /// An integer (ids).
    I(i64),
    /// Text.
    S(String),
    /// A real (the store's timestamps, a `@date>` bound).
    F(f64),
}

impl rusqlite::ToSql for Val {
    fn to_sql(&self) -> rusqlite::Result<rusqlite::types::ToSqlOutput<'_>> {
        Ok(match self {
            Val::I(i) => rusqlite::types::ToSqlOutput::from(*i),
            Val::S(s) => rusqlite::types::ToSqlOutput::from(s.as_str()),
            Val::F(f) => rusqlite::types::ToSqlOutput::from(*f),
        })
    }
}

struct Cached {
    deps: Rc<Vec<String>>,
    gens: Vec<u64>,
    rows: Rc<dyn Any>,
}

/// The store: a read-only view over the one database, plus the reactive
/// query layer, plus a handle to the [`Db`] gate every write goes through.
///
/// The UI thread owns one; each worker thread builds its own over the *same*
/// [`Db`] (CR-005 phase 0). Reads run on this connection; writes are closures
/// submitted to the single writer. `conn` is `query_only`, so a stray write
/// here fails loudly instead of racing.
pub struct Store {
    /// The one writer, shared. Every mutation goes through it.
    db: Arc<Db>,
    /// This thread's read-only connection.
    conn: Connection,
    /// Per-table commit generation — the invalidation clock.
    generations: RefCell<HashMap<String, u64>>,
    /// Authorizer-captured read-set per SQL text. Keyed by the text itself:
    /// a rich table's queries are *built* from its filter (see
    /// [`crate::richtable`]), so a query is not always a `static`.
    deps: RefCell<HashMap<String, Rc<Vec<String>>>>,
    /// Result cache per `(sql, params)`.
    cache: RefCell<HashMap<(String, String), Cached>>,
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
    /// The SQL as it ran — for a built query, the text the builder
    /// produced, filter and all.
    pub sql: String,
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

/// Schema v9 (CR-007): threads. `message.thread` is the id of the lowest
/// member of the conversation a mail belongs to — an anchor, not a root;
/// no row is the parent of another, and what a thread *has* (participants,
/// last date, unread) is a `GROUP BY` at read time. `topic` is the subject
/// with its reply prefixes stripped. `reference` holds one row per id in
/// `References` ∪ `In-Reply-To`, indexed by the id, so the three lookups
/// threading is made of are index walks. The back-fill re-parses `raw` for
/// every mail that has one, oldest first, exactly as v6 did for the HTML
/// reading; mail without raw (the seed) anchors itself.
const SCHEMA_V9: &str = "
ALTER TABLE message ADD COLUMN thread INTEGER;
ALTER TABLE message ADD COLUMN topic TEXT;
CREATE TABLE reference(
  message INTEGER NOT NULL,
  mid     TEXT NOT NULL
);
CREATE INDEX idx_reference_mid ON reference(mid);
CREATE INDEX idx_reference_message ON reference(message);
CREATE INDEX idx_message_thread ON message(thread);
CREATE INDEX idx_message_mid ON message(account, message_id);
";

/// Threads every mail already in the store (schema v9), oldest id first so
/// a reply always finds what it answers already anchored. Runs once, inside
/// the migration.
pub(crate) fn backfill_threads(conn: &Connection) -> rusqlite::Result<()> {
    struct Row {
        id: i64,
        account: i64,
        subject: String,
        mid: Option<String>,
        raw: Option<Vec<u8>>,
    }
    let rows: Vec<Row> = conn
        .prepare("SELECT id, account, subject, message_id, raw FROM message ORDER BY id")?
        .query_map([], |r| {
            Ok(Row {
                id: r.get(0)?,
                account: r.get(1)?,
                subject: r.get(2)?,
                mid: r.get(3)?,
                raw: r.get(4)?,
            })
        })?
        .collect::<rusqlite::Result<_>>()?;
    for Row { id, account, subject, mid, raw } in rows {
        let (mid, refs, topic) = match raw {
            Some(raw) => {
                let p = crate::sync::parse_mail(&raw);
                (p.message_id, p.references, p.topic)
            }
            None => (mid.unwrap_or_default(), Vec::new(), crate::mail::topic_of(&subject)),
        };
        conn.execute(
            "UPDATE message SET topic = ?2, message_id = ?3 WHERE id = ?1",
            rusqlite::params![id, topic, mid],
        )?;
        crate::mail::thread_tx(conn, account, id, &mid, &refs)?;
    }
    Ok(())
}

/// Schema v10 (CR-005): the replication log and its local state. `repl_log`
/// is a **queue that drains and prunes**, not a durable changeset table — the
/// session extension CR-004 removed comes back for exactly this, and nothing
/// migrates through it. `repl` is local-only, never replicated: it holds this
/// install's stable device id and its sequence counters.
///
/// `repl_log.seq` is fed from `repl.next_local_seq`, **not** a bare rowid — a
/// snapshot install clears `repl_log` while `repl` survives, and SQLite would
/// otherwise reassign rowids from 1 and make a fresh row look long published.
const SCHEMA_V10: &str = "
CREATE TABLE repl_log(
  seq       INTEGER PRIMARY KEY,      -- local order, from repl.next_local_seq
  pub_seq   INTEGER,                  -- global seq at publish; NULL until then
  ts        REAL NOT NULL,
  changeset BLOB NOT NULL
);
CREATE INDEX idx_repl_log_pending ON repl_log(seq) WHERE pub_seq IS NULL;

CREATE TABLE repl(
  id      INTEGER PRIMARY KEY CHECK(id = 1),
  device  TEXT NOT NULL,              -- stable per install; survives snapshots
  epoch   INTEGER NOT NULL DEFAULT 0,
  next_local_seq   INTEGER NOT NULL DEFAULT 1,  -- monotone for the device's life
  materialized_seq INTEGER NOT NULL DEFAULT 0,  -- global seq contained through
  holding INTEGER NOT NULL DEFAULT 0
);
";
/// Rewrites `message.html` from the `raw` blob each synced mail keeps. The
/// narrowing ([`crate::html::sanitize`]) runs at ingest, so a stored reading
/// is only as good as the build that stored it, and a better narrowing has
/// to be run over the rows already there: schema v6 first filled the column,
/// and since then [`crate::html::VERSION`], kept in `meta`, says which
/// narrowing the store holds (see [`Store::open`]). Messages without `raw` — the
/// demo seed — are left alone, and a mail whose sender wrote text only
/// stays NULL. Runs inside the migration.
pub(crate) fn backfill_html(conn: &Connection) -> rusqlite::Result<()> {
    let rows: Vec<(i64, Vec<u8>)> = conn
        .prepare("SELECT id, raw FROM message WHERE raw IS NOT NULL")?
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?
        .collect::<rusqlite::Result<_>>()?;
    for (id, raw) in rows {
        conn.execute(
            "UPDATE message SET html = ?2 WHERE id = ?1",
            rusqlite::params![id, crate::sync::parse_mail(&raw).html],
        )?;
    }
    Ok(())
}

// -- the one writer -----------------------------------------------------------

/// A type-erased write result travelling back from the writer thread.
type Erased = Box<dyn Any + Send>;

/// A caller's write closure, boxed and type-erased, on its way to the writer
/// thread. The `Send` bound is what CR-005 phase 0 costs the call sites.
type RunFn = Box<dyn FnOnce(&Transaction) -> rusqlite::Result<Erased> + Send>;

/// The same, for a replication-internal op on the raw connection.
type RawFn = Box<dyn FnOnce(&Connection) -> rusqlite::Result<Erased> + Send>;

/// A reply channel's payload: the closure's value plus the tables it touched.
type WriteOut = rusqlite::Result<(Erased, HashSet<String>)>;

/// One unit of work for the writer thread.
enum Job {
    /// A captured write: run inside one `IMMEDIATE` transaction with a
    /// session open over [`REPLICATED`], harvest the changeset into
    /// `repl_log` in the *same* transaction, commit.
    Write {
        run: RunFn,
        reply: mpsc::Sender<WriteOut>,
    },
    /// A peer frame: apply a changeset with **no session and no `repl_log`
    /// row**, so applying records nothing and never echoes back.
    Apply {
        changeset: Vec<u8>,
        reply: mpsc::Sender<rusqlite::Result<()>>,
    },
    /// A replication-internal operation on the raw connection: no session, no
    /// `writable` check. This is how the sync engine touches `repl`, applies
    /// batches, and installs snapshots — bookkeeping that must run even on a
    /// follower whose ordinary writes are closed.
    Raw {
        run: RawFn,
        reply: mpsc::Sender<rusqlite::Result<Erased>>,
    },
}

/// How a reader re-opens the same database.
#[derive(Clone)]
enum Target {
    /// A file on disk: readers open it `READ_ONLY` (WAL — readers never block).
    File(PathBuf),
    /// A shared-cache in-memory database (tests). Every connection names the
    /// same URI so a reader sees the writer's commits; the `READ_ONLY` open
    /// flag does **not** bind a shared-cache memory database, so readers lean
    /// on `query_only` instead.
    Memory(String),
}

/// **The one writable connection** — private, single, and living on its own
/// thread. Every mutation in the process is a closure submitted here and
/// awaited; every other connection is a reader. This is the capture seam
/// CR-005 phase 0 exists to make un-bypassable: a write nobody captured is
/// silent divergence, not a crash, so there is exactly one door and `Db`
/// owns it.
pub struct Db {
    jobs: mpsc::Sender<Job>,
    target: Target,
    /// Whether ordinary [`Db::write`] mutations are allowed. A follower holds
    /// this `false` (CR-005): its ordinary writes fail read-only at the gate,
    /// while the replication [`Db::raw`] and [`Db::apply`] paths still run.
    writable: Arc<AtomicBool>,
}

impl Db {
    /// Opens (and migrates) the database, then hands its one writable
    /// connection to a dedicated writer thread. `None` is a private
    /// in-memory database — shared-cache, so a reader on another connection
    /// still sees it (what a test's `World::fake` uses).
    pub fn open(path: Option<&Path>) -> rusqlite::Result<Arc<Db>> {
        let target = match path {
            Some(p) => Target::File(p.to_path_buf()),
            None => {
                static N: AtomicU64 = AtomicU64::new(0);
                let n = N.fetch_add(1, Ordering::Relaxed);
                Target::Memory(format!(
                    "file:superapp-mem-{}-{n}?mode=memory&cache=shared",
                    std::process::id()
                ))
            }
        };
        let conn = open_writer(&target)?;
        migrate(&conn)?;
        sweep_effects(&conn)?;

        let dirty: Arc<Mutex<HashSet<String>>> = Arc::default();
        let d = dirty.clone();
        conn.update_hook(Some(move |_op, _db: &str, table: &str, _rowid: i64| {
            d.lock().expect("dirty set").insert(table.to_string());
        }))?;
        let (jobs, rx) = mpsc::channel::<Job>();
        std::thread::Builder::new()
            .name("store-writer".into())
            .spawn(move || writer_loop(&conn, &dirty, &rx))
            .expect("spawn the store writer");
        Ok(Arc::new(Db {
            jobs,
            target,
            writable: Arc::new(AtomicBool::new(true)),
        }))
    }

    /// Opens the gate to ordinary writes (a holder) or closes it (a follower,
    /// or a stranded device). Replication's own [`Db::raw`]/[`Db::apply`]
    /// paths ignore this — only [`Db::write`] is gated.
    pub fn set_writable(&self, writable: bool) {
        self.writable.store(writable, Ordering::Release);
    }

    /// Whether ordinary writes are currently allowed.
    #[must_use]
    pub fn is_writable(&self) -> bool {
        self.writable.load(Ordering::Acquire)
    }

    /// A fresh read-only connection to the same database.
    fn reader(&self) -> rusqlite::Result<Connection> {
        open_reader(&self.target)
    }

    /// Submits a write and blocks for its value plus the tables it touched
    /// (the invalidation the caller's [`Store`] consumes). A panic inside
    /// `f` is caught and returned as an error — one bad closure must not
    /// kill the only writer.
    fn write<T: Send + 'static>(
        &self,
        f: impl FnOnce(&Transaction) -> rusqlite::Result<T> + Send + 'static,
    ) -> rusqlite::Result<(T, HashSet<String>)> {
        if !self.writable.load(Ordering::Acquire) {
            return Err(store_err(
                "the store is read-only: another device holds the lease",
            ));
        }
        let (reply, rx) = mpsc::channel();
        let run: RunFn = Box::new(move |tx| f(tx).map(|v| Box::new(v) as Erased));
        self.jobs.send(Job::Write { run, reply }).map_err(|_| gone())?;
        let (erased, dirty) = rx.recv().map_err(|_| gone())??;
        let v = *erased.downcast::<T>().expect("write result type");
        Ok((v, dirty))
    }

    /// A replication-internal operation on the raw connection — no session, no
    /// `writable` gate. The closure owns its own transactions. This is how the
    /// sync engine reads and advances `repl`, applies batches, and installs
    /// snapshots, work that must run even while ordinary writes are closed.
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
            .send(Job::Apply { changeset: changeset.to_vec(), reply })
            .map_err(|_| gone())?;
        rx.recv().map_err(|_| gone())?
    }
}

/// A store error carrying a plain message — for the failures that are ours,
/// not SQLite's (a dead writer, a panicked closure).
fn store_err(msg: &str) -> rusqlite::Error {
    rusqlite::Error::SqliteFailure(
        rusqlite::ffi::Error::new(rusqlite::ffi::SQLITE_ERROR),
        Some(msg.to_string()),
    )
}

/// The error a store call answers with when its writer thread is gone — only
/// reachable if the last [`Db`] handle was dropped mid-call, which cannot
/// happen while a [`Store`] holds one.
fn gone() -> rusqlite::Error {
    store_err("the store's writer thread is gone")
}

/// Opens the one writable connection and applies the write-side pragmas.
fn open_writer(target: &Target) -> rusqlite::Result<Connection> {
    let conn = match target {
        Target::File(p) => Connection::open(p)?,
        Target::Memory(uri) => Connection::open_with_flags(
            uri,
            OpenFlags::SQLITE_OPEN_READ_WRITE
                | OpenFlags::SQLITE_OPEN_CREATE
                | OpenFlags::SQLITE_OPEN_URI,
        )?,
    };
    // `journal_mode` answers with a row, which pragma_update may reject.
    conn.pragma_update(None, "journal_mode", "WAL")
        .or_else(|_| conn.query_row("PRAGMA journal_mode=WAL", [], |_| Ok(())))?;
    conn.pragma_update(None, "synchronous", "NORMAL")?;
    conn.pragma_update(None, "foreign_keys", "ON")?;
    conn.busy_timeout(std::time::Duration::from_millis(5000))?;
    Ok(conn)
}

/// Opens a read-only connection. On a file this is the `READ_ONLY` open flag
/// (WAL readers never block); on shared-cache memory the flag does not bind,
/// so `query_only` is what makes a stray write fail with `SQLITE_READONLY`.
fn open_reader(target: &Target) -> rusqlite::Result<Connection> {
    let conn = match target {
        Target::File(p) => {
            let c = Connection::open_with_flags(p, OpenFlags::SQLITE_OPEN_READ_ONLY)?;
            c.busy_timeout(std::time::Duration::from_millis(5000))?;
            c
        }
        Target::Memory(uri) => {
            let c = Connection::open_with_flags(
                uri,
                OpenFlags::SQLITE_OPEN_READ_WRITE | OpenFlags::SQLITE_OPEN_URI,
            )?;
            c.pragma_update(None, "query_only", true)?;
            c
        }
    };
    Ok(conn)
}

/// The migration ladder, run once on the writable connection at open.
fn migrate(conn: &Connection) -> rusqlite::Result<()> {
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
        backfill_html(conn)?;
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
    if version < 9 {
        conn.execute_batch(SCHEMA_V9)?;
        backfill_threads(conn)?;
        conn.pragma_update(None, "user_version", 9)?;
    }
    if version < 10 {
        conn.execute_batch(SCHEMA_V10)?;
        conn.execute("INSERT INTO repl(id, device) VALUES(1, ?1)", [device_id()])?;
        conn.pragma_update(None, "user_version", 10)?;
    }
    // The HTML narrowing runs at ingest, so a stored reading is as good as
    // the build that wrote it. The version of the narrowing the store holds
    // lives in `meta`; when the build's differs, every reading is redone
    // from raw — schema versions are for the schema.
    let narrowed: i64 = conn
        .query_row("SELECT value FROM meta WHERE key = 'html_version'", [], |r| r.get(0))
        .unwrap_or(0);
    if narrowed != i64::from(crate::html::VERSION) {
        backfill_html(conn)?;
        conn.execute(
            "INSERT OR REPLACE INTO meta(key, value) VALUES ('html_version', ?1)",
            [i64::from(crate::html::VERSION)],
        )?;
    }
    Ok(())
}

/// A stable per-install device id (CR-005): two devices must never share one,
/// or they publish under the same name and corrupt `state.acked`. No `rand`
/// dependency — a mix of the wall clock, the pid and a process-local counter
/// is unique enough for two devices.
fn device_id() -> String {
    use std::time::{SystemTime, UNIX_EPOCH};
    static N: AtomicU64 = AtomicU64::new(0);
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos() as u64)
        .unwrap_or(0);
    let salt = (u64::from(std::process::id()) << 32) ^ N.fetch_add(1, Ordering::Relaxed);
    format!("{nanos:016x}{salt:016x}")
}

/// Seconds since the epoch, for a log row's `ts` — a record timestamp, not
/// the app's logical clock (the writer thread has no `World`).
fn now_secs() -> f64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs_f64())
        .unwrap_or(0.0)
}

/// The writer thread: own the one writable connection, serve jobs until the
/// last [`Db`] handle drops, then close the connection.
fn writer_loop(conn: &Connection, dirty: &Arc<Mutex<HashSet<String>>>, rx: &mpsc::Receiver<Job>) {
    while let Ok(job) = rx.recv() {
        match job {
            Job::Write { run, reply } => {
                let _ = reply.send(do_write(conn, dirty, run));
            }
            Job::Apply { changeset, reply } => {
                let _ = reply.send(do_apply(conn, &changeset));
            }
            Job::Raw { run, reply } => {
                dirty.lock().expect("dirty set").clear();
                let ran = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| run(conn)));
                let _ = reply.send(match ran {
                    Ok(r) => r,
                    Err(_) => Err(store_err("a raw closure panicked")),
                });
            }
        }
    }
}

/// One captured write, on the writer thread: begin, open a session over the
/// replicated tables, run the closure, harvest its changeset into `repl_log`
/// in the same transaction, commit. A panic or error rolls the whole thing
/// back and captures nothing.
fn do_write(
    conn: &Connection,
    dirty: &Arc<Mutex<HashSet<String>>>,
    run: RunFn,
) -> WriteOut {
    dirty.lock().expect("dirty set").clear();
    let tx = Transaction::new_unchecked(conn, rusqlite::TransactionBehavior::Immediate)?;
    let mut sess = Session::new(conn)?;
    for t in REPLICATED {
        sess.attach(Some(*t))?;
    }
    let ran = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| run(&tx)));
    let value = match ran {
        Ok(Ok(v)) => v,
        Ok(Err(e)) => {
            drop(sess);
            drop(tx); // rollback: those writes never happened
            dirty.lock().expect("dirty set").clear();
            return Err(e);
        }
        Err(_) => {
            drop(sess);
            drop(tx);
            dirty.lock().expect("dirty set").clear();
            return Err(store_err("a write closure panicked"));
        }
    };
    // The tables the closure touched — snapshotted before the `repl_log`
    // insert adds its own, which no query depends on.
    let touched: HashSet<String> = dirty.lock().expect("dirty set").clone();
    let mut cs: Vec<u8> = Vec::new();
    sess.changeset_strm(&mut cs)?;
    drop(sess);
    if !cs.is_empty() {
        let seq: i64 = tx.query_row("SELECT next_local_seq FROM repl", [], |r| r.get(0))?;
        tx.execute(
            "INSERT INTO repl_log(seq, pub_seq, ts, changeset) VALUES(?1, NULL, ?2, ?3)",
            rusqlite::params![seq, now_secs(), cs],
        )?;
        tx.execute("UPDATE repl SET next_local_seq = next_local_seq + 1", [])?;
    }
    tx.commit()?;
    Ok((value, touched))
}

/// One peer frame, on the writer thread: apply the changeset atomically with
/// no session (records nothing) and `ABORT` on conflict.
fn do_apply(conn: &Connection, changeset: &[u8]) -> rusqlite::Result<()> {
    conn.apply_strm(
        &mut &changeset[..],
        None::<fn(&str) -> bool>,
        |_conflict, _item| ConflictAction::SQLITE_CHANGESET_ABORT,
    )
}

impl Store {
    /// Opens the store over a fresh [`Db`] — the UI thread's constructor, and
    /// what a test's in-memory world uses. `None` is a private in-memory
    /// database.
    pub fn open(path: Option<&Path>) -> rusqlite::Result<Store> {
        let db = Db::open(path)?;
        Store::with_db(db)
    }

    /// A reader over an existing [`Db`] — how a worker thread joins the one
    /// writer instead of opening a second (CR-005 phase 0).
    pub fn with_db(db: Arc<Db>) -> rusqlite::Result<Store> {
        let conn = db.reader()?;
        Ok(Store {
            db,
            conn,
            generations: RefCell::default(),
            deps: RefCell::default(),
            cache: RefCell::default(),
            redraw: Cell::new(false),
            data_version: Cell::new(-1),
            traces: RefCell::default(),
            active_trace: Cell::new(None),
        })
    }

    /// The one writer, for building another reader on the same database (a
    /// worker's [`Store::with_db`]) — never a second writable connection.
    #[must_use]
    pub fn db(&self) -> Arc<Db> {
        self.db.clone()
    }

    /// Runs one mutation as one transaction through the single writer; on
    /// commit, every touched table's generation bumps and dependent cached
    /// queries go stale. The closure runs on the writer thread, so it must
    /// own what it touches — `Send + 'static`, no borrowing UI state.
    pub fn write<T: Send + 'static>(
        &self,
        f: impl FnOnce(&Transaction) -> rusqlite::Result<T> + Send + 'static,
    ) -> rusqlite::Result<T> {
        let (out, dirty) = self.db.write(f)?;
        self.bump(&dirty);
        Ok(out)
    }

    /// Bumps the generation of every table a commit touched, so cached
    /// queries that read them go stale, then refreshes this reader's
    /// `data_version` baseline: the write landed on the *writer's*
    /// connection, foreign to this one, and we have already accounted for it
    /// — `poll_external` must not re-run every query again for the same
    /// commit.
    fn bump(&self, dirty: &HashSet<String>) {
        if !dirty.is_empty() {
            let mut gens = self.generations.borrow_mut();
            for t in dirty {
                *gens.entry(t.clone()).or_insert(0) += 1;
            }
            self.redraw.set(true);
        }
        let v: i64 = self
            .conn
            .query_row("PRAGMA data_version", [], |r| r.get(0))
            .unwrap_or(0);
        self.data_version.set(v);
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
    fn deps_for(&self, id: &str, sql: &str) -> Rc<Vec<String>> {
        if let Some(d) = self.deps.borrow().get(sql) {
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
        let prepared = self.conn.prepare_cached(sql);
        let _ = self
            .conn
            .authorizer(None::<fn(AuthContext<'_>) -> Authorization>);
        if let Err(e) = prepared {
            eprintln!("store: preparing {id} failed: {e}");
        }
        let tables: Vec<String> = seen.lock().expect("dep set").iter().cloned().collect();
        let rc = Rc::new(tables);
        self.deps.borrow_mut().insert(sql.to_string(), rc.clone());
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
        self.rows_sql(q.id, q.describe, q.sql, params, map)
    }

    /// [`Store::rows`] for a query whose text is built at run time — a rich
    /// table's page, count and rank queries, whose `WHERE` is the operator's
    /// filter. Same cache, same dependency capture, same trace: the built
    /// text is the cache key, so two panels on the same filter share one
    /// result, and the context an agent receives shows the SQL that
    /// actually ran.
    pub fn rows_sql<T: 'static>(
        &self,
        id: &'static str,
        describe: &'static str,
        sql: &str,
        params: &[Val],
        map: fn(&rusqlite::Row) -> rusqlite::Result<T>,
    ) -> Rc<Vec<T>> {
        let pkey = fmt_params(params);
        let key = (sql.to_string(), pkey.clone());
        let deps = self.deps_for(id, sql);
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
                let mut stmt = self.conn.prepare_cached(sql)?;
                let iter = stmt.query_map(rusqlite::params_from_iter(params.iter()), map)?;
                iter.collect()
            };
            let rows = Rc::new(run().unwrap_or_else(|e| {
                eprintln!("store: query {id} failed: {e}");
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
                if !v.iter().any(|e| e.id == id && e.sql == sql && e.params == pkey) {
                    v.push(TraceEntry {
                        id,
                        sql: sql.to_string(),
                        describe,
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
        let snap = snap.clone();
        self.write(move |c| save_wm_tx(c, &snap))
    }

    // -- replication (CR-005) ------------------------------------------------

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
    /// If the changeset conflicts (a broken invariant under a single writer).
    pub fn apply_frame(&self, changeset: &[u8]) -> rusqlite::Result<()> {
        self.db.apply(changeset)?;
        self.poll_external();
        Ok(())
    }

    /// Marks every unpublished frame through `seq` as published, so a second
    /// drain moves nothing. `pub_seq` gets a global sequence in CR-005 phase
    /// 2; locally it is the local seq, which is enough to stop a re-drain.
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
    /// holder is accruing, surfaced in the UI (CR-005 phase 3).
    #[must_use]
    pub fn unpublished(&self) -> i64 {
        self.conn
            .query_row(
                "SELECT COUNT(*) FROM repl_log WHERE pub_seq IS NULL",
                [],
                |r| r.get(0),
            )
            .unwrap_or(0)
    }

    /// This install's stable device id (CR-005).
    #[must_use]
    pub fn device(&self) -> String {
        self.conn
            .query_row("SELECT device FROM repl WHERE id=1", [], |r| r.get(0))
            .unwrap_or_default()
    }

    /// The global sequence this store *contains* through — whatever the
    /// origin, including its own published writes (CR-005).
    #[must_use]
    pub fn materialized(&self) -> i64 {
        self.conn
            .query_row("SELECT materialized_seq FROM repl WHERE id=1", [], |r| r.get(0))
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

    /// Opens (holder) or closes (follower/stranded) the write gate.
    pub fn set_writable(&self, writable: bool) {
        self.db.set_writable(writable);
    }

    /// Whether ordinary writes are currently allowed.
    #[must_use]
    pub fn is_writable(&self) -> bool {
        self.db.is_writable()
    }

    /// Records the epoch, holding flag and high-water — replication's own
    /// local state, through the raw path so it runs even on a follower and is
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

    /// Advances the high-water mark this store contains through.
    ///
    /// # Errors
    ///
    /// If the update fails.
    pub fn set_materialized(&self, seq: i64) -> rusqlite::Result<()> {
        self.db.raw(move |c| {
            c.execute(
                "UPDATE repl SET materialized_seq = ?1 WHERE id = 1",
                [seq],
            )
            .map(|_| ())
        })
    }

    /// `VACUUM INTO` a fresh file — a snapshot of the whole logical database,
    /// taken at a drained boundary (CR-005 bootstrap). Replication's own
    /// bookkeeping rides along and is dropped on install.
    ///
    /// # Errors
    ///
    /// If the vacuum fails.
    pub fn vacuum_into(&self, path: &Path) -> rusqlite::Result<()> {
        let path = path.to_string_lossy().to_string();
        self.db.raw(move |c| c.execute("VACUUM INTO ?1", [path]).map(|_| ()))
    }

    /// The genesis snapshot: `VACUUM INTO` a fresh file **and**, in the same
    /// writer-thread turn, bury every frame captured so far (they are already
    /// in the snapshot, so they must never also ship as a batch) and set the
    /// high-water to 0. Because the writer serves one job at a time, no write
    /// interleaves — a mutation is either before this turn (in the snapshot,
    /// buried) or after it (a future batch), never both. That is the "drained
    /// boundary" the snapshot needs.
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
    /// high-water and epoch. `repl.device` is **preserved** — two devices
    /// must not share an id — and the snapshot's own `repl_log`/`repl` are
    /// not copied, so the sender's queue and identity do not come with it.
    ///
    /// Not a file swap: a live connection keeps using the database it has
    /// open, so the rows are copied in with `ATTACH` rather than the file
    /// replaced under it.
    ///
    /// # Errors
    ///
    /// If the attach, copy or commit fails.
    pub fn install_snapshot(&self, path: &Path, materialized: i64, epoch: i64) -> rusqlite::Result<()> {
        let path = path.to_string_lossy().to_string();
        self.db.raw(move |c| {
            c.execute("ATTACH DATABASE ?1 AS snap", [&path])?;
            let result = (|| -> rusqlite::Result<()> {
                c.execute_batch("PRAGMA defer_foreign_keys = ON")?;
                let tx = Transaction::new_unchecked(c, rusqlite::TransactionBehavior::Immediate)?;
                for t in REPLICATED {
                    tx.execute(&format!("DELETE FROM main.\"{t}\""), [])?;
                    tx.execute(
                        &format!("INSERT INTO main.\"{t}\" SELECT * FROM snap.\"{t}\""),
                        [],
                    )?;
                }
                // The local pending queue is relative to the *old* baseline —
                // meaningless against the snapshot we just installed. Clear it
                // (CR-005: "repl_log cleared") while `repl.next_local_seq`
                // survives, so a re-drain never resends a stale frame.
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
    /// `last_seq`, all in **one** transaction — so a crash mid-batch rolls the
    /// whole thing back and re-applies from the unchanged watermark, never
    /// half-lands. Conflicts `ABORT`. Records nothing (no session).
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
            tx.execute(
                "UPDATE repl SET materialized_seq = ?1 WHERE id = 1",
                [last_seq],
            )?;
            tx.commit()
        })?;
        self.poll_external();
        Ok(())
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
            Val::F(f) => f.to_string(),
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
        // A blank compose keeps the `0` it always had, so a session an
        // earlier build saved still reads.
        Kind::Compose { seed: Seed::Blank } => ("compose", Some(0), None),
        Kind::Compose {
            seed: Seed::Reply(id),
        } => ("compose", Some(*id), None),
        Kind::Compose {
            seed: Seed::Forward(id),
        } => ("forward", Some(*id), None),
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
        "compose" => Kind::Compose {
            seed: match p_int? {
                0 => Seed::Blank,
                id => Seed::Reply(id),
            },
        },
        "forward" => Kind::Compose {
            seed: Seed::Forward(p_int?),
        },
        "settings" => Kind::Settings,
        "add_account" => Kind::AddAccount,
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::{Kind, Wm};

    /// The narrowing's version is kept with the store: a fresh store holds
    /// the build's, so nothing is redone at its next open, and a store from
    /// another build is redone once.
    #[test]
    fn the_store_records_the_narrowing_it_holds() {
        let s = Store::open(None).unwrap();
        let held = || -> i64 {
            s.conn()
                .query_row("SELECT value FROM meta WHERE key = 'html_version'", [], |r| r.get(0))
                .unwrap()
        };
        assert_eq!(held(), i64::from(crate::html::VERSION));
    }

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
        assert_eq!(*s.deps_for(Q_META.id, Q_META.sql), vec!["meta".to_string()]);

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
        // The three seeds a compose has, each its own row.
        for seed in [Seed::Blank, Seed::Reply(3), Seed::Forward(3)] {
            wm.open(Kind::Compose { seed }, None, false);
        }
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

    /// The reader really is read-only: a write attempted on it fails at the
    /// SQLite level, not by convention. That is what makes [`Store::write`]
    /// the seam a peer's replication cannot be routed around (CR-005 phase 0).
    #[test]
    fn the_reader_refuses_writes() {
        let s = store();
        let e = s
            .conn()
            .execute("INSERT INTO meta(key, value) VALUES('x', 1)", []);
        assert!(e.is_err(), "the reader connection must reject writes");
        // And the gate accepts the same write.
        s.write(|tx| {
            tx.execute("INSERT INTO meta(key, value) VALUES('x', 1)", [])
                .map(|_| ())
        })
        .unwrap();
    }

    /// A write is captured as a frame; a rolled-back write captures nothing.
    #[test]
    fn writes_are_captured_as_frames() {
        let s = store();
        assert_eq!(s.unpublished(), 0, "a fresh store has an empty log");
        s.write(|tx| {
            tx.execute("INSERT INTO account(label, email) VALUES('a', 'a@a')", [])
                .map(|_| ())
        })
        .unwrap();
        assert_eq!(s.unpublished(), 1, "the insert was captured");

        // A rolled-back write captures nothing.
        let _ = s.write(|tx| -> rusqlite::Result<()> {
            tx.execute("INSERT INTO account(label, email) VALUES('b', 'b@b')", [])?;
            Err(rusqlite::Error::QueryReturnedNoRows)
        });
        assert_eq!(s.unpublished(), 1, "a rolled-back write leaves no frame");
    }

    /// The enforcement ceiling, written down as a test: `Connection::open`
    /// lives only in this module. Every other module reads through a
    /// `Store` reader or writes through the gate, so no code can quietly
    /// open a second writable handle and bypass capture (CR-005 phase 0).
    #[test]
    fn connection_open_is_confined_to_this_module() {
        let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
        let mut offenders = Vec::new();
        for entry in std::fs::read_dir(&dir).expect("read src") {
            let path = entry.expect("dir entry").path();
            if path.extension().and_then(|e| e.to_str()) != Some("rs") {
                continue;
            }
            if path.file_name().and_then(|n| n.to_str()) == Some("store.rs") {
                continue; // the one place the writer lives
            }
            let src = std::fs::read_to_string(&path).expect("read source");
            for (n, line) in src.lines().enumerate() {
                // Ignore the comment tail so prose mentioning it is fine.
                let code = line.split("//").next().unwrap_or("");
                if code.contains("Connection::open") {
                    offenders.push(format!("{}:{}", path.display(), n + 1));
                }
            }
        }
        assert!(
            offenders.is_empty(),
            "Connection::open must stay in store.rs; found: {offenders:?}"
        );
    }
}

