//! Quitting is a lifecycle the window pumps, not a blocking window callback.
//! Accepted completions retain the session until they commit. Service cleanup
//! then owns only detached handles and runs outside the UI.

use super::Session;
use std::future::Future;
use std::sync::Arc;
use tokio::sync::oneshot;

pub(super) enum Shutdown {
    Running,
    Draining { retired: bool },
    Flushing { retired: bool, completion: Completion },
    Retiring(Completion),
    Releasing(Completion),
    Complete,
}

impl Shutdown {
    pub(super) fn accepts_completions(&self) -> bool {
        matches!(self, Self::Running | Self::Draining { .. })
    }
}

pub(super) struct Completion(Option<oneshot::Receiver<()>>);

impl Completion {
    fn start<F, Fut>(wake: Option<Arc<dyn Fn() + Send + Sync>>, start: F) -> Self
    where
        F: FnOnce() -> Fut + Send + 'static,
        Fut: Future<Output = ()> + 'static,
    {
        // Observe task failure too: a panicking service must still wake the
        // window, rather than leaving its lifecycle permanently pending.
        let task = crate::runtime::spawn_local(start);
        let (send, receive) = oneshot::channel();
        crate::runtime::spawn(async move {
            if let Err(error) = task.await { eprintln!("shutdown: service failed: {error}"); }
            let _ = send.send(());
            if let Some(wake) = wake { wake(); }
        });
        Self(Some(receive))
    }

    fn ready(&mut self) -> bool {
        let Some(receive) = &mut self.0 else { return true; };
        if matches!(receive.try_recv(), Err(oneshot::error::TryRecvError::Empty)) {
            return false;
        }
        self.0 = None;
        true
    }

    fn wait(&mut self) {
        if let Some(receive) = self.0.take() { let _ = crate::runtime::block_on(receive); }
    }
}

impl Session {
    /// Stops admitting window input and submits unsaved drafts once. The shell
    /// continues delivering completion signals until `poll_shutdown` is ready.
    pub fn begin_shutdown(&mut self) {
        if self.is_closing() { return; }
        self.shutdown = Shutdown::Draining { retired: false };
        for panel in self.instances.values() { panel.borrow_mut().flush(); }
        self.save();
    }

    pub fn is_closing(&self) -> bool { !matches!(self.shutdown, Shutdown::Running) }

    /// Advances shutdown without waiting for I/O. Each background completion
    /// wakes the attached UI; repeated quit requests are harmless.
    pub fn poll_shutdown(&mut self) -> bool {
        loop {
            match &mut self.shutdown {
                Shutdown::Running => return false,
                Shutdown::Draining { retired } => {
                    let retired = *retired;
                    self.settle();
                    if !self.preparations.is_empty() || !self.effects.is_empty() || !self.events.is_empty()
                        || !self.edits.is_empty() || self.walk_pending() || !self.commands.is_empty() {
                        return false;
                    }
                    let apps = self.apps.list();
                    let db = self.store.db();
                    let completion = Completion::start(self.store.ui_waker(), move || async move {
                        for app in apps { app.flush(db.clone()).await; }
                    });
                    self.shutdown = Shutdown::Flushing { retired, completion };
                }
                Shutdown::Flushing { retired, completion } => {
                    if !completion.ready() { return false; }
                    if !*retired {
                        let workers = self.workers.shutdown();
                        self.shutdown = Shutdown::Retiring(Completion::start(self.store.ui_waker(), move || workers));
                        continue;
                    }
                    let repl = self.repl.take();
                    let db = self.store.db();
                    self.shutdown = Shutdown::Releasing(Completion::start(self.store.ui_waker(), move || async move {
                        if let Err(error) = db.flush_async().await {
                            eprintln!("shutdown: database flush failed: {error}");
                        }
                        if let Some(repl) = repl { repl.shutdown(db).await; }
                    }));
                }
                Shutdown::Retiring(completion) => {
                    if !completion.ready() { return false; }
                    // A worker's final accepted operation can publish an app
                    // completion (including native undo/compensation). Apply
                    // it on the UI and drain its writes before lease release.
                    self.shutdown = Shutdown::Draining { retired: true };
                }
                Shutdown::Releasing(completion) => {
                    if !completion.ready() { return false; }
                    self.shutdown = Shutdown::Complete;
                }
                Shutdown::Complete => return true,
            }
        }
    }

