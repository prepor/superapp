use super::*;
use crate::apps::telegram::{seed::BERLIN, topics};
use serde_json::{json, Value};
use std::time::{Duration, Instant};

fn call(s: &mut Session, name: &str, input: Value) -> Result<Value, String> {
    let tool = s.apps().tool(name).expect("registered tool").clone();
    tool.check(&input)?;
    let result = (tool.run)(s, &input);
    s.settle();
    result
}

fn draft(s: &mut Session, input: Value) -> Value {
    call(s, "telegram.draft", input).expect("draft staged")
}

fn slot(draft: &Value) -> SlotId {
    draft["slot"].as_u64().unwrap()
}

#[test]
fn telegram_explains_its_data_and_registers_draft_send_and_status() {
    let s = session();
    let describe = TELEGRAM.describe().unwrap();
    assert!(describe.contains("telegram-chat") && describe.contains("sql.write"));
    for (name, writes, asks) in [
        ("telegram.draft", true, false),
        ("telegram.send", true, true),
        ("telegram.status", false, false),
    ] {
        let tool = s.apps().tool(name).unwrap();
        assert_eq!((tool.writes, tool.asks), (writes, asks));
    }
    let send = s.apps().tool("telegram.send").unwrap();
    assert!(
        send.check(&json!({"slot": 1})).is_err(),
        "approval names the contents too"
    );
    assert!(send
        .check(&json!({"slot": 1, "chat": VERA, "text": "hi", "topic": "2"}))
        .is_err());
}

#[test]
fn an_agent_draft_opens_the_correct_composer_and_uses_its_send_path() {
    let mut s = session();
    open_root(&mut s, Chats::id());
    let inbox = runtime::of(s.store()).connect();
    let before = model::history(s.store(), VERA).len();
    let d = draft(
        &mut s,
        json!({"chat": VERA, "text": "Привет!\nSee you at seven."}),
    );
    assert_eq!(s.focus(), Some(slot(&d)));
    assert_eq!(field_now(&s, slot(&d)), d["text"].as_str().unwrap());
    assert_eq!(draft_row(&s, VERA), d["text"].as_str().unwrap());
    assert!(inbox.try_recv().is_err(), "drafting sends no message");

    let result = call(&mut s, "telegram.send", d.clone()).unwrap();
    assert_eq!(result["status"], "queued");
    let request: Value = serde_json::from_str(&inbox.try_recv().unwrap()).unwrap();
    assert_eq!(request["@type"], "sendMessage");
    assert_eq!(request["chat_id"], VERA);
    assert_eq!(request["input_message_content"]["text"]["text"], d["text"]);
    assert_eq!(request["input_message_content"]["clear_draft"], true);
    assert_eq!(request["@extra"]["operation"], result["operation"]);
    assert_eq!(field_now(&s, slot(&d)), "");
    assert_eq!(draft_row(&s, VERA), "");
    assert_eq!(
        model::history(s.store(), VERA).len(),
        before,
        "only TDLib may project the sent line"
    );
    let state = call(
        &mut s,
        "telegram.status",
        json!({"operation": result["operation"]}),
    )
    .unwrap();
    assert_eq!(state["status"], "pending", "queued does not mean delivered");
    runtime::of(s.store()).operations.reply(
        s.store(),
        &json!({
            "@type": "message", "chat_id": VERA, "id": 9999,
            "@extra": request["@extra"], "sending_state": null
        }),
    );
    let state = call(
        &mut s,
        "telegram.status",
        json!({"operation": result["operation"]}),
    )
    .unwrap();
    assert_eq!(
        state["status"], "done",
        "delivery requires Telegram's acknowledgement"
    );
    assert!(
        call(&mut s, "telegram.send", d).is_err(),
        "the same draft cannot send twice"
    );
    assert!(inbox.try_recv().is_err());
}

