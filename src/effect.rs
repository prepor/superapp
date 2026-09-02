//! Effects: everything that leaves the process (CR-004).
//!
//! The rule, and the whole of it:
//!
//! > **An effect is anything whose result the store cannot reproduce.**
//!
//! The store is the app's memory — replaying its transactions replays the
//! app — so SQLite is emphatically *not* an effect. A socket, the keychain,
//! the clipboard, a file beside the store, the screen and the clock are.
//! Archiving a mail is a plain store write (intent); *pushing* that archive
//! to the server is an effect.
//!
//! Every effect is a serializable value that describes itself in one line
//! ([`Effect`]). Effects worth retrying are also [`Deferred`]: they become
//! rows in the one `effect` table, claimed and performed by one executor,
//! with a status and a reply anyone can read — including the panel that
//! asked for them, through the ordinary reactive query layer.
//!
//! Two classes, told apart by one question — *would anyone retry it, wait
//! for it, or want to see that it failed?*
//!
//! | | examples | how it runs |
//! |---|---|---|
//! | deferred | [`Move`], [`Seen`], [`Submit`] | enqueued, claimed, executed by the pass |
//! | in-memory | [`Now`], [`Connect`], [`Fetch`], [`SecretGet`] | performed at the call, answer returned, nothing written |
//!
//! [`Outside`] is the swappable backend — [`Real`], [`Fake`], [`Deny`] —
//! and it owns the clock too, so a fake world controls time the same way it
//! controls everything else.

use std::cell::RefCell;
use std::sync::{Arc, Mutex};
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::rc::Rc;

use rusqlite::{Connection, Transaction};
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};

use crate::filter::Op;
use crate::richtable::{Dir, SqlSource, SqlSpec, Suggestion, TagDef, TagSql, TagType, Values};
use crate::store::{Store, Val};

// -- what the outside answers with --------------------------------------------

/// A folder as the server lists it.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RemoteFolder {
    pub name: String,
    /// inbox | archive | sent | trash — `None` folders are not mirrored.
    pub role: Option<String>,
}

/// SELECT results.
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub struct FolderMeta {
    pub uidvalidity: u32,
    pub uidnext: u32,
    /// Whether the folder keeps keywords such as `$Forwarded` — its
    /// `PERMANENTFLAGS` name the keyword or allow any (`\*`). A server
    /// that says otherwise, or says nothing, may accept a `STORE` and
    /// forget the flag by the next session, so a mark is neither pushed
    /// to nor read from one; it stays local there.
    pub keywords: bool,
}

/// One fetched message.
#[derive(Debug, Clone)]
pub struct RemoteMail {
    pub uid: u32,
    pub unread: bool,
    /// The `$Forwarded` keyword — set by this app or by another client.
    pub forwarded: bool,
    pub raw: Vec<u8>,
}

/// Which of a folder's uids to list.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UidSet {
    All,
    /// Without `\Seen`.
    Unseen,
    /// With the `$Forwarded` keyword.
    Forwarded,
}

/// A per-message flag the app keeps on both sides of the desired/actual
/// split, and pushes when they disagree.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MailFlag {
    /// `\Seen`.
    Seen,
    /// `$Forwarded` — the keyword every client's forwarded arrow reads.
    Forwarded,
}

/// A mail on its way out.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Outgoing {
    pub to: String,
    pub subject: String,
    pub body: String,
    /// The Message-ID this replies to, for threading headers.
    pub in_reply_to: Option<String>,
    /// What the mail replied to itself referenced, so `References` carries
    /// the whole chain (RFC 5322) and a reply to a reply threads for the
    /// other side too. Absent on payloads filed before CR-007.
    #[serde(default)]
    pub references: Vec<String>,
}

/// How to reach a server. `Debug` redacts the password so a stray `{:?}`
/// cannot leak one, and no [`Effect::describe`] ever prints it — `describe`
/// is what lands in the table.
#[derive(Clone)]
pub struct Creds {
    pub host: String,
    pub user: String,
    pub pass: String,
}

impl std::fmt::Debug for Creds {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Creds")
            .field("host", &self.host)
            .field("user", &self.user)
            .field("pass", &"…")
            .finish()
    }
}

// -- the backend ---------------------------------------------------------------

/// The verbs the outside world understands. Object-safe on purpose: this is
/// the axis where the compiler still tells you a backend forgot a case.
///
/// Errors are strings — they land on a status line, for a human.
pub trait Outside {
    /// Unix seconds. The clock is an effect like any other, which is why
    /// there is no separate `Clock` type.
    fn now(&mut self) -> f64;
    /// Opens (or replaces) this account's mail session.
    fn connect(&mut self, account: i64, c: &Creds) -> Result<(), String>;
    fn folders(&mut self, account: i64) -> Result<Vec<RemoteFolder>, String>;
    fn folder_meta(&mut self, account: i64, folder: &str) -> Result<FolderMeta, String>;
    /// Messages with `uid >= from`, ascending.
    fn fetch(&mut self, account: i64, folder: &str, from: u32)
        -> Result<Vec<RemoteMail>, String>;
    /// The uids in the folder: every one, or those with a flag.
    fn uids(&mut self, account: i64, folder: &str, which: UidSet)
        -> Result<HashSet<u32>, String>;
    /// `UID MOVE`; the new uid when the server says (UIDPLUS' COPYUID),
    /// `None` otherwise — adoption by Message-ID covers that.
    fn move_uid(&mut self, account: i64, from: &str, to: &str, uid: u32)
        -> Result<Option<u32>, String>;
    /// `UID STORE` a flag on or off.
    fn store_flag(&mut self, account: i64, folder: &str, uid: u32, flag: MailFlag, on: bool)
        -> Result<(), String>;
    /// `APPEND` raw bytes (filing sent mail).
    fn append(&mut self, account: i64, folder: &str, raw: &[u8]) -> Result<(), String>;
    /// SMTP submission; answers the formatted RFC 822 bytes.
    fn submit(&mut self, c: &Creds, m: &Outgoing) -> Result<Vec<u8>, String>;

    fn secret_get(&mut self, email: &str) -> Option<String>;
    fn secret_set(&mut self, email: &str, pass: &str) -> bool;
    fn clip(&mut self, text: &str) -> Result<(), String>;
    fn write_file(&mut self, path: &Path, bytes: &[u8]) -> Result<(), String>;
    fn shot(&mut self, path: &Path) -> Result<(), String>;

    /// Reach the concrete backend — how a test arranges a [`Fake`] world.
    fn as_any(&mut self) -> &mut dyn std::any::Any;
}

/// What an effect is performed against: the outside, plus read-only store
/// access so a payload can reference a row instead of embedding its
/// contents. No transaction is ever open here — that is the point.
pub struct Ctx<'a> {
    pub out: &'a mut dyn Outside,
    pub db: &'a Connection,
}

// -- the traits ----------------------------------------------------------------

/// Something that leaves the process.
///
/// Deliberately **not** `Serialize`: an in-memory effect is performed at the
/// call and written nowhere, so making it serializable would be a lie — and
/// a dangerous one, since [`Connect`] carries a password. Serializability
/// belongs to [`Deferred`], where a row actually exists.
pub trait Effect: Sized {
    /// Stable, greppable, the table's `kind`.
    const KIND: &'static str;
    /// What this call answers.
    type Reply;
    /// One line of English — the row's description, the label in a status
    /// UI, and what an assertion failure prints. Never carries a secret.
    fn describe(&self) -> String;
    /// Do it.
    fn perform(&self, cx: &mut Ctx<'_>) -> Result<Self::Reply, String>;
}

/// An effect worth persisting: queued, retried, its status and reply
/// readable from the table. Both the effect and its reply must survive a
/// round trip through JSON, so an effect that cannot be written down is a
/// compile error rather than a discovery.
// `Send` is required because a job's `settle` closure travels to the store's
// writer thread (CR-005 phase 0): the effect value and its reply are captured
// and committed there. Every real effect is plain data, so this is free.
pub trait Deferred: Effect + Serialize + DeserializeOwned + Send + 'static
where
    Self::Reply: Serialize + DeserializeOwned + Send,
{
    /// Is running this twice safe? No default — it is the one judgement a
    /// crash cannot guess, and it drives the boot sweep.
    fn idempotent(&self) -> bool;

    /// What this job belongs to, in the `action.entity` vocabulary —
    /// `account:2`, `panel:7`. Lets a panel query its own effects.
    fn entity(&self) -> Option<String> {
        None
    }

    /// Does the world still want this? Checked after the claim and before
    /// the round trip: if undo landed while the job sat in the queue, it
    /// goes `obsolete` instead of performing stale work.
    fn still_wanted(&self, _db: &Connection) -> bool {
        true
    }

    /// What the success establishes — runs in the **same transaction** as
    /// the status update, so "the effect happened" and "the world now looks
    /// like this" land together or not at all.
    fn settle(&self, _tx: &Transaction, _reply: &Self::Reply) -> rusqlite::Result<()> {
        Ok(())
    }
}

// -- the app's own in-memory effects -------------------------------------------
//
// Not `Deferred`: nobody retries a clipboard write or waits on a row for the
// time. They exist so that *everything* leaving the process goes through one
// door, and so a `Deny` world can refuse them.

/// What time it is. An effect like any other, which is why there is no
/// separate `Clock` type — a fake world moves it like it moves anything.
pub struct Now;

impl Effect for Now {
    const KIND: &'static str = "now";
    type Reply = f64;
    fn describe(&self) -> String {
        "read the clock".into()
    }
    fn perform(&self, cx: &mut Ctx<'_>) -> Result<f64, String> {
        Ok(cx.out.now())
    }
}

/// Recall an account's password.
pub struct SecretGet<'a>(pub &'a str);