    /// The same lifecycle at a synchronous boundary: CLI, tests, or the last
    /// platform shutdown callback when no preflight quit event was available.
    pub fn shutdown(&mut self) {
        self.begin_shutdown();
        while !self.poll_shutdown() {
            match &mut self.shutdown {
                Shutdown::Flushing { completion, .. } | Shutdown::Retiring(completion)
                    | Shutdown::Releasing(completion) => completion.wait(),
                Shutdown::Draining { .. } => {
                    self.poll_events();
                    self.flush_edits();
                    self.poll_events();
                    self.flush_work();
                    self.poll_events();
                }
                Shutdown::Running | Shutdown::Complete => {}
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::App;
    use crate::panel::PanelKind;
    use crate::session::Edit;
    use std::sync::{atomic::{AtomicUsize, Ordering}, mpsc, Mutex};
    use std::time::{Duration, Instant};

    struct HeldFlush {
        ui: std::thread::ThreadId,
        release: Mutex<Option<oneshot::Receiver<()>>>,
        calls: AtomicUsize,
    }

    impl App for HeldFlush {
        fn id(&self) -> &'static str { "shutdown-test" }
        fn as_any(&self) -> &dyn std::any::Any { self }
        fn kinds(&self) -> &'static [&'static dyn PanelKind] { &[] }
        fn flush(&self, db: Arc<crate::store::Db>) -> std::pin::Pin<Box<dyn Future<Output = ()> + Send>> {
            assert_ne!(std::thread::current().id(), self.ui, "even flush setup belongs off the UI");
            self.calls.fetch_add(1, Ordering::SeqCst);
            let receive = self.release.lock().unwrap().take();
            Box::pin(async move {
                let Some(receive) = receive else { return; };
                tokio::time::timeout(Duration::from_secs(5), receive).await.unwrap().unwrap();
                db.raw_async(|conn| {
                    conn.execute("INSERT INTO meta(key,value) VALUES('shutdown-tail',2)", [])?;
                    Ok(())
                }).await.unwrap();
            })
        }
    }

    fn pump_until(session: &mut Session, wake: &mpsc::Receiver<()>, ready: impl Fn(&Session) -> bool) {
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            session.store.poll_external();
            session.poll_shutdown();
            if ready(session) { return; }
            wake.recv_timeout(deadline.checked_duration_since(Instant::now()).expect("shutdown completed"))
                .expect("shutdown completion wakes the window");
        }
    }

    #[test]
    fn native_quit_returns_while_work_is_held_and_drains_every_completion_once() {
        let (release_flush, receive_flush) = oneshot::channel();
        let app = Box::leak(Box::new(HeldFlush {
            ui: std::thread::current().id(), release: Mutex::new(Some(receive_flush)), calls: AtomicUsize::new(0),
        }));
        let apps: &'static [&'static dyn App] = Box::leak(Box::new([app as &dyn App]));
        let mut session = Session::fake(apps);
        let (notify, wake) = mpsc::channel();
        session.store.attach_ui(move || { let _ = notify.send(()); });
        let (release_prepare, receive_prepare) = oneshot::channel();
        session.prepare_work(move |_| Box::pin(async move {
            receive_prepare.await.map_err(|error| error.to_string())
        }), |session, result| {
            result.unwrap();
            assert!(tokio::runtime::Handle::try_current().is_err(), "completion stays on the UI");
            session.after_event(|session| {
                session.act_async(Edit::writing("quit", "accepted before quit", |tx| {
                    tx.execute("INSERT INTO meta(key,value) VALUES('shutdown-prepared',1)", [])?;
                    Ok(())
                }), |_, result| assert!(result.is_some()));
            });
        });
        let started = Instant::now();
        session.begin_shutdown();
        assert!(!session.poll_shutdown());
        assert!(started.elapsed() < Duration::from_millis(200), "quit does not wait for accepted preparation");
        assert!(session.is_closing());
        release_prepare.send(()).unwrap();
        pump_until(&mut session, &wake, |session| matches!(session.shutdown, Shutdown::Flushing { .. }));
        let started = Instant::now();
        session.begin_shutdown();
        assert!(!session.poll_shutdown(), "app flush remains accepted and held");
        assert!(started.elapsed() < Duration::from_millis(200), "quit does not wait for local app cleanup");
        release_flush.send(()).unwrap();
        pump_until(&mut session, &wake, |session| matches!(session.shutdown, Shutdown::Complete));
        session.shutdown();
        assert_eq!(app.calls.load(Ordering::SeqCst), 2,
            "flush before and after worker retirement; repeated quit does not repeat either phase");
        let values = session.store.conn().query_row(
            "SELECT sum(value) FROM meta WHERE key IN ('shutdown-prepared','shutdown-tail')", [], |row| row.get::<_, i64>(0),
        ).unwrap();
        assert_eq!(values, 3, "both UI completion and service flush commit before exit");
    }
}