#[test]
fn delivered_send_status_survives_feedback_expiry_for_the_session() {
    for confirmed_by_update in [false, true] {
        let mut s = session();
        open_root(&mut s, Chats::id());
        let rt = runtime::of(s.store());
        let inbox = rt.connect();
        let d = draft(&mut s, json!({"chat": VERA, "text": "hello"}));
        let queued = call(&mut s, "telegram.send", d).unwrap();
        let id = queued["operation"].as_u64().unwrap();
        let request: Value = serde_json::from_str(&inbox.try_recv().unwrap()).unwrap();
        rt.operations.reply(
            s.store(),
            &json!({
                "@type": "message", "chat_id": VERA, "id": 100,
                "@extra": request["@extra"],
                "sending_state": confirmed_by_update.then(|| json!({
                    "@type": "messageSendingStatePending"
                }))
            }),
        );
        if confirmed_by_update {
            rt.operations.sent(
                s.store(),
                &json!({
                    "@type": "updateMessageSendSucceeded", "old_message_id": 100,
                    "message": {"chat_id": VERA, "id": 200}
                }),
            );
        }

        for elapsed in [6, 300, 3600] {
            rt.operations
                .expire(s.store(), Instant::now() + Duration::from_secs(elapsed));
            assert!(
                rt.operations.list().iter().all(|op| op.id != id),
                "completed feedback still disappears"
            );
            assert_eq!(
                call(&mut s, "telegram.status", json!({"operation": id})).unwrap(),
                json!({
                    "operation": id, "chat": VERA, "status": "done",
                    "error": null, "uncertain": false, "retryable": false
                })
            );
        }
        assert!(call(&mut s, "telegram.status", json!({"operation": u64::MAX})).is_err());
        assert!(inbox.try_recv().is_err(), "checking status never resends");
    }
}

#[test]
fn dismissing_send_feedback_keeps_status_but_releases_retry() {
    for uncertain in [None, Some(false), Some(true)] {
        let mut s = session();
        open_root(&mut s, Chats::id());
        let rt = runtime::of(s.store());
        let inbox = rt.connect();
        let d = draft(&mut s, json!({"chat": VERA, "text": "hello"}));
        let queued = call(&mut s, "telegram.send", d).unwrap();
        let id = queued["operation"].as_u64().unwrap();
        let request: Value = serde_json::from_str(&inbox.try_recv().unwrap()).unwrap();
        if let Some(uncertain) = uncertain {
            rt.operations.fail(s.store(), id, "send failed", uncertain);
        } else {
            rt.operations.reply(
                s.store(),
                &json!({
                    "@type": "message", "chat_id": VERA, "id": 9999,
                    "@extra": request["@extra"], "sending_state": null
                }),
            );
        }
        let mut expected = call(&mut s, "telegram.status", json!({"operation": id})).unwrap();
        assert_eq!(expected["retryable"], uncertain == Some(false));
        expected["retryable"] = json!(false);

        rt.operations.dismiss(id);
        assert!(rt.operations.list().iter().all(|op| op.id != id));
        assert_eq!(
            call(&mut s, "telegram.status", json!({"operation": id})).unwrap(),
            expected
        );
        assert!(rt.operations.retry(id).is_none());
        assert!(inbox.try_recv().is_err());
    }
}

#[test]
fn drafting_reuses_message_anchored_panels_and_updates_every_open_copy() {
    let mut s = session();
    let first = open_root(&mut s, Chat::id(VERA));
    let message = model::history(s.store(), VERA).last().unwrap().id;
    let second = open_root(&mut s, Chat::at(VERA, message));
    with_chat(&s, first, |c| c.typed("local text"));
    with_chat(&s, second, |c| c.typed("another local text"));
    let inbox = runtime::of(s.store()).connect();
    let count = s.panels().len();
    let text = "a long agent draft\n".repeat(40);
    let d = draft(&mut s, json!({"chat": VERA, "text": text, "replace": true}));
    assert_eq!(s.panels().len(), count);
    for open in [first, second] {
        assert_eq!(field_now(&s, open), text);
        assert_eq!(with_chat(&s, open, Chat::take_field_wish), open == slot(&d));
    }
    with_chat(&s, slot(&d), Chat::flush_draft);
    let request: Value = serde_json::from_str(&inbox.try_recv().unwrap()).unwrap();
    assert_eq!(request["@type"], "setChatDraftMessage");
    assert_eq!(
        request["draft_message"]["input_message_text"]["text"]["text"],
        text.trim()
    );
    call(&mut s, "telegram.send", d).unwrap();
    for open in [first, second] {
        assert_eq!(field_now(&s, open), "");
    }
}

