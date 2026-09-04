//! What mail does outside the process, and what it claimed of the world.
//!
//! The deferred half — [`Move`], [`Seen`], [`Submit`] — is a row in the
//! effect table each: retryable, observable, and cancellable by undo while it
//! is still `pending`. Every one revalidates before the round trip, so undo
//! costs no server traffic. The in-memory half is the reads a sync pass makes;
//! nobody retries them and nobody waits on a row for them, so they are
//! [`Effect`] but not [`Deferred`] — and they still go through the one door,
//! which is what puts them in the log beside the queue.

use std::collections::HashSet;

use kernel::caps::Secrets;
use kernel::effect::{Ctx, Deferred, Effect, Registry, World};
use kernel::history::Intent;
use rusqlite::{Connection, Transaction};
use serde::{Deserialize, Serialize};

use super::caps::{Creds, FolderMeta, Imap, MailFlag, Outgoing, RemoteFolder, RemoteMail, Smtp, UidSet};
use super::carry;
use super::model::{self, Draft, MailId, Seed};

/// The mail app's deferred effects. Each app registers its own, so adding one
/// touches no central list.
pub fn register(reg: &mut Registry) {
    reg.register::<Move>();
    reg.register::<Seen>();
    reg.register::<Submit>();
}

// -- the deferred half -----------------------------------------------------------

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

    fn writes(&self) -> bool {
        true
    }

    fn entity(&self) -> Option<String> {
        Some(account_entity(self.account))
    }

    fn perform(&self, cx: &mut Ctx<'_>) -> Result<Self::Reply, String> {
        cx.cap::<dyn Imap>()?
            .move_uid(self.account, &self.from, &self.to, self.uid)
    }
}

impl Deferred for Move {
    /// Moving an already-moved uid fails harmlessly, and `still_wanted`
    /// catches the common case first.
    fn idempotent(&self) -> bool {
        true
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
        // No COPYUID means identity is lost until the next fetch adopts it by
        // Message-ID; the uid goes null rather than stale.
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

    fn writes(&self) -> bool {
        true
    }

    fn entity(&self) -> Option<String> {
        Some(account_entity(self.account))
    }

    fn perform(&self, cx: &mut Ctx<'_>) -> Result<(), String> {
        cx.cap::<dyn Imap>()?.store_flag(
            self.account,
            &self.folder,
            self.uid,
            MailFlag::Seen,
            self.seen,
        )
    }
}

impl Deferred for Seen {
    fn idempotent(&self) -> bool {
        true
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

/// Hand a mail to the submission server, then file it into Sent. The one
/// genuinely irreversible effect in the app.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Submit {
    /// The outbox row, which shares its compose slot's id. Referenced, not
    /// embedded: a payload that carried the body would go stale.
    pub outbox: i64,
}

impl Effect for Submit {
    const KIND: &'static str = "submit";
    /// `None` when the mail was also filed to Sent; `Some(why)` when it was
    /// sent but filing failed — best effort.
    type Reply = Option<String>;

    fn describe(&self) -> String {
        format!("submit outbox:{}", self.outbox)
    }

    fn writes(&self) -> bool {
        true
    }

    fn entity(&self) -> Option<String> {
        Some(outbox_entity(self.outbox))
    }

    fn perform(&self, cx: &mut Ctx<'_>) -> Result<Self::Reply, String> {
        let d = load_outgoing(cx.db, self.outbox)?;
        let smtp = {
            let secrets = cx.cap::<dyn Secrets>()?;
            model::creds_for(secrets, &d.email, &d.smtp)?
        };
        let raw = cx.cap::<dyn Smtp>()?.submit(&smtp, &d.mail)?;
        // The mail is gone; filing it is best effort and never fails a send.
        if d.imap.is_empty() {
            return Ok(Some("no imap host to file to Sent".into()));
        }
        // The same secret reaches both servers, so it is read once.
        let imap = Creds::password(&d.imap, &d.email, smtp.secret());
        let filed = {
            let server = cx.cap::<dyn Imap>()?;
            server
                .connect(d.account, &imap)
                .and_then(|()| server.append(d.account, &d.sent, &raw))
        };
        Ok(filed.err().map(|e| format!("sent; filing to Sent failed: {e}")))
    }
}

impl Deferred for Submit {
    /// Never. A second submission is a second mail, so a crash mid-send must
    /// ask a human rather than guess.
    fn idempotent(&self) -> bool {
        false
    }

