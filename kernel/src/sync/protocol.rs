//! The conversation two devices have over one byte stream.
//!
//! Both sides say [`Frame::Hello`], then what they hold, then what the
//! other lacks — from every origin either of them holds, so a laptop
//! carries a phone's ops to a tablet and the roster need not be fully
//! connected at once. The connection then stays open: ops that land here
//! go out on it, and a peer's ops are applied as they arrive.
//!
//! Nothing is acknowledged. A lost frame is harmless because the next
//! [`Frame::Have`] says what is actually held, and a reconnect starts
//! there.
//!
//! The frames are JSON with a four-byte length in front, over
//! `AsyncRead + AsyncWrite`: the kernel's tests drive it over
//! `tokio::io::duplex`, and [`super::iroh::Link`] is the other stream it
//! runs on.

use std::collections::HashMap;
use std::sync::Arc;

use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
use tokio::sync::{mpsc, watch};
use tokio::task::JoinHandle;

use super::Op;
use crate::store::Db;

/// The protocol this build speaks. A peer at another number is refused.
pub const VERSION: u32 = 1;

/// How many ops travel in one frame at most. A frame also stops at
/// [`BUDGET`] bytes, because one note's body is as large as somebody made
/// it.
pub const BATCH: usize = 512;

/// About how many bytes of ops go in one frame.
const BUDGET: usize = 4 * 1024 * 1024;

/// The longest frame this will read. Anything larger is not one of ours,
/// and is refused before a byte of it is allocated.
const MAX_FRAME: u32 = 64 * 1024 * 1024;

/// One frame.
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(tag = "frame", rename_all = "lowercase")]
enum Frame {
    /// Both sides, first: who I am, what I call myself, and — when this is
    /// a *pair with* — the secret the other device's ticket showed.
    Hello {
        v: u32,
        device: String,
        name: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pairing: Option<String>,
    },
    /// What I hold, contiguous, per origin.
    Have { have: Vec<(String, i64)> },
    /// What you lack, in `(origin, seq)` order.
    Ops { ops: Vec<Op> },
}

/// What the other end said about itself.
#[derive(Clone, Debug, PartialEq)]
pub struct Greeting {
    pub device: String,
    pub name: String,
    /// The pairing secret it showed, if any.
    pub pairing: Option<[u8; 16]>,
}

/// What the caller makes of a greeting. The caller holds the roster and
/// this device's live pairing secret; the protocol holds neither.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Verdict {
    /// A device already in `sync_peer` and not removed.
    Known,
    /// Not a peer yet, but it showed the right secret: it is added to the
    /// roster through an ordinary write, so the roster replicates.
    Pair,
    /// Not ours. The link closes before any `Have`.
    Refused,
}

/// Which end of the connection this is.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Side {
    /// We dialed, showing a pairing secret if this is a *pair with*.
    Dial(Option<[u8; 16]>),
    /// We answered.
    Listen,
}

/// What one connection did, from `Hello` to close.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Exchange {
    /// Who was on the other end.
    pub peer: String,
    pub name: String,
    /// Ops put on the wire.
    pub sent: u64,
    /// Ops taken off it.
    pub received: u64,
    /// Of those, the ones that moved a cell here.
    pub applied: u64,
    /// Of those, the ones kept and not applied.
    pub skipped: u64,
    /// Of those, the ones a constraint here refused, and what the first one
    /// said — the problem the service reports once.
    pub refused: u64,
    pub refusal: Option<String>,
}

/// What can end a session.
#[derive(Debug)]
pub enum Error {
    /// The other end went away. The ordinary end of a session.
    Closed,
    Io(std::io::Error),
    /// A frame that is not ours, or one too large to be.
    Malformed(String),
    /// The other end speaks another version of this.
    Version(u32),
    /// The roster said no, before any `Have`.
    Refused,
    Store(rusqlite::Error),
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Closed => f.write_str("the other device closed the connection"),
            Self::Io(e) => write!(f, "the connection failed: {e}"),
            Self::Malformed(s) => write!(f, "that is not one of our frames: {s}"),
            Self::Version(v) => write!(f, "the other device speaks sync {v}, and this one {VERSION}"),
            Self::Refused => f.write_str("that device is not one of ours"),
            Self::Store(e) => write!(f, "the store refused: {e}"),
        }
    }
}

impl std::error::Error for Error {}

impl From<std::io::Error> for Error {
    fn from(e: std::io::Error) -> Error {
        if e.kind() == std::io::ErrorKind::UnexpectedEof {
            Error::Closed
        } else {
            Error::Io(e)
        }
    }
}

impl From<rusqlite::Error> for Error {
    fn from(e: rusqlite::Error) -> Error {
        Error::Store(e)
    }
}

