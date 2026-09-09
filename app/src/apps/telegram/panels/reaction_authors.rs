//! The line card's reaction authors. Recent senders come with the message;
//! Telegram also supplies a paginated complete list where permitted.

use kernel::store::Store;
use serde_json::Value;

use super::super::{model, panel_read::Read, requests, runtime};
use model::{Msg, PeerId};

#[derive(Clone, Debug, PartialEq, Eq)]
struct Author {
    sender: PeerId,
    emoji: String,
}

fn sender(value: &Value) -> Option<PeerId> {
    value["user_id"].as_i64().or_else(|| value["chat_id"].as_i64())
}

fn emoji(value: &Value) -> Option<String> {
    value["reaction"].as_str().or_else(|| value["type"]["emoji"].as_str())
        .or_else(|| match value["type"]["@type"].as_str() {
            Some("reactionTypeCustomEmoji") => Some("custom emoji"),
            Some("reactionTypePaid") => Some("⭐"),
            _ => None,
        }).map(str::to_string)
}

fn recent(value: &Value) -> Vec<Author> {
    value["reactions"].as_array().or_else(|| value.as_array()).into_iter().flatten()
        .flat_map(|reaction| {
            reaction["recent_sender_ids"].as_array().into_iter().flatten().filter_map(|id|
                Some(Author { sender: sender(id)?, emoji: emoji(reaction)? }))
        }).collect()
}

fn added(value: &Value) -> Vec<Author> {
    value["reactions"].as_array().into_iter().flatten().filter_map(|reaction|
        Some(Author { sender: sender(&reaction["sender_id"])?, emoji: emoji(reaction)? })).collect()
}

#[derive(Default)]
pub(super) struct ReactionAuthors {
    counts: Option<Option<String>>,
    refreshed: f64,
    read: Option<Read>,
    details: bool,
    authors: Vec<Author>,
    offset: String,
    note: String,
    failed: bool,
}

impl ReactionAuthors {
    pub fn refresh(&mut self, store: &Store, msg: &Msg, now: f64, visible: bool) {
        if !visible {
            if self.read.take().is_some() { self.counts = None; }
            return;
        }
        if !super::live(store) { return; }
        if self.counts.as_ref() != Some(&msg.reactions) {
            self.authors.clear();
            self.offset.clear();
            self.note.clear();
            self.read = None;
            self.counts = Some(msg.reactions.clone());
            self.refreshed = now - 30.0;
        }
        if msg.reactions.is_none() { return; }
        if let Some(result) = self.read.as_ref().and_then(|read| read.poll(now)) {
            self.read = None;
            self.refreshed = now;
            match result {
                Ok(value) if self.details => {
                    let reactions = &value["interaction_info"]["reactions"];
                    self.authors = recent(reactions);
                    if reactions["can_get_added_reactions"] == true {
                        self.page(store, msg, now);
                    } else {
                        self.note = if !self.authors.is_empty() { "recent reaction authors" }
                            else if reactions["can_get_added_reactions"] == false { "reaction authors are hidden" }
                            else { "reaction authors are unavailable" }.into();
                    }
                }
                Ok(value) => {
                    if self.offset.is_empty() { self.authors.clear(); }
                    for author in added(&value) {
                        if !self.authors.contains(&author) { self.authors.push(author); }
                    }
                    let next = value["next_offset"].as_str().unwrap_or_default();
                    self.offset = if next == self.offset { String::new() } else { next.into() };
                    self.note.clear();
                }
                Err(error) => {
                    self.note = format!("could not load reaction authors: {error}");
                    self.failed = true;
                }
            }
        }
        if self.read.is_none() && now - self.refreshed >= 30.0 {
            self.refreshed = now;
            self.failed = false;
            self.details = true;
            self.offset.clear();
            self.read = Some(Read::start(store, &requests::get_message(msg.chat, msg.id), now));
        }
    }

    fn page(&mut self, store: &Store, msg: &Msg, now: f64) {
        self.failed = false;
        self.details = false;
        self.read = Some(Read::start(store,
            &requests::get_message_added_reactions(msg.chat, msg.id, &self.offset), now));
    }

    pub fn more(&mut self, store: &Store, msg: &Msg, now: f64) {
        if self.read.is_some() { return; }
        if self.failed {
            self.counts = None;
            self.refresh(store, msg, now, true);
        } else if !self.offset.is_empty() {
            self.page(store, msg, now);
        }
    }

