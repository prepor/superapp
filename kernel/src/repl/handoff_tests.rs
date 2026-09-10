//! Deterministic transport faults and handoff races; no network or user data.
use crate::repl::{
    self,
    object::{self, Blob, Cas, MemBucket, Object, PutNew},
    Role,
};
use crate::runtime::block_on;
use crate::store::{Db, Store};
use std::sync::{
    atomic::{AtomicBool, AtomicUsize, Ordering},
    Arc,
};

fn store() -> Store {
    Store::open(None, &[]).unwrap()
}
fn put(s: &Store, key: &'static str, value: &'static str) {
    s.write(move |tx| tx.execute("INSERT INTO meta(key,value) VALUES(?1,?2) ON CONFLICT(key) DO UPDATE SET value=excluded.value", [key,value]).map(|_| ())).unwrap();
}
fn got(s: &Store, key: &str) -> Option<String> {
    s.conn()
        .query_row("SELECT value FROM meta WHERE key=?1", [key], |r| r.get(0))
        .ok()
}
fn state(b: &dyn Object) -> object::State {
    block_on(object::read_state(b)).unwrap().unwrap().0
}
fn pair() -> (Store, Store, MemBucket) {
    let (a, b, bucket) = (store(), store(), MemBucket::new());
    assert_eq!(block_on(repl::poll(&a, &bucket)).role, Role::Holder);
    assert!(matches!(
        block_on(repl::poll(&b, &bucket)).role,
        Role::Follower { .. }
    ));
    (a, b, bucket)
}

enum Fault {
    WriteWhileUploading(Arc<Db>),
    TakeoverOnSecondRead(Arc<Db>),
    CompetingAcquireBeforeCas(Arc<Db>),
    LosePublishAcknowledgement,
    UnknownBatchHeaderVersion,
    BreakBatchAncestry,
}
struct FaultBucket {
    inner: MemBucket,
    fault: Fault,
    fired: AtomicBool,
    accepted: AtomicBool,
    reads: AtomicUsize,
}
impl FaultBucket {
    fn new(inner: MemBucket, fault: Fault) -> Self {
        Self {
            inner,
            fault,
            fired: AtomicBool::new(false),
            accepted: AtomicBool::new(false),
            reads: AtomicUsize::new(0),
        }
    }
    fn once(&self) -> bool {
        !self.fired.swap(true, Ordering::SeqCst)
    }
}
#[async_trait::async_trait(?Send)]
impl Object for FaultBucket {
    async fn get(&self, key: &str) -> Result<Option<Blob>, String> {
        if key == object::STATE_KEY {
            if let Fault::TakeoverOnSecondRead(db) = &self.fault {
                if self.reads.fetch_add(1, Ordering::SeqCst) == 1 {
                    let other = Store::with_db(db.clone()).unwrap();
                    repl::override_lease(&other, &self.inner).await?;
                }
            }
        }
        let mut blob = self.inner.get(key).await?;
        if key.starts_with("log/") {
            if let Some(b) = blob.as_mut() {
                if matches!(
                    self.fault,
                    Fault::UnknownBatchHeaderVersion | Fault::BreakBatchAncestry
                ) && self.once()
                {
                    let len = u32::from_le_bytes(b.bytes[..4].try_into().unwrap()) as usize;
                    let mut header: serde_json::Value =
                        serde_json::from_slice(&b.bytes[4..4 + len]).unwrap();
                    match self.fault {
                        Fault::UnknownBatchHeaderVersion => header["v"] = 999.into(),
                        Fault::BreakBatchAncestry => header["prev"] = serde_json::Value::Null,
                        _ => unreachable!(),
                    }
                    let bytes = serde_json::to_vec(&header).unwrap();
                    let mut encoded = (bytes.len() as u32).to_le_bytes().to_vec();
                    encoded.extend(bytes);
                    encoded.extend_from_slice(&b.bytes[4 + len..]);
                    b.bytes = encoded;
                }
            }
        }
        Ok(blob)
    }
    async fn put_new(&self, key: &str, body: &[u8]) -> Result<PutNew, String> {
        if key.starts_with("log/") {
            if let Fault::WriteWhileUploading(db) = &self.fault {
                if self.once() {
                    let s = Store::with_db(db.clone()).unwrap();
                    let admitted=s.write_async(|tx| tx.execute("INSERT INTO meta(key,value) VALUES('accepted-during-release','keep me')",[]).map(|_|())).await.is_ok();
                    self.accepted.store(admitted, Ordering::SeqCst);
                }
            }
        }
        self.inner.put_new(key, body).await
    }
    async fn cas(&self, key: &str, body: &[u8], etag: &str) -> Result<Cas, String> {
        if let Fault::CompetingAcquireBeforeCas(db) = &self.fault {
            if self.once() {
                let other = Store::with_db(db.clone()).unwrap();
                repl::acquire(&other, &self.inner).await?;
            }
        }
        let result = self.inner.cas(key, body, etag).await?;
        if matches!(self.fault, Fault::LosePublishAcknowledgement)
            && matches!(result, Cas::Ok(_))
            && self.once()
        {
            return Err("injected: remote committed, response was lost".into());
        }
        Ok(result)
    }
}

