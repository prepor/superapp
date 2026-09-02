//! Object storage: the transport CR-005's replication rides on.
//!
//! One small interface — get, conditional-create, compare-and-swap — is all
//! the lease and the log need. The `state` object is the only mutable one and
//! is advanced with a CAS on its ETag: the write that moves the log *is* the
//! check that we still hold the lease. Batches and snapshots are immutable and
//! written create-only.
//!
//! Three backends ship: [`MemBucket`] (in-process, for tests), [`HttpBucket`]
//! (a plain-HTTP client for the local `bucketd` daemon), and [`crate::r2`]
//! (Cloudflare R2 over its S3 API — the same wire, with TLS and request
//! signing). The last two share the HTTP framing here; only the stream and
//! the headers differ. ETags are opaque per-key version tokens — the client
//! never interprets them, only round-trips them — so a backend is free to use
//! a counter (what `MemBucket` does), a content hash (what `bucketd` and S3
//! do), or anything else.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use serde::{Deserialize, Serialize};

/// The one mutable object: the lease *and* the head pointer, together, so the
/// CAS that advances the log cannot succeed for a device that has lost the
/// lease. Splitting them would let an overridden holder publish into a history
/// it no longer owns.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct State {
    /// Wire version — an unknown value refuses rather than guesses.
    pub v: u32,
    /// The `user_version` this lineage is at. A device whose schema differs
    /// refuses the lease and asks you to update the other device.
    pub schema: i64,
    /// Bumps on every acquisition and every override — the fence.
    pub epoch: i64,
    /// The device that holds the lease, or `None` before anyone has.
    pub holder: Option<String>,
    /// `true` means the holder handed it back: free to acquire.
    pub released: bool,
    /// The global high-water sequence: the last published seq.
    pub seq: i64,
    /// The head batch's key, or `None` at genesis (nothing published yet).
    pub batch: Option<String>,
    /// The lineage's snapshot — how a cold device catches up without replaying
    /// from genesis.
    pub snapshot: Snapshot,
}

/// Where the canonical snapshot lives, and what it should hash to.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Snapshot {
    pub key: String,
    pub seq: i64,
    pub schema: i64,
    pub hash: String,
}

/// The current wire version for every object CR-005 writes.
pub const WIRE_V: u32 = 1;

/// The one mutable object's key.
pub const STATE_KEY: &str = "state";

/// A batch's key: epoch and the writing device make it unique by
/// construction, so an orphan from a failed CAS never squats on a sequence the
/// next holder needs.
#[must_use]
pub fn batch_key(epoch: i64, device: &str, first: i64, last: i64) -> String {
    format!("log/{epoch}/{device}/{first}-{last}")
}

/// A snapshot's key carries schema and content hash, not sequence alone: a
/// migration produces a new snapshot without advancing the log.
#[must_use]
pub fn snap_key(seq: i64, schema: i64, hash: &str) -> String {
    format!("snap/{seq}-{schema}-{hash}.db")
}

/// A stored object: its bytes and the ETag to CAS against.
#[derive(Debug, Clone)]
pub struct Blob {
    pub bytes: Vec<u8>,
    pub etag: String,
}

/// The outcome of a conditional create.
#[derive(Debug, PartialEq)]
pub enum PutNew {
    /// Written; here is its ETag.
    Created(String),
    /// The key already exists (someone else, or our own earlier attempt).
    Exists,
}

/// The outcome of a compare-and-swap.
#[derive(Debug, PartialEq)]
pub enum Cas {
    /// Written; here is the new ETag.
    Ok(String),
    /// The stored ETag did not match — someone advanced it first.
    Mismatch,
}

/// The transport. Object-safe and `Send + Sync`, because the replication
/// worker owns one on its own thread.
pub trait Object: Send + Sync {
    /// The object at `key`, with its ETag, or `None` if absent.
    ///
    /// # Errors
    ///
    /// If the backend is unreachable or answers malformed.
    fn get(&self, key: &str) -> Result<Option<Blob>, String>;

