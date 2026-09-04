//! Cloudflare R2 implementation of the device-sync object store.
//!
//! Requests use TLS and AWS SigV4. S3 conditional writes provide create-only
//! and compare-and-swap behavior. [`open`] selects this implementation for an
//! HTTPS bucket URL.
//!
//! The secret never comes from here: it is asked of the world's
//! [`Secrets`] capability, which is the macOS keychain
//! on a real run and memory under a script. This module only knows the key it
//! is filed under.
//!
//! What is filed there is the Cloudflare API token's **value**, not the S3
//! secret access key: by Cloudflare's definition the latter is the SHA-256 of
//! the former, so [`creds`] hashes on the way to a signature and the same
//! entry can be borne as a token by whatever else this account owns —
//! [`gateway`] is the AI gateway asking for exactly that.

use std::net::TcpStream;
use std::path::Path;
use std::sync::{Arc, OnceLock};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use super::object::{self, Blob, Cas, Object, PutNew};
use crate::caps::Secrets;

/// A request gets this long to connect, send and answer. Generous: a snapshot
/// upload on a slow uplink is a legitimate minute.
const TIMEOUT: Duration = Duration::from_secs(60);

/// The desktop's environment: the access key id, its secret, and (for a
/// non-R2 S3 endpoint) the region to sign under.
const ENV_KEY: &str = "SUPERAPP_R2_ACCESS_KEY_ID";
const ENV_SECRET: &str = "SUPERAPP_R2_SECRET_ACCESS_KEY";
const ENV_REGION: &str = "SUPERAPP_R2_REGION";

/// Where the bucket is, when nothing pointed this process at one. The shell
/// reads the same variable (behind its `--bucket` flag); this is the half of
/// that resolution the kernel can do on its own.
const ENV_BUCKET: &str = "SUPERAPP_BUCKET";

/// R2 signs under one region name whatever the bucket's jurisdiction.
const R2_REGION: &str = "auto";

/// Every R2 endpoint is a label under this domain, and the label is the
/// Cloudflare account — which is how [`account_of`] finds the gateway.
const R2_DOMAIN: &str = "r2.cloudflarestorage.com";

// -- credentials ---------------------------------------------------------------

/// What an S3-compatible endpoint needs to believe a request is ours.
#[derive(Clone)]
pub struct Creds {
    pub key_id: String,
    /// The *secret access key*, which for an R2 token is the SHA-256 of its
    /// value — never the value itself. [`creds`] is what computes it.
    pub secret: String,
    pub region: String,
}

/// Never let the secret reach a log line by accident.
impl std::fmt::Debug for Creds {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Creds")
            .field("key_id", &self.key_id)
            .field("secret", &"<redacted>")
            .field("region", &self.region)
            .finish()
    }
}

/// Reads the `bucket` file beside the store: line 1 the URL, line 2 the
/// access key id, line 3 the Cloudflare API token's value. One file, because
/// a device that is configured by `adb push` (android) has no environment and
/// no keychain — the app sandbox is its perimeter, the same trade the
/// platform's secret store makes. Blank lines and `#` comments are skipped,
/// so the file can carry a note.
fn from_file(dir: Option<&Path>) -> Vec<String> {
    let Some(dir) = dir else {
        return Vec::new();
    };
    let Ok(text) = std::fs::read_to_string(dir.join("bucket")) else {
        return Vec::new();
    };
    text.lines()
        .map(str::trim)
        .filter(|l| !l.is_empty() && !l.starts_with('#'))
        .map(str::to_string)
        .collect()
}

/// The bucket URL, from the `bucket` file beside the store — its first
/// meaningful line. (The flag and the environment variable are read by the
/// app, which owns the command line; this is the file half of the same
/// resolution.)
#[must_use]
pub fn url_from_file(dir: Option<&Path>) -> Option<String> {
    from_file(dir).into_iter().next()
}

/// Where a bucket's secret access key is filed in the world's
/// [`Secrets`]: under its key id, behind a prefix, so
/// it can never collide with a mail account's address.
#[must_use]
pub fn secret_key(key_id: &str) -> String {
    format!("r2/{key_id}")
}

/// The credentials for a real bucket, from — in order — the environment, the
/// `bucket` file's second and third lines, and the world's secret store (the
/// macOS keychain on a real run, memory under a script).
///
/// The key id is not a secret and may sit in a file; the secret is looked up
/// last so that a keychain entry, once written by `--r2-login`, is enough.
/// What is filed is the token's value and what signs is its hash: see
/// [`s3_secret`].
///
/// # Errors
///
/// If either half cannot be found — with the places we looked, because a
/// device that silently syncs nothing is the worst outcome here.
pub fn creds(dir: Option<&Path>, secrets: &mut dyn Secrets) -> Result<Creds, String> {
    let (key_id, secret) = filed(dir, secrets)?;
    Ok(Creds {
        key_id,
        secret: s3_secret(&secret),
        region: env(ENV_REGION).unwrap_or_else(|| R2_REGION.to_string()),
    })
}

/// Where the secret store remembers which key id `--r2-login` last filed a
/// token for, so a device with no `bucket` file and no environment — a
/// laptop that never joined a bucket — still finds its token by the id it
/// was filed under. Not a secret, but the store is the one place every
/// platform has.
const KEY_ID_KEY: &str = "r2/key_id";

/// The key id and the secret exactly as they were filed, in the one order
/// every reader of them uses.
///
/// Two things read this now — [`creds`], which hashes, and [`gateway`], which
/// bears the value — and they must never disagree about *which* token this
/// device holds, so neither may look somewhere the other does not.
fn filed(dir: Option<&Path>, secrets: &mut dyn Secrets) -> Result<(String, String), String> {
    let file = from_file(dir);
    let key_id = env(ENV_KEY)
        .or_else(|| file.get(1).cloned())
        .or_else(|| secrets.get(KEY_ID_KEY).and_then(clean_secret))
        .ok_or_else(|| {
            format!("no access key id — set {ENV_KEY}, or put it on line 2 of the `bucket` file")
        })?;
    let secret = env(ENV_SECRET)
        .or_else(|| file.get(2).cloned())
        .or_else(|| secrets.get(&secret_key(&key_id)))
        .and_then(clean_secret)
        .ok_or_else(|| {
            format!(
                "no secret for {key_id} — run `superapp --r2-login`, set {ENV_SECRET}, \
                 or put it on line 3 of the `bucket` file"
            )
        })?;
    Ok((key_id, secret))
}

