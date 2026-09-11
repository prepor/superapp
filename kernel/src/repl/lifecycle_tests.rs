//! Boundaries that span transport failures, process restart, and ownership.
//! These exercise real store capture and replay against an isolated bucket.
use super::{
    object::{self, Blob, Cas, MemBucket, Object, PutNew},
    Role,
};
use crate::{runtime::block_on, store::Store};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

fn store() -> Store {
    Store::open(None, &[]).unwrap()
}
fn pair() -> (Store, Store, MemBucket) {
    let (a, b, bucket) = (store(), store(), MemBucket::new());
    assert_eq!(block_on(super::poll(&a, &bucket)).role, Role::Holder);
    assert!(matches!(
        block_on(super::poll(&b, &bucket)).role,
        Role::Follower { .. }
    ));
    (a, b, bucket)
}
fn put(store: &Store, value: &'static str) {
    store.write(move |tx| tx.execute(
        "INSERT INTO meta(key,value) VALUES('lifecycle',?1) ON CONFLICT(key) DO UPDATE SET value=excluded.value",
        [value],
    ).map(|_| ())).unwrap();
}
fn got(store: &Store) -> Option<String> {
    store
        .conn()
        .query_row("SELECT value FROM meta WHERE key='lifecycle'", [], |r| {
            r.get(0)
        })
        .ok()
}

#[test]
fn an_unreachable_bucket_closes_a_previously_confirmed_writer() {
    let (a, _, bucket) = pair();
    put(&a, "accepted before outage");
    let unavailable = super::r2::Broken("injected network failure".into());
    for _ in 0..2 {
        let status = block_on(super::poll(&a, &unavailable));
        assert_eq!(status.role, Role::Offline);
        assert!(
            !a.is_writable(),
            "cached ownership is not present write authority"
        );
        assert!(a
            .write(|tx| tx
                .execute("INSERT INTO meta VALUES('late','rejected')", [])
                .map(|_| ()))
            .is_err());
        assert_eq!(a.unpublished(), 1);
    }
    assert_eq!(block_on(super::poll(&a, &bucket)).role, Role::Holder);
    assert_eq!(a.unpublished(), 0);
    assert_eq!(got(&a).as_deref(), Some("accepted before outage"));
}

/// The gate is shut from boot until the first pass answers, and an answer
/// of "unreachable" opens it for a device that has never joined: there is no
/// writer it could be fenced from, and the bucket form has to stay reachable
/// for the url that may be the reason. The same failure keeps a joined
/// device closed — the holder above, the follower in `mod.rs`'s tests.
#[test]
fn an_unreachable_bucket_leaves_a_device_that_never_joined_local() {
    let (_, _, bucket) = pair();
    let fresh = store();
    fresh.set_writable(false);
    let unavailable = super::r2::Broken("injected network failure".into());
    let status = block_on(super::poll(&fresh, &unavailable));
    assert_eq!(status.role, Role::Detached);
    assert!(fresh.is_writable(), "nothing to be fenced from before a first join");
    assert_eq!(status.note.as_deref(), Some("injected network failure"));
    assert_eq!(fresh.epoch(), 0, "local until the bucket answers");

    // What it writes meanwhile is local, and the join replaces it: the
    // lineage never sees a frame captured before the install.
    put(&fresh, "typed before the join");
    assert!(matches!(block_on(super::poll(&fresh, &bucket)).role, Role::Follower { .. }));
    assert!(!fresh.is_writable());
    assert_eq!(got(&fresh), None, "the install is the baseline, not the local rows");
    assert_eq!(fresh.unpublished(), 0);
}

