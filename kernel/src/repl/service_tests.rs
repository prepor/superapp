//! Integration of actual asynchronous drivers, worker supervisors, and stores.
//! No UI polling participates in handing service ownership between devices.
use super::{
    object::{Blob, Cas, MemBucket, Object, PutNew},
    Role,
};
use crate::{
    app::{App, Env, Mode, Retirement, Wake, Worker, Workers},
    effect::{Job, World},
    store::Store,
};
use std::{
    any::Any,
    rc::Rc,
    sync::{
        atomic::{AtomicUsize, Ordering},
        Arc,
    },
    time::Duration,
};

#[derive(Default)]
struct Counters {
    live: AtomicUsize,
    passes: AtomicUsize,
    starts: AtomicUsize,
}
struct ProviderApp(Arc<Counters>);
impl App for ProviderApp {
    fn id(&self) -> &'static str {
        "handoff-provider"
    }
    fn kinds(&self) -> &'static [&'static dyn crate::panel::PanelKind] {
        &[]
    }
    fn as_any(&self) -> &dyn Any {
        self
    }
    fn workers(&self, _: &Store) -> Vec<Box<dyn Worker>> {
        vec![Box::new(Provider {
            counters: self.0.clone(),
            started: false,
            retirement: Retirement::default(),
        })]
    }
}
struct Provider {
    counters: Arc<Counters>,
    started: bool,
    retirement: Retirement,
}
#[async_trait::async_trait(?Send)]
impl Worker for Provider {
    fn name(&self) -> String {
        "provider".into()
    }
    fn claims(&self, _: &Job) -> bool {
        false
    }
    fn retiring(&mut self, retirement: Retirement) {
        self.retirement = retirement;
    }
    async fn pass(&mut self, world: &World) -> Wake {
        if !self.started {
            assert_eq!(
                self.counters.live.fetch_add(1, Ordering::SeqCst),
                0,
                "native sessions must not overlap across devices"
            );
            self.counters.starts.fetch_add(1, Ordering::SeqCst);
            self.started = true;
        }
        if !self.retirement.requested() {
            let result = world.store().write_async(|tx| tx.execute(
                "INSERT INTO meta(key,value) VALUES('provider-counter',1) ON CONFLICT(key) DO UPDATE SET value=value+1", []).map(|_| ())).await;
            if let Err(error) = result {
                assert!(crate::store::is_suspended(&error), "{error}");
            } else {
                self.counters.passes.fetch_add(1, Ordering::SeqCst);
            }
        }
        Wake::After(Duration::from_millis(1))
    }
    async fn shutdown(&mut self, _: &World) {
        // Native shutdown remains alive after admission closes.
        tokio::time::sleep(Duration::from_millis(2)).await;
        if self.started {
            assert_eq!(self.counters.live.fetch_sub(1, Ordering::SeqCst), 1);
        }
    }
}
struct FastBucket(MemBucket);
#[async_trait::async_trait(?Send)]
impl Object for FastBucket {
    async fn get(&self, key: &str) -> Result<Option<Blob>, String> {
        self.0.get(key).await
    }
    async fn put_new(&self, key: &str, body: &[u8]) -> Result<PutNew, String> {
        self.0.put_new(key, body).await
    }
    async fn cas(&self, key: &str, body: &[u8], etag: &str) -> Result<Cas, String> {
        self.0.cas(key, body, etag).await
    }
    fn poll_every(&self) -> Duration {
        Duration::from_millis(2)
    }
}
async fn until(stage: &str, ready: impl Fn() -> bool, detail: impl Fn() -> String) {
    tokio::time::timeout(Duration::from_secs(5), async {
        while !ready() {
            tokio::time::sleep(Duration::from_millis(1)).await;
        }
    })
    .await
    .unwrap_or_else(|_| panic!("{stage}: {}", detail()));
}

#[test]
fn repeated_handoffs_join_native_services_and_converge_without_ui_ticks() {
    // Match the deployed store's WAL concurrency. Shared-cache memory readers
    // take table locks that can reject otherwise valid provider writes.
    struct Directory(std::path::PathBuf);
    impl Drop for Directory {
        fn drop(&mut self) { let _ = std::fs::remove_dir_all(&self.0); }
    }
    let directory = Directory(std::env::temp_dir().join(format!("superapp-service-handoff-{}-{}",
        std::process::id(), std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos())));
    std::fs::create_dir(&directory.0).unwrap();
    let counters = Arc::new(Counters::default());
    let app = Box::leak(Box::new(ProviderApp(counters.clone())));
    let apps: &'static [&'static dyn App] = Box::leak(Box::new([app as &dyn App]));
    let a = Rc::new(Store::open(Some(&directory.0.join("phone.db")), &[]).unwrap());
    let b = Rc::new(Store::open(Some(&directory.0.join("desktop.db")), &[]).unwrap());
    let bucket = Arc::new(FastBucket(MemBucket::new()));
    crate::runtime::block_on(async {
        assert_eq!(super::poll(&a, &*bucket).await.role, Role::Holder);
        assert!(matches!(
            super::poll(&b, &*bucket).await.role,
            Role::Follower { .. }
        ));
    });
    let wa = Workers::async_io(apps, a.clone(), Mode::Fake, Env::default(), || {});
    let wb = Workers::async_io(apps, b.clone(), Mode::Fake, Env::default(), || {});
    let da = super::spawn(a.db(), bucket.clone(), || {});
    let db = super::spawn(b.db(), bucket.clone(), || {});
    wa.kick_all();
    wb.kick_all();
    crate::runtime::block_on(async {
      let detail = || format!("A={:?}; B={:?}; live={} passes={} starts={}; workers A={:?}, B={:?}; authority A={:?}, B={:?}",
        da.status(), db.status(), counters.live.load(Ordering::SeqCst), counters.passes.load(Ordering::SeqCst),
        counters.starts.load(Ordering::SeqCst), wa.names(), wb.names(), a.db().authority().state(), b.db().authority().state());
        until("initial writer", || da.status().role == Role::Holder && counters.live.load(Ordering::SeqCst) == 1, &detail)
            .await;
        for turn in 0..20 {
            let before = counters.passes.load(Ordering::SeqCst);
            until("provider progress", || counters.passes.load(Ordering::SeqCst) >= before + 3, &detail).await;
            let (requester, former, old) = if turn % 2 == 0 {
                (&db, &da, &a)
            } else {
                (&da, &db, &b)
            };
            requester.acquire();
            until("requester acquisition", || requester.status().role == Role::Holder, &detail).await;
            until("former holder follows", || matches!(former.status().role, Role::Follower { .. }), &detail).await;
            assert_eq!(
                old.unpublished(),
                0,
                "former holder drained every accepted database write"
            );
            assert!(!old.is_writable());
        }
        da.release_wait().await;
        db.release_wait().await;
        until("final convergence", || a.materialized() == b.materialized(), &detail).await;
        assert_eq!(counters.live.load(Ordering::SeqCst), 0);
        assert!(counters.starts.load(Ordering::SeqCst) >= 20);
        let count = |store: &Store| {
            store
                .conn()
                .query_row(
                    "SELECT value FROM meta WHERE key='provider-counter'",
                    [],
                    |r| r.get::<_, i64>(0),
                )
                .unwrap()
        };
        assert_eq!(count(&a), count(&b));
        assert_eq!(count(&a) as usize, counters.passes.load(Ordering::SeqCst));
        da.stop().await;
        db.stop().await;
        wa.shutdown().await;
        wb.shutdown().await;
    });
}
