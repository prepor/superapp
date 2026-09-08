//! Automatic history follows settled, visible transcripts. Leaving a preview
//! retires its walk, including retries and late replies, without delaying draws.

use std::collections::{BTreeMap, BTreeSet};

use super::*;

#[derive(Default)]
pub(super) struct HistoryViews {
    visits: BTreeMap<(PeerId, i64), u64>,
    next: u64,
    pub(super) originals: BTreeSet<(PeerId, Option<u64>)>,
    pub(super) known_originals: BTreeSet<PeerId>,
    pub(super) opening: BTreeSet<PeerId>,
    pub(super) explicit: BTreeSet<PeerId>,
    pub(super) metadata: BTreeSet<PeerId>,
}

impl Page {
    pub(super) fn extra(self) -> String {
        let extra = history_extra_in(self.chat, self.topic, self.from, self.walk);
        let extra = self.parent.map_or_else(|| extra.clone(), |parent| format!("{extra}:parent:{parent}"));
        match self.view {
            Some(view) => format!("{extra}:view:{view}"),
            None => extra,
        }
    }

    pub(super) fn request(self) -> String {
        let mut request: Value = serde_json::from_str(&get_history_in(self.chat, self.topic, self.from, self.walk))
            .expect("history request");
        request["@extra"] = serde_json::json!(self.extra());
        request.to_string()
    }

    pub(super) fn parse(extra: &str) -> Option<Self> {
        let (chat, topic, walk, from) = parse_history_in(extra)?;
        let view = extra.rsplit_once(":view:").map(|(_, id)| id.parse()).transpose().ok()?;
        let parent = extra.split(":parent:").nth(1).and_then(|s| s.split(':').next()?.parse().ok());
        Some(Self { chat, topic, walk, from, view, parent })
    }
}

impl<T: Td> Account<T> {
    pub(super) fn request_upgrade_info(&self, w: &World, chat: PeerId) {
        if chat >= super::super::upgrades::SUPERGROUP_BASE || !self.auth_ready.get()
            || runtime::of(w.store()).list_syncing() { return; }
        if super::super::upgrades::original(w.store(), chat).is_some_and(|old|
            self.known_chats.borrow().contains(&old) || self.history_views.borrow().known_originals.contains(&old)) {
            return;
        }
        if !self.history_views.borrow_mut().metadata.insert(chat) { return; }
        self.send(w, &serde_json::json!({"@type": "getSupergroupFullInfo",
            "supergroup_id": super::super::upgrades::SUPERGROUP_BASE - chat,
            "@extra": format!("upgrade_info:{chat}")}).to_string());
    }

    pub(super) fn want_original_history(&self, w: &World, chat: PeerId, topic: i64, view: Option<u64>) {
        if topic != 0 || !self.auth_ready.get() || runtime::of(w.store()).list_syncing() { return; }
        let Some(old) = super::super::upgrades::original(w.store(), chat) else { return };
        let mut state = self.history_views.borrow_mut();
        if !self.known_chats.borrow().contains(&old) && !state.known_originals.contains(&old) {
            state.opening.insert(old);
            return;
        }
        if !state.originals.insert((chat, view)) { return; }
        drop(state);
        runtime::of(w.store()).set_loading_in(old, 0, self.known_chats.borrow().contains(&old));
        self.pages.borrow_mut().push_back(Page {
            chat: old, topic: 0, from: 0, walk: Walk::Fill, view, parent: Some(chat),
        });
        // An old group may not be in either chat list on a fresh TDLib client.
        // Its chat must exist in that client before any history requests go out.
        if !self.known_chats.borrow().contains(&old) {
            self.history_views.borrow_mut().opening.insert(old);
            self.send(w, &serde_json::json!({"@type": "createBasicGroupChat",
                "basic_group_id": -old, "force": true,
                "@extra": format!("upgrade_chat:{old}")}).to_string());
        }
    }

    pub(super) fn history_wanted(&self, w: &World, page: Page) -> bool {
        page.view.is_none_or(|view| {
            self.history_views.borrow().visits.get(&(page.parent.unwrap_or(page.chat), page.topic)) == Some(&view)
                && runtime::of(w.store()).visible_history(w.now()).contains(&(page.parent.unwrap_or(page.chat), page.topic))
        })
    }

    pub(super) fn sync_history_views(&self, w: &World) {
        let rt = runtime::of(w.store());
        let visible = rt.visible_history(w.now());
        let mut state = self.history_views.borrow_mut();
        let mut retired = Vec::new();
        let mut old_chats = Vec::new();
        state.visits.retain(|key, visit| {
            if self.auth_ready.get() && visible.contains(key) { return true; }
            rt.set_loading_in(key.0, key.1, false);
            if let Some(old) = super::super::upgrades::original(w.store(), key.0) {
                rt.set_loading_in(old, 0, false);
                old_chats.push(old);
            }
            rt.operations.changed();
            retired.push(*visit);
            false
        });
        for old in old_chats { state.opening.remove(&old); }
        state.originals.retain(|(_, view)| view.is_none_or(|id| !retired.contains(&id)));
        if !retired.is_empty() {
            for op in rt.operations.list() {
                if let Some(context) = op.context().filter(|context| {
                    Page::parse(context).and_then(|page| page.view).is_some_and(|view| retired.contains(&view))
                }) {
                    rt.operations.retire_context(context);
                }
            }
        }
        let wanted = |page: &Page| page.view.is_none_or(|view| {
            state.visits.get(&(page.parent.unwrap_or(page.chat), page.topic)) == Some(&view)
        });
        self.pages.borrow_mut().retain(wanted);
        if let Some((page, _)) = self.in_flight.get().filter(|(page, _)| !wanted(page)) {
            // TDLib may still answer, but this visit no longer owns the pacing
            // slot. Its unique tag prevents an old answer restarting the walk
            // or releasing the slot of a later visit to the same chat.
            rt.operations.retire_context(&page.extra());
            self.in_flight.set(None);
        }
        if !self.auth_ready.get() || rt.list_syncing() || rt.connection_error().is_some() { return; }
        let mut topics = Vec::new();
        let mut metadata = Vec::new();
        for (chat, topic) in visible {
            if state.visits.contains_key(&(chat, topic)) { continue; }
            if topic != 0 && !model::peer(w.store(), chat).is_some_and(|p| p.is_forum) { continue; }
            state.next += 1;
            let view = state.next;
            state.visits.insert((chat, topic), view);
            state.metadata.remove(&chat);
            rt.set_loading_in(chat, topic, true);
            self.pages.borrow_mut().push_front(Page { chat, topic, from: 0, walk: Walk::Fill, view: Some(view), parent: None });
            if topic != 0 { topics.push((chat, topic)); }
            if topic == 0 && chat < super::super::upgrades::SUPERGROUP_BASE
                && super::super::upgrades::original(w.store(), chat).is_none_or(|old|
                    !self.known_chats.borrow().contains(&old) && !state.known_originals.contains(&old)) {
                metadata.push(chat);
            }
        }
        let explicit: Vec<_> = state.explicit.iter().copied().collect();
        let originals: Vec<_> = state.visits.iter().map(|(&(chat, topic), &view)| (chat, topic, view)).collect();
        drop(state);
        for (chat, topic, view) in originals { self.want_original_history(w, chat, topic, Some(view)); }
        for chat in explicit {
            self.request_upgrade_info(w, chat);
            self.want_original_history(w, chat, 0, None);
        }
        for chat in metadata { self.request_upgrade_info(w, chat); }
        for (chat, topic) in topics { self.request_topic(w, chat, topic); }
    }
}
