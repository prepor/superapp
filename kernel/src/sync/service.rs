//! The endpoint, the accept loop and the dial loop, as one kernel task.
//!
//! It is not a worker's pass: a worker answers with one pass and a wake,
//! and this wants a listener and a set of connections that outlive any
//! pass. The session mounts it at boot and stops it before the store
//! closes.
//!
//! What it owns is the [`Net`], one session per peer, and the snapshot the
//! *device sync* panel draws. What it does not own is the roster: that is
//! `sync_peer`, an ordinary replicated table, read again on every pass.
//! Pairing and forgetting are writes to it like any other, so what one
//! device decides reaches the rest.
//!
//! A session stays open, and the protocol pushes every local commit over
//! it. The dial loop is therefore only ever about the peers that have no
//! connection: it tries each one with a backoff from five seconds to five
//! minutes, and a kick — foreground, resume, a pairing — puts them all back
//! to now.

use std::collections::HashMap;
use std::path::PathBuf;
use std::str::FromStr;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use tokio::sync::{mpsc, oneshot, watch};
use tokio::task::JoinHandle;

use super::iroh::{self, EndpointAddr, EndpointId, Link, Mode, Net};
use super::protocol::{self, Greeting, Side, Verdict};
use super::status::{PeerStatus, SyncStatus};
use crate::problems::Problem;
use crate::store::Db;

/// The first wait after a failed dial.
const BACKOFF_MIN: Duration = Duration::from_secs(5);

/// And the longest. A device that is away all day is dialed twelve times an
/// hour, which no relay minds.
const BACKOFF_MAX: Duration = Duration::from_secs(300);

/// A session that lasted at least this long counts as a success, so a peer
/// that refuses us the moment we arrive backs off like any other failure.
const SETTLED: Duration = BACKOFF_MIN;

/// How long a pass sleeps when nobody is waiting to be dialed. The endpoint
/// does the listening, so this is only how often a peer that has never
/// failed is looked at again.
const IDLE: Duration = Duration::from_secs(60);

/// How a run's endpoint is configured. The shell decides: a device binds
/// the internet preset, the two-process walk asks for loopback, and a
/// scripted run that asks for neither mounts no service at all.
pub struct Mount {
    /// The device key. The endpoint binds on it, and its public half is the
    /// id every op this device writes is signed with.
    pub secret: [u8; 32],
    pub mode: Mode,
    /// Where a scripted run drops the ticket the panel is showing, so the
    /// other process of a two-device walk can paste it. Never set on a run
    /// nobody is scripting: a ticket on a person's disk is a ticket that
    /// leaves by a route nobody chose.
    pub ticket_out: Option<PathBuf>,
}

/// What the panel asks of the service.
enum Cmd {
    /// A ticket somebody pasted: where to dial, and the secret to show.
    Pair(Box<EndpointAddr>, [u8; 16]),
    /// A device to drop: `removed` is written, its session ends, and it is
    /// refused from then on.
    Forget(String),
    /// This device's own name, as the panel's field has it.
    Rename(String),
    /// Open a pairing window: a fresh secret, and a ticket once the
    /// endpoint is reachable.
    BeginPairing,
    /// Close it. The ticket that was shown is worthless from here.
    EndPairing,
    /// Try every peer that has no connection, now.
    Kick,
    Stop(oneshot::Sender<()>),
}

/// What a session task tells the loop.
enum Note {
    /// The endpoint is reachable: a ticket can be shown.
    Reachable,
    /// A session is live with this device, reached at this address.
    Up(String, Box<EndpointAddr>),
    /// And it has ended.
    Down {
        device: String,
        /// Why, in one line, or nothing for the ordinary close.
        why: Option<String>,
        /// Ops it sent that a constraint here refused.
        refused: u64,
        refusal: Option<String>,
    },
}

/// The pairing secret this device is showing, while a panel is open. The
/// loop and every session it started share the one cell, so a window that
/// closes — or opens again on a fresh secret — while a handshake is still
/// in the air is answered with what is showing now.
type Showing = Arc<Mutex<Option<[u8; 16]>>>;

/// The pairing window a panel opened. Dropping it closes the window, which
/// is what closing the panel does: a ticket is worthless once the panel
/// that showed it is gone.
pub struct Pairing(mpsc::UnboundedSender<Cmd>);

impl Drop for Pairing {
    fn drop(&mut self) {
        let _ = self.0.send(Cmd::EndPairing);
    }
}

/// The handle the session keeps.
pub struct Service {
    cmd: mpsc::UnboundedSender<Cmd>,
    status: watch::Receiver<SyncStatus>,
}

