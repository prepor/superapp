use super::*;
use crate::apps::telegram::{history, operations::Status};
use serde_json::{json, Value};
use std::time::{Duration, Instant};

mod deletion;

fn receive(inbox: &runtime::Inbox) -> Value {
    serde_json::from_str(&inbox.try_recv().expect("queued command")).unwrap()
}

fn ok(acc: &sync::Account<FakeTd>, s: &Session, request: &Value) {
    acc.on_update(s.world(), &json!({"@type": "ok", "@extra": request["@extra"]}).to_string());
    acc.drain(s.world());
}

fn snapshot(acc: &sync::Account<FakeTd>, s: &Session, td: &FakeTd, mut message: Value) {
    let request = last_reaction_request(td, "getMessage");
    assert!(request["@extra"]["context"].as_str().unwrap().starts_with("undo_snapshot:"));
    message["@type"] = json!("message");
    message["chat_id"] = request["chat_id"].clone();
    message["id"] = request["message_id"].clone();
    message["@extra"] = request["@extra"].clone();
    acc.on_update(s.world(), &message.to_string());
}

#[test]
fn send_undo_uses_the_final_id_even_when_delivery_precedes_the_response() {
    for early in [false, true] {
        let mut s = session();
        let slot = open_root(&mut s, Chat::id(VERA));
        let rt = runtime::of(s.store());
        let inbox = rt.connect();
        with_chat(&s, slot, |c| c.set_draft("a real send"));
        send(&mut s, slot);
        let request = receive(&inbox);
        assert_eq!(s.history().rows().0.last().unwrap().kind, "send");
        let pending = json!({"@type": "message", "chat_id": VERA, "id": 100,
            "sending_state": {"@type": "messageSendingStatePending"}, "@extra": request["@extra"]});
        let delivered = json!({"@type": "updateMessageSendSucceeded", "old_message_id": 100,
            "message": {"chat_id": VERA, "id": 900}});
        // The user can undo before either answer, without guessing an id.
        assert!(s.undo());
        assert!(inbox.try_recv().is_err());
        if early { rt.operations.sent(s.store(), &delivered); }
        rt.operations.reply(s.store(), &pending);
        if !early { rt.operations.sent(s.store(), &delivered); }
        rt.operations.expire(s.store(), Instant::now() + Duration::from_secs(6));
        history::pump(s.store());
        let delete = receive(&inbox);
        assert_eq!(delete["@type"], "deleteMessages");
        assert_eq!(delete["chat_id"], VERA);
        assert_eq!(delete["message_ids"], json!([900]));
        assert_eq!(delete["revoke"], true);

        // A quick redo must wait until deletion is confirmed, then obtain
        // a fresh send id. It must leave the user's newer draft intact.
        with_chat(&s, slot, |c| c.set_draft("a newer draft"));
        assert!(s.redo());
        assert!(inbox.try_recv().is_err());
        rt.operations.reply(s.store(), &json!({"@type": "ok", "@extra": delete["@extra"]}));
        history::pump(s.store());
        let redo = receive(&inbox);
        assert_eq!(redo["input_message_content"]["text"]["text"], "a real send");
        assert_eq!(redo["input_message_content"]["clear_draft"], false);
        rt.operations.reply(s.store(), &json!({"@type": "message", "chat_id": VERA,
            "id": 901, "@extra": redo["@extra"]}));
        history::pump(s.store());
        assert!(s.undo());
        assert_eq!(receive(&inbox)["message_ids"], json!([901]));
        assert_eq!(with_chat(&s, slot, |c| c.field_text().to_string()), "a newer draft");
    }
}

#[test]
fn undo_then_redo_before_delivery_does_not_resend_or_delete() {
    let mut s = session();
    let rt = runtime::of(s.store());
    let inbox = rt.connect();
    history::command(&mut s, &requests::send_message(VERA, "once", Some(42))).unwrap();
    let request = receive(&inbox);
    assert!(s.undo());
    assert!(s.redo());
    rt.operations.reply(s.store(), &json!({"@type": "message", "chat_id": VERA,
        "id": 900, "@extra": request["@extra"]}));
    history::pump(s.store());
    assert!(inbox.try_recv().is_err());
}

