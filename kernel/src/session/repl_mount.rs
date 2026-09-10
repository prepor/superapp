//! The session's device-sync half: how the lease driver is mounted, what it
//! reports, and the one form that points a device at a bucket.
//!
//! Device sync is not an app, so this is not `App::outside`: it replicates
//! the store itself, every app's tables included, and the session holds it
//! because the session is what the write gate, the locked screen and every
//! action go through.

use std::path::Path;
use std::sync::Arc;

use super::Session;
use crate::caps::{Disk, Secrets};
use crate::effect::{Ctx, Effect};
use crate::repl::{self, object::Object, r2};

/// How device sync runs for this session.
pub(super) enum Repl {
    /// A local asynchronous driver, with its own reader on the one writer.
    Tasks(repl::Driver),
    /// Inline passes on the caller's thread, driven by the frame loop, so a
    /// scripted `wait` advances a handoff exactly the way it advances a
    /// background pass.
    Manual { bucket: Arc<dyn Object> },
    Connecting(tokio::sync::oneshot::Receiver<Connected>),
}

pub(super) struct Connected {
    driver: Option<repl::Driver>,
    manual: Option<Arc<dyn Object>>,
    error: Option<String>,
}

/// Only the device's sync configuration is exempt from shared writer
/// authority. Ordinary file writes must continue to use the gated effect.
struct BucketConfig<'a> {
    dir: &'a Path,
    url: &'a str,
    key_id: &'a str,
}

impl Effect for BucketConfig<'_> {
    const KIND: &'static str = "bucket_config";
    type Reply = ();
    fn describe(&self) -> String { "save this device's sync configuration".into() }
    fn writes(&self) -> bool { true }
    fn requires_writer(&self) -> bool { false }
    fn perform(&self, cx: &mut Ctx<'_>) -> Result<(), String> {
        cx.cap::<dyn Disk>()?.write_file(&r2::config_path(self.dir), &r2::config_bytes(self.url, self.key_id))
    }
}

fn released_for_reconnect(status: repl::Status, same_bucket: bool) -> Result<(), String> {
    match status.role {
        repl::Role::Free | repl::Role::Follower { .. } => Ok(()),
        repl::Role::Offline if same_bucket => Ok(()),
        _ => Err(status.note.unwrap_or_else(|| status.role.line())),
    }
}

impl Repl {
    /// The final release owns the mount. No UI reader or session borrow crosses
    /// into this service, and the driver cannot acquire again after release.
    pub(super) async fn shutdown(mut self, db: Arc<crate::store::Db>) {
        db.request_release();
        loop {
            match self {
                Self::Connecting(receive) => {
                    let Ok(done) = receive.await else { return; };
                    let Some(next) = done.driver.map(Self::Tasks)
                        .or_else(|| done.manual.map(|bucket| Self::Manual { bucket })) else { return; };
                    self = next;
                }
                Self::Tasks(driver) => {
                    driver.release_wait().await;
                    driver.stop().await;
                    return;
                }
                Self::Manual { bucket } => {
                    match crate::store::Store::with_db(db) {
                        Ok(store) => { let _ = repl::release(&store, &*bucket).await; }
                        Err(error) => eprintln!("shutdown: cannot open device sync reader: {error}"),
                    }
                    return;
                }
            }
        }
    }
}

/// Which of the two a session mounts. The shell decides: tasks in
/// production, inline under virtual time.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReplMount {
    Tasks,
    Inline,
}

/// What one sync pass moved, and so what the shell owes the screen: a role
/// change redraws the world; a new failure only needs saying.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ReplChange {
    pub role: bool,
    pub note: bool,
}

impl Session {
    /// How the lease driver is to be mounted, and what wakes the shell after
    /// a pass. Said once at boot, before [`Session::start_repl`], and kept so
    /// [`Session::connect_bucket`] can restart onto another bucket.
    pub fn mount_repl(&mut self, mount: ReplMount, notify: impl Fn() + Send + Sync + 'static) {
        self.repl_mount = Some((mount, Arc::new(notify)));
    }

