//! Work whose result cannot be recreated from the database.
//!
//! Retryable [`Deferred`] effects are stored as jobs. Immediate effects return
//! directly and the latest [`KEPT`] entries stay in [`MemLog`]. The Effects
//! query combines both sources. [`Outside`] selects real access, fake test
//! data, or no outside access. It also owns the clock.

use std::cell::RefCell;
use std::sync::atomic::{AtomicI64, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::collections::{HashMap, HashSet, VecDeque};
use std::path::{Path, PathBuf};
use std::rc::Rc;

use rusqlite::functions::FunctionFlags;
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
    /// This is the provider's *all mail* view (`\All`), not a folder of its
    /// own: Gmail's, where every message also lives under whatever labels
    /// it has. A move target, never an ingest source — see
    /// [`crate::sync::fetch_account`].
    pub all_mail: bool,
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
    /// other side too. Older payloads may omit it.
    #[serde(default)]
    pub references: Vec<String>,
    /// What it carries — read off the disk by [`crate::mail::Submit`]
    /// as it goes out, never stored: this value is built at submit time, and
    /// a payload holding a file's bytes would be both stale and enormous.
    #[serde(default, skip)]
    pub attachments: Vec<Part>,
}

/// One part of a mail on its way out: what compose attached, with the bytes
/// it will actually carry.
#[derive(Clone, Serialize, Deserialize)]
pub struct Part {
    pub name: String,
    pub mime: String,
    pub bytes: Vec<u8>,
}

impl std::fmt::Debug for Part {
    /// The bytes are megabytes and never worth printing — an outgoing mail
    /// gets logged, and a log is for reading.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Part")
            .field("name", &self.name)
            .field("mime", &self.mime)
            .field("size", &self.bytes.len())
            .finish()
    }
}

/// How a session proves who it is. Two mechanisms, and the account row's
/// `auth` column is what picks between them.
#[derive(Clone, PartialEq)]
pub enum Auth {
    /// An app password: IMAP `LOGIN`, SMTP `AUTH PLAIN`.
    Password(String),
    /// An OAuth 2 access token: SASL `XOAUTH2` on both (see
    /// [`crate::oauth`]). Short-lived — the caller fetches a fresh one per
    /// connect rather than holding it.
    Bearer(String),
}

impl Auth {
    /// The secret itself, for the one backend that must compare it.
    #[must_use]
    pub fn secret(&self) -> &str {
        match self {
            Auth::Password(s) | Auth::Bearer(s) => s,
        }
    }
}

/// `Debug` names the mechanism and redacts the secret, so a stray `{:?}`
/// anywhere — a test failure, a log line — cannot leak one.
impl std::fmt::Debug for Auth {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Auth::Password(_) => "password …",
            Auth::Bearer(_) => "bearer …",
        })
    }
}

/// How to reach a server. `Debug` redacts the secret so a stray `{:?}`
/// cannot leak one, and no [`Effect::describe`] ever prints it — `describe`
/// is what lands in the table.
#[derive(Clone)]
pub struct Creds {
    pub host: String,
    pub user: String,
    pub auth: Auth,
}

impl Creds {
    /// Credentials that log in with an app password.
    #[must_use]
    pub fn password(
        host: impl Into<String>,
        user: impl Into<String>,
        pass: impl Into<String>,
    ) -> Creds {
        Creds {
            host: host.into(),
            user: user.into(),
            auth: Auth::Password(pass.into()),
        }
    }

    /// Credentials that authenticate with an OAuth access token.
    #[must_use]
    pub fn bearer(
        host: impl Into<String>,
        user: impl Into<String>,
        token: impl Into<String>,
    ) -> Creds {
        Creds {
            host: host.into(),
            user: user.into(),
            auth: Auth::Bearer(token.into()),
        }
    }
}

impl std::fmt::Debug for Creds {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Creds")
            .field("host", &self.host)
            .field("user", &self.user)
            .field("auth", &self.auth)
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
    /// A usable OAuth access token for this address, refreshed against the
    /// provider if the cached one has expired.
    ///
    /// One verb rather than "read the refresh token, then POST": the token
    /// endpoint is a network round trip that must be fakeable, and the
    /// cache that keeps it from happening per connect belongs to the
    /// backend that owns the process, not to the caller.
    fn access_token(&mut self, email: &str) -> Result<String, String>;
    /// The device-sync bucket's secret access key, by key id. Its
    /// own door rather than [`Outside::secret_set`]'s: losing a mailbox and
    /// losing a lineage are different accidents, and they are kept under
    /// different services.
    fn bucket_secret_set(&mut self, key_id: &str, secret: &str) -> bool;
    fn clip(&mut self, text: &str) -> Result<(), String>;
    fn write_file(&mut self, path: &Path, bytes: &[u8]) -> Result<(), String>;
    fn shot(&mut self, path: &Path) -> Result<(), String>;

    // The disk, read. A files panel lists through these during
    // draw; the fake serves the demo tree, the real one the filesystem.
    /// One directory's entries, in the browser's order.
    fn list_dir(&mut self, dir: &Path) -> Result<Vec<crate::files::Entry>, String>;
    /// One path's entry, `None` when there is nothing there.
    fn stat(&mut self, path: &Path) -> Result<Option<crate::files::Entry>, String>;
    /// The first `max` bytes of a file.
    fn read_file(&mut self, path: &Path, max: usize) -> Result<Vec<u8>, String>;
    /// Hand a path to the OS — whatever opens that kind of file. Nothing
    /// is executed by us.
    fn open_path(&mut self, path: &Path) -> Result<(), String>;

    // The disk, written. None of these is `rm`: what a delete
    // takes goes to the trash, and undo moves it back out — so a backend
    // that implements them can never make a path unrecoverable.
    /// One directory, where nothing is yet. Refuses a taken name rather
    /// than adopting whatever is there.
    fn make_dir(&mut self, path: &Path) -> Result<(), String>;
    /// A file, or a directory with everything under it, copied. Refuses a
    /// taken destination — a copy never writes over anything.
    fn copy_path(&mut self, from: &Path, to: &Path) -> Result<(), String>;
    /// A path moved, and the same verb undo puts one back with. Refuses a
    /// taken destination, for the same reason.
    fn move_path(&mut self, from: &Path, to: &Path) -> Result<(), String>;
    /// To the trash, answering where it landed — the trash picks the name,
    /// and undo needs the one it picked.
    fn trash(&mut self, path: &Path) -> Result<PathBuf, String>;
    /// What the disk calls the object at this path, as opposed to what the
    /// path calls it — `None` where there is nothing. Never follows a
    /// link: the question is about the object the name is bound to.
    fn file_id(&mut self, path: &Path) -> Result<Option<crate::files::FileId>, String>;

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
    /// Did the world change because of this, or was it only asked
    /// something? A `MOVE`, a `STORE`, a send, a file written, a password
    /// filed: those changed it. A `FETCH`, a `SEARCH`, a folder listing,
    /// the clock, a password recalled: those did not — and neither did a
    /// connect, which is what makes the rest possible and nothing more.
    ///
    /// No default, on purpose. A sync pass asks the outside a dozen
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
    fn perform(&self, cx: &mut Ctx<'_>) -> Result<Self::Reply, String>;
}

/// An effect worth persisting: queued, retried, its status and reply
/// readable from the table. Both the effect and its reply must survive a
/// round trip through JSON, so an effect that cannot be written down is a
/// compile error rather than a discovery.
// `Send` is required because a job's `settle` closure travels to the store's
// writer thread: the effect value and its reply are captured
// and committed there. Every real effect is plain data, so this is free.
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
    fn writes(&self) -> bool {
        false
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
    fn writes(&self) -> bool {
        false
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
    fn writes(&self) -> bool {
        true
    }
    fn perform(&self, cx: &mut Ctx<'_>) -> Result<(), String> {
        cx.out
            .secret_set(self.email, self.pass)
            .then_some(())
            .ok_or_else(|| "the keychain refused the password".to_string())
    }
}

/// Store the device-sync bucket's secret access key.
pub struct BucketSecret<'a> {
    pub key_id: &'a str,
    pub secret: &'a str,
}

impl Effect for BucketSecret<'_> {
    const KIND: &'static str = "bucket_secret";
    type Reply = ();
    fn describe(&self) -> String {
        format!("store the bucket secret for {}", self.key_id)
    }
    fn writes(&self) -> bool {
        true
    }
    fn perform(&self, cx: &mut Ctx<'_>) -> Result<(), String> {
        cx.out
            .bucket_secret_set(self.key_id, self.secret)
            .then_some(())
            .ok_or_else(|| "the keychain refused the bucket secret".to_string())
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
    fn writes(&self) -> bool {
        true
    }
    fn perform(&self, cx: &mut Ctx<'_>) -> Result<(), String> {
        cx.out.clip(self.text)
    }
}

/// Hand a path to the OS: whatever opens that kind of file.
pub struct OpenPath<'a> {
    pub path: &'a Path,
}

impl Effect for OpenPath<'_> {
    const KIND: &'static str = "open";
    type Reply = ();
    fn describe(&self) -> String {
        format!("open {}", self.path.display())
    }
    /// Nothing of ours changes; something out there starts, which is more
    /// than a question.
    fn writes(&self) -> bool {
        true
    }
    fn perform(&self, cx: &mut Ctx<'_>) -> Result<(), String> {
        cx.out.open_path(self.path)
    }
}

