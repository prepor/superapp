//! Gmail sign-in: OAuth 2 where the other accounts have an app password.
//!
//! Google turned off password IMAP, so a Gmail account is reached with a
//! bearer token instead — SASL `XOAUTH2` on both IMAP and SMTP. Getting one
//! is the *installed application* flow (RFC 8252): the browser does the
//! consent, and the answer comes back to a loopback listener this process
//! opened a moment earlier. No embedded webview, no password ever typed
//! into this app.
//!
//! Three durations, and they are the whole design:
//!
//! - the **authorization code** lives seconds, and never leaves this module;
//! - the **refresh token** is the account, and lives in the keychain beside
//!   the app passwords ([`crate::secret`]) under a key of its own;
//! - the **access token** lives an hour and stays in memory — a per-process
//!   cache in [`crate::effect::Real`], refreshed when a session asks and it
//!   has gone stale. It is deliberately never written down.
//!
//! The one thing this cannot ship is the client registration: Google issues
//! those per developer, so the app reads yours ([`Client::load`]) rather
//! than pretending to have one.

use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::Path;
use std::time::{Duration, Instant};

use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine;
use serde::Deserialize;

/// Where a provider's consent, tokens and mail servers live. One value for
/// now; a second provider (Microsoft speaks the same two protocols) is a
/// second const rather than a second module.
#[derive(Debug, Clone, Copy)]
pub struct Provider {
    /// The `account.auth` value, and the word the UI says.
    pub name: &'static str,
    /// The consent page the browser opens.
    pub authorize: &'static str,
    /// The endpoint both grants POST to.
    pub token: &'static str,
    /// What we ask for: full mail access, plus the identity claim that
    /// tells us which address consented.
    pub scope: &'static str,
    pub imap: &'static str,
    pub smtp: &'static str,
    /// The provider's own SMTP puts a copy of everything it sends into the
    /// Sent mailbox. A client that also APPENDs one files the mail twice.
    pub files_sent_itself: bool,
}

/// Gmail. `https://mail.google.com/` is the only scope Google's IMAP and
/// SMTP gateways accept — the finer `gmail.*` scopes are for the REST API.
pub const GOOGLE: Provider = Provider {
    name: "google",
    authorize: "https://accounts.google.com/o/oauth2/v2/auth",
    token: "https://oauth2.googleapis.com/token",
    scope: "https://mail.google.com/ openid email",
    imap: "imap.gmail.com",
    smtp: "smtp.gmail.com",
    // Gmail's SMTP saves to Sent Mail on its own, unlike a plain relay.
    files_sent_itself: true,
};

/// The `account.auth` value for a password account. `NULL` means the same —
/// every row written before Gmail existed here.
pub const PASSWORD: &str = "password";

/// How long the loopback listener waits for the browser to come back before
/// giving up and freeing the port.
const CONSENT_TIMEOUT: Duration = Duration::from_secs(300);

/// The keychain key a refresh token lives under. Deliberately not the bare
/// address: an account can hold both an app password and a Google grant
/// during a migration, and they must not overwrite each other.
#[must_use]
pub fn refresh_key(email: &str) -> String {
    format!("oauth:{email}")
}

// -- the client registration ---------------------------------------------------

/// The OAuth client this app authenticates *as*. Google issues one per
/// developer; a "desktop app" client's secret is not a secret in the usual
/// sense (RFC 8252 §8.5 says as much — it ships inside every installed copy,
/// which is why PKCE exists), but Google's token endpoint still demands it.
#[derive(Clone, PartialEq)]
pub struct Client {
    pub id: String,
    pub secret: String,
}

/// `Debug` redacts, on the same rule as [`crate::effect::Creds`].
impl std::fmt::Debug for Client {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Client")
            .field("id", &self.id)
            .field("secret", &"…")
            .finish()
    }
}

/// The shape Google's console hands you when you create a desktop client —
/// accepted verbatim, so the setup step is "move the downloaded file here".
#[derive(Deserialize)]
struct ConsoleFile {
    installed: Option<ConsoleClient>,
    web: Option<ConsoleClient>,
}

#[derive(Deserialize)]
struct ConsoleClient {
    client_id: String,
    #[serde(default)]
    client_secret: String,
}