impl Effect for SecretGet<'_> {
    const KIND: &'static str = "secret_get";
    type Reply = Option<String>;
    fn describe(&self) -> String {
        format!("read the password for {}", self.0)
    }
    fn perform(&self, cx: &mut Ctx<'_>) -> Result<Self::Reply, String> {
        Ok(cx.out.secret_get(self.0))
    }
}

/// Store an account's password. Never persisted, for the obvious reason.
pub struct SecretSet<'a> {
    pub email: &'a str,
    pub pass: &'a str,
}

impl Effect for SecretSet<'_> {
    const KIND: &'static str = "secret_set";
    type Reply = ();
    fn describe(&self) -> String {
        format!("store the password for {}", self.email)
    }
    fn perform(&self, cx: &mut Ctx<'_>) -> Result<(), String> {
        cx.out
            .secret_set(self.email, self.pass)
            .then_some(())
            .ok_or_else(|| "the keychain refused the password".to_string())
    }
}

/// Put text on the system clipboard.
pub struct Clip<'a> {
    pub text: &'a str,
    /// What the text is, for the description — the text itself may be long.
    pub what: &'static str,
}

impl Effect for Clip<'_> {
    const KIND: &'static str = "clip";
    type Reply = ();
    fn describe(&self) -> String {
        format!("copy {} ({} bytes)", self.what, self.text.len())
    }
    fn perform(&self, cx: &mut Ctx<'_>) -> Result<(), String> {
        cx.out.clip(self.text)
    }
}

/// Write a file outside the store.
pub struct WriteFile<'a> {
    pub path: &'a Path,
    pub bytes: &'a [u8],
}

impl Effect for WriteFile<'_> {
    const KIND: &'static str = "write_file";
    type Reply = ();
    fn describe(&self) -> String {
        format!("write {} ({} bytes)", self.path.display(), self.bytes.len())
    }
    fn perform(&self, cx: &mut Ctx<'_>) -> Result<(), String> {
        cx.out.write_file(self.path, self.bytes)
    }
}

/// Photograph the window (e2e).
pub struct Shot<'a>(pub &'a Path);

impl Effect for Shot<'_> {
    const KIND: &'static str = "shot";
    type Reply = ();
    fn describe(&self) -> String {
        format!("capture {}", self.0.display())
    }
    fn perform(&self, cx: &mut Ctx<'_>) -> Result<(), String> {
        cx.out.shot(self.0)
    }
}

// -- the registry --------------------------------------------------------------

/// The bookkeeping a success carries, committed with its status update.
/// `Send`, because it is committed on the store's writer thread (CR-005).
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

/// Decode-and-perform, per kind. Each domain registers its own effects, so
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
                serde_json::from_str::<E>(payload).ok().map(|e| e.describe())
            }),
        );
    }

    /// One line of English for a filed job: the effect decoded from its
    /// payload and asked to describe itself. `None` when this build cannot
    /// read the kind — an unregistered domain, or a row an older version
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

    /// Every registered kind — the completeness test reads this.
    #[must_use]
    pub fn kinds(&self) -> Vec<&'static str> {
        let mut v: Vec<&'static str> = self.handlers.keys().copied().collect();
        v.sort_unstable();
        v
    }
}

// -- the log -------------------------------------------------------------------

/// One row of the effect table, as tests and the log viewer read it. The
/// whole row, payload included: this is the only shape the queue is ever
/// read in, and a viewer that showed less than `sqlite3` does would defeat
/// the reason the queue lives in the store at all.
#[derive(Debug, Clone, PartialEq)]
pub struct Job {
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
    /// time so the crash sweep never has to decode a payload.
    pub idempotent: bool,
    /// Filed at, last touched at, and the earliest the executor may claim
    /// it (a backoff, or the send window) — unix seconds, the world's clock.
    pub created: f64,
    pub updated: f64,
    pub not_before: f64,
}

impl Job {
    /// The status as the log reads it aloud: the word, and — once a job has
    /// been tried more than once — how many times. A count on every row
    /// would be noise; a count on the rows that fought is the whole story.
    #[must_use]
    pub fn status_line(&self) -> String {
        if self.attempts > 1 {
            format!("{} · {} tries", self.status, self.attempts)
        } else {
            self.status.clone()
        }
    }
}

/// How long a failed job waits before its next attempt, by attempt count —
/// capped, because a mail server that is down stays down for a while.
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
        idempotent: r.get::<_, i64>(8)? != 0,
        created: r.get(9)?,
        updated: r.get(10)?,
        not_before: r.get(11)?,
    })
}

/// The one column list, shared by the helpers below and by [`LOG_SPEC`] —
/// so the table the log viewer pages through and the rows a test asserts on
/// decode through the same [`job_row`], in the same order. Qualified,
/// because the spec's `FROM` aliases the table.
const JOB_COLS: &str = "e.id, e.kind, e.entity, e.status, e.reply, e.error, e.attempts,
                        e.payload, e.idempotent, e.created, e.updated, e.not_before";

/// Every job, oldest first.
pub fn jobs(db: &Connection) -> Vec<Job> {
    let Ok(mut stmt) = db.prepare(&format!("SELECT {JOB_COLS} FROM effect e ORDER BY e.id")) else {
        return Vec::new();
    };
    stmt.query_map([], job_row)
        .map(|it| it.filter_map(Result::ok).collect())
        .unwrap_or_default()
}

/// Jobs after `id` — how a test marks a point and asserts on what followed.
pub fn jobs_since(db: &Connection, id: i64) -> Vec<Job> {
    let Ok(mut stmt) = db.prepare(&format!(
        "SELECT {JOB_COLS} FROM effect e WHERE e.id > ?1 ORDER BY e.id"
    )) else {
        return Vec::new();
    };
    stmt.query_map([id], job_row)
        .map(|it| it.filter_map(Result::ok).collect())
        .unwrap_or_default()
}

/// One entity's jobs — what a panel shows about its own in-flight work.
pub fn jobs_of(db: &Connection, entity: &str) -> Vec<Job> {
    let Ok(mut stmt) = db.prepare(&format!(
        "SELECT {JOB_COLS} FROM effect e WHERE e.entity = ?1 ORDER BY e.id"
    )) else {
        return Vec::new();
    };
    stmt.query_map([entity], job_row)
        .map(|it| it.filter_map(Result::ok).collect())
        .unwrap_or_default()
}

/// The newest job id — the mark for [`jobs_since`].
pub fn mark(db: &Connection) -> i64 {
    db.query_row("SELECT COALESCE(MAX(id), 0) FROM effect", [], |r| r.get(0))
        .unwrap_or(0)
}

// -- the log as a rich table ---------------------------------------------------
//
// The queue is a table like any other, so the log viewer is the rich table
// (CR-006) over it rather than a widget of its own invention: the same
// filter grammar, the same paging, the same reactive pages — a commit by
// the executor invalidates exactly the pages on screen, so watching a job
// run is invalidation and not polling.

/// The statuses a row can be in, as the filter offers them.
const STATUSES: &[(&str, &str)] = &[
    ("pending", "pending"),
    ("processing", "processing"),
    ("done", "done"),
    ("failed", "failed"),
    ("obsolete", "obsolete"),
];

