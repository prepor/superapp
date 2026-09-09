use super::*;
use crate::apps::telegram::{panel_read::Read, requests};

fn message(count: i64) -> serde_json::Value {
    json!({"@type": "message", "chat_id": 7, "id": 42,
        "content": {"@type": "messageText", "text": {"text": "post"}},
        "interaction_info": {"reactions": {"reactions": [
            {"type": {"@type": "reactionTypeEmoji", "emoji": "👍"}, "total_count": count}
        ]}}})
}

fn push_count(acc: &Account<FakeTd>, w: &World, count: i64) {
    acc.on_update(w, &json!({"@type": "updateMessageInteractionInfo", "chat_id": 7,
        "message_id": 42, "interaction_info": message(count)["interaction_info"]}).to_string());
}

fn reply_count(acc: &Account<FakeTd>, w: &World, request: &serde_json::Value, count: i64) {
    acc.on_update(w, &json!({"@type": "foundChatMessages", "@extra": request["@extra"],
        "messages": [message(count)]}).to_string());
}

fn author_page(request: &serde_json::Value) -> serde_json::Value {
    json!({"@type": "addedReactions", "@extra": request["@extra"], "total_count": 3,
        "reactions": [
            {"type": {"@type": "reactionTypeEmoji", "emoji": "👍"},
                "sender_id": {"@type": "messageSenderUser", "user_id": 9}},
            {"type": {"@type": "reactionTypeEmoji", "emoji": "👍"},
                "sender_id": {"@type": "messageSenderUser", "user_id": 10}}
        ], "next_offset": "more"})
}

#[test]
fn unchanged_author_reads_do_not_start_count_snapshots() {
    let clock = FakeClock::default();
    let w = timed_world(&clock);
    let td = FakeTd::new();
    let acc = account(td.clone(), None);
    acc.drain(&w);
    acc.on_update(&w, &message(3).to_string());
    for _ in 0..12 {
        push_count(&acc, &w, 3);
        for request in [requests::get_message(7, 42), requests::get_message_added_reactions(7, 42, ""),
            requests::get_message_added_reactions(7, 42, "more")] {
            let read = Read::start(w.store(), &request, w.now());
            acc.drain(&w);
            let kind = serde_json::from_str::<serde_json::Value>(&request).unwrap()["@type"].as_str().unwrap().to_owned();
            let request = last_request(&td, &kind);
            let mut reply = if kind == "getMessage" { message(1) } else { author_page(&request) };
            reply["@extra"] = request["@extra"].clone();
            acc.on_update(&w, &reply.to_string());
            assert!(read.poll(w.now()).unwrap().is_ok());
            acc.drain(&w);
        }
        clock.advance(30.0);
    }
    assert!(!td.sent_types().contains(&"searchChatMessages".into()),
        "unchanged author totals and stale cached metadata do not invalidate settled counts");
}

#[test]
fn later_pages_and_duplicate_cards_preserve_the_pending_count_snapshot() {
    let clock = FakeClock::default();
    let w = timed_world(&clock);
    let td = FakeTd::new();
    let acc = account(td.clone(), None);
    acc.drain(&w);
    acc.on_update(&w, &message(1).to_string());
    push_count(&acc, &w, 1);
    let first = Read::start(w.store(), &requests::get_message_added_reactions(7, 42, ""), w.now());
    acc.drain(&w);
    acc.on_update(&w, &author_page(&last_request(&td, "getMessageAddedReactions")).to_string());
    assert!(first.poll(w.now()).unwrap().is_ok());
    acc.drain(&w);
    let snapshot = last_request(&td, "searchChatMessages");

    for offset in ["more", "last", ""] {
        clock.advance(2.0);
        let read = Read::start(w.store(), &requests::get_message_added_reactions(7, 42, offset), w.now());
        acc.drain(&w);
        acc.on_update(&w, &author_page(&last_request(&td, "getMessageAddedReactions")).to_string());
        assert!(read.poll(w.now()).unwrap().is_ok());
        acc.drain(&w);
        assert_eq!(last_request(&td, "searchChatMessages"), snapshot,
            "later pages and another card reporting the same total share the existing check");
    }
    reply_count(&acc, &w, &snapshot, 3);
    assert_eq!(super::super::model::line(w.store(), 7, 42).unwrap().reactions.as_deref(), Some("👍 3"),
        "the original snapshot can still reconcile the shared counts");
}

