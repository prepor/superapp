//! Display identity and source revision are separate. A changed source refreshes
//! the same reading in place; changed controls require a new reading. Retaining
//! the last complete display during refresh also retains its widget identities.
use kernel::store::Store;
use std::{cell::RefCell, sync::Arc};
use tokio::sync::oneshot;

pub enum State<T> {
    Loading,
    Ready(Arc<T>),
    Refreshing(Arc<T>),
    Failed(String),
}
impl<T> State<T> {
    /// The last complete display for these inputs, including during a refresh.
    /// Data mutations still validate their current inputs when they commit.
    pub fn ready(self) -> Option<Arc<T>> {
        match self { Self::Ready(value) | Self::Refreshing(value) => Some(value), _ => None }
    }
}

type Answer<T> = Result<Arc<T>, String>;
struct Entry<K, T, V> {
    ready: Option<(K, V, Answer<T>)>,
    running: Option<(K, V, oneshot::Receiver<Answer<T>>)>,
}
pub struct Snapshot<K, T, V = Vec<u64>>(RefCell<Entry<K, T, V>>);
impl<K, T, V> Default for Snapshot<K, T, V> {
    fn default() -> Self { Self(RefCell::new(Entry { ready: None, running: None })) }
}

impl<K, T, V> Snapshot<K, T, V>
where
    K: PartialEq + Clone + Send + 'static,
    T: Send + Sync + 'static,
    V: PartialEq + Clone + Send + 'static,
{
    pub fn get(&self, store: &Store, key: K, revision: V,
        load: impl FnOnce(&Store) -> Result<T, String> + Send + 'static) -> State<T> {
        let mut entry = self.0.borrow_mut();
        if entry.ready.as_ref().is_some_and(|(previous, _, _)| *previous != key) {
            entry.ready = None;
        }
        if let Some((running, version, receive)) = entry.running.as_mut() {
            let answer = match receive.try_recv() {
                Ok(answer) => Some(answer),
                Err(oneshot::error::TryRecvError::Empty) => None,
                Err(oneshot::error::TryRecvError::Closed) => Some(Err("Calendar display preparation stopped".into())),
            };
            if let Some(answer) = answer {
                if *running == key && *version == revision {
                    entry.ready = Some((key.clone(), revision.clone(), answer));
                }
                entry.running = None;
            }
        }
        if let Some((_, _, answer)) = entry.ready.as_ref().filter(|(_, version, _)| *version == revision) {
            return match answer { Ok(value) => State::Ready(value.clone()), Err(error) => State::Failed(error.clone()) };
        }
        if entry.running.is_none() {
            if !store.ui_attached() {
                let answer = load(store).map(Arc::new);
                let state = match &answer { Ok(value) => State::Ready(value.clone()), Err(error) => State::Failed(error.clone()) };
                entry.ready = Some((key, revision, answer));
                return state;
            }
            let (send, receive) = oneshot::channel();
            entry.running = Some((key, revision, receive));
            let (db, notify) = (store.db(), store.ui_waker());
            kernel::runtime::spawn(async move {
                let answer = kernel::runtime::spawn_blocking(move || {
                    let store = Store::with_db(db).map_err(|error| error.to_string())?;
                    load(&store).map(Arc::new)
                }).await.map_err(|error| error.to_string()).and_then(|result| result);
                let _ = send.send(answer);
                if let Some(notify) = notify { notify(); }
            });
        }
        match entry.ready.as_ref() {
            Some((_, _, Ok(value))) => State::Refreshing(value.clone()),
            _ => State::Loading,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{sync::mpsc, thread, time::Duration};

    #[test]
    fn blocked_preparation_leaves_ui_available_and_superseded_results_do_not_publish() {
        let store = Store::open(None, &[]).unwrap();
        let (notify, woke) = mpsc::channel();
        store.attach_ui(move || { let _ = notify.send(()); });
        let display = Snapshot::<u32, u32>::default();
        let ui = thread::current().id();
        let (entered, started) = mpsc::channel();
        let (release, held) = mpsc::channel();
        assert!(matches!(display.get(&store,1,vec![],move |_| {
            assert_ne!(thread::current().id(),ui,"parsing belongs to the background reader");
            entered.send(()).unwrap();
            held.recv_timeout(Duration::from_secs(5)).unwrap();
            Ok(1)
        }),State::Loading));
        started.recv_timeout(Duration::from_secs(5)).unwrap();
        // A new set of controls remains usable while the prior parse is held.
        assert!(matches!(display.get(&store,2,vec![],|_|panic!("only one preparation may run")),State::Loading));
        release.send(()).unwrap();
        let deadline = std::time::Instant::now()+Duration::from_secs(5);
        loop {
            assert!(std::time::Instant::now()<deadline);
            match display.get(&store,2,vec![],move |_| { assert_ne!(thread::current().id(),ui); Ok(2) }) {
                State::Ready(value) => { assert_eq!(*value,2,"old controls never receive the superseded answer"); break; }
                State::Loading | State::Refreshing(_) => {
                    woke.recv_timeout(Duration::from_secs(5)).unwrap();
                    // Native Signal handling consumes the coalesced display
                    // event before another draw can request its next snapshot.
                    store.poll_external();
                }
                State::Failed(error) => panic!("{error}"),
            }
        }
        assert!(matches!(display.get(&store,2,vec![],|_|panic!("redraw reuses the ready snapshot")),State::Ready(_)));
    }

    #[test]
    fn deterministic_readers_prepare_inline_and_cache_failures_by_input() {
        let store = Store::open(None,&[]).unwrap();
        let display = Snapshot::<u32,u32>::default();
        assert!(matches!(display.get(&store,1,vec![],|_|Err("missing draft".into())),State::Failed(_)));
        assert!(matches!(display.get(&store,1,vec![],|_|panic!("failure is stable until inputs change")),State::Failed(_)));
        assert_eq!(*display.get(&store,2,vec![],|_|Ok(2)).ready().unwrap(),2);
    }

    #[test]
    fn refreshing_keeps_the_complete_reading_and_skips_superseded_revisions() {
        let store = Store::open(None, &[]).unwrap();
        let display = Snapshot::<&str, u32, u32>::default();
        let original = display.get(&store,"same controls",1,|_|Ok(1)).ready().unwrap();
        let (notify,woke) = mpsc::channel();
        store.attach_ui(move || {let _ = notify.send(());});
        let (entered,started) = mpsc::channel();
        let (release,held) = mpsc::channel();
        let refreshing = display.get(&store,"same controls",2,move |_| {
            entered.send(()).unwrap();
            held.recv_timeout(Duration::from_secs(5)).unwrap();
            Ok(2)
        });
        assert!(matches!(refreshing,State::Refreshing(ref value) if Arc::ptr_eq(value,&original)));
        started.recv_timeout(Duration::from_secs(5)).unwrap();
        assert!(matches!(display.get(&store,"same controls",3,|_|panic!("one preparation at a time")),
            State::Refreshing(ref value) if Arc::ptr_eq(value,&original)));
        release.send(()).unwrap();
        let deadline = std::time::Instant::now()+Duration::from_secs(5);
        loop {
            assert!(std::time::Instant::now()<deadline);
            match display.get(&store,"same controls",3,|_|Ok(3)) {
                State::Ready(value) => {assert_eq!(*value,3);break;}
                State::Refreshing(value) => {
                    assert!(Arc::ptr_eq(&value,&original),"an obsolete completion cannot replace the visible reading");
                    woke.recv_timeout(Duration::from_secs(5)).unwrap();
                    store.poll_external();
                }
                _ => panic!("a refresh cannot clear the complete display"),
            }
        }
        // Different controls never borrow a ready value from the prior view.
        assert!(matches!(display.get(&store,"different controls",3,|_|Ok(4)),State::Loading));
    }
}