    /// Points this session at a bucket and starts the lease driver.
    ///
    /// A bucket that cannot be opened at all — no credentials, a malformed
    /// endpoint — still gets a driver, over a bucket that answers every verb
    /// with the reason. Returning without one would leave a device that had
    /// *already joined a lineage* with no driver and no locked screen, and
    /// the store opens writable: a follower would come back as a writer
    /// outside the lease. This way the ordinary path holds — the role falls
    /// to `Offline`, the gate stays shut, and the reason reaches the screen.
    ///
    /// The gate is shut until the first pass answers: until then this device
    /// does not know whether the lineage already has a writer, and an edit
    /// made in that window is one an install is about to discard.
    pub fn start_repl(&mut self, url: &str) {
        if self.repl_mount.is_none() {
            eprintln!("session: device sync was not mounted; the bucket is ignored");
            return;
        }
        if let (Some((ReplMount::Tasks, notify)), Some(factory)) = (self.repl_mount.clone(), self.world.factory()) {
            let url = url.to_owned();
            let dir = self.store.dir().map(Path::to_path_buf);
            let db = self.store.db();
            self.store.set_writable(false);
            self.lease = repl::Status { device: self.store.device(), ..repl::Status::default() };
            let (tx, rx) = tokio::sync::oneshot::channel();
            self.repl = Some(Repl::Connecting(rx));
            crate::runtime::spawn_local(move || async move {
                let opened = crate::runtime::spawn_blocking(move || {
                    let world = factory.build().map_err(|e| e.to_string())?;
                    world.caps(|caps| match caps.get::<dyn Secrets>() {
                        Some(secrets) => r2::open(&url, dir.as_deref(), secrets),
                        None => Err("this world has no Secrets".to_string()),
                    })
                }).await.unwrap_or_else(|e| Err(e.to_string()));
                let bucket = opened.unwrap_or_else(|e| Arc::new(r2::Broken(e)));
                let wake = notify.clone();
                let driver = repl::spawn(db, bucket, move || wake());
                let _ = tx.send(Connected { driver: Some(driver), manual: None, error: None });
                notify();
            });
            return;
        }
        let dir = self.store.dir().map(Path::to_path_buf);
        let opened = self.world.caps(|c| match c.get::<dyn Secrets>() {
            Some(s) => r2::open(url, dir.as_deref(), s),
            None => Err("this world has no Secrets".to_string()),
        });
        let bucket = opened.unwrap_or_else(|e| {
            eprintln!("superapp: device sync cannot start — {e}");
            Arc::new(r2::Broken(e)) as Arc<dyn Object>
        });
        self.start_repl_with(bucket);
    }

    /// The same, over a bucket the caller already has — a test, or a demo
    /// driving the passes by hand.
    pub fn start_repl_with(&mut self, bucket: Arc<dyn Object>) {
        let Some((mount, notify)) = self.repl_mount.clone() else {
            eprintln!("session: device sync was not mounted; the bucket is ignored");
            return;
        };
        self.store.set_writable(false);
        self.lease = repl::Status {
            device: self.store.device(),
            ..repl::Status::default()
        };
        self.repl = Some(match mount {
            ReplMount::Inline => Repl::Manual { bucket },
            ReplMount::Tasks => {
                Repl::Tasks(repl::spawn(self.store.db(), bucket, move || notify()))
            }
        });
    }

    /// The lease status the last pass reported, or `None` when no bucket is
    /// configured — which is what the locked screen asks before it draws.
    #[must_use]
    pub fn lease(&self) -> Option<&repl::Status> {
        self.repl.as_ref().map(|_| &self.lease)
    }

    /// Runs (or reads) one sync pass and reconciles the result. Called on
    /// every driver signal and, under virtual time, from the frame loop.
    pub fn repl_poll(&mut self) -> ReplChange {
        if let Some(Repl::Connecting(rx)) = &mut self.repl {
            match rx.try_recv() {
                Ok(done) => {
                    self.repl = done.driver.map(Repl::Tasks).or_else(|| done.manual.map(|bucket| Repl::Manual { bucket }));
                    if let Some(error) = done.error { self.notify(error, true); }
                    else { self.seeded = false; }
                }
                Err(tokio::sync::oneshot::error::TryRecvError::Empty) => return ReplChange::default(),
                Err(tokio::sync::oneshot::error::TryRecvError::Closed) => {
                    self.repl = None;
                    self.notify("device sync connection task failed", true);
                    return ReplChange::default();
                }
            }
        }
        let status = match &self.repl {
            Some(Repl::Tasks(d)) => d.status(),
            Some(Repl::Manual { bucket }) => crate::runtime::block_on(repl::poll(&self.store, &**bucket)),
            Some(Repl::Connecting(_)) | None => return ReplChange::default(),
        };
        self.apply_repl(status)
    }

