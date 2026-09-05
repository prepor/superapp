//! A hand-rolled HTTP/1.1 client: one verb, one host, no redirects, one long
//! streamed answer.
//!
//! There is no HTTP crate in this tree on purpose — the ones that exist bring
//! an async runtime or a second TLS stack, and android must build the same
//! crate — so the two clients this tree already hand-rolls (device sync's
//! bucket, a mail sign-in) get a third, small enough to read in one sitting
//! and shaped so that both of them can move onto it.
//!
//! The connection is verified against the Mozilla roots, not the machine's:
//! a phone has no machine roots to verify against, so [`tls_config`] is the
//! honest one on every target. The body is a [`Read`] that undoes
//! `Transfer-Encoding: chunked` as it arrives and never buffers the whole
//! answer, so a model's reply is read as it is written rather than at the
//! end.
//!
//! The parsing is split from the socket on purpose: the head reader and the
//! chunked reader are driven by tests over an in-memory cursor, so the wire's
//! edge cases are pinned without a network.

use std::io::{BufRead, BufReader, ErrorKind, Read, Write};
use std::net::{TcpStream, ToSocketAddrs};
use std::sync::{Arc, OnceLock};
use std::time::Duration;

/// One request. Borrowed throughout: the caller owns the url, the headers and
/// the body, and this is read once and dropped.
pub struct Request<'a> {
    /// `POST`, `GET` — as it goes on the request line.
    pub method: &'a str,
    /// `https://host[:port]/path` — https only.
    pub url: &'a str,
    /// The headers the caller sets, sent in this order. `Host`,
    /// `Content-Length` and `Connection` are added from the url and the body
    /// unless one of them is already here.
    pub headers: &'a [(&'a str, String)],
    /// The bytes to send, empty for a body-less verb.
    pub body: &'a [u8],
}

/// The answer, with the body still arriving. The status and the headers are
/// read whole before this is handed back; the body streams, already
/// de-chunked.
pub struct Response {
    /// The code off the status line.
    pub status: u16,
    /// Every header, names lower-cased and values trimmed, in the order the
    /// peer sent them.
    pub headers: Vec<(String, String)>,
    /// The body as it arrives. One `read` answers as soon as some bytes are
    /// there, which is what makes a streamed answer readable while the model
    /// is still writing it.
    pub body: Box<dyn Read>,
}

impl Response {
    /// The first header of that name, whatever case the caller asks in.
    #[must_use]
    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(k, _)| k.eq_ignore_ascii_case(name))
            .map(|(_, v)| v.as_str())
    }

    /// The whole body as text, at most `cap` bytes of it — for the short
    /// answers something reads whole: an error page, a token reply. Bytes
    /// past the cap are dropped rather than made an error: a cap is a guard
    /// against an endpoint that answers with a gigabyte, not a contract with
    /// it. Invalid UTF-8 is replaced, because an error's words are worth more
    /// than its encoding.
    ///
    /// # Errors
    ///
    /// If reading the body fails.
    pub fn text(self, cap: usize) -> Result<String, String> {
        let mut out = Vec::new();
        let cap = u64::try_from(cap).unwrap_or(u64::MAX);
        self.body
            .take(cap)
            .read_to_end(&mut out)
            .map_err(|e| why(&e))?;
        Ok(String::from_utf8_lossy(&out).into_owned())
    }
}

impl std::fmt::Debug for Response {
    /// The head, and not the body: a body that has not arrived yet cannot be
    /// printed, and one that has would be spent by printing it. Hand-written
    /// because a boxed reader has no `Debug` of its own — and worth having,
    /// since `unwrap_err()` on a failed request asks for one.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Response")
            .field("status", &self.status)
            .field("headers", &self.headers)
            .finish_non_exhaustive()
    }
}

/// How long a request may wait, at the three places where waiting means
/// something different.
#[derive(Clone, Copy, Debug)]
pub struct Timeouts {
    /// To reach the host and finish the handshake.
    pub connect: Duration,
    /// For the answer to begin — the head, and then the first byte of the
    /// body. Generous by design: a model that thinks for a minute before its
    /// first token is not a dead peer.
    pub first_byte: Duration,
    /// Between two bytes once the answer is flowing. A stream that has gone
    /// this long without a word is gone.
    pub idle: Duration,
}

impl Default for Timeouts {
    fn default() -> Timeouts {
        Timeouts {
            connect: Duration::from_secs(30),
            first_byte: Duration::from_secs(120),
            idle: Duration::from_secs(60),
        }
    }
}