impl Service {
    /// Binds the endpoint and starts the loops. Returns at once: the bind
    /// happens in the task, and the panel shows what came of it.
    #[must_use]
    pub fn spawn(db: Arc<Db>, mount: Mount, notify: impl Fn() + Send + Sync + 'static) -> Service {
        let (cmd, commands) = mpsc::unbounded_channel();
        let (report, status) = watch::channel(SyncStatus::default());
        crate::runtime::spawn(serve(db, mount, report, commands, Box::new(notify)));
        Service { cmd, status }
    }

    /// The snapshot the panel draws.
    #[must_use]
    pub fn status(&self) -> SyncStatus {
        self.status.borrow().clone()
    }

    /// Whether the snapshot has moved since this was last asked — what the
    /// shell polls, so a connection coming up redraws the panel.
    pub fn moved(&mut self) -> bool {
        if self.status.has_changed().unwrap_or(false) {
            self.status.borrow_and_update();
            return true;
        }
        false
    }

    /// Pairs with what somebody pasted. The ticket is read here, so a
    /// mistyped one is refused on the spot rather than through a status
    /// line.
    ///
    /// # Errors
    ///
    /// If the string is not a ticket, or is this device's own.
    pub fn pair(&self, ticket: &str) -> Result<(), String> {
        let (addr, secret) = iroh::parse_ticket(ticket).map_err(|e| e.to_string())?;
        if addr.id.to_string() == self.status.borrow().device {
            return Err("that is this device's own ticket".into());
        }
        let _ = self.cmd.send(Cmd::Pair(Box::new(addr), secret));
        Ok(())
    }

    /// Drops a device from the roster.
    pub fn forget(&self, device: &str) {
        let _ = self.cmd.send(Cmd::Forget(device.to_string()));
    }

    /// What this device calls itself, written to its own roster row — a
    /// captured write, so the other devices hear it.
    pub fn rename(&self, name: &str) {
        let _ = self.cmd.send(Cmd::Rename(name.to_string()));
    }

    /// Opens a pairing window and hands back the guard that closes it.
    #[must_use]
    pub fn begin_pairing(&self) -> Pairing {
        let _ = self.cmd.send(Cmd::BeginPairing);
        Pairing(self.cmd.clone())
    }

    /// Closes the window now, for a caller that kept no guard.
    pub fn end_pairing(&self) {
        let _ = self.cmd.send(Cmd::EndPairing);
    }

    /// Tries every peer that has no connection, now.
    pub fn kick(&self) {
        let _ = self.cmd.send(Cmd::Kick);
    }

    /// What stands wrong: the ops a constraint here refused. A device that
    /// cannot be reached is not wrong — a phone in a pocket is the ordinary
    /// state of a peer — so it is the panel's line, not a problem.
    #[must_use]
    pub fn problems(&self) -> Vec<Problem> {
        let status = self.status.borrow();
        let mut out = Vec::new();
        if status.refused > 0 {
            let line = format!("{} op(s) another device sent were refused here", status.refused);
            out.push(
                Problem::new("sync:refused", "device sync", line.clone(), &status.refusal)
                    .announcing(line),
            );
        }
        out
    }

    /// Closes every session and the endpoint. Awaited by the shutdown
    /// lifecycle before the store closes.
    pub async fn stop(self) {
        let (ack, done) = oneshot::channel();
        if self.cmd.send(Cmd::Stop(ack)).is_ok() {
            let _ = done.await;
        }
    }
}

// -- the loop ------------------------------------------------------------------

/// One peer, as this run has it.
struct Peer {
    name: String,
    /// The session task, while one is running.
    task: Option<JoinHandle<()>>,
    /// Whether this side dialed it, which is how two devices dialing each
    /// other at once settle on one connection.
    dialed: bool,
    connected: bool,
    /// When the live session began.
    since: Instant,
    last_seen: f64,
    last_error: String,
    /// Consecutive failures, for the backoff.
    tries: u32,
    /// Not dialed again before this.
    next: Instant,
    /// Where it was last reached, from its ticket or from a session.
    addr: Option<EndpointAddr>,
    /// In the roster. A pairing dial in flight is not, yet.
    known: bool,
}

impl Peer {
    fn new(name: String) -> Peer {
        Peer {
            name,
            task: None,
            dialed: false,
            connected: false,
            since: Instant::now(),
            last_seen: 0.0,
            last_error: String::new(),
            tries: 0,
            next: Instant::now(),
            addr: None,
            known: false,
        }
    }

    fn live(&self) -> bool {
        self.task.as_ref().is_some_and(|t| !t.is_finished())
    }
}