    /// Create `key` only if it does not exist (`If-None-Match: *`).
    ///
    /// # Errors
    ///
    /// If the backend is unreachable.
    fn put_new(&self, key: &str, body: &[u8]) -> Result<PutNew, String>;

    /// Replace `key` only if its ETag still matches (`If-Match`).
    ///
    /// # Errors
    ///
    /// If the backend is unreachable.
    fn cas(&self, key: &str, body: &[u8], etag: &str) -> Result<Cas, String>;

    /// How often the replication worker should poll this backend when nothing
    /// kicks it. A local daemon is free, so the default is "often enough that
    /// a handoff feels live in a demo"; a metered endpoint across the network
    /// says otherwise ([`crate::r2`]). A write still publishes at once — the
    /// worker is kicked — so this is only how fast a *follower* notices.
    fn poll_every(&self) -> std::time::Duration {
        std::time::Duration::from_millis(1500)
    }
}

// -- content hash --------------------------------------------------------------

/// FNV-1a over the bytes, hex. Not cryptographic — an integrity sanity check
/// for a snapshot install, and a stable content id. Dependency-free on
/// purpose (the daemon shares the algorithm without sharing a crate).
#[must_use]
pub fn hash(bytes: &[u8]) -> String {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for &b in bytes {
        h ^= u64::from(b);
        h = h.wrapping_mul(0x0000_0100_0000_01b3);
    }
    format!("{h:016x}")
}

// -- state helpers -------------------------------------------------------------

/// A `state` value together with the ETag it was read at — what a CAS needs.
pub type StateAt = (State, String);

/// Reads and decodes `state`, with its ETag. `None` means the lineage has not
/// been bootstrapped yet.
///
/// # Errors
///
/// If the backend errors, or `state` is malformed / an unknown wire version.
pub fn read_state(obj: &dyn Object) -> Result<Option<StateAt>, String> {
    let Some(blob) = obj.get(STATE_KEY)? else {
        return Ok(None);
    };
    let state: State = serde_json::from_slice(&blob.bytes)
        .map_err(|e| format!("state is malformed: {e}"))?;
    if state.v != WIRE_V {
        return Err(format!("state is wire version {}, we speak {WIRE_V}", state.v));
    }
    Ok(Some((state, blob.etag)))
}

/// Encodes a `state` value for the bucket.
#[must_use]
pub fn encode_state(state: &State) -> Vec<u8> {
    serde_json::to_vec(state).expect("state encodes")
}

// -- MemBucket -----------------------------------------------------------------

/// An in-process bucket for tests: a map with a per-key version counter as the
/// ETag. Shares nothing with the filesystem or the network, so any number run
/// in parallel. `Clone` shares the same backing store, which is how a test
/// hands the "same bucket" to two worlds.
/// A stored object in [`MemBucket`]: its bytes and version (the ETag).
type Entry = (Vec<u8>, u64);

#[derive(Clone, Default)]
pub struct MemBucket {
    inner: Arc<Mutex<HashMap<String, Entry>>>,
}

impl MemBucket {
    #[must_use]
    pub fn new() -> MemBucket {
        MemBucket::default()
    }
}

impl Object for MemBucket {
    fn get(&self, key: &str) -> Result<Option<Blob>, String> {
        let g = self.inner.lock().expect("bucket");
        Ok(g.get(key).map(|(bytes, ver)| Blob {
            bytes: bytes.clone(),
            etag: ver.to_string(),
        }))
    }

    fn put_new(&self, key: &str, body: &[u8]) -> Result<PutNew, String> {
        let mut g = self.inner.lock().expect("bucket");
        if g.contains_key(key) {
            return Ok(PutNew::Exists);
        }
        g.insert(key.to_string(), (body.to_vec(), 1));
        Ok(PutNew::Created("1".to_string()))
    }

