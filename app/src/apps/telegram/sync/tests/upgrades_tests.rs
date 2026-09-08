use super::*;
use crate::apps::telegram::{model, upgrades};
use serde_json::Value;

const OLD: i64 = -123;
const NEW: i64 = -1_000_000_000_456;

fn answer(acc: &Account<FakeTd>, w: &World, request: &Value, messages: Vec<Value>) {
    acc.on_update(w, &json!({"@type": "messages", "@extra": request["@extra"],
        "messages": messages, "total_count": messages.len()}).to_string());
}

fn message(chat: i64, id: i64, date: i64, text: &str) -> Value {
    json!({"@type": "message", "chat_id": chat, "id": id, "date": date,
        "content": {"@type": "messageText", "text": {"text": text}}})
}

fn restore_old(acc: &Account<FakeTd>, w: &World, td: &FakeTd) {
    let create = last_request(td, "createBasicGroupChat");
    acc.on_update(w, &json!({"@type": "chat", "id": OLD, "title": "Old group",
        "type": {"@type": "chatTypeBasicGroup", "basic_group_id": -OLD},
        "@extra": create["@extra"]}).to_string());
}

#[test]
fn an_upgrade_boundary_loads_the_original_group_and_keeps_the_walk_owned_by_its_view() {
    let clock = FakeClock::at(1_000.0);
    let w = timed_world(&clock);
    let td = FakeTd::new();
    let acc = account(td.clone(), None);
    let view = runtime::of(w.store()).watch_messages(NEW, Some(0), vec![], 0.0);
    acc.drain(&w);
    let first = last_request(&td, "getChatHistory");
    assert_eq!(first["chat_id"], NEW);
    let mut boundary = message(NEW, 1, 20, "");
    boundary["content"] = json!({"@type": "messageChatUpgradeFrom", "basic_group_id": -OLD});
    answer(&acc, &w, &first, vec![message(NEW, 42, 21, "new"), boundary]);
    restore_old(&acc, &w, &td);
    clock.advance(PAGE_GAP);
    acc.drain(&w);
    assert_eq!(upgrades::original(w.store(), NEW), Some(OLD));
    let create = last_request(&td, "createBasicGroupChat");
    assert_eq!(create["basic_group_id"], -OLD);
    assert_eq!(create["force"], true);
    let inherited = last_request(&td, "getChatHistory");
    assert_eq!(inherited["chat_id"], OLD);
    assert_eq!(inherited["from_message_id"], 0);
    answer(&acc, &w, &inherited, vec![message(OLD, 42, 10, "old")]);
    let combined = model::history(w.store(), NEW);
    assert_eq!(combined.iter().map(model::Msg::key).collect::<Vec<_>>(), [(OLD, 42), (NEW, 1), (NEW, 42)]);
    assert!(combined[1].service);
    assert_eq!(combined[1].text, "group upgraded");

    let mut cross_reply = message(NEW, 43, 22, "answer to old");
    cross_reply["reply_to"] = json!({"@type": "messageReplyToMessage", "chat_id": OLD, "message_id": 42});
    acc.on_update(&w, &json!({"@type": "updateNewMessage", "message": cross_reply}).to_string());
    let projected = model::line(w.store(), NEW, 43).unwrap();
    assert_eq!(projected.reply_key(), Some((OLD, 42)));
    assert_eq!(projected.reply_text, "old");

    // Both halves retire with the same transcript. A delayed old page cannot
    // put its walk back on the wire after that panel has gone away.
    drop(view);
    acc.drain(&w);
    let sent = td.sent().len();
    answer(&acc, &w, &inherited, vec![message(OLD, 41, 9, "late")]);
    clock.advance(30.0);
    acc.drain(&w);
    assert_eq!(td.sent().len(), sent);
    assert!(!runtime::of(w.store()).loading(OLD));
    assert!(!runtime::of(w.store()).loading(NEW));
    assert!(model::line(w.store(), OLD, 41).is_none());
}

