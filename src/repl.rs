//! Device-sync data format, write permission, and sync work.
//!
//! Every local transaction becomes a recorded change in `repl_log`. Batches
//! keep transactions separate so an error can identify the one that failed.
//! Applying a change from another device does not record it again.

use std::sync::mpsc;
use std::sync::{Arc, Mutex};

use serde::{Deserialize, Serialize};

use crate::object::{
    self, batch_key, encode_state, read_state, snap_key, Cas, Object, PutNew, Snapshot, State,
    STATE_KEY, WIRE_V,
};
use crate::store::{Db, Store};

/// One captured transaction: its local sequence and the SQLite changeset.
#[derive(Debug, Clone, PartialEq)]
pub struct Frame {
    /// The writer's local order (`repl_log.seq`), monotone for the device.
    pub local_seq: i64,
    /// The session changeset — what the transaction did to the replicated
    /// tables.
    pub changeset: Vec<u8>,
}

/// Batch wire-format version. Every device-sync object carries
/// one, and an unknown value refuses rather than guesses.
const BATCH_V: u8 = 1;

/// Encodes frames as a length-prefixed batch:
/// `[v:u8][count:u32]  ( [local_seq:i64][len:u32][changeset: len bytes] )*`,
/// all little-endian. Framed rather than concatenated so the decoder — and a
/// failed apply — can name the individual transaction.
#[must_use]
pub fn encode_batch(frames: &[Frame]) -> Vec<u8> {
    let mut out = Vec::new();
    out.push(BATCH_V);
    out.extend_from_slice(&(frames.len() as u32).to_le_bytes());
    for f in frames {
        out.extend_from_slice(&f.local_seq.to_le_bytes());
        out.extend_from_slice(&(f.changeset.len() as u32).to_le_bytes());
        out.extend_from_slice(&f.changeset);
    }
    out
}

/// Decodes a batch produced by [`encode_batch`].
///
/// # Errors
///
/// If the version is unknown or the bytes are truncated — a corrupt batch is
/// refused, never half-read.
pub fn decode_batch(bytes: &[u8]) -> Result<Vec<Frame>, String> {
    let mut p = 0usize;
    let take = |p: &mut usize, n: usize| -> Result<&[u8], String> {
        let end = p.checked_add(n).ok_or("batch: length overflow")?;
        let slice = bytes.get(*p..end).ok_or("batch: truncated")?;
        *p = end;
        Ok(slice)
    };
    let v = *take(&mut p, 1)?.first().ok_or("batch: empty")?;
    if v != BATCH_V {
        return Err(format!("batch: unknown version {v}"));
    }
    let count = u32::from_le_bytes(take(&mut p, 4)?.try_into().unwrap()) as usize;
    let mut frames = Vec::with_capacity(count.min(1024));
    for _ in 0..count {
        let local_seq = i64::from_le_bytes(take(&mut p, 8)?.try_into().unwrap());
        let len = u32::from_le_bytes(take(&mut p, 4)?.try_into().unwrap()) as usize;
        let changeset = take(&mut p, len)?.to_vec();
        frames.push(Frame {
            local_seq,
            changeset,
        });
    }
    Ok(frames)
}

/// Reads a store's unpublished frames as a [`Frame`] list.
#[must_use]
pub fn pending(from: &Store) -> Vec<Frame> {
    from.pending_frames()
        .into_iter()
        .map(|(local_seq, changeset)| Frame {
            local_seq,
            changeset,
        })
        .collect()
}

/// Drains one store's unpublished frames into another — the local half of
/// replication, no network. Encodes the frames to the wire format and back
/// (so the test exercises the real encoder), applies each on `into`, and
/// marks them published on `from`. Answers how many frames moved.
///
/// Applying records nothing on `into`, so the frames never echo back into its
/// own log — the property phase 1 exists to establish.
///
/// # Errors
///
/// If the batch will not round-trip, or an apply conflicts (a broken
/// invariant under a single writer).
pub fn drain(from: &Store, into: &Store) -> Result<usize, String> {
    let frames = pending(from);
    if frames.is_empty() {
        return Ok(0);
    }
    let batch = encode_batch(&frames);
    let decoded = decode_batch(&batch)?;
    for f in &decoded {
        into.apply_frame(&f.changeset).map_err(|e| e.to_string())?;
    }
    let last = frames.last().map(|f| f.local_seq).unwrap_or(0);
    from.mark_published(last).map_err(|e| e.to_string())?;
    Ok(frames.len())
}


// -- lease and sync passes ----------------------------------------------------

/// Where this device stands relative to the lease, as the UI shows it.
#[derive(Debug, Clone, PartialEq)]
pub enum Role {
    /// No bucket configured, or no lineage yet and we could not start one.
    Detached,
    /// We hold the lease: the store is writable.
    Holder,
    /// The lease is free (the last holder released it). Read-only until we
    /// acquire — anyone may.
    Free,
    /// Another device holds the lease: read-only, the locked screen.
    Follower { holder: String },
    /// The bucket says the lineage moved to an epoch past ours — someone
    /// overrode us while we were away. Read-only; recovery is manual.
    Stranded { holder: String },
    /// The bucket could not be reached this pass. A prior holder keeps
    /// writing; a follower stays locked.
    Offline,
}

impl Role {
    /// Whether this role may write locally.
    #[must_use]
    pub fn writable(&self) -> bool {
        matches!(self, Role::Holder | Role::Detached)
    }

    /// A one-line status for the account line / the locked screen.
    #[must_use]
    pub fn line(&self) -> String {
        match self {
            Role::Detached => "local only".into(),
            Role::Holder => "you hold the lease".into(),
            Role::Free => "the lease is free — acquire to write".into(),
            Role::Follower { holder } => format!("held by {} — read-only", short(holder)),
            Role::Stranded { holder } => {
                format!("diverged: {} took over — recover to continue", short(holder))
            }
            Role::Offline => "offline — the bucket is unreachable".into(),
        }
    }
}

/// A device id, shortened for a status line.
fn short(device: &str) -> String {
    device.chars().take(8).collect()
}

/// What a sync pass reports back to the UI.
#[derive(Debug, Clone, PartialEq)]
pub struct Status {
    pub role: Role,
    pub epoch: i64,
    /// Frames captured locally but not yet published — the risk an offline
    /// holder is accruing.
    pub unpublished: i64,
    /// This install's device id.
    pub device: String,
    /// Why the last pass failed, if it did. A bucket that refuses us —
    /// `403 SignatureDoesNotMatch`, a bucket that does not exist — is not the
    /// same thing as a dead network, and against a real endpoint that
    /// difference is most of the debugging. `None` when the pass went
    /// through.
    pub note: Option<String>,
}

