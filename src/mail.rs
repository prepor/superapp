//! The mail domain over the store: typed queries, titles, the demo seed,
//! and the local mutations (read flags, archive).
//!
//! Everything panels show comes through the registered [`Q`] queries — that
//! is the reactive contract (see [`crate::store`]) and, later, the panel
//! context an agent receives. Filtering keeps the shell's semantics: the
//! typed filter is one lowercase substring over sender + subject; the
//! launcher's word-AND lives in [`crate::launcher`].

use std::rc::Rc;

use rusqlite::{Connection, Transaction};
use serde::{Deserialize, Serialize};

use crate::core::{Kind, MailId};
use crate::effect::{Creds, Ctx, Deferred, Effect, Outgoing, Registry, World};
use crate::history::Intent;
use crate::store::{Q, Store, Val};

/// One list row: what the inbox and the launcher show.
#[derive(Debug, Clone)]
pub struct MailHead {
    pub id: MailId,
    pub from_name: String,
    pub from_email: String,
    pub subject: String,
    pub date: f64,
    pub unread: bool,
}

/// One whole mail: the message panel's content.
#[derive(Debug, Clone)]
pub struct MailFull {
    pub head: MailHead,
    /// Paragraphs, `\n\n`-separated in the store.
    pub body: String,
    /// The HTML reading, narrowed at ingest to what the panel draws. When
    /// present the panel shows this instead of [`Self::body`] — the letter
    /// keeps its lists, emphasis and links.
    pub html: Option<String>,
    /// An optional status line; `true` marks it as an error.
    pub status: Option<(String, bool)>,
    /// The receiving account's address (the TO line).
    pub to: String,
}

/// A distinct sender: the launcher's contact entries.
#[derive(Debug, Clone)]
pub struct Sender {
    pub email: String,
    pub name: String,
}

static Q_INBOX: Q = Q {
    id: "inbox",
    sql: "SELECT m.id, m.from_name, m.from_email, m.subject, m.date, m.unread
          FROM message m JOIN folder f ON m.folder = f.id
          WHERE f.role = 'inbox'
          ORDER BY m.date DESC, m.id DESC",
    describe: "every mail in the inbox folders, newest first",
};

static Q_ALL: Q = Q {
    id: "all_mail",
    sql: "SELECT id, from_name, from_email, subject, date, unread
          FROM message ORDER BY date DESC, id DESC",
    describe: "every mail, archived included, newest first",
};

static Q_MAIL: Q = Q {
    id: "mail",
    sql: "SELECT m.id, m.from_name, m.from_email, m.subject, m.date, m.unread,
                 m.body, m.status, m.status_err, a.email, m.html
          FROM message m JOIN account a ON a.id = m.account
          WHERE m.id = ?1",
    describe: "one mail, both bodies included, with its account's address",
};

static Q_SENDERS: Q = Q {
    id: "senders",
    sql: "SELECT from_email, from_name, MAX(date) AS last
          FROM message GROUP BY from_email ORDER BY last DESC",
    describe: "distinct senders, most recently heard from first",
};

static Q_CONTACT: Q = Q {
    id: "contact",
    sql: "SELECT from_name, COUNT(*) FROM message WHERE from_email = ?1",
    describe: "a sender's display name and how many mails they sent",
};

static Q_ME: Q = Q {
    id: "me",
    sql: "SELECT email FROM account ORDER BY id LIMIT 1",
    describe: "the local account's address",
};

static Q_ACCOUNTS: Q = Q {
    id: "accounts",
    sql: "SELECT id, label, email, imap_host, smtp_host, status, synced
          FROM account ORDER BY id",
    describe: "every account with its connection config and sync status",
};

fn head_row(r: &rusqlite::Row) -> rusqlite::Result<MailHead> {
    Ok(MailHead {
        id: r.get(0)?,
        from_name: r.get(1)?,
        from_email: r.get(2)?,
        subject: r.get(3)?,
        date: r.get(4)?,
        unread: r.get(5)?,
    })
}

fn full_row(r: &rusqlite::Row) -> rusqlite::Result<MailFull> {
    let status: Option<String> = r.get(7)?;
    let err: bool = r.get(8)?;
    Ok(MailFull {
        head: head_row(r)?,
        body: r.get(6)?,
        html: r.get(10)?,
        status: status.map(|s| (s, err)),
        to: r.get(9)?,
    })
}

fn sender_row(r: &rusqlite::Row) -> rusqlite::Result<Sender> {
    Ok(Sender {
        email: r.get(0)?,
        name: r.get(1)?,
    })
}

/// The inbox, newest first.
pub fn inbox(store: &Store) -> Rc<Vec<MailHead>> {
    store.rows(&Q_INBOX, &[], head_row)
}

/// The inbox under the typed filter: one lowercase substring over
/// sender name + address + subject (the shell's historical semantics).
pub fn inbox_filtered(store: &Store, filter: &str) -> Vec<MailHead> {
    let q = filter.trim().to_lowercase();
    inbox(store)
        .iter()
        .filter(|m| {
            q.is_empty()
                || format!("{} {} {}", m.from_name, m.from_email, m.subject)
                    .to_lowercase()
                    .contains(&q)
        })
        .cloned()
        .collect()
}

/// Every mail, archived included (the launcher's corpus).
pub fn all(store: &Store) -> Rc<Vec<MailHead>> {
    store.rows(&Q_ALL, &[], head_row)
}

/// One mail by id.
pub fn mail(store: &Store, id: MailId) -> Option<MailFull> {
    store.rows(&Q_MAIL, &[Val::I(id)], full_row).first().cloned()
}

/// Distinct senders, most recent first.
pub fn senders(store: &Store) -> Rc<Vec<Sender>> {
    store.rows(&Q_SENDERS, &[], sender_row)
}

/// A sender's `(name, mail count)`; the name falls back to the address.
pub fn contact(store: &Store, email: &str) -> (String, i64) {
    store
        .rows(&Q_CONTACT, &[Val::S(email.to_string())], |r| {
            Ok((r.get::<_, Option<String>>(0)?, r.get::<_, i64>(1)?))
        })
        .first()
        .map(|(name, n)| {
            (
                name.clone().filter(|s| !s.is_empty()).unwrap_or_else(|| email.to_string()),
                *n,
            )
        })
        .unwrap_or_else(|| (email.to_string(), 0))
}

/// One account row, as settings shows it.
#[derive(Debug, Clone)]
pub struct Account {
    pub id: i64,
    pub label: String,
    pub email: String,
    pub imap_host: Option<String>,
    pub smtp_host: Option<String>,
    pub status: Option<String>,
    pub synced: Option<f64>,
}

