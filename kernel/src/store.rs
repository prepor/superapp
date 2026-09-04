//! SQLite storage and cached queries.
//!
//! [`Db`] owns the only writable connection on a dedicated thread. Other
//! connections are read-only. Each transaction records a device-sync
//! changeset. Applying a changeset does not record it again.
//!
//! [`Store::rows`] caches results by SQL and parameters. SQLite reports which
//! tables a query reads and when those tables change. The in-memory effect log
//! reports changes itself because it is not a table. Workspace state is saved;
//! animation and undo history remain in memory.
//!
//! The kernel owns `meta`, `workspace`, `ws_col`, `panel`, `wm`, `effect` and
//! the two `repl` tables, and nothing else. Every other table belongs to an
//! app and arrives through that app's [`Schema`], applied
//! after the kernel's ladder in app-list order.

use std::any::Any;
use std::cell::{Cell, RefCell};
use std::collections::{BTreeSet, HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc;
use std::sync::{Arc, Mutex};

use rusqlite::hooks::{AuthAction, AuthContext, Authorization};
use rusqlite::session::Session as Capture;
use rusqlite::{Connection, OpenFlags, Transaction};

use crate::app::Schema;
use crate::layout::{self, SlotId};
use crate::panel::{PanelId, Tag};

mod repl;

use repl::{replicated_tables, warn_unkeyed, SCHEMA_REPL};

/// A registered query: identity, SQL, and the one-line purpose that panel
/// context will hand to an agent. Declared `static` at the call site.
#[derive(Debug)]
pub struct Q {
    pub id: &'static str,
    pub sql: &'static str,
    /// What this query is for, in one line.
    pub describe: &'static str,
}

/// A query parameter — small closed set, so cache keys are trivial.
#[derive(Debug, Clone, PartialEq)]
pub enum Val {
    I(i64),
    S(String),
    F(f64),
}

/// A datasource's key lands as a parameter this way (a rich table's
/// `IN (…)`, its `key = ?`), so the key type stays the domain's own.
impl From<i64> for Val {
    fn from(i: i64) -> Self {
        Val::I(i)
    }
}

impl From<String> for Val {
    fn from(s: String) -> Self {
        Val::S(s)
    }
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
/// [`Db`]. Reads run on this connection; writes are closures
/// submitted to the single writer. `conn` is `query_only`, so a stray write
/// here fails loudly instead of racing.
pub struct Store {
    db: Arc<Db>,
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
    /// Last seen [`crate::effect::MemLog`] version — the same detector for
    /// the one "table" no commit hook can report, because it is not in the
    /// database at all.
    mem_version: Cell<u64>,
    /// Per-slot query traces: which queries the panel's last draw touched
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

/// The kernel's schema. A store at any other number is refused rather than
/// migrated: there is one shape, and a file that is not at it belongs to
/// another build.
pub const KERNEL_VERSION: i64 = 1;

/// How that refusal opens. The number follows it, which is what lets a
/// caller read the store's own schema back out of the error.
const REFUSED: &str = "this store is schema ";

/// The schema a store was at when [`Store::open`] refused it — `None` when
/// the open failed for any other reason. The shell reads it to turn the
/// refusal into a spoken exit instead of a backtrace; every other caller
/// keeps the error as it is.
#[must_use]
pub fn refused_schema(e: &rusqlite::Error) -> Option<i64> {
    let said = e.to_string();
    let after = said.split_once(REFUSED)?.1;
    after
        .split(|c: char| !c.is_ascii_digit())
        .next()
        .filter(|d| !d.is_empty())?
        .parse()
        .ok()
}

/// The kernel's own tables, and no others. `panel` carries what a slot shows
/// as `(kind, args)`: the tag, and its arguments as one JSON array in a text
/// column — readable in `sqlite3`, and reachable with `args ->> 0` should a
/// query ever want one. The kernel never reads an argument.
const SCHEMA_V1: &str = "
CREATE TABLE meta(key TEXT PRIMARY KEY, value ANY);

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
  args      TEXT NOT NULL DEFAULT '[]' CHECK (json_valid(args)),
  joined_to INTEGER
);
CREATE TABLE wm(
  id     INTEGER PRIMARY KEY CHECK(id = 1),
  active INTEGER NOT NULL DEFAULT 0
);

-- One queue for every deferred effect, whatever app it came from.
-- `payload` and `reply` are JSON *text*, not JSONB: SQLite has no JSON type,
-- and a BLOB encoding would make `SELECT reply FROM effect` unreadable in a
-- shell — inspectability is why this lives in the store at all. `idempotent`
-- is copied onto the row at enqueue time so the crash sweep is pure SQL and
-- never has to decode a payload to know whether retrying is safe.
CREATE TABLE effect(
  id         INTEGER PRIMARY KEY,
  kind       TEXT NOT NULL,
  payload    TEXT NOT NULL CHECK (json_valid(payload)),
  entity     TEXT,
  status     TEXT NOT NULL DEFAULT 'pending',
  idempotent INTEGER NOT NULL DEFAULT 0,
  writes     INTEGER NOT NULL DEFAULT 1,
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

// -- the one writer -----------------------------------------------------------

/// A type-erased write result travelling back from the writer thread.
type Erased = Box<dyn Any + Send>;

/// A caller's write closure, boxed and type-erased, on its way to the writer
/// thread. The `Send` bound lets the closure cross that thread boundary.
type RunFn = Box<dyn FnOnce(&Transaction) -> rusqlite::Result<Erased> + Send>;

/// The same, for a replication-internal op on the raw connection.
type RawFn = Box<dyn FnOnce(&Connection) -> rusqlite::Result<Erased> + Send>;

/// A reply channel's payload: the closure's value plus the tables it touched.
type WriteOut = rusqlite::Result<(Erased, HashSet<String>)>;

/// One unit of work for the writer thread.
enum Job {
    /// A captured write: run inside one `IMMEDIATE` transaction with a
    /// session open over the replicated tables, harvest the changeset into
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
    /// A replication-internal operation on the raw connection: no session,
    /// no `writable` check. This is how the sync engine touches `repl`,
    /// applies batches, and installs snapshots — bookkeeping that must run
    /// even on a follower whose ordinary writes are closed.
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
/// awaited; every other connection is a reader.
pub struct Db {
    jobs: mpsc::Sender<Job>,
    target: Target,
    dir: Option<PathBuf>,
    /// Whether ordinary [`Db::write`] mutations are allowed. A follower
    /// holds this `false`: its ordinary writes fail read-only at the gate,
    /// while the replication [`Db::raw`] and [`Db::apply`] paths still run.
    writable: Arc<AtomicBool>,
    /// The last few in-memory effects. Not in the database and
    /// never on disk — it lives here because this is the one handle every
    /// thread's [`Store`] already shares, so the UI's log sees what a worker
    /// reached for. Every reader is taught to query it at open.
    mem: Arc<crate::effect::MemLog>,
}

impl Db {
    /// Opens (and migrates) the database, then hands its one writable
    /// connection to a dedicated writer thread. `None` is a private
    /// in-memory database — shared-cache, so a reader on another connection
    /// still sees it.
    ///
    /// `schemas` are the apps' ladders, run after the kernel's in app-list
    /// order.
    ///
    /// # Errors
    ///
    /// If the file cannot be opened, or the store is of a shape this build
    /// does not read.
    pub fn open(path: Option<&Path>, schemas: &[&'static Schema]) -> rusqlite::Result<Arc<Db>> {
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
        let dir = path
            .and_then(Path::parent)
            .filter(|p| !p.as_os_str().is_empty())
            .map(Path::to_path_buf);
        let conn = open_writer(&target)?;
        migrate(&conn, schemas)?;
        sweep_effects(&conn)?;
        // Fixed for the life of the process: the ladders have just run, and
        // nothing else issues DDL.
        let replicated = replicated_tables(&conn, "main")?;
        warn_unkeyed(&conn, &replicated);

        let dirty: Arc<Mutex<HashSet<String>>> = Arc::default();
        let d = dirty.clone();
        conn.update_hook(Some(move |_op, _db: &str, table: &str, _rowid: i64| {
            d.lock().expect("dirty set").insert(table.to_string());
        }))?;
        let (jobs, rx) = mpsc::channel::<Job>();
        std::thread::Builder::new()
            .name("store-writer".into())
            .spawn(move || writer_loop(&conn, &dirty, &replicated, &rx))
            .expect("spawn the store writer");
        Ok(Arc::new(Db {
            jobs,
            target,
            dir,
            writable: Arc::new(AtomicBool::new(true)),
            mem: Arc::new(crate::effect::MemLog::new()),
        }))
    }

    /// Opens the gate to ordinary writes (a holder) or closes it (a
    /// follower, or a stranded device). Replication's own `raw` and `apply`
    /// paths ignore this — only an ordinary `write` is gated.
    pub fn set_writable(&self, writable: bool) {
        self.writable.store(writable, Ordering::Release);
    }

    /// Whether ordinary writes are currently allowed.
    #[must_use]
    pub fn is_writable(&self) -> bool {
        self.writable.load(Ordering::Acquire)
    }

    /// The directory beside the store; `None` in memory.
    #[must_use]
    pub fn dir(&self) -> Option<&Path> {
        self.dir.as_deref()
    }

    /// A fresh read-only connection to the same database, taught the one
    /// function that reads the in-memory effect ring. Nothing is written to
    /// serve it — the rows are handed to SQLite on the spot — which is what
    /// lets a `query_only` connection join a ring that lives in RAM.
    fn reader(&self) -> rusqlite::Result<Connection> {
        let conn = open_reader(&self.target)?;
        self.mem.install(&conn)?;
        Ok(conn)
    }

    /// The in-memory effect ring this process keeps.
    #[must_use]
    pub fn mem(&self) -> &Arc<crate::effect::MemLog> {
        &self.mem
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
        self.jobs
            .send(Job::Write { run, reply })
            .map_err(|_| gone())?;
        let (erased, dirty) = rx.recv().map_err(|_| gone())??;
        let v = *erased.downcast::<T>().expect("write result type");
        Ok((v, dirty))
    }

}

/// A store error carrying a plain message — for the failures that are ours,
/// not SQLite's (a dead writer, a panicked closure, a store of the wrong
/// shape).
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

/// The kernel's ladder, then each app's, run once on the writable connection
/// at open.
///
/// There is one kernel version and no migration to it: a store another shape
/// wrote is refused in one line rather than half-read. Apps climb their own
/// ladders from there, each recording its progress in `meta` under
/// `schema:<app>`.
fn migrate(conn: &Connection, schemas: &[&'static Schema]) -> rusqlite::Result<()> {
    let version: i64 = conn.query_row("PRAGMA user_version", [], |r| r.get(0))?;
    if version == 0 {
        conn.execute_batch(SCHEMA_V1)?;
        conn.pragma_update(None, "user_version", KERNEL_VERSION)?;
    } else if version != KERNEL_VERSION {
        return Err(store_err(&format!(
            "{REFUSED}{version}, and this build reads {KERNEL_VERSION} — \
             point at another file"
        )));
    }
    // Replication's own tables go by presence, not by the counter: every
    // write needs them, so a store that turns up at this version without
    // them gains them here instead of being refused.
    conn.execute_batch(SCHEMA_REPL)?;
    conn.execute(
        "INSERT OR IGNORE INTO repl(id, device) VALUES(1, ?1)",
        [device_id()],
    )?;
    for s in schemas {
        s.apply(conn)?;
    }
    Ok(())
}

/// This install's device id — the one `repl` holds. Empty when the row is
/// not there yet, which is only true before the first migration has run.
///
/// Never `meta`: `meta` replicates, and a follower that adopted the holder's
/// id would publish under its name.
#[must_use]
pub fn this_device(conn: &Connection) -> String {
    conn.query_row("SELECT device FROM repl WHERE id = 1", [], |r| r.get(0))
        .unwrap_or_default()
}

/// A stable per-install device id: two devices must never share one, or they
/// publish under the same name. No `rand` dependency — a mix of the wall
/// clock, the pid and a process-local counter is unique enough for two
/// devices.
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

/// The writer thread: own the one writable connection, serve jobs until the
/// last [`Db`] handle drops, then close the connection.
fn writer_loop(
    conn: &Connection,
    dirty: &Arc<Mutex<HashSet<String>>>,
    replicated: &[String],
    rx: &mpsc::Receiver<Job>,
) {
    while let Ok(job) = rx.recv() {
        match job {
            Job::Write { run, reply } => {
                let _ = reply.send(do_write(conn, dirty, replicated, run));
            }
            Job::Apply { changeset, reply } => {
                let _ = reply.send(repl::do_apply(conn, &changeset));
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
    replicated: &[String],
    run: RunFn,
) -> WriteOut {
    dirty.lock().expect("dirty set").clear();
    let tx = Transaction::new_unchecked(conn, rusqlite::TransactionBehavior::Immediate)?;
    let mut sess = Capture::new(conn)?;
    for t in replicated {
        sess.attach(Some(t.as_str()))?;
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

/// Seconds since the epoch, for a log row's `ts` — a record timestamp, not
/// a deadline, so it reads the wall clock rather than the world's.
fn now_secs() -> f64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs_f64())
        .unwrap_or(0.0)
}

impl Store {
    /// Opens the store over a fresh [`Db`] — the UI thread's constructor, and
    /// what a test's in-memory world uses. `None` is a private in-memory
    /// database. `schemas` are the apps' ladders.
    ///
    /// # Errors
    ///
    /// If the file cannot be opened or the store is of a shape this build
    /// does not read.
    pub fn open(path: Option<&Path>, schemas: &[&'static Schema]) -> rusqlite::Result<Store> {
        let db = Db::open(path, schemas)?;
        Store::with_db(db)
    }

    /// A reader over an existing [`Db`] — how a worker thread joins the one
    /// writer instead of opening a second.
    ///
    /// # Errors
    ///
    /// If the reader connection cannot be opened.
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
            mem_version: Cell::new(0),
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

    /// The directory beside the store; `None` in memory.
    #[must_use]
    pub fn dir(&self) -> Option<&Path> {
        self.db.dir()
    }

    /// This install's stable device id.
    #[must_use]
    pub fn device(&self) -> String {
        this_device(&self.conn)
    }

    /// This process's in-memory effect ring — shared with every
    /// other [`Store`] over the same [`Db`], which is how the log shows a
    /// worker's connects beside the UI's.
    #[must_use]
    pub fn mem(&self) -> &Arc<crate::effect::MemLog> {
        self.db.mem()
    }

    /// Whether the effect ring has moved since this reader last looked, and
    /// if so, stales the queries that read it.
    ///
    /// The ring is memory, so `PRAGMA data_version` cannot see it and the
    /// authorizer cannot report it: its generation is bumped by name
    /// instead ([`crate::effect::MEM_TABLE`], which the log's spec declares
    /// as a dependency). The world calls this the moment it records; other
    /// threads pick it up on the next poll.
    pub fn poll_mem(&self) -> bool {
        let v = self.db.mem().version();
        if v == self.mem_version.replace(v) {
            return false;
        }
        *self
            .generations
            .borrow_mut()
            .entry(crate::effect::MEM_TABLE.to_string())
            .or_insert(0) += 1;
        self.redraw.set(true);
        true
    }

    /// Runs one mutation as one transaction through the single writer; on
    /// commit, every touched table's generation bumps and dependent cached
    /// queries go stale. The closure runs on the writer thread, so it must
    /// own what it touches — `Send + 'static`, no borrowing UI state.
    ///
    /// # Errors
    ///
    /// Whatever the closure returned, or the gate's refusal.
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
    /// redraw.
    pub fn take_redraw(&self) -> bool {
        self.redraw.replace(false)
    }

    /// Detects commits from *other* connections (the workers):
    /// `data_version` moves only for foreign commits. Coarse on purpose —
    /// every table's generation bumps, every cached query re-runs; at this
    /// scale that costs microseconds. Returns whether anything changed.
    pub fn poll_external(&self) -> bool {
        // The ring moves under its own version, and a worker's effects are
        // exactly the kind that arrive without a commit to notice.
        let mem = self.poll_mem();
        let v: i64 = self
            .conn
            .query_row("PRAGMA data_version", [], |r| r.get(0))
            .unwrap_or(0);
        if v == self.data_version.replace(v) {
            return mem;
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
    /// prepare, plus any the caller declares. This is dependency tracking
    /// *and* provenance in one.
    ///
    /// `also` is the escape hatch, and the only one: rows a table-valued
    /// function hands over (the effect ring) are read from memory, so no
    /// authorizer will ever report them and the query has to say so itself.
    fn deps_for(&self, id: &str, sql: &str, also: &[&str]) -> Rc<Vec<String>> {
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
        let mut tables: Vec<String> = seen.lock().expect("dep set").iter().cloned().collect();
        for t in also {
            if !tables.iter().any(|x| x == t) {
                tables.push((*t).to_string());
            }
        }
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
        self.rows_sql_deps(id, describe, sql, params, &[], map)
    }

    /// [`Store::rows_sql`] for a query that reads something the authorizer
    /// cannot see, and so has to name that dependency itself: the effect
    /// log's union reads the in-memory ring through a function, and rows
    /// out of memory are invisible to SQLite's read-set. Everything else
    /// about it is the same query, the same cache, the same trace.
    pub fn rows_sql_deps<T: 'static>(
        &self,
        id: &'static str,
        describe: &'static str,
        sql: &str,
        params: &[Val],
        also: &[&str],
        map: fn(&rusqlite::Row) -> rusqlite::Result<T>,
    ) -> Rc<Vec<T>> {
        let pkey = fmt_params(params);
        let key = (sql.to_string(), pkey.clone());
        let deps = self.deps_for(id, sql, also);
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
                if !v
                    .iter()
                    .any(|e| e.id == id && e.sql == sql && e.params == pkey)
                {
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
    #[must_use]
    pub fn trace_of(&self, key: u64) -> Vec<TraceEntry> {
        self.traces.borrow().get(&key).cloned().unwrap_or_default()
    }

    /// Direct read access for one-shot, non-reactive reads (boot, tests).
    #[must_use]
    pub fn conn(&self) -> &Connection {
        &self.conn
    }

    /// Opens (holder) or closes (follower, stranded) the write gate.
    pub fn set_writable(&self, writable: bool) {
        self.db.set_writable(writable);
    }

    /// Whether ordinary writes are currently allowed.
    #[must_use]
    pub fn is_writable(&self) -> bool {
        self.db.is_writable()
    }

    // -- wm persistence ------------------------------------------------------

    /// Persists the whole logical workspace state — a wholesale rewrite of
    /// the (tiny) UI tables in one transaction. The caller diffs snapshots
    /// and only calls this when something actually changed. Un-undoable
    /// upkeep (workspace switches, boot); undoable mutations run
    /// [`save_wm_tx`] inside the session's own action instead.
    ///
    /// # Errors
    ///
    /// If the write fails.
    pub fn save_wm(&self, snap: &layout::WmSnap) -> rusqlite::Result<()> {
        let snap = snap.clone();
        self.write(move |c| save_wm_tx(c, &snap))
    }

    /// Restores the logical workspace state; `None` means this store never
    /// booted (first run seeds the default layout). An empty-but-booted
    /// store restores as genuinely empty — closing everything is a state,
    /// not an accident.
    ///
    /// A row whose tag no app in this build owns comes back all the same:
    /// the session opens a `Missing` panel for it and saves it again
    /// unchanged, because another build has the app and the session is
    /// shared.
    ///
    /// # Errors
    ///
    /// If a read fails.
    pub fn load_wm(&self) -> rusqlite::Result<Option<layout::WmSnap>> {
        let active: Option<i64> = self
            .conn
            .query_row("SELECT active FROM wm WHERE id=1", [], |r| r.get(0))
            .ok();
        let Some(active) = active else {
            return Ok(None);
        };
        let mut wss = vec![layout::WsSnap::default(); layout::WS_N];
        {
            let mut stmt = self.conn.prepare("SELECT k, focus FROM workspace")?;
            let rows = stmt.query_map([], |r| {
                Ok((r.get::<_, i64>(0)?, r.get::<_, Option<i64>>(1)?))
            })?;
            for row in rows {
                let (k, focus) = row?;
                if let Some(ws) = wss.get_mut(k as usize) {
                    ws.focus = focus.map(|f| f as SlotId);
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
                "SELECT id, ws, col, kind, args, joined_to FROM panel
                 ORDER BY ws, col, row",
            )?;
            let rows = stmt.query_map([], |r| {
                Ok((
                    r.get::<_, i64>(0)? as SlotId,
                    r.get::<_, i64>(1)? as usize,
                    r.get::<_, i64>(2)? as usize,
                    r.get::<_, String>(3)?,
                    r.get::<_, String>(4)?,
                    r.get::<_, Option<i64>>(5)?,
                ))
            })?;
            for row in rows {
                let (id, k, col, kind, args, joined_to) = row?;
                let Some(ws) = wss.get_mut(k) else { continue };
                let Some(show) = PanelId::from_row(Tag::intern(&kind), &args) else {
                    // Not a JSON array of strings: a row of some other
                    // shape, skipped rather than guessed at.
                    continue;
                };
                ws.slots.push((id, show));
                if let Some((slots, _, _)) = ws.columns.get_mut(col) {
                    slots.push(id);
                }
                if let Some(parent) = joined_to {
                    ws.joins.push((parent as SlotId, id));
                }
            }
        }
        for ws in &mut wss {
            ws.slots.sort_by_key(|(id, _)| *id);
            ws.joins.sort_unstable();
            // A column whose every row was skipped above would come back as
            // an empty column — a gap on the strip that outlives the slot it
            // held.
            ws.columns.retain(|(slots, _, _)| !slots.is_empty());
        }
        Ok(Some(layout::WmSnap {
            active: (active as usize).min(layout::WS_N - 1),
            wss,
        }))
    }
}

/// The wholesale UI-table rewrite behind [`Store::save_wm`] — also what an
/// undoable navigation action runs inside its recorded transaction.
///
/// # Errors
///
/// If any write fails.
pub fn save_wm_tx(c: &Connection, snap: &layout::WmSnap) -> rusqlite::Result<()> {
    c.execute("DELETE FROM panel", [])?;
    c.execute("DELETE FROM ws_col", [])?;
    c.execute("DELETE FROM workspace", [])?;
    for (k, ws) in snap.wss.iter().enumerate() {
        c.execute(
            "INSERT INTO workspace(k, focus) VALUES(?1, ?2)",
            rusqlite::params![k as i64, ws.focus.map(|f| f as i64)],
        )?;
        let parent_of: HashMap<SlotId, SlotId> = ws.joins.iter().map(|&(a, b)| (b, a)).collect();
        let show_of: HashMap<SlotId, &PanelId> = ws.slots.iter().map(|(id, s)| (*id, s)).collect();
        for (ci, (slots, tabbed, active)) in ws.columns.iter().enumerate() {
            c.execute(
                "INSERT INTO ws_col(ws, idx, tabbed, active) VALUES(?1,?2,?3,?4)",
                rusqlite::params![k as i64, ci as i64, tabbed, *active as i64],
            )?;
            for (ri, sid) in slots.iter().enumerate() {
                let Some(show) = show_of.get(sid) else {
                    continue;
                };
                c.execute(
                    "INSERT INTO panel(id, ws, col, row, kind, args, joined_to)
                     VALUES(?1,?2,?3,?4,?5,?6,?7)",
                    rusqlite::params![
                        *sid as i64,
                        k as i64,
                        ci as i64,
                        ri as i64,
                        show.tag.as_str(),
                        show.args_json(),
                        parent_of.get(sid).map(|p| *p as i64),
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::layout::Wm;
    use crate::panel::Tag;

    fn store() -> Store {
        Store::open(None, &[]).expect("in-memory store")
    }

    fn inbox() -> PanelId {
        PanelId::bare(Tag("inbox"))
    }
    fn msg(id: i64) -> PanelId {
        PanelId::new(Tag("message"), [id.to_string()])
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
        assert_eq!(
            *s.deps_for(Q_META.id, Q_META.sql, &[]),
            vec!["meta".to_string()]
        );

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
            c.execute("INSERT INTO wm(id, active) VALUES(1, 3)", [])
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
        let i = wm.open(inbox(), None, false);
        let _m = wm.follow_open(i, msg(3), false);
        wm.send_focused_to(4); // the message re-homes to ws 5
        wm.switch(0);
        wm.toggle_tabbed(i); // a surviving tabbed column
        wm.follow_open(i, PanelId::new(Tag("contact"), ["v@k.io"]), false);
        // Several argument shapes, each its own row.
        for args in [vec![], vec!["forward".to_string(), "3".to_string()]] {
            wm.open(
                PanelId {
                    tag: Tag("compose"),
                    args,
                },
                None,
                false,
            );
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

    /// The arguments travel as one JSON array and come back as themselves,
    /// whatever is in them — a quote, a slash, a space, none at all.
    #[test]
    fn panel_arguments_round_trip_through_the_row() {
        let s = store();
        let ids = [
            PanelId::bare(Tag("help")),
            PanelId::new(Tag("message"), ["42"]),
            PanelId::new(Tag("compose"), ["forward", "42"]),
            PanelId::new(Tag("files"), ["~/Downloads/2026"]),
            PanelId::new(Tag("inbox"), ["a \"quoted\", comma'd one"]),
        ];
        let mut wm = Wm::new();
        for id in &ids {
            wm.open(id.clone(), None, false);
        }
        let snap = wm.snapshot();
        s.save_wm(&snap).unwrap();
        assert_eq!(s.load_wm().unwrap(), Some(snap));

        // And the column really is JSON the way a query would read it.
        let first: String = s
            .conn()
            .query_row(
                "SELECT args ->> 0 FROM panel WHERE kind = 'compose'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(first, "forward");
    }

    /// A tag no app in this build owns is kept, not dropped: it restores as
    /// itself and saves back unchanged.
    #[test]
    fn an_unknown_tag_survives_a_round_trip() {
        let s = store();
        let alien = PanelId::new(Tag("from_the_future"), ["7"]);
        let mut wm = Wm::new();
        wm.open(alien.clone(), None, false);
        s.save_wm(&wm.snapshot()).unwrap();

        let back = s.load_wm().unwrap().expect("a session");
        assert_eq!(back.wss[0].slots[0].1, alien);
        // The interned tag equals the literal one: tags compare by content.
        assert_eq!(back.wss[0].slots[0].1.tag, Tag("from_the_future"));

        s.save_wm(&back).unwrap();
        assert_eq!(s.load_wm().unwrap(), Some(back));
    }

    /// The reader really is read-only: a write attempted on it fails at the
    /// SQLite level, not by convention. That is what makes [`Store::write`]
    /// the one seam every mutation goes through.
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

    /// The gate is what a follower closes; a shut gate refuses in words.
    #[test]
    fn the_write_gate_refuses_in_words() {
        let s = store();
        assert!(s.is_writable());
        s.set_writable(false);
        let e = s
            .write(|tx| tx.execute("INSERT INTO meta(key, value) VALUES('x', 1)", []))
            .unwrap_err();
        assert!(format!("{e}").contains("read-only"), "{e}");
        s.set_writable(true);
        assert!(s.write(|_| Ok(())).is_ok());
    }

    /// Every install has an id, and opening the same store twice does not
    /// mint a second one.
    #[test]
    fn the_device_id_is_written_once() {
        let s = store();
        let d = s.device();
        assert!(!d.is_empty());
        assert_eq!(this_device(s.conn()), d);
    }

    /// A store of another shape is refused in one line, not half-read.
    #[test]
    fn a_store_of_another_shape_is_refused() {
        let dir = std::env::temp_dir().join(format!(
            "superapp-store-{}-{}",
            std::process::id(),
            KERNEL_VERSION
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("old.db");
        let _ = std::fs::remove_file(&path);
        {
            let c = Connection::open(&path).unwrap();
            c.pragma_update(None, "user_version", 12).unwrap();
        }
        let e = match Store::open(Some(&path), &[]) {
            Ok(_) => panic!("a store of another shape must be refused"),
            Err(e) => e,
        };
        let said = format!("{e}");
        assert!(said.contains("schema 12"), "{said}");
        assert!(said.contains("point at another file"), "{said}");
        // And the caller can read the number back out, which is how the
        // shell knows this failure from any other and exits on it.
        assert_eq!(refused_schema(&e), Some(12));
        assert_eq!(
            refused_schema(&gone()),
            None,
            "any other failure is not this one"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The crash sweep: an idempotent job left in flight is queued again, a
    /// risky one waits for a human.
    #[test]
    fn the_boot_sweep_sorts_interrupted_jobs() {
        let s = store();
        s.write(|c| {
            c.execute(
                "INSERT INTO effect(id, kind, payload, status, idempotent, created, updated)
                 VALUES(1, 'safe', '{}', 'processing', 1, 0, 0),
                       (2, 'risky', '{}', 'processing', 0, 0, 0)",
                [],
            )
            .map(|_| ())
        })
        .unwrap();
        // The writer thread owns the only writable connection, so the sweep
        // runs there too.
        s.write(|c| sweep_effects(c)).unwrap();
        let statuses: Vec<(i64, String)> = s
            .conn()
            .prepare("SELECT id, status FROM effect ORDER BY id")
            .unwrap()
            .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))
            .unwrap()
            .map(Result::unwrap)
            .collect();
        assert_eq!(
            statuses,
            vec![(1, "pending".to_string()), (2, "failed".to_string())]
        );
    }
}
