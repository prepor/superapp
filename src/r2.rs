//! R2: the bucket, for real.
//!
//! [`crate::object`] states the contract device sync needs — get, create-only
//! put, compare-and-swap — and ships two backends that speak it over a plain
//! socket: an in-process map, and a client for the local `bucketd` daemon.
//! This is the third: **Cloudflare R2** over its S3 API. Same wire, plus the
//! two things a local demo could do without — TLS, and AWS **SigV4** request
//! signing.
//!
//! The compare-and-swap is not emulated. R2 implements S3's conditional
//! writes, so `If-None-Match: *` *is* the create-only put and `If-Match:
//! <etag>` *is* the compare-and-swap, each answered `412` when its
//! precondition loses. The single-writer property the lease rests on — and
//! that `formal/Lease.tla` checks — therefore rests on the object store
//! itself, not on anything this client does.
//!
//! Nothing above this module knows R2 exists: [`open`] answers an
//! `Arc<dyn Object>` from a URL, `https://` signing and `http://` not.

use std::net::TcpStream;
use std::path::Path;
use std::sync::{Arc, OnceLock};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use crate::object::{self, Blob, Cas, Object, PutNew};

/// A request gets this long to connect, send and answer. Generous: a snapshot
/// upload on a slow uplink is a legitimate minute.
const TIMEOUT: Duration = Duration::from_secs(60);

/// The desktop's environment: the access key id, its secret, and (for a
/// non-R2 S3 endpoint) the region to sign under.
const ENV_KEY: &str = "SUPERAPP_R2_ACCESS_KEY_ID";
const ENV_SECRET: &str = "SUPERAPP_R2_SECRET_ACCESS_KEY";
const ENV_REGION: &str = "SUPERAPP_R2_REGION";

/// R2 signs under one region name whatever the bucket's jurisdiction.
const R2_REGION: &str = "auto";

// -- credentials ---------------------------------------------------------------

/// What an S3-compatible endpoint needs to believe a request is ours.
#[derive(Clone)]
pub struct Creds {
    pub key_id: String,
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
/// access key id, line 3 the secret. One file, because a device that is
/// configured by `adb push` (android) has no environment and no keychain —
/// the app sandbox is its perimeter, the same trade [`crate::secret`] makes.
/// Blank lines and `#` comments are skipped, so the file can carry a note.
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

/// The credentials for a real bucket, from — in order — the environment, the
/// `bucket` file's second and third lines, and the platform's secret store
/// (the macOS keychain; see [`crate::secret`]).
///
/// The key id is not a secret and may sit in a file; the secret is looked up
/// last so that a keychain entry, once written by `--r2-login`, is enough.
///
/// # Errors
///
/// If either half cannot be found — with the places we looked, because a
/// device that silently syncs nothing is the worst outcome here.
pub fn creds(dir: Option<&Path>) -> Result<Creds, String> {
    let file = from_file(dir);
    let key_id = env(ENV_KEY)
        .or_else(|| file.get(1).cloned())
        .ok_or_else(|| {
            format!("no access key id — set {ENV_KEY}, or put it on line 2 of the `bucket` file")
        })?;
    let secret = env(ENV_SECRET)
        .or_else(|| file.get(2).cloned())
        .or_else(|| crate::secret::bucket_secret(dir, &key_id))
        .ok_or_else(|| {
            format!(
                "no secret for {key_id} — run `superapp --r2-login`, set {ENV_SECRET}, \
                 or put it on line 3 of the `bucket` file"
            )
        })?;
    Ok(Creds {
        key_id,
        secret,
        region: env(ENV_REGION).unwrap_or_else(|| R2_REGION.to_string()),
    })
}

/// A non-empty environment variable, trimmed.
fn env(name: &str) -> Option<String> {
    let v = std::env::var(name).ok()?.trim().to_string();
    (!v.is_empty()).then_some(v)
}

/// The bucket for a URL: `https://…` is R2 (signed, over TLS), anything else
/// is the plain `bucketd` client. One door, so the app's start-up path does
/// not branch on transports.
///
/// # Errors
///
/// If an `https://` URL is malformed or its credentials cannot be found.
pub fn open(url: &str, dir: Option<&Path>) -> Result<Arc<dyn Object>, String> {
    if url.trim().starts_with("https://") {
        Ok(Arc::new(R2::new(url, creds(dir)?)?))
    } else {
        Ok(Arc::new(object::HttpBucket::new(url)))
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
            let (status, _, body) = self.send("GET", &path, &query, &[], &[])?;
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
        let (status, _, body) = self.send("DELETE", &self.key_path(key), "", &[], &[])?;
        match status {
            200 | 204 | 404 => Ok(()),
            other => Err(self.refused("DELETE", key, other, &body)),
        }
    }
}

impl Object for R2 {
    fn get(&self, key: &str) -> Result<Option<Blob>, String> {
        let (status, etag, body) = self.send("GET", &self.key_path(key), "", &[], &[])?;
        match status {
            200 => Ok(Some(Blob {
                bytes: body,
                etag: etag.unwrap_or_default(),
            })),
            404 => Ok(None),
            other => Err(self.refused("GET", key, other, &body)),
        }
    }

    fn put_new(&self, key: &str, body: &[u8]) -> Result<PutNew, String> {
        let (status, etag, resp) = self.send(
            "PUT",
            &self.key_path(key),
            "",
            &[("if-none-match", "*")],
            body,
        )?;
        match status {
            200 | 201 => Ok(PutNew::Created(etag.unwrap_or_default())),
            // 412: it exists. 409: someone is writing it right now — which,
            // for a create-only put, is the same answer to the same question.
            412 | 409 => Ok(PutNew::Exists),
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
        let (status, new_etag, resp) =
            self.send("PUT", &self.key_path(key), "", &[("if-match", etag)], body)?;
        match status {
            200 | 201 => Ok(Cas::Ok(new_etag.unwrap_or_default())),
            // 412: the stored ETag moved. 409: it is moving as we ask. Either
            // way we did not win this round, and the next pass re-reads.
            412 | 409 => Ok(Cas::Mismatch),
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

/// `superapp --r2-login`: read the secret access key from **stdin** and put it
/// where the platform keeps secrets — the macOS keychain, or a private file
/// beside the store. Answers the process exit code, or `None` when the flag
/// was not given (in which case the app starts normally).
///
/// Stdin, not a flag: an argument is in `ps` and in the shell's history, and
/// this one key can write the whole lineage.
#[must_use]
pub fn login_from_argv() -> Option<i32> {
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

    let key_id = match env(ENV_KEY).or_else(|| from_file(dir.as_deref()).get(1).cloned()) {
        Some(k) => k,
        None => {
            eprintln!(
                "--r2-login: no access key id — set {ENV_KEY}, or put it on line 2 of the \
                 `bucket` file beside the store"
            );
            return Some(2);
        }
    };
    eprintln!("secret access key for {key_id} (from stdin):");
    let mut secret = String::new();
    if std::io::stdin().read_line(&mut secret).is_err() {
        eprintln!("--r2-login: could not read the secret");
        return Some(2);
    }
    let secret = secret.trim();
    if secret.is_empty() {
        eprintln!("--r2-login: the secret is empty — nothing stored");
        return Some(2);
    }
    let dir = dir.unwrap_or_else(std::env::temp_dir);
    if crate::secret::set_bucket_secret(&dir, &key_id, secret) {
        eprintln!("--r2-login: stored the secret for {key_id}");
        Some(0)
    } else {
        eprintln!("--r2-login: the platform refused to store it");
        Some(2)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
