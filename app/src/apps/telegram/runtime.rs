//! Transient coordination for one store's panels and Telegram worker.
//!
//! The store's readers share this value through `Store::local`. A fixture
//! gets its own value even when it draws the same chat ids as the live app.
//! Commands (including login secrets) stay in memory and go through the
//! worker's inbox. Closing the session or dropping that inbox disconnects
//! the send side and releases actions whose replies can no longer arrive.

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::sync::{mpsc, Arc, Mutex, MutexGuard, Weak};

use kernel::effect::World;
use kernel::store::Store;

use super::model::{DownloadProgress, MsgId, PeerId};
use super::requests::PeerAction;

/// Chosen when the world is created, independently of worker availability.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Delivery {
    Live,
    Demo,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Forward {
    pub from: PeerId,
    pub ids: Vec<MsgId>,
}

/// A reaction picker waits in memory for either the available emoji or the
/// acknowledgement of its choice. Closing the picker drops the reply.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ReactionResult {
    Waiting,
    Choices(Vec<String>),
    Unavailable(String),
    Added,
    Error(String),
}

pub type ReactionReply = Arc<Mutex<Option<ReactionResult>>>;

/// Cached panels draw immediately; automatic network reads wait until a
/// viewport survives a brief arrow-key preview.
pub const VIEW_SETTLE: f64 = 0.35;

/// A widget owns its viewport; dropping it cancels work that has not started.
pub struct Viewport {
    chat: PeerId,
    topic: Option<i64>,
    pub(super) ids: Vec<MsgId>,
    ready_at: f64,
    files: HashMap<String, bool>,
}

pub type MessageView = Arc<Mutex<Viewport>>;

pub fn show_messages(view: &mut Option<MessageView>, world: &World, chat: PeerId, topic: Option<i64>, ids: Vec<MsgId>) {
    if (ids.is_empty() && topic.is_none()) || !world.with_cap::<Delivery, _>(|d| *d == Delivery::Live).unwrap_or(false) {
        *view = None;
        return;
    }
    if let Some(view) = view {
        let mut view = view.lock().expect("visible messages");
        if (view.chat, view.topic) != (chat, topic) {
            view.ready_at = world.now() + VIEW_SETTLE;
            view.files.clear();
        }
        view.chat = chat;
        view.topic = topic;
        view.ids = ids;
    } else {
        *view = Some(of(world.store()).watch_messages(chat, topic, ids, world.now()));
    }
}

pub fn view_settled(view: &Option<MessageView>, now: f64) -> bool {
    view.as_ref().is_some_and(|view| now >= view.lock().expect("visible messages").ready_at)
}

pub fn want_view_file(view: &Option<MessageView>, remote_id: &str) {
    if let Some(view) = view {
        let mut view = view.lock().expect("visible messages");
        if !view.files.contains_key(remote_id) { view.files.insert(remote_id.to_string(), false); }
    }
}

#[derive(Default)]
pub struct Runtime {
    state: Mutex<State>,
    pub operations: super::operations::Tracker,
}

#[derive(Default)]
struct State {
    sender: Option<mpsc::Sender<String>>,
    connection: u64,
    next_action: u64,
    forward: Option<Forward>,
    loading: Vec<(PeerId, i64)>,
    topic_lists: std::collections::HashMap<PeerId, Result<bool, String>>,
    mentions_loading: Vec<PeerId>,
    mentions_failed: Vec<PeerId>,
    connection_error: Option<String>,
    list_syncing: bool,
    connection_note: Option<String>,
    downloads: HashMap<String, DownloadProgress>,
    connection_status: Option<String>,
    wanted: Wanted,
    next_reaction: u64,
    reactions: HashMap<u64, Weak<Mutex<Option<ReactionResult>>>>,
    demo_reactions: HashSet<(PeerId, MsgId, String)>,
    peer_actions: Vec<(PeerId, PeerAction, u64)>,
    notices: Vec<(String, bool)>,
    views: Vec<Weak<Mutex<Viewport>>>,
}