/// Make one directory — a files panel's `new dir`.
pub struct MakeDir<'a> {
    pub path: &'a Path,
}

impl Effect for MakeDir<'_> {
    const KIND: &'static str = "make_dir";
    type Reply = ();
    fn describe(&self) -> String {
        format!("make the directory {}", self.path.display())
    }
    fn writes(&self) -> bool {
        true
    }
    fn perform(&self, cx: &mut Ctx<'_>) -> Result<(), String> {
        cx.out.make_dir(self.path)
    }
}

/// `copy here`, one path of it.
pub struct CopyPath<'a> {
    pub from: &'a Path,
    pub to: &'a Path,
}

impl Effect for CopyPath<'_> {
    const KIND: &'static str = "copy_path";
    type Reply = ();
    fn describe(&self) -> String {
        format!("copy {} to {}", self.from.display(), self.to.display())
    }
    fn writes(&self) -> bool {
        true
    }
    fn perform(&self, cx: &mut Ctx<'_>) -> Result<(), String> {
        cx.out.copy_path(self.from, self.to)
    }
}

/// `move here`, one path of it — and what undo reverses every one of these
/// verbs with.
pub struct MovePath<'a> {
    pub from: &'a Path,
    pub to: &'a Path,
}

impl Effect for MovePath<'_> {
    const KIND: &'static str = "move_path";
    type Reply = ();
    fn describe(&self) -> String {
        format!("move {} to {}", self.from.display(), self.to.display())
    }
    fn writes(&self) -> bool {
        true
    }
    fn perform(&self, cx: &mut Ctx<'_>) -> Result<(), String> {
        cx.out.move_path(self.from, self.to)
    }
}

/// `delete`: to the trash, never `rm`, answering where it landed so undo
/// can take it back out.
pub struct Trash<'a> {
    pub path: &'a Path,
}

