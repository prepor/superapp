//! One task serializes all protocol passes and UI commands.
use super::{
    object::Object,
    poll,
    protocol::{acquire_requested, failed, release_requested},
    recover, Role, Status,
};
use crate::store::{Db, Store};
use std::sync::{
    atomic::{AtomicBool, AtomicU64, Ordering},
    Arc, Mutex,
};
use tokio::sync::{mpsc, oneshot, watch};

// -- the driver ---------------------------------------------------------------

/// Commands are serialized with the current pass, including shutdown release.
enum Cmd {
    Stop,
    Kick,
    Acquire(u64),
    Override(u64),
    Recover(u64, RecoveryPending),
    Release(u64, Option<oneshot::Sender<()>>),
}

/// A recovery stays visible from enqueue through completion. Owning the guard
/// in the command also clears it when a newer intent supersedes that command.
struct RecoveryPending(Arc<AtomicBool>);
impl Drop for RecoveryPending {
    fn drop(&mut self) {
        self.0.store(false, Ordering::SeqCst);
    }
}

/// A local Tokio task owns one reader and serializes all lease operations.
/// The status channel keeps the UI's read synchronous without shared locks.
pub struct Driver {
    db: Arc<Db>,
    cmd: mpsc::UnboundedSender<Cmd>,
    status: watch::Receiver<Status>,
    done: Option<oneshot::Receiver<()>>,
    latest_intent: Arc<AtomicU64>,
    enqueue_intent: Mutex<()>,
    recovering: Arc<AtomicBool>,
}

impl Driver {
    #[must_use]
    pub fn status(&self) -> Status {
        let mut status = self.status.borrow().clone();
        if self.recovering.load(Ordering::SeqCst) {
            status.role = Role::Recovering;
            status.note = None;
        }
        status
    }
    pub fn kick(&self) {
        let _ = self.cmd.send(Cmd::Kick);
    }
    pub fn acquire(&self) {
        self.intent(true, Cmd::Acquire);
    }
    pub fn override_lease(&self) {
        self.intent(true, Cmd::Override);
    }
    pub fn recover(&self) {
        if !self.recovering.swap(true, Ordering::SeqCst) {
            let pending = RecoveryPending(self.recovering.clone());
            self.intent(false, |ticket| Cmd::Recover(ticket, pending));
        }
    }
    pub fn release(&self) {
        self.intent(false, |ticket| Cmd::Release(ticket, None));
    }

    /// Stamp commands when requested, not when the network loop reaches them.
    /// A later pause seals authority now and invalidates older queued acquires.
    fn intent(&self, resume: bool, command: impl FnOnce(u64) -> Cmd) -> bool {
        let _enqueue = self.enqueue_intent.lock().expect("sync intent");
        let ticket = self.latest_intent.fetch_add(1, Ordering::SeqCst) + 1;
        if resume {
            self.db.request_acquire();
        } else {
            self.db.request_release();
        }
        self.cmd.send(command(ticket)).is_ok()
    }

    /// Waits for the in-flight pass, then publishes and hands back the lease.
    pub async fn release_wait(&self) {
        let (tx, rx) = oneshot::channel();
        if self.intent(false, |ticket| Cmd::Release(ticket, Some(tx))) {
            let _ = rx.await;
        }
    }

    /// No replacement driver may begin until this task has finished.
    pub async fn stop(mut self) {
        self.intent(false, |_| Cmd::Stop);
        if let Some(done) = self.done.take() {
            let _ = done.await;
        }
    }
}

impl Drop for Driver {
    fn drop(&mut self) {
        self.intent(false, |_| Cmd::Stop);
    }
}

/// A failed or cancelled driver cannot leave services running without a
/// protocol owner. Native retirement is signalled by the authority itself.
struct DriverLifetime(Arc<Db>);
impl Drop for DriverLifetime {
    fn drop(&mut self) {
        self.0.request_release();
    }
}

#[must_use]
pub fn spawn(db: Arc<Db>, bucket: Arc<dyn Object>, notify: impl Fn() + Send + 'static) -> Driver {
    db.set_writable(false);
    let driver_db = db.clone();
    let latest_intent = Arc::new(AtomicU64::new(0));
    let current_intent = latest_intent.clone();
    let (cmd, mut rx) = mpsc::unbounded_channel();
    let (report, status) = watch::channel(Status::default());
    let done = crate::runtime::spawn_local(move || async move {
        let _lifetime = DriverLifetime(db.clone());
        let Ok(store) = Store::with_db(db) else {
            return;
        };
        let mut said: Option<String> = None;
        let mut publish_status = |s: Status| {
            if s.note != said {
                if let Some(why) = &s.note {
                    eprintln!("repl: {why}");
                }
                said = s.note.clone();
            }
            report.send_replace(s);
            notify();
        };
        publish_status(poll(&store, &*bucket).await);
        let every = bucket.poll_every();
        loop {
            let cmd = tokio::select! {
                cmd = rx.recv() => match cmd { Some(cmd) => cmd, None => break },
                () = tokio::time::sleep(every) => Cmd::Kick,
            };
            let ticket = match &cmd {
                Cmd::Acquire(ticket)
                | Cmd::Override(ticket)
                | Cmd::Recover(ticket, _)
                | Cmd::Release(ticket, _) => Some(*ticket),
                Cmd::Stop | Cmd::Kick => None,
            };
            if ticket.is_some_and(|ticket| ticket != current_intent.load(Ordering::SeqCst)) {
                // A superseded waiter still completes; it must not hang a
                // shutdown that was subsequently replaced by another intent.
                if let Cmd::Release(_, Some(ack)) = cmd {
                    let _ = ack.send(());
                }
                continue;
            }
            let mut recovery = None;
            let (result, ack) = match cmd {
                Cmd::Stop => break,
                Cmd::Kick => (Ok(poll(&store, &*bucket).await), None),
                Cmd::Acquire(_) => (acquire_requested(&store, &*bucket, false).await, None),
                Cmd::Override(_) => (acquire_requested(&store, &*bucket, true).await, None),
                Cmd::Recover(_, pending) => {
                    recovery = Some(pending);
                    publish_status(Status {
                        role: Role::Recovering,
                        epoch: store.epoch(),
                        unpublished: store.unpublished(),
                        device: store.device(),
                        note: None,
                    });
                    (recover(&store, &*bucket).await, None)
                }
                Cmd::Release(_, ack) => (release_requested(&store, &*bucket).await, ack),
            };
            let next = match result {
                Ok(s) => s,
                Err(why) => failed(&store, why).await,
            };
            drop(recovery);
            publish_status(next);
            if let Some(ack) = ack {
                let _ = ack.send(());
            }
        }
    });
    Driver {
        db: driver_db,
        cmd,
        status,
        done: Some(done),
        latest_intent,
        enqueue_intent: Mutex::new(()),
        recovering: Arc::new(AtomicBool::new(false)),
    }
}
