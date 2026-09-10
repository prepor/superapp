use super::*;

#[test]
fn restored_panels_keep_requests_and_replies_until_the_first_worker_and_chat_are_ready() {
    use crate::apps::telegram::{panel_read::Read, panels, requests};

    let w = world();
    cached_message(&w, 7);
    let td = FakeTd::new();
    let acc = Account::new(td.clone(), 17844, tdlib_dir(), None);
    let rt = runtime::of(w.store());
    rt.prepare();
    let read = Read::start(w.store(), &requests::get_message(7, 42), w.now());
    let abandoned = Read::start(w.store(), &requests::get_message_added_reactions(7, 42, ""), w.now());
    assert!(panels::wire(w.store(), &requests::view_messages(7, &[42])));
    drop(abandoned);
    rt.prepare();

    acc.drain(&w);
    assert!(td.sent().is_empty(), "startup requests wait for authorization");
    assert!(read.poll(w.now()).is_none(), "attaching the worker must preserve pending replies");
    acc.on_update(&w, &auth("authorizationStateReady"));
    acc.drain(&w);
    assert_eq!(td.sent_types(), vec!["loadChats"], "read receipts also wait for chat restoration");

    td.push(chat_object(7, "restored chat", json!([])));
    acc.drain(&w);
    for kind in ["getMessage", "viewMessages"] {
        assert_eq!(td.sent_types().iter().filter(|sent| *sent == kind).count(), 1, "{kind} is delivered once");
    }
    assert!(!td.sent_types().contains(&"getMessageAddedReactions".to_string()),
        "a panel closed during startup cancels its queued request");
    let request = last_request(&td, "getMessage");
    acc.on_update(&w, &json!({"@type": "message", "@extra": request["@extra"], "chat_id": 7, "id": 42,
        "content": {"@type": "messageText", "text": {"text": "restored post"}},
    }).to_string());
    assert_eq!(read.poll(w.now()).unwrap().unwrap()["id"], 42);
    assert!(!rt.operations.list().iter().any(|op| matches!(op.status,
        crate::apps::telegram::operations::Status::Failed { .. })));
}

