//! A visible row is subscribed only after its snapshot has loaded. Failed,
//! missing and timed-out snapshots stay eligible for a later worker pass.

use std::collections::{BTreeMap, HashMap};

use super::*;

const PATIENCE: f64 = 30.0;
const RETRY_GAP: f64 = 2.0;

#[derive(Default)]
pub(super) struct Views {
    chats: BTreeMap<PeerId, BTreeMap<MsgId, Row>>,
    batches: HashMap<u64, Batch>,
    next: u64,
    online: bool,
    revision: u64,
    snapshots: HashMap<u64, (u64, f64)>,
    snapshot_revision: Option<u64>,
}

#[derive(Default)]
struct Row {
    loaded: bool,
    pending: Option<u64>,
    retry_at: f64,
    dirty: bool,
    previous: Option<u64>,
    interaction: Option<(u64, updates::Interaction)>,
}

struct Batch {
    chat: PeerId,
    ids: Vec<MsgId>,
    due: f64,
}

impl Views {
    pub(super) fn reset(&mut self) {
        self.chats.clear();
        self.batches.clear();
        self.online = false;
        self.snapshots.clear();
        self.snapshot_revision = None;
        // Keep request ids unique across reauthorization as well.
    }

    fn finish(&mut self, request: u64, retry_at: f64) -> Option<Batch> {
        let batch = self.batches.remove(&request)?;
        if let Some(rows) = self.chats.get_mut(&batch.chat) {
            for id in &batch.ids {
                if let Some(row) = rows.get_mut(id).filter(|r| r.pending == Some(request)) {
                    row.pending = None;
                    row.previous = Some(request);
                    row.retry_at = retry_at;
                }
            }
        }
        Some(batch)
    }
}

impl<T: Td> Account<T> {
    pub(super) fn track_snapshot(&self, w: &World, request: &Value) {
        if !matches!(request["@type"].as_str(), Some("getMessages" | "getMessage" | "getChatHistory"
            | "getForumTopicHistory" | "getForumTopics" | "getForumTopic" | "searchChatMessages")) { return; }
        if let Some(id) = request["@extra"]["operation"].as_u64() {
            let mut state = self.viewed.borrow_mut();
            let revision = state.revision;
            state.snapshots.insert(id, (revision, w.now() + PATIENCE));
        }
    }

    pub(super) fn begin_snapshot(&self, response: &Value) {
        let mut state = self.viewed.borrow_mut();
        state.snapshot_revision = response["@extra"]["operation"].as_u64()
            .and_then(|id| state.snapshots.remove(&id)).map(|(revision, _)| revision);
    }

    pub(super) fn end_snapshot(&self) {
        self.viewed.borrow_mut().snapshot_revision = None;
    }

    pub(super) fn sync_views(&self, w: &World) {
        let next = runtime::of(w.store()).visible_messages();
        let now = w.now();
        let mut state = self.viewed.borrow_mut();
        state.snapshots.retain(|_, (_, due)| now < *due);
        let mut requests = Vec::new();
        if !next.is_empty() && !state.online {
            requests.push(set_online(true));
            state.online = true;
        }
        state.chats.retain(|chat, _| {
            if next.contains_key(chat) { return true; }
            requests.push(chat_open(*chat, false));
            false
        });
        if next.is_empty() && state.online {
            requests.push(set_online(false));
            state.online = false;
        }
        let mut new_rows = false;
        for (chat, ids) in &next {
            let rows = state.chats.entry(*chat).or_insert_with(|| {
                requests.push(chat_open(*chat, true));
                BTreeMap::new()
            });
            rows.retain(|id, _| ids.contains(id));
            for id in ids {
                rows.entry(*id).or_insert_with(|| {
                    new_rows = true;
                    Row::default()
                });
            }
        }
        // A response issued for a previous visit is older than this view,
        // even when no live interaction update arrived between visits.
        if new_rows { state.revision += 1; }
        let retired: Vec<_> = state.batches.iter().filter(|(_, batch)| {
            now >= batch.due || !next.get(&batch.chat).is_some_and(|ids| batch.ids.iter().any(|id| ids.contains(id)))
        }).map(|(id, _)| *id).collect();
        for id in retired {
            if let Some(batch) = state.finish(id, now) {
                runtime::of(w.store()).operations.retire_context(&format!("visible:{}:{id}", batch.chat));
            }
        }
        let fresh: Vec<_> = state.chats.iter().map(|(chat, rows)| {
            (*chat, rows.iter().filter(|(_, r)| !r.loaded && r.pending.is_none() && now >= r.retry_at)
                .map(|(id, _)| *id).collect::<Vec<_>>())
        }).collect();
        for (chat, ids) in fresh {
            for ids in ids.chunks(100) {
                state.next += 1;
                let request = state.next;
                for id in ids {
                    let row = state.chats.get_mut(&chat).unwrap().get_mut(id).unwrap();
                    if let Some(previous) = row.previous.take() {
                        runtime::of(w.store()).operations.retire_context(&format!("visible:{chat}:{previous}"));
                    }
                    row.pending = Some(request);
                    row.dirty = false;
                }
                state.batches.insert(request, Batch { chat, ids: ids.to_vec(), due: now + PATIENCE });
                requests.push(get_visible_messages(chat, ids, request));
            }
        }
        drop(state);
        for request in requests { self.send(w, &request); }
    }

