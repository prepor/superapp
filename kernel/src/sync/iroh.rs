//! The endpoint two devices meet on.
//!
//! A device's identity is an Ed25519 key it makes once and keeps in the
//! platform secret store. Its public half is the endpoint id, and iroh proves
//! it on every connection: whoever answers a dial to an id holds that key.
//! So this module owes the layer above it only three things — an id, a
//! reachable address for it, and a bidirectional byte stream to another one.
//!
//! [`Mode`] is the whole of the configuration. [`Mode::Internet`] is what a
//! real device binds: the n0 preset (public relays and DNS lookup) plus
//! local-network lookup, so two devices on one Wi-Fi never leave it.
//! [`Mode::Loopback`] binds `127.0.0.1` with no relay and no lookup, which is
//! what an e2e walk wants when the two peers are two processes on one
//! machine; a ticket made in that mode carries the bound port, and nothing
//! off the machine can use it.
//!
//! [`Link`] is a QUIC bidirectional stream that implements
//! [`tokio::io::AsyncRead`] and [`tokio::io::AsyncWrite`], so the protocol
//! written over `tokio::io::duplex` runs over it unchanged.

use std::fmt;
use std::io;
use std::pin::Pin;
use std::task::{Context, Poll};

use iroh::endpoint::{presets, Connection, RecvStream, SendStream};
use iroh::{Endpoint, SecretKey, TransportAddr};
use iroh_mdns_address_lookup::MdnsAddressLookup;
use iroh_tickets::endpoint::EndpointTicket;
use iroh_tickets::Ticket;
use tokio::io::ReadBuf;

pub use iroh::{EndpointAddr, EndpointId};

/// The protocol name on the wire. A dial that does not name it is refused by
/// the TLS handshake, before any frame is read.
pub const ALPN: &[u8] = b"superapp/sync/1";

/// The prefix on a pairing ticket, so a pasted string that is not one says so
/// before anything is decoded.
const TICKET_KIND: &str = "superapp-";

/// The pairing secret is sixteen fresh random bytes, and the ticket carries
/// them after the endpoint's own.
const PAIRING_LEN: usize = 16;

/// How the endpoint reaches the world.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    /// Public relays, the n0 DNS lookup, and local-network lookup. What a
    /// device binds.
    Internet,
    /// One loopback socket, no relay, no lookup. What two processes on one
    /// machine bind.
    Loopback,
}

/// What can go wrong between two endpoints. Everything iroh reports is
/// flattened to its message: the layer above turns these into a peer's *last
/// error*, and nothing branches on the cause.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NetError {
    /// The endpoint could not bind — a port in use, a missing interface.
    Bind(String),
    /// The peer did not answer, or refused the protocol.
    Dial(String),
    /// A pasted string was not one of ours.
    Ticket(String),
}

impl fmt::Display for NetError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Bind(s) => write!(f, "cannot bind the sync endpoint: {s}"),
            Self::Dial(s) => write!(f, "cannot reach the device: {s}"),
            Self::Ticket(s) => write!(f, "not a pairing ticket: {s}"),
        }
    }
}

impl std::error::Error for NetError {}

/// One bound endpoint: this device's half of every connection.
pub struct Net {
    endpoint: Endpoint,
    mode: Mode,
}

