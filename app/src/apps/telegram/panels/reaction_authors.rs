//! The line card's reaction authors. Recent senders come with the message;
//! Telegram also supplies a paginated complete list where permitted.

use kernel::store::Store;
use serde_json::Value;

use super::super::{model, panel_read::Read, requests, runtime::{self, REACTION_REFRESH}};
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
struct Listing {
    authors: Vec<Author>,
    offset: String,
    // Preserve the depth the reader opened when counts invalidate the names.
    pages: usize,
}

impl Listing {
    fn append(&mut self, value: &Value) {
        for author in added(value) {
            if !self.authors.contains(&author) { self.authors.push(author); }
        }
        let next = value["next_offset"].as_str().unwrap_or_default();
        self.offset = if next == self.offset { String::new() } else { next.into() };
        self.pages += 1;
    }
}

#[derive(Default)]
pub(super) struct ReactionAuthors {
    counts: Option<Option<String>>,
    refreshed: f64,
    read: Option<Read>,
    details: bool,
    listing: Listing,
    // Rebuild the loaded pages offscreen. A metadata reply or a single page
    // must not replace the complete reading the person has opened.
    refreshing: Option<Listing>,
    note: String,
    failed: bool,
}

impl ReactionAuthors {
    pub fn refresh(&mut self, store: &Store, msg: &Msg, now: f64, visible: bool) {
        if !visible {
            if self.read.take().is_some() {
                self.refreshing = None;
                self.refreshed = now - REACTION_REFRESH;
            }
            return;
        }
        if !super::live(store) { return; }
        if self.counts.as_ref() != Some(&msg.reactions) {
            self.listing.authors.clear();
            self.listing.offset.clear();
            self.note.clear();
            self.failed = false;
            self.read = None;
            self.refreshing = None;
            self.counts = Some(msg.reactions.clone());
            self.refreshed = now - REACTION_REFRESH;
        }
        if msg.reactions.is_none() {
            self.listing = Listing::default();
            return;
        }
        if let Some(result) = self.read.as_ref().and_then(|read| read.poll(now)) {
            self.read = None;
            self.refreshed = now;
            match result {
                Ok(value) if self.details => {
                    let reactions = &value["interaction_info"]["reactions"];
                    // getMessage can omit reaction metadata from its cache.
                    // Unknown availability still needs the author lookup;
                    // only an explicit denial selects the recent-sender fallback.
                    if reactions["can_get_added_reactions"].as_bool() != Some(false) {
                        self.page(store, msg, now);
                    } else {
                        self.refreshing = None;
                        self.listing = Listing { authors: recent(reactions), ..Listing::default() };
                        self.note = if !self.listing.authors.is_empty() { "recent reaction authors" }
                            else { "reaction authors are hidden" }.into();
                    }
                }
                Ok(value) => {
                    if let Some(fresh) = self.refreshing.as_mut() {
                        fresh.append(&value);
                        if fresh.pages < self.listing.pages && !fresh.offset.is_empty() {
                            self.page(store, msg, now);
                        } else {
                            self.listing = self.refreshing.take().unwrap();
                        }
                    } else {
                        self.listing.append(&value);
                    }
                    self.note.clear();
                }
                Err(error) => {
                    self.note = format!("could not load reaction authors: {error}");
                    self.failed = true;
                }
            }
        }
        if self.read.is_none() && now - self.refreshed >= REACTION_REFRESH {
            self.start_refresh(store, msg, now);
        }
    }

    fn start_refresh(&mut self, store: &Store, msg: &Msg, now: f64) {
        self.refreshed = now;
        self.failed = false;
        self.details = true;
        self.refreshing = Some(Listing::default());
        self.read = Some(Read::start(store, &requests::get_message(msg.chat, msg.id), now));
    }

    fn page(&mut self, store: &Store, msg: &Msg, now: f64) {
        self.failed = false;
        self.details = false;
        let listing = self.refreshing.as_ref().unwrap_or(&self.listing);
        self.read = Some(Read::start(store,
            &requests::get_message_added_reactions(msg.chat, msg.id, &listing.offset), now));
    }

    pub fn more(&mut self, store: &Store, msg: &Msg, now: f64) {
        if self.read.is_some() { return; }
        if self.failed {
            if self.details { self.start_refresh(store, msg, now); }
            else { self.page(store, msg, now); }
        } else if !self.listing.offset.is_empty() {
            self.page(store, msg, now);
        }
    }

