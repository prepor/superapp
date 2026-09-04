//! The actual outside: IMAP over rustls, lettre for submission, and the
//! Google token endpoint.
//!
//! One session per account (port 993, `LOGIN` with an app password —
//! fastmail-style — or SASL `XOAUTH2` with a bearer token, which is how Gmail
//! is reached), and a per-process cache of access tokens, because the refresh
//! that mints one is a round trip no connect should pay twice.
//!
//! Nothing here runs under a script: [`install`](super::caps::install) hands a
//! scripted or virtual-clock world the fake instead. What is proved by tests
//! is what can be proved without a server — the folder roles, the keyword
//! rule, the message a draft goes out as.

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::time::Duration;

use kernel::app::{Capabilities, Env};
use kernel::caps::{ClockSource, MemSecrets, Secrets};

use super::caps::{
    Auth, Creds, FolderMeta, Imap, MailFlag, OAuth, Outgoing, RemoteFolder, RemoteMail, Smtp,
    UidSet, Watched,
};
#[cfg(test)]
use super::caps::Part;
use super::oauth;

/// How early a cached access token is treated as spent, so a long sync
/// started with 3 seconds left does not die halfway through.
const TOKEN_MARGIN: f64 = 120.0;

/// Mail's real capabilities for one world. One IMAP session per account and
/// one submission transport per send; both live on the thread that built the
/// world, which is what [`Worker::claims`](kernel::app::Worker::claims) keeps
/// a job on.
pub fn install(env: &Env, caps: &mut Capabilities) {
    caps.insert::<dyn Imap>(Box::new(RealServers::default()));
    caps.insert::<dyn Smtp>(Box::new(RealServers::default()));
    caps.insert::<dyn OAuth>(Box::new(RealOAuth::new(
        env.db_dir.clone(),
        env.secrets.clone(),
        env.clock.clone(),
    )));
}

/// The sessions one world holds.
#[derive(Default)]
pub struct RealServers {
    sessions: HashMap<i64, session::Imap>,
}

impl RealServers {
    fn session(&mut self, account: i64) -> Result<&mut session::Imap, String> {
        self.sessions
            .get_mut(&account)
            .ok_or_else(|| "not connected".to_string())
    }
}

impl Imap for RealServers {
    /// The session this account already has, if the server still has it —
    /// a `NOOP` is one round trip and says so. A pass a minute, and the
    /// batches a backfill takes, must not be a sign-in each: providers
    /// count logins, and Gmail counts them narrowly.
    fn connect(&mut self, account: i64, c: &Creds) -> Result<(), String> {
        if self
            .sessions
            .get_mut(&account)
            .is_some_and(session::Imap::alive)
        {
            return Ok(());
        }
        self.sessions.remove(&account);
        let s = session::connect(&c.host, &c.user, &c.auth)?;
        self.sessions.insert(account, s);
        Ok(())
    }

    fn folders(&mut self, account: i64) -> Result<Vec<RemoteFolder>, String> {
        self.session(account)?.folders()
    }

    fn folder_meta(&mut self, account: i64, folder: &str) -> Result<FolderMeta, String> {
        self.session(account)?.select(folder)
    }

    fn fetch(&mut self, account: i64, folder: &str, from: u32) -> Result<Vec<RemoteMail>, String> {
        self.session(account)?.fetch_from(folder, from)
    }

    fn fetch_uids(
        &mut self,
        account: i64,
        folder: &str,
        uids: &[u32],
    ) -> Result<Vec<RemoteMail>, String> {
        if uids.is_empty() {
            return Ok(Vec::new());
        }
        self.session(account)?.fetch_set(folder, &seq_set(uids))
    }

    fn uids(&mut self, account: i64, folder: &str, which: UidSet) -> Result<HashSet<u32>, String> {
        self.session(account)?.uids(folder, which)
    }

    fn disconnect(&mut self, account: i64) -> Result<(), String> {
        // Removed first: whatever `LOGOUT` says, this world is done with the
        // session, and dropping it closes the socket.
        match self.sessions.remove(&account) {
            Some(mut s) => s.logout(),
            None => Ok(()),
        }
    }