#[test]
fn uncertain_send_and_failed_reversal_never_cause_an_automatic_resend() {
    for fail_send in [true, false] {
        let mut s = session();
        let rt = runtime::of(s.store());
        let inbox = rt.connect();
        let operation = history::command(&mut s, &requests::send_message(VERA, "once", None)).unwrap();
        let request = receive(&inbox);
        if fail_send {
            rt.operations.fail(s.store(), operation, "delivery unknown", true);
            s.undo();
        } else {
            rt.operations.reply(s.store(), &json!({"@type": "message", "chat_id": VERA,
                "id": 900, "@extra": request["@extra"]}));
            assert!(s.undo());
            let undo = receive(&inbox);
            rt.operations.reply(s.store(), &json!({"@type": "error", "code": 400,
                "message": "MESSAGE_DELETE_FORBIDDEN", "@extra": undo["@extra"]}));
        }
        history::pump(s.store());
        s.redo();
        history::pump(s.store());
        assert!(inbox.try_recv().is_err());
        assert_eq!(s.history().rows().0.last().unwrap().state, "expired");
        assert!(rt.operations.list().iter().any(|op| matches!(op.status, Status::Failed { .. })));
    }
}

#[test]
fn edits_restore_complete_server_entities_and_captions_without_rewriting_the_projection() {
    for caption in [false, true] {
        let mut s = session();
        let td = FakeTd::new();
        let acc = connected_reaction_account(&s, td.clone());
        acc.drain(s.world());
        let m = model::history(s.store(), VERA).iter().find(|m| m.out).unwrap().clone();
        let before = json!({"@type": "formattedText", "text": "old 🦀 linked words",
            "entities": [{"@type": "textEntity", "offset": 0, "length": 3,
                "type": {"@type": "textEntityTypeBold"}},
                {"@type": "textEntity", "offset": 7, "length": 12,
                 "type": {"@type": "textEntityTypeTextUrl", "url": "https://example.org/"}}]});
        let request = if caption { requests::edit_message_caption(m.chat, m.id, "new caption") }
            else { requests::edit_message_text(m.chat, m.id, "new text") };
        history::command(&mut s, &request).unwrap();
        acc.drain(s.world());
        let field = if caption { "caption" } else { "text" };
        let kind = if caption { "editMessageCaption" } else { "editMessageText" };
        assert!(!td.sent_types().iter().any(|t| t == kind));
        snapshot(&acc, &s, &td, json!({"content": {field: before}}));
        let edit = last_reaction_request(&td, kind);
        assert!(s.undo());
        assert_eq!(td.sent_types().iter().filter(|t| *t == kind).count(), 1);
        ok(&acc, &s, &edit);
        acc.drain(s.world());
        let undo = last_reaction_request(&td, kind);
        let text = if caption { &undo["caption"] } else { &undo["input_message_content"]["text"] };
        assert_eq!(text, &before);
        assert_eq!(model::line(s.store(), m.chat, m.id).unwrap(), m,
            "only Telegram's updates may change the transcript");
        ok(&acc, &s, &undo);
        assert!(s.redo());
        acc.drain(s.world());
        let redo = last_reaction_request(&td, kind);
        assert_eq!(redo[field], edit[field]);
        assert_eq!(redo["input_message_content"], edit["input_message_content"]);
    }
}

#[test]
fn reaction_undo_removes_our_add_and_restores_an_evicted_custom_reaction() {
    let mut s = session();
    let td = FakeTd::new();
    let acc = connected_reaction_account(&s, td.clone());
    acc.drain(s.world());
    history::command(&mut s, &requests::add_message_reaction(VERA, 42, "👍", 1)).unwrap();
    acc.drain(s.world());
    let custom = json!({"@type": "reactionTypeCustomEmoji", "custom_emoji_id": "123456789012345678"});
    snapshot(&acc, &s, &td, json!({"interaction_info": {"reactions": {"reactions": [
        {"type": custom, "is_chosen": true, "total_count": 1},
        {"type": {"@type": "reactionTypeEmoji", "emoji": "👍"}, "is_chosen": false, "total_count": 4}
    ]}}}));
    let add = last_reaction_request(&td, "addMessageReaction");
    ok(&acc, &s, &add);
    assert!(s.undo());
    acc.drain(s.world());
    let remove = last_reaction_request(&td, "removeMessageReaction");
    assert_eq!(remove["reaction_type"], add["reaction_type"]);
    assert_eq!(td.sent_types().iter().filter(|t| *t == "addMessageReaction").count(), 1);
    ok(&acc, &s, &remove);
    acc.drain(s.world());
    let restore = last_reaction_request(&td, "addMessageReaction");
    assert_eq!(restore["reaction_type"], custom);
    ok(&acc, &s, &restore);
    assert!(s.redo());
    acc.drain(s.world());
    assert_eq!(last_reaction_request(&td, "addMessageReaction")["reaction_type"], add["reaction_type"]);
}