/// A batch object's header — enough to place it in the global order and walk
/// back to the one before it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
struct BatchHeader {
    v: u32,
    schema: i64,
    epoch: i64,
    device: String,
    first_seq: i64,
    last_seq: i64,
    /// The preceding batch's full key, or `None` for the first ever.
    prev: Option<String>,
}

/// A batch object is its JSON header, length-prefixed, then the framed body.
fn encode_batch_object(header: &BatchHeader, frames: &[Frame]) -> Vec<u8> {
    let hjson = serde_json::to_vec(header).expect("batch header encodes");
    let mut out = Vec::new();
    out.extend_from_slice(&(hjson.len() as u32).to_le_bytes());
    out.extend_from_slice(&hjson);
    out.extend_from_slice(&encode_batch(frames));
    out
}

fn decode_batch_object(bytes: &[u8]) -> Result<(BatchHeader, Vec<Frame>), String> {
    let hlen = bytes
        .get(0..4)
        .ok_or("batch object: truncated header length")?;
    let hlen = u32::from_le_bytes(hlen.try_into().unwrap()) as usize;
    let end = 4usize.checked_add(hlen).ok_or("batch object: length overflow")?;
    let hbytes = bytes.get(4..end).ok_or("batch object: truncated header")?;
    let header: BatchHeader =
        serde_json::from_slice(hbytes).map_err(|e| format!("batch header is malformed: {e}"))?;
    let frames = decode_batch(bytes.get(end..).ok_or("batch object: truncated body")?)?;
    Ok((header, frames))
}

/// The store's current `PRAGMA user_version` — the schema the lineage must
/// agree on.
fn schema_of(store: &Store) -> i64 {
    store
        .conn()
        .query_row("PRAGMA user_version", [], |r| r.get(0))
        .unwrap_or(0)
}

/// One sync pass: read `state`, reconcile our role, and do the role's work —
/// a holder publishes what it has captured, a follower catches up. Bootstraps
/// the lineage if none exists. Answers the [`Status`] the UI draws.
///
/// # Errors
///
/// Never — a pass that cannot reach the bucket answers `Offline` rather than
/// failing, because offline must keep working. Genuinely broken states
/// (schema drift) resolve to a read-only role with a spoken reason.
pub fn poll(store: &Store, obj: &dyn Object) -> Status {
    match poll_inner(store, obj) {
        Ok(role) => status(store, role),
        Err(why) => Status {
            note: Some(why),
            ..status(store, offline_role(store))
        },
    }
}

/// The role to fall back to when the bucket is unreachable. A holder keeps
/// holding and writing (offline is allowed; the risk is surfaced as the
/// unpublished count). A device that never joined a lineage stays writable
/// and local — it should not be locked out just because the bucket is down
/// before its first join. Only a device that *is* a follower locks.
fn offline_role(store: &Store) -> Role {
    if store.holding() {
        Role::Holder
    } else if store.epoch() == 0 {
        Role::Detached
    } else {
        Role::Offline
    }
}

fn status(store: &Store, role: Role) -> Status {
    // The gate follows the role: only a holder (or a detached, bucket-less
    // device) may write.
    store.set_writable(role.writable());
    Status {
        epoch: store.epoch(),
        unpublished: store.unpublished(),
        device: store.device(),
        role,
        note: None,
    }
}

fn poll_inner(store: &Store, obj: &dyn Object) -> Result<Role, String> {
    poll_from(store, obj, true)
}

/// One pass. `may_bootstrap` is spent on the first attempt: a bucket with no
/// `state` is a lineage waiting to be started, but a bucket that *cannot* be
/// written — a name with a typo in it, a key without permission — answers
/// "no object" and refuses the write every time, and an unbounded retry
/// there is a stack that grows until the process dies.
fn poll_from(store: &Store, obj: &dyn Object, may_bootstrap: bool) -> Result<Role, String> {
    let device = store.device();
    let Some((state, etag)) = read_state(obj)? else {
        if !may_bootstrap {
            // Someone else's bootstrap should have been visible by now; that
            // it is not makes this a pass with nothing to say, not a loop.
            return Err("the lineage is neither there nor startable".into());
        }
        // No lineage: try to become canonical. If someone beat us to it,
        // fall through and read their state on the next pass.
        return match bootstrap(store, obj) {
            Ok(true) => Ok(Role::Holder),
            Ok(false) => poll_from(store, obj, false),
            Err(why) => Err(why),
        };
    };

    // A schema the lineage does not share refuses the lease: a
    // changeset naming an unknown table is skipped, not refused, so this
    // check is the only thing standing between drift and quiet loss.
    if state.schema != schema_of(store) {
        return Ok(if state.holder.as_deref() == Some(&device) {
            Role::Holder // our own lineage, mid-migration — do not lock ourselves out
        } else {
            Role::Stranded {
                holder: state.holder.clone().unwrap_or_default(),
            }
        });
    }

    let we_hold = state.holder.as_deref() == Some(&device) && !state.released;
    if we_hold {
        if store.epoch() > state.epoch {
            // We recorded a newer epoch than the bucket shows — impossible
            // unless the object regressed; treat conservatively as holder.
        }
        store.set_lease(state.epoch, true).map_err(|e| e.to_string())?;
        // Publish what we have captured. A lost CAS means we no longer hold
        // the lease; the next pass re-reads and re-roles.
        let _ = publish(store, obj, &state, &etag)?;
        return Ok(Role::Holder);
    }

    // A follower (or the lease is free). First, a device that has never
    // joined this lineage installs its snapshot to gain a common ancestry.
    if store.epoch() == 0 {
        install(store, obj, &state)?;
    } else if store.holding()
        && state.epoch > store.epoch()
        && !state.released
        && store.unpublished() > 0
    {
        // We *thought* we held the lease, but the lineage moved past us under
        // a different holder — an override, not a handoff (a handoff clears
        // `holding` when we release) — AND we captured writes that never
        // reached the canonical history. Those are divergent: we are stranded,
        // and recovery is a manual reset. A holder overridden with *nothing*
        // unpublished has not diverged — it published all it wrote — so it
        // falls through to follow cleanly rather than demand a recover.
        return Ok(Role::Stranded {
            holder: state.holder.clone().unwrap_or_default(),
        });
    } else if state.epoch > store.epoch() && store.unpublished() > 0 {
        // The lineage moved past us with writes of ours still unpublished, but
        // we are not the stranded holder — the overrider has since released,
        // or we never held. What we captured is divergent all the same: reset
        // before catching up, so it can never surface under a later lease
        // (`formal/Lease.tla`, `NoStaleWrite`).
        install(store, obj, &state)?;
    }

    store.set_lease(state.epoch, false).map_err(|e| e.to_string())?;
    materialize(store, obj, &state)?;

    Ok(if state.released {
        Role::Free
    } else {
        Role::Follower {
            holder: state.holder.clone().unwrap_or_default(),
        }
    })
}