/// One request, with the default [`Timeouts`].
///
/// # Errors
///
/// If the url is not https or is malformed, the host refuses the connection
/// or the handshake, nothing arrives in time, or the response head cannot be
/// read.
pub fn send(req: &Request<'_>) -> Result<Response, String> {
    send_with(req, Timeouts::default())
}

/// Opens the connection, sends the request, and returns the moment the status
/// line and the headers are in; the body streams from [`Response::body`].
/// https only, one request per connection, and no redirects followed — a
/// caller handed a `301` is being told something it should decide about.
///
/// # Errors
///
/// If the url is not https or is malformed, the host refuses the connection
/// or the handshake, nothing arrives in time, or the response head cannot be
/// read.
pub fn send_with(req: &Request<'_>, t: Timeouts) -> Result<Response, String> {
    let (host, port, target) = split_url(req.url)?;

    let sock = connect(&host, port, t.connect)?;
    // The handshake's own budget: it is part of connecting, and it happens
    // below the wrapper, on the socket itself.
    sock.set_read_timeout(Some(t.connect))
        .and_then(|()| sock.set_write_timeout(Some(t.connect)))
        .map_err(|e| sentence(&host, &e))?;
    let dup = sock.try_clone().map_err(|e| sentence(&host, &e))?;

    let name = rustls::pki_types::ServerName::try_from(host.clone())
        .map_err(|_| format!("not a server name: {host}"))?;
    let conn = rustls::ClientConnection::new(tls_config(), name)
        .map_err(|e| format!("tls to {host}: {e}"))?;
    let tls = rustls::StreamOwned::new(conn, sock);

    // The buffer is left intact across the head/body seam: whatever it read
    // past the blank line is the body's first bytes, so the same reader goes
    // on as the body's source.
    let mut r = BufReader::new(Timed::new(tls, dup, host.clone(), t));
    let authority = if port == 443 {
        host.clone()
    } else {
        format!("{host}:{port}")
    };
    write_request(r.get_mut(), &authority, &target, req).map_err(|e| sentence(&host, &e))?;

    let (status, headers) = read_head(&mut r).map_err(|e| sentence(&host, &e))?;
    // The head has arrived and the answer has not: a gateway sends its `200`
    // at once and then holds the line while the model thinks.
    r.get_mut().expect_first_byte();
    let body = body_reader(r, &headers);
    Ok(Response {
        status,
        headers,
        body,
    })
}