fn account_row(r: &rusqlite::Row) -> rusqlite::Result<Account> {
    Ok(Account {
        id: r.get(0)?,
        label: r.get(1)?,
        email: r.get(2)?,
        imap_host: r.get(3)?,
        smtp_host: r.get(4)?,
        status: r.get(5)?,
        synced: r.get(6)?,
    })
}

/// Every account.
pub fn accounts(store: &Store) -> Rc<Vec<Account>> {
    store.rows(&Q_ACCOUNTS, &[], account_row)
}

/// Creates an account (the add-account form's action). Folders arrive with the
/// first sync; the password goes to the keychain, never here.
pub fn add_account_tx(
    c: &rusqlite::Connection,
    email: &str,
    imap_host: &str,
    smtp_host: &str,
) -> rusqlite::Result<i64> {
    c.execute(
        "INSERT INTO account(label, email, imap_host, smtp_host) VALUES(?1,?1,?2,?3)",
        rusqlite::params![email, imap_host, smtp_host],
    )?;
    Ok(c.last_insert_rowid())
}

/// Removes an account and everything it brought.
pub fn remove_account_tx(c: &rusqlite::Connection, id: i64) -> rusqlite::Result<()> {
    c.execute("DELETE FROM message WHERE account=?1", [id])?;
    c.execute("DELETE FROM folder WHERE account=?1", [id])?;
    c.execute("DELETE FROM account WHERE id=?1", [id])?;
    Ok(())
}

/// The local account's address.
pub fn me(store: &Store) -> String {
    store
        .rows(&Q_ME, &[], |r| r.get::<_, String>(0))
        .first()
        .cloned()
        .unwrap_or_default()
}

/// The inbox neighbours of a mail: `(newer, older)`.
pub fn neighbours(store: &Store, id: MailId) -> (Option<MailId>, Option<MailId>) {
    let list = inbox(store);
    let Some(i) = list.iter().position(|m| m.id == id) else {
        return (None, None);
    };
    (
        i.checked_sub(1).map(|j| list[j].id),
        list.get(i + 1).map(|m| m.id),
    )
}

/// How many lines the letter reads as when wrapped at `cols` columns — how
/// *long* a mail is, which is what the message panel's height wish is made
/// of (the shell turns lines into grid rows).
///
/// The reading measured is the one the panel draws: the HTML when the
/// sender sent one, the plain text otherwise. Wrapping is counted by
/// character, so a real word wrap breaks a line or two earlier than this
/// says; the wish rounds up to whole rows and swallows the difference.
#[must_use]
pub fn reading_lines(m: &MailFull, cols: usize) -> usize {
    let text = match &m.html {
        Some(h) => crate::html::plain(h),
        None => m.body.clone(),
    };
    let cols = cols.max(1);
    text.lines()
        .map(|l| l.chars().count().div_ceil(cols).max(1))
        .sum::<usize>()
        .max(1)
}

/// The panel's display title for a kind — what headers, tab strips, the
/// overlay and the launcher all show. Data-carrying kinds resolve through
/// the store (cached like everything else).
pub fn title(store: &Store, kind: &Kind) -> String {
    match kind {
        Kind::Help => "help".into(),
        Kind::About => "about".into(),
        Kind::Inbox { filter: Some(f) } => format!("inbox · {f}"),
        Kind::Inbox { filter: None } => "inbox".into(),
        Kind::Message { id } => mail(store, *id)
            .map(|m| m.head.subject)
            .unwrap_or_else(|| "message".into()),
        Kind::Contact { email } => contact(store, email).0,
        Kind::Compose { re } => mail(store, *re)
            .map(|m| format!("re: {}", m.head.subject))
            .unwrap_or_else(|| "new mail".into()),
        Kind::Settings => "settings".into(),
        Kind::AddAccount => "add account".into(),
    }
}

// -- local mutations ---------------------------------------------------------
//
// Transaction-level pieces, composed into undoable actions by the shell
// (the session records them; undo inverts them). Phase 4 makes the server
// agree via the op queue.

/// Marks a mail read (opening it does this). A no-change update touches no
/// row — so it records nothing, and undoing the open of an already-read
/// mail correctly leaves it read. This writes **intent** only; the sync
/// worker pushes wherever intent and `server_msg` disagree.
pub fn mark_read_tx(c: &rusqlite::Connection, id: MailId) -> rusqlite::Result<()> {
    c.execute(
        "UPDATE message SET unread = 0 WHERE id = ?1 AND unread = 1",
        [id],
    )?;
    Ok(())
}

/// Moves a mail out of the inbox into one of its account's role folders.
/// Intent only — the push pass makes the server agree (see [`mark_read_tx`]),
/// and it is generic over the move, so trash rides the same path archive
/// already proved.
///
/// The `EXISTS` guard is load-bearing: an account whose server advertises no
/// such folder (see [`crate::sync`]'s role detection) would otherwise get a
/// `NULL` from the subquery, and a mail with a null folder falls out of the
/// inbox query *and* out of the push set's join — vanishing silently, with
/// nothing to sync it back. Returns whether the mail actually moved, so the
/// caller can say so.
fn file_tx(c: &rusqlite::Connection, id: MailId, role: &str) -> rusqlite::Result<bool> {
    let n = c.execute(
        "UPDATE message SET folder =
           (SELECT f.id FROM folder f
            WHERE f.account = message.account AND f.role = ?2)
         WHERE id = ?1
           AND EXISTS (SELECT 1 FROM folder f
                       WHERE f.account = message.account AND f.role = ?2)",
        rusqlite::params![id, role],
    )?;
    Ok(n > 0)
}

/// Archives a mail: it moves to its account's archive folder.
pub fn archive_tx(c: &rusqlite::Connection, id: MailId) -> rusqlite::Result<bool> {
    file_tx(c, id, "archive")
}

/// Deletes a mail: it moves to its account's trash folder. Recoverable by
/// undo like any other action, and by the server's own trash after that.
pub fn delete_tx(c: &rusqlite::Connection, id: MailId) -> rusqlite::Result<bool> {
    file_tx(c, id, "trash")
}

/// Whether this mail's account has the folder a triage would move it to.
/// The pre-flight for [`archive_tx`] / [`delete_tx`]: the shell asks first so
/// it can say *why* nothing happened rather than record an empty action.
pub fn can_file(store: &Store, id: MailId, role: &str) -> bool {
    store
        .conn()
        .query_row(
            "SELECT 1 FROM message m JOIN folder f ON f.account = m.account
             WHERE m.id = ?1 AND f.role = ?2",
            rusqlite::params![id, role],
            |_| Ok(true),
        )
        .unwrap_or(false)
}

// -- drafts and the send window ----------------------------------------------