/// Become the canonical device: snapshot the store, upload it, and write the
/// first `state` create-only. Answers whether we won (someone may have
/// bootstrapped first). Only ever called when no `state` exists.
fn bootstrap(store: &Store, obj: &dyn Object) -> Result<bool, String> {
    let device = store.device();
    let schema = schema_of(store);
    let snap = snapshot(store, obj, schema, 0)?;
    let state = State {
        v: WIRE_V,
        schema,
        epoch: 1,
        holder: Some(device),
        released: false,
        seq: 0,
        batch: None,
        snapshot: snap,
    };
    match obj.put_new(STATE_KEY, &encode_state(&state))? {
        PutNew::Created(_) => {
            store.set_lease(1, true).map_err(|e| e.to_string())?;
            store.set_writable(true);
            Ok(true)
        }
        PutNew::Exists => Ok(false),
    }
}

/// `VACUUM INTO` a temp file, upload it create-only under a content-addressed
/// key, and answer the [`Snapshot`] pointer. The temp file is removed after.
fn snapshot(store: &Store, obj: &dyn Object, schema: i64, seq: i64) -> Result<Snapshot, String> {
    let dir = std::env::temp_dir();
    let path = dir.join(format!(
        "superapp-snap-{}-{}.db",
        std::process::id(),
        store.device()
    ));
    let _ = std::fs::remove_file(&path);
    // A genesis snapshot at a drained boundary: it captures the current state
    // and buries the frames already inside it, so nothing double-applies on a
    // device that installs it.
    store.snapshot_genesis(&path).map_err(|e| e.to_string())?;
    let bytes = std::fs::read(&path).map_err(|e| e.to_string())?;
    let _ = std::fs::remove_file(&path);
    let h = object::hash(&bytes);
    let key = snap_key(seq, schema, &h);
    // Create-only; an existing key with the same hash is our own upload.
    match obj.put_new(&key, &bytes)? {
        PutNew::Created(_) | PutNew::Exists => {}
    }
    Ok(Snapshot {
        key,
        seq,
        schema,
        hash: h,
    })
}

/// Install the lineage's snapshot into a device that has none — download it,
/// verify its hash, and hand it to the store to replace its replicated tables
/// with. Backs the current store up first, so nothing local is lost silently.
fn install(store: &Store, obj: &dyn Object, state: &State) -> Result<(), String> {
    let blob = obj
        .get(&state.snapshot.key)?
        .ok_or("the lineage's snapshot is missing")?;
    if object::hash(&blob.bytes) != state.snapshot.hash {
        return Err("the snapshot's hash does not match — refusing to install".into());
    }
    // Unique per device: parallel tests (and, in principle, parallel installs)
    // must not share one temp file.
    let dir = std::env::temp_dir();
    let path = dir.join(format!(
        "superapp-install-{}-{}.db",
        std::process::id(),
        store.device()
    ));
    std::fs::write(&path, &blob.bytes).map_err(|e| e.to_string())?;
    store
        .install_snapshot(&path, state.snapshot.seq, state.epoch)
        .map_err(|e| e.to_string())?;
    let _ = std::fs::remove_file(&path);
    Ok(())
}

/// Publish captured frames as one batch, then CAS `state` to point at it.
/// Answers whether the CAS won — a loss means we no longer hold the lease.
/// The batch is uploaded *before* the CAS, so a failed CAS leaves an orphan
/// object rather than a corrupt history.
fn publish(store: &Store, obj: &dyn Object, state: &State, etag: &str) -> Result<bool, String> {
    let pending = store.pending_frames();
    if pending.is_empty() {
        return Ok(true);
    }
    let device = store.device();
    let first = state.seq + 1;
    let last = state.seq + pending.len() as i64;
    // The frames carry their *global* sequence.
    let frames: Vec<Frame> = pending
        .iter()
        .enumerate()
        .map(|(i, (_local, cs))| Frame {
            local_seq: first + i as i64,
            changeset: cs.clone(),
        })
        .collect();
    let key = batch_key(state.epoch, &device, first, last);
    let header = BatchHeader {
        v: WIRE_V,
        schema: state.schema,
        epoch: state.epoch,
        device: device.clone(),
        first_seq: first,
        last_seq: last,
        prev: state.batch.clone(),
    };
    let body = encode_batch_object(&header, &frames);
    // Upload create-only. An `Exists` is our own earlier attempt (an orphan
    // from a CAS we never confirmed) — safe to proceed once its bytes match.
    if let PutNew::Exists = obj.put_new(&key, &body)? {
        let existing = obj.get(&key)?.ok_or("batch vanished after Exists")?;
        if object::hash(&existing.bytes) != object::hash(&body) {
            return Err("a different batch already occupies our key".into());
        }
    }
    let mut next = state.clone();
    next.batch = Some(key);
    next.seq = last;
    match obj.cas(STATE_KEY, &encode_state(&next), etag)? {
        Cas::Ok(_) => {
            let last_local = pending.last().map(|(s, _)| *s).unwrap_or(0);
            store.mark_published(last_local).map_err(|e| e.to_string())?;
            store.set_materialized(last).map_err(|e| e.to_string())?;
            Ok(true)
        }
        // Someone advanced state first — we lost the lease or raced a peer.
        // The orphan batch stays; the next holder's keys are unique by
        // construction, so it squats on nothing.
        Cas::Mismatch => Ok(false),
    }
}