impl Client {
    /// The client registration, from the environment or from a file beside
    /// the store.
    ///
    /// The env pair wins so a dev run can point at a throwaway client
    /// without moving files; `<dir>/google-oauth.json` is the durable
    /// place, and it takes the console's own JSON.
    ///
    /// # Errors
    ///
    /// If neither is present, or the file is not a client registration —
    /// the message is the setup instruction, because it is the only place a
    /// human meets this.
    pub fn load(dir: &Path) -> Result<Client, String> {
        let env = |k: &str| std::env::var(k).ok().filter(|v| !v.trim().is_empty());
        if let Some(id) = env("SUPERAPP_GOOGLE_CLIENT_ID") {
            return Ok(Client {
                id,
                secret: env("SUPERAPP_GOOGLE_CLIENT_SECRET").unwrap_or_default(),
            });
        }
        Client::from_dir(dir)
    }

    /// The file half of [`Client::load`], without the environment — which
    /// is also what a test can hold to, since env vars are process-global.
    ///
    /// # Errors
    ///
    /// If the file is absent or is not a client registration.
    pub fn from_dir(dir: &Path) -> Result<Client, String> {
        let path = dir.join("google-oauth.json");
        let Ok(text) = std::fs::read_to_string(&path) else {
            return Err(format!(
                "no google client — put the console's json at {} \
                 (or set SUPERAPP_GOOGLE_CLIENT_ID)",
                path.display()
            ));
        };
        Client::parse(&text).map_err(|why| format!("{}: {why}", path.display()))
    }

    /// The console's desktop-client file, and the flat pair a human might
    /// type by hand.
    ///
    /// A **Web** client is refused by name rather than accepted: this flow
    /// redirects to `http://127.0.0.1:<port>` on a port the OS picks per
    /// sign-in, and a web client only accepts redirect URIs registered in
    /// advance, port and all. Taking one would trade this message for a
    /// `redirect_uri_mismatch` in the browser, three steps later.
    ///
    /// # Errors
    ///
    /// If the text is not JSON, is a web client, or names no client id.
    pub fn parse(text: &str) -> Result<Client, String> {
        let some = |c: ConsoleClient| {
            if c.client_id.is_empty() {
                Err("no client_id".to_string())
            } else {
                Ok(Client {
                    id: c.client_id,
                    secret: c.client_secret,
                })
            }
        };
        if let Ok(f) = serde_json::from_str::<ConsoleFile>(text) {
            if let Some(c) = f.installed {
                return some(c);
            }
            if f.web.is_some() {
                return Err("that is a Web client — this sign-in needs an \
                            \"Application type: Desktop app\" client, whose \
                            loopback redirect needs no registration"
                    .into());
            }
        }
        let c: ConsoleClient =
            serde_json::from_str(text).map_err(|_| "not a client registration".to_string())?;
        some(c)
    }
}

// -- the flow ------------------------------------------------------------------

/// What a completed sign-in establishes: which address consented, the grant
/// to keep, and the token to use right now.
#[derive(Debug, Clone)]
pub struct Signed {
    pub email: String,
    /// The durable half — this goes to the keychain.
    pub refresh: String,
    /// The hour-long half.
    pub access: String,
    /// Unix seconds the access token stops working at.
    pub expires_at: f64,
}

/// A sign-in in flight.
///
/// Split in two on purpose: [`Flow::start`] and [`Flow::url`] are instant
/// and must happen on the UI thread (it is the one that can open a
/// browser), while [`Flow::wait`] blocks for as long as a human takes and
/// belongs on a thread of its own. The listener is bound before the browser
/// opens, so the redirect can never arrive at a closed port.
pub struct Flow {
    client: Client,
    provider: Provider,
    listener: TcpListener,
    redirect: String,
    verifier: String,
    /// The CSRF nonce; a redirect carrying a different one is not ours.
    state: String,
}

impl Flow {
    /// Binds the loopback listener and mints the PKCE pair.
    ///
    /// # Errors
    ///
    /// If no loopback port can be bound.
    pub fn start(client: Client, provider: Provider) -> Result<Flow, String> {
        // Port 0: the OS picks. Google allows any port on 127.0.0.1 for an
        // installed app, precisely so this needs no registration.
        let listener = TcpListener::bind("127.0.0.1:0").map_err(|e| format!("loopback: {e}"))?;
        let port = listener
            .local_addr()
            .map_err(|e| format!("loopback: {e}"))?
            .port();
        listener
            .set_nonblocking(true)
            .map_err(|e| format!("loopback: {e}"))?;
        Ok(Flow {
            client,
            provider,
            listener,
            redirect: format!("http://127.0.0.1:{port}"),
            verifier: nonce(64)?,
            state: nonce(24)?,
        })
    }