    fn cas(&self, key: &str, body: &[u8], etag: &str) -> Result<Cas, String> {
        let mut g = self.inner.lock().expect("bucket");
        match g.get(key) {
            Some((_, ver)) if ver.to_string() == etag => {
                let next = ver + 1;
                g.insert(key.to_string(), (body.to_vec(), next));
                Ok(Cas::Ok(next.to_string()))
            }
            _ => Ok(Cas::Mismatch),
        }
    }
}

// -- HttpBucket ----------------------------------------------------------------

/// A plain-HTTP object client — the transport for a real run, pointed at the
/// local `bucketd` daemon (and the shape an R2/S3 backend would take, minus
/// TLS and request signing). Connection-per-request, no keep-alive: robust and
/// trivial against a daemon we control on the same machine.
///
/// The macOS app points this at `127.0.0.1`; the android emulator reaches the
/// same host daemon at `10.0.2.2`.
pub struct HttpBucket {
    /// `host:port` — no scheme, no trailing slash.
    hostport: String,
}

impl HttpBucket {
    /// From a `http://host:port` (or bare `host:port`) base URL.
    #[must_use]
    pub fn new(base: &str) -> HttpBucket {
        let hostport = base
            .trim()
            .trim_start_matches("http://")
            .trim_end_matches('/')
            .to_string();
        HttpBucket { hostport }
    }

    fn request(
        &self,
        method: &str,
        key: &str,
        precond: Option<(&str, &str)>,
        body: &[u8],
    ) -> Result<Reply, String> {
        use std::net::TcpStream;
        use std::time::Duration;

        let mut stream = TcpStream::connect(&self.hostport)
            .map_err(|e| format!("bucket {}: {e}", self.hostport))?;
        stream
            .set_read_timeout(Some(Duration::from_secs(10)))
            .and_then(|()| stream.set_write_timeout(Some(Duration::from_secs(10))))
            .map_err(|e| e.to_string())?;

        let host = self.hostport.split(':').next().unwrap_or("localhost");
        let mut headers = vec![("Host".to_string(), host.to_string())];
        if let Some((h, v)) = precond {
            headers.push((h.to_string(), v.to_string()));
        }
        round_trip(&mut stream, method, &format!("/{key}"), &headers, body)
    }
}

/// One response: the status, the `ETag` header, and the body.
pub type Reply = (u16, Option<String>, Vec<u8>);

/// Writes one HTTP/1.1 request and reads the whole response back.
///
/// Shared by the plain-socket client below and the TLS one in [`crate::r2`]:
/// the framing is identical, only the stream and the headers differ. The
/// caller supplies `Host` (a signed request has to sign the value it sends)
/// and any preconditions; `Connection: close` and `Content-Length` are ours,
/// because the connection-per-request shape is what makes reading the
/// response a matter of reading to the end.
///
/// # Errors
///
/// If the stream fails, or the response cannot be parsed.
pub fn round_trip<S: std::io::Read + std::io::Write>(
    io: &mut S,
    method: &str,
    target: &str,
    headers: &[(String, String)],
    body: &[u8],
) -> Result<Reply, String> {
    let mut req = format!("{method} {target} HTTP/1.1\r\n");
    for (k, v) in headers {
        req.push_str(&format!("{k}: {v}\r\n"));
    }
    req.push_str(&format!(
        "Connection: close\r\nContent-Length: {}\r\n\r\n",
        body.len()
    ));
    io.write_all(req.as_bytes()).map_err(|e| e.to_string())?;
    io.write_all(body).map_err(|e| e.to_string())?;
    io.flush().map_err(|e| e.to_string())?;

    let mut buf = Vec::new();
    read_to_close(io, &mut buf)?;
    parse_response(&buf)
}

