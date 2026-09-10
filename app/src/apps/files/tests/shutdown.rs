//! The native runner's final handoff belongs to the same closing session.
use super::*;
use kernel::app::{Apps, Env, Mode, Workers};
use kernel::caps::{DemoDisk, DiskFactory, FileId};
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::{atomic::{AtomicUsize, Ordering}, mpsc, Arc};
use std::time::{Duration, Instant};

struct HeldDirectory {
    disk: DemoDisk,
    entered: mpsc::Sender<()>,
    release: mpsc::Receiver<()>,
    created: Arc<AtomicUsize>,
}
impl Disk for HeldDirectory {
    fn list_dir(&mut self, path: &Path) -> Result<Vec<Entry>, String> { self.disk.list_dir(path) }
    fn stat(&mut self, path: &Path) -> Result<Option<Entry>, String> { self.disk.stat(path) }
    fn read_file(&mut self, path: &Path, max: usize) -> Result<Vec<u8>, String> { self.disk.read_file(path, max) }
    fn write_file(&mut self, path: &Path, bytes: &[u8]) -> Result<(), String> { self.disk.write_file(path, bytes) }
    fn open_path(&mut self, path: &Path) -> Result<(), String> { self.disk.open_path(path) }
    fn make_dir(&mut self, path: &Path) -> Result<(), String> {
        self.disk.make_dir(path)?;
        if self.created.fetch_add(1, Ordering::SeqCst) == 0 {
            self.entered.send(()).unwrap();
            self.release.recv_timeout(Duration::from_secs(5)).map_err(|error| error.to_string())?;
        }
        Ok(())
    }
    fn copy_path(&mut self, from: &Path, to: &Path) -> Result<(), String> { self.disk.copy_path(from, to) }
    fn move_path(&mut self, from: &Path, to: &Path) -> Result<(), String> { self.disk.move_path(from, to) }
    fn trash(&mut self, path: &Path) -> Result<PathBuf, String> { self.disk.trash(path) }
    fn file_id(&mut self, path: &Path) -> Result<Option<FileId>, String> { self.disk.file_id(path) }
}

#[test]
fn closing_drains_accepted_file_runs_and_lands_claims_or_compensation() {
    let _alone = alone();
    for (lose_lease, stopped) in [(false, false), (true, false), (false, true)] {
        let (entered, started) = mpsc::channel();
        let (release, held) = mpsc::channel();
        let created = Arc::new(AtomicUsize::new(0));
        let mut env = Env::default();
        env.disk = Some(DiskFactory::shared(HeldDirectory {
            disk: DemoDisk::new(env.clock.clone()), entered, release: held, created: created.clone(),
        }));
        let apps = Apps::new(APPS);
        let store = Store::open(None, &apps.schemas()).unwrap();
        let world = Rc::new(apps.world(store, Mode::Fake, &env));
        let (notify, woke) = mpsc::channel();
        let worker_wake = notify.clone();
        let workers = Workers::async_io(APPS, world.store().clone(), Mode::Fake, env,
            move || { let _ = worker_wake.send(()); });
        let mut session = Session::new(apps, world, workers, Mode::Fake);
        session.store().attach_ui(move || { let _ = notify.send(()); });
        let before = session.history().head();
        let db = super::super::run::whose(session.store());
        for path in ["~/closing first", "~/closing second"] {
            FILES.start(&session, Task::MakeDir { path: path.into() }, 0, Dir::id(HOME));
        }
        started.recv_timeout(Duration::from_secs(5)).unwrap();
        assert_eq!(created.load(Ordering::SeqCst), 1, "one path is accepted and held, the next run remains queued");
        if stopped { FILES.stop(db, FILES.drawing(db).0); }
        let began = Instant::now();
        session.begin_shutdown();
        assert!(!session.poll_shutdown());
        assert!(began.elapsed() < Duration::from_millis(200), "closing does not wait on the native path");
        let deadline = Instant::now() + Duration::from_secs(5);
        while session.workers().any() {
            session.store().poll_external();
            assert!(!session.poll_shutdown(), "the accepted native path is still held");
            if !session.workers().any() { break; }
            woke.recv_timeout(deadline.checked_duration_since(Instant::now()).expect("worker retirement requested"))
                .expect("shutdown progress wakes the window");
        }
        assert_eq!(created.load(Ordering::SeqCst), 1, "the path completes only after retirement begins");
        if lose_lease { session.store().set_writable(false); }
        release.send(()).unwrap();
        session.shutdown();
        assert!(!FILES.busy(db), "retirement clears the accepted run and queue");
        assert!(FILES.take_landed(db).is_empty(), "the closing session must consume the worker's final handoff");
        for path in ["~/closing first", "~/closing second"] {
            let present = !(lose_lease || stopped && path == "~/closing second");
            assert_eq!(stat_in(session.world(), path).is_some(), present,
                "each completed path is retained with a claim or compensated before closing");
        }
        if lose_lease {
            assert_eq!(session.history().head(), before, "compensated native changes do not leave undo claims");
        } else {
            assert!(session.history().head() > before, "completed native runs retain their undo claims");
        }
        assert_eq!(created.load(Ordering::SeqCst), if stopped || lose_lease { 1 } else { 2 },
            "normal closing drains queued runs; cancellation or revoked authority never starts the second path");
    }
}