    /// The consent page to open in the browser.
    ///
    /// `access_type=offline` with `prompt=consent` is what makes Google
    /// return a refresh token: without the pair, a second sign-in for an
    /// address that already granted consent answers with an access token
    /// alone, and the account would work for an hour and then die.
    #[must_use]
    pub fn url(&self) -> String {
        let challenge = URL_SAFE_NO_PAD.encode(sha256(self.verifier.as_bytes()));
        query(
            self.provider.authorize,
            &[
                ("client_id", &self.client.id),
                ("redirect_uri", &self.redirect),
                ("response_type", "code"),
                ("scope", self.provider.scope),
                ("code_challenge", &challenge),
                ("code_challenge_method", "S256"),
                ("state", &self.state),
                ("access_type", "offline"),
                ("prompt", "consent"),
            ],
        )
    }

    /// Blocks until the browser comes back, then trades the code for
    /// tokens. Consumes the flow — a code is good once.
    ///
    /// # Errors
    ///
    /// If the human declines, closes the tab (the timeout), or the token
    /// endpoint refuses.
    pub fn wait(self, now: f64) -> Result<Signed, String> {
        let code = self.await_code()?;
        let r = post_token(
            self.provider.token,
            &[
                ("client_id", &self.client.id),
                ("client_secret", &self.client.secret),
                ("code", &code),
                ("code_verifier", &self.verifier),
                ("redirect_uri", &self.redirect),
                ("grant_type", "authorization_code"),
            ],
        )?;
        let refresh = r.refresh_token.ok_or(
            "google returned no refresh token — revoke the app's access and sign in again",
        )?;
        // The address comes from the id_token's `email` claim. The
        // signature goes unchecked *and that is sound here*: the JWT did
        // not come via the browser, it came back over this TLS connection
        // to Google's own token endpoint, which is a stronger statement
        // than the signature would be.
        let email = r
            .id_token
            .as_deref()
            .and_then(id_token_email)
            .ok_or("google returned no address for the account")?;
        Ok(Signed {
            email,
            refresh,
            access: r.access_token,
            expires_at: now + r.expires_in.unwrap_or(3600.0),
        })
    }

    /// Serves the loopback redirect: the first request carrying `code` or
    /// `error` wins, and everything else (a browser's `/favicon.ico`) is
    /// answered and ignored.
    fn await_code(&self) -> Result<String, String> {
        let deadline = Instant::now() + CONSENT_TIMEOUT;
        loop {
            match self.listener.accept() {
                Ok((stream, _)) => {
                    if let Some(q) = read_request(stream) {
                        if let Some(err) = param(&q, "error") {
                            return Err(format!("google refused: {err}"));
                        }
                        if let Some(code) = param(&q, "code") {
                            if param(&q, "state").as_deref() != Some(self.state.as_str()) {
                                return Err("the redirect did not match this sign-in".into());
                            }
                            return Ok(code);
                        }
                    }
                }
                Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                    if Instant::now() >= deadline {
                        return Err("google never came back — the sign-in timed out".into());
                    }
                    std::thread::sleep(Duration::from_millis(100));
                }
                Err(e) => return Err(format!("loopback: {e}")),
            }
        }
    }
}

/// Trades a stored refresh token for a fresh access token. Answers the
/// token and the unix second it stops working at.
///
/// # Errors
///
/// If the grant was revoked (the human, or Google's 6-month idle rule), or
/// the endpoint is unreachable.
pub fn refresh(
    client: &Client,
    provider: Provider,
    refresh_token: &str,
    now: f64,
) -> Result<(String, f64), String> {
    let r = post_token(
        provider.token,
        &[
            ("client_id", &client.id),
            ("client_secret", &client.secret),
            ("refresh_token", refresh_token),
            ("grant_type", "refresh_token"),
        ],
    )?;
    Ok((r.access_token, now + r.expires_in.unwrap_or(3600.0)))
}

/// The SASL `XOAUTH2` initial response, before base64 — the same bytes for
/// IMAP and SMTP (Google's own spec, not an RFC).
#[must_use]
pub fn xoauth2(user: &str, token: &str) -> String {
    format!("user={user}\x01auth=Bearer {token}\x01\x01")
}

// -- the wire ------------------------------------------------------------------

