//! Prepared transcripts. Disk reads and row construction run on one reader;
//! draws take an Arc and render only the visible rows. The newest requests go
//! first, and an evicted request cannot delay the next chat in a cursor walk.

use std::cell::{Cell, RefCell};
use std::collections::VecDeque;
use std::sync::{atomic::{AtomicBool, Ordering}, mpsc, Arc, Mutex, OnceLock};

use kernel::store::Store;

use super::model::{self, Msg, MsgId, PeerId};
use super::panels::chat::{rows_of, Row};

const DEPENDENCIES: &[&str] = &["tg_message", "tg_message_reaction", "tg_peer"];
const CAPACITY: usize = 8;

#[derive(Default)]
pub struct Snapshot {
    pub ready: bool,
    pub history: Arc<Vec<Msg>>,
    pub rows: Arc<Vec<Row>>,
}

impl Snapshot {
    fn new(history: Vec<Msg>, first_unread: Option<MsgId>, now: f64) -> Self {
        let rows = Arc::new(rows_of(&history, first_unread, now));
        Self { ready: true, history: Arc::new(history), rows }
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
struct Key {
    peer: PeerId,
    topic: i64,
    first_unread: Option<MsgId>,
    day: i64,
}

struct Entry {
    key: Key,
    revision: Vec<u64>,
    load: Arc<Load>,
}

#[derive(Default)]
struct Load {
    snapshot: Mutex<Arc<Snapshot>>,
    failed_at: Mutex<Option<std::time::Instant>>,
}

struct Job {
    key: Key,
    now: f64,
    load: std::sync::Weak<Load>,
}

#[derive(Default)]
struct State {
    entries: VecDeque<Entry>,
    jobs: Vec<Job>,
    retired: Vec<Entry>,
}

#[derive(Default)]
struct Loader {
    state: Mutex<State>,
    wake: OnceLock<mpsc::SyncSender<()>>,
    changed: AtomicBool,
}

impl Loader {
    fn request(self: &Arc<Self>, store: &Store, key: Key, now: f64) -> Arc<Snapshot> {
        let revision = store.revision(DEPENDENCIES);
        let mut state = self.state.lock().expect("transcript queue");
        let previous = state.entries.iter().position(|e| e.key == key)
            .and_then(|i| state.entries.remove(i));
        let entry = match previous {
            Some(entry) if entry.revision == revision
                && entry.load.failed_at.lock().expect("transcript retry")
                    .is_none_or(|at| at.elapsed().as_secs_f64() < 1.0) => entry,
            previous => {
                // Keep the current reading visible while a live update loads.
                let snapshot = previous.map(|e| e.load.snapshot.lock().unwrap().clone())
                    .unwrap_or_default();
                let load = Arc::new(Load { snapshot: Mutex::new(snapshot), failed_at: Mutex::new(None) });
                state.jobs.push(Job { key, now, load: Arc::downgrade(&load) });
                Entry { key, revision, load }
            }
        };
        let snapshot = entry.load.snapshot.lock().expect("transcript result").clone();
        state.entries.push_back(entry);
        while state.entries.len() > CAPACITY {
            if let Some(entry) = state.entries.pop_front() { state.retired.push(entry); }
        }
        // Retired entries keep their large buffers alive until the worker can
        // free them, but must not keep obsolete queries in the work queue.
        let live: Vec<_> = state.entries.iter().map(|e| Arc::downgrade(&e.load)).collect();
        state.jobs.retain(|j| live.iter().any(|load| load.ptr_eq(&j.load)));
        let pending = !state.jobs.is_empty() || !state.retired.is_empty();
        drop(state);
        if pending {
            let wake = self.wake.get_or_init(|| {
                let (tx, rx) = mpsc::sync_channel(1);
                let owner = Arc::downgrade(self);
                let db = Arc::downgrade(&store.db());
                std::thread::Builder::new().name("telegram-transcripts".into()).spawn(move || {
                    while rx.recv().is_ok() {
                        loop {
                            let Some(owner) = owner.upgrade() else { return };
                            let (job, retired) = {
                                let mut state = owner.state.lock().expect("transcript queue");
                                (state.jobs.pop(), std::mem::take(&mut state.retired))
                            };
                            drop(retired);
                            let Some(job) = job else { break };
                            let Some(load) = job.load.upgrade() else { continue };
                            let Some(db) = db.upgrade() else { return };
                            // Drop the reader before waiting: a database-local service
                            // must not keep its own database alive through a cycle.
                            let result = Store::with_db(db).and_then(|reader| {
                                model::read_history(reader.conn(), job.key.peer, job.key.topic)
                            });
                            match result {
                                Ok(history) => {
                                    let snapshot = Snapshot::new(history, job.key.first_unread, job.now);
                                    *load.snapshot.lock().expect("transcript result") = Arc::new(snapshot);
                                }
                                Err(error) => {
                                    eprintln!("telegram: reading transcript failed: {error}");
                                    *load.failed_at.lock().expect("transcript retry") = Some(std::time::Instant::now());
                                }
                            }
                            owner.changed.store(true, Ordering::Release);
                            makepad_widgets::SignalToUI::set_ui_signal();
                        }
                    }
                }).expect("spawn transcript reader");
                tx
            });
            let _ = wake.try_send(());
        }
        snapshot
    }
}


pub fn take_changed(store: &Store) -> bool {
    store.local::<Loader>().changed.swap(false, Ordering::AcqRel)
}

pub struct Transcript {
    key: Cell<Key>,
    now: Cell<f64>,
    inline: RefCell<Option<Inline>>,
}

struct Inline {
    revision: Vec<u64>,
    key: Key,
    snapshot: Arc<Snapshot>,
}

impl Transcript {
    pub fn new(peer: PeerId, topic: i64, first_unread: Option<MsgId>, now: f64) -> Self {
        Self {
            key: Cell::new(Key { peer, topic, first_unread, day: (now / 86400.0).floor() as i64 }),
            now: Cell::new(now),
            inline: RefCell::default(),
        }
    }

    pub fn at(&self, now: f64) {
        self.now.set(now);
        self.key.set(Key { day: (now / 86400.0).floor() as i64, ..self.key.get() });
    }

    pub fn get(&self, store: &Store) -> Arc<Snapshot> {
        let key = self.key.get();
        if store.dir().is_some() && !cfg!(headless) {
            let snapshot = store.local::<Loader>().request(store, key, self.now.get());
            model::trace_history(store, key.peer, key.topic, snapshot.history.len());
            return snapshot;
        }
        // Scripted/library worlds use inline passes and virtual time throughout.
        // Keep their deterministic draws, with the same prepared-row cache.
        let revision = store.revision(DEPENDENCIES);
        let mut inline = self.inline.borrow_mut();
        if let Some(cached) = &*inline {
            if cached.revision == revision && cached.key == key {
                model::trace_history(store, key.peer, key.topic, cached.snapshot.history.len());
                return cached.snapshot.clone();
            }
        }
        let history = model::history_in(store, key.peer, key.topic);
        let snapshot = Arc::new(Snapshot::new((*history).clone(), key.first_unread, self.now.get()));
        *inline = Some(Inline { revision, key, snapshot: snapshot.clone() });
        snapshot
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::apps::telegram::{schema, seed};
    use std::time::{Duration, Instant};

    fn store() -> Store {
        let store = Store::open(None, &[&schema::SCHEMA]).unwrap();
        seed::seed_if_empty(&store).unwrap();
        store
    }

    fn key(peer: PeerId) -> Key { Key { peer, topic: 0, first_unread: None, day: 0 } }

    fn loaded(loader: &Arc<Loader>, store: &Store, key: Key) -> Arc<Snapshot> {
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            let snapshot = loader.request(store, key, 0.0);
            if snapshot.ready { return snapshot; }
            assert!(Instant::now() < deadline, "the background transcript did not finish");
            std::thread::sleep(Duration::from_millis(1));
        }
    }

    #[test]
    fn background_history_never_queries_the_calling_connection_and_keeps_updates() {
        let store = store();
        let loader = store.local::<Loader>();
        let expected = model::history(&store, seed::VERA);
        // Every read on the UI connection would fail. The background reader
        // has its own connection and must still deliver the complete transcript.
        store.conn().authorizer(Some(|_: rusqlite::hooks::AuthContext<'_>| rusqlite::hooks::Authorization::Deny)).unwrap();
        let first = loader.request(&store, key(seed::VERA), 0.0);
        assert!(!first.ready, "the first request returns before starting the read");
        let before = loaded(&loader, &store, key(seed::VERA));
        assert_eq!(*before.history, *expected);
        store.conn().authorizer(None::<fn(rusqlite::hooks::AuthContext<'_>) -> rusqlite::hooks::Authorization>).unwrap();

        let worker = Store::with_db(store.db()).unwrap();
        worker.write(|c| c.execute("INSERT OR REPLACE INTO meta(key,value) VALUES('unrelated',1)", []).map(|_| ())).unwrap();
        store.poll_external();
        assert!(Arc::ptr_eq(&before, &loader.request(&store, key(seed::VERA), 0.0)));

        let id = expected[0].id;
        worker.write(move |c| c.execute("UPDATE tg_message SET text='updated' WHERE chat=?1 AND id=?2", [seed::VERA, id]).map(|_| ())).unwrap();
        let pending = loader.request(&store, key(seed::VERA), 0.0);
        assert!(Arc::ptr_eq(&before, &pending), "keep the old reading during refresh");
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            let after = loader.request(&store, key(seed::VERA), 0.0);
            if after.history[0].text == "updated" { break; }
            assert!(Instant::now() < deadline);
            std::thread::sleep(Duration::from_millis(1));
        }
        assert_ne!(before.history[0].text, "updated", "published snapshots are immutable");
        let other = loaded(&loader, &store, key(seed::STELAXIS));
        assert!(other.history.iter().all(|m| m.chat == seed::STELAXIS));
    }

    #[test]
    fn rapid_switches_bound_the_queue_and_prioritize_the_latest_chat() {
        let store = store();
        let loader = store.local::<Loader>();
        // Hold the worker so a burst can be inspected independently of timing.
        let (tx, _rx) = mpsc::sync_channel(1);
        loader.wake.set(tx).unwrap();
        for peer in 1..=100 { loader.request(&store, key(peer), 0.0); }
        let mut state = loader.state.lock().unwrap();
        assert_eq!(state.entries.len(), CAPACITY);
        assert_eq!(state.jobs.len(), CAPACITY);
        assert_eq!(state.jobs.pop().unwrap().key.peer, 100);
        assert!(state.jobs.iter().all(|j| j.key.peer > 100 - CAPACITY as i64));
    }
}
