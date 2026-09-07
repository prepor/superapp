use super::*;
use crate::apps::telegram::{model, requests, topics};

const GROUP: i64 = -1_000_000_000_042;

fn forum(acc: &Account<FakeTd>, w: &World) {
    acc.on_update(
        w,
        &json!({"@type": "updateSupergroup", "supergroup": {
            "id": 42, "is_forum": true, "member_count": 1000
        }})
        .to_string(),
    );
    acc.on_update(
        w,
        &chat_object(
            GROUP,
            "Вастрик.Берлин",
            json!([
                {"list": {"@type": "chatListMain"}, "order": "42"}
            ]),
        ),
    );
}

fn topic(id: i64, name: &str) -> serde_json::Value {
    json!({"@type": "forumTopic", "info": {"chat_id": GROUP, "forum_topic_id": id,
        "name": name, "is_closed": false, "is_hidden": false},
        "unread_count": 3, "last_read_inbox_message_id": 1,
        "unread_mention_count": 0, "notification_settings": {"mute_for": 0},
        "draft_message": null})
}

fn line(id: i64, topic: i64) -> serde_json::Value {
    json!({"id": id, "chat_id": GROUP, "date": id,
        "topic_id": {"@type": "messageTopicForum", "forum_topic_id": topic},
        "content": {"@type": "messageText", "text": {"text": format!("topic {topic} line {id}")}}})
}

#[test]
fn topic_responses_do_not_feed_a_request_loop() {
    let td = FakeTd::new();
    let acc = account(td.clone(), None);
    let w = world();
    forum(&acc, &w);
    for id in [20, 21, 22] {
        acc.on_update(
            &w,
            &json!({"@type": "updateNewMessage", "message": line(id, 2)}).to_string(),
        );
    }
    assert_eq!(
        td.sent_types(),
        vec!["getForumTopic"],
        "one request covers the burst"
    );
    let request: serde_json::Value = serde_json::from_str(&td.sent()[0]).unwrap();
    let update = json!({"@type": "updateForumTopic", "chat_id": GROUP, "forum_topic_id": 2,
        "last_read_inbox_message_id": 20, "last_read_outbox_message_id": 0,
        "unread_mention_count": 1, "notification_settings": {"mute_for": 0}, "draft_message": null});
    let mut answer = topic(2, "Meetups");
    answer["@extra"] = request["@extra"].clone();
    // TDLib emits updateForumTopic for a fetched topic, even if no fields
    // changed. This is the sequence repeated thousands of times in the log.
    for _ in 0..5 {
        acc.on_update(&w, &update.to_string());
        acc.on_update(&w, &answer.to_string());
    }
    acc.drain(&w);
    assert_eq!(td.sent_types(), vec!["getForumTopic"]);
    assert_eq!(topics::get(w.store(), GROUP, 2).unwrap().name, "Meetups");
    assert_eq!(model::history_in(w.store(), GROUP, 2).len(), 3);
    acc.on_update(
        &w,
        &json!({"@type": "updateNewMessage", "message": line(23, 2)}).to_string(),
    );
    assert_eq!(
        td.sent_types(),
        vec!["getForumTopic", "getForumTopic"],
        "a later message can refresh counts"
    );
}

#[test]
fn private_chat_topic_ids_do_not_trigger_forum_requests_or_hide_messages() {
    let td = FakeTd::new();
    let acc = account(td.clone(), None);
    let w = world();
    let user = json!({"@type": "updateUser", "user": {"id": 7, "first_name": "Person",
        "type": {"@type": "userTypeRegular"}}});
    acc.on_update(&w, &user.to_string());
    let mut message = line(30, 2996);
    message["chat_id"] = json!(7);
    acc.on_update(
        &w,
        &json!({"@type": "updateNewMessage", "message": message}).to_string(),
    );
    assert!(td.sent().is_empty());
    assert!(!model::peer(w.store(), 7).unwrap().is_forum);
    assert_eq!(model::history(w.store(), 7).len(), 1);
    assert_eq!(model::message_topic(w.store(), 7, 30), 0);

    // Repair a message cached by the earlier build, keeping its content.
    w.store()
        .write(|c| {
            c.execute("UPDATE tg_message SET topic = 2996 WHERE chat = 7", [])?;
            Ok(())
        })
        .unwrap();
    acc.on_update(&w, &user.to_string());
    assert_eq!(model::history(w.store(), 7)[0].text, "topic 2996 line 30");
    assert_eq!(model::message_topic(w.store(), 7, 30), 0);

    // Bot forums remain valid: their user metadata advertises topics.
    let bot = json!({"@type": "updateUser", "user": {"id": 8, "first_name": "Bot",
        "type": {"@type": "userTypeBot", "has_topics": true}}});
    acc.on_update(&w, &bot.to_string());
    message["chat_id"] = json!(8);
    acc.on_update(
        &w,
        &json!({"@type": "updateNewMessage", "message": message}).to_string(),
    );
    assert!(model::peer(w.store(), 8).unwrap().is_forum);
    assert_eq!(model::history_in(w.store(), 8, 2996).len(), 1);
    assert_eq!(td.sent_types(), vec!["getForumTopic"]);
}

