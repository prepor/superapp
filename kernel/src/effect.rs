//! Work whose result cannot be recreated from the database.
//!
//! Retryable [`Deferred`] effects are stored as jobs. Immediate effects return
//! directly and the latest [`KEPT`] entries stay in [`MemLog`]. The effect log
//! combines both sources. A [`World`] holds the store, the capabilities one
//! thread may reach the outside through, and the registry that decodes a
//! filed payload back into an effect.

use std::cell::RefCell;
use std::collections::HashMap;
use std::collections::VecDeque;
use std::rc::Rc;
use std::sync::atomic::{AtomicI64, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use rusqlite::functions::FunctionFlags;
use rusqlite::{Connection, Transaction};
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};

use crate::app::{Capabilities, Env, Mode};
use crate::filter::Op;
use crate::richtable::{Dir, SqlSource, SqlSpec, Suggestion, TagDef, TagSql, TagType, Values};
use crate::store::{Store, Val};

/// What an effect is performed against: the world's capabilities, plus
/// read-only store access so a payload can reference a row instead of
/// embedding its contents. No transaction is ever open here — that is the
/// point.
pub struct Ctx<'a> {
    pub caps: &'a mut Capabilities,
    pub db: &'a Connection,
}

impl Ctx<'_> {
    /// How an effect's `perform` reaches the outside:
    /// `cx.cap::<dyn Imap>()?.fetch(...)`. A missing capability is the
    /// error, in words.
    ///
    /// # Errors
    ///
    /// If this world has no such capability.
    pub fn cap<C: ?Sized + 'static>(&mut self) -> Result<&mut C, String> {
        // Borrowed twice on the error path, so the name is taken first.
        let name = short_name::<C>();
        self.caps
            .get::<C>()
            .ok_or_else(|| format!("this world has no {name}"))
    }
}

/// A capability's name as the error says it: `dyn kernel::caps::Disk` reads
/// as *Disk*.
pub(crate) fn short_name<C: ?Sized + 'static>() -> &'static str {
    let full = std::any::type_name::<C>();
    full.rsplit("::").next().unwrap_or(full)
}

// -- the traits ----------------------------------------------------------------

/// Something that leaves the process.
///
/// Deliberately **not** `Serialize`: an in-memory effect is performed at the
/// call and written nowhere, so making it serializable would be a lie — and
/// a dangerous one, since such an effect may carry a password.
/// Serializability belongs to [`Deferred`], where a row actually exists.
pub trait Effect: Sized {
    /// Stable, greppable, the table's `kind`.
    const KIND: &'static str;
    /// What this call answers.
    type Reply;
    /// One line of English — the row's description, the label in a status
    /// UI, and what an assertion failure prints. Never carries a secret.
    fn describe(&self) -> String;
    /// Did the world change because of this, or was it only asked
    /// something? A move, a send, a file written, a password filed: those
    /// changed it. A fetch, a folder listing, the clock, a password
    /// recalled: those did not — and neither did a connect, which is what
    /// makes the rest possible and nothing more.
    ///
    /// No default, on purpose. A background pass asks the outside a dozen
    /// questions for every answer it acts on, so the log is mostly reads,
    /// and the panel opens on `@wrote` for exactly that reason — a new
    /// effect that guessed here would either bury the panel or vanish from
    /// it, and neither failure announces itself. The compiler asks instead.
    fn writes(&self) -> bool;
    /// What this belongs to, in the `action.entity` vocabulary —
    /// `account:2`, `outbox:7`. A deferred effect files it on its row so a
    /// panel can query its own work; an in-memory one hands it to the ring
    /// for the same reason, which is why the question is asked here and not
    /// one trait down.
    fn entity(&self) -> Option<String> {
        None
    }
    /// Do it.
    ///
    /// # Errors
    ///
    /// Whatever the outside said, in words a human reads.
    fn perform(&self, cx: &mut Ctx<'_>) -> Result<Self::Reply, String>;
}

/// An effect worth persisting: queued, retried, its status and reply
/// readable from the table. Both the effect and its reply must survive a
/// round trip through JSON, so an effect that cannot be written down is a
/// compile error rather than a discovery.
// `Send` is required because a job's `settle` closure travels to the store's
// writer thread: the effect value and its reply are captured and committed
// there. Every real effect is plain data, so this is free.
pub trait Deferred: Effect + Serialize + DeserializeOwned + Send + 'static
where
    Self::Reply: Serialize + DeserializeOwned + Send,
{
    /// Is running this twice safe? No default — it is the one judgement a
    /// crash cannot guess, and it drives the boot sweep.
    fn idempotent(&self) -> bool;

    /// Does the world still want this? Checked after the claim and before
    /// the round trip: if undo landed while the job sat in the queue, it
    /// goes `obsolete` instead of performing stale work.
    fn still_wanted(&self, _db: &Connection) -> bool {
        true
    }

    /// What the success establishes — runs in the **same transaction** as
    /// the status update, so "the effect happened" and "the world now looks
    /// like this" land together or not at all.
    ///
    /// # Errors
    ///
    /// If the store refuses the write; the job is then retried.
    fn settle(&self, _tx: &Transaction, _reply: &Self::Reply) -> rusqlite::Result<()> {
        Ok(())
    }
}

// -- what the process keeps of the in-memory ones -------------------------------
//
// An in-memory effect writes nothing, and for a long time that also meant
// nobody could look at one: a connect that failed lived exactly as long as
// the string it returned. The ring fixes that without touching the rule —
// it keeps a *description* of the last few, in memory, and the log reads it
// through SQL beside the queue.

/// How many in-memory effects the ring keeps. A background pass and the
/// keystrokes around it fit; the whole ring is one JSON string the log's
/// query reads in full, so this is also how big that string gets.
pub const KEPT: usize = 200;

/// The name the ring goes by in the store's invalidation clock. Not a
/// table: SQLite's authorizer cannot report the rows a function handed it,
/// so the log's spec names this dependency itself
/// ([`SqlSpec::deps`](crate::richtable::SqlSpec::deps)), and the store
/// bumps it when the ring moves.
pub const MEM_TABLE: &str = "mem_effect";

/// One in-memory effect, after the fact — everything the log can show of
/// one, and nothing else. There is no payload because there was never one
/// to have (an in-memory effect is deliberately not `Serialize`: see
/// [`Effect`]), and no reply because the reply went to the caller. What is
/// left is the sentence the effect described itself with, which is what a
/// human reads anyway.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MemEffect {
    /// Its place in the ring, counting up for the life of the process. The
    /// log carries it **negated** — see `LOG_FROM`.
    pub seq: i64,
    pub kind: &'static str,
    pub entity: Option<String>,
    /// [`Effect::writes`] — the one question the panel opens on.
    pub writes: bool,
    /// [`Effect::describe`], taken at the call. Never carries a secret.
    pub what: String,
    pub error: Option<String>,
    pub at: f64,
}

