//! The three servers mail talks to, and the fake that stands in for them.
//!
//! An app defines its own capabilities and supplies them in
//! [`App::outside`](kernel::app::App::outside). Mail's are [`Imap`], [`Smtp`]
//! and [`OAuth`]. A window's own run reaches the real ones ([`real`]); a
//! scripted run and every `Fake` world get [`FakeServers`], which registers
//! itself under all three traits *and* its concrete type, so a test can reach
//! `get::<FakeServers>()` to plant a mail or take the server offline.

use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use kernel::app::{Capabilities, Env, Mode};

use super::seed;

// -- what the outside answers with ---------------------------------------------

/// A folder as the server lists it.
#[derive(Debug, Clone)]
pub struct RemoteFolder {
    pub name: String,
    /// inbox | archive | sent | spam | trash — `None` folders are not
    /// mirrored.
    pub role: Option<String>,
    /// This is the provider's *all mail* view (`\All`), not a folder of its
    /// own: Gmail's, where every message also lives under whatever labels it
    /// has. A move target, never an ingest source — see
    /// [`fetch_account`](super::sync::fetch_account).
    pub all_mail: bool,
}

/// SELECT results.
#[derive(Debug, Clone, Copy)]
pub struct FolderMeta {
    pub uidvalidity: u32,
    pub uidnext: u32,
    /// Whether the folder keeps keywords such as `$Forwarded` — its
    /// `PERMANENTFLAGS` name the keyword or allow any (`\*`). A server that
    /// says otherwise, or says nothing, may accept a `STORE` and forget the
    /// flag by the next session, so a mark is neither pushed to nor read from
    /// one; it stays local there.
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

/// What waiting on the server came to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Watched {
    /// The server spoke: mail arrived, or went. Worth a pass.
    Changed,
    /// The window ran out with the folder as it was.
    Quiet,
    /// This server does not offer `IDLE`, so there is nothing to wait on
    /// and the interval is the only cadence it has.
    Unsupported,
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
#[derive(Debug, Clone, Default)]
pub struct Outgoing {
    pub to: String,
    pub subject: String,
    pub body: String,
    /// The Message-ID this replies to, for threading headers.
    pub in_reply_to: Option<String>,
    /// What the mail replied to itself referenced, so `References` carries
    /// the whole chain (RFC 5322) and a reply to a reply threads for the
    /// other side too.
    pub references: Vec<String>,
    /// What it carries — read off the disk by [`Submit`](super::effects::Submit)
    /// as it goes out, never stored: this value is built at submit time, and a
    /// payload holding a file's bytes would be both stale and enormous.
    pub attachments: Vec<Part>,
}

/// One part of a mail on its way out: what compose attached, with the bytes
/// it will actually carry.
#[derive(Clone, Default)]
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
#[derive(Clone, PartialEq, Eq)]
pub enum Auth {
    /// An app password: IMAP `LOGIN`, SMTP `AUTH PLAIN`.
    Password(String),
    /// An OAuth 2 access token: SASL `XOAUTH2` on both (see
    /// [`oauth`](super::oauth)). Short-lived — the caller fetches a fresh one
    /// per connect rather than holding it.
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