    pub fn action(&self) -> Option<&'static str> {
        if self.read.is_some() { None }
        else if self.failed { Some("retry reaction authors") }
        else if !self.offset.is_empty() { Some("more reaction authors") }
        else { None }
    }

    pub fn text(&self, store: &Store, msg: &Msg) -> String {
        if msg.reactions.is_none() { return String::new(); }
        if !super::live(store) {
            return runtime::of(store).demo_reaction_emojis(msg.chat, msg.id).iter()
                .map(|emoji| format!("{emoji}  me")).collect::<Vec<_>>().join("\n");
        }
        let mut lines = self.authors.iter().map(|author| {
            let name = if model::self_peer(store) == Some(author.sender) { "me".into() }
                else { model::peer(store, author.sender).map_or_else(|| author.sender.to_string(), |peer| peer.name) };
            format!("{}  {name}", author.emoji)
        }).collect::<Vec<_>>();
        if self.read.is_some() { lines.push("loading reaction authors…".into()); }
        else if !self.note.is_empty() { lines.push(self.note.clone()); }
        lines.join("\n")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::apps::telegram::{panel_read, seed, TELEGRAM};
    use kernel::session::Session;
    use serde_json::json;

    fn reply(rt: &runtime::Runtime, inbox: &runtime::Inbox, expected: &str, value: Value) -> Value {
        let request: Value = serde_json::from_str(&inbox.try_recv().unwrap()).unwrap();
        assert_eq!(request["@type"], expected);
        let id = panel_read::id(request["@extra"]["context"].as_str().unwrap()).unwrap();
        rt.reads.lock().unwrap().finish(id, Ok(value));
        request
    }

    static APPS: &[&dyn kernel::app::App] = &[&TELEGRAM];

    #[test]
    fn authors_page_deduplicate_and_refresh_after_reactions_change() {
        let session = Session::fake(APPS);
        let store = session.store();
        let rt = runtime::of(store);
        let inbox = rt.connect();
        let mut msg = model::history(store, seed::STELAXIS).last().unwrap().clone();
        msg.reactions = Some("👍 2".into());
        let mut authors = ReactionAuthors::default();
        authors.refresh(store, &msg, 0.0, true);
        reply(&rt, &inbox, "getMessage", json!({"interaction_info": {"reactions": {
            "can_get_added_reactions": true, "reactions": []}}}));
        authors.refresh(store, &msg, 0.1, true);
        reply(&rt, &inbox, "getMessageAddedReactions", json!({"reactions": [
            {"type": {"emoji": "👍"}, "sender_id": {"user_id": seed::VERA}}
        ], "next_offset": "page2"}));
        authors.refresh(store, &msg, 0.2, true);
        assert_eq!(authors.text(store, &msg), "👍  Vera Kovac");
        assert_eq!(authors.action(), Some("more reaction authors"));
        authors.more(store, &msg, 0.3);
        let request = reply(&rt, &inbox, "getMessageAddedReactions", json!({"reactions": [
            {"type": {"emoji": "👍"}, "sender_id": {"user_id": seed::VERA}},
            {"type": {"@type": "reactionTypeCustomEmoji"}, "sender_id": {"chat_id": seed::STELAXIS}}
        ], "next_offset": ""}));
        assert_eq!(request["offset"], "page2");
        authors.refresh(store, &msg, 0.4, true);
        assert_eq!(authors.authors.len(), 2);
        assert!(authors.text(store, &msg).contains("custom emoji  stelaxis"));
        assert_eq!(authors.action(), None);
        msg.reactions = None;
        authors.refresh(store, &msg, 0.5, true);
        assert!(authors.text(store, &msg).is_empty());
        assert!(authors.authors.is_empty());
    }

    #[test]
    fn private_reactions_show_recent_senders_and_hidden_reactions_do_not_request_authors() {
        let session = Session::fake(APPS);
        let store = session.store();
        let rt = runtime::of(store);
        let inbox = rt.connect();
        let mut msg = model::history(store, seed::VERA).last().unwrap().clone();
        msg.reactions = Some("❤️ 1".into());
        for recent in [true, false] {
            let mut authors = ReactionAuthors::default();
            authors.refresh(store, &msg, 0.0, true);
            reply(&rt, &inbox, "getMessage", json!({"interaction_info": {"reactions": {
                "can_get_added_reactions": false,
                "reactions": [{"type": {"emoji": "❤️"}, "recent_sender_ids":
                    if recent { json!([{"user_id": seed::VERA}]) } else { json!([]) }}]
            }}}));
            authors.refresh(store, &msg, 0.1, true);
            assert!(inbox.try_recv().is_err());
            assert!(authors.text(store, &msg).contains(if recent { "Vera Kovac" } else { "hidden" }));
        }
    }

    #[test]
    fn canceled_and_timed_out_author_reads_never_replace_a_new_list() {
        let session = Session::fake(APPS);
        let store = session.store();
        let rt = runtime::of(store);
        let inbox = rt.connect();
        let mut msg = model::history(store, seed::STELAXIS).last().unwrap().clone();
        msg.reactions = Some("👍 1".into());
        let mut authors = ReactionAuthors::default();
        authors.refresh(store, &msg, 0.0, true);
        let request: Value = serde_json::from_str(&inbox.try_recv().unwrap()).unwrap();
        let id = panel_read::id(request["@extra"]["context"].as_str().unwrap()).unwrap();
        authors.refresh(store, &msg, 0.1, false);
        assert!(!rt.reads.lock().unwrap().alive(id));
        authors.refresh(store, &msg, 0.2, true);
        rt.reads.lock().unwrap().finish(id, Ok(json!({"interaction_info": {"reactions": {
            "can_get_added_reactions": true}}})));
        authors.refresh(store, &msg, 30.3, true);
        assert_eq!(authors.action(), Some("retry reaction authors"));
        assert!(authors.text(store, &msg).contains("could not load"));
        assert!(authors.authors.is_empty());
    }
}