impl Net {
    /// Binds the endpoint on `secret`, which is the device's key and decides
    /// its id.
    ///
    /// This returns as soon as the socket is bound. In [`Mode::Internet`] the
    /// endpoint has not yet picked a relay, so its address is incomplete
    /// until [`Net::online`] resolves — wait for that before showing a
    /// ticket.
    pub async fn bind(secret: [u8; 32], mode: Mode) -> Result<Net, NetError> {
        let key = SecretKey::from_bytes(&secret);
        let alpns = vec![ALPN.to_vec()];
        let bound = match mode {
            Mode::Internet => {
                Endpoint::builder(presets::N0)
                    .secret_key(key)
                    .alpns(alpns)
                    // Two devices on one Wi-Fi find each other without the
                    // relay or the DNS server hearing about it.
                    .address_lookup(MdnsAddressLookup::builder())
                    .bind()
                    .await
            }
            Mode::Loopback => {
                // `clear_ip_transports` drops the two unspecified binds the
                // builder comes with, so the only address this endpoint has
                // is the loopback one, and the only ticket it can make is one
                // no other machine could use.
                Endpoint::builder(presets::Minimal)
                    .secret_key(key)
                    .alpns(alpns)
                    .clear_ip_transports()
                    .bind_addr("127.0.0.1:0")
                    .map_err(|e| NetError::Bind(e.to_string()))?
                    .bind()
                    .await
            }
        };
        let endpoint = bound.map_err(|e| NetError::Bind(e.to_string()))?;
        Ok(Net { endpoint, mode })
    }

    /// Resolves once the endpoint is reachable from off the machine — in
    /// [`Mode::Internet`], once it holds a relay connection. Returns at once
    /// in [`Mode::Loopback`], where there is no relay to wait for.
    pub async fn online(&self) {
        if self.mode == Mode::Internet {
            self.endpoint.online().await;
        }
    }

    /// This device's id: the hex encoding of its public key, which is what
    /// [`EndpointId`] prints.
    pub fn id(&self) -> String {
        self.endpoint.id().to_string()
    }

    /// The first eight characters of [`Net::id`] — enough to tell two devices
    /// apart on a panel, and short enough to sit next to a name.
    pub fn short_id(&self) -> String {
        self.id().chars().take(8).collect()
    }

    /// Where this endpoint can be reached right now.
    ///
    /// In [`Mode::Internet`] this is iroh's own view: the home relay and
    /// whatever direct addresses it has found, which is empty until
    /// [`Net::online`] resolves. In [`Mode::Loopback`] iroh reports no direct
    /// address at all — its net report drops loopback whenever a real
    /// interface exists — so the bound socket is the address, and it is known
    /// the moment [`Net::bind`] returns.
    pub fn addr(&self) -> EndpointAddr {
        match self.mode {
            Mode::Internet => self.endpoint.addr(),
            Mode::Loopback => EndpointAddr::from_parts(
                self.endpoint.id(),
                self.endpoint
                    .bound_sockets()
                    .into_iter()
                    .map(TransportAddr::Ip),
            ),
        }
    }

    /// This device's ticket: where to reach it, and the pairing secret that
    /// lets a stranger in. One string, prefixed `superapp-`, worthless once
    /// the panel that showed it closes and the secret is rotated.
    pub fn ticket(&self, pairing: [u8; PAIRING_LEN]) -> String {
        SyncTicket {
            addr: self.addr(),
            pairing,
        }
        .encode_string()
    }

    /// Opens a connection to `addr` on our ALPN and a stream on it.
    ///
    /// The stream is lazy in QUIC's sense: the other end's [`Net::accept`]
    /// does not see it until the first byte is written, which the protocol's
    /// `Hello` supplies.
    pub async fn dial(&self, addr: EndpointAddr) -> Result<Link, NetError> {
        let conn = self
            .endpoint
            .connect(addr, ALPN)
            .await
            .map_err(|e| NetError::Dial(e.to_string()))?;
        let (send, recv) = conn
            .open_bi()
            .await
            .map_err(|e| NetError::Dial(e.to_string()))?;
        Ok(Link { conn, send, recv })
    }

    /// Waits for the next peer to arrive and for its first stream.
    ///
    /// Returns the dialer's id — proved by the handshake, not claimed by it —
    /// and the stream. `None` means the endpoint was closed. A connection
    /// that fails to establish is skipped rather than reported: any host on
    /// the network can send a packet to this socket.
    pub async fn accept(&self) -> Option<(String, Link)> {
        loop {
            let incoming = self.endpoint.accept().await?;
            let Ok(conn) = incoming.await else { continue };
            if conn.alpn() != ALPN {
                continue;
            }
            let remote = conn.remote_id().to_string();
            let Ok((send, recv)) = conn.accept_bi().await else {
                continue;
            };
            return Some((remote, Link { conn, send, recv }));
        }
    }

