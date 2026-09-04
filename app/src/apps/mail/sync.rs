//! The background passes: one sync worker per account, and one sender.
//!
//! `message` records what the person wants; `server_msg` records what the
//! server last said. Differences become queued jobs, which check again before
//! running. Network work always finishes before a database transaction
//! begins.

use std::collections::HashSet;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;

use kernel::app::{Wake, Worker};
use kernel::caps::{Kicker, Secrets};
use kernel::effect::{Job, World};
use kernel::store::Store;
use kernel::time::fmt_date;
use rusqlite::Transaction;

use super::accounts;
use super::parts;
use super::caps::{Creds, OAuth, RemoteMail, UidSet, Watched};
use super::effects::{
    account_entity, Backfill, Connect, Disconnect, Fetch, Folders, Forwarded, Meta, Move, Seen,
    Submit, Uids, Watch,
};
use super::model::{self, topic_of};

/// How many older messages one pass reaches back for. Nothing is dropped: a
/// folder is mirrored entire, newest first, a batch a turn — the batching is
/// what keeps a first sync from holding a whole mailbox in memory, and what
/// lets new mail land while the past is still arriving.
pub const BACKFILL: usize = 200;

/// How long a worker sleeps between kicks — and, since the kernel drives
/// every pass from the frame loop under virtual time, how long a pass waits
/// before it looks outside again (see [`SyncPass`]).
const POLL: Duration = Duration::from_secs(60);

/// How long one pass spends reaching into a folder's past before it hands
/// the thread back. The batches themselves are cheap; what is not cheap is
/// keeping this account's own jobs — an archive, a mark read — waiting
/// behind the first sync of fifty thousand letters.
const REACH_BUDGET: Duration = Duration::from_secs(20);

/// How soon a pass that ran out of budget comes back for the rest. Not
/// [`POLL`], because there is somewhere to go; not at once either, because
/// the folder discovery and the three searches a batch sits behind are
/// round trips of their own.
const REACH: Duration = Duration::from_secs(5);

/// How long one [`Watch`] waits before it is re-issued. RFC 2177 puts the
/// ceiling at 29 minutes — a server may log out a client that idles longer —
/// and this is well under it on purpose: the wait cannot be interrupted, so
/// it is also how long a watch takes to notice that it has been retired,
/// that the machine woke with a dead socket, or that the account is gone.
pub(super) const WATCH: Duration = Duration::from_secs(5 * 60);

/// How long a watch holds off after a refusal. The interval is still
/// running underneath, so this costs latency, never mail.
const WATCH_RETRY: Duration = Duration::from_secs(60);

// -- one account's pass -----------------------------------------------------------

/// One full sync pass for one account: connect, **push first** (queue what
/// the server must be told), then mirror folders, fetch what is new, and
/// reconcile facts.
///
/// Answers `true` while some folder is still filling in its past, which is
/// what asks [`SyncPass`] for another turn at once rather than in a minute.
///
/// # Errors
///
/// If the session cannot be opened, or a folder's round trips fail.
pub fn sync_account(w: &World, account: i64) -> Result<bool, String> {
    connect(w, account)?;
    push_account(w, account)?;
    fetch_account(w, account)
}

/// Opens the account's session from its row plus the keychain — or, for an
/// account that signed in with Google, plus a freshly minted bearer token.
///
/// # Errors
///
/// If the account has no host, no secret, or the server refuses.
pub fn connect(w: &World, account: i64) -> Result<(), String> {
    let (email, host, bearer): (String, String, bool) = w
        .store()
        .conn()
        .query_row(
            "SELECT email, COALESCE(imap_host, ''), COALESCE(auth, '') = ?2
             FROM account WHERE id = ?1",
            rusqlite::params![account, super::oauth::GOOGLE.name],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )
        .map_err(|e| e.to_string())?;
    if host.is_empty() {
        return Err("account has no imap host".into());
    }
    let creds = creds(w, &email, &host, bearer)?;
    w.run(&Connect { account, creds })
}