#[test]
fn cached_topic_panels_wait_for_this_clients_auth_and_chat_list() {
    let td = FakeTd::new();
    let acc = Account::new(td.clone(), 17844, tdlib_dir(), None);
    let w = world();
    // Both the cached forum and authorization row survived a previous run.
    forum(&acc, &w);
    w.store()
        .write(|c| schema::set_session(c, None, "ready", None, 0.0))
        .unwrap();
    acc.drain(&w);
    let rt = runtime::of(w.store());
    rt.want_topic_history(GROUP, 2);
    rt.refresh_topics(GROUP);
    rt.refresh_topics(GROUP);
    acc.drain(&w);
    assert!(
        td.sent().is_empty(),
        "a persisted ready row cannot authorize the new client"
    );
    assert_eq!(rt.topics_status(GROUP), Ok(true));

    acc.on_update(&w, &auth("authorizationStateWaitTdlibParameters"));
    acc.drain(&w);
    assert_eq!(td.sent_types(), vec!["setTdlibParameters"]);
    acc.on_update(&w, &auth("authorizationStateReady"));
    acc.drain(&w);
    assert_eq!(td.sent_types(), vec!["setTdlibParameters", "loadChats"]);
    for list in ["main", "archive"] {
        acc.on_update(
            &w,
            &json!({"@type": "error", "code": 404,
            "@extra": format!("load_chats:{list}")})
            .to_string(),
        );
    }
    acc.drain(&w);
    assert_eq!(
        td.sent_types(),
        vec![
            "setTdlibParameters",
            "loadChats",
            "loadChats",
            "getForumTopic",
            "getForumTopics",
            "getForumTopicHistory"
        ]
    );
    let request: serde_json::Value = serde_json::from_str(td.sent().last().unwrap()).unwrap();
    assert_eq!(request["forum_topic_id"], 2);
}

#[test]
fn a_timed_out_topic_lookup_can_refresh_again() {
    let td = FakeTd::new();
    let acc = account(td.clone(), None);
    let w = world();
    forum(&acc, &w);
    acc.on_update(
        &w,
        &json!({"@type": "updateNewMessage", "message": line(20, 2)}).to_string(),
    );
    let first: serde_json::Value = serde_json::from_str(&td.sent()[0]).unwrap();
    runtime::of(w.store()).operations.expire(
        w.store(),
        std::time::Instant::now() + std::time::Duration::from_secs(121),
    );
    acc.on_update(
        &w,
        &json!({"@type": "updateNewMessage", "message": line(21, 2)}).to_string(),
    );
    assert_eq!(td.sent_types(), vec!["getForumTopic", "getForumTopic"]);
    let retry: serde_json::Value = serde_json::from_str(&td.sent()[1]).unwrap();
    assert_eq!(first["@extra"], retry["@extra"]);
    let mut answer = topic(2, "Meetups");
    answer["@extra"] = retry["@extra"].clone();
    acc.on_update(&w, &answer.to_string());
    assert!(runtime::of(w.store())
        .operations
        .list()
        .iter()
        .all(|op| op.status == crate::apps::telegram::operations::Status::Done));
}

