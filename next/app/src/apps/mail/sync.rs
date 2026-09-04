//! The background passes: one sync worker per account, and one sender.
//!
//! `message` records what the person wants; `server_msg` records what the
//! server last said. Differences become queued jobs, which check again before
//! running. Network work always finishes before a database transaction
//! begins.

use std::collections::HashSet;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use kernel::app::{Wake, Worker};
use kernel::caps::Secrets;
use kernel::effect::{Job, World};
use kernel::richtable::date_span;
use kernel::store::Store;
use kernel::time::fmt_date;
use rusqlite::Transaction;

use super::caps::{Creds, RemoteMail, UidSet};
use super::effects::{account_entity, Connect, Fetch, Folders, Meta, Move, Seen, Submit, Uids};
use super::model::{self, topic_of};

/// How many most-recent messages a folder retains on first contact (and after
/// a UIDVALIDITY reset). Bounded coverage, stated honestly.
pub const FETCH_CAP: u32 = 200;

/// How long a worker sleeps between kicks — and, since the kernel drives
/// every pass from the frame loop under virtual time, how long a pass waits
/// before it looks outside again (see [`SyncPass`]).
const POLL: Duration = Duration::from_secs(60);

// -- one account's pass -----------------------------------------------------------

/// One full sync pass for one account: connect, **push first** (queue what
/// the server must be told), then mirror folders, fetch what is new, and
/// reconcile facts.
///
/// # Errors
///
/// If the session cannot be opened, or a folder's round trips fail.
pub fn sync_account(w: &World, account: i64) -> Result<(), String> {
    connect(w, account)?;
    push_account(w, account)?;
    fetch_account(w, account)
}