#[test]
fn adding_an_existing_reaction_never_removes_it_on_undo() {
    let mut s = session();
    let td = FakeTd::new();
    let acc = connected_reaction_account(&s, td.clone());
    acc.drain(s.world());
    history::command(&mut s, &requests::add_message_reaction(VERA, 42, "👍", 1)).unwrap();
    acc.drain(s.world());
    snapshot(&acc, &s, &td, json!({"interaction_info": {"reactions": {"reactions": [
        {"type": {"@type": "reactionTypeEmoji", "emoji": "👍"}, "is_chosen": true}
    ]}}}));
    ok(&acc, &s, &last_reaction_request(&td, "addMessageReaction"));
    assert!(s.undo());
    acc.drain(s.world());
    assert!(!td.sent_types().contains(&"removeMessageReaction".to_string()));
}

#[test]
fn incoming_deletes_are_recorded_and_skipped_without_recreating_local_messages() {
    let mut s = session();
    let chat = open_root(&mut s, Chat::id(VERA));
    let m = model::history(s.store(), VERA).iter().find(|m| !m.out && !m.service).unwrap().clone();
    let rt = runtime::of(s.store());
    let inbox = rt.connect();
    with_chat(&s, chat, |c| c.set_cursor(m.id));
    history::command(&mut s, &requests::delete_messages(VERA, &[m.id], true)).unwrap();
    assert_eq!(receive(&inbox)["@type"], "deleteMessages");
    let row = s.history().rows().0.last().unwrap().clone();
    assert_eq!(row.kind, "delete");
    assert!(row.label.contains("cannot undo"));
    assert!(s.undo(), "undo walks past the deletion to the earlier open");
    assert!(inbox.try_recv().is_err());
    assert_eq!(s.history().rows().0.last().unwrap().state, "expired");
    assert_eq!(model::line(s.store(), m.chat, m.id).unwrap(), m);
}

#[test]
fn offline_reaction_undo_restores_counts_and_allows_choosing_it_again() {
    let mut s = session();
    let m = model::history(s.store(), VERA).iter().find(|m| !m.service).unwrap().clone();
    let before = m.reactions.clone();
    assert!(super::super::verbs::react_demo(&mut s, m.chat, m.id, "👍"));
    let after = model::line(s.store(), m.chat, m.id).unwrap().reactions;
    assert_ne!(before, after);
    assert!(s.undo());
    assert_eq!(model::line(s.store(), m.chat, m.id).unwrap().reactions, before);
    assert!(s.redo());
    assert_eq!(model::line(s.store(), m.chat, m.id).unwrap().reactions, after);
    assert!(s.undo());
    assert!(super::super::verbs::react_demo(&mut s, m.chat, m.id, "👍"));
    assert_eq!(model::line(s.store(), m.chat, m.id).unwrap().reactions, after);
}

#[test]
fn undoing_successive_edits_waits_for_each_remote_reversal() {
    let mut s = session();
    let td = FakeTd::new();
    let acc = connected_reaction_account(&s, td.clone());
    acc.drain(s.world());
    for (before, after) in [("first", "second"), ("second", "third")] {
        history::command(&mut s, &requests::edit_message_text(VERA, 42, after)).unwrap();
        acc.drain(s.world());
        snapshot(&acc, &s, &td, json!({"content": {"text": {"text": before, "entities": []}}}));
        ok(&acc, &s, &last_reaction_request(&td, "editMessageText"));
    }
    assert!(s.undo());
    assert!(s.undo());
    acc.drain(s.world());
    let undo = last_reaction_request(&td, "editMessageText");
    assert_eq!(undo["input_message_content"]["text"]["text"], "second");
    assert_eq!(td.sent_types().iter().filter(|t| *t == "editMessageText").count(), 3);
    ok(&acc, &s, &undo);
    acc.drain(s.world());
    assert_eq!(last_reaction_request(&td, "editMessageText")["input_message_content"]["text"]["text"], "first");
}