    /// Whether it is a bearer token — the one a refusal spends.
    #[must_use]
    pub fn is_bearer(&self) -> bool {
        matches!(self, Auth::Bearer(_))
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
/// cannot leak one, and no `describe` ever prints it — `describe` is what
/// lands in the log.
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

    /// The secret itself, for the one backend that must compare it.
    #[must_use]
    pub fn secret(&self) -> &str {
        self.auth.secret()
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

// -- the capabilities ----------------------------------------------------------

/// Reading and filing mail on a server. Object-safe on purpose: this is the
/// axis where the compiler still tells you a backend forgot a case.
///
/// Errors are strings — they land on a status line, for a human.
pub trait Imap {
    /// Opens (or replaces) this account's mail session.
    ///
    /// # Errors
    ///
    /// If the server is unreachable or refuses the credentials.
    fn connect(&mut self, account: i64, c: &Creds) -> Result<(), String>;

    /// # Errors
    ///
    /// If there is no session, or the server refuses.
    fn folders(&mut self, account: i64) -> Result<Vec<RemoteFolder>, String>;

    /// # Errors
    ///
    /// If there is no such folder.
    fn folder_meta(&mut self, account: i64, folder: &str) -> Result<FolderMeta, String>;

    /// Messages with `uid >= from`, ascending — what a pass receives new
    /// mail with.
    ///
    /// # Errors
    ///
    /// If there is no such folder.
    fn fetch(&mut self, account: i64, folder: &str, from: u32) -> Result<Vec<RemoteMail>, String>;

    /// Exactly these uids, ascending — what the backfill reaches into a
    /// folder's past with. `uids` is sorted and holds no duplicates.
    ///
    /// # Errors
    ///
    /// If there is no such folder.
    fn fetch_uids(
        &mut self,
        account: i64,
        folder: &str,
        uids: &[u32],
    ) -> Result<Vec<RemoteMail>, String>;

    /// The uids in the folder: every one, those without `\Seen`, or those
    /// wearing `$Forwarded`.
    ///
    /// # Errors
    ///
    /// If there is no such folder.
    fn uids(&mut self, account: i64, folder: &str, which: UidSet) -> Result<HashSet<u32>, String>;

    /// Waits on the folder: RFC 2177 `IDLE` until the server says something
    /// worth a pass, or `window` runs out. Blocks for that long — this is
    /// the one verb that is meant to.
    ///
    /// A flag another client set is *not* worth a pass: the interval carries
    /// those, and a mark this app just pushed would otherwise come back as
    /// news and cost a full sync for nothing.
    ///
    /// # Errors
    ///
    /// If there is no session, no such folder, or the link died waiting.
    fn idle(&mut self, account: i64, folder: &str, window: Duration) -> Result<Watched, String>;

    /// `UID MOVE`; the new uid when the server says (UIDPLUS' COPYUID),
    /// `None` otherwise.
    ///
    /// # Errors
    ///
    /// If either folder or the uid is gone.
    fn move_uid(
        &mut self,
        account: i64,
        from: &str,
        to: &str,
        uid: u32,
    ) -> Result<Option<u32>, String>;

    /// `UID STORE` a flag on or off.
    ///
    /// # Errors
    ///
    /// If there is no such folder.
    fn store_flag(
        &mut self,
        account: i64,
        folder: &str,
        uid: u32,
        flag: MailFlag,
        on: bool,
    ) -> Result<(), String>;

    /// `APPEND` raw bytes — filing sent mail.
    ///
    /// # Errors
    ///
    /// If there is no session.
    fn append(&mut self, account: i64, folder: &str, raw: &[u8]) -> Result<(), String>;
}

/// Handing a mail to a submission server.
pub trait Smtp {
    /// Answers the formatted RFC 822 bytes, which are what gets filed to
    /// Sent.
    ///
    /// # Errors
    ///
    /// If the server is unreachable or refuses the credentials.
    fn submit(&mut self, c: &Creds, m: &Outgoing) -> Result<Vec<u8>, String>;
}

/// The OAuth grant an account signs in with.
///
/// One verb rather than "read the refresh token, then POST": the token
/// endpoint is a network round trip that must be fakeable, and the cache that
/// keeps it from happening per connect belongs to the backend that owns the
/// process, not to the caller.
pub trait OAuth {
    /// A usable access token for this address, refreshed against the provider
    /// when the cached one has expired.
    ///
    /// # Errors
    ///
    /// If there is no grant, or the provider refuses to renew it.
    fn access_token(&mut self, email: &str) -> Result<String, String>;
}

// -- the fake ------------------------------------------------------------------

/// One account's in-memory mail server.
#[derive(Default, Clone, Debug)]
pub struct FakeServer {
    /// `folder → (uidvalidity, next uid, mails)`.
    pub folders: HashMap<String, (u32, u32, Vec<RemoteMail>)>,
    /// Whether MOVE reports the new uid (UIDPLUS' COPYUID). Both server
    /// behaviours exist in the wild; the demo's reports it, because a
    /// uid-less move is only re-established by a Message-ID the demo seed
    /// mostly does not carry.
    pub copyuid: bool,
    /// Whether the folders keep keywords — a server whose `PERMANENTFLAGS`
    /// carry `$Forwarded` or `\*`. Off, and the mark stays local.
    pub keywords: bool,
    /// Whether this server offers `IDLE`. Off, and a watch parks.
    pub idle: bool,
    /// Something has arrived that a watch has not been told about yet. The
    /// fake cannot block, so this is how it says the same thing: whoever
    /// waits next is told once, and the folder is not part of it — one
    /// server, one piece of news.
    pub news: bool,
    /// Mail this account handed to SMTP.
    pub submitted: Vec<Outgoing>,
    /// How many letters this account has handed to a fetch — what a test
    /// counts to see that a folder already mirrored costs no round trip.
    pub fetched: usize,
    /// The backfill batches it was asked for, in order: `(folder, uids)`.
    /// A test reads them to see a folder arrive newest-first, a batch at a
    /// time.
    pub backfills: Vec<(String, Vec<u32>)>,
}

impl FakeServer {
    /// The same, for a letter that already wears `$Forwarded` — what the
    /// demo server holds for the one the seed says was passed on.
    pub fn deliver_flagged(
        &mut self,
        folder: &str,
        unread: bool,
        forwarded: bool,
        raw: &str,
    ) -> u32 {
        let f = self
            .folders
            .entry(folder.to_string())
            .or_insert((1, 1, Vec::new()));
        let uid = f.1;
        f.1 += 1;
        f.2.push(RemoteMail {
            uid,
            unread,
            forwarded,
            raw: raw.as_bytes().to_vec(),
        });
        self.news = true;
        uid
    }

    /// Creates an empty folder with a chosen uidvalidity.
    pub fn folder(&mut self, name: &str, uidvalidity: u32) {
        self.folders
            .insert(name.to_string(), (uidvalidity, 1, Vec::new()));
    }

    fn get(&self, name: &str) -> Result<&(u32, u32, Vec<RemoteMail>), String> {
        self.folders
            .get(name)
            .ok_or_else(|| "no such folder".to_string())
    }

    /// The role a folder of this name plays, as the server reports it.
    fn role_of(name: &str) -> Option<String> {
        match name {
            "INBOX" => Some("inbox".into()),
            "Archive" => Some("archive".into()),
            "Sent" => Some("sent".into()),
            "Spam" => Some("spam".into()),
            "Trash" => Some("trash".into()),
            _ => None,
        }
    }
}

/// What a scripted run's mail capability is: one in-memory server per
/// account, a keychain that is a map, a Google grant that mints a token
/// without leaving the process, and a switch that takes the whole thing
/// offline.
///
/// Shared inside, like the kernel's own fakes: each worker builds its own
/// world, and a mail moved on the sync thread must be there for the panel
/// that reads it back.
#[derive(Clone, Default)]
pub struct FakeServers(Arc<Mutex<Servers>>);

#[derive(Default)]
struct Servers {
    by_account: HashMap<i64, FakeServer>,
    /// `address → password`, as the keychain would hold it.
    secrets: HashMap<String, String>,
    /// `address → access token`, as a Google grant would mint it.
    grants: HashMap<String, String>,
    /// Accounts with a live session. A verb that reaches a server without
    /// one is a bug in the pass, and this catches it.
    connected: HashSet<i64>,
    /// When set, every verb fails with this — the offline test.
    down: Option<String>,
}

impl std::fmt::Debug for FakeServers {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("FakeServers")
            .field("accounts", &self.accounts())
            .field("down", &self.why())
            .finish()
    }
}

impl FakeServers {
    #[must_use]
    pub fn new() -> FakeServers {
        FakeServers::default()
    }

    /// The demo account's server, filled from the same list the store's seed
    /// is: the same folders, the same order, and so the same uids. The first
    /// sync pass is then a no-op, which is what a mirror of a server the
    /// store already agrees with should be.
    #[must_use]
    pub fn demo() -> FakeServers {
        let s = FakeServers::new();
        {
            let mut g = s.0.lock().expect("the fake servers");
            g.secrets
                .insert(seed::ADDRESS.to_string(), seed::PASSWORD.to_string());
            let mut server = FakeServer {
                copyuid: true,
                keywords: true,
                idle: true,
                ..FakeServer::default()
            };
            for (name, _) in seed::FOLDERS {
                server.folder(name, 1);
            }
            for m in seed::mails() {
                server.deliver_flagged(
                    seed::folder_name(m.folder),
                    m.unread,
                    m.forwarded,
                    &seed::rfc822(&m),
                );
            }
            // What the demo server was seeded with is not news: it is what
            // the store already agrees with, and a watch has nothing to
            // report about it.
            server.news = false;
            g.by_account.insert(seed::ACCOUNT, server);
        }
        s
    }

    /// Takes every server offline with this reason, or brings them back.
    pub fn set_down(&self, why: Option<&str>) {
        if let Ok(mut g) = self.0.lock() {
            g.down = why.map(str::to_string);
        }
    }

    /// Why the servers are refusing, if they are.
    #[must_use]
    pub fn why(&self) -> Option<String> {
        self.0.lock().ok()?.down.clone()
    }

    /// How many accounts have a server.
    #[must_use]
    pub fn accounts(&self) -> usize {
        self.0.lock().map(|g| g.by_account.len()).unwrap_or(0)
    }

    /// Plants an OAuth grant, as a finished sign-in would: the address, and
    /// the access token [`OAuth::access_token`] hands back for it.
    #[cfg(test)]
    pub fn grant(&self, email: &str, token: &str) {
        if let Ok(mut g) = self.0.lock() {
            g.grants.insert(email.to_string(), token.to_string());
            // A bearer session compares the token the same way a password
            // session compares a password.
            g.secrets.insert(email.to_string(), token.to_string());
        }
    }

    /// Reaches one account's server — what a test plants a mail through.
    #[cfg(test)]
    pub fn with<T>(&self, account: i64, f: impl FnOnce(&mut FakeServer) -> T) -> Option<T> {
        let mut g = self.0.lock().ok()?;
        Some(f(g.by_account.entry(account).or_default()))
    }

    /// Everything handed to SMTP, oldest first.
    #[cfg(test)]
    #[must_use]
    pub fn submitted(&self) -> Vec<Outgoing> {
        let Ok(g) = self.0.lock() else {
            return Vec::new();
        };
        let mut all: Vec<(i64, Outgoing)> = g
            .by_account
            .iter()
            .flat_map(|(a, s)| s.submitted.iter().map(move |m| (*a, m.clone())))
            .collect();
        all.sort_by_key(|(a, _)| *a);
        all.into_iter().map(|(_, m)| m).collect()
    }

    /// The account's server, once a session is open and the servers are up.
    fn live<T>(
        &self,
        account: i64,
        f: impl FnOnce(&mut FakeServer) -> Result<T, String>,
    ) -> Result<T, String> {
        let mut g = self.0.lock().map_err(|_| "the servers are poisoned")?;
        if let Some(e) = &g.down {
            return Err(e.clone());
        }
        if !g.connected.contains(&account) {
            return Err("not connected".into());
        }
        f(g.by_account.entry(account).or_default())
    }
}

impl Imap for FakeServers {
    fn connect(&mut self, account: i64, c: &Creds) -> Result<(), String> {
        let mut g = self.0.lock().map_err(|_| "the servers are poisoned")?;
        if let Some(e) = &g.down {
            return Err(e.clone());
        }
        if g.secrets.get(&c.user).map(String::as_str) != Some(c.secret()) {
            return Err("authentication failed".into());
        }
        g.connected.insert(account);
        Ok(())
    }

    fn folders(&mut self, account: i64) -> Result<Vec<RemoteFolder>, String> {
        self.live(account, |s| {
            let mut names: Vec<String> = s.folders.keys().cloned().collect();
            names.sort();
            Ok(names
                .into_iter()
                .map(|n| RemoteFolder {
                    role: FakeServer::role_of(&n),
                    all_mail: false,
                    name: n,
                })
                .collect())
        })
    }

    fn folder_meta(&mut self, account: i64, folder: &str) -> Result<FolderMeta, String> {
        self.live(account, |s| {
            let keywords = s.keywords;
            let f = s.get(folder)?;
            Ok(FolderMeta {
                uidvalidity: f.0,
                uidnext: f.1,
                keywords,
            })
        })
    }

    fn fetch(&mut self, account: i64, folder: &str, from: u32) -> Result<Vec<RemoteMail>, String> {
        self.live(account, |s| {
            let f = s.get(folder)?;
            let out: Vec<RemoteMail> = f.2.iter().filter(|m| m.uid >= from).cloned().collect();
            s.fetched += out.len();
            Ok(out)
        })
    }

    fn fetch_uids(
        &mut self,
        account: i64,
        folder: &str,
        uids: &[u32],
    ) -> Result<Vec<RemoteMail>, String> {
        self.live(account, |s| {
            let f = s.get(folder)?;
            let out: Vec<RemoteMail> =
                f.2.iter()
                    .filter(|m| uids.contains(&m.uid))
                    .cloned()
                    .collect();
            s.fetched += out.len();
            s.backfills.push((folder.to_string(), uids.to_vec()));
            Ok(out)
        })
    }

    fn uids(&mut self, account: i64, folder: &str, which: UidSet) -> Result<HashSet<u32>, String> {
        self.live(account, |s| {
            let f = s.get(folder)?;
            Ok(f.2
                .iter()
                .filter(|m| match which {
                    UidSet::All => true,
                    UidSet::Unseen => m.unread,
                    UidSet::Forwarded => m.forwarded,
                })
                .map(|m| m.uid)
                .collect())
        })
    }

    /// The fake cannot block, so it answers what it knows at once: news if
    /// something arrived since the last watch, quiet otherwise. The caller
    /// paces itself on a watch that came back faster than its window, which
    /// is what keeps a fake world from spinning — see
    /// [`IdleWatch`](super::sync::IdleWatch).
    fn idle(&mut self, account: i64, folder: &str, _window: Duration) -> Result<Watched, String> {
        self.live(account, |s| {
            if !s.idle {
                return Ok(Watched::Unsupported);
            }
            s.get(folder)?;
            Ok(if std::mem::take(&mut s.news) {
                Watched::Changed
            } else {
                Watched::Quiet
            })
        })
    }

    fn move_uid(
        &mut self,
        account: i64,
        from: &str,
        to: &str,
        uid: u32,
    ) -> Result<Option<u32>, String> {
        self.live(account, |s| {
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
            let dst = s
                .folders
                .entry(to.to_string())
                .or_insert((1, 1, Vec::new()));
            m.uid = dst.1;
            dst.1 += 1;
            let new = m.uid;
            dst.2.push(m);
            s.news = true;
            Ok(s.copyuid.then_some(new))
        })
    }

    fn store_flag(
        &mut self,
        account: i64,
        folder: &str,
        uid: u32,
        flag: MailFlag,
        on: bool,
    ) -> Result<(), String> {
        self.live(account, |s| {
            // A server that keeps no keywords takes the STORE and forgets
            // it — which is the behaviour the local-only rule exists for.
            let no_keywords = !s.keywords;
            let f = s
                .folders
                .get_mut(folder)
                .ok_or_else(|| "no such folder".to_string())?;
            for m in &mut f.2 {
                if m.uid == uid {
                    match flag {
                        MailFlag::Seen => m.unread = !on,
                        MailFlag::Forwarded if no_keywords => {}
                        MailFlag::Forwarded => m.forwarded = on,
                    }
                }
            }
            Ok(())
        })
    }

    fn append(&mut self, account: i64, folder: &str, raw: &[u8]) -> Result<(), String> {
        self.live(account, |s| {
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
            s.news = true;
            Ok(())
        })
    }
}

impl Smtp for FakeServers {
    fn submit(&mut self, c: &Creds, m: &Outgoing) -> Result<Vec<u8>, String> {
        let mut g = self.0.lock().map_err(|_| "the servers are poisoned")?;
        if let Some(e) = &g.down {
            return Err(e.clone());
        }
        if g.secrets.get(&c.user).map(String::as_str) != Some(c.secret()) {
            return Err("authentication failed".into());
        }
        // The bytes the real transport would file to Sent, headers
        // included, so a sent mail that syncs back threads as it would.
        let n: usize = g.by_account.values().map(|s| s.submitted.len()).sum::<usize>() + 1;
        let mut raw = format!(
            "From: {} <{}>\r\nTo: {}\r\nSubject: {}\r\nDate: {}\r\nMessage-ID: <sent-{n}@fake>\r\n",
            c.user,
            c.user,
            m.to,
            m.subject,
            seed::sent_date()
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
            // With parts it is a `multipart/mixed`, as the real transport
            // builds one: the letter first, then each file base64'd under
            // its own `Content-Disposition`.
            raw += "MIME-Version: 1.0\r\nContent-Type: multipart/mixed; \
                    boundary=\"sent\"\r\n\r\n";
            raw += &format!(
                "--sent\r\nContent-Type: text/plain; charset=utf-8\r\n\r\n{}\r\n",
                m.body
            );
            for p in &m.attachments {
                raw += &format!(
                    "--sent\r\nContent-Type: {}\r\n\
                     Content-Disposition: attachment; filename=\"{}\"\r\n\
                     Content-Transfer-Encoding: base64\r\n\r\n{}\r\n",
                    p.mime,
                    p.name,
                    super::html::base64_encode(&p.bytes)
                );
            }
            raw += "--sent--\r\n";
        }
        // Whichever account owns this address; the first server otherwise.
        let acct = *g.by_account.keys().next().unwrap_or(&seed::ACCOUNT);
        g.by_account
            .entry(acct)
            .or_default()
            .submitted
            .push(m.clone());
        Ok(raw.into_bytes())
    }
}

impl OAuth for FakeServers {
    /// The grant a test planted, or the refusal a real one gives when there
    /// is none.
    fn access_token(&mut self, email: &str) -> Result<String, String> {
        let g = self.0.lock().map_err(|_| "the servers are poisoned")?;
        if let Some(e) = &g.down {
            return Err(e.clone());
        }
        g.grants
            .get(email)
            .cloned()
            .ok_or_else(|| format!("{email} has no google grant — sign in again"))
    }
}

/// Mail's capabilities for one world.
///
/// The real protocols are reached by one kind of run and one only: a window's
/// own stage ([`Mode::Real`]) that is not replaying a script and is not on a
/// virtual clock. Everything else — every suite, every test, every library
/// mount — gets [`FakeServers`], because a suite that opened a socket would be
/// neither reproducible nor anyone's business but the machine's. The password
/// the demo account signs in with is planted in the world's own keychain, so
/// [`creds_for`](super::model::creds_for) finds it exactly as it would find a
/// human's.
pub fn install(mode: Mode, env: &Env, caps: &mut Capabilities) {
    if real_run(mode, env) {
        super::real::install(env, caps);
        return;
    }
    let servers = servers_for(env);
    env.secrets.plant(seed::ADDRESS, seed::PASSWORD);
    caps.insert::<dyn Imap>(Box::new(servers.clone()));
    caps.insert::<dyn Smtp>(Box::new(servers.clone()));
    caps.insert::<dyn OAuth>(Box::new(servers.clone()));
    caps.insert::<FakeServers>(Box::new(servers));
}

/// Whether this world may open a socket: the window's own outside, replaying
/// nothing, on the wall clock.
fn real_run(mode: Mode, env: &Env) -> bool {
    mode == Mode::Real && !env.scripted && !env.clock.is_virtual()
}

/// Which servers a world reaches.
///
/// A real run puts each worker on its own thread with its own world, and all
/// of them are looking at *one* mailbox out there — so they share one fake,
/// or a letter the sender files to Sent would never come back through the
/// sync pass. Under virtual time the passes run inline in the one world that
/// built them, and that world is a test or a scripted run: it gets servers of
/// its own, so any number run in parallel.
fn servers_for(env: &Env) -> FakeServers {
    use std::sync::OnceLock;
    static SHARED: OnceLock<FakeServers> = OnceLock::new();
    if env.clock.is_virtual() {
        demo_servers()
    } else {
        SHARED.get_or_init(demo_servers).clone()
    }
}

/// The demo servers, refusing everything if this run asked them to.
///
/// `SUPERAPP_MAIL_DOWN=<reason>` is mail's own knob, read by mail from the
/// environment as [`send_delay`](super::model::send_delay) is: argv belongs
/// to the shell. It is what lets a suite watch a send fail honestly, rather
/// than mime a failure.
fn demo_servers() -> FakeServers {
    let s = FakeServers::demo();
    if let Some(why) = std::env::var("SUPERAPP_MAIL_DOWN")
        .ok()
        .map(|w| w.trim().to_string())
        .filter(|w| !w.is_empty())
    {
        s.set_down(Some(&why));
    }
    s
}
