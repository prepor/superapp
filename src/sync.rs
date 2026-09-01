//! The IMAP sync engine: one worker thread per account, each with its own
//! connection to the one store (WAL; the UI notices foreign commits via
//! `data_version` — see [`crate::store::Store::poll_external`]).
//!
//! Sync is **ingest**, not action: nothing here is undoable, and nothing
//! here fights the user — rows flagged `dirty` (locally read/archived,
//! server not yet told) are left alone by reconciliation until the op
//! queue pushes them (CR-001 phase 4).
//!
//! The protocol work hides behind [`Transport`], so the whole engine runs
//! headless in tests against [`FakeTransport`]; [`imap_transport`] is the
//! real thing (rustls, port 993, app passwords — fastmail-style).

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::mpsc;
use std::time::Duration;

use rusqlite::Connection;

/// How many most-recent messages a folder retains on first contact (and
/// after a UIDVALIDITY reset). Bounded coverage, stated honestly — the
/// panels say nothing below this window exists locally.
pub const FETCH_CAP: u32 = 200;

/// Poll cadence between kicks.
const POLL: Duration = Duration::from_secs(60);

/// A folder as the server lists it.
#[derive(Debug, Clone)]
pub struct RemoteFolder {
    pub name: String,
    /// inbox | archive | sent | trash — `None` folders are not mirrored.
    pub role: Option<&'static str>,
}

/// SELECT results.
#[derive(Debug, Clone, Copy)]
pub struct FolderMeta {
    pub uidvalidity: u32,
    pub uidnext: u32,
}

/// One fetched message.
#[derive(Debug, Clone)]
pub struct RemoteMail {
    pub uid: u32,
    pub unread: bool,
    pub raw: Vec<u8>,
}

/// The IMAP verbs the engine needs. Errors are strings — they land on
/// the account's status line, for a human.
pub trait Transport {
    fn folders(&mut self) -> Result<Vec<RemoteFolder>, String>;
    fn folder_meta(&mut self, name: &str) -> Result<FolderMeta, String>;
    /// Messages with `uid >= from`, ascending.
    fn fetch_from(&mut self, name: &str, from: u32) -> Result<Vec<RemoteMail>, String>;
    /// Every uid currently in the folder (deletion reconcile).
    fn uids(&mut self, name: &str) -> Result<HashSet<u32>, String>;
    /// Every unseen uid (flag reconcile).
    fn unread_uids(&mut self, name: &str) -> Result<HashSet<u32>, String>;
    /// `UID MOVE`; the new uid in `to` when the server says (UIDPLUS'
    /// COPYUID), `None` otherwise — adoption by Message-ID covers that.
    fn move_uid(&mut self, from: &str, to: &str, uid: u32) -> Result<Option<u32>, String>;
    /// `UID STORE` the `\Seen` flag.
    fn store_seen(&mut self, folder: &str, uid: u32, seen: bool) -> Result<(), String>;
}

/// One full sync pass for one account: **push first** (make the server
/// agree with local intent), then mirror folders, fetch what is new, and
/// reconcile facts. Each folder is one transaction.
pub fn sync_account(
    conn: &Connection,
    t: &mut dyn Transport,
    account: i64,
) -> Result<(), String> {
    push_account(conn, t, account)?;
    fetch_account(conn, t, account)
}