    fn still_wanted(&self, db: &Connection) -> bool {
        matches!(
            db.query_row("SELECT status FROM outbox WHERE id = ?1", [self.outbox], |r| {
                r.get::<_, String>(0)
            }),
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
    /// The name of the folder a sent copy is filed to.
    sent: String,
    mail: Outgoing,
}

fn load_outgoing(db: &Connection, outbox: i64) -> Result<Outgo, String> {
    db.query_row(
        "SELECT o.account, a.email, COALESCE(a.smtp_host,''), COALESCE(a.imap_host,''),
                COALESCE((SELECT name FROM folder WHERE account = a.id AND role = 'sent'), 'Sent'),
                d.to_addr, d.subject, d.body,
                (SELECT message_id FROM message WHERE id = d.re_message),
                (SELECT message_id FROM message
                  WHERE id = COALESCE(d.re_message, d.fwd_message)),
                (SELECT GROUP_CONCAT(mid, ' ') FROM reference
                  WHERE message = COALESCE(d.re_message, d.fwd_message))
         FROM outbox o
         JOIN account a ON a.id = o.account
         JOIN draft d ON d.panel = o.id
         WHERE o.id = ?1",
        [outbox],
        |r| {
            // The chain: what the source itself referenced, then the source —
            // so a reply to a reply, or a forward of one, threads for whoever
            // already has the conversation (RFC 5322).
            let source: Option<String> = r.get::<_, Option<String>>(9)?.filter(|s| !s.is_empty());
            let mut references: Vec<String> = r
                .get::<_, Option<String>>(10)?
                .unwrap_or_default()
                .split_whitespace()
                .map(str::to_string)
                .collect();
            if let Some(mid) = source {
                if !references.contains(&mid) {
                    references.push(mid);
                }
            }
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
                    in_reply_to: r.get::<_, Option<String>>(8)?.filter(|s| !s.is_empty()),
                    references,
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

// -- the in-memory half ------------------------------------------------------------

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
    /// A session is what makes the rest possible; nothing out there is
    /// different for having opened one.
    fn writes(&self) -> bool {
        false
    }
    fn entity(&self) -> Option<String> {
        Some(account_entity(self.account))
    }
    fn perform(&self, cx: &mut Ctx<'_>) -> Result<(), String> {
        cx.cap::<dyn Imap>()?.connect(self.account, &self.creds)
    }
}

/// List the account's folders.
#[derive(Debug, Clone)]
pub struct Folders {
    pub account: i64,
}

impl Effect for Folders {
    const KIND: &'static str = "folders";
    type Reply = Vec<RemoteFolder>;
    fn describe(&self) -> String {
        "list folders".into()
    }
    fn writes(&self) -> bool {
        false
    }
    fn entity(&self) -> Option<String> {
        Some(account_entity(self.account))
    }
    fn perform(&self, cx: &mut Ctx<'_>) -> Result<Self::Reply, String> {
        cx.cap::<dyn Imap>()?.folders(self.account)
    }
}

/// SELECT a folder for its uidvalidity and uidnext.
#[derive(Debug, Clone)]
pub struct Meta {
    pub account: i64,
    pub folder: String,
}

impl Effect for Meta {
    const KIND: &'static str = "meta";
    type Reply = FolderMeta;
    fn describe(&self) -> String {
        format!("select {}", self.folder)
    }
    fn writes(&self) -> bool {
        false
    }
    fn entity(&self) -> Option<String> {
        Some(account_entity(self.account))
    }
    fn perform(&self, cx: &mut Ctx<'_>) -> Result<Self::Reply, String> {
        cx.cap::<dyn Imap>()?.folder_meta(self.account, &self.folder)
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
    type Reply = Vec<RemoteMail>;
    fn describe(&self) -> String {
        format!("fetch {} from uid {}", self.folder, self.from)
    }
    fn writes(&self) -> bool {
        false
    }
    fn entity(&self) -> Option<String> {
        Some(account_entity(self.account))
    }
    fn perform(&self, cx: &mut Ctx<'_>) -> Result<Self::Reply, String> {
        cx.cap::<dyn Imap>()?
            .fetch(self.account, &self.folder, self.from)
    }
}

/// Search a folder's uids — all of them, or the unseen.
#[derive(Debug, Clone)]
pub struct Uids {
    pub account: i64,
    pub folder: String,
    pub which: UidSet,
}

impl Effect for Uids {
    const KIND: &'static str = "uids";
    type Reply = HashSet<u32>;
    fn describe(&self) -> String {
        let which = match self.which {
            UidSet::All => "all",
            UidSet::Unseen => "unseen",
        };
        format!("search {which} in {}", self.folder)
    }
    fn writes(&self) -> bool {
        false
    }
    fn entity(&self) -> Option<String> {
        Some(account_entity(self.account))
    }
    fn perform(&self, cx: &mut Ctx<'_>) -> Result<Self::Reply, String> {
        cx.cap::<dyn Imap>()?
            .uids(self.account, &self.folder, self.which)
    }
}

/// One account, in the `action.entity` vocabulary: a job's row, a worker's
/// kick address, and an action's coalescing scope all spell it this way.
#[must_use]
pub fn account_entity(account: i64) -> String {
    format!("account:{account}")
}

/// One filed send, in the same vocabulary.
#[must_use]
pub fn outbox_entity(outbox: i64) -> String {
    format!("outbox:{outbox}")
}

// -- what an action claimed of the world --------------------------------------------
//
// In memory, on a history node, never serialized — what survives a restart is
// the row each one wrote.

/// Opening a mail marks it read.
pub struct MarkRead {
    pub mail: MailId,
}

impl Intent for MarkRead {
    fn describe(&self) -> String {
        format!("mail:{} read", self.mail)
    }
    fn reverse(&self, w: &World) -> Result<(), String> {
        let mail = self.mail;
        w.store()
            .write(move |c| {
                c.execute("UPDATE message SET unread = 1 WHERE id = ?1", [mail])
                    .map(|_| ())
            })
            .map_err(|e| e.to_string())
    }
    fn reapply(&self, w: &World) -> Result<(), String> {
        let mail = self.mail;
        w.store()
            .write(move |c| model::mark_read_tx(c, mail))
            .map_err(|e| e.to_string())
    }
}

/// Filing moves a mail out of the folder it was in — archive or trash, the
/// same move to a different role. Reversing is a plain intent flip: the push
/// pass re-converges, so nothing compensates.
pub struct Filed {
    pub mail: MailId,
    /// Where it was, so undo can put it back exactly there.
    pub from_folder: i64,
    /// The role it went to: `archive` or `trash`.
    pub role: &'static str,
}

impl Intent for Filed {
    fn describe(&self) -> String {
        let verb = if self.role == "trash" {
            "deleted"
        } else {
            "archived"
        };
        format!("mail:{} {verb}", self.mail)
    }
    fn reverse(&self, w: &World) -> Result<(), String> {
        let (mail, from_folder) = (self.mail, self.from_folder);
        w.store()
            .write(move |c| {
                c.execute(
                    "UPDATE message SET folder = ?1 WHERE id = ?2",
                    rusqlite::params![from_folder, mail],
                )
                .map(|_| ())
            })
            .map_err(|e| e.to_string())
    }
    fn reapply(&self, w: &World) -> Result<(), String> {
        let (mail, role) = (self.mail, self.role);
        w.store()
            .write(move |c| model::file_tx(c, mail, role).map(|_| ()))
            .map_err(|e| e.to_string())
    }
}

/// A send, claimable back only until the sender takes it.
pub struct Sent {
    /// The compose slot, which is also the outbox row's id.
    pub slot: i64,
    /// How long the window is, so redo files a fresh one.
    pub delay: f64,
}

impl Intent for Sent {
    fn describe(&self) -> String {
        format!("outbox:{} filed", self.slot)
    }

    /// The status guard is the whole race: `pending` means the executor has
    /// not taken it, and undo wins. Anything else means the mail is gone.
    fn blocked(&self, w: &World) -> Option<String> {
        match w.store().conn().query_row(
            "SELECT status FROM outbox WHERE id = ?1",
            [self.slot],
            |r| r.get::<_, String>(0),
        ) {
            Ok(s) if s == "pending" => None,
            Ok(s) if s == "failed" => None, // never left; still cancellable
            Ok(_) => Some("already sent".into()),
            Err(_) => None, // no row: nothing to give back, nothing to block
        }
    }

    fn reverse(&self, w: &World) -> Result<(), String> {
        let slot = self.slot;
        w.store()
            .write(move |c| {
                c.execute(
                    "DELETE FROM outbox WHERE id = ?1 AND status IN ('pending','failed')",
                    [slot],
                )
                .map(|_| ())
            })
            .map_err(|e| e.to_string())
    }

    fn reapply(&self, w: &World) -> Result<(), String> {
        let (slot, after) = (self.slot, w.now() + self.delay);
        w.store()
            .write(move |c| model::file_send_tx(c, slot, after))
            .map_err(|e| e.to_string())
    }
}

/// A draft was given files to carry.
pub struct Attached {
    /// The compose slot, which is what a draft is keyed by.
    pub slot: i64,
    /// What the action **added** — never a path the draft already carried,
    /// which was not this action's to take away.
    pub paths: Vec<String>,
}

impl Intent for Attached {
    fn describe(&self) -> String {
        format!("slot:{} carries {} file(s)", self.slot, self.paths.len())
    }
    fn reverse(&self, w: &World) -> Result<(), String> {
        let (slot, paths) = (self.slot, self.paths.clone());
        w.store()
            .write(move |c| carry::detach_tx(c, slot, &paths))
            .map_err(|e| e.to_string())
    }
    fn reapply(&self, w: &World) -> Result<(), String> {
        let (slot, paths, now) = (self.slot, self.paths.clone(), w.now());
        w.store()
            .write(move |c| carry::attach_tx(c, slot, &paths, now).map(|_| ()))
            .map_err(|e| e.to_string())
    }
}

/// Discarding a compose takes its text with it.
pub struct Discarded {
    pub slot: i64,
    pub draft: Draft,
    pub seed: Seed,
}

impl Intent for Discarded {
    fn describe(&self) -> String {
        format!("slot:{} draft discarded", self.slot)
    }
    fn reverse(&self, w: &World) -> Result<(), String> {
        let (now, slot, seed, draft) = (w.now(), self.slot, self.seed, self.draft.clone());
        w.store()
            .write(move |c| model::upsert_draft_tx(c, slot, seed, &draft, now))
            .map_err(|e| e.to_string())
    }
    fn reapply(&self, w: &World) -> Result<(), String> {
        let slot = self.slot;
        w.store()
            .write(move |c| model::discard_draft_tx(c, slot))
            .map_err(|e| e.to_string())
    }
}