/// The effect log's fixed query: every job, newest first. Flat — a job is a
/// row, and nothing about it is an aggregate.
static LOG_SPEC: SqlSpec = SqlSpec {
    id: "effect log",
    describe: "the effect queue under the panel's filter, newest first, one page at a time",
    select: JOB_COLS,
    from: "effect e",
    base: "",
    // Bare words search what a human would type: the verb, whose it was,
    // and what went wrong. The payload too — that is where a uid or an
    // address actually lives.
    text: &["e.kind", "e.entity", "e.payload", "e.error"],
    tags: &[
        ("failed", TagSql::Where("e.status = 'failed'")),
        (
            "live",
            TagSql::Where("e.status IN ('pending', 'processing')"),
        ),
        ("retried", TagSql::Where("e.attempts > 1")),
        ("risky", TagSql::Where("e.idempotent = 0")),
        ("status", TagSql::Col("e.status")),
        ("kind", TagSql::Col("e.kind")),
        ("entity", TagSql::Col("e.entity")),
        ("attempts", TagSql::Col("e.attempts")),
        ("date", TagSql::Col("e.created")),
    ],
    // Total by construction: the id is unique, and it is also the order the
    // queue was filed in.
    order: &[("e.id", Dir::Desc)],
    group: None,
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
        describe: "the effect's verb — move, seen, submit",
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
/// read off the queue itself rather than off the registry: what is *in* the
/// table is what filtering it can find, and a kind this build no longer
/// registers is exactly the row a human goes looking for.
fn suggest_log(store: &Store, tag: &str, typed: &str) -> Vec<Suggestion> {
    let col = match tag {
        "kind" => "kind",
        "entity" => "entity",
        _ => return Vec::new(),
    };
    let sql = format!(
        "SELECT DISTINCT {col} FROM effect
          WHERE {col} IS NOT NULL AND {col} != '' ORDER BY {col}"
    );
    store
        .rows_sql("effect log values", "the distinct values one effect-log tag takes", &sql, &[], |r| {
            r.get::<_, String>(0)
        })
        .iter()
        .filter(|v| v.to_lowercase().contains(typed))
        .map(Suggestion::value)
        .collect()
}

/// The effect log's datasource: what the log panel's rich table runs on.
pub static LOG: SqlSource<Job> = SqlSource {
    spec: &LOG_SPEC,
    tags: LOG_TAGS,
    map: job_row,
    key: |j| vec![Val::I(j.id)],
    suggest: suggest_log,
};

/// Rows per page of the log table.
pub const LOG_PAGE: usize = 50;

// -- the world -----------------------------------------------------------------

fn json_err(e: serde_json::Error) -> rusqlite::Error {
    rusqlite::Error::ToSqlConversionFailure(Box::new(e))
}

/// A deferred effect encoded and timestamped, ready to insert inside any
/// write transaction. Owned and `Send`, so it can be moved into a
/// [`Store::write`](crate::store::Store::write) closure that runs on the
/// writer thread — the composition primitive the passes build their jobs
/// from (CR-005 phase 0).
pub struct Enqueue {
    kind: &'static str,
    payload: String,
    entity: Option<String>,
    idempotent: bool,
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
            "INSERT INTO effect(kind, payload, entity, status, idempotent,
                                attempts, not_before, created, updated)
             VALUES(?1, ?2, ?3, 'pending', ?4, 0, ?5, ?6, ?6)",
            rusqlite::params![
                self.kind,
                self.payload,
                self.entity,
                self.idempotent,
                self.not_before,
                self.now
            ],
        )?;
        Ok(tx.last_insert_rowid())
    }
}

/// Cancels an unclaimed job inside the caller's transaction — undo's half of
/// the race with the executor, as a free function so a `Send` write closure
/// can call it without capturing the `World`.
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

/// The store, the outside and the registry, as one value you construct —
/// never a global, never a path, never a thread you cannot see.
/// Single-threaded: the UI owns one, and each worker thread builds its own.
pub struct World {
    store: Rc<Store>,
    outside: RefCell<Box<dyn Outside>>,
    /// Shared, so a panel can hold one and name what it is looking at
    /// ([`Registry::describe`]) — and no more than that: performing an
    /// effect needs an [`Outside`], which stays behind this world.
    registry: Rc<Registry>,
}

impl World {
    #[must_use]
    pub fn new(store: Rc<Store>, outside: Box<dyn Outside>, registry: Registry) -> World {
        World {
            store,
            outside: RefCell::new(outside),
            registry: Rc::new(registry),
        }
    }

    /// An isolated world: its own in-memory store, a [`Fake`] outside and a
    /// clock that only moves when a test moves it. Touches nothing beyond
    /// itself, so any number run in parallel.
    ///
    /// # Panics
    ///
    /// If SQLite cannot open an in-memory database.
    #[must_use]
    pub fn fake(registry: Registry) -> World {
        let store = Store::open(None).expect("in-memory store");
        World::new(Rc::new(store), Box::<Fake>::default(), registry)
    }

    #[must_use]
    pub fn store(&self) -> &Rc<Store> {
        &self.store
    }

    #[must_use]
    pub fn registry(&self) -> &Registry {
        &self.registry
    }

    /// The registry as a shared handle — what the log panel carries so it
    /// can turn a filed payload back into a sentence.
    #[must_use]
    pub fn registry_rc(&self) -> Rc<Registry> {
        self.registry.clone()
    }

    /// Unix seconds, from whichever backend this world has. Shorthand for
    /// `run(&Now)`, because it is on every hot path there is.
    #[must_use]
    pub fn now(&self) -> f64 {
        self.outside.borrow_mut().now()
    }

    /// Performs an in-memory effect and swallows the failure, after saying
    /// so on stderr. For the ones a draw pass or a keystroke fires and has
    /// nowhere better to put an error.
    pub fn try_run<E: Effect>(&self, e: &E) {
        if let Err(err) = self.run(e) {
            eprintln!("effect: {} failed: {err}", e.describe());
        }
    }

    /// The backend, for arranging a world (deliver a mail, plant a
    /// password) or reading what it captured.
    pub fn outside<T>(&self, f: impl FnOnce(&mut dyn Outside) -> T) -> T {
        f(&mut **self.outside.borrow_mut())
    }

    /// The backend as a [`Fake`]. Panics if this world is not fake — which
    /// is what a test wants, and replaces the unsafe downcast the escape
    /// hatch used to need.
    ///
    /// # Panics
    ///
    /// If the backend is not a [`Fake`].
    pub fn with_fake<T>(&self, f: impl FnOnce(&mut Fake) -> T) -> T {
        self.outside(|o| {
            f(o.as_any()
                .downcast_mut::<Fake>()
                .expect("this world's outside is not a Fake"))
        })
    }

    /// Performs an in-memory effect now and answers it. Nothing is written:
    /// these are the effects nobody would retry or wait for.
    ///
    /// # Errors
    ///
    /// Whatever the backend said, verbatim.
    pub fn run<E: Effect>(&self, e: &E) -> Result<E::Reply, String> {
        let mut out = self.outside.borrow_mut();
        let mut cx = Ctx {
            out: &mut **out,
            db: self.store.conn(),
        };
        e.perform(&mut cx)
    }

    /// Files a deferred effect inside the caller's transaction, so the job
    /// and whatever domain row references it land together. Answers the id.
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