#[test]
fn retiring_an_account_drains_accepted_commands_and_projects_without_starting_new_work() {
    let w = world();
    cached_message(&w, 7);
    w.store().write(|tx| {
        tx.execute("UPDATE tg_peer SET kind = 'person' WHERE id = 7", [])?;
        Ok(())
    }).unwrap();
    let td = FakeTd::new();
    let acc = account(td.clone(), Some("+15550000000"));
    acc.drain(&w);
    let rt = runtime::of(w.store());
    assert!(rt.send_peer_action(7, super::super::PeerAction::Block));
    assert!(rt.send(r#"{"@type":"setChatDraftMessage","chat_id":7,"draft_message":null}"#));
    acc.begin_shutdown(&w);
    let sent = td.sent();
    assert_eq!(sent.len(), 2, "the final accepted commands enter TDLib once");
    assert!(!rt.can_send());
    assert!(!rt.send(r#"{"@type":"getOption","name":"version"}"#));
    assert!(rt.peer_action_pending(7), "retiring admission preserves accepted reply guards");

    struct RetiredSecrets;
    impl kernel::caps::Secrets for RetiredSecrets {
        fn get(&mut self, _: &str) -> Option<String> { panic!("closing must not start a keychain read"); }
        fn set(&mut self, _: &str, _: &str) -> bool { panic!("closing must not start a keychain write"); }
    }
    w.caps(|caps| caps.insert::<dyn kernel::caps::Secrets>(Box::new(RetiredSecrets)));

    // These may already be in the native backlog when close is requested.
    // They must not read a keychain, restart authorization, or load chats.
    for state in ["authorizationStateWaitTdlibParameters", "authorizationStateWaitPhoneNumber", "authorizationStateReady"] {
        td.push(auth(state));
    }
    td.push(auth("authorizationStateClosing"));
    let block: serde_json::Value = serde_json::from_str(&sent[0]).unwrap();
    td.push(json!({"@type": "ok", "@extra": block["@extra"]}).to_string());
    for i in 0..super::super::UPDATES_PER_PASS * 2 {
        td.push(json!({"@type": "updateChatTitle", "chat_id": 7, "title": format!("closing title {i}")}).to_string());
    }
    td.push(auth("authorizationStateClosed"));
    assert_eq!(acc.drain(&w), super::super::UPDATES_PER_PASS);
    assert_eq!(acc.drain(&w), super::super::UPDATES_PER_PASS);
    assert_eq!(acc.drain(&w), 6);
    assert_eq!(acc.drain(&w), 0);
    acc.begin_shutdown(&w);
    assert_eq!(td.sent(), sent, "no new work or duplicate commands while closing");
    assert_eq!(num(&w, "SELECT count(*) FROM tg_peer WHERE id = 7 AND blocked = 1"), 1,
        "the accepted command's confirmation still projects");
    assert_eq!(crate::apps::telegram::model::peer(w.store(), 7).unwrap().name,
        format!("closing title {}", super::super::UPDATES_PER_PASS * 2 - 1));
    assert_eq!(state(&w), "closed");
    assert!(!rt.peer_action_pending(7));
}

#[cfg(feature = "tdlib")]
#[test]
fn retiring_a_native_account_drains_receive_backpressure_without_authorizing() {
    use kernel::app::Worker;
    let env = Env::default();
    let w = kernel::app::world_for(&[], Store::open(None, &[&SCHEMA]).unwrap(), Mode::Fake, &env);
    let td = crate::apps::telegram::transport::RealTd::new();
    let account = Account::new(td, 0, tdlib_dir(), None);
    let rt = runtime::of(w.store());
    *account.commands.borrow_mut() = Some(rt.connect());
    // More responses than the bridge's bounded inbox can hold. These are
    // local option reads; no credentials, account database or network login.
    for i in 0..1024 {
        assert!(rt.send(&json!({"@type": "getOption", "name": "version", "@extra": format!("closing-{i}")}).to_string()));
    }
    let mut worker = super::super::RealWorker { tdlib_dir: tdlib_dir(), account: Some(account) };
    kernel::runtime::block_on(async {
        tokio::time::timeout(std::time::Duration::from_secs(10), worker.shutdown(&w)).await
            .expect("native closing drains a full receive queue without authorization");
    });
    let account = worker.account.as_ref().unwrap();
    assert!(account.td.is_closed());
    assert!(!account.td.has_updates());
    assert!(!account.waiting_for_parameters.get());
    assert_eq!(state(&w), "closed");
    assert!(!rt.can_send());
}

#[test]
fn commands_and_wire_updates_wake_without_waiting_for_housekeeping() {
    for command in [false, true] {
        let w = world();
        let td = FakeTd::new();
        let acc = account(td.clone(), None);
        let rt = runtime::of(w.store());
        let inbox = rt.connect();
        let timer = tokio::runtime::Builder::new_current_thread().enable_all()
            .start_paused(true).build().unwrap();
        timer.block_on(async {
            let start = tokio::time::Instant::now();
            let wait = acc.wait(&w, Wake::After(std::time::Duration::from_secs(300)));
            let input = async {
                tokio::task::yield_now().await;
                if command { assert!(rt.send(r#"{"@type":"getOption","name":"version"}"#)); }
                else { td.push(r#"{"@type":"ok"}"#); }
            };
            tokio::time::timeout(std::time::Duration::from_secs(1), async {
                tokio::join!(wait, input);
            }).await.expect("input wakes the account before its timer");
            assert_eq!(tokio::time::Instant::now(), start);
        });
        // Waiting observes availability; only the next projection pass
        // consumes a command or update, in the usual account order.
        if command { assert!(inbox.try_recv().is_ok()); }
        else { assert!(td.try_receive().is_some()); }
    }
}

#[test]
fn startup_burst_services_visible_chats_and_commands_between_batches() {
    use super::super::{BACKLOG_POLL, UPDATES_PER_PASS};
    let w = world();
    cached_message(&w, 7);
    let td = FakeTd::new();
    let acc = account(td.clone(), None);
    acc.on_ready(&w);
    let _view = watch_messages(&w, 7, vec![42]);
    td.push(chat_object(7, "restored chat", json!([])));
    for i in 1..=UPDATES_PER_PASS * 2 {
        td.push(json!({"@type": "updateChatTitle", "chat_id": 7, "title": format!("title {i}")}).to_string());
    }
    assert_eq!(next_pass(acc.drain(&w)), Wake::After(BACKLOG_POLL));
    assert_eq!(crate::apps::telegram::model::peer(w.store(), 7).unwrap().name,
        format!("title {}", UPDATES_PER_PASS - 1));
    assert_eq!(last_request(&td, "openChat")["chat_id"], 7,
        "visible chats are serviced before the startup queue empties");

    assert!(runtime::of(w.store()).send(&json!({"@type": "getOption", "name": "version"}).to_string()));
    assert_eq!(next_pass(acc.drain(&w)), Wake::After(BACKLOG_POLL));
    assert_eq!(last_request(&td, "getOption")["name"], "version");
    assert_eq!(next_pass(acc.drain(&w)), Wake::After(POLL));
    assert_eq!(crate::apps::telegram::model::peer(w.store(), 7).unwrap().name,
        format!("title {}", UPDATES_PER_PASS * 2), "all updates land in order");
}

fn cached_message(w: &World, chat: i64) {
    // A previous worker populated SQLite, not this TDLib client's memory.
    let previous = account(FakeTd::new(), None);
    previous.on_update(w, &chat_object(chat, "saved chat", json!([])));
    previous.on_update(w, &json!({"@type": "message", "chat_id": chat, "id": 42,
        "content": {"@type": "messageText", "text": {"text": "saved post"}},
        "interaction_info": {"reactions": {"reactions": [
            {"type": {"@type": "reactionTypeEmoji", "emoji": "👍"}, "total_count": 8},
        ]}},
    }).to_string());
}

#[test]
fn cached_chat_waits_for_its_announcement_before_opening_or_loading_reactions() {
    let clock = FakeClock::default();
    let w = timed_world(&clock);
    cached_message(&w, 7);
    let td = FakeTd::new();
    let acc = Account::new(td.clone(), 17844, tdlib_dir(), None);
    let rt = runtime::of(w.store());
    let view = watch_messages(&w, 7, vec![42]);
    acc.drain(&w);
    assert!(td.sent().is_empty(), "a persisted session is not authorization for this client");
    acc.on_update(&w, &auth("authorizationStateReady"));
    acc.drain(&w);
    clock.advance(45.0);
    acc.drain(&w);
    assert_eq!(td.sent_types(), vec!["loadChats"],
        "auth ready must not send openChat, getMessages or searchChatMessages before updateNewChat");
    assert_eq!(crate::apps::telegram::model::line(w.store(), 7, 42).unwrap().reactions.as_deref(), Some("👍 8"));

    td.push(chat_object(7, "restored chat", json!([])));
    acc.drain(&w);
    assert!(rt.list_syncing(), "this chat need not wait for the remaining lists");
    for kind in ["openChat", "getMessages", "searchChatMessages"] {
        assert_eq!(last_request(&td, kind)["chat_id"], 7);
    }
    let fetch = last_request(&td, "getMessages");
    acc.on_update(&w, &json!({"@type": "messages", "@extra": fetch["@extra"],
        "messages": [{"chat_id": 7, "id": 42,
            "content": {"@type": "messageText", "text": {"text": "saved post"}}}]}).to_string());
    assert_eq!(last_request(&td, "viewMessages")["message_ids"], json!([42]));
    acc.drain(&w);
    assert_eq!(td.sent_types().iter().filter(|s| *s == "openChat").count(), 1);
    drop(view);
    acc.drain(&w);
    assert_eq!(td.sent_types().iter().filter(|s| *s == "closeChat").count(), 1);
    assert!(!rt.operations.list().iter().any(|op| matches!(op.status,
        crate::apps::telegram::operations::Status::Failed { .. })));
}

#[test]
fn closing_a_view_while_its_chat_restores_does_not_leave_a_subscription() {
    let w = world();
    for chat in [7, 8] { cached_message(&w, chat); }
    let td = FakeTd::new();
    let acc = Account::new(td.clone(), 17844, tdlib_dir(), None);
    let abandoned = watch_messages(&w, 7, vec![42]);
    let _visible = watch_messages(&w, 8, vec![42]);
    acc.on_ready(&w);
    acc.drain(&w);
    drop(abandoned);
    td.push(chat_object(8, "visible", json!([])));
    acc.drain(&w);
    td.push(chat_object(7, "closed", json!([])));
    acc.drain(&w);
    let sent: Vec<serde_json::Value> = td.sent().iter().map(|raw| serde_json::from_str(raw).unwrap()).collect();
    for kind in ["openChat", "closeChat", "getMessages", "searchChatMessages"] {
        assert!(!sent.iter().any(|v| v["@type"] == kind && v["chat_id"] == 7), "{kind}");
    }
    assert_eq!(last_request(&td, "openChat")["chat_id"], 8);
    assert_eq!(last_request(&td, "searchChatMessages")["chat_id"], 8);
}

#[test]
fn picker_waits_for_authorization_and_chat_restoration_without_starting_its_reply_timeout() {
    let clock = FakeClock::default();
    let w = timed_world(&clock);
    let td = FakeTd::new();
    let acc = Account::new(td.clone(), 17844, tdlib_dir(), None);
    acc.drain(&w);
    let rt = runtime::of(w.store());
    let (id, reply) = rt.await_reaction();
    rt.send(&super::super::get_message_available_reactions(7, 42, id));
    acc.drain(&w);
    assert!(td.sent().is_empty());
    acc.on_ready(&w);
    clock.advance(45.0);
    acc.drain(&w);
    assert_eq!(td.sent_types(), vec!["loadChats"]);
    assert!(reply.lock().unwrap().is_none());
    td.push(chat_object(7, "restored", json!([])));
    acc.drain(&w);
    let request = last_request(&td, "getMessageAvailableReactions");
    acc.on_update(&w, &json!({"@type": "availableReactions", "@extra": request["@extra"],
        "top_reactions": [{"type": {"@type": "reactionTypeEmoji", "emoji": "👍"}}]}).to_string());
    assert!(matches!(reply.lock().unwrap().as_ref(), Some(runtime::ReactionResult::Choices(choices)) if choices == &["👍"]));
}

#[test]
fn canceled_and_expired_pickers_do_not_send_after_chat_restoration() {
    for expire in [false, true] {
        let w = world();
        let td = FakeTd::new();
        let acc = Account::new(td.clone(), 17844, tdlib_dir(), None);
        acc.drain(&w);
        let rt = runtime::of(w.store());
        let (id, reply) = rt.await_reaction();
        rt.send(&super::super::get_message_available_reactions(7, 42, id));
        acc.drain(&w);
        if expire {
            rt.operations.expire(w.store(), std::time::Instant::now() + std::time::Duration::from_secs(121));
            acc.drain(&w);
            assert!(matches!(reply.lock().unwrap().as_ref(), Some(runtime::ReactionResult::Error(_))),
                "a startup timeout must release the picker even before sign-in finishes");
        }
        drop(reply);
        acc.on_ready(&w);
        td.push(chat_object(7, "restored", json!([])));
        acc.drain(&w);
        assert_eq!(td.sent_types(), vec!["loadChats"], "never replay a closed or expired picker");
    }
}

#[test]
fn a_waiting_urgent_count_check_does_not_block_a_restored_visible_chat() {
    let clock = FakeClock::default();
    let w = timed_world(&clock);
    for chat in [7, 8] { cached_message(&w, chat); }
    let td = FakeTd::new();
    let acc = Account::new(td.clone(), 17844, tdlib_dir(), None);
    let _view = watch_messages(&w, 8, vec![42]);
    acc.on_ready(&w);
    acc.counts_after_add(&w, 7, 42);
    acc.drain(&w);
    assert_eq!(td.sent_types(), vec!["loadChats"]);
    td.push(chat_object(8, "visible", json!([])));
    acc.drain(&w);
    let search = last_request(&td, "searchChatMessages");
    assert_eq!(search["chat_id"], 8, "the urgent request must not bypass readiness or starve ready chats");
    acc.on_update(&w, &json!({"@type": "foundChatMessages", "@extra": search["@extra"],
        "messages": [{"chat_id": 8, "id": 42, "interaction_info": {
            "reactions": {"reactions": []}}}]}).to_string());
    td.push(chat_object(7, "urgent", json!([])));
    clock.advance(2.0);
    acc.drain(&w);
    assert_eq!(last_request(&td, "searchChatMessages")["chat_id"], 7);
}

#[test]
fn a_chat_missing_from_both_lists_reports_its_actual_error_instead_of_waiting_forever() {
    let w = world();
    let td = FakeTd::new();
    let acc = Account::new(td.clone(), 17844, tdlib_dir(), None);
    acc.on_ready(&w);
    acc.drain(&w);
    let rt = runtime::of(w.store());
    let (id, reply) = rt.await_reaction();
    acc.send(&w, &super::super::get_message_available_reactions(7, 42, id));
    for list in ["main", "archive"] {
        let load = last_request(&td, "loadChats");
        assert_eq!(load["@extra"]["context"], format!("load_chats:{list}"));
        acc.on_update(&w, &json!({"@type": "error", "code": 404, "@extra": load["@extra"]}).to_string());
    }
    acc.drain(&w);
    let query = last_request(&td, "getMessageAvailableReactions");
    acc.on_update(&w, &json!({"@type": "error", "code": 400, "message": "Chat not found",
        "@extra": query["@extra"]}).to_string());
    assert!(matches!(reply.lock().unwrap().as_ref(), Some(runtime::ReactionResult::Error(error)) if error == "Chat not found"));
}

#[test]
fn losing_authorization_forgets_the_clients_chat_readiness() {
    let w = world();
    let td = FakeTd::new();
    let acc = account(td.clone(), None);
    acc.on_update(&w, &chat_object(7, "old client", json!([])));
    acc.on_update(&w, &auth("authorizationStateWaitTdlibParameters"));
    acc.on_update(&w, &auth("authorizationStateReady"));
    let _view = watch_messages(&w, 7, vec![42]);
    acc.drain(&w);
    assert!(!td.sent_types().iter().any(|t| t == "openChat"));
    td.push(chat_object(7, "new client", json!([])));
    acc.drain(&w);
    assert_eq!(td.sent_types().iter().filter(|s| *s == "openChat").count(), 1);
}