/// Opens the account's session from its row plus the keychain.
///
/// # Errors
///
/// If the account has no host, no password, or the server refuses.
pub fn connect(w: &World, account: i64) -> Result<(), String> {
    let (email, host): (String, String) = w
        .store()
        .conn()
        .query_row(
            "SELECT email, COALESCE(imap_host, '') FROM account WHERE id = ?1",
            [account],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .map_err(|e| e.to_string())?;
    if host.is_empty() {
        return Err("account has no imap host".into());
    }
    let creds = w.with_cap::<dyn Secrets, Result<Creds, String>>(|s| {
        model::creds_for(s, &email, &host)
    })??;
    w.run(&Connect { account, creds })
}

/// The push pass: every message whose intent differs from the server — folder
/// or read state — becomes a job, unless one is already in flight for it. No
/// network here at all.
///
/// # Errors
///
/// If the store cannot be read or the jobs cannot be filed.
pub fn push_account(w: &World, account: i64) -> Result<(), String> {
    let err = |e: rusqlite::Error| e.to_string();
    struct Row {
        message: i64,
        uid: u32,
        want_folder: i64,
        want_name: String,
        have_name: String,
        moving: bool,
        unread: bool,
        seen: bool,
    }
    let rows: Vec<Row> = {
        let db = w.store().conn();
        let mut stmt = db
            .prepare(
                "SELECT m.id, s.uid, m.folder, fw.name, fh.name,
                        m.folder != s.folder, m.unread, s.seen
                 FROM message m
                 JOIN server_msg s ON s.message = m.id
                 JOIN folder fw ON fw.id = m.folder
                 JOIN folder fh ON fh.id = s.folder
                 WHERE m.account = ?1 AND s.uid IS NOT NULL
                   AND (m.folder != s.folder OR m.unread = s.seen)",
            )
            .map_err(err)?;
        let it = stmt
            .query_map([account], |r| {
                Ok(Row {
                    message: r.get(0)?,
                    uid: r.get::<_, i64>(1)? as u32,
                    want_folder: r.get(2)?,
                    want_name: r.get(3)?,
                    have_name: r.get(4)?,
                    moving: r.get(5)?,
                    unread: r.get(6)?,
                    seen: r.get(7)?,
                })
            })
            .map_err(err)?;
        it.collect::<rusqlite::Result<Vec<_>>>().map_err(err)?
    };

    for p in rows {
        let message = p.message;
        if p.moving {
            // The job is encoded *outside* the write: the payload and the
            // clock read need the `World`, which cannot travel to the writer
            // thread. The `in_flight` guard stays inside the transaction, so
            // the claim is still atomic.
            let job = w
                .prepare(&Move {
                    account,
                    message: p.message,
                    to_folder: p.want_folder,
                    from: p.have_name.clone(),
                    to: p.want_name.clone(),
                    uid: p.uid,
                })
                .map_err(err)?;
            w.store()
                .write(move |tx| {
                    if !in_flight(tx, "move", message)? {
                        job.insert(tx)?;
                    }
                    Ok(())
                })
                .map_err(err)?;
            // The flag push waits for the move to re-establish identity — a
            // uid in the old folder means nothing in the new one.
            continue;
        }
        if p.unread == p.seen {
            let job = w
                .prepare(&Seen {
                    account,
                    message: p.message,
                    folder: p.have_name.clone(),
                    uid: p.uid,
                    seen: !p.unread,
                })
                .map_err(err)?;
            w.store()
                .write(move |tx| {
                    if !in_flight(tx, "seen", message)? {
                        job.insert(tx)?;
                    }
                    Ok(())
                })
                .map_err(err)?;
        }
    }
    Ok(())
}

/// Is a job of this kind already queued or running for this message? The
/// payload is JSON *text*, so `->>` reads into it without a schema change.
fn in_flight(tx: &Transaction, kind: &str, message: i64) -> rusqlite::Result<bool> {
    tx.query_row(
        "SELECT 1 FROM effect
         WHERE kind = ?1 AND status IN ('pending', 'processing')
           AND payload ->> 'message' = ?2",
        rusqlite::params![kind, message],
        |_| Ok(()),
    )
    .map(|()| true)
    .or_else(|e| match e {
        rusqlite::Error::QueryReturnedNoRows => Ok(false),
        other => Err(other),
    })
}

/// Everything one folder's round trips produced, before anything is
/// committed.
struct Gathered {
    fid: i64,
    reset: bool,
    uidvalidity: u32,
    uidnext: u32,
    mails: Vec<RemoteMail>,
    server: HashSet<u32>,
    unseen: HashSet<u32>,
}

/// The fetch/reconcile pass: for each folder, gather over the network, then
/// commit once.
///
/// # Errors
///
/// If any round trip fails, or the commit does.
pub fn fetch_account(w: &World, account: i64) -> Result<(), String> {
    let err = |e: rusqlite::Error| e.to_string();
    for rf in w.run(&Folders { account })? {
        let Some(role) = rf.role.clone() else { continue };

        // The folder row and what we last knew about it — a short write, no
        // network in sight. Owned copies cross to the writer thread; the
        // originals stay for the round trips below.
        let name = rf.name.clone();
        let (fid, known): (i64, (Option<i64>, Option<i64>)) = w
            .store()
            .write(move |tx| {
                let fid: i64 = tx
                    .query_row(
                        "SELECT id FROM folder WHERE account = ?1 AND name = ?2",
                        rusqlite::params![account, name],
                        |r| r.get(0),
                    )
                    .or_else(|_| {
                        tx.execute(
                            "INSERT INTO folder(account, name, role) VALUES(?1, ?2, ?3)",
                            rusqlite::params![account, name, role],
                        )
                        .map(|_| tx.last_insert_rowid())
                    })?;
                let known = tx.query_row(
                    "SELECT uidvalidity, uidnext FROM folder WHERE id = ?1",
                    [fid],
                    |r| Ok((r.get(0)?, r.get(1)?)),
                )?;
                Ok((fid, known))
            })
            .map_err(err)?;

        let meta = w.run(&Meta {
            account,
            folder: rf.name.clone(),
        })?;

        let floor = u32::max(1, meta.uidnext.saturating_sub(FETCH_CAP));
        // The server renumbered (or this is first contact): local copies of
        // this folder are meaningless, so start over inside the window.
        let reset = known.0 != Some(i64::from(meta.uidvalidity));
        let from = if reset {
            floor
        } else {
            known.1.map_or(floor, |n| n as u32)
        };

        // Gather. Every round trip happens here, with nothing held open.
        let mails = if meta.uidnext > from {
            w.run(&Fetch {
                account,
                folder: rf.name.clone(),
                from,
            })?
        } else {
            Vec::new()
        };
        let search = |which: UidSet| {
            w.run(&Uids {
                account,
                folder: rf.name.clone(),
                which,
            })
        };
        let server = search(UidSet::All)?;
        let unseen = search(UidSet::Unseen)?;

        // Commit. One transaction, no network.
        let g = Gathered {
            fid,
            reset,
            uidvalidity: meta.uidvalidity,
            uidnext: meta.uidnext,
            mails,
            server,
            unseen,
        };
        w.store()
            .write(move |tx| land(tx, account, from, &g))
            .map_err(err)?;
    }
    Ok(())
}

/// The commit half of one folder's pass.
fn land(tx: &Transaction, account: i64, from: u32, g: &Gathered) -> rusqlite::Result<()> {
    if g.reset {
        tx.execute(
            "DELETE FROM message WHERE id IN
               (SELECT message FROM server_msg WHERE folder = ?1)",
            [g.fid],
        )?;
        tx.execute("DELETE FROM server_msg WHERE folder = ?1", [g.fid])?;
    }
    for m in &g.mails {
        if m.uid < from {
            continue; // `from:*` quirk: a lone highest message
        }
        ingest_message(tx, account, g.fid, m)?;
    }
    tx.execute(
        "UPDATE folder SET uidvalidity = ?1, uidnext = ?2 WHERE id = ?3",
        rusqlite::params![g.uidvalidity, g.uidnext, g.fid],
    )?;

    // Reconcile facts over the retained window, by the *server's* view.
    // Divergent intent stays local truth: an unpushed read or archive is never
    // clobbered, only recorded.
    struct Local {
        id: i64,
        uid: u32,
        seen: bool,
        unread: bool,
    }
    let local: Vec<Local> = {
        let mut stmt = tx.prepare(
            "SELECT m.id, s.uid, s.seen, m.unread
             FROM server_msg s JOIN message m ON m.id = s.message
             WHERE s.folder = ?1 AND s.uid IS NOT NULL",
        )?;
        let rows = stmt.query_map([g.fid], |r| {
            Ok(Local {
                id: r.get(0)?,
                uid: r.get::<_, i64>(1)? as u32,
                seen: r.get(2)?,
                unread: r.get(3)?,
            })
        })?;
        rows.collect::<rusqlite::Result<Vec<_>>>()?
    };
    for l in local {
        let id = l.id;
        if !g.server.contains(&l.uid) {
            // Gone upstream (deleted, or moved beyond our mirror): deletion
            // wins, divergent intent included.
            tx.execute("DELETE FROM message WHERE id = ?1", [id])?;
            tx.execute("DELETE FROM server_msg WHERE message = ?1", [id])?;
            continue;
        }
        let now_seen = !g.unseen.contains(&l.uid);
        if now_seen != l.seen {
            tx.execute(
                "UPDATE server_msg SET seen = ?1 WHERE message = ?2",
                rusqlite::params![now_seen, id],
            )?;
            // Clean rows (intent agrees with the old server state) follow the
            // server; divergent intent will be pushed over it instead.
            if l.unread != l.seen {
                tx.execute(
                    "UPDATE message SET unread = ?1 WHERE id = ?2",
                    rusqlite::params![!now_seen, id],
                )?;
            }
        }
    }
    Ok(())
}

/// Parses and stores one fetched message. A moved mail whose new uid the
/// server never told us (no COPYUID) is **adopted** by Message-ID instead of
/// duplicated.
fn ingest_message(
    tx: &Transaction,
    account: i64,
    folder: i64,
    m: &RemoteMail,
) -> rusqlite::Result<()> {
    let exists: bool = tx
        .query_row(
            "SELECT 1 FROM server_msg WHERE folder = ?1 AND uid = ?2",
            rusqlite::params![folder, m.uid],
            |_| Ok(true),
        )
        .unwrap_or(false);
    if exists {
        return Ok(());
    }
    let p = parse_mail(&m.raw);
    if !p.message_id.is_empty() {
        // A uid-less twin in this account is the same mail, post-move.
        let orphan: Option<i64> = tx
            .query_row(
                "SELECT m.id FROM message m JOIN server_msg s ON s.message = m.id
                 WHERE m.account = ?1 AND m.message_id = ?2 AND s.uid IS NULL",
                rusqlite::params![account, p.message_id],
                |r| r.get(0),
            )
            .ok();
        if let Some(id) = orphan {
            tx.execute(
                "UPDATE server_msg SET folder = ?1, uid = ?2, seen = ?3 WHERE message = ?4",
                rusqlite::params![folder, m.uid, !m.unread, id],
            )?;
            return Ok(());
        }
    }
    tx.execute(
        "INSERT INTO message(account, folder, from_name, from_email, subject, date,
                             unread, body, message_id, topic)
         VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10)",
        rusqlite::params![
            account,
            folder,
            p.from_name,
            p.from_email,
            p.subject,
            p.date,
            m.unread,
            p.body,
            p.message_id,
            p.topic,
        ],
    )?;
    let id = tx.last_insert_rowid();
    tx.execute(
        "INSERT INTO server_msg(message, folder, uid, seen) VALUES(?1, ?2, ?3, ?4)",
        rusqlite::params![id, folder, m.uid, !m.unread],
    )?;
    // Which conversation it belongs to — decided here, in the same
    // transaction, so no draw ever sees an unthreaded mail.
    model::thread_tx(tx, account, id, &p.message_id, &p.references)?;
    Ok(())
}