#[test]
fn drafts_keep_existing_work_unless_replacement_is_explicit() {
    let mut s = session();
    let chat = open_root(&mut s, Chat::id(VERA));
    with_chat(&s, chat, |c| c.typed("my unsent words"));
    let input = json!({"chat": VERA, "text": "agent words"});
    assert!(call(&mut s, "telegram.draft", input.clone()).is_err());
    assert_eq!(field_now(&s, chat), "my unsent words");
    // Refusing replacement must also preserve a keystroke's pending save.
    with_chat(&s, chat, Chat::save_pending_draft);
    assert_eq!(draft_row(&s, VERA), "my unsent words");

    // A remote row cannot conceal a different local draft from the tool.
    write_draft(&s, VERA, "agent words");
    assert!(call(&mut s, "telegram.draft", input).is_err());
    assert_eq!(field_now(&s, chat), "my unsent words");
    let d = draft(
        &mut s,
        json!({"chat": VERA, "text": "agent words", "replace": true}),
    );
    assert_eq!(slot(&d), chat);
    assert_eq!(field_now(&s, chat), "agent words");

    // A persisted draft in a closed chat is protected too.
    assert!(call(
        &mut s,
        "telegram.draft",
        json!({"chat": HIKE, "text": "overwrite"})
    )
    .is_err());
    assert_eq!(draft_row(&s, HIKE), "I'll bring the thermos and");
}

#[test]
fn drafting_never_discards_an_edit_or_attachments() {
    let mut s = session();
    let chat = open_root(&mut s, Chat::id(VERA));
    let own = model::history(s.store(), VERA)
        .iter()
        .find(|m| m.out && !m.service)
        .unwrap()
        .id;
    let input = json!({"chat": VERA, "text": "new text", "replace": true});
    with_chat(&s, chat, |c| {
        assert!(c.edit(own));
        c.typed("edited text");
    });
    assert!(call(&mut s, "telegram.draft", input.clone()).is_err());
    assert_eq!(field_now(&s, chat), "edited text");
    with_chat(&s, chat, |c| {
        c.cancel_edit();
        c.carry(&["/tmp/keep.pdf".into()]);
    });
    assert!(call(&mut s, "telegram.draft", input).is_err());
    assert_eq!(
        with_chat(&s, chat, |c| c.carrying()[0].path.clone()),
        "/tmp/keep.pdf"
    );
}

#[test]
fn topic_drafts_and_replies_stay_in_their_destination() {
    let mut s = session();
    open_root(&mut s, Chats::id());
    let inbox = runtime::of(s.store()).connect();
    let input = json!({"chat": BERLIN, "topic": 2, "text": "Saturday works", "reply_to": 2000});
    let d = draft(&mut s, input);
    assert_eq!(
        s.panel(slot(&d)).unwrap().borrow().id(),
        &Chat::topic(BERLIN, 2)
    );
    assert_eq!(
        topics::get(s.store(), BERLIN, 2).unwrap().draft.as_deref(),
        Some("Saturday works")
    );
    assert_eq!(draft_row(&s, BERLIN), "");
    call(&mut s, "telegram.send", d).unwrap();
    // A native build also queues the topic's read receipt when it opens.
    let request = inbox
        .try_iter()
        .map(|r| serde_json::from_str::<Value>(&r).unwrap())
        .find(|r| r["@type"] == "sendMessage")
        .unwrap();
    assert_eq!(request["chat_id"], BERLIN);
    assert_eq!(request["topic_id"]["forum_topic_id"], 2);
    assert_eq!(request["reply_to"]["message_id"], 2000);
    assert!(topics::get(s.store(), BERLIN, 2).unwrap().draft.is_none());
}

#[test]
fn invalid_destinations_and_reply_targets_make_no_draft() {
    let mut s = session();
    open_root(&mut s, Chats::id());
    let panels = s.panels().len();
    for input in [
        json!({"chat": 9999999, "text": "hi"}),
        json!({"chat": VERA, "topic": 99999, "text": "hi"}),
        json!({"chat": VERA, "topic": -1, "text": "hi"}),
        json!({"chat": VERA, "text": " \n "}),
        json!({"chat": VERA, "text": "hi", "reply_to": 2000}),
        json!({"chat": BERLIN, "topic": 4, "text": "hi", "reply_to": 2000}),
        json!({"chat": RUST_WEEKLY, "text": "hi"}),
    ] {
        assert!(
            call(&mut s, "telegram.draft", input.clone()).is_err(),
            "{input}"
        );
    }
    assert_eq!(s.panels().len(), panels);
    assert_eq!(draft_row(&s, VERA), "");
}