/// Reads until the peer closes. A server that drops the connection without a
/// TLS `close_notify` — or resets it after the last byte — has still
/// delivered a whole response: the framing says where the body ends, not the
/// socket, so an unclean end is an end and not an error.
fn read_to_close<S: std::io::Read>(io: &mut S, out: &mut Vec<u8>) -> Result<(), String> {
    use std::io::ErrorKind;
    let mut tmp = [0u8; 8192];
    loop {
        match io.read(&mut tmp) {
            Ok(0) => return Ok(()),
            Ok(n) => out.extend_from_slice(&tmp[..n]),
            Err(e) if e.kind() == ErrorKind::Interrupted => {}
            Err(e)
                if matches!(
                    e.kind(),
                    ErrorKind::UnexpectedEof
                        | ErrorKind::ConnectionAborted
                        | ErrorKind::ConnectionReset
                ) =>
            {
                return Ok(())
            }
            Err(e) => return Err(e.to_string()),
        }
    }
}

/// Splits an HTTP response into its status, `ETag`, and body. `bucketd`
/// always answers with a `Content-Length`; a real S3 endpoint may answer a
/// `GET` chunked, so both framings are decoded here.
///
/// # Errors
///
/// If the response is not a parseable HTTP/1.1 response.
pub fn parse_response(buf: &[u8]) -> Result<Reply, String> {
    let split = buf
        .windows(4)
        .position(|w| w == b"\r\n\r\n")
        .ok_or("bucket: response has no header terminator")?;
    let head = std::str::from_utf8(&buf[..split]).map_err(|_| "bucket: non-utf8 headers")?;
    let raw = &buf[split + 4..];
    let mut lines = head.lines();
    let status: u16 = lines
        .next()
        .and_then(|l| l.split_whitespace().nth(1))
        .and_then(|c| c.parse().ok())
        .ok_or("bucket: malformed status line")?;
    let (mut etag, mut chunked, mut len) = (None, false, None);
    for l in lines {
        let Some((k, v)) = l.split_once(':') else {
            continue;
        };
        let (k, v) = (k.trim(), v.trim());
        if k.eq_ignore_ascii_case("etag") {
            etag = Some(v.to_string());
        } else if k.eq_ignore_ascii_case("transfer-encoding") {
            chunked = v.eq_ignore_ascii_case("chunked");
        } else if k.eq_ignore_ascii_case("content-length") {
            len = v.parse::<usize>().ok();
        }
    }
    let body = if chunked {
        dechunk(raw)?
    } else if let Some(n) = len {
        // Short of what was promised is a *broken* response, not a small
        // one. Returned as a body it would be a truncated snapshot or a
        // half-read batch handed on as though it were whole.
        raw.get(..n).ok_or_else(|| {
            format!("bucket: body cut short — {} of {n} bytes", raw.len())
        })?.to_vec()
    } else {
        raw.to_vec()
    };
    Ok((status, etag, body))
}

/// Joins a `Transfer-Encoding: chunked` body: `<hex len>\r\n<bytes>\r\n`
/// repeated, ended by a zero-length chunk. Chunk extensions (`;name=v`) are
/// ignored, trailers are whatever follows the terminator.
fn dechunk(raw: &[u8]) -> Result<Vec<u8>, String> {
    let mut out = Vec::new();
    let mut p = 0usize;
    loop {
        let eol = raw
            .get(p..)
            .ok_or("bucket: truncated chunk header")?
            .windows(2)
            .position(|w| w == b"\r\n")
            .ok_or("bucket: truncated chunk header")?;
        let head = std::str::from_utf8(&raw[p..p + eol]).map_err(|_| "bucket: bad chunk header")?;
        let hex = head.split(';').next().unwrap_or("").trim();
        let n = usize::from_str_radix(hex, 16).map_err(|_| format!("bucket: bad chunk size {hex:?}"))?;
        p += eol + 2;
        if n == 0 {
            return Ok(out);
        }
        let end = p.checked_add(n).ok_or("bucket: chunk length overflow")?;
        out.extend_from_slice(raw.get(p..end).ok_or("bucket: truncated chunk")?);
        p = end + 2; // the CRLF that ends the chunk
    }
}