/// The last [`KEPT`] in-memory effects.
///
/// One per process, held by the [`Db`](crate::store::Db) every [`World`]
/// shares — the UI's and each worker's — so the log shows what a worker
/// reached for as readily as what the keyboard did. It is also why this is
/// `Mutex` and not `RefCell`: the writers are threads.
#[derive(Debug)]
pub struct MemLog {
    rows: Mutex<VecDeque<MemEffect>>,
    /// Bumped on every record. This is the ring's `PRAGMA data_version`:
    /// a reader compares it against what it last saw and invalidates the
    /// pages that read the ring, since no commit hook will ever fire for
    /// something that is not in the database.
    version: AtomicU64,
    /// The next `seq`. Starts at 1, so a negated id is always negative.
    next: AtomicI64,
}

impl Default for MemLog {
    fn default() -> MemLog {
        MemLog::new()
    }
}

impl MemLog {
    /// An empty ring. `seq` starts at 1, never 0 — a negated 0 is 0, and
    /// 0 would read as a filed row.
    #[must_use]
    pub fn new() -> MemLog {
        MemLog {
            rows: Mutex::default(),
            version: AtomicU64::new(0),
            next: AtomicI64::new(1),
        }
    }

    /// Files one, dropping the oldest once the ring is full.
    pub fn record(&self, e: MemEffect) {
        {
            let mut rows = self.rows.lock().expect("the effect ring");
            while rows.len() >= KEPT {
                rows.pop_front();
            }
            rows.push_back(e);
        }
        self.version.fetch_add(1, Ordering::Release);
    }

    /// The next seq — taken before the effect runs, so the ring's order is
    /// the order things were *asked for*, as the queue's ids are.
    pub fn next_seq(&self) -> i64 {
        self.next.fetch_add(1, Ordering::Relaxed)
    }

    /// How many records the ring holds.
    #[must_use]
    pub fn len(&self) -> usize {
        self.rows.lock().expect("the effect ring").len()
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// What the ring has moved to. Compared, never interpreted.
    #[must_use]
    pub fn version(&self) -> u64 {
        self.version.load(Ordering::Acquire)
    }

    /// The ring as one JSON array, oldest first — what `mem_effects()`
    /// answers with.
    #[must_use]
    pub fn json(&self) -> String {
        let rows = self.rows.lock().expect("the effect ring");
        serde_json::to_string(&*rows).unwrap_or_else(|_| "[]".to_string())
    }

    /// Teaches one connection the `mem_effects()` function `LOG_FROM` reads
    /// the ring through. Every reader gets it at open, which is what makes
    /// the ring queryable from a `query_only` connection at all — nothing is
    /// written anywhere, the rows are handed to SQLite on the spot.
    ///
    /// Deliberately **not** `SQLITE_DETERMINISTIC`: the ring moves under a
    /// prepared statement, and a call SQLite factored out would freeze it.
    ///
    /// # Errors
    ///
    /// If SQLite refuses the registration.
    pub fn install(self: &Arc<Self>, conn: &Connection) -> rusqlite::Result<()> {
        let me = Arc::clone(self);
        // The name is spelled out in `LOG_FROM` too — a `const` cannot
        // interpolate one, and the two live in this file together.
        conn.create_scalar_function("mem_effects", 0, FunctionFlags::SQLITE_UTF8, move |_| {
            Ok(me.json())
        })
    }
}

// -- the registry --------------------------------------------------------------

/// The bookkeeping a success carries, committed with its status update.
/// `Send`, because it is committed on the store's writer thread.
type Settle = Box<dyn FnOnce(&Transaction) -> rusqlite::Result<()> + Send>;

/// What running one claimed job produced.
pub(crate) enum Ran {
    /// Reply JSON, plus the bookkeeping to commit alongside the status.
    Done(String, Settle),
    /// The world moved on; this job is no longer wanted.
    Obsolete,
    Failed(String),
    /// Nobody registered this kind — the loud failure an open set needs.
    NoHandler,
}

type Handler = Box<dyn Fn(&str, &mut Ctx<'_>) -> Ran>;

/// Decode a filed payload back into its effect's one line of English.
/// Fallible for the same reason a handler is: the row outlives the build
/// that wrote it.
type Describer = Box<dyn Fn(&str) -> Option<String>>;

/// Decode-and-perform, per kind. Each app registers its own effects, so
/// adding one touches no central list.
///
/// The cost of an open set is that a forgotten registration is a runtime
/// failure — so the executor makes it loud (`no handler for kind …`) rather
/// than leaving a job `pending` forever.
#[derive(Default)]
pub struct Registry {
    handlers: HashMap<&'static str, Handler>,
    describers: HashMap<&'static str, Describer>,
}

impl Registry {
    #[must_use]
    pub fn new() -> Registry {
        Registry::default()
    }

    /// Registers one deferred effect kind.
    pub fn register<E: Deferred>(&mut self)
    where
        E::Reply: Serialize + DeserializeOwned + Send,
    {
        self.handlers.insert(
            E::KIND,
            Box::new(|payload, cx| {
                let e: E = match serde_json::from_str(payload) {
                    Ok(e) => e,
                    Err(err) => return Ran::Failed(format!("undecodable payload: {err}")),
                };
                if !e.still_wanted(cx.db) {
                    return Ran::Obsolete;
                }
                match e.perform(cx) {
                    Ok(reply) => match serde_json::to_string(&reply) {
                        Ok(json) => Ran::Done(json, Box::new(move |tx| e.settle(tx, &reply))),
                        Err(err) => Ran::Failed(format!("unencodable reply: {err}")),
                    },
                    Err(err) => Ran::Failed(err),
                }
            }),
        );
        // The same registration teaches the queue to *read* itself back:
        // [`Effect::describe`] is the line a status UI wants, and a log
        // viewer that had to keep its own table of kinds would be exactly
        // the central list this registry exists to avoid.
        self.describers.insert(
            E::KIND,
            Box::new(|payload| {
                serde_json::from_str::<E>(payload)
                    .ok()
                    .map(|e| e.describe())
            }),
        );
    }

    /// One line of English for a filed job: the effect decoded from its
    /// payload and asked to describe itself. `None` when this build cannot
    /// read the kind — an unregistered app, or a row an older version
    /// wrote — and the caller falls back to the payload as it stands.
    #[must_use]
    pub fn describe(&self, kind: &str, payload: &str) -> Option<String> {
        self.describers.get(kind).and_then(|d| d(payload))
    }

    /// Decodes and performs one claimed job.
    pub(crate) fn run(&self, kind: &str, payload: &str, cx: &mut Ctx<'_>) -> Ran {
        match self.handlers.get(kind) {
            Some(h) => h(payload, cx),
            None => Ran::NoHandler,
        }
    }

    /// Every registered kind — a completeness test reads this.
    #[must_use]
    pub fn kinds(&self) -> Vec<&'static str> {
        let mut v: Vec<&'static str> = self.handlers.keys().copied().collect();
        v.sort_unstable();
        v
    }
}

// -- the log -------------------------------------------------------------------

/// One row of the log, as tests and the log viewer read it: a job of the
/// queue, or an in-memory effect the ring kept ([`Job::transient`]). The
/// whole row, payload included — this is the only shape the queue is ever
/// read in, and a viewer that showed less than `sqlite3` does would defeat
/// the reason the queue lives in the store at all.
#[derive(Debug, Clone, PartialEq)]
pub struct Job {
    /// The `effect` rowid — or, for a ring row, the negated
    /// [`MemEffect::seq`]. Positive is filed, negative never was.
    pub id: i64,
    pub kind: String,
    pub entity: Option<String>,
    /// pending | processing | done | failed | obsolete
    pub status: String,
    pub reply: Option<String>,
    pub error: Option<String>,
    pub attempts: i64,
    /// The JSON the effect was filed as — the registry decodes it back
    /// into one line of English ([`Registry::describe`]).
    pub payload: String,
    /// Whether running it twice is safe, copied onto the row at enqueue
    /// time so the crash sweep never has to decode a payload. False for a
    /// ring row, which nobody was going to retry either way — the column
    /// itself is `NULL` there, so `@risky` never sweeps one in.
    pub idempotent: bool,
    /// Filed at, last touched at, and the earliest the executor may claim
    /// it (a backoff, or a send window) — unix seconds, the world's clock.
    pub created: f64,
    pub updated: f64,
    pub not_before: f64,
    /// The sentence, for a row that carries no payload to derive one from:
    /// a ring row's [`MemEffect::what`]. `None` on a filed job, whose
    /// sentence the registry decodes ([`Registry::describe`]).
    pub what: Option<String>,
    /// Whether the world changed for it ([`Effect::writes`]). What the
    /// panel opens narrowed to, because a background pass asks a dozen
    /// questions for every answer it acts on.
    pub writes: bool,
}

impl Job {
    /// Whether this effect ran at the call and left no row — an
    /// [`MemEffect`] out of the ring rather than a job of the queue. The
    /// id says so: the queue's are rowids, the ring's are negated, so the
    /// two streams share one total order and one unique key without ever
    /// colliding.
    #[must_use]
    pub fn transient(&self) -> bool {
        self.id < 0
    }