/// Whether a filed secret is already an S3 secret access key rather than the
/// token it is made from: 64 hex digits, which is what a SHA-256 in hex is.
///
/// A device configured before the gateway existed filed what the dashboard
/// showed it — the hash — and from a hash no token can be recovered. Asking
/// every such device to run `--r2-login` once more would be the tidier rule;
/// judging a secret by its shape is deliberately preferred to it, because
/// then a device that already syncs keeps syncing whatever it holds and only
/// the gateway asks for anything. The shape is unambiguous: a Cloudflare API
/// token is 40 characters of `[A-Za-z0-9_-]` and can never be 64 hex digits.
#[must_use]
fn is_hash(secret: &str) -> bool {
    secret.len() == 64 && secret.bytes().all(|b| b.is_ascii_hexdigit())
}

/// The S3 secret access key a filed secret signs with: by Cloudflare's
/// definition the SHA-256 of the API token's value, hex — or the secret
/// itself, when a device filed that hash before this change.
#[must_use]
fn s3_secret(secret: &str) -> String {
    if is_hash(secret) {
        secret.to_string()
    } else {
        sha256_hex(secret.as_bytes())
    }
}

// -- the same token, at the gateway --------------------------------------------

/// What Cloudflare's AI gateway needs: whose account it is, and the API token
/// to bear. Both are things device sync already holds — there is no second
/// credential anywhere in this app.
#[derive(Clone)]
pub struct GatewayCreds {
    pub account: String,
    pub token: String,
}

/// Never let the token reach a log line by accident, as [`Creds`] does not.
impl std::fmt::Debug for GatewayCreds {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("GatewayCreds")
            .field("account", &self.account)
            .field("token", &"<redacted>")
            .finish()
    }
}

/// The Cloudflare account a bucket URL belongs to: the first label of an
/// `https://{account}.r2.cloudflarestorage.com/…` host.
///
/// A jurisdiction puts one more label between the two
/// (`{account}.eu.r2.cloudflarestorage.com`); the account is still the first,
/// so any host under the R2 domain answers.
///
/// `None` for anything else — the local `bucketd`, some other S3 endpoint —
/// which is a device with a bucket and no gateway.
#[must_use]
pub fn account_of(url: &str) -> Option<String> {
    if !url.trim().starts_with("https://") {
        return None;
    }
    let host = host_of(url).to_ascii_lowercase();
    let (account, domain) = host.split_once('.')?;
    // Nothing but a jurisdiction's own label may stand between the two.
    let between = domain.strip_suffix(R2_DOMAIN);
    let on_r2 = between.is_some_and(|j| j.is_empty() || j.ends_with('.'));
    (!account.is_empty() && on_r2).then(|| account.to_string())
}

/// A URL's host, without scheme, port or path — the part worth quoting when
/// it is the host that is wrong.
fn host_of(url: &str) -> &str {
    let rest = url.trim();
    let rest = rest.split_once("://").map_or(rest, |(_, r)| r);
    let authority = rest.split('/').next().unwrap_or(rest);
    match authority.rsplit_once(':') {
        Some((h, port)) if !port.is_empty() && port.bytes().all(|b| b.is_ascii_digit()) => h,
        _ => authority,
    }
}

/// The Cloudflare account a run's bucket belongs to, when a bucket is known
/// and it is R2's. `url` is what the shell resolved — argv's `--bucket`
/// first — and the environment and the `bucket` file are read behind it, so
/// a run pointed at a bucket by flag alone finds its account here too.
///
/// `None` for a device with no bucket, or one whose bucket is not on R2 —
/// the local `bucketd`. Neither is a device without a gateway: the app then
/// asks Cloudflare whose token it holds, over [`gateway_token`].
#[must_use]
pub fn account_from(url: Option<&str>, dir: Option<&Path>) -> Option<String> {
    let url = url
        .map(str::trim)
        .filter(|u| !u.is_empty())
        .map(str::to_string)
        .or_else(|| env(ENV_BUCKET))
        .or_else(|| url_from_file(dir))?;
    account_of(&url)
}

/// The token the gateway bears — the one device sync files, by value — with
/// the key id it was filed under. The same lookup in the same order
/// [`creds`] uses, so the gateway and the bucket can never be opened by two
/// different tokens.
///
/// # Errors
///
/// If no key id or no secret can be found, or if what is filed is the old S3
/// hash — from which no token can be recovered. Each sentence says where to
/// put what is missing.
pub fn gateway_token(
    dir: Option<&Path>,
    secrets: &mut dyn Secrets,
) -> Result<(String, String), String> {
    let (key_id, token) = filed(dir, secrets)?;
    if is_hash(&token) {
        return Err(format!(
            "the secret for {key_id} is the S3 hash, not the token — \
             run `superapp --r2-login` again, with the token's value"
        ));
    }
    Ok((key_id, token))
}

/// The gateway's credentials, out of what device sync is already configured
/// with: the account off the bucket's host, and the same API token, borne
/// whole this time. `url` is the bucket the shell resolved, read as
/// [`account_from`] reads it; the token comes through [`gateway_token`].
///
/// # Errors
///
/// If there is no bucket to read an account off, if its host is not R2's, if
/// no secret is filed, or if what is filed is the old S3 hash. Each says
/// where to put what is missing.
pub fn gateway(
    url: Option<&str>,
    dir: Option<&Path>,
    secrets: &mut dyn Secrets,
) -> Result<GatewayCreds, String> {
    let url = url
        .map(str::trim)
        .filter(|u| !u.is_empty())
        .map(str::to_string)
        .or_else(|| env(ENV_BUCKET))
        .or_else(|| url_from_file(dir))
        .ok_or_else(|| {
            format!(
                "no bucket — the gateway's account is read off the bucket's host: \
                 run with `--bucket URL`, set {ENV_BUCKET}, or put it on line 1 of the `bucket` file"
            )
        })?;
    let account = account_of(&url).ok_or_else(|| {
        format!(
            "the gateway's account cannot be read off {:?} — it is the first label of \
             an R2 host, <account>.{R2_DOMAIN}",
            host_of(&url)
        )
    })?;
    let (_key_id, token) = gateway_token(dir, secrets)?;
    Ok(GatewayCreds { account, token })
}

/// A secret as the signature needs it: nothing around it, and not empty.
///
/// A key placed in a file by hand ends in a newline — that is what `>` and
/// every editor do — and a newline inside the signing key is a `403
/// SignatureDoesNotMatch` with nothing on screen to explain it. The
/// environment and the `bucket` file are trimmed where they are read; this is
/// the same courtesy for the platform's secret store, which hands back
/// whatever was put in it.
fn clean_secret(s: String) -> Option<String> {
    let s = s.trim().to_string();
    (!s.is_empty()).then_some(s)
}

