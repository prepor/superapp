//! Device sync: the data format, the write permission, and the passes.
//!
//! Every local transaction becomes a recorded change in `repl_log`. Batches
//! keep transactions separate so an error can identify the one that failed.
//! Applying a change from another device does not record it again.
//!
//! Device sync is not an app. It replicates the store itself, every app's
//! tables included, and the shell depends on it: the write gate in
//! [`Session::act`](crate::session::Session::act), the lock screen, and the
//! lease driver. The lease is one mutable object in a bucket; the model
//! behind it — and the property that found two of the bugs the tests below
//! pin — is `formal/Lease.tla` at the root of the repository.

pub mod object;
pub mod r2;
mod error;
mod protocol;
pub use protocol::{poll, acquire, release, override_lease, recover};
pub use error::SyncError;

mod driver;
pub use driver::{Driver, spawn};


use crate::caps::Secrets;
use crate::effect::{Ctx, Effect};
use crate::problems::{Problem, ProblemSource};
use crate::store::Store;

/// One captured transaction: its local sequence and the SQLite changeset.
#[derive(Debug, Clone, PartialEq)]
pub struct Frame {
    /// The writer's local order (`repl_log.seq`), monotone for the device.
    pub local_seq: i64,
    /// The session changeset — what the transaction did to the replicated
    /// tables.
    pub changeset: Vec<u8>,
}

/// Batch wire-format version. Every device-sync object carries one, and an
/// unknown value refuses rather than guesses.
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
    if p != bytes.len() { return Err("batch: trailing bytes".into()); }
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
/// Applying records nothing on `into`, so the frames never echo back into
/// its own log.
///
/// # Errors
///
/// If the batch will not round-trip, or an apply conflicts (a broken
/// invariant under a single writer).
pub async fn drain(from: &Store, into: &Store) -> Result<usize, String> {
    let frames = pending(from);
    if frames.is_empty() {
        return Ok(0);
    }
    let batch = encode_batch(&frames);
    let decoded = decode_batch(&batch)?;
    for f in &decoded {
        into.apply_frame_async(&f.changeset).await.map_err(|e| e.to_string())?;
    }
    let last = frames.last().map(|f| f.local_seq).unwrap_or(0);
    from.mark_published_async(last).await.map_err(|e| e.to_string())?;
    Ok(frames.len())
}

// -- lease and sync passes ----------------------------------------------------

/// Where this device stands relative to the lease, as the lock screen shows
/// it.
#[derive(Debug, Clone, PartialEq, Eq)]
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
    /// The current holder has been asked to drain and release. A second,
    /// explicitly labelled action is required to force an offline takeover.
    Waiting { holder: String },
    /// The bucket says the lineage moved to an epoch past ours — someone
    /// overrode us while we were away. Read-only; recovery is manual.
    Stranded { holder: String },
    /// Shared table layouts differ. Recovery cannot repair a version mismatch.
    Incompatible,
    /// A replay, storage, or history invariant failed; inspect the actual reason.
    Fault,
    /// Ownership changed during this pass; admission is closed while rechecking.
    Syncing,
    /// The bucket could not be reached this pass. Execution is suspended
    /// until a successful pass confirms current ownership.
    Offline,
}

impl Role {
    /// Whether this role may write locally.
    #[must_use]
    pub fn writable(&self) -> bool {
        matches!(self, Role::Holder | Role::Detached)
    }