/// A pause, a reconnect and a shutdown all ask the driver to release. A
/// device that never joined has no lease to release, and the asking must
/// not be what locks it: the intent is spent on the pass that finds this
/// out, as a pause without a bucket is.
#[test]
fn a_release_asked_of_a_device_that_never_joined_is_vacuous() {
    let fresh = store();
    let unavailable = super::r2::Broken("injected network failure".into());
    assert!(block_on(super::release(&fresh, &unavailable)).is_err());
    assert!(fresh.db().release_requested());
    assert!(!fresh.is_writable(), "the attempt closed admission");
    let status = block_on(super::poll(&fresh, &unavailable));
    assert_eq!(status.role, Role::Detached);
    assert!(fresh.is_writable());
    assert!(!fresh.db().release_requested());
}

#[test]
fn a_failed_release_retries_release_without_resuming_background_writes() {
    let (a, b, bucket) = pair();
    put(&a, "accepted before pause");
    let unavailable = super::r2::Broken("injected network failure".into());
    assert!(block_on(super::release(&a, &unavailable)).is_err());
    assert!(!a.is_writable());
    assert_eq!(a.unpublished(), 1);

    // Connectivity returning is not a resume command. Finish the requested
    // pause and publish what was accepted, then remain stopped.
    assert_eq!(block_on(super::poll(&a, &bucket)).role, Role::Free);
    assert!(!a.is_writable());
    assert_eq!(a.unpublished(), 0);
    assert_eq!(block_on(super::poll(&a, &bucket)).role, Role::Free);
    assert_eq!(
        block_on(super::acquire(&b, &bucket)).unwrap().role,
        Role::Holder
    );
    assert_eq!(got(&b).as_deref(), Some("accepted before pause"));
}

#[test]
fn failed_native_cleanup_refuses_publish_release_and_reacquisition_without_hanging() {
    let (a, _, bucket) = pair();
    put(&a, "accepted before native cleanup failed");
    let before = block_on(object::read_state(&bucket)).unwrap().unwrap().0;
    a.db().authority().poison("native shutdown failed; restart Superapp");
    block_on(async {
        let released = tokio::time::timeout(std::time::Duration::from_secs(5), super::release(&a, &bucket))
            .await.expect("a cleanup fault returns an error instead of hanging");
        assert!(released.unwrap_err().to_string().contains("restart Superapp"));
        assert!(a.db().grant_async().await.is_err());
        assert_eq!(super::poll(&a, &bucket).await.role, Role::Fault);
        assert_eq!(object::read_state(&bucket).await.unwrap().unwrap().0, before);
    });
    assert_eq!(a.unpublished(), 1, "unconfirmed native cleanup cannot cross a handoff boundary");
    assert!(!a.is_writable());
}

struct LostReleaseResponse {
    bucket: MemBucket,
    fired: AtomicBool,
}
#[async_trait::async_trait(?Send)]
impl Object for LostReleaseResponse {
    async fn get(&self, key: &str) -> Result<Option<Blob>, String> {
        self.bucket.get(key).await
    }
    async fn put_new(&self, key: &str, bytes: &[u8]) -> Result<PutNew, String> {
        self.bucket.put_new(key, bytes).await
    }
    async fn cas(&self, key: &str, bytes: &[u8], etag: &str) -> Result<Cas, String> {
        let result = self.bucket.cas(key, bytes, etag).await?;
        let releasing = key == object::STATE_KEY
            && serde_json::from_slice::<object::State>(bytes).is_ok_and(|s| s.released);
        if releasing && matches!(result, Cas::Ok(_)) && !self.fired.swap(true, Ordering::SeqCst) {
            return Err("injected lost release acknowledgement".into());
        }
        Ok(result)
    }
}

#[test]
fn a_lost_release_acknowledgement_does_not_reacquire_or_diverge() {
    let (a, b, bucket) = pair();
    put(&a, "saved");
    let fault = LostReleaseResponse {
        bucket: bucket.clone(),
        fired: AtomicBool::new(false),
    };
    assert!(block_on(super::release(&a, &fault)).is_err());
    assert!(!a.is_writable());
    assert_eq!(a.unpublished(), 0);
    assert_eq!(
        block_on(super::acquire(&b, &bucket)).unwrap().role,
        Role::Holder
    );
    assert!(matches!(
        block_on(super::poll(&a, &bucket)).role,
        Role::Follower { .. }
    ));
    assert!(!a.is_writable());
    assert_eq!(got(&a), got(&b));
}

