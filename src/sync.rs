//! The IMAP sync engine: one worker thread per account, each with its own
//! [`World`] over its own **reader** on the one store — writes go to the
//! shared single writer (CR-005 phase 0), and the UI notices a worker's
//! commit via `data_version` (see [`crate::store::Store::poll_external`]).
//!
//! Sync is **ingest**, not action: nothing here is undoable, and nothing
//! here fights the user. Local intent (`message`) and server fact
//! (`server_msg`) are separate columns, and their disagreement *is* the push
//! queue.
//!
//! Two rules this module exists to obey (CR-004):
//!
//! - **The push pass does not talk to the server.** It materializes each
//!   disagreement as a [`mail::Move`], [`mail::Seen`] or
//!   [`mail::Forwarded`] job and lets the
//!   executor perform it. Every job revalidates first, so a disagreement
//!   that undo removes before the executor reaches it is never pushed at
//!   all — undo still costs zero server traffic.
//! - **No effect runs inside a transaction.** The fetch pass gathers
//!   everything it needs over the network first, then commits once. It used
//!   to hold `BEGIN IMMEDIATE` across three round trips per folder.

use std::collections::HashSet;
use std::sync::mpsc;
use std::sync::Arc;
use std::time::Duration;

use rusqlite::Transaction;

use crate::effect::{Clock, Creds, RemoteMail, Secrets, UidSet, World};
use crate::mail;

/// How many most-recent messages a folder retains on first contact (and
/// after a UIDVALIDITY reset). Bounded coverage, stated honestly — the
/// panels say nothing below this window exists locally.
pub const FETCH_CAP: u32 = 200;

/// Poll cadence between kicks.
const POLL: Duration = Duration::from_secs(60);

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
    let pass = w
        .outside(|o| o.secret_get(&email))
        .ok_or("no password in the keychain")?;
    w.run(&mail::Connect {
        account,
        creds: Creds {
            host,
            user: email,
            pass,
        },
    })
}