/// A non-empty environment variable, trimmed.
fn env(name: &str) -> Option<String> {
    let v = std::env::var(name).ok()?.trim().to_string();
    (!v.is_empty()).then_some(v)
}

/// The access key id this device is configured with, for a form to show —
/// never the secret, which is write-only by design.
#[must_use]
pub fn configured_key_id(dir: Option<&Path>) -> String {
    env(ENV_KEY)
        .or_else(|| from_file(dir).get(1).cloned())
        .unwrap_or_default()
}

/// Where the `bucket` file lives beside a store.
#[must_use]
pub fn config_path(dir: &Path) -> std::path::PathBuf {
    dir.join("bucket")
}

/// The `bucket` file's contents for a URL and key id. The token is
/// deliberately **not** written: the form puts it in the platform's secret
/// store instead, which is the whole reason the form exists. A file pushed by
/// hand may still carry the token's value on line 3 — this is what replaces
/// it.
#[must_use]
pub fn config_bytes(url: &str, key_id: &str) -> Vec<u8> {
    let mut out = format!("{}\n", url.trim());
    if !key_id.trim().is_empty() {
        out.push_str(&format!("{}\n", key_id.trim()));
    }
    out.into_bytes()
}

/// Whether this configuration would open — the URL parses, and a secret for
/// `key_id` can be found. Nothing is written and nothing is contacted: it is
/// the check a form runs *before* persisting what the user typed, so a typo
/// never becomes the thing the next launch reads.
///
/// Either shape of secret passes: the token's value, which is what a form
/// takes now, and the S3 hash a device filed before it did.
///
/// # Errors
///
/// With the same sentence [`open`] would fail with.
pub fn check(
    url: &str,
    dir: Option<&Path>,
    key_id: &str,
    secrets: &mut dyn Secrets,
) -> Result<(), String> {
    if !url.trim().starts_with("https://") {
        return Ok(()); // the local daemon: no credentials to find
    }
    let secret = env(ENV_SECRET)
        .or_else(|| from_file(dir).get(2).cloned())
        .or_else(|| secrets.get(&secret_key(key_id)))
        .and_then(clean_secret)
        .ok_or_else(|| {
            format!(
                "no secret for {key_id} — type it in, run `superapp --r2-login`, \
                 or set {ENV_SECRET}"
            )
        })?;
    R2::new(
        url,
        Creds {
            key_id: key_id.to_string(),
            secret: s3_secret(&secret),
            region: env(ENV_REGION).unwrap_or_else(|| R2_REGION.to_string()),
        },
    )
    .map(|_| ())
}

/// The bucket for a URL: `https://…` is R2 (signed, over TLS), anything else
/// is the plain `bucketd` client. One door, so the app's start-up path does
/// not branch on transports.
///
/// # Errors
///
/// If an `https://` URL is malformed or its credentials cannot be found.
pub fn open(
    url: &str,
    dir: Option<&Path>,
    secrets: &mut dyn Secrets,
) -> Result<Arc<dyn Object>, String> {
    if url.trim().starts_with("https://") {
        Ok(Arc::new(R2::new(url, creds(dir, secrets)?)?))
    } else {
        Ok(Arc::new(object::HttpBucket::new(url)))
    }
}

/// A bucket that refuses every verb with the same sentence — the reason its
/// real counterpart could not be built.
///
/// A device configured for sync whose credentials have gone missing must not
/// quietly become a *local* device: [`crate::store`] opens writable, so a
/// follower that simply loses its worker would come back as a writer outside
/// the lease, which is the one thing the whole design exists to prevent.
/// Handing the worker this instead keeps the ordinary path — every pass
/// fails, the role falls to `Offline`, a joined device stays locked, and the
/// reason reaches the screen rather than only the console.
pub struct Broken(pub String);

impl Object for Broken {
    fn get(&self, _key: &str) -> Result<Option<Blob>, String> {
        Err(self.0.clone())
    }
    fn put_new(&self, _key: &str, _body: &[u8]) -> Result<PutNew, String> {
        Err(self.0.clone())
    }
    fn cas(&self, _key: &str, _body: &[u8], _etag: &str) -> Result<Cas, String> {
        Err(self.0.clone())
    }
    /// Nothing to poll for, but the role is re-derived each pass and the
    /// worker is what keeps the gate shut: slowly, then.
    fn poll_every(&self) -> Duration {
        Duration::from_secs(30)
    }
}

// -- the bucket ----------------------------------------------------------------

/// A bucket at an S3-compatible endpoint. Connection-per-request, like its
/// plain-HTTP sibling: a sync pass makes two or three requests every couple
/// of seconds, and a pool would buy less than the half-closed connections it
/// would have to reason about.
pub struct R2 {
    host: String,
    port: u16,
    /// The first path segment of the endpoint URL.
    bucket: String,
    /// Everything after it, `""` or slash-terminated — a lineage can live in
    /// a subdirectory of a shared bucket.
    prefix: String,
    creds: Creds,
}

impl R2 {
    /// From `https://<account>.r2.cloudflarestorage.com/<bucket>[/<prefix>]`.
    ///
    /// # Errors
    ///
    /// If the URL is not https, or names no bucket.
    pub fn new(url: &str, creds: Creds) -> Result<R2, String> {
        let url = url.trim();
        let rest = url
            .strip_prefix("https://")
            .ok_or_else(|| format!("a real bucket needs an https:// endpoint, not {url:?}"))?;
        let (authority, path) = rest.split_once('/').unwrap_or((rest, ""));
        let (host, port) = match authority.rsplit_once(':') {
            Some((h, p)) if p.chars().all(|c| c.is_ascii_digit()) && !p.is_empty() => (
                h.to_string(),
                p.parse().map_err(|_| format!("bad port in {url:?}"))?,
            ),
            _ => (authority.to_string(), 443u16),
        };
        if host.is_empty() {
            return Err(format!("no host in {url:?}"));
        }
        let mut segs = path.split('/').filter(|s| !s.is_empty());
        let bucket = segs
            .next()
            .ok_or_else(|| format!("no bucket in {url:?} — expected https://<host>/<bucket>"))?
            .to_string();
        let rest: Vec<&str> = segs.collect();
        let prefix = if rest.is_empty() {
            String::new()
        } else {
            format!("{}/", rest.join("/"))
        };
        Ok(R2 {
            host,
            port,
            bucket,
            prefix,
            creds,
        })
    }

    /// The endpoint, as a status line would say it (never the credentials).
    #[must_use]
    pub fn endpoint(&self) -> String {
        format!("https://{}/{}/{}", self.host, self.bucket, self.prefix)
    }