#[test]
fn restarting_a_follower_preserves_another_devices_inflight_jobs() {
    let a = store();
    let directory =
        std::env::temp_dir().join(format!("superapp-follower-lifecycle-{}", a.device()));
    std::fs::create_dir(&directory).unwrap();
    let path = directory.join("store.db");
    let b = Store::open(Some(&path), &[]).unwrap();
    let bucket = MemBucket::new();
    block_on(super::poll(&a, &bucket));
    block_on(super::poll(&b, &bucket));
    a.write(|tx| {
        tx.execute(
            "INSERT INTO effect(id,kind,payload,status,idempotent,created,updated,not_before)
        VALUES(1,'test','{}','processing',1,0,0,0)",
            [],
        )
        .map(|_| ())
    })
    .unwrap();
    block_on(super::poll(&a, &bucket));
    block_on(super::poll(&b, &bucket));
    drop(b);
    let b = Store::open(Some(&path), &[]).unwrap();
    let restarted: String = b
        .conn()
        .query_row("SELECT status FROM effect WHERE id=1", [], |r| r.get(0))
        .unwrap();
    assert_eq!(
        restarted, "processing",
        "opening a follower may not rewrite the holder's shared job"
    );
    assert_eq!(b.unpublished(), 0);
    a.write(|tx| {
        tx.execute("UPDATE effect SET status='done' WHERE id=1", [])
            .map(|_| ())
    })
    .unwrap();
    block_on(super::poll(&a, &bucket));
    let status = block_on(super::poll(&b, &bucket));
    assert!(matches!(status.role, Role::Follower { .. }), "{status:?}");
    assert_eq!(
        b.conn()
            .query_row("SELECT status FROM effect WHERE id=1", [], |r| r
                .get::<_, String>(0))
            .unwrap(),
        "done"
    );
    drop(b);
    std::fs::remove_dir_all(directory).unwrap();
}

#[test]
fn replay_conflicts_are_not_reported_as_bucket_outages() {
    let (a, b, bucket) = pair();
    put(&a, "canonical");
    block_on(super::poll(&a, &bucket));
    block_on(super::poll(&b, &bucket));
    // Reproduce an old startup hook corrupting a follower baseline, without
    // letting its local change become part of the canonical history.
    block_on(b.db().raw_async(|conn| {
        conn.execute(
            "UPDATE meta SET value='corrupted follower' WHERE key='lifecycle'",
            [],
        )
        .map(|_| ())
    }))
    .unwrap();
    let have = b.materialized();
    put(&a, "later canonical");
    block_on(super::poll(&a, &bucket));
    let status = block_on(super::poll(&b, &bucket));
    assert_eq!(status.role.word(), "fault", "{status:?}");
    assert!(
        status
            .note
            .as_deref()
            .is_some_and(|note| note.contains("replay")),
        "{status:?}"
    );
    assert!(!b.is_writable());
    assert_eq!(b.materialized(), have);
    assert_eq!(got(&b).as_deref(), Some("corrupted follower"));
    assert_eq!(b.unpublished(), 0);
}

#[test]
fn a_joined_device_must_not_bootstrap_over_a_missing_history() {
    let (a, _, _) = pair();
    put(&a, "unpublished local work");
    let epoch = a.epoch();
    let empty = MemBucket::new();
    let status = block_on(super::poll(&a, &empty));
    assert_eq!(status.role.word(), "fault", "{status:?}");
    assert!(!a.is_writable());
    assert_eq!(a.epoch(), epoch);
    assert_eq!(a.unpublished(), 1);
    assert_eq!(got(&a).as_deref(), Some("unpublished local work"));
    assert!(block_on(object::read_state(&empty)).unwrap().is_none());
}

