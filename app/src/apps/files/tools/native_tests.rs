use super::*;
use kernel::app::{App, Apps, Env, Mode, Workers};
use kernel::caps::{DemoDisk, Disk, DiskFactory, Entry, FileId};
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::{atomic::{AtomicUsize, Ordering}, mpsc, Arc};
use std::time::{Duration, Instant};

struct HeldDisk {
    disk: DemoDisk,
    started: mpsc::Sender<()>,
    release: mpsc::Receiver<()>,
    writes: Arc<AtomicUsize>,
}

impl Disk for HeldDisk {
    fn list_dir(&mut self, p: &Path) -> Result<Vec<Entry>, String> { self.disk.list_dir(p) }
    fn stat(&mut self, p: &Path) -> Result<Option<Entry>, String> { self.disk.stat(p) }
    fn read_file(&mut self, p: &Path, max: usize) -> Result<Vec<u8>, String> { self.disk.read_file(p, max) }
    fn write_file(&mut self, p: &Path, bytes: &[u8]) -> Result<(), String> {
        self.disk.write_file(p, bytes)?;
        if self.writes.fetch_add(1, Ordering::SeqCst) == 0 {
            self.started.send(()).unwrap();
            self.release.recv_timeout(Duration::from_secs(5)).map_err(|error| error.to_string())?;
        }
        Ok(())
    }
    fn open_path(&mut self, p: &Path) -> Result<(), String> { self.disk.open_path(p) }
    fn make_dir(&mut self, p: &Path) -> Result<(), String> { self.disk.make_dir(p) }
    fn copy_path(&mut self, from: &Path, to: &Path) -> Result<(), String> { self.disk.copy_path(from, to) }
    fn move_path(&mut self, from: &Path, to: &Path) -> Result<(), String> { self.disk.move_path(from, to) }
    fn trash(&mut self, p: &Path) -> Result<PathBuf, String> { self.disk.trash(p) }
    fn file_id(&mut self, p: &Path) -> Result<Option<FileId>, String> { self.disk.file_id(p) }
}

static APPS: &[&dyn App] = &[&FILES];

fn finish(s: &mut Session, ready: impl Fn() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(5);
    while !ready() {
        s.settle();
        assert!(Instant::now() < deadline, "native completion reached the UI");
        std::thread::sleep(Duration::from_millis(1));
    }
}

#[test]
fn accepted_native_writes_run_off_ui_and_lease_loss_compensates_off_ui() {
    for lose_lease in [false, true] {
        let (started, observed) = mpsc::channel();
        let (release, held) = mpsc::channel();
        let writes = Arc::new(AtomicUsize::new(0));
        let mut env = Env::default();
        env.disk = Some(DiskFactory::shared(HeldDisk {
            disk: DemoDisk::new(env.clock.clone()), started, release: held, writes: writes.clone(),
        }));
        let apps = Apps::new(APPS);
        let store = kernel::store::Store::open(None, &apps.schemas()).unwrap();
        let world = Rc::new(apps.world(store, Mode::Fake, &env));
        let workers = Workers::inline(APPS, world.clone());
        let mut session = Session::new(apps, world, workers, Mode::Fake);
        session.store().attach_ui(|| {});
        let before = read_in(session.world(), "~/notes.md", MAX_REWRITE).unwrap();
        let head = session.history().head();
        let prepared = kernel::runtime::block_on(prepare_command(
            &json!({"path": "~/notes.md", "text": "replacement"}), Operation::Write,
        )(session.world())).unwrap();
        assert_eq!(writes.load(Ordering::SeqCst), 0, "preparation cannot mutate native state");
        let result = Rc::new(std::cell::RefCell::new(None));
        let received = result.clone();
        prepared.commit(&mut session, move |_, completed| *received.borrow_mut() = Some(completed));
        finish(&mut session, || observed.try_recv().is_ok());
        assert!(result.borrow().is_none(), "commit returned while native write was still running");
        if lose_lease { session.store().set_writable(false); }
        release.send(()).unwrap();
        finish(&mut session, || result.borrow().is_some());
        let outcome = result.take().unwrap();
        if lose_lease {
            assert!(outcome.unwrap_err().contains("given back"));
            assert_eq!(read_in(session.world(), "~/notes.md", MAX_REWRITE).unwrap(), before);
            assert_eq!(session.history().head(), head);
            assert_eq!(writes.load(Ordering::SeqCst), 2, "one accepted write and one compensation");
        } else {
            assert!(outcome.is_ok());
            assert_eq!(read_in(session.world(), "~/notes.md", MAX_REWRITE).unwrap(), b"replacement");
            assert!(session.history().head() > head);
            assert_eq!(writes.load(Ordering::SeqCst), 1, "completion never replays the write");
        }
        session.shutdown();
    }
}
