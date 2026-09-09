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
use std::time::Duration;

use kernel::app::{Capabilities, Env};
#[cfg(test)]
use kernel::caps::{MemSecrets, SecretsFactory};

#[cfg(test)]
use super::caps::Part;
use super::caps::{
    Auth, Creds, FolderMeta, Imap, MailFlag, OAuth, Outgoing, RemoteFolder, RemoteMail, Smtp,
    UidSet, Watched,
};
use super::oauth;

const CONNECT_TIMEOUT: Duration = Duration::from_secs(30);

/// Mail's real capabilities for one world. One IMAP session per account and
/// one submission transport per send. Each worker owns its sessions on the
/// local executor; [`Worker::claims`](kernel::app::Worker::claims) routes jobs
/// to the worker for that account.
pub fn install(env: &Env, caps: &mut Capabilities) {
    caps.insert::<dyn Imap>(Box::new(RealServers::default()));
    caps.insert::<dyn Smtp>(Box::new(RealServers::default()));
    caps.insert::<dyn OAuth>(Box::new(RealOAuth::new(env)));
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

#[async_trait::async_trait(?Send)]
impl Imap for RealServers {
    /// The session this account already has, if the server still has it —
    /// a `NOOP` is one round trip and says so. A pass a minute, and the
    /// batches a backfill takes, must not be a sign-in each: providers
    /// count logins, and Gmail counts them narrowly.
    async fn connect(&mut self, account: i64, c: &Creds) -> Result<(), String> {
        if let Some(session) = self.sessions.get_mut(&account) {
            if session.alive().await { return Ok(()); }
        }
        self.sessions.remove(&account);
        let s = tokio::time::timeout(CONNECT_TIMEOUT, session::connect(&c.host, &c.user, &c.auth)).await
            .map_err(|_| "IMAP connection timed out".to_string())??;
        self.sessions.insert(account, s);
        Ok(())
    }

    async fn folders(&mut self, account: i64) -> Result<Vec<RemoteFolder>, String> {
        self.session(account)?.folders().await
    }

    async fn folder_meta(&mut self, account: i64, folder: &str) -> Result<FolderMeta, String> {
        self.session(account)?.select(folder).await
    }

    async fn fetch(&mut self, account: i64, folder: &str, from: u32) -> Result<Vec<RemoteMail>, String> {
        self.session(account)?.fetch_from(folder, from).await
    }

    async fn fetch_uids(
        &mut self,
        account: i64,
        folder: &str,
        uids: &[u32],
    ) -> Result<Vec<RemoteMail>, String> {
        if uids.is_empty() {
            return Ok(Vec::new());
        }
        self.session(account)?.fetch_set(folder, &seq_set(uids)).await
    }

    async fn part(
        &mut self,
        account: i64,
        folder: &str,
        uidvalidity: u32,
        uid: u32,
        section: &str,
    ) -> Result<Vec<u8>, String> {
        let session = self.session(account)?;
        if session.select(folder).await?.uidvalidity != uidvalidity {
            return Err("mailbox changed; sync before downloading this attachment".into());
        }
        session.section(uid, section).await
    }

    async fn uids(&mut self, account: i64, folder: &str, which: UidSet) -> Result<HashSet<u32>, String> {
        self.session(account)?.uids(folder, which).await
    }

    async fn disconnect(&mut self, account: i64) -> Result<(), String> {
        // Removed first: whatever `LOGOUT` says, this world is done with the
        // session, and dropping it closes the socket.
        match self.sessions.remove(&account) {
            Some(mut s) => s.logout().await,
            None => Ok(()),
        }
    }

    async fn idle(&mut self, account: i64, folder: &str, window: Duration, retirement: &kernel::app::Retirement) -> Result<Watched, String> {
        self.session(account)?.idle(folder, window, retirement).await
    }

    async fn move_uid(
        &mut self,
        account: i64,
        from: &str,
        to: &str,
        uid: u32,
    ) -> Result<Option<u32>, String> {
        self.session(account)?.move_uid(from, to, uid).await
    }

    async fn store_flag(
        &mut self,
        account: i64,
        folder: &str,
        uid: u32,
        flag: MailFlag,
        on: bool,
    ) -> Result<(), String> {
        self.session(account)?.store_flag(folder, uid, flag, on).await
    }

    async fn append(&mut self, account: i64, folder: &str, raw: &[u8]) -> Result<(), String> {
        self.session(account)?.append(folder, raw).await
    }
}

#[async_trait::async_trait(?Send)]
impl Smtp for RealServers {
    async fn submit(&mut self, c: &Creds, m: &Outgoing) -> Result<Vec<u8>, String> {
        use lettre::transport::smtp::authentication::{Credentials, Mechanism};
        use lettre::{AsyncSmtpTransport, AsyncTransport, Tokio1Executor};
        let s = |e: &dyn std::fmt::Display| format!("{e}");
        let (from, outgoing) = (c.user.clone(), m.clone());
        let (msg, raw) = kernel::runtime::spawn_blocking(move || {
            let message = rfc822(&from, &outgoing)?;
            let raw = message.formatted();
            Ok::<_, String>((message, raw))
        }).await.map_err(|e| e.to_string())??;
        let mut relay = AsyncSmtpTransport::<Tokio1Executor>::relay(&c.host)
            .map_err(|e| s(&e))?
            .credentials(Credentials::new(c.user.clone(), c.secret().to_string()));
        // A bearer token is not a password: offered PLAIN, Gmail's SMTP
        // rejects it. Pin the mechanism rather than letting lettre pick by
        // what the server advertises.
        if c.auth.is_bearer() {
            relay = relay.authentication(vec![Mechanism::Xoauth2]);
        }
        relay.build().send(msg).await.map_err(|e| s(&e))?;
        Ok(raw)
    }
}

/// The RFC 822 message a draft goes out as. The recipients are the TO field
/// split on commas, as the sheet offers them. `In-Reply-To` names the parent
/// a reply answers; `References` carries whatever chain the draft has — a
/// reply's parent and what it referenced, a forward's source and what *it*
/// referenced — so both thread for anyone who already has the conversation. A
/// forward names no parent: it is not a reply.
///
/// A letter with parts is a `multipart/mixed` with the text first, which is
/// the shape every reader understands.
///
/// # Errors
///
/// If there is no recipient, if an address does not parse, or if lettre
/// refuses the body.
pub fn rfc822(from: &str, m: &Outgoing) -> Result<lettre::Message, String> {
    use lettre::message::{header, Mailboxes};
    use lettre::Message;
    let s = |e: &dyn std::fmt::Display| format!("{e}");
    let bracket = |id: &str| {
        format!(
            "<{}>",
            id.trim().trim_start_matches('<').trim_end_matches('>')
        )
    };
    // The TO field holds a *list* — its completion is comma-separated, and a
    // reply to one's own letter is prefilled with every address that letter
    // went to. It is read as a header is read rather than cut on commas: a
    // display name may carry one of its own (`"Doe, Jane" <jane@x>` is one
    // recipient, not two), and only the parser knows which comma separates.
    // All of them land in one `To`, which is also the envelope.
    if m.to.trim().is_empty() {
        return Err("no recipient".into());
    }
    let to: Mailboxes = m.to.parse().map_err(|e| s(&e))?;
    if to.iter().next().is_none() {
        return Err("no recipient".into());
    }
    let mut b = Message::builder()
        .from(from.parse().map_err(|e| s(&e))?)
        .mailbox(header::To::from(to))
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

pub use crate::identity::Tokens as RealOAuth;
#[async_trait::async_trait(?Send)]
impl OAuth for RealOAuth {
    async fn access_token(&mut self,email:&str)->Result<String,String>{self.access(email).await}
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
/// holes. A UID set fits one command either way, and a folder whose
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
    use std::collections::{BTreeMap, HashMap, HashSet};
    use std::time::Duration;

    use futures_util::TryStreamExt;
    use async_imap::extensions::idle::IdleResponse;


    trait Connection: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin + Send + std::fmt::Debug {}
    impl<T: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin + Send + std::fmt::Debug> Connection for T {}
    /// IMAP commands end at their tagged completion; closing a persistent
    /// connection cannot finish a command or prove that NOOP succeeded.
    #[derive(Debug)]
    struct ImapIo<S>(S);

    impl<S: tokio::io::AsyncRead + Unpin> tokio::io::AsyncRead for ImapIo<S> {
        fn poll_read(mut self: std::pin::Pin<&mut Self>, cx: &mut std::task::Context<'_>, buf: &mut tokio::io::ReadBuf<'_>) -> std::task::Poll<std::io::Result<()>> {
            let before = buf.filled().len();
            let available = buf.remaining();
            match std::pin::Pin::new(&mut self.0).poll_read(cx, buf) {
                std::task::Poll::Ready(Ok(())) if available > 0 && buf.filled().len() == before => std::task::Poll::Ready(Err(std::io::Error::new(std::io::ErrorKind::UnexpectedEof, "IMAP connection closed before command completion"))),
                result => result,
            }
        }
    }

    impl<S: tokio::io::AsyncWrite + Unpin> tokio::io::AsyncWrite for ImapIo<S> {
        fn poll_write(mut self: std::pin::Pin<&mut Self>, cx: &mut std::task::Context<'_>, bytes: &[u8]) -> std::task::Poll<std::io::Result<usize>> { std::pin::Pin::new(&mut self.0).poll_write(cx, bytes) }
        fn poll_flush(mut self: std::pin::Pin<&mut Self>, cx: &mut std::task::Context<'_>) -> std::task::Poll<std::io::Result<()>> { std::pin::Pin::new(&mut self.0).poll_flush(cx) }
        fn poll_shutdown(mut self: std::pin::Pin<&mut Self>, cx: &mut std::task::Context<'_>) -> std::task::Poll<std::io::Result<()>> { std::pin::Pin::new(&mut self.0).poll_shutdown(cx) }
    }

    fn client(stream: impl Connection + 'static) -> async_imap::Client<Box<dyn Connection>> {
        async_imap::Client::new(Box::new(ImapIo(stream)) as Box<dyn Connection>)
    }

    type ImapSession = async_imap::Session<Box<dyn Connection>>;

    pub struct Imap {
        session: Option<ImapSession>,
        selected: Option<String>,
        /// Whether this server offers `IDLE`, asked once. `CAPABILITY` is a
        /// round trip, and the answer does not change inside a session.
        idle: Option<bool>,
    }

    fn s<E: std::fmt::Display>(e: E) -> String {
        format!("{e}")
    }

    fn section_path(section: &str) -> Result<imap_proto::types::SectionPath, String> {
        let ids: Vec<u32> = section
            .split('.')
            .map(|p| p.parse::<u32>())
            .collect::<Result<_, _>>()
            .map_err(|_| "invalid MIME section")?;
        if ids.is_empty() || ids.contains(&0) {
            return Err("invalid MIME section".into());
        }
        Ok(imap_proto::types::SectionPath::Part(ids, None))
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
    pub(super) fn keeps_keywords(permanent: &[async_imap::types::Flag<'_>]) -> bool {
        use async_imap::types::Flag;
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
    pub(super) fn worth_a_pass(r: &imap_proto::Response<'_>) -> bool {
        use imap_proto::{Response, MailboxDatum, Status};
        matches!(r, Response::MailboxData(MailboxDatum::Exists(_) | MailboxDatum::Recent(_)) | Response::Expunge(_) | Response::Data { status: Status::Bye, .. })
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

    impl async_imap::Authenticator for &mut XOAuth2 {
        type Response = String;
        fn process(&mut self, challenge: &[u8]) -> String {
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
    pub async fn connect(host: &str, user: &str, auth: &Auth) -> Result<Imap, String> {
        let roots = rustls::RootCertStore::from_iter(webpki_roots::TLS_SERVER_ROOTS.iter().cloned());
        let config = rustls::ClientConfig::builder_with_provider(std::sync::Arc::new(rustls::crypto::ring::default_provider()))
            .with_safe_default_protocol_versions().map_err(s)?.with_root_certificates(roots).with_no_client_auth();
        let name = rustls::pki_types::ServerName::try_from(host.to_owned()).map_err(s)?;
        let socket = tokio::net::TcpStream::connect((host, 993)).await.map_err(s)?;
        let socket = tokio_rustls::TlsConnector::from(std::sync::Arc::new(config)).connect(name, socket).await.map_err(s)?;
        let mut client = client(socket);
        client.read_response().await.map_err(s)?.ok_or("IMAP server closed before greeting")?;
        let session = match auth {
            Auth::Password(pass) => client.login(user, pass).await.map_err(|e| s(e.0))?,
            Auth::Bearer(token) => {
                let mut sasl = XOAuth2 {
                    user: user.to_string(),
                    token: token.clone(),
                    refused: std::cell::RefCell::new(None),
                };
                match client.authenticate("XOAUTH2", &mut sasl).await {
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
            session: Some(session),
            selected: None,
            idle: None,
        })
    }

    impl Imap {
        fn session(&mut self) -> Result<&mut ImapSession, String> {
            self.session.as_mut().ok_or_else(|| "IMAP session closed".to_string())
        }
        /// `LOGOUT`, so the server is told rather than left to time the
        /// connection out itself.
        pub async fn logout(&mut self) -> Result<(), String> {
            self.session()?.logout().await.map_err(s)
        }

        pub async fn select(&mut self, name: &str) -> Result<FolderMeta, String> {
            let mb = self.session()?.select(name).await.map_err(s)?;
            self.selected = Some(name.to_string());
            Ok(FolderMeta {
                uidvalidity: mb.uid_validity.unwrap_or(0),
                uidnext: mb.uid_next.unwrap_or(1),
                keywords: keeps_keywords(&mb.permanent_flags),
            })
        }

        /// Whether the server still has this session. A dead one answers
        /// with an error rather than a lie, which is what makes reuse safe.
        pub async fn alive(&mut self) -> bool {
            let Some(mut session) = self.session.take() else { return false; };
            // Reusing a connection has the same finite setup budget as a
            // new connection. An unanswered read-only NOOP cannot hold worker
            // retirement forever. Own the stream until its tagged completion:
            // a timed-out or cancelled probe must never leave it reusable.
            let alive = matches!(
                tokio::time::timeout(super::CONNECT_TIMEOUT, session.noop()).await,
                Ok(Ok(()))
            );
            if alive {
                self.session = Some(session);
            } else {
                self.selected = None;
                self.idle = None;
            }
            alive
        }

        async fn ensure(&mut self, name: &str) -> Result<(), String> {
            if self.selected.as_deref() != Some(name) {
                self.select(name).await?;
            }
            Ok(())
        }

        pub async fn folders(&mut self) -> Result<Vec<RemoteFolder>, String> {
            let names = self.session()?.list(Some(""), Some("*")).await.map_err(s)?.try_collect::<Vec<_>>().await.map_err(s)?;
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

        pub async fn fetch_from(&mut self, name: &str, from: u32) -> Result<Vec<RemoteMail>, String> {
            self.fetch_set(name, &format!("{from}:*")).await
        }

        /// Fetch the envelope and MIME structure, then only reading sections.
        /// PEEK keeps both mirroring and attachment downloads from setting Seen.
        pub async fn fetch_set(&mut self, name: &str, set: &str) -> Result<Vec<RemoteMail>, String> {
            use super::super::content::FetchPlan;
            self.ensure(name).await?;
            let fetches = self
                .session()?
                .uid_fetch(set, "(UID FLAGS BODYSTRUCTURE BODY.PEEK[HEADER])").await
                .map_err(s)?.try_collect::<Vec<_>>().await.map_err(s)?;
            let mut groups: BTreeMap<Vec<String>, Vec<(RemoteMail, FetchPlan)>> = BTreeMap::new();
            for f in fetches.iter() {
                let Some(uid) = f.uid else { continue };
                let (Some(header), Some(structure)) = (f.header(), f.bodystructure()) else {
                    eprintln!(
                        "mail: skipped uid {uid} in {name}: missing headers or MIME structure"
                    );
                    continue;
                };
                let plan = match FetchPlan::new(header, structure) {
                    Ok(plan) => plan,
                    Err(e) => {
                        eprintln!("mail: skipped uid {uid} in {name}: {e}");
                        continue;
                    }
                };
                let unread = !f
                    .flags()
                    .any(|fl| matches!(fl, async_imap::types::Flag::Seen));
                let forwarded = f.flags().any(|fl| {
                    matches!(fl, async_imap::types::Flag::Custom(k)
                            if k.eq_ignore_ascii_case(FORWARDED))
                });
                groups.entry(plan.readings.clone()).or_default().push((
                    RemoteMail {
                        uid,
                        unread,
                        forwarded,
                        raw: Vec::new(),
                    },
                    plan,
                ));
            }
            let mut out = Vec::new();
            for (readings, group) in groups {
                let mut bodies: HashMap<u32, HashMap<String, Vec<u8>>> = HashMap::new();
                if !readings.is_empty() {
                    let paths = match readings
                        .iter()
                        .map(|p| section_path(p))
                        .collect::<Result<Vec<_>, _>>()
                    {
                        Ok(paths) => paths,
                        Err(e) => {
                            eprintln!("mail: skipped MIME sections in {name}: {e}");
                            continue;
                        }
                    };
                    // Messages with the same reading sections share one
                    // command, even when their attachment layouts differ.
                    let mut uids: Vec<_> = group.iter().map(|(m, _)| m.uid).collect();
                    uids.sort_unstable();
                    uids.dedup();
                    let requested: HashSet<_> = uids.iter().copied().collect();
                    let query = format!(
                        "(UID {})",
                        readings
                            .iter()
                            .map(|p| format!("BODY.PEEK[{p}]"))
                            .collect::<Vec<_>>()
                            .join(" ")
                    );
                    // Transport/command failures still fail the fetch; bad
                    // content and omitted sections affect only their UID.
                    let replies = self
                        .session()?
                        .uid_fetch(super::seq_set(&uids), query).await
                        .map_err(s)?.try_collect::<Vec<_>>().await.map_err(s)?;
                    for reply in replies.iter() {
                        let Some(uid) = reply.uid.filter(|uid| requested.contains(uid)) else {
                            continue;
                        };
                        for (section, path) in readings.iter().zip(&paths) {
                            if let Some(bytes) = reply.section(path) {
                                bodies
                                    .entry(uid)
                                    .or_default()
                                    .insert(section.clone(), bytes.to_vec());
                            }
                        }
                    }
                }
                let empty = HashMap::new();
                for (mut mail, plan) in group {
                    match plan.finish(bodies.get(&mail.uid).unwrap_or(&empty)) {
                        Ok(raw) => {
                            mail.raw = raw;
                            out.push(mail);
                        }
                        Err(e) => eprintln!("mail: skipped uid {} in {name}: {e}", mail.uid),
                    }
                }
            }
            out.sort_by_key(|m| m.uid);
            Ok(out)
        }

        pub async fn section(&mut self, uid: u32, section: &str) -> Result<Vec<u8>, String> {
            let path = section_path(section)?;
            let replies = self
                .session()?
                .uid_fetch(uid.to_string(), format!("(UID BODY.PEEK[{section}])")).await
                .map_err(s)?.try_collect::<Vec<_>>().await.map_err(s)?;
            replies
                .iter()
                .filter(|f| f.uid == Some(uid))
                .find_map(|f| f.section(&path))
                .map(<[u8]>::to_vec)
                .ok_or_else(|| "attachment is no longer on the server".into())
        }

        pub async fn uids(&mut self, name: &str, which: UidSet) -> Result<HashSet<u32>, String> {
            self.ensure(name).await?;
            let query = match which {
                UidSet::All => "ALL".to_string(),
                UidSet::Unseen => "UNSEEN".to_string(),
                UidSet::Forwarded => format!("KEYWORD {FORWARDED}"),
            };
            self.session()?.uid_search(query).await.map_err(s)
        }

        /// One `IDLE`, at most `window` long. The selected mailbox is what
        /// the server reports on, so the folder is selected first and stays
        /// selected after — the next fetch on this session skips its own
        /// `SELECT`.
        pub async fn idle(&mut self, folder: &str, window: Duration, retirement: &kernel::app::Retirement) -> Result<Watched, String> {
            if retirement.requested() { return Ok(Watched::Quiet); }
            let ready = tokio::select! {
                biased;
                () = retirement.wait() => None,
                result = async {
                    if !self.offers_idle().await? { return Ok(false); }
                    self.ensure(folder).await?;
                    Ok::<_, String>(true)
                } => Some(result),
            };
            match ready {
                None => {
                    // An unfinished read-only command cannot share a session
                    // with later commands. Retirement closes this watch's link.
                    self.session = None;
                    self.selected = None;
                    return Ok(Watched::Quiet);
                }
                Some(Ok(false)) => return Ok(Watched::Unsupported),
                Some(result) => { result?; }
            }
            let session = self.session.take().ok_or("IMAP session closed")?;
            let mut handle = session.idle();
            tokio::select! {
                biased;
                () = retirement.wait() => return Ok(Watched::Quiet),
                result = tokio::time::timeout(Duration::from_secs(30), handle.init()) => {
                    result.map_err(|_| "IMAP IDLE start timed out")?.map_err(s)?;
                }
            }
            let deadline = tokio::time::Instant::now() + window;
            let outcome = loop {
                let (wait, _interrupt) = handle.wait_with_timeout(window);
                let response = tokio::select! {
                    biased;
                    () = retirement.wait() => break Ok(Watched::Quiet),
                    response = tokio::time::timeout_at(deadline, wait) => response,
                };
                match response {
                    Err(_) | Ok(Ok(IdleResponse::Timeout)) => break Ok(Watched::Quiet),
                    Ok(Ok(IdleResponse::ManualInterrupt)) => break Err("IMAP IDLE ended unexpectedly".to_string()),
                    Ok(Ok(IdleResponse::NewData(data))) => {
                        if worth_a_pass(data.parsed()) {
                            break Ok(Watched::Changed);
                        }
                    }
                    Ok(Err(e)) => break Err(s(e)),
                }
            };
            // DONE is acknowledged before the connection can serve another command.
            self.session = Some(tokio::time::timeout(Duration::from_secs(30), handle.done()).await.map_err(|_| "IMAP IDLE completion timed out")?.map_err(s)?);
            outcome
        }

        async fn offers_idle(&mut self) -> Result<bool, String> {
            if let Some(known) = self.idle {
                return Ok(known);
            }
            let yes = self.session()?.capabilities().await.map_err(s)?.has_str("IDLE");
            self.idle = Some(yes);
            Ok(yes)
        }

        pub async fn move_uid(&mut self, from: &str, to: &str, uid: u32) -> Result<Option<u32>, String> {
            self.ensure(from).await?;
            self.session()?.uid_mv(uid.to_string(), to).await.map_err(s)?;
            // The crate acks the MOVE but does not surface COPYUID; the new
            // uid arrives via Message-ID adoption on the next fetch.
            Ok(None)
        }

        pub async fn store_flag(
            &mut self,
            folder: &str,
            uid: u32,
            flag: MailFlag,
            on: bool,
        ) -> Result<(), String> {
            self.ensure(folder).await?;
            let name = match flag {
                MailFlag::Seen => "\\Seen",
                MailFlag::Forwarded => FORWARDED,
            };
            let sign = if on { '+' } else { '-' };
            self.session()?
                .uid_store(uid.to_string(), format!("{sign}FLAGS ({name})")).await
                .map_err(s)?.try_collect::<Vec<_>>().await.map_err(s)?;
            Ok(())
        }

        pub async fn append(&mut self, folder: &str, raw: &[u8]) -> Result<(), String> {
            self.session()?
                .append(folder, Some("\\Seen"), None, raw).await
                .map_err(s)?;
            Ok(())
        }
    }

    #[cfg(test)]
    mod tests {
        use super::*;
        use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
        use tokio::net::{TcpListener, TcpStream};

        const TEXT: &str = "(\"TEXT\" \"PLAIN\" NIL NIL NIL \"7BIT\" 5 1)";
        const MIXED: &str = "((\"TEXT\" \"PLAIN\" NIL NIL NIL \"7BIT\" 5 1)(\"APPLICATION\" \"OCTET-STREAM\" (\"NAME\" \"file.bin\") NIL NIL \"BASE64\" 8) \"MIXED\" (\"BOUNDARY\" \"x\"))";
        const SELECTED: &str =
            "* 1 EXISTS\r\n* OK [UIDVALIDITY 9] current\r\n* OK [UIDNEXT 204] next\r\n";

        fn metadata(uid: u32, structure: &str) -> String {
            let header = format!("From: me@example.org\r\nSubject: message {uid}\r\n\r\n");
            format!("* {uid} FETCH (UID {uid} FLAGS () BODYSTRUCTURE {structure} BODY[HEADER] {{{}}}\r\n{header})\r\n", header.len())
        }

        async fn scripted(steps: Vec<(&'static str, String)>) -> (Imap, tokio::task::JoinHandle<()>) {
            let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
            let address = listener.local_addr().unwrap();
            let server = tokio::spawn(async move {
                let (socket, _) = listener.accept().await.unwrap();
                let mut io = BufReader::new(socket);
                io.get_mut().write_all(b"* OK test server\r\n").await.unwrap();
                for (expected, reply) in
                    std::iter::once(("LOGIN \"test\" \"password\"", String::new())).chain(steps)
                {
                    let mut line = String::new();
                    io.read_line(&mut line).await.unwrap();
                    let (tag, command) = line.trim_end().split_once(' ').expect("command with tag");
                    assert_eq!(command, expected);
                    io.get_mut().write_all(format!("{reply}{tag} OK completed\r\n").as_bytes()).await.unwrap();
                }
            });
            let socket = TcpStream::connect(address).await.unwrap();
            let mut client = client(socket);
            client.read_response().await.unwrap().unwrap();
            let session = client
                .login("test", "password").await
                .map_err(|(e, _)| e)
                .unwrap();
            (
                Imap {
                    session: Some(session),
                    selected: None,
                    idle: None,
                },
                server,
            )
        }

        async fn idle_adapter(changed: bool, entered: Option<tokio::sync::oneshot::Sender<()>>) -> (Imap, tokio::task::JoinHandle<()>) {
            let (client, server) = tokio::io::duplex(4096);
            let server = tokio::spawn(async move {
                let mut io = BufReader::new(server);
                io.get_mut().write_all(b"* OK test server\r\n").await.unwrap();
                for (expected, reply) in [
                    ("LOGIN \"test\" \"password\"", ""),
                    ("CAPABILITY", "* CAPABILITY IMAP4rev1 IDLE\r\n"),
                    ("SELECT \"INBOX\"", SELECTED),
                ] {
                    let mut line = String::new();
                    io.read_line(&mut line).await.unwrap();
                    let (tag, command) = line.trim_end().split_once(' ').unwrap();
                    assert_eq!(command, expected);
                    io.get_mut().write_all(format!("{reply}{tag} OK completed\r\n").as_bytes()).await.unwrap();
                }
                let mut line = String::new();
                io.read_line(&mut line).await.unwrap();
                let (tag, command) = line.trim_end().split_once(' ').unwrap();
                assert_eq!(command, "IDLE");
                let tag = tag.to_owned();
                io.get_mut().write_all(b"+ idling\r\n* 1 FETCH (FLAGS (\\Seen))\r\n").await.unwrap();
                if changed { io.get_mut().write_all(b"* 2 EXISTS\r\n").await.unwrap(); }
                if let Some(entered) = entered { let _ = entered.send(()); }
                line.clear();
                io.read_line(&mut line).await.unwrap();
                assert_eq!(line, "DONE\r\n");
                io.get_mut().write_all(format!("{tag} OK idle finished\r\n").as_bytes()).await.unwrap();
                line.clear();
                io.read_line(&mut line).await.unwrap();
                let (tag, command) = line.trim_end().split_once(' ').unwrap();
                assert_eq!(command, "NOOP");
                io.get_mut().write_all(format!("{tag} OK alive\r\n").as_bytes()).await.unwrap();
            });
            let mut client = super::client(client);
            client.read_response().await.unwrap().unwrap();
            let session = client.login("test", "password").await.map_err(|(e, _)| e).unwrap();
            (Imap { session: Some(session), selected: None, idle: None }, server)
        }

        #[tokio::test]
        async fn idle_filters_flags_and_completes_done_before_reusing_the_connection() {
            let (mut adapter, server) = idle_adapter(true, None).await;
            assert_eq!(adapter.idle("INBOX", Duration::from_secs(1), &kernel::app::Retirement::default()).await.unwrap(), Watched::Changed);
            assert!(adapter.alive().await);
            server.await.unwrap();
        }

        #[tokio::test]
        async fn idle_expires_without_blocking_the_executor() {
            let (mut adapter, server) = idle_adapter(false, None).await;
            let pulse = tokio::spawn(async { tokio::time::sleep(Duration::from_millis(1)).await; 1 });
            assert_eq!(adapter.idle("INBOX", Duration::from_millis(10), &kernel::app::Retirement::default()).await.unwrap(), Watched::Quiet);
            assert!(pulse.is_finished(), "IDLE must yield to other tasks");
            assert_eq!(pulse.await.unwrap(), 1);
            assert!(adapter.alive().await);
            server.await.unwrap();
        }

        #[tokio::test]
        async fn retiring_an_idle_watch_sends_done_before_returning_the_live_session() {
            let (entered, started) = tokio::sync::oneshot::channel();
            let (mut adapter, server) = idle_adapter(false, Some(entered)).await;
            let retirement = kernel::app::Retirement::default();
            let result = {
                use futures_util::FutureExt;
                let idle = adapter.idle("INBOX", Duration::from_secs(5 * 60), &retirement);
                tokio::pin!(idle);
                tokio::select! {
                    result = &mut idle => panic!("the quiet server must keep IDLE pending: {result:?}"),
                    result = started => result.unwrap(),
                }
                // The server has made '+ idling' available. Consume it before
                // retiring, so this exercises DONE rather than setup cancellation.
                assert!(idle.as_mut().now_or_never().is_none());
                retirement.request();
                tokio::time::timeout(Duration::from_secs(2), idle).await
                    .expect("retirement must interrupt the five-minute passive wait").unwrap()
            };
            assert_eq!(result, Watched::Quiet);
            assert!(adapter.alive().await, "DONE must complete before the connection is reused");
            server.await.unwrap();
        }

        #[tokio::test(start_paused = true)]
        async fn unanswered_liveness_probe_expires_and_discards_the_connection() {
            use futures_util::FutureExt;
            use tokio::io::AsyncReadExt;

            let (socket, server) = tokio::io::duplex(4096);
            let (entered, started) = tokio::sync::oneshot::channel();
            let server = tokio::spawn(async move {
                let mut io = BufReader::new(server);
                io.get_mut().write_all(b"* OK test server\r\n").await.unwrap();
                let mut line = String::new();
                io.read_line(&mut line).await.unwrap();
                let (tag, command) = line.trim_end().split_once(' ').unwrap();
                assert_eq!(command, "LOGIN \"test\" \"password\"");
                io.get_mut().write_all(format!("{tag} OK logged in\r\n").as_bytes()).await.unwrap();
                line.clear();
                io.read_line(&mut line).await.unwrap();
                assert_eq!(line.trim_end().split_once(' ').unwrap().1, "NOOP");
                // An unsolicited update is not the command's tagged reply.
                io.get_mut().write_all(b"* 2 EXISTS\r\n").await.unwrap();
                entered.send(()).unwrap();
                let mut remaining = Vec::new();
                io.read_to_end(&mut remaining).await.unwrap();
                assert!(remaining.is_empty(), "the incomplete probe closes instead of issuing another command");
            });
            let mut client = super::client(socket);
            client.read_response().await.unwrap().unwrap();
            let session = client.login("test", "password").await.map_err(|(error, _)| error).unwrap();
            let mut adapter = Imap {
                session: Some(session), selected: Some("INBOX".into()), idle: Some(true),
            };
            {
                let probe = adapter.alive();
                tokio::pin!(probe);
                tokio::select! {
                    result = &mut probe => panic!("unanswered NOOP completed early: {result}"),
                    result = started => result.unwrap(),
                }
                tokio::time::advance(super::super::CONNECT_TIMEOUT - Duration::from_secs(1)).await;
                assert!(probe.as_mut().now_or_never().is_none());
                tokio::time::advance(Duration::from_secs(1)).await;
                assert!(!tokio::time::timeout(Duration::from_secs(1), probe).await
                    .expect("liveness must stop waiting at the connection deadline"));
            }
            assert!(adapter.session.is_none());
            assert!(adapter.selected.is_none());
            assert!(adapter.idle.is_none());
            assert!(!adapter.alive().await, "the unacknowledged session is never reused");
            server.await.unwrap();
        }

        #[tokio::test]
        async fn retirement_closes_silent_read_only_watch_setup() {
            use tokio::io::AsyncReadExt;
            for silent in ["CAPABILITY", "SELECT \"INBOX\"", "IDLE"] {
                let (socket, server) = tokio::io::duplex(4096);
                let (entered, started) = tokio::sync::oneshot::channel();
                let server = tokio::spawn(async move {
                    let mut io = BufReader::new(server);
                    io.get_mut().write_all(b"* OK test server\r\n").await.unwrap();
                    for (expected, reply) in [
                        ("LOGIN \"test\" \"password\"", ""),
                        ("CAPABILITY", "* CAPABILITY IMAP4rev1 IDLE\r\n"),
                        ("SELECT \"INBOX\"", SELECTED),
                        ("IDLE", ""),
                    ] {
                        let mut line = String::new();
                        io.read_line(&mut line).await.unwrap();
                        let (tag, command) = line.trim_end().split_once(' ').unwrap();
                        assert_eq!(command, expected);
                        if command == silent {
                            entered.send(()).unwrap();
                            let mut rest = Vec::new();
                            io.read_to_end(&mut rest).await.unwrap();
                            assert!(rest.is_empty(), "an incomplete setup closes its connection");
                            return;
                        }
                        io.get_mut().write_all(format!("{reply}{tag} OK completed\r\n").as_bytes()).await.unwrap();
                    }
                });
                let mut client = super::client(socket);
                client.read_response().await.unwrap().unwrap();
                let session = client.login("test", "password").await.map_err(|(error, _)| error).unwrap();
                let mut adapter = Imap { session: Some(session), selected: None, idle: None };
                let retirement = kernel::app::Retirement::default();
                let stop = retirement.clone();
                let retiring = tokio::spawn(async move { started.await.unwrap(); stop.request(); });
                let result = tokio::time::timeout(Duration::from_secs(2),
                    adapter.idle("INBOX", Duration::from_secs(5 * 60), &retirement)).await
                    .expect("silent setup must not keep the closing app alive").unwrap();
                assert_eq!(result, Watched::Quiet);
                assert!(adapter.session.is_none(), "retirement discards an unacknowledged command");
                retiring.await.unwrap();
                server.await.unwrap();
            }
        }

        /// Any eager attachment fetch fails the command assertion before
        /// the server provides those bytes.
        #[tokio::test]
        async fn sync_fetches_readings_and_a_download_fetches_only_its_section() {
            let (mut adapter, server) = scripted(vec![
                ("SELECT \"INBOX\"", SELECTED.into()),
                (
                    "UID FETCH 42 (UID FLAGS BODYSTRUCTURE BODY.PEEK[HEADER])",
                    metadata(42, MIXED),
                ),
                (
                    "UID FETCH 42 (UID BODY.PEEK[1])",
                    "* 1 FETCH (UID 42 BODY[1] {5}\r\nhello)\r\n".into(),
                ),
                (
                    "UID FETCH 42 (UID BODY.PEEK[2])",
                    "* 1 FETCH (UID 42 BODY[2] {8}\r\naGVsbG8=)\r\n".into(),
                ),
            ]).await;
            let mails = adapter.fetch_set("INBOX", "42").await.unwrap();
            assert_eq!(mails.len(), 1);
            assert!(mails[0].unread);
            let parsed = super::super::super::sync::parse_mail(&mails[0].raw).unwrap();
            assert_eq!(parsed.body, "hello");
            assert_eq!(parsed.attachments[0].name, "file.bin");
            assert_eq!(adapter.section(42, "2").await.unwrap(), b"aGVsbG8=");
            assert!(adapter.section(42, "2] BODY[]").await.is_err());
            server.await.unwrap();
        }

        #[tokio::test]
        async fn quoted_printable_html_is_fetched_as_the_message_body() {
            let body = "<p>caf=C3=A9</p>";
            let structure = format!(
                "(\"TEXT\" \"HTML\" (\"CHARSET\" \"UTF-8\") NIL NIL \"QUOTED-PRINTABLE\" {} 1)",
                body.len()
            );
            let (mut adapter, server) = scripted(vec![
                ("SELECT \"INBOX\"", SELECTED.into()),
                (
                    "UID FETCH 42 (UID FLAGS BODYSTRUCTURE BODY.PEEK[HEADER])",
                    metadata(42, &structure),
                ),
                (
                    "UID FETCH 42 (UID BODY.PEEK[1])",
                    format!("* 1 FETCH (UID 42 BODY[1] {{{}}}\r\n{body})\r\n", body.len()),
                ),
            ]).await;
            let mails = adapter.fetch_set("INBOX", "42").await.unwrap();
            assert_eq!(mails.len(), 1);
            assert!(mails[0].unread);
            let parsed = super::super::super::sync::parse_mail(&mails[0].raw).unwrap();
            assert_eq!(parsed.body, "café");
            assert_eq!(parsed.html.as_deref(), Some("<p>café</p>"));
            assert!(parsed.attachments.is_empty());
            server.await.unwrap();
        }

        #[tokio::test]
        async fn readings_are_batched_by_section_list_and_matched_by_uid() {
            // Two hundred common layouts cost one reading fetch. A second
            // layout costs one more, and a standalone file costs neither.
            let mut headers = (1..=200)
                .rev()
                .map(|uid| metadata(uid, MIXED))
                .collect::<String>();
            let alternative = "((\"TEXT\" \"PLAIN\" NIL NIL NIL \"7BIT\" 5 1)(\"TEXT\" \"HTML\" NIL NIL NIL \"7BIT\" 11 1) \"ALTERNATIVE\")";
            headers += &metadata(201, alternative);
            headers += &metadata(202, alternative);
            headers += &metadata(
                203,
                "(\"APPLICATION\" \"PDF\" NIL NIL NIL \"BASE64\" 12000)",
            );
            let bodies = (1..=200)
                .map(|uid| format!("* {uid} FETCH (UID {uid} BODY[1] {{5}}\r\n{uid:05})\r\n"))
                .collect();
            let alternatives = (201..=202).rev().map(|uid| format!("* {uid} FETCH (UID {uid} BODY[2] {{11}}\r\n<b>html</b> BODY[1] {{5}}\r\nplain)\r\n")).collect();
            let (mut adapter, server) = scripted(vec![
                ("SELECT \"INBOX\"", SELECTED.into()),
                (
                    "UID FETCH 1:203 (UID FLAGS BODYSTRUCTURE BODY.PEEK[HEADER])",
                    headers,
                ),
                ("UID FETCH 1:200 (UID BODY.PEEK[1])", bodies),
                (
                    "UID FETCH 201:202 (UID BODY.PEEK[1] BODY.PEEK[2])",
                    alternatives,
                ),
            ]).await;
            let mails = adapter.fetch_set("INBOX", "1:203").await.unwrap();
            assert_eq!(mails.len(), 203);
            for m in &mails[..200] {
                let parsed = super::super::super::sync::parse_mail(&m.raw).unwrap();
                assert_eq!(parsed.body, format!("{:05}", m.uid));
                assert_eq!(parsed.attachments.len(), 1);
            }
            for m in &mails[200..202] {
                let parsed = super::super::super::sync::parse_mail(&m.raw).unwrap();
                assert_eq!(parsed.body, "plain");
                assert!(parsed.html.is_some());
            }
            assert_eq!(
                super::super::super::sync::parse_mail(&mails[202].raw).unwrap().attachments[0].mime,
                "application/pdf"
            );
            server.await.unwrap();
        }

        #[tokio::test]
        async fn unusable_responses_do_not_discard_good_mail_or_stop_later_folders() {
            let mut headers = format!("* 1 FETCH (UID 1 BODYSTRUCTURE {TEXT})\r\n");
            headers += &metadata(2, TEXT).replace(&format!("BODYSTRUCTURE {TEXT} "), "");
            headers += &metadata(3, TEXT);
            headers += &metadata(4, TEXT);
            let (mut adapter, server) = scripted(vec![
                ("SELECT \"INBOX\"", SELECTED.into()),
                ("UID FETCH 1:4 (UID FLAGS BODYSTRUCTURE BODY.PEEK[HEADER])", headers),
                // UID 3 disappeared; an unsolicited response for UID 99
                // must neither supply its body nor be ingested itself.
                ("UID FETCH 3:4 (UID BODY.PEEK[1])", "* 99 FETCH (UID 99 BODY[1] {5}\r\nwrong)\r\n* 4 FETCH (UID 4 BODY[1] {5}\r\nhello)\r\n".into()),
                ("SELECT \"Archive\"", SELECTED.into()),
                ("UID FETCH 1 (UID FLAGS BODYSTRUCTURE BODY.PEEK[HEADER])", metadata(1, TEXT)),
                ("UID FETCH 1 (UID BODY.PEEK[1])", "* 1 FETCH (UID 1 BODY[1] {5}\r\nlater)\r\n".into()),
            ]).await;
            let mails = adapter.fetch_set("INBOX", "1:4").await.unwrap();
            assert_eq!(mails.iter().map(|m| m.uid).collect::<Vec<_>>(), [4]);
            assert_eq!(
                super::super::super::sync::parse_mail(&mails[0].raw).unwrap().body,
                "hello"
            );
            let later = adapter.fetch_set("Archive", "1").await.unwrap();
            assert_eq!(
                super::super::super::sync::parse_mail(&later[0].raw).unwrap().body,
                "later"
            );
            server.await.unwrap();
        }

        #[tokio::test]
        async fn a_disconnected_reading_fetch_still_reports_a_transport_error() {
            let (mut adapter, server) = scripted(vec![
                ("SELECT \"INBOX\"", SELECTED.into()),
                (
                    "UID FETCH 1 (UID FLAGS BODYSTRUCTURE BODY.PEEK[HEADER])",
                    metadata(1, TEXT),
                ),
                // Close before the reading fetch can finish.
            ]).await;
            assert!(adapter.fetch_set("INBOX", "1").await.is_err());
            server.await.unwrap();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The grant is looked for where the sign-in put it. The form files the
    /// refresh token through the [`Secrets`] capability, and in a real run
    /// that capability is the machine's keychain — so an `OAuth` reading the
    /// shared in-memory map would refuse every real Gmail account with *no
    /// google grant — sign in again*, however freshly it had signed in.
    #[test]
    fn the_grant_comes_from_the_store_the_sign_in_wrote_to() {
        // Stands in for the platform store: a map of its own, reached only
        // through the factory, exactly as the keychain is.
        let platform = MemSecrets::new();
        let handed = platform.clone();
        let env = Env {
            secrets_backend: Some(SecretsFactory::new(move || Box::new(handed.clone()))),
            ..Env::default()
        };
        let oauth = RealOAuth::new(&env);
        assert_eq!(oauth.grant("vera@gmail.com"), None, "nothing signed in yet");
        platform.plant(&oauth::refresh_key("vera@gmail.com"), "1//refresh");
        assert_eq!(oauth.grant("vera@gmail.com"), Some("1//refresh".into()));
        // And the map the env also carries is not that place.
        env.secrets
            .plant(&oauth::refresh_key("ana@gmail.com"), "1//other");
        assert_eq!(oauth.grant("ana@gmail.com"), None);
    }

    /// With no platform store — a scripted run, a build without one — the
    /// shared map is where the capability wrote, and where the grant is read.
    #[test]
    fn with_no_platform_store_the_shared_map_holds_the_grant() {
        let env = Env::default();
        env.secrets
            .plant(&oauth::refresh_key("vera@gmail.com"), "1//refresh");
        assert_eq!(
            RealOAuth::new(&env).grant("vera@gmail.com"),
            Some("1//refresh".into())
        );
    }

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

    /// A batch's UID set fits one command: consecutive uids collapse to a
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
        use async_imap::types::Flag;
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
        let worth = |line: &[u8]| session::worth_a_pass(&imap_proto::parser::parse_response(line).unwrap().1);
        assert!(worth(b"* 3 EXISTS\r\n"));
        assert!(worth(b"* 1 RECENT\r\n"));
        assert!(worth(b"* 2 EXPUNGE\r\n"));
        assert!(worth(b"* BYE closing\r\n"));
        assert!(!worth(b"* 4 FETCH (FLAGS (\\Seen))\r\n"));
        assert!(!worth(b"* FLAGS ()\r\n"));
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

        // A field holding a list — what a reply to one's own letter to three
        // people is prefilled with — goes out to all of them, and an empty
        // one is refused rather than sent to nobody.
        let many = Outgoing {
            to: "vera@kovac.io, max@ivanov.dev".into(),
            subject: "the two of you".into(),
            body: "hello".into(),
            ..Outgoing::default()
        };
        let msg = rfc822("me@prepor.dev", &many).expect("a message");
        assert_eq!(
            msg.envelope().to().len(),
            2,
            "both are on the envelope, not only the header"
        );
        let raw = String::from_utf8_lossy(&msg.formatted()).into_owned();
        assert!(raw.contains("To: vera@kovac.io, max@ivanov.dev"), "{raw}");
        assert!(rfc822("me@prepor.dev", &Outgoing::default()).is_err());

        // The comma inside a display name is not a separator. The list is
        // read as a header is read, so this is one recipient — cutting the
        // field on commas would have made it two, and neither would parse.
        let named = Outgoing {
            to: "\"Doe, Jane\" <jane@doe.com>".into(),
            subject: "one of you".into(),
            body: "hello".into(),
            ..Outgoing::default()
        };
        let msg = rfc822("me@prepor.dev", &named).expect("a message");
        assert_eq!(msg.envelope().to().len(), 1, "one recipient, not two");
        // Read back through the app's own reader — the name is encoded on
        // the wire, as a name with a comma in it must be, and comes back
        // whole.
        let out = msg.formatted();
        let back = mail_parser::MessageParser::default()
            .parse_headers(&out)
            .expect("the letter reads back");
        let to: Vec<_> = back.to().expect("a To line").iter().collect();
        assert_eq!(to.len(), 1, "one recipient, not two");
        assert_eq!(to[0].name(), Some("Doe, Jane"));
        assert_eq!(to[0].address(), Some("jane@doe.com"));
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
