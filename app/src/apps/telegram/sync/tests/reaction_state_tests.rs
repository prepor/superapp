use super::*;

fn post() -> serde_json::Value {
    json!({"@type": "message", "chat_id": 7, "id": 42,
        "content": {"@type": "messageText", "text": {"text": "post"}}})
}

fn push(acc: &Account<FakeTd>, w: &World, count: Option<i64>) {
    let info = count.map(|n| json!({"reactions": {"reactions": [
        {"type": {"@type": "reactionTypeEmoji", "emoji": "👍"}, "total_count": n},
    ]}}));
    acc.on_update(w, &json!({"@type": "updateMessageInteractionInfo", "chat_id": 7,
        "message_id": 42, "interaction_info": info}).to_string());
}

fn counts(w: &World) -> Option<String> {
    crate::apps::telegram::model::line(w.store(), 7, 42).unwrap().reactions
}

#[test]
fn reconciliation_searches_text_senders_topics_and_captionless_media_on_the_server() {
    let w = world();
    let td = FakeTd::new();
    let acc = account(td.clone(), None);
    acc.parameters(&w);
    assert_eq!(last_request(&td, "setTdlibParameters")["use_message_database"], false,
        "filtered reconciliation reads must bypass TDLib's database cache");
    acc.on_update(&w, &post().to_string());
    let mut m = crate::apps::telegram::model::line(w.store(), 7, 42).unwrap();
    let request = |m: &crate::apps::telegram::model::Msg| -> serde_json::Value {
        serde_json::from_str(&super::super::reaction_count_snapshot(m, "test")).unwrap()
    };
    assert_eq!(request(&m)["query"], "post", "bare empty searches are not valid");
    m.text.clear();
    m.sender = Some(123);
    assert_eq!(request(&m)["sender_id"], json!({"@type": "messageSenderUser", "user_id": 123}));
    m.sender = None;
    m.topic = 55;
    assert_eq!(request(&m)["topic_id"], json!({"@type": "messageTopicForum", "forum_topic_id": 55}));
    m.topic = 0;
    for kind in ["Photo", "Video", "VoiceNote", "VideoNote", "Audio", "Document", "Animation", "Poll"] {
        m.content_type = Some(format!("message{kind}"));
        assert_eq!(request(&m)["filter"]["@type"], format!("searchMessagesFilter{kind}"));
    }
    acc.on_update(&w, &json!({"@type": "updateMessageContent", "chat_id": 7, "message_id": 42,
        "new_content": {"@type": "messageAnimation", "animation": {}}}).to_string());
    let edited = crate::apps::telegram::model::line(w.store(), 7, 42).unwrap();
    assert_eq!(request(&edited)["filter"]["@type"], "searchMessagesFilterAnimation",
        "edited media must retain its exact wire type, independent of its renderer");
}

#[test]
fn counts_belong_to_the_message_even_without_a_view() {
    let w = world();
    let acc = account(FakeTd::new(), None);
    acc.on_update(&w, &post().to_string());
    push(&acc, &w, Some(8));
    // History, chat-list and media loads can all carry an older snapshot.
    let mut stale = post();
    stale["interaction_info"] = json!({"reactions": {"reactions": [
        {"type": {"@type": "reactionTypeEmoji", "emoji": "👍"}, "total_count": 1},
    ]}});
    acc.on_update(&w, &stale.to_string());
    acc.on_update(&w, &post().to_string());
    assert_eq!(counts(&w).as_deref(), Some("👍 8"));
    drop(acc);
    let replacement = account(FakeTd::new(), None);
    replacement.on_update(&w, &post().to_string());
    assert_eq!(counts(&w).as_deref(), Some("👍 8"), "worker replacement must keep the projection");
}

#[test]
fn an_interaction_can_arrive_before_the_message_body() {
    let w = world();
    let acc = account(FakeTd::new(), None);
    push(&acc, &w, Some(8));
    acc.on_update(&w, &post().to_string());
    assert_eq!(counts(&w).as_deref(), Some("👍 8"));
}

#[test]
fn an_acknowledged_add_refreshes_counts_even_after_the_picker_closes_without_a_push() {
    let w = world();
    let td = FakeTd::new();
    let acc = account(td.clone(), None);
    acc.on_update(&w, &post().to_string());
    push(&acc, &w, Some(8));
    let (id, reply) = runtime::of(w.store()).await_reaction();
    acc.send(&w, &super::super::add_message_reaction(7, 42, "👍", id));
    let add = last_request(&td, "addMessageReaction");
    drop(reply);
    acc.on_update(&w, &json!({"@type": "ok", "@extra": add["@extra"]}).to_string());
    acc.drain(&w);
    let search = last_request(&td, "searchChatMessages");
    let mut confirmed = post();
    confirmed["interaction_info"] = json!({"reactions": {"reactions": [
        {"type": {"@type": "reactionTypeEmoji", "emoji": "👍"}, "total_count": 9},
    ]}});
    server_reply(&acc, &w, &search, confirmed);
    assert_eq!(counts(&w).as_deref(), Some("👍 9"));
}