/// The client TLS configuration, built once: the Mozilla root set, the `ring`
/// provider (chosen explicitly rather than inherited from whatever the process
/// installed as its default), and no client certificate. Never the machine's
/// certificates — android has none, and a build that works on one target and
/// not on the other is worse than one that works the same way everywhere.
///
/// Device sync's bucket builds the same configuration today; it moves onto
/// this one when its `send` moves onto [`send_with`].
#[must_use]
pub fn tls_config() -> Arc<rustls::ClientConfig> {
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

// -- the url -------------------------------------------------------------------

/// `https://host[:port]/path?query` → the host, the port (443 unless said),
/// and the request target (`/path?query`, at least `/`). https only, because
/// every caller here talks to an endpoint behind TLS and a plaintext fallback
/// is a footgun nobody in this tree needs.
fn split_url(url: &str) -> Result<(String, u16, String), String> {
    let rest = url
        .strip_prefix("https://")
        .ok_or_else(|| format!("not https: {url}"))?;
    let (authority, path) = rest.split_once('/').map_or((rest, ""), |(a, p)| (a, p));
    let target = if path.is_empty() {
        "/".to_string()
    } else {
        format!("/{path}")
    };
    let (host, port) = match authority.rsplit_once(':') {
        Some((h, p)) => (
            h.to_string(),
            p.parse()
                .map_err(|_| format!("not a port in the url: {url}"))?,
        ),
        None => (authority.to_string(), 443u16),
    };
    if host.is_empty() {
        return Err(format!("no host in the url: {url}"));
    }
    Ok((host, port, target))
}

// -- the wire ------------------------------------------------------------------

/// Writes the request head and body. Adds the three headers the transport
/// owns — `Host` from the url, `Content-Length` from the body, and
/// `Connection: close`, one request to a connection — unless the caller has
/// set that one already. A `POST` is given a `Content-Length` even for an
/// empty body: a verb that carries a body and does not say how long it is
/// reads as a truncated request.
fn write_request(
    mut w: impl Write,
    authority: &str,
    target: &str,
    req: &Request<'_>,
) -> std::io::Result<()> {
    let has = |name: &str| {
        req.headers
            .iter()
            .any(|(k, _)| k.eq_ignore_ascii_case(name))
    };
    let mut head = format!("{} {target} HTTP/1.1\r\n", req.method);
    if !has("host") {
        head.push_str(&format!("Host: {authority}\r\n"));
    }
    for (k, v) in req.headers {
        head.push_str(&format!("{k}: {v}\r\n"));
    }
    if !has("content-length") && (!req.body.is_empty() || req.method.eq_ignore_ascii_case("post")) {
        head.push_str(&format!("Content-Length: {}\r\n", req.body.len()));
    }
    if !has("connection") {
        head.push_str("Connection: close\r\n");
    }
    head.push_str("\r\n");
    w.write_all(head.as_bytes())?;
    w.write_all(req.body)?;
    w.flush()
}

/// Reads the status line and the headers, stopping at the blank line that
/// ends the head; the reader is left at the first byte of the body, its
/// buffer intact. Header names come back lower-cased and values trimmed, so a
/// lookup is a comparison and not a guess about the peer's taste in capitals.
/// Split from the socket so a test can drive it over a cursor.
///
/// # Errors
///
/// If the stream ends inside the head, or the head is not one: no code on the
/// status line, a header line with no colon.
fn read_head<R: BufRead>(r: &mut R) -> std::io::Result<(u16, Vec<(String, String)>)> {
    let status: u16 = read_line(r)?
        .split_whitespace()
        .nth(1)
        .and_then(|c| c.parse().ok())
        .ok_or_else(malformed)?;
    let mut headers = Vec::new();
    loop {
        let line = read_line(r)?;
        if line.is_empty() {
            break; // the blank line that ends the head
        }
        // The first colon separates; the rest belongs to the value, which is
        // how a `Location` keeps its `https://`.
        let (k, v) = line.split_once(':').ok_or_else(malformed)?;
        headers.push((k.trim().to_ascii_lowercase(), v.trim().to_string()));
    }
    Ok((status, headers))
}

/// The one sentence every failure of the head is: what a person can do about
/// a head this client cannot read is the same in every case.
fn malformed() -> std::io::Error {
    std::io::Error::new(
        ErrorKind::InvalidData,
        "malformed response head".to_string(),
    )
}

/// One line of the head, without its `\r\n`. A line that ends at the end of
/// the stream rather than at a newline is a truncated head, and is an error.
fn read_line<R: BufRead>(r: &mut R) -> std::io::Result<String> {
    let mut buf = Vec::new();
    if r.read_until(b'\n', &mut buf)? == 0 || buf.last() != Some(&b'\n') {
        return Err(malformed());
    }
    buf.pop();
    if buf.last() == Some(&b'\r') {
        buf.pop();
    }
    Ok(String::from_utf8_lossy(&buf).into_owned())
}

/// The body as a `Read`, chosen by the head: a chunked body is de-chunked as
/// it arrives, a `Content-Length` bounds the read, and otherwise the body runs
/// to the end of the stream, which `Connection: close` makes the end of the
/// body.
fn body_reader<R: BufRead + 'static>(r: R, headers: &[(String, String)]) -> Box<dyn Read> {
    let value = |name: &str| {
        headers
            .iter()
            .find(|(k, _)| k.eq_ignore_ascii_case(name))
            .map(|(_, v)| v.as_str())
    };
    if value("transfer-encoding").is_some_and(|v| v.to_ascii_lowercase().contains("chunked")) {
        Box::new(Chunked::new(r))
    } else if let Some(n) = value("content-length").and_then(|v| v.parse::<u64>().ok()) {
        Box::new(r.take(n))
    } else {
        Box::new(CloseIsEnd(r))
    }
}

// -- the socket ----------------------------------------------------------------

/// A socket to the host, tried at every address it resolves to — a host with
/// an AAAA record on a network with no IPv6 is a wait, not a failure, and the
/// next address is the answer. Resolution itself is the system's and has no
/// timeout of its own.
fn connect(host: &str, port: u16, timeout: Duration) -> Result<TcpStream, String> {
    let addrs = (host, port)
        .to_socket_addrs()
        .map_err(|e| format!("{host}: {}", why(&e)))?;
    let mut last = None;
    for addr in addrs {
        match TcpStream::connect_timeout(&addr, timeout) {
            Ok(sock) => return Ok(sock),
            Err(e) => last = Some(e),
        }
    }
    Err(match last {
        Some(e) => format!("{host}: {}", why(&e)),
        None => format!("{host}: no address"),
    })
}

/// The read side of the answer, with the patience the caller asked for: the
/// first-byte budget until the answer begins, the idle budget between bytes
/// after that.
///
/// `sock` is a second handle on the same socket. The timeout is an option on
/// the socket rather than on the descriptor, so setting it here reaches the
/// reads happening inside the TLS stream — which is what lets one wrapper
/// govern a body it does not itself read.
struct Timed<R> {
    inner: R,
    sock: TcpStream,
    host: String,
    first_byte: Duration,
    idle: Duration,
    /// The answer has begun; from here it is the gap between two reads that
    /// is bounded.
    flowing: bool,
    /// What is armed on the socket, so re-arming costs a syscall only when
    /// the budget actually changes.
    armed: Option<Duration>,
}