    /// The request path for an object key: the bucket, the lineage prefix,
    /// the key — URI-encoded as the signature will encode it.
    fn key_path(&self, key: &str) -> String {
        uri_encode(&format!("/{}/{}{key}", self.bucket, self.prefix), false)
    }

    /// One signed request, over one TLS connection.
    fn send(
        &self,
        method: &str,
        path: &str,
        query: &str,
        extra: &[(&str, &str)],
        body: &[u8],
    ) -> Result<object::Reply, String> {
        let (amz_date, day) = amz_time(SystemTime::now());
        let payload = sha256_hex(body);
        let host = if self.port == 443 {
            self.host.clone()
        } else {
            format!("{}:{}", self.host, self.port)
        };
        // Every header we send is signed, preconditions included: the CAS
        // that carries the lease should not be something a middlebox can
        // quietly drop.
        let mut headers = vec![
            ("host".to_string(), host),
            ("x-amz-content-sha256".to_string(), payload.clone()),
            ("x-amz-date".to_string(), amz_date.clone()),
        ];
        headers.extend(extra.iter().map(|(k, v)| ((*k).to_string(), (*v).to_string())));
        let auth = authorization(
            &self.creds,
            "s3",
            &day,
            &amz_date,
            method,
            path,
            query,
            &headers,
            &payload,
        );
        headers.push(("authorization".to_string(), auth));

        let target = if query.is_empty() {
            path.to_string()
        } else {
            format!("{path}?{query}")
        };
        let mut io = self.connect()?;
        // Name the endpoint on the way out: a TLS or socket failure reaches a
        // status line, where "received fatal alert: HandshakeFailure" alone
        // says nothing about which host refused us.
        object::round_trip(&mut io, method, &target, &headers, body)
            .map_err(|e| format!("bucket {}: {e}", self.host))
    }

    /// One request, with the endpoint's "ask again" answers retried.
    ///
    /// R2 limits writes to the *same key* to roughly one a second, and the
    /// lease lives in one key by design — a release right after a publish is
    /// exactly the pattern that earns a `429`. S3 answers `409
    /// ConditionalRequestConflict` for the same reason: two conditional
    /// writes met, and the loser is meant to ask again rather than conclude
    /// anything. Neither is an answer to the question we asked, and treating
    /// them as one is how a lease stays held through a shutdown.
    ///
    /// Retrying is safe for every verb here because every write carries a
    /// precondition: a retry either wins or comes back `412`, which is an
    /// answer.
    fn send_retrying(
        &self,
        method: &str,
        path: &str,
        query: &str,
        extra: &[(&str, &str)],
        body: &[u8],
    ) -> Result<object::Reply, String> {
        // Short, and bounded: `release` runs on the way out of the app, where
        // a long wait is its own kind of failure.
        const BACKOFF_MS: [u64; 3] = [200, 600, 1200];
        let mut last = self.send(method, path, query, extra, body)?;
        for wait in BACKOFF_MS {
            if !matches!(last.0, 409 | 429 | 500 | 502 | 503 | 504) {
                return Ok(last);
            }
            std::thread::sleep(Duration::from_millis(wait));
            last = self.send(method, path, query, extra, body)?;
        }
        Ok(last)
    }

    /// A TLS connection to the endpoint, verified against the Mozilla roots.
    fn connect(&self) -> Result<rustls::StreamOwned<rustls::ClientConnection, TcpStream>, String> {
        let sock = TcpStream::connect((self.host.as_str(), self.port))
            .map_err(|e| format!("{}:{}: {e}", self.host, self.port))?;
        sock.set_read_timeout(Some(TIMEOUT))
            .and_then(|()| sock.set_write_timeout(Some(TIMEOUT)))
            .map_err(|e| e.to_string())?;
        let name = rustls::pki_types::ServerName::try_from(self.host.clone())
            .map_err(|_| format!("not a server name: {}", self.host))?;
        let conn = rustls::ClientConnection::new(tls_config(), name)
            .map_err(|e| format!("tls to {}: {e}", self.host))?;
        Ok(rustls::StreamOwned::new(conn, sock))
    }

    /// What a refusal means, in a line a status bar can hold. S3 answers
    /// errors as XML; the `<Code>` is the part worth repeating —
    /// `SignatureDoesNotMatch` and `AccessDenied` are configuration, not
    /// weather, and they should not read as "the network is down".
    fn refused(&self, what: &str, key: &str, status: u16, body: &[u8]) -> String {
        match xml_tag(body, "Code") {
            Some(code) => format!("bucket {what} {key}: {status} {code}"),
            None => format!("bucket {what} {key}: status {status}"),
        }
    }

    /// Every key under a prefix, following continuation tokens. Not part of
    /// the [`Object`] contract — sync never lists — but a demo that made a
    /// lineage in someone's real bucket should be able to clean it up.
    ///
    /// # Errors
    ///
    /// If the endpoint refuses or answers unparseably.
    pub fn list(&self, prefix: &str) -> Result<Vec<String>, String> {
        let full = format!("{}{prefix}", self.prefix);
        let mut out = Vec::new();
        let mut token: Option<String> = None;
        loop {
            // Query parameters are signed in sorted order:
            // continuation-token < list-type < prefix.
            let mut query = String::new();
            if let Some(t) = &token {
                query.push_str(&format!("continuation-token={}&", uri_encode(t, true)));
            }
            query.push_str(&format!("list-type=2&prefix={}", uri_encode(&full, true)));
            let path = uri_encode(&format!("/{}", self.bucket), false);
            let (status, _, body) = self.send_retrying("GET", &path, &query, &[], &[])?;
            if status != 200 {
                return Err(self.refused("LIST", prefix, status, &body));
            }
            let text = String::from_utf8_lossy(&body).to_string();
            for key in xml_tags(&text, "Key") {
                // Answer keys as the caller names them: prefix-relative.
                out.push(key.strip_prefix(&self.prefix).unwrap_or(&key).to_string());
            }
            if xml_tag(&body, "IsTruncated").as_deref() != Some("true") {
                return Ok(out);
            }
            token = xml_tag(&body, "NextContinuationToken");
            if token.is_none() {
                return Ok(out);
            }
        }
    }

    /// Removes one object. Also outside the [`Object`] contract — the log is
    /// append-only and sync never deletes — and for the same reason.
    ///
    /// # Errors
    ///
    /// If the endpoint refuses.
    pub fn delete(&self, key: &str) -> Result<(), String> {
        let (status, _, body) = self.send_retrying("DELETE", &self.key_path(key), "", &[], &[])?;
        match status {
            200 | 204 | 404 => Ok(()),
            other => Err(self.refused("DELETE", key, other, &body)),
        }
    }
}

