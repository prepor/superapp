//! Serialized sync operations: ownership evidence, validated history, and handoff.
//! Local authority is owned by the store; the protocol closes it and joins
//! services before publishing or releasing the remote fencing token.

use super::object::{
    self, batch_key, encode_state, snap_key, Cas, Object, PutNew, Snapshot, State, STATE_KEY,
    WIRE_V,
};
use super::{decode_batch, encode_batch, Frame, Role, Status, SyncError};
use crate::store::Store;
use serde::{Deserialize, Serialize};

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
    let end = 4usize
        .checked_add(hlen)
        .ok_or("batch object: length overflow")?;
    let hbytes = bytes.get(4..end).ok_or("batch object: truncated header")?;
    let header: BatchHeader =
        serde_json::from_slice(hbytes).map_err(|e| format!("batch header is malformed: {e}"))?;
    let frames = decode_batch(bytes.get(end..).ok_or("batch object: truncated body")?)?;
    Ok((header, frames))
}

/// SQLite changesets address columns by position, so the kernel version
/// alone cannot prove compatibility after app migrations. Fingerprint every
/// replicated table's ordered columns, types, and primary-key positions.
pub(super) fn schema_of(store: &Store) -> Result<i64, String> {
    let conn = store.conn();
    let version: i64 = conn
        .query_row("PRAGMA user_version", [], |r| r.get(0))
        .map_err(|e| e.to_string())?;
    let columns = conn
        .prepare(
            "SELECT t.name,p.name,p.type,p.pk,p.hidden
        FROM pragma_table_list t JOIN pragma_table_xinfo(t.name) p
        WHERE t.schema='main' AND t.type='table'
          AND t.name NOT LIKE 'sqlite_%' AND t.name NOT IN ('repl','repl_log','repl_event')
        ORDER BY t.name,p.cid",
        )
        .map_err(|e| e.to_string())?
        .query_map([], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, String>(2)?,
                r.get::<_, i64>(3)?,
                r.get::<_, i64>(4)?,
            ))
        })
        .map_err(|e| e.to_string())?
        .collect::<rusqlite::Result<Vec<_>>>()
        .map_err(|e| e.to_string())?;
    let bytes = serde_json::to_vec(&(version, columns)).map_err(|e| e.to_string())?;
    let hash = u64::from_str_radix(&object::hash(&bytes), 16).map_err(|e| e.to_string())?;
    Ok((hash & i64::MAX as u64) as i64)
}

/// One sync pass: read `state`, reconcile our role, and do the role's work —
/// a holder publishes what it has captured, a follower catches up.
/// Bootstraps the lineage if none exists. Answers the [`Status`] the shell
/// draws.
///
/// Always reports a status. Transport failures suspend execution; invalid
/// history and replay conflicts report their actual cause as `Fault`.
///
/// Not `#[must_use]`: a pass is worth running for what it does — a holder's
/// poll publishes, a follower's materializes — and the status is only how it
/// reports.
pub async fn poll(store: &Store, obj: &dyn Object) -> Status {
    match poll_inner(store, obj).await {
        Ok(role) => status(store, role, None).await,
        Err(why) => failed(store, why).await,
    }
}

/// Losing contact never renews authority from a remembered ownership flag.
///
/// A device that has never joined a lineage has no such flag to renew from,
/// and nothing to be fenced from: it stays what it was, local and writable.
/// What it writes before its first join is replaced by the install, as it
/// always was; what locking it would buy is a device whose one mistyped url
/// sits behind a screen with no button and no form to correct it on.
pub(super) async fn failed(store: &Store, error: SyncError) -> Status {
    let persisted: String = store
        .conn()
        .query_row("SELECT role FROM repl WHERE id=1", [], |r| r.get(0))
        .unwrap_or_default();
    let role = match persisted.as_str() {
        "incompatible" => Role::Incompatible,
        "stranded" => Role::Stranded {
            holder: String::new(),
        },
        "fault" => Role::Fault,
        _ => match error {
            SyncError::Transport(_) if store.epoch() == 0 => Role::Detached,
            SyncError::Transport(_) => Role::Offline,
            SyncError::Protocol(_) => Role::Fault,
            SyncError::Changed => Role::Syncing,
        },
    };
    if !role.writable() {
        store.set_writable(false);
        store.db().authority().quiesce().await;
    }
    status(store, role, Some(error.to_string())).await
}

