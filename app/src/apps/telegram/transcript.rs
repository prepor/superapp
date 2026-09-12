//! Prepared transcripts. Disk reads and row construction run on one reader;
//! draws take an Arc and render only the visible rows. The newest requests go
//! first, and an evicted request cannot delay the next chat in a cursor walk.

use std::cell::{Cell, RefCell};
use std::collections::{HashMap, VecDeque};
use std::sync::{atomic::{AtomicBool, Ordering}, Arc, Mutex};

use kernel::store::Store;

use super::model::{self, Msg, MsgId, MsgKey, PeerId};
use super::panels::chat::{rows_of, Row};

const DEPENDENCIES: &[&str] = &["tg_message", "tg_message_reaction", "tg_peer", "tg_chat_upgrade"];
const CAPACITY: usize = 8;

#[derive(Default)]
pub struct Snapshot {
    pub ready: bool,
    pub history: Arc<Vec<Msg>>,
    pub rows: Arc<Vec<Row>>,
    messages: HashMap<MsgKey, usize>,
    row_indices: HashMap<MsgKey, usize>,
}

impl Snapshot {
    fn new(history: Vec<Msg>, first_unread: Option<MsgKey>, now: f64) -> Self {
        let rows = Arc::new(rows_of(&history, first_unread, now));
        let messages = history.iter().enumerate().map(|(i, m)| (m.key(), i)).collect();
        let row_indices = rows.iter().enumerate()
            .filter_map(|(i, r)| r.msg().map(|m| (m.key(), i))).collect();
        Self { ready: true, history: Arc::new(history), rows, messages, row_indices }
    }

    pub fn message(&self, id: MsgKey) -> Option<&Msg> {
        self.messages.get(&id).and_then(|&i| self.history.get(i))
    }

