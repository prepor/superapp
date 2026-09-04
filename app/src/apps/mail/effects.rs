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
use std::sync::atomic::{AtomicI64, Ordering};
use std::sync::Arc;

use kernel::caps::{Disk, Secrets};
use kernel::effect::{Ctx, Deferred, Effect, Registry, World};
use kernel::history::Intent;
use rusqlite::{Connection, Transaction};
use serde::{Deserialize, Serialize};

use super::accounts;
use super::caps::{
    Creds, FolderMeta, Imap, MailFlag, OAuth, Outgoing, Part, RemoteFolder, RemoteMail, Smtp,
    UidSet,
};
use super::carry;
use super::model::{self, Draft, MailId, Seed};

/// The mail app's deferred effects. Each app registers its own, so adding one
/// touches no central list.
pub fn register(reg: &mut Registry) {
    reg.register::<Move>();
    reg.register::<Seen>();
    reg.register::<Forwarded>();
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

/// Make the server agree that a mail was passed on — the `$Forwarded`
/// keyword, which is what every other client draws its arrow from. The read
/// flag's twin in every respect.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Forwarded {
    pub account: i64,
    pub message: i64,
    pub folder: String,
    pub uid: u32,
    pub on: bool,
}

impl Effect for Forwarded {
    const KIND: &'static str = "forwarded";
    type Reply = ();

    fn describe(&self) -> String {
        format!(
            "mark uid {} in {} {}",
            self.uid,
            self.folder,
            if self.on {
                "forwarded"
            } else {
                "not forwarded"
            }
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
            MailFlag::Forwarded,
            self.on,
        )
    }
}

impl Deferred for Forwarded {
    fn idempotent(&self) -> bool {
        true
    }

    fn still_wanted(&self, db: &Connection) -> bool {
        db.query_row(
            "SELECT 1 FROM message m JOIN server_msg s ON s.message = m.id
             WHERE m.id = ?1 AND m.forwarded = ?2 AND s.forwarded != ?2",
            rusqlite::params![self.message, self.on],
            |_| Ok(()),
        )
        .is_ok()
    }