#[test]
fn redoing_a_send_then_its_edit_follows_the_new_message_id() {
    let mut s = session();
    let td = FakeTd::new();
    let acc = connected_reaction_account(&s, td.clone());
    acc.drain(s.world());
    history::command(&mut s, &requests::send_message(VERA, "first", None)).unwrap();
    acc.drain(s.world());
    let sent = last_reaction_request(&td, "sendMessage");
    acc.on_update(s.world(), &json!({"@type": "message", "chat_id": VERA, "id": 900,
        "@extra": sent["@extra"], "content": {"@type": "messageText", "text": {"text": "first"}}}).to_string());
    history::command(&mut s, &requests::edit_message_text(VERA, 900, "second")).unwrap();
    acc.drain(s.world());
    snapshot(&acc, &s, &td, json!({"content": {"text": {"text": "first", "entities": []}}}));
    ok(&acc, &s, &last_reaction_request(&td, "editMessageText"));
    assert!(s.undo());
    assert!(s.undo());
    acc.drain(s.world());
    assert!(!td.sent_types().contains(&"deleteMessages".to_string()));
    ok(&acc, &s, &last_reaction_request(&td, "editMessageText"));
    acc.drain(s.world());
    ok(&acc, &s, &last_reaction_request(&td, "deleteMessages"));
    assert!(s.redo());
    assert!(s.redo());
    acc.drain(s.world());
    assert_eq!(td.sent_types().iter().filter(|t| *t == "editMessageText").count(), 2);
    let sent = last_reaction_request(&td, "sendMessage");
    acc.on_update(s.world(), &json!({"@type": "message", "chat_id": VERA, "id": 901,
        "@extra": sent["@extra"], "content": {"@type": "messageText", "text": {"text": "first"}}}).to_string());
    acc.drain(s.world());
    acc.drain(s.world());
    let edit = last_reaction_request(&td, "editMessageText");
    assert_eq!(edit["message_id"], 901);
    assert_eq!(edit["input_message_content"]["text"]["text"], "second");
}

#[test]
fn forwarding_keeps_message_order_when_delivery_updates_arrive_backwards() {
    let mut s = session();
    let rt = runtime::of(s.store());
    let inbox = rt.connect();
    let id = history::command(&mut s, &requests::forward_messages(VERA, STELAXIS, &[1, 2])).unwrap();
    let request = receive(&inbox);
    rt.operations.reply(s.store(), &json!({"@type": "messages", "@extra": request["@extra"], "messages": [
        {"chat_id": VERA, "id": 100, "sending_state": {"@type": "messageSendingStatePending"}},
        {"chat_id": VERA, "id": 101, "sending_state": {"@type": "messageSendingStatePending"}}
    ]}));
    for (old, new) in [(101, 901), (100, 900)] {
        rt.operations.sent(s.store(), &json!({"@type": "updateMessageSendSucceeded", "old_message_id": old,
            "message": {"chat_id": VERA, "id": new}}));
    }
    assert_eq!(rt.operations.watch(id).unwrap().lock().unwrap().messages, vec![(VERA, 900), (VERA, 901)]);
    assert!(s.undo());
    assert_eq!(receive(&inbox)["message_ids"], json!([900, 901]));
}