#[test]
fn equal_counters_do_not_join_unrelated_histories() {
    let (a, b, original) = pair();
    put(&a, "original history");
    block_on(super::poll(&a, &original));
    block_on(super::poll(&b, &original));
    let other = store();
    let unrelated = MemBucket::new();
    block_on(super::poll(&other, &unrelated));
    put(&other, "unrelated history");
    block_on(super::poll(&other, &unrelated));
    assert_eq!(
        (b.epoch(), b.materialized()),
        (other.epoch(), other.materialized())
    );
    let status = block_on(super::poll(&b, &unrelated));
    assert_eq!(status.role.word(), "fault", "{status:?}");
    assert_eq!(got(&b).as_deref(), Some("original history"));
    assert!(!b.is_writable());
}

struct SnapshotUnavailable(MemBucket);
#[async_trait::async_trait(?Send)]
impl Object for SnapshotUnavailable {
    async fn get(&self, key: &str) -> Result<Option<Blob>, String> {
        if key.starts_with("snap/") {
            return Err("injected failed snapshot download".into());
        }
        self.0.get(key).await
    }
    async fn put_new(&self, key: &str, bytes: &[u8]) -> Result<PutNew, String> {
        self.0.put_new(key, bytes).await
    }
    async fn cas(&self, key: &str, bytes: &[u8], etag: &str) -> Result<Cas, String> {
        self.0.cas(key, bytes, etag).await
    }
}

#[test]
fn failed_recovery_cannot_bind_old_rows_to_a_new_history() {
    let a = store();
    let directory =
        std::env::temp_dir().join(format!("superapp-recovery-lifecycle-{}", a.device()));
    std::fs::create_dir(&directory).unwrap();
    let b = Store::open(Some(&directory.join("store.db")), &[]).unwrap();
    let original = MemBucket::new();
    block_on(super::poll(&a, &original));
    put(&a, "original history");
    block_on(super::poll(&a, &original));
    block_on(super::poll(&b, &original));
    let before = (b.epoch(), b.materialized());
    let other = store();
    let unrelated = MemBucket::new();
    block_on(super::poll(&other, &unrelated));
    put(&other, "unrelated history");
    block_on(super::poll(&other, &unrelated));
    let failure = SnapshotUnavailable(unrelated.clone());
    assert!(block_on(super::recover(&b, &failure)).is_err());
    assert_eq!((b.epoch(), b.materialized()), before);
    assert_eq!(got(&b).as_deref(), Some("original history"));
    let status = block_on(super::poll(&b, &unrelated));
    assert_eq!(
        status.role.word(),
        "fault",
        "failed recovery silently trusted an unrelated watermark: {status:?}"
    );
    assert_eq!(got(&b).as_deref(), Some("original history"));
    drop(b);
    std::fs::remove_dir_all(directory).unwrap();
}

struct RequestBeforePublish {
    bucket: MemBucket,
    target: String,
    fired: AtomicBool,
}
#[async_trait::async_trait(?Send)]
impl Object for RequestBeforePublish {
    async fn get(&self, key: &str) -> Result<Option<Blob>, String> {
        self.bucket.get(key).await
    }
    async fn put_new(&self, key: &str, bytes: &[u8]) -> Result<PutNew, String> {
        self.bucket.put_new(key, bytes).await
    }
    async fn cas(&self, key: &str, bytes: &[u8], etag: &str) -> Result<Cas, String> {
        if key == object::STATE_KEY && !self.fired.swap(true, Ordering::SeqCst) {
            let (mut current, current_etag) = object::read_state(&self.bucket).await?.unwrap();
            current.handoff = Some(self.target.clone());
            assert!(matches!(
                self.bucket
                    .cas(key, &object::encode_state(&current), &current_etag)
                    .await?,
                Cas::Ok(_)
            ));
        }
        self.bucket.cas(key, bytes, etag).await
    }
}