impl Object for HttpBucket {
    fn get(&self, key: &str) -> Result<Option<Blob>, String> {
        let (status, etag, body) = self.request("GET", key, None, &[])?;
        match status {
            200 => Ok(Some(Blob {
                bytes: body,
                etag: etag.unwrap_or_default(),
            })),
            404 => Ok(None),
            other => Err(format!("bucket GET {key}: status {other}")),
        }
    }

    fn put_new(&self, key: &str, body: &[u8]) -> Result<PutNew, String> {
        let (status, etag, _) = self.request("PUT", key, Some(("If-None-Match", "*")), body)?;
        match status {
            200 | 201 => Ok(PutNew::Created(etag.unwrap_or_default())),
            412 => Ok(PutNew::Exists),
            other => Err(format!("bucket PUT {key}: status {other}")),
        }
    }

    fn cas(&self, key: &str, body: &[u8], etag: &str) -> Result<Cas, String> {
        let (status, new_etag, _) = self.request("PUT", key, Some(("If-Match", etag)), body)?;
        match status {
            200 | 201 => Ok(Cas::Ok(new_etag.unwrap_or_default())),
            412 => Ok(Cas::Mismatch),
            other => Err(format!("bucket CAS {key}: status {other}")),
        }
    }
}

// -- the daemon's request handler ---------------------------------------------
//
// Shared by `bucketd` (the standalone binary) and the round-trip test, so the
// wire contract is exercised in-process. ETags are content hashes, so the
// daemon is stateless and survives a restart.

/// One parsed HTTP request the daemon serves.
pub struct BucketReq {
    pub method: String,
    pub key: String,
    pub if_none_match: bool,
    pub if_match: Option<String>,
    pub body: Vec<u8>,
}

/// Serves one request against a directory: the CAS semantics `Object`
/// promises, over files whose ETag is their content hash. Answers `(status,
/// etag, body)`.
#[must_use]
pub fn serve(dir: &std::path::Path, req: &BucketReq) -> (u16, Option<String>, Vec<u8>) {
    // Reject traversal; keys are `a/b/c` with no `..`.
    if req.key.split('/').any(|c| c == ".." || c.is_empty()) && req.method != "GET" {
        return (400, None, Vec::new());
    }
    let path = dir.join(&req.key);
    let etag_of = |p: &std::path::Path| std::fs::read(p).ok().map(|b| hash(&b));

    match req.method.as_str() {
        "GET" => match std::fs::read(&path) {
            Ok(bytes) => {
                let e = hash(&bytes);
                (200, Some(e), bytes)
            }
            Err(_) => (404, None, Vec::new()),
        },
        "PUT" => {
            let exists = path.exists();
            let write = || -> std::io::Result<String> {
                if let Some(parent) = path.parent() {
                    std::fs::create_dir_all(parent)?;
                }
                std::fs::write(&path, &req.body)?;
                Ok(hash(&req.body))
            };
            if req.if_none_match {
                if exists {
                    (412, None, Vec::new())
                } else {
                    match write() {
                        Ok(e) => (201, Some(e), Vec::new()),
                        Err(_) => (500, None, Vec::new()),
                    }
                }
            } else if let Some(want) = &req.if_match {
                if exists && etag_of(&path).as_deref() == Some(want.as_str()) {
                    match write() {
                        Ok(e) => (200, Some(e), Vec::new()),
                        Err(_) => (500, None, Vec::new()),
                    }
                } else {
                    (412, None, Vec::new())
                }
            } else {
                match write() {
                    Ok(e) => (200, Some(e), Vec::new()),
                    Err(_) => (500, None, Vec::new()),
                }
            }
        }
        _ => (405, None, Vec::new()),
    }
}