#[test]
fn chat_preferences_restore_complete_settings_and_previous_list() {
    for request in [requests::set_chat_muted(VERA, true), requests::toggle_chat_pinned(VERA, true),
        requests::add_chat_to_list(VERA, true)] {
        let mut s = session();
        let td = FakeTd::new();
        let acc = connected_reaction_account(&s, td.clone());
        acc.drain(s.world());
        history::command(&mut s, &request).unwrap();
        acc.drain(s.world());
        let get = last_reaction_request(&td, "getChat");
        let settings = json!({"@type": "chatNotificationSettings", "use_default_mute_for": true,
            "mute_for": 45, "sound_id": "123456789012345678", "use_default_sound": false});
        acc.on_update(s.world(), &json!({"@type": "chat", "id": VERA, "@extra": get["@extra"],
            "notification_settings": settings,
            "positions": [{"list": {"@type": "chatListMain"}, "order": "123", "is_pinned": false}]
        }).to_string());
        let original: Value = serde_json::from_str(&request).unwrap();
        let kind = original["@type"].as_str().unwrap();
        ok(&acc, &s, &last_reaction_request(&td, kind));
        assert!(s.undo());
        acc.drain(s.world());
        let undo = last_reaction_request(&td, kind);
        match kind {
            "setChatNotificationSettings" => assert_eq!(undo["notification_settings"], settings),
            "toggleChatIsPinned" => assert_eq!(undo["is_pinned"], false),
            _ => assert_eq!(undo["chat_list"]["@type"], "chatListMain"),
        }
    }
}

#[test]
fn missing_reaction_ownership_is_not_assumed_to_mean_no_reaction() {
    let mut s = session();
    let td = FakeTd::new();
    let acc = connected_reaction_account(&s, td.clone());
    acc.drain(s.world());
    history::command(&mut s, &requests::add_message_reaction(VERA, 42, "👍", 1)).unwrap();
    acc.drain(s.world());
    snapshot(&acc, &s, &td, json!({"interaction_info": null}));
    ok(&acc, &s, &last_reaction_request(&td, "addMessageReaction"));
    s.undo();
    acc.drain(s.world());
    assert_eq!(s.history().rows().0.last().unwrap().state, "expired");
    assert!(!td.sent_types().contains(&"removeMessageReaction".to_string()));
}

#[test]
fn replaying_a_reply_waits_for_its_parent_messages_new_id() {
    let mut s = session();
    let rt = runtime::of(s.store());
    let inbox = rt.connect();
    for (text, reply, id) in [("parent", None, 900), ("reply", Some(900), 800)] {
        history::command(&mut s, &requests::send_message(VERA, text, reply)).unwrap();
        let request = receive(&inbox);
        rt.operations.reply(s.store(), &json!({"@type": "message", "chat_id": VERA,
            "id": id, "@extra": request["@extra"]}));
        history::pump(s.store());
    }
    assert!(s.undo());
    assert!(s.undo());
    for expected in [800, 900] {
        let delete = receive(&inbox);
        assert_eq!(delete["message_ids"], json!([expected]));
        rt.operations.reply(s.store(), &json!({"@type": "ok", "@extra": delete["@extra"]}));
        history::pump(s.store());
    }
    assert!(s.redo());
    assert!(s.redo());
    let parent = receive(&inbox);
    assert!(inbox.try_recv().is_err(), "the reply depends on the new parent identity");
    rt.operations.reply(s.store(), &json!({"@type": "message", "chat_id": VERA,
        "id": 901, "@extra": parent["@extra"]}));
    history::pump(s.store());
    assert_eq!(receive(&inbox)["reply_to"]["message_id"], 901);
}

#[test]
fn a_reply_for_another_chat_cannot_make_undo_delete_in_that_chat() {
    let mut s = session();
    let rt = runtime::of(s.store());
    let inbox = rt.connect();
    history::command(&mut s, &requests::send_message(VERA, "hello", None)).unwrap();
    let request = receive(&inbox);
    rt.operations.reply(s.store(), &json!({"@type": "message", "chat_id": STELAXIS,
        "id": 900, "@extra": request["@extra"]}));
    s.undo();
    history::pump(s.store());
    assert!(inbox.try_recv().is_err());
    assert_eq!(s.history().rows().0.last().unwrap().state, "expired");
}

#[test]
fn a_refused_send_keeps_the_composer_and_does_not_record_a_phantom_action() {
    let mut s = session();
    let chat = open_root(&mut s, Chat::id(VERA));
    drop(runtime::of(s.store()).connect());
    with_chat(&s, chat, |c| c.set_draft("keep this"));
    let before = s.history().rows();
    send(&mut s, chat);
    assert_eq!(s.history().rows(), before);
    assert_eq!(with_chat(&s, chat, |c| c.field_text().to_string()), "keep this");
}