/// One session, from `Hello` until the link closes or fails. Dropping this
/// future ends the session whole — the reader and the writer go with it —
/// which is how a caller cancels one.
///
/// `roster` answers for the other end — it is the caller that knows the
/// roster and this device's live pairing secret. `wake` is the store's
/// *ops landed* counter: every bump is a pass over the log, so a local
/// commit reaches every open connection and a carried op reaches the next
/// device along.
///
/// # Errors
///
/// [`Error::Refused`] when the roster says no, and whatever ended the
/// connection otherwise. [`Error::Closed`] is the ordinary end.
pub async fn run<L>(
    db: Arc<Db>,
    link: L,
    side: Side,
    roster: impl Fn(&Greeting) -> Verdict,
    mut wake: watch::Receiver<u64>,
) -> Result<Exchange, Error>
where
    L: AsyncRead + AsyncWrite + Unpin + Send + 'static,
{
    let (mut read, mut write) = tokio::io::split(link);
    let (mine, name) = db.identity().await?;
    let pairing = match side {
        Side::Dial(secret) => secret.map(|s| hex(&s)),
        Side::Listen => None,
    };
    send(&mut write, &Frame::Hello { v: VERSION, device: mine, name, pairing }).await?;
    let greeting = match frame(&mut read).await? {
        Frame::Hello { v, .. } if v != VERSION => return Err(Error::Version(v)),
        Frame::Hello { v: _, device, name, pairing } => Greeting {
            device,
            name,
            pairing: pairing.as_deref().and_then(unhex),
        },
        other => return Err(Error::Malformed(format!("{other:?} came before a hello"))),
    };
    let verdict = roster(&greeting);
    if verdict == Verdict::Refused {
        return Err(Error::Refused);
    }
    let mut out = Exchange {
        peer: greeting.device.clone(),
        name: greeting.name.clone(),
        ..Exchange::default()
    };

    send(&mut write, &Frame::Have { have: db.have().await? }).await?;
    let mut cursor: HashMap<String, i64> = match frame(&mut read).await? {
        Frame::Have { have } => have.into_iter().collect(),
        other => return Err(Error::Malformed(format!("{other:?} came before a have"))),
    };
    // The roster gains a new device only once the other end has answered,
    // so a refused pairing leaves nothing behind on either side.
    if verdict == Verdict::Pair {
        db.add_peer(&greeting.device, &greeting.name).await?;
    }

    // The reader is its own task so that waiting for a local commit never
    // cancels a half-read frame.
    let (frames, mut inbox) = mpsc::channel::<Result<Frame, Error>>(4);
    let reading = frames.clone();
    let reader = crate::runtime::spawn(async move {
        loop {
            tokio::select! {
                () = reading.closed() => return,
                next = frame(&mut read) => {
                    let ended = next.is_err();
                    if reading.send(next).await.is_err() || ended { return; }
                }
            }
        }
    });
    // And the writer is its own task for the same reason, the other way
    // round: a frame half-written cannot be taken back, so sending never
    // sits in a `select!` arm. Two devices that both come up with a
    // backlog would otherwise each send until the other's pipe was full
    // and neither would ever read; here the loop below only ever waits for
    // *room* in this queue, and it takes the inbox in preference to that.
    let (outbox, mut queue) = mpsc::channel::<Frame>(2);
    let failed = frames;
    let writer = crate::runtime::spawn(async move {
        while let Some(frame) = queue.recv().await {
            if let Err(e) = send(&mut write, &frame).await {
                let _ = failed.send(Err(e)).await;
                return;
            }
        }
    });
    // Both halves end with this future, however it ends. A session is
    // cancelled by dropping it — which is what forgetting a device does —
    // and a writer left behind, blocked on a full pipe, would go on sending
    // to a device this one has just dropped.
    let _halves = Halves(reader, writer);

    // What the peer lacks may still be going out. The cursor says where
    // the backlog stands, and a `Have` puts it back.
    let mut backlog = true;
    loop {
        if backlog {
            tokio::select! {
                biased;
                got = inbox.recv() => match heard(&db, got, &mut cursor, &mut out).await? {
                    Heard::Ended => return Ok(out),
                    // Already sending: a `Have` only moves where from, and
                    // `heard` has moved it.
                    Heard::Asked | Heard::Took => {}
                },
                room = outbox.reserve() => match room {
                    // The writer gave up; what it hit is on its way to the
                    // inbox, which the next pass reads.
                    Err(_) => backlog = false,
                    Ok(room) => match next(&db, &mut cursor, &mut out).await? {
                        Some(frame) => room.send(frame),
                        None => backlog = false,
                    },
                },
            }
        } else {
            tokio::select! {
                got = inbox.recv() => match heard(&db, got, &mut cursor, &mut out).await? {
                    Heard::Ended => return Ok(out),
                    Heard::Asked => backlog = true,
                    // What was applied landed in the log, so the wake this
                    // session also watches says so for itself.
                    Heard::Took => {}
                },
                moved = wake.changed() => {
                    if moved.is_err() { return Ok(out); }
                    backlog = true;
                }
            }
        }
    }
}

/// The reader and the writer of one session. Dropping it aborts both, so
/// nothing is left holding a half of the stream once the session is over.
struct Halves(JoinHandle<()>, JoinHandle<()>);