/// Reads and serves one HTTP connection against `dir`. The daemon's accept
/// loop calls this per connection; the round-trip test calls it too.
///
/// # Errors
///
/// If the socket read or write fails.
pub fn serve_conn(dir: &std::path::Path, stream: &mut std::net::TcpStream) -> std::io::Result<()> {
    use std::io::{Read, Write};
    // Read until the header terminator, then the declared body.
    let mut buf = Vec::new();
    let mut tmp = [0u8; 4096];
    let head_end = loop {
        if let Some(p) = buf.windows(4).position(|w| w == b"\r\n\r\n") {
            break p;
        }
        let n = stream.read(&mut tmp)?;
        if n == 0 {
            return Ok(()); // client hung up
        }
        buf.extend_from_slice(&tmp[..n]);
        if buf.len() > 64 * 1024 * 1024 {
            break buf.len(); // absurd header; give up parsing below
        }
    };
    let head = String::from_utf8_lossy(&buf[..head_end]).to_string();
    let mut lines = head.lines();
    let (method, key) = lines
        .next()
        .and_then(|l| {
            let mut it = l.split_whitespace();
            Some((it.next()?.to_string(), it.next()?.trim_start_matches('/').to_string()))
        })
        .unwrap_or_default();
    let mut clen = 0usize;
    let mut if_none_match = false;
    let mut if_match = None;
    for l in lines {
        if let Some((k, v)) = l.split_once(':') {
            let (k, v) = (k.trim().to_ascii_lowercase(), v.trim());
            match k.as_str() {
                "content-length" => clen = v.parse().unwrap_or(0),
                "if-none-match" => if_none_match = v == "*",
                "if-match" => if_match = Some(v.to_string()),
                _ => {}
            }
        }
    }
    let mut body = buf[head_end + 4..].to_vec();
    while body.len() < clen {
        let n = stream.read(&mut tmp)?;
        if n == 0 {
            break;
        }
        body.extend_from_slice(&tmp[..n]);
    }
    body.truncate(clen);

    let (status, etag, out) = serve(
        dir,
        &BucketReq {
            method,
            key,
            if_none_match,
            if_match,
            body,
        },
    );
    let reason = match status {
        200 => "OK",
        201 => "Created",
        400 => "Bad Request",
        404 => "Not Found",
        405 => "Method Not Allowed",
        412 => "Precondition Failed",
        _ => "Error",
    };
    let mut resp = format!("HTTP/1.1 {status} {reason}\r\nConnection: close\r\nContent-Length: {}\r\n", out.len());
    if let Some(e) = etag {
        resp.push_str(&format!("ETag: {e}\r\n"));
    }
    resp.push_str("\r\n");
    stream.write_all(resp.as_bytes())?;
    stream.write_all(&out)?;
    stream.flush()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn create_only_and_cas() {
        let b = MemBucket::new();
        assert!(b.get("k").unwrap().is_none());

        // First create wins; a second is refused.
        assert_eq!(b.put_new("k", b"one").unwrap(), PutNew::Created("1".into()));
        assert_eq!(b.put_new("k", b"two").unwrap(), PutNew::Exists);

        let blob = b.get("k").unwrap().unwrap();
        assert_eq!(blob.bytes, b"one");

        // CAS with the wrong ETag is refused; with the right one it advances.
        assert_eq!(b.cas("k", b"x", "999").unwrap(), Cas::Mismatch);
        let Cas::Ok(e2) = b.cas("k", b"two", &blob.etag).unwrap() else {
            panic!("cas should have won");
        };
        assert_eq!(b.get("k").unwrap().unwrap().bytes, b"two");
        // The old ETag is now stale.
        assert_eq!(b.cas("k", b"z", &blob.etag).unwrap(), Cas::Mismatch);
        assert_ne!(e2, blob.etag);
    }

    #[test]
    fn state_round_trips_and_guards_version() {
        let b = MemBucket::new();
        assert!(read_state(&b).unwrap().is_none());
        let s = State {
            v: WIRE_V,
            schema: 9,
            epoch: 1,
            holder: Some("dev-a".into()),
            released: false,
            seq: 0,
            batch: None,
            snapshot: Snapshot {
                key: "snap/0-9-abc.db".into(),
                seq: 0,
                schema: 9,
                hash: "abc".into(),
            },
        };
        b.put_new(STATE_KEY, &encode_state(&s)).unwrap();
        let (got, _etag) = read_state(&b).unwrap().unwrap();
        assert_eq!(got, s);

        // An unknown wire version refuses rather than guesses.
        let mut bad = s.clone();
        bad.v = 99;
        b.cas(STATE_KEY, &encode_state(&bad), &b.get(STATE_KEY).unwrap().unwrap().etag)
            .unwrap();
        assert!(read_state(&b).is_err());
    }

    #[test]
    fn keys_are_unique_by_construction() {
        assert_eq!(batch_key(3, "dev-a", 11, 12), "log/3/dev-a/11-12");
        assert_ne!(batch_key(3, "dev-a", 11, 12), batch_key(4, "dev-a", 11, 12));
        assert_ne!(batch_key(3, "dev-a", 11, 12), batch_key(3, "dev-b", 11, 12));
        assert!(snap_key(0, 9, "abcd").starts_with("snap/0-9-abcd"));
    }

    /// A response that stops short of its `Content-Length` is refused, not
    /// passed on as a shorter object.
    #[test]
    fn a_cut_short_body_is_an_error_not_a_small_one() {
        let whole = b"HTTP/1.1 200 OK\r\nContent-Length: 5\r\nETag: \"e\"\r\n\r\nhello";
        let (status, etag, body) = parse_response(whole).unwrap();
        assert_eq!((status, etag.as_deref(), &body[..]), (200, Some("\"e\""), &b"hello"[..]));

        let cut = b"HTTP/1.1 200 OK\r\nContent-Length: 5\r\n\r\nhel";
        assert!(parse_response(cut).unwrap_err().contains("cut short"));

        // Chunked, whole and then truncated mid-chunk.
        let ch = b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n3\r\nabc\r\n2\r\nde\r\n0\r\n\r\n";
        assert_eq!(parse_response(ch).unwrap().2, b"abcde");
        let ch_cut = b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n9\r\nabc";
        assert!(parse_response(ch_cut).is_err());
    }

    #[test]
    fn the_hash_is_stable_and_sensitive() {
        assert_eq!(hash(b"hello"), hash(b"hello"));
        assert_ne!(hash(b"hello"), hash(b"hell0"));
    }

    /// The HTTP client and the daemon handler honour the same CAS contract as
    /// `MemBucket`, over a real localhost socket — the round trip a mac and an
    /// emulator make.
    #[test]
    fn http_round_trips_the_cas_contract() {
        use std::net::TcpListener;
        let dir = std::env::temp_dir().join(format!("superapp-bucket-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let served = dir.clone();
        let handle = std::thread::spawn(move || {
            // Serve exactly the requests this test makes, then stop.
            for _ in 0..8 {
                let (mut s, _) = listener.accept().unwrap();
                let _ = serve_conn(&served, &mut s);
            }
        });

        let b = HttpBucket::new(&format!("http://{addr}"));
        assert!(b.get("log/1/dev/1-1").unwrap().is_none()); // GET absent → 404
        let PutNew::Created(_) = b.put_new("state", b"one").unwrap() else {
            panic!("first create wins");
        };
        assert_eq!(b.put_new("state", b"two").unwrap(), PutNew::Exists); // create-only
        let blob = b.get("state").unwrap().unwrap();
        assert_eq!(blob.bytes, b"one");
        assert_eq!(b.cas("state", b"x", "wrong").unwrap(), Cas::Mismatch);
        let Cas::Ok(_) = b.cas("state", b"two", &blob.etag).unwrap() else {
            panic!("cas with the current etag wins");
        };
        assert_eq!(b.get("state").unwrap().unwrap().bytes, b"two");
        assert_eq!(b.cas("state", b"z", &blob.etag).unwrap(), Cas::Mismatch); // stale etag

        handle.join().unwrap();
        let _ = std::fs::remove_dir_all(&dir);
    }
}
