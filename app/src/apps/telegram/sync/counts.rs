//! Reconcile the durable reaction projection, independently of message loads.
//!
//! getMessage/getMessages may return TDLib's cache. Instead,
//! searchChatMessages goes to the server with our message database disabled,
//! using the message's sender, topic, text or media type as its search criterion.
//! It starts at the exact message. Missing results are not
//! empty reactions. Before accepting null, load reaction metadata and repeat
//! the server read, rejecting replies across metadata changes or newer counts.

use std::collections::{BTreeMap, BTreeSet};

use super::*;
use super::super::reaction_state;

type Key = (PeerId, MsgId);
const PATIENCE: f64 = 30.0;
const GAP: f64 = 1.0;
// A fallback for missed pushes, separate from initial loads and post-add
// checks. Sweeping every minute keeps searching even a settled viewport.
const RECHECK: f64 = 5.0 * 60.0;

#[derive(Default)]
pub(super) struct Counts {
    next: u64,
    epoch: u64,
    pending: Option<Attempt>,
    retries: BTreeMap<Key, (f64, u32)>,
    urgent: BTreeSet<Key>,
    not_before: f64,
    next_sweep: f64,
    cursor: Option<Key>,
}

struct Attempt {
    id: u64,
    key: Key,
    revision: i64,
    epoch: u64,
    phase: &'static str,
    due: f64,
}

impl Attempt {
    fn context(&self) -> String { format!("reaction_count:{}:{}", self.id, self.phase) }
}

impl Counts {
    pub(super) fn reset(&mut self, w: &World) {
        self.epoch += 1;
        if let Some(p) = self.pending.take() { runtime::of(w.store()).operations.retire_context(&p.context()); }
        self.retries.clear();
        self.urgent.clear();
        self.not_before = 0.0;
        self.next_sweep = 0.0;
        self.cursor = None;
    }
}

impl<T: Td> Account<T> {
    pub(super) fn refresh_counts(&self, w: &World, chat: PeerId, ids: &[MsgId]) {
        let ids = ids.to_vec();
        self.filed(w, "refresh reactions", w.store().write(move |c| {
            for id in ids { reaction_state::refresh(c, chat, id)?; }
            Ok(())
        }));
    }

    pub(super) fn counts_metadata_changed(&self, w: &World, chat: Option<PeerId>) {
        let mut state = self.counts.borrow_mut();
        // Metadata updates can follow the null interaction updates they
        // caused. Retire the whole attempt, including its readiness check.
        state.retries.retain(|(c, _), _| chat.is_some_and(|chat| chat != *c));
        if chat.is_none() || state.pending.as_ref().is_some_and(|p| Some(p.key.0) == chat) {
            state.epoch += 1;
            if let Some(pending) = state.pending.take() {
                runtime::of(w.store()).operations.retire_context(&pending.context());
            }
        }
        drop(state);
        for (c, ids) in runtime::of(w.store()).visible_messages() {
            if chat.is_none_or(|chat| chat == c) { self.refresh_counts(w, c, &ids); }
        }
    }

    pub(super) fn counts_after_add(&self, w: &World, chat: PeerId, message: MsgId) {
        self.refresh_counts(w, chat, &[message]);
        let mut state = self.counts.borrow_mut();
        state.urgent.insert((chat, message));
        state.retries.remove(&(chat, message));
        // A read sent before the add cannot confirm its result.
        if state.pending.as_ref().is_some_and(|p| p.key == (chat, message)) {
            let pending = state.pending.take().unwrap();
            runtime::of(w.store()).operations.retire_context(&pending.context());
        }
    }

    pub(super) fn sync_counts(&self, w: &World) {
        if !self.auth_ready.get() { return; }
        let mut state = self.counts.borrow_mut();
        let visible = runtime::of(w.store()).visible_messages();
        // Push updates provide the fast path. Periodic reconciliation also
        // repairs missed updates and TDLib's limited set of polled messages.
        // Counts stay displayed throughout; this expires freshness, not data.
        if w.now() >= state.next_sweep {
            for (chat, ids) in &visible { self.refresh_counts(w, *chat, ids); }
            state.next_sweep = w.now() + RECHECK;
        }
        state.urgent.retain(|&(chat, message)| reaction_state::state(w.store().conn(), chat, message)
            .is_ok_and(|s| s.refresh));
        let mut wanted: BTreeSet<_> = visible.into_iter()
            .flat_map(|(chat, ids)| ids.into_iter().map(move |id| (chat, id))).collect();
        wanted.extend(&state.urgent);
        state.retries.retain(|key, _| wanted.contains(key));
        if state.pending.as_ref().is_some_and(|p| w.now() >= p.due || !wanted.contains(&p.key)
            || reaction_state::state(w.store().conn(), p.key.0, p.key.1)
                .is_ok_and(|s| s.revision != p.revision || !s.refresh)) {
            let p = state.pending.take().unwrap();
            runtime::of(w.store()).operations.retire_context(&p.context());
            if w.now() >= p.due && wanted.contains(&p.key) { self.retry_counts(w, &mut state, p.key, None); }
        }
        if state.pending.is_some() || w.now() < state.not_before { return; }
        // One message check at a time, with a gap between messages. Retry delays
        // belong to individual messages so one inaccessible row cannot starve
        // the rest of the viewport.
        // Resume after the previous message so repeated invalidations in a
        // large viewport cannot keep the last rows from getting their turn.
        let cursor = state.cursor;
        let order = wanted.iter().filter(|key| cursor.is_none_or(|cursor| **key > cursor))
            .chain(wanted.iter().filter(|key| cursor.is_some_and(|cursor| **key <= cursor)));
        let next = state.urgent.iter().chain(order).copied().find_map(|key| {
            if !self.chat_ready(key.0) { return None; }
            if state.retries.get(&key).is_some_and(|(due, _)| w.now() < *due) { return None; }
            let row = reaction_state::state(w.store().conn(), key.0, key.1).ok()?;
            if !row.refresh { return None; }
            let message = model::line(w.store(), key.0, key.1).filter(|m| !m.service)?;
            Some((key, row.revision, message))
        });
        let Some((key, revision, message)) = next else { return; };
        state.next += 1;
        state.cursor = Some(key);
        let attempt = Attempt { id: state.next, key, revision, epoch: state.epoch,
            phase: "snapshot", due: w.now() + PATIENCE };
        let request = reaction_count_snapshot(&message, &attempt.context());
        state.pending = Some(attempt);
        state.not_before = w.now() + GAP;
        drop(state);
        self.send(w, &request);
    }