impl Drop for Halves {
    fn drop(&mut self) {
        self.0.abort();
        self.1.abort();
    }
}

/// What one inbound frame left behind.
enum Heard {
    /// The other end is gone.
    Ended,
    /// Ops, applied here. Whatever they were, the peer has them.
    Took,
    /// A `Have`: the cursor went back, so there is a backlog again.
    Asked,
}

/// One frame off the inbox, applied.
async fn heard(
    db: &Arc<Db>,
    got: Option<Result<Frame, Error>>,
    cursor: &mut HashMap<String, i64>,
    out: &mut Exchange,
) -> Result<Heard, Error> {
    match got {
        None | Some(Err(Error::Closed)) => Ok(Heard::Ended),
        Some(Err(e)) => Err(e),
        Some(Ok(Frame::Ops { ops })) => {
            // Whatever it sent, it has: never send it back.
            for op in &ops {
                let at = cursor.entry(op.origin.clone()).or_insert(0);
                *at = (*at).max(op.seq);
            }
            out.received += ops.len() as u64;
            let applied = db.apply_ops(ops).await?;
            out.applied += applied.applied;
            out.skipped += applied.skipped;
            out.refused += applied.refused;
            if out.refusal.is_none() {
                out.refusal = applied.refusal;
            }
            Ok(Heard::Took)
        }
        Some(Ok(Frame::Have { have })) => {
            for (origin, seq) in have {
                let at = cursor.entry(origin).or_insert(0);
                *at = (*at).max(seq);
            }
            Ok(Heard::Asked)
        }
        Some(Ok(other)) => Err(Error::Malformed(format!("{other:?} mid-session"))),
    }
}

/// The next frame of what the peer lacks, or `None` when it lacks nothing.
/// The cursor moves over exactly what goes in the frame, so a session that
/// ends mid-backlog leaves the rest for the next `Have`.
async fn next(
    db: &Arc<Db>,
    cursor: &mut HashMap<String, i64>,
    out: &mut Exchange,
) -> Result<Option<Frame>, Error> {
    let have: Vec<(String, i64)> = cursor.iter().map(|(o, s)| (o.clone(), *s)).collect();
    let ops = db.ops_since(have, BATCH).await?;
    if ops.is_empty() {
        return Ok(None);
    }
    // A frame stops at the budget as well as at the batch, because one
    // note's body is as large as somebody made it.
    let mut bytes = 0;
    let mut room = 0;
    for op in &ops {
        if room > 0 && bytes + weight(op) > BUDGET {
            break;
        }
        bytes += weight(op);
        room += 1;
    }
    let ops: Vec<Op> = ops.into_iter().take(room).collect();
    for op in &ops {
        let at = cursor.entry(op.origin.clone()).or_insert(0);
        *at = (*at).max(op.seq);
    }
    out.sent += ops.len() as u64;
    Ok(Some(Frame::Ops { ops }))
}

/// About how much of a frame one op takes. An estimate is enough: the
/// budget is what keeps a frame from being as large as the log.
fn weight(op: &Op) -> usize {
    let val = match &op.val {
        serde_json::Value::String(s) => s.len() * 2,
        other => other.to_string().len(),
    };
    op.origin.len() + op.tbl.len() + op.key.len() + op.col.len() + val + 64
}

/// One frame out: four bytes of length, then the JSON.
async fn send<W: AsyncWrite + Unpin>(write: &mut W, frame: &Frame) -> Result<(), Error> {
    let body = serde_json::to_vec(frame).map_err(|e| Error::Malformed(e.to_string()))?;
    let too_big = || Error::Malformed("a frame too large to send".into());
    let n = u32::try_from(body.len()).map_err(|_| too_big())?;
    if n > MAX_FRAME {
        return Err(too_big());
    }
    write.write_all(&n.to_be_bytes()).await?;
    write.write_all(&body).await?;
    write.flush().await?;
    Ok(())
}

/// One frame in.
async fn frame<R: AsyncRead + Unpin>(read: &mut R) -> Result<Frame, Error> {
    let mut len = [0u8; 4];
    read.read_exact(&mut len).await?;
    let n = u32::from_be_bytes(len);
    if n > MAX_FRAME {
        return Err(Error::Malformed(format!("a frame of {n} bytes")));
    }
    let mut body = vec![0u8; n as usize];
    read.read_exact(&mut body).await?;
    serde_json::from_slice(&body).map_err(|e| Error::Malformed(e.to_string()))
}

/// The pairing secret as a frame carries it.
fn hex(secret: &[u8; 16]) -> String {
    secret.iter().map(|b| format!("{b:02x}")).collect()
}

fn unhex(s: &str) -> Option<[u8; 16]> {
    if s.len() != 32 {
        return None;
    }
    let mut out = [0u8; 16];
    for (i, byte) in out.iter_mut().enumerate() {
        *byte = u8::from_str_radix(s.get(i * 2..i * 2 + 2)?, 16).ok()?;
    }
    Some(out)
}