impl<R> Timed<R> {
    fn new(inner: R, sock: TcpStream, host: String, t: Timeouts) -> Timed<R> {
        Timed {
            inner,
            sock,
            host,
            first_byte: t.first_byte,
            idle: t.idle,
            flowing: false,
            armed: None,
        }
    }

    /// Say that what comes next is the beginning of an answer and not its
    /// continuation: the head is in and the body has not started.
    fn expect_first_byte(&mut self) {
        self.flowing = false;
    }
}

impl<R: Read> Read for Timed<R> {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        let budget = if self.flowing {
            self.idle
        } else {
            self.first_byte
        };
        if self.armed != Some(budget) {
            self.sock.set_read_timeout(Some(budget))?;
            self.armed = Some(budget);
        }
        loop {
            return match self.inner.read(buf) {
                Ok(n) => {
                    if n > 0 {
                        self.flowing = true;
                    }
                    Ok(n)
                }
                Err(e) if e.kind() == ErrorKind::Interrupted => continue,
                // A socket out of patience says `WouldBlock` on one platform
                // and `TimedOut` on another; both mean silence.
                Err(e) if matches!(e.kind(), ErrorKind::WouldBlock | ErrorKind::TimedOut) => {
                    Err(std::io::Error::new(
                        ErrorKind::TimedOut,
                        format!("{}: no bytes for {} s", self.host, budget.as_secs()),
                    ))
                }
                Err(e) => Err(e),
            };
        }
    }
}

impl<R: Write> Write for Timed<R> {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.inner.write(buf)
    }

    fn flush(&mut self) -> std::io::Result<()> {
        self.inner.flush()
    }
}

/// What went wrong, as words: the operating system's parenthetical dropped
/// and the first letter lowered, so it reads as the tail of a sentence —
/// "connection refused", not "Connection refused (os error 61)".
fn why(e: &std::io::Error) -> String {
    let text = e.to_string();
    let text = text.split(" (os error").next().unwrap_or(&text).trim();
    let mut rest = text.chars();
    match rest.next() {
        // Only a plainly capitalised sentence is lowered: `TLS`,
        // `UnknownIssuer` and their kind are names, and keep their capitals.
        Some(first) if first.is_uppercase() && !rest.clone().any(char::is_uppercase) => {
            first.to_lowercase().collect::<String>() + rest.as_str()
        }
        _ => text.to_string(),
    }
}

/// A failure as one sentence a person reads: which host, then what happened.
/// A failure that has already named the host — a timeout says which line went
/// quiet — is passed through rather than said twice.
fn sentence(host: &str, e: &std::io::Error) -> String {
    let text = why(e);
    if text.starts_with(host) {
        text
    } else {
        format!("{host}: {text}")
    }
}

// -- the framings --------------------------------------------------------------

/// Undoes `Transfer-Encoding: chunked` as it goes: each chunk is a hex size
/// line (extensions after a `;` ignored), the bytes, a CRLF, and the body ends
/// at a zero-size chunk, whose trailers are read and dropped. A `read` never
/// waits for more than the chunk it is in the middle of, so a streamed answer
/// is handed on as it arrives rather than at the end.
struct Chunked<R: Read> {
    inner: R,
    /// Bytes left in the chunk being handed out.
    remaining: usize,
    /// The zero-size chunk has been seen; nothing more comes.
    done: bool,
}

impl<R: Read> Chunked<R> {
    fn new(inner: R) -> Chunked<R> {
        Chunked {
            inner,
            remaining: 0,
            done: false,
        }
    }

    /// One line ending in `\n`, without its terminator, or `None` when the
    /// stream ends before one arrives. Read a byte at a time because the
    /// inner reader is only [`Read`] and a framing line is a handful of
    /// bytes; behind the buffered reader a body comes through, that is not a
    /// syscall each.
    fn line_or_end(&mut self) -> std::io::Result<Option<String>> {
        let mut out = Vec::new();
        let mut b = [0u8; 1];
        loop {
            match self.inner.read(&mut b)? {
                0 => return Ok(None),
                _ if b[0] == b'\n' => break,
                _ => out.push(b[0]),
            }
        }
        if out.last() == Some(&b'\r') {
            out.pop();
        }
        Ok(Some(String::from_utf8_lossy(&out).into_owned()))
    }