/// The credentials for one address, out of this world's own backends: the
/// keychain, and — for a Google account — the token endpoint behind
/// [`OAuth`].
///
/// The two capabilities are taken one at a time because `with_cap` borrows
/// the bag: a bearer sign-in reads no password, and a password sign-in never
/// asks for a token.
///
/// # Errors
///
/// If the secret is missing, or the grant is gone.
pub fn creds(w: &World, email: &str, host: &str, bearer: bool) -> Result<Creds, String> {
    if bearer {
        let token = w.with_cap::<dyn OAuth, _>(|o| o.access_token(email))??;
        return Ok(Creds::bearer(host, email, token));
    }
    w.with_cap::<dyn Secrets, Result<Creds, String>>(|s| accounts::creds_for(s, email, host))?
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
        /// Intent, then fact, for `$Forwarded`.
        forwarded: bool,
        has_forwarded: bool,
        /// Whether the folder it sits in keeps keywords at all.
        keywords: bool,
    }
    let rows: Vec<Row> = {
        let db = w.store().conn();
        let mut stmt = db
            .prepare(
                "SELECT m.id, s.uid, m.folder, fw.name, fh.name,
                        m.folder != s.folder, m.unread, s.seen,
                        m.forwarded, s.forwarded, fh.keywords
                 FROM message m
                 JOIN server_msg s ON s.message = m.id
                 JOIN folder fw ON fw.id = m.folder
                 JOIN folder fh ON fh.id = s.folder
                 WHERE m.account = ?1 AND s.uid IS NOT NULL
                   AND (m.folder != s.folder OR m.unread = s.seen
                        OR m.forwarded != s.forwarded)",
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
                    forwarded: r.get(8)?,
                    has_forwarded: r.get(9)?,
                    keywords: r.get(10)?,
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
        // A server that keeps no keywords is never asked: it would take the
        // STORE and forget it, and the mark would look pushed. The mark stays
        // local truth there, and the next fetch never reads its absence as
        // another client clearing it (see [`land`]).
        if p.forwarded != p.has_forwarded && p.keywords {
            let job = w
                .prepare(&Forwarded {
                    account,
                    message: p.message,
                    folder: p.have_name.clone(),
                    uid: p.uid,
                    on: p.forwarded,
                })
                .map_err(err)?;
            w.store()
                .write(move |tx| {
                    if !in_flight(tx, "forwarded", message)? {
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
    forwarded: HashSet<u32>,
    /// Whether the folder keeps keywords; `forwarded` means nothing when it
    /// does not.
    keywords: bool,
}

/// The fetch/reconcile pass: for each folder, gather over the network, then
/// commit once.
///
/// Answers `true` when a folder handed back a full backfill batch — there is
/// more of its past to come, and the pass that asked should come straight
/// back for it.
///
/// # Errors
///
/// If any round trip fails, or the commit does.
pub fn fetch_account(w: &World, account: i64) -> Result<bool, String> {
    let err = |e: rusqlite::Error| e.to_string();
    let mut more = false;
    for rf in w.run(&Folders { account })? {
        let Some(role) = rf.role.clone() else { continue };

        // The folder row and what we last knew about it — a short write, no
        // network in sight. Owned copies cross to the writer thread; the
        // originals stay for the round trips below.
        let name = rf.name.clone();
        let all_mail = rf.all_mail;
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
                            "INSERT INTO folder(account, name, role, all_mail)
                             VALUES(?1, ?2, ?3, ?4)",
                            rusqlite::params![account, name, role, all_mail],
                        )
                        .map(|_| tx.last_insert_rowid())
                    })?;
                // What the server says now, not what it said the first time:
                // a provider that grows an `\All` view is a fact about the
                // folder, and a move target is decided by it.
                tx.execute(
                    "UPDATE folder SET all_mail = ?2 WHERE id = ?1",
                    rusqlite::params![fid, all_mail],
                )?;
                let known = tx.query_row(
                    "SELECT uidvalidity, uidnext FROM folder WHERE id = ?1",
                    [fid],
                    |r| Ok((r.get(0)?, r.get(1)?)),
                )?;
                Ok((fid, known))
            })
            .map_err(err)?;

        // An all-mail view is a *move target*, not a source. Gmail's
        // `[Gmail]/All Mail` holds every message the account has, inbox
        // included, under different uids — and this store gives a message one
        // folder, so ingesting from it would file a second row for every mail
        // already mirrored from INBOX. The row above exists so archive has
        // somewhere to move to (a MOVE into All Mail is exactly what
        // archiving is on Gmail); the round trips stop here.
        //
        // The cost is stated rather than hidden: mail archived on another
        // device does not appear locally. What this device archives stays,
        // because the push records the move rather than re-reading it.
        if rf.all_mail {
            continue;
        }

        let meta = w.run(&Meta {
            account,
            folder: rf.name.clone(),
        })?;

        // The server renumbered (or this is first contact): local copies of
        // this folder are meaningless, so nothing counts as new mail here —
        // the whole folder is missing, and the backfill below brings it.
        let reset = known.0 != Some(i64::from(meta.uidvalidity));
        let from = if reset {
            meta.uidnext
        } else {
            known.1.map_or(meta.uidnext, |n| n as u32)
        };

        // Gather. Every round trip happens here, with nothing held open.
        let mut mails = if meta.uidnext > from {
            w.run(&Fetch {
                account,
                folder: rf.name.clone(),
                from,
            })?
        } else {
            Vec::new()
        };
        // `from:*` quirk: a server with nothing new answers with its highest
        // message anyway.
        mails.retain(|m| m.uid >= from);
        let search = |which: UidSet| {
            w.run(&Uids {
                account,
                folder: rf.name.clone(),
                which,
            })
        };
        let server = search(UidSet::All)?;
        let unseen = search(UidSet::Unseen)?;
        let forwarded = if meta.keywords {
            search(UidSet::Forwarded)?
        } else {
            HashSet::new()
        };

        // Commit. One transaction, no network.
        let g = Gathered {
            fid,
            reset,
            uidvalidity: meta.uidvalidity,
            uidnext: meta.uidnext,
            mails,
            // The loop below reads it too: the folder entire, as the
            // server just listed it.
            server: server.clone(),
            unseen,
            forwarded,
            keywords: meta.keywords,
        };
        w.store()
            .write(move |tx| land(tx, account, &g))
            .map_err(err)?;

        // Reach back, over the session this pass already holds. The `ALL`
        // search above is the folder entire, so what this store is missing
        // is a set difference rather than a guess: one fetch and one commit
        // a batch, which is what keeps a whole mailbox out of memory while
        // still mirroring it in one sitting. A folder already whole asks
        // for nothing and costs no round trip at all.
        let until = w.now() + REACH_BUDGET.as_secs_f64();
        loop {
            let batch = missing(w.store(), fid, &server);
            if batch.is_empty() {
                break;
            }
            if w.now() >= until {
                more = true;
                break;
            }
            let got = w.run(&Backfill {
                account,
                folder: rf.name.clone(),
                uids: batch,
            })?;
            // Listed by the search and then not handed over: nothing this
            // pass can do about it, and asking again in a loop is a spin.
            if got.is_empty() {
                break;
            }
            w.store()
                .write(move |tx| {
                    for m in &got {
                        ingest_message(tx, account, fid, m)?;
                    }
                    Ok(())
                })
                .map_err(err)?;
        }
    }
    Ok(more)
}