    /// Caches the reported status, seeds the demo world the first time this
    /// device holds, and answers what moved.
    fn apply_repl(&mut self, status: repl::Status) -> ReplChange {
        if status == self.lease {
            return ReplChange::default();
        }
        let changed = ReplChange {
            role: status.role != self.lease.role,
            note: status.note != self.lease.note,
        };
        self.lease = status;
        if self.lease.role == repl::Role::Holder && !self.seeded {
            self.seeded = true;
            // Only into a store nobody has ever booted: one that has been
            // used keeps whatever it was left as, and one that installed a
            // snapshot has the holder's world already.
            if matches!(self.store.load_wm(), Ok(None)) {
                if let Err(e) = self.apps.seed(&self.store, self.seed_mode) {
                    eprintln!("store: seeding the demo world failed: {e}");
                }
            }
        }
        changed
    }

    /// Acquires a free lease or requests a handoff from its current holder.
    pub fn repl_acquire(&mut self) {
        match &self.repl {
            Some(Repl::Tasks(d)) => d.acquire(),
            Some(Repl::Manual { bucket }) => {
                let b = bucket.clone();
                match crate::runtime::block_on(repl::acquire(&self.store, &*b)) {
                    Ok(s) => { self.apply_repl(s); }
                    Err(why) => self.notify(why.to_string(), true),
                }
            }
            Some(Repl::Connecting(_)) | None => {}
        }
    }

    /// Explicitly interrupts a holder that cannot complete a handoff.
    pub fn repl_override(&mut self) {
        match &self.repl {
            Some(Repl::Tasks(d)) => d.override_lease(),
            Some(Repl::Manual { bucket }) => {
                let b = bucket.clone();
                match crate::runtime::block_on(repl::override_lease(&self.store, &*b)) {
                    Ok(s) => { self.apply_repl(s); }
                    Err(why) => self.notify(why.to_string(), true),
                }
            }
            Some(Repl::Connecting(_)) | None => {}
        }
    }

    /// Saves a recovery backup and follows the canonical shared history.
    pub fn repl_recover(&mut self) {
        match &self.repl {
            Some(Repl::Tasks(d)) => d.recover(),
            Some(Repl::Manual { bucket }) => {
                let b = bucket.clone();
                match crate::runtime::block_on(repl::recover(&self.store, &*b)) {
                    Ok(s) => { self.apply_repl(s); }
                    Err(why) => self.notify(why.to_string(), true),
                }
            }
            Some(Repl::Connecting(_)) | None => {}
        }
    }

    /// Publishes promptly after an action (or nudges the driver to).
    pub fn repl_kick(&mut self) {
        match &self.repl {
            Some(Repl::Tasks(d)) => d.kick(),
            Some(Repl::Manual { bucket }) => {
                let b = bucket.clone();
                let s = crate::runtime::block_on(repl::poll(&self.store, &*b));
                self.apply_repl(s);
            }
            Some(Repl::Connecting(_)) | None => {}
        }
    }

    /// Hands the lease back (best effort).
    pub fn repl_release(&mut self) {
        // Pause can arrive before credential lookup returns a driver, or
        // while reconnect owns the previous driver off the UI thread.
        // The eventual driver shares this intent and must not reopen us.
        // A bucketless session has no lease lifecycle to suspend.
        if self.repl.is_some() { self.store.db().request_release(); }
        match &self.repl {
            Some(Repl::Tasks(d)) => d.release(),
            Some(Repl::Manual { bucket }) => {
                let b = bucket.clone();
                let s = match crate::runtime::block_on(repl::release(&self.store, &*b)) { Ok(s) => s, Err(_) => crate::runtime::block_on(repl::poll(&self.store, &*b)) };
                self.apply_repl(s);
            }
            Some(Repl::Connecting(_)) | None => {}
        }
    }

    /// Completes a pending connection and hands the lease back on shutdown.
    pub async fn repl_release_wait(&mut self) {
        if self.repl.is_some() { self.store.db().request_release(); }
        if let Some(Repl::Connecting(rx)) = &mut self.repl {
            if let Ok(done) = rx.await {
                self.repl = done.driver.map(Repl::Tasks).or_else(|| done.manual.map(|bucket| Repl::Manual { bucket }));
            }
        }
        match &self.repl {
            Some(Repl::Tasks(d)) => d.release_wait().await,
            Some(Repl::Manual { bucket }) => {
                let b = bucket.clone();
                let _ = repl::release(&self.store, &*b).await;
            }
            Some(Repl::Connecting(_)) | None => {}
        }
    }