    pub(super) fn visible_reactions_changed(&self, chat: Option<PeerId>) {
        for (id, rows) in &mut self.viewed.borrow_mut().chats {
            if chat.is_none_or(|chat| chat == *id) {
                for row in rows.values_mut() {
                    row.loaded = false;
                    row.dirty = true;
                }
            }
        }
    }

    pub(super) fn on_visible_error(&self, w: &World, v: &Value) -> bool {
        let Some((chat, request)) = v["@extra"].as_str().and_then(parse_visible_extra) else { return false; };
        let mut state = self.viewed.borrow_mut();
        if state.batches.get(&request).is_some_and(|b| b.chat == chat) {
            let delay = (v["code"] == 429).then(|| retry_after(v["message"].as_str().unwrap_or(""))).flatten()
                .map(|n| n + 1.0).unwrap_or(RETRY_GAP);
            state.finish(request, w.now() + delay);
        }
        true
    }

    pub(super) fn on_visible_messages(&self, w: &World, v: &Value, chat: PeerId, request: u64) {
        let mut state = self.viewed.borrow_mut();
        if state.batches.get(&request).is_none_or(|b| b.chat != chat) { return; }
        let eligible: Vec<_> = state.chats.get(&chat).into_iter().flat_map(|rows| rows.iter())
            .filter(|(_, row)| row.pending == Some(request)).map(|(id, _)| *id).collect();
        state.finish(request, w.now() + RETRY_GAP);
        drop(state);
        let visible = runtime::of(w.store()).visible_messages();
        let messages: Vec<_> = v["messages"].as_array().into_iter().flatten()
            .filter(|v| v["chat_id"] == chat && v["id"].as_i64().is_some_and(|id| eligible.contains(&id)
                && visible.get(&chat).is_some_and(|ids| ids.contains(&id))))
            .filter_map(|v| self.message(v)).collect();
        let ids: Vec<_> = messages.iter().map(|m| m.id).collect();
        if messages.is_empty() { return; }
        let written = w.store().write(move |c| {
            ensure_peer(c, chat)?;
            model::ensure_chat_tx(c, chat)?;
            for sender in messages.iter().filter_map(|m| m.sender) { ensure_peer(c, sender)?; }
            project_messages(c, &messages)?;
            apply_read_outbox(c, chat)
        });
        if written.is_ok() {
            if let Some(rows) = self.viewed.borrow_mut().chats.get_mut(&chat) {
                for id in &ids {
                    if let Some(row) = rows.get_mut(id) {
                        row.loaded = !row.dirty;
                        row.retry_at = 0.0;
                    }
                }
            }
            self.send(w, &observe_messages(chat, &ids));
        }
        self.filed(w, "visible messages", written);
    }

    pub(super) fn remember_interaction(&self, interaction: &updates::Interaction) {
        let mut state = self.viewed.borrow_mut();
        state.revision += 1;
        let revision = state.revision;
        if let Some(row) = state.chats.get_mut(&interaction.chat).and_then(|rows| rows.get_mut(&interaction.id)) {
            row.interaction = Some((revision, interaction.clone()));
        }
    }

    /// Snapshots requested before a live interaction update cannot undo it.
    /// A later fetch (including after an add) can advance the counts. TDLib
    /// may omit cached reactions while chat metadata loads; only an explicit
    /// interaction update clears a known count during this subscription.
    /// Closing/reopening the view forgets this overlay and reconciles SQLite.
    pub(super) fn message(&self, value: &Value) -> Option<IncomingMessage> {
        let mut msg = updates::message(value)?;
        let mut state = self.viewed.borrow_mut();
        let snapshot = state.snapshot_revision;
        if let Some(row) = state.chats.get_mut(&msg.chat).and_then(|rows| rows.get_mut(&msg.id)) {
            if let Some((revision, info)) = &row.interaction {
                if snapshot.is_none_or(|sent| sent < *revision) {
                    msg.views = info.views;
                    msg.comments = info.comments;
                    msg.reactions.clone_from(&info.reactions);
                } else if msg.reactions.is_none() {
                    msg.reactions.clone_from(&info.reactions);
                }
            }
            if let Some(revision) = snapshot.filter(|sent| row.interaction.as_ref().is_none_or(|(revision, _)| sent >= revision)) {
                row.interaction = Some((revision, updates::Interaction { chat: msg.chat, id: msg.id,
                    views: msg.views, comments: msg.comments, reactions: msg.reactions.clone() }));
            }
        }
        Some(msg)
    }
}
