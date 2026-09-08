use super::*;

fn live_world(clock: &FakeClock) -> World {
    let w = timed_world(clock);
    w.caps(|caps| caps.insert(Box::new(runtime::Delivery::Live)));
    w
}

fn preview(view: &mut Option<runtime::MessageView>, w: &World, chat: i64, topic: i64, ids: Vec<i64>) {
    runtime::show_messages(view, w, chat, Some(topic), ids);
}

fn reply(request: &serde_json::Value, ids: &[i64]) -> String {
    let mut reply: serde_json::Value = serde_json::from_str(&history_page(
        request["chat_id"].as_i64().unwrap(), ids, request["@extra"]["context"].as_str().unwrap(),
    )).unwrap();
    reply["@extra"] = request["@extra"].clone();
    reply.to_string()
}

#[test]
fn rapid_previews_only_refresh_the_chat_that_settles() {
    let clock = FakeClock::default();
    let w = live_world(&clock);
    let td = FakeTd::new();
    let acc = account(td.clone(), None);
    w.store().write(|c| {
        for chat in 1..=100 {
            super::super::ensure_peer(c, chat)?;
            crate::apps::telegram::model::ensure_chat_tx(c, chat)?;
            c.execute("INSERT INTO tg_message(chat,id,date,text) VALUES(?1,42,1,'cached message')", [chat])?;
            crate::apps::telegram::reaction_state::refresh(c, chat, 42)?;
        }
        Ok(())
    }).unwrap();
    let mut view = None;
    for chat in 1..=100 {
        // Exercise both retained widgets changing identity and replaced widgets.
        if chat % 2 == 0 { view = None; }
        preview(&mut view, &w, chat, 0, vec![42]);
        runtime::want_view_file(&view, &format!("photo-{chat}"));
        acc.drain(&w);
        clock.advance(0.04);
        acc.drain(&w);
    }
    assert!(td.sent().is_empty(), "traversed chats must not send history, message, or reaction refreshes");
    clock.advance(runtime::VIEW_SETTLE);
    // A redraw of the same chat must not restart the settling clock.
    preview(&mut view, &w, 100, 0, vec![42]);
    acc.drain(&w);
    for kind in ["getChatHistory", "getMessages", "searchChatMessages"] {
        let sent: Vec<_> = td.sent().iter().map(|raw| serde_json::from_str::<serde_json::Value>(raw).unwrap())
            .filter(|v| v["@type"] == kind).collect();
        assert_eq!(sent.len(), 1, "one {kind} for the settled chat");
        assert_eq!(sent[0]["chat_id"], 100);
    }
    assert_eq!(last_request(&td, "getRemoteFile")["remote_file_id"], "photo-100");
    assert_eq!(td.sent_types().iter().filter(|t| *t == "getRemoteFile").count(), 1);
    let count = td.sent().len();
    for _ in 0..5 {
        clock.advance(0.1);
        acc.drain(&w);
    }
    assert_eq!(td.sent().len(), count, "redraws and worker passes must not restart refreshes");
}