/// Decode separately from transport so malformed history is never an outage.
async fn remote_state(obj: &dyn Object) -> Result<Option<object::StateAt>, SyncError> {
    let Some(blob) = obj.get(STATE_KEY).await.map_err(SyncError::Transport)? else {
        return Ok(None);
    };
    let state: State = serde_json::from_slice(&blob.bytes)
        .map_err(|e| SyncError::Protocol(format!("state is malformed: {e}")))?;
    if state.v != WIRE_V {
        return Err(format!("state is wire version {}, we speak {WIRE_V}", state.v).into());
    }
    if state.epoch <= 0
        || state.seq < 0
        || state.snapshot.seq < 0
        || state.snapshot.seq > state.seq
        || state.snapshot.schema != state.schema
        || (!state.released && state.holder.is_none())
    {
        return Err("state has an invalid epoch, owner, or snapshot range".into());
    }
    Ok(Some((state, blob.etag)))
}

async fn set_resume(store: &Store, resume: bool) -> Result<(), SyncError> {
    store
        .db()
        .raw_async(move |conn| {
            conn.execute("UPDATE repl SET resume=?1 WHERE id=1", [resume])
                .map(|_| ())
        })
        .await?;
    Ok(())
}
fn resume_allowed(store: &Store) -> bool {
    store
        .conn()
        .query_row("SELECT resume FROM repl WHERE id=1", [], |r| r.get(0))
        .unwrap_or(false)
}

/// Genesis snapshot identity binds legacy histories without trusting equal counters.
/// A bucket switch must not silently reuse another history's materialized watermark.
async fn bind_lineage(store: &Store, state: &State) -> Result<(), SyncError> {
    let identity = state.snapshot.key.clone();
    let known: Option<String> =
        store
            .conn()
            .query_row("SELECT lineage FROM repl WHERE id=1", [], |r| r.get(0))?;
    if known.as_ref().is_some_and(|old| old != &identity) {
        return Err("this bucket contains a different history; reconnect the original bucket or explicitly recover after backing up".into());
    }
    if known.is_none() {
        store
            .db()
            .raw_async(move |conn| {
                conn.execute("UPDATE repl SET lineage=?1 WHERE id=1", [identity])
                    .map(|_| ())
            })
            .await?;
    }
    Ok(())
}

/// Moves the write gate to match the role, records both in `repl` — where
/// the problem source reads them — and answers what to report.
async fn status(store: &Store, mut role: Role, mut note: Option<String>) -> Status {
    // The gate follows the role: only a holder (or a detached, bucket-less
    // device) may write.
    if role.writable() && store.db().release_requested() {
        role = Role::Syncing;
    }
    if role.writable() {
        if let Err(error) = store.db().grant_async().await {
            role = Role::Fault;
            note = Some(error.to_string());
        }
        if role == Role::Holder && !store.is_writable() {
            role = Role::Syncing;
        }
    } else {
        store.db().authority().quiesce().await;
    }
    if let Err(e) = store.set_status_async(role.word(), note.as_deref()).await {
        eprintln!("repl: recording the status failed: {e}");
    }
    Status {
        epoch: store.epoch(),
        unpublished: store.unpublished(),
        device: store.device(),
        role,
        note,
    }
}

async fn poll_inner(store: &Store, obj: &dyn Object) -> Result<Role, SyncError> {
    poll_from(store, obj, true).await
}