#[test]
fn a_handoff_request_racing_publication_is_a_transition_not_an_outage() {
    let (a, b, bucket) = pair();
    put(&a, "accepted before handoff");
    let race = RequestBeforePublish {
        bucket: bucket.clone(),
        target: b.device(),
        fired: AtomicBool::new(false),
    };
    let status = block_on(super::poll(&a, &race));
    assert_ne!(
        status.role,
        Role::Offline,
        "a changed CAS is not a transport failure: {status:?}"
    );
    assert_ne!(
        status.role.word(),
        "fault",
        "an ordinary handoff is not corrupt history: {status:?}"
    );
    // A bounded retry may finish the handoff now or during the next pass.
    let status = block_on(super::poll(&a, &bucket));
    assert_eq!(status.role, Role::Free, "{status:?}");
    assert_eq!(a.unpublished(), 0);
    assert!(!a.is_writable());
    assert_eq!(block_on(super::poll(&b, &bucket)).role, Role::Holder);
    assert_eq!(got(&b).as_deref(), Some("accepted before handoff"));
    assert_eq!(a.materialized(), b.materialized());
}

struct HeldPublication {
    bucket: MemBucket,
    upload_started: tokio::sync::Notify,
    upload_continue: tokio::sync::Semaphore,
    upload_returned: AtomicBool,
    release_started: tokio::sync::Notify,
    release_continue: tokio::sync::Semaphore,
    release_held: AtomicBool,
}
#[async_trait::async_trait(?Send)]
impl Object for HeldPublication {
    async fn get(&self, key: &str) -> Result<Option<Blob>, String> {
        if key == object::STATE_KEY
            && self.upload_returned.load(Ordering::SeqCst)
            && !self.release_held.swap(true, Ordering::SeqCst)
        {
            self.release_started.notify_one();
            self.release_continue.acquire().await.unwrap().forget();
        }
        self.bucket.get(key).await
    }
    async fn put_new(&self, key: &str, bytes: &[u8]) -> Result<PutNew, String> {
        if key.starts_with("log/") && !self.upload_returned.load(Ordering::SeqCst) {
            self.upload_started.notify_one();
            self.upload_continue.acquire().await.unwrap().forget();
            self.upload_returned.store(true, Ordering::SeqCst);
        }
        self.bucket.put_new(key, bytes).await
    }
    async fn cas(&self, key: &str, bytes: &[u8], etag: &str) -> Result<Cas, String> {
        self.bucket.cas(key, bytes, etag).await
    }
}

#[test]
fn driver_release_closes_immediately_during_upload_and_old_status_cannot_regrant() {
    driver_release_during_upload(false);
}

#[test]
fn an_acquisition_queued_before_pause_cannot_cancel_the_newer_pause() {
    driver_release_during_upload(true);
}