impl Object for R2 {
    fn get(&self, key: &str) -> Result<Option<Blob>, String> {
        let (status, etag, body) = self.send_retrying("GET", &self.key_path(key), "", &[], &[])?;
        match status {
            200 => Ok(Some(Blob {
                bytes: body,
                etag: etag.unwrap_or_default(),
            })),
            // A missing *bucket* also answers 404, and it is not the same
            // question: "no object yet" is what makes a device bootstrap a
            // lineage, and a typo in the bucket name would send it around
            // that loop forever, creating nothing each time.
            404 if xml_tag(&body, "Code").as_deref() == Some("NoSuchBucket") => {
                Err(self.refused("GET", key, status, &body))
            }
            404 => Ok(None),
            other => Err(self.refused("GET", key, other, &body)),
        }
    }

    fn put_new(&self, key: &str, body: &[u8]) -> Result<PutNew, String> {
        let (status, etag, resp) = self.send_retrying(
            "PUT",
            &self.key_path(key),
            "",
            &[("if-none-match", "*")],
            body,
        )?;
        match status {
            200 | 201 => Ok(PutNew::Created(etag.unwrap_or_default())),
            412 => Ok(PutNew::Exists),
            // A 409 that outlived its retries is *not* "it exists": read as
            // one, a snapshot upload would report success and `state` would
            // come to point at an object nobody wrote.
            other => Err(self.refused("PUT", key, other, &resp)),
        }
    }

    /// Slower than the local default: an idle follower polling every 1.5s
    /// would spend some two million class-B operations a month asking a
    /// question whose answer almost never changed. Five seconds is still
    /// under the time it takes to walk to the other device, and a write on
    /// the holder publishes immediately either way.
    fn poll_every(&self) -> Duration {
        Duration::from_secs(5)
    }

    fn cas(&self, key: &str, body: &[u8], etag: &str) -> Result<Cas, String> {
        let (status, new_etag, resp) = self.send_retrying(
            "PUT",
            &self.key_path(key),
            "",
            &[("if-match", etag)],
            body,
        )?;
        match status {
            200 | 201 => Ok(Cas::Ok(new_etag.unwrap_or_default())),
            // 412: the stored ETag moved — someone else advanced the log,
            // and the next pass re-reads. A 409 that outlived its retries is
            // a different thing (nobody won) and is said as one.
            412 => Ok(Cas::Mismatch),
            other => Err(self.refused("CAS", key, other, &resp)),
        }
    }
}

/// The client TLS configuration, built once: the Mozilla root set, the `ring`
/// provider (chosen explicitly rather than inherited from whatever the
/// process installed as its default), and no client certificate.
fn tls_config() -> Arc<rustls::ClientConfig> {
    static CFG: OnceLock<Arc<rustls::ClientConfig>> = OnceLock::new();
    CFG.get_or_init(|| {
        let mut roots = rustls::RootCertStore::empty();
        roots.extend(webpki_roots::TLS_SERVER_ROOTS.iter().cloned());
        let cfg = rustls::ClientConfig::builder_with_provider(Arc::new(
            rustls::crypto::ring::default_provider(),
        ))
        .with_safe_default_protocol_versions()
        .expect("ring supports the default protocol versions")
        .with_root_certificates(roots)
        .with_no_client_auth();
        Arc::new(cfg)
    })
    .clone()
}

// -- SigV4 ---------------------------------------------------------------------

/// The `Authorization` value for one signed request.
///
/// Pure: the clock, the socket and the credentials all arrive as arguments,
/// which is what lets the AWS test vector in the tests below pin it exactly.
/// `headers` are the ones that will be sent (names in any case); every one of
/// them is signed.
#[allow(clippy::too_many_arguments)]
fn authorization(
    creds: &Creds,
    service: &str,
    day: &str,
    amz_date: &str,
    method: &str,
    canonical_uri: &str,
    canonical_query: &str,
    headers: &[(String, String)],
    payload_hash: &str,
) -> String {
    let mut h: Vec<(String, String)> = headers
        .iter()
        .map(|(k, v)| (k.to_ascii_lowercase(), canon_value(v)))
        .collect();
    h.sort_by(|a, b| a.0.cmp(&b.0));
    let canonical_headers: String = h.iter().map(|(k, v)| format!("{k}:{v}\n")).collect();
    let signed = h
        .iter()
        .map(|(k, _)| k.as_str())
        .collect::<Vec<_>>()
        .join(";");

    let canonical_request = format!(
        "{method}\n{canonical_uri}\n{canonical_query}\n{canonical_headers}\n{signed}\n{payload_hash}"
    );
    let scope = format!("{day}/{}/{service}/aws4_request", creds.region);
    let to_sign = format!(
        "AWS4-HMAC-SHA256\n{amz_date}\n{scope}\n{}",
        sha256_hex(canonical_request.as_bytes())
    );

    // The signing key is the secret walked through the scope, one HMAC per
    // step, so a leaked signature never reaches back to the secret itself.
    let mut key = hmac(format!("AWS4{}", creds.secret).as_bytes(), day.as_bytes());
    key = hmac(&key, creds.region.as_bytes());
    key = hmac(&key, service.as_bytes());
    key = hmac(&key, b"aws4_request");
    let signature = hex(&hmac(&key, to_sign.as_bytes()));

    format!(
        "AWS4-HMAC-SHA256 Credential={}/{scope}, SignedHeaders={signed}, Signature={signature}",
        creds.key_id
    )
}

/// A header value in canonical form: trimmed, with runs of whitespace
/// collapsed to one space.
fn canon_value(v: &str) -> String {
    let mut out = String::with_capacity(v.len());
    let mut space = false;
    for c in v.trim().chars() {
        if c.is_ascii_whitespace() {
            space = true;
        } else {
            if space && !out.is_empty() {
                out.push(' ');
            }
            space = false;
            out.push(c);
        }
    }
    out
}

/// RFC 3986 percent-encoding, as SigV4 wants it: unreserved characters pass,
/// everything else is `%XX` in upper case. S3 signs the path encoded **once**
/// (other AWS services encode it twice), so `/` stays a separator there and
/// becomes `%2F` inside a query value.
fn uri_encode(s: &str, encode_slash: bool) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(b as char);
            }
            b'/' if !encode_slash => out.push('/'),
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

/// `("20260902T101112Z", "20260902")` — the two forms one request needs.
/// Hand-rolled rather than pulling a date crate in for eight fields: the
/// civil-from-days step is Howard Hinnant's, exact for any Unix day.
fn amz_time(t: SystemTime) -> (String, String) {
    let secs = t
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let (days, tod) = ((secs / 86_400) as i64, secs % 86_400);
    let (y, m, d) = civil_from_days(days);
    let (hh, mm, ss) = (tod / 3600, (tod / 60) % 60, tod % 60);
    (
        format!("{y:04}{m:02}{d:02}T{hh:02}{mm:02}{ss:02}Z"),
        format!("{y:04}{m:02}{d:02}"),
    )
}