#[test]
fn leaving_a_chat_cancels_queued_pages_and_late_replies_cannot_restart_a_visit() {
    let clock = FakeClock::default();
    let w = live_world(&clock);
    let td = FakeTd::new();
    let acc = account(td.clone(), None);
    let mut view = None;
    preview(&mut view, &w, 7, 0, vec![]);
    clock.advance(runtime::VIEW_SETTLE);
    acc.drain(&w);
    let first = last_request(&td, "getChatHistory");
    acc.on_update(&w, &reply(&first, &[42, 41]));
    assert_eq!(acc.pages.borrow().len(), 1, "the settled chat can keep filling its history");

    preview(&mut view, &w, 8, 0, vec![]);
    acc.drain(&w);
    assert!(acc.pages.borrow().is_empty(), "leaving must discard the next page");
    assert!(!runtime::of(w.store()).loading(7));
    clock.advance(PAGE_GAP);
    acc.drain(&w);
    assert_eq!(last_request(&td, "getChatHistory")["chat_id"], 8);

    // Leave a request on the wire and return before its answer. Waiting for
    // the old request's thirty-second timeout would make switching slow again.
    preview(&mut view, &w, 7, 0, vec![]);
    clock.advance(PAGE_GAP);
    acc.drain(&w);
    let reopened = last_request(&td, "getChatHistory");
    assert_eq!(reopened["chat_id"], 7);
    assert_ne!(first["@extra"], reopened["@extra"]);
    let pending = acc.in_flight.get();
    acc.on_update(&w, &reply(&first, &[40, 39]));
    assert_eq!(acc.in_flight.get(), pending, "the previous visit cannot release the new request");
    assert!(acc.pages.borrow().is_empty(), "a late answer cannot restart an abandoned walk");

    runtime::want_view_file(&view, "abandoned-photo");
    drop(view);
    // Replies can arrive before the next worker pass prunes its queue.
    let count = td.sent().len();
    let photo: serde_json::Value = serde_json::from_str(&photo_message(42, 7, 1, "abandoned-photo")).unwrap();
    acc.on_update(&w, &json!({"@type": "messages", "@extra": reopened["@extra"],
        "messages": [photo["message"]]}).to_string());
    assert_eq!(td.sent().len(), count);
    acc.drain(&w);
    clock.advance(60.0);
    acc.drain(&w);
    assert!(acc.pages.borrow().is_empty());
    assert!(!runtime::of(w.store()).loading(7));
    assert!(!td.sent_types().iter().any(|t| t == "getRemoteFile" || t == "downloadFile"));
}

#[test]
fn duplicate_visible_panels_share_history_and_empty_chats_load_after_startup() {
    let clock = FakeClock::default();
    let w = live_world(&clock);
    let td = FakeTd::new();
    let acc = Account::new(td.clone(), 17844, tdlib_dir(), None);
    let mut abandoned = None;
    let mut first = None;
    preview(&mut abandoned, &w, 7, 0, vec![]);
    clock.advance(runtime::VIEW_SETTLE);
    acc.drain(&w);
    drop(abandoned);
    preview(&mut first, &w, 8, 0, vec![]);
    let mut second = None;
    preview(&mut second, &w, 8, 0, vec![]);
    clock.advance(runtime::VIEW_SETTLE);
    acc.drain(&w);
    assert!(td.sent().is_empty());
    acc.on_update(&w, &auth("authorizationStateReady"));
    acc.drain(&w);
    assert!(!td.sent_types().contains(&"getChatHistory".to_string()));
    for list in ["main", "archive"] {
        acc.on_update(&w, &json!({"@type": "error", "code": 404,
            "@extra": format!("load_chats:{list}")}).to_string());
    }
    acc.drain(&w);
    let request = last_request(&td, "getChatHistory");
    assert_eq!(request["chat_id"], 8, "startup must forget abandoned previews");
    drop(first);
    acc.on_update(&w, &reply(&request, &[]));
    clock.advance(10.0);
    acc.drain(&w);
    assert_eq!(td.sent_types().iter().filter(|t| *t == "getChatHistory").count(), 1);
    assert!(!runtime::of(w.store()).loading(8));
    assert!(runtime::view_settled(&second, w.now()));
}

#[test]
fn an_abandoned_history_request_still_honors_telegrams_flood_wait() {
    let clock = FakeClock::default();
    let w = live_world(&clock);
    let td = FakeTd::new();
    let acc = account(td.clone(), None);
    let mut view = None;
    preview(&mut view, &w, 7, 0, vec![]);
    clock.advance(runtime::VIEW_SETTLE);
    acc.drain(&w);
    let request = last_request(&td, "getChatHistory");
    preview(&mut view, &w, 8, 0, vec![]);
    acc.drain(&w);
    acc.on_update(&w, &json!({"@type": "error", "code": 429, "message": "Too Many Requests: retry after 30",
        "@extra": request["@extra"]}).to_string());
    clock.advance(20.0);
    acc.drain(&w);
    assert_eq!(td.sent_types(), vec!["getChatHistory"]);
    clock.advance(12.0);
    acc.drain(&w);
    assert_eq!(last_request(&td, "getChatHistory")["chat_id"], 8);
    assert_eq!(td.sent_types(), vec!["getChatHistory", "getChatHistory"]);
}