struct State {
    db: Arc<Db>,
    net: Option<Arc<Net>>,
    me: String,
    name: String,
    peers: HashMap<String, Peer>,
    /// The live pairing secret, while a panel is open. Shared with the
    /// sessions, which read it when a hello arrives rather than when they
    /// started.
    pairing: Showing,
    ticket: String,
    note: String,
    refused: u64,
    refusal: String,
    ticket_out: Option<PathBuf>,
    notes: mpsc::UnboundedSender<Note>,
}

/// The task: bind, then answer commands, arrivals and sessions until
/// somebody says stop.
async fn serve(
    db: Arc<Db>,
    mount: Mount,
    report: watch::Sender<SyncStatus>,
    mut commands: mpsc::UnboundedReceiver<Cmd>,
    notify: Box<dyn Fn() + Send + Sync>,
) {
    let (me, name) = db.identity().await.unwrap_or_default();
    let (notes, mut inbox) = mpsc::unbounded_channel();
    let (incoming, mut arrivals) = mpsc::channel::<(String, Link)>(4);
    let mut ops = db.ops_landed();
    let mut state = State {
        db,
        net: None,
        me,
        name,
        peers: HashMap::new(),
        pairing: Showing::default(),
        ticket: String::new(),
        note: String::new(),
        refused: 0,
        refusal: String::new(),
        ticket_out: mount.ticket_out,
        notes,
    };
    match Net::bind(mount.secret, mount.mode).await {
        Ok(net) => {
            let net = Arc::new(net);
            state.net = Some(net.clone());
            // The listener is a task of its own: an accept cancelled
            // mid-handshake is a peer that has to dial all over again.
            crate::runtime::spawn(async move {
                while let Some(peer) = net.accept().await {
                    if incoming.send(peer).await.is_err() {
                        return;
                    }
                }
            });
        }
        // A device with no endpoint still answers the panel, and says why.
        Err(e) => state.note = e.to_string(),
    }

    let (mut watching, mut listening) = (true, state.net.is_some());
    state.pass().await;
    publish(&state, &report, &*notify);
    loop {
        let idle = state.idle();
        tokio::select! {
            cmd = commands.recv() => match cmd {
                Some(Cmd::Stop(ack)) => {
                    state.stop().await;
                    let _ = ack.send(());
                    return;
                }
                // The session is gone: nothing is left to serve.
                None => {
                    state.stop().await;
                    return;
                }
                Some(cmd) => state.command(cmd).await,
            },
            got = arrivals.recv(), if listening => match got {
                Some((remote, link)) => state.accepted(remote, link).await,
                None => listening = false,
            },
            got = inbox.recv() => if let Some(note) = got {
                state.heard(note).await;
            },
            moved = ops.changed(), if watching => {
                if moved.is_err() {
                    watching = false;
                } else {
                    // A live session pushes what landed by itself; this
                    // pass is for the peers that have none.
                    state.pass().await;
                }
            },
            () = tokio::time::sleep(idle) => state.pass().await,
        }
        publish(&state, &report, &*notify);
    }
}

/// Replaces the snapshot, and wakes the window when it moved.
fn publish(state: &State, report: &watch::Sender<SyncStatus>, notify: &dyn Fn()) {
    let mut peers: Vec<PeerStatus> = state
        .peers
        .iter()
        .map(|(device, p)| PeerStatus {
            device: device.clone(),
            name: p.name.clone(),
            connected: p.connected,
            last_seen: p.last_seen,
            last_error: p.last_error.clone(),
            behind: 0,
        })
        .collect();
    // One order, whatever a hash map's is: named devices first, then by id.
    peers.sort_by(|a, b| {
        (a.name.is_empty(), &a.name, &a.device).cmp(&(b.name.is_empty(), &b.name, &b.device))
    });
    let next = SyncStatus {
        device: state.me.clone(),
        name: state.name.clone(),
        online: state.net.is_some(),
        peers,
        refused: state.refused,
        refusal: state.refusal.clone(),
        ticket: state.ticket.clone(),
        note: state.note.clone(),
    };
    let moved = report.send_if_modified(|was| {
        let moved = *was != next;
        if moved {
            *was = next;
        }
        moved
    });
    if moved {
        notify();
    }
}

impl State {
    /// How long the loop may sleep: until the nearest peer is due, or a
    /// minute when none is waiting. Nothing is due on a device whose
    /// endpoint never bound.
    fn idle(&self) -> Duration {
        if self.net.is_none() {
            return IDLE;
        }
        let now = Instant::now();
        self.peers
            .values()
            .filter(|p| p.known && !p.live())
            .map(|p| p.next.saturating_duration_since(now))
            .min()
            .unwrap_or(IDLE)
            .min(IDLE)
    }

