//! Preparation has no session borrow while it runs. Its result comes
//! back to the session before the caller decides whether to commit it.

use super::Session;
use crate::tool::{Prepare, Prepared};
use futures_util::task::{waker, ArcWake};
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::task::{Context, Poll};

type WorkResult<T> = std::result::Result<T, String>;
type WorkFuture<'a, T> = Pin<Box<dyn Future<Output = WorkResult<T>> + 'a>>;
type Complete = Box<dyn FnOnce(&mut Session)>;

pub(super) struct PendingWork {
    work: Pin<Box<dyn Future<Output = Complete>>>,
}

struct Redraw(Option<Arc<dyn Fn() + Send + Sync>>);
impl ArcWake for Redraw {
    fn wake_by_ref(this: &Arc<Self>) {
        if let Some(notify) = &this.0 { notify(); }
    }
}

impl Session {
    /// Run an owned operation on this session's exact injected capability.
    /// Its start releases the capability borrow before any wait; completion
    /// belongs to the session and is drained during shutdown.
    pub fn run_effect<E: crate::effect::OwnedEffect + 'static>(&mut self, effect: E,
        complete: impl FnOnce(&mut Session, WorkResult<E::Reply>) + 'static) {
        let world = self.world.clone();
        let work = Box::pin(async move {
            let result = world.run_owned(effect).await;
            Box::new(move |session: &mut Session| complete(session, result)) as Complete
        });
        self.effects.push(PendingWork { work });
        self.poll_work();
    }

    /// Start the expensive half of a tool. The caller owns approval and
    /// cancellation, and checks them again before committing the result.
    /// Accepted work belongs to the session, so closing a chat does not lose
    /// its completion. Fixtures without a factory use their injected world.
    pub fn prepare_tool(&mut self, prepare: Prepare,
        complete: impl FnOnce(&mut Session, WorkResult<Prepared>) + 'static) {
        self.prepare_work(prepare, complete);
    }

    /// Prepares owned input without holding the UI session. Completion runs
    /// on the UI and can submit a transaction using the prepared result.
    pub fn prepare_work<T: Send + 'static>(&mut self,
        prepare: impl for<'a> FnOnce(&'a crate::effect::World) -> WorkFuture<'a, T> + Send + 'static,
        complete: impl FnOnce(&mut Session, WorkResult<T>) + 'static) {
        let work: WorkFuture<'static, T> = if let Some(factory) = self.world.factory() {
            let result = crate::runtime::spawn_local(move || async move {
                let world = factory.build().map_err(|error| error.to_string())?;
                prepare(&world).await
            });
            Box::pin(async move { result.await.map_err(|error| error.to_string())? })
        } else {
            let world = self.world.clone();
            Box::pin(async move { prepare(&world).await })
        };
        let work = Box::pin(async move {
            let result = work.await;
            Box::new(move |session: &mut Session| complete(session, result)) as Complete
        });
        self.preparations.push(PendingWork { work });
        self.poll_work();
    }

    /// Finish the inverse of already accepted native work: what a verb ran
    /// outside the store, undone where it happened.
    pub fn compensate(&mut self, intent: Box<dyn crate::history::Intent>,
        complete: impl FnOnce(&mut Session, Box<dyn crate::history::Intent>, Result<(), String>) + 'static) {
        let world = self.world.clone();
        let work = Box::pin(async move {
            let reverse = |world: &crate::effect::World, intent: &dyn crate::history::Intent| {
                std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| world.compensate(intent)))
                    .unwrap_or_else(|_| Err("native compensation panicked".into()))
            };
            let (intent, result) = if let Some(factory) = world.factory() {
                crate::runtime::spawn_blocking(move || {
                    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                        factory.build().map_err(|error| error.to_string())
                            .and_then(|world| reverse(&world, intent.as_ref()))
                    })).unwrap_or_else(|_| Err("native compensation setup panicked".into()));
                    (intent, result)
                }).await.expect("native compensation owns and catches its execution")
            } else {
                let result = reverse(&world, intent.as_ref());
                (intent, result)
            };
            Box::new(move |session: &mut Session| complete(session, intent, result)) as Complete
        });
        self.preparations.push(PendingWork { work });
        self.poll_work();
    }

    pub(super) fn poll_work(&mut self) {
        let wake = waker(Arc::new(Redraw(self.store.ui_waker())));
        let mut context = Context::from_waker(&wake);
        let mut ready = Vec::new();
        for operations in [&mut self.preparations, &mut self.effects] {
            let mut remaining = Vec::new();
            for mut operation in std::mem::take(operations) {
                match operation.work.as_mut().poll(&mut context) {
                    Poll::Pending => remaining.push(operation),
                    Poll::Ready(complete) => ready.push(complete),
                }
            }
            *operations = remaining;
        }
        for complete in ready { complete(self); }
    }

    /// Shutdown still runs the same completion path: an edit that finished
    /// preparing must commit and publish its tool result before the store
    /// closes.
    pub(super) fn flush_work(&mut self) {
        if self.preparations.is_empty() && self.effects.is_empty() { return; }
        let mut pending = std::mem::take(&mut self.preparations);
        let preparations = pending.len();
        pending.append(&mut self.effects);
        let (at, complete) = crate::runtime::block_on(std::future::poll_fn(|context| {
            for (index, preparation) in pending.iter_mut().enumerate() {
                if let Poll::Ready(complete) = preparation.work.as_mut().poll(context) {
                    return Poll::Ready((index, complete));
                }
            }
            Poll::Pending
        }));
        drop(pending.remove(at));
        self.effects = pending.split_off(preparations - usize::from(at < preparations));
        self.preparations = pending;
        complete(self);
        // Return to the lifecycle pump: this completion may have queued a
        // transaction whose reply releases another preparation's permit.
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::effect::{Ctx, OwnedEffect, OwnedFuture};
    use std::cell::Cell;
    use std::rc::Rc;

    struct Held(tokio::sync::oneshot::Receiver<usize>);
    impl OwnedEffect for Held {
        const KIND: &'static str = "held copy";
        type Reply = usize;
        fn describe(&self) -> String { "copy a prepared reading".into() }
        fn writes(&self) -> bool { true }
        fn start(self, _: &mut Ctx<'_>) -> Result<OwnedFuture<usize>, String> {
            let caller = std::thread::current().id();
            Ok(Box::pin(async move {
                assert_ne!(std::thread::current().id(), caller, "owned work runs off its requesting thread");
                tokio::time::sleep(std::time::Duration::from_millis(1)).await;
                self.0.await.map_err(|error| error.to_string())
            }))
        }
    }

    #[test]
    fn a_pending_owned_effect_leaves_ui_capabilities_available_and_shutdown_joins_it() {
        let mut session = Session::fake(&[]);
        let (release, receive) = tokio::sync::oneshot::channel();
        let completed = Rc::new(Cell::new(None));
        let completion = completed.clone();
        session.act(crate::session::Action::new("test", "test history"));
        session.run_effect(Held(receive), move |_, result| completion.set(Some(result.unwrap())));
        assert_eq!(completed.get(), None);
        assert!(session.world().with_cap::<dyn crate::caps::Clock, _>(|clock| clock.now()).is_ok(),
            "a slow owned operation must release the whole world's capability borrow");
        assert!(session.undo(), "an unrelated clipboard operation must not delay undo");
        release.send(7).unwrap();
        session.shutdown();
        assert_eq!(completed.get(), Some(7), "accepted completion survives shutdown");
    }

    #[test]
    fn event_completions_can_release_other_preparations_during_settle_and_shutdown() {
        for settling in [true, false] {
            let mut session = Session::fake(&[]);
            let (release, receive) = tokio::sync::oneshot::channel();
            session.prepare_work(|_| Box::pin(async { Ok(()) }), move |session, result| {
                result.unwrap();
                session.after_event(move |_| { release.send(()).unwrap(); });
            });
            let complete = Rc::new(Cell::new(false));
            let completed = complete.clone();
            session.prepare_work(move |_| Box::pin(async move {
                crate::runtime::spawn(async move {
                    tokio::time::timeout(std::time::Duration::from_secs(2), receive).await
                        .map_err(|_| "event completion was blocked behind its dependent preparation".to_string())?
                        .map_err(|error| error.to_string())
                }).await.map_err(|error| error.to_string())?
            }), move |_, result| {
                result.unwrap();
                completed.set(true);
            });
            if settling { session.settle(); }
            session.shutdown();
            assert!(complete.get());
        }
    }

}