impl State {
    fn disconnect(&mut self) {
        self.sender = None;
        for (_, reply) in self.reactions.drain() {
            if let Some(reply) = reply.upgrade() {
                *reply.lock().expect("reaction reply") = Some(ReactionResult::Error("Telegram is disconnected".into()));
            }
        }
        if !self.peer_actions.is_empty() {
            self.peer_actions.clear();
            self.notices.push(("Telegram disconnected before confirming pending user actions".to_string(), true));
        }
    }
}

/// One worker's command connection. A replaced worker can neither drain old
/// commands nor disconnect its successor when it stops.
pub struct Inbox {
    receiver: mpsc::Receiver<String>,
    runtime: Weak<Runtime>,
    connection: u64,
}

impl Inbox {
    pub fn try_recv(&self) -> Result<String, mpsc::TryRecvError> {
        let runtime = self.runtime.upgrade().ok_or(mpsc::TryRecvError::Disconnected)?;
        let state = runtime.state();
        if state.connection != self.connection || state.sender.is_none() {
            return Err(mpsc::TryRecvError::Disconnected);
        }
        self.receiver.try_recv()
    }

    pub fn try_iter(&self) -> impl Iterator<Item = String> + '_ {
        std::iter::from_fn(|| self.try_recv().ok())
    }
}

impl Drop for Inbox {
    fn drop(&mut self) {
        if let Some(runtime) = self.runtime.upgrade() {
            let mut state = runtime.state();
            if state.connection == self.connection {
                state.disconnect();
            }
        }
    }
}

/// Work requested since the last worker pass. Deduplicated at enqueue time.
#[derive(Default)]
pub struct Wanted {
    pub chats: Vec<PeerId>,
    pub mentions: Vec<PeerId>,
    pub topic_chats: Vec<(PeerId, i64)>,
    pub topic_lists: Vec<PeerId>,
    pub files: Vec<String>,
}

pub fn of(store: &Store) -> Arc<Runtime> {
    store.local()
}