#[test]
fn normal_release_must_preserve_an_edit_accepted_during_upload() {
    let (a, b, bucket) = pair();
    put(&a, "before-release", "published");
    let fault = FaultBucket::new(bucket.clone(), Fault::WriteWhileUploading(a.db()));
    assert_eq!(
        block_on(repl::release(&a, &fault)).unwrap().role,
        Role::Free
    );
    if !fault.accepted.load(Ordering::SeqCst) {
        assert_eq!(
            a.unpublished(),
            0,
            "closing the gate must also drain already accepted writes"
        );
        return;
    }
    assert_eq!(
        got(&a, "accepted-during-release").as_deref(),
        Some("keep me")
    );
    eprintln!(
        "successful release left {} unpublished frame(s)",
        a.unpublished()
    );
    block_on(repl::acquire(&b, &bucket)).unwrap();
    block_on(repl::poll(&a, &bucket));
    assert_eq!(
        got(&a, "accepted-during-release").as_deref(),
        Some("keep me"),
        "ordinary handoff deleted an acknowledged edit without recovery"
    );
}

#[test]
fn release_must_not_release_a_new_holders_lease() {
    let (a, b, bucket) = pair();
    let fault = FaultBucket::new(bucket.clone(), Fault::TakeoverOnSecondRead(b.db()));
    block_on(repl::release(&a, &fault)).unwrap();
    let remote = state(&bucket);
    assert_eq!(remote.holder.as_deref(), Some(b.device().as_str()));
    assert!(b.is_writable());
    assert!(
        !remote.released,
        "old holder released the replacement holder's epoch"
    );
}

#[test]
fn lost_publish_acknowledgement_must_not_duplicate_a_committed_frame() {
    let (a, b, bucket) = pair();
    put(&a, "exactly-once", "saved");
    let fault = FaultBucket::new(bucket.clone(), Fault::LosePublishAcknowledgement);
    let first = block_on(repl::poll(&a, &fault));
    assert!(first.note.is_some());
    assert_eq!(state(&bucket).seq, 1);
    assert_eq!(a.unpublished(), 1);
    block_on(repl::poll(&a, &bucket));
    let follower = block_on(repl::poll(&b, &bucket));
    eprintln!(
        "remote seq={}, follower role={:?}, note={:?}",
        state(&bucket).seq,
        follower.role,
        follower.note
    );
    assert_eq!(
        state(&bucket).seq,
        1,
        "same local transaction was published twice"
    );
    assert!(matches!(follower.role, Role::Follower { .. }));
}

#[test]
fn known_stranding_must_stay_read_only_when_the_network_fails() {
    let (a, b, bucket) = pair();
    put(&a, "offline-edit", "preserve");
    block_on(repl::override_lease(&b, &bucket)).unwrap();
    assert!(matches!(
        block_on(repl::poll(&a, &bucket)).role,
        Role::Stranded { .. }
    ));
    assert!(!a.is_writable());
    let failed = block_on(repl::poll(&a, &repl::r2::Broken("temporary outage".into())));
    assert!(
        !failed.role.writable(),
        "known lost lease became writable again: {:?}",
        failed.role
    );
}

#[test]
fn stranding_must_not_discard_edits_just_because_the_other_device_releases() {
    let (a, b, bucket) = pair();
    put(&a, "unexported-edit", "preserve");
    block_on(repl::override_lease(&b, &bucket)).unwrap();
    assert!(matches!(
        block_on(repl::poll(&a, &bucket)).role,
        Role::Stranded { .. }
    ));
    block_on(repl::release(&b, &bucket)).unwrap();
    block_on(repl::poll(&a, &bucket));
    assert_eq!(
        got(&a, "unexported-edit").as_deref(),
        Some("preserve"),
        "poll deleted the divergent branch with no export or recovery action"
    );
}