/// Days since the Unix epoch to `(year, month, day)`, proleptic Gregorian.
fn civil_from_days(z: i64) -> (i64, u64, u64) {
    let z = z + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = (z - era * 146_097) as u64; // [0, 146096]
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365; // [0, 399]
    let y = yoe as i64 + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100); // [0, 365]
    let mp = (5 * doy + 2) / 153; // [0, 11], March-based
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    (y + i64::from(m <= 2), m, d)
}

/// SHA-256, hex — the payload hash every S3 request carries.
fn sha256_hex(bytes: &[u8]) -> String {
    hex(ring::digest::digest(&ring::digest::SHA256, bytes).as_ref())
}

/// HMAC-SHA256, raw.
fn hmac(key: &[u8], msg: &[u8]) -> Vec<u8> {
    let k = ring::hmac::Key::new(ring::hmac::HMAC_SHA256, key);
    ring::hmac::sign(&k, msg).as_ref().to_vec()
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// The text of the first `<tag>…</tag>`, for the two-field XML we care about:
/// an error's `<Code>`, a listing's continuation token. Not an XML parser —
/// a scanner, deliberately, for two shapes S3 documents exactly.
fn xml_tag(body: &[u8], tag: &str) -> Option<String> {
    let text = std::str::from_utf8(body).ok()?;
    xml_tags(text, tag).into_iter().next()
}

/// Every `<tag>…</tag>` text in order — a listing's keys.
fn xml_tags(text: &str, tag: &str) -> Vec<String> {
    let (open, close) = (format!("<{tag}>"), format!("</{tag}>"));
    let mut out = Vec::new();
    let mut rest = text;
    while let Some(i) = rest.find(&open) {
        let after = &rest[i + open.len()..];
        let Some(j) = after.find(&close) else { break };
        out.push(after[..j].to_string());
        rest = &after[j + close.len()..];
    }
    out
}

// -- --r2-login ----------------------------------------------------------------

/// `superapp --r2-login`: read the Cloudflare API token's value from
/// **stdin** and put it where the platform keeps secrets — the macOS
/// keychain, or a private file beside the store. Answers the process exit
/// code, or `None` when the flag was not given (in which case the app starts
/// normally).
///
/// The value, not the S3 secret access key the dashboard shows beside it:
/// the key is the value's hash ([`s3_secret`] takes it from here), and this
/// device has more than a bucket to open with it.
///
/// Stdin, not a flag: an argument is in `ps` and in the shell's history, and
/// this one key can write the whole lineage.
#[must_use]
pub fn login_from_argv(secrets: &mut dyn Secrets) -> Option<i32> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if !args.iter().any(|a| a == "--r2-login") {
        return None;
    }
    // The store's directory, for the file fallback and for reading the key id
    // out of a `bucket` file: `--db PATH`'s parent, when given.
    let dir = args
        .iter()
        .position(|a| a == "--db")
        .and_then(|i| args.get(i + 1))
        .map(std::path::PathBuf::from)
        .and_then(|p| p.parent().map(std::path::Path::to_path_buf));

    let key_id = match env(ENV_KEY)
        .or_else(|| from_file(dir.as_deref()).get(1).cloned())
        .or_else(|| secrets.get(KEY_ID_KEY).and_then(clean_secret))
    {
        Some(k) => k,
        None => {
            eprintln!(
                "--r2-login: no access key id — set {ENV_KEY} (once: it is remembered), \
                 or put it on line 2 of the `bucket` file beside the store"
            );
            return Some(2);
        }
    };
    eprintln!("cloudflare api token for {key_id} — its value, not the S3 hash (from stdin):");
    let mut secret = String::new();
    if std::io::stdin().read_line(&mut secret).is_err() {
        eprintln!("--r2-login: could not read the token");
        return Some(2);
    }
    let secret = secret.trim();
    if secret.is_empty() {
        eprintln!("--r2-login: the token is empty — nothing stored");
        return Some(2);
    }
    if file_login(secrets, &key_id, secret) {
        eprintln!("--r2-login: stored the token for {key_id}");
        Some(0)
    } else {
        eprintln!("--r2-login: the platform refused to store it");
        Some(2)
    }
}