// -- the wire form ------------------------------------------------------------------

/// What panels need out of a letter's bytes.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct ParsedMail {
    pub from_name: String,
    pub from_email: String,
    pub subject: String,
    pub date: f64,
    pub body: String,
    /// The Message-ID header, angle brackets off — move adoption, and the
    /// identity threading walks.
    pub message_id: String,
    /// `References` ∪ `In-Reply-To`, brackets off, header order, deduped:
    /// every conversation this mail claims to belong to.
    pub references: Vec<String>,
    /// The subject with its reply and forward prefixes stripped.
    pub topic: String,
}

/// Headers, then a blank line, then the letter.
///
/// A plain-text reader for the plain-text mail the prototype's own server
/// writes: no MIME, no encodings, and a date in the one spelling
/// [`header_date`](super::seed::header_date) produces. A real transport
/// brings a real parser with it.
#[must_use]
pub fn parse_mail(raw: &[u8]) -> ParsedMail {
    let text = String::from_utf8_lossy(raw).replace("\r\n", "\n");
    let (head, body) = match text.find("\n\n") {
        Some(at) => (&text[..at], text[at + 2..].trim_end().to_string()),
        None => (text.as_str(), String::new()),
    };
    // Continuation lines belong to the header above them.
    let mut fields: Vec<(String, String)> = Vec::new();
    for line in head.lines() {
        if line.starts_with([' ', '\t']) {
            if let Some((_, v)) = fields.last_mut() {
                v.push(' ');
                v.push_str(line.trim());
            }
            continue;
        }
        if let Some((k, v)) = line.split_once(':') {
            fields.push((k.trim().to_ascii_lowercase(), v.trim().to_string()));
        }
    }
    let field = |name: &str| {
        fields
            .iter()
            .find(|(k, _)| k == name)
            .map(|(_, v)| v.as_str())
            .unwrap_or_default()
    };

    let (from_name, from_email) = address_of(field("from"));
    let subject = {
        let s = field("subject");
        if s.is_empty() {
            "(no subject)".to_string()
        } else {
            s.to_string()
        }
    };
    let mut references: Vec<String> = Vec::new();
    for id in ids_of(field("references"))
        .into_iter()
        .chain(ids_of(field("in-reply-to")))
    {
        if !references.contains(&id) {
            references.push(id);
        }
    }
    ParsedMail {
        from_name,
        from_email,
        topic: topic_of(&subject),
        subject,
        date: date_span(field("date")).map_or(0.0, |(start, _)| start),
        body,
        message_id: norm_id(field("message-id")),
        references,
    }
}