    /// One pass: the roster as the store has it now, then a dial to
    /// everyone who has no connection and is due one.
    async fn pass(&mut self) {
        self.reload().await;
        let now = Instant::now();
        let due: Vec<String> = self
            .peers
            .iter()
            .filter(|(_, p)| p.known && !p.live() && p.next <= now)
            .map(|(device, _)| device.clone())
            .collect();
        for device in due {
            // Due again no sooner than the first backoff, whatever comes of
            // this; a dial that ends says when to try next itself. Without
            // it a peer nothing can be done about would be "due" forever
            // and the loop would never sleep.
            if let Some(peer) = self.peers.get_mut(&device) {
                peer.next = now + BACKOFF_MIN;
            }
            self.dial(&device, None);
        }
    }

    /// The roster and the addresses, read on the writer. A device forgotten
    /// here or removed on another device loses its session with this read.
    async fn reload(&mut self) {
        if let Ok((me, name)) = self.db.identity().await {
            if !me.is_empty() {
                self.me = me;
            }
            self.name = name;
        }
        let me = self.me.clone();
        let rows = self
            .db
            .read_on_writer(move |conn| {
                let mut stmt = conn.prepare(
                    "SELECT p.device, p.name, coalesce(l.addr, '')
                     FROM sync_peer p LEFT JOIN sync_link l ON l.device = p.device
                     WHERE p.removed = 0 AND p.device <> ?1",
                )?;
                let rows = stmt.query_map([me], |r| {
                    Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?, r.get::<_, String>(2)?))
                })?;
                rows.collect::<rusqlite::Result<Vec<_>>>()
            })
            .await;
        let Ok(rows) = rows else { return };
        for (device, name, addr) in &rows {
            let peer = self
                .peers
                .entry(device.clone())
                .or_insert_with(|| Peer::new(name.clone()));
            peer.name.clone_from(name);
            peer.known = true;
            if peer.addr.is_none() {
                peer.addr = iroh::addr_from_text(addr);
            }
        }
        // Gone from the roster: forgotten here, or removed on another
        // device. A pairing session still in flight is not in it yet, and
        // is left alone.
        let stale: Vec<String> = self
            .peers
            .iter()
            .filter(|(device, p)| {
                let pairing = p.live() && !p.known;
                !pairing && !rows.iter().any(|(known, _, _)| known == *device)
            })
            .map(|(device, _)| device.clone())
            .collect();
        for device in stale {
            self.drop_peer(&device);
        }
    }

    /// Starts a session with a device. `pairing` is the secret off a
    /// pasted ticket, and says this is a *pair with* rather than a dial to
    /// somebody the roster already holds.
    fn dial(&mut self, device: &str, pairing: Option<[u8; 16]>) {
        let Some(net) = self.net.clone() else { return };
        let Some(peer) = self.peers.get(device) else {
            return;
        };
        if peer.live() {
            return;
        }
        // The bare id when nothing better is known: in internet mode the
        // lookups resolve it, which is how a device that moved is found.
        let Some(addr) = peer.addr.clone().or_else(|| bare(device)) else {
            return;
        };
        let roster = self.roster(device, pairing.is_some());
        let task = session(
            self.db.clone(),
            self.notes.clone(),
            device.to_string(),
            Wire::Dial(net, Box::new(addr), pairing),
            roster,
        );
        if let Some(peer) = self.peers.get_mut(device) {
            peer.dialed = true;
            peer.task = Some(task);
        }
    }

    /// A peer arrived. It is let in if the roster holds it, or if it shows
    /// the secret an open panel is displaying; anything else is closed
    /// before a `Have` crosses.
    async fn accepted(&mut self, remote: String, link: Link) {
        match self.peers.get(&remote) {
            Some(peer) if peer.live() => {
                // Two devices dialing each other at once: both keep the
                // connection whose dialer has the smaller id, so each side
                // drops the same one.
                if !peer.dialed || self.me < remote {
                    return;
                }
                self.end_session(&remote);
            }
            Some(_) => {}
            // Not in the roster: a stranger, which the verdict answers for.
            None => {
                self.peers.insert(remote.clone(), Peer::new(String::new()));
            }
        }
        let roster = self.roster(&remote, false);
        let task = session(
            self.db.clone(),
            self.notes.clone(),
            remote.clone(),
            Wire::Listen(link),
            roster,
        );
        if let Some(peer) = self.peers.get_mut(&remote) {
            peer.dialed = false;
            peer.task = Some(task);
        }
    }

    /// What the protocol asks about the greeting on the other end. The
    /// roster and the live pairing secret are this side's business, not the
    /// protocol's.
    fn roster(&self, device: &str, dialing_ticket: bool) -> Roster {
        Roster {
            device: device.to_string(),
            known: self.peers.get(device).is_some_and(|p| p.known),
            pairing: self.pairing.clone(),
            dialing_ticket,
        }
    }

    /// What a session task said.
    async fn heard(&mut self, note: Note) {
        match note {
            Note::Reachable => self.show_ticket(),
            Note::Up(device, addr) => {
                let now = self.db.clock().read();
                let text = iroh::addr_text(&addr);
                if let Some(peer) = self.peers.get_mut(&device) {
                    peer.connected = true;
                    peer.since = Instant::now();
                    peer.last_seen = now;
                    peer.last_error.clear();
                    peer.addr = Some(*addr);
                }
                self.remember(&device, text).await;
            }
            Note::Down { device, why, refused, refusal } => {
                self.refused += refused;
                if self.refusal.is_empty() {
                    if let Some(said) = refusal {
                        self.refusal = said;
                    }
                }
                self.went_down(&device, why);
                // A pairing dial that failed leaves nothing behind but the
                // reason it failed for.
                let orphan = self.peers.get(&device).is_some_and(|p| !p.known && !p.live());
                if orphan {
                    if let Some(peer) = self.peers.remove(&device) {
                        if peer.dialed {
                            self.note = peer.last_error;
                        }
                    }
                } else {
                    self.pass().await;
                }
            }
        }
    }

    /// A session ended: what it was worth, and when to try again.
    fn went_down(&mut self, device: &str, why: Option<String>) {
        let now = self.db.clock().read();
        let Some(peer) = self.peers.get_mut(device) else {
            return;
        };
        let settled = peer.connected && peer.since.elapsed() >= SETTLED;
        if peer.connected {
            peer.last_seen = now;
        }
        peer.connected = false;
        peer.last_error = why.unwrap_or_default();
        // A connection that stood is a success however it ended; one that
        // did not is a failure however quietly it closed, which is what
        // keeps a peer that refuses us from being dialed in a loop.
        peer.tries = if settled { 0 } else { peer.tries.saturating_add(1) };
        peer.next = Instant::now() + backoff(peer.tries);
    }

    /// One command from the panel.
    async fn command(&mut self, cmd: Cmd) {
        match cmd {
            Cmd::Pair(addr, secret) => {
                let device = addr.id.to_string();
                self.note.clear();
                self.remember(&device, iroh::addr_text(&addr)).await;
                let live = {
                    let peer = self
                        .peers
                        .entry(device.clone())
                        .or_insert_with(|| Peer::new(String::new()));
                    peer.addr = Some(*addr);
                    peer.next = Instant::now();
                    peer.tries = 0;
                    peer.live()
                };
                // Already talking to it: pairing again would say nothing
                // the open session has not said.
                if !live {
                    self.dial(&device, Some(secret));
                }
            }
            Cmd::Forget(device) => {
                self.end_session(&device);
                self.peers.remove(&device);
                let wanted = device.clone();
                let _ = self
                    .db
                    .write_async(move |tx| {
                        tx.execute("UPDATE sync_peer SET removed = 1 WHERE device = ?1", [&wanted])?;
                        tx.execute("DELETE FROM sync_link WHERE device = ?1", [&wanted])?;
                        Ok(())
                    })
                    .await;
            }
            Cmd::Rename(name) => {
                let (me, wanted) = (self.me.clone(), name.clone());
                let _ = self
                    .db
                    .write_async(move |tx| {
                        tx.execute(
                            "UPDATE sync_peer SET name = ?2 WHERE device = ?1",
                            rusqlite::params![me, wanted],
                        )?;
                        Ok(())
                    })
                    .await;
                self.name = name;
            }
            Cmd::BeginPairing => {
                *self.pairing.lock().expect("the pairing secret") = Some(secret());
                self.ticket.clear();
                self.note.clear();
                // In internet mode the endpoint has no address worth
                // showing until it holds a relay; the panel says
                // *connecting…* until this answers.
                if let Some(net) = self.net.clone() {
                    let notes = self.notes.clone();
                    crate::runtime::spawn(async move {
                        net.online().await;
                        let _ = notes.send(Note::Reachable);
                    });
                }
            }
            Cmd::EndPairing => {
                *self.pairing.lock().expect("the pairing secret") = None;
                self.ticket.clear();
            }
            Cmd::Kick => {
                for peer in self.peers.values_mut() {
                    peer.tries = 0;
                    peer.next = Instant::now();
                }
                self.pass().await;
            }
            Cmd::Stop(_) => {}
        }
    }

    /// The endpoint is reachable and a window is open: this is the ticket,
    /// and a scripted run drops it where the other process will read it.
    fn show_ticket(&mut self) {
        let showing = *self.pairing.lock().expect("the pairing secret");
        let (Some(secret), Some(net)) = (showing, self.net.as_ref()) else {
            return;
        };
        self.ticket = net.ticket(secret);
        self.note.clear();
        if let Some(path) = &self.ticket_out {
            if let Err(e) = std::fs::write(path, &self.ticket) {
                eprintln!("sync: cannot write the ticket to {}: {e}", path.display());
            }
        }
    }

    /// Writes down where a peer was reached, so the next dial goes straight
    /// there. Device-local: nothing here travels.
    async fn remember(&self, device: &str, addr: String) {
        let device = device.to_string();
        let _ = self
            .db
            .write_async(move |tx| {
                tx.execute(
                    "INSERT INTO sync_link(device, addr) VALUES(?1, ?2)
                     ON CONFLICT(device) DO UPDATE SET addr = excluded.addr",
                    rusqlite::params![device, addr],
                )?;
                Ok(())
            })
            .await;
    }

    fn end_session(&mut self, device: &str) {
        if let Some(peer) = self.peers.get_mut(device) {
            if let Some(task) = peer.task.take() {
                task.abort();
            }
            peer.connected = false;
        }
    }

    fn drop_peer(&mut self, device: &str) {
        self.end_session(device);
        self.peers.remove(device);
    }

    /// Every session closed and the endpoint with them, so the peers hear
    /// why rather than timing out.
    async fn stop(&mut self) {
        let devices: Vec<String> = self.peers.keys().cloned().collect();
        for device in devices {
            self.end_session(&device);
        }
        if let Some(net) = self.net.take() {
            net.close().await;
        }
    }
}