/// A compose panel's persisted draft.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Draft {
    pub to: String,
    pub subject: String,
    pub body: String,
}

/// Loads a panel's draft, if any (boot restore, prefill).
pub fn draft(store: &Store, panel: i64) -> Option<Draft> {
    store
        .conn()
        .query_row(
            "SELECT to_addr, subject, body FROM draft WHERE panel=?1",
            [panel],
            |r| {
                Ok(Draft {
                    to: r.get(0)?,
                    subject: r.get(1)?,
                    body: r.get(2)?,
                })
            },
        )
        .ok()
}

/// Persists a compose panel's fields — plain typing upkeep, deliberately
/// **not** an action (text editing is the future editor's local undo).
/// The caller skips no-op saves; this just writes.
pub fn save_draft(store: &Store, panel: i64, re: Option<MailId>, d: &Draft, now: f64) {
    let _ = store.write(|c| {
        upsert_draft_tx(c, panel, re, d, now)?;
        Ok(())
    });
}

/// The transaction-level draft upsert (also part of the send action, so
/// the recorded changeset carries the final content).
pub fn upsert_draft_tx(
    c: &rusqlite::Connection,
    panel: i64,
    re: Option<MailId>,
    d: &Draft,
    now: f64,
) -> rusqlite::Result<()> {
    let account: Option<i64> = re
        .and_then(|id| {
            c.query_row("SELECT account FROM message WHERE id=?1", [id], |r| r.get(0))
                .ok()
        })
        .or_else(|| {
            c.query_row(
                "SELECT id FROM account
                 WHERE COALESCE(smtp_host,'') != '' ORDER BY id LIMIT 1",
                [],
                |r| r.get(0),
            )
            .ok()
        })
        .or_else(|| c.query_row("SELECT id FROM account ORDER BY id LIMIT 1", [], |r| r.get(0)).ok());
    c.execute(
        "INSERT INTO draft(panel, account, re_message, to_addr, subject, body, updated)
         VALUES(?1,?2,?3,?4,?5,?6,?7)
         ON CONFLICT(panel) DO UPDATE SET
           to_addr=excluded.to_addr, subject=excluded.subject,
           body=excluded.body, updated=excluded.updated",
        rusqlite::params![panel, account, re, d.to, d.subject, d.body, now],
    )?;
    Ok(())
}

/// Files the outbox row for a send action — the row id is the panel id, so
/// the undo entity (`outbox:{panel}`) exists before the row does.
pub fn file_send_tx(
    c: &rusqlite::Connection,
    panel: i64,
    send_after: f64,
) -> rusqlite::Result<()> {
    c.execute(
        "INSERT OR REPLACE INTO outbox(id, account, send_after, status, error)
         SELECT panel, COALESCE(account, 1), ?2, 'pending', NULL FROM draft WHERE panel=?1",
        rusqlite::params![panel, send_after],
    )?;
    Ok(())
}

/// Discard: the draft goes with the panel (both revert on undo).
pub fn discard_draft_tx(c: &rusqlite::Connection, panel: i64) -> rusqlite::Result<()> {
    c.execute("DELETE FROM draft WHERE panel=?1", [panel])?;
    Ok(())
}

/// Outbox rows that failed — surfaced as toasts and on settings.
pub fn outbox_failures(store: &Store) -> Vec<(i64, String)> {
    let Ok(mut stmt) = store
        .conn()
        .prepare("SELECT id, COALESCE(error,'send failed') FROM outbox WHERE status='failed' ORDER BY id")
    else {
        return Vec::new();
    };
    stmt.query_map([], |r| Ok((r.get(0)?, r.get(1)?)))
        .map(|it| it.filter_map(Result::ok).collect())
        .unwrap_or_default()
}

// -- the mail domain's effects ------------------------------------------------
//
// Deferred, so each one is a row in the effect table: retryable, observable,
// and cancellable by undo while it is still `pending`. Every one of them
// revalidates before the round trip — if undo landed while the job waited,
// it goes `obsolete` rather than pushing stale intent at the server. That is
// what keeps CR-001 phase 4's property: undo costs no server traffic.

/// Make the server agree about which folder a mail lives in.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Move {
    pub account: i64,
    pub message: i64,
    /// The folder row the mail now claims — `settle` records it as fact.
    pub to_folder: i64,
    pub from: String,
    pub to: String,
    pub uid: u32,
}

impl Effect for Move {
    const KIND: &'static str = "move";
    type Reply = Option<u32>;

    fn describe(&self) -> String {
        format!("move uid {} from {} to {}", self.uid, self.from, self.to)
    }

    fn perform(&self, cx: &mut Ctx<'_>) -> Result<Self::Reply, String> {
        cx.out.move_uid(self.account, &self.from, &self.to, self.uid)
    }
}

impl Deferred for Move {
    /// Moving an already-moved uid fails harmlessly, and `still_wanted`
    /// catches the common case first.
    fn idempotent(&self) -> bool {
        true
    }

    fn entity(&self) -> Option<String> {
        Some(format!("account:{}", self.account))
    }

    fn still_wanted(&self, db: &Connection) -> bool {
        db.query_row(
            "SELECT 1 FROM message m JOIN server_msg s ON s.message = m.id
             WHERE m.id = ?1 AND m.folder = ?2 AND s.folder != ?2 AND s.uid = ?3",
            rusqlite::params![self.message, self.to_folder, self.uid],
            |_| Ok(()),
        )
        .is_ok()
    }

    fn settle(&self, tx: &Transaction, reply: &Self::Reply) -> rusqlite::Result<()> {
        // No COPYUID means identity is lost until the next fetch adopts it
        // by Message-ID; the uid goes null rather than stale.
        tx.execute(
            "UPDATE server_msg SET folder = ?1, uid = ?2 WHERE message = ?3",
            rusqlite::params![self.to_folder, reply, self.message],
        )?;
        Ok(())
    }
}

/// Make the server agree about whether a mail has been read.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Seen {
    pub account: i64,
    pub message: i64,
    pub folder: String,
    pub uid: u32,
    pub seen: bool,
}

impl Effect for Seen {
    const KIND: &'static str = "seen";
    type Reply = ();

    fn describe(&self) -> String {
        format!(
            "mark uid {} in {} {}",
            self.uid,
            self.folder,
            if self.seen { "seen" } else { "unseen" }
        )
    }

    fn perform(&self, cx: &mut Ctx<'_>) -> Result<(), String> {
        cx.out
            .store_seen(self.account, &self.folder, self.uid, self.seen)
    }
}

impl Deferred for Seen {
    fn idempotent(&self) -> bool {
        true
    }

    fn entity(&self) -> Option<String> {
        Some(format!("account:{}", self.account))
    }