/// Catch up to the head: walk batches backward from `state.batch` by `prev`
/// until we cover everything past our watermark, then apply forward.
fn materialize(store: &Store, obj: &dyn Object, state: &State) -> Result<(), String> {
    if state.batch.is_none() || store.materialized() >= state.seq {
        return Ok(());
    }
    let have = store.materialized();
    let mut chain: Vec<(BatchHeader, Vec<Frame>)> = Vec::new();
    let mut key = state.batch.clone();
    let mut guard = 0;
    while let Some(k) = key {
        let blob = obj.get(&k)?.ok_or_else(|| format!("batch {k} is missing"))?;
        let (header, frames) = decode_batch_object(&blob.bytes)?;
        let reaches_down = header.first_seq <= have + 1;
        key = if reaches_down { None } else { header.prev.clone() };
        chain.push((header, frames));
        guard += 1;
        if guard > 100_000 {
            return Err("batch chain is unreasonably long".into());
        }
    }
    // Oldest first.
    for (_header, frames) in chain.into_iter().rev() {
        let apply: Vec<(i64, Vec<u8>)> = frames
            .into_iter()
            .filter(|f| f.local_seq > store.materialized())
            .map(|f| (f.local_seq, f.changeset))
            .collect();
        if let Some(&(last, _)) = apply.last() {
            store.apply_batch(&apply, last).map_err(|e| e.to_string())?;
        }
    }
    Ok(())
}

/// Take the lease: catch up fully, then CAS `state` to us with `epoch + 1`.
/// From a free lease this is ordinary; from a live other holder it is an
/// **override** — the same bump, and the caller is expected to have warned
/// that the other device may hold work it never published.
///
/// # Errors
///
/// If the bucket is unreachable, the schema does not match, or the CAS keeps
/// losing to a faster device.
pub fn acquire(store: &Store, obj: &dyn Object) -> Result<Status, String> {
    let device = store.device();
    for _ in 0..8 {
        let (state, etag) = read_state(obj)?.ok_or("no lineage to acquire yet")?;
        if state.schema != schema_of(store) {
            return Err("the other device is on a different schema — update it first".into());
        }
        if store.epoch() == 0 {
            install(store, obj, &state)?;
        }
        // Catch up first. If this device has unpublished changes from an older
        // write lease, discard them and reinstall the shared snapshot. Another
        // device has written since then without seeing those changes, so they
        // must not enter the newer history. See `NoStaleWrite` in
        // `formal/Lease.tla`. Any other replay conflict resets the same way.
        // A dated backup is not implemented yet.
        let superseded = store.epoch() < state.epoch && store.unpublished() > 0;
        if superseded || materialize(store, obj, &state).is_err() {
            install(store, obj, &state)?;
            materialize(store, obj, &state)?;
        }
        let mut next = state.clone();
        next.holder = Some(device.clone());
        next.released = false;
        next.epoch = state.epoch + 1;
        match obj.cas(STATE_KEY, &encode_state(&next), &etag)? {
            Cas::Ok(_) => {
                store.set_lease(next.epoch, true).map_err(|e| e.to_string())?;
                store.set_writable(true);
                return Ok(status(store, Role::Holder));
            }
            Cas::Mismatch => continue, // state moved; re-read and try again
        }
    }
    Err("could not take the lease — it kept changing under us".into())
}

/// Hand the lease back: publish anything captured, then CAS `state` to
/// `released`. Called on sleep and on close, so the other device can take
/// over cleanly without an override.
///
/// # Errors
///
/// If the bucket is unreachable or the CAS keeps losing.
pub fn release(store: &Store, obj: &dyn Object) -> Result<Status, String> {
    let device = store.device();
    for _ in 0..8 {
        let (state, etag) = read_state(obj)?.ok_or("no lineage to release")?;
        if state.holder.as_deref() != Some(&device) || state.released {
            // Already not ours to release.
            store.set_writable(false);
            return Ok(status(store, Role::Free));
        }
        // Drain first, then re-read the (now advanced) state to release it.
        publish(store, obj, &state, &etag)?;
        let (state, etag) = read_state(obj)?.ok_or("state vanished mid-release")?;
        let mut next = state.clone();
        next.released = true;
        match obj.cas(STATE_KEY, &encode_state(&next), &etag)? {
            Cas::Ok(_) => {
                store.set_lease(next.epoch, false).map_err(|e| e.to_string())?;
                store.set_writable(false);
                return Ok(status(store, Role::Free));
            }
            Cas::Mismatch => continue,
        }
    }
    Err("could not release the lease — it kept changing under us".into())
}

/// The break-glass override: take a lease a crashed holder never released, at
/// the stated cost that the other device may hold work it never published.
/// Mechanically an [`acquire`] — the epoch bump fences the stranded holder out
/// of publishing — surfaced separately so the UI can word the risk.
///
/// # Errors
///
/// As [`acquire`].
pub fn override_lease(store: &Store, obj: &dyn Object) -> Result<Status, String> {
    acquire(store, obj)
}

// -- the worker ---------------------------------------------------------------

/// A command to the replication worker.
enum Cmd {
    /// Retire: finish what is in flight and end the thread.
    Stop,
    /// Poll now (an action just captured something, or the UI woke).
    Kick,
    /// Take the lease from a free or held state.
    Acquire,
    /// Hand the lease back.
    Release,
    /// Override a crashed holder that never released.
    Override,
}

/// The UI's handle to the replication worker: it reads the latest [`Status`]
/// and issues lease commands. The worker itself runs the passes on its own
/// thread over its own reader on the shared writer.
pub struct Worker {
    cmd: mpsc::Sender<Cmd>,
    status: Arc<Mutex<Status>>,
    db: Arc<Db>,
    bucket: Arc<dyn Object>,
    /// Kept so a retiring worker can be *waited for*, not merely dropped.
    thread: Option<std::thread::JoinHandle<()>>,
}

impl Worker {
    /// The latest status the worker reported.
    #[must_use]
    pub fn status(&self) -> Status {
        self.status.lock().expect("repl status").clone()
    }

    /// Poll now — call after an action so a holder publishes promptly.
    pub fn kick(&self) {
        let _ = self.cmd.send(Cmd::Kick);
    }

    /// Ask to take the lease (from free, or by override from a live holder).
    pub fn acquire(&self) {
        let _ = self.cmd.send(Cmd::Acquire);
    }

    /// Ask to override a crashed holder — the epoch bump fences it out.
    pub fn override_lease(&self) {
        let _ = self.cmd.send(Cmd::Override);
    }

    /// Ask to hand the lease back (a normal release).
    pub fn release(&self) {
        let _ = self.cmd.send(Cmd::Release);
    }