#[derive(Deserialize)]
struct TokenReply {
    access_token: String,
    #[serde(default)]
    refresh_token: Option<String>,
    #[serde(default)]
    id_token: Option<String>,
    #[serde(default)]
    expires_in: Option<f64>,
}

/// Google's error body. Worth parsing: `invalid_grant` on a refresh is the
/// one failure a human must act on (sign in again), and the raw JSON says
/// so far less clearly.
#[derive(Deserialize)]
struct TokenError {
    error: String,
    #[serde(default)]
    error_description: Option<String>,
}

fn post_token(url: &str, form: &[(&str, &str)]) -> Result<TokenReply, String> {
    let body = form
        .iter()
        .map(|(k, v)| format!("{}={}", enc(k), enc(v)))
        .collect::<Vec<_>>()
        .join("&");
    let (status, text) = post(url, &body)?;
    if status != 200 {
        let why = serde_json::from_str::<TokenError>(&text).map_or_else(
            |_| text.trim().chars().take(200).collect::<String>(),
            |e| match e.error_description {
                Some(d) => format!("{} ({})", d, e.error),
                None => e.error,
            },
        );
        return Err(format!("google: {why}"));
    }
    serde_json::from_str(&text).map_err(|e| format!("google sent something unreadable: {e}"))
}

/// One HTTPS POST of a form body, answering `(status, body)`.
///
/// Hand-rolled on the TLS connector `imap` already brings, because that is
/// the honest size of the need: two endpoints, one verb, no redirects, no
/// keep-alive. `Connection: close` means the body ends with the stream, so
/// no `Content-Length` is needed — but it does **not** rule out
/// `Transfer-Encoding: chunked`, which an HTTP/1.1 server may send anyway
/// and which would otherwise reach serde with the chunk sizes still in it.
fn post(url: &str, body: &str) -> Result<(u16, String), String> {
    let rest = url
        .strip_prefix("https://")
        .ok_or_else(|| format!("{url}: only https"))?;
    let (host, path) = rest.split_once('/').map_or((rest, "/"), |(h, p)| (h, p));
    let path = if path.starts_with('/') {
        path.to_string()
    } else {
        format!("/{path}")
    };

    let tcp = TcpStream::connect((host, 443)).map_err(|e| format!("{host}: {e}"))?;
    tcp.set_read_timeout(Some(Duration::from_secs(30)))
        .map_err(|e| format!("{host}: {e}"))?;
    let connector = rustls_connector::RustlsConnector::new_with_native_certs()
        .map_err(|e| format!("tls: {e}"))?;
    let mut tls = connector
        .connect(host, tcp)
        .map_err(|e| format!("{host}: {e}"))?;

    let req = format!(
        "POST {path} HTTP/1.1\r\n\
         Host: {host}\r\n\
         Content-Type: application/x-www-form-urlencoded\r\n\
         Content-Length: {len}\r\n\
         Connection: close\r\n\
         Accept: application/json\r\n\
         \r\n{body}",
        len = body.len()
    );
    tls.write_all(req.as_bytes())
        .map_err(|e| format!("{host}: {e}"))?;
    let mut raw = Vec::new();
    // A server that closes mid-body still leaves what it sent; the parse
    // below is what decides whether that was enough.
    let _ = tls.read_to_end(&mut raw);
    let text = String::from_utf8_lossy(&raw).into_owned();
    let (head, rest) = text
        .split_once("\r\n\r\n")
        .ok_or_else(|| format!("{host}: truncated response"))?;
    let status = head
        .lines()
        .next()
        .and_then(|l| l.split_whitespace().nth(1))
        .and_then(|c| c.parse().ok())
        .ok_or_else(|| format!("{host}: no status line"))?;
    let chunked = head
        .lines()
        .skip(1)
        .filter_map(|l| l.split_once(':'))
        .any(|(k, v)| {
            k.eq_ignore_ascii_case("transfer-encoding")
                && v.to_ascii_lowercase().contains("chunked")
        });
    let body = if chunked {
        dechunk(rest).ok_or_else(|| format!("{host}: malformed chunked response"))?
    } else {
        rest.to_string()
    };
    Ok((status, body))
}