    pub fn row_index(&self, id: MsgKey) -> Option<usize> {
        self.row_indices.get(&id).copied()
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
    running: AtomicBool,
    changed: AtomicBool,
}

impl Loader {
    fn request(self: &Arc<Self>, store: &Store, key: Key, now: f64) -> Arc<Snapshot> {
        let revision = store.revision(DEPENDENCIES);
        let mut state = self.state.lock().expect("transcript queue");
        let previous = state.entries.iter().position(|e| {
            e.key.peer == key.peer && e.key.topic == key.topic && e.key.first_unread == key.first_unread
        })
            .and_then(|i| state.entries.remove(i));
        let entry = match previous {
            Some(entry) if entry.key == key && entry.revision == revision
                && entry.load.failed_at.lock().expect("transcript retry")
                    .is_none_or(|at| at.elapsed().as_secs_f64() < 1.0) => entry,
            previous => {
                // A day change invalidates captions, not the current reading.
                // Keep it visible while either new captions or messages load.
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
        if pending && !self.running.swap(true, Ordering::AcqRel) {
            let owner = Arc::downgrade(self);
            let db = Arc::downgrade(&store.db());
            kernel::runtime::spawn_blocking(move || {
                loop {
                    let Some(owner) = owner.upgrade() else { return };
                    let (job, retired) = {
                        let mut state = owner.state.lock().expect("transcript queue");
                        let job = state.jobs.pop();
                        if job.is_none() { owner.running.store(false, Ordering::Release); }
                        (job, std::mem::take(&mut state.retired))
                    };
                    drop(retired);
                    let Some(job) = job else { break };
                    let Some(load) = job.load.upgrade() else { continue };
                    let Some(db) = db.upgrade() else { return };
                    // SQLite and row construction stay on the blocking
                    // pool; no connection crosses an await point.
                    let result = Store::with_db(db).and_then(|reader| {
                        model::read_history(reader.conn(), job.key.peer, job.key.topic)
                    });
                    match result {
                        Ok(history) => {
                            let snapshot = Snapshot::new(history, job.key.first_unread.map(|id| (job.key.peer, id)), job.now);
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
            });
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
        if store.ui_attached() || (store.dir().is_some() && !cfg!(headless)) {
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
        let snapshot = Arc::new(Snapshot::new((*history).clone(), key.first_unread.map(|id| (key.peer, id)), self.now.get()));
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
        let store = Store::open(None, &[&schema::SCHEMA], kernel::sync::Device::fake()).unwrap();
        seed::seed_if_empty(&store).unwrap();
        store
    }

    fn key(peer: PeerId) -> Key { Key { peer, topic: 0, first_unread: None, day: 0 } }

    #[test]
    fn message_lookups_follow_backfills_deletions_and_dividers() {
        let store = store();
        let mut history = model::history(&store, seed::VERA).as_ref().clone();
        let id = history.last().unwrap().key();
        let before = Snapshot::new(history.clone(), Some(id), 0.0);
        let mut older = history[0].clone();
        older.id = -1;
        older.date -= 86400.0;
        older.text = "backfilled".into();
        history.insert(0, older);
        history.retain(|m| m.key() != id);
        let after = Snapshot::new(history, None, 0.0);
        assert!(before.message(id).is_some());
        assert!(after.message(id).is_none());
        assert!(after.row_index(id).is_none());
        assert_eq!(after.message((seed::VERA, -1)).unwrap().text, "backfilled");
        for snapshot in [&before, &after] {
            for (i, row) in snapshot.rows.iter().enumerate() {
                if let Some(msg) = row.msg() {
                    assert_eq!(snapshot.row_index(msg.key()), Some(i));
                    assert_eq!(snapshot.message(msg.key()), Some(msg));
                }
            }
        }
    }

    fn loaded(loader: &Arc<Loader>, store: &Store, key: Key) -> Arc<Snapshot> {
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            let snapshot = loader.request(store, key, key.day as f64 * 86400.0);
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
        loader.running.store(true, Ordering::Release);
        for peer in 1..=100 { loader.request(&store, key(peer), 0.0); }
        let mut state = loader.state.lock().unwrap();
        assert_eq!(state.entries.len(), CAPACITY);
        assert_eq!(state.jobs.len(), CAPACITY);
        assert_eq!(state.jobs.pop().unwrap().key.peer, 100);
        assert!(state.jobs.iter().all(|j| j.key.peer > 100 - CAPACITY as i64));
    }

    #[test]
    fn day_rollover_keeps_the_reading_until_background_captions_arrive() {
        let store = store();
        store.write(|c| c.execute("UPDATE tg_message SET date = 90000 WHERE chat = ?1", [seed::VERA]).map(|_| ())).unwrap();
        let loader = store.local::<Loader>();
        let first_unread = model::history(&store, seed::VERA).first().map(|m| m.id);
        let today = Key { day: 1, first_unread, ..key(seed::VERA) };
        let before = loaded(&loader, &store, today);
        assert!(matches!(before.rows.first(), Some(Row::Day(day)) if day == "TODAY"));
        let unread = before.rows.iter().position(|row| matches!(row, Row::Unread));
        assert!(unread.is_some());

        // Request the new day through the real background loader, even in
        // headless tests whose usual Transcript path prepares rows inline.
        let tomorrow = Key { day: 2, ..today };
        let pending = loader.request(&store, tomorrow, 172800.0);
        assert!(Arc::ptr_eq(&before, &pending), "midnight must preserve the current reading while loading");
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            let after = loader.request(&store, tomorrow, 172800.0);
            assert!(after.ready);
            assert_eq!(*after.history, *before.history, "verbs must retain access to the messages");
            assert_eq!(after.rows.iter().position(|row| matches!(row, Row::Unread)), unread);
            if matches!(after.rows.first(), Some(Row::Day(day)) if day == "YESTERDAY") {
                assert!(!Arc::ptr_eq(&before, &after));
                break;
            }
            assert!(Instant::now() < deadline, "the background day captions did not refresh");
            std::thread::sleep(Duration::from_millis(1));
        }
        assert!(matches!(before.rows.first(), Some(Row::Day(day)) if day == "TODAY"),
            "published snapshots stay immutable");
    }

    #[test]
    fn a_fallback_reading_cannot_come_from_another_chat_topic_or_unread_boundary() {
        let store = store();
        let loader = store.local::<Loader>();
        let today = Key { day: 1, ..key(seed::VERA) };
        assert!(!loaded(&loader, &store, today).history.is_empty());
        for different in [
            Key { peer: seed::STELAXIS, day: 2, ..today },
            Key { topic: 42, day: 2, ..today },
            Key { first_unread: Some(42), day: 2, ..today },
        ] {
            let pending = loader.request(&store, different, 172800.0);
            assert!(!pending.ready);
            assert!(pending.rows.is_empty());
            assert!(pending.history.is_empty());
        }
    }
}