/// The uids the server still has and this store does not, newest first,
/// capped at one batch — what the backfill asks for next. Read back out of
/// the store after each commit, so a folder that was just reset counts as
/// holding nothing and a batch already landed is never asked for twice.
fn missing(store: &Store, fid: i64, server: &HashSet<u32>) -> Vec<u32> {
    let mut have: HashSet<u32> = HashSet::new();
    if let Ok(mut stmt) = store
        .conn()
        .prepare("SELECT uid FROM server_msg WHERE folder = ?1 AND uid IS NOT NULL")
    {
        if let Ok(rows) = stmt.query_map([fid], |r| r.get::<_, i64>(0)) {
            have.extend(rows.filter_map(Result::ok).map(|u| u as u32));
        }
    }
    let mut batch: Vec<u32> = server
        .iter()
        .copied()
        .filter(|u| !have.contains(u))
        .collect();
    // Newest first — a person reading down a mailbox meets the batches in
    // the order they arrive — then ascending, which is what a fetch wants.
    batch.sort_unstable_by(|a, b| b.cmp(a));
    batch.truncate(BACKFILL);
    batch.sort_unstable();
    batch
}

/// The commit half of one folder's pass.
fn land(tx: &Transaction, account: i64, g: &Gathered) -> rusqlite::Result<()> {
    if g.reset {
        tx.execute(
            "DELETE FROM message WHERE id IN
               (SELECT message FROM server_msg WHERE folder = ?1)",
            [g.fid],
        )?;
        tx.execute("DELETE FROM server_msg WHERE folder = ?1", [g.fid])?;
    }
    for m in &g.mails {
        ingest_message(tx, account, g.fid, m)?;
    }
    tx.execute(
        "UPDATE folder SET uidvalidity = ?1, uidnext = ?2, keywords = ?3 WHERE id = ?4",
        rusqlite::params![g.uidvalidity, g.uidnext, g.keywords, g.fid],
    )?;

    // Reconcile facts over the retained window, by the *server's* view.
    // Divergent intent stays local truth: an unpushed read or archive is never
    // clobbered, only recorded.
    struct Local {
        id: i64,
        uid: u32,
        seen: bool,
        unread: bool,
        has_forwarded: bool,
        forwarded: bool,
    }
    let local: Vec<Local> = {
        let mut stmt = tx.prepare(
            "SELECT m.id, s.uid, s.seen, m.unread, s.forwarded, m.forwarded
             FROM server_msg s JOIN message m ON m.id = s.message
             WHERE s.folder = ?1 AND s.uid IS NOT NULL",
        )?;
        let rows = stmt.query_map([g.fid], |r| {
            Ok(Local {
                id: r.get(0)?,
                uid: r.get::<_, i64>(1)? as u32,
                seen: r.get(2)?,
                unread: r.get(3)?,
                has_forwarded: r.get(4)?,
                forwarded: r.get(5)?,
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
        // `$Forwarded`, by the same rule: another client's mark (or its
        // clearing) is followed unless this one disagrees unpushed. On a
        // server that keeps no keywords there is nothing to follow — its
        // silence is not a clearing — and the local mark stands.
        let now_fwd = g.forwarded.contains(&l.uid);
        if g.keywords && now_fwd != l.has_forwarded {
            tx.execute(
                "UPDATE server_msg SET forwarded = ?1 WHERE message = ?2",
                rusqlite::params![now_fwd, id],
            )?;
            if l.forwarded == l.has_forwarded {
                tx.execute(
                    "UPDATE message SET forwarded = ?1 WHERE id = ?2",
                    rusqlite::params![now_fwd, id],
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
                "UPDATE server_msg SET folder = ?1, uid = ?2, seen = ?3, forwarded = ?4
                 WHERE message = ?5",
                rusqlite::params![folder, m.uid, !m.unread, m.forwarded, id],
            )?;
            return Ok(());
        }
    }
    tx.execute(
        "INSERT INTO message(account, folder, from_name, from_email, subject, date,
                             unread, body, message_id, topic, forwarded, html, raw)
         VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13)",
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
            m.forwarded,
            p.html,
            m.raw,
        ],
    )?;
    let id = tx.last_insert_rowid();
    tx.execute(
        "INSERT INTO server_msg(message, folder, uid, seen, forwarded)
         VALUES(?1, ?2, ?3, ?4, ?5)",
        rusqlite::params![id, folder, m.uid, !m.unread, m.forwarded],
    )?;
    // Which conversation it belongs to, and what it carries — both decided
    // here, in the same transaction, so no draw ever sees an unthreaded mail
    // or one whose parts are still coming.
    model::thread_tx(tx, account, id, &p.message_id, &p.references)?;
    parts::attach_tx(tx, id, &p.attachments)?;
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
    /// The same letter as HTML, **narrowed** on the way through
    /// ([`html::sanitize`](super::html::sanitize)): what the `Html` widget
    /// draws, and nothing that fetches. `None` when the sender sent text
    /// alone.
    pub html: Option<String>,
    /// The Message-ID header, angle brackets off — move adoption, and the
    /// identity threading walks.
    pub message_id: String,
    /// `References` ∪ `In-Reply-To`, brackets off, header order, deduped:
    /// every conversation this mail claims to belong to.
    pub references: Vec<String>,
    /// The subject with its reply and forward prefixes stripped.
    pub topic: String,
    /// The parts the letter carries beside its readings — what the message
    /// panel lists and a card opens. The bytes stay in `raw`; only the
    /// description is stored (see [`parts`](super::parts)).
    pub attachments: Vec<Part>,
}

/// MIME → panel text, through `mail-parser`. Paragraph structure survives as
/// the `\n\n` convention the message panel already renders.
///
/// A multipart/alternative mail yields both halves: the plain text as `body`,
/// the HTML as `html`. Both are kept because they answer different questions;
/// quoting a reply wants the text.
#[must_use]
pub fn parse_mail(raw: &[u8]) -> ParsedMail {
    let Some(msg) = mail_parser::MessageParser::default().parse(raw) else {
        return ParsedMail {
            subject: "(unparseable message)".into(),
            topic: "(unparseable message)".into(),
            ..ParsedMail::default()
        };
    };
    let (from_name, from_email) = msg
        .from()
        .and_then(|a| a.first())
        .map(|a| {
            (
                a.name().unwrap_or_default().to_string(),
                a.address().unwrap_or_default().to_string(),
            )
        })
        .unwrap_or_default();
    // A sender without a name is stored under their address as the name, so
    // nothing has to fall back later.
    let from_name = if from_name.is_empty() {
        from_email.clone()
    } else {
        from_name
    };
    // Only a genuine `text/html` part counts. `body_html` would answer for a
    // text-only mail too, by running the plain text through `text_to_html` —
    // which would call every plain letter an HTML one.
    let html = msg
        .html_bodies()
        .next()
        .and_then(|p| match &p.body {
            mail_parser::PartType::Html(h) => Some(super::html::sanitize(h.as_ref())),
            _ => None,
        })
        .filter(|h| !h.trim().is_empty());
    let subject = msg.subject().unwrap_or("(no subject)").to_string();
    let mut references: Vec<String> = Vec::new();
    for id in header_ids(msg.references())
        .into_iter()
        .chain(header_ids(msg.in_reply_to()))
    {
        if !id.is_empty() && !references.contains(&id) {
            references.push(id);
        }
    }
    let attachments = parts_of(&msg, html.as_deref());
    ParsedMail {
        from_name,
        from_email,
        topic: topic_of(&subject),
        subject,
        date: msg.date().map_or(0.0, |d| d.to_timestamp() as f64),
        body: msg
            .body_text(0)
            .map(|t| t.replace("\r\n", "\n").trim().to_string())
            .unwrap_or_default(),
        html,
        message_id: norm_id(msg.message_id().unwrap_or_default()),
        references,
        attachments,
    }
}

/// One part of a letter, as a row describes it. The bytes are not here:
/// they live in the `raw` the store already keeps, and [`part_bytes`] reads
/// them back by `at` — which is what keeps a mailbox one copy of itself
/// rather than two.
#[derive(Debug, Clone, PartialEq)]
pub struct Part {
    /// Which part of the parsed message it is — the index [`part_bytes`]
    /// reads back by.
    pub at: u32,
    /// What to call it: the `filename`, else the `name`, else a made-up one
    /// — a part with no name is still a part.
    pub name: String,
    pub mime: String,
    /// Bytes, decoded — what the card and the list show.
    pub size: u64,
    /// Its Content-ID, brackets off, for a part the reading refers to.
    pub cid: String,
}

/// The parts of a message a row would describe — the same walk
/// [`parse_mail`] does, so a stored row and a fresh read cannot disagree.
/// `html` is the letter's reading: a part it already draws inline is not
/// also an attachment, or a pasted screenshot would be listed under the
/// picture of itself.
fn parts_of(msg: &mail_parser::Message<'_>, html: Option<&str>) -> Vec<Part> {
    use mail_parser::MimeHeaders;
    let mut out = Vec::new();
    for at in msg.attachments.iter().copied() {
        let Some(p) = msg.parts.get(at as usize) else {
            continue;
        };
        let cid = norm_id(p.content_id().unwrap_or_default());
        // Drawn in the letter already: the `multipart/related` a composer
        // writes around a pasted screenshot (see [`inline_images`]).
        if !cid.is_empty() && html.is_some_and(|h| h.contains(&format!("cid:{cid}"))) {
            continue;
        }
        let mime = p
            .content_type()
            .map(|t| match t.subtype() {
                Some(sub) => format!("{}/{sub}", t.ctype()),
                None => t.ctype().to_string(),
            })
            .unwrap_or_else(|| "application/octet-stream".into());
        let name = p
            .attachment_name()
            .map(parts::safe_name)
            .filter(|n| !n.is_empty())
            .unwrap_or_else(|| format!("part-{}", out.len() + 1));
        out.push(Part {
            at,
            name,
            mime,
            size: p.contents().len() as u64,
            cid,
        });
    }
    out
}

/// One part's bytes, decoded, out of the letter it arrived in. `None` when
/// the raw no longer parses or no longer has that part — a row from a build
/// whose walk numbered them differently, or a mail refetched.
#[must_use]
pub fn part_bytes(raw: &[u8], at: u32) -> Option<Vec<u8>> {
    let msg = mail_parser::MessageParser::default().parse(raw)?;
    Some(msg.parts.get(at as usize)?.contents().to_vec())
}

/// The images a letter carries inside itself — its parts with a Content-ID
/// and an image type, the `multipart/related` a composer writes around a
/// pasted screenshot — as `(cid, bytes)`, brackets off: the names its HTML
/// refers to them by (`src="cid:…"`).
#[must_use]
pub fn inline_images(raw: &[u8]) -> Vec<(String, Vec<u8>)> {
    use mail_parser::MimeHeaders;
    let Some(msg) = mail_parser::MessageParser::default().parse(raw) else {
        return Vec::new();
    };
    msg.parts
        .iter()
        .filter_map(|p| {
            let cid = p
                .content_id()?
                .trim()
                .trim_start_matches('<')
                .trim_end_matches('>');
            let image = p
                .content_type()
                .is_some_and(|t| t.ctype().eq_ignore_ascii_case("image"));
            (image && !cid.is_empty()).then(|| (cid.to_string(), p.contents().to_vec()))
        })
        .collect()
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
fn header_ids(v: &mail_parser::HeaderValue<'_>) -> Vec<String> {
    use mail_parser::HeaderValue;
    match v {
        HeaderValue::Text(t) => vec![norm_id(t)],
        HeaderValue::TextList(l) => l.iter().map(|t| norm_id(t)).collect(),
        _ => Vec::new(),
    }
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

/// Walks the `raw` of every mail nobody has walked at this build's version,
/// so its parts are rows a panel can list.
///
/// The ingest writes them in the transaction that stored the letter, and the
/// schema's derived step covers a version bump — but a mail that arrives
/// through **replication** runs no ingest code at all, and its `raw` is
/// nobody's to walk until somebody looks. This is the somebody. It is an
/// anti-join that reads no letter once they have all been walked, which is
/// what makes running it every turn affordable.
fn scan_pass(w: &World) {
    let unwalked: bool = w
        .store()
        .conn()
        .query_row(
            "SELECT EXISTS(SELECT 1 FROM message m
                           LEFT JOIN attachment_scan s ON s.message = m.id
                           WHERE s.version IS NULL OR s.version != ?1)",
            [parts::ATTACH_VERSION],
            |r| r.get(0),
        )
        .unwrap_or(false);
    if unwalked {
        let _ = w.store().write(|tx| parts::scan(tx));
    }
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
    /// What this account's [`IdleWatch`] heard: the server said something
    /// arrived. Set there, consumed here, and one account's own, which is
    /// what keeps a letter for one mailbox from pulling every other.
    news: Arc<AtomicBool>,
}

impl SyncPass {
    #[must_use]
    pub fn new(account: i64, news: Arc<AtomicBool>) -> SyncPass {
        SyncPass {
            account,
            due: 0.0,
            seen: PULL.load(Ordering::Relaxed),
            news,
        }
    }

    /// Whether this turn looks outside: the interval has run out, somebody
    /// asked, or the watch heard something. Consumes the request either way.
    fn pull_due(&mut self, w: &World) -> bool {
        let asked = PULL.load(Ordering::Relaxed);
        let heard = self.news.swap(false, Ordering::Relaxed);
        let due = heard || asked != self.seen || w.now() >= self.due;
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
            Ok(_) => format!("ok · {}", fmt_date(w.now())),
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
        // A folder still reaching into its past wants the next batch now,
        // not in a minute: a first sync finishes in one sitting rather than
        // two hundred messages an hour.
        if matches!(outcome, Ok(true)) {
            self.due = w.now();
            return Wake::After(REACH);
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
        scan_pass(w);
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

/// One account's watch: a second session, sitting in `IDLE` on the inbox so
/// the server can say that a letter arrived instead of being asked once a
/// minute.
///
/// It fetches nothing. The account's own [`SyncPass`] holds the session that
/// may write, and two threads ingesting one mailbox would race for the same
/// uids — so a watch that hears something sets the account's [`NEWS`] flag,
/// wakes that pass, and goes back to waiting.
///
/// The second connection is what buys the first one's manners: a wait cannot
/// be cut short, and a pass that spent five minutes inside `IDLE` could not
/// push a mark the moment a verb made one.
pub struct IdleWatch {
    account: i64,
    /// What the sync pass reads: the server said something arrived.
    news: Arc<AtomicBool>,
    /// Not before this, on the world's clock — a backoff after a refusal,
    /// and the rest of a window a watch came back from early.
    next: f64,
    /// Whether this session is open. A failed wait drops it.
    connected: bool,
    /// This server offers no `IDLE`, so there is nothing to wait on and the
    /// watch is over: the session is handed back and the pass sleeps until
    /// kicked, holding nothing. The interval carries the account, as it
    /// always did.
    parked: bool,
}

impl IdleWatch {
    #[must_use]
    pub fn new(account: i64, news: Arc<AtomicBool>) -> IdleWatch {
        IdleWatch {
            account,
            news,
            next: 0.0,
            connected: false,
            parked: false,
        }
    }

    /// The wait left before the next attempt.
    fn holding(&self, w: &World) -> Wake {
        Wake::After(Duration::from_secs_f64(
            (self.next - w.now()).clamp(0.0, WATCH.as_secs_f64()),
        ))
    }

    /// Which folder this watch sits on: the inbox, once a pass has mirrored
    /// the folders. `IDLE` reports on the selected mailbox and no other, and
    /// the inbox is the one whose latency anybody feels.
    fn inbox(&self, w: &World) -> Option<String> {
        w.store()
            .conn()
            .query_row(
                "SELECT name FROM folder WHERE account = ?1 AND role = 'inbox'",
                [self.account],
                |r| r.get(0),
            )
            .ok()
    }
}

impl Worker for IdleWatch {
    fn name(&self) -> String {
        format!("watch-{}", self.account)
    }

    fn entity(&self) -> Option<String> {
        Some(account_entity(self.account))
    }

    /// Nothing. The session this one holds is for waiting, and every job an
    /// account has needs the session that may write.
    fn claims(&self, _job: &Job) -> bool {
        false
    }

    fn pass(&mut self, w: &World) -> Wake {
        if self.parked {
            return Wake::OnKick;
        }
        if w.now() < self.next {
            return self.holding(w);
        }
        let account = self.account;
        // Before the first pass has mirrored them there is no inbox to sit
        // on, and nothing to hear about it either.
        let Some(folder) = self.inbox(w) else {
            self.next = w.now() + WATCH_RETRY.as_secs_f64();
            return self.holding(w);
        };
        if !self.connected {
            if connect(w, account).is_err() {
                self.next = w.now() + WATCH_RETRY.as_secs_f64();
                return self.holding(w);
            }
            self.connected = true;
        }
        let started = w.now();
        match w.run(&Watch {
            account,
            folder,
            window: WATCH,
        }) {
            Ok(Watched::Changed) => {
                // The pass that may fetch is asleep on its interval, so it
                // is told what was heard and then woken to act on it.
                self.news.store(true, Ordering::Relaxed);
                let _ = w.with_cap::<dyn Kicker, _>(|k| k.kick(&account_entity(account)));
                Wake::After(Duration::ZERO)
            }
            // A wait that came back before its window was up did not wait:
            // a fake, which cannot block, or a link that hung up. Hold the
            // rest of the window rather than ask again at once — with a
            // server that really waited, that remainder is already spent.
            Ok(Watched::Quiet) => {
                self.next = started + WATCH.as_secs_f64();
                self.holding(w)
            }
            Ok(Watched::Unsupported) => {
                // Nothing to wait on is nothing to hold open. A server that
                // offers no `IDLE` may well be one that counts connections,
                // and the pass that fetches needs one more than a watch with
                // no work left does.
                let _ = w.run(&Disconnect { account });
                self.connected = false;
                self.parked = true;
                Wake::OnKick
            }
            Err(_) => {
                self.connected = false;
                self.next = w.now() + WATCH_RETRY.as_secs_f64();
                self.holding(w)
            }
        }
    }
}

/// The passes mail wants running now: one per configured account — the sync
/// pass and its watch, which share what the watch hears — and the sender.
/// Derived from the store, so an account added later starts a worker without
/// a restart.
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
        // The pair's own flag, made here where the pair is: an account id
        // means something to one store and this process may hold several,
        // so nothing about a watch is kept anywhere a second store could
        // reach. The two halves are always answered for together, and the
        // kernel spawns a name it does not already have — so the running
        // pair is one answer's pair.
        let news = Arc::new(AtomicBool::new(false));
        v.push(Box::new(SyncPass::new(a, news.clone())));
        v.push(Box::new(IdleWatch::new(a, news)));
    }
    v
}