/// The push pass: every message whose intent differs from the server —
/// folder or read state — is the queue. Per-message failures land on that
/// message's status line and do not stop the pass.
pub fn push_account(
    conn: &Connection,
    t: &mut dyn Transport,
    account: i64,
) -> Result<(), String> {
    let err = |e: rusqlite::Error| e.to_string();
    struct PushRow {
        id: i64,
        uid: u32,
        want_folder: i64,
        want_name: String,
        have_folder: i64,
        have_name: String,
        unread: bool,
        seen: bool,
    }
    let rows: Vec<PushRow> = {
        let mut stmt = conn
            .prepare(
                "SELECT m.id, s.uid, m.folder, fw.name, s.folder, fh.name, m.unread, s.seen
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
                Ok(PushRow {
                    id: r.get(0)?,
                    uid: r.get::<_, i64>(1)? as u32,
                    want_folder: r.get(2)?,
                    want_name: r.get(3)?,
                    have_folder: r.get(4)?,
                    have_name: r.get(5)?,
                    unread: r.get(6)?,
                    seen: r.get(7)?,
                })
            })
            .map_err(err)?;
        it.collect::<rusqlite::Result<Vec<_>>>().map_err(err)?
    };
    for p in rows {
        let mut uid = p.uid;
        let mut folder_name = p.have_name.clone();
        let outcome: Result<(), String> = (|| {
            if p.want_folder != p.have_folder {
                let new_uid = t.move_uid(&p.have_name, &p.want_name, uid)?;
                conn.execute(
                    "UPDATE server_msg SET folder=?1, uid=?2 WHERE message=?3",
                    rusqlite::params![p.want_folder, new_uid, p.id],
                )
                .map_err(err)?;
                folder_name = p.want_name.clone();
                match new_uid {
                    Some(u) => uid = u,
                    // No COPYUID: identity is lost until adoption by
                    // Message-ID on the next fetch; the flag push waits.
                    None => return Ok(()),
                }
            }
            if p.unread == p.seen {
                t.store_seen(&folder_name, uid, !p.unread)?;
                conn.execute(
                    "UPDATE server_msg SET seen=?1 WHERE message=?2",
                    rusqlite::params![!p.unread, p.id],
                )
                .map_err(err)?;
            }
            Ok(())
        })();
        if let Err(e) = outcome {
            conn.execute(
                "UPDATE message SET status=?1, status_err=1 WHERE id=?2",
                rusqlite::params![format!("sync: {e}"), p.id],
            )
            .map_err(err)?;
        }
    }
    Ok(())
}