/// `Name <addr>`, or a bare address. A sender without a name is stored under
/// their address as the name, so nothing has to fall back later.
fn address_of(v: &str) -> (String, String) {
    let (name, email) = match (v.find('<'), v.rfind('>')) {
        (Some(a), Some(b)) if a < b => (v[..a].trim(), v[a + 1..b].trim()),
        _ => ("", v.trim()),
    };
    let email = email.to_string();
    let name = name.trim_matches('"').trim();
    if name.is_empty() {
        (email.clone(), email)
    } else {
        (name.to_string(), email)
    }
}

/// One id out of an id header, as threading compares it: trimmed, and without
/// the angle brackets a well-formed one wears.
fn norm_id(s: &str) -> String {
    s.trim()
        .trim_start_matches('<')
        .trim_end_matches('>')
        .trim()
        .to_string()
}

/// The ids in an id-list header (`References`, `In-Reply-To`).
fn ids_of(v: &str) -> Vec<String> {
    v.split_whitespace()
        .map(norm_id)
        .filter(|s| !s.is_empty())
        .collect()
}

// -- the sender ----------------------------------------------------------------------

/// One pass over the outbox: claim everything due and queue it. Answers how
/// many rows were claimed. Also reconciles rows whose job has given up, so a
/// permanent failure reaches the problems panel.
pub fn outbox_pass(w: &World) -> usize {
    let now = w.now();
    let due: Vec<i64> = {
        let Ok(mut stmt) = w.store().conn().prepare(
            "SELECT id FROM outbox WHERE status = 'pending' AND send_after <= ?1 ORDER BY id",
        ) else {
            return 0;
        };
        stmt.query_map([now], |r| r.get(0))
            .map(|it| it.filter_map(Result::ok).collect())
            .unwrap_or_default()
    };

    let mut claimed = 0;
    for id in due {
        // Encode before the write, because `World` cannot cross to the writer
        // thread.
        let Ok(job) = w.prepare(&Submit { outbox: id }) else {
            continue;
        };
        // The claim: one winner between this pass and a concurrent undo,
        // whose reversal only deletes the row while it is 'pending'.
        let won = w
            .store()
            .write(move |tx| {
                let n = tx.execute(
                    "UPDATE outbox SET status = 'sending' WHERE id = ?1 AND status = 'pending'",
                    [id],
                )?;
                if n == 1 {
                    job.insert(tx)?;
                }
                Ok(n)
            })
            .unwrap_or(0);
        claimed += won;
    }

    // A job that has given up leaves its outbox row stranded at 'sending'.
    // Derive the failure back onto the row rather than teaching the effect
    // machinery about outboxes.
    let _ = w.store().write(|tx| {
        tx.execute(
            "UPDATE outbox SET status = 'failed',
                    error = (SELECT e.error FROM effect e
                             WHERE e.kind = 'submit' AND e.status = 'failed'
                               AND e.payload ->> 'outbox' = outbox.id)
             WHERE status = 'sending'
               AND EXISTS (SELECT 1 FROM effect e
                           WHERE e.kind = 'submit' AND e.status = 'failed'
                             AND e.payload ->> 'outbox' = outbox.id)",
            [],
        )
        .map(|_| ())
    });

    claimed
}