#[test]
fn a_send_rechecks_contents_recipient_topic_reply_and_composer_mode() {
    for change in [
        "text", "chat", "topic", "reply", "files", "edit", "closed", "blocked",
    ] {
        let mut s = session();
        open_root(&mut s, Chats::id());
        let inbox = runtime::of(s.store()).connect();
        let mut d = draft(&mut s, json!({"chat": VERA, "text": "reviewed words"}));
        let chat = slot(&d);
        let message = model::history(s.store(), VERA)
            .iter()
            .find(|m| m.out && !m.service)
            .unwrap()
            .id;
        match change {
            "text" => with_chat(&s, chat, |c| c.typed("newer words")),
            "chat" => d["chat"] = json!(ELENA),
            "topic" => d["topic"] = json!(99999),
            "reply" => with_chat(&s, chat, |c| {
                c.reply(message);
            }),
            "files" => with_chat(&s, chat, |c| {
                c.carry(&["/tmp/keep.pdf".into()]);
            }),
            "edit" => with_chat(&s, chat, |c| {
                c.edit(message);
            }),
            "closed" => go(
                &mut s,
                Nav::Close {
                    slot: chat,
                    label: None,
                },
            ),
            "blocked" => s
                .store()
                .write(|c| model::set_blocked_tx(c, VERA, true))
                .unwrap(),
            _ => unreachable!(),
        }
        assert!(call(&mut s, "telegram.send", d).is_err(), "{change}");
        assert!(
            inbox
                .try_iter()
                .all(|r| serde_json::from_str::<Value>(&r).unwrap()["@type"] != "sendMessage"),
            "{change}"
        );
        assert!(!draft_row(&s, VERA).is_empty(), "{change}");
    }
}

#[test]
fn offline_or_disconnected_sends_preserve_the_draft_and_report_failure() {
    for disconnected in [false, true] {
        let mut s = session();
        open_root(&mut s, Chats::id());
        let rt = runtime::of(s.store());
        if disconnected {
            drop(rt.connect());
        }
        let d = draft(&mut s, json!({"chat": VERA, "text": "keep me"}));
        assert!(call(&mut s, "telegram.send", d.clone()).is_err());
        assert_eq!(draft_row(&s, VERA), "keep me");
        assert_eq!(field_now(&s, slot(&d)), "keep me");
    }
}

#[test]
fn an_imported_sql_draft_can_still_be_sent_from_the_panel() {
    let mut s = session();
    let chat = open_root(&mut s, Chat::id(VERA));
    let inbox = runtime::of(s.store()).connect();
    let text = "imported draft\n".repeat(40);
    call(
        &mut s,
        "sql.write",
        json!({"sql": "UPDATE tg_chat SET draft = ? WHERE peer = ?", "params": [text, VERA]}),
    )
    .unwrap();
    // Send must reconcile the row even before the next draw.
    send(&mut s, chat);
    let request: Value = serde_json::from_str(&inbox.try_recv().unwrap()).unwrap();
    assert_eq!(
        request["input_message_content"]["text"]["text"],
        text.trim()
    );
    assert_eq!(field_now(&s, chat), "");
}

#[test]
fn a_remote_draft_cannot_replace_the_reviewed_text_at_the_queue_boundary() {
    let mut s = session();
    open_root(&mut s, Chats::id());
    let inbox = runtime::of(s.store()).connect();
    let d = draft(&mut s, json!({"chat": VERA, "text": "reviewed text"}));
    with_chat(&s, slot(&d), Chat::flush_draft);
    inbox.try_recv().unwrap();
    // The next row update arrives after the caller has checked its snapshot.
    write_draft(&s, VERA, "different words from another device");
    let panel = s.panel(slot(&d)).unwrap();
    let operation = panel
        .borrow_mut()
        .as_any()
        .downcast_mut::<Chat>()
        .unwrap()
        .send_text_draft(&mut s)
        .unwrap();
    let request: Value = serde_json::from_str(&inbox.try_recv().unwrap()).unwrap();
    assert_eq!(request["@extra"]["operation"], operation);
    assert_eq!(
        request["input_message_content"]["text"]["text"],
        "reviewed text"
    );
}

#[test]
fn send_status_reports_failed_and_uncertain_delivery_without_retrying() {
    let mut s = session();
    open_root(&mut s, Chats::id());
    let rt = runtime::of(s.store());
    let inbox = rt.connect();
    let d = draft(&mut s, json!({"chat": VERA, "text": "hello"}));
    let queued = call(&mut s, "telegram.send", d).unwrap();
    inbox.try_recv().unwrap();
    let id = queued["operation"].as_u64().unwrap();
    rt.operations
        .fail(s.store(), id, "no acknowledgement", true);
    let result = call(&mut s, "telegram.status", json!({"operation": id})).unwrap();
    assert_eq!(result["status"], "failed");
    assert_eq!(result["uncertain"], true);
    assert_eq!(result["retryable"], false);
    assert_eq!(result["error"], "no acknowledgement");
    assert!(inbox.try_recv().is_err());
}