#[test]
fn leaving_during_a_retry_wait_removes_the_retry_and_its_failure() {
    let clock = FakeClock::default();
    let w = live_world(&clock);
    let td = FakeTd::new();
    let acc = account(td.clone(), None);
    let mut view = None;
    preview(&mut view, &w, 7, 0, vec![]);
    clock.advance(runtime::VIEW_SETTLE);
    acc.drain(&w);
    let request = last_request(&td, "getChatHistory");
    acc.on_update(&w, &json!({"@type": "error", "code": 429, "message": "Too Many Requests: retry after 30",
        "@extra": request["@extra"]}).to_string());
    assert_eq!(acc.pages.borrow().len(), 1);
    drop(view);
    acc.drain(&w);
    assert!(acc.pages.borrow().is_empty());
    assert!(runtime::of(w.store()).operations.list().is_empty());
    clock.advance(60.0);
    acc.drain(&w);
    assert_eq!(td.sent_types(), vec!["getChatHistory"]);
}

#[test]
fn topic_previews_only_refresh_the_settled_topic() {
    let clock = FakeClock::default();
    let w = live_world(&clock);
    let td = FakeTd::new();
    let acc = account(td.clone(), None);
    w.store().write(|c| {
        super::super::ensure_peer(c, 7)?;
        crate::apps::telegram::model::ensure_chat_tx(c, 7)?;
        c.execute("UPDATE tg_peer SET is_forum = 1 WHERE id = 7", []).map(|_| ())
    }).unwrap();
    let mut view = None;
    for topic in 1..=30 {
        preview(&mut view, &w, 7, topic, vec![]);
        clock.advance(0.04);
        acc.drain(&w);
    }
    assert!(td.sent().is_empty());
    clock.advance(runtime::VIEW_SETTLE);
    acc.drain(&w);
    assert_eq!(td.sent_types(), vec!["getForumTopic", "getForumTopicHistory"]);
    let request = last_request(&td, "getForumTopicHistory");
    assert_eq!(request["forum_topic_id"], 30);
    assert_eq!(last_request(&td, "getForumTopic")["forum_topic_id"], 30);
    assert!(runtime::of(w.store()).loading_in(7, 30));
    acc.on_update(&w, &reply(&request, &[]));
    assert!(!runtime::of(w.store()).loading_in(7, 30));
    clock.advance(10.0);
    acc.drain(&w);
    assert_eq!(td.sent().len(), 2);
}

#[test]
fn explicit_commands_and_history_retries_do_not_wait_for_another_visit() {
    let clock = FakeClock::default();
    let w = live_world(&clock);
    let td = FakeTd::new();
    let acc = account(td.clone(), None);
    acc.drain(&w);
    let rt = runtime::of(w.store());
    let mut view = None;
    preview(&mut view, &w, 7, 0, vec![]);
    assert!(rt.send(&json!({"@type": "sendMessage", "chat_id": 7,
        "input_message_content": {"@type": "inputMessageText", "text": {"@type": "formattedText", "text": "hello"}}
    }).to_string()));
    acc.drain(&w);
    assert_eq!(td.sent_types(), vec!["sendMessage"], "a deliberate command must be immediate");
    clock.advance(runtime::VIEW_SETTLE);
    acc.drain(&w);
    let request = last_request(&td, "getChatHistory");
    acc.on_update(&w, &json!({"@type": "error", "code": 500, "message": "temporary failure",
        "@extra": request["@extra"]}).to_string());
    let retry = rt.operations.retry(request["@extra"]["operation"].as_u64().unwrap()).unwrap();
    assert!(rt.send(&retry));
    acc.drain(&w);
    let retried = last_request(&td, "getChatHistory");
    acc.on_update(&w, &reply(&retried, &[42]));
    assert_eq!(crate::apps::telegram::model::history(w.store(), 7).len(), 1);
    assert_eq!(acc.pages.borrow().len(), 1, "an explicit retry still resumes the active history walk");
}