    fn still_wanted(&self, db: &Connection) -> bool {
        db.query_row(
            "SELECT 1 FROM message m JOIN server_msg s ON s.message = m.id
             WHERE m.id = ?1 AND m.unread = ?2 AND s.seen != ?3",
            rusqlite::params![self.message, !self.seen, self.seen],
            |_| Ok(()),
        )
        .is_ok()
    }

    fn settle(&self, tx: &Transaction, _reply: &()) -> rusqlite::Result<()> {
        tx.execute(
            "UPDATE server_msg SET seen = ?1 WHERE message = ?2",
            rusqlite::params![self.seen, self.message],
        )?;
        Ok(())
    }
}

/// Hand a mail to the SMTP server, then file it into Sent. The one
/// genuinely irreversible effect in the app.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Submit {
    /// The outbox row, which shares its compose panel's id. Referenced, not
    /// embedded: a payload that carried the body would go stale.
    pub outbox: i64,
}

impl Effect for Submit {
    const KIND: &'static str = "submit";
    /// `None` when the mail was also filed to Sent; `Some(why)` when it was
    /// sent but filing failed — best effort, exactly as before.
    type Reply = Option<String>;

    fn describe(&self) -> String {
        format!("submit outbox:{}", self.outbox)
    }

    fn perform(&self, cx: &mut Ctx<'_>) -> Result<Self::Reply, String> {
        let d = load_outgoing(cx.db, self.outbox)?;
        let pass = cx
            .out
            .secret_get(&d.email)
            .ok_or("no password in the keychain")?;
        let smtp = Creds {
            host: d.smtp,
            user: d.email.clone(),
            pass: pass.clone(),
        };
        let raw = cx.out.submit(&smtp, &d.mail)?;
        // The mail is gone; filing it is best effort and never fails a send.
        if d.imap.is_empty() {
            return Ok(Some("no imap host to file to Sent".into()));
        }
        let imap = Creds {
            host: d.imap,
            user: d.email,
            pass,
        };
        let filed = cx
            .out
            .connect(d.account, &imap)
            .and_then(|()| cx.out.append(d.account, &d.sent, &raw));
        Ok(filed.err().map(|e| format!("sent; filing to Sent failed: {e}")))
    }
}

impl Deferred for Submit {
    /// Never. A second submission is a second mail, so a crash mid-send
    /// must ask a human rather than guess.
    fn idempotent(&self) -> bool {
        false
    }

    fn entity(&self) -> Option<String> {
        Some(format!("outbox:{}", self.outbox))
    }

    fn still_wanted(&self, db: &Connection) -> bool {
        matches!(
            db.query_row(
                "SELECT status FROM outbox WHERE id = ?1",
                [self.outbox],
                |r| r.get::<_, String>(0)
            ),
            Ok(s) if s == "sending"
        )
    }

    fn settle(&self, tx: &Transaction, reply: &Self::Reply) -> rusqlite::Result<()> {
        tx.execute(
            "UPDATE outbox SET status = 'sent', error = ?2 WHERE id = ?1",
            rusqlite::params![self.outbox, reply],
        )?;
        Ok(())
    }
}

/// Everything [`Submit`] needs, read from rows at execution time.
struct Outgo {
    account: i64,
    email: String,
    smtp: String,
    imap: String,
    sent: String,
    mail: Outgoing,
}

fn load_outgoing(db: &Connection, outbox: i64) -> Result<Outgo, String> {
    db.query_row(
        "SELECT o.account, a.email, COALESCE(a.smtp_host,''), COALESCE(a.imap_host,''),
                COALESCE((SELECT name FROM folder WHERE account=a.id AND role='sent'), 'Sent'),
                d.to_addr, d.subject, d.body,
                (SELECT message_id FROM message WHERE id = d.re_message)
         FROM outbox o
         JOIN account a ON a.id = o.account
         JOIN draft d ON d.panel = o.id
         WHERE o.id = ?1",
        [outbox],
        |r| {
            Ok(Outgo {
                account: r.get(0)?,
                email: r.get(1)?,
                smtp: r.get(2)?,
                imap: r.get(3)?,
                sent: r.get(4)?,
                mail: Outgoing {
                    to: r.get(5)?,
                    subject: r.get(6)?,
                    body: r.get(7)?,
                    in_reply_to: r.get(8)?,
                },
            })
        },
    )
    .map_err(|e| format!("outbox:{outbox} is not sendable: {e}"))
    .and_then(|d| {
        if d.smtp.is_empty() {
            Err("account has no smtp host".to_string())
        } else {
            Ok(d)
        }
    })
}

// -- the mail domain's intents ------------------------------------------------
//
// What an action claimed of the world, and how to give it back. In memory,
// on a history node, never serialized — what survives a restart is the row
// each one wrote.

/// Opening a mail marks it read.
pub struct MarkRead {
    pub mail: MailId,
}

impl Intent for MarkRead {
    fn describe(&self) -> String {
        format!("mail:{} read", self.mail)
    }
    fn reverse(&self, w: &World) -> Result<(), String> {
        w.store()
            .write(|c| {
                c.execute("UPDATE message SET unread = 1 WHERE id = ?1", [self.mail])
                    .map(|_| ())
            })
            .map_err(|e| e.to_string())
    }
    fn reapply(&self, w: &World) -> Result<(), String> {
        w.store()
            .write(|c| mark_read_tx(c, self.mail))
            .map_err(|e| e.to_string())
    }
}

/// Filing moves a mail out of the folder it was in — archive or trash, the
/// same move to a different role (CR-005). Reversing is a plain intent flip
/// — the push pass re-converges, so nothing compensates.
pub struct Filed {
    pub mail: MailId,
    /// Where it was, so undo can put it back exactly there.
    pub from_folder: i64,
    /// The role it went to: `archive` or `trash`.
    pub role: &'static str,
}

impl Intent for Filed {
    fn describe(&self) -> String {
        let verb = if self.role == "trash" { "deleted" } else { "archived" };
        format!("mail:{} {verb}", self.mail)
    }
    fn reverse(&self, w: &World) -> Result<(), String> {
        w.store()
            .write(|c| {
                c.execute(
                    "UPDATE message SET folder = ?1 WHERE id = ?2",
                    rusqlite::params![self.from_folder, self.mail],
                )
                .map(|_| ())
            })
            .map_err(|e| e.to_string())
    }
    fn reapply(&self, w: &World) -> Result<(), String> {
        let role = self.role;
        w.store()
            .write(|c| file_tx(c, self.mail, role).map(|_| ()))
            .map_err(|e| e.to_string())
    }
}