    /// Closes the endpoint and every connection on it, telling the peers why
    /// rather than leaving them to time out.
    pub async fn close(&self) {
        self.endpoint.close().await;
    }
}

impl fmt::Debug for Net {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Net")
            .field("id", &self.short_id())
            .field("mode", &self.mode)
            .finish()
    }
}

/// Reads a ticket: where the other device is, and the secret it will want to
/// hear.
pub fn parse_ticket(s: &str) -> Result<(EndpointAddr, [u8; PAIRING_LEN]), NetError> {
    let ticket =
        SyncTicket::decode_string(s.trim()).map_err(|e| NetError::Ticket(e.to_string()))?;
    Ok((ticket.addr, ticket.pairing))
}

/// An address as the store keeps one: iroh's own endpoint ticket, so a
/// later iroh that learns a new kind of address needs nothing here.
#[must_use]
pub fn addr_text(addr: &EndpointAddr) -> String {
    EndpointTicket::new(addr.clone()).encode_string()
}

/// The other direction. `None` for anything that is not one of ours, so a
/// row from an older build is a dial to the bare id and not a failure.
#[must_use]
pub fn addr_from_text(s: &str) -> Option<EndpointAddr> {
    EndpointTicket::decode_string(s.trim())
        .ok()
        .map(|t| t.endpoint_addr().clone())
}

/// A ticket is an [`EndpointTicket`] with the pairing secret after it. The
/// endpoint half is iroh's own encoding, so a later iroh that learns a new
/// kind of address needs nothing here.
#[derive(Debug, Clone, PartialEq, Eq)]
struct SyncTicket {
    addr: EndpointAddr,
    pairing: [u8; PAIRING_LEN],
}

impl Ticket for SyncTicket {
    const KIND: &'static str = TICKET_KIND;

    fn encode_bytes(&self) -> Vec<u8> {
        let mut out = EndpointTicket::new(self.addr.clone()).encode_bytes();
        out.extend_from_slice(&self.pairing);
        out
    }

    fn decode_bytes(bytes: &[u8]) -> Result<Self, iroh_tickets::ParseError> {
        let cut = bytes
            .len()
            .checked_sub(PAIRING_LEN)
            .ok_or_else(|| iroh_tickets::ParseError::verification_failed("ticket is too short"))?;
        let (head, tail) = bytes.split_at(cut);
        let addr = EndpointTicket::decode_bytes(head)?.endpoint_addr().clone();
        let mut pairing = [0u8; PAIRING_LEN];
        pairing.copy_from_slice(tail);
        Ok(SyncTicket { addr, pairing })
    }
}

/// One bidirectional stream to one peer.
///
/// Holding it holds the connection open: drop it and the peer sees the
/// connection go, unflushed bytes with it. It is `AsyncRead + AsyncWrite`, so
/// [`tokio::io::split`] gives the two halves to two tasks.
pub struct Link {
    /// Kept because dropping the last handle closes the connection under the
    /// streams.
    conn: Connection,
    send: SendStream,
    recv: RecvStream,
}

impl Link {
    /// The peer on the other end, as [`Net::id`] would print it.
    pub fn remote_id(&self) -> String {
        self.conn.remote_id().to_string()
    }

    /// Where the peer is being reached on this connection — the sockets it
    /// is on, and the relay carrying it. The service writes this down, so
    /// the next dial goes straight there instead of asking the network who
    /// that is.
    pub fn remote_addr(&self) -> EndpointAddr {
        let addrs: Vec<TransportAddr> = self
            .conn
            .paths()
            .iter()
            .map(|p| p.remote_addr().clone())
            .collect();
        EndpointAddr::from_parts(self.conn.remote_id(), addrs)
    }
}