impl Runtime {
    fn state(&self) -> MutexGuard<'_, State> {
        self.state.lock().expect("Telegram runtime")
    }

    pub fn watch_messages(&self, chat: PeerId, topic: Option<i64>, ids: Vec<MsgId>, now: f64) -> MessageView {
        let view = Arc::new(Mutex::new(Viewport { chat, topic, ids, ready_at: now + VIEW_SETTLE, files: HashMap::new() }));
        let mut state = self.state();
        state.views.retain(|view| view.strong_count() > 0);
        state.views.push(Arc::downgrade(&view));
        view
    }

    /// Combine duplicate panels before the worker subscribes to a chat.
    pub fn visible_messages(&self, now: f64) -> BTreeMap<PeerId, Vec<MsgId>> {
        let mut out: BTreeMap<PeerId, Vec<MsgId>> = BTreeMap::new();
        self.state().views.retain(|view| {
            let Some(view) = view.upgrade() else { return false };
            let view = view.lock().expect("visible messages");
            if now >= view.ready_at && !view.ids.is_empty() {
                out.entry(view.chat).or_default().extend(&view.ids);
            }
            true
        });
        for ids in out.values_mut() {
            ids.sort_unstable();
            ids.dedup();
        }
        out
    }

    /// Empty transcripts need history too. Line cards subscribe to messages
    /// without starting a walk of the whole conversation.
    pub fn visible_history(&self, now: f64) -> BTreeSet<(PeerId, i64)> {
        let mut out = BTreeSet::new();
        self.state().views.retain(|view| {
            let Some(view) = view.upgrade() else { return false };
            let view = view.lock().expect("visible messages");
            if now >= view.ready_at {
                if let Some(topic) = view.topic { out.insert((view.chat, topic)); }
            }
            true
        });
        out
    }

    /// Automatic thumbnails belong to their viewport, unlike an explicit
    /// download command. Drop pending files when that viewport goes away.
    pub fn take_view_files(&self, now: f64) -> Vec<String> {
        let mut out = Vec::new();
        self.state().views.retain(|view| {
            let Some(view) = view.upgrade() else { return false };
            let mut view = view.lock().expect("visible messages");
            if now >= view.ready_at {
                for (rid, sent) in &mut view.files {
                    if !*sent { push_unique(&mut out, rid.clone()); }
                    *sent = true;
                }
            }
            true
        });
        out
    }

    /// Called by the worker on its first pass, never by a panel.
    pub fn connect(self: &Arc<Self>) -> Inbox {
        let (sender, receiver) = mpsc::channel();
        let mut state = self.state();
        state.disconnect();
        state.connection += 1;
        state.sender = Some(sender);
        Inbox { receiver, runtime: Arc::downgrade(self), connection: state.connection }
    }

    /// TDLib is logging out or closing. Keep the old inbox retired until a
    /// new account connects, and leave durable peer state to server updates.
    pub fn disconnect(&self) {
        self.state().disconnect();
    }

    /// Enqueue a command; success means queued, not acknowledged by Telegram.
    pub fn send(&self, request: &str) -> bool {
        let state = self.state();
        if state.connection_error.is_some() {
            return false;
        }
        let Some(sender) = state.sender.as_ref() else {
            return false;
        };
        sender.send(self.operations.track(request)).is_ok()
    }

    pub fn has_worker(&self) -> bool {
        // An inbox that has disconnected still belongs to a live account;
        // its panels must not fall back to simulated fixture actions.
        self.state().connection != 0
    }

    pub fn connection(&self) -> Option<String> {
        self.state().connection_status.clone()
    }

    pub fn set_connection(&self, state: &str) {
        self.state().connection_status = match state {
            "connectionStateReady" => None,
            "connectionStateWaitingForNetwork" => Some("waiting for network…".into()),
            "connectionStateConnectingToProxy" => Some("connecting to proxy…".into()),
            "connectionStateUpdating" => Some("updating Telegram…".into()),
            _ => Some("connecting to Telegram…".into()),
        };
    }

    /// A connection failure belongs to this process, even when another app
    /// window shares its database and is successfully signed in.
    pub fn connection_error(&self) -> Option<String> {
        self.state().connection_error.clone()
    }

    pub fn set_connection_error(&self, error: Option<String>) {
        let mut state = self.state();
        if error.is_some() {
            state.list_syncing = false;
        }
        state.connection_error = error;
    }

    /// Serialize profile actions for each person until their reply arrives.
    pub fn send_peer_action(&self, peer: PeerId, action: PeerAction) -> bool {
        let mut state = self.state();
        if state.peer_actions.iter().any(|(id, _, _)| *id == peer) {
            return false;
        }
        state.next_action += 1;
        let id = state.next_action;
        if state.sender.as_ref().is_none_or(|s| s.send(action.request(peer, id)).is_err()) {
            return false;
        }
        state.peer_actions.push((peer, action, id));
        true
    }

    pub fn peer_action_pending(&self, peer: PeerId) -> bool {
        self.state().peer_actions.iter().any(|(id, _, _)| *id == peer)
    }

    /// Only this attempt's reply may finish it; late replies after a
    /// disconnect cannot complete a retry or change its projected state.
    pub fn finish_peer_action(&self, peer: PeerId, action: PeerAction, id: u64) -> bool {
        let mut state = self.state();
        let Some(index) = state.peer_actions.iter().position(|p| *p == (peer, action, id)) else {
            return false;
        };
        state.peer_actions.remove(index);
        true
    }

    pub fn notice(&self, text: String, error: bool) {
        self.state().notices.push((text, error));
    }

    pub fn take_notices(&self) -> Vec<(String, bool)> {
        std::mem::take(&mut self.state().notices)
    }

    /// Each request has its own reply, even when two panels show the same
    /// message. Late answers cannot change a newer picker or another store.
    pub fn await_reaction(&self) -> (u64, ReactionReply) {
        let reply = Arc::new(Mutex::new(None));
        let mut state = self.state();
        state.reactions.retain(|_, reply| reply.strong_count() > 0);
        state.next_reaction += 1;
        let id = state.next_reaction;
        state.reactions.insert(id, Arc::downgrade(&reply));
        (id, reply)
    }

    pub fn finish_reaction(&self, id: u64, result: ReactionResult) {
        // Available choices are a subscription: TDLib can change them after
        // chat metadata or message interactions arrive. The panel owns its
        // lifetime, including after an empty result.
        let reply = {
            let mut state = self.state();
            if matches!(result, ReactionResult::Waiting | ReactionResult::Choices(_) | ReactionResult::Unavailable(_)) {
                state.reactions.get(&id).and_then(Weak::upgrade)
            } else {
                state.reactions.remove(&id).and_then(|r| r.upgrade())
            }
        };
        if let Some(reply) = reply {
            *reply.lock().expect("reaction reply") = Some(result);
            self.operations.changed();
        }
    }

    pub fn reaction_alive(&self, id: u64) -> bool {
        self.state().reactions.get(&id).is_some_and(|r| r.strong_count() > 0)
    }

    pub fn demo_reacted(&self, chat: PeerId, msg: MsgId, emoji: &str) -> bool {
        self.state().demo_reactions.contains(&(chat, msg, emoji.to_string()))
    }

    pub fn remember_demo_reaction(&self, chat: PeerId, msg: MsgId, emoji: &str) {
        self.state().demo_reactions.insert((chat, msg, emoji.to_string()));
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

    #[cfg(test)]
    pub fn loading(&self, chat: PeerId) -> bool {
        self.loading_in(chat, 0)
    }

    pub fn loading_in(&self, chat: PeerId, topic: i64) -> bool {
        self.state().loading.contains(&(chat, topic))
    }

    #[cfg(test)]
    pub fn set_loading(&self, chat: PeerId, on: bool) {
        self.set_loading_in(chat, 0, on);
    }

    pub fn set_loading_in(&self, chat: PeerId, topic: i64, on: bool) {
        let mut state = self.state();
        if on {
            push_unique(&mut state.loading, (chat, topic));
        } else {
            state.loading.retain(|c| *c != (chat, topic));
        }
    }

    pub fn list_syncing(&self) -> bool {
        self.state().list_syncing
    }

    pub fn set_list_syncing(&self, on: bool) {
        self.state().list_syncing = on;
    }

    /// This client's connection state, independent of another process's
    /// writes to the shared session row. None once this client is ready.
    pub fn connection_note(&self) -> Option<String> {
        self.state().connection_note.clone()
    }

    pub fn set_connection_note(&self, note: Option<&str>) {
        self.state().connection_note = note.map(str::to_string);
    }

    /// Progress follows the same `tg:` key as the media cache, so a clip and
    /// its poster never share counts. Finished or stopped downloads are removed.
    pub fn set_download(&self, reference: &str, progress: Option<DownloadProgress>) {
        let mut state = self.state();
        if let Some(progress) = progress {
            state.downloads.insert(reference.to_string(), progress);
        } else {
            state.downloads.remove(reference);
        }
    }

    pub fn download(&self, reference: &str) -> Option<DownloadProgress> {
        self.state().downloads.get(reference).copied()
    }

    #[cfg(test)]
    pub fn want_history(&self, chat: PeerId) {
        let mut state = self.state();
        push_unique(&mut state.loading, (chat, 0));
        push_unique(&mut state.wanted.chats, chat);
    }

    #[cfg(test)]
    pub fn want_topic_history(&self, chat: PeerId, topic: i64) {
        let mut state = self.state();
        push_unique(&mut state.loading, (chat, topic));
        push_unique(&mut state.wanted.topic_chats, (chat, topic));
    }

    pub fn refresh_topics(&self, chat: PeerId) {
        let mut state = self.state();
        if state.sender.is_none() || state.connection_error.is_some()
            || state.topic_lists.get(&chat) == Some(&Ok(true))
        {
            return;
        }
        push_unique(&mut state.wanted.topic_lists, chat);
        state.topic_lists.insert(chat, Ok(true));
    }

    pub fn topics_loaded(&self, chat: PeerId, status: Result<bool, String>) {
        self.state().topic_lists.insert(chat, status);
    }

    pub fn topic_list_queued(&self, chat: PeerId) -> bool {
        self.state().wanted.topic_lists.contains(&chat)
    }

    pub fn topics_status(&self, chat: PeerId) -> Result<bool, String> {
        let state = self.state();
        if let Some(error) = &state.connection_error {
            return Err(error.clone());
        }
        state.topic_lists.get(&chat).cloned().unwrap_or(Ok(false))
    }

    pub fn want_mentions(&self, chat: PeerId) {
        let mut state = self.state();
        // Offline scenes must not acquire a loading state with no worker.
        if state.sender.is_some() {
            push_unique(&mut state.wanted.mentions, chat);
            push_unique(&mut state.mentions_loading, chat);
            state.mentions_failed.retain(|c| *c != chat);
        }
    }

    pub fn mentions_status(&self, chat: Option<PeerId>) -> (bool, bool) {
        let state = self.state();
        let matches = |peers: &[PeerId]| peers.iter().any(|p| chat.is_none_or(|c| c == *p));
        (matches(&state.mentions_loading), matches(&state.mentions_failed))
    }

    pub fn set_mentions_status(&self, chat: PeerId, loading: bool, failed: bool) {
        let mut state = self.state();
        state.mentions_loading.retain(|c| *c != chat);
        state.mentions_failed.retain(|c| *c != chat);
        if loading {
            push_unique(&mut state.mentions_loading, chat);
        }
        if failed {
            push_unique(&mut state.mentions_failed, chat);
        }
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
    fn replacing_a_worker_releases_peer_actions_and_retires_its_inbox() {
        let state = Arc::new(Runtime::default());
        let old = state.connect();
        assert!(state.send_peer_action(7, PeerAction::Block));
        let new = state.connect();
        assert!(!state.peer_action_pending(7));
        assert!(old.try_recv().is_err(), "the retired inbox cannot send a cancelled request");
        assert!(state.send_peer_action(7, PeerAction::Block));
        drop(old);
        assert!(state.peer_action_pending(7), "dropping an old inbox cannot reset its replacement");
        assert!(new.try_recv().is_ok());
    }

    #[test]
    fn coordination_follows_the_database_across_threads() {
        let store = Store::open(None, &[]).unwrap();
        let state = of(&store);
        state.carry_forward(7, vec![42]);
        state.set_list_syncing(true);
        state.set_connection_note(Some("connecting to Telegram…"));
        let progress = DownloadProgress {
            downloaded: 1024,
            total: Some(4096),
            estimated: false,
        };
        state.set_download("tg:photo", Some(progress));
        for _ in 0..2 {
            state.want_history(7);
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
            assert_eq!(state.connection_note().as_deref(), Some("connecting to Telegram…"));
            assert_eq!(state.download("tg:photo"), Some(progress));
            let wanted = state.take_wanted();
            assert_eq!(wanted.chats, vec![7]);
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
        assert!(!other.loading(7));
        assert_eq!(other.connection_note(), None);
        assert_eq!(other.download("tg:photo"), None);
        assert_eq!(state.download("tg:photo"), Some(progress));
        assert!(other.take_wanted().files.is_empty());

        let weak = Arc::downgrade(&state);
        drop(state);
        drop(store);
        assert!(
            weak.upgrade().is_none(),
            "coordination does not outlive its store"
        );
    }
}