    /// Points this device at a bucket: the secret to the platform's secret
    /// store, the URL and key id to the `bucket` file beside the store, and
    /// the driver restarted onto them. Answers what to say, either way.
    ///
    /// This is the road a device with no shell and no cable has — a phone is
    /// still a device that has to be given a key.
    ///
    /// # Errors
    ///
    /// If there is no store file, the form is incomplete, the credentials
    /// cannot be found, or the file cannot be written.
    pub fn connect_bucket(&mut self, url: &str, key_id: &str, secret: &str) -> Result<String, String> {
        let dir = self.store.dir().ok_or("no store file — device sync needs one")?.to_owned();
        if url.is_empty() { return Err("the bucket url is required".into()); }
        if url.starts_with("https://") && key_id.is_empty() { return Err("an https bucket needs an access key id".into()); }
        if !secret.is_empty() && key_id.is_empty() { return Err("a secret needs the key id it belongs to".into()); }
        if matches!(self.repl, Some(Repl::Connecting(_))) { return Err("device sync is already connecting".into()); }
        let factory = self.world.factory().ok_or("this world cannot open a background connection")?;
        let (mount, notify) = self.repl_mount.clone().ok_or("device sync was not mounted")?;
        let (url, key_id, secret) = (url.to_owned(), key_id.to_owned(), secret.to_owned());
        let db = self.store.db();
        let previous = self.repl.take();
        if previous.is_some() { db.request_release(); }
        let (tx, rx) = tokio::sync::oneshot::channel();
        self.repl = Some(Repl::Connecting(rx));
        crate::runtime::spawn_local(move || async move {
            let (configured_dir, candidate) = (dir.clone(), url.clone());
            let same_bucket = crate::runtime::spawn_blocking(move || {
                r2::url_from_file(Some(&configured_dir)).as_deref() == Some(candidate.trim())
            }).await.unwrap_or(false);
            // A reconnect cannot change credentials used by other services
            // until the old mount has joined them and handed back ownership.
            // An unreachable mount may repair its exact configured endpoint
            // after draining; it cannot switch histories without releasing.
            // Keep the paused old driver available if the new config fails.
            let released = match &previous {
                Some(Repl::Tasks(driver)) => {
                    driver.release_wait().await;
                    released_for_reconnect(driver.status(), same_bucket)
                }
                Some(Repl::Manual { bucket }) => match crate::store::Store::with_db(db.clone()) {
                    Ok(store) => match repl::release(&store, &**bucket).await {
                        Ok(status) => released_for_reconnect(status, same_bucket),
                        Err(repl::SyncError::Transport(_)) if same_bucket => Ok(()),
                        Err(error) => Err(error.to_string()),
                    },
                    Err(error) => Err(error.to_string()),
                },
                _ => Ok(()),
            };
            let released = if released.is_ok() && previous.is_some() {
                db.authority().quiesce().await;
                db.flush_async().await.map_err(|error| error.to_string())
            } else { released };
            if let Err(error) = released {
                let mut done = Connected { driver: None, manual: None, error: Some(error) };
                match previous {
                    Some(Repl::Tasks(driver)) => done.driver = Some(driver),
                    Some(Repl::Manual { bucket }) => done.manual = Some(bucket),
                    _ => {}
                }
                let _ = tx.send(done);
                notify();
                return;
            }
            // Keychain, config files and world construction are native work.
            let opened = crate::runtime::spawn_blocking(move || {
                let world = factory.build().map_err(|e| e.to_string())?;
                if !secret.is_empty() {
                    world.run(&repl::BucketSecret { key_id: &key_id, secret: &secret })?;
                }
                world.caps(|caps| match caps.get::<dyn Secrets>() {
                    Some(secrets) => r2::check(&url, Some(&dir), &key_id, secrets),
                    None => Err("this world has no Secrets".to_string()),
                })?;
                world.run(&BucketConfig { dir: &dir, url: &url, key_id: &key_id })?;
                world.caps(|caps| match caps.get::<dyn Secrets>() {
                    Some(secrets) => r2::open(&url, Some(&dir), secrets),
                    None => Err("this world has no Secrets".to_string()),
                })
            }).await.unwrap_or_else(|e| Err(e.to_string()));
            let mut done = Connected { driver: None, manual: None, error: None };
            match opened {
                Ok(bucket) => {
                    if let Some(Repl::Tasks(driver)) = previous { driver.stop().await; }
                    match mount {
                        ReplMount::Tasks => {
                            let wake = notify.clone();
                            done.driver = Some(repl::spawn(db, bucket, move || wake()));
                        }
                        ReplMount::Inline => done.manual = Some(bucket),
                    }
                }
                Err(error) => {
                    done.error = Some(error);
                    match previous {
                        Some(Repl::Tasks(driver)) => done.driver = Some(driver),
                        Some(Repl::Manual { bucket }) => done.manual = Some(bucket),
                        _ => {}
                    }
                }
            }
            let _ = tx.send(done);
            notify();
        });
        Ok("device sync: connecting".into())
    }
}