    /// One stable word, as the `repl` row records it and the problem source
    /// reads it back.
    #[must_use]
    pub fn word(&self) -> &'static str {
        match self {
            Role::Detached => "detached",
            Role::Holder => "holder",
            Role::Free => "free",
            Role::Follower { .. } => "follower",
            Role::Waiting { .. } => "waiting",
            Role::Stranded { .. } => "stranded",
            Role::Incompatible => "incompatible",
            Role::Fault => "fault",
            Role::Syncing => "syncing",
            Role::Offline => "offline",
        }
    }

    /// A one-line status for the locked screen.
    #[must_use]
    pub fn line(&self) -> String {
        match self {
            Role::Detached => "local only".into(),
            Role::Holder => "you hold the lease".into(),
            Role::Free => "the lease is free — acquire to write".into(),
            Role::Follower { holder } => format!("held by {} — read-only", short(holder)),
            Role::Waiting { holder } => format!("waiting for {} to finish syncing", short(holder)),
            Role::Stranded { holder } => {
                format!("diverged: {} took over — recover to continue", short(holder))
            }
            Role::Incompatible => "update both devices before syncing".into(),
            Role::Fault => "sync stopped — inspect the sync error".into(),
            Role::Syncing => "syncing — checking ownership".into(),
            Role::Offline => "offline — the bucket is unreachable".into(),
        }
    }

    /// The locked screen's title and the word on its button, or no button at
    /// all where there is nothing to take.
    #[must_use]
    pub fn locked_screen(&self) -> (&'static str, Option<&'static str>) {
        match self {
            Role::Free => ("the lease is free", Some("acquire")),
            Role::Follower { .. } => ("another device is writing", Some("take over")),
            Role::Waiting { .. } => ("switching devices", Some("force takeover")),
            Role::Stranded { .. } => ("this device has diverged", Some("recover")),
            Role::Incompatible => ("devices need compatible versions", None),
            Role::Fault => ("sync needs attention", None),
            Role::Syncing => ("checking device ownership", None),
            Role::Offline => ("offline — the bucket is unreachable", None),
            Role::Detached | Role::Holder => ("read-only", Some("acquire")),
        }
    }
}

/// A device id, shortened for a status line.
fn short(device: &str) -> String {
    device.chars().take(8).collect()
}

/// What a sync pass reports back to the UI.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Status {
    pub role: Role,
    pub epoch: i64,
    /// Frames captured locally but not yet published — the risk an offline
    /// holder is accruing.
    pub unpublished: i64,
    /// This install's device id.
    pub device: String,
    /// Why the last pass failed, if it did. A bucket that refuses us —
    /// `403 SignatureDoesNotMatch`, a bucket that does not exist — is not
    /// the same thing as a dead network, and against a real endpoint that
    /// difference is most of the debugging. `None` when the pass went
    /// through.
    pub note: Option<String>,
}

impl Default for Status {
    /// What a device believes before its first pass: nothing yet. Admission
    /// is closed until the bucket has answered, and the answer is a change
    /// from this whatever it is — a `Detached` answer included, which is
    /// how a device whose bucket is down still opens its first root.
    fn default() -> Status {
        Status {
            role: Role::Syncing,
            epoch: 0,
            unpublished: 0,
            device: String::new(),
            note: None,
        }
    }
}

// -- the unreachable bucket ---------------------------------------------------

/// The one standing condition device sync can be in: the bucket could not be
/// reached this pass, and a holder is accruing frames nobody else has seen.
///
/// The kernel's own source, listed before any app's, and derived like every
/// other problem: the last pass wrote its role and its reason into `repl`,
/// and this reads them back. A follower behind the locked screen is not
/// listed — the screen says it already.
pub struct BucketProblem;

/// The one in this build.
pub static BUCKET_PROBLEM: BucketProblem = BucketProblem;

impl ProblemSource for BucketProblem {
    fn list(&self, store: &Store) -> Vec<Problem> {
        let row: Option<(String, Option<String>)> = store
            .conn()
            .query_row("SELECT role, note FROM repl WHERE id = 1", [], |r| {
                Ok((r.get(0)?, r.get(1)?))
            })
            .ok();
        let Some((role, note)) = row else {
            return Vec::new();
        };
        if role != Role::Offline.word() && role != Role::Fault.word() {
            return Vec::new();
        }
        let detail = match store.unpublished() {
            0 => "nothing waiting to publish".to_string(),
            1 => "1 frame waiting to publish".to_string(),
            n => format!("{n} frames waiting to publish"),
        };
        vec![Problem::new(
            "sync",
            "device sync",
            note.unwrap_or_else(|| "the bucket is unreachable".into()),
            detail,
        )]
    }
}

// -- the secret a bucket is opened with ---------------------------------------

/// Store the device-sync bucket's Cloudflare API token, under the id it
/// belongs to — its value, which [`r2::creds`] hashes into a secret access
/// key. The one road a device with no shell and no cable has to a credential.
pub struct BucketSecret<'a> {
    pub key_id: &'a str,
    pub secret: &'a str,
}