/// A send, claimable back only until the sender takes it.
pub struct Sent {
    pub panel: i64,
    /// How long the window is, so redo files a fresh one.
    pub delay: f64,
}

impl Intent for Sent {
    fn describe(&self) -> String {
        format!("outbox:{} filed", self.panel)
    }

    /// The status guard is the whole race: `pending` means the executor has
    /// not taken it, and undo wins. Anything else means the mail is gone.
    fn blocked(&self, w: &World) -> Option<String> {
        match w.store().conn().query_row(
            "SELECT status FROM outbox WHERE id = ?1",
            [self.panel],
            |r| r.get::<_, String>(0),
        ) {
            Ok(s) if s == "pending" => None,
            Ok(s) if s == "failed" => None, // never left; still cancellable
            Ok(_) => Some("already sent".into()),
            Err(_) => None, // no row: nothing to give back, nothing to block
        }
    }

    fn reverse(&self, w: &World) -> Result<(), String> {
        w.store()
            .write(|c| {
                c.execute(
                    "DELETE FROM outbox WHERE id = ?1 AND status IN ('pending','failed')",
                    [self.panel],
                )
                .map(|_| ())
            })
            .map_err(|e| e.to_string())
    }

    fn reapply(&self, w: &World) -> Result<(), String> {
        let after = w.now() + self.delay;
        w.store()
            .write(|c| file_send_tx(c, self.panel, after))
            .map_err(|e| e.to_string())
    }
}

/// Discarding a compose takes its text with it.
pub struct Discarded {
    pub panel: i64,
    pub draft: Draft,
    pub re: Option<MailId>,
}

impl Intent for Discarded {
    fn describe(&self) -> String {
        format!("panel:{} draft discarded", self.panel)
    }
    fn reverse(&self, w: &World) -> Result<(), String> {
        let now = w.now();
        w.store()
            .write(|c| upsert_draft_tx(c, self.panel, self.re, &self.draft, now))
            .map_err(|e| e.to_string())
    }
    fn reapply(&self, w: &World) -> Result<(), String> {
        w.store()
            .write(|c| discard_draft_tx(c, self.panel))
            .map_err(|e| e.to_string())
    }
}

/// Adding an account. Reversible while it is still empty, which it is at
/// the moment it is added.
pub struct AccountAdded {
    pub id: i64,
    pub email: String,
    pub imap: String,
    pub smtp: String,
}

impl Intent for AccountAdded {
    fn describe(&self) -> String {
        format!("account:{} added", self.id)
    }
    fn reverse(&self, w: &World) -> Result<(), String> {
        w.store()
            .write(|c| remove_account_tx(c, self.id))
            .map_err(|e| e.to_string())
    }
    fn reapply(&self, w: &World) -> Result<(), String> {
        w.store()
            .write(|c| {
                c.execute(
                    "INSERT INTO account(id, label, email, imap_host, smtp_host)
                     VALUES(?1, ?2, ?2, ?3, ?4)",
                    rusqlite::params![self.id, self.email, self.imap, self.smtp],
                )
                .map(|_| ())
            })
            .map_err(|e| e.to_string())
    }
}

/// Removing an account takes its mail with it, and no snapshot brings that
/// back. Stated honestly rather than half-restored: the node goes expired
/// and the walk steps past it.
pub struct AccountRemoved {
    pub email: String,
}

impl Intent for AccountRemoved {
    fn describe(&self) -> String {
        format!("account {} removed", self.email)
    }
    fn blocked(&self, _w: &World) -> Option<String> {
        Some("an account's mail cannot be restored".into())
    }
    fn reverse(&self, _w: &World) -> Result<(), String> {
        Ok(())
    }
    fn reapply(&self, _w: &World) -> Result<(), String> {
        Ok(())
    }
}

// -- the in-memory half -------------------------------------------------------
//
// Reads and session handling: nobody retries them, nobody waits on a row for
// them, and their answers are values the caller needs now. So they are
// `Effect` but not `Deferred` — performed at the call, written nowhere.

/// Open this account's mail session. Not `Deferred`, and therefore not
/// serializable — which is the point: it carries a password.
#[derive(Debug, Clone)]
pub struct Connect {
    pub account: i64,
    pub creds: Creds,
}

impl Effect for Connect {
    const KIND: &'static str = "connect";
    type Reply = ();
    fn describe(&self) -> String {
        format!("connect to {} as {}", self.creds.host, self.creds.user)
    }
    fn perform(&self, cx: &mut Ctx<'_>) -> Result<(), String> {
        cx.out.connect(self.account, &self.creds)
    }
}

/// List the account's folders.
#[derive(Debug, Clone)]
pub struct Folders {
    pub account: i64,
}

impl Effect for Folders {
    const KIND: &'static str = "folders";
    type Reply = Vec<crate::effect::RemoteFolder>;
    fn describe(&self) -> String {
        "list folders".into()
    }
    fn perform(&self, cx: &mut Ctx<'_>) -> Result<Self::Reply, String> {
        cx.out.folders(self.account)
    }
}

/// SELECT a folder for its uidvalidity/uidnext.
#[derive(Debug, Clone)]
pub struct Meta {
    pub account: i64,
    pub folder: String,
}

impl Effect for Meta {
    const KIND: &'static str = "meta";
    type Reply = crate::effect::FolderMeta;
    fn describe(&self) -> String {
        format!("select {}", self.folder)
    }
    fn perform(&self, cx: &mut Ctx<'_>) -> Result<Self::Reply, String> {
        cx.out.folder_meta(self.account, &self.folder)
    }
}

/// Fetch everything at or above a uid.
#[derive(Debug, Clone)]
pub struct Fetch {
    pub account: i64,
    pub folder: String,
    pub from: u32,
}

impl Effect for Fetch {
    const KIND: &'static str = "fetch";
    type Reply = Vec<crate::effect::RemoteMail>;
    fn describe(&self) -> String {
        format!("fetch {} from uid {}", self.folder, self.from)
    }
    fn perform(&self, cx: &mut Ctx<'_>) -> Result<Self::Reply, String> {
        cx.out.fetch(self.account, &self.folder, self.from)
    }
}

/// Search a folder's uids — all of them, or only the unseen.
#[derive(Debug, Clone)]
pub struct Uids {
    pub account: i64,
    pub folder: String,
    pub unread_only: bool,
}

impl Effect for Uids {
    const KIND: &'static str = "uids";
    type Reply = std::collections::HashSet<u32>;
    fn describe(&self) -> String {
        format!(
            "search {} in {}",
            if self.unread_only { "unseen" } else { "all" },
            self.folder
        )
    }
    fn perform(&self, cx: &mut Ctx<'_>) -> Result<Self::Reply, String> {
        cx.out.uids(self.account, &self.folder, self.unread_only)
    }
}