/// Files a token under its key id, and the key id under [`KEY_ID_KEY`], so
/// the next run — and the next login — need neither the environment nor the
/// `bucket` file to know which token this device holds.
pub fn file_login(secrets: &mut dyn Secrets, key_id: &str, token: &str) -> bool {
    secrets.set(&secret_key(key_id), token) && secrets.set(KEY_ID_KEY, key_id)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::caps::MemSecrets;

    /// The AWS SigV4 test suite's `get-vanilla` case, byte for byte. It fixes
    /// every step at once — canonical request, scope, signing key, signature —
    /// so a change to any of them fails here rather than at a real endpoint
    /// with a `SignatureDoesNotMatch` and no clue which step drifted.
    #[test]
    fn the_signature_matches_the_aws_test_vector() {
        let creds = Creds {
            key_id: "AKIDEXAMPLE".into(),
            secret: "wJalrXUtnFEMI/K7MDENG+bPxRfiCYEXAMPLEKEY".into(),
            region: "us-east-1".into(),
        };
        let headers = vec![
            ("Host".to_string(), "example.amazonaws.com".to_string()),
            ("X-Amz-Date".to_string(), "20150830T123600Z".to_string()),
        ];
        let auth = authorization(
            &creds,
            "service",
            "20150830",
            "20150830T123600Z",
            "GET",
            "/",
            "",
            &headers,
            // The suite's empty-payload hash.
            "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855",
        );
        assert_eq!(
            auth,
            "AWS4-HMAC-SHA256 Credential=AKIDEXAMPLE/20150830/us-east-1/service/aws4_request, \
             SignedHeaders=host;x-amz-date, \
             Signature=5fa00fa31553b73ebf1942676e86291e8372ff2a2260956d9b8aae1d763fbf31"
        );
    }

    /// A signed PUT with preconditions: the header set and the ordering are
    /// what R2 will recompute, and both conditional headers ride inside it.
    #[test]
    fn the_conditional_headers_are_signed() {
        let creds = Creds {
            key_id: "AKIDEXAMPLE".into(),
            secret: "secret".into(),
            region: R2_REGION.into(),
        };
        let headers = vec![
            ("host".to_string(), "acct.r2.cloudflarestorage.com".to_string()),
            ("x-amz-content-sha256".to_string(), sha256_hex(b"body")),
            ("x-amz-date".to_string(), "20260902T101112Z".to_string()),
            ("if-match".to_string(), "\"abc\"".to_string()),
        ];
        let auth = authorization(
            &creds,
            "s3",
            "20260902",
            "20260902T101112Z",
            "PUT",
            "/bucket/state",
            "",
            &headers,
            &sha256_hex(b"body"),
        );
        assert!(
            auth.contains("SignedHeaders=host;if-match;x-amz-content-sha256;x-amz-date"),
            "{auth}"
        );
        assert!(auth.contains("Credential=AKIDEXAMPLE/20260902/auto/s3/aws4_request"));
    }

    /// The pieces the signature is built from, pinned individually.
    #[test]
    fn the_signing_primitives_hold() {
        assert_eq!(
            sha256_hex(b""),
            "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
        );
        // S3 signs the object path once: slashes stay, everything else goes.
        assert_eq!(uri_encode("/b/log/3/dev a/1-2", false), "/b/log/3/dev%20a/1-2");
        // A query *value* encodes its slashes.
        assert_eq!(uri_encode("demo/1/", true), "demo%2F1%2F");
        assert_eq!(canon_value("  a   b  "), "a b");
    }

    /// The clock, in the two shapes a request needs. Fixed instants, so a
    /// leap-year or month-boundary slip cannot pass.
    #[test]
    fn the_clock_formats_as_amz_wants() {
        let at = |s: u64| amz_time(UNIX_EPOCH + Duration::from_secs(s));
        assert_eq!(at(0), ("19700101T000000Z".into(), "19700101".into()));
        // 2015-08-30T12:36:00Z — the test vector's instant.
        assert_eq!(at(1_440_938_160), ("20150830T123600Z".into(), "20150830".into()));
        // 2024-02-29T23:59:59Z — a leap day, at the last second.
        assert_eq!(at(1_709_251_199), ("20240229T235959Z".into(), "20240229".into()));
    }

    /// The endpoint URL splits into bucket and lineage prefix, and the key
    /// path is built from both.
    #[test]
    fn the_endpoint_splits_into_bucket_and_prefix() {
        let creds = Creds {
            key_id: "k".into(),
            secret: "s".into(),
            region: R2_REGION.into(),
        };
        let b = R2::new(
            "https://acct.r2.cloudflarestorage.com/superapp/demo-7",
            creds.clone(),
        )
        .unwrap();
        assert_eq!(b.host, "acct.r2.cloudflarestorage.com");
        assert_eq!(b.port, 443);
        assert_eq!(b.bucket, "superapp");
        assert_eq!(b.prefix, "demo-7/");
        assert_eq!(b.key_path("log/3/dev/1-2"), "/superapp/demo-7/log/3/dev/1-2");

        // No prefix: the lineage is the bucket root.
        let plain = R2::new("https://acct.r2.cloudflarestorage.com/superapp", creds.clone()).unwrap();
        assert_eq!(plain.prefix, "");
        assert_eq!(plain.key_path("state"), "/superapp/state");

        // Refusals, both of them spoken.
        assert!(R2::new("http://acct.r2.cloudflarestorage.com/b", creds.clone()).is_err());
        assert!(R2::new("https://acct.r2.cloudflarestorage.com", creds).is_err());
    }

    /// The file the form writes: the URL, the key id, and no secret.
    #[test]
    fn the_config_file_never_carries_the_secret() {
        assert_eq!(
            String::from_utf8(config_bytes(" https://h/b ", " AK ")).unwrap(),
            "https://h/b\nAK\n"
        );
        // No key id (the local daemon needs none): one line, and nothing
        // that could be mistaken for one.
        assert_eq!(
            String::from_utf8(config_bytes("http://127.0.0.1:9000", "")).unwrap(),
            "http://127.0.0.1:9000\n"
        );
    }

    /// A secret that came back from a store with a newline on it still
    /// signs. Found on a real emulator: the key was right, the file was
    /// right, and every request came back 403 with nothing to see.
    #[test]
    fn a_secret_is_taken_without_what_surrounds_it() {
        assert_eq!(clean_secret("abc123\n".into()).as_deref(), Some("abc123"));
        assert_eq!(clean_secret("  abc123  \r\n".into()).as_deref(), Some("abc123"));
        assert_eq!(clean_secret("abc123".into()).as_deref(), Some("abc123"));
        // Nothing but whitespace is nothing: the caller says "no secret" and
        // names where to put one, instead of signing with the empty string.
        assert_eq!(clean_secret("\n".into()), None);
        assert_eq!(clean_secret(String::new()), None);
    }

    /// A real R2 endpoint, and a token of the shape Cloudflare mints: 40
    /// characters of `[A-Za-z0-9_-]`.
    const URL: &str = "https://acc7.r2.cloudflarestorage.com/superapp";
    const TOKEN: &str = "3Zt_qL9xN2mWv0bYcE8sRfKu-1TgHjAd6PoQiXeZ";

    /// A directory with a `bucket` file in it, as a store's directory has.
    fn bucket_dir(what: &str, lines: &[&str]) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "superapp-r2-{what}-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        if lines.is_empty() {
            let _ = std::fs::remove_file(dir.join("bucket"));
        } else {
            std::fs::write(dir.join("bucket"), lines.join("\n")).unwrap();
        }
        dir
    }

    /// The one fact this whole arrangement rests on: what is filed is the
    /// token's value, what signs is its SHA-256, and the gateway gets the
    /// value — so one credential opens both and neither is stored twice.
    #[test]
    fn the_filed_token_signs_as_its_hash_and_is_borne_whole() {
        let dir = bucket_dir("token", &[URL, "AK", TOKEN]);
        let mut secrets = MemSecrets::new();

        let c = creds(Some(&dir), &mut secrets).unwrap();
        assert_eq!(c.key_id, "AK");
        assert_eq!(c.secret, sha256_hex(TOKEN.as_bytes()));
        assert_ne!(c.secret, TOKEN);

        let g = gateway(None, Some(&dir), &mut secrets).unwrap();
        assert_eq!(g.token, TOKEN);
        assert_eq!(g.account, "acc7");
        // And it is not the sort of thing that lands in a log.
        assert!(!format!("{g:?}").contains(TOKEN), "{g:?}");
    }

    /// The keychain of a device configured before any of this: it holds the
    /// hash, which still signs — the sync it has never breaks — and cannot be
    /// turned back into a token, so the gateway says exactly that.
    #[test]
    fn a_secret_filed_as_the_hash_still_signs_and_opens_no_gateway() {
        let hash = sha256_hex(TOKEN.as_bytes());
        let dir = bucket_dir("hash", &[URL, "AK"]);
        let mut secrets = MemSecrets::new();
        secrets.plant(&secret_key("AK"), &hash);

        assert_eq!(creds(Some(&dir), &mut secrets).unwrap().secret, hash);
        assert_eq!(
            gateway(None, Some(&dir), &mut secrets).unwrap_err(),
            "the secret for AK is the S3 hash, not the token — run `superapp --r2-login` \
             again, with the token's value"
        );
    }

    /// What the gateway says when it cannot be opened — each sentence naming
    /// the place the missing thing goes, because a run that quietly cannot
    /// ask anything is the worst outcome here too.
    #[test]
    fn the_gateway_says_what_it_is_missing() {
        let mut secrets = MemSecrets::new();

        // No bucket at all: no host, so no account.
        let none = bucket_dir("no-bucket", &[]);
        assert_eq!(
            gateway(None, Some(&none), &mut secrets).unwrap_err(),
            "no bucket — the gateway's account is read off the bucket's host: \
             run with `--bucket URL`, set SUPERAPP_BUCKET, or put it on line 1 of the `bucket` file"
        );

        // The local daemon is a bucket, but it is nobody's Cloudflare account.
        let local = bucket_dir("local", &["http://127.0.0.1:9299", "AK", TOKEN]);
        assert_eq!(
            gateway(None, Some(&local), &mut secrets).unwrap_err(),
            "the gateway's account cannot be read off \"127.0.0.1\" — it is the first \
             label of an R2 host, <account>.r2.cloudflarestorage.com"
        );

        // A real bucket, and nothing filed for its key.
        let bare = bucket_dir("bare", &[URL, "AK"]);
        assert_eq!(
            gateway(None, Some(&bare), &mut secrets).unwrap_err(),
            "no secret for AK — run `superapp --r2-login`, set \
             SUPERAPP_R2_SECRET_ACCESS_KEY, or put it on line 3 of the `bucket` file"
        );
    }

    /// A laptop that never joined a bucket: `--r2-login` remembered the key
    /// id beside the token, so the token is found with no file and no
    /// environment, and the account comes off whatever bucket the shell was
    /// pointed at — `--bucket` included — or off nothing, which is the app's
    /// cue to ask Cloudflare.
    #[test]
    fn a_login_remembers_its_key_id_for_a_device_with_no_bucket() {
        let mut secrets = MemSecrets::new();
        assert!(file_login(&mut secrets, "AK", TOKEN));
        let none = bucket_dir("no-file-at-all", &[]);
        let (k, t) = gateway_token(Some(&none), &mut secrets).unwrap();
        assert_eq!((k.as_str(), t.as_str()), ("AK", TOKEN));
        assert_eq!(creds(Some(&none), &mut secrets).unwrap().key_id, "AK");

        assert_eq!(account_from(None, Some(&none)), None);
        assert_eq!(account_from(Some(""), Some(&none)), None);
        assert_eq!(account_from(Some(URL), Some(&none)).as_deref(), Some("acc7"));
        assert_eq!(account_from(Some("http://127.0.0.1:9299"), Some(&none)), None);
        let g = gateway(Some(URL), Some(&none), &mut secrets).unwrap();
        assert_eq!((g.account.as_str(), g.token.as_str()), ("acc7", TOKEN));
    }

    /// The account is the first label of an R2 host, and nothing else is an
    /// R2 host.
    #[test]
    fn the_account_is_read_off_an_r2_host_only() {
        assert_eq!(
            account_of("https://acc7.r2.cloudflarestorage.com/superapp/demo-7").as_deref(),
            Some("acc7")
        );
        assert_eq!(
            account_of("  https://acc7.r2.cloudflarestorage.com  ").as_deref(),
            Some("acc7")
        );
        // A jurisdiction's endpoint has one more label; the account is still
        // the first one.
        assert_eq!(
            account_of("https://acc7.eu.r2.cloudflarestorage.com/superapp").as_deref(),
            Some("acc7")
        );
        // The local daemon, an ordinary host, a plaintext R2 URL, junk.
        assert_eq!(account_of("http://127.0.0.1:9299"), None);
        assert_eq!(account_of("https://example.com/superapp"), None);
        assert_eq!(account_of("http://acc7.r2.cloudflarestorage.com/b"), None);
        assert_eq!(account_of("not a url"), None);
        assert_eq!(account_of(""), None);
    }

    /// Telling the two shapes apart is the whole of the compatibility rule,
    /// so it is pinned on both of them.
    #[test]
    fn a_hash_is_told_from_a_token_by_its_shape() {
        let hash = sha256_hex(TOKEN.as_bytes());
        assert_eq!(hash.len(), 64);
        assert!(is_hash(&hash));
        assert!(is_hash(&hash.to_ascii_uppercase()));
        // One digit short of a hash is not one.
        assert!(!is_hash(&hash[..63]));
        // A token: 40 characters, and `_` and `-` are not hex.
        assert_eq!(TOKEN.len(), 40);
        assert!(!is_hash(TOKEN));
        assert!(!is_hash(""));
        // Which is what decides whether the value is hashed or passed on.
        assert_eq!(s3_secret(TOKEN), hash);
        assert_eq!(s3_secret(&hash), hash);
    }

    /// A broken bucket answers every verb with its reason — which is what
    /// keeps a follower locked instead of quietly writable.
    #[test]
    fn a_broken_bucket_refuses_everything_with_its_reason() {
        let b = Broken("no secret for AK".to_string());
        assert_eq!(b.get("state").unwrap_err(), "no secret for AK");
        assert_eq!(b.put_new("state", b"x").unwrap_err(), "no secret for AK");
        assert_eq!(b.cas("state", b"x", "e").unwrap_err(), "no secret for AK");
    }

    /// An S3 refusal carries its reason in XML; the status line says it.
    #[test]
    fn a_refusal_is_read_out_of_the_xml() {
        let body = br#"<?xml version="1.0" encoding="UTF-8"?><Error><Code>SignatureDoesNotMatch</Code><Message>no</Message></Error>"#;
        assert_eq!(xml_tag(body, "Code").as_deref(), Some("SignatureDoesNotMatch"));
        assert_eq!(xml_tag(b"<Error/>", "Code"), None);
        let listing = "<ListBucketResult><Contents><Key>a/1</Key></Contents><Contents><Key>a/2</Key></Contents><IsTruncated>false</IsTruncated></ListBucketResult>";
        assert_eq!(xml_tags(listing, "Key"), vec!["a/1", "a/2"]);
    }
}