#[test]
fn unknown_batch_header_version_must_be_refused() {
    let (a, b, bucket) = pair();
    put(&a, "future-wire", "value");
    block_on(repl::poll(&a, &bucket));
    let fault = FaultBucket::new(bucket, Fault::UnknownBatchHeaderVersion);
    let status = block_on(repl::poll(&b, &fault));
    assert!(
        status.note.is_some(),
        "unknown outer wire version was applied without validation"
    );
    assert_eq!(b.materialized(), 0);
}

#[test]
fn a_gap_in_batch_ancestry_must_not_advance_the_watermark() {
    let (a, b, bucket) = pair();
    put(&a, "first-frame", "one");
    block_on(repl::poll(&a, &bucket));
    put(&a, "second-frame", "two");
    block_on(repl::poll(&a, &bucket));
    let fault = FaultBucket::new(bucket, Fault::BreakBatchAncestry);
    let status = block_on(repl::poll(&b, &fault));
    eprintln!(
        "follower watermark={}, missing first frame={}, role={:?}",
        b.materialized(),
        got(&b, "first-frame").is_none(),
        status.role
    );
    assert!(
        status.note.is_some(),
        "missing ancestry silently skipped a transaction"
    );
}

#[test]
fn control_an_ordinary_handoff_converges() {
    let (a, b, bucket) = pair();
    put(&a, "control", "first");
    block_on(repl::release(&a, &bucket)).unwrap();
    block_on(repl::acquire(&b, &bucket)).unwrap();
    assert_eq!(got(&b, "control").as_deref(), Some("first"));
    put(&b, "control", "second");
    block_on(repl::release(&b, &bucket)).unwrap();
    block_on(repl::poll(&a, &bucket));
    assert_eq!(got(&a, "control"), got(&b, "control"));
    assert_eq!((a.unpublished(), b.unpublished()), (0, 0));
}

#[test]
fn acquiring_a_free_lease_must_not_silently_override_the_race_winner() {
    let (a, b, bucket) = pair();
    block_on(repl::release(&a, &bucket)).unwrap();
    assert_eq!(block_on(repl::poll(&b, &bucket)).role, Role::Free);
    let c = store();
    assert_eq!(block_on(repl::poll(&c, &bucket)).role, Role::Free);
    let fault = FaultBucket::new(bucket.clone(), Fault::CompetingAcquireBeforeCas(c.db()));
    // Both users saw "acquire", not "take over". C wins first; B's CAS loses.
    let result = block_on(repl::acquire(&b, &fault));
    let remote = state(&bucket);
    eprintln!(
        "free-lease race: result={result:?}, first winner still writable={}, epoch={}",
        c.is_writable(),
        remote.epoch
    );
    assert_eq!(
        remote.holder.as_deref(),
        Some(c.device().as_str()),
        "losing ordinary acquisition automatically escalated to a forced takeover"
    );
}

#[test]
fn takeover_drains_a_busy_holder_in_both_directions() {
    // Exercise the actual file-backed WAL configuration. SQLite's shared
    // in-memory cache has table-level reader locks (SQLITE_LOCKED_SHAREDCACHE)
    // that production WAL readers do not acquire; using it here can reject
    // queued writes merely because the protocol reads its local repl row.
    struct Directory(std::path::PathBuf);
    impl Drop for Directory {
        fn drop(&mut self) { let _ = std::fs::remove_dir_all(&self.0); }
    }
    let directory = Directory(std::env::temp_dir().join(format!("superapp-busy-handoff-{}-{}",
        std::process::id(), std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos())));
    std::fs::create_dir(&directory.0).unwrap();
    let a = Store::open(Some(&directory.0.join("phone.db")), &[]).unwrap();
    let b = Store::open(Some(&directory.0.join("desktop.db")), &[]).unwrap();
    let bucket = MemBucket::new();
    for device in [&a, &b] {
        let mode: String = device.conn().query_row("PRAGMA journal_mode", [], |r| r.get(0)).unwrap();
        assert_eq!(mode, "wal");
    }
    assert_eq!(block_on(repl::poll(&a, &bucket)).role, Role::Holder);
    assert!(matches!(block_on(repl::poll(&b, &bucket)).role, Role::Follower { .. }));
    for (holder, next, value) in [(&a, &b, "phone"), (&b, &a, "desktop")] {
        let epoch = state(&bucket).epoch;
        assert!(matches!(
            block_on(repl::acquire(next, &bucket)).unwrap().role,
            Role::Waiting { .. }
        ));
        assert_eq!(
            state(&bucket).epoch,
            epoch,
            "requesting does not revoke the holder"
        );
        assert!(holder.is_writable());
        assert!(!next.is_writable());

        // Provider updates continue after the request, including writes still
        // in the writer queue when the holder begins its next pass.
        let mut accepted = Vec::new();
        for id in 0..1500 {
            accepted.push(holder.submit_write(move |tx| {
                tx.execute("INSERT INTO meta(key,value) VALUES(?1,?2) ON CONFLICT(key) DO UPDATE SET value=excluded.value",
                    [format!("background-{id}"), value.to_string()]).map(|_| ())
            }).unwrap());
        }
        assert_eq!(block_on(repl::poll(holder, &bucket)).role, Role::Free);
        assert!(!holder.is_writable());
        for (index, mut write) in accepted.into_iter().enumerate() {
            let result = write.poll(holder).expect("the release barrier drained every accepted write");
            assert!(result.is_ok(), "accepted write {index} during {value} handoff failed: {result:?}");
        }
        assert_eq!(holder.unpublished(), 0);
        assert_eq!(block_on(repl::poll(next, &bucket)).role, Role::Holder);
        assert_eq!(state(&bucket).epoch, epoch + 1);
        assert!(matches!(
            block_on(repl::poll(holder, &bucket)).role,
            Role::Follower { .. }
        ));
        assert_eq!(holder.materialized(), next.materialized());
        for id in 0..1500 {
            assert_eq!(
                got(next, &format!("background-{id}")).as_deref(),
                Some(value)
            );
        }
    }
}