impl Effect for Trash<'_> {
    const KIND: &'static str = "trash";
    type Reply = PathBuf;
    fn describe(&self) -> String {
        format!("move {} to the trash", self.path.display())
    }
    fn writes(&self) -> bool {
        true
    }
    fn perform(&self, cx: &mut Ctx<'_>) -> Result<PathBuf, String> {
        cx.out.trash(self.path)
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
    fn writes(&self) -> bool {
        true
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
    fn writes(&self) -> bool {
        true
    }
    fn perform(&self, cx: &mut Ctx<'_>) -> Result<(), String> {
        cx.out.shot(self.0)
    }
}

// -- what the process keeps of them --------------------------------------------
//
// An in-memory effect writes nothing, and for a long time that also meant
// nobody could look at one: a connect that failed lived exactly as long as
// the string it returned. The ring fixes that without touching the rule —
// it keeps a *description* of the last few, in memory, and the log reads it
// through SQL beside the queue.

/// How many in-memory effects the ring keeps. A sync pass and the
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
#[derive(Debug, Clone, Serialize)]
pub struct MemEffect {
    /// Its place in the ring, counting up for the life of the process. The
    /// log carries it **negated** — see [`LOG_FROM`].
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
/// shares — the UI's and each worker's — so the log shows what the sync
/// thread reached for as readily as what the keyboard did. It is also why
/// this is `Mutex` and not `RefCell`: the writers are threads.
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

    /// Teaches one connection the `mem_effects()` function [`LOG_FROM`]
    /// reads the ring through. Every reader gets it at open, which is what
    /// makes the ring queryable from a `query_only` connection at all —
    /// nothing is written anywhere, the rows are handed to SQLite on the
    /// spot.
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
    /// it (a backoff, or the send window) — unix seconds, the world's clock.
    pub created: f64,
    pub updated: f64,
    pub not_before: f64,
    /// The sentence, for a row that carries no payload to derive one from:
    /// a ring row's [`MemEffect::what`]. `None` on a filed job, whose
    /// sentence the registry decodes ([`Registry::describe`]).
    pub what: Option<String>,
    /// Whether the world changed for it ([`Effect::writes`]). What the
    /// panel opens narrowed to, because a sync pass asks a dozen questions
    /// for every answer it acts on.
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

/// Which of the due jobs an executor pass may claim.
///
/// The pass used to claim every due row, whoever ran it — and that is wrong
/// the moment a job needs something only *one* thread holds. An account's
/// IMAP session lives in that account's sync worker's [`Real`] and nowhere
/// else, so the sender thread, waking on its own timer, would take a `move`,
/// fail it with "not connected", burn an attempt and leave it sitting out a
/// backoff — a round trip the worker beside it could have made at once. A
/// pass now claims only what it can perform.
///
/// A job says what it needs by what it is filed against: an entity of
/// `account:N` needs that account's session, anything else needs none. So a
/// new deferred effect routes itself, and still no central list names it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Scope {
    /// Everything due. The manual pump, where one thread is every thread.
    All,
    /// One account's jobs and nothing else — its worker's, being the thread
    /// that holds its session.
    Account(i64),
    /// Only what needs no session: the sender, whose `submit` carries its
    /// own credentials and opens its own connection to file the copy to
    /// Sent.
    Sessionless,
}

impl Scope {
    /// The claim's extra predicate, and the `?2` it binds.
    fn sql(self) -> (&'static str, Option<String>) {
        match self {
            Scope::All => ("", None),
            Scope::Account(a) => ("AND entity = ?2", Some(format!("account:{a}"))),
            Scope::Sessionless => ("AND (entity IS NULL OR entity NOT LIKE 'account:%')", None),
        }
    }
}

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
pub fn mark(db: &Connection) -> i64 {
    db.query_row("SELECT COALESCE(MAX(id), 0) FROM effect", [], |r| r.get(0))
        .unwrap_or(0)
}

// -- the log as a rich table ---------------------------------------------------
//
// The queue is a table like any other, so the log viewer is the rich table
// over it rather than a widget of its own invention: the same
// filter grammar, the same paging, the same reactive pages — a commit by
// the executor invalidates exactly the pages on screen, so watching a job
// run is invalidation and not polling.
//
// The ring joins it there, in SQL (`LOG_FROM`), for the same reason: an
// in-memory effect that arrived as a second list beside the first would
// need its own filter, its own paging and its own idea of order, and the
// three would drift. As a `UNION ALL` arm it gets the real ones.

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
    // and what went wrong. The payload too — that is where a uid or an
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
        describe: "the effect's verb — move, seen, forwarded, submit",
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
/// exactly the row a human goes looking for. Off the union, so `connect`
/// and `fetch` are on offer the moment a sync has reached for them.
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

/// The effect log's datasource: what the log panel's rich table runs on.
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

/// What the log panel opens with in its filter field.
///
/// A sync pass asks the outside a dozen questions for every answer it acts
/// on — connect, select, search, fetch, and again next minute — so an
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
/// [`Store::write`](crate::store::Store::write) closure that runs on the
/// writer thread — the composition primitive the passes build their jobs
/// from.
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
    /// `run(&Now)`, because it is on every hot path there is — and the one
    /// place the ring is deliberately skipped: the clock is asked several
    /// times a frame, and a ring of clock readings would have room for
    /// nothing a human meant to do. `run(&Now)` still records, for the
    /// caller who genuinely wants that noted.
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

    /// Performs an in-memory effect now and answers it. Nothing is written
    /// — these are the effects nobody would retry or wait for — but the
    /// ring keeps what it was and what it said, so the log can show it
    /// beside the queue. What the ring keeps is [`Effect::describe`], which
    /// never carries a secret; the payload stays where it was, which is
    /// nowhere.
    ///
    /// # Errors
    ///
    /// Whatever the backend said, verbatim.
    pub fn run<E: Effect>(&self, e: &E) -> Result<E::Reply, String> {
        // The seq is taken before the round trip, so the ring orders
        // effects by when they were *asked for*, as the queue's ids do.
        let seq = self.store.mem().next_seq();
        let (at, ran) = {
            let mut out = self.outside.borrow_mut();
            let at = out.now();
            let mut cx = Ctx {
                out: &mut **out,
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
            "INSERT INTO effect(kind, payload, entity, status, idempotent, writes,
                                attempts, not_before, created, updated)
             VALUES(?1, ?2, ?3, 'pending', ?4, ?5, 0, ?6, ?7, ?7)",
            rusqlite::params![
                E::KIND,
                payload,
                e.entity(),
                e.idempotent(),
                e.writes(),
                not_before,
                now
            ],
        )?;
        Ok(tx.last_insert_rowid())
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
        self.run_effects_in(Scope::All)
    }

    /// One executor pass: claim every due job this pass is allowed to run
    /// ([`Scope`]) and perform it. Answers how many were claimed.
    pub fn run_effects_in(&self, scope: Scope) -> usize {
        let now = self.now();
        let (mine, param) = scope.sql();
        let due: Vec<(i64, String, String)> = {
            let sql = format!(
                "SELECT id, kind, payload FROM effect
                 WHERE status='pending' AND not_before <= ?1 {mine} ORDER BY id"
            );
            let Ok(mut stmt) = self.store.conn().prepare(&sql) else {
                return 0;
            };
            fn row(r: &rusqlite::Row<'_>) -> rusqlite::Result<(i64, String, String)> {
                Ok((r.get(0)?, r.get(1)?, r.get(2)?))
            }
            let rows = match &param {
                Some(p) => stmt.query_map(rusqlite::params![now, p], row),
                None => stmt.query_map(rusqlite::params![now], row),
            };
            rows.map(|it| it.filter_map(Result::ok).collect())
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
    fn access_token(&mut self, _e: &str) -> Result<String, String> {
        Self::no("access_token")
    }
    fn bucket_secret_set(&mut self, _k: &str, _s: &str) -> bool {
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
    fn list_dir(&mut self, _d: &Path) -> Result<Vec<crate::files::Entry>, String> {
        Self::no("list_dir")
    }
    fn stat(&mut self, _p: &Path) -> Result<Option<crate::files::Entry>, String> {
        Self::no("stat")
    }
    fn read_file(&mut self, _p: &Path, _max: usize) -> Result<Vec<u8>, String> {
        Self::no("read_file")
    }
    fn open_path(&mut self, _p: &Path) -> Result<(), String> {
        Self::no("open_path")
    }
    fn make_dir(&mut self, _p: &Path) -> Result<(), String> {
        Self::no("make_dir")
    }
    fn copy_path(&mut self, _f: &Path, _t: &Path) -> Result<(), String> {
        Self::no("copy_path")
    }
    fn move_path(&mut self, _f: &Path, _t: &Path) -> Result<(), String> {
        Self::no("move_path")
    }
    fn trash(&mut self, _p: &Path) -> Result<PathBuf, String> {
        Self::no("trash")
    }
    fn file_id(&mut self, _p: &Path) -> Result<Option<crate::files::FileId>, String> {
        Self::no("file_id")
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
    /// Folders reported with `\All` — Gmail's all-mail view, which holds
    /// every message the account has and must never be ingested from.
    pub all_mail: HashSet<String>,
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
            "Archive" | "[Gmail]/All Mail" => Some("archive".into()),
            "Sent" => Some("sent".into()),
            "Spam" | "Junk" => Some("spam".into()),
            "Trash" => Some("trash".into()),
            _ => None,
        }
    }

    /// Reports this folder the way Gmail reports All Mail: the archive
    /// role, played by a view over everything.
    pub fn as_all_mail(&mut self, name: &str) {
        self.all_mail.insert(name.to_string());
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
    /// What `open` handed to the OS.
    pub opened: Vec<PathBuf>,
    /// The demo tree this world's file browser walks — its own copy, which
    /// is what lets a verb that writes run in any number of tests at once.
    pub disk: crate::files::demo::Disk,
    /// Accounts with a live session. A verb that reaches a server without
    /// one is a bug in the pass, and this catches it.
    pub connected: HashSet<i64>,
    /// Unix seconds; only moves when a test moves it.
    pub clock: f64,
    /// When set, every network verb fails with this — the offline test.
    pub down: Option<String>,
    /// Addresses that have signed in with OAuth, and the token the provider
    /// hands back. A fake sign-in is one map entry — no browser, no
    /// loopback, no token endpoint.
    pub grants: HashMap<String, String>,
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

    /// Plants an OAuth grant, as a completed sign-in would: the provider
    /// will hand out `token`, and the server accepts exactly it.
    pub fn grant(&mut self, email: &str, token: &str) {
        self.grants.insert(email.into(), token.into());
        self.secrets.insert(email.into(), token.into());
    }

    /// Revokes a grant, as a human clicking "remove access" at the provider
    /// would: the refresh fails, and every session with it dies.
    pub fn revoke(&mut self, email: &str) {
        self.grants.remove(email);
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

/// The demo tree as a disk, in the panels' spelling: what a fake world
/// serves, and what a real one serves under `--demo-disk` — the panels
/// library's fixture, a machine-independent `~` a suite can address a row
/// of by name. Written as well as read: the verbs act on the
/// fixture exactly as they act on the filesystem, so a suite proves them.
///
/// The one translation each of its verbs needs: an outside is handed real
/// paths, and the fixture is keyed by the spelling the panels show.
fn demo(path: &Path) -> String {
    crate::files::display_path(path)
}

impl Outside for Fake {
    fn now(&mut self) -> f64 {
        self.clock
    }

    fn connect(&mut self, account: i64, c: &Creds) -> Result<(), String> {
        if let Some(e) = &self.down {
            return Err(e.clone());
        }
        if self.secrets.get(&c.user).map(String::as_str) != Some(c.auth.secret()) {
            return Err("authentication failed".into());
        }
        self.connected.insert(account);
        Ok(())
    }

    fn folders(&mut self, account: i64) -> Result<Vec<RemoteFolder>, String> {
        let s = self.live(account)?;
        let mut names: Vec<String> = s.folders.keys().cloned().collect();
        let all = s.all_mail.clone();
        names.sort();
        Ok(names
            .into_iter()
            .map(|n| RemoteFolder {
                role: FakeServer::role_of(&n),
                all_mail: all.contains(&n),
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
        if self.secrets.get(&c.user).map(String::as_str) != Some(c.auth.secret()) {
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
        if m.attachments.is_empty() {
            raw += &format!("\r\n{}", m.body);
        } else {
            // The same envelope the real transport writes (see [`rfc822`]),
            // by hand: what is filed to Sent has to carry the parts, or a
            // sent mail that syncs back would lose them on the way home.
            raw += "MIME-Version: 1.0\r\nContent-Type: multipart/mixed; boundary=\"fake\"\r\n\r\n";
            raw += &format!(
                "--fake\r\nContent-Type: text/plain; charset=utf-8\r\n\r\n{}\r\n",
                m.body
            );
            for p in &m.attachments {
                raw += &format!(
                    "--fake\r\nContent-Type: {}\r\nContent-Disposition: attachment; filename=\"{}\"\r\nContent-Transfer-Encoding: base64\r\n\r\n{}\r\n",
                    p.mime,
                    p.name,
                    crate::html::base64_encode(&p.bytes)
                );
            }
            raw += "--fake--\r\n";
        }
        // Whichever account owns this address; the first server otherwise.
        let acct = *self.servers.keys().next().unwrap_or(&1);
        self.server(acct).submitted.push(m.clone());
        Ok(raw.into_bytes())
    }

    fn secret_get(&mut self, email: &str) -> Option<String> {
        self.secrets.get(email).cloned()
    }

    fn access_token(&mut self, email: &str) -> Result<String, String> {
        if let Some(e) = &self.down {
            return Err(e.clone());
        }
        self.grants
            .get(email)
            .cloned()
            .ok_or_else(|| format!("google: no grant for {email} (invalid_grant)"))
    }

    fn secret_set(&mut self, email: &str, pass: &str) -> bool {
        self.secrets.insert(email.to_string(), pass.to_string());
        true
    }

    fn bucket_secret_set(&mut self, key_id: &str, secret: &str) -> bool {
        self.secrets.insert(format!("r2/{key_id}"), secret.to_string());
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

    // The demo tree, in the panels' spelling: a fake world's disk, read
    // and written.
    fn list_dir(&mut self, dir: &Path) -> Result<Vec<crate::files::Entry>, String> {
        self.disk.list(&demo(dir))
    }

    fn stat(&mut self, path: &Path) -> Result<Option<crate::files::Entry>, String> {
        Ok(self.disk.entry(&demo(path)))
    }

    /// What was written here, if anything was — a fake disk is writable,
    /// which is what lets a test say *this file changed since* — and the
    /// demo tree otherwise.
    fn read_file(&mut self, path: &Path, max: usize) -> Result<Vec<u8>, String> {
        match self.files.get(path) {
            Some(b) => Ok(b[..b.len().min(max)].to_vec()),
            None => self.disk.read(&demo(path), max),
        }
    }

    fn open_path(&mut self, path: &Path) -> Result<(), String> {
        self.opened.push(path.to_path_buf());
        Ok(())
    }

    fn make_dir(&mut self, path: &Path) -> Result<(), String> {
        let now = self.clock;
        self.disk.make_dir(&demo(path), now)
    }

    fn copy_path(&mut self, from: &Path, to: &Path) -> Result<(), String> {
        self.disk.copy(&demo(from), &demo(to))
    }

    fn move_path(&mut self, from: &Path, to: &Path) -> Result<(), String> {
        self.disk.mv(&demo(from), &demo(to))
    }

    fn trash(&mut self, path: &Path) -> Result<PathBuf, String> {
        let now = self.clock;
        self.disk
            .trash(&demo(path), now)
            .map(|p| crate::files::real_path(&p))
    }

    fn file_id(&mut self, path: &Path) -> Result<Option<crate::files::FileId>, String> {
        Ok(self.disk.id(&demo(path)))
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
    if m.attachments.is_empty() {
        return b.body(m.body.clone()).map_err(|e| s(&e));
    }
    // With parts it is a `multipart/mixed`: the letter first, then each
    // file as its own `Content-Disposition: attachment`. A type lettre
    // will not parse falls back to the one every reader accepts rather
    // than failing the send over a label.
    use lettre::message::{Attachment, MultiPart, SinglePart};
    let mut mp = MultiPart::mixed().singlepart(SinglePart::plain(m.body.clone()));
    for p in &m.attachments {
        let ctype = header::ContentType::parse(&p.mime)
            .or_else(|_| header::ContentType::parse("application/octet-stream"))
            .map_err(|e| s(&e))?;
        mp = mp.singlepart(Attachment::new(p.name.clone()).body(p.bytes.clone(), ctype));
    }
    b.multipart(mp).map_err(|e| s(&e))
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

    /// Unix seconds. Public because a thread that has no [`World`] — a
    /// Gmail sign-in waiting on the browser — still needs the app's clock
    /// rather than the wall's.
    #[must_use]
    pub fn read(&self) -> f64 {
        match self {
            Clock::System => std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_secs_f64())
                .unwrap_or(0.0),
            Clock::Virtual(t) => t.lock().map(|g| *g).unwrap_or(0.0),
        }
    }
}

/// `EXDEV` — "cross-device link", what a `rename` between two filesystems
/// answers. The same number on macOS and on linux, and not worth a libc
/// dependency to name.
const EXDEV: i32 = 18;

/// What a real copy and a real move ask of a destination before they write
/// anything: nothing is there. `std::fs::rename` and `std::fs::copy` both
/// replace silently, and this app never does — a clash is refused on the
/// panel's status line, and undo is free to move a path back without
/// wondering what it lands on.
fn free(to: &Path) -> Result<(), String> {
    match std::fs::symlink_metadata(to) {
        Ok(_) => Err(format!("{} is already there", to.display())),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(format!("{}: {e}", to.display())),
    }
}

/// A file, a symlink as itself, or a directory with everything under it.
/// The recursion is the tree's depth, which is the disk's own bound.
///
/// Nothing else: a FIFO, a socket or a device node is refused by name.
/// `fs::copy` opens what it is given, and opening a FIFO blocks until
/// somebody writes to the other end — the browser performs its verbs on
/// the frame of the click, so that would be the window stopped for good.
fn copy_tree(from: &Path, to: &Path) -> Result<(), CopyFail> {
    let meta = std::fs::symlink_metadata(from).map_err(CopyFail::before)?;
    let ft = meta.file_type();
    if ft.is_symlink() {
        // Copied as a link, the way `cp -R` does it: following it would
        // duplicate what it points at, which is not what was asked for.
        let target = std::fs::read_link(from).map_err(CopyFail::before)?;
        return std::os::unix::fs::symlink(target, to).map_err(CopyFail::before);
    }
    if ft.is_file() {
        return copy_file(from, to);
    }
    if !ft.is_dir() {
        return Err(CopyFail::before(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            format!("{} is not a file or a directory", from.display()),
        )));
    }
    // `create_dir` is the atomic claim on the name, and the line either
    // side of which the destination becomes ours to clean up.
    std::fs::create_dir(to).map_err(CopyFail::before)?;
    let walk = || -> std::io::Result<()> {
        for ent in std::fs::read_dir(from)? {
            let ent = ent?;
            copy_tree(&ent.path(), &to.join(ent.file_name())).map_err(|f| f.err)?;
        }
        // The mode last, and only once the children are in: a directory
        // the source kept to itself must not land 0755 in a shared parent,
        // and setting 0700 (or 0500) before the walk would lock the walk
        // out of its own destination.
        std::fs::set_permissions(to, meta.permissions())
    };
    walk().map_err(CopyFail::after)
}

/// A copy that did not finish, and the one thing the caller cannot work
/// out for itself afterwards: whether the destination root standing there
/// is **ours** — created by this call — or somebody else's, which we ran
/// into. Only the first may be swept away.
struct CopyFail {
    err: std::io::Error,
    made_root: bool,
}

impl CopyFail {
    /// Failed before the destination was claimed: whatever is at that
    /// name, if anything, belongs to somebody else.
    fn before(err: std::io::Error) -> CopyFail {
        CopyFail {
            err,
            made_root: false,
        }
    }

    /// Failed after: the destination is ours, half-made.
    fn after(err: std::io::Error) -> CopyFail {
        CopyFail {
            err,
            made_root: true,
        }
    }
}

/// One file's bytes, written **through the descriptor that claimed the
/// name**. `create_new` is `O_EXCL` — an atomic claim — but reopening `to`
/// by name afterwards (which is what `fs::copy` does) gives a racer room
/// to unlink our empty file and leave a file or a symlink of their own for
/// the second open to truncate. The descriptor cannot be swapped under us,
/// so the bytes and the mode go through it.
fn copy_file(from: &Path, to: &Path) -> Result<(), CopyFail> {
    let mut src = std::fs::File::open(from).map_err(CopyFail::before)?;
    let mut dst = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(to)
        .map_err(CopyFail::before)?;
    std::io::copy(&mut src, &mut dst).map_err(CopyFail::after)?;
    let mode = src.metadata().map_err(CopyFail::after)?.permissions();
    dst.set_permissions(mode).map_err(CopyFail::after)?;
    Ok(())
}

/// Whether `to` is `from` itself or somewhere under it — asked of the
/// **resolved** paths, which is the question a comparison of two spellings
/// cannot answer. An alias in the middle (`~/link` pointing at
/// `~/Downloads`) makes a copy feed itself its own output otherwise: the
/// walk creates entries in the directory it is still reading.
///
/// Only a real directory can contain anything, so a symlink source answers
/// *no*: it is copied as a link and recurses into nothing. A destination
/// that does not exist yet — the ordinary case — is resolved through its
/// parent.
fn inside(from: &Path, to: &Path) -> bool {
    if std::fs::symlink_metadata(from).is_ok_and(|m| !m.is_dir()) {
        return false;
    }
    let Ok(src) = from.canonicalize() else {
        return false;
    };
    let dst = match to.canonicalize() {
        Ok(p) => p,
        Err(_) => match (to.parent().map(Path::canonicalize), to.file_name()) {
            (Some(Ok(parent)), Some(name)) => parent.join(name),
            _ => return false,
        },
    };
    dst.starts_with(&src)
}

/// Takes back something this process made and nobody has seen: the
/// half-tree of a copy that failed, or the far side of a cross-volume move
/// that could not finish. **Not** the trash: a path the user never had is
/// not a deletion, and filling the trash with failures would be the ruder
/// of the two. Never called on anything but a destination this call
/// created — see [`CopyFail::made_root`].
fn sweep(path: &Path) -> std::io::Result<()> {
    let ft = std::fs::symlink_metadata(path)?.file_type();
    if ft.is_dir() {
        std::fs::remove_dir_all(path)
    } else {
        std::fs::remove_file(path)
    }
}

/// `RENAME_EXCL` from macOS' `sys/stdio.h`: fail if the destination exists.
#[cfg(target_os = "macos")]
const RENAME_EXCL: std::ffi::c_uint = 0x0000_0004;

/// A rename that refuses an existing destination, where the platform has
/// one. Plain `rename(2)` replaces silently, and checking first is a
/// window another program can write into — so on macOS this is
/// `renamex_np`, whose exclusion is the kernel's.
#[cfg(target_os = "macos")]
#[allow(unsafe_code)]
fn rename_excl(from: &Path, to: &Path) -> std::io::Result<()> {
    use std::ffi::{c_char, c_int, c_uint, CString};
    use std::os::unix::ffi::OsStrExt;

    extern "C" {
        fn renamex_np(from: *const c_char, to: *const c_char, flags: c_uint) -> c_int;
    }

    let f = CString::new(from.as_os_str().as_bytes())?;
    let t = CString::new(to.as_os_str().as_bytes())?;
    // SAFETY: two NUL-terminated paths that outlive the call, and the one
    // flag the man page defines for it.
    if unsafe { renamex_np(f.as_ptr(), t.as_ptr(), RENAME_EXCL) } == 0 {
        Ok(())
    } else {
        Err(std::io::Error::last_os_error())
    }
}

/// Other platforms cannot make this rename exclusive. [`free`] checks the
/// destination first, but another write could still win the race.
#[cfg(not(target_os = "macos"))]
fn rename_excl(from: &Path, to: &Path) -> std::io::Result<()> {
    std::fs::rename(from, to)
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
/// `LOGIN` with an app password — fastmail-style — or SASL `XOAUTH2` with a
/// bearer token, which is how Gmail is reached), lettre over rustls for
/// submission, and the platform for everything else.
pub struct Real {
    sessions: HashMap<i64, imap_session::Imap>,
    secrets: Secrets,
    clock: Clock,
    /// Where the store lives: the OAuth client registration sits beside it
    /// (see [`crate::oauth::Client::load`]). `None` for an in-memory run,
    /// which then has no Gmail either.
    dir: Option<PathBuf>,
    /// Access tokens by address, with the unix second each expires at.
    /// Per-process and never written down: a token is worth an hour, and
    /// the refresh that mints one is a network round trip no connect
    /// should pay twice.
    tokens: HashMap<String, (String, f64)>,
    /// The disk this outside reads is the **demo tree**, not the machine's
    /// (`--demo-disk`). Everything else stays real: an e2e run's file
    /// browser then walks the same fixture the panels library shows — a
    /// suite can address a row by name — while its screenshots, its
    /// network and its keychain are the ones a run always had.
    /// The demo tree, when a run asked for it — otherwise the real disk.
    demo: Option<crate::files::demo::Disk>,
    /// Whether the file browser's verbs may write at all. Off for an e2e
    /// script replayed against a real disk: a suite must no more delete a
    /// human's files than write to their keychain, and the refusal lands
    /// on the status line where a forgotten `--demo-disk` is a failing
    /// step rather than a trip to the trash.
    writes: bool,
}

/// How early a cached access token is treated as spent, so a long sync
/// started with 3 seconds left does not die halfway through.
const TOKEN_MARGIN: f64 = 120.0;

impl Real {
    #[must_use]
    pub fn new(secrets: Secrets, clock: Clock) -> Real {
        // The keychain variant already knows where the store lives; an
        // in-memory one has nowhere to read a client registration from —
        // and neither does a keychain one with no store file, whose path is
        // the empty default.
        let dir = match &secrets {
            Secrets::Keychain(d) if !d.as_os_str().is_empty() => Some(d.clone()),
            _ => None,
        };
        Real {
            sessions: HashMap::new(),
            secrets,
            clock,
            dir,
            tokens: HashMap::new(),
            demo: None,
            writes: true,
        }
    }

    /// The same outside with the demo tree for a disk (see [`Real`]).
    #[must_use]
    pub fn with_demo_disk(mut self) -> Real {
        self.demo = Some(crate::files::demo::Disk::new());
        self
    }

    /// The same outside with the file browser's verbs that write refused
    /// (see the `writes` field).
    #[must_use]
    pub fn read_only_disk(mut self) -> Real {
        self.writes = false;
        self
    }

    /// The one sentence every refused write gives.
    fn sealed<T>() -> Result<T, String> {
        Err("this run may not write to the disk — a script wants --demo-disk".into())
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
        let s = match imap_session::connect(&c.host, &c.user, &c.auth) {
            Ok(s) => s,
            Err(e) => {
                // A bearer token the server refused is spent, whatever the
                // cache still believes about its hour: a revoked grant kills
                // the access token too. Drop it so the next pass mints a
                // fresh one — otherwise signing in again would fix nothing
                // until the cached token aged out on its own.
                if matches!(c.auth, Auth::Bearer(_)) {
                    self.tokens.remove(&c.user);
                }
                return Err(e);
            }
        };
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
        use lettre::transport::smtp::authentication::{Credentials, Mechanism};
        use lettre::{SmtpTransport, Transport};
        let s = |e: &dyn std::fmt::Display| format!("{e}");
        let msg = rfc822(&c.user, m)?;
        let raw = msg.formatted();
        let mut relay = SmtpTransport::relay(&c.host)
            .map_err(|e| s(&e))?
            .credentials(Credentials::new(
                c.user.clone(),
                c.auth.secret().to_string(),
            ));
        // A bearer token is not a password: offered PLAIN, Gmail's SMTP
        // rejects it. Pin the mechanism rather than letting lettre pick by
        // what the server advertises.
        if matches!(c.auth, Auth::Bearer(_)) {
            relay = relay.authentication(vec![Mechanism::Xoauth2]);
        }
        let t = relay.build();
        if let Err(e) = t.send(&msg) {
            // The same rule as `connect`: a bearer token the server refused
            // is spent, and the sender thread holds a cache of its own — so
            // without this, every retry of this send would re-offer the dead
            // token until it aged out.
            if matches!(c.auth, Auth::Bearer(_)) {
                self.tokens.remove(&c.user);
            }
            return Err(s(&e));
        }
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

    fn access_token(&mut self, email: &str) -> Result<String, String> {
        let now = self.clock.read();
        if let Some((tok, until)) = self.tokens.get(email) {
            if now + TOKEN_MARGIN < *until {
                return Ok(tok.clone());
            }
        }
        let dir = self
            .dir
            .clone()
            .ok_or("this run has no store directory, so no google client")?;
        let client = crate::oauth::Client::load(&dir)?;
        let refresh = self
            .secret_get(&crate::oauth::refresh_key(email))
            .ok_or_else(|| format!("{email} has no google grant — sign in again"))?;
        let (tok, until) = crate::oauth::refresh(&client, crate::oauth::GOOGLE, &refresh, now)?;
        self.tokens
            .insert(email.to_string(), (tok.clone(), until));
        Ok(tok)
    }

    fn bucket_secret_set(&mut self, key_id: &str, secret: &str) -> bool {
        match &self.secrets {
            Secrets::Keychain(dir) => crate::secret::set_bucket_secret(dir, key_id, secret),
            // An e2e run never writes to a human's keychain. The replication
            // worker reads the platform store directly (it has no World), so
            // a suite that "connects" proves the form and the file, not a
            // live bucket — which is the only half a suite could prove.
            Secrets::Memory(m) => m
                .0
                .lock()
                .map(|mut g| g.insert(format!("r2/{key_id}"), secret.to_string()))
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

    fn list_dir(&mut self, dir: &Path) -> Result<Vec<crate::files::Entry>, String> {
        if let Some(d) = &self.demo {
            return d.list(&demo(dir));
        }
        let rd = std::fs::read_dir(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
        let mut out = Vec::new();
        for ent in rd.flatten() {
            let name = ent.file_name().to_string_lossy().into_owned();
            // A link is listed as what it points at while that exists,
            // as itself otherwise; an entry that cannot be read at all is
            // left out rather than failing the listing.
            let meta = match std::fs::metadata(ent.path()).or_else(|_| ent.metadata()) {
                Ok(m) => m,
                Err(_) => continue,
            };
            out.push(crate::files::Entry::from_metadata(&name, &meta));
        }
        crate::files::sort(&mut out);
        Ok(out)
    }

    fn stat(&mut self, path: &Path) -> Result<Option<crate::files::Entry>, String> {
        if let Some(d) = &self.demo {
            return Ok(d.entry(&demo(path)));
        }
        // A link is answered as what it points at while that exists, and as
        // itself otherwise — the rule [`Outside::list_dir`] lists by. A
        // dangling link is a row the panel is showing, so a verb that
        // called it absent would refuse a source that is right there and
        // plan a destination the boundary then refuses in other words.
        let found = std::fs::metadata(path).or_else(|e| match e.kind() {
            std::io::ErrorKind::NotFound => std::fs::symlink_metadata(path),
            _ => Err(e),
        });
        match found {
            Ok(m) => {
                let name = path
                    .file_name()
                    .map(|n| n.to_string_lossy().into_owned())
                    .unwrap_or_else(|| "/".into());
                Ok(Some(crate::files::Entry::from_metadata(&name, &m)))
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(format!("{}: {e}", path.display())),
        }
    }

    fn read_file(&mut self, path: &Path, max: usize) -> Result<Vec<u8>, String> {
        if let Some(d) = &self.demo {
            return d.read(&demo(path), max);
        }
        use std::io::Read;
        let f = std::fs::File::open(path).map_err(|e| format!("{}: {e}", path.display()))?;
        let mut buf = Vec::new();
        f.take(max as u64)
            .read_to_end(&mut buf)
            .map_err(|e| format!("{}: {e}", path.display()))?;
        Ok(buf)
    }

    /// macOS: `/usr/bin/open`, the same door the Finder uses — the OS
    /// picks the viewer, and nothing runs under our name. Elsewhere there
    /// is no opener yet (android wants a FileProvider).
    fn open_path(&mut self, path: &Path) -> Result<(), String> {
        // A demo path names a file only the fixture has: nothing is handed
        // to the OS, and the card's `open` still answers.
        if self.demo.is_some() {
            return Ok(());
        }
        #[cfg(target_os = "macos")]
        {
            let status = std::process::Command::new("/usr/bin/open")
                .arg(path)
                .status()
                .map_err(|e| format!("open: {e}"))?;
            if status.success() {
                Ok(())
            } else {
                Err(format!("open refused {} ({status})", path.display()))
            }
        }
        #[cfg(not(target_os = "macos"))]
        {
            let _ = path;
            Err("no opener on this platform".into())
        }
    }

    fn make_dir(&mut self, path: &Path) -> Result<(), String> {
        if !self.writes {
            return Self::sealed();
        }
        if let Some(d) = &mut self.demo {
            let now = self.clock.read();
            return d.make_dir(&demo(path), now);
        }
        // `create_dir`, never `create_dir_all`: `new dir` makes the one
        // directory it named, and a typo is a refusal rather than a tree.
        std::fs::create_dir(path).map_err(|e| format!("{}: {e}", path.display()))
    }

    fn copy_path(&mut self, from: &Path, to: &Path) -> Result<(), String> {
        if !self.writes {
            return Self::sealed();
        }
        if let Some(d) = &mut self.demo {
            return d.copy(&demo(from), &demo(to));
        }
        if inside(from, to) {
            return Err(format!("{} cannot go inside itself", from.display()));
        }
        free(to)?;
        let Err(f) = copy_tree(from, to) else {
            return Ok(());
        };
        let err = format!("{}: {}", from.display(), f.err);
        // Only what this call made is this call's to clean up. A
        // destination that was already there when we reached for it — a
        // racer got the name between [`free`] and the claim — is somebody
        // else's object, and sweeping it would be the overwrite this
        // whole path exists to prevent.
        if !f.made_root {
            return Err(err);
        }
        // A half-made copy is nobody's: not the copy that was asked for,
        // and not anything a panel is showing. It is removed rather than
        // trashed — a tree we made and no one has seen is not a deletion,
        // and littering the trash with failures would be the rudeness of
        // the two. Its source is untouched throughout.
        match sweep(to) {
            Ok(()) => Err(err),
            Err(e) => Err(format!(
                "{err} — and a part of it was left at {}: {e}",
                to.display()
            )),
        }
    }

    fn move_path(&mut self, from: &Path, to: &Path) -> Result<(), String> {
        if !self.writes {
            return Self::sealed();
        }
        if let Some(d) = &mut self.demo {
            return d.mv(&demo(from), &demo(to));
        }
        if inside(from, to) {
            return Err(format!("{} cannot go inside itself", from.display()));
        }
        // The check is for the sentence; the exclusion is the kernel's.
        free(to)?;
        match rename_excl(from, to) {
            Ok(()) => Ok(()),
            // Somebody took the name between the check and the rename.
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
                Err(format!("{} is already there", to.display()))
            }
            // EXDEV: the two paths are on different filesystems, where a
            // rename cannot reach. Copy, then trash the source — so even
            // the halfway state of a cross-volume move is recoverable.
            Err(e) if e.raw_os_error() == Some(EXDEV) => {
                self.copy_path(from, to)?;
                match self.trash(from) {
                    Ok(_) => Ok(()),
                    // The copy stands but the source did not go, which is
                    // not a move — and the caller is about to be told this
                    // failed, so it will record no node for the copy that
                    // would be left behind. Take it back off the disk: the
                    // move either happened or it did not.
                    Err(why) => Err(match sweep(to) {
                        Ok(()) => why,
                        Err(e) => format!("{why} — and the copy was left at {}: {e}", to.display()),
                    }),
                }
            }
            Err(e) => Err(format!("{}: {e}", from.display())),
        }
    }

    /// macOS: `NSFileManager`'s own `trashItemAtURL:`, the door the Finder
    /// uses — the right trash for the volume the file is on, a name that
    /// does not clash, and Put Back where the Finder shows it. Never a
    /// `remove_file`: undo has to be able to move it back.
    fn trash(&mut self, path: &Path) -> Result<PathBuf, String> {
        if !self.writes {
            return Self::sealed();
        }
        if let Some(d) = &mut self.demo {
            let now = self.clock.read();
            return d
                .trash(&demo(path), now)
                .map(|p| crate::files::real_path(&p));
        }
        #[cfg(target_os = "macos")]
        {
            crate::mac::trash(path)
        }
        #[cfg(not(target_os = "macos"))]
        {
            let _ = path;
            Err("no trash on this platform".into())
        }
    }

    fn file_id(&mut self, path: &Path) -> Result<Option<crate::files::FileId>, String> {
        if let Some(d) = &self.demo {
            return Ok(d.id(&demo(path)));
        }
        // `symlink_metadata`, never `metadata`: a link replaced by a link
        // to the same target is a different object, and following the link
        // would report the target's identity for both.
        use std::os::unix::fs::MetadataExt;
        match std::fs::symlink_metadata(path) {
            Ok(m) => Ok(Some(crate::files::FileId {
                dev: m.dev(),
                ino: m.ino(),
            })),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(format!("{}: {e}", path.display())),
        }
    }

    fn as_any(&mut self) -> &mut dyn std::any::Any {
        self
    }
}

/// The `imap` crate, wrapped. Stateful (a selected mailbox), so `ensure`
/// suppresses redundant SELECTs — that optimisation stays private.
/// Which of the five roles a mailbox plays, from its name and its RFC 6154
/// special-use attributes (rendered — see the caller).
///
/// `\All` is the Gmail case, and it is why archive is not just `\Archive`:
/// Gmail advertises no archive mailbox, because archiving there *is*
/// dropping the inbox label, leaving the message in All Mail — which is
/// exactly what a MOVE into it does. A real `\Archive` wins where a server
/// has one (fastmail does), and `\All` is the fallback.
///
/// `\Junk` is spelled `spam` here, which is what every mail client and
/// every server that is not the RFC calls it — and what the panel is
/// titled.
fn role_for(name: &str, attrs: &[String]) -> (Option<String>, bool) {
    let has = |want: &str| attrs.iter().any(|a| a == want);
    let role = if name.eq_ignore_ascii_case("inbox") {
        "inbox"
    } else if has("Archive") || has("All") {
        "archive"
    } else if has("Sent") {
        "sent"
    } else if has("Junk") {
        "spam"
    } else if has("Trash") {
        "trash"
    } else {
        return (None, false);
    };
    // `\All` without a real `\Archive` beside it: the archive role is being
    // played by an all-mail view, and the caller must not ingest from it.
    (Some(role.to_string()), role == "archive" && !has("Archive"))
}

mod imap_session {
    use super::{Auth, FolderMeta, MailFlag, RemoteFolder, RemoteMail, UidSet};
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

    /// The SASL exchange for `AUTHENTICATE XOAUTH2`.
    ///
    /// Two challenges, not one, and they mean opposite things. The first is
    /// empty: the server's invitation, answered with the envelope (the
    /// crate base64s what `process` returns, so this hands it over in the
    /// clear). Any **second** challenge is Google saying no, and it carries
    /// the reason as base64 JSON — the protocol then wants an *empty*
    /// response to acknowledge it, after which the server sends the tagged
    /// `NO`. Answering that one with the envelope again, as a single-shot
    /// authenticator does, throws the reason away and leaves the human
    /// holding "no response [AUTHENTICATION FAILED]" — which says nothing
    /// about a missing scope or a mailbox with IMAP switched off.
    ///
    /// So the refusal is kept, and [`connect`] speaks it.
    struct XOAuth2 {
        user: String,
        token: String,
        /// What Google said when it refused, verbatim.
        refused: std::cell::RefCell<Option<String>>,
    }

    impl imap::Authenticator for XOAuth2 {
        type Response = String;
        fn process(&self, challenge: &[u8]) -> String {
            if challenge.is_empty() {
                return crate::oauth::xoauth2(&self.user, &self.token);
            }
            *self.refused.borrow_mut() = Some(String::from_utf8_lossy(challenge).into_owned());
            String::new()
        }
    }

    pub fn connect(host: &str, user: &str, auth: &Auth) -> Result<Imap, String> {
        let client = imap::ClientBuilder::new(host, 993).connect().map_err(s)?;
        let session = match auth {
            Auth::Password(pass) => client.login(user, pass).map_err(|e| s(e.0))?,
            Auth::Bearer(token) => {
                let sasl = XOAuth2 {
                    user: user.to_string(),
                    token: token.clone(),
                    refused: std::cell::RefCell::new(None),
                };
                match client.authenticate("XOAUTH2", &sasl) {
                    Ok(session) => session,
                    Err((e, _)) => {
                        let why = sasl.refused.into_inner();
                        return Err(match why {
                            Some(w) => format!("{}: {}", s(e), crate::oauth::refusal(&w)),
                            None => s(e),
                        });
                    }
                }
            }
        };
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
                // The attributes as whole `Debug` renderings, one per entry:
                // `imap` does not re-export `NameAttribute`, so the variants
                // cannot be named here, and matching a rendering entire is
                // what keeps an `Extension("...")` that merely spells one of
                // these words from passing for it.
                let attrs: Vec<String> =
                    n.attributes().iter().map(|a| format!("{a:?}")).collect();
                let (role, all_mail) = super::role_for(n.name(), &attrs);
                out.push(RemoteFolder {
                    role,
                    all_mail,
                    name: n.name().to_string(),
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
        /// The account whose session it would need, if any — what
        /// [`Scope`] routes on.
        acct: Option<i64>,
    }

    impl Poke {
        fn ok(note: &str) -> Poke {
            Poke { note: note.into(), fails: false, idem: true, wanted: true, acct: None }
        }
    }

    impl Effect for Poke {
        const KIND: &'static str = "poke";
        type Reply = String;
        fn describe(&self) -> String {
            format!("poke {}", self.note)
        }
        fn writes(&self) -> bool {
            true
        }
        fn entity(&self) -> Option<String> {
            Some(match self.acct {
                Some(a) => format!("account:{a}"),
                None => format!("panel:{}", self.note.len()),
            })
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
            attachments: Vec::new(),
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

    /// A mail that carries something goes out as a `multipart/mixed`: the
    /// letter first, then each part with its name and its type — and the
    /// round trip through the parser gets the same parts back, which is the
    /// only assertion that means anything.
    #[test]
    fn a_mail_that_carries_something_goes_out_as_multipart() {
        let plain = Outgoing {
            to: "a@b.c".into(),
            subject: "here it is".into(),
            body: "the numbers".into(),
            in_reply_to: None,
            references: Vec::new(),
            attachments: Vec::new(),
        };
        let bare = String::from_utf8(rfc822("me@b.c", &plain).unwrap().formatted()).unwrap();
        assert!(!bare.contains("multipart"), "nothing to carry, nothing to wrap");

        let carrying = Outgoing {
            attachments: vec![
                Part {
                    name: "q3.csv".into(),
                    mime: "text/csv".into(),
                    bytes: b"line,aug\ncdn,640\n".to_vec(),
                },
                Part {
                    name: "sketch.png".into(),
                    mime: "image/png".into(),
                    bytes: vec![0x89, b'P', b'N', b'G', 1, 2, 3],
                },
            ],
            ..plain
        };
        let raw = rfc822("me@b.c", &carrying).unwrap().formatted();
        let p = crate::sync::parse_mail(&raw);
        assert_eq!(p.body, "the numbers");
        let got: Vec<(String, String, u64)> = p
            .attachments
            .iter()
            .map(|a| (a.name.clone(), a.mime.clone(), a.size))
            .collect();
        assert_eq!(
            got,
            [
                ("q3.csv".to_string(), "text/csv".to_string(), 17),
                ("sketch.png".to_string(), "image/png".to_string(), 7),
            ]
        );
        // …and the bytes come back by part index, which is what the card
        // reads them with.
        assert_eq!(
            crate::sync::part_bytes(&raw, p.attachments[1].at).as_deref(),
            Some(&[0x89, b'P', b'N', b'G', 1, 2, 3][..])
        );
        // A type nothing can parse does not fail the send: the part goes
        // out labelled as the bytes it is.
        let odd = Outgoing {
            attachments: vec![Part {
                name: "x".into(),
                mime: "not a media type".into(),
                bytes: b"x".to_vec(),
            }],
            ..carrying
        };
        assert!(rfc822("me@b.c", &odd).is_ok());
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

    /// A pass claims only what it can perform. An account's jobs are its
    /// own worker's — the one thread holding its session — and a
    /// sessionless pass beside it takes the rest instead of failing them on
    /// its way past.
    #[test]
    fn a_pass_claims_only_what_it_can_run() {
        let w = world();
        let clips = || w.with_fake(|f| f.clips.clone());
        w.enqueue(&Poke { acct: Some(7), ..Poke::ok("seven") }).unwrap();
        w.enqueue(&Poke { acct: Some(9), ..Poke::ok("nine") }).unwrap();
        w.enqueue(&Poke::ok("free")).unwrap();

        assert_eq!(w.run_effects_in(Scope::Sessionless), 1, "the unbound job alone");
        assert_eq!(clips(), vec!["free"]);

        assert_eq!(w.run_effects_in(Scope::Account(7)), 1);
        assert_eq!(clips(), vec!["free", "seven"], "its own, and only its own");

        assert_eq!(w.run_effects_in(Scope::Account(7)), 0, "never account 9's");
        assert_eq!(w.run_effects_in(Scope::All), 1, "the manual pump takes it");
        assert_eq!(clips(), vec!["free", "seven", "nine"]);

        // And nothing was failed on the way past: three jobs, three dones.
        assert!(w.jobs().iter().all(|j| j.status == "done"), "{:?}", w.jobs());
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
        let c = Creds::password("h", "u", "s3cret");
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
            acct: None,
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

    /// An in-memory effect leaves no row, and the log shows it anyway: the
    /// ring keeps the last few, and the table's `FROM` is the queue and the
    /// ring joined. Filed and unfiled sit in one list, in the order they
    /// happened, and `@memory` / `@filed` are what tell them apart.
    #[test]
    fn the_log_joins_the_ring_to_the_queue() {
        let w = world();
        w.enqueue(&Poke::ok("filed")).unwrap();
        w.with_fake(|f| f.clock += 10.0);
        w.run(&Clip {
            text: "hello",
            what: "a note",
        })
        .unwrap();

        let rows = log(&w, "");
        assert_eq!(rows.len(), 2, "the queue's row and the ring's");
        assert_eq!(rows[0].kind, "clip", "newest first, across both");
        assert!(rows[0].transient());
        assert!(!rows[1].transient());
        assert_eq!(rows[0].what.as_deref(), Some("copy a note (5 bytes)"));
        assert_eq!(rows[0].status_line(), "done · in memory");

        // The queue's own readers still see the queue and nothing else:
        // a ring row was never filed, and `jobs()` is what a test asserts
        // the *queue* with.
        assert_eq!(w.jobs().len(), 1);

        assert_eq!(log(&w, "@memory").len(), 1);
        assert_eq!(log(&w, "@filed").len(), 1);
        assert_eq!(log(&w, "@kind:clip").len(), 1);
        // The sentence is searchable: it is all a row with no payload has.
        assert_eq!(log(&w, "a note").len(), 1);
        // Nobody was going to retry it, so it is not "risky" — the column
        // is `NULL` rather than a plausible zero. And "not risky" is the
        // complement of that, so it keeps the ring row rather than losing
        // it to `NOT NULL`.
        assert_eq!(log(&w, "@risky").len(), 0);
        assert_eq!(log(&w, "@not:risky").len(), 2);
        assert_eq!(log(&w, "@live").len(), 1);

        // …and the title a tab strip shows reads the log, so a ring row
        // wears its verb instead of a number nothing answers to.
        assert_eq!(
            crate::mail::title(w.store(), &crate::core::Kind::Job { id: rows[0].id }),
            "clip"
        );
        assert_eq!(
            crate::mail::title(w.store(), &crate::core::Kind::Job { id: -9999 }),
            "effect · gone"
        );
    }

    /// What the outside refused is the whole reason the ring exists: before
    /// it, the sentence lived exactly as long as the `Err` it returned.
    #[test]
    fn the_ring_keeps_what_was_refused() {
        let w = World::new(
            Rc::new(Store::open(None).expect("in-memory store")),
            Box::new(Deny::default()),
            Registry::new(),
        );
        assert!(w.run(&Clip { text: "x", what: "a note" }).is_err());

        let rows = log(&w, "");
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].status, "failed");
        assert_eq!(
            rows[0].error.as_deref(),
            Some("this world has no outside (clip)")
        );
        assert_eq!(log(&w, "@failed").len(), 1);

        // …and it opens like any other row of the log, by its own id.
        let j = job(w.store(), rows[0].id).expect("the ring row reads back by id");
        assert_eq!(j, rows[0]);
        assert!(j.payload.is_empty(), "there was never a payload to keep");
    }

    /// The log tells apart what changed the world from what only asked it,
    /// on both sides of the union — and the panel opens on the first,
    /// because a sync pass asks a dozen questions for every answer.
    #[test]
    fn the_log_tells_a_write_from_a_read() {
        let w = world();
        w.enqueue(&Poke::ok("filed")).unwrap(); // a poke reaches the clipboard
        // Each on its own instant, so "newest first" is not a tie.
        w.with_fake(|f| f.clock += 1.0);
        w.run(&Clip { text: "x", what: "a note" }).unwrap();
        w.with_fake(|f| f.clock += 1.0);
        w.run(&SecretGet("elena@fastmail.com")).unwrap();
        w.with_fake(|f| f.clock += 1.0);
        w.run(&Now).unwrap();

        assert_eq!(log(&w, "").len(), 4, "everything, unfiltered");
        let wrote = log(&w, LOG_DEFAULT);
        assert_eq!(wrote.len(), 2, "the job and the clipboard write");
        assert!(wrote.iter().all(|j| j.writes));
        assert_eq!(wrote[0].kind, "clip");
        assert_eq!(wrote[1].kind, "poke");

        let read = log(&w, "@read");
        assert_eq!(read.len(), 2);
        assert!(read.iter().all(|j| !j.writes));
        // Both halves are total: a row is one or the other, never neither.
        assert_eq!(log(&w, "@not:wrote").len(), 2);

        // The ring's reads join the queue's writes under one filter, so
        // `@wrote @memory` is the in-memory half of what was changed.
        assert_eq!(log(&w, "@wrote @memory").len(), 1);
        assert_eq!(log(&w, "@wrote @filed").len(), 1);
    }

    /// The column is on the row, not derived from the kind: the log filters
    /// a queue written by a build whose effects this one may not have.
    #[test]
    fn the_queue_keeps_what_an_effect_was() {
        let w = world();
        w.enqueue(&Poke::ok("filed")).unwrap();
        let filed: i64 = w
            .store()
            .conn()
            .query_row("SELECT writes FROM effect", [], |r| r.get(0))
            .expect("the column is written at enqueue");
        assert_eq!(filed, 1);
    }

    /// The pages that read the ring go stale when it moves. There is no
    /// commit to notice here — nothing was written — so this is the whole
    /// of the mechanism: the ring's version is its `data_version`, and the
    /// log's spec names the dependency the authorizer cannot see.
    #[test]
    fn a_ring_that_moves_stales_the_pages_that_read_it() {
        let w = world();
        assert_eq!(log(&w, "").len(), 0, "the page is cached empty");

        w.run(&Clip { text: "x", what: "a note" }).unwrap();
        assert_eq!(log(&w, "").len(), 1, "and re-runs once the ring moved");
    }

    /// The ring is the process's, not one reader's: a second store over the
    /// same writer — which is what every worker thread holds — sees what
    /// this one recorded, on the same poll that catches a foreign commit.
    #[test]
    fn another_reader_sees_the_ring_on_its_next_poll() {
        let w = world();
        let other = Store::with_db(w.store().db()).expect("a second reader");
        assert_eq!(
            other
                .rows_sql_deps(
                    "test",
                    "the ring, from another reader",
                    &format!("SELECT COUNT(*) FROM {LOG_FROM}"),
                    &[],
                    LOG_DEPS,
                    |r| r.get::<_, i64>(0),
                )
                .first()
                .copied(),
            Some(0)
        );

        w.run(&Clip { text: "x", what: "a note" }).unwrap();
        assert!(other.poll_external(), "the ring moved under it");
        assert_eq!(
            other
                .rows_sql_deps(
                    "test",
                    "the ring, from another reader",
                    &format!("SELECT COUNT(*) FROM {LOG_FROM}"),
                    &[],
                    LOG_DEPS,
                    |r| r.get::<_, i64>(0),
                )
                .first()
                .copied(),
            Some(1)
        );
    }

    /// The ring is bounded, and it drops from the old end.
    #[test]
    fn the_ring_keeps_only_the_last_few() {
        let w = world();
        for i in 0..KEPT + 5 {
            w.run(&Clip {
                text: "x",
                what: "a note",
            })
            .unwrap();
            // Each on its own instant, so "newest" is not a tie.
            w.with_fake(|f| f.clock += 1.0);
            let _ = i;
        }
        assert_eq!(w.store().mem().len(), KEPT);
        let rows = log(&w, "");
        assert_eq!(rows.len(), KEPT);
        // The ids count up for the life of the process, negated: the five
        // that fell off are the five nearest zero.
        assert_eq!(rows[0].id, -(KEPT as i64 + 5));
        assert_eq!(rows[KEPT - 1].id, -6);
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

    /// The special-use mapping, against what the two servers this app is
    /// actually pointed at advertise. Gmail is the reason `\All` counts:
    /// it names no `\Archive`, so without this an archive on a Gmail
    /// account would find no folder to move to and quietly do nothing.
    #[test]
    fn gmails_all_mail_is_the_archive() {
        let a = |v: &[&str]| v.iter().map(|s| (*s).to_string()).collect::<Vec<_>>();
        let role = |n: &str, at: &[&str]| {
            let (r, all) = role_for(n, &a(at));
            (r.unwrap_or_default(), all)
        };

        // Gmail's LIST, as it comes. All Mail plays archive — and is
        // flagged, because it is a view over everything rather than a
        // folder, so the sync pass must not ingest from it.
        assert_eq!(role("INBOX", &["HasNoChildren"]), ("inbox".into(), false));
        assert_eq!(
            role("[Gmail]/All Mail", &["HasNoChildren", "All"]),
            ("archive".into(), true)
        );
        assert_eq!(
            role("[Gmail]/Sent Mail", &["HasNoChildren", "Sent"]),
            ("sent".into(), false)
        );
        // \Junk is spam, whatever the folder is called.
        assert_eq!(role("[Gmail]/Spam", &["HasNoChildren", "Junk"]), ("spam".into(), false));
        assert_eq!(role("Junk", &["Junk"]), ("spam".into(), false));
        assert_eq!(
            role("[Gmail]/Trash", &["HasNoChildren", "Trash"]),
            ("trash".into(), false)
        );
        assert_eq!(role("[Gmail]", &["NoSelect", "HasChildren"]), (String::new(), false));

        // A real \Archive is a folder like any other: it takes the role and
        // it *is* ingested — and it wins over \All beside it.
        assert_eq!(role("Archive", &["Archive"]), ("archive".into(), false));
        assert_eq!(
            role("Everything", &["All", "Archive"]),
            ("archive".into(), false)
        );

        // A plain folder is no role, and an extension attribute that merely
        // spells one of the words is not that role.
        assert_eq!(role("Receipts", &["HasNoChildren"]), (String::new(), false));
        assert_eq!(
            role("Odd", &[r#"Extension("All")"#]),
            (String::new(), false)
        );
    }

    /// The real disk, on a scratch tree of its own: a file copied, a
    /// directory copied whole, a symlink copied as a symlink, a taken
    /// destination refused rather than written over, and a move that
    /// empties where it came from. The demo disk answers the same
    /// questions in `files`; this is the backend that actually touches a
    /// filesystem.
    #[test]
    fn the_real_outside_copies_and_moves_on_disk() {
        let stamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| d.as_nanos());
        let root = std::env::temp_dir().join(format!("superapp-disk-{stamp}"));
        std::fs::create_dir_all(root.join("src/deep")).unwrap();
        std::fs::write(root.join("src/a.txt"), b"a").unwrap();
        std::fs::write(root.join("src/deep/b.txt"), b"b").unwrap();
        std::os::unix::fs::symlink("a.txt", root.join("src/link")).unwrap();

        use std::os::unix::fs::PermissionsExt;
        let mut out = Real::new(Secrets::Memory(MemSecrets::new()), Clock::System);
        // `new dir` makes the one directory it named, and refuses a taken
        // name rather than adopting what is there.
        out.make_dir(&root.join("dest")).unwrap();
        assert!(out.make_dir(&root.join("dest")).is_err());

        // A file, then the whole tree beside it.
        out.copy_path(&root.join("src/a.txt"), &root.join("dest/a.txt"))
            .unwrap();
        assert_eq!(std::fs::read(root.join("dest/a.txt")).unwrap(), b"a");
        assert!(
            out.copy_path(&root.join("src/a.txt"), &root.join("dest/a.txt"))
                .is_err(),
            "a copy never writes over anything"
        );
        out.copy_path(&root.join("src"), &root.join("dest/src"))
            .unwrap();
        assert_eq!(
            std::fs::read(root.join("dest/src/deep/b.txt")).unwrap(),
            b"b"
        );
        assert!(
            std::fs::symlink_metadata(root.join("dest/src/link"))
                .unwrap()
                .file_type()
                .is_symlink(),
            "a link is copied as a link, not as what it points at"
        );

        // A directory keeps the mode it had: a 0700 source that landed
        // 0755 would expose its children to anyone who can read the
        // parent.
        std::fs::set_permissions(
            root.join("src/deep"),
            std::fs::Permissions::from_mode(0o700),
        )
        .unwrap();
        out.copy_path(&root.join("src/deep"), &root.join("dest/deep"))
            .unwrap();
        assert_eq!(
            std::fs::metadata(root.join("dest/deep"))
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o700
        );

        // A dangling link is a path the browser lists, so it is a path the
        // verbs agree is there: `stat` answers the link itself once what it
        // pointed at has gone.
        std::os::unix::fs::symlink(root.join("src/gone.txt"), root.join("dangle")).unwrap();
        assert!(
            out.stat(&root.join("dangle")).unwrap().is_some(),
            "listed, so not absent"
        );
        assert!(out.stat(&root.join("nothing-at-all")).unwrap().is_none());

        // A move empties where it came from, and refuses a taken
        // destination — `rename` would have replaced it silently.
        out.move_path(&root.join("dest/a.txt"), &root.join("dest/moved.txt"))
            .unwrap();
        assert!(!root.join("dest/a.txt").exists());
        assert_eq!(std::fs::read(root.join("dest/moved.txt")).unwrap(), b"a");
        assert!(out
            .move_path(&root.join("src/a.txt"), &root.join("dest/moved.txt"))
            .is_err());

        // An alias in the middle is still inside: a copy that fed itself
        // its own output would grow until the disk did not.
        std::os::unix::fs::symlink(root.join("src"), root.join("alias")).unwrap();
        assert!(
            out.copy_path(&root.join("src"), &root.join("alias/again"))
                .is_err(),
            "resolved, not spelled"
        );
        assert!(out
            .move_path(&root.join("src"), &root.join("alias/again"))
            .is_err());
        // …but the alias itself copies fine: it is a link, and a link
        // recurses into nothing.
        out.copy_path(&root.join("alias"), &root.join("dest/alias"))
            .unwrap();

        // The exclusive rename is the kernel's, not a check of ours: it
        // refuses a destination that is there, whoever put it there.
        assert!(rename_excl(&root.join("dest/moved.txt"), &root.join("src/a.txt")).is_err());
        assert_eq!(
            std::fs::read(root.join("src/a.txt")).unwrap(),
            b"a",
            "untouched"
        );

        // Nothing that is not a file, a directory or a link: opening a
        // FIFO blocks until somebody writes to it, and the window with it.
        // What the walk had already made is swept — removed, not trashed:
        // it is a tree this call created and nobody ever saw, so the test
        // leaves nothing behind it either.
        std::process::Command::new("/usr/bin/mkfifo")
            .arg(root.join("src/pipe"))
            .status()
            .unwrap();
        assert!(out
            .copy_path(&root.join("src"), &root.join("dest/pipes"))
            .is_err());
        assert!(
            !root.join("dest/pipes").exists(),
            "the half-copy was swept, not left"
        );
        // …but a destination that was already somebody else's is not ours
        // to sweep, however the copy failed.
        std::fs::create_dir(root.join("dest/theirs")).unwrap();
        std::fs::write(root.join("dest/theirs/keep.txt"), b"keep").unwrap();
        assert!(out
            .copy_path(&root.join("src"), &root.join("dest/theirs"))
            .is_err());
        assert_eq!(
            std::fs::read(root.join("dest/theirs/keep.txt")).unwrap(),
            b"keep",
            "somebody else's directory stands"
        );
        std::fs::remove_file(root.join("src/pipe")).unwrap();

        // …and a run that may not write to a real disk refuses all of it.
        let mut sealed =
            Real::new(Secrets::Memory(MemSecrets::new()), Clock::System).read_only_disk();
        assert!(sealed.make_dir(&root.join("nope")).is_err());
        assert!(sealed
            .copy_path(&root.join("src/a.txt"), &root.join("nope.txt"))
            .is_err());
        assert!(sealed
            .move_path(&root.join("src/a.txt"), &root.join("nope.txt"))
            .is_err());
        assert!(sealed.trash(&root.join("src/a.txt")).is_err());
        assert!(!root.join("nope").exists());

        std::fs::remove_dir_all(&root).unwrap();
    }
}