    /// One framing line that has to be there. A body that stops in the middle
    /// of its own frame is broken, not short.
    fn line(&mut self) -> std::io::Result<String> {
        self.line_or_end()?.ok_or_else(|| {
            std::io::Error::new(
                ErrorKind::UnexpectedEof,
                "chunked body ended inside a line".to_string(),
            )
        })
    }

    /// The size of the next chunk, the extensions after `;` discarded.
    fn next_size(&mut self) -> std::io::Result<usize> {
        let line = self.line()?;
        let hex = line.split(';').next().unwrap_or("").trim();
        usize::from_str_radix(hex, 16).map_err(|_| {
            std::io::Error::new(ErrorKind::InvalidData, format!("bad chunk size {hex:?}"))
        })
    }

    /// The terminator's own CRLF and any trailer lines after it, read and
    /// dropped — nothing here wants a trailer, and leaving one on the socket
    /// would hand the next reader a body that is not one. A stream that simply
    /// stops after the `0` has still delivered a whole body.
    fn swallow_trailers(&mut self) -> std::io::Result<()> {
        loop {
            match self.line_or_end()? {
                Some(line) if !line.is_empty() => {}
                _ => return Ok(()),
            }
        }
    }
}

impl<R: Read> Read for Chunked<R> {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        if self.done || buf.is_empty() {
            return Ok(0);
        }
        if self.remaining == 0 {
            // Positioned at a size line: the CRLF after the previous chunk's
            // bytes was taken the moment that chunk ran out, below.
            let size = self.next_size()?;
            if size == 0 {
                self.swallow_trailers()?;
                self.done = true;
                return Ok(0);
            }
            self.remaining = size;
        }
        let want = self.remaining.min(buf.len());
        let n = self.inner.read(&mut buf[..want])?;
        if n == 0 {
            return Err(std::io::Error::new(
                ErrorKind::UnexpectedEof,
                "chunked body ended inside a chunk".to_string(),
            ));
        }
        self.remaining -= n;
        if self.remaining == 0 {
            let _ = self.line()?; // the CRLF that closes this chunk's bytes
        }
        Ok(n)
    }
}

/// A body with no framing of its own, where the end of the stream is the end
/// of the body. A peer that resets the connection or drops TLS's
/// `close_notify` after its last byte has still delivered a whole answer: the
/// framing says where a body ends, and this body's framing is the close.
struct CloseIsEnd<R: Read>(R);

impl<R: Read> Read for CloseIsEnd<R> {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        match self.0.read(buf) {
            Err(e)
                if matches!(
                    e.kind(),
                    ErrorKind::UnexpectedEof
                        | ErrorKind::ConnectionAborted
                        | ErrorKind::ConnectionReset
                ) =>
            {
                Ok(0)
            }
            other => other,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    /// A reader that hands over at most `max` bytes a call, so a test can
    /// prove the parsers reassemble across reads rather than leaning on one
    /// read answering with the whole body.
    struct Slices<R> {
        inner: R,
        max: usize,
    }

    impl<R: Read> Read for Slices<R> {
        fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
            let n = buf.len().min(self.max);
            self.inner.read(&mut buf[..n])
        }
    }

    /// A reader that hands over the frames it is given, one a call, so a test
    /// can place a split exactly where it wants one.
    struct Frames {
        frames: Vec<Vec<u8>>,
    }

    impl Frames {
        fn new(frames: &[&[u8]]) -> Frames {
            let mut frames: Vec<Vec<u8>> = frames.iter().map(|f| f.to_vec()).collect();
            frames.reverse();
            Frames { frames }
        }
    }