#[cfg(test)]
mod lifecycle_tests {
    use super::*;
    use crate::app::{Apps, Env, Mode, Workers};
    use crate::caps::{DemoDisk, DiskFactory, MemSecrets, WriteFile};
    use crate::repl::object::MemBucket;
    use crate::runtime::block_on;
    use crate::store::Store;
    use std::rc::Rc;
    use std::time::Duration;

    fn file_session() -> (tempfile::TempDir, Session, DiskFactory, MemSecrets) {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(Some(&dir.path().join("store.db")), &[]).unwrap();
        let mut env = Env::default();
        let disk = DiskFactory::shared(DemoDisk::new(env.clock.clone()));
        env.disk = Some(disk.clone());
        let secrets = env.secrets.clone();
        let apps = Apps::new(&[]);
        let world = Rc::new(apps.world(store, Mode::Fake, &env));
        let workers = Workers::none(world.store().clone());
        let session = Session::new(apps, world, workers, Mode::Fake);
        (dir, session, disk, secrets)
    }

    #[test]
    fn pause_during_connection_survives_the_delayed_driver_mount() {
        let mut session = Session::fake(&[]);
        let bucket = Arc::new(MemBucket::new());
        assert_eq!(block_on(repl::poll(&session.store, &*bucket)).role, repl::Role::Holder);
        let generation = session.store.db().authority().state().generation;
        let (send, receive) = tokio::sync::oneshot::channel();
        session.repl = Some(Repl::Connecting(receive));

        // Android pauses while native credential lookup still owns the mount.
        session.repl_release();
        assert!(!session.writable());
        assert!(session.store.db().release_requested());

        let updated = Arc::new(tokio::sync::Notify::new());
        let wake = updated.clone();
        let driver = repl::spawn(session.store.db(), bucket.clone(), move || wake.notify_one());
        block_on(async {
            tokio::time::timeout(Duration::from_secs(5), async {
                while driver.status().role != repl::Role::Free {
                    assert_ne!(driver.status().role, repl::Role::Holder);
                    updated.notified().await;
                }
            }).await.unwrap();
        });
        assert!(send.send(Connected { driver: Some(driver), manual: None, error: None }).is_ok());
        session.repl_poll();
        assert_eq!(session.lease().unwrap().role, repl::Role::Free);
        assert!(!session.writable());
        assert_eq!(session.store.db().authority().state().generation, generation,
            "the late connection must never grant a replacement generation");
        let resume: bool = session.store.conn().query_row("SELECT resume FROM repl WHERE id=1", [], |row| row.get(0)).unwrap();
        assert!(!resume, "the eventual mount persists the paused intent");
        if let Some(Repl::Tasks(driver)) = session.repl.take() { block_on(driver.stop()); }
    }

    #[test]
    fn pause_without_a_bucket_keeps_local_mode_writable() {
        let mut session = Session::fake(&[]);
        session.repl_release();
        assert!(session.writable());
        assert!(!session.store.db().release_requested());
    }

    #[test]
    fn only_bucket_configuration_can_be_written_from_a_closed_display_world() {
        let session = Session::fake(&[]);
        session.store.set_writable(false);
        let dir = Path::new("/device-state");
        session.world.run(&BucketConfig { dir, url: "http://bucket", key_id: "key" }).unwrap();
        assert_eq!(session.world.with_cap::<dyn Disk, _>(|disk|
            disk.read_file(&r2::config_path(dir), 1024)).unwrap().unwrap(),
            r2::config_bytes("http://bucket", "key"));
        assert_eq!(session.world.run(&WriteFile { path: Path::new("/ordinary.txt"), bytes: b"no" }),
            Err(crate::effect::SUSPENDED.into()));
        assert!(!session.writable());
    }

