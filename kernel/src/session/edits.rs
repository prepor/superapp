//! Data edits with asynchronous commit and explicit UI completion. They do
//! not carry speculative layout snapshots: navigation may continue while the
//! writer is busy, and a failed edit never rolls that navigation back.

use super::*;

type UiCompletion = Box<dyn FnOnce(&mut Session) + Send>;

pub struct Edit<R> {
    kind: &'static str,
    label: String,
    entity: Option<String>,
    data: Data<Committed<R>>,
}

struct Committed<R> {
    value: R,
    intents: Vec<Box<dyn Intent>>,
    record: bool,
    wake: bool,
    ui: Vec<UiCompletion>,
}

impl<R: 'static> Edit<R> {
    pub fn label(&self) -> &str { &self.label }

    /// Bookkeeping shares the transaction; a failed precondition rolls back
    /// the edit and its completion record together.
    pub fn then_write(mut self, write: impl FnOnce(&Transaction, &R) -> rusqlite::Result<()> + Send + 'static) -> Self {
        let data = self.data;
        self.data = Box::new(move |tx| {
            let result = data(tx)?;
            write(tx, &result.value)?;
            Ok(result)
        });
        self
    }

    pub fn writing(kind: &'static str, label: impl Into<String>,
        data: impl FnOnce(&Transaction) -> rusqlite::Result<R> + Send + 'static) -> Self {
        Self { kind, label: label.into(), entity: None, data: Box::new(move |tx| {
            Ok(Committed { value: data(tx)?, intents: Vec::new(), record: true, wake: true, ui: Vec::new() })
        }) }
    }

    pub fn about(mut self, entity: impl Into<String>) -> Self {
        self.entity = Some(entity.into());
        self
    }

    /// A successful no-op does not grow the undo tree.
    pub fn record_if(mut self, predicate: impl FnOnce(&R) -> bool + Send + 'static) -> Self {
        let data = self.data;
        self.data = Box::new(move |tx| {
            let mut result = data(tx)?;
            result.record = predicate(&result.value);
            Ok(result)
        });
        self
    }

    /// Build claims from generated IDs or owned snapshots on the writer.
    pub fn claiming_with(mut self, claims: impl FnOnce(&mut R) -> Vec<Box<dyn Intent>> + Send + 'static) -> Self {
        let data = self.data;
        self.data = Box::new(move |tx| {
            let mut result = data(tx)?;
            if result.record { result.intents.extend(claims(&mut result.value)); }
            Ok(result)
        });
        self
    }

    pub fn claiming(self, intents: Vec<Box<dyn Intent>>) -> Self {
        self.claiming_with(move |_| intents)
    }

    /// Suppress scheduler work for a transaction that found no change.
    /// This is separate from history: durable metadata still needs wakeups.
    pub fn wake_if(mut self, predicate: impl FnOnce(&R) -> bool + Send + 'static) -> Self {
        let data = self.data;
        self.data = Box::new(move |tx| {
            let mut result = data(tx)?;
            result.wake = predicate(&result.value);
            Ok(result)
        });
        self
    }

    /// Construct a lightweight UI consequence from the committed reply.
    /// The closure runs after history records the edit, on the UI thread.
    pub fn on_commit(mut self, prepare: impl FnOnce(&mut R) -> UiCompletion + Send + 'static) -> Self {
        let data = self.data;
        self.data = Box::new(move |tx| {
            let mut result = data(tx)?;
            result.ui.push(prepare(&mut result.value));
            Ok(result)
        });
        self
    }

    /// Transform the reply after building claims; large intermediate snapshots
    /// remain on the writer while the UI receives only the public result.
    pub fn map<T: 'static>(self, map: impl FnOnce(R) -> T + Send + 'static) -> Edit<T> {
        self.and_then(move |_, value| Ok(map(value)))
    }

    /// Read or validate the resulting state in the same transaction. Failure
    /// rolls back the whole edit; existing claims and UI consequences survive
    /// a successful continuation unchanged.
    pub fn and_then<T: 'static>(self,
        next: impl FnOnce(&Transaction, R) -> rusqlite::Result<T> + Send + 'static,
    ) -> Edit<T> {
        let Self { kind, label, entity, data } = self;
        Edit { kind, label, entity, data: Box::new(move |tx| {
            let Committed { value, intents, record, wake, ui } = data(tx)?;
            Ok(Committed { value: next(tx, value)?, intents, record, wake, ui })
        }) }
    }
}

