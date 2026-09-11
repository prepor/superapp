//! Recovery must replace the divergence screen while it runs, and stop
//! calling a restored baseline divergent if its remaining replay fails.
use super::{
    object::{Blob, Cas, MemBucket, Object, PutNew},
    Role,
};
use crate::{runtime::block_on, store::Store};
use std::sync::{
    atomic::{AtomicBool, AtomicUsize, Ordering},
    Arc,
};
use tokio::sync::{Notify, Semaphore};

fn put(store: &Store, value: &'static str) {
    store.write(move |tx| tx.execute(
        "INSERT INTO meta(key,value) VALUES('recovery-test',?1) \
         ON CONFLICT(key) DO UPDATE SET value=excluded.value", [value],
    ).map(|_| ())).unwrap();
}

fn diverged(path: &std::path::Path) -> (Store, Store, MemBucket) {
    let a = Store::open(Some(path), &[]).unwrap();
    let b = Store::open(None, &[]).unwrap();
    let bucket = MemBucket::new();
    assert_eq!(block_on(super::poll(&a, &bucket)).role, Role::Holder);
    block_on(super::poll(&b, &bucket));
    put(&a, "local branch");
    block_on(super::override_lease(&b, &bucket)).unwrap();
    put(&b, "canonical value");
    block_on(super::poll(&b, &bucket));
    assert!(matches!(block_on(super::poll(&a, &bucket)).role, Role::Stranded { .. }));
    (a, b, bucket)
}

struct HeldSnapshot {
    bucket: MemBucket,
    started: Notify,
    proceed: Semaphore,
    downloads: AtomicUsize,
    hold_state: AtomicBool,
}

#[async_trait::async_trait(?Send)]
impl Object for HeldSnapshot {
    async fn get(&self, key: &str) -> Result<Option<Blob>, String> {
        if key == "state" && self.hold_state.swap(false, Ordering::SeqCst) {
            self.started.notify_one();
            self.proceed.acquire().await.unwrap().forget();
        }
        if key.starts_with("snap/") {
            self.downloads.fetch_add(1, Ordering::SeqCst);
            self.started.notify_one();
            self.proceed.acquire().await.unwrap().forget();
        }
        self.bucket.get(key).await
    }
    async fn put_new(&self, key: &str, bytes: &[u8]) -> Result<PutNew, String> {
        self.bucket.put_new(key, bytes).await
    }
    async fn cas(&self, key: &str, bytes: &[u8], etag: &str) -> Result<Cas, String> {
        self.bucket.cas(key, bytes, etag).await
    }
}

#[test]
fn recovery_is_visible_immediately_and_repeated_clicks_do_not_restart_it() {
    let directory = tempfile::tempdir().unwrap();
    let (a, b, bucket) = diverged(&directory.path().join("store.db"));
    let held = Arc::new(HeldSnapshot {
        bucket,
        started: Notify::new(),
        proceed: Semaphore::new(0),
        downloads: AtomicUsize::new(0),
        hold_state: AtomicBool::new(false),
    });
    let updated = Arc::new(Notify::new());
    let notify = updated.clone();
    let driver = super::spawn(a.db(), held.clone(), move || notify.notify_one());
    block_on(async {
        tokio::time::timeout(std::time::Duration::from_secs(5), async {
            while !matches!(driver.status().role, Role::Stranded { .. }) {
                updated.notified().await;
            }
            driver.recover();
            assert_eq!(driver.status().role.word(), "recovering");
            assert_eq!(driver.status().role.locked_screen().1, None);
            assert!(!a.is_writable());
            held.started.notified().await;
            assert_eq!(driver.status().role.word(), "recovering");
            driver.recover();
            driver.recover();
            held.proceed.add_permits(3);
            while !matches!(driver.status().role, Role::Follower { .. }) {
                updated.notified().await;
            }
            // A later command is also a barrier for any duplicate recoveries.
            driver.release_wait().await;
            assert_eq!(held.downloads.load(Ordering::SeqCst), 1);
            assert_eq!(std::fs::read_dir(directory.path().join("sync-recovery")).unwrap().count(), 1);
            assert_eq!(a.unpublished(), 0);
            assert_eq!(a.materialized(), b.materialized());
            let value: String = a.conn().query_row(
                "SELECT value FROM meta WHERE key='recovery-test'", [], |row| row.get(0),
            ).unwrap();
            assert_eq!(value, "canonical value");
            assert!(!a.is_writable());
            driver.stop().await;
        }).await.unwrap();
    });
}