    /// The status as the log reads it aloud: the word, and — once a job has
    /// been tried more than once — how many times. A count on every row
    /// would be noise; a count on the rows that fought is the whole story.
    /// A ring row says where it lives instead: it was never filed, so there
    /// is no row to go and look at, and that is worth saying on the line.
    #[must_use]
    pub fn status_line(&self) -> String {
        if self.transient() {
            format!("{} · in memory", self.status)
        } else if self.attempts > 1 {
            format!("{} · {} tries", self.status, self.attempts)
        } else {
            self.status.clone()
        }
    }
}

/// How long a failed job waits before its next attempt, by attempt count —
/// capped, because a server that is down stays down for a while.
fn backoff(attempts: i64) -> f64 {
    match attempts {
        0 | 1 => 5.0,
        2 => 30.0,
        3 => 120.0,
        _ => 600.0,
    }
}

/// After this many attempts a job stops retrying and waits for a human.
pub const MAX_ATTEMPTS: i64 = 6;

// -- reading the table ---------------------------------------------------------

fn job_row(r: &rusqlite::Row<'_>) -> rusqlite::Result<Job> {
    Ok(Job {
        id: r.get(0)?,
        kind: r.get(1)?,
        entity: r.get(2)?,
        status: r.get(3)?,
        reply: r.get(4)?,
        error: r.get(5)?,
        attempts: r.get(6)?,
        payload: r.get(7)?,
        // `NULL` on a ring row: it was never going to be retried, so it has
        // no answer here rather than the wrong one.
        idempotent: r.get::<_, Option<i64>>(8)?.is_some_and(|v| v != 0),
        created: r.get(9)?,
        updated: r.get(10)?,
        not_before: r.get(11)?,
        what: r.get(12)?,
        writes: r.get::<_, i64>(13)? != 0,
    })
}

/// The one column list, shared by the helpers below and by [`LOG_SPEC`] —
/// so the table the log viewer pages through and the rows a test asserts on
/// decode through the same [`job_row`], in the same order. Qualified,
/// because the spec's `FROM` aliases the table.
const JOB_COLS: &str = "e.id, e.kind, e.entity, e.status, e.reply, e.error, e.attempts,
                        e.payload, e.idempotent, e.created, e.updated, e.not_before,
                        e.what, e.writes";

/// The same, read straight off `effect` — the helpers below want the queue
/// and only the queue, so the sentence column a ring row would fill is a
/// literal `NULL` and [`job_row`] decodes both shapes.
const QUEUE_COLS: &str = "e.id, e.kind, e.entity, e.status, e.reply, e.error, e.attempts,
                          e.payload, e.idempotent, e.created, e.updated, e.not_before,
                          NULL, e.writes";

/// What the log selects from: the queue, and the ring of effects that never
/// became rows. One `UNION ALL` rather than two lists stitched together in
/// the panel, so the filter grammar, the paging, the count and the rank
/// stay the rich table's own — a ring row is narrowed by exactly the same
/// `@kind:` a filed one is.
///
/// Two things carry the join. The ring's ids are **negated**, and the
/// queue's are SQLite rowids, so the streams cannot collide: `e.id` is
/// still unique (the key a mark holds) and still the tiebreak that makes
/// the order total, and `e.id < 0` is how "never became a row" is asked.
/// And the columns a ring row has no answer for are `NULL` rather than a
/// plausible zero — `idempotent` above all, since `@risky` reads
/// `idempotent = 0` and must not sweep in effects nobody was going to
/// retry.
///
/// `mem_effects()` is the ring itself, one JSON array, taught to every
/// reader at open by [`MemLog::install`].
const LOG_FROM: &str = "(SELECT id, kind, entity, status, reply, error, attempts,
                                payload, idempotent, created, updated, not_before,
                                NULL AS what, writes
                           FROM effect
                          UNION ALL
                         SELECT -json_extract(r.value, '$.seq'),
                                json_extract(r.value, '$.kind'),
                                json_extract(r.value, '$.entity'),
                                CASE WHEN json_extract(r.value, '$.error') IS NULL
                                     THEN 'done' ELSE 'failed' END,
                                NULL,
                                json_extract(r.value, '$.error'),
                                1, '', NULL,
                                json_extract(r.value, '$.at'),
                                json_extract(r.value, '$.at'),
                                0,
                                json_extract(r.value, '$.what'),
                                json_extract(r.value, '$.writes')
                           FROM json_each(mem_effects()) r) e";

/// The dependency [`LOG_FROM`] has that no authorizer can see: the rows
/// `mem_effects()` hands over come from memory, so nothing in SQLite will
/// ever report them as read. Every query built on the union declares this
/// and the store bumps it when the ring moves.
const LOG_DEPS: &[&str] = &[MEM_TABLE];

/// Every job, oldest first.
#[must_use]
pub fn jobs(db: &Connection) -> Vec<Job> {
    let Ok(mut stmt) = db.prepare(&format!("SELECT {QUEUE_COLS} FROM effect e ORDER BY e.id"))
    else {
        return Vec::new();
    };
    stmt.query_map([], job_row)
        .map(|it| it.filter_map(Result::ok).collect())
        .unwrap_or_default()
}

/// Jobs after `id` — how a test marks a point and asserts on what followed.
#[must_use]
pub fn jobs_since(db: &Connection, id: i64) -> Vec<Job> {
    let Ok(mut stmt) = db.prepare(&format!(
        "SELECT {QUEUE_COLS} FROM effect e WHERE e.id > ?1 ORDER BY e.id"
    )) else {
        return Vec::new();
    };
    stmt.query_map([id], job_row)
        .map(|it| it.filter_map(Result::ok).collect())
        .unwrap_or_default()
}