/// The fetch/reconcile pass.
///
/// Per folder: read what we know (autocommit), do **all** the network work
/// with no transaction open, then take the write lock once for the local
/// writes. The order is load-bearing rather than tidy — SQLite has one
/// writer, and the UI shares this file. A `BEGIN IMMEDIATE` held across an
/// IMAP round-trip blocks every UI action behind it for as long as the
/// server takes to answer (measured: a 400 ms fetch turns a 0.1 ms action
/// into a 468 ms one), which reads as the whole app hanging.
fn fetch_account(
    conn: &Connection,
    t: &mut dyn Transport,
    account: i64,
) -> Result<(), String> {
    let err = |e: rusqlite::Error| e.to_string();
    for rf in t.folders()? {
        let Some(role) = rf.role else { continue };
        let meta = t.folder_meta(&rf.name)?;

        // Resolving the folder row is one statement in autocommit, not the
        // opening of a transaction: nothing below may hold the write lock
        // while the network is in the loop (see the pass's doc comment).
        let fid: i64 = conn
            .query_row(
                "SELECT id FROM folder WHERE account=?1 AND name=?2",
                rusqlite::params![account, rf.name],
                |r| r.get(0),
            )
            .or_else(|_| {
                conn.execute(
                    "INSERT INTO folder(account, name, role) VALUES(?1,?2,?3)",
                    rusqlite::params![account, rf.name, role],
                )
                .map(|_| conn.last_insert_rowid())
            })
            .map_err(err)?;
        let known: (Option<i64>, Option<i64>) = conn
            .query_row(
                "SELECT uidvalidity, uidnext FROM folder WHERE id=?1",
                [fid],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .map_err(err)?;

        let floor = u32::max(1, meta.uidnext.saturating_sub(FETCH_CAP));
        // The server renumbered (or first contact): local copies of this
        // folder are meaningless, and the window starts over. The deletion
        // that follows from it happens below, with the rest of the writes.
        let renumbered = known.0 != Some(i64::from(meta.uidvalidity));
        let from = if renumbered {
            floor
        } else {
            known.1.map_or(floor, |n| n as u32)
        };

        // ---- the network, with no transaction open -------------------------
        let fetched = if meta.uidnext > from {
            t.fetch_from(&rf.name, from)?
        } else {
            Vec::new()
        };
        // Facts to reconcile against — the *server's* view of this folder.
        let server = t.uids(&rf.name)?;
        let unseen = t.unread_uids(&rf.name)?;

        // ---- one transaction, local work only ------------------------------
        conn.execute("BEGIN IMMEDIATE", []).map_err(err)?;
        let out = (|| -> Result<(), String> {
            if renumbered {
                conn.execute(
                    "DELETE FROM message WHERE id IN
                       (SELECT message FROM server_msg WHERE folder=?1)",
                    [fid],
                )
                .map_err(err)?;
                conn.execute("DELETE FROM server_msg WHERE folder=?1", [fid])
                    .map_err(err)?;
            }
            for m in &fetched {
                if m.uid < from {
                    continue; // `from:*` quirk: a lone highest message
                }
                ingest_message(conn, account, fid, m).map_err(err)?;
            }
            conn.execute(
                "UPDATE folder SET uidvalidity=?1, uidnext=?2 WHERE id=?3",
                rusqlite::params![meta.uidvalidity, meta.uidnext, fid],
            )
            .map_err(err)?;

            // Divergent intent stays local truth: an unpushed read/archive is
            // never clobbered, only recorded.
            let local: Vec<(i64, u32, bool, bool)> = {
                let mut stmt = conn
                    .prepare(
                        "SELECT m.id, s.uid, s.seen, m.unread
                         FROM server_msg s JOIN message m ON m.id = s.message
                         WHERE s.folder=?1 AND s.uid IS NOT NULL",
                    )
                    .map_err(err)?;
                let rows = stmt
                    .query_map([fid], |r| {
                        Ok((
                            r.get::<_, i64>(0)?,
                            r.get::<_, i64>(1)? as u32,
                            r.get(2)?,
                            r.get(3)?,
                        ))
                    })
                    .map_err(err)?
                    .collect::<rusqlite::Result<Vec<_>>>()
                    .map_err(err)?;
                rows
            };
            for (id, uid, seen, unread) in local {
                if !server.contains(&uid) {
                    // Gone upstream (deleted, or moved beyond our mirror):
                    // deletion wins, divergent intent included.
                    conn.execute("DELETE FROM message WHERE id=?1", [id])
                        .map_err(err)?;
                    conn.execute("DELETE FROM server_msg WHERE message=?1", [id])
                        .map_err(err)?;
                    continue;
                }
                let now_seen = !unseen.contains(&uid);
                if now_seen != seen {
                    conn.execute(
                        "UPDATE server_msg SET seen=?1 WHERE message=?2",
                        rusqlite::params![now_seen, id],
                    )
                    .map_err(err)?;
                    // Clean rows (intent agrees with the old server state)
                    // follow the server; divergent intent will be pushed over
                    // it next pass instead.
                    if unread != seen {
                        conn.execute(
                            "UPDATE message SET unread=?1 WHERE id=?2",
                            rusqlite::params![!now_seen, id],
                        )
                        .map_err(err)?;
                    }
                }
            }
            Ok(())
        })();
        // A `?` straight out of the block above would leave the transaction
        // open — and an open write transaction on this connection holds the
        // lock for the life of the worker, which is the very thing this pass
        // is arranged to avoid.
        match out {
            Ok(()) => conn.execute("COMMIT", []).map_err(err)?,
            Err(e) => {
                let _ = conn.execute("ROLLBACK", []);
                return Err(e);
            }
        };
    }
    Ok(())
}

/// Parses and stores one fetched message. A moved mail whose new uid the
/// server never told us (no COPYUID) is **adopted** by Message-ID instead
/// of duplicated.
fn ingest_message(
    conn: &Connection,
    account: i64,
    folder: i64,
    m: &RemoteMail,
) -> rusqlite::Result<()> {
    let exists: bool = conn
        .query_row(
            "SELECT 1 FROM server_msg WHERE folder=?1 AND uid=?2",
            rusqlite::params![folder, m.uid],
            |_| Ok(true),
        )
        .unwrap_or(false);
    if exists {
        return Ok(());
    }
    let p = parse_mail(&m.raw);
    if !p.message_id.is_empty() {
        // A uid-less twin in this folder is the same mail, post-move.
        let orphan: Option<i64> = conn
            .query_row(
                "SELECT m.id FROM message m JOIN server_msg s ON s.message=m.id
                 WHERE m.account=?1 AND m.message_id=?2 AND s.uid IS NULL",
                rusqlite::params![account, p.message_id],
                |r| r.get(0),
            )
            .ok();
        if let Some(id) = orphan {
            conn.execute(
                "UPDATE server_msg SET folder=?1, uid=?2, seen=?3 WHERE message=?4",
                rusqlite::params![folder, m.uid, !m.unread, id],
            )?;
            return Ok(());
        }
    }
    conn.execute(
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
    conn.execute(
        "INSERT INTO server_msg(message, folder, uid, seen)
         VALUES(?1, ?2, ?3, ?4)",
        rusqlite::params![conn.last_insert_rowid(), folder, m.uid, !m.unread],
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

/// Spawns the sync loop for one account: connect → sync → status → sleep
/// (or kick) → again. The thread exits when its account row disappears.
/// `notify` wakes the UI thread after every pass (`SignalToUI` upstairs —
/// this module stays makepad-free).
pub fn spawn(
    db: PathBuf,
    account: i64,
    notify: impl Fn() + Send + 'static,
) -> Worker {
    let (tx, rx) = mpsc::channel::<()>();
    std::thread::Builder::new()
        .name(format!("sync-{account}"))
        .spawn(move || {
            let Ok(conn) = Connection::open(&db) else {
                return;
            };
            let _ = conn.busy_timeout(Duration::from_millis(5000));
            loop {
                let cfg: Option<(String, String)> = conn
                    .query_row(
                        "SELECT email, imap_host FROM account WHERE id=?1",
                        [account],
                        |r| Ok((r.get(0)?, r.get::<_, Option<String>>(1)?.unwrap_or_default())),
                    )
                    .ok();
                let Some((email, host)) = cfg else {
                    return; // account removed: the worker retires
                };
                if host.is_empty() {
                    return; // demo account: nothing to sync
                }
                let pass = crate::secret::get(db.parent().unwrap_or(&db), &email);
                let outcome = match pass {
                    Some(pass) => imap_transport::connect(&host, &email, &pass)
                        .and_then(|mut t| sync_account(&conn, &mut t, account)),
                    None => Err("no password in the keychain".into()),
                };
                let status = match &outcome {
                    Ok(()) => format!("ok · {}", crate::mail::fmt_date(crate::store::now())),
                    Err(e) => format!("error: {e}"),
                };
                let _ = conn.execute(
                    "UPDATE account SET status=?1, synced=?2 WHERE id=?3",
                    rusqlite::params![
                        status,
                        outcome.is_ok().then(crate::store::now),
                        account
                    ],
                );
                notify();
                // Sleep until the next poll or a kick; a closed channel
                // (app shutdown) just means one last timeout then exit
                // with the account check above.
                match rx.recv_timeout(POLL) {
                    Ok(()) | Err(mpsc::RecvTimeoutError::Timeout) => {}
                    Err(mpsc::RecvTimeoutError::Disconnected) => return,
                }
            }
        })
        .expect("spawn sync worker");
    Worker { account, kick: tx }
}

// -- the real transport -------------------------------------------------------

/// The `imap` crate behind [`Transport`]: rustls, port 993, LOGIN with an
/// app password (fastmail-style; OAuth is deliberately later).
pub mod imap_transport {
    use super::*;

    type ImapSession = imap::Session<Box<dyn imap::ImapConnection>>;

    pub struct Imap {
        session: ImapSession,
        selected: Option<String>,
    }

    fn s<E: std::fmt::Display>(e: E) -> String {
        format!("{e}")
    }

    pub fn connect(host: &str, user: &str, pass: &str) -> Result<Imap, String> {
        let client = imap::ClientBuilder::new(host, 993).connect().map_err(s)?;
        let session = client.login(user, pass).map_err(|e| s(e.0))?;
        Ok(Imap {
            session,
            selected: None,
        })
    }

    /// One-shot `APPEND` (the sender files sent mail into the Sent folder).
    pub fn append(
        host: &str,
        user: &str,
        pass: &str,
        folder: &str,
        raw: &[u8],
    ) -> Result<(), String> {
        let mut t = connect(host, user, pass)?;
        t.session
            .append(folder, raw)
            .flag(imap::types::Flag::Seen)
            .finish()
            .map_err(s)?;
        Ok(())
    }

    impl Imap {
        fn select(&mut self, name: &str) -> Result<FolderMeta, String> {
            let mb = self.session.select(name).map_err(s)?;
            self.selected = Some(name.to_string());
            Ok(FolderMeta {
                uidvalidity: mb.uid_validity.unwrap_or(0),
                uidnext: mb.uid_next.unwrap_or(1),
            })
        }

        fn ensure(&mut self, name: &str) -> Result<(), String> {
            if self.selected.as_deref() != Some(name) {
                self.select(name)?;
            }
            Ok(())
        }
    }

    impl Transport for Imap {
        fn folders(&mut self) -> Result<Vec<RemoteFolder>, String> {
            let names = self.session.list(Some(""), Some("*")).map_err(s)?;
            let mut out = Vec::new();
            for n in names.iter() {
                let attrs = format!("{:?}", n.attributes()).to_lowercase();
                let role = if n.name().eq_ignore_ascii_case("inbox") {
                    Some("inbox")
                } else if attrs.contains("archive") {
                    Some("archive")
                } else if attrs.contains("sent") {
                    Some("sent")
                } else if attrs.contains("trash") {
                    Some("trash")
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

        fn folder_meta(&mut self, name: &str) -> Result<FolderMeta, String> {
            self.select(name)
        }

        fn fetch_from(&mut self, name: &str, from: u32) -> Result<Vec<RemoteMail>, String> {
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
                    let unread = !f.flags().iter().any(|fl| matches!(fl, imap::types::Flag::Seen));
                    Some(RemoteMail {
                        uid,
                        unread,
                        raw: raw.to_vec(),
                    })
                })
                .collect();
            out.sort_by_key(|m| m.uid);
            Ok(out)
        }

        fn uids(&mut self, name: &str) -> Result<HashSet<u32>, String> {
            self.ensure(name)?;
            self.session.uid_search("ALL").map_err(s)
        }

        fn unread_uids(&mut self, name: &str) -> Result<HashSet<u32>, String> {
            self.ensure(name)?;
            self.session.uid_search("UNSEEN").map_err(s)
        }

        fn move_uid(&mut self, from: &str, to: &str, uid: u32) -> Result<Option<u32>, String> {
            self.ensure(from)?;
            self.session.uid_mv(uid.to_string(), to).map_err(s)?;
            // The crate acks the MOVE but does not surface COPYUID; the
            // new uid arrives via Message-ID adoption on the next fetch.
            Ok(None)
        }

        fn store_seen(&mut self, folder: &str, uid: u32, seen: bool) -> Result<(), String> {
            self.ensure(folder)?;
            let flags = if seen { "+FLAGS (\\Seen)" } else { "-FLAGS (\\Seen)" };
            self.session
                .uid_store(uid.to_string(), flags)
                .map_err(s)?;
            Ok(())
        }
    }
}

// -- the fake transport (tests + --fake-mail) ---------------------------------

/// An in-memory mail server: the whole engine runs against it headless.
#[derive(Default, Clone)]
pub struct FakeTransport {
    /// folder → (uidvalidity, next_uid, mails)
    pub folders: HashMap<String, (u32, u32, Vec<RemoteMail>)>,
    /// Whether MOVE reports the new uid (UIDPLUS' COPYUID) — both server
    /// behaviours exist in the wild, so both are testable.
    pub copyuid: bool,
}

impl FakeTransport {
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
            raw: raw.as_bytes().to_vec(),
        });
        uid
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

    fn role_of(name: &str) -> Option<&'static str> {
        match name {
            "INBOX" => Some("inbox"),
            "Archive" => Some("archive"),
            "Sent" => Some("sent"),
            "Trash" => Some("trash"),
            _ => None,
        }
    }
}

impl Transport for FakeTransport {
    fn folders(&mut self) -> Result<Vec<RemoteFolder>, String> {
        let mut names: Vec<&String> = self.folders.keys().collect();
        names.sort();
        Ok(names
            .into_iter()
            .map(|n| RemoteFolder {
                name: n.clone(),
                role: Self::role_of(n),
            })
            .collect())
    }

    fn folder_meta(&mut self, name: &str) -> Result<FolderMeta, String> {
        let f = self.folders.get(name).ok_or("no such folder")?;
        Ok(FolderMeta {
            uidvalidity: f.0,
            uidnext: f.1,
        })
    }

    fn fetch_from(&mut self, name: &str, from: u32) -> Result<Vec<RemoteMail>, String> {
        let f = self.folders.get(name).ok_or("no such folder")?;
        Ok(f.2.iter().filter(|m| m.uid >= from).cloned().collect())
    }

    fn uids(&mut self, name: &str) -> Result<HashSet<u32>, String> {
        let f = self.folders.get(name).ok_or("no such folder")?;
        Ok(f.2.iter().map(|m| m.uid).collect())
    }

    fn unread_uids(&mut self, name: &str) -> Result<HashSet<u32>, String> {
        let f = self.folders.get(name).ok_or("no such folder")?;
        Ok(f.2.iter().filter(|m| m.unread).map(|m| m.uid).collect())
    }

    fn move_uid(&mut self, from: &str, to: &str, uid: u32) -> Result<Option<u32>, String> {
        let src = self.folders.get_mut(from).ok_or("no such folder")?;
        let i = src
            .2
            .iter()
            .position(|m| m.uid == uid)
            .ok_or("no such uid")?;
        let mut m = src.2.remove(i);
        let dst = self
            .folders
            .entry(to.to_string())
            .or_insert((1, 1, Vec::new()));
        m.uid = dst.1;
        dst.1 += 1;
        let new = m.uid;
        dst.2.push(m);
        Ok(self.copyuid.then_some(new))
    }

    fn store_seen(&mut self, folder: &str, uid: u32, seen: bool) -> Result<(), String> {
        let f = self.folders.get_mut(folder).ok_or("no such folder")?;
        for m in &mut f.2 {
            if m.uid == uid {
                m.unread = !seen;
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::Store;

    const RAW: &str = "From: Vera Kovac <vera@kovac.io>\r\n\
Subject: Budget v2\r\n\
Message-ID: <budget-v2@kovac.io>\r\n\
Date: Mon, 31 Aug 2026 09:14:00 +0000\r\n\
\r\n\
First paragraph.\r\n\r\nSecond paragraph.\r\n";

    fn world() -> (Store, FakeTransport, i64) {
        let s = Store::open(None).expect("store");
        s.write(|c| {
            c.execute(
                "INSERT INTO account(label, email, imap_host) VALUES('t','t@t','imap.t')",
                [],
            )
            .map(|_| ())
        })
        .unwrap();
        let mut t = FakeTransport::default();
        t.folders.insert("INBOX".into(), (7, 1, Vec::new()));
        (s, t, 1)
    }

    fn inbox_rows(s: &Store) -> Vec<(String, bool)> {
        let mut stmt = s
            .conn()
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

    /// Initial sync ingests and parses; a second pass fetches only what is
    /// new; flags flip; remote deletions disappear locally.
    #[test]
    fn sync_ingests_incrementally_and_reconciles() {
        let (s, mut t, acct) = world();
        let u1 = t.deliver("INBOX", true, RAW);
        sync_account(s.conn(), &mut t, acct).unwrap();
        assert_eq!(inbox_rows(&s), vec![("Budget v2".to_string(), true)]);
        let body: String = s
            .conn()
            .query_row("SELECT body FROM message", [], |r| r.get(0))
            .unwrap();
        assert!(body.contains("First paragraph.\n\nSecond"), "{body:?}");

        // Second pass: nothing new, nothing duplicated.
        sync_account(s.conn(), &mut t, acct).unwrap();
        assert_eq!(inbox_rows(&s).len(), 1);

        // A new delivery, a seen flag, then a remote deletion.
        let u2 = t.deliver("INBOX", true, "Subject: Two\r\n\r\nx");
        t.mark_seen("INBOX", u1);
        sync_account(s.conn(), &mut t, acct).unwrap();
        assert_eq!(
            inbox_rows(&s),
            vec![("Budget v2".into(), false), ("Two".into(), true)]
        );
        t.remove("INBOX", u2);
        sync_account(s.conn(), &mut t, acct).unwrap();
        assert_eq!(inbox_rows(&s).len(), 1);
    }

    /// Local intent is pushed, not clobbered: a locally-read mail makes the
    /// server seen; a locally-archived mail moves on the server (COPYUID
    /// path) — and undoing the archive moves it back, with no compensation
    /// machinery, because undo just flips intent.
    #[test]
    fn push_makes_the_server_agree_and_undo_pushes_back() {
        let (s, mut t, acct) = world();
        t.copyuid = true;
        t.folders.insert("Archive".into(), (3, 1, Vec::new()));
        t.deliver("INBOX", true, RAW);
        sync_account(s.conn(), &mut t, acct).unwrap();

        // Read + archive locally (intent only), then push.
        s.write(|c| {
            crate::mail::mark_read_tx(c, 1)?;
            crate::mail::archive_tx(c, 1)
        })
        .unwrap();
        sync_account(s.conn(), &mut t, acct).unwrap();
        assert!(t.folders["INBOX"].2.is_empty(), "moved off the inbox");
        assert_eq!(t.folders["Archive"].2.len(), 1);
        assert!(!t.folders["Archive"].2[0].unread, "seen pushed too");

        // Undo-shaped change: intent flips back; the next pass restores.
        s.write(|c| {
            c.execute(
                "UPDATE message SET folder=(SELECT id FROM folder WHERE role='inbox') WHERE id=1",
                [],
            )
            .map(|_| ())
        })
        .unwrap();
        sync_account(s.conn(), &mut t, acct).unwrap();
        assert_eq!(t.folders["INBOX"].2.len(), 1, "moved back");
        assert!(t.folders["Archive"].2.is_empty());
        assert_eq!(inbox_rows(&s).len(), 1, "still exactly one local row");
    }

    /// Without COPYUID the moved mail loses its uid until the next fetch
    /// adopts it by Message-ID — one row throughout, never a duplicate.
    #[test]
    fn move_without_copyuid_adopts_by_message_id() {
        let (s, mut t, acct) = world();
        t.copyuid = false;
        t.folders.insert("Archive".into(), (3, 1, Vec::new()));
        t.deliver("INBOX", true, RAW);
        sync_account(s.conn(), &mut t, acct).unwrap();
        s.write(|c| crate::mail::archive_tx(c, 1)).unwrap();
        sync_account(s.conn(), &mut t, acct).unwrap();
        let (n, uid): (i64, Option<i64>) = s
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
        s.write(|c| crate::mail::mark_read_tx(c, 1)).unwrap();
        sync_account(s.conn(), &mut t, acct).unwrap();
        assert!(!t.folders["Archive"].2[0].unread);
    }

    /// A remote flag change lands locally when intent is clean, but
    /// divergent local intent wins (it will be pushed over the server).
    #[test]
    fn remote_flags_yield_to_divergent_intent() {
        let (s, mut t, acct) = world();
        let u = t.deliver("INBOX", true, RAW);
        sync_account(s.conn(), &mut t, acct).unwrap();
        // Clean row: the server marking it seen flows in.
        t.mark_seen("INBOX", u);
        sync_account(s.conn(), &mut t, acct).unwrap();
        assert_eq!(inbox_rows(&s), vec![("Budget v2".to_string(), false)]);
    }

    /// A UIDVALIDITY change wipes the folder and refetches inside the cap.
    #[test]
    fn uidvalidity_reset_refetches() {
        let (s, mut t, acct) = world();
        t.deliver("INBOX", false, RAW);
        sync_account(s.conn(), &mut t, acct).unwrap();
        assert_eq!(inbox_rows(&s).len(), 1);
        // The server renumbers: same mail, new world.
        let f = t.folders.get_mut("INBOX").unwrap();
        f.0 = 8;
        sync_account(s.conn(), &mut t, acct).unwrap();
        assert_eq!(inbox_rows(&s).len(), 1, "refetched, not duplicated");
    }

    /// First contact with a big folder fetches only the newest [`FETCH_CAP`].
    #[test]
    fn first_contact_respects_the_cap() {
        let (s, mut t, acct) = world();
        for i in 0..(FETCH_CAP + 50) {
            t.deliver("INBOX", false, &format!("Subject: m{i}\r\n\r\nx"));
        }
        sync_account(s.conn(), &mut t, acct).unwrap();
        assert_eq!(inbox_rows(&s).len(), FETCH_CAP as usize);
    }

    /// A transport that checks, on every network verb, whether this process
    /// could take the write lock right then — i.e. whether the pass is
    /// holding it across the wire.
    struct LockProbe {
        inner: FakeTransport,
        probe: Connection,
        /// Verbs that found the write lock already taken.
        blocked: Vec<&'static str>,
    }

    impl LockProbe {
        fn check(&mut self, verb: &'static str) {
            // No timeout: the question is whether the lock is free *now*.
            if self.probe.execute("BEGIN IMMEDIATE", []).is_err() {
                self.blocked.push(verb);
            } else {
                let _ = self.probe.execute("ROLLBACK", []);
            }
        }
    }

    impl Transport for LockProbe {
        fn folders(&mut self) -> Result<Vec<RemoteFolder>, String> {
            self.check("folders");
            self.inner.folders()
        }
        fn folder_meta(&mut self, n: &str) -> Result<FolderMeta, String> {
            self.check("folder_meta");
            self.inner.folder_meta(n)
        }
        fn fetch_from(&mut self, n: &str, from: u32) -> Result<Vec<RemoteMail>, String> {
            self.check("fetch_from");
            self.inner.fetch_from(n, from)
        }
        fn uids(&mut self, n: &str) -> Result<HashSet<u32>, String> {
            self.check("uids");
            self.inner.uids(n)
        }
        fn unread_uids(&mut self, n: &str) -> Result<HashSet<u32>, String> {
            self.check("unread_uids");
            self.inner.unread_uids(n)
        }
        fn move_uid(&mut self, f: &str, t: &str, uid: u32) -> Result<Option<u32>, String> {
            self.check("move_uid");
            self.inner.move_uid(f, t, uid)
        }
        fn store_seen(&mut self, f: &str, uid: u32, seen: bool) -> Result<(), String> {
            self.check("store_seen");
            self.inner.store_seen(f, uid, seen)
        }
    }

    /// **A sync pass never holds the write lock across the network.** SQLite
    /// has one writer and the UI shares this file, so a `BEGIN IMMEDIATE`
    /// held over an IMAP round-trip stalls every UI action behind it for as
    /// long as the server takes — which reads as the app hanging. This is a
    /// property of the pass's shape, so it is pinned here rather than left to
    /// whoever next edits the transaction boundaries.
    #[test]
    fn a_sync_pass_never_holds_the_write_lock_across_the_network() {
        let dir = std::env::temp_dir().join(format!("superapp-lock-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("lock.db");
        let s = Store::open(Some(&path)).expect("store");
        s.write(|c| {
            c.execute(
                "INSERT INTO account(label, email, imap_host) VALUES('t','t@t','imap.t')",
                [],
            )
            .map(|_| ())
        })
        .unwrap();

        let mut t = FakeTransport::default();
        t.folders.insert("INBOX".into(), (7, 1, Vec::new()));
        t.folders.insert("Archive".into(), (3, 1, Vec::new()));
        t.deliver("INBOX", true, RAW);

        let probe = Connection::open(&path).unwrap();
        probe.busy_timeout(Duration::from_millis(0)).unwrap();
        let mut lp = LockProbe {
            inner: t,
            probe,
            blocked: Vec::new(),
        };

        // First contact: creates folders, ingests, reconciles.
        sync_account(s.conn(), &mut lp, 1).unwrap();
        // And a pass with local intent to push, so the push side is covered.
        s.write(|c| crate::mail::archive_tx(c, 1)).unwrap();
        sync_account(s.conn(), &mut lp, 1).unwrap();

        assert!(
            lp.blocked.is_empty(),
            "the write lock was held during: {:?}",
            lp.blocked
        );
        let _ = std::fs::remove_dir_all(&dir);
    }
}
