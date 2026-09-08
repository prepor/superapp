//! Automatic history follows settled, visible transcripts. Leaving a preview
//! retires its walk, including retries and late replies, without delaying draws.

use std::collections::BTreeMap;

use super::*;

#[derive(Default)]
pub(super) struct HistoryViews {
    visits: BTreeMap<(PeerId, i64), u64>,
    next: u64,
}

impl Page {
    pub(super) fn extra(self) -> String {
        let extra = history_extra_in(self.chat, self.topic, self.from, self.walk);
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
        Some(Self { chat, topic, walk, from, view })
    }
}

impl<T: Td> Account<T> {
    pub(super) fn history_wanted(&self, w: &World, page: Page) -> bool {
        page.view.is_none_or(|view| {
            self.history_views.borrow().visits.get(&(page.chat, page.topic)) == Some(&view)
                && runtime::of(w.store()).visible_history(w.now()).contains(&(page.chat, page.topic))
        })
    }

    pub(super) fn sync_history_views(&self, w: &World) {
        let rt = runtime::of(w.store());
        let visible = rt.visible_history(w.now());
        let mut state = self.history_views.borrow_mut();
        let mut retired = Vec::new();
        state.visits.retain(|key, visit| {
            if self.auth_ready.get() && visible.contains(key) { return true; }
            rt.set_loading_in(key.0, key.1, false);
            rt.operations.changed();
            retired.push(*visit);
            false
        });
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
            state.visits.get(&(page.chat, page.topic)) == Some(&view)
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
        for (chat, topic) in visible {
            if state.visits.contains_key(&(chat, topic)) { continue; }
            if topic != 0 && !model::peer(w.store(), chat).is_some_and(|p| p.is_forum) { continue; }
            state.next += 1;
            let view = state.next;
            state.visits.insert((chat, topic), view);
            rt.set_loading_in(chat, topic, true);
            self.pages.borrow_mut().push_front(Page { chat, topic, from: 0, walk: Walk::Fill, view: Some(view) });
            if topic != 0 { topics.push((chat, topic)); }
        }
        drop(state);
        for (chat, topic) in topics { self.request_topic(w, chat, topic); }
    }
}