// -- the workers -----------------------------------------------------------------------

/// Asks every sync pass to look outside on its next turn, whenever it comes.
/// Bumped rather than set, so a pass that has already looked can tell a new
/// request from the one it answered.
static PULL: AtomicU64 = AtomicU64::new(1);

/// Go and look now: what *sync* on a mailbox's bar means, and what a letter
/// that has just left calls for — the copy the transport files to Sent is
/// not ours to invent.
pub fn pull_now() {
    PULL.fetch_add(1, Ordering::Relaxed);
}

/// One account's sync pass. It holds the only session for its account, so it
/// claims that account's jobs and no other thread does.
///
/// The pass has two halves and they run at different rates. The **push** is
/// local — it reads rows and files jobs, and touches nothing outside — so it
/// runs on every turn, and what a verb has just claimed becomes a job at
/// once. The **pull** asks the outside a dozen questions for every answer,
/// so it runs when it is due or when someone asked: with threads a worker
/// sleeps [`POLL`] between turns and the two are the same thing, but under
/// virtual time the kernel drives every pass from the frame loop, sixty
/// turns to the second, and a pull on each of them would be a torrent —
/// through the network, and through the effect log that records it.
pub struct SyncPass {
    account: i64,
    /// When the next pull is due, on the world's clock. Zero is "now",
    /// which is what a fresh worker means.
    due: f64,
    /// The [`PULL`] generation this pass has answered.
    seen: u64,
}