#[test]
fn disabling_forum_mode_restores_the_chat_without_losing_topic_preferences() {
    let td = FakeTd::new();
    let acc = account(td.clone(), None);
    let w = world();
    forum(&acc, &w);
    acc.on_topic(&w, GROUP, &topic(2, "Meetups"));
    w.store()
        .write(|c| topics::select_tx(c, GROUP, &[2], true))
        .unwrap();
    acc.on_update(
        &w,
        &json!({"@type": "updateNewMessage", "message": line(20, 2)}).to_string(),
    );
    // A partial group update must not erase the known capability.
    acc.on_update(
        &w,
        &json!({"@type": "updateSupergroup", "supergroup": {"id": 42,
        "member_count": 1001}})
        .to_string(),
    );
    assert!(model::peer(w.store(), GROUP).unwrap().is_forum);
    acc.on_update(
        &w,
        &json!({"@type": "updateSupergroup", "supergroup": {"id": 42,
        "is_forum": false}})
        .to_string(),
    );
    assert!(!model::peer(w.store(), GROUP).unwrap().is_forum);
    assert_eq!(model::history(w.store(), GROUP).len(), 1);
    assert!(topics::get(w.store(), GROUP, 2).unwrap().selected);
    acc.drain(&w);
    runtime::of(w.store()).want_topic_history(GROUP, 2);
    runtime::of(w.store()).refresh_topics(GROUP);
    let sent = td.sent().len();
    acc.drain(&w);
    assert_eq!(
        td.sent().len(),
        sent,
        "the cached topic cannot trigger invalid forum requests"
    );
    assert!(!runtime::of(w.store()).loading_in(GROUP, 2));
    assert!(runtime::of(w.store()).topics_status(GROUP).is_err());
}

#[test]
fn forum_discovery_pages_topics_and_preserves_selection_on_refresh() {
    let td = FakeTd::new();
    let acc = account(td.clone(), None);
    let w = world();
    forum(&acc, &w);
    assert!(model::peer(w.store(), GROUP).unwrap().is_forum);
    acc.on_update(
        &w,
        &json!({"@type": "forumTopics", "topics": [topic(1, "General"), topic(2, "Meetups")],
        "next_offset_date": 10, "next_offset_message_id": 100, "next_offset_forum_topic_id": 2,
        "@extra": format!("topics:{GROUP}:0:0:0")})
        .to_string(),
    );
    let sent: serde_json::Value = serde_json::from_str(td.sent().last().unwrap()).unwrap();
    assert_eq!(sent["offset_forum_topic_id"], 2);
    assert!(runtime::of(w.store()).topics_status(GROUP).unwrap());
    w.store()
        .write(|c| topics::select_tx(c, GROUP, &[2], true))
        .unwrap();
    acc.on_update(
        &w,
        &json!({"@type": "forumTopics", "topics": [topic(3, "Housing")],
        "next_offset_forum_topic_id": 0, "@extra": sent["@extra"]})
        .to_string(),
    );
    assert!(!runtime::of(w.store()).topics_status(GROUP).unwrap());
    assert_eq!(topics::list(w.store(), GROUP).len(), 3);
    let mut renamed = topic(2, "Meetups and coffee");
    renamed["@extra"] = json!(format!("topic:{GROUP}:2"));
    acc.on_update(&w, &renamed.to_string());
    let t = topics::get(w.store(), GROUP, 2).unwrap();
    assert!(t.selected);
    assert_eq!(t.name, "Meetups and coffee");
    acc.on_update(
        &w,
        &json!({"@type": "error", "code": 500,
        "@extra": format!("topics:{GROUP}:0:0:0")})
        .to_string(),
    );
    assert!(runtime::of(w.store()).topics_status(GROUP).is_err());
    assert!(topics::get(w.store(), GROUP, 2).unwrap().selected);
}

#[test]
fn topic_history_uses_its_own_page_cursor_and_ignores_other_topics() {
    let clock = FakeClock::default();
    let w = timed_world(&clock);
    let td = FakeTd::new();
    let acc = account(td.clone(), None);
    forum(&acc, &w);
    runtime::of(w.store()).want_topic_history(GROUP, 2);
    acc.drain(&w);
    let sent: serde_json::Value = serde_json::from_str(td.sent().last().unwrap()).unwrap();
    assert_eq!(sent["@type"], "getForumTopicHistory");
    assert_eq!(sent["forum_topic_id"], 2);
    assert_eq!(sent["from_message_id"], 0);
    acc.on_update(
        &w,
        &json!({"@type": "messages", "messages": [line(20, 2), line(10, 2), line(30, 3)],
        "@extra": sent["@extra"]})
        .to_string(),
    );
    assert_eq!(model::history_in(w.store(), GROUP, 2).len(), 2);
    assert!(model::history_in(w.store(), GROUP, 3).is_empty());
    clock.advance(PAGE_GAP + 0.1);
    acc.drain(&w);
    let next: serde_json::Value = serde_json::from_str(td.sent().last().unwrap()).unwrap();
    assert_eq!(next["from_message_id"], 10);
    assert_eq!(next["forum_topic_id"], 2);
    acc.on_update(
        &w,
        &json!({"@type": "messages", "messages": [], "@extra": next["@extra"]}).to_string(),
    );
    assert!(!runtime::of(w.store()).loading_in(GROUP, 2));
    assert!(!runtime::of(w.store()).loading_in(GROUP, 3));
}