/// The mail domain's deferred effects. Each domain registers its own, so
/// adding one touches no central list.
pub fn register(reg: &mut Registry) {
    reg.register::<Move>();
    reg.register::<Seen>();
    reg.register::<Submit>();
}

/// A registry with every domain's effects — what a real world and a test
/// world both start from.
#[must_use]
pub fn registry() -> Registry {
    let mut reg = Registry::new();
    register(&mut reg);
    reg
}

// -- dates -------------------------------------------------------------------

const MONTHS: [&str; 12] = [
    "jan", "feb", "mar", "apr", "may", "jun", "jul", "aug", "sep", "oct", "nov", "dec",
];

/// Days from civil date (Howard Hinnant's algorithm), epoch 1970-01-01.
fn days_from_civil(y: i64, m: u32, d: u32) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = (y - era * 400) as u64;
    let mp = if m > 2 { m - 3 } else { m + 9 } as u64;
    let doy = (153 * mp + 2) / 5 + u64::from(d) - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146097 + doe as i64 - 719468
}

/// Civil date from days since the epoch (the inverse of the above).
fn civil_from_days(z: i64) -> (i64, u32, u32) {
    let z = z + 719468;
    let era = if z >= 0 { z } else { z - 146096 } / 146097;
    let doe = (z - era * 146097) as u64;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let y = yoe as i64 + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    (if m <= 2 { y + 1 } else { y }, m, d)
}

/// A timestamp from a civil date-time (naïve — the store's demo timezone).
#[must_use]
pub fn ts(y: i64, mo: u32, d: u32, h: u32, min: u32) -> f64 {
    (days_from_civil(y, mo, d) * 86_400 + i64::from(h) * 3_600 + i64::from(min) * 60) as f64
}

/// The list/date style the panels always used: `aug 31 09:14`.
#[must_use]
pub fn fmt_date(ts: f64) -> String {
    let secs = ts as i64;
    let (days, rem) = (secs.div_euclid(86_400), secs.rem_euclid(86_400));
    let (_, m, d) = civil_from_days(days);
    let (h, min) = (rem / 3_600, (rem % 3_600) / 60);
    format!("{} {d:02} {h:02}:{min:02}", MONTHS[(m - 1) as usize])
}

// -- the demo seed -----------------------------------------------------------