    pub fn action(&self) -> Option<&'static str> {
        if self.read.is_some() { None }
        else if self.failed { Some("retry reaction authors") }
        else if !self.listing.offset.is_empty() { Some("more reaction authors") }
        else { None }
    }

    pub fn text(&self, store: &Store, msg: &Msg) -> String {
        if msg.reactions.is_none() { return String::new(); }
        if !super::live(store) {
            return runtime::of(store).demo_reaction_emojis(msg.chat, msg.id).iter()
                .map(|emoji| format!("{emoji}  me")).collect::<Vec<_>>().join("\n");
        }
        let mut lines = self.listing.authors.iter().map(|author| {
            let name = if model::self_peer(store) == Some(author.sender) { "me".into() }
                else { model::peer(store, author.sender).map_or_else(|| author.sender.to_string(), |peer| peer.name) };
            format!("{}  {name}", author.emoji)
        }).collect::<Vec<_>>();
        if self.read.is_some() {
            if lines.is_empty() { lines.push("loading reaction authors…".into()); }
        } else if !self.note.is_empty() { lines.push(self.note.clone()); }
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

    fn metadata() -> Value {
        json!({"interaction_info": {"reactions": {
            "can_get_added_reactions": true, "reactions": [{"type": {"emoji": "👍"},
                "recent_sender_ids": [{"user_id": seed::VERA}]}]
        }}})
    }

    fn page(first: i64, end: i64, offset: &str) -> Value {
        json!({"reactions": (first..end).map(|id| json!({
            "type": {"emoji": "👍"}, "sender_id": {"user_id": id}
        })).collect::<Vec<_>>(), "next_offset": offset})
    }

    #[test]
    fn missing_cached_metadata_still_loads_authors_and_allows_retrying_a_failed_lookup() {
        let session = Session::fake(APPS);
        let store = session.store();
        let rt = runtime::of(store);
        let inbox = rt.connect();
        let mut msg = model::history(store, seed::STELAXIS).last().unwrap().clone();
        msg.reactions = Some("👍 1".into());
        let mut authors = ReactionAuthors::default();
        authors.refresh(store, &msg, 0.0, true);
        reply(&rt, &inbox, "getMessage", json!({"interaction_info": null}));
        authors.refresh(store, &msg, 0.1, true);
        assert_eq!(authors.text(store, &msg), "loading reaction authors…",
            "missing cached metadata is not a final availability result");
        let request: Value = serde_json::from_str(&inbox.try_recv().unwrap()).unwrap();
        assert_eq!(request["@type"], "getMessageAddedReactions");
        let id = panel_read::id(request["@extra"]["context"].as_str().unwrap()).unwrap();
        rt.reads.lock().unwrap().finish(id, Err("request failed".into()));
        authors.refresh(store, &msg, 0.2, true);
        assert_eq!(authors.action(), Some("retry reaction authors"));
        assert!(authors.text(store, &msg).contains("request failed"));
        authors.more(store, &msg, 0.3);
        reply(&rt, &inbox, "getMessageAddedReactions", page(seed::VERA, seed::VERA + 1, ""));
        authors.refresh(store, &msg, 0.4, true);
        assert_eq!(authors.text(store, &msg), "👍  Vera Kovac");
        assert_eq!(authors.action(), None);
    }

    #[test]
    fn periodic_refresh_preserves_loaded_pages_until_their_replacement_is_complete() {
        let session = Session::fake(APPS);
        let store = session.store();
        let rt = runtime::of(store);
        let inbox = rt.connect();
        let mut msg = model::history(store, seed::STELAXIS).last().unwrap().clone();
        msg.reactions = Some("👍 150".into());
        let mut authors = ReactionAuthors::default();
        authors.refresh(store, &msg, 0.0, true);
        reply(&rt, &inbox, "getMessage", metadata());
        authors.refresh(store, &msg, 0.1, true);
        reply(&rt, &inbox, "getMessageAddedReactions", page(1000, 1100, "page2"));
        authors.refresh(store, &msg, 0.2, true);
        authors.more(store, &msg, 0.3);
        reply(&rt, &inbox, "getMessageAddedReactions", page(1100, 1150, ""));
        authors.refresh(store, &msg, 0.4, true);
        let before = authors.text(store, &msg);
        assert_eq!(before.lines().count(), 150);
        assert_eq!(authors.action(), None);

        for second in 1..300 {
            authors.refresh(store, &msg, f64::from(second), true);
            assert!(inbox.try_recv().is_err(), "an unchanged card does not poll every thirty seconds");
            assert_eq!(authors.text(store, &msg), before);
        }

        authors.refresh(store, &msg, 301.0, true);
        assert_eq!(authors.text(store, &msg), before, "refresh must keep the reading stable");
        reply(&rt, &inbox, "getMessage", json!({"interaction_info": {"reactions": null}}));
        authors.refresh(store, &msg, 301.1, true);
        assert_eq!(authors.text(store, &msg), before, "missing metadata must not replace loaded pages");
        reply(&rt, &inbox, "getMessageAddedReactions", page(1001, 1101, "fresh-page2"));
        authors.refresh(store, &msg, 301.2, true);
        assert_eq!(authors.text(store, &msg), before, "a partial refresh must not replace the list");
        let request = reply(&rt, &inbox, "getMessageAddedReactions", page(1101, 1151, ""));
        assert_eq!(request["offset"], "fresh-page2");
        authors.refresh(store, &msg, 301.3, true);
        let after = authors.text(store, &msg);
        assert_eq!(after.lines().count(), 150);
        assert!(!after.contains("1000"));
        assert!(after.contains("1150"));
        assert_eq!(authors.action(), None, "a completed list stays completed");

        msg.reactions = Some("👍 1".into());
        authors.refresh(store, &msg, 302.0, true);
        assert_eq!(authors.text(store, &msg), "loading reaction authors…", "changed counts invalidate old authors");
        reply(&rt, &inbox, "getMessage", metadata());
        authors.refresh(store, &msg, 302.1, true);
        reply(&rt, &inbox, "getMessageAddedReactions", page(1150, 1151, ""));
        authors.refresh(store, &msg, 302.2, true);
        assert_eq!(authors.text(store, &msg), "👍  1150", "a shorter complete list may replace the loaded pages");
        assert!(inbox.try_recv().is_err());
    }

    #[test]
    fn a_failed_refresh_keeps_the_reading_and_retries_its_page_before_more() {
        let session = Session::fake(APPS);
        let store = session.store();
        let rt = runtime::of(store);
        let inbox = rt.connect();
        let mut msg = model::history(store, seed::STELAXIS).last().unwrap().clone();
        msg.reactions = Some("👍 250".into());
        let mut authors = ReactionAuthors::default();
        authors.refresh(store, &msg, 0.0, true);
        reply(&rt, &inbox, "getMessage", metadata());
        authors.refresh(store, &msg, 0.1, true);
        reply(&rt, &inbox, "getMessageAddedReactions", page(1000, 1100, "page2"));
        authors.refresh(store, &msg, 0.2, true);
        authors.more(store, &msg, 0.3);
        reply(&rt, &inbox, "getMessageAddedReactions", page(1100, 1200, "old-page3"));
        authors.refresh(store, &msg, 0.4, true);
        let before = authors.text(store, &msg);
        assert_eq!(before.lines().count(), 200);

        authors.refresh(store, &msg, 301.0, true);
        reply(&rt, &inbox, "getMessage", metadata());
        authors.refresh(store, &msg, 301.1, true);
        reply(&rt, &inbox, "getMessageAddedReactions", page(1001, 1101, "fresh-page2"));
        authors.refresh(store, &msg, 301.2, true);
        let request: Value = serde_json::from_str(&inbox.try_recv().unwrap()).unwrap();
        let id = panel_read::id(request["@extra"]["context"].as_str().unwrap()).unwrap();
        rt.reads.lock().unwrap().finish(id, Err("temporary failure".into()));
        authors.refresh(store, &msg, 301.3, true);
        assert!(authors.text(store, &msg).starts_with(&before));
        assert_eq!(authors.action(), Some("retry reaction authors"));

        authors.more(store, &msg, 301.4);
        assert_eq!(authors.text(store, &msg), before);
        let retry = reply(&rt, &inbox, "getMessageAddedReactions", page(1101, 1201, "fresh-page3"));
        assert_eq!(retry["offset"], "fresh-page2");
        authors.refresh(store, &msg, 301.5, true);
        assert_eq!(authors.text(store, &msg).lines().count(), 200);
        assert_eq!(authors.action(), Some("more reaction authors"));
        authors.more(store, &msg, 301.6);
        let more = reply(&rt, &inbox, "getMessageAddedReactions", page(1201, 1251, ""));
        assert_eq!(more["offset"], "fresh-page3", "more must continue the refreshed pagination");
        authors.refresh(store, &msg, 301.7, true);
        assert_eq!(authors.text(store, &msg).lines().count(), 250);
        assert_eq!(authors.action(), None);
    }

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
        assert_eq!(authors.listing.authors.len(), 2);
        assert!(authors.text(store, &msg).contains("custom emoji  stelaxis"));
        assert_eq!(authors.action(), None);
        msg.reactions = None;
        authors.refresh(store, &msg, 0.5, true);
        assert!(authors.text(store, &msg).is_empty());
        assert!(authors.listing.authors.is_empty());
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
        assert!(authors.listing.authors.is_empty());
    }
}