    fn idle(&mut self, account: i64, folder: &str, window: Duration) -> Result<Watched, String> {
        self.session(account)?.idle(folder, window)
    }

    fn move_uid(
        &mut self,
        account: i64,
        from: &str,
        to: &str,
        uid: u32,
    ) -> Result<Option<u32>, String> {
        self.session(account)?.move_uid(from, to, uid)
    }

    fn store_flag(
        &mut self,
        account: i64,
        folder: &str,
        uid: u32,
        flag: MailFlag,
        on: bool,
    ) -> Result<(), String> {
        self.session(account)?.store_flag(folder, uid, flag, on)
    }

    fn append(&mut self, account: i64, folder: &str, raw: &[u8]) -> Result<(), String> {
        self.session(account)?.append(folder, raw)
    }
}

impl Smtp for RealServers {
    fn submit(&mut self, c: &Creds, m: &Outgoing) -> Result<Vec<u8>, String> {
        use lettre::transport::smtp::authentication::{Credentials, Mechanism};
        use lettre::{SmtpTransport, Transport};
        let s = |e: &dyn std::fmt::Display| format!("{e}");
        let msg = rfc822(&c.user, m)?;
        let raw = msg.formatted();
        let mut relay = SmtpTransport::relay(&c.host)
            .map_err(|e| s(&e))?
            .credentials(Credentials::new(c.user.clone(), c.secret().to_string()));
        // A bearer token is not a password: offered PLAIN, Gmail's SMTP
        // rejects it. Pin the mechanism rather than letting lettre pick by
        // what the server advertises.
        if c.auth.is_bearer() {
            relay = relay.authentication(vec![Mechanism::Xoauth2]);
        }
        relay.build().send(&msg).map_err(|e| s(&e))?;
        Ok(raw)
    }
}

/// The RFC 822 message a draft goes out as. `In-Reply-To` names the parent a
/// reply answers; `References` carries whatever chain the draft has — a
/// reply's parent and what it referenced, a forward's source and what *it*
/// referenced — so both thread for anyone who already has the conversation. A
/// forward names no parent: it is not a reply.
///
/// A letter with parts is a `multipart/mixed` with the text first, which is
/// the shape every reader understands.
///
/// # Errors
///
/// If an address does not parse, or lettre refuses the body.
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
    // With parts it is a `multipart/mixed`: the letter first, then each file
    // as its own `Content-Disposition: attachment`. A type lettre will not
    // parse falls back to the one every reader accepts rather than failing
    // the send over a label.
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

// -- the grant -------------------------------------------------------------

/// The Google grant, with the cache that keeps a refresh from happening per
/// connect. Per-process and never written down: a token is worth an hour.
pub struct RealOAuth {
    /// Where the store lives: the client registration sits beside it (see
    /// [`oauth::Client::load`]). `None` for an in-memory run, which then has
    /// no Gmail either.
    dir: Option<PathBuf>,
    secrets: MemSecrets,
    clock: ClockSource,
    /// Access tokens by address, with the unix second each expires at.
    tokens: HashMap<String, (String, f64)>,
}

impl RealOAuth {
    #[must_use]
    pub fn new(dir: Option<PathBuf>, secrets: MemSecrets, clock: ClockSource) -> RealOAuth {
        RealOAuth {
            dir,
            secrets,
            clock,
            tokens: HashMap::new(),
        }
    }
}

impl OAuth for RealOAuth {
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
        let client = oauth::Client::load(&dir)?;
        let refresh = self
            .secrets
            .get(&oauth::refresh_key(email))
            .ok_or_else(|| format!("{email} has no google grant — sign in again"))?;
        let (tok, until) = oauth::refresh(&client, oauth::GOOGLE, &refresh, now)?;
        self.tokens.insert(email.to_string(), (tok.clone(), until));
        Ok(tok)
    }
}

// -- the roles a server advertises -----------------------------------------