impl Effect for BucketSecret<'_> {
    const KIND: &'static str = "bucket_secret";
    type Reply = ();

    fn describe(&self) -> String {
        format!("store the bucket secret for {}", self.key_id)
    }

    fn writes(&self) -> bool {
        true
    }

    fn requires_writer(&self) -> bool { false }

    fn perform(&self, cx: &mut Ctx<'_>) -> Result<(), String> {
        cx.cap::<dyn Secrets>()?
            .set(&r2::secret_key(self.key_id), self.secret)
            .then_some(())
            .ok_or_else(|| "the keychain refused the bucket secret".to_string())
    }
}

#[cfg(test)]
mod handoff_tests;

#[cfg(test)]
mod lifecycle_tests;

#[cfg(test)]
mod service_tests;

#[cfg(test)]
mod tests {
    use super::*;
    use object::{MemBucket, Cas, PutNew, Object, read_state};
    use std::sync::Arc;

    fn store() -> Store {
        Store::open(None, &[]).expect("in-memory store")
    }

    /// A key nobody else writes, so a `meta` row stands in for whatever an
    /// app would have written.
    fn put(s: &Store, key: &'static str, value: &'static str) {
        s.write(move |tx| {
            tx.execute(
                "INSERT INTO meta(key, value) VALUES(?1, ?2)
                 ON CONFLICT(key) DO UPDATE SET value = excluded.value",
                [key, value],
            )
            .map(|_| ())
        })
        .expect("a write");
    }