    /// Release **synchronously**, on the calling thread — for app close and
    /// sleep, where the worker may never get another turn. Best effort.
    pub fn release_blocking(&self) {
        if let Ok(store) = Store::with_db(self.db.clone()) {
            let _ = release(&store, &*self.bucket);
        }
    }

    /// Retire this worker and **wait for it**. Dropping the handle is not
    /// enough: the thread only notices a closed channel on its next timeout,
    /// and until then it is still a device — it can materialize a snapshot,
    /// publish frames, and move the write gate, all against the bucket its
    /// replacement was configured to leave. Two workers over one store is
    /// exactly the thing the lease forbids between machines.
    ///
    /// An idle worker stops at once (the command wakes it); one mid-request
    /// finishes that request first.
    pub fn stop(mut self) {
        let _ = self.cmd.send(Cmd::Stop);
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

/// Spawns the replication worker over the shared writer and the given bucket.
/// It polls on a timer and on kicks, and reports status through the handle;
/// `notify` wakes the UI after each pass.
///
/// # Panics
///
/// If the thread cannot be spawned.
#[must_use]
pub fn spawn(db: Arc<Db>, bucket: Arc<dyn Object>, notify: impl Fn() + Send + 'static) -> Worker {
    let (cmd, rx) = mpsc::channel::<Cmd>();
    let status = Arc::new(Mutex::new(Status {
        role: Role::Detached,
        epoch: 0,
        unpublished: 0,
        device: String::new(),
        note: None,
    }));
    let wstatus = status.clone();
    let wdb = db.clone();
    let wbucket = bucket.clone();
    let thread = std::thread::Builder::new()
        .name("repl".into())
        .spawn(move || {
            let Ok(store) = Store::with_db(wdb) else {
                return;
            };
            // A holder that cannot reach the bucket keeps its role, so a
            // wrong key would otherwise be invisible on the way past: say
            // each *new* reason once, on stderr.
            let mut said: Option<String> = None;
            let mut report = |s: Status, st: &Arc<Mutex<Status>>| {
                if s.note != said {
                    if let Some(why) = &s.note {
                        eprintln!("repl: {why}");
                    }
                    said = s.note.clone();
                }
                *st.lock().expect("repl status") = s;
                notify();
            };
            // First pass immediately, so the UI has a role at once.
            report(poll(&store, &*wbucket), &wstatus);
            // The transport sets the cadence: a local daemon is free to poll
            // hard, a metered bucket across the network is not.
            let every = wbucket.poll_every();
            loop {
                let next = match rx.recv_timeout(every) {
                    Ok(Cmd::Stop) => return,
                    Ok(Cmd::Kick) | Err(mpsc::RecvTimeoutError::Timeout) => poll(&store, &*wbucket),
                    Ok(Cmd::Acquire) => {
                        acquire(&store, &*wbucket).unwrap_or_else(|_| poll(&store, &*wbucket))
                    }
                    Ok(Cmd::Override) => {
                        override_lease(&store, &*wbucket).unwrap_or_else(|_| poll(&store, &*wbucket))
                    }
                    Ok(Cmd::Release) => {
                        release(&store, &*wbucket).unwrap_or_else(|_| poll(&store, &*wbucket))
                    }
                    Err(mpsc::RecvTimeoutError::Disconnected) => return,
                };
                report(next, &wstatus);
            }
        })
        .expect("spawn repl worker");
    Worker {
        cmd,
        status,
        db,
        bucket,
        thread: Some(thread),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The batch round-trips through its wire format, frames intact.
    #[test]
    fn batches_round_trip() {
        let frames = vec![
            Frame {
                local_seq: 1,
                changeset: vec![1, 2, 3],
            },
            Frame {
                local_seq: 2,
                changeset: vec![],
            },
            Frame {
                local_seq: 9,
                changeset: vec![7; 300],
            },
        ];
        let bytes = encode_batch(&frames);
        assert_eq!(decode_batch(&bytes).unwrap(), frames);
    }

    /// A truncated or mis-versioned batch is refused, not half-read.
    #[test]
    fn a_corrupt_batch_is_refused() {
        let bytes = encode_batch(&[Frame {
            local_seq: 1,
            changeset: vec![1, 2, 3, 4],
        }]);
        assert!(decode_batch(&bytes[..bytes.len() - 2]).is_err(), "truncated");
        let mut bad = bytes.clone();
        bad[0] = 9;
        assert!(decode_batch(&bad).is_err(), "unknown version");
    }

    /// The whole point of phase 1: a local write is captured, drains into a
    /// peer, the peer converges — and applying records **nothing** on the
    /// peer, so nothing echoes back.
    #[test]
    fn a_write_captures_drains_and_does_not_echo() {
        let a = Store::open(None).expect("store a");
        let b = Store::open(None).expect("store b");

        // Two separate writes on A → two frames.
        a.write(|tx| {
            tx.execute("INSERT INTO account(label, email) VALUES('x', 'x@x')", [])
                .map(|_| ())
        })
        .unwrap();
        a.write(|tx| {
            tx.execute("INSERT INTO meta(key, value) VALUES('k', 'v')", [])
                .map(|_| ())
        })
        .unwrap();
        assert_eq!(a.pending_frames().len(), 2, "two writes, two frames");
        assert_eq!(a.unpublished(), 2);

        // Drain A into B.
        assert_eq!(drain(&a, &b).unwrap(), 2);

        // Convergence: B now holds what A wrote.
        let accounts: i64 = b
            .conn()
            .query_row("SELECT COUNT(*) FROM account", [], |r| r.get(0))
            .unwrap();
        assert_eq!(accounts, 1);
        let v: String = b
            .conn()
            .query_row("SELECT value FROM meta WHERE key='k'", [], |r| r.get(0))
            .unwrap();
        assert_eq!(v, "v");

        // No echo: applying on B recorded nothing in B's own log.
        assert_eq!(b.pending_frames().len(), 0, "apply must not capture");
        assert_eq!(drain(&b, &a).unwrap(), 0, "B has nothing to send back");

        // A's frames are published now, so a second drain moves nothing.
        assert_eq!(a.unpublished(), 0);
        assert_eq!(drain(&a, &b).unwrap(), 0);
    }

    /// A no-op write produces an empty changeset, so no frame is logged: the
    /// log carries real deltas, not every call to `write`.
    #[test]
    fn a_no_op_write_logs_no_frame() {
        let a = Store::open(None).expect("store");
        a.write(|tx| {
            // Nothing matches, so no row changes.
            tx.execute("UPDATE meta SET value='x' WHERE key='absent'", [])
                .map(|_| ())
        })
        .unwrap();
        assert_eq!(a.pending_frames().len(), 0, "no rows changed, no frame");
    }

    /// Two installs mint different device ids — they must never collide, or
    /// they would publish under the same name.
    #[test]
    fn devices_are_distinct() {
        let a = Store::open(None).unwrap();
        let b = Store::open(None).unwrap();
        assert_ne!(a.device(), b.device());
        assert!(!a.device().is_empty());
    }

    /// A bucket that answers "no object" and then refuses to create one — a
    /// name with a typo in it, a key without permission — is not a lineage
    /// waiting to be started. The pass says so once instead of asking again
    /// forever: before this was bounded, the retry was a recursion and the
    /// process died of it.
    #[test]
    fn a_bucket_that_cannot_be_written_is_not_bootstrapped_forever() {
        struct RefusesWrites;
        impl Object for RefusesWrites {
            fn get(&self, _key: &str) -> Result<Option<object::Blob>, String> {
                Ok(None)
            }
            fn put_new(&self, _key: &str, _body: &[u8]) -> Result<PutNew, String> {
                Err("bucket PUT: 404 NoSuchBucket".into())
            }
            fn cas(&self, _key: &str, _body: &[u8], _etag: &str) -> Result<Cas, String> {
                Err("bucket CAS: 404 NoSuchBucket".into())
            }
        }
        let store = Store::open(None).unwrap();
        // Returning at all is the assertion; the rest is what it should say.
        let s = poll(&store, &RefusesWrites);
        assert_eq!(s.role, Role::Detached, "a device that never joined stays local");
        assert!(store.is_writable());
        assert_eq!(s.note.as_deref(), Some("bucket PUT: 404 NoSuchBucket"));
    }

    /// A follower whose credentials go missing must not come back as a
    /// writer. The store opens *writable*, so "no bucket" cannot mean "no
    /// lease": a device that has joined a lineage keeps its gate shut and
    /// says why.
    #[test]
    fn a_follower_that_loses_its_bucket_stays_locked() {
        use crate::object::MemBucket;
        let bucket = MemBucket::new();
        let a = Store::open(None).unwrap();
        let b = Store::open(None).unwrap();
        assert_eq!(poll(&a, &bucket).role, Role::Holder);
        assert!(matches!(poll(&b, &bucket).role, Role::Follower { .. }));
        assert!(!b.is_writable());

        let broken = crate::r2::Broken("no secret for AKIDEXAMPLE".into());
        let s = poll(&b, &broken);
        assert_eq!(s.role, Role::Offline);
        assert!(
            !b.is_writable(),
            "a follower with no reachable bucket is still a follower"
        );
        assert_eq!(s.note.as_deref(), Some("no secret for AKIDEXAMPLE"));

        // The holder is the other case: offline is allowed, and it keeps
        // writing — the risk shows as the unpublished count, not a lock.
        let sa = poll(&a, &broken);
        assert_eq!(sa.role, Role::Holder);
        assert!(a.is_writable());
    }

    /// The whole lease lifecycle across two devices sharing one bucket:
    /// bootstrap, install, publish/materialize both ways, a clean handoff
    /// through release+acquire, follower read-only, and an override that
    /// strands the old holder. The headless proof that the mechanism works.
    #[test]
    fn two_devices_sync_acquire_and_strand() {
        use crate::object::MemBucket;
        let bucket = MemBucket::new();
        let a = Store::open(None).unwrap();
        let b = Store::open(None).unwrap();

        let count = |s: &Store, label: &str| -> i64 {
            s.conn()
                .query_row(
                    "SELECT COUNT(*) FROM account WHERE label = ?1",
                    [label],
                    |r| r.get(0),
                )
                .unwrap()
        };

        // A writes, then polls: no lineage, so A bootstraps and holds.
        a.write(|tx| {
            tx.execute("INSERT INTO account(label,email) VALUES('alice','a@x')", [])
                .map(|_| ())
        })
        .unwrap();
        assert_eq!(poll(&a, &bucket).role, Role::Holder);
        assert!(a.is_writable());

        // B polls: A holds, so B installs the snapshot (gaining alice) and locks.
        let sb = poll(&b, &bucket);
        assert!(matches!(sb.role, Role::Follower { .. }), "{:?}", sb.role);
        assert!(!b.is_writable(), "a follower is read-only");
        assert_eq!(count(&b, "alice"), 1, "B got alice from the snapshot");

        // A follower's ordinary write is refused at the gate.
        assert!(
            b.write(|tx| tx
                .execute("INSERT INTO account(label,email) VALUES('x','x')", [])
                .map(|_| ()))
                .is_err(),
            "the gate refuses a follower's write"
        );

        // A writes more; a poll publishes it; B's poll materializes it.
        a.write(|tx| {
            tx.execute("INSERT INTO account(label,email) VALUES('bob','b@x')", [])
                .map(|_| ())
        })
        .unwrap();
        poll(&a, &bucket);
        assert_eq!(a.unpublished(), 0, "the holder published what it captured");
        poll(&b, &bucket);
        assert_eq!(count(&b, "bob"), 1, "B materialized bob");

        // A hands the lease back; B sees it free and acquires it.
        assert_eq!(release(&a, &bucket).unwrap().role, Role::Free);
        assert!(!a.is_writable(), "a released holder is read-only");
        assert_eq!(poll(&b, &bucket).role, Role::Free);
        assert_eq!(acquire(&b, &bucket).unwrap().role, Role::Holder);
        assert!(b.is_writable());

        // A now follows B — a handoff, not a strand.
        let sa = poll(&a, &bucket);
        assert!(matches!(sa.role, Role::Follower { .. }), "{:?}", sa.role);

        // B writes; A materializes it the other direction.
        b.write(|tx| {
            tx.execute("INSERT INTO account(label,email) VALUES('carol','c@x')", [])
                .map(|_| ())
        })
        .unwrap();
        poll(&b, &bucket);
        poll(&a, &bucket);
        assert_eq!(count(&a, "carol"), 1, "sync flows both ways");

        // B captures one more write but does NOT publish it — divergent work
        // that the canonical history never receives.
        b.write(|tx| {
            tx.execute("INSERT INTO account(label,email) VALUES('dave','d@x')", [])
                .map(|_| ())
        })
        .unwrap();
        assert!(b.unpublished() > 0, "B holds an unpublished write");

        // Override: B still holds, A takes it anyway (epoch bump). B never
        // released AND has divergent unpublished work, so it is stranded on
        // its next pass — recovery is a manual reset. (A holder overridden
        // with nothing unpublished would follow cleanly instead; see
        // `an_overridden_holder_with_nothing_unpublished_follows_cleanly`.)
        let e_before = read_epoch(&bucket);
        assert_eq!(override_lease(&a, &bucket).unwrap().role, Role::Holder);
        assert!(read_epoch(&bucket) > e_before, "an override bumps the epoch");
        let sb = poll(&b, &bucket);
        assert!(matches!(sb.role, Role::Stranded { .. }), "{:?}", sb.role);
        assert!(!b.is_writable(), "a stranded device is read-only");
    }

    fn read_epoch(bucket: &crate::object::MemBucket) -> i64 {
        crate::object::read_state(bucket).unwrap().unwrap().0.epoch
    }

    /// A stranded device — one that held unpublished writes when it was
    /// overridden — recovers by resetting to the canonical baseline and
    /// replaying, discarding its divergent local writes. Its ordinary
    /// catch-up would conflict; the "recover" path (acquire, self-healing on
    /// conflict) resolves it. This is the case the mac hit in the live demo.
    #[test]
    fn a_stranded_holder_recovers_by_reset() {
        use crate::object::MemBucket;
        let bucket = MemBucket::new();
        let a = Store::open(None).unwrap();
        let b = Store::open(None).unwrap();
        let meta = |s: &Store, k: &str| -> Option<String> {
            s.conn()
                .query_row("SELECT value FROM meta WHERE key = ?1", [k], |r| r.get(0))
                .ok()
        };

        // A bootstraps and holds; writes 'shared'='v1' and publishes it.
        assert_eq!(poll(&a, &bucket).role, Role::Holder);
        a.write(|tx| {
            tx.execute("INSERT INTO meta(key,value) VALUES('shared','v1')", [])
                .map(|_| ())
        })
        .unwrap();
        poll(&a, &bucket);

        // B joins and catches up.
        assert!(matches!(poll(&b, &bucket).role, Role::Follower { .. }));
        assert_eq!(meta(&b, "shared").as_deref(), Some("v1"));

        // A makes a divergent local write it has NOT published.
        a.write(|tx| {
            tx.execute("INSERT INTO meta(key,value) VALUES('k','local-only')", [])
                .map(|_| ())
        })
        .unwrap();
        assert!(a.unpublished() >= 1, "A holds an unpublished divergent write");

        // B overrides and writes a conflicting value to the same key.
        assert_eq!(override_lease(&b, &bucket).unwrap().role, Role::Holder);
        b.write(|tx| {
            tx.execute("INSERT INTO meta(key,value) VALUES('k','from-b')", [])
                .map(|_| ())
        })
        .unwrap();
        poll(&b, &bucket);

        // A is stranded: read-only, its history diverged.
        assert!(matches!(poll(&a, &bucket).role, Role::Stranded { .. }));
        assert!(!a.is_writable());

        // Recover. Ordinary catch-up would conflict (A has 'k'='local-only',
        // the canonical line has 'k'='from-b'); acquire resets to the baseline
        // and replays instead.
        assert_eq!(acquire(&a, &bucket).unwrap().role, Role::Holder);
        assert!(a.is_writable());
        assert_eq!(
            meta(&a, "k").as_deref(),
            Some("from-b"),
            "A adopted the canonical value, discarding its divergent local one"
        );
        assert_eq!(meta(&a, "shared").as_deref(), Some("v1"));
        assert_eq!(a.unpublished(), 0, "the stale pending frame was cleared on install");
    }

    /// The model's finding (`formal/Lease.tla`, `NoStaleWrite`): a superseded
    /// holder's unpublished write that does NOT row-conflict with the
    /// canonical line used to survive its re-acquire and be published under
    /// the new epoch — after writes it never saw. It is divergent all the
    /// same, and is discarded unconditionally.
    #[test]
    fn a_superseded_holders_nonconflicting_write_is_discarded_on_acquire() {
        use crate::object::MemBucket;
        let bucket = MemBucket::new();
        let a = Store::open(None).unwrap();
        let b = Store::open(None).unwrap();
        let meta = |s: &Store, k: &str| -> Option<String> {
            s.conn()
                .query_row("SELECT value FROM meta WHERE key = ?1", [k], |r| r.get(0))
                .ok()
        };

        assert_eq!(poll(&a, &bucket).role, Role::Holder);
        assert!(matches!(poll(&b, &bucket).role, Role::Follower { .. }));

        // A's unpublished write, under epoch 1.
        a.write(|tx| {
            tx.execute("INSERT INTO meta(key,value) VALUES('k','stale')", []).map(|_| ())
        })
        .unwrap();

        // B takes over (epoch 2) and writes a DIFFERENT key: no row conflict.
        assert_eq!(override_lease(&b, &bucket).unwrap().role, Role::Holder);
        b.write(|tx| {
            tx.execute("INSERT INTO meta(key,value) VALUES('other','from-b')", [])
                .map(|_| ())
        })
        .unwrap();
        poll(&b, &bucket);

        // A recovers (epoch 3). Its epoch-1 frame must not survive.
        assert!(matches!(poll(&a, &bucket).role, Role::Stranded { .. }));
        assert_eq!(acquire(&a, &bucket).unwrap().role, Role::Holder);
        assert_eq!(meta(&a, "other").as_deref(), Some("from-b"));
        assert_eq!(meta(&a, "k"), None, "the superseded write was discarded, not merged");
        assert_eq!(a.unpublished(), 0, "nothing stale is left to publish");

        // And so it never reaches B. (B published all it wrote before A's
        // override, so B follows cleanly rather than stranding.)
        poll(&a, &bucket);
        assert!(matches!(poll(&b, &bucket).role, Role::Follower { .. }));
        assert_eq!(meta(&b, "k"), None);
    }

    /// The same hole on the poll path: if the overrider has *released*, the
    /// superseded device follows rather than strands — and used to keep its
    /// stale frame pending for a later acquire. Following resets instead.
    #[test]
    fn a_superseded_holder_following_a_released_lease_drops_its_stale_write() {
        use crate::object::MemBucket;
        let bucket = MemBucket::new();
        let a = Store::open(None).unwrap();
        let b = Store::open(None).unwrap();
        let meta = |s: &Store, k: &str| -> Option<String> {
            s.conn()
                .query_row("SELECT value FROM meta WHERE key = ?1", [k], |r| r.get(0))
                .ok()
        };

        assert_eq!(poll(&a, &bucket).role, Role::Holder);
        assert!(matches!(poll(&b, &bucket).role, Role::Follower { .. }));
        a.write(|tx| {
            tx.execute("INSERT INTO meta(key,value) VALUES('k','stale')", []).map(|_| ())
        })
        .unwrap();

        assert_eq!(override_lease(&b, &bucket).unwrap().role, Role::Holder);
        b.write(|tx| {
            tx.execute("INSERT INTO meta(key,value) VALUES('other','from-b')", [])
                .map(|_| ())
        })
        .unwrap();
        assert_eq!(release(&b, &bucket).unwrap().role, Role::Free);

        // A never polled while B held: it sees a free, newer lineage.
        assert_eq!(poll(&a, &bucket).role, Role::Free);
        assert_eq!(meta(&a, "other").as_deref(), Some("from-b"));
        assert_eq!(meta(&a, "k"), None, "the stale write went with the reset");
        assert_eq!(a.unpublished(), 0);

        // Acquiring now publishes nothing stale.
        assert_eq!(acquire(&a, &bucket).unwrap().role, Role::Holder);
        poll(&a, &bucket);
        poll(&b, &bucket);
        assert_eq!(meta(&b, "k"), None);
    }

    /// A holder that published everything it wrote, then was overridden, has
    /// NOT diverged: it follows cleanly (the "take over" screen), not strands
    /// (the "recover" screen). Only genuine unpublished divergence strands.
    /// This is the two-live-apps hand-off the demo exercises.
    #[test]
    fn an_overridden_holder_with_nothing_unpublished_follows_cleanly() {
        use crate::object::MemBucket;
        let bucket = MemBucket::new();
        let a = Store::open(None).unwrap();
        let b = Store::open(None).unwrap();
        let meta = |s: &Store, k: &str| -> Option<String> {
            s.conn()
                .query_row("SELECT value FROM meta WHERE key = ?1", [k], |r| r.get(0))
                .ok()
        };

        // A holds and publishes a write — nothing left unpublished.
        assert_eq!(poll(&a, &bucket).role, Role::Holder);
        a.write(|tx| {
            tx.execute("INSERT INTO meta(key,value) VALUES('k','from-a')", []).map(|_| ())
        })
        .unwrap();
        poll(&a, &bucket);
        assert_eq!(a.unpublished(), 0, "A published all it wrote");

        // B joins and overrides A (an override, not a handoff: A never released).
        assert!(matches!(poll(&b, &bucket).role, Role::Follower { .. }));
        assert_eq!(override_lease(&b, &bucket).unwrap().role, Role::Holder);
        b.write(|tx| {
            tx.execute("INSERT INTO meta(key,value) VALUES('k2','from-b')", [])
                .map(|_| ())
        })
        .unwrap();
        poll(&b, &bucket);

        // A polls: overridden, but with nothing unpublished it is a plain
        // Follower — read-only, "take over" — not Stranded / "recover".
        let sa = poll(&a, &bucket);
        assert!(
            matches!(sa.role, Role::Follower { .. }),
            "clean override follows, got {:?}",
            sa.role
        );
        assert!(!a.is_writable());
        assert_eq!(meta(&a, "k2").as_deref(), Some("from-b"), "A caught up to B's write");
        assert_eq!(meta(&a, "k").as_deref(), Some("from-a"), "A's own published write survived");

        // And A can take the lease straight back — a plain acquire, no reset.
        assert_eq!(acquire(&a, &bucket).unwrap().role, Role::Holder);
        assert!(a.is_writable());
    }

    /// The same sync, but over the **real HTTP transport** — the daemon's
    /// handler and the `HttpBucket` client on a live socket — so the stack the
    /// mac and the emulator use (snapshot upload/install, batch upload/apply,
    /// the lease CAS, all over HTTP) is proven end to end, not just against
    /// the in-memory bucket.
    #[test]
    fn two_devices_sync_over_real_http() {
        use crate::object::{serve_conn, HttpBucket};
        use std::net::TcpListener;
        use std::sync::atomic::{AtomicBool, Ordering};

        let dir = std::env::temp_dir().join(format!("superapp-repl-http-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        listener.set_nonblocking(true).unwrap();
        let stop = std::sync::Arc::new(AtomicBool::new(false));
        let sdir = dir.clone();
        let sstop = stop.clone();
        let server = std::thread::spawn(move || {
            while !sstop.load(Ordering::Relaxed) {
                match listener.accept() {
                    Ok((mut s, _)) => {
                        s.set_nonblocking(false).ok();
                        let _ = serve_conn(&sdir, &mut s);
                    }
                    Err(ref e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                        std::thread::sleep(std::time::Duration::from_millis(2));
                    }
                    Err(_) => break,
                }
            }
        });

        let bucket = HttpBucket::new(&format!("http://{addr}"));
        let a = Store::open(None).unwrap();
        let b = Store::open(None).unwrap();
        let has = |s: &Store, label: &str| -> i64 {
            s.conn()
                .query_row(
                    "SELECT COUNT(*) FROM account WHERE label = ?1",
                    [label],
                    |r| r.get(0),
                )
                .unwrap()
        };

        // A bootstraps over HTTP and holds; the snapshot and state are uploaded.
        a.write(|tx| {
            tx.execute("INSERT INTO account(label,email) VALUES('alice','a@x')", [])
                .map(|_| ())
        })
        .unwrap();
        assert_eq!(poll(&a, &bucket).role, Role::Holder);

        // B installs A's snapshot over HTTP and locks.
        assert!(matches!(poll(&b, &bucket).role, Role::Follower { .. }));
        assert_eq!(has(&b, "alice"), 1, "B installed the snapshot over HTTP");

        // A writes more; a poll uploads the batch; B materializes it over HTTP.
        a.write(|tx| {
            tx.execute("INSERT INTO account(label,email) VALUES('bob','b@x')", [])
                .map(|_| ())
        })
        .unwrap();
        poll(&a, &bucket);
        poll(&b, &bucket);
        assert_eq!(has(&b, "bob"), 1, "B materialized the batch over HTTP");

        // The lease CAS works over HTTP: B takes over.
        assert_eq!(acquire(&b, &bucket).unwrap().role, Role::Holder);
        assert!(b.is_writable());

        stop.store(true, Ordering::Relaxed);
        server.join().unwrap();
        let _ = std::fs::remove_dir_all(&dir);
    }
}