    /// The same, held back until `not_before` — the send window is exactly
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
        let payload = serde_json::to_string(e).map_err(json_err)?;
        let now = self.now();
        tx.execute(
            "INSERT INTO effect(kind, payload, entity, status, idempotent,
                                attempts, not_before, created, updated)
             VALUES(?1, ?2, ?3, 'pending', ?4, 0, ?5, ?6, ?6)",
            rusqlite::params![E::KIND, payload, e.entity(), e.idempotent(), not_before, now],
        )?;
        Ok(tx.last_insert_rowid())
    }

    /// Encodes and timestamps a deferred effect into an owned [`Enqueue`],
    /// **outside** any transaction. This is the `Send`-safe half of filing a
    /// job: the caller can then insert it inside a write closure that runs on
    /// the store's writer thread, where the `&World` itself cannot travel
    /// (CR-005 phase 0).
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

    /// One executor pass: claim every due job and run it. Answers how many
    /// were claimed.
    pub fn run_effects(&self) -> usize {
        let now = self.now();
        let due: Vec<(i64, String, String)> = {
            let Ok(mut stmt) = self.store.conn().prepare(
                "SELECT id, kind, payload FROM effect
                 WHERE status='pending' AND not_before <= ?1 ORDER BY id",
            ) else {
                return 0;
            };
            stmt.query_map([now], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))
                .map(|it| it.filter_map(Result::ok).collect())
                .unwrap_or_default()
        };

        let mut claimed = 0;
        for (id, kind, payload) in due {
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
                let mut out = self.outside.borrow_mut();
                let mut cx = Ctx {
                    out: &mut **out,
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
                .query_row("SELECT attempts FROM effect WHERE id=?1", [id], |r| r.get(0))
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

// -- Deny ----------------------------------------------------------------------

/// A world that refuses. The default for a components-library mount: a
/// panel that quietly sends mail while you look at it fails loudly instead
/// of succeeding invisibly.
#[derive(Default)]
pub struct Deny {
    /// The one thing a sealed world still answers: what time it is. `None`
    /// reads as the epoch, which is what the tests expect.
    clock: Option<Clock>,
}

impl Deny {
    /// A sealed world on this clock — a panels-library mount, whose springs
    /// and deadlines have to move with its frame loop.
    #[must_use]
    pub fn with_clock(clock: Clock) -> Deny {
        Deny { clock: Some(clock) }
    }

    fn no<T>(what: &str) -> Result<T, String> {
        Err(format!("this world has no outside ({what})"))
    }
}

impl Outside for Deny {
    fn now(&mut self) -> f64 {
        self.clock.as_ref().map_or(0.0, Clock::read)
    }
    fn connect(&mut self, _a: i64, _c: &Creds) -> Result<(), String> {
        Self::no("connect")
    }
    fn folders(&mut self, _a: i64) -> Result<Vec<RemoteFolder>, String> {
        Self::no("folders")
    }
    fn folder_meta(&mut self, _a: i64, _f: &str) -> Result<FolderMeta, String> {
        Self::no("folder_meta")
    }
    fn fetch(&mut self, _a: i64, _f: &str, _u: u32) -> Result<Vec<RemoteMail>, String> {
        Self::no("fetch")
    }
    fn uids(&mut self, _a: i64, _f: &str, _w: UidSet) -> Result<HashSet<u32>, String> {
        Self::no("uids")
    }
    fn move_uid(&mut self, _a: i64, _f: &str, _t: &str, _u: u32) -> Result<Option<u32>, String> {
        Self::no("move")
    }
    fn store_flag(&mut self, _a: i64, _f: &str, _u: u32, _fl: MailFlag, _on: bool)
        -> Result<(), String>
    {
        Self::no("flag")
    }
    fn append(&mut self, _a: i64, _f: &str, _r: &[u8]) -> Result<(), String> {
        Self::no("append")
    }
    fn submit(&mut self, _c: &Creds, _m: &Outgoing) -> Result<Vec<u8>, String> {
        Self::no("submit")
    }
    fn secret_get(&mut self, _e: &str) -> Option<String> {
        None
    }
    fn secret_set(&mut self, _e: &str, _p: &str) -> bool {
        false
    }
    fn clip(&mut self, _t: &str) -> Result<(), String> {
        Self::no("clip")
    }
    fn write_file(&mut self, _p: &Path, _b: &[u8]) -> Result<(), String> {
        Self::no("write_file")
    }
    fn shot(&mut self, _p: &Path) -> Result<(), String> {
        Self::no("shot")
    }
    fn as_any(&mut self) -> &mut dyn std::any::Any {
        self
    }
}

// -- Fake ----------------------------------------------------------------------

/// One account's in-memory mail server: `folder → (uidvalidity, next uid,
/// mails)`.
#[derive(Default, Clone)]
pub struct FakeServer {
    pub folders: HashMap<String, (u32, u32, Vec<RemoteMail>)>,
    /// Whether MOVE reports the new uid (UIDPLUS' COPYUID). Both server
    /// behaviours exist in the wild, so both are testable.
    pub copyuid: bool,
    /// A server whose `PERMANENTFLAGS` allow no keywords: it takes a
    /// `STORE` of one and keeps nothing.
    pub no_keywords: bool,
    /// Mail this account handed to SMTP.
    pub submitted: Vec<Outgoing>,
}

impl FakeServer {
    /// Puts a mail in a folder, answering its uid.
    pub fn deliver(&mut self, folder: &str, unread: bool, raw: &str) -> u32 {
        let f = self
            .folders
            .entry(folder.to_string())
            .or_insert((1, 1, Vec::new()));
        let uid = f.1;
        f.1 += 1;
        f.2.push(RemoteMail {
            uid,
            unread,
            forwarded: false,
            raw: raw.as_bytes().to_vec(),
        });
        uid
    }

    /// Creates an empty folder with a chosen uidvalidity.
    pub fn folder(&mut self, name: &str, uidvalidity: u32) {
        self.folders
            .insert(name.to_string(), (uidvalidity, 1, Vec::new()));
    }

    pub fn remove(&mut self, folder: &str, uid: u32) {
        if let Some(f) = self.folders.get_mut(folder) {
            f.2.retain(|m| m.uid != uid);
        }
    }

    pub fn mark_seen(&mut self, folder: &str, uid: u32) {
        if let Some(f) = self.folders.get_mut(folder) {
            for m in &mut f.2 {
                if m.uid == uid {
                    m.unread = false;
                }
            }
        }
    }

    /// Sets or clears `$Forwarded`, as another client would.
    pub fn set_forwarded(&mut self, folder: &str, uid: u32, on: bool) {
        if let Some(f) = self.folders.get_mut(folder) {
            for m in &mut f.2 {
                if m.uid == uid {
                    m.forwarded = on;
                }
            }
        }
    }

    fn role_of(name: &str) -> Option<String> {
        match name {
            "INBOX" => Some("inbox".into()),
            "Archive" => Some("archive".into()),
            "Sent" => Some("sent".into()),
            "Trash" => Some("trash".into()),
            _ => None,
        }
    }

    fn get(&self, name: &str) -> Result<&(u32, u32, Vec<RemoteMail>), String> {
        self.folders
            .get(name)
            .ok_or_else(|| "no such folder".to_string())
    }
}

/// An in-memory outside: mail servers per account, a keychain that is a
/// map, a clock the test moves, and captured clipboard, files and shots.
/// Nothing here touches the filesystem, the network or the keychain, which
/// is what makes any number of fake worlds safe to run in parallel.
#[derive(Default)]
pub struct Fake {
    pub servers: HashMap<i64, FakeServer>,
    pub secrets: HashMap<String, String>,
    pub clips: Vec<String>,
    pub files: HashMap<PathBuf, Vec<u8>>,
    pub shots: Vec<PathBuf>,
    /// Accounts with a live session. A verb that reaches a server without
    /// one is a bug in the pass, and this catches it.
    pub connected: HashSet<i64>,
    /// Unix seconds; only moves when a test moves it.
    pub clock: f64,
    /// When set, every network verb fails with this — the offline test.
    pub down: Option<String>,
}

impl Fake {
    /// This account's server, created empty on first touch.
    pub fn server(&mut self, account: i64) -> &mut FakeServer {
        self.servers.entry(account).or_default()
    }

    /// Plants a password, as the settings form would.
    pub fn keychain(&mut self, email: &str, pass: &str) {
        self.secrets.insert(email.into(), pass.into());
    }

    fn live(&mut self, account: i64) -> Result<&mut FakeServer, String> {
        if let Some(e) = &self.down {
            return Err(e.clone());
        }
        if !self.connected.contains(&account) {
            return Err("not connected".into());
        }
        Ok(self.servers.entry(account).or_default())
    }
}

impl Outside for Fake {
    fn now(&mut self) -> f64 {
        self.clock
    }

    fn connect(&mut self, account: i64, c: &Creds) -> Result<(), String> {
        if let Some(e) = &self.down {
            return Err(e.clone());
        }
        if self.secrets.get(&c.user).map(String::as_str) != Some(c.pass.as_str()) {
            return Err("authentication failed".into());
        }
        self.connected.insert(account);
        Ok(())
    }

    fn folders(&mut self, account: i64) -> Result<Vec<RemoteFolder>, String> {
        let s = self.live(account)?;
        let mut names: Vec<String> = s.folders.keys().cloned().collect();
        names.sort();
        Ok(names
            .into_iter()
            .map(|n| RemoteFolder {
                role: FakeServer::role_of(&n),
                name: n,
            })
            .collect())
    }

    fn folder_meta(&mut self, account: i64, folder: &str) -> Result<FolderMeta, String> {
        let s = self.live(account)?;
        let keywords = !s.no_keywords;
        let f = s.get(folder)?;
        Ok(FolderMeta {
            uidvalidity: f.0,
            uidnext: f.1,
            keywords,
        })
    }

    fn fetch(&mut self, account: i64, folder: &str, from: u32)
        -> Result<Vec<RemoteMail>, String>
    {
        let f = self.live(account)?.get(folder)?;
        Ok(f.2.iter().filter(|m| m.uid >= from).cloned().collect())
    }

    fn uids(&mut self, account: i64, folder: &str, which: UidSet)
        -> Result<HashSet<u32>, String>
    {
        let f = self.live(account)?.get(folder)?;
        Ok(f.2
            .iter()
            .filter(|m| match which {
                UidSet::All => true,
                UidSet::Unseen => m.unread,
                UidSet::Forwarded => m.forwarded,
            })
            .map(|m| m.uid)
            .collect())
    }

    fn move_uid(&mut self, account: i64, from: &str, to: &str, uid: u32)
        -> Result<Option<u32>, String>
    {
        let s = self.live(account)?;
        let src = s
            .folders
            .get_mut(from)
            .ok_or_else(|| "no such folder".to_string())?;
        let i = src
            .2
            .iter()
            .position(|m| m.uid == uid)
            .ok_or_else(|| "no such uid".to_string())?;
        let mut m = src.2.remove(i);
        let dst = s.folders.entry(to.to_string()).or_insert((1, 1, Vec::new()));
        m.uid = dst.1;
        dst.1 += 1;
        let new = m.uid;
        dst.2.push(m);
        Ok(s.copyuid.then_some(new))
    }

    fn store_flag(&mut self, account: i64, folder: &str, uid: u32, flag: MailFlag, on: bool)
        -> Result<(), String>
    {
        let s = self.live(account)?;
        let no_keywords = s.no_keywords;
        let f = s
            .folders
            .get_mut(folder)
            .ok_or_else(|| "no such folder".to_string())?;
        for m in &mut f.2 {
            if m.uid == uid {
                match flag {
                    MailFlag::Seen => m.unread = !on,
                    // Accepted and forgotten, as RFC 3501 lets a server.
                    MailFlag::Forwarded if no_keywords => {}
                    MailFlag::Forwarded => m.forwarded = on,
                }
            }
        }
        Ok(())
    }

    fn append(&mut self, account: i64, folder: &str, raw: &[u8]) -> Result<(), String> {
        let s = self.live(account)?;
        let f = s
            .folders
            .entry(folder.to_string())
            .or_insert((1, 1, Vec::new()));
        let uid = f.1;
        f.1 += 1;
        f.2.push(RemoteMail {
            uid,
            unread: false,
            forwarded: false,
            raw: raw.to_vec(),
        });
        Ok(())
    }

    fn submit(&mut self, c: &Creds, m: &Outgoing) -> Result<Vec<u8>, String> {
        if let Some(e) = &self.down {
            return Err(e.clone());
        }
        if self.secrets.get(&c.user).map(String::as_str) != Some(c.pass.as_str()) {
            return Err("authentication failed".into());
        }
        // The bytes the real transport would file to Sent, headers
        // included, so a sent mail that syncs back threads as it would.
        let n = self
            .servers
            .values()
            .map(|s| s.submitted.len())
            .sum::<usize>()
            + 1;
        let mut raw = format!(
            "From: {}\r\nTo: {}\r\nSubject: {}\r\nMessage-ID: <sent-{n}@fake>\r\n",
            c.user, m.to, m.subject
        );
        if let Some(mid) = &m.in_reply_to {
            raw += &format!("In-Reply-To: <{mid}>\r\n");
        }
        if !m.references.is_empty() {
            let refs: Vec<String> = m.references.iter().map(|r| format!("<{r}>")).collect();
            raw += &format!("References: {}\r\n", refs.join(" "));
        }
        raw += &format!("\r\n{}", m.body);
        // Whichever account owns this address; the first server otherwise.
        let acct = *self.servers.keys().next().unwrap_or(&1);
        self.server(acct).submitted.push(m.clone());
        Ok(raw.into_bytes())
    }

    fn secret_get(&mut self, email: &str) -> Option<String> {
        self.secrets.get(email).cloned()
    }

    fn secret_set(&mut self, email: &str, pass: &str) -> bool {
        self.secrets.insert(email.to_string(), pass.to_string());
        true
    }

    fn clip(&mut self, text: &str) -> Result<(), String> {
        self.clips.push(text.to_string());
        Ok(())
    }

    fn write_file(&mut self, path: &Path, bytes: &[u8]) -> Result<(), String> {
        self.files.insert(path.to_path_buf(), bytes.to_vec());
        Ok(())
    }

    fn shot(&mut self, path: &Path) -> Result<(), String> {
        self.shots.push(path.to_path_buf());
        Ok(())
    }

    fn as_any(&mut self) -> &mut dyn std::any::Any {
        self
    }
}

/// The RFC 822 message a draft goes out as. `In-Reply-To` names the parent
/// a reply answers; `References` carries whatever chain the draft has — a
/// reply's parent and what it referenced, a forward's source and what *it*
/// referenced — so both thread for anyone who already has the
/// conversation. A forward names no parent: it is not a reply.
pub fn rfc822(from: &str, m: &Outgoing) -> Result<lettre::Message, String> {
    use lettre::message::header;
    use lettre::Message;
    let s = |e: &dyn std::fmt::Display| format!("{e}");
    let bracket = |id: &str| {
        format!(
            "<{}>",
            id.trim().trim_start_matches('<').trim_end_matches('>')
        )
    };
    let mut b = Message::builder()
        .from(from.parse().map_err(|e| s(&e))?)
        .to(m.to.parse().map_err(|e| s(&e))?)
        .subject(m.subject.clone());
    if let Some(mid) = &m.in_reply_to {
        b = b.header(header::InReplyTo::from(bracket(mid)));
    }
    let mut refs: Vec<String> = Vec::new();
    for id in m.references.iter().map(|r| bracket(r)) {
        if !refs.contains(&id) {
            refs.push(id);
        }
    }
    if !refs.is_empty() {
        b = b.header(header::References::from(refs.join(" ")));
    }
    b.body(m.body.clone()).map_err(|e| s(&e))
}

// -- Real ----------------------------------------------------------------------

/// Passwords held in memory and shared across threads.
///
/// It has to be *shared*, not merely in-memory: each worker thread builds
/// its own [`Real`], so a password written by the UI thread is read by a
/// sync thread. A `HashMap` on the instance would be empty on the reader's
/// side — which is the real reason [`crate::secret`] reaches for something
/// process-external at all.
#[derive(Clone, Default)]
pub struct MemSecrets(Arc<Mutex<HashMap<String, String>>>);

impl MemSecrets {
    #[must_use]
    pub fn new() -> MemSecrets {
        MemSecrets::default()
    }
}

/// Where [`Real`] gets the time.
///
/// Shared for the same reason [`MemSecrets`] is: each worker thread builds
/// its own [`Real`], and a deadline written on the UI thread is read on a
/// sender thread. If the two disagreed about what time it is, the sender
/// would claim a send the script still thinks is cancellable.
#[derive(Clone)]
pub enum Clock {
    /// The wall clock.
    System,
    /// Virtual, advanced by whoever owns the frame loop. A headless run
    /// steps it one frame at a time, so the app's deadlines move with the
    /// script rather than with the machine — which is what makes a run
    /// reproducible under load.
    Virtual(Arc<Mutex<f64>>),
}

impl Clock {
    /// A virtual clock starting at `start` (unix seconds).
    #[must_use]
    pub fn virtual_from(start: f64) -> Clock {
        Clock::Virtual(Arc::new(Mutex::new(start)))
    }

    /// Moves a virtual clock on; the system clock ignores this.
    pub fn advance(&self, secs: f64) {
        if let Clock::Virtual(t) = self {
            if let Ok(mut g) = t.lock() {
                *g += secs;
            }
        }
    }

    fn read(&self) -> f64 {
        match self {
            Clock::System => std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_secs_f64())
                .unwrap_or(0.0),
            Clock::Virtual(t) => t.lock().map(|g| *g).unwrap_or(0.0),
        }
    }
}

/// Where [`Real`] keeps passwords.
#[derive(Clone)]
pub enum Secrets {
    /// The macOS keychain, or an app-private file elsewhere.
    Keychain(PathBuf),
    /// In memory, dying with the process — what an e2e run uses, so a suite
    /// never writes to a human's keychain and two runs never collide.
    Memory(MemSecrets),
}

/// The newest frame the headless rasterizer wrote, copied to `path`. Under
/// a headless build there is no window to photograph — makepad renders the
/// frames itself, so a "screenshot" is picking the right one.
#[cfg(headless)]
fn headless_shot(path: &Path) -> Result<(), String> {
    let dir = std::env::var("MAKEPAD_HEADLESS_OUT_DIR")
        .map_err(|_| "MAKEPAD_HEADLESS_OUT_DIR is not set".to_string())?;
    let newest = std::fs::read_dir(&dir)
        .map_err(|e| format!("{dir}: {e}"))?
        .filter_map(Result::ok)
        .filter(|e| {
            e.file_name()
                .to_string_lossy()
                .starts_with("window_")
        })
        .max_by_key(|e| e.file_name())
        .ok_or_else(|| format!("no rendered frame in {dir}"))?;
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| format!("{}: {e}", parent.display()))?;
    }
    std::fs::copy(newest.path(), path)
        .map(|_| ())
        .map_err(|e| format!("{}: {e}", path.display()))
}