/// One pass. `may_bootstrap` is spent on the first attempt: a bucket with no
/// `state` is a lineage waiting to be started, but a bucket that *cannot* be
/// written — a name with a typo in it, a key without permission — answers
/// "no object" and refuses the write every time, and an unbounded retry
/// there is a stack that grows until the process dies.
async fn poll_from(
    store: &Store,
    obj: &dyn Object,
    may_bootstrap: bool,
) -> Result<Role, SyncError> {
    let device = store.device();
    let Some((state, etag)) = remote_state(obj).await? else {
        if store.epoch() != 0 {
            return Err("the joined history is missing; refusing to create a replacement".into());
        }
        if !may_bootstrap {
            // Someone else's bootstrap should have been visible by now; that
            // it is not makes this a pass with nothing to say, not a loop.
            return Err("the lineage is neither there nor startable".into());
        }
        // No lineage: try to become canonical. If someone beat us to it,
        // fall through and read their state on the next pass.
        return match bootstrap(store, obj).await {
            Ok(true) => Ok(Role::Holder),
            Ok(false) => Box::pin(poll_from(store, obj, false)).await,
            Err(why) => Err(why),
        };
    };

    bind_lineage(store, &state).await?;

    // A schema the lineage does not share refuses the lease: a changeset
    // naming an unknown table is skipped, not refused, so this check is the
    // only thing standing between drift and quiet loss.
    if state.schema != schema_of(store)? {
        freeze(store).await?;
        store
            .set_lease_async(store.epoch(), false)
            .await
            .map_err(|e| e.to_string())?;
        return Ok(Role::Incompatible);
    }

    let we_hold = state.holder.as_deref() == Some(&device) && !state.released;
    if !we_hold {
        freeze(store).await?;
        store
            .set_lease_async(store.epoch(), false)
            .await
            .map_err(|e| e.to_string())?;
    }
    if store.epoch() != 0 {
        reconcile_published(store, obj, &state).await?;
    }
    if we_hold {
        if !resume_allowed(store) || store.db().release_requested() {
            return Ok(Box::pin(release_requested(store, obj)).await?.role);
        }
        store
            .set_lease_async(state.epoch, true)
            .await
            .map_err(|e| e.to_string())?;
        if state.handoff.as_deref().is_some_and(|next| next != device) {
            return Ok(Box::pin(release_requested(store, obj)).await?.role);
        }
        if !publish(store, obj, &state, &etag).await? {
            // A changed state may be a handoff request or a forced takeover.
            // Freeze immediately; only a fresh, successful pass can reopen.
            freeze(store).await?;
            store
                .set_lease_async(store.epoch(), false)
                .await
                .map_err(|e| e.to_string())?;
            return Err(SyncError::Changed);
        }
        return Ok(Role::Holder);
    }

    freeze(store).await?;
    // A follower (or the lease is free). First, a device that has never
    // joined this lineage installs its snapshot to gain a common ancestry.
    if store.epoch() == 0 {
        install(store, obj, &state).await?;
    } else if store.unpublished() > 0 {
        // Preserve the old baseline and its pending frames until explicit
        // recovery. Releasing the newer lease must not discard this branch.
        // Clear the stale ownership claim so a later outage cannot reopen it.
        store
            .set_lease_async(store.epoch(), false)
            .await
            .map_err(|e| e.to_string())?;
        return Ok(Role::Stranded {
            holder: state.holder.clone().unwrap_or_default(),
        });
    }

    store
        .set_lease_async(state.epoch, false)
        .await
        .map_err(|e| e.to_string())?;
    materialize(store, obj, &state).await?;

    if state.released
        && state.handoff.as_deref() == Some(&device)
        && resume_allowed(store)
        && !store.db().release_requested()
    {
        return Ok(Box::pin(acquire_requested(store, obj, false)).await?.role);
    }
    Ok(if state.handoff.as_deref() == Some(&device) {
        Role::Waiting {
            holder: state.holder.clone().unwrap_or_default(),
        }
    } else if state.released {
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
async fn bootstrap(store: &Store, obj: &dyn Object) -> Result<bool, SyncError> {
    freeze(store).await?;
    let device = store.device();
    let schema = schema_of(store)?;
    let snap = snapshot(store, obj, schema, 0).await?;
    let state = State {
        v: WIRE_V,
        schema,
        epoch: 1,
        holder: Some(device),
        released: false,
        handoff: None,
        seq: 0,
        batch: None,
        snapshot: snap,
    };
    match obj
        .put_new(STATE_KEY, &encode_state(&state))
        .await
        .map_err(SyncError::Transport)?
    {
        PutNew::Created(_) => {
            store
                .set_lease_async(1, true)
                .await
                .map_err(|e| e.to_string())?;
            bind_lineage(store, &state).await?;
            Ok(true)
        }
        PutNew::Exists => Ok(false),
    }
}

/// `VACUUM INTO` a temp file, upload it create-only under a content-addressed
/// key, and answer the [`Snapshot`] pointer. The temp file is removed after.
async fn snapshot(
    store: &Store,
    obj: &dyn Object,
    schema: i64,
    seq: i64,
) -> Result<Snapshot, SyncError> {
    let dir = std::env::temp_dir();
    let path = dir.join(format!(
        "superapp-snap-{}-{}.db",
        std::process::id(),
        store.device()
    ));
    let _ = tokio::fs::remove_file(&path).await;
    // A genesis snapshot at a drained boundary: it captures the current state
    // and buries the frames already inside it, so nothing double-applies on a
    // device that installs it.
    store
        .snapshot_genesis_async(&path)
        .await
        .map_err(|e| e.to_string())?;
    let bytes = tokio::fs::read(&path).await.map_err(|e| e.to_string())?;
    let _ = tokio::fs::remove_file(&path).await;
    let h = object::hash(&bytes);
    let key = snap_key(seq, schema, &h);
    // Create-only; an existing key with the same hash is our own upload.
    match obj
        .put_new(&key, &bytes)
        .await
        .map_err(SyncError::Transport)?
    {
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
/// verify its hash, and hand it to the store to replace its replicated
/// tables with.
async fn install(store: &Store, obj: &dyn Object, state: &State) -> Result<(), SyncError> {
    let blob = obj
        .get(&state.snapshot.key)
        .await
        .map_err(SyncError::Transport)?
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
    tokio::fs::write(&path, &blob.bytes)
        .await
        .map_err(|e| e.to_string())?;
    store
        .install_snapshot_bound_async(
            &path,
            state.snapshot.seq,
            state.epoch,
            Some(state.snapshot.key.clone()),
        )
        .await
        .map_err(|e| e.to_string())?;
    let _ = tokio::fs::remove_file(&path).await;
    Ok(())
}

/// Publish captured frames as one batch, then CAS `state` to point at it.
/// Answers whether the CAS won — a loss means we no longer hold the lease.
/// The batch is uploaded *before* the CAS, so a failed CAS leaves an orphan
/// object rather than a corrupt history.
async fn publish(
    store: &Store,
    obj: &dyn Object,
    state: &State,
    etag: &str,
) -> Result<bool, SyncError> {
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
    if let PutNew::Exists = obj
        .put_new(&key, &body)
        .await
        .map_err(SyncError::Transport)?
    {
        let existing = obj
            .get(&key)
            .await
            .map_err(SyncError::Transport)?
            .ok_or("batch vanished after Exists")?;
        if object::hash(&existing.bytes) != object::hash(&body) {
            return Err("a different batch already occupies our key".into());
        }
    }
    let mut next = state.clone();
    next.batch = Some(key);
    next.seq = last;
    match obj
        .cas(STATE_KEY, &encode_state(&next), etag)
        .await
        .map_err(SyncError::Transport)?
    {
        Cas::Ok(_) => {
            let last_local = pending.last().map(|(s, _)| *s).unwrap_or(0);
            store
                .acknowledge_publish_async(last_local, last)
                .await
                .map_err(|e| e.to_string())?;
            Ok(true)
        }
        // Someone advanced state first — we lost the lease or raced a peer.
        // The orphan batch stays; the next holder's keys are unique by
        // construction, so it squats on nothing.
        Cas::Mismatch => Ok(false),
    }
}

/// Read and validate the complete missing ancestry before applying anything.
/// A truncated chain must never become a successful, advanced watermark.
async fn batches_since(
    obj: &dyn Object,
    state: &State,
    have: i64,
) -> Result<Vec<(BatchHeader, Vec<Frame>)>, SyncError> {
    if state.seq < have {
        return Err("the shared history is behind this device's watermark".into());
    }
    let mut chain: Vec<(BatchHeader, Vec<Frame>)> = Vec::new();
    let mut key = state.batch.clone();
    let mut expected = state.seq;
    let mut epoch = state.epoch;
    while expected > have {
        let k = key.ok_or("batch chain is missing its ancestry")?;
        let blob = obj
            .get(&k)
            .await
            .map_err(SyncError::Transport)?
            .ok_or_else(|| format!("batch {k} is missing"))?;
        let (header, frames) = decode_batch_object(&blob.bytes)?;
        if header.v != WIRE_V {
            return Err(format!("batch header: unknown version {}", header.v).into());
        }
        if header.schema != state.schema {
            return Err("batch schema differs from the shared history".into());
        }
        if header.first_seq <= 0
            || header.last_seq != expected
            || header.first_seq > header.last_seq
            || header.epoch <= 0
            || header.epoch > epoch
            || k != batch_key(
                header.epoch,
                &header.device,
                header.first_seq,
                header.last_seq,
            )
            || i64::try_from(frames.len()).ok() != Some(header.last_seq - header.first_seq + 1)
            || frames
                .iter()
                .enumerate()
                .any(|(i, f)| f.local_seq != header.first_seq + i as i64)
        {
            return Err("batch chain has an invalid range, order, or owner".into());
        }
        expected = header.first_seq - 1;
        epoch = header.epoch;
        key = header.prev.clone();
        chain.push((header, frames));
        if chain.len() > 100_000 {
            return Err("batch chain is unreasonably long".into());
        }
    }
    chain.reverse();
    Ok(chain)
}

/// A remote CAS may have committed even when its response was lost or the
/// process died before saving its checkpoint. Match our exact pending prefix
/// against canonical batches before publishing it again or declaring it lost.
async fn reconcile_published(
    store: &Store,
    obj: &dyn Object,
    state: &State,
) -> Result<(), SyncError> {
    let have = store.materialized();
    if state.seq <= have || store.unpublished() == 0 {
        return Ok(());
    }
    let pending = store.pending_frames();
    let device = store.device();
    let mut count = 0;
    let mut global = have;
    for (header, frames) in batches_since(obj, state, have).await? {
        if header.device != device {
            break;
        }
        for frame in frames.into_iter().filter(|f| f.local_seq > have) {
            if pending.get(count).map(|(_, cs)| cs) != Some(&frame.changeset) {
                return Err("our published batch does not match the local pending changes".into());
            }
            count += 1;
            global = frame.local_seq;
        }
    }
    if count > 0 {
        store
            .acknowledge_publish_async(pending[count - 1].0, global)
            .await
            .map_err(|e| e.to_string())?;
    }
    Ok(())
}

/// Catch up to the head only after validating the complete missing chain.
async fn materialize(store: &Store, obj: &dyn Object, state: &State) -> Result<(), SyncError> {
    for (_header, frames) in batches_since(obj, state, store.materialized()).await? {
        let apply: Vec<(i64, Vec<u8>)> = frames
            .into_iter()
            .filter(|f| f.local_seq > store.materialized())
            .map(|f| (f.local_seq, f.changeset))
            .collect();
        if let Some(&(last, _)) = apply.last() {
            store
                .apply_batch_async(&apply, last)
                .await
                .map_err(|e| e.to_string())?;
        }
    }
    Ok(())
}

/// Close admission atomically with enqueueing, then drain every accepted
/// database transaction before reading the queue or releasing ownership.
async fn freeze(store: &Store) -> Result<(), SyncError> {
    store.db().authority().quiesce().await;
    store.db().check_authority_health()?;
    store.db().flush_async().await.map_err(SyncError::from)
}

/// Acquire a free lease, or ask its current holder to publish and release.
/// A lost CAS never escalates an ordinary acquisition into a forced takeover.
pub async fn acquire(store: &Store, obj: &dyn Object) -> Result<Status, SyncError> {
    store.db().request_acquire();
    acquire_requested(store, obj, false).await
}

/// Execute an admitted command without resetting a newer synchronous UI
/// intent that may arrive while this async operation is already in flight.
pub(super) async fn acquire_requested(
    store: &Store,
    obj: &dyn Object,
    force: bool,
) -> Result<Status, SyncError> {
    set_resume(store, true).await?;
    acquire_inner(store, obj, force).await
}

async fn acquire_inner(store: &Store, obj: &dyn Object, force: bool) -> Result<Status, SyncError> {
    freeze(store).await?;
    let device = store.device();
    for _ in 0..8 {
        let (state, etag) = remote_state(obj)
            .await?
            .ok_or("no lineage to acquire yet")?;
        bind_lineage(store, &state).await?;
        if state.schema != schema_of(store)? {
            return Err("the other device is on a different schema — update it first".into());
        }
        if store.epoch() == 0 {
            install(store, obj, &state).await?;
        }
        if state.holder.as_deref() != Some(&device) || state.released {
            store
                .set_lease_async(store.epoch(), false)
                .await
                .map_err(|e| e.to_string())?;
        }
        reconcile_published(store, obj, &state).await?;
        if store.epoch() < state.epoch && store.unpublished() > 0 {
            store
                .set_lease_async(store.epoch(), false)
                .await
                .map_err(|e| e.to_string())?;
            return Ok(status(
                store,
                Role::Stranded {
                    holder: state.holder.clone().unwrap_or_default(),
                },
                None,
            )
            .await);
        }
        let ours = state.holder.as_deref() == Some(&device) && !state.released;
        if ours {
            return Ok(Box::pin(poll(store, obj)).await);
        }
        if !force && !state.released {
            if state.handoff.as_deref() != Some(&device) {
                // Do not replace another device's outstanding request.
                if state.handoff.is_some() {
                    return Ok(status(
                        store,
                        Role::Follower {
                            holder: state.holder.clone().unwrap_or_default(),
                        },
                        Some("another device is already waiting for a handoff".into()),
                    )
                    .await);
                }
                let mut requested = state.clone();
                requested.handoff = Some(device.clone());
                if obj
                    .cas(STATE_KEY, &encode_state(&requested), &etag)
                    .await
                    .map_err(SyncError::Transport)?
                    == Cas::Mismatch
                {
                    continue;
                }
            }
            return Ok(status(
                store,
                Role::Waiting {
                    holder: state.holder.clone().unwrap_or_default(),
                },
                None,
            )
            .await);
        }
        if !force && state.handoff.as_deref().is_some_and(|next| next != device) {
            return Ok(status(
                store,
                Role::Follower {
                    holder: state.handoff.clone().unwrap_or_default(),
                },
                Some("the lease is reserved for the requested handoff".into()),
            )
            .await);
        }
        // Replay failures preserve the local database and pending queue. Only
        // explicit recover() may replace a joined device's baseline.
        materialize(store, obj, &state).await?;
        let mut next = state.clone();
        next.holder = Some(device.clone());
        next.released = false;
        next.handoff = None;
        next.epoch = state.epoch + 1;
        match obj
            .cas(STATE_KEY, &encode_state(&next), &etag)
            .await
            .map_err(SyncError::Transport)?
        {
            Cas::Ok(_) => {
                store
                    .set_lease_async(next.epoch, true)
                    .await
                    .map_err(|e| e.to_string())?;
                return Ok(status(store, Role::Holder, None).await);
            }
            Cas::Mismatch => continue,
        }
    }
    Err("could not take the lease — it kept changing under us".into())
}

/// Drain accepted writes before publishing, then release only our exact lease.
/// A replacement holder must never be released by this device's final CAS.
pub async fn release(store: &Store, obj: &dyn Object) -> Result<Status, SyncError> {
    store.db().request_release();
    release_requested(store, obj).await
}

pub(super) async fn release_requested(
    store: &Store,
    obj: &dyn Object,
) -> Result<Status, SyncError> {
    set_resume(store, false).await?;
    freeze(store).await?;
    let device = store.device();
    for _ in 0..8 {
        let (state, etag) = remote_state(obj).await?.ok_or("no lineage to release")?;
        bind_lineage(store, &state).await?;
        if state.schema != schema_of(store)? {
            return Err("the other device is on a different schema — update it first".into());
        }
        if state.holder.as_deref() != Some(&device) || state.released {
            // Stepping away cancels our outstanding acquisition, including a
            // released reservation. Resume alone must not take the lease.
            if state.handoff.as_deref() == Some(&device) {
                let mut next = state.clone();
                next.handoff = None;
                if obj
                    .cas(STATE_KEY, &encode_state(&next), &etag)
                    .await
                    .map_err(SyncError::Transport)?
                    == Cas::Mismatch
                {
                    continue;
                }
            }
            store
                .set_lease_async(store.epoch(), false)
                .await
                .map_err(|e| e.to_string())?;
            return Ok(Box::pin(poll(store, obj)).await);
        }
        reconcile_published(store, obj, &state).await?;
        if !publish(store, obj, &state, &etag).await? {
            continue;
        }
        let (latest, etag) = remote_state(obj)
            .await?
            .ok_or("state vanished mid-release")?;
        bind_lineage(store, &latest).await?;
        if latest.schema != state.schema {
            return Err("the shared schema changed while releasing the lease".into());
        }
        if latest.epoch != state.epoch || latest.holder != state.holder || latest.released {
            continue;
        }
        // Admission is still closed and the queue was drained before upload.
        // The handoff request survives release and reserves the next lease.
        let mut next = latest;
        next.released = true;
        match obj
            .cas(STATE_KEY, &encode_state(&next), &etag)
            .await
            .map_err(SyncError::Transport)?
        {
            Cas::Ok(_) => {
                store
                    .set_lease_async(next.epoch, false)
                    .await
                    .map_err(|e| e.to_string())?;
                return Ok(status(store, Role::Free, None).await);
            }
            Cas::Mismatch => continue,
        }
    }
    Err("could not release the lease — it kept changing under us".into())
}

/// Explicit break-glass action for a holder that cannot answer a handoff.
/// Its unshared work remains on that device for backed-up recovery.
pub async fn override_lease(store: &Store, obj: &dyn Object) -> Result<Status, SyncError> {
    store.db().request_acquire();
    acquire_requested(store, obj, true).await
}

/// Preserve the local branch before replacing it with canonical history.
/// Recovery does not take the other device's lease.
pub async fn recover(store: &Store, obj: &dyn Object) -> Result<Status, SyncError> {
    freeze(store).await?;
    let (state, _) = remote_state(obj).await?.ok_or("no lineage to recover")?;
    if state.schema != schema_of(store)? {
        return Err("the other device is on a different schema — update it first".into());
    }
    if state.holder.as_deref() == Some(&store.device()) && !state.released {
        return Err(
            "this device still owns the lease; publish or release it before recovery".into(),
        );
    }
    let root = store
        .dir()
        .map(std::path::Path::to_path_buf)
        .unwrap_or_else(std::env::temp_dir);
    let directory = root.join("sync-recovery");
    tokio::fs::create_dir_all(&directory)
        .await
        .map_err(|e| e.to_string())?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        tokio::fs::set_permissions(&directory, std::fs::Permissions::from_mode(0o700))
            .await
            .map_err(|e| e.to_string())?;
    }
    let stamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_err(|e| e.to_string())?
        .as_nanos();
    let path = directory.join(format!("{}-{}-{stamp}.db", store.device(), store.epoch()));
    store
        .vacuum_into_async(&path)
        .await
        .map_err(|e| e.to_string())?;
    // Finish the backup on disk before replacing its source tables.
    let backup = tokio::fs::OpenOptions::new()
        .write(true)
        .open(&path)
        .await
        .map_err(|e| e.to_string())?;
    backup.sync_all().await.map_err(|e| e.to_string())?;
    set_resume(store, false).await?;
    install(store, obj, &state).await?;
    materialize(store, obj, &state).await?;
    let mut result = Box::pin(poll(store, obj)).await;
    result.note = Some("local changes were saved to a recovery backup".into());
    Ok(result)
}