#[test]
fn null_metadata_is_not_a_confirmed_removal() {
    let w = world();
    let acc = account(FakeTd::new(), None);
    acc.on_update(&w, &post().to_string());
    push(&acc, &w, Some(8));
    push(&acc, &w, None);
    acc.on_update(&w, &json!({"@type": "updateChatAvailableReactions", "chat_id": 7,
        "available_reactions": {"@type": "chatAvailableReactionsAll"}}).to_string());
    assert_eq!(counts(&w).as_deref(), Some("👍 8"));
}

fn server_reply(acc: &Account<FakeTd>, w: &World, request: &serde_json::Value, message: serde_json::Value) {
    assert!(request["query"].as_str().is_some_and(|q| !q.is_empty()) || !request["sender_id"].is_null()
        || !request["topic_id"].is_null() || !request["filter"].is_null(), "the server requires a search criterion");
    acc.on_update(w, &json!({"@type": "foundChatMessages", "@extra": request["@extra"],
        "messages": [message]}).to_string());
}

pub(super) fn confirm_empty(acc: &Account<FakeTd>, td: &FakeTd, w: &World, message: serde_json::Value) {
    acc.drain(w);
    let search = last_request(td, "searchChatMessages");
    assert_eq!(search["from_message_id"], message["id"]);
    assert_eq!(search["limit"], 1);
    server_reply(acc, w, &search, message.clone());
    let metadata = last_request(td, "getMessageAvailableReactions");
    acc.on_update(w, &json!({"@type": "availableReactions", "@extra": metadata["@extra"],
        "top_reactions": [{"type": {"@type": "reactionTypeEmoji", "emoji": "👍"}}]}).to_string());
    let confirmed = last_request(td, "searchChatMessages");
    assert_ne!(search["@extra"], confirmed["@extra"]);
    server_reply(acc, w, &confirmed, message);
}

#[test]
fn a_confirmed_removal_survives_stale_snapshots_and_worker_replacement() {
    let w = world();
    let td = FakeTd::new();
    let acc = account(td.clone(), None);
    acc.on_update(&w, &post().to_string());
    push(&acc, &w, Some(8));
    let view = runtime::of(w.store()).watch_messages(7, vec![42]);
    push(&acc, &w, None);
    assert_eq!(counts(&w).as_deref(), Some("👍 8"));
    confirm_empty(&acc, &td, &w, post());
    assert_eq!(counts(&w), None);
    drop(view);
    drop(acc);
    let replacement = account(FakeTd::new(), None);
    let mut old = post();
    old["interaction_info"] = json!({"reactions": {"reactions": [
        {"type": {"@type": "reactionTypeEmoji", "emoji": "👍"}, "total_count": 8},
    ]}});
    replacement.on_update(&w, &old.to_string());
    // An old app process can still update the legacy column of a shared DB.
    w.store().write(|c| c.execute("UPDATE tg_message SET reactions = '👍 8'", []).map(|_| ())).unwrap();
    assert_eq!(counts(&w), None, "known empty must not fall back to the legacy cache");
}

#[test]
fn an_initially_empty_metadata_cache_retries_without_hiding_counts() {
    let clock = FakeClock::default();
    let w = timed_world(&clock);
    let td = FakeTd::new();
    let acc = account(td.clone(), None);
    acc.on_update(&w, &post().to_string());
    push(&acc, &w, Some(8));
    let _view = runtime::of(w.store()).watch_messages(7, vec![42]);
    acc.drain(&w);
    let search = last_request(&td, "searchChatMessages");
    server_reply(&acc, &w, &search, post());
    let metadata = last_request(&td, "getMessageAvailableReactions");
    acc.on_update(&w, &json!({"@type": "availableReactions", "@extra": metadata["@extra"],
        "top_reactions": [], "popular_reactions": [], "recent_reactions": []}).to_string());
    assert_eq!(counts(&w).as_deref(), Some("👍 8"));
    clock.advance(3.0);
    acc.drain(&w);
    let retry = last_request(&td, "searchChatMessages");
    assert_ne!(retry["@extra"], search["@extra"]);
    let mut fresh = post();
    fresh["interaction_info"] = json!({"reactions": {"reactions": [
        {"type": {"@type": "reactionTypeEmoji", "emoji": "👍"}, "total_count": 12},
        {"type": {"@type": "reactionTypePaid"}, "total_count": 30},
        {"type": {"@type": "reactionTypeCustomEmoji", "custom_emoji_id": "123"}, "total_count": 4},
    ]}});
    server_reply(&acc, &w, &retry, fresh);
    assert_eq!(counts(&w).as_deref(), Some("👍 12 · ⭐ 30 · custom emoji 4"));
}