/// The actual outside: one IMAP session per account (rustls, port 993,
/// LOGIN with an app password — fastmail-style; OAuth is deliberately
/// later), lettre over rustls for submission, and the platform for
/// everything else.
pub struct Real {
    sessions: HashMap<i64, imap_session::Imap>,
    secrets: Secrets,
    clock: Clock,
}

impl Real {
    #[must_use]
    pub fn new(secrets: Secrets, clock: Clock) -> Real {
        Real {
            sessions: HashMap::new(),
            secrets,
            clock,
        }
    }

    fn session(&mut self, account: i64) -> Result<&mut imap_session::Imap, String> {
        self.sessions
            .get_mut(&account)
            .ok_or_else(|| "not connected".to_string())
    }
}

impl Outside for Real {
    fn now(&mut self) -> f64 {
        self.clock.read()
    }

    fn connect(&mut self, account: i64, c: &Creds) -> Result<(), String> {
        let s = imap_session::connect(&c.host, &c.user, &c.pass)?;
        self.sessions.insert(account, s);
        Ok(())
    }

    fn folders(&mut self, account: i64) -> Result<Vec<RemoteFolder>, String> {
        self.session(account)?.folders()
    }

    fn folder_meta(&mut self, account: i64, folder: &str) -> Result<FolderMeta, String> {
        self.session(account)?.select(folder)
    }

