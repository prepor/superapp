//! The IMAP sync engine: one worker thread per account, each with its own
//! [`World`] over its own connection to the one store (WAL; the UI notices
//! foreign commits via `data_version` — see
//! [`crate::store::Store::poll_external`]).
//!
//! Sync is **ingest**, not action: nothing here is undoable, and nothing
//! here fights the user. Local intent (`message`) and server fact
//! (`server_msg`) are separate columns, and their disagreement *is* the push
//! queue.
//!
//! Two rules this module exists to obey (CR-004):
//!
//! - **The push pass does not talk to the server.** It materializes each
//!   disagreement as a [`mail::Move`] or [`mail::Seen`] job and lets the
//!   executor perform it. Every job revalidates first, so a disagreement
//!   that undo removes before the executor reaches it is never pushed at
//!   all — undo still costs zero server traffic.
//! - **No effect runs inside a transaction.** The fetch pass gathers
//!   everything it needs over the network first, then commits once. It used
//!   to hold `BEGIN IMMEDIATE` across three round trips per folder.

use std::collections::HashSet;
use std::path::PathBuf;
use std::sync::mpsc;
use std::time::Duration;

use rusqlite::Transaction;

use crate::effect::{Clock, Creds, RemoteMail, Secrets, World};
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
/// folder or read state — becomes a job, unless one is already in flight
/// for it. No network here at all.
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
        w.store()
            .write(|tx| {
                if p.moving {
                    if !in_flight(tx, "move", p.message)? {
                        w.enqueue_in(
                            tx,
                            &mail::Move {
                                account,
                                message: p.message,
                                to_folder: p.want_folder,
                                from: p.have_name.clone(),
                                to: p.want_name.clone(),
                                uid: p.uid,
                            },
                        )?;
                    }
                    // The flag push waits for the move to re-establish
                    // identity — a uid in the old folder means nothing in
                    // the new one.
                    return Ok(());
                }
                if p.unread == p.seen && !in_flight(tx, "seen", p.message)? {
                    w.enqueue_in(
                        tx,
                        &mail::Seen {
                            account,
                            message: p.message,
                            folder: p.have_name.clone(),
                            uid: p.uid,
                            seen: !p.unread,
                        },
                    )?;
                }
                Ok(())
            })
            .map_err(err)?;
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
fn fetch_account(w: &World, account: i64) -> Result<(), String> {
    let err = |e: rusqlite::Error| e.to_string();
    for rf in w.run(&mail::Folders { account })? {
        let Some(role) = rf.role.clone() else { continue };
        let meta = w.run(&mail::Meta {
            account,
            folder: rf.name.clone(),
        })?;

        // The folder row and what we last knew about it — a short write,
        // no network in sight.
        let (fid, known): (i64, (Option<i64>, Option<i64>)) = w
            .store()
            .write(|tx| {
                let fid: i64 = tx
                    .query_row(
                        "SELECT id FROM folder WHERE account = ?1 AND name = ?2",
                        rusqlite::params![account, rf.name],
                        |r| r.get(0),
                    )
                    .or_else(|_| {
                        tx.execute(
                            "INSERT INTO folder(account, name, role) VALUES(?1, ?2, ?3)",
                            rusqlite::params![account, rf.name, role],
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
        let server = w.run(&mail::Uids {
            account,
            folder: rf.name.clone(),
            unread_only: false,
        })?;
        let unseen = w.run(&mail::Uids {
            account,
            folder: rf.name.clone(),
            unread_only: true,
        })?;

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
            .write(|tx| land(tx, account, from, &g))
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
    let local: Vec<(i64, u32, bool, bool)> = {
        let mut stmt = tx.prepare(
            "SELECT m.id, s.uid, s.seen, m.unread
             FROM server_msg s JOIN message m ON m.id = s.message
             WHERE s.folder = ?1 AND s.uid IS NOT NULL",
        )?;
        let rows = stmt.query_map([g.fid], |r| {
            Ok((
                r.get::<_, i64>(0)?,
                r.get::<_, i64>(1)? as u32,
                r.get(2)?,
                r.get(3)?,
            ))
        })?;
        rows.collect::<rusqlite::Result<Vec<_>>>()?
    };
    for (id, uid, seen, unread) in local {
        if !g.server.contains(&uid) {
            // Gone upstream (deleted, or moved beyond our mirror): deletion
            // wins, divergent intent included.
            tx.execute("DELETE FROM message WHERE id = ?1", [id])?;
            tx.execute("DELETE FROM server_msg WHERE message = ?1", [id])?;
            continue;
        }
        let now_seen = !g.unseen.contains(&uid);
        if now_seen != seen {
            tx.execute(
                "UPDATE server_msg SET seen = ?1 WHERE message = ?2",
                rusqlite::params![now_seen, id],
            )?;
            // Clean rows (intent agrees with the old server state) follow
            // the server; divergent intent will be pushed over it instead.
            if unread != seen {
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
                "UPDATE server_msg SET folder = ?1, uid = ?2, seen = ?3 WHERE message = ?4",
                rusqlite::params![folder, m.uid, !m.unread, id],
            )?;
            return Ok(());
        }
    }
    tx.execute(
        "INSERT INTO message(account, folder, from_name, from_email,
                             subject, date, unread, body, raw, message_id)
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
            m.raw,
            p.message_id,
        ],
    )?;
    tx.execute(
        "INSERT INTO server_msg(message, folder, uid, seen)
         VALUES(?1, ?2, ?3, ?4)",
        rusqlite::params![tx.last_insert_rowid(), folder, m.uid, !m.unread],
    )?;
    Ok(())
}

/// What panels need out of an RFC 822 blob.
pub struct ParsedMail {
    pub from_name: String,
    pub from_email: String,
    pub subject: String,
    pub date: f64,
    pub body: String,
    /// The Message-ID header — move adoption (and threading, someday).
    pub message_id: String,
}

/// MIME → panel text via `mail-parser`. Paragraph structure survives as
/// the `\n\n` convention the message panel already renders.
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
            message_id: String::new(),
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
    ParsedMail {
        from_name,
        from_email,
        subject: msg.subject().unwrap_or("(no subject)").to_string(),
        date: msg.date().map(|d| d.to_timestamp() as f64).unwrap_or(0.0),
        body: msg
            .body_text(0)
            .map(|t| t.replace("\r\n", "\n").trim().to_string())
            .unwrap_or_default(),
        message_id: msg.message_id().unwrap_or_default().to_string(),
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
    db: PathBuf,
    account: i64,
    secrets: Secrets,
    clock: Clock,
    notify: impl Fn() + Send + 'static,
) -> Worker {
    let (tx, rx) = mpsc::channel::<()>();
    std::thread::Builder::new()
        .name(format!("sync-{account}"))
        .spawn(move || {
            let Ok(store) = crate::store::Store::open(Some(&db)) else {
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
                let _ = w.store().write(|c| {
                    c.execute(
                        "UPDATE account SET status = ?1, synced = ?2 WHERE id = ?3",
                        rusqlite::params![
                            status,
                            outcome.is_ok().then(|| w.now()),
                            account
                        ],
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
            let _ = w.store().write(|c| {
                c.execute(
                    "UPDATE account SET status = ?1 WHERE id = ?2",
                    rusqlite::params![format!("error: {e}"), a],
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
        db: &std::path::Path,
        secrets: &Secrets,
        clock: &Clock,
        notify: impl Fn() + Send + Clone + 'static,
    ) {
        let Pump::Threads { workers, sender } = self else {
            return;
        };
        if sender.is_none() {
            *sender = Some(crate::send::spawn(
                db.to_path_buf(),
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
                db.to_path_buf(),
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
    use crate::effect::World;

    const RAW: &str = "From: Vera Kovac <vera@kovac.io>\r\n\
Subject: Budget v2\r\n\
Message-ID: <budget-v2@kovac.io>\r\n\
Date: Mon, 31 Aug 2026 09:14:00 +0000\r\n\
\r\n\
First paragraph.\r\n\r\nSecond paragraph.\r\n";

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
            .write(|c| {
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
            .write(|c| {
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
}

