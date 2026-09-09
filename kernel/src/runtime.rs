//! The boundary between the synchronous window and asynchronous services.
//!
//! Send tasks share Tokio's pool. Stateful services have thread-local worlds
//! (SQLite readers and platform capabilities), so their factories cross to one
//! local executor and construct their state there. There is no thread per
//! account, connection, search, or request. Blocking native/CPU work belongs on
//! the blocking pool; SQLite's single writer and TDLib's receiver own their
//! dedicated threads because their APIs require long-lived blocking loops.

use std::future::Future;
use std::sync::OnceLock;
use tokio::runtime::{Builder, Runtime};
use tokio::sync::{mpsc, oneshot};
use tokio::task::{JoinHandle, LocalSet};

fn shared() -> &'static Runtime {
    static RUNTIME: OnceLock<Runtime> = OnceLock::new();
    RUNTIME.get_or_init(|| {
        Builder::new_multi_thread()
            .thread_name("superapp-io")
            .enable_all()
            .build()
            .expect("start the I/O runtime")
    })
}

/// Starts independent asynchronous I/O from any thread, including the UI.
pub fn spawn<F>(future: F) -> JoinHandle<F::Output>
where
    F: Future + Send + 'static,
    F::Output: Send + 'static,
{
    shared().spawn(future)
}

/// Isolates a bounded blocking operation from both UI and I/O executors.
pub fn spawn_blocking<F, T>(work: F) -> JoinHandle<T>
where
    F: FnOnce() -> T + Send + 'static,
    T: Send + 'static,
{
    shared().spawn_blocking(work)
}

type Start = Box<dyn FnOnce() + Send>;

fn local() -> &'static mpsc::UnboundedSender<Start> {
    static LOCAL: OnceLock<mpsc::UnboundedSender<Start>> = OnceLock::new();
    LOCAL.get_or_init(|| {
        let (tx, mut rx) = mpsc::unbounded_channel::<Start>();
        std::thread::Builder::new()
            .name("superapp-services".into())
            .spawn(move || {
                let runtime = Builder::new_current_thread().enable_all().build()
                    .expect("start the service runtime");
                LocalSet::new().block_on(&runtime, async move {
                    while let Some(start) = rx.recv().await {
                        start();
                    }
                });
            })
            .expect("start the service executor");
        tx
    })
}

/// Constructs a thread-local service on the executor. The completion receiver
/// reports a dropped/panicked service; dropping it does not cancel a side effect.
pub fn spawn_local<F, Fut, T>(factory: F) -> oneshot::Receiver<T>
where
    F: FnOnce() -> Fut + Send + 'static,
    Fut: Future<Output = T> + 'static,
    T: Send + 'static,
{
    let (done, result) = oneshot::channel();
    local().send(Box::new(move || {
        tokio::task::spawn_local(async move {
            let output = factory().await;
            let _ = done.send(output);
        });
    })).expect("service executor is alive");
    result
}

/// Drives async work at a synchronous entry point (CLI, shutdown, or a fake
/// frame/test). Never call this from an asynchronous service.
pub fn block_on<F: Future>(future: F) -> F::Output {
    assert!(tokio::runtime::Handle::try_current().is_err(),
        "block_on is only for synchronous entry points");
    shared().block_on(future)
}