/// One entity's jobs — what a panel shows about its own in-flight work.
#[must_use]
pub fn jobs_of(db: &Connection, entity: &str) -> Vec<Job> {
    let Ok(mut stmt) = db.prepare(&format!(
        "SELECT {QUEUE_COLS} FROM effect e WHERE e.entity = ?1 ORDER BY e.id"
    )) else {
        return Vec::new();
    };
    stmt.query_map([entity], job_row)
        .map(|it| it.filter_map(Result::ok).collect())
        .unwrap_or_default()
}

/// One row of the log by id — what its panel reads on every draw. Through
/// the query cache like everything else, so a job that finishes while it is
/// open finishes on screen. Over the union, so a negative id (an effect the
/// ring kept) opens as readily as a filed one.
#[must_use]
pub fn job(store: &Store, id: i64) -> Option<Job> {
    let sql = format!("SELECT {JOB_COLS} FROM {LOG_FROM} WHERE e.id = ?1");
    store
        .rows_sql_deps(
            "effect job",
            "one effect of the log, in full",
            &sql,
            &[Val::I(id)],
            LOG_DEPS,
            job_row,
        )
        .first()
        .cloned()
}

/// The newest job id — the mark for [`jobs_since`].
#[must_use]
pub fn mark(db: &Connection) -> i64 {
    db.query_row("SELECT COALESCE(MAX(id), 0) FROM effect", [], |r| r.get(0))
        .unwrap_or(0)
}

// -- the log as a rich table ---------------------------------------------------

/// The statuses a row can be in, as the filter offers them.
const STATUSES: &[(&str, &str)] = &[
    ("pending", "pending"),
    ("processing", "processing"),
    ("done", "done"),
    ("failed", "failed"),
    ("obsolete", "obsolete"),
];

/// The effect log's fixed query: everything that left the process, newest
/// first — the queue and the ring both. Flat: an effect is a row, and
/// nothing about it is an aggregate.
static LOG_SPEC: SqlSpec = SqlSpec {
    id: "effect log",
    describe: "everything that left the process, under the panel's filter, newest first",
    select: JOB_COLS,
    from: LOG_FROM,
    base: "",
    // Bare words search what a human would type: the verb, whose it was,
    // and what went wrong. The payload too — that is where a row id or an
    // address actually lives — and the ring's sentence, which is all a
    // row with no payload has.
    text: &["e.kind", "e.entity", "e.payload", "e.error", "e.what"],
    tags: &[
        ("failed", TagSql::Where("e.status = 'failed'")),
        (
            "live",
            TagSql::Where("e.status IN ('pending', 'processing')"),
        ),
        ("retried", TagSql::Where("e.attempts > 1")),
        // `NULL` on a ring row, so this never sweeps one in.
        ("risky", TagSql::Where("e.idempotent = 0")),
        ("memory", TagSql::Where("e.id < 0")),
        ("filed", TagSql::Where("e.id > 0")),
        ("wrote", TagSql::Where("e.writes = 1")),
        ("read", TagSql::Where("e.writes = 0")),
        ("status", TagSql::Col("e.status")),
        ("kind", TagSql::Col("e.kind")),
        ("entity", TagSql::Col("e.entity")),
        ("attempts", TagSql::Col("e.attempts")),
        ("date", TagSql::Col("e.created")),
    ],
    // When it happened, then the id. Total by construction: within each
    // stream the id counts up, and across the two it cannot collide,
    // because the ring's is negated.
    order: &[("e.created", Dir::Desc), ("e.id", Dir::Desc)],
    group: None,
    // …and the id is the row's identity too.
    key: "e.id",
    deps: LOG_DEPS,
};

/// The effect filter's tags: what `@` offers in the log panel.
static LOG_TAGS: &[TagDef] = &[
    TagDef {
        name: "failed",
        kind: TagType::Bool,
        ops: &[],
        describe: "gave up, waiting for a human",
        values: Values::None,
    },
    TagDef {
        name: "live",
        kind: TagType::Bool,
        ops: &[],
        describe: "still queued or in flight",
        values: Values::None,
    },
    TagDef {
        name: "retried",
        kind: TagType::Bool,
        ops: &[],
        describe: "took more than one attempt",
        values: Values::None,
    },
    TagDef {
        name: "risky",
        kind: TagType::Bool,
        ops: &[],
        describe: "not idempotent: a crash cannot retry it",
        values: Values::None,
    },
    TagDef {
        name: "wrote",
        kind: TagType::Bool,
        ops: &[],
        describe: "changed something out there — what the panel opens on",
        values: Values::None,
    },
    TagDef {
        name: "read",
        kind: TagType::Bool,
        ops: &[],
        describe: "only asked: a fetch, a search, a folder listing, a connect",
        values: Values::None,
    },
    TagDef {
        name: "memory",
        kind: TagType::Bool,
        ops: &[],
        describe: "ran at the call and left no row — kept only in the ring",
        values: Values::None,
    },
    TagDef {
        name: "filed",
        kind: TagType::Bool,
        ops: &[],
        describe: "a job of the queue: filed, claimed, retried",
        values: Values::None,
    },
    TagDef {
        name: "status",
        kind: TagType::Text,
        ops: &[Op::Eq],
        describe: "pending, processing, done, failed, obsolete",
        values: Values::Static(STATUSES),
    },
    TagDef {
        name: "kind",
        kind: TagType::Text,
        ops: &[Op::Eq],
        describe: "the effect's verb — now, clip, move, submit",
        values: Values::Dynamic,
    },
    TagDef {
        name: "entity",
        kind: TagType::Text,
        ops: &[Op::Eq],
        describe: "what it belongs to — account:1, outbox:7",
        values: Values::Dynamic,
    },
    TagDef {
        name: "attempts",
        kind: TagType::Number,
        ops: &[Op::Eq, Op::Gt, Op::Gte, Op::Lt, Op::Lte],
        describe: "how many times it has been tried",
        values: Values::None,
    },
    TagDef {
        name: "date",
        kind: TagType::Date,
        ops: &[Op::Eq, Op::Gt, Op::Gte, Op::Lt, Op::Lte],
        describe: "the day it was filed, 30.08.2026",
        values: Values::None,
    },
];

/// Values for the log's dynamic tags, under what has been typed. Both are
/// read off the log itself rather than off the registry: what is *there* is
/// what filtering it can find, and a kind this build no longer registers is
/// exactly the row a human goes looking for.
fn suggest_log(store: &Store, tag: &str, typed: &str) -> Vec<Suggestion> {
    let col = match tag {
        "kind" => "kind",
        "entity" => "entity",
        _ => return Vec::new(),
    };
    let sql = format!(
        "SELECT DISTINCT e.{col} FROM {LOG_FROM}
          WHERE e.{col} IS NOT NULL AND e.{col} != '' ORDER BY e.{col}"
    );
    store
        .rows_sql_deps(
            "effect log values",
            "the distinct values one effect-log tag takes",
            &sql,
            &[],
            LOG_DEPS,
            |r| r.get::<_, String>(0),
        )
        .iter()
        .filter(|v| v.to_lowercase().contains(typed))
        .map(Suggestion::value)
        .collect()
}