struct SeedMail {
    from_name: &'static str,
    from_email: &'static str,
    subject: &'static str,
    date: f64,
    unread: bool,
    body: &'static str,
    /// The HTML reading, when the demo sender sent one. Stored raw here
    /// and narrowed on the way in, exactly as a synced mail would be — the
    /// seed exercises the real path rather than a tidied version of it.
    html: Option<&'static str>,
    status: Option<(&'static str, bool)>,
}

/// The hand-written demo mail, newest first — ids land as 1..=8 in a fresh
/// store, which the tests and e2e suites rely on.
fn base_mails() -> Vec<SeedMail> {
    vec![
        SeedMail {
            from_name: "Vera Kovac",
            from_email: "vera@kovac.io",
            subject: "Q3 infra budget draft",
            date: ts(2026, 8, 31, 9, 14),
            unread: true,
            body: "Draft for Q3 infra spend is ready. Main deltas: the old staging cluster goes away and CI runners move to the new box.\n\nCan you sanity-check the numbers before Thursday? Especially egress — I suspect the CDN line is stale.",
            html: None,
            status: None,
        },
        SeedMail {
            from_name: "GitHub",
            from_email: "notifications@github.com",
            subject: "[stelaxis] CI failed on main",
            date: ts(2026, 8, 31, 8, 2),
            unread: true,
            body: "Workflow main #4128 failed on push 9f3c2a1.\n\nFailed steps: mix test (2 failures), credo --strict (1 warning). Full logs are attached to the run.",
            // The one demo sender that writes HTML — and it writes it the
            // way real senders do: a stylesheet, tables holding the page
            // together, a pixel counting the open, a `javascript:` link.
            // What survives the narrowing is the letter.
            html: Some(
                "<html><head><style>.hd{background:#24292f;color:#fff}</style></head>\
                 <body><table width=\"100%\"><tr><td class=\"hd\">\
                 <div><b>Workflow failed</b> in \
                 <a href=\"https://github.com/x/stelaxis\">stelaxis</a></div>\
                 </td></tr><tr><td>\
                 <p>Run <b>main #4128</b> failed on push <code>9f3c2a1</code>.</p>\
                 <p>Failed steps: &#55357;&#56960;</p>\
                 <ul><li>mix test &mdash; <b>2 failures</b></li>\
                 <li>credo --strict &mdash; <i>1 warning</i></li></ul>\
                 <p><i>This run was triggered by a push to </i><b><i>main</i></b>.</p>\
                 <p><a href=\"https://github.com/x/stelaxis/actions/runs/4128\">View the run</a> \
                 or <a href=\"javascript:unsub()\">unsubscribe</a>.</p>\
                 </td></tr></table>\
                 <img src=\"https://github.com/pixel.gif\" width=\"1\" height=\"1\">\
                 </body></html>",
            ),
            status: Some(("ci: FAILED — build (2m 14s), tests (41s)", true)),
        },
        SeedMail {
            from_name: "Max Ivanov",
            from_email: "max@ivanov.dev",
            subject: "Re: superapp panel model",
            date: ts(2026, 8, 30, 22, 47),
            unread: false,
            body: "Read your note on panels. The joined/replace rule feels like the right default — it is the preview-pane pattern, but generalized to everything.\n\nOne question though: what happens to a half-written draft if a joined compose panel gets replaced by the next link? Feels like some panels need a way to resist replacement.",
            html: None,
            status: None,
        },
        SeedMail {
            from_name: "Elena Petrova",
            from_email: "elena.p@gmail.com",
            subject: "Sat hike — early start?",
            date: ts(2026, 8, 30, 18, 20),
            unread: false,
            body: "Weather looks fine for Saturday. Early start (7:30) or lazy start (10:00)?\n\nThere is a new trail variant, ~14 km, one café stop. Bring the good thermos.",
            html: None,
            status: None,
        },
        SeedMail {
            from_name: "RSS Digest",
            from_email: "digest@rss.local",
            subject: "weekly: 14 unread items in 3 feeds",
            date: ts(2026, 8, 30, 7, 0),
            unread: false,
            body: "Unread this week: niri release notes (2), simonwillison.net (9), lobste.rs top (3).\n\nThis digest is itself a candidate for an rss/feed panel, by the way.",
            html: None,
            status: None,
        },
        SeedMail {
            from_name: "Calendar",
            from_email: "calendar@local",
            subject: "invite: dentist — tue 10:00",
            date: ts(2026, 8, 29, 16, 41),
            unread: false,
            body: "Dentist, Tuesday 10:00–10:45. Reminder set for 30 minutes before.\n\nReply yes to confirm, or propose a new time.",
            html: None,
            status: None,
        },
        SeedMail {
            from_name: "Hetzner",
            from_email: "billing@hetzner.com",
            subject: "invoice 2026-08 — €46.20",
            date: ts(2026, 8, 29, 11, 5),
            unread: false,
            body: "Invoice 2026-08 for €46.20 is available. Auto-charge on Sep 3.\n\nUsage: 2× CX32, 1× volume 100 GB, egress 214 GB.",
            html: None,
            status: None,
        },
        SeedMail {
            from_name: "Dmitry Orlov",
            from_email: "dorlov@fastmail.com",
            subject: "that airport book",
            date: ts(2026, 8, 28, 20, 33),
            unread: false,
            body: "Found it — the airport design book you mentioned at dinner. Ordering a copy tomorrow.\n\nBorrowing rights claimed for after you finish, obviously.",
            html: None,
            status: None,
        },
        // The one long letter in the demo world: it does not fit a message
        // panel's three rows, so the panel asks for more and opens tall.
        SeedMail {
            from_name: "Max Ivanov",
            from_email: "max@ivanov.dev",
            subject: "long version: what panels owe their content",
            date: ts(2026, 8, 28, 9, 12),
            unread: false,
            body: "You asked for the long version, so here it is — the argument I could not fit into two lines yesterday.\n\n\
                   What bothers me about every mail client I have used is that the reading pane is a fixed hole in the layout. A two-line \"ok, see you Thursday\" and a four-page release note are poured into the same box: one leaves most of it empty, the other is cut off a third of the way down. The box was sized for neither.\n\n\
                   Your panels already know better. A panel asks for grid units — a request, rather than a rectangle handed down. But the request is a constant per kind, which makes it a guess about the average letter, and the average letter does not exist.\n\n\
                   So let the kind's request be a floor rather than a promise. A short mail keeps its three rows — no reason to make a one-liner tall. A long one asks for as many rows as it needs, up to the whole column, and the grid clamps it there like it clamps everything else. Nothing new in the layout, just a better number going in.\n\n\
                   The nice consequence is that the layout tells you something before you read a word: a column where one panel is visibly taller is a column where one letter is long. That is real information, and you got it for free.\n\n\
                   Anyway — this letter is its own test case. If it opens in three rows you have proven my point; if it opens tall, yours.",
            html: None,
            status: None,
        },
    ]
}

/// Seeds the demo account and mail into an empty store — the same content
/// the static module used to hold, so panels and e2e suites behave
/// identically. A store with any mail is left alone.
pub fn seed_if_empty(store: &Store) -> rusqlite::Result<()> {
    let n: i64 = store
        .conn()
        .query_row("SELECT COUNT(*) FROM message", [], |r| r.get(0))?;
    if n > 0 {
        return Ok(());
    }
    store.write(|c| {
        c.execute(
            "INSERT INTO account(label, email) VALUES('demo', 'me@prepor.dev')",
            [],
        )?;
        let acct = c.last_insert_rowid();
        let folder = |name: &str, role: &str| -> rusqlite::Result<i64> {
            c.execute(
                "INSERT INTO folder(account, name, role) VALUES(?1, ?2, ?3)",
                rusqlite::params![acct, name, role],
            )?;
            Ok(c.last_insert_rowid())
        };
        let inbox = folder("Inbox", "inbox")?;
        folder("Archive", "archive")?;
        folder("Sent", "sent")?;
        folder("Trash", "trash")?;

        let insert = |m: &SeedMail| -> rusqlite::Result<()> {
            c.execute(
                "INSERT INTO message(account, folder, from_name, from_email,
                                     subject, date, unread, body, html,
                                     status, status_err)
                 VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11)",
                rusqlite::params![
                    acct,
                    inbox,
                    m.from_name,
                    m.from_email,
                    m.subject,
                    m.date,
                    m.unread,
                    m.body,
                    m.html.map(crate::html::sanitize),
                    m.status.map(|(s, _)| s),
                    m.status.map(|(_, e)| e).unwrap_or(false),
                ],
            )?;
            Ok(())
        };
        for m in &base_mails() {
            insert(m)?;
        }
        // The generated archive tail: the inbox genuinely overflows, so
        // in-panel scrolling has something to do.
        let senders: [(&str, &str); 4] = [
            ("RSS Digest", "digest@rss.local"),
            ("GitHub", "notifications@github.com"),
            ("Hetzner", "billing@hetzner.com"),
            ("Calendar", "calendar@local"),
        ];
        for i in 0..60u32 {
            let (name, email) = senders[(i as usize) % senders.len()];
            let n = 60 - i;
            c.execute(
                "INSERT INTO message(account, folder, from_name, from_email,
                                     subject, date, unread, body)
                 VALUES(?1,?2,?3,?4,?5,?6,0,?7)",
                rusqlite::params![
                    acct,
                    inbox,
                    name,
                    email,
                    format!("archive digest #{n:02}"),
                    ts(2026, 8, 27 - i / 6, 8 + (i % 12), (i * 7) % 60),
                    format!(
                        "Archive item #{n:02} from {name} — generated filler so the inbox overflows and in-panel scrolling is honest.\n\nNothing to see here beyond the scrollbar."
                    ),
                ],
            )?;
        }
        Ok(())
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn store() -> Store {
        let s = Store::open(None).expect("in-memory store");
        seed_if_empty(&s).expect("seed");
        s
    }

    /// The seed reproduces the demo world: 69 mails, m1/m2 unread, ids in
    /// insert order (m1 = 1), newest first.
    #[test]
    fn seed_reproduces_the_demo_world() {
        let s = store();
        let rows = inbox(&s);
        assert_eq!(rows.len(), 69);
        assert_eq!(rows[0].id, 1);
        assert_eq!(rows[0].subject, "Q3 infra budget draft");
        assert!(rows[0].unread && rows[1].unread && !rows[2].unread);
        assert_eq!(fmt_date(rows[0].date), "aug 31 09:14");
        assert_eq!(me(&s), "me@prepor.dev");
        // Seeding an already-seeded store is a no-op.
        seed_if_empty(&s).unwrap();
        assert_eq!(inbox(&s).len(), 69);
    }

