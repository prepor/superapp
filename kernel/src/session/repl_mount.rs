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
use crate::caps::{Secrets, WriteFile};
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

impl Repl {
    /// The final release owns the mount. No UI reader or session borrow crosses
    /// into this service, and the driver cannot acquire again after release.
    pub(super) async fn shutdown(mut self, db: Arc<crate::store::Db>) {
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

    /// Asks to take the lease — from a free one, or by override from a live
    /// holder. Which of the two it is is the driver's to decide.
    pub fn repl_acquire(&mut self) {
        match &self.repl {
            Some(Repl::Tasks(d)) => d.acquire(),
            Some(Repl::Manual { bucket }) => {
                let b = bucket.clone();
                let s = match crate::runtime::block_on(repl::acquire(&self.store, &*b)) { Ok(s) => s, Err(_) => crate::runtime::block_on(repl::poll(&self.store, &*b)) };
                self.apply_repl(s);
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
        let (tx, rx) = tokio::sync::oneshot::channel();
        self.repl = Some(Repl::Connecting(rx));
        crate::runtime::spawn_local(move || async move {
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
                world.run(&WriteFile { path: &r2::config_path(&dir), bytes: &r2::config_bytes(&url, &key_id) })?;
                world.caps(|caps| match caps.get::<dyn Secrets>() {
                    Some(secrets) => r2::open(&url, Some(&dir), secrets),
                    None => Err("this world has no Secrets".to_string()),
                })
            }).await.unwrap_or_else(|e| Err(e.to_string()));
            let mut done = Connected { driver: None, manual: None, error: None };
            match opened {
                Ok(bucket) => {
                    match previous {
                        Some(Repl::Tasks(driver)) => { driver.release_wait().await; driver.stop().await; }
                        Some(Repl::Manual { bucket }) => {
                            if let Ok(store) = crate::store::Store::with_db(db.clone()) { let _ = repl::release(&store, &*bucket).await; }
                        }
                        _ => {}
                    }
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