/// The push pass: every message whose intent differs from the server —
/// folder, read state, or passed on — becomes a job, unless one is already
/// in flight for it. No network here at all.
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
    }
    let rows: Vec<Row> = {
        let db = w.store().conn();
        let mut stmt = db
            .prepare(
                "SELECT m.id, s.uid, m.folder, fw.name, fh.name,
                        m.folder != s.folder, m.unread, s.seen,
                        m.forwarded, s.forwarded
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
            // thread (CR-005 phase 0). The `in_flight` guard stays inside the
            // transaction, so the claim is still atomic.
            let job = w
                .prepare(&mail::Move {
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
                .prepare(&mail::Seen {
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
        if p.forwarded != p.has_forwarded {
            let job = w
                .prepare(&mail::Forwarded {
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
}

/// The fetch/reconcile pass: for each folder, gather over the network, then
/// commit once.
///
/// # Errors
///
/// If any round trip fails, or the commit does.
fn fetch_account(w: &World, account: i64) -> Result<(), String> {
    let err = |e: rusqlite::Error| e.to_string();
    for rf in w.run(&mail::Folders { account })? {
        let Some(role) = rf.role.clone() else { continue };
        let meta = w.run(&mail::Meta {
            account,
            folder: rf.name.clone(),
        })?;

        // The folder row and what we last knew about it — a short write,
        // no network in sight. Owned copies cross to the writer thread; the
        // originals stay for the round trips below (CR-005 phase 0).
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
            w.run(&mail::Fetch {
                account,
                folder: rf.name.clone(),
                from,
            })?
        } else {
            Vec::new()
        };
        let search = |which: UidSet| {
            w.run(&mail::Uids {
                account,
                folder: rf.name.clone(),
                which,
            })
        };
        let server = search(UidSet::All)?;
        let unseen = search(UidSet::Unseen)?;
        let forwarded = search(UidSet::Forwarded)?;

        // Commit. One transaction, no network.
        let g = Gathered {
            fid,
            reset,
            uidvalidity: meta.uidvalidity,
            uidnext: meta.uidnext,
            mails,
            server,
            unseen,
            forwarded,
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
    // Divergent intent stays local truth: an unpushed read or archive is
    // never clobbered, only recorded.
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
            // Clean rows (intent agrees with the old server state) follow
            // the server; divergent intent will be pushed over it instead.
            if l.unread != l.seen {
                tx.execute(
                    "UPDATE message SET unread = ?1 WHERE id = ?2",
                    rusqlite::params![!now_seen, id],
                )?;
            }
        }
        // `$Forwarded`, by the same rule: another client's mark (or its
        // clearing) is followed unless this one disagrees unpushed.
        let now_fwd = g.forwarded.contains(&l.uid);
        if now_fwd != l.has_forwarded {
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
/// server never told us (no COPYUID) is **adopted** by Message-ID instead
/// of duplicated.
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
        "INSERT INTO message(account, folder, from_name, from_email,
                             subject, date, unread, body, html, raw, message_id, topic,
                             forwarded)
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
            p.html,
            m.raw,
            p.message_id,
            p.topic,
            m.forwarded,
        ],
    )?;
    let id = tx.last_insert_rowid();
    tx.execute(
        "INSERT INTO server_msg(message, folder, uid, seen, forwarded)
         VALUES(?1, ?2, ?3, ?4, ?5)",
        rusqlite::params![id, folder, m.uid, !m.unread, m.forwarded],
    )?;
    // Which conversation it belongs to (CR-007) — decided here, in the same
    // transaction, so no draw ever sees an unthreaded mail.
    mail::thread_tx(tx, account, id, &p.message_id, &p.references)?;
    Ok(())
}

/// What panels need out of an RFC 822 blob.
pub struct ParsedMail {
    pub from_name: String,
    pub from_email: String,
    pub subject: String,
    pub date: f64,
    pub body: String,
    /// The same letter as HTML, already narrowed to what the panel draws
    /// ([`crate::html::sanitize`]). `None` when the sender sent text alone,
    /// or when the narrowing left nothing worth showing.
    pub html: Option<String>,
    /// The Message-ID header, angle brackets off — move adoption, and the
    /// identity threading walks (CR-007).
    pub message_id: String,
    /// `References` ∪ `In-Reply-To`, brackets off, header order, deduped:
    /// every conversation this mail claims to belong to.
    pub references: Vec<String>,
    /// The subject with its reply/forward prefixes stripped.
    pub topic: String,
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

/// One id out of an id header, as threading compares it: trimmed, and
/// without the angle brackets a well-formed one wears.
fn norm_id(s: &str) -> String {
    s.trim().trim_start_matches('<').trim_end_matches('>').trim().to_string()
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

/// MIME → panel text via `mail-parser`. Paragraph structure survives as
/// the `\n\n` convention the message panel already renders.
///
/// A multipart/alternative mail yields both halves: the plain text as
/// `body`, the HTML — narrowed on the way through — as `html`. Both are
/// kept because they answer different questions; the panel prefers the
/// HTML, while quoting a reply wants the text.
#[must_use]
pub fn parse_mail(raw: &[u8]) -> ParsedMail {
    let msg = mail_parser::MessageParser::default().parse(raw);
    let Some(msg) = msg else {
        return ParsedMail {
            from_name: String::new(),
            from_email: String::new(),
            subject: "(unparseable message)".into(),
            date: 0.0,
            body: String::new(),
            html: None,
            message_id: String::new(),
            references: Vec::new(),
            topic: "(unparseable message)".into(),
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
    let from_name = if from_name.is_empty() {
        from_email.clone()
    } else {
        from_name
    };
    // Only a genuine text/html part counts. `body_html` would answer for a
    // text-only mail too, by running the plain text through `text_to_html`
    // — which would route every plain letter through the HTML panel and
    // lose the `\n\n` paragraphs it renders.
    let html = msg
        .html_bodies()
        .next()
        .and_then(|p| match &p.body {
            mail_parser::PartType::Html(h) => Some(crate::html::sanitize(h.as_ref())),
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
    ParsedMail {
        from_name,
        from_email,
        topic: crate::mail::topic_of(&subject),
        subject,
        date: msg.date().map(|d| d.to_timestamp() as f64).unwrap_or(0.0),
        body: msg
            .body_text(0)
            .map(|t| t.replace("\r\n", "\n").trim().to_string())
            .unwrap_or_default(),
        html,
        message_id: norm_id(msg.message_id().unwrap_or_default()),
        references,
    }
}

// -- the worker ---------------------------------------------------------------

/// A handle to kick a worker out of its poll sleep (the Refresh button).
pub struct Worker {
    pub account: i64,
    kick: mpsc::Sender<()>,
}

impl Worker {
    pub fn kick(&self) {
        let _ = self.kick.send(());
    }
}

/// Spawns the sync loop for one account: sync → status → sleep (or kick) →
/// again. The thread exits when its account row disappears. It builds its
/// own [`World`] — its own store connection, its own `Real` outside —
/// exactly as it used to build its own `Connection`. `notify` wakes the UI
/// thread after every pass (`SignalToUI` upstairs — this module stays
/// makepad-free).
///
/// # Panics
///
/// If the thread cannot be spawned.
pub fn spawn(
    db: Arc<crate::store::Db>,
    account: i64,
    secrets: Secrets,
    clock: Clock,
    notify: impl Fn() + Send + 'static,
) -> Worker {
    let (tx, rx) = mpsc::channel::<()>();
    std::thread::Builder::new()
        .name(format!("sync-{account}"))
        .spawn(move || {
            // The worker joins the *one* writer (CR-005 phase 0) — its own
            // reader over the shared `Db`, never a second writable connection.
            let Ok(store) = crate::store::Store::with_db(db) else {
                return;
            };
            let w = World::new(
                std::rc::Rc::new(store),
                Box::new(crate::effect::Real::new(secrets, clock)),
                mail::registry(),
            );
            loop {
                let alive: Option<String> = w
                    .store()
                    .conn()
                    .query_row(
                        "SELECT COALESCE(imap_host, '') FROM account WHERE id = ?1",
                        [account],
                        |r| r.get(0),
                    )
                    .ok();
                match alive {
                    None => return,              // account removed: retire
                    Some(h) if h.is_empty() => return, // demo account
                    Some(_) => {}
                }
                let outcome = sync_account(&w, account);
                // The push pass only queued; the executor is what talks.
                w.run_effects();
                let status = match &outcome {
                    Ok(()) => format!("ok · {}", mail::fmt_date(w.now())),
                    Err(e) => format!("error: {e}"),
                };
                let synced = outcome.is_ok().then(|| w.now());
                let _ = w.store().write(move |c| {
                    c.execute(
                        "UPDATE account SET status = ?1, synced = ?2 WHERE id = ?3",
                        rusqlite::params![status, synced, account],
                    )
                    .map(|_| ())
                });
                notify();
                match rx.recv_timeout(POLL) {
                    Ok(()) | Err(mpsc::RecvTimeoutError::Timeout) => {}
                    Err(mpsc::RecvTimeoutError::Disconnected) => return,
                }
            }
        })
        .expect("spawn sync worker");
    Worker { account, kick: tx }
}

/// Convenience for a headless world: sync every configured account, then
/// drain the effect queue until it stops moving. The manual pump.
pub fn tick(w: &World) {
    let accounts: Vec<i64> = {
        let Ok(mut stmt) = w
            .store()
            .conn()
            .prepare("SELECT id FROM account WHERE COALESCE(imap_host, '') != '' ORDER BY id")
        else {
            return;
        };
        stmt.query_map([], |r| r.get(0))
            .map(|it| it.filter_map(Result::ok).collect())
            .unwrap_or_default()
    };
    for a in accounts {
        if let Err(e) = sync_account(w, a) {
            let status = format!("error: {e}");
            let _ = w.store().write(move |c| {
                c.execute(
                    "UPDATE account SET status = ?1 WHERE id = ?2",
                    rusqlite::params![status, a],
                )
                .map(|_| ())
            });
        }
    }
}

/// Runs passes until nothing changes: sync, send, execute, repeat. Bounded,
/// because a job that re-queues itself forever is a bug and should look
/// like one.
///
/// # Panics
///
/// If the world will not settle within a sane number of rounds.
pub fn settle(w: &World) {
    for _ in 0..16 {
        tick(w);
        crate::send::outbox_pass(w);
        let ran = w.run_effects();
        // A job waiting out its backoff is scheduled, not unsettled.
        let stuck: i64 = w
            .store()
            .conn()
            .query_row(
                "SELECT COUNT(*) FROM effect
                 WHERE status = 'processing'
                    OR (status = 'pending' AND not_before <= ?1)",
                [w.now()],
                |r| r.get(0),
            )
            .unwrap_or(0);
        if ran == 0 && stuck == 0 {
            return;
        }
    }
    panic!("world did not settle: {:?}", w.jobs());
}

// -- the pump -----------------------------------------------------------------

/// Who runs the passes. The passes themselves — [`sync_account`],
/// [`crate::send::outbox_pass`], [`World::run_effects`] — are the same
/// either way; only the scheduler differs.
#[derive(Default)]
pub enum Pump {
    /// Production: one thread per account, plus the sender. Each builds its
    /// own [`World`] over its own store connection.
    Threads {
        workers: Vec<Worker>,
        sender: Option<crate::send::Sender>,
    },
    /// Tests and the components library: passes run inline, on the calling
    /// thread, when [`settle`] says so. An in-memory store finally has a
    /// mail engine, and every invalidation happens in a knowable order.
    #[default]
    Manual,
}

impl Pump {
    /// A threaded pump with nothing spawned yet.
    #[must_use]
    pub fn threads() -> Pump {
        Pump::Threads {
            workers: Vec::new(),
            sender: None,
        }
    }

    /// Wakes everything out of its poll sleep — an action just changed
    /// intent, and the server should hear about it without waiting.
    pub fn kick(&self) {
        if let Pump::Threads { workers, sender } = self {
            for w in workers {
                w.kick();
            }
            if let Some(s) = sender {
                s.kick();
            }
        }
    }

    /// Whether any account currently has a worker.
    #[must_use]
    pub fn idle(&self) -> bool {
        match self {
            Pump::Threads { workers, .. } => workers.is_empty(),
            Pump::Manual => true,
        }
    }

    /// Spawns a worker for every configured account that lacks one, and
    /// retires those whose account is gone. Idempotent — call after boot and
    /// after the accounts change. A `Manual` pump does nothing.
    pub fn ensure(
        &mut self,
        w: &World,
        db: &Arc<crate::store::Db>,
        secrets: &Secrets,
        clock: &Clock,
        notify: impl Fn() + Send + Clone + 'static,
    ) {
        let Pump::Threads { workers, sender } = self else {
            return;
        };
        if sender.is_none() {
            *sender = Some(crate::send::spawn(
                db.clone(),
                secrets.clone(),
                clock.clone(),
                notify.clone(),
            ));
        }
        let accounts = crate::mail::accounts(w.store());
        workers.retain(|k| accounts.iter().any(|a| a.id == k.account));
        for a in accounts.iter() {
            if a.imap_host.as_deref().unwrap_or("").is_empty() {
                continue; // the local demo account
            }
            if workers.iter().any(|k| k.account == a.id) {
                continue;
            }
            workers.push(spawn(
                db.clone(),
                a.id,
                secrets.clone(),
                clock.clone(),
                notify.clone(),
            ));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::Seed;
    use crate::effect::World;

    const RAW: &str = "From: Vera Kovac <vera@kovac.io>\r\n\
Subject: Budget v2\r\n\
Message-ID: <budget-v2@kovac.io>\r\n\
Date: Mon, 31 Aug 2026 09:14:00 +0000\r\n\
\r\n\
First paragraph.\r\n\r\nSecond paragraph.\r\n";

    /// The shape most mail actually arrives in: both readings of one
    /// letter, the HTML half wrapped in layout and quoted-printable.
    const RAW_ALT: &str = "From: Vera Kovac <vera@kovac.io>\r\n\
Subject: Budget v3\r\n\
Message-ID: <budget-v3@kovac.io>\r\n\
Date: Mon, 31 Aug 2026 09:14:00 +0000\r\n\
MIME-Version: 1.0\r\n\
Content-Type: multipart/alternative; boundary=\"bnd\"\r\n\
\r\n\
--bnd\r\n\
Content-Type: text/plain; charset=utf-8\r\n\
\r\n\
Plain reading.\r\n\
--bnd\r\n\
Content-Type: text/html; charset=utf-8\r\n\
Content-Transfer-Encoding: quoted-printable\r\n\
\r\n\
<html><head><style>p{color:red}</style></head><body>\r\n\
<div style=3D\"padding:8px\"><p>Rich <b>reading</b>.</p>\r\n\
<img src=3D\"https://t.co/px.gif\" width=3D\"1\">\r\n\
<a href=3D\"https://x.dev\">link</a></div></body></html>\r\n\
--bnd--\r\n";

    /// A pasted screenshot the way a composer sends it: `multipart/related`,
    /// the HTML referring to the image part by its Content-ID.
    const RAW_RELATED: &str = "From: Max Ivanov <max@ivanov.dev>\r\n\
Subject: the sketch\r\n\
Message-ID: <sketch@ivanov.dev>\r\n\
Date: Mon, 31 Aug 2026 10:00:00 +0000\r\n\
MIME-Version: 1.0\r\n\
Content-Type: multipart/related; boundary=\"rel\"\r\n\
\r\n\
--rel\r\n\
Content-Type: text/html; charset=utf-8\r\n\
\r\n\
<p>Like so:</p><img src=\"cid:sketch.png@ivanov.dev\" alt=\"the sketch\" width=\"120\" height=\"80\">\r\n\
--rel\r\n\
Content-Type: image/png; name=\"sketch.png\"\r\n\
Content-ID: <sketch.png@ivanov.dev>\r\n\
Content-Disposition: inline; filename=\"sketch.png\"\r\n\
Content-Transfer-Encoding: base64\r\n\
\r\n\
iVBORw0KGgoAAAANSUhEUgAAAAIAAAACCAIAAAD91JpzAAAAC0lEQVR42mNgQAYAAA4AATo1BFYAAAAASUVORK5CYII=\r\n\
--rel--\r\n";

    /// A pasted screenshot arrives as a `multipart/related` part: the
    /// narrowing keeps the `<img>` under its Content-ID, and the raw gives
    /// the bytes back under the same name.
    #[test]
    fn inline_images_come_out_of_the_raw() {
        let p = parse_mail(RAW_RELATED.as_bytes());
        assert!(
            p.html.as_deref().unwrap_or("").contains(
                r#"<img src="cid:sketch.png@ivanov.dev" alt="the sketch" width="120" height="80"/>"#
            ),
            "{:?}",
            p.html
        );
        let imgs = inline_images(RAW_RELATED.as_bytes());
        assert_eq!(imgs.len(), 1);
        assert_eq!(imgs[0].0, "sketch.png@ivanov.dev");
        assert!(imgs[0].1.starts_with(b"\x89PNG"));
        assert!(inline_images(RAW_ALT.as_bytes()).is_empty(), "no cid parts, no images");
    }

    /// An isolated world with one real-looking account and an empty inbox.
    /// No files, no keychain, no threads — nothing outside this value.
    fn world() -> World {
        let w = World::fake(mail::registry());
        w.store()
            .write(|c| {
                c.execute(
                    "INSERT INTO account(label, email, imap_host, smtp_host)
                     VALUES('t','t@t','imap.t','smtp.t')",
                    [],
                )
                .map(|_| ())
            })
            .unwrap();
        w.with_fake(|f| {
            f.keychain("t@t", "pw");
            f.server(1).folder("INBOX", 7);
        });
        w
    }

    fn inbox_rows(w: &World) -> Vec<(String, bool)> {
        let db = w.store().conn();
        let mut stmt = db
            .prepare(
                "SELECT m.subject, m.unread FROM message m
                 JOIN folder f ON m.folder=f.id WHERE f.role='inbox' ORDER BY m.id",
            )
            .unwrap();
        stmt.query_map([], |r| Ok((r.get(0)?, r.get(1)?)))
            .unwrap()
            .collect::<Result<_, _>>()
            .unwrap()
    }

    fn deeds(w: &World) -> Vec<(String, String)> {
        w.jobs()
            .into_iter()
            .map(|j| (j.kind, j.status))
            .collect()
    }

    fn archive_folder(w: &World) -> i64 {
        w.store()
            .conn()
            .query_row("SELECT id FROM folder WHERE role='archive'", [], |r| r.get(0))
            .unwrap()
    }

    /// Initial sync ingests and parses; a second pass fetches only what is
    /// new; flags flip; remote deletions disappear locally.
    #[test]
    fn sync_ingests_incrementally_and_reconciles() {
        let w = world();
        let u1 = w.with_fake(|f| f.server(1).deliver("INBOX", true, RAW));
        settle(&w);
        assert_eq!(inbox_rows(&w), vec![("Budget v2".to_string(), true)]);
        let body: String = w
            .store()
            .conn()
            .query_row("SELECT body FROM message", [], |r| r.get(0))
            .unwrap();
        assert!(body.contains("First paragraph.\n\nSecond"), "{body:?}");

        settle(&w);
        assert_eq!(inbox_rows(&w).len(), 1, "nothing duplicated");

        let u2 = w.with_fake(|f| {
            let u = f.server(1).deliver("INBOX", true, "Subject: Two\r\n\r\nx");
            f.server(1).mark_seen("INBOX", u1);
            u
        });
        settle(&w);
        assert_eq!(
            inbox_rows(&w),
            vec![("Budget v2".into(), false), ("Two".into(), true)]
        );
        w.with_fake(|f| f.server(1).remove("INBOX", u2));
        settle(&w);
        assert_eq!(inbox_rows(&w).len(), 1);
        assert!(deeds(&w).is_empty(), "ingest queues nothing");
    }

    /// Local intent becomes a job, the job moves the mail, and the fact is
    /// recorded in the same transaction as the job's success.
    #[test]
    fn intent_becomes_a_job_and_the_job_moves_the_mail() {
        let w = world();
        w.with_fake(|f| {
            f.server(1).copyuid = true;
            f.server(1).folder("Archive", 3);
            f.server(1).deliver("INBOX", true, RAW);
        });
        settle(&w);

        w.store()
            .write(|c| {
                mail::mark_read_tx(c, 1)?;
                mail::archive_tx(c, 1)
            })
            .unwrap();
        settle(&w);

        assert!(
            w.with_fake(|f| f.server(1).folders["INBOX"].2.is_empty()),
            "moved off the inbox"
        );
        assert_eq!(w.with_fake(|f| f.server(1).folders["Archive"].2.len()), 1);
        assert!(
            !w.with_fake(|f| f.server(1).folders["Archive"].2[0].unread),
            "seen pushed too"
        );
        let done: Vec<String> = w
            .jobs()
            .into_iter()
            .filter(|j| j.status == "done")
            .map(|j| j.kind)
            .collect();
        assert_eq!(done, vec!["move", "seen"]);

        // Fact recorded: server_msg now agrees, so nothing re-queues.
        let before = w.jobs().len();
        settle(&w);
        assert_eq!(w.jobs().len(), before, "convergence is quiet");
    }

    /// The property phase 4 bought and CR-004 keeps: intent reverted before
    /// the executor runs costs the server *nothing*. No job, no round trip.
    #[test]
    fn intent_reverted_before_the_pass_never_reaches_the_server() {
        let w = world();
        w.with_fake(|f| {
            f.server(1).folder("Archive", 3);
            f.server(1).deliver("INBOX", true, RAW);
        });
        settle(&w);
        let mark = w.mark();

        let inbox: i64 = w
            .store()
            .conn()
            .query_row("SELECT id FROM folder WHERE role='inbox'", [], |r| r.get(0))
            .unwrap();
        // Archive, then undo it — both before any pass runs.
        w.store().write(|c| mail::archive_tx(c, 1)).unwrap();
        w.store()
            .write(move |c| {
                c.execute("UPDATE message SET folder=?1 WHERE id=1", [inbox])
                    .map(|_| ())
            })
            .unwrap();
        settle(&w);

        assert!(w.jobs_since(mark).is_empty(), "no job was ever filed");
        assert_eq!(w.with_fake(|f| f.server(1).folders["INBOX"].2.len()), 1);
    }

    /// A job whose intent is reverted *after* it is queued goes obsolete
    /// rather than pushing stale work — the revalidation safety net.
    #[test]
    fn a_queued_job_revalidates_before_it_runs() {
        let w = world();
        w.with_fake(|f| {
            f.server(1).folder("Archive", 3);
            f.server(1).deliver("INBOX", true, RAW);
        });
        settle(&w);

        let inbox: i64 = w
            .store()
            .conn()
            .query_row("SELECT id FROM folder WHERE role='inbox'", [], |r| r.get(0))
            .unwrap();
        w.store().write(|c| mail::archive_tx(c, 1)).unwrap();
        push_account(&w, 1).unwrap(); // queue it, but do not execute
        assert_eq!(deeds(&w), vec![("move".to_string(), "pending".to_string())]);

        // Undo lands while the job waits.
        w.store()
            .write(move |c| {
                c.execute("UPDATE message SET folder=?1 WHERE id=1", [inbox])
                    .map(|_| ())
            })
            .unwrap();
        w.run_effects();

        assert_eq!(deeds(&w), vec![("move".to_string(), "obsolete".to_string())]);
        assert_eq!(
            w.with_fake(|f| f.server(1).folders["INBOX"].2.len()),
            1,
            "the server was never touched"
        );
    }

    /// Without COPYUID the moved mail loses its uid until the next fetch
    /// adopts it by Message-ID — one row throughout, never a duplicate.
    #[test]
    fn move_without_copyuid_adopts_by_message_id() {
        let w = world();
        w.with_fake(|f| {
            f.server(1).copyuid = false;
            f.server(1).folder("Archive", 3);
            f.server(1).deliver("INBOX", true, RAW);
        });
        settle(&w);
        w.store().write(|c| mail::archive_tx(c, 1)).unwrap();
        settle(&w);

        let (n, uid): (i64, Option<i64>) = w
            .store()
            .conn()
            .query_row(
                "SELECT (SELECT COUNT(*) FROM message), uid FROM server_msg",
                [],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap();
        assert_eq!(n, 1, "adopted, not duplicated");
        assert!(uid.is_some(), "identity re-established by Message-ID");

        // And the flag intent left waiting now pushes cleanly.
        let arch = archive_folder(&w);
        assert!(arch > 0);
        w.store().write(|c| mail::mark_read_tx(c, 1)).unwrap();
        settle(&w);
        assert!(!w.with_fake(|f| f.server(1).folders["Archive"].2[0].unread));
    }

    fn forwarded_of(w: &World, id: i64) -> bool {
        w.store()
            .conn()
            .query_row("SELECT forwarded FROM message WHERE id = ?1", [id], |r| {
                r.get(0)
            })
            .unwrap()
    }

    /// `$Forwarded` rides the desired/actual split like the read flag: it
    /// arrives with a fetch, a mark another client clears is followed
    /// while nothing local disagrees, and a local mark pushes.
    #[test]
    fn the_forwarded_keyword_syncs_both_ways() {
        let w = world();
        let uid = w.with_fake(|f| {
            let u = f.server(1).deliver("INBOX", false, RAW);
            f.server(1).set_forwarded("INBOX", u, true);
            u
        });
        settle(&w);
        assert!(forwarded_of(&w, 1), "ingested with the mail");
        assert!(deeds(&w).is_empty(), "a fact queues nothing");

        w.with_fake(|f| f.server(1).set_forwarded("INBOX", uid, false));
        settle(&w);
        assert!(
            !forwarded_of(&w, 1),
            "another client's clearing is followed"
        );

        w.store()
            .write(|c| {
                c.execute("UPDATE message SET forwarded = 1 WHERE id = 1", [])
                    .map(|_| ())
            })
            .unwrap();
        settle(&w);
        assert!(
            w.with_fake(|f| f.server(1).folders["INBOX"].2[0].forwarded),
            "pushed"
        );
        assert!(deeds(&w).contains(&("forwarded".to_string(), "done".to_string())));
        let before = w.jobs().len();
        settle(&w);
        assert_eq!(w.jobs().len(), before, "convergence is quiet");
    }

    /// A forward carries its source's chain in `References` and no
    /// `In-Reply-To`, so its Sent copy folds into the conversation when it
    /// syncs back; the mail it passed on is marked once the send has gone,
    /// and the mark reaches the server as `$Forwarded` on the next push.
    #[test]
    fn a_sent_forward_threads_with_its_source_and_marks_it() {
        let w = world();
        w.with_fake(|f| {
            f.server(1).folder("Sent", 5);
            f.server(1).deliver("INBOX", false, RAW);
        });
        settle(&w);

        let now = w.now();
        let draft = mail::Draft {
            to: "x@y".into(),
            subject: "Fwd: Budget v2".into(),
            body: "fyi".into(),
        };
        w.store()
            .write(move |c| {
                mail::upsert_draft_tx(c, 9, Seed::Forward(1), &draft, now)?;
                mail::file_send_tx(c, 9, now)
            })
            .unwrap();
        assert!(!forwarded_of(&w, 1), "not before it has gone");
        settle(&w);

        let sent = w.with_fake(|f| f.server(1).submitted.clone());
        assert_eq!(sent.len(), 1);
        assert_eq!(sent[0].in_reply_to, None, "a forward is not a reply");
        assert_eq!(sent[0].references, vec!["budget-v2@kovac.io".to_string()]);

        assert!(forwarded_of(&w, 1), "marked once sent");
        assert!(
            w.with_fake(|f| f.server(1).folders["INBOX"].2[0].forwarded),
            "$Forwarded set upstream"
        );
        let sent_id: i64 = w
            .store()
            .conn()
            .query_row(
                "SELECT m.id FROM message m JOIN folder f ON f.id = m.folder
                 WHERE f.role = 'sent'",
                [],
                |r| r.get(0),
            )
            .expect("the Sent copy synced back");
        assert_eq!(
            mail::thread_of(w.store(), sent_id),
            mail::thread_of(w.store(), 1),
            "the forward is in the conversation"
        );
    }

    /// A UIDVALIDITY change wipes the folder and refetches inside the cap.
    #[test]
    fn uidvalidity_reset_refetches() {
        let w = world();
        w.with_fake(|f| f.server(1).deliver("INBOX", false, RAW));
        settle(&w);
        assert_eq!(inbox_rows(&w).len(), 1);
        w.with_fake(|f| f.server(1).folders.get_mut("INBOX").unwrap().0 = 8);
        settle(&w);
        assert_eq!(inbox_rows(&w).len(), 1, "refetched, not duplicated");
    }

    /// First contact with a big folder fetches only the newest [`FETCH_CAP`].
    #[test]
    fn first_contact_respects_the_cap() {
        let w = world();
        w.with_fake(|f| {
            for i in 0..(FETCH_CAP + 50) {
                f.server(1)
                    .deliver("INBOX", false, &format!("Subject: m{i}\r\n\r\nx"));
            }
        });
        settle(&w);
        assert_eq!(inbox_rows(&w).len(), FETCH_CAP as usize);
    }

    /// Offline: the pass fails honestly, queues nothing, and the account's
    /// status line says so.
    #[test]
    fn offline_queues_nothing_and_says_so() {
        let w = world();
        w.with_fake(|f| f.down = Some("network is down".into()));
        settle(&w);
        assert!(w.jobs().is_empty());
        let status: Option<String> = w
            .store()
            .conn()
            .query_row("SELECT status FROM account WHERE id=1", [], |r| r.get(0))
            .unwrap();
        assert!(status.unwrap().contains("network is down"));
    }

    /// Both readings survive the trip: a multipart/alternative mail lands
    /// with its text in `body` and its narrowed HTML in `html`, and the
    /// panel's query hands back the pair.
    #[test]
    fn alternative_mail_keeps_both_readings() {
        let w = world();
        w.with_fake(|f| f.server(1).deliver("INBOX", true, RAW_ALT));
        settle(&w);

        let m = crate::mail::mail(w.store(), 1).expect("the mail");
        assert_eq!(m.body, "Plain reading.");
        let html = m.html.expect("the html reading");
        // Quoted-printable decoded, layout unwrapped, style and tracking
        // pixel gone, emphasis and link intact. The newline that separated
        // the two in the source is gone with the paragraph's own close: a
        // block separates itself, and the widget starts the link on a new
        // line without being told.
        assert_eq!(
            html,
            "<p>Rich <b>reading</b>.</p><a href=\"https://x.dev\">link</a>"
        );

        // A text-only mail leaves `html` empty, so the panel keeps showing
        // the plain reading.
        w.with_fake(|f| f.server(1).deliver("INBOX", true, RAW));
        settle(&w);
        assert_eq!(
            crate::mail::mail(w.store(), 2).expect("plain mail").html,
            None
        );
    }

    /// Mail that arrived before schema v6 gains its HTML from the `raw`
    /// blob it already kept — no refetch.
    #[test]
    fn the_migration_backfills_html_from_raw() {
        let w = world();
        w.with_fake(|f| f.server(1).deliver("INBOX", true, RAW_ALT));
        settle(&w);
        // Rewind to the pre-v6 world: the column exists but is empty.
        w.store()
            .write(|c| c.execute("UPDATE message SET html = NULL", []).map(|_| ()))
            .unwrap();

        // Read the column, not the query layer: the real backfill runs
        // inside `Store::open`, before any query has been cached, so it
        // never needs to advance a generation.
        let html_of = || -> Option<String> {
            w.store()
                .conn()
                .query_row("SELECT html FROM message WHERE id=1", [], |r| r.get(0))
                .expect("the row")
        };
        assert_eq!(html_of(), None);
        w.store().write(|tx| crate::store::backfill_html(tx)).unwrap();
        assert!(html_of().expect("backfilled").contains("<b>reading</b>"));
    }
}