#[test]
fn a_timed_out_topic_page_can_retry_without_releasing_another_topic() {
    let clock = FakeClock::default();
    let w = timed_world(&clock);
    let td = FakeTd::new();
    let acc = account(td.clone(), None);
    acc.want_in(&w, GROUP, 2);
    acc.drain(&w);
    let first: serde_json::Value = serde_json::from_str(td.sent().last().unwrap()).unwrap();
    acc.want_in(&w, GROUP, 3);
    clock.advance(super::super::PAGE_PATIENCE + 1.0);
    acc.drain(&w);
    let second: serde_json::Value = serde_json::from_str(td.sent().last().unwrap()).unwrap();
    let runtime = runtime::of(w.store());
    assert!(!runtime.loading_in(GROUP, 2));
    assert!(runtime.loading_in(GROUP, 3));
    assert!(runtime
        .operations
        .list()
        .iter()
        .any(|op| op.id == first["@extra"]["operation"].as_u64().unwrap() && op.retryable()));
    acc.on_update(
        &w,
        &json!({"@type": "error", "code": 400, "message": "late reply",
        "@extra": first["@extra"]})
        .to_string(),
    );
    acc.want_in(&w, GROUP, 2);
    clock.advance(2.0);
    acc.drain(&w);
    assert_eq!(td.sent().len(), 2, "topic 3 still owns the request slot");
    assert!(runtime.loading_in(GROUP, 3));
    acc.on_update(
        &w,
        &json!({"@type": "messages", "messages": [],
        "@extra": second["@extra"]})
        .to_string(),
    );
    acc.drain(&w);
    let retry: serde_json::Value = serde_json::from_str(td.sent().last().unwrap()).unwrap();
    assert_eq!(retry["forum_topic_id"], 2);
    assert_eq!(retry["@extra"]["operation"], first["@extra"]["operation"]);
}

#[test]
fn refused_topic_sends_retain_the_topic_and_reply_for_explicit_retry() {
    let td = FakeTd::new();
    let acc = account(td.clone(), None);
    let w = world();
    forum(&acc, &w);
    for id in [2, 3] {
        acc.on_topic(&w, GROUP, &topic(id, "topic"));
    }
    w.store()
        .write(|c| {
            topics::draft_tx(c, GROUP, 0, "group draft")?;
            topics::draft_tx(c, GROUP, 3, "housing draft")
        })
        .unwrap();
    acc.drain(&w);
    let runtime = runtime::of(w.store());
    assert!(runtime.send(&requests::in_topic(
        requests::send_message(GROUP, "coffee: 10:00", Some(25)),
        2,
    )));
    acc.drain(&w);
    let request: serde_json::Value = serde_json::from_str(td.sent().last().unwrap()).unwrap();
    acc.on_update(
        &w,
        &json!({"@type": "error", "code": 400, "message": "TOPIC_CLOSED",
        "@extra": request["@extra"]})
        .to_string(),
    );
    let id = request["@extra"]["operation"].as_u64().unwrap();
    let retry: serde_json::Value =
        serde_json::from_str(&runtime.operations.retry(id).unwrap()).unwrap();
    assert_eq!(retry["topic_id"]["forum_topic_id"], 2);
    assert_eq!(retry["reply_to"]["message_id"], 25);
    assert_eq!(
        retry["input_message_content"]["text"]["text"],
        "coffee: 10:00"
    );
    assert!(
        retry["@extra"]["context"].is_null(),
        "message text is not correlation metadata"
    );
    assert_eq!(
        topics::get(w.store(), GROUP, 3).unwrap().draft.as_deref(),
        Some("housing draft")
    );
    assert_eq!(
        model::peer(w.store(), GROUP).unwrap().draft.as_deref(),
        Some("group draft")
    );
}