#[test]
fn leaving_cancels_a_handoff_and_its_released_reservation() {
    for release_first in [false, true] {
        let (a, b, bucket) = pair();
        block_on(repl::acquire(&b, &bucket)).unwrap();
        if release_first {
            block_on(repl::poll(&a, &bucket));
        }
        block_on(repl::release(&b, &bucket)).unwrap();
        assert_eq!(state(&bucket).handoff, None);
        assert!(!b.is_writable());
        assert_ne!(block_on(repl::poll(&b, &bucket)).role, Role::Holder);
    }
}

#[test]
fn a_lost_publish_acknowledgement_before_takeover_is_not_divergence() {
    let (a, b, bucket) = pair();
    put(&a, "committed", "once");
    let fault = FaultBucket::new(bucket.clone(), Fault::LosePublishAcknowledgement);
    assert!(block_on(repl::poll(&a, &fault)).note.is_some());
    block_on(repl::override_lease(&b, &bucket)).unwrap();
    put(&b, "later", "from-b");
    block_on(repl::poll(&b, &bucket));
    assert!(matches!(
        block_on(repl::poll(&a, &bucket)).role,
        Role::Follower { .. }
    ));
    assert_eq!(a.unpublished(), 0);
    assert_eq!(got(&a, "later").as_deref(), Some("from-b"));
    assert_eq!(state(&bucket).seq, 2);
}

#[test]
fn explicit_recovery_preserves_a_readable_backup_of_the_branch() {
    let (a, _, bucket) = pair();
    let directory = std::env::temp_dir().join(format!("superapp-recovery-test-{}", a.device()));
    std::fs::create_dir_all(&directory).unwrap();
    let b = Store::open(Some(&directory.join("store.db")), &[]).unwrap();
    block_on(repl::poll(&b, &bucket));
    block_on(repl::release(&a, &bucket)).unwrap();
    block_on(repl::acquire(&b, &bucket)).unwrap();
    put(&b, "private-branch", "preserve me");
    block_on(repl::override_lease(&a, &bucket)).unwrap();
    assert!(matches!(
        block_on(repl::poll(&b, &bucket)).role,
        Role::Stranded { .. }
    ));
    assert!(matches!(
        block_on(repl::recover(&b, &bucket)).unwrap().role,
        Role::Follower { .. }
    ));
    let backups: Vec<_> = std::fs::read_dir(directory.join("sync-recovery"))
        .unwrap()
        .collect();
    assert_eq!(backups.len(), 1);
    let backup = Store::open(Some(&backups[0].as_ref().unwrap().path()), &[]).unwrap();
    let value: String = backup
        .conn()
        .query_row(
            "SELECT value FROM meta WHERE key='private-branch'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(value, "preserve me");
    let pending: i64 = backup
        .conn()
        .query_row(
            "SELECT count(*) FROM repl_log WHERE pub_seq IS NULL",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert!(pending > 0);
    assert_eq!(got(&b, "private-branch"), None);
    assert_eq!(state(&bucket).holder.as_deref(), Some(a.device().as_str()));
    drop(backup);
    drop(b);
    std::fs::remove_dir_all(directory).unwrap();
}