/// Reassembles a `Transfer-Encoding: chunked` body: each chunk is a hex
/// length (extensions after a `;` ignored), CRLF, that many bytes, CRLF,
/// ending at a zero-length chunk. Trailers after it are not read — nothing
/// here wants one.
fn dechunk(body: &str) -> Option<String> {
    let mut rest = body;
    let mut out = String::new();
    loop {
        let (line, after) = rest.split_once("\r\n")?;
        let size = usize::from_str_radix(line.split(';').next()?.trim(), 16).ok()?;
        if size == 0 {
            return Some(out);
        }
        let chunk = after.get(..size)?;
        out.push_str(chunk);
        rest = after.get(size..)?.strip_prefix("\r\n")?;
    }
}

/// Reads one loopback request, answers the page the human is left looking
/// at, and hands back its query string.
fn read_request(mut stream: TcpStream) -> Option<String> {
    let _ = stream.set_read_timeout(Some(Duration::from_secs(5)));
    // The request line is the whole interest, and it is the first line.
    let mut buf = [0u8; 4096];
    let n = stream.read(&mut buf).ok()?;
    let line = String::from_utf8_lossy(&buf[..n]);
    let target = line.split_whitespace().nth(1).unwrap_or("").to_string();

    let page = "<!doctype html><meta charset=utf-8>\
        <title>signed in</title>\
        <body style=\"font:14px ui-monospace,monospace;padding:40px\">\
        signed in. you can close this tab.";
    let _ = stream.write_all(
        format!(
            "HTTP/1.1 200 OK\r\n\
             Content-Type: text/html; charset=utf-8\r\n\
             Content-Length: {}\r\n\
             Connection: close\r\n\r\n{page}",
            page.len()
        )
        .as_bytes(),
    );
    let _ = stream.flush();
    Some(
        target
            .split_once('?')
            .map_or(String::new(), |(_, q)| q.to_string()),
    )
}

/// One parameter out of a `a=1&b=2` query, percent-decoded.
fn param(query: &str, key: &str) -> Option<String> {
    query
        .split('&')
        .filter_map(|p| p.split_once('='))
        .find(|(k, _)| *k == key)
        .map(|(_, v)| dec(v))
}

/// A JWT's `email` claim. The signature is not checked — see [`Flow::wait`]
/// for why that is sound at the one call site.
fn id_token_email(jwt: &str) -> Option<String> {
    #[derive(Deserialize)]
    struct Claims {
        email: Option<String>,
    }
    let payload = jwt.split('.').nth(1)?;
    let bytes = URL_SAFE_NO_PAD.decode(payload).ok()?;
    let c: Claims = serde_json::from_slice(&bytes).ok()?;
    c.email.filter(|e| !e.is_empty())
}

// -- small change --------------------------------------------------------------

/// A URL with a query appended.
fn query(base: &str, params: &[(&str, &str)]) -> String {
    let q = params
        .iter()
        .map(|(k, v)| format!("{}={}", enc(k), enc(v)))
        .collect::<Vec<_>>()
        .join("&");
    format!("{base}?{q}")
}

/// Percent-encoding for a form value / query parameter: everything outside
/// RFC 3986's unreserved set goes to `%XX`, spaces included (`+` is legal
/// in a form body but not in a query, and one rule is fewer rules).
fn enc(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' => {
                out.push(b as char);
            }
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