#[test]
fn late_empty_confirmations_cannot_erase_a_newer_update() {
    let w = world();
    let td = FakeTd::new();
    let acc = account(td.clone(), None);
    acc.on_update(&w, &post().to_string());
    push(&acc, &w, Some(8));
    let _view = runtime::of(w.store()).watch_messages(7, vec![42]);
    acc.drain(&w);
    let search = last_request(&td, "searchChatMessages");
    server_reply(&acc, &w, &search, post());
    let metadata = last_request(&td, "getMessageAvailableReactions");
    acc.on_update(&w, &json!({"@type": "availableReactions", "@extra": metadata["@extra"],
        "allow_custom_emoji": true}).to_string());
    let confirmed = last_request(&td, "searchChatMessages");
    push(&acc, &w, Some(9));
    server_reply(&acc, &w, &confirmed, post());
    assert_eq!(counts(&w).as_deref(), Some("👍 9"));
}

#[test]
fn a_quiet_view_repairs_missed_push_updates_without_expiring_its_counts() {
    let clock = FakeClock::default();
    let w = timed_world(&clock);
    let td = FakeTd::new();
    let acc = account(td.clone(), None);
    acc.on_update(&w, &post().to_string());
    push(&acc, &w, Some(8));
    let _view = runtime::of(w.store()).watch_messages(7, vec![42]);
    acc.drain(&w);
    let first = last_request(&td, "searchChatMessages");
    let message = |count| {
        let mut m = post();
        m["interaction_info"] = json!({"reactions": {"reactions": [
            {"type": {"@type": "reactionTypeEmoji", "emoji": "👍"}, "total_count": count},
        ]}});
        m
    };
    server_reply(&acc, &w, &first, message(8));
    clock.advance(61.0);
    acc.drain(&w);
    let next = last_request(&td, "searchChatMessages");
    assert_ne!(first["@extra"], next["@extra"]);
    assert_eq!(counts(&w).as_deref(), Some("👍 8"), "refreshing never blanks the footer");
    server_reply(&acc, &w, &next, message(12));
    assert_eq!(counts(&w).as_deref(), Some("👍 12"));
}

#[test]
fn a_metadata_change_invalidates_an_in_flight_removal_check() {
    let clock = FakeClock::default();
    let w = timed_world(&clock);
    let td = FakeTd::new();
    let acc = account(td.clone(), None);
    acc.on_update(&w, &post().to_string());
    push(&acc, &w, Some(8));
    let _view = runtime::of(w.store()).watch_messages(7, vec![42]);
    acc.drain(&w);
    let old = last_request(&td, "searchChatMessages");
    push(&acc, &w, None);
    acc.on_update(&w, &json!({"@type": "updateActiveEmojiReactions", "emojis": ["👍"]}).to_string());
    server_reply(&acc, &w, &old, post());
    assert_eq!(counts(&w).as_deref(), Some("👍 8"));
    clock.advance(2.0);
    confirm_empty(&acc, &td, &w, post());
    assert_eq!(counts(&w), None);
}

#[test]
fn failed_missing_and_timed_out_reads_preserve_counts_and_ignore_late_replies() {
    let clock = FakeClock::default();
    let w = timed_world(&clock);
    let td = FakeTd::new();
    let acc = account(td.clone(), None);
    acc.on_update(&w, &post().to_string());
    push(&acc, &w, Some(8));
    let _view = runtime::of(w.store()).watch_messages(7, vec![42]);
    acc.drain(&w);
    let first = last_request(&td, "searchChatMessages");
    acc.on_update(&w, &json!({"@type": "error", "@extra": first["@extra"], "code": 429,
        "message": "Too Many Requests: retry after 10"}).to_string());
    assert!(runtime::of(w.store()).operations.list().iter().all(|op|
        !matches!(op.status, crate::apps::telegram::operations::Status::Failed { .. })),
        "an automatically retried read must not flash an error row across the UI");
    clock.advance(10.0);
    acc.drain(&w);
    assert_eq!(last_request(&td, "searchChatMessages"), first);
    clock.advance(2.0);
    acc.drain(&w);
    let missing = last_request(&td, "searchChatMessages");
    // An adjacent search result says nothing about this message's counts.
    let mut adjacent = post();
    adjacent["id"] = json!(41);
    server_reply(&acc, &w, &missing, adjacent);
    assert_eq!(counts(&w).as_deref(), Some("👍 8"));
    clock.advance(5.0);
    acc.drain(&w);
    let timeout = last_request(&td, "searchChatMessages");
    clock.advance(31.0);
    acc.drain(&w);
    clock.advance(9.0);
    confirm_empty(&acc, &td, &w, post());
    server_reply(&acc, &w, &timeout, post());
    assert_eq!(counts(&w), None);
}