    fn fetch(&mut self, account: i64, folder: &str, from: u32)
        -> Result<Vec<RemoteMail>, String>
    {
        self.session(account)?.fetch_from(folder, from)
    }

    fn uids(&mut self, account: i64, folder: &str, which: UidSet)
        -> Result<HashSet<u32>, String>
    {
        self.session(account)?.uids(folder, which)
    }

    fn move_uid(&mut self, account: i64, from: &str, to: &str, uid: u32)
        -> Result<Option<u32>, String>
    {
        self.session(account)?.move_uid(from, to, uid)
    }

    fn store_flag(&mut self, account: i64, folder: &str, uid: u32, flag: MailFlag, on: bool)
        -> Result<(), String>
    {
        self.session(account)?.store_flag(folder, uid, flag, on)
    }

    fn append(&mut self, account: i64, folder: &str, raw: &[u8]) -> Result<(), String> {
        self.session(account)?.append(folder, raw)
    }

    fn submit(&mut self, c: &Creds, m: &Outgoing) -> Result<Vec<u8>, String> {
        use lettre::transport::smtp::authentication::Credentials;
        use lettre::{SmtpTransport, Transport};
        let s = |e: &dyn std::fmt::Display| format!("{e}");
        let msg = rfc822(&c.user, m)?;
        let raw = msg.formatted();
        let t = SmtpTransport::relay(&c.host)
            .map_err(|e| s(&e))?
            .credentials(Credentials::new(c.user.clone(), c.pass.clone()))
            .build();
        t.send(&msg).map_err(|e| s(&e))?;
        Ok(raw)
    }

    fn secret_get(&mut self, email: &str) -> Option<String> {
        match &self.secrets {
            Secrets::Keychain(dir) => crate::secret::get(dir, email),
            Secrets::Memory(m) => m.0.lock().ok()?.get(email).cloned(),
        }
    }

    fn secret_set(&mut self, email: &str, pass: &str) -> bool {
        match &self.secrets {
            Secrets::Keychain(dir) => crate::secret::set(dir, email, pass),
            Secrets::Memory(m) => m
                .0
                .lock()
                .map(|mut g| g.insert(email.to_string(), pass.to_string()))
                .is_ok(),
        }
    }

    fn clip(&mut self, text: &str) -> Result<(), String> {
        #[cfg(target_os = "macos")]
        {
            use std::io::Write;
            let mut child = std::process::Command::new("/usr/bin/pbcopy")
                .stdin(std::process::Stdio::piped())
                .spawn()
                .map_err(|e| format!("pbcopy: {e}"))?;
            if let Some(stdin) = child.stdin.as_mut() {
                stdin
                    .write_all(text.as_bytes())
                    .map_err(|e| format!("pbcopy: {e}"))?;
            }
            let _ = child.wait();
            Ok(())
        }
        #[cfg(not(target_os = "macos"))]
        {
            let _ = text;
            Err("no clipboard on this platform".into())
        }
    }

    fn write_file(&mut self, path: &Path, bytes: &[u8]) -> Result<(), String> {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
        }
        std::fs::write(path, bytes).map_err(|e| format!("{}: {e}", path.display()))
    }

    fn shot(&mut self, path: &Path) -> Result<(), String> {
        #[cfg(headless)]
        {
            headless_shot(path)
        }
        #[cfg(all(not(headless), target_os = "macos"))]
        {
            crate::mac::screenshot(path)
        }
        #[cfg(all(not(headless), not(target_os = "macos")))]
        {
            let _ = path;
            Err("no window capture on this platform".into())
        }
    }

    fn as_any(&mut self) -> &mut dyn std::any::Any {
        self
    }
}

/// The `imap` crate, wrapped. Stateful (a selected mailbox), so `ensure`
/// suppresses redundant SELECTs — that optimisation stays private.
mod imap_session {
    use super::{FolderMeta, MailFlag, RemoteFolder, RemoteMail, UidSet};
    use std::collections::HashSet;

    type ImapSession = imap::Session<Box<dyn imap::ImapConnection>>;

    pub struct Imap {
        session: ImapSession,
        selected: Option<String>,
    }

    fn s<E: std::fmt::Display>(e: E) -> String {
        format!("{e}")
    }

    /// The IMAP keyword for "passed on" (registered in RFC 5788's list):
    /// what Apple Mail, Thunderbird, Fastmail and Dovecot set and read.
    const FORWARDED: &str = "$Forwarded";