    fn retry_counts(&self, w: &World, state: &mut Counts, key: Key, delay: Option<f64>) {
        if let Some(delay) = delay { state.not_before = state.not_before.max(w.now() + delay); }
        let failures = state.retries.get(&key).map_or(0, |(_, n)| *n).saturating_add(1);
        let delay = delay.unwrap_or_else(|| 2_f64.powi(failures.min(6) as i32).min(60.0));
        state.retries.insert(key, (w.now() + delay, failures));
        self.log(&format!("<< reaction counts deferred chat={} message={} retry_in={delay}", key.0, key.1));
    }

    pub(super) fn on_count_reply(&self, w: &World, v: &Value) -> bool {
        let Some(context) = v["@extra"].as_str().filter(|s| s.starts_with("reaction_count:")) else { return false; };
        // This worker owns recovery. Keep automatic reconciliation failures
        // out of the shared action bar, where they would shift every panel
        // and leave an error for an attempt already being retried.
        runtime::of(w.store()).operations.retire_context(context);
        let mut state = self.counts.borrow_mut();
        if state.pending.as_ref().is_none_or(|p| p.context() != context) { return true; }
        let mut p = state.pending.take().unwrap();
        if p.epoch != state.epoch { return true; }
        let Some(message) = model::line(w.store(), p.key.0, p.key.1) else { return true; };
        let current = reaction_state::state(w.store().conn(), p.key.0, p.key.1);
        if current.is_ok_and(|s| s.revision != p.revision || !s.refresh) { return true; }
        if v["@type"] == "error" {
            let delay = (v["code"] == 429).then(|| retry_after(v["message"].as_str().unwrap_or("")))
                .flatten().map(|n| n + 1.0);
            self.retry_counts(w, &mut state, p.key, delay);
            return true;
        }
        let request = if p.phase == "metadata" {
            let ready = v["@type"] == "availableReactions" && (v["allow_custom_emoji"] == true
                || ["top_reactions", "recent_reactions", "popular_reactions"].iter()
                    .any(|key| v[key].as_array().is_some_and(|a| !a.is_empty())));
            if !ready {
                self.retry_counts(w, &mut state, p.key, None);
                return true;
            }
            p.phase = "confirmed";
            reaction_count_snapshot(&message, &p.context())
        } else {
            let Some(message) = v["messages"].as_array().into_iter().flatten()
                .find(|m| m["chat_id"] == p.key.0 && m["id"] == p.key.1) else {
                    self.retry_counts(w, &mut state, p.key, None);
                    return true;
                };
            let info = &message["interaction_info"];
            let counts = updates::reaction_counts(info);
            if counts.is_some() || (p.phase == "confirmed" && (info.is_null() || info.is_object())
                && info["reactions"].is_null()) {
                let counts = counts.flatten();
                drop(state);
                let key = p.key;
                let written = w.store().write(move |c| {
                    reaction_state::reconcile(c, key.0, key.1, p.revision, counts.as_deref())
                });
                let mut state = self.counts.borrow_mut();
                if matches!(written, Ok(true)) {
                    state.retries.remove(&key);
                    state.urgent.remove(&key);
                } else {
                    self.retry_counts(w, &mut state, key, None);
                }
                self.filed(w, "reconcile reactions", written);
                return true;
            }
            if !info["reactions"].is_null() {
                // Unknown future wire shapes must never turn into a removal.
                self.retry_counts(w, &mut state, p.key, None);
                return true;
            }
            p.phase = "metadata";
            reaction_count_metadata(p.key.0, p.key.1, &p.context())
        };
        p.due = w.now() + PATIENCE;
        state.pending = Some(p);
        state.not_before = w.now() + GAP;
        drop(state);
        self.send(w, &request);
        true
    }
}