impl SyncPass {
    #[must_use]
    pub fn new(account: i64) -> SyncPass {
        SyncPass {
            account,
            due: 0.0,
            seen: PULL.load(Ordering::Relaxed),
        }
    }

    /// Whether this turn looks outside: the interval has run out, or
    /// somebody asked. Consumes the request either way.
    fn pull_due(&mut self, w: &World) -> bool {
        let asked = PULL.load(Ordering::Relaxed);
        let due = asked != self.seen || w.now() >= self.due;
        if due {
            self.seen = asked;
            self.due = w.now() + POLL.as_secs_f64();
        }
        due
    }
}

impl Worker for SyncPass {
    fn name(&self) -> String {
        format!("sync-{}", self.account)
    }

    fn entity(&self) -> Option<String> {
        Some(account_entity(self.account))
    }

    fn claims(&self, job: &Job) -> bool {
        job.entity.as_deref() == Some(account_entity(self.account).as_str())
    }

    fn pass(&mut self, w: &World) -> Wake {
        let account = self.account;
        // The local half, every turn: what a verb has just claimed is a job
        // before the next frame, whoever kicked.
        if !self.pull_due(w) {
            let _ = push_account(w, account);
            return Wake::After(POLL);
        }
        let outcome = sync_account(w, account);
        let status = match &outcome {
            Ok(()) => format!("ok · {}", fmt_date(w.now())),
            Err(e) => format!("error: {e}"),
        };
        let synced = outcome.is_ok().then(|| w.now());
        // Only when it changed: a pass that says the same thing twice would
        // stale every cached query that reads the account row, once a tick.
        let was: Option<String> = w
            .store()
            .conn()
            .query_row("SELECT status FROM account WHERE id = ?1", [account], |r| {
                r.get(0)
            })
            .ok()
            .flatten();
        if was.as_deref() != Some(status.as_str()) {
            let _ = w.store().write(move |c| {
                c.execute(
                    "UPDATE account SET status = ?1, synced = ?2 WHERE id = ?3",
                    rusqlite::params![status, synced, account],
                )
                .map(|_| ())
            });
        }
        Wake::After(POLL)
    }
}

/// The sender: it claims the jobs that need no session, which is every job
/// that is not one account's own.
pub struct SenderPass;

impl Worker for SenderPass {
    fn name(&self) -> String {
        "sender".into()
    }

    fn claims(&self, job: &Job) -> bool {
        !job.entity
            .as_deref()
            .is_some_and(|e| e.starts_with("account:"))
    }

    fn pass(&mut self, w: &World) -> Wake {
        // A letter that has just left changes what is out there: the copy
        // the transport files to Sent is not ours to invent, so the sync
        // pass is asked to go and look for it.
        if outbox_pass(w) > 0 {
            pull_now();
        }
        // Sleep until the next deadline, capped — kicks cut it short.
        let next: Option<f64> = w
            .store()
            .conn()
            .query_row(
                "SELECT MIN(send_after) FROM outbox WHERE status = 'pending'",
                [],
                |r| r.get(0),
            )
            .unwrap_or(None);
        let wait = next.map_or(30.0, |t| (t - w.now()).clamp(0.2, 30.0));
        Wake::After(Duration::from_secs_f64(wait))
    }
}

/// The passes mail wants running now: one per configured account, and the
/// sender. Derived from the store, so an account added later starts a worker
/// without a restart.
#[must_use]
pub fn workers(store: &Store) -> Vec<Box<dyn Worker>> {
    let mut v: Vec<Box<dyn Worker>> = vec![Box::new(SenderPass)];
    let Ok(mut stmt) = store
        .conn()
        .prepare("SELECT id FROM account WHERE COALESCE(imap_host, '') != '' ORDER BY id")
    else {
        return v;
    };
    let accounts: Vec<i64> = stmt
        .query_map([], |r| r.get(0))
        .map(|it| it.filter_map(Result::ok).collect())
        .unwrap_or_default();
    for a in accounts {
        v.push(Box::new(SyncPass::new(a)));
    }
    v
}
