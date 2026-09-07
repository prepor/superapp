//! Transient coordination for one store's panels and Telegram worker.
//!
//! The store's readers share this value through `Store::local`. A fixture
//! gets its own value even when it draws the same chat ids as the live app.
//! Commands (including login secrets) stay in memory and go through the
//! worker's inbox. Dropping that inbox disconnects the send side.

use std::sync::{mpsc, Arc, Mutex, MutexGuard};

use kernel::store::Store;

use super::model::{MsgId, PeerId};
use super::requests::PeerAction;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Forward {
    pub from: PeerId,
    pub ids: Vec<MsgId>,
}

#[derive(Default)]
pub struct Runtime {
    state: Mutex<State>,
}

#[derive(Default)]
struct State {
    sender: Option<mpsc::Sender<String>>,
    forward: Option<Forward>,
    play_next: Option<(PeerId, MsgId)>,
    loading: Vec<PeerId>,
    list_syncing: bool,
    wanted: Wanted,
    peer_actions: Vec<(PeerId, PeerAction)>,
    notices: Vec<(String, bool)>,
}

/// Work requested since the last worker pass. Deduplicated at enqueue time.
#[derive(Default)]
pub struct Wanted {
    pub chats: Vec<PeerId>,
    pub lines: Vec<(PeerId, MsgId)>,
    pub files: Vec<String>,
}

pub fn of(store: &Store) -> Arc<Runtime> {
    store.local()
}

impl Runtime {
    fn state(&self) -> MutexGuard<'_, State> {
        self.state.lock().expect("Telegram runtime")
    }

    /// Called by the worker on its first pass, never by a panel.
    pub fn connect(&self) -> mpsc::Receiver<String> {
        let (sender, receiver) = mpsc::channel();
        self.state().sender = Some(sender);
        receiver
    }

    /// Enqueue a command; success means queued, not acknowledged by Telegram.
    pub fn send(&self, request: &str) -> bool {
        self.state()
            .sender
            .as_ref()
            .is_some_and(|sender| sender.send(request.to_string()).is_ok())
    }

    /// Serialize profile actions for each person until their reply arrives.
    pub fn send_peer_action(&self, peer: PeerId, action: PeerAction) -> bool {
        let mut state = self.state();
        if state.peer_actions.iter().any(|(id, _)| *id == peer) {
            return false;
        }
        if state.sender.as_ref().is_none_or(|s| s.send(action.request(peer)).is_err()) {
            return false;
        }
        state.peer_actions.push((peer, action));
        true
    }

    pub fn peer_action_pending(&self, peer: PeerId) -> bool {
        self.state().peer_actions.iter().any(|(id, _)| *id == peer)
    }

    pub fn finish_peer_action(&self, peer: PeerId, action: PeerAction) {
        self.state().peer_actions.retain(|p| *p != (peer, action));
    }

    pub fn notice(&self, text: String, error: bool) {
        self.state().notices.push((text, error));
    }

    pub fn take_notices(&self) -> Vec<(String, bool)> {
        std::mem::take(&mut self.state().notices)
    }

    pub fn carry_forward(&self, from: PeerId, ids: Vec<MsgId>) {
        self.state().forward = Some(Forward { from, ids });
    }

    pub fn pending_forward(&self) -> Option<Forward> {
        self.state().forward.clone()
    }

    pub fn take_forward(&self) -> Option<Forward> {
        self.state().forward.take()
    }

    pub fn play_on_open(&self, chat: PeerId, id: MsgId) {
        self.state().play_next = Some((chat, id));
    }

    pub fn take_play_on_open(&self, chat: PeerId, id: MsgId) -> bool {
        let mut state = self.state();
        if state.play_next != Some((chat, id)) {
            return false;
        }
        state.play_next = None;
        true
    }

    pub fn loading(&self, chat: PeerId) -> bool {
        self.state().loading.contains(&chat)
    }

    pub fn set_loading(&self, chat: PeerId, on: bool) {
        let mut state = self.state();
        if on {
            push_unique(&mut state.loading, chat);
        } else {
            state.loading.retain(|c| *c != chat);
        }
    }

    pub fn list_syncing(&self) -> bool {
        self.state().list_syncing
    }

    pub fn set_list_syncing(&self, on: bool) {
        self.state().list_syncing = on;
    }

    #[cfg(any(feature = "tdlib", test))]
    pub fn want_history(&self, chat: PeerId) {
        let mut state = self.state();
        push_unique(&mut state.loading, chat);
        push_unique(&mut state.wanted.chats, chat);
    }

    pub fn want_line(&self, chat: PeerId, id: MsgId) {
        push_unique(&mut self.state().wanted.lines, (chat, id));
    }

    pub fn want_file(&self, remote_id: &str) {
        push_unique(&mut self.state().wanted.files, remote_id.to_string());
    }

    pub fn take_wanted(&self) -> Wanted {
        std::mem::take(&mut self.state().wanted)
    }
}

fn push_unique<T: PartialEq>(values: &mut Vec<T>, value: T) {
    if !values.contains(&value) {
        values.push(value);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn coordination_follows_the_database_across_threads() {
        let store = Store::open(None, &[]).unwrap();
        let state = of(&store);
        state.carry_forward(7, vec![42]);
        state.play_on_open(7, 42);
        state.set_list_syncing(true);
        for _ in 0..2 {
            state.want_history(7);
            state.want_line(7, 42);
            state.want_file("remote-photo");
        }

        let db = store.db();
        std::thread::spawn(move || {
            let reader = Store::with_db(db).unwrap();
            let state = of(&reader);
            assert_eq!(
                state.pending_forward(),
                Some(Forward {
                    from: 7,
                    ids: vec![42]
                })
            );
            assert!(state.loading(7));
            assert!(state.list_syncing());
            let wanted = state.take_wanted();
            assert_eq!(wanted.chats, vec![7]);
            assert_eq!(wanted.lines, vec![(7, 42)]);
            assert_eq!(wanted.files, vec!["remote-photo"]);
            state.set_loading(7, false);
            state.set_list_syncing(false);
        })
        .join()
        .unwrap();
        assert!(!state.loading(7));
        assert!(!state.list_syncing());
        assert!(state.take_wanted().chats.is_empty());

        let fixture = Store::open(None, &[]).unwrap();
        let other = of(&fixture);
        assert!(other.take_forward().is_none());
        assert!(!other.take_play_on_open(7, 42));
        assert!(!other.loading(7));
        assert!(other.take_wanted().files.is_empty());
        assert!(state.take_play_on_open(7, 42));
        assert!(!state.take_play_on_open(7, 42));

        let weak = Arc::downgrade(&state);
        drop(state);
        drop(store);
        assert!(
            weak.upgrade().is_none(),
            "coordination does not outlive its store"
        );
    }
}