impl fmt::Debug for Link {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Link")
            .field("remote", &self.remote_id())
            .finish()
    }
}

impl tokio::io::AsyncRead for Link {
    fn poll_read(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        // `RecvStream` has inherent poll methods too; name the trait's.
        tokio::io::AsyncRead::poll_read(Pin::new(&mut self.get_mut().recv), cx, buf)
    }
}

impl tokio::io::AsyncWrite for Link {
    fn poll_write(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<io::Result<usize>> {
        // `SendStream::poll_write` is an inherent method returning iroh's
        // own error; the trait's is the one this impl owes.
        tokio::io::AsyncWrite::poll_write(Pin::new(&mut self.get_mut().send), cx, buf)
    }

    fn poll_flush(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        tokio::io::AsyncWrite::poll_flush(Pin::new(&mut self.get_mut().send), cx)
    }

    fn poll_shutdown(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        tokio::io::AsyncWrite::poll_shutdown(Pin::new(&mut self.get_mut().send), cx)
    }
}

/// Where the device key lives in the platform secret store, beside the R2
/// token and the mail passwords.
pub const SECRET_KEY: &str = "sync/key";

/// A fresh device key: thirty-two random bytes, made once on a device's
/// first open and kept. A scripted run makes one in its in-memory secrets.
#[must_use]
pub fn new_secret() -> [u8; 32] {
    use ring::rand::SecureRandom;
    let mut secret = [0u8; 32];
    ring::rand::SystemRandom::new()
        .fill(&mut secret)
        .expect("the system random source");
    secret
}

/// The endpoint id a key answers to — this device's id, sixty-four
/// lowercase hex characters. Reading it binds nothing.
#[must_use]
pub fn device_of(secret: &[u8; 32]) -> String {
    SecretKey::from_bytes(secret).public().to_string()
}

/// Lowercase hex, one nibble per character.
const HEX: &[u8; 16] = b"0123456789abcdef";

/// The device key as the secret store keeps it: sixty-four lowercase hex
/// characters. `None` for anything else, so a corrupted entry is a fresh
/// identity and not a panic.
pub fn secret_from_hex(s: &str) -> Option<[u8; 32]> {
    let s = s.trim();
    if s.len() != 64 {
        return None;
    }
    let mut out = [0u8; 32];
    for (i, byte) in out.iter_mut().enumerate() {
        *byte = u8::from_str_radix(s.get(i * 2..i * 2 + 2)?, 16).ok()?;
    }
    Some(out)
}

/// The other direction: what to write into the secret store.
pub fn secret_to_hex(secret: &[u8; 32]) -> String {
    let mut out = String::with_capacity(64);
    for byte in secret {
        out.push(HEX[usize::from(byte >> 4)] as char);
        out.push(HEX[usize::from(byte & 0x0f)] as char);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    /// A key that is the same on every run, so a failure is the same failure.
    fn key(n: u8) -> [u8; 32] {
        let mut k = [0u8; 32];
        k[0] = n;
        k[31] = 0x2a;
        k
    }

    /// What a secret key's public half prints as, without binding anything.
    fn key_id(secret: [u8; 32]) -> String {
        SecretKey::from_bytes(&secret).public().to_string()
    }

    /// Two endpoints on one machine, one dialing the other on our ALPN: a few
    /// bytes each way through [`Link`], and the acceptor knowing who dialed.
    ///
    /// This is the e2e walk's shape in miniature — no relay, no lookup, and
    /// the address is the bound socket.
    #[tokio::test]
    async fn two_loopback_endpoints_exchange_bytes() {
        let server = Net::bind(key(1), Mode::Loopback)
            .await
            .expect("bind server");
        let client = Net::bind(key(2), Mode::Loopback)
            .await
            .expect("bind client");

        // The bound socket must be in the addr, or the dial has nowhere to go.
        let addr = server.addr();
        assert_eq!(addr.id.to_string(), key_id(key(1)));
        assert!(
            addr.ip_addrs().any(|a| a.ip().is_loopback()),
            "a loopback addr carries its bound socket: {addr:?}"
        );

        let client_id = client.id();
        let accepting = tokio::spawn(async move {
            let (remote, mut link) = server.accept().await.expect("a peer arrives");
            let mut buf = [0u8; 5];
            link.read_exact(&mut buf).await.expect("read the greeting");
            link.write_all(b"world").await.expect("answer");
            // Stay until the dialer has read the answer and hung up: a
            // dropped endpoint takes bytes still in flight with it.
            let mut eof = [0u8; 1];
            let read = link.read(&mut eof).await.expect("wait for the hang-up");
            assert_eq!(read, 0, "the dialer finished its side");
            (remote, buf)
        });

        let mut link = tokio::time::timeout(Duration::from_secs(5), client.dial(addr))
            .await
            .expect("the dial does not hang")
            .expect("the dial succeeds");
        assert_eq!(link.remote_id(), key_id(key(1)));
        link.write_all(b"hello").await.expect("greet");
        let mut back = [0u8; 5];
        link.read_exact(&mut back).await.expect("read the answer");
        assert_eq!(&back, b"world");
        link.shutdown().await.expect("finish our side");

        let (remote, greeting) = tokio::time::timeout(Duration::from_secs(5), accepting)
            .await
            .expect("the acceptor finishes")
            .expect("the acceptor does not panic");
        assert_eq!(&greeting, b"hello");
        assert_eq!(remote, client_id, "the acceptor sees the dialer's id");

        client.close().await;
    }

    /// A ticket is one string with a `superapp-` prefix, and it comes back as
    /// the address and the sixteen bytes that went in.
    #[tokio::test]
    async fn a_ticket_survives_the_round_trip() {
        let net = Net::bind(key(3), Mode::Loopback).await.expect("bind");
        let pairing = [7u8; PAIRING_LEN];
        let ticket = net.ticket(pairing);

        assert!(
            ticket.starts_with(TICKET_KIND),
            "a ticket says what it is: {ticket}"
        );
        let (addr, back) = parse_ticket(&ticket).expect("our own ticket parses");
        assert_eq!(addr, net.addr());
        assert_eq!(back, pairing);
        assert!(addr.ip_addrs().any(|a| a.ip().is_loopback()));

        // Whitespace from a paste is not a failure; the wrong kind is.
        let pasted = format!("  {ticket}\n");
        assert_eq!(parse_ticket(&pasted).expect("a pasted ticket").1, pairing);
        assert!(matches!(
            parse_ticket("endpointaaaa"),
            Err(NetError::Ticket(_))
        ));
        assert!(matches!(parse_ticket(""), Err(NetError::Ticket(_))));

        net.close().await;
    }

    /// A dial to an id nobody answers fails; it does not hang. With no relay
    /// and no lookup there is nowhere to look, and iroh says so at once
    /// rather than after a connection timeout.
    #[tokio::test]
    async fn a_dial_to_an_unknown_id_fails_promptly() {
        let net = Net::bind(key(4), Mode::Loopback).await.expect("bind");
        let stranger = EndpointAddr::new(SecretKey::from_bytes(&key(5)).public());

        let outcome = tokio::time::timeout(Duration::from_secs(5), net.dial(stranger))
            .await
            .expect("the dial is bounded");
        assert!(matches!(outcome, Err(NetError::Dial(_))), "{outcome:?}");

        net.close().await;
    }

    /// The secret store keeps strings; the endpoint wants bytes.
    #[test]
    fn a_secret_round_trips_through_hex() {
        let secret = key(9);
        let hex = secret_to_hex(&secret);
        assert_eq!(hex.len(), 64);
        assert_eq!(secret_from_hex(&hex), Some(secret));
        let padded = format!(" {hex} ");
        assert_eq!(secret_from_hex(&padded), Some(secret));
        assert_eq!(secret_from_hex("beef"), None);
        assert_eq!(secret_from_hex(&"z".repeat(64)), None);
    }
}