    impl Read for Frames {
        fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
            let Some(front) = self.frames.last_mut() else {
                return Ok(0);
            };
            let n = front.len().min(buf.len());
            buf[..n].copy_from_slice(&front[..n]);
            front.drain(..n);
            if front.is_empty() {
                self.frames.pop();
            }
            Ok(n)
        }
    }

    /// A reader with a rude ending: some bytes, then the failure a peer that
    /// drops the connection makes.
    struct Rude {
        left: Vec<u8>,
        kind: ErrorKind,
    }

    impl Read for Rude {
        fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
            if self.left.is_empty() {
                return Err(std::io::Error::new(self.kind, "the peer went away"));
            }
            let n = self.left.len().min(buf.len());
            buf[..n].copy_from_slice(&self.left[..n]);
            self.left.drain(..n);
            Ok(n)
        }
    }

    /// A whole chunked body, decoded with the inner reads capped at `inner`
    /// bytes and the caller's buffer `out` bytes wide — the two ways a stream
    /// can be cut into pieces, driven independently.
    fn dechunk(raw: &[u8], inner: usize, out: usize) -> std::io::Result<String> {
        let mut r = Chunked::new(Slices {
            inner: Cursor::new(raw.to_vec()),
            max: inner,
        });
        let (mut buf, mut got) = (vec![0u8; out], Vec::new());
        loop {
            match r.read(&mut buf)? {
                0 => return Ok(String::from_utf8_lossy(&got).into_owned()),
                n => got.extend_from_slice(&buf[..n]),
            }
        }
    }

    #[test]
    fn the_head_parses_status_and_headers() {
        let raw = "HTTP/1.1 200 OK\r\n\
                   Content-Type: text/event-stream\r\n\
                   Transfer-Encoding: chunked\r\n\
                   \r\n\
                   the body";
        let mut r = Cursor::new(raw.as_bytes().to_vec());
        let (status, headers) = read_head(&mut r).unwrap();
        assert_eq!(status, 200);
        // Names lower-cased, values trimmed, order kept.
        assert_eq!(
            headers,
            vec![
                ("content-type".to_string(), "text/event-stream".to_string()),
                ("transfer-encoding".to_string(), "chunked".to_string()),
            ]
        );
        // The reader is left at the body.
        let mut rest = String::new();
        r.read_to_string(&mut rest).unwrap();
        assert_eq!(rest, "the body");
    }

    #[test]
    fn a_header_value_keeps_its_own_colons() {
        let raw = "HTTP/1.1 302 Found\r\n\
                   Location: https://example.com:8443/next\r\n\
                   Date: Thu, 04 Sep 2026 10:11:12 GMT\r\n\
                   \r\n";
        let (status, headers) = read_head(&mut Cursor::new(raw.as_bytes().to_vec())).unwrap();
        assert_eq!(status, 302);
        assert_eq!(headers[0].1, "https://example.com:8443/next");
        assert_eq!(headers[1].1, "Thu, 04 Sep 2026 10:11:12 GMT");
    }

    #[test]
    fn a_status_with_no_headers_reads_out_whole() {
        let (status, headers) = read_head(&mut Cursor::new(
            b"HTTP/1.1 401 Unauthorized\r\n\r\n".to_vec(),
        ))
        .unwrap();
        assert_eq!(status, 401);
        assert!(headers.is_empty());
    }

    #[test]
    fn a_head_that_never_ends_is_malformed() {
        let mut r = Cursor::new(b"HTTP/1.1 200 OK\r\nContent-Type: x".to_vec());
        let e = read_head(&mut r).unwrap_err();
        assert_eq!(e.kind(), ErrorKind::InvalidData);
        assert_eq!(e.to_string(), "malformed response head");
    }

    #[test]
    fn a_status_line_with_no_code_is_malformed() {
        let mut r = Cursor::new(b"a fine morning to you\r\n\r\n".to_vec());
        assert_eq!(
            read_head(&mut r).unwrap_err().to_string(),
            "malformed response head"
        );
    }

    #[test]
    fn a_chunked_body_is_read_as_it_arrives() {
        // "Mozilla" (0x7) then "Developer Network" (0x11): the size lines,
        // the CRLFs and the data all get cut apart by the slicing.
        let raw = b"7\r\nMozilla\r\n11\r\nDeveloper Network\r\n0\r\n\r\n";
        for inner in [1usize, 3, 7, 4096] {
            for out in [1usize, 3, 7, 4096] {
                assert_eq!(
                    dechunk(raw, inner, out).unwrap(),
                    "MozillaDeveloper Network",
                    "inner {inner}, out {out}"
                );
            }
        }
    }

    #[test]
    fn a_chunk_boundary_may_land_anywhere() {
        // Splits placed by hand: inside the data, between a CRLF's two bytes,
        // and inside a two-digit size line.
        let mut r = Chunked::new(Frames::new(&[
            b"7\r\nMoz",
            b"illa\r",
            b"\n1",
            b"1\r\nDeveloper Network\r\n0\r\n\r\n",
        ]));
        let mut got = String::new();
        r.read_to_string(&mut got).unwrap();
        assert_eq!(got, "MozillaDeveloper Network");
    }

    #[test]
    fn a_chunk_may_carry_extensions_and_the_body_trailers() {
        let raw = b"3;name=value;q=x\r\nabc\r\n0\r\nExpires: never\r\nX-Sum: 9\r\n\r\n";
        assert_eq!(dechunk(raw, 1, 1).unwrap(), "abc");
        assert_eq!(dechunk(raw, 4096, 4096).unwrap(), "abc");
        // The empty body: a zero chunk and nothing else.
        assert_eq!(dechunk(b"0\r\n\r\n", 1, 4).unwrap(), "");
    }

    #[test]
    fn a_chunk_cut_short_is_an_error() {
        // Claims five bytes, delivers two, then ends — a broken body, not a
        // small one.
        let e = dechunk(b"5\r\nab", 1, 8).unwrap_err();
        assert_eq!(e.kind(), ErrorKind::UnexpectedEof);
    }

    #[test]
    fn a_bad_chunk_size_is_an_error() {
        let e = dechunk(b"zz\r\nab\r\n0\r\n\r\n", 4096, 8).unwrap_err();
        assert_eq!(e.kind(), ErrorKind::InvalidData);
    }

    #[test]
    fn a_content_length_bounds_the_body() {
        let head = vec![("content-length".to_string(), "5".to_string())];
        let raw = BufReader::new(Cursor::new(b"hellothere is more".to_vec()));
        let mut got = String::new();
        body_reader(raw, &head).read_to_string(&mut got).unwrap();
        assert_eq!(got, "hello");
    }

    #[test]
    fn a_body_with_no_framing_runs_to_the_close() {
        // No content-length, no transfer-encoding: the body ends when the
        // peer goes away, rudely or not.
        let head = Vec::new();
        let rude = Rude {
            left: b"a whole answer".to_vec(),
            kind: ErrorKind::ConnectionReset,
        };
        let mut got = String::new();
        body_reader(BufReader::new(rude), &head)
            .read_to_string(&mut got)
            .unwrap();
        assert_eq!(got, "a whole answer");
    }

    #[test]
    fn a_chunked_head_wins_over_a_content_length() {
        let head = vec![
            ("content-length".to_string(), "99".to_string()),
            ("transfer-encoding".to_string(), "chunked".to_string()),
        ];
        let raw = BufReader::new(Cursor::new(b"3\r\nabc\r\n0\r\n\r\n".to_vec()));
        let mut got = String::new();
        body_reader(raw, &head).read_to_string(&mut got).unwrap();
        assert_eq!(got, "abc");
    }

    #[test]
    fn a_post_goes_out_with_its_length_and_a_close() {
        let headers = [
            ("content-type", "application/json".to_string()),
            ("authorization", "Bearer t".to_string()),
        ];
        let req = Request {
            method: "POST",
            url: "https://gateway.example.com/v1/chat/completions",
            headers: &headers,
            body: br#"{"stream":true}"#,
        };
        let mut out = Vec::new();
        write_request(
            &mut out,
            "gateway.example.com",
            "/v1/chat/completions",
            &req,
        )
        .unwrap();
        assert_eq!(
            String::from_utf8(out).unwrap(),
            "POST /v1/chat/completions HTTP/1.1\r\n\
             Host: gateway.example.com\r\n\
             content-type: application/json\r\n\
             authorization: Bearer t\r\n\
             Content-Length: 15\r\n\
             Connection: close\r\n\
             \r\n\
             {\"stream\":true}"
        );
    }

    #[test]
    fn a_get_goes_out_with_no_length_at_all() {
        let req = Request {
            method: "GET",
            url: "https://example.com/",
            headers: &[],
            body: &[],
        };
        let mut out = Vec::new();
        write_request(&mut out, "example.com", "/", &req).unwrap();
        assert_eq!(
            String::from_utf8(out).unwrap(),
            "GET / HTTP/1.1\r\nHost: example.com\r\nConnection: close\r\n\r\n"
        );
    }

    #[test]
    fn an_empty_post_still_says_how_long_it_is() {
        let req = Request {
            method: "POST",
            url: "https://example.com/ping",
            headers: &[],
            body: &[],
        };
        let mut out = Vec::new();
        write_request(&mut out, "example.com", "/ping", &req).unwrap();
        assert!(String::from_utf8(out)
            .unwrap()
            .contains("Content-Length: 0\r\n"));
    }

    #[test]
    fn the_caller_keeps_the_headers_it_set_itself() {
        let headers = [
            ("Host", "elsewhere.example".to_string()),
            ("Connection", "keep-alive".to_string()),
            ("Content-Length", "4".to_string()),
        ];
        let req = Request {
            method: "POST",
            url: "https://example.com/x",
            headers: &headers,
            body: b"body",
        };
        let mut out = Vec::new();
        write_request(&mut out, "example.com", "/x", &req).unwrap();
        assert_eq!(
            String::from_utf8(out).unwrap(),
            "POST /x HTTP/1.1\r\n\
             Host: elsewhere.example\r\n\
             Connection: keep-alive\r\n\
             Content-Length: 4\r\n\
             \r\n\
             body"
        );
    }

    #[test]
    fn a_url_says_its_host_its_port_and_its_target() {
        assert_eq!(
            split_url("https://example.com/a/b?c=d").unwrap(),
            ("example.com".to_string(), 443, "/a/b?c=d".to_string())
        );
        assert_eq!(
            split_url("https://example.com").unwrap(),
            ("example.com".to_string(), 443, "/".to_string())
        );
        assert_eq!(
            split_url("https://example.com:8443/x").unwrap(),
            ("example.com".to_string(), 8443, "/x".to_string())
        );
    }

    #[test]
    fn a_url_this_client_cannot_take_is_refused_in_words() {
        assert_eq!(
            send(&Request {
                method: "GET",
                url: "http://example.com/",
                headers: &[],
                body: &[],
            })
            .unwrap_err(),
            "not https: http://example.com/"
        );
        assert_eq!(
            split_url("ftp://example.com/").unwrap_err(),
            "not https: ftp://example.com/"
        );
        assert_eq!(
            split_url("https:///nowhere").unwrap_err(),
            "no host in the url: https:///nowhere"
        );
        assert_eq!(
            split_url("https://example.com:donkey/x").unwrap_err(),
            "not a port in the url: https://example.com:donkey/x"
        );
    }

    #[test]
    fn text_stops_at_the_cap() {
        let body = "x".repeat(1000);
        let r = Response {
            status: 200,
            headers: Vec::new(),
            body: Box::new(Cursor::new(body.into_bytes())),
        };
        assert_eq!(r.text(10).unwrap(), "xxxxxxxxxx");
    }

    #[test]
    fn a_header_is_found_whatever_case_it_is_asked_in() {
        let r = Response {
            status: 200,
            headers: vec![
                ("content-type".to_string(), "text/event-stream".to_string()),
                ("content-type".to_string(), "text/plain".to_string()),
            ],
            body: Box::new(Cursor::new(Vec::new())),
        };
        assert_eq!(r.header("Content-Type"), Some("text/event-stream"));
        assert_eq!(r.header("CONTENT-TYPE"), Some("text/event-stream"));
        assert_eq!(r.header("etag"), None);
    }

    #[test]
    fn a_failure_reads_as_a_sentence() {
        let refused = std::io::Error::from(ErrorKind::ConnectionRefused);
        assert_eq!(
            sentence("gateway.ai.cloudflare.com", &refused),
            "gateway.ai.cloudflare.com: connection refused"
        );
        // A failure that named the host already is not made to say it twice.
        let quiet = std::io::Error::new(
            ErrorKind::TimedOut,
            "gateway.ai.cloudflare.com: no bytes for 60 s".to_string(),
        );
        assert_eq!(
            sentence("gateway.ai.cloudflare.com", &quiet),
            "gateway.ai.cloudflare.com: no bytes for 60 s"
        );
        // A name keeps its capitals.
        let tls = std::io::Error::other("invalid peer certificate: UnknownIssuer".to_string());
        assert_eq!(
            sentence("example.com", &tls),
            "example.com: invalid peer certificate: UnknownIssuer"
        );
    }

    #[test]
    fn the_default_patience_is_the_one_a_thinking_model_needs() {
        let t = Timeouts::default();
        assert_eq!(t.connect, Duration::from_secs(30));
        assert_eq!(t.first_byte, Duration::from_secs(120));
        assert_eq!(t.idle, Duration::from_secs(60));
    }

    /// The real thing, over the real network. Ignored by default — `cargo
    /// test -- --ignored` is what asks for it — because a test suite that
    /// needs a network is a test suite that fails on a train.
    #[test]
    #[ignore]
    fn a_real_request_reaches_a_real_host() {
        let r = send(&Request {
            method: "GET",
            url: "https://example.com/",
            headers: &[],
            body: &[],
        })
        .unwrap();
        assert_eq!(r.status, 200);
        assert!(r.text(1 << 20).unwrap().contains("Example Domain"));
    }

    /// The same, with a chunked body streamed a line at a time — the shape a
    /// gateway's answer has.
    #[test]
    #[ignore]
    fn a_real_chunked_body_streams_a_line_at_a_time() {
        let r = send(&Request {
            method: "GET",
            url: "https://httpbin.org/stream/3",
            headers: &[],
            body: &[],
        })
        .unwrap();
        assert_eq!(r.status, 200);
        let lines: Vec<String> = std::io::BufReader::new(r.body)
            .lines()
            .map(|l| l.unwrap())
            .filter(|l| !l.trim().is_empty())
            .collect();
        assert_eq!(lines.len(), 3, "{lines:?}");
        for line in lines {
            serde_json::from_str::<serde_json::Value>(&line).expect("a line of json");
        }
    }
}