/// The effect log's datasource: what a log panel's rich table runs on.
pub static LOG: SqlSource<Job, i64> = SqlSource {
    spec: &LOG_SPEC,
    tags: LOG_TAGS,
    map: job_row,
    key: |j| j.id,
    rank: |j| vec![Val::F(j.created), Val::I(j.id)],
    suggest: suggest_log,
};

/// Rows per page of the log table.
pub const LOG_PAGE: usize = 50;

/// What a log panel opens with in its filter field.
///
/// A background pass asks the outside a dozen questions for every answer it
/// acts on — connect, select, search, fetch, and again next minute — so an
/// unfiltered log is mostly the app clearing its throat, and what a human
/// came to see (what was *changed* out there, and whether it worked) is
/// buried. It is typed into the field rather than folded into the query, so
/// it is visible, and clearing it is one gesture: this is a default, not a
/// rule about what the panel can show.
pub const LOG_DEFAULT: &str = "@wrote";

// -- the world -----------------------------------------------------------------

fn json_err(e: serde_json::Error) -> rusqlite::Error {
    rusqlite::Error::ToSqlConversionFailure(Box::new(e))
}

/// A deferred effect encoded and timestamped, ready to insert inside any
/// write transaction. Owned and `Send`, so it can be moved into a
/// [`Store::write`] closure that runs on the writer thread — the
/// composition primitive the passes build their jobs from.
pub struct Enqueue {
    kind: &'static str,
    payload: String,
    entity: Option<String>,
    idempotent: bool,
    /// [`Effect::writes`], copied onto the row for the same reason
    /// `idempotent` is: the log filters on it, and asking would mean
    /// decoding every payload on the page.
    writes: bool,
    not_before: f64,
    now: f64,
}

impl Enqueue {
    /// Inserts the job row into the caller's transaction, answering its id.
    ///
    /// # Errors
    ///
    /// If the insert fails.
    pub fn insert(&self, tx: &Transaction) -> rusqlite::Result<i64> {
        tx.execute(
            "INSERT INTO effect(kind, payload, entity, status, idempotent, writes,
                                attempts, not_before, created, updated)
             VALUES(?1, ?2, ?3, 'pending', ?4, ?5, 0, ?6, ?7, ?7)",
            rusqlite::params![
                self.kind,
                self.payload,
                self.entity,
                self.idempotent,
                self.writes,
                self.not_before,
                self.now
            ],
        )?;
        Ok(tx.last_insert_rowid())
    }
}

/// Cancels an unclaimed job inside the caller's transaction — undo's half of
/// the race with the executor, as a free function so a `Send` write closure
/// can call it without capturing the [`World`].
///
/// # Errors
///
/// If the update fails.
pub fn cancel_tx(tx: &Transaction, id: i64, now: f64) -> rusqlite::Result<bool> {
    let n = tx.execute(
        "UPDATE effect SET status='obsolete', updated=?2
         WHERE id=?1 AND status='pending'",
        rusqlite::params![id, now],
    )?;
    Ok(n == 1)
}

/// The store, the capabilities and the registry, as one value you construct
/// — never a global, never a path, never a thread you cannot see.
/// Single-threaded: the UI owns one, and each worker thread builds its own.
pub struct World {
    store: Rc<Store>,
    caps: RefCell<Capabilities>,
    /// Shared, so a panel can hold one and name what it is looking at
    /// ([`Registry::describe`]) — and no more than that: performing an
    /// effect needs the capabilities, which stay behind this world.
    registry: Rc<Registry>,
}

impl World {
    #[must_use]
    pub fn new(store: Rc<Store>, caps: Capabilities, registry: Registry) -> World {
        World {
            store,
            caps: RefCell::new(caps),
            registry: Rc::new(registry),
        }
    }

    /// An isolated world: its own in-memory store, the kernel's fake
    /// capabilities and a clock that only moves when a test moves it.
    /// Touches nothing beyond itself, so any number run in parallel.
    ///
    /// # Panics
    ///
    /// If SQLite cannot open an in-memory database.
    #[must_use]
    pub fn fake(registry: Registry) -> World {
        World::fake_with(&Env::default(), registry)
    }

    /// The same, over an environment a caller arranged — one shared clock
    /// across several worlds, a planted secret, a directory.
    ///
    /// # Panics
    ///
    /// If SQLite cannot open an in-memory database.
    #[must_use]
    pub fn fake_with(env: &Env, registry: Registry) -> World {
        let store = Store::open(None, &[]).expect("in-memory store");
        let mut caps = Capabilities::default();
        crate::caps::install(Mode::Fake, env, &mut caps);
        World::new(Rc::new(store), caps, registry)
    }

    #[must_use]
    pub fn store(&self) -> &Rc<Store> {
        &self.store
    }

    #[must_use]
    pub fn registry(&self) -> &Registry {
        &self.registry
    }

    /// The registry as a shared handle — what a log panel carries so it can
    /// turn a filed payload back into a sentence.
    #[must_use]
    pub fn registry_rc(&self) -> Rc<Registry> {
        self.registry.clone()
    }

    /// Unix seconds, from whichever clock this world has. Shorthand for
    /// `run(&Now)`, because it is on every hot path there is — and the one
    /// place the ring is deliberately skipped: the clock is asked several
    /// times a frame, and a ring of clock readings would have room for
    /// nothing a human meant to do. `run(&Now)` still records, for the
    /// caller who genuinely wants that noted.
    #[must_use]
    pub fn now(&self) -> f64 {
        self.with_cap::<dyn crate::caps::Clock, f64>(|c| c.now())
            .unwrap_or(0.0)
    }