fn driver_release_during_upload(queue_acquisition: bool) {
    let (a, b, bucket) = pair();
    let held = Arc::new(HeldPublication {
        bucket: bucket.clone(),
        upload_started: tokio::sync::Notify::new(),
        upload_continue: tokio::sync::Semaphore::new(0),
        upload_returned: AtomicBool::new(false),
        release_started: tokio::sync::Notify::new(),
        release_continue: tokio::sync::Semaphore::new(0),
        release_held: AtomicBool::new(false),
    });
    let updated = Arc::new(tokio::sync::Notify::new());
    let notify = updated.clone();
    let driver = super::spawn(a.db(), held.clone(), move || notify.notify_one());
    // A newly mounted driver first confirms ownership with admission closed.
    // Arm the upload race only after that initial confirmation has completed.
    block_on(async {
        tokio::time::timeout(std::time::Duration::from_secs(5), async {
            while driver.status().role != Role::Holder {
                updated.notified().await;
            }
        })
        .await
        .unwrap();
    });
    put(&a, "accepted before backgrounding");
    driver.kick();
    block_on(async {
        let timeout = std::time::Duration::from_secs(5);
        tokio::time::timeout(timeout, held.upload_started.notified())
            .await
            .unwrap();
        assert!(a.is_writable());
        // Simulate Android Pause while the network pass is waiting on R2.
        // This must not need the driver queue to make progress first.
        if queue_acquisition {
            driver.acquire();
        }
        driver.release();
        assert!(!a.is_writable());
        assert!(a
            .submit_write(|tx| tx
                .execute("INSERT INTO meta VALUES('too-late','rejected')", [])
                .map(|_| ()))
            .is_err());
        held.upload_continue.add_permits(1);
        // The old publication completed and reported status. Hold the later
        // release request so the test can inspect exactly that boundary.
        tokio::time::timeout(timeout, held.release_started.notified())
            .await
            .unwrap();
        assert_eq!(driver.status().role, Role::Syncing);
        assert!(
            a.db().release_requested(),
            "an old queued acquisition cannot clear a newer pause intent"
        );
        assert!(
            !a.is_writable(),
            "a successful in-flight pass cannot undo a synchronous release request"
        );
        assert_eq!(a.unpublished(), 0);
        held.release_continue.add_permits(1);
        tokio::time::timeout(timeout, driver.release_wait())
            .await
            .unwrap();
        assert_eq!(driver.status().role, Role::Free);
        assert!(!a.is_writable());
        driver.stop().await;
    });
    assert_eq!(
        block_on(super::acquire(&b, &bucket)).unwrap().role,
        Role::Holder
    );
    assert_eq!(got(&b).as_deref(), Some("accepted before backgrounding"));
    assert_eq!(a.materialized(), b.materialized());
}

#[test]
fn release_cannot_publish_into_an_unrelated_prefix_owned_by_the_same_device() {
    let a = store();
    let original = MemBucket::new();
    block_on(super::poll(&a, &original));
    put(&a, "original baseline");
    block_on(super::poll(&a, &original));
    put(&a, "unpublished original history");
    // A previous prefix can legitimately name the same install as its holder.
    // Device ownership alone therefore cannot prove the baseline identity.
    let previous = store();
    let device = a.device();
    block_on(previous.db().raw_async(move |conn| {
        conn.execute("UPDATE repl SET device=?1 WHERE id=1", [device])
            .map(|_| ())
    }))
    .unwrap();
    put(&previous, "unrelated baseline");
    let unrelated = MemBucket::new();
    block_on(super::poll(&previous, &unrelated));
    let before = block_on(object::read_state(&unrelated)).unwrap().unwrap().0;
    assert!(
        block_on(super::release(&a, &unrelated)).is_err(),
        "release must check ancestry before publishing"
    );
    let after = block_on(object::read_state(&unrelated)).unwrap().unwrap().0;
    assert_eq!(after, before);
    assert_eq!(a.unpublished(), 1);
    assert_eq!(got(&a).as_deref(), Some("unpublished original history"));
    assert!(!a.is_writable());
}

#[test]
fn release_cannot_publish_frames_under_an_incompatible_schema_header() {
    let (a, _, bucket) = pair();
    put(&a, "pending incompatible frame");
    let (mut changed, etag) = block_on(object::read_state(&bucket)).unwrap().unwrap();
    changed.schema += 1;
    changed.snapshot.schema = changed.schema;
    assert!(matches!(
        block_on(bucket.cas(object::STATE_KEY, &object::encode_state(&changed), &etag)).unwrap(),
        Cas::Ok(_)
    ));
    assert!(
        block_on(super::release(&a, &bucket)).is_err(),
        "release must validate the same schema as acquisition"
    );
    let after = block_on(object::read_state(&bucket)).unwrap().unwrap().0;
    assert_eq!(after, changed);
    assert_eq!(a.unpublished(), 1);
    assert!(!a.is_writable());
}