#[test]
fn restored_chats_discover_the_upgrade_from_full_info_even_after_the_new_walk_ends() {
    let clock = FakeClock::at(1_000.0);
    let w = timed_world(&clock);
    let td = FakeTd::new();
    let acc = account(td.clone(), None);
    let _view = runtime::of(w.store()).watch_messages(NEW, Some(0), vec![], 0.0);
    acc.drain(&w);
    let info = last_request(&td, "getSupergroupFullInfo");
    answer(&acc, &w, &last_request(&td, "getChatHistory"), vec![]);
    assert!(!runtime::of(w.store()).loading(NEW));
    acc.on_update(&w, &json!({"@type": "supergroupFullInfo", "@extra": info["@extra"],
        "upgraded_from_basic_group_id": -OLD, "upgraded_from_max_message_id": 42}).to_string());
    clock.advance(PAGE_GAP);
    acc.drain(&w);
    restore_old(&acc, &w, &td);
    acc.drain(&w);
    let old = last_request(&td, "getChatHistory");
    assert_eq!(old["chat_id"], OLD);
    assert!(runtime::of(w.store()).loading(OLD));
    acc.on_update(&w, &json!({"@type": "error", "code": 429, "message": "retry after 3",
        "@extra": old["@extra"]}).to_string());
    clock.advance(5.0);
    acc.drain(&w);
    let retry = last_request(&td, "getChatHistory");
    assert_eq!(retry["chat_id"], OLD);
    assert_eq!(retry["from_message_id"], 0);
    answer(&acc, &w, &retry, vec![]);
    acc.drain(&w);
    assert!(!runtime::of(w.store()).loading(OLD));
    let reads = td.sent_types().iter().filter(|t| *t == "getChatHistory").count();
    clock.advance(10.0);
    acc.drain(&w);
    assert_eq!(td.sent_types().iter().filter(|t| *t == "getChatHistory").count(), reads,
        "an empty old history finishes rather than being restarted each pass");
}

#[test]
fn visible_inherited_rows_refresh_their_source_chat_without_starting_an_independent_walk() {
    let clock = FakeClock::at(1_000.0);
    let w = timed_world(&clock);
    w.caps(|caps| caps.insert(Box::new(runtime::Delivery::Live)));
    let mut main = None;
    let mut inherited = std::collections::BTreeMap::new();
    runtime::show_history_messages(&mut main, &mut inherited, &w, NEW, 0, vec![(OLD, 42), (NEW, 42)]);
    clock.advance(runtime::VIEW_SETTLE);
    let rt = runtime::of(w.store());
    assert_eq!(rt.visible_messages(w.now()), [(OLD, vec![42]), (NEW, vec![42])].into());
    assert_eq!(rt.visible_history(w.now()), [(NEW, 0)].into());
    runtime::show_history_messages(&mut main, &mut inherited, &w, NEW, 0, vec![(NEW, 42)]);
    assert_eq!(rt.visible_messages(w.now()), [(NEW, vec![42])].into());
}

#[test]
fn a_persisted_link_restores_metadata_and_the_chat_before_fetching_old_rows() {
    let clock = FakeClock::at(1_000.0);
    let w = timed_world(&clock);
    w.store().write(|c| upgrades::record(c, OLD, NEW)).unwrap();
    let td = FakeTd::new();
    let acc = account(td.clone(), None);
    let rt = runtime::of(w.store());
    let _view = rt.watch_messages(NEW, Some(0), vec![], 0.0);
    let _rows = rt.watch_messages(OLD, None, vec![42], 0.0);
    acc.drain(&w);
    let info = last_request(&td, "getSupergroupFullInfo");
    assert!(!td.sent_types().contains(&"createBasicGroupChat".to_string()));
    assert!(!td.sent_types().contains(&"getMessages".to_string()));
    answer(&acc, &w, &last_request(&td, "getChatHistory"), vec![]);
    acc.on_update(&w, &json!({"@type": "supergroupFullInfo", "@extra": info["@extra"],
        "upgraded_from_basic_group_id": -OLD}).to_string());
    clock.advance(PAGE_GAP);
    acc.drain(&w);
    assert!(td.sent_types().contains(&"createBasicGroupChat".to_string()));
    assert!(!td.sent_types().contains(&"getMessages".to_string()));
    restore_old(&acc, &w, &td);
    acc.drain(&w);
    assert_eq!(last_request(&td, "getMessages")["chat_id"], OLD);
    assert_eq!(last_request(&td, "getChatHistory")["chat_id"], OLD);
}

#[test]
fn explicit_upgrade_refreshes_wait_for_authorization_and_do_not_repeat_metadata_reads() {
    let clock = FakeClock::at(1_000.0);
    let w = timed_world(&clock);
    let td = FakeTd::new();
    let acc = account(td.clone(), None);
    acc.auth_ready.set(false);
    acc.want(&w, NEW);
    assert!(td.sent().is_empty());
    acc.auth_ready.set(true);
    runtime::of(w.store()).set_list_syncing(true);
    acc.drain(&w);
    assert!(td.sent().is_empty());
    runtime::of(w.store()).set_list_syncing(false);
    acc.drain(&w);
    let info = last_request(&td, "getSupergroupFullInfo");
    answer(&acc, &w, &last_request(&td, "getChatHistory"), vec![]);
    for _ in 0..3 { acc.drain(&w); }
    assert_eq!(td.sent_types().iter().filter(|t| *t == "getSupergroupFullInfo").count(), 1);
    acc.on_update(&w, &json!({"@type": "supergroupFullInfo", "@extra": info["@extra"],
        "upgraded_from_basic_group_id": -OLD}).to_string());
    clock.advance(PAGE_GAP);
    acc.drain(&w);
    restore_old(&acc, &w, &td);
    acc.drain(&w);
    assert_eq!(last_request(&td, "getChatHistory")["chat_id"], OLD);
}