type Complete = Box<dyn FnOnce(&mut Session)>;
pub(super) type PendingEdit = Box<dyn FnMut(&Store) -> Option<Complete>>;

impl Session {
    /// Commits data on the single writer and invokes `complete` on a later UI
    /// settle. The callback may navigate using the returned row ID. Fakes have
    /// no external waits and complete synchronously for deterministic scripts.
    pub fn act_async<R: Send + 'static>(&mut self, edit: Edit<R>,
        complete: impl FnOnce(&mut Session, Option<R>) + 'static) {
        self.act_async_result(edit, move |session, result| complete(session, result.ok()));
    }

    pub fn act_async_result<R: Send + 'static>(&mut self, edit: Edit<R>,
        complete: impl FnOnce(&mut Session, rusqlite::Result<R>) + 'static) {
        if self.walk_pending() {
            self.commands.push_back(self.bind_completion(move |session| session.act_async_result(edit, complete)));
            return;
        }
        if !self.writable() {
            self.notify("another device holds the lease — nothing was written", true);
            complete(self, Err(rusqlite::Error::SqliteFailure(
                rusqlite::ffi::Error::new(rusqlite::ffi::SQLITE_READONLY), Some("another device holds the lease".into()))));
            return;
        }
        let activity = self.completion_activity.as_ref().map(crate::store::authority::Activity::fork)
            .or_else(|| self.store.db().authority().enter());
        let Some(activity) = activity else {
            complete(self, Err(rusqlite::Error::SqliteFailure(
                rusqlite::ffi::Error::new(rusqlite::ffi::SQLITE_READONLY), Some(crate::effect::SUSPENDED.into()))));
            return;
        };
        let Edit { kind, label, entity, data } = edit;
        let mut finish = Some(move |session: &mut Session, result: rusqlite::Result<Committed<R>>| {
            session.complete_accepted(activity, move |session| match result {
                Ok(Committed { value, intents, record, wake, ui }) => {
                    if record {
                        let snap = session.wm.snapshot();
                        session.history.apply(history::Action {
                            kind, label, entity, before: snap.clone(), after: snap,
                            intents, ts: session.now(),
                        });
                    }
                    if wake {
                        session.workers.kick_all();
                        session.repl_kick();
                    }
                    session.redraw();
                    for complete in ui { complete(session); }
                    complete(session, Ok(value));
                }
                Err(error) => {
                    session.notify(format!("the store refused: {error}"), true);
                    complete(session, Err(error));
                }
            });
        });
        if !self.store.ui_attached() {
            let result = self.store.write(data);
            finish.take().expect("one completion")(self, result);
            return;
        }
        let mut pending = match self.store.submit_write(data) {
            Ok(pending) => pending,
            Err(error) => {
                finish.take().expect("one completion")(self, Err(error));
                return;
            }
        };
        self.edits.push(Box::new(move |store| {
            let result = pending.poll(store)?;
            let finish = finish.take().expect("one commit completion");
            Some(Box::new(move |session| finish(session, result)))
        }));
    }

    pub(super) fn submit_layout(&mut self, snapshot: WmSnap) {
        let saved = snapshot.clone();
        let mut pending = match self.store.submit_write(move |tx| save_wm_tx(tx, &snapshot)) {
            Ok(pending) => pending,
            Err(error) => {
                self.notify(format!("could not save workspace: {error}"), true);
                return;
            }
        };
        let mut saved = Some(saved);
        self.edits.push(Box::new(move |store| {
            let result = pending.poll(store)?;
            let saved = saved.take().expect("one layout completion");
            Some(Box::new(move |session| {
                if let Err(error) = result {
                    if session.last_saved.as_ref() == Some(&saved) { session.last_saved = None; }
                    session.notify(format!("could not save workspace: {error}"), true);
                }
            }))
        }));
    }

    pub(super) fn poll_edits(&mut self) {
        // Commit callbacks may submit further edits; invoke them outside the
        // pending list's borrow and always preserve writer submission order.
        // Leave edits submitted by these callbacks for their next wake, rather
        // than following an arbitrarily long chain in one window event.
        for _ in 0..self.edits.len() {
            let Some(edit) = self.edits.first_mut() else { break; };
            let Some(complete) = edit(&self.store) else { break; };
            drop(self.edits.remove(0));
            complete(self);
            // A completion's panel work may wait for its current borrow to
            // end, but still belongs to this edit. Land it before recording
            // the next ready edit, which may own another history node.
            self.poll_events();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::Cell;
    use std::time::Duration;

    #[test]
    fn ready_edits_land_their_event_completions_before_recording_the_next_edit() {
        let mut session = Session::fake(&[]);
        session.store.attach_ui(|| {});
        let seen = Rc::new(RefCell::new(Vec::new()));
        for label in ["first", "second"] {
            let seen = seen.clone();
            session.act_async(Edit::writing("ordered", label, move |tx| {
                tx.execute("INSERT INTO meta(key,value) VALUES(?1,1)", [label])?;
                Ok(())
            }), move |session, result| {
                assert!(result.is_some());
                let origin = session.history.head();
                session.after_event(move |session| seen.borrow_mut().push((origin, session.history.head())));
            });
        }
        crate::runtime::block_on(session.store.flush_async()).unwrap();
        session.settle();
        let seen = seen.borrow();
        assert_eq!(seen.len(), 2);
        assert_ne!(seen[0].0, seen[1].0, "the edits own separate history nodes");
        assert!(seen.iter().all(|(origin, landed)| origin == landed),
            "a panel consequence must never merge into a later edit's history node");
        session.shutdown();
    }

    #[test]
    fn a_composed_edit_reads_its_write_and_rolls_back_if_the_continuation_fails() {
        use std::sync::atomic::{AtomicBool, Ordering};
        for fail in [false, true] {
            let mut session = Session::fake(&[]);
            let before = session.history.head();
            let committed = Arc::new(AtomicBool::new(false));
            let noticed = committed.clone();
            let edit = Edit::writing("composed", "write and select", |tx| {
                tx.execute("INSERT INTO meta(key,value) VALUES('composed-edit',41)", [])?;
                Ok(41)
            }).record_if(|_| false).wake_if(|_| false)
                .on_commit(move |_| Box::new(move |_| { noticed.store(true, Ordering::SeqCst); }))
                .and_then(move |tx, value| {
                    let row = tx.query_row("SELECT value FROM meta WHERE key='composed-edit'", [],
                        |row| row.get::<_, i64>(0))?;
                    assert_eq!(row, value, "the continuation observes the uncommitted write");
                    if fail { Err(rusqlite::Error::InvalidQuery) } else { Ok(row + 1) }
                });
            session.act_async_result(edit, move |_, result| {
                if fail { assert!(result.is_err()); } else { assert_eq!(result.unwrap(), 42); }
            });
            assert_eq!(session.history.head(), before, "composition retains the record predicate");
            assert_eq!(committed.load(Ordering::SeqCst), !fail);
            let count = session.store.conn().query_row("SELECT count(*) FROM meta WHERE key='composed-edit'", [],
                |row| row.get::<_, i64>(0)).unwrap();
            assert_eq!(count, i64::from(!fail), "failure rolls back data and suppresses UI completion");
            session.shutdown();
        }
    }

    #[test]
    fn a_pending_edit_preserves_navigation_and_records_only_after_commit() {
        let mut session = Session::fake(&[]);
        let (wake, mut woke) = tokio::sync::mpsc::unbounded_channel();
        session.store.attach_ui(move || { let _ = wake.send(()); });
        let (entered, waiting) = std::sync::mpsc::channel();
        let (release, held) = std::sync::mpsc::channel();
        let _blocking = session.store.submit_write(move |_| {
            entered.send(()).unwrap();
            held.recv().unwrap();
            Ok(())
        }).unwrap();
        waiting.recv_timeout(Duration::from_secs(5)).unwrap();
        let result = Rc::new(Cell::new(None));
        let received = result.clone();
        let before = session.history.head();
        session.act_async(Edit::writing("test.edit", "edit a row", |tx| {
            tx.execute("INSERT INTO meta(key,value) VALUES('async-edit',7)", [])?;
            Ok(7)
        }), move |_, value| received.set(value));
        assert!(result.get().is_none());
        assert_eq!(session.history.head(), before);
        session.act(Action::new("test.layout", "open a panel").moving(|wm| {
            wm.open(PanelId::bare(crate::panel::Tag("pending-test")), None, false);
        }));
        let navigated = session.wm.snapshot();
        release.send(()).unwrap();
        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        while result.get().is_none() {
            assert!(std::time::Instant::now() < deadline);
            crate::runtime::block_on(async {
                tokio::time::timeout(Duration::from_secs(5), woke.recv()).await.unwrap();
            });
            session.poll_edits();
        }
        assert_eq!(result.get(), Some(7));
        assert_eq!(session.wm.snapshot(), navigated, "commit does not rewind navigation");
        assert!(session.history.head() > before);
        session.shutdown();
    }

    #[test]
    fn a_successful_noop_does_not_grow_history() {
        let mut session = Session::fake(&[]);
        let before = session.history.head();
        session.act_async(Edit::writing("test.noop", "nothing changed", |_| Ok(0))
            .record_if(|changed| *changed > 0), |_, result| assert_eq!(result, Some(0)));
        assert_eq!(session.history.head(), before);
    }

    #[test]
    fn committed_edit_retains_authority_through_its_deferred_ui_consequence() {
        let mut session = Session::fake(&[]);
        session.store.attach_ui(|| {});
        let completed = Rc::new(Cell::new(false));
        let observed = completed.clone();
        session.act_async(Edit::writing("accepted", "accepted write", |tx| {
            tx.execute("INSERT INTO meta(key,value) VALUES('accepted-before-revoke',1)", [])?;
            Ok(())
        }), move |session, result| {
            assert!(result.is_some(), "the accepted database transaction is preserved");
            assert!(!session.writable());
            session.after_event(move |session| {
                assert!(!session.writable(), "deferred descendants keep the old authority");
                assert!(session.store.write(|tx| tx.execute(
                    "INSERT INTO meta(key,value) VALUES('late-consequence',1)", [])).is_err());
                observed.set(true);
            });
        });
        crate::runtime::block_on(session.store.db().flush_async()).unwrap();
        session.store.set_writable(false);
        // Deliver the SQL completion, pausing at the same boundary where
        // poll_edits next pumps its deferred UI events.
        let complete = session.edits.first_mut().unwrap()(&session.store).unwrap();
        drop(session.edits.remove(0));
        complete(&mut session);
        crate::runtime::block_on(async {
            assert!(tokio::time::timeout(Duration::from_millis(10),
                session.store.db().authority().quiesce()).await.is_err());
        });
        session.poll_events();
        crate::runtime::block_on(session.store.db().authority().quiesce());
        assert!(completed.get());
    }

    #[test]
    fn shutdown_finishes_pending_preparation_and_commits_outside_runtime() {
        let mut session = Session::fake(&[]);
        let (release, wait) = tokio::sync::oneshot::channel();
        session.prepare_work(move |_| Box::pin(async move {
            wait.await.map_err(|error| error.to_string())?;
            Ok(Edit::writing("shutdown", "accepted edit", |tx| {
                tx.execute("INSERT INTO meta(key,value) VALUES('shutdown-edit',1)", [])?;
                Ok(())
            }))
        }), |session, result| {
            session.act_async(result.unwrap(), |_, result| assert!(result.is_some()));
        });
        release.send(()).unwrap();
        session.shutdown();
        assert_eq!(session.store.conn().query_row("SELECT value FROM meta WHERE key='shutdown-edit'", [], |row| row.get::<_, i64>(0)).unwrap(), 1);
    }

}