#[test]
fn a_superseded_queued_recovery_clears_its_busy_state() {
    let directory = tempfile::tempdir().unwrap();
    let (a, _, bucket) = diverged(&directory.path().join("store.db"));
    let held = Arc::new(HeldSnapshot {
        bucket,
        started: Notify::new(),
        proceed: Semaphore::new(0),
        downloads: AtomicUsize::new(0),
        hold_state: AtomicBool::new(true),
    });
    let updated = Arc::new(Notify::new());
    let notify = updated.clone();
    let driver = super::spawn(a.db(), held.clone(), move || notify.notify_one());
    block_on(async {
        tokio::time::timeout(std::time::Duration::from_secs(5), async {
            // Pause arrives before the first network pass lets recovery start.
            held.started.notified().await;
            driver.recover();
            assert_eq!(driver.status().role.word(), "recovering");
            driver.release();
            held.proceed.add_permits(1);
            driver.release_wait().await;
            assert!(matches!(driver.status().role, Role::Stranded { .. }));
            assert_eq!(held.downloads.load(Ordering::SeqCst), 0);
            assert!(!directory.path().join("sync-recovery").exists());

            // Cancelling the queued action must not suppress the next request.
            driver.recover();
            held.started.notified().await;
            held.proceed.add_permits(1);
            while !matches!(driver.status().role, Role::Follower { .. }) {
                updated.notified().await;
            }
            assert_eq!(held.downloads.load(Ordering::SeqCst), 1);
            driver.stop().await;
        }).await.unwrap();
    });
}

struct ReplayFailure {
    bucket: MemBucket,
    corrupt: bool,
}

#[async_trait::async_trait(?Send)]
impl Object for ReplayFailure {
    async fn get(&self, key: &str) -> Result<Option<Blob>, String> {
        if key.starts_with("log/") {
            if !self.corrupt {
                return Err("injected: replay download failed".into());
            }
            let mut blob = self.bucket.get(key).await?.unwrap();
            blob.bytes = vec![0];
            return Ok(Some(blob));
        }
        self.bucket.get(key).await
    }
    async fn put_new(&self, key: &str, bytes: &[u8]) -> Result<PutNew, String> {
        self.bucket.put_new(key, bytes).await
    }
    async fn cas(&self, key: &str, bytes: &[u8], etag: &str) -> Result<Cas, String> {
        self.bucket.cas(key, bytes, etag).await
    }
}

#[test]
fn failed_replay_after_snapshot_installation_is_no_longer_divergence() {
    for corrupt in [false, true] {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("store.db");
        let (a, b, bucket) = diverged(&path);
        let failure = ReplayFailure { bucket: bucket.clone(), corrupt };
        let error = block_on(super::recover(&a, &failure)).unwrap_err();
        assert_eq!(a.unpublished(), 0, "snapshot installation finished before replay failed");
        assert_eq!(a.materialized(), 0);
        let status = block_on(super::protocol::failed(&a, error));
        assert_eq!(status.role, if corrupt { Role::Fault } else { Role::Offline });
        assert!(status.note.is_some());
        assert!(!a.is_writable());
        drop(a);

        // Restart resumes replay from the installed baseline, without another
        // recovery action, backup, or snapshot download.
        let a = Store::open(Some(&path), &[]).unwrap();
        let status = block_on(super::poll(&a, &bucket));
        assert!(matches!(status.role, Role::Follower { .. }), "{status:?}");
        assert_eq!(a.materialized(), b.materialized());
        assert_eq!(std::fs::read_dir(directory.path().join("sync-recovery")).unwrap().count(), 1);
    }
}

#[test]
fn a_legacy_stranded_status_without_pending_changes_does_not_hide_an_outage() {
    let a = Store::open(None, &[]).unwrap();
    a.set_status("stranded", None).unwrap();
    let status = block_on(super::poll(&a, &super::r2::Broken("injected outage".into())));
    assert_eq!(status.role, Role::Offline);
    assert!(!a.is_writable());
}

#[test]
fn a_failed_recovery_before_installation_preserves_the_branch_and_recovery_action() {
    let directory = tempfile::tempdir().unwrap();
    let (a, _, _) = diverged(&directory.path().join("store.db"));
    let error = block_on(super::recover(&a, &super::r2::Broken("injected outage".into()))).unwrap_err();
    let status = block_on(super::protocol::failed(&a, error));
    assert!(matches!(status.role, Role::Stranded { .. }));
    assert_eq!(status.role.locked_screen().1, Some("recover"));
    assert_eq!(a.unpublished(), 1);
    assert!(!a.is_writable());
}