#[test]
fn mismatching_author_totals_refresh_the_counts_in_both_views() {
    use crate::apps::telegram::{model, transcript::Transcript};

    let w = world();
    let td = FakeTd::new();
    let acc = account(td.clone(), None);
    acc.drain(&w);
    acc.on_update(&w, &message(1).to_string());
    push_count(&acc, &w, 1);
    let transcript = Transcript::new(7, 0, None, w.now());
    assert_eq!(transcript.get(w.store()).message((7, 42)).unwrap().reactions.as_deref(), Some("👍 1"));

    let read = Read::start(w.store(), &requests::get_message_added_reactions(7, 42, ""), w.now());
    acc.drain(&w);
    let request = last_request(&td, "getMessageAddedReactions");
    acc.on_update(&w, &author_page(&request).to_string());
    assert!(read.poll(w.now()).unwrap().is_ok());
    assert_eq!(model::line(w.store(), 7, 42).unwrap().reactions.as_deref(), Some("👍 1"),
        "a partial author page cannot replace the shared counts");

    // A different server total must trigger this check even when the
    // message already belongs to a settled viewport (or has just closed).
    acc.drain(&w);
    let search = last_request(&td, "searchChatMessages");
    assert_eq!(search["from_message_id"], 42);
    reply_count(&acc, &w, &search, 3);
    assert_eq!(model::line(w.store(), 7, 42).unwrap().reactions.as_deref(), Some("👍 3"));
    assert_eq!(transcript.get(w.store()).message((7, 42)).unwrap().reactions.as_deref(), Some("👍 3"));
}

#[test]
fn custom_emoji_totals_match_and_paid_stars_do_not_cause_spurious_checks() {
    for counts in ["custom emoji 2 · 👍 1", "⭐ 500 · 👍 3"] {
        let w = world();
        let td = FakeTd::new();
        let acc = account(td.clone(), None);
        acc.drain(&w);
        acc.on_update(&w, &message(1).to_string());
        w.store().write(move |c| crate::apps::telegram::reaction_state::set(c, 7, 42, Some(counts))).unwrap();
        let read = Read::start(w.store(), &requests::get_message_added_reactions(7, 42, ""), w.now());
        acc.drain(&w);
        acc.on_update(&w, &author_page(&last_request(&td, "getMessageAddedReactions")).to_string());
        assert!(read.poll(w.now()).unwrap().is_ok());
        acc.drain(&w);
        assert_eq!(td.sent_types(), vec!["getMessageAddedReactions"]);
    }
}

#[test]
fn a_new_author_total_retires_an_older_count_read_and_preserves_newer_pushes() {
    let clock = FakeClock::default();
    let w = timed_world(&clock);
    let td = FakeTd::new();
    let acc = account(td.clone(), None);
    acc.on_update(&w, &message(1).to_string());
    push_count(&acc, &w, 1);
    acc.counts_after_add(&w, 7, 42);
    acc.drain(&w);
    let old = last_request(&td, "searchChatMessages");

    let read = Read::start(w.store(), &requests::get_message_added_reactions(7, 42, ""), w.now());
    acc.drain(&w);
    let request = last_request(&td, "getMessageAddedReactions");
    acc.on_update(&w, &author_page(&request).to_string());
    assert!(read.poll(w.now()).unwrap().is_ok());
    reply_count(&acc, &w, &old, 0);
    assert_eq!(super::super::model::line(w.store(), 7, 42).unwrap().reactions.as_deref(), Some("👍 1"),
        "a read predating the author refresh cannot settle the counts");

    clock.advance(2.0);
    acc.drain(&w);
    let fresh = last_request(&td, "searchChatMessages");
    assert_ne!(fresh["@extra"], old["@extra"]);
    push_count(&acc, &w, 4);
    reply_count(&acc, &w, &fresh, 3);
    assert_eq!(super::super::model::line(w.store(), 7, 42).unwrap().reactions.as_deref(), Some("👍 4"));
}