/// An address that is nothing but the id, for a device nobody has reached
/// yet. `None` if the roster holds something that is not an endpoint id.
fn bare(device: &str) -> Option<EndpointAddr> {
    EndpointId::from_str(device).ok().map(EndpointAddr::new)
}

/// Five seconds, then ten, then twenty, up to five minutes.
fn backoff(tries: u32) -> Duration {
    if tries == 0 {
        return Duration::ZERO;
    }
    BACKOFF_MIN
        .saturating_mul(1u32 << tries.min(7).saturating_sub(1))
        .min(BACKOFF_MAX)
}

/// Sixteen fresh random bytes: the pairing secret a ticket carries.
fn secret() -> [u8; 16] {
    use ring::rand::SecureRandom;
    let mut out = [0u8; 16];
    ring::rand::SystemRandom::new()
        .fill(&mut out)
        .expect("the system random source");
    out
}

/// Which end of a session this is.
enum Wire {
    Dial(Arc<Net>, Box<EndpointAddr>, Option<[u8; 16]>),
    Listen(Link),
}

/// This side's half of the handshake: who the other end must be, whether
/// the roster already holds it, and the secret that would let a stranger
/// in.
struct Roster {
    device: String,
    known: bool,
    /// The secret this device is showing, read when the hello arrives: a
    /// window that closed since this session started shows nothing, and a
    /// window opened again shows another secret.
    pairing: Showing,
    /// We dialed a ticket: the answerer is the device the ticket named, and
    /// iroh proved it holds that key.
    dialing_ticket: bool,
}