#[test]
fn muting_a_topic_waits_for_acknowledgement_and_keeps_sibling_settings() {
    let td = FakeTd::new();
    let acc = account(td.clone(), None);
    let w = world();
    forum(&acc, &w);
    for id in [2, 3] {
        acc.on_topic(&w, GROUP, &topic(id, "topic"));
    }
    acc.drain(&w);
    let runtime = runtime::of(w.store());
    assert!(runtime.send(&requests::set_topic_muted(GROUP, 2, true)));
    acc.drain(&w);
    let request: serde_json::Value = serde_json::from_str(td.sent().last().unwrap()).unwrap();
    assert!(!topics::get(w.store(), GROUP, 2).unwrap().muted);
    acc.on_update(
        &w,
        &json!({"@type": "error", "code": 400, "message": "refused",
        "@extra": request["@extra"]})
        .to_string(),
    );
    assert!(!topics::get(w.store(), GROUP, 2).unwrap().muted);
    let id = request["@extra"]["operation"].as_u64().unwrap();
    assert!(runtime.send(&runtime.operations.retry(id).unwrap()));
    acc.drain(&w);
    acc.on_update(
        &w,
        &json!({"@type": "ok", "@extra": request["@extra"]}).to_string(),
    );
    assert!(topics::get(w.store(), GROUP, 2).unwrap().muted);
    assert!(!topics::get(w.store(), GROUP, 3).unwrap().muted);
    assert!(!model::peer(w.store(), GROUP).unwrap().muted);
}

#[test]
fn identical_topic_ids_in_different_groups_and_general_are_separate() {
    let w = world();
    let td = FakeTd::new();
    let acc = account(td, None);
    forum(&acc, &w);
    acc.on_topic(&w, GROUP, &topic(1, "General"));
    acc.on_topic(&w, GROUP - 1, &topic(1, "Other general"));
    acc.on_update(
        &w,
        &json!({"@type": "updateNewMessage", "message": line(100, 1)}).to_string(),
    );
    assert_eq!(model::history_in(w.store(), GROUP, 1).len(), 1);
    assert!(model::history_in(w.store(), GROUP - 1, 1).is_empty());
    w.store()
        .write(|c| topics::select_tx(c, GROUP, &[1], true))
        .unwrap();
    assert!(!topics::get(w.store(), GROUP - 1, 1).unwrap().selected);
}

#[test]
fn topic_updates_keep_their_own_drafts_reads_and_notification_settings() {
    let w = world();
    let td = FakeTd::new();
    let acc = account(td, None);
    forum(&acc, &w);
    for id in [2, 3] {
        acc.on_topic(&w, GROUP, &topic(id, "topic"));
    }
    w.store()
        .write(|c| model::set_muted_tx(c, GROUP, true))
        .unwrap();
    assert!(topics::get(w.store(), GROUP, 2).unwrap().muted);
    acc.on_update(
        &w,
        &json!({"@type": "updateForumTopic", "chat_id": GROUP, "forum_topic_id": 2,
            "last_read_inbox_message_id": 25, "last_read_outbox_message_id": 30,
            "notification_settings": {"use_default_mute_for": false, "mute_for": 0},
            "draft_message": {"input_message_text": {"text": {"text": "phone draft"}}}
        })
        .to_string(),
    );
    let t = topics::get(w.store(), GROUP, 2).unwrap();
    assert_eq!(t.draft.as_deref(), Some("phone draft"));
    assert_eq!(t.last_read, Some(25));
    assert!(!t.muted);
    assert!(topics::get(w.store(), GROUP, 3).unwrap().muted);
    assert_eq!(topics::get(w.store(), GROUP, 3).unwrap().last_read, Some(1));
    // A group-wide outbox cursor must not mark another topic's messages read.
    let mut outgoing = line(20, 3);
    outgoing["is_outgoing"] = json!(true);
    acc.on_update(
        &w,
        &json!({"@type": "updateNewMessage", "message": outgoing}).to_string(),
    );
    acc.on_update(
        &w,
        &json!({"@type": "updateChatReadOutbox", "chat_id": GROUP,
        "last_read_outbox_message_id": 100})
        .to_string(),
    );
    assert_eq!(
        model::history_in(w.store(), GROUP, 3)[0].state.as_deref(),
        Some("sent")
    );
}