    /// The demo world carries one HTML sender, narrowed on the way in: the
    /// letter survives, the stylesheet, the layout table, the tracking
    /// pixel and the `javascript:` link do not.
    #[test]
    fn the_seeded_html_mail_is_narrowed() {
        let s = store();
        let h = mail(&s, 2).expect("the github mail").html.expect("html");
        assert!(h.contains("<ul><li>mix test"), "the list survives: {h}");
        assert!(h.contains(r#"<a href="https://github.com/x/stelaxis">"#));
        assert!(h.contains("&mdash;"), "entities are makepad's to decode");
        // …except the ones it decodes by unwrapping `char::from_u32`. The
        // seed carries an emoji spelled as its UTF-16 surrogate pair, the way
        // real composers send them, so every run that draws this mail is a
        // check that the pair was put back together before the widget saw it.
        assert!(h.contains('🚀'), "the surrogate pair is repaired: {h}");
        assert!(!h.contains("&#55357;"), "no bare surrogate reaches the widget");
        assert!(!h.contains("background:#24292f"), "the stylesheet is gone");
        assert!(!h.contains("<table") && !h.contains("<td"), "layout is gone");
        assert!(!h.contains("pixel.gif"), "the tracking pixel is gone");
        assert!(!h.contains("javascript:"), "the script link is defused");
        assert!(h.contains("unsubscribe"), "but its text is kept");
        // Its plain reading is still there for quoting a reply.
        assert!(mail(&s, 2).expect("mail").body.starts_with("Workflow main"));
        // And a text-only sender stays on the plain path.
        assert_eq!(mail(&s, 1).expect("vera").html, None);
    }

    /// Filter semantics are the shell's: one substring, sender + subject.
    #[test]
    fn filter_and_neighbours_match_the_old_semantics() {
        let s = store();
        let hits = inbox_filtered(&s, "vera");
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].id, 1);
        assert!(inbox_filtered(&s, "GITHUB").len() > 10, "case-insensitive");
        let (newer, older) = neighbours(&s, 2);
        assert_eq!(newer, Some(1));
        assert_eq!(older, Some(3));
        assert_eq!(neighbours(&s, 1).0, None);
    }

    /// Archive moves a mail out of the inbox (and out of neighbours), the
    /// read flag clears once, titles resolve through the store.
    #[test]
    fn mutations_and_titles() {
        let s = store();
        assert_eq!(title(&s, &Kind::Message { id: 1 }), "Q3 infra budget draft");
        assert_eq!(
            title(&s, &Kind::Contact { email: "vera@kovac.io".into() }),
            "Vera Kovac"
        );
        assert_eq!(title(&s, &Kind::Compose { re: 1 }), "re: Q3 infra budget draft");
        assert_eq!(title(&s, &Kind::Inbox { filter: Some("x".into()) }), "inbox · x");

        s.write(|c| mark_read_tx(c, 1)).unwrap();
        assert!(!inbox(&s)[0].unread);
        assert!(s.write(|c| archive_tx(c, 1)).unwrap(), "archive moved it");
        assert_eq!(inbox(&s).len(), 68);
        assert_ne!(inbox(&s)[0].id, 1);
        assert_eq!(all(&s).len(), 69, "archived mail stays in the corpus");
        let (name, n) = contact(&s, "vera@kovac.io");
        assert_eq!((name.as_str(), n), ("Vera Kovac", 1));
    }

    /// Delete is archive's twin on the trash folder, and both refuse to move
    /// a mail whose account has no such folder rather than stranding it with
    /// a null one (which would drop it from the inbox *and* from the push).
    #[test]
    fn delete_files_to_trash_and_needs_the_folder() {
        let s = store();
        assert!(can_file(&s, 2, "trash"));
        assert!(s.write(|c| delete_tx(c, 2)).unwrap(), "delete moved it");
        assert_eq!(inbox(&s).len(), 68);
        assert!(!inbox(&s).iter().any(|m| m.id == 2));
        assert_eq!(all(&s).len(), 69, "deleted mail stays in the corpus");

        // An account without the folder: the mail must stay exactly where it
        // is. A fresh store, because the folder can only be dropped while
        // nothing references it.
        let s = store();
        s.write(|c| c.execute("DELETE FROM folder WHERE role = 'trash'", []))
            .unwrap();
        assert!(!can_file(&s, 3, "trash"));
        assert!(!s.write(|c| delete_tx(c, 3)).unwrap(), "nothing to move to");
        assert!(inbox(&s).iter().any(|m| m.id == 3), "still in the inbox");
    }

    /// How long a letter is, counted in wrapped lines — the measure behind
    /// a message panel's height wish. Paragraph breaks count as the blank
    /// line they draw, and the reading measured is the one the panel shows.
    #[test]
    fn a_letters_length_is_counted_in_wrapped_lines() {
        let s = store();
        let vera = mail(&s, 1).expect("vera");
        // Two paragraphs, wide enough not to wrap: two lines and the blank
        // one between them.
        assert_eq!(reading_lines(&vera, 1000), 3);
        // The narrower the column, the more lines the same letter takes.
        assert!(reading_lines(&vera, 40) > reading_lines(&vera, 80));
        // The HTML sender is measured on its HTML — the reading the panel
        // draws — so its list is lines rather than one long blob.
        let gh = mail(&s, 2).expect("github");
        assert!(reading_lines(&gh, 1000) >= 5, "{}", reading_lines(&gh, 1000));
        // And the demo world's one long letter dwarfs them both.
        let long = all(&s)
            .iter()
            .find(|m| m.subject.starts_with("long version"))
            .and_then(|m| mail(&s, m.id))
            .expect("the long letter");
        assert!(reading_lines(&long, 60) > 4 * reading_lines(&vera, 60));
    }

    /// A trace records exactly what was read between begin and end — the
    /// provenance the panel context serializes.
    #[test]
    fn traces_record_panel_provenance() {
        let s = store();
        s.trace_begin(7);
        let _ = inbox(&s);
        let _ = mail(&s, 1);
        let _ = mail(&s, 1); // repeated reads dedupe
        s.trace_end();
        let _ = senders(&s); // outside the trace: not recorded
        let t = s.trace_of(7);
        assert_eq!(
            t.iter().map(|e| e.id).collect::<Vec<_>>(),
            vec!["inbox", "mail"]
        );
        assert_eq!(t[0].rows, 69);
        assert_eq!(t[1].params, "1");
        assert!(t[0].describe.contains("inbox"));
    }

    /// The civil-date maths round-trips.
    #[test]
    fn dates_round_trip() {
        assert_eq!(fmt_date(ts(2026, 8, 31, 9, 14)), "aug 31 09:14");
        assert_eq!(fmt_date(ts(2026, 1, 1, 0, 0)), "jan 01 00:00");
        assert_eq!(fmt_date(ts(2025, 12, 31, 23, 59)), "dec 31 23:59");
    }
}