/// Which of the five roles a mailbox plays, from its name and its RFC 6154
/// special-use attributes (rendered — see the caller). The second half is
/// whether the archive role is being played by an all-mail view.
///
/// `\All` is the Gmail case, and it is why archive is not just `\Archive`:
/// Gmail advertises no archive mailbox, because archiving there *is* dropping
/// the inbox label, leaving the message in All Mail — which is exactly what a
/// MOVE into it does. A real `\Archive` wins where a server has one (fastmail
/// does), and `\All` is the fallback.
///
/// `\Junk` is spelled `spam` here, which is what every mail client and every
/// server that is not the RFC calls it — and what the panel is titled.
#[must_use]
pub fn role_for(name: &str, attrs: &[String]) -> (Option<String>, bool) {
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

/// A sorted uid list as an IMAP sequence set, runs collapsed: `1:200` where
/// the batch is consecutive, `4,9:11` where the server's deletions left
/// holes. A backfill batch is one command either way, and a folder whose
/// past is intact costs eleven characters rather than a kilobyte of commas.
fn seq_set(uids: &[u32]) -> String {
    let mut out = String::new();
    let mut i = 0;
    while i < uids.len() {
        let lo = uids[i];
        while i + 1 < uids.len() && uids[i + 1] == uids[i] + 1 {
            i += 1;
        }
        let hi = uids[i];
        if !out.is_empty() {
            out.push(',');
        }
        if hi > lo {
            out.push_str(&format!("{lo}:{hi}"));
        } else {
            out.push_str(&lo.to_string());
        }
        i += 1;
    }
    out
}

/// The `imap` crate, wrapped. Stateful (a selected mailbox), so `ensure`
/// suppresses redundant SELECTs — that optimisation stays private.
mod session {
    use super::{Auth, FolderMeta, MailFlag, RemoteFolder, RemoteMail, UidSet, Watched};
    use std::collections::HashSet;
    use std::time::Duration;

    use imap::extensions::idle::WaitOutcome;
    use imap::types::UnsolicitedResponse;

    type ImapSession = imap::Session<Box<dyn imap::ImapConnection>>;

    pub struct Imap {
        session: ImapSession,
        selected: Option<String>,
        /// Whether this server offers `IDLE`, asked once. `CAPABILITY` is a
        /// round trip, and the answer does not change inside a session.
        idle: Option<bool>,
    }

    fn s<E: std::fmt::Display>(e: E) -> String {
        format!("{e}")
    }

    /// The IMAP keyword for "passed on" (registered in RFC 5788's list):
    /// what Apple Mail, Thunderbird, Fastmail and Dovecot set and read.
    pub(super) const FORWARDED: &str = "$Forwarded";

    /// Whether a folder's `PERMANENTFLAGS` let the keyword be kept: the
    /// keyword itself, or `\*` (any keyword). An empty list is *not* support
    /// — the crate hands back the same empty list for `PERMANENTFLAGS ()` (a
    /// folder that keeps nothing, an EXAMINEd one) and for a server that sent
    /// no such response at all, and only the second could be read as "all
    /// flags are permanent" (RFC 3501 §7.1). Between a mark kept local on a
    /// server that said nothing and a mark taken and forgotten by one that
    /// said `()`, keep it local.
    pub(super) fn keeps_keywords(permanent: &[imap::types::Flag<'_>]) -> bool {
        use imap::types::Flag;
        permanent.iter().any(|f| match f {
            Flag::MayCreate => true,
            Flag::Custom(k) => k.eq_ignore_ascii_case(FORWARDED),
            _ => false,
        })
    }

    /// Whether a remark the server made while idling is worth ending the
    /// wait for. `false` keeps waiting — the callback's sense is the crate's.
    ///
    /// `EXISTS` and `RECENT` are mail arriving, `EXPUNGE` is mail going, and
    /// `BYE` is the session ending, which the next round trip must find out
    /// about anyway. A `FETCH` is a flag: another client marked something
    /// read, or *this* app just did — its own `STORE` comes back on this
    /// connection — and a pass for that would be a pull per mark. The
    /// interval carries flags, as it did before there was a watch.
    pub(super) fn worth_a_pass(r: &UnsolicitedResponse) -> bool {
        matches!(
            r,
            UnsolicitedResponse::Exists(_)
                | UnsolicitedResponse::Recent(_)
                | UnsolicitedResponse::Expunge(_)
                | UnsolicitedResponse::Bye { .. }
        )
    }

    /// The SASL exchange for `AUTHENTICATE XOAUTH2`.
    ///
    /// Two challenges, not one, and they mean opposite things. The first is
    /// empty: the server's invitation, answered with the envelope (the crate
    /// base64s what `process` returns, so this hands it over in the clear).
    /// Any **second** challenge is Google saying no, and it carries the
    /// reason as base64 JSON — the protocol then wants an *empty* response to
    /// acknowledge it, after which the server sends the tagged `NO`.
    /// Answering that one with the envelope again, as a single-shot
    /// authenticator does, throws the reason away and leaves the human
    /// holding "no response [AUTHENTICATION FAILED]".
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
                return super::oauth::xoauth2(&self.user, &self.token);
            }
            *self.refused.borrow_mut() = Some(String::from_utf8_lossy(challenge).into_owned());
            String::new()
        }
    }

    /// Opens a session: `LOGIN` with a password, SASL `XOAUTH2` with a token.
    ///
    /// # Errors
    ///
    /// If the server is unreachable or refuses the credentials.
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
                            Some(w) => format!("{}: {}", s(e), super::oauth::refusal(&w)),
                            None => s(e),
                        });
                    }
                }
            }
        };
        Ok(Imap {
            session,
            selected: None,
            idle: None,
        })
    }

    impl Imap {
        /// `LOGOUT`, so the server is told rather than left to time the
        /// connection out itself.
        pub fn logout(&mut self) -> Result<(), String> {
            self.session.logout().map_err(s)
        }

        pub fn select(&mut self, name: &str) -> Result<FolderMeta, String> {
            let mb = self.session.select(name).map_err(s)?;
            self.selected = Some(name.to_string());
            Ok(FolderMeta {
                uidvalidity: mb.uid_validity.unwrap_or(0),
                uidnext: mb.uid_next.unwrap_or(1),
                keywords: keeps_keywords(&mb.permanent_flags),
            })
        }

        /// Whether the server still has this session. A dead one answers
        /// with an error rather than a lie, which is what makes reuse safe.
        pub fn alive(&mut self) -> bool {
            self.session.noop().is_ok()
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
                let attrs: Vec<String> = n.attributes().iter().map(|a| format!("{a:?}")).collect();
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
            self.fetch_set(name, &format!("{from}:*"))
        }

        /// One `UID FETCH` over any sequence set — `12:*` for new mail,
        /// `1:200` and its like for the backfill.
        ///
        /// `BODY.PEEK[]` rather than `RFC822`, which is the same bytes
        /// without the `\Seen` a plain fetch sets: mirroring a folder is
        /// not reading it, and a backfill that walked a mailbox's whole
        /// past would otherwise mark every unread letter in it read — on
        /// the server, for every client the person owns.
        pub fn fetch_set(&mut self, name: &str, set: &str) -> Result<Vec<RemoteMail>, String> {
            self.ensure(name)?;
            let fetches = self
                .session
                .uid_fetch(set, "(UID FLAGS BODY.PEEK[])")
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
                        matches!(fl, imap::types::Flag::Custom(k)
                            if k.eq_ignore_ascii_case(FORWARDED))
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

        /// One `IDLE`, at most `window` long. The selected mailbox is what
        /// the server reports on, so the folder is selected first and stays
        /// selected after — the next fetch on this session skips its own
        /// `SELECT`.
        pub fn idle(&mut self, folder: &str, window: Duration) -> Result<Watched, String> {
            if !self.offers_idle()? {
                return Ok(Watched::Unsupported);
            }
            self.ensure(folder)?;
            let mut handle = self.session.idle();
            // Ours, not the crate's: it re-issues in the background and
            // never comes back, and a wait that never returns is a thread
            // that cannot notice it has been retired.
            handle.timeout(window).keepalive(false);
            let outcome = handle.wait_while(|r| !worth_a_pass(&r)).map_err(s)?;
            Ok(match outcome {
                WaitOutcome::MailboxChanged => Watched::Changed,
                WaitOutcome::TimedOut => Watched::Quiet,
            })
        }

        fn offers_idle(&mut self) -> Result<bool, String> {
            if let Some(known) = self.idle {
                return Ok(known);
            }
            let yes = self.session.capabilities().map_err(s)?.has_str("IDLE");
            self.idle = Some(yes);
            Ok(yes)
        }

        pub fn move_uid(
            &mut self,
            from: &str,
            to: &str,
            uid: u32,
        ) -> Result<Option<u32>, String> {
            self.ensure(from)?;
            self.session.uid_mv(uid.to_string(), to).map_err(s)?;
            // The crate acks the MOVE but does not surface COPYUID; the new
            // uid arrives via Message-ID adoption on the next fetch.
            Ok(None)
        }

        pub fn store_flag(
            &mut self,
            folder: &str,
            uid: u32,
            flag: MailFlag,
            on: bool,
        ) -> Result<(), String> {
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

    /// The five roles, off the names and the special-use attributes a server
    /// advertises. Gmail's all-mail view plays archive and says so.
    #[test]
    fn a_servers_folders_take_their_roles() {
        let a = |v: &[&str]| v.iter().map(|s| (*s).to_string()).collect::<Vec<_>>();
        assert_eq!(role_for("INBOX", &[]), (Some("inbox".into()), false));
        assert_eq!(role_for("inbox", &[]), (Some("inbox".into()), false));
        assert_eq!(
            role_for("Archive", &a(&["Archive"])),
            (Some("archive".into()), false)
        );
        // Gmail: `\All` plays archive, and the caller must not ingest from it.
        assert_eq!(
            role_for("[Gmail]/All Mail", &a(&["All", "HasNoChildren"])),
            (Some("archive".into()), true)
        );
        // A server with both: the real archive wins and nothing is skipped.
        assert_eq!(
            role_for("Archive", &a(&["All", "Archive"])),
            (Some("archive".into()), false)
        );
        assert_eq!(role_for("Sent", &a(&["Sent"])), (Some("sent".into()), false));
        assert_eq!(role_for("Junk", &a(&["Junk"])), (Some("spam".into()), false));
        assert_eq!(
            role_for("Trash", &a(&["Trash"])),
            (Some("trash".into()), false)
        );
        // Anything else is not mirrored — and an extension that merely
        // spells one of the words is not one of them.
        assert_eq!(role_for("Notes", &[]), (None, false));
        assert_eq!(
            role_for("Notes", &a(&["Extension(\"Sent\")"])),
            (None, false)
        );
    }

    /// A backfill batch is one command: consecutive uids collapse to a
    /// range, and the holes a server's deletions left are named beside it.
    #[test]
    fn a_uid_batch_is_one_sequence_set() {
        assert_eq!(seq_set(&[]), "");
        assert_eq!(seq_set(&[7]), "7");
        assert_eq!(seq_set(&[1, 2, 3, 4]), "1:4");
        assert_eq!(seq_set(&[1, 2, 4, 9, 10, 11, 20]), "1:2,4,9:11,20");
    }

    /// A folder keeps the keyword when its `PERMANENTFLAGS` name it or allow
    /// any. Silence is not support: the crate answers the same empty list for
    /// a server that said nothing and one that said `()`.
    #[test]
    fn keywords_are_kept_only_where_the_server_says_so() {
        use imap::types::Flag;
        assert!(session::keeps_keywords(&[Flag::MayCreate]));
        assert!(session::keeps_keywords(&[Flag::Custom("$Forwarded".into())]));
        assert!(session::keeps_keywords(&[Flag::Custom("$forwarded".into())]));
        assert!(!session::keeps_keywords(&[]));
        assert!(!session::keeps_keywords(&[Flag::Seen, Flag::Deleted]));
    }

    /// What ends a wait, and what does not. Mail arriving or going is worth
    /// a pass; a flag is not — the `STORE` this app just pushed comes back
    /// on the watch's own connection, and a pull for each would be one per
    /// mark.
    #[test]
    fn a_wait_ends_on_mail_and_not_on_a_flag() {
        use imap::types::UnsolicitedResponse as Said;
        assert!(session::worth_a_pass(&Said::Exists(3)));
        assert!(session::worth_a_pass(&Said::Recent(1)));
        assert!(session::worth_a_pass(&Said::Expunge(2)));
        assert!(session::worth_a_pass(&Said::Bye {
            code: None,
            information: None
        }));
        assert!(!session::worth_a_pass(&Said::Fetch {
            id: 4,
            attributes: Vec::new()
        }));
        assert!(!session::worth_a_pass(&Said::Flags(Vec::new())));
    }

    /// The message a draft goes out as: the threading headers a reply and a
    /// forward carry, and the chain deduped.
    #[test]
    fn a_draft_goes_out_with_its_chain() {
        let m = Outgoing {
            to: "vera@kovac.io".into(),
            subject: "Re: Q3".into(),
            body: "yes".into(),
            in_reply_to: Some("a@x".into()),
            references: vec!["a@x".into(), "<a@x>".into(), "b@x".into()],
            attachments: Vec::new(),
        };
        let msg = rfc822("me@prepor.dev", &m).expect("a message");
        let raw = String::from_utf8_lossy(&msg.formatted()).into_owned();
        assert!(raw.contains("To: vera@kovac.io"), "{raw}");
        assert!(raw.contains("In-Reply-To: <a@x>"), "{raw}");
        assert!(raw.contains("References: <a@x> <b@x>"), "{raw}");
        assert!(raw.trim_end().ends_with("yes"), "{raw}");

        // A forward names no parent: it is not a reply.
        let fwd = Outgoing {
            to: "max@ivanov.dev".into(),
            subject: "Fwd: Q3".into(),
            body: "fyi".into(),
            in_reply_to: None,
            references: vec!["a@x".into()],
            attachments: Vec::new(),
        };
        let raw = String::from_utf8_lossy(&rfc822("me@prepor.dev", &fwd).unwrap().formatted())
            .into_owned();
        assert!(!raw.contains("In-Reply-To"), "{raw}");
        assert!(raw.contains("References: <a@x>"), "{raw}");

        // An address that is not one is a failure, not a silent send.
        let bad = Outgoing {
            to: "not an address".into(),
            ..Outgoing::default()
        };
        assert!(rfc822("me@prepor.dev", &bad).is_err());
    }

    /// A letter that carries something goes out as a `multipart/mixed`: the
    /// text first, then each file under its own disposition and media type.
    /// A type lettre cannot parse falls back rather than failing the send.
    #[test]
    fn a_letter_with_parts_goes_out_as_a_multipart() {
        let m = Outgoing {
            to: "vera@kovac.io".into(),
            subject: "the budget".into(),
            body: "attached".into(),
            attachments: vec![
                Part {
                    name: "q3-budget.csv".into(),
                    mime: "text/csv".into(),
                    bytes: b"line,aug\ncdn,640\n".to_vec(),
                },
                Part {
                    name: "odd".into(),
                    mime: "not a media type".into(),
                    bytes: vec![0, 1, 2],
                },
            ],
            ..Outgoing::default()
        };
        let raw = String::from_utf8_lossy(&rfc822("me@prepor.dev", &m).unwrap().formatted())
            .into_owned();
        assert!(raw.contains("multipart/mixed"), "{raw}");
        assert!(raw.contains("Content-Type: text/csv"), "{raw}");
        assert!(
            raw.contains("filename=\"q3-budget.csv\""),
            "{raw}"
        );
        assert!(
            raw.contains("application/octet-stream"),
            "an unparseable type falls back: {raw}"
        );
        // The text is a part of its own and still first.
        let text = raw.find("attached").expect("the letter");
        let file = raw.find("q3-budget.csv").expect("the part");
        assert!(text < file, "{raw}");
    }
}