    fn settle(&self, tx: &Transaction, _reply: &()) -> rusqlite::Result<()> {
        tx.execute(
            "UPDATE server_msg SET forwarded = ?1 WHERE message = ?2",
            rusqlite::params![self.on, self.message],
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
        let mut d = load_outgoing(cx.db, self.outbox)?;
        // The files the draft named are read *now*, through the disk, rather
        // than having been copied into the store when they were attached:
        // what leaves is the file as it stands, and a file that has since
        // gone fails the send instead of sending a stale copy of it.
        let here = kernel::store::this_device(cx.db);
        for f in &d.files {
            // A path is a file on the machine it was picked on. These rows
            // replicate, so `~/Downloads/report-q3.pdf` over here is some
            // other file or none — refuse rather than carry out whatever
            // happens to sit there.
            if !f.device.is_empty() && !here.is_empty() && f.device != here {
                return Err(format!(
                    "“{}” was attached on another device — attach it again here",
                    f.name
                ));
            }
            // One byte past the cap is asked for, so a file that grew since
            // it was attached is *refused* rather than quietly truncated to
            // the limit and sent under its own name.
            let cap = kernel::caps::ATTACH_MAX as usize;
            let bytes = cx
                .cap::<dyn Disk>()?
                .read_file(&kernel::caps::real_path(&f.path), cap + 1)
                .map_err(|e| format!("cannot attach “{}”: {e}", f.name))?;
            if bytes.len() > cap {
                return Err(format!(
                    "“{}” is past {} now — attach it again or send it another way",
                    f.name,
                    kernel::caps::fmt_size(kernel::caps::ATTACH_MAX)
                ));
            }
            d.mail.attachments.push(Part {
                name: f.name.clone(),
                mime: kernel::caps::mime_of(&f.name).to_string(),
                bytes,
            });
        }
        // The two backends are taken one at a time: `cap` borrows the bag,
        // and a bearer sign-in reads no password while a password one never
        // asks for a token.
        let smtp = if d.oauth {
            let token = cx.cap::<dyn OAuth>()?.access_token(&d.email)?;
            Creds::bearer(&d.smtp, &d.email, token)
        } else {
            let secrets = cx.cap::<dyn Secrets>()?;
            accounts::creds_for(secrets, &d.email, &d.smtp)?
        };
        let raw = cx.cap::<dyn Smtp>()?.submit(&smtp, &d.mail)?;
        // Gmail's SMTP files its own copy into Sent Mail, so appending one
        // would leave the human looking at the same letter twice. The
        // account's provider is what knows; a plain relay files nothing.
        if d.oauth && super::oauth::GOOGLE.files_sent_itself {
            return Ok(None);
        }
        // The mail is gone; filing it is best effort and never fails a send.
        if d.imap.is_empty() {
            return Ok(Some("no imap host to file to Sent".into()));
        }
        // The same secret reaches both servers, so the token is not minted
        // twice — `Creds` is cheap, and the backend's cache is the point.
        let imap = Creds {
            host: d.imap,
            user: d.email,
            auth: smtp.auth,
        };
        let filed = {
            let server = cx.cap::<dyn Imap>()?;
            server
                .connect(d.account, &imap)
                .and_then(|()| server.append(d.account, &d.sent, &raw))
        };
        Ok(filed
            .err()
            .map(|e| format!("sent; filing to Sent failed: {e}")))
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
        // The mail a forward passed on is now forwarded — intent, which the
        // next push pass sets on the server as `$Forwarded`. Not an action:
        // it is a consequence of a send that has already left.
        tx.execute(
            "UPDATE message SET forwarded = 1
             WHERE id = (SELECT fwd_message FROM draft WHERE panel = ?1)",
            [self.outbox],
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
    /// Whether the account signs in with a Google grant.
    oauth: bool,
    mail: Outgoing,
    /// What the draft named to carry out — paths; the bytes are read at the
    /// last moment, in [`Submit::perform`].
    files: Vec<carry::DraftFile>,
}

fn load_outgoing(db: &Connection, outbox: i64) -> Result<Outgo, String> {
    let files = carry::all(db, outbox)
        .map_err(|e| format!("outbox:{outbox} cannot read its attachments: {e}"))?;
    db.query_row(
        "SELECT o.account, a.email, COALESCE(a.smtp_host,''), COALESCE(a.imap_host,''),
                COALESCE((SELECT name FROM folder WHERE account = a.id AND role = 'sent'), 'Sent'),
                d.to_addr, d.subject, d.body,
                (SELECT message_id FROM message WHERE id = d.re_message),
                (SELECT message_id FROM message
                  WHERE id = COALESCE(d.re_message, d.fwd_message)),
                (SELECT GROUP_CONCAT(mid, ' ') FROM reference
                  WHERE message = COALESCE(d.re_message, d.fwd_message)),
                COALESCE(a.auth, '') = ?2
         FROM outbox o
         JOIN account a ON a.id = o.account
         JOIN draft d ON d.panel = o.id
         WHERE o.id = ?1",
        rusqlite::params![outbox, super::oauth::GOOGLE.name],
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
                oauth: r.get(11)?,
                mail: Outgoing {
                    to: r.get(5)?,
                    subject: r.get(6)?,
                    body: r.get(7)?,
                    in_reply_to: r.get::<_, Option<String>>(8)?.filter(|s| !s.is_empty()),
                    references,
                    attachments: Vec::new(),
                },
                files: files.clone(),
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

/// Fetch an exact set of uids — the backfill's reach into a folder's past.
#[derive(Debug, Clone)]
pub struct Backfill {
    pub account: i64,
    pub folder: String,
    /// Ascending, no duplicates: what
    /// [`fetch_account`](super::sync::fetch_account) found the server still
    /// has and this store does not.
    pub uids: Vec<u32>,
}

impl Effect for Backfill {
    const KIND: &'static str = "backfill";
    type Reply = Vec<RemoteMail>;
    fn describe(&self) -> String {
        let lowest = self.uids.first().copied().unwrap_or(0);
        let highest = self.uids.last().copied().unwrap_or(0);
        format!(
            "backfill {} older in {} (uid {lowest}..{highest})",
            self.uids.len(),
            self.folder,
        )
    }
    fn writes(&self) -> bool {
        false
    }
    fn entity(&self) -> Option<String> {
        Some(account_entity(self.account))
    }
    fn perform(&self, cx: &mut Ctx<'_>) -> Result<Self::Reply, String> {
        cx.cap::<dyn Imap>()?
            .fetch_uids(self.account, &self.folder, &self.uids)
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
            UidSet::Forwarded => "forwarded",
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
    /// The role it went to: `archive`, `trash`, or `inbox` for a letter taken
    /// back out of the spam.
    pub role: &'static str,
}

impl Intent for Filed {
    fn describe(&self) -> String {
        let verb = match self.role {
            "trash" => "deleted",
            "inbox" => "put in the inbox",
            _ => "archived",
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

/// A failed send filed again (the problems panel's *retry*): the row went
/// back to `pending` with a fresh window, and the submit job that failed last
/// time stood down. Giving it back puts the failure back — the row and the
/// job — so the draft stays reachable through the problems panel rather than
/// stranded behind a compose that no snapshot reopens.
pub struct Retried {
    pub outbox: i64,
    /// The failure the row carried, put back with it.
    pub error: String,
    /// How long the window is, so redo files a fresh one.
    pub delay: f64,
}

impl Intent for Retried {
    fn describe(&self) -> String {
        format!("outbox:{} retried", self.outbox)
    }

    /// As a send's: once the executor has taken the row, the mail is gone.
    fn blocked(&self, w: &World) -> Option<String> {
        match w.store().conn().query_row(
            "SELECT status FROM outbox WHERE id = ?1",
            [self.outbox],
            |r| r.get::<_, String>(0),
        ) {
            Ok(s) if s == "pending" || s == "failed" => None,
            Ok(_) => Some("already sent".into()),
            Err(_) => None,
        }
    }

    fn reverse(&self, w: &World) -> Result<(), String> {
        let (outbox, error) = (self.outbox, self.error.clone());
        w.store()
            .write(move |c| {
                c.execute(
                    "UPDATE outbox SET status = 'failed', error = ?2
                     WHERE id = ?1 AND status IN ('pending', 'failed')",
                    rusqlite::params![outbox, error],
                )?;
                // The job the retry stood down stands again, so the row reads
                // as it did: the attempts it took, the error it gave.
                c.execute(
                    "UPDATE effect SET status = 'failed'
                     WHERE id = (SELECT MAX(id) FROM effect
                                 WHERE kind = 'submit' AND status = 'obsolete'
                                   AND payload ->> 'outbox' = ?1)",
                    [outbox],
                )
                .map(|_| ())
            })
            .map_err(|e| e.to_string())
    }

    fn reapply(&self, w: &World) -> Result<(), String> {
        let (outbox, after) = (self.outbox, w.now() + self.delay);
        w.store()
            .write(move |c| model::file_send_tx(c, outbox, after))
            .map_err(|e| e.to_string())
    }
}

/// A failed send reopened as a draft (the problems panel's *reopen*): the
/// draft moved from the outbox's id to a fresh compose slot, and the failed
/// row went. Giving it back moves the draft home and restores the row with
/// the error it carried.
pub struct Reopened {
    /// The failed outbox row — and the draft's old slot id.
    pub old: i64,
    /// The compose slot it reopened on. The link's `Nav` cannot name a slot
    /// that does not exist yet, so the panel writes it here once it has been
    /// placed.
    pub new: Arc<AtomicI64>,
    /// The failure the row carried, put back with it.
    pub error: String,
}

impl Reopened {
    fn new_id(&self) -> i64 {
        self.new.load(Ordering::Relaxed)
    }
}

/// The slot a reopened send landed in, by the outbox row it came from.
///
/// A shared cell rather than a value, because the two halves happen at
/// different moments: the *reopen* verb records the intent while the open is
/// still an intention, and the compose panel writes its own slot in when the
/// layout has placed it. One entry per reopen for the life of the process,
/// which is as many as a person presses the button.
pub fn reopen_cell(outbox: i64) -> Arc<AtomicI64> {
    use std::collections::HashMap;
    use std::sync::{Mutex, OnceLock};
    static CELLS: OnceLock<Mutex<HashMap<i64, Arc<AtomicI64>>>> = OnceLock::new();
    let mut g = CELLS
        .get_or_init(|| Mutex::new(HashMap::new()))
        .lock()
        .expect("the reopen cells");
    g.entry(outbox)
        .or_insert_with(|| Arc::new(AtomicI64::new(0)))
        .clone()
}

impl Intent for Reopened {
    fn describe(&self) -> String {
        format!("outbox:{} reopened", self.old)
    }

    /// Once the reopened draft has *gone out* from its new slot, there is no
    /// failed send to put back — the walk steps past this node.
    fn blocked(&self, w: &World) -> Option<String> {
        match w.store().conn().query_row(
            "SELECT status FROM outbox WHERE id = ?1",
            [self.new_id()],
            |r| r.get::<_, String>(0),
        ) {
            Ok(s) if s == "pending" || s == "failed" => None,
            Ok(_) => Some("already sent".into()),
            Err(_) => None,
        }
    }

    fn reverse(&self, w: &World) -> Result<(), String> {
        let (old, new, error, now) = (self.old, self.new_id(), self.error.clone(), w.now());
        if new == 0 {
            return Ok(()); // never placed: nothing moved
        }
        w.store()
            .write(move |c| {
                model::move_draft_tx(c, new, old, now)?;
                c.execute(
                    "INSERT OR REPLACE INTO outbox(id, account, send_after, status, error)
                     SELECT panel, COALESCE(account, 1), 0, 'failed', ?2
                       FROM draft WHERE panel = ?1",
                    rusqlite::params![old, error],
                )
                .map(|_| ())
            })
            .map_err(|e| e.to_string())
    }

    fn reapply(&self, w: &World) -> Result<(), String> {
        let (old, new, now) = (self.old, self.new_id(), w.now());
        if new == 0 {
            return Ok(());
        }
        w.store()
            .write(move |c| model::reopen_send_tx(c, old, new, now))
            .map_err(|e| e.to_string())
    }
}

/// Adding an account. Reversible while it is still empty, which it is at the
/// moment it is added.
pub struct AccountAdded {
    pub id: i64,
    pub email: String,
    pub imap: String,
    pub smtp: String,
    /// The `account.auth` word, so a redo restores a Gmail account as a
    /// Gmail account rather than as one asking for a password.
    pub auth: String,
}

impl Intent for AccountAdded {
    fn describe(&self) -> String {
        format!("account:{} added", self.id)
    }
    fn reverse(&self, w: &World) -> Result<(), String> {
        let (id, now) = (self.id, w.now());
        w.store()
            .write(move |c| accounts::remove_account_tx(c, id, now))
            .map_err(|e| e.to_string())
    }
    fn reapply(&self, w: &World) -> Result<(), String> {
        let (id, email, imap, smtp, auth) = (
            self.id,
            self.email.clone(),
            self.imap.clone(),
            self.smtp.clone(),
            self.auth.clone(),
        );
        w.store()
            .write(move |c| {
                c.execute(
                    "INSERT INTO account(id, label, email, imap_host, smtp_host, auth)
                     VALUES(?1, ?2, ?2, ?3, ?4, ?5)",
                    rusqlite::params![id, email, imap, smtp, auth],
                )
                .map(|_| ())
            })
            .map_err(|e| e.to_string())
    }
}

/// Removing an account takes its mail with it, and no snapshot brings that
/// back. Stated honestly rather than half-restored: the node goes expired and
/// the walk steps past it.
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

/// A draft was given files to carry.
pub struct Attached {
    /// The compose slot, which is what a draft is keyed by.
    pub slot: i64,
    /// What the action **added** — never a file the draft already carried,
    /// which was not this action's to take away.
    pub files: Vec<carry::DraftFile>,
}

impl Intent for Attached {
    fn describe(&self) -> String {
        format!("slot:{} carries {} file(s)", self.slot, self.files.len())
    }
    fn reverse(&self, w: &World) -> Result<(), String> {
        let (slot, paths) = (
            self.slot,
            self.files.iter().map(|f| f.path.clone()).collect::<Vec<_>>(),
        );
        w.store()
            .write(move |c| carry::detach_tx(c, slot, &paths))
            .map_err(|e| e.to_string())
    }
    fn reapply(&self, w: &World) -> Result<(), String> {
        let (slot, files, now) = (self.slot, self.files.clone(), w.now());
        w.store()
            .write(move |c| carry::attach_tx(c, slot, &files, now).map(|_| ()))
            .map_err(|e| e.to_string())
    }
}

/// Discarding a compose takes its text with it — and what it was going to
/// carry, which undo has to put back too.
pub struct Discarded {
    pub slot: i64,
    pub draft: Draft,
    pub seed: Seed,
    /// The rows behind the `CARRIES` line as the discard found them. The
    /// draft row goes back first, because the files hang off the slot *and*
    /// its seed.
    pub files: Vec<carry::DraftFile>,
}

impl Intent for Discarded {
    fn describe(&self) -> String {
        format!("slot:{} draft discarded", self.slot)
    }
    fn reverse(&self, w: &World) -> Result<(), String> {
        let (now, slot, seed, draft) = (w.now(), self.slot, self.seed, self.draft.clone());
        let files = self.files.clone();
        w.store()
            .write(move |c| {
                model::upsert_draft_tx(c, slot, seed, &draft, now)?;
                carry::attach_tx(c, slot, &files, now).map(|_| ())
            })
            .map_err(|e| e.to_string())
    }
    fn reapply(&self, w: &World) -> Result<(), String> {
        let slot = self.slot;
        w.store()
            .write(move |c| model::discard_draft_tx(c, slot))
            .map_err(|e| e.to_string())
    }
}