#[test]
fn author_refreshes_respect_count_retry_delays() {
    let clock = FakeClock::default();
    let w = timed_world(&clock);
    let td = FakeTd::new();
    let acc = account(td.clone(), None);
    acc.on_update(&w, &message(1).to_string());
    acc.counts_after_add(&w, 7, 42);
    acc.drain(&w);
    let old = last_request(&td, "searchChatMessages");
    acc.on_update(&w, &json!({"@type": "error", "@extra": old["@extra"], "code": 429,
        "message": "Too Many Requests: retry after 20"}).to_string());

    let read = Read::start(w.store(), &requests::get_message_added_reactions(7, 42, ""), w.now());
    acc.drain(&w);
    let request = last_request(&td, "getMessageAddedReactions");
    acc.on_update(&w, &author_page(&request).to_string());
    assert!(read.poll(w.now()).unwrap().is_ok());
    clock.advance(10.0);
    acc.drain(&w);
    assert_eq!(last_request(&td, "searchChatMessages"), old);
    clock.advance(12.0);
    acc.drain(&w);
    let retry = last_request(&td, "searchChatMessages");
    assert_ne!(retry["@extra"], old["@extra"]);
    reply_count(&acc, &w, &retry, 3);
    assert_eq!(super::super::model::line(w.store(), 7, 42).unwrap().reactions.as_deref(), Some("👍 3"));
}

#[test]
fn canceled_or_failed_author_reads_leave_settled_counts_alone() {
    for canceled in [true, false] {
        let w = world();
        let td = FakeTd::new();
        let acc = account(td.clone(), None);
        acc.drain(&w);
        acc.on_update(&w, &message(1).to_string());
        push_count(&acc, &w, 1);
        let read = Read::start(w.store(), &requests::get_message_added_reactions(7, 42, ""), w.now());
        acc.drain(&w);
        let request = last_request(&td, "getMessageAddedReactions");
        if canceled {
            drop(read);
            acc.on_update(&w, &author_page(&request).to_string());
        } else {
            acc.on_update(&w, &json!({"@type": "error", "@extra": request["@extra"],
                "code": 403, "message": "CHAT_ACCESS_DENIED"}).to_string());
            assert!(read.poll(w.now()).unwrap().is_err());
        }
        acc.drain(&w);
        assert!(!td.sent_types().contains(&"searchChatMessages".to_string()));
        assert_eq!(super::super::model::line(w.store(), 7, 42).unwrap().reactions.as_deref(), Some("👍 1"));
    }
}

#[test]
fn panel_reads_resolve_through_the_account_and_disconnect_promptly() {
    let w = world();
    let td = FakeTd::new();
    let acc = account(td.clone(), None);
    acc.drain(&w);
    let read = Read::start(w.store(), &requests::get_message_added_reactions(7, 42, ""), w.now());
    acc.drain(&w);
    let request = last_request(&td, "getMessageAddedReactions");
    assert!(read.poll(w.now()).is_none());
    let result = json!({"@type": "addedReactions", "total_count": 1,
        "reactions": [{"type": {"@type": "reactionTypeEmoji", "emoji": "👍"},
            "sender_id": {"@type": "messageSenderUser", "user_id": 9}}],
        "next_offset": "", "@extra": request["@extra"]});
    acc.on_update(&w, &result.to_string());
    assert_eq!(read.poll(w.now()).unwrap().unwrap()["reactions"], result["reactions"]);

    let read = Read::start(w.store(), &requests::search_mention_members(7, 0, "an"), w.now());
    acc.drain(&w);
    let request = last_request(&td, "searchChatMembers");
    acc.on_update(&w, &json!({"@type": "error", "code": 403, "message": "CHAT_ACCESS_DENIED",
        "@extra": request["@extra"]}).to_string());
    assert_eq!(read.poll(w.now()).unwrap().unwrap_err(), "CHAT_ACCESS_DENIED");

    let read = Read::start(w.store(), &requests::search_mention_members(7, 0, "ve"), w.now());
    runtime::of(w.store()).disconnect();
    assert!(read.poll(w.now()).unwrap().unwrap_err().contains("disconnected"));
}

#[test]
fn canceled_panel_reads_are_retired_before_reaching_the_transport() {
    let w = world();
    let td = FakeTd::new();
    let acc = account(td.clone(), None);
    acc.drain(&w);
    let read = Read::start(w.store(), &requests::search_mention_members(7, 0, "old"), w.now());
    drop(read);
    acc.drain(&w);
    assert!(!td.sent_types().contains(&"searchChatMembers".to_string()));
    assert!(runtime::of(w.store()).operations.list().iter()
        .all(|operation| !operation.context().is_some_and(|context| context.starts_with("panel_read:"))));
}