    /// Whether a folder's `PERMANENTFLAGS` let the keyword be kept: the
    /// keyword itself, or `\*` (any keyword). An empty list is *not*
    /// support — the crate hands back the same empty list for
    /// `PERMANENTFLAGS ()` (a folder that keeps nothing, an EXAMINEd one)
    /// and for a server that sent no such response at all, and only the
    /// second could be read as "all flags are permanent" (RFC 3501 §7.1).
    /// Between a mark kept local on a server that said nothing and a
    /// mark taken and forgotten by one that said `()`, keep it local.
    pub(super) fn keeps_keywords(permanent: &[imap::types::Flag<'_>]) -> bool {
        use imap::types::Flag;
        permanent.iter().any(|f| match f {
            Flag::MayCreate => true,
            Flag::Custom(k) => k.eq_ignore_ascii_case(FORWARDED),
            _ => false,
        })
    }

    pub fn connect(host: &str, user: &str, pass: &str) -> Result<Imap, String> {
        let client = imap::ClientBuilder::new(host, 993).connect().map_err(s)?;
        let session = client.login(user, pass).map_err(|e| s(e.0))?;
        Ok(Imap {
            session,
            selected: None,
        })
    }

    impl Imap {
        pub fn select(&mut self, name: &str) -> Result<FolderMeta, String> {
            let mb = self.session.select(name).map_err(s)?;
            self.selected = Some(name.to_string());
            Ok(FolderMeta {
                uidvalidity: mb.uid_validity.unwrap_or(0),
                uidnext: mb.uid_next.unwrap_or(1),
                keywords: keeps_keywords(&mb.permanent_flags),
            })
        }

        fn ensure(&mut self, name: &str) -> Result<(), String> {
            if self.selected.as_deref() != Some(name) {
                self.select(name)?;
            }
            Ok(())
        }

        pub fn folders(&mut self) -> Result<Vec<RemoteFolder>, String> {
            let names = self.session.list(Some(""), Some("*")).map_err(s)?;
            let mut out = Vec::new();
            for n in names.iter() {
                let attrs = format!("{:?}", n.attributes()).to_lowercase();
                let role = if n.name().eq_ignore_ascii_case("inbox") {
                    Some("inbox".to_string())
                } else if attrs.contains("archive") {
                    Some("archive".to_string())
                } else if attrs.contains("sent") {
                    Some("sent".to_string())
                } else if attrs.contains("trash") {
                    Some("trash".to_string())
                } else {
                    None
                };
                out.push(RemoteFolder {
                    name: n.name().to_string(),
                    role,
                });
            }
            Ok(out)
        }

        pub fn fetch_from(&mut self, name: &str, from: u32) -> Result<Vec<RemoteMail>, String> {
            self.ensure(name)?;
            let fetches = self
                .session
                .uid_fetch(format!("{from}:*"), "(UID FLAGS RFC822)")
                .map_err(s)?;
            let mut out: Vec<RemoteMail> = fetches
                .iter()
                .filter_map(|f| {
                    let uid = f.uid?;
                    let raw = f.body().or_else(|| f.text())?;
                    let unread = !f
                        .flags()
                        .iter()
                        .any(|fl| matches!(fl, imap::types::Flag::Seen));
                    let forwarded = f.flags().iter().any(|fl| {
                        matches!(fl, imap::types::Flag::Custom(k) if k.eq_ignore_ascii_case(FORWARDED))
                    });
                    Some(RemoteMail {
                        uid,
                        unread,
                        forwarded,
                        raw: raw.to_vec(),
                    })
                })
                .collect();
            out.sort_by_key(|m| m.uid);
            Ok(out)
        }

        pub fn uids(&mut self, name: &str, which: UidSet) -> Result<HashSet<u32>, String> {
            self.ensure(name)?;
            let query = match which {
                UidSet::All => "ALL".to_string(),
                UidSet::Unseen => "UNSEEN".to_string(),
                UidSet::Forwarded => format!("KEYWORD {FORWARDED}"),
            };
            self.session.uid_search(query).map_err(s)
        }

        pub fn move_uid(&mut self, from: &str, to: &str, uid: u32)
            -> Result<Option<u32>, String>
        {
            self.ensure(from)?;
            self.session.uid_mv(uid.to_string(), to).map_err(s)?;
            // The crate acks the MOVE but does not surface COPYUID; the new
            // uid arrives via Message-ID adoption on the next fetch.
            Ok(None)
        }

        pub fn store_flag(&mut self, folder: &str, uid: u32, flag: MailFlag, on: bool)
            -> Result<(), String>
        {
            self.ensure(folder)?;
            let name = match flag {
                MailFlag::Seen => "\\Seen",
                MailFlag::Forwarded => FORWARDED,
            };
            let sign = if on { '+' } else { '-' };
            self.session
                .uid_store(uid.to_string(), format!("{sign}FLAGS ({name})"))
                .map_err(s)?;
            Ok(())
        }

        pub fn append(&mut self, folder: &str, raw: &[u8]) -> Result<(), String> {
            self.session
                .append(folder, raw)
                .flag(imap::types::Flag::Seen)
                .finish()
                .map_err(s)?;
            Ok(())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A test effect that writes to the fake clipboard, so "did it run?" is
    /// observable, and whose failure and idempotency are dialable.
    #[derive(Serialize, Deserialize)]
    struct Poke {
        note: String,
        fails: bool,
        idem: bool,
        wanted: bool,
    }

    impl Poke {
        fn ok(note: &str) -> Poke {
            Poke { note: note.into(), fails: false, idem: true, wanted: true }
        }
    }

    impl Effect for Poke {
        const KIND: &'static str = "poke";
        type Reply = String;
        fn describe(&self) -> String {
            format!("poke {}", self.note)
        }
        fn perform(&self, cx: &mut Ctx<'_>) -> Result<String, String> {
            if self.fails {
                return Err("poke refused".into());
            }
            cx.out.clip(&self.note)?;
            Ok(format!("poked {}", self.note))
        }
    }

    impl Deferred for Poke {
        fn idempotent(&self) -> bool {
            self.idem
        }
        fn entity(&self) -> Option<String> {
            Some(format!("panel:{}", self.note.len()))
        }
        fn still_wanted(&self, _db: &Connection) -> bool {
            self.wanted
        }
    }

    fn world() -> World {
        let mut reg = Registry::new();
        reg.register::<Poke>();
        World::fake(reg)
    }

    /// What a folder's PERMANENTFLAGS say about keeping `$Forwarded`: the
    /// keyword, or the `\*` wildcard; not the system flags alone, and not
    /// an empty list, which is what `()` and no response both arrive as.
    #[test]
    fn keywords_need_the_servers_word() {
        use imap::types::Flag;
        use std::borrow::Cow;
        let keeps = imap_session::keeps_keywords;
        assert!(!keeps(&[]), "`()` and no response look alike: kept local");
        assert!(!keeps(&[Flag::Seen, Flag::Flagged]));
        assert!(keeps(&[Flag::Seen, Flag::MayCreate]));
        assert!(keeps(&[Flag::Custom(Cow::Borrowed("$forwarded"))]));
        assert!(!keeps(&[Flag::Custom(Cow::Borrowed("$MDNSent"))]));
    }

    /// The threading headers a draft goes out with: a reply names its
    /// parent and carries the chain; a forward carries the chain alone;
    /// a blank mail carries nothing.
    #[test]
    fn threading_headers_follow_the_draft() {
        let out =
            |m: &Outgoing| String::from_utf8(rfc822("me@b.c", m).unwrap().formatted()).unwrap();
        let reply = Outgoing {
            to: "a@b.c".into(),
            subject: "Re: x".into(),
            body: "hi".into(),
            in_reply_to: Some("p@b.c".into()),
            references: vec!["r@b.c".into(), "p@b.c".into()],
        };
        let raw = out(&reply);
        assert!(raw.contains("In-Reply-To: <p@b.c>\r\n"), "{raw}");
        assert!(raw.contains("References: <r@b.c> <p@b.c>\r\n"), "{raw}");

        let forward = Outgoing {
            in_reply_to: None,
            references: vec!["p@b.c".into()],
            ..reply.clone()
        };
        let raw = out(&forward);
        assert!(
            !raw.contains("In-Reply-To"),
            "a forward is not a reply: {raw}"
        );
        assert!(raw.contains("References: <p@b.c>\r\n"), "{raw}");

        let blank = Outgoing {
            in_reply_to: None,
            references: Vec::new(),
            ..reply
        };
        let raw = out(&blank);
        assert!(
            !raw.contains("In-Reply-To") && !raw.contains("References"),
            "{raw}"
        );
    }

    /// The row exists, `pending`, *before* anything is performed — and the
    /// reply lands on it after.
    #[test]
    fn a_job_is_committed_before_it_runs_and_closed_after() {
        let w = world();
        w.enqueue(&Poke::ok("hello")).unwrap();

        let j = &w.jobs()[0];
        assert_eq!((j.kind.as_str(), j.status.as_str()), ("poke", "pending"));
        assert_eq!(j.entity.as_deref(), Some("panel:5"));
        assert!(j.reply.is_none(), "nothing has happened yet");
        assert!(w.with_fake(|f| f.clips.is_empty()));

        assert_eq!(w.run_effects(), 1);
        let j = &w.jobs()[0];
        assert_eq!(j.status, "done");
        assert_eq!(j.reply.as_deref(), Some("\"poked hello\""));
        assert_eq!(w.with_fake(|f| f.clips.clone()), vec!["hello"]);

        assert_eq!(w.run_effects(), 0, "a closed job is not reclaimed");
    }

    /// Cancelling beats the executor while the row is `pending`, and the
    /// effect never happens.
    #[test]
    fn cancel_wins_the_race_while_pending() {
        let w = world();
        let id = w.enqueue(&Poke::ok("doomed")).unwrap();

        let now = w.now();
        let won = w.store().write(move |tx| cancel_tx(tx, id, now)).unwrap();
        assert!(won);
        assert_eq!(w.jobs()[0].status, "obsolete");
        assert_eq!(w.run_effects(), 0);
        assert!(w.with_fake(|f| f.clips.is_empty()), "never performed");

        // A second cancel loses — there is exactly one winner.
        assert!(!w.store().write(move |tx| cancel_tx(tx, id, now)).unwrap());
    }

    /// A job the world no longer wants goes obsolete instead of running.
    #[test]
    fn revalidation_skips_stale_work() {
        let w = world();
        w.enqueue(&Poke { wanted: false, ..Poke::ok("stale") }).unwrap();
        assert_eq!(w.run_effects(), 1, "claimed…");
        assert_eq!(w.jobs()[0].status, "obsolete", "…but not performed");
        assert!(w.with_fake(|f| f.clips.is_empty()));
    }

    /// Failures retry with backoff, then give up and wait for a human.
    #[test]
    fn failures_retry_with_backoff_then_give_up() {
        let w = world();
        w.enqueue(&Poke { fails: true, ..Poke::ok("nope") }).unwrap();

        w.run_effects();
        let j = &w.jobs()[0];
        assert_eq!(j.status, "pending", "queued again");
        assert_eq!(j.error.as_deref(), Some("poke refused"));
        assert_eq!(j.attempts, 1);

        // Held back: the executor will not touch it until the clock moves.
        assert_eq!(w.run_effects(), 0, "backoff is respected");

        for _ in 0..MAX_ATTEMPTS {
            w.with_fake(|f| f.clock += 3600.0);
            w.run_effects();
        }
        let j = &w.jobs()[0];
        assert_eq!(j.status, "failed", "gave up rather than spinning");
        assert_eq!(j.attempts, MAX_ATTEMPTS);
    }

    /// An unregistered kind fails loudly. The price of an open set is that
    /// this is a runtime error — so it must never be a silent stall.
    #[test]
    fn an_unregistered_kind_fails_loudly() {
        let w = World::fake(Registry::new()); // nothing registered
        w.enqueue(&Poke::ok("orphan")).unwrap();
        w.run_effects();
        let j = &w.jobs()[0];
        assert_eq!(j.status, "failed");
        assert_eq!(j.error.as_deref(), Some("no handler for kind poke"));
    }

    /// The crash sweep: idempotent work is safe to redo, and everything
    /// else must ask a human rather than guess.
    #[test]
    fn the_crash_sweep_never_guesses() {
        let w = world();
        let safe = w.enqueue(&Poke::ok("safe")).unwrap();
        let risky = w.enqueue(&Poke { idem: false, ..Poke::ok("risky") }).unwrap();
        // Both caught mid-flight by the crash.
        w.store()
            .write(|tx| {
                tx.execute("UPDATE effect SET status='processing'", [])
                    .map(|_| ())
            })
            .unwrap();

        w.store().write(|tx| crate::store::sweep_effects(tx)).unwrap();

        let by_id = |id: i64| w.jobs().into_iter().find(|j| j.id == id).unwrap();
        assert_eq!(by_id(safe).status, "pending", "idempotent: retry it");
        let r = by_id(risky);
        assert_eq!(r.status, "failed", "not idempotent: do not guess");
        assert_eq!(r.error.as_deref(), Some("interrupted; outcome unknown"));
    }

    /// A panel can ask what it has in flight — points 6 and 7 of the
    /// design, through the existing `entity` vocabulary.
    #[test]
    fn a_panel_can_query_its_own_effects() {
        let w = world();
        w.enqueue(&Poke::ok("aaa")).unwrap();
        w.enqueue(&Poke::ok("bbbb")).unwrap();
        assert_eq!(w.jobs_of("panel:3").len(), 1);
        assert_eq!(w.jobs_of("panel:4").len(), 1);
        assert_eq!(w.jobs_of("panel:9").len(), 0);
    }

    /// `Deny` refuses everything, which is what a components-library mount
    /// wants: a panel that quietly sends mail fails instead of succeeding.
    #[test]
    fn deny_refuses_everything() {
        let mut reg = Registry::new();
        reg.register::<Poke>();
        let w = World::new(
            Rc::new(Store::open(None).unwrap()),
            Box::new(Deny::default()),
            reg,
        );
        w.enqueue(&Poke::ok("nope")).unwrap();
        w.run_effects();
        let j = &w.jobs()[0];
        assert_eq!(j.status, "pending", "retryable, but refused");
        assert!(j.error.as_deref().unwrap().contains("no outside"), "{j:?}");
    }

    /// Two fake worlds share nothing: no file, no keychain, no clock.
    #[test]
    fn worlds_are_isolated_from_each_other() {
        let a = world();
        let b = world();
        a.enqueue(&Poke::ok("a")).unwrap();
        a.run_effects();
        a.with_fake(|f| f.clock += 500.0);

        assert_eq!(b.jobs().len(), 0);
        assert!(b.with_fake(|f| f.clips.is_empty()));
        assert_eq!(b.now(), 0.0);
        assert_eq!(a.now(), 500.0);
    }

    /// A password must never reach the record — not via `describe`, and not
    /// via a stray `{:?}` on the credentials.
    #[test]
    fn secrets_never_reach_the_record() {
        let c = Creds { host: "h".into(), user: "u".into(), pass: "s3cret".into() };
        assert!(!format!("{c:?}").contains("s3cret"), "{c:?}");
    }

    // -- the log, as its viewer reads it ------------------------------------

    /// The queue through the rich table, under a filter — what the log
    /// panel does on every draw, minus the widgets.
    fn log(w: &World, filter: &str) -> Vec<Job> {
        let mut t = crate::richtable::Table::new(&LOG, LOG_PAGE);
        t.set_filter(filter);
        assert!(t.errors().is_empty(), "{filter:?}: {:?}", t.errors());
        let n = t.len(w.store());
        t.rows(w.store(), 0, n)
    }

    /// The log holds no rows of its own: every one is a page of the queue,
    /// newest first, and the executor's commits show through.
    #[test]
    fn the_log_pages_the_queue_newest_first() {
        let w = world();
        w.enqueue(&Poke::ok("one")).unwrap();
        w.enqueue(&Poke::ok("two")).unwrap();

        let rows = log(&w, "");
        assert_eq!(rows.len(), 2);
        assert!(rows[0].id > rows[1].id, "newest first");
        assert!(rows.iter().all(|j| j.status == "pending"));

        w.run_effects();
        let rows = log(&w, "");
        assert!(rows.iter().all(|j| j.status == "done"));
        assert_eq!(rows[0].reply.as_deref(), Some("\"poked two\""));
    }

    /// The filter grammar over the queue's own columns — including a tag
    /// value that carries a colon, which is how every entity is spelled.
    #[test]
    fn the_log_filters_by_its_own_tags() {
        let w = world();
        w.enqueue(&Poke::ok("one")).unwrap();
        w.enqueue(&Poke {
            note: "sevenxx".into(),
            fails: true,
            idem: false,
            wanted: true,
        })
        .unwrap();
        w.run_effects();

        assert_eq!(log(&w, "@kind:poke").len(), 2);
        assert_eq!(log(&w, "@kind:submit").len(), 0);

        // `@risky` is the work a crash cannot retry for you.
        let risky = log(&w, "@risky");
        assert_eq!(risky.len(), 1);
        assert_eq!(risky[0].entity.as_deref(), Some("panel:7"));

        // `panel:3` is one value: a filter that stopped at the colon would
        // read as "contains panel" and keep both rows.
        assert_eq!(log(&w, "@entity:panel:3").len(), 1);

        // Bare words search the payload, which is where the arguments are.
        assert_eq!(log(&w, "sevenxx").len(), 1);

        // The failure went back in the queue with a backoff, so it is live
        // and has not been retried yet.
        assert_eq!(log(&w, "@live").len(), 1);
        assert_eq!(log(&w, "@retried").len(), 0);

        // Past the backoff, the second attempt says so in one phrase.
        w.with_fake(|f| f.clock += 60.0);
        w.run_effects();
        let retried = log(&w, "@retried");
        assert_eq!(retried.len(), 1);
        assert_eq!(retried[0].status_line(), "pending · 2 tries");
        assert_eq!(retried[0].error.as_deref(), Some("poke refused"));
    }

    /// The one line a row shows comes from the effect itself: the registry
    /// decodes the payload and asks it. No central table of kinds, and no
    /// panic on a payload this build cannot read.
    #[test]
    fn a_filed_payload_describes_itself() {
        let w = world();
        w.enqueue(&Poke::ok("hello")).unwrap();
        let j = &w.jobs()[0];

        assert_eq!(
            w.registry().describe(&j.kind, &j.payload).as_deref(),
            Some("poke hello")
        );
        assert!(
            w.registry().describe("nosuch", &j.payload).is_none(),
            "an unregistered kind names itself with nothing"
        );
        assert!(w.registry().describe("poke", "{}").is_none());
    }
}
