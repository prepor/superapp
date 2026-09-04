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
    /// A driver thread of its own, over its own reader on the one writer.
    Threads(repl::Driver),
    /// Inline passes on the caller's thread, driven by the frame loop, so a
    /// scripted `wait` advances a handoff exactly the way it advances a
    /// background pass.
    Manual { bucket: Arc<dyn Object> },
}

/// Which of the two a session mounts. The shell decides: threads in
/// production, inline under virtual time.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReplMount {
    Threads,
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
            ReplMount::Threads => {
                Repl::Threads(repl::spawn(self.store.db(), bucket, move || notify()))
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
        let status = match &self.repl {
            Some(Repl::Threads(d)) => d.status(),
            Some(Repl::Manual { bucket }) => {
                let b = bucket.clone();
                repl::poll(&self.store, &*b)
            }
            None => return ReplChange::default(),
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
                if let Err(e) = self.apps.seed(&self.store) {
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
            Some(Repl::Threads(d)) => d.acquire(),
            Some(Repl::Manual { bucket }) => {
                let b = bucket.clone();
                let s = repl::acquire(&self.store, &*b)
                    .unwrap_or_else(|_| repl::poll(&self.store, &*b));
                self.apply_repl(s);
            }
            None => {}
        }
    }

    /// Publishes promptly after an action (or nudges the driver to).
    pub fn repl_kick(&mut self) {
        match &self.repl {
            Some(Repl::Threads(d)) => d.kick(),
            Some(Repl::Manual { bucket }) => {
                let b = bucket.clone();
                let s = repl::poll(&self.store, &*b);
                self.apply_repl(s);
            }
            None => {}
        }
    }

    /// Hands the lease back (best effort).
    pub fn repl_release(&mut self) {
        match &self.repl {
            Some(Repl::Threads(d)) => d.release(),
            Some(Repl::Manual { bucket }) => {
                let b = bucket.clone();
                let s = repl::release(&self.store, &*b)
                    .unwrap_or_else(|_| repl::poll(&self.store, &*b));
                self.apply_repl(s);
            }
            None => {}
        }
    }

    /// Releases synchronously on the calling thread — the last chance to
    /// hand back, on close and on sleep.
    pub fn repl_release_blocking(&self) {
        match &self.repl {
            Some(Repl::Threads(d)) => d.release_blocking(),
            Some(Repl::Manual { bucket }) => {
                let b = bucket.clone();
                let _ = repl::release(&self.store, &*b);
            }
            None => {}
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
    pub fn connect_bucket(
        &mut self,
        url: &str,
        key_id: &str,
        secret: &str,
    ) -> Result<String, String> {
        let dir = self
            .store
            .dir()
            .ok_or("no store file — device sync needs one")?
            .to_path_buf();
        if url.is_empty() {
            return Err("the bucket url is required".into());
        }
        if url.starts_with("https://") && key_id.is_empty() {
            return Err("an https bucket needs an access key id".into());
        }
        if !secret.is_empty() {
            if key_id.is_empty() {
                return Err("a secret needs the key id it belongs to".into());
            }
            // The secret goes first, because the check below has to be able
            // to find it — and a key in the keychain that nothing points at
            // is inert, which is not true of a written-down endpoint.
            self.world
                .run(&repl::BucketSecret { key_id, secret })
                .map_err(|e| format!("storing the bucket secret failed: {e}"))?;
        }
        // Check *before* anything is written down: a typo that reaches the
        // `bucket` file is what the next launch will read, and the launch
        // after that.
        self.world.caps(|c| match c.get::<dyn Secrets>() {
            Some(s) => r2::check(url, Some(&dir), key_id, s),
            None => Err("this world has no Secrets".to_string()),
        })?;

        self.world
            .run(&WriteFile {
                path: &r2::config_path(&dir),
                bytes: &r2::config_bytes(url, key_id),
            })
            .map_err(|e| format!("writing the bucket file failed: {e}"))?;

        // Hand the lease back before the old driver goes — the bucket it
        // holds it in may not be the one we are moving to — and then *wait*
        // for it. A dropped handle leaves a thread that is still a device.
        if let Some(Repl::Threads(d)) = self.repl.take() {
            d.release_blocking();
            d.stop();
        }
        self.repl = None;
        self.seeded = false;
        self.start_repl(url);
        let host = url
            .trim_start_matches("https://")
            .trim_start_matches("http://")
            .split('/')
            .next()
            .unwrap_or(url);
        Ok(format!("device sync: connecting to {host}"))
    }
}