    fn got(s: &Store, key: &str) -> Option<String> {
        s.conn()
            .query_row("SELECT value FROM meta WHERE key = ?1", [key], |r| r.get(0))
            .ok()
    }

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
        let mut extra = bytes.clone();
        extra.push(0);
        assert!(decode_batch(&extra).is_err(), "trailing data is not part of a canonical batch");
    }

    /// A local write is captured, drains into a peer, the peer converges —
    /// and applying records **nothing** on the peer, so nothing echoes back.
    #[test]
    fn a_write_captures_drains_and_does_not_echo() {
        let a = store();
        let b = store();

        put(&a, "one", "first");
        put(&a, "two", "second");
        assert_eq!(a.pending_frames().len(), 2, "two writes, two frames");
        assert_eq!(a.unpublished(), 2);

        assert_eq!(crate::runtime::block_on(drain(&a, &b)).unwrap(), 2);

        assert_eq!(got(&b, "one").as_deref(), Some("first"));
        assert_eq!(got(&b, "two").as_deref(), Some("second"));

        // No echo: applying on B recorded nothing in B's own log.
        assert_eq!(b.pending_frames().len(), 0, "apply must not capture");
        assert_eq!(crate::runtime::block_on(drain(&b, &a)).unwrap(), 0, "B has nothing to send back");

        // A's frames are published now, so a second drain moves nothing.
        assert_eq!(a.unpublished(), 0);
        assert_eq!(crate::runtime::block_on(drain(&a, &b)).unwrap(), 0);
    }

    /// Two installs mint different device ids — they must never collide, or
    /// they would publish under the same name. And the id is the store's
    /// own: it lives outside `meta`, so replication never carries it.
    #[test]
    fn devices_are_distinct_and_never_replicate() {
        let a = store();
        let b = store();
        assert_ne!(a.device(), b.device());
        assert!(!a.device().is_empty());

        let (was_a, was_b) = (a.device(), b.device());
        put(&a, "shared", "v1");
        crate::runtime::block_on(drain(&a, &b)).unwrap();
        assert_eq!(got(&b, "shared").as_deref(), Some("v1"));
        assert_eq!(a.device(), was_a);
        assert_eq!(b.device(), was_b, "B kept its own id");
    }

    /// A bucket that answers "no object" and then refuses to create one — a
    /// name with a typo in it, a key without permission — is not a lineage
    /// waiting to be started. The pass says so once instead of asking again
    /// forever: before this was bounded, the retry was a recursion and the
    /// process died of it.
    #[test]
    fn a_bucket_that_cannot_be_written_is_not_bootstrapped_forever() {
        struct RefusesWrites;
        #[async_trait::async_trait(?Send)]
        impl Object for RefusesWrites {
            async fn get(&self, _key: &str) -> Result<Option<object::Blob>, String> {
                Ok(None)
            }
            async fn put_new(&self, _key: &str, _body: &[u8]) -> Result<PutNew, String> {
                Err("bucket PUT: 404 NoSuchBucket".into())
            }
            async fn cas(&self, _key: &str, _body: &[u8], _etag: &str) -> Result<Cas, String> {
                Err("bucket CAS: 404 NoSuchBucket".into())
            }
        }
        let store = store();
        // Returning at all is the assertion; the rest is what it should say.
        let s = crate::runtime::block_on(poll(&store, &RefusesWrites));
        assert_eq!(
            s.role,
            Role::Detached,
            "a device that never joined stays local"
        );
        assert!(store.is_writable());
        assert_eq!(s.note.as_deref(), Some("bucket PUT: 404 NoSuchBucket"));
    }

    /// A follower whose credentials go missing must not come back as a
    /// writer. The store opens *writable*, so "no bucket" cannot mean "no
    /// lease": a device that has joined a lineage keeps its gate shut and
    /// says why — and that is the one standing problem device sync has.
    #[test]
    fn a_follower_that_loses_its_bucket_stays_locked_and_says_so() {
        let bucket = MemBucket::new();
        let a = store();
        let b = store();
        assert_eq!(crate::runtime::block_on(poll(&a, &bucket)).role, Role::Holder);
        assert!(matches!(crate::runtime::block_on(poll(&b, &bucket)).role, Role::Follower { .. }));
        assert!(!b.is_writable());
        assert!(
            BUCKET_PROBLEM.list(&b).is_empty(),
            "a follower behind the locked screen is not listed twice over"
        );

        let broken = r2::Broken("no secret for AKIDEXAMPLE".into());
        let s = crate::runtime::block_on(poll(&b, &broken));
        assert_eq!(s.role, Role::Offline);
        assert!(
            !b.is_writable(),
            "a follower with no reachable bucket is still a follower"
        );
        assert_eq!(s.note.as_deref(), Some("no secret for AKIDEXAMPLE"));

        // …and the kernel's own problem source says it, off the row the pass
        // wrote.
        let p = BUCKET_PROBLEM.list(&b);
        assert_eq!(p.len(), 1);
        assert_eq!(p[0].key, "sync");
        assert_eq!(p[0].line, "no secret for AKIDEXAMPLE");
        assert_eq!(p[0].detail, "nothing waiting to publish");

        // A previous holder also suspends until ownership can be confirmed.
        put(&a, "offline", "yes");
        let sa = crate::runtime::block_on(poll(&a, &broken));
        assert_eq!(sa.role, Role::Offline);
        assert!(!a.is_writable());
        assert_eq!(BUCKET_PROBLEM.list(&a).len(), 1, "the suspended writer reports its transport failure");
    }

    /// What the problem counts: the frames a device that cannot reach the
    /// bucket is sitting on. The row is what it reads, so the state is
    /// arranged the way a pass would leave it.
    #[test]
    fn an_unreachable_bucket_counts_its_backlog() {
        let s = store();
        assert!(BUCKET_PROBLEM.list(&s).is_empty(), "nothing has run");

        put(&s, "one", "first");
        put(&s, "two", "second");
        s.set_status(Role::Offline.word(), Some("connection refused"))
            .unwrap();
        let p = BUCKET_PROBLEM.list(&s);
        assert_eq!(p.len(), 1);
        assert_eq!(p[0].key, "sync");
        assert_eq!(p[0].label, "device sync");
        assert_eq!(p[0].line, "connection refused");
        assert_eq!(p[0].detail, "2 frames waiting to publish");

        // One reads as one, and a role that is not offline lists nothing.
        s.mark_published(1).unwrap();
        assert_eq!(BUCKET_PROBLEM.list(&s)[0].detail, "1 frame waiting to publish");
        s.set_status(Role::Holder.word(), None).unwrap();
        assert!(BUCKET_PROBLEM.list(&s).is_empty());
    }

    /// The whole lease lifecycle across two devices sharing one bucket:
    /// bootstrap, install, publish/materialize both ways, a clean handoff
    /// through release+acquire, follower read-only, and an override that
    /// strands the old holder.
    #[test]
    fn equal_versions_with_different_column_orders_refuse_to_sync() {
        use crate::app::{Schema, Step};
        static A: Schema = Schema { app: "flags", steps: &[Step::Sql(
            "CREATE TABLE flags(id INTEGER PRIMARY KEY,blocked INTEGER,is_forum INTEGER)")] };
        static B: Schema = Schema { app: "flags", steps: &[Step::Sql(
            "CREATE TABLE flags(id INTEGER PRIMARY KEY,is_forum INTEGER,blocked INTEGER)")] };
        let a = Store::open(None, &[&A]).unwrap();
        let b = Store::open(None, &[&B]).unwrap();
        a.write(|c| c.execute("INSERT INTO flags VALUES(1,0,1)", []).map(|_| ())).unwrap();
        let bucket = MemBucket::new();
        assert_eq!(crate::runtime::block_on(poll(&a, &bucket)).role, Role::Holder);
        assert_eq!(crate::runtime::block_on(poll(&b, &bucket)).role, Role::Incompatible);
        assert!(!b.is_writable());
        assert!(crate::runtime::block_on(acquire(&b, &bucket)).is_err());
        let count: i64 = b.conn().query_row("SELECT COUNT(*) FROM flags", [], |r| r.get(0)).unwrap();
        assert_eq!(count, 0, "a mismatched snapshot must never be copied positionally");
        a.write(|c| c.execute("ALTER TABLE flags ADD COLUMN extra INTEGER", []).map(|_| ())).unwrap();
        assert_eq!(crate::runtime::block_on(poll(&a, &bucket)).role, Role::Incompatible);
        assert!(!a.is_writable(), "a migrated holder must not publish into an older layout");
        assert_eq!(crate::runtime::block_on(poll(&a, &r2::Broken("offline".into()))).role, Role::Incompatible);
        assert!(!a.is_writable());
    }

    #[test]
    fn two_devices_sync_acquire_and_strand() {
        let bucket = MemBucket::new();
        let a = store();
        let b = store();

        // A writes, then polls: no lineage, so A bootstraps and holds.
        put(&a, "alice", "a@x");
        assert_eq!(crate::runtime::block_on(poll(&a, &bucket)).role, Role::Holder);
        assert!(a.is_writable());

        // B polls: A holds, so B installs the snapshot and locks.
        let sb = crate::runtime::block_on(poll(&b, &bucket));
        assert!(matches!(sb.role, Role::Follower { .. }), "{:?}", sb.role);
        assert!(!b.is_writable(), "a follower is read-only");
        assert_eq!(got(&b, "alice").as_deref(), Some("a@x"));

        // A follower's ordinary write is refused at the gate.
        assert!(
            b.write(|tx| tx
                .execute("INSERT INTO meta(key, value) VALUES('x','x')", [])
                .map(|_| ()))
                .is_err(),
            "the gate refuses a follower's write"
        );

        // A writes more; a poll publishes it; B's poll materializes it.
        put(&a, "bob", "b@x");
        crate::runtime::block_on(poll(&a, &bucket));
        assert_eq!(a.unpublished(), 0, "the holder published what it captured");
        crate::runtime::block_on(poll(&b, &bucket));
        assert_eq!(got(&b, "bob").as_deref(), Some("b@x"));

        // A hands the lease back; B sees it free and acquires it.
        assert_eq!(crate::runtime::block_on(release(&a, &bucket)).unwrap().role, Role::Free);
        assert!(!a.is_writable(), "a released holder is read-only");
        assert_eq!(crate::runtime::block_on(poll(&b, &bucket)).role, Role::Free);
        assert_eq!(crate::runtime::block_on(acquire(&b, &bucket)).unwrap().role, Role::Holder);
        assert!(b.is_writable());

        // A now follows B — a handoff, not a strand.
        let sa = crate::runtime::block_on(poll(&a, &bucket));
        assert!(matches!(sa.role, Role::Follower { .. }), "{:?}", sa.role);

        // B writes; A materializes it the other direction.
        put(&b, "carol", "c@x");
        crate::runtime::block_on(poll(&b, &bucket));
        crate::runtime::block_on(poll(&a, &bucket));
        assert_eq!(got(&a, "carol").as_deref(), Some("c@x"), "both ways");

        // B captures one more write but does NOT publish it — divergent work
        // that the canonical history never receives.
        put(&b, "dave", "d@x");
        assert!(b.unpublished() > 0, "B holds an unpublished write");

        // Override: B still holds, A takes it anyway (epoch bump). B never
        // released AND has divergent unpublished work, so it is stranded on
        // its next pass — recovery is a manual reset.
        let e_before = read_epoch(&bucket);
        assert_eq!(crate::runtime::block_on(override_lease(&a, &bucket)).unwrap().role, Role::Holder);
        assert!(read_epoch(&bucket) > e_before, "an override bumps the epoch");
        let sb = crate::runtime::block_on(poll(&b, &bucket));
        assert!(matches!(sb.role, Role::Stranded { .. }), "{:?}", sb.role);
        assert!(!b.is_writable(), "a stranded device is read-only");
    }

    fn read_epoch(bucket: &MemBucket) -> i64 {
        crate::runtime::block_on(read_state(bucket)).unwrap().unwrap().0.epoch
    }

    /// A stranded device — one that held unpublished writes when it was
    /// overridden — recovers by resetting to the canonical baseline and
    /// replaying, discarding its divergent local writes.
    #[test]
    fn a_stranded_holder_recovers_by_reset() {
        let bucket = MemBucket::new();
        let a = store();
        let b = store();

        assert_eq!(crate::runtime::block_on(poll(&a, &bucket)).role, Role::Holder);
        put(&a, "shared", "v1");
        crate::runtime::block_on(poll(&a, &bucket));

        assert!(matches!(crate::runtime::block_on(poll(&b, &bucket)).role, Role::Follower { .. }));
        assert_eq!(got(&b, "shared").as_deref(), Some("v1"));

        // A makes a divergent local write it has NOT published.
        put(&a, "k", "local-only");
        assert!(a.unpublished() >= 1, "A holds an unpublished divergent write");

        // B overrides and writes a conflicting value to the same key.
        assert_eq!(crate::runtime::block_on(override_lease(&b, &bucket)).unwrap().role, Role::Holder);
        put(&b, "k", "from-b");
        crate::runtime::block_on(poll(&b, &bucket));

        // A is stranded: read-only, its history diverged.
        assert!(matches!(crate::runtime::block_on(poll(&a, &bucket)).role, Role::Stranded { .. }));
        assert!(!a.is_writable());

        // Acquiring cannot discard a branch. Explicit recovery backs it up
        // before adopting the canonical history, and remains a follower.
        assert!(matches!(crate::runtime::block_on(acquire(&a, &bucket)).unwrap().role, Role::Stranded { .. }));
        assert!(matches!(crate::runtime::block_on(recover(&a, &bucket)).unwrap().role, Role::Follower { .. }));
        assert!(!a.is_writable());
        assert_eq!(
            got(&a, "k").as_deref(),
            Some("from-b"),
            "A adopted the canonical value, discarding its divergent local one"
        );
        assert_eq!(got(&a, "shared").as_deref(), Some("v1"));
        assert_eq!(
            a.unpublished(),
            0,
            "the stale pending frame was cleared on install"
        );
    }

    /// The model's finding (`formal/Lease.tla`, `NoStaleWrite`): a superseded
    /// holder's unpublished write that does NOT row-conflict with the
    /// canonical line used to survive its re-acquire and be published under
    /// the new epoch — after writes it never saw. It is divergent all the
    /// same, and must be recovered explicitly before acquiring.
    #[test]
    fn a_superseded_holders_nonconflicting_write_requires_recovery() {
        let bucket = MemBucket::new();
        let a = store();
        let b = store();

        assert_eq!(crate::runtime::block_on(poll(&a, &bucket)).role, Role::Holder);
        assert!(matches!(crate::runtime::block_on(poll(&b, &bucket)).role, Role::Follower { .. }));

        // A's unpublished write, under epoch 1.
        put(&a, "k", "stale");

        // B takes over (epoch 2) and writes a DIFFERENT key: no row conflict.
        assert_eq!(crate::runtime::block_on(override_lease(&b, &bucket)).unwrap().role, Role::Holder);
        put(&b, "other", "from-b");
        crate::runtime::block_on(poll(&b, &bucket));

        // A's epoch-1 frame cannot be published under a newer lease.
        assert!(matches!(crate::runtime::block_on(poll(&a, &bucket)).role, Role::Stranded { .. }));
        assert!(matches!(crate::runtime::block_on(acquire(&a, &bucket)).unwrap().role, Role::Stranded { .. }));
        assert_eq!(got(&a, "k").as_deref(), Some("stale"));
        crate::runtime::block_on(recover(&a, &bucket)).unwrap();
        assert_eq!(got(&a, "other").as_deref(), Some("from-b"));
        assert_eq!(
            got(&a, "k"),
            None,
            "the superseded write was discarded, not merged"
        );
        assert_eq!(a.unpublished(), 0, "nothing stale is left to publish");

        // And so it never reaches B.
        crate::runtime::block_on(poll(&a, &bucket));
        assert_eq!(crate::runtime::block_on(poll(&b, &bucket)).role, Role::Holder);
        assert_eq!(got(&b, "k"), None);
    }

    /// The same hole on the poll path: if the overrider has *released*, the
    /// superseded device follows rather than strands — and used to keep its
    /// stale frame pending for a later acquire. It must stay stranded until
    /// explicit, backed-up recovery instead.
    #[test]
    fn a_superseded_holder_following_a_released_lease_preserves_its_branch() {
        let bucket = MemBucket::new();
        let a = store();
        let b = store();

        assert_eq!(crate::runtime::block_on(poll(&a, &bucket)).role, Role::Holder);
        assert!(matches!(crate::runtime::block_on(poll(&b, &bucket)).role, Role::Follower { .. }));
        put(&a, "k", "stale");

        assert_eq!(crate::runtime::block_on(override_lease(&b, &bucket)).unwrap().role, Role::Holder);
        put(&b, "other", "from-b");
        assert_eq!(crate::runtime::block_on(release(&b, &bucket)).unwrap().role, Role::Free);

        // A never polled while B held: it sees a free, newer lineage.
        assert!(matches!(crate::runtime::block_on(poll(&a, &bucket)).role, Role::Stranded { .. }));
        assert_eq!(got(&a, "k").as_deref(), Some("stale"));
        assert!(a.unpublished() > 0);
        assert_eq!(crate::runtime::block_on(recover(&a, &bucket)).unwrap().role, Role::Free);
        assert_eq!(got(&a, "other").as_deref(), Some("from-b"));
        assert_eq!(got(&a, "k"), None, "the stale write went with the reset");
        assert_eq!(a.unpublished(), 0);

        // Acquiring now publishes nothing stale.
        assert_eq!(crate::runtime::block_on(acquire(&a, &bucket)).unwrap().role, Role::Holder);
        crate::runtime::block_on(poll(&a, &bucket));
        crate::runtime::block_on(poll(&b, &bucket));
        assert_eq!(got(&b, "k"), None);
    }

    /// A holder that published everything it wrote, then was overridden, has
    /// NOT diverged: it follows cleanly (the "take over" screen), not strands
    /// (the "recover" screen). Only genuine unpublished divergence strands.
    #[test]
    fn an_overridden_holder_with_nothing_unpublished_follows_cleanly() {
        let bucket = MemBucket::new();
        let a = store();
        let b = store();

        // A holds and publishes a write — nothing left unpublished.
        assert_eq!(crate::runtime::block_on(poll(&a, &bucket)).role, Role::Holder);
        put(&a, "k", "from-a");
        crate::runtime::block_on(poll(&a, &bucket));
        assert_eq!(a.unpublished(), 0, "A published all it wrote");

        // B joins and overrides A (an override: A never released).
        assert!(matches!(crate::runtime::block_on(poll(&b, &bucket)).role, Role::Follower { .. }));
        assert_eq!(crate::runtime::block_on(override_lease(&b, &bucket)).unwrap().role, Role::Holder);
        put(&b, "k2", "from-b");
        crate::runtime::block_on(poll(&b, &bucket));

        // A polls: overridden, but with nothing unpublished it is a plain
        // Follower — read-only, "take over" — not Stranded / "recover".
        let sa = crate::runtime::block_on(poll(&a, &bucket));
        assert!(
            matches!(sa.role, Role::Follower { .. }),
            "clean override follows, got {:?}",
            sa.role
        );
        assert!(!a.is_writable());
        assert_eq!(got(&a, "k2").as_deref(), Some("from-b"), "A caught up");
        assert_eq!(got(&a, "k").as_deref(), Some("from-a"), "and kept its own");

        // The screen words each of those.
        assert_eq!(sa.role.locked_screen().1, Some("take over"));
        assert_eq!(Role::Free.locked_screen().1, Some("acquire"));
        assert_eq!(Role::Offline.locked_screen().1, None);
        assert!(Role::Holder.line().contains("hold"));

        // A can request a handoff without resetting either device.
        assert!(matches!(crate::runtime::block_on(acquire(&a, &bucket)).unwrap().role, Role::Waiting { .. }));
        crate::runtime::block_on(poll(&b, &bucket));
        assert_eq!(crate::runtime::block_on(poll(&a, &bucket)).role, Role::Holder);
        assert!(a.is_writable());
    }

    /// The same sync, but over the **real HTTP transport** — the daemon's
    /// handler and the `HttpBucket` client on a live socket — so the stack
    /// the desktop uses (snapshot upload/install, batch upload/apply, the
    /// lease CAS, all over HTTP) is proven end to end.
    #[test]
    fn two_devices_sync_over_real_http() {
        use object::{serve_conn, HttpBucket};
        use tokio::net::TcpListener;

        let dir = std::env::temp_dir().join(format!("superapp-repl-http-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let listener = crate::runtime::block_on(TcpListener::bind("127.0.0.1:0")).unwrap();
        let addr = listener.local_addr().unwrap();
        let sdir = dir.clone();
        let server = crate::runtime::spawn(async move {
            let lock = tokio::sync::Mutex::new(());
            while let Ok((mut stream, _)) = listener.accept().await {
                let _ = serve_conn(&sdir, &mut stream, &lock).await;
            }
        });

        let bucket = HttpBucket::new(&format!("http://{addr}"));
        let a = store();
        let b = store();

        // A bootstraps over HTTP and holds; the snapshot and state go up.
        put(&a, "alice", "a@x");
        assert_eq!(crate::runtime::block_on(poll(&a, &bucket)).role, Role::Holder);

        // B installs A's snapshot over HTTP and locks.
        assert!(matches!(crate::runtime::block_on(poll(&b, &bucket)).role, Role::Follower { .. }));
        assert_eq!(got(&b, "alice").as_deref(), Some("a@x"));

        // A writes more; a poll uploads the batch; B materializes it.
        put(&a, "bob", "b@x");
        crate::runtime::block_on(poll(&a, &bucket));
        crate::runtime::block_on(poll(&b, &bucket));
        assert_eq!(got(&b, "bob").as_deref(), Some("b@x"));

        // The lease CAS works over HTTP: B takes over.
        assert!(matches!(crate::runtime::block_on(acquire(&b, &bucket)).unwrap().role, Role::Waiting { .. }));
        crate::runtime::block_on(poll(&a, &bucket));
        assert_eq!(crate::runtime::block_on(poll(&b, &bucket)).role, Role::Holder);
        assert!(b.is_writable());

        server.abort();
        let _ = crate::runtime::block_on(server);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The driver is a thread with a command channel: it reports a role
    /// without being asked, takes the lease when told to, hands it back, and
    /// can be waited for rather than merely dropped.
    #[test]
    fn the_driver_serializes_passes_and_shutdown() {
        let bucket = Arc::new(MemBucket::new());
        let a = store();
        // A holds, so the driver's device will find the lease taken.
        assert_eq!(crate::runtime::block_on(poll(&a, &*bucket)).role, Role::Holder);

        let b = store();
        let driver = spawn(b.db(), bucket.clone(), || {});
        let settled = |want: fn(&Role) -> bool| {
            for _ in 0..400 {
                if want(&driver.status().role) {
                    return true;
                }
                std::thread::sleep(std::time::Duration::from_millis(10));
            }
            false
        };
        assert!(
            settled(|r| matches!(r, Role::Follower { .. })),
            "the first pass runs at once: {:?}",
            driver.status()
        );
        assert!(!b.is_writable());

        driver.acquire();
        assert!(settled(|r| matches!(r, Role::Waiting { .. })), "{:?}", driver.status());
        assert_eq!(crate::runtime::block_on(poll(&a, &*bucket)).role, Role::Free);
        driver.kick();
        assert!(settled(|r| *r == Role::Holder), "{:?}", driver.status());
        assert!(b.is_writable());

        driver.kick();
        driver.release();
        assert!(settled(|r| *r == Role::Free), "{:?}", driver.status());
        crate::runtime::block_on(driver.release_wait());
        crate::runtime::block_on(driver.stop());
    }
}