/// The inverse, for the redirect's query. `+` means space there.
fn dec(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        match b[i] {
            b'%' if i + 2 < b.len() => {
                let hex = std::str::from_utf8(&b[i + 1..i + 3]).ok();
                match hex.and_then(|h| u8::from_str_radix(h, 16).ok()) {
                    Some(v) => {
                        out.push(v);
                        i += 3;
                    }
                    None => {
                        out.push(b[i]);
                        i += 1;
                    }
                }
            }
            b'+' => {
                out.push(b' ');
                i += 1;
            }
            c => {
                out.push(c);
                i += 1;
            }
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

fn sha256(bytes: &[u8]) -> Vec<u8> {
    ring::digest::digest(&ring::digest::SHA256, bytes)
        .as_ref()
        .to_vec()
}

/// `n` characters of URL-safe randomness — the PKCE verifier (RFC 7636 asks
/// for 43..128 unreserved characters) and the state nonce.
///
/// The failure is propagated rather than swallowed, and that is the whole
/// point of the `Result`: a zeroed buffer would still hash to a *matching*
/// challenge, so the flow would sail through with a verifier and a CSRF
/// nonce an attacker could write down. There is no safe fallback — the only
/// honest move is to refuse the sign-in.
///
/// # Errors
///
/// If the OS cannot supply randomness.
fn nonce(n: usize) -> Result<String, String> {
    use ring::rand::SecureRandom;
    let mut bytes = vec![0u8; n];
    ring::rand::SystemRandom::new()
        .fill(&mut bytes)
        .map_err(|_| "no randomness for the sign-in".to_string())?;
    Ok(URL_SAFE_NO_PAD.encode(&bytes)[..n].to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use base64::engine::general_purpose::STANDARD;

    /// RFC 7636's own worked example: this verifier must produce this
    /// challenge, or every sign-in fails at Google with a mismatch.
    #[test]
    fn pkce_matches_the_rfc_vector() {
        let verifier = "dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk";
        let challenge = URL_SAFE_NO_PAD.encode(sha256(verifier.as_bytes()));
        assert_eq!(challenge, "E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM");
    }

    /// The verifier is the length asked for, inside RFC 7636's alphabet and
    /// its 43..128 window.
    #[test]
    fn the_verifier_is_well_formed() {
        let v = nonce(64).expect("randomness");
        assert_eq!(v.len(), 64);
        assert!((43..=128).contains(&v.len()));
        assert!(v
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || "-._~".contains(c)));
        assert_ne!(
            v,
            nonce(64).expect("randomness"),
            "two flows never share a verifier"
        );
    }

    /// The consent URL carries what makes the flow work at all: PKCE with
    /// S256, and the offline/consent pair that yields a refresh token.
    #[test]
    fn the_consent_url_asks_for_a_refresh_token() {
        let f = Flow::start(
            Client {
                id: "cid.apps.googleusercontent.com".into(),
                secret: "shh".into(),
            },
            GOOGLE,
        )
        .unwrap();
        let u = f.url();
        assert!(u.starts_with("https://accounts.google.com/o/oauth2/v2/auth?"));
        for want in [
            "client_id=cid.apps.googleusercontent.com",
            "code_challenge_method=S256",
            "access_type=offline",
            "prompt=consent",
            "response_type=code",
            "redirect_uri=http%3A%2F%2F127.0.0.1%3A",
            "scope=https%3A%2F%2Fmail.google.com%2F%20openid%20email",
        ] {
            assert!(u.contains(want), "{want} missing from {u}");
        }
        // The secret is the one thing the browser must never see.
        assert!(!u.contains("shh"));
    }

    /// The console's downloaded json, both shapes, and a flat one.
    #[test]
    fn the_console_file_is_taken_verbatim() {
        let installed = r#"{"installed":{"client_id":"a.apps.googleusercontent.com",
            "project_id":"p","client_secret":"s","redirect_uris":["http://localhost"]}}"#;
        assert_eq!(
            Client::parse(installed),
            Ok(Client {
                id: "a.apps.googleusercontent.com".into(),
                secret: "s".into()
            })
        );
        assert_eq!(
            Client::parse(r#"{"client_id":"flat","client_secret":"y"}"#),
            Ok(Client {
                id: "flat".into(),
                secret: "y".into()
            })
        );
        assert!(Client::parse("{}").is_err());
        assert!(Client::parse("not json").is_err());
    }

    /// A web client is refused where the human can still act on it, and the
    /// message names the type to create instead. It cannot work: the
    /// redirect port is picked per sign-in, and a web client's redirect URIs
    /// are registered in advance.
    #[test]
    fn a_web_client_is_refused_by_name() {
        let e = Client::parse(r#"{"web":{"client_id":"w","client_secret":"x"}}"#).unwrap_err();
        assert!(e.contains("Web client"), "{e}");
        assert!(e.contains("Desktop app"), "{e}");
    }

    /// A client's `Debug` never prints its secret — the same rule the
    /// password creds are held to.
    #[test]
    fn the_client_secret_never_prints() {
        let c = Client {
            id: "public".into(),
            secret: "s3cret".into(),
        };
        let s = format!("{c:?}");
        assert!(s.contains("public"));
        assert!(!s.contains("s3cret"));
    }

    /// The address is read out of the id_token's payload without a
    /// signature check (sound: it arrived over TLS from the token endpoint).
    #[test]
    fn the_address_comes_from_the_id_token() {
        let payload = URL_SAFE_NO_PAD.encode(r#"{"email":"a@gmail.com","email_verified":true}"#);
        assert_eq!(
            id_token_email(&format!("header.{payload}.signature")).as_deref(),
            Some("a@gmail.com")
        );
        assert_eq!(id_token_email("nonsense"), None);
        assert_eq!(
            id_token_email(&format!("h.{}.s", URL_SAFE_NO_PAD.encode("{}"))),
            None
        );
    }

    /// A chunked body reassembles, because `Connection: close` does not
    /// stop a server from sending one — and the chunk sizes reaching serde
    /// would read as "google sent something unreadable" on every sign-in.
    #[test]
    fn a_chunked_body_reassembles() {
        let body = "1a\r\n{\"access_token\":\"ya29.abc\"\r\n6\r\n,\"a\":1\r\n1\r\n}\r\n0\r\n\r\n";
        assert_eq!(
            dechunk(body).as_deref(),
            Some("{\"access_token\":\"ya29.abc\",\"a\":1}")
        );
        // One chunk, an extension on the size line, and the empty body.
        assert_eq!(dechunk("3;x=y\r\nabc\r\n0\r\n\r\n").as_deref(), Some("abc"));
        assert_eq!(dechunk("0\r\n\r\n").as_deref(), Some(""));
        // Truncated mid-chunk is a failure, not a silent short read.
        assert_eq!(dechunk("5\r\nab"), None);
        assert_eq!(dechunk("zz\r\n"), None);
    }

    /// The redirect's query, as a browser sends it.
    #[test]
    fn the_redirect_query_reads_back() {
        let q = "state=abc&code=4%2F0Ab_c-d&scope=https%3A%2F%2Fmail.google.com%2F";
        assert_eq!(param(q, "code").as_deref(), Some("4/0Ab_c-d"));
        assert_eq!(param(q, "state").as_deref(), Some("abc"));
        assert_eq!(param(q, "error"), None);
        assert_eq!(
            param("error=access_denied", "error").as_deref(),
            Some("access_denied")
        );
    }

    /// Percent-encoding round-trips the characters a scope and a code carry.
    #[test]
    fn encoding_round_trips() {
        for s in [
            "https://mail.google.com/ openid email",
            "4/0Ab_c-d",
            "a@b.c",
            "~-._",
        ] {
            assert_eq!(dec(&enc(s)), s);
        }
        assert_eq!(enc("a b"), "a%20b");
        assert_eq!(dec("a+b"), "a b");
    }

    /// The SASL envelope, byte for byte — Google rejects anything else, and
    /// the two control bytes are invisible in a diff.
    #[test]
    fn the_xoauth2_envelope_is_exact() {
        assert_eq!(
            xoauth2("a@gmail.com", "tok"),
            "user=a@gmail.com\u{1}auth=Bearer tok\u{1}\u{1}"
        );
        // And what goes on the wire once the protocol layer base64s it —
        // `imap`'s `authenticate` and lettre's XOAUTH2 mechanism both do
        // that encoding themselves, so this is the byte string they must
        // arrive at.
        assert_eq!(
            STANDARD.encode(xoauth2("a@gmail.com", "tok")),
            "dXNlcj1hQGdtYWlsLmNvbQFhdXRoPUJlYXJlciB0b2sBAQ=="
        );
    }

    /// The refresh token is keyed apart from the app password, so an
    /// account can hold both without either overwriting the other.
    #[test]
    fn the_refresh_token_has_its_own_key() {
        assert_eq!(refresh_key("a@gmail.com"), "oauth:a@gmail.com");
        assert_ne!(refresh_key("a@gmail.com"), "a@gmail.com");
    }

    /// The missing registration explains itself: this error is the whole
    /// setup documentation a human gets.
    #[test]
    fn a_missing_client_says_what_to_do() {
        let dir = std::env::temp_dir().join(format!("superapp-oauth-{}", std::process::id()));
        let e = Client::from_dir(&dir).unwrap_err();
        assert!(e.contains("google-oauth.json"), "{e}");
        assert!(e.contains("SUPERAPP_GOOGLE_CLIENT_ID"), "{e}");
    }

    /// The console file, dropped where the store lives, is picked up.
    #[test]
    fn a_dropped_console_file_is_found() {
        let dir = std::env::temp_dir().join(format!("superapp-oauth-ok-{}", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);
        std::fs::write(
            dir.join("google-oauth.json"),
            r#"{"installed":{"client_id":"c","client_secret":"s"}}"#,
        )
        .unwrap();
        assert_eq!(
            Client::from_dir(&dir),
            Ok(Client {
                id: "c".into(),
                secret: "s".into()
            })
        );
        let _ = std::fs::remove_dir_all(&dir);
    }
}