    /// The UI-thread read a draw is allowed: a files panel lists its
    /// directory through [`Disk`](crate::caps::Disk).
    ///
    /// # Errors
    ///
    /// If this world has no such capability.
    pub fn with_cap<C: ?Sized + 'static, T>(
        &self,
        f: impl FnOnce(&mut C) -> T,
    ) -> Result<T, String> {
        let mut caps = self.caps.borrow_mut();
        match caps.get::<C>() {
            Some(c) => Ok(f(c)),
            None => Err(format!("this world has no {}", short_name::<C>())),
        }
    }

    /// The whole bag, for arranging a world (planting a secret) or reading
    /// what a fake captured.
    pub fn caps<T>(&self, f: impl FnOnce(&mut Capabilities) -> T) -> T {
        f(&mut self.caps.borrow_mut())
    }

    /// Performs an in-memory effect and swallows the failure, after saying
    /// so on stderr. For the ones a draw pass or a keystroke fires and has
    /// nowhere better to put an error.
    pub fn try_run<E: Effect>(&self, e: &E) {
        if let Err(err) = self.run(e) {
            eprintln!("effect: {} failed: {err}", e.describe());
        }
    }

    /// Performs an in-memory effect now and answers it. Nothing is written
    /// — these are the effects nobody would retry or wait for — but the
    /// ring keeps what it was and what it said, so the log can show it
    /// beside the queue. What the ring keeps is [`Effect::describe`], which
    /// never carries a secret; the payload stays where it was, which is
    /// nowhere.
    ///
    /// # Errors
    ///
    /// Whatever the capability said, verbatim.
    pub fn run<E: Effect>(&self, e: &E) -> Result<E::Reply, String> {
        // The seq is taken before the round trip, so the ring orders
        // effects by when they were *asked for*, as the queue's ids do.
        let seq = self.store.mem().next_seq();
        let (at, ran) = {
            let mut caps = self.caps.borrow_mut();
            let at = caps
                .get::<dyn crate::caps::Clock>()
                .map_or(0.0, |c| c.now());
            let mut cx = Ctx {
                caps: &mut caps,
                db: self.store.conn(),
            };
            (at, e.perform(&mut cx))
        };
        self.store.mem().record(MemEffect {
            seq,
            kind: E::KIND,
            entity: e.entity(),
            writes: e.writes(),
            what: e.describe(),
            error: ran.as_ref().err().cloned(),
            at,
        });
        // This reader's own pages go stale at once; other threads' notice
        // on their next poll, exactly as they do for a foreign commit.
        self.store.poll_mem();
        ran
    }

    /// Files a deferred effect inside the caller's transaction, so the job
    /// and whatever row references it land together. Answers the id.
    ///
    /// # Errors
    ///
    /// If the payload will not encode, or the insert fails.
    pub fn enqueue_in<E: Deferred>(&self, tx: &Transaction, e: &E) -> rusqlite::Result<i64>
    where
        E::Reply: Serialize + DeserializeOwned + Send,
    {
        self.enqueue_at_in(tx, e, 0.0)
    }

    /// The same, held back until `not_before` — a send window is exactly
    /// this, and it is why an effect needs no notion of time itself.
    ///
    /// # Errors
    ///
    /// If the payload will not encode, or the insert fails.
    pub fn enqueue_at_in<E: Deferred>(
        &self,
        tx: &Transaction,
        e: &E,
        not_before: f64,
    ) -> rusqlite::Result<i64>
    where
        E::Reply: Serialize + DeserializeOwned + Send,
    {
        self.prepare_at(e, not_before)?.insert(tx)
    }

    /// Encodes and timestamps a deferred effect into an owned [`Enqueue`],
    /// **outside** any transaction. This is the `Send`-safe half of filing a
    /// job. The caller can insert it from a writer-thread closure without
    /// moving `&World` across threads.
    ///
    /// # Errors
    ///
    /// If the payload will not encode.
    pub fn prepare<E: Deferred>(&self, e: &E) -> rusqlite::Result<Enqueue>
    where
        E::Reply: Serialize + DeserializeOwned + Send,
    {
        self.prepare_at(e, 0.0)
    }

    /// The same, held back until `not_before`.
    ///
    /// # Errors
    ///
    /// If the payload will not encode.
    pub fn prepare_at<E: Deferred>(&self, e: &E, not_before: f64) -> rusqlite::Result<Enqueue>
    where
        E::Reply: Serialize + DeserializeOwned + Send,
    {
        Ok(Enqueue {
            kind: E::KIND,
            payload: serde_json::to_string(e).map_err(json_err)?,
            entity: e.entity(),
            idempotent: e.idempotent(),
            writes: e.writes(),
            not_before,
            now: self.now(),
        })
    }

    /// Files a deferred effect in its own transaction.
    ///
    /// # Errors
    ///
    /// If the payload will not encode, or the insert fails.
    pub fn enqueue<E: Deferred>(&self, e: &E) -> rusqlite::Result<i64>
    where
        E::Reply: Serialize + DeserializeOwned + Send,
    {
        let spec = self.prepare(e)?;
        self.store.write(move |tx| spec.insert(tx))
    }

    /// Cancels a job that has not been claimed — undo's half of the race
    /// with the executor. Answers whether it won.
    ///
    /// # Errors
    ///
    /// If the update fails.
    pub fn cancel_in(&self, tx: &Transaction, id: i64) -> rusqlite::Result<bool> {
        cancel_tx(tx, id, self.now())
    }

    /// One executor pass over every due job, whoever it belongs to. Answers
    /// how many were claimed.
    pub fn run_effects(&self) -> usize {
        self.run_effects_where(|_| true)
    }

    /// One executor pass: claim every due job this pass is allowed to run
    /// and perform it. Answers how many were claimed.
    ///
    /// `claims` is [`Worker::claims`](crate::app::Worker::claims). A job may
    /// need something only one thread holds, such as a live session; the
    /// worker holding it claims the job and no other worker does, so it
    /// never burns an attempt on the wrong thread. The pass used to claim
    /// every due row, whoever ran it, and that is wrong the moment a job
    /// needs a thread's own state.
    pub fn run_effects_where(&self, claims: impl Fn(&Job) -> bool) -> usize {
        let now = self.now();
        let due: Vec<Job> = {
            let sql = format!(
                "SELECT {QUEUE_COLS} FROM effect e
                 WHERE e.status='pending' AND e.not_before <= ?1 ORDER BY e.id"
            );
            let Ok(mut stmt) = self.store.conn().prepare(&sql) else {
                return 0;
            };
            stmt.query_map(rusqlite::params![now], job_row)
                .map(|it| it.filter_map(Result::ok).filter(|j| claims(j)).collect())
                .unwrap_or_default()
        };

        let mut claimed = 0;
        for job in due {
            let (id, kind, payload) = (job.id, job.kind, job.payload);
            // The claim: one winner between this pass and a concurrent
            // undo, whose cancel only fires while the row is 'pending'.
            let won = self
                .store
                .write(move |tx| {
                    tx.execute(
                        "UPDATE effect SET status='processing', attempts=attempts+1,
                                           updated=?2
                         WHERE id=?1 AND status='pending'",
                        rusqlite::params![id, now],
                    )
                })
                .unwrap_or(0)
                == 1;
            if !won {
                continue;
            }
            claimed += 1;

            // Deliberately outside every transaction: this is the round trip.
            let ran = {
                let mut caps = self.caps.borrow_mut();
                let mut cx = Ctx {
                    caps: &mut caps,
                    db: self.store.conn(),
                };
                self.registry.run(&kind, &payload, &mut cx)
            };

            let closed = match ran {
                Ran::Done(reply, settle) => self.store.write(move |tx| {
                    settle(tx)?;
                    tx.execute(
                        "UPDATE effect SET status='done', reply=?2, error=NULL, updated=?3
                         WHERE id=?1",
                        rusqlite::params![id, reply, now],
                    )?;
                    Ok(())
                }),
                Ran::Obsolete => self.store.write(move |tx| {
                    tx.execute(
                        "UPDATE effect SET status='obsolete', updated=?2 WHERE id=?1",
                        rusqlite::params![id, now],
                    )?;
                    Ok(())
                }),
                Ran::NoHandler => self.fail(id, &format!("no handler for kind {kind}"), true),
                Ran::Failed(err) => self.fail(id, &err, false),
            };
            if let Err(e) = closed {
                eprintln!("effect: closing job {id} failed: {e}");
            }
        }
        claimed
    }

    /// Records a failure: retry with backoff while attempts remain, and
    /// give up (waiting for a human) once they do not. `terminal` skips
    /// straight to giving up — an unregistered kind will never succeed by
    /// being tried again.
    fn fail(&self, id: i64, err: &str, terminal: bool) -> rusqlite::Result<()> {
        let now = self.now();
        let err = err.to_string();
        self.store.write(move |tx| {
            let attempts: i64 = tx
                .query_row("SELECT attempts FROM effect WHERE id=?1", [id], |r| {
                    r.get(0)
                })
                .unwrap_or(MAX_ATTEMPTS);
            if terminal || attempts >= MAX_ATTEMPTS {
                tx.execute(
                    "UPDATE effect SET status='failed', error=?2, updated=?3 WHERE id=?1",
                    rusqlite::params![id, err, now],
                )?;
            } else {
                tx.execute(
                    "UPDATE effect SET status='pending', error=?2, not_before=?3, updated=?4
                     WHERE id=?1",
                    rusqlite::params![id, err, now + backoff(attempts), now],
                )?;
            }
            Ok(())
        })
    }

    // -- reading the table, through this world's store ----------------------

    #[must_use]
    pub fn jobs(&self) -> Vec<Job> {
        jobs(self.store.conn())
    }

    #[must_use]
    pub fn jobs_since(&self, id: i64) -> Vec<Job> {
        jobs_since(self.store.conn(), id)
    }

    #[must_use]
    pub fn jobs_of(&self, entity: &str) -> Vec<Job> {
        jobs_of(self.store.conn(), entity)
    }

    #[must_use]
    pub fn mark(&self) -> i64 {
        mark(self.store.conn())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::caps::{Clipboard, Clock, Disk, FakeClipboard, FakeClock};
    use crate::richtable::{Datasource, Table};
    use std::path::Path;

    /// A deferred effect that counts a number up, and can be told to fail.
    #[derive(Serialize, Deserialize)]
    struct Bump {
        row: i64,
        by: i64,
        fails: bool,
        /// Which worker may run it, in the `entity` vocabulary.
        owner: Option<String>,
    }

    impl Effect for Bump {
        const KIND: &'static str = "bump";
        type Reply = i64;
        fn describe(&self) -> String {
            format!("bump row {} by {}", self.row, self.by)
        }
        fn writes(&self) -> bool {
            true
        }
        fn entity(&self) -> Option<String> {
            self.owner.clone()
        }
        fn perform(&self, cx: &mut Ctx<'_>) -> Result<i64, String> {
            // It reaches the outside for its timestamp, so a missing
            // capability is a failure like any other.
            let _ = cx.cap::<dyn Clock>()?.now();
            if self.fails {
                return Err("the outside said no".into());
            }
            Ok(self.by)
        }
    }

    impl Deferred for Bump {
        fn idempotent(&self) -> bool {
            true
        }
        fn settle(&self, tx: &Transaction, reply: &i64) -> rusqlite::Result<()> {
            tx.execute(
                "INSERT INTO meta(key, value) VALUES(?1, ?2)
                 ON CONFLICT(key) DO UPDATE SET value = value + excluded.value",
                rusqlite::params![format!("row:{}", self.row), reply],
            )
            .map(|_| ())
        }
    }

    fn world() -> World {
        let mut reg = Registry::new();
        reg.register::<Bump>();
        World::fake(reg)
    }

    fn bump(row: i64, by: i64) -> Bump {
        Bump {
            row,
            by,
            fails: false,
            owner: None,
        }
    }

    fn counter(w: &World, row: i64) -> i64 {
        w.store()
            .conn()
            .query_row(
                "SELECT value FROM meta WHERE key = ?1",
                [format!("row:{row}")],
                |r| r.get(0),
            )
            .unwrap_or(0)
    }

    /// An in-memory effect runs at the call, answers, and leaves a line in
    /// the ring — never a row.
    #[test]
    fn an_immediate_effect_runs_and_is_kept_in_the_ring() {
        let w = world();
        assert!(w.store().mem().is_empty());
        let t = w.run(&crate::caps::Now).expect("the clock");
        assert_eq!(t, crate::time::virtual_epoch());
        assert_eq!(w.store().mem().len(), 1);
        assert!(w.jobs().is_empty(), "nothing was filed");

        // The ring shows up in the log beside the queue, as a negative id.
        let t = Table::new(&LOG, LOG_PAGE);
        assert_eq!(t.len(w.store()), 1);
        let row = t.row(w.store(), 0).expect("a row");
        assert!(row.transient());
        assert_eq!(row.kind, "now");
        assert_eq!(row.what.as_deref(), Some("read the clock"));
        assert!(!row.writes, "the clock only asked");
        assert_eq!(row.status_line(), "done · in memory");
    }

    /// A failure is kept too, with what it said.
    #[test]
    fn a_failed_effect_says_so_in_the_ring() {
        let w = World::fake(Registry::new());
        // A world without the capability the effect asks for.
        w.caps(|c| {
            c.remove::<dyn Clipboard>();
        });
        let e = w
            .run(&crate::caps::Clip {
                text: "hello",
                what: "the body",
            })
            .expect_err("no clipboard");
        assert_eq!(e, "this world has no Clipboard");
        let t = Table::new(&LOG, LOG_PAGE);
        let row = t.row(w.store(), 0).expect("a row");
        assert_eq!(row.status, "failed");
        assert_eq!(row.error.as_deref(), Some("this world has no Clipboard"));
    }

    /// The bag hands a fake back, so a test reads what was copied.
    #[test]
    fn a_capability_can_be_replaced_and_read_back() {
        let w = World::fake(Registry::new());
        let clip = FakeClipboard::new();
        w.caps(|c| c.insert::<dyn Clipboard>(Box::new(clip.clone())));
        w.run(&crate::caps::Clip {
            text: "hello",
            what: "the body",
        })
        .unwrap();
        assert_eq!(clip.last(), Some("hello".into()));

        // And the disk a world came with is the demo tree.
        let names = w
            .with_cap::<dyn Disk, _>(|d| d.list_dir(Path::new(&crate::caps::real_path("~"))))
            .expect("a disk")
            .expect("home lists");
        assert!(names.iter().any(|e| e.name == "Downloads"));
    }

    /// A deferred effect is a row first and a round trip second: the pass
    /// claims it, performs it, and settles its bookkeeping in the same
    /// transaction as the status.
    #[test]
    fn a_deferred_effect_is_filed_claimed_and_settled() {
        let w = world();
        let id = w.enqueue(&bump(1, 5)).expect("filed");
        assert_eq!(w.jobs().len(), 1);
        assert_eq!(w.jobs()[0].status, "pending");
        assert_eq!(counter(&w, 1), 0, "nothing has run yet");

        assert_eq!(w.run_effects(), 1);
        let job = w.jobs_since(id - 1).into_iter().next().expect("the job");
        assert_eq!(job.status, "done");
        assert_eq!(job.reply.as_deref(), Some("5"));
        assert_eq!(job.attempts, 1);
        assert_eq!(counter(&w, 1), 5, "the settle landed with the status");
        assert_eq!(w.run_effects(), 0, "and nothing is due twice");

        // The registry reads a filed payload back into a sentence.
        assert_eq!(
            w.registry().describe(&job.kind, &job.payload).as_deref(),
            Some("bump row 1 by 5")
        );
        assert_eq!(w.registry().kinds(), vec!["bump"]);
        assert_eq!(w.mark(), id);
        assert_eq!(w.jobs_since(id).len(), 0);
    }

    /// A failure backs off and retries; past [`MAX_ATTEMPTS`] it gives up
    /// and waits for a human.
    #[test]
    fn a_failure_backs_off_and_finally_gives_up() {
        let w = world();
        let clock = FakeClock::default();
        w.caps(|c| c.insert::<dyn Clock>(Box::new(clock.clone())));
        w.enqueue(&Bump {
            row: 2,
            by: 1,
            fails: true,
            owner: None,
        })
        .unwrap();

        for attempt in 1..MAX_ATTEMPTS {
            assert_eq!(w.run_effects(), 1, "attempt {attempt}");
            let job = &w.jobs()[0];
            assert_eq!(job.status, "pending");
            assert_eq!(job.attempts, attempt);
            assert_eq!(job.error.as_deref(), Some("the outside said no"));
            // Held back until its window: a second pass now claims nothing.
            assert_eq!(w.run_effects(), 0, "backed off after {attempt}");
            clock.advance(700.0);
        }
        assert_eq!(w.run_effects(), 1, "the last attempt");
        let job = &w.jobs()[0];
        assert_eq!(job.status, "failed");
        assert_eq!(job.attempts, MAX_ATTEMPTS);
        assert_eq!(w.run_effects(), 0, "a job that gave up is not due");
    }

    /// A kind nobody registered fails at once rather than sitting pending
    /// forever.
    #[test]
    fn an_unregistered_kind_fails_loudly() {
        let w = World::fake(Registry::new());
        w.store()
            .write(|tx| {
                tx.execute(
                    "INSERT INTO effect(kind, payload, status, idempotent, created, updated)
                     VALUES('nobodys', '{}', 'pending', 1, 0, 0)",
                    [],
                )
                .map(|_| ())
            })
            .unwrap();
        assert_eq!(w.run_effects(), 1);
        let job = &w.jobs()[0];
        assert_eq!(job.status, "failed");
        assert_eq!(job.error.as_deref(), Some("no handler for kind nobodys"));
    }

    /// A pass claims only what it says it can run: a job filed against
    /// another worker's entity is left where it is.
    #[test]
    fn a_pass_claims_only_what_it_asks_for() {
        let w = world();
        w.enqueue(&Bump {
            row: 3,
            by: 1,
            fails: false,
            owner: Some("account:1".into()),
        })
        .unwrap();
        w.enqueue(&Bump {
            row: 4,
            by: 1,
            fails: false,
            owner: Some("account:2".into()),
        })
        .unwrap();
        w.enqueue(&bump(5, 1)).unwrap();

        let mine = |j: &Job| j.entity.as_deref() == Some("account:1");
        assert_eq!(w.run_effects_where(mine), 1);
        assert_eq!(counter(&w, 3), 1);
        assert_eq!(counter(&w, 4), 0, "another worker's job stayed pending");

        // The sessionless pass: anything with no owner.
        assert_eq!(w.run_effects_where(|j| j.entity.is_none()), 1);
        assert_eq!(counter(&w, 5), 1);
        assert_eq!(w.jobs_of("account:2")[0].status, "pending");
        assert_eq!(w.run_effects(), 1, "and everything else on the next pass");
    }

    /// A held-back job waits for its window; time moving is what releases it.
    #[test]
    fn a_window_holds_a_job_back() {
        let w = world();
        let clock = FakeClock::at(1_000.0);
        w.caps(|c| c.insert::<dyn Clock>(Box::new(clock.clone())));
        let spec = w.prepare_at(&bump(6, 2), 1_060.0).unwrap();
        w.store().write(move |tx| spec.insert(tx)).unwrap();
        assert_eq!(w.run_effects(), 0, "not yet");
        clock.set(1_100.0);
        assert_eq!(w.run_effects(), 1);
        assert_eq!(counter(&w, 6), 2);
    }

    /// Undo's half of the race: a pending job cancels, a claimed one does
    /// not.
    #[test]
    fn a_pending_job_can_be_cancelled() {
        let w = world();
        let id = w.enqueue(&bump(7, 1)).unwrap();
        let won = w.store().write(move |tx| cancel_tx(tx, id, 0.0)).unwrap();
        assert!(won);
        assert_eq!(w.jobs()[0].status, "obsolete");
        assert_eq!(w.run_effects(), 0);
        let again = w.store().write(move |tx| cancel_tx(tx, id, 0.0)).unwrap();
        assert!(!again, "only once");
    }

    /// Everything a world does can be filed against an entity, and read
    /// back by it.
    #[test]
    fn jobs_are_readable_by_entity() {
        let w = world();
        w.enqueue(&Bump {
            row: 8,
            by: 1,
            fails: false,
            owner: Some("outbox:9".into()),
        })
        .unwrap();
        w.enqueue(&bump(9, 1)).unwrap();
        assert_eq!(w.jobs_of("outbox:9").len(), 1);
        assert_eq!(w.jobs_of("nothing").len(), 0);
        assert_eq!(w.jobs().len(), 2);
    }

    /// The log is a rich table over both streams: the filter grammar sorts
    /// a ring row from a filed one, and `job()` opens either.
    #[test]
    fn the_log_filters_over_the_queue_and_the_ring() {
        let w = world();
        w.enqueue(&bump(10, 1)).unwrap();
        w.run(&crate::caps::Now).unwrap();
        w.try_run(&crate::caps::Now);

        let mut t = Table::new(&LOG, LOG_PAGE);
        assert_eq!(t.len(w.store()), 3);
        t.set_filter("@memory");
        assert_eq!(t.len(w.store()), 2);
        t.set_filter("@filed");
        assert_eq!(t.len(w.store()), 1);
        t.set_filter("@wrote");
        assert_eq!(t.len(w.store()), 1, "the clock only reads");
        t.set_filter("@read");
        assert_eq!(t.len(w.store()), 2);
        t.set_filter("@kind:bump");
        assert_eq!(t.len(w.store()), 1);
        t.set_filter("@risky");
        assert_eq!(t.len(w.store()), 0, "a ring row is never swept in");

        // The dynamic tag's values come off the log itself.
        let kinds: Vec<String> = LOG
            .suggest(w.store(), "kind", "")
            .into_iter()
            .map(|s| s.value)
            .collect();
        assert_eq!(kinds, vec!["bump".to_string(), "now".to_string()]);

        // And one row of it opens by id, either stream.
        let filed = w.jobs()[0].id;
        assert_eq!(job(w.store(), filed).map(|j| j.kind), Some("bump".into()));
        assert_eq!(job(w.store(), -1).map(|j| j.kind), Some("now".into()));
        assert_eq!(job(w.store(), 999), None);
    }

    /// The ring is bounded, and the oldest fall off it.
    #[test]
    fn the_ring_is_bounded() {
        let w = World::fake(Registry::new());
        for _ in 0..(KEPT + 10) {
            w.run(&crate::caps::Now).unwrap();
        }
        assert_eq!(w.store().mem().len(), KEPT);
        let t = Table::new(&LOG, LOG_PAGE);
        assert_eq!(t.len(w.store()), KEPT);
    }

    /// The ring is one per process, so a second world over the same store
    /// sees what the first reached for.
    #[test]
    fn the_ring_is_shared_by_every_world_over_one_store() {
        let a = World::fake(Registry::new());
        let b = World::new(
            Rc::new(Store::with_db(a.store().db()).unwrap()),
            Capabilities::default(),
            Registry::new(),
        );
        a.run(&crate::caps::Now).unwrap();
        b.store().poll_mem();
        let t = Table::new(&LOG, LOG_PAGE);
        assert_eq!(t.len(b.store()), 1, "b's reader sees a's ring");
    }
}