impl Roster {
    fn verdict(&self, greeting: &Greeting) -> Verdict {
        if greeting.device != self.device {
            return Verdict::Refused;
        }
        if self.known {
            return Verdict::Known;
        }
        if self.dialing_ticket {
            return Verdict::Pair;
        }
        let showing = *self.pairing.lock().expect("the pairing secret");
        match (showing, greeting.pairing) {
            (Some(ours), Some(theirs)) if ours == theirs => Verdict::Pair,
            _ => Verdict::Refused,
        }
    }
}

/// One session, from the dial to the close. Everything it learns comes back
/// as a [`Note`]; none of the loop's state is borrowed here.
fn session(
    db: Arc<Db>,
    notes: mpsc::UnboundedSender<Note>,
    device: String,
    wire: Wire,
    roster: Roster,
) -> JoinHandle<()> {
    let wake = db.ops_landed();
    crate::runtime::spawn(async move {
        let (link, side) = match wire {
            Wire::Listen(link) => (link, Side::Listen),
            Wire::Dial(net, addr, pairing) => match net.dial(*addr).await {
                Ok(link) => (link, Side::Dial(pairing)),
                Err(e) => {
                    let _ = notes.send(Note::Down {
                        device,
                        why: Some(e.to_string()),
                        refused: 0,
                        refusal: None,
                    });
                    return;
                }
            },
        };
        let _ = notes.send(Note::Up(device.clone(), Box::new(link.remote_addr())));
        let out = protocol::run(db, link, side, move |g| roster.verdict(g), wake).await;
        let _ = notes.send(match out {
            Ok(done) => Note::Down {
                device,
                why: None,
                refused: done.refused,
                refusal: done.refusal,
            },
            Err(protocol::Error::Closed) => Note::Down {
                device,
                why: None,
                refused: 0,
                refusal: None,
            },
            Err(e) => Note::Down {
                device,
                why: Some(e.to_string()),
                refused: 0,
                refusal: None,
            },
        });
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::{Schema, Step};
    use crate::caps::ClockSource;
    use crate::sync::{Device, Replicated};

    /// A note with a key of its own, so two devices merge it.
    static SCHEMA: Schema = Schema {
        app: "sync-service-test",
        steps: &[Step::Sql(
            "CREATE TABLE t_note(
               id    INTEGER PRIMARY KEY AUTOINCREMENT,
               uid   TEXT NOT NULL UNIQUE DEFAULT (lower(hex(randomblob(16)))),
               title TEXT NOT NULL DEFAULT ''
             );",
        )],
    };

    static DECLARED: &[Replicated] = &[Replicated {
        table: "t_note",
        key: &["uid"],
        columns: &["title"],
    }];

    /// One store, which the parts of the loop that are asked on their own
    /// want without an endpoint under them.
    fn store(secret: &[u8; 32], name: &str) -> Arc<Db> {
        Db::open(
            None,
            &[&SCHEMA],
            Device::new(super::super::device_of(secret))
                .named(name)
                .clocked(ClockSource::virtual_from(1000.0))
                .replicating(DECLARED.to_vec()),
        )
        .expect("a store")
    }

    /// One device: a store in memory, and a service on a loopback endpoint.
    fn device(name: &str) -> (Arc<Db>, Service) {
        let secret = super::super::new_secret();
        let db = store(&secret, name);
        let service = Service::spawn(
            db.clone(),
            Mount { secret, mode: Mode::Loopback, ticket_out: None },
            || {},
        );
        (db, service)
    }

    /// Waits for a store to say this, and says what it said instead if it
    /// never does.
    async fn eventually(db: &Arc<Db>, sql: &'static str, want: &str) {
        let mut said = String::new();
        for _ in 0..200 {
            said = db
                .read_on_writer(move |c| c.query_row(sql, [], |r| r.get::<_, String>(0)))
                .await
                .unwrap_or_default();
            if said == want {
                return;
            }
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
        panic!("waited for {want:?} and got {said:?}");
    }

    /// The ticket, once the endpoint has one to show.
    async fn ticket(service: &Service) -> String {
        for _ in 0..200 {
            let ticket = service.status().ticket;
            if !ticket.is_empty() {
                return ticket;
            }
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
        panic!("the endpoint never showed a ticket");
    }

    /// Whether a peer looks like this from the panel, in time.
    async fn seen(service: &Service, ok: impl Fn(&SyncStatus) -> bool) -> bool {
        for _ in 0..200 {
            if ok(&service.status()) {
                return true;
            }
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
        false
    }

    /// The whole of pairing, over two real endpoints: one device shows a
    /// ticket, the other pastes it, and from then on what either writes
    /// reaches the other over the connection that stays open. Forgetting
    /// ends it.
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn two_devices_pair_on_a_ticket_and_stay_in_touch() {
        let (a_db, a) = device("laptop");
        let (b_db, b) = device("phone");

        let window = a.begin_pairing();
        let ticket = ticket(&a).await;
        assert!(ticket.starts_with("superapp-"), "{ticket}");
        assert!(
            b.pair("superapp-not-a-ticket").is_err(),
            "a string that is not a ticket is refused where it is pasted"
        );
        assert!(a.pair(&ticket).is_err(), "and so is this device's own");
        b.pair(&ticket).expect("the ticket pairs");

        // Each device's roster holds the other, through an ordinary write.
        let b_id = b.status().device;
        eventually(&a_db, "SELECT count(*) || '' FROM sync_peer WHERE removed = 0", "2").await;
        eventually(&b_db, "SELECT count(*) || '' FROM sync_peer WHERE removed = 0", "2").await;

        // And what either writes now reaches the other on the connection
        // that stayed open.
        a_db.write_async(|tx| {
            tx.execute("INSERT INTO t_note(uid, title) VALUES('n1', 'from the laptop')", [])
        })
        .await
        .expect("the write");
        eventually(&b_db, "SELECT coalesce(max(title), '') FROM t_note", "from the laptop").await;
        b_db.write_async(|tx| {
            tx.execute("INSERT INTO t_note(uid, title) VALUES('n2', 'from the phone')", [])
        })
        .await
        .expect("the write");
        eventually(
            &a_db,
            "SELECT coalesce(max(title), '') FROM t_note WHERE uid = 'n2'",
            "from the phone",
        )
        .await;

        // The snapshot the panel draws: each says the other is connected,
        // by the name it introduced itself with.
        assert!(
            seen(&a, |s| s.peers.iter().any(|p| p.name == "phone" && p.connected)).await,
            "the phone is connected on the laptop's panel"
        );
        assert!(
            seen(&b, |s| s.peers.iter().any(|p| p.name == "laptop" && p.connected)).await,
            "and the laptop on the phone's"
        );
        assert!(a.problems().is_empty(), "nothing stands wrong while they are talking");

        // Forgetting is a write like any other: the row says removed, and
        // the session goes with it.
        a.forget(&b_id);
        eventually(&a_db, "SELECT count(*) || '' FROM sync_peer WHERE removed = 1", "1").await;
        assert!(seen(&a, |s| s.peers.is_empty()).await, "and the panel drops the row");

        drop(window);
        a.stop().await;
        b.stop().await;
    }

    /// A dialer that shows a stale secret is refused before any `Have`, and
    /// nothing is written on either side.
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn a_stale_ticket_leaves_nothing_behind() {
        let (a_db, a) = device("laptop");
        let (b_db, b) = device("stranger");

        // A ticket made while the window was open, and the window closed
        // before it was used: the secret is worthless.
        let window = a.begin_pairing();
        let ticket = ticket(&a).await;
        drop(window);
        assert!(seen(&a, |s| s.ticket.is_empty()).await, "the ticket goes with the window");
        b.pair(&ticket).expect("the ticket parses");

        // Long enough for a pairing that was going to work to have worked.
        tokio::time::sleep(Duration::from_millis(600)).await;
        for db in [&a_db, &b_db] {
            let peers: i64 = db
                .read_on_writer(|c| c.query_row("SELECT count(*) FROM sync_peer", [], |r| r.get(0)))
                .await
                .expect("the count");
            assert_eq!(peers, 1, "only this device's own row");
        }
        a.stop().await;
        b.stop().await;
    }

    /// The loop's own state, with no endpoint under it: what a command
    /// leaves behind is the whole of what these ask.
    fn loop_state(db: Arc<Db>) -> (State, mpsc::UnboundedReceiver<Note>) {
        let (notes, inbox) = mpsc::unbounded_channel();
        let state = State {
            db,
            net: None,
            me: String::new(),
            name: String::new(),
            peers: HashMap::new(),
            pairing: Showing::default(),
            ticket: String::new(),
            note: String::new(),
            refused: 0,
            refusal: String::new(),
            ticket_out: None,
            notes,
        };
        (state, inbox)
    }

    /// The secret the panel is showing, of which there is one while a
    /// window is open.
    fn showing(state: &State) -> [u8; 16] {
        state
            .pairing
            .lock()
            .expect("the pairing secret")
            .expect("a window is open")
    }

    /// What a hello says it is, and what it shows.
    fn greeting(device: &str, pairing: Option<[u8; 16]>) -> Greeting {
        Greeting { device: device.to_string(), name: "phone".into(), pairing }
    }

    /// A window that closes while a dialer's hello is still in the air takes
    /// its secret with it: the roster the session started with reads what
    /// the panel is showing when the hello arrives, and not what it was
    /// showing when the session began.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_window_that_closed_refuses_the_secret_it_showed() {
        let (mut state, _notes) = loop_state(store(&super::super::new_secret(), "laptop"));
        let dialer = "b".repeat(64);
        state.command(Cmd::BeginPairing).await;
        let shown = showing(&state);

        // The session starts here, while the panel is open.
        let roster = state.roster(&dialer, false);
        assert_eq!(roster.verdict(&greeting(&dialer, Some(shown))), Verdict::Pair);

        // And the panel closes before that hello arrives.
        state.command(Cmd::EndPairing).await;
        assert_eq!(
            roster.verdict(&greeting(&dialer, Some(shown))),
            Verdict::Refused,
            "the secret went with the window"
        );

        // A window opened again shows a fresh secret: what the old ticket
        // showed is worthless, and what this one shows pairs.
        state.command(Cmd::BeginPairing).await;
        let now = showing(&state);
        assert_ne!(now, shown, "every window is a secret of its own");
        assert_eq!(roster.verdict(&greeting(&dialer, Some(shown))), Verdict::Refused);
        assert_eq!(roster.verdict(&greeting(&dialer, Some(now))), Verdict::Pair);
    }

    /// The backoff walks up and stops at five minutes.
    #[test]
    fn a_failing_peer_is_tried_less_and_less() {
        assert_eq!(backoff(0), Duration::ZERO);
        assert_eq!(backoff(1), BACKOFF_MIN);
        assert_eq!(backoff(2), Duration::from_secs(10));
        assert_eq!(backoff(4), Duration::from_secs(40));
        assert_eq!(backoff(9), BACKOFF_MAX);
    }
}