    #[test]
    fn reconnect_waits_for_old_native_work_before_changing_local_configuration() {
        let (_dir, mut session, disk, mut secrets) = file_session();
        // Store resolves directory aliases for its process lock. DemoDisk
        // keys exact paths, so observe the same canonical device directory.
        let config = r2::config_path(session.store.dir().unwrap());
        // The replacement stays inline, so this test never contacts a server.
        session.mount_repl(ReplMount::Inline, || {});
        let bucket = Arc::new(MemBucket::new());
        assert_eq!(block_on(repl::poll(&session.store, &*bucket)).role, repl::Role::Holder);
        let updated = Arc::new(tokio::sync::Notify::new());
        let wake = updated.clone();
        let driver = repl::spawn(session.store.db(), bucket, move || wake.notify_one());
        block_on(async {
            tokio::time::timeout(Duration::from_secs(5), async {
                while driver.status().role != repl::Role::Holder { updated.notified().await; }
            }).await.unwrap();
        });
        session.repl = Some(Repl::Tasks(driver));
        let native = session.store.db().authority().enter().unwrap();
        session.connect_bucket("http://replacement", "replacement-key", "replacement-secret").unwrap();
        assert!(!session.writable(), "reconnect closes ordinary admission immediately");
        // Wait until the old driver reaches release, which must still be
        // waiting for this admitted native operation. This avoids a timing
        // assertion against an unscheduled connection task.
        block_on(async {
            tokio::time::timeout(Duration::from_secs(5), async {
                loop {
                    let resume: bool = session.store.conn().query_row("SELECT resume FROM repl WHERE id=1", [], |row| row.get(0)).unwrap();
                    if !resume { break; }
                    tokio::task::yield_now().await;
                }
            }).await.unwrap();
        });
        assert_eq!(secrets.get(&r2::secret_key("replacement-key")), None);
        assert!(disk.make().read_file(&config, 1024).is_err());
        drop(native);
        let Some(Repl::Connecting(receive)) = session.repl.take() else { panic!("connection pending"); };
        let connected = block_on(async { tokio::time::timeout(Duration::from_secs(5), receive).await.unwrap().unwrap() });
        assert!(connected.error.is_none(), "{:?}", connected.error);
        assert!(connected.manual.is_some());
        assert_eq!(secrets.get(&r2::secret_key("replacement-key")).as_deref(), Some("replacement-secret"));
        assert_eq!(disk.make().read_file(&config, 1024).unwrap(),
            r2::config_bytes("http://replacement", "replacement-key"));
        assert!(!session.writable(), "saving local configuration does not grant writer authority");
    }

    #[test]
    fn failed_release_allows_only_exact_configured_endpoint_credential_repair() {
        for same_bucket in [false, true] {
            let (_dir, mut session, disk, mut secrets) = file_session();
            let config = r2::config_path(session.store.dir().unwrap());
            std::fs::write(&config, r2::config_bytes("http://original", "old-key")).unwrap();
            session.mount_repl(ReplMount::Inline, || {});
            session.repl = Some(Repl::Manual { bucket: Arc::new(r2::Broken("expired credentials".into())) });
            let url = if same_bucket { "http://original" } else { "http://different-history" };
            session.connect_bucket(url, "new-key", "new-secret").unwrap();
            let Some(Repl::Connecting(receive)) = session.repl.take() else { panic!("connection pending"); };
            let connected = block_on(async { tokio::time::timeout(Duration::from_secs(5), receive).await.unwrap().unwrap() });
            assert_eq!(connected.error.is_none(), same_bucket, "{:?}", connected.error);
            assert_eq!(secrets.get(&r2::secret_key("new-key")).as_deref(), same_bucket.then_some("new-secret"));
            let saved = disk.make().read_file(&config, 1024);
            if same_bucket { assert_eq!(saved.unwrap(), r2::config_bytes(url, "new-key")); }
            else { assert!(saved.is_err(), "a failed release leaves the original configuration untouched"); }
            assert!(!session.writable());
        }
    }
}
