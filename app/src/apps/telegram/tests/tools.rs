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

struct Attachments(std::path::PathBuf);

impl Attachments {
    fn new() -> Self {
        use std::sync::atomic::{AtomicU64, Ordering};
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let dir = std::env::temp_dir().join(format!(
            "superapp-telegram-agent-files-{}-{}", std::process::id(), NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir(&dir).unwrap();
        Self(dir)
    }

    fn file(&self, name: &str, bytes: &[u8]) -> String {
        let path = self.0.join(name);
        std::fs::write(&path, bytes).unwrap();
        path.to_str().unwrap().to_string()
    }
}

impl Drop for Attachments {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

const PHOTO: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/resources/telegram/palette.png");

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
fn agents_send_photos_and_documents_with_a_caption_and_individual_delivery_status() {
    let attachments = Attachments::new();
    let document = attachments.file("report.txt", b"the report");
    let mut s = session();
    open_root(&mut s, Chats::id());
    let inbox = runtime::of(s.store()).connect();
    let d = draft(&mut s, json!({
        "chat": BERLIN, "topic": 2, "reply_to": 2000, "text": "The picture and report",
        "files": [PHOTO, document]
    }));
    assert_eq!(d["files"], json!([PHOTO, document]));
    assert_eq!(with_chat(&s, slot(&d), |c| c.carrying().len()), 2);
    assert!(inbox.try_iter().all(|r| serde_json::from_str::<Value>(&r).unwrap()["@type"] != "sendMessage"));

    let before_send = s.history().head();
    let result = call(&mut s, "telegram.send", d.clone()).unwrap();
    assert_eq!(result["status"], "queued");
    let operations = result["operations"].as_array().unwrap();
    assert_eq!(operations.len(), 2);
    assert_eq!(result["operation"], operations[0]);
    let requests: Vec<Value> = inbox.try_iter().map(|r| serde_json::from_str(&r).unwrap()).collect();
    assert_eq!(requests.len(), 2);
    for (i, request) in requests.iter().enumerate() {
        assert_eq!(request["@type"], "sendMessage");
        assert_eq!(request["chat_id"], BERLIN);
        assert_eq!(request["topic_id"]["forum_topic_id"], 2);
        assert_eq!(request["@extra"]["operation"], operations[i]);
        assert_eq!(call(&mut s, "telegram.status", json!({"operation": operations[i]})).unwrap()["status"], "pending");
    }
    let photo = &requests[0]["input_message_content"];
    assert_eq!(photo["@type"], "inputMessagePhoto");
    assert_eq!(photo["photo"]["photo"], json!({"@type": "inputFileLocal", "path": PHOTO}));
    assert_eq!(photo["caption"]["text"], "The picture and report");
    assert_eq!(requests[0]["reply_to"]["message_id"], 2000);
    let file = &requests[1]["input_message_content"];
    assert_eq!(file["@type"], "inputMessageDocument");
    assert_eq!(file["document"]["document"], json!({"@type": "inputFileLocal", "path": document}));
    assert_eq!(file["caption"]["text"], "");
    assert!(requests[1].get("reply_to").is_none());

    let rt = runtime::of(s.store());
    rt.operations.reply(s.store(), &json!({
        "@type": "message", "chat_id": BERLIN, "id": 9999,
        "@extra": requests[0]["@extra"], "sending_state": null
    }));
    rt.operations.fail(s.store(), operations[1].as_u64().unwrap(), "upload failed", false);
    assert_eq!(call(&mut s, "telegram.status", json!({"operation": operations[0]})).unwrap()["status"], "done");
    assert_eq!(call(&mut s, "telegram.status", json!({"operation": operations[1]})).unwrap()["status"], "failed");
    assert_eq!(field_now(&s, slot(&d)), "");
    assert!(with_chat(&s, slot(&d), |c| c.carrying().is_empty()));
    assert!(call(&mut s, "telegram.send", d).is_err());
    assert!(inbox.try_recv().is_err(), "delivery checks never repeat a send");

    assert!(s.undo());
    assert_eq!(s.history().head(), before_send, "a rejected attachment must not skip the send's undo node");
    let deletes: Vec<Value> = inbox.try_iter().map(|r| serde_json::from_str(&r).unwrap()).collect();
    assert_eq!(deletes.len(), 1);
    assert_eq!(deletes[0]["@type"], "deleteMessages");
    assert_eq!(deletes[0]["chat_id"], BERLIN);
    assert_eq!(deletes[0]["message_ids"], json!([9999]));
    assert_eq!(deletes[0]["revoke"], true);
}

#[test]
fn a_disconnected_batch_keeps_all_files_in_every_copy() {
    let attachments = Attachments::new();
    let document = attachments.file("report.txt", b"the report");
    let mut s = session();
    let first = open_root(&mut s, Chat::id(VERA));
    let message = model::history(s.store(), VERA).last().unwrap().id;
    let second = open_root(&mut s, Chat::at(VERA, message));
    let rt = runtime::of(s.store());
    let inbox = rt.connect();
    let d = draft(&mut s, json!({"chat": VERA, "text": "caption", "files": [PHOTO, document]}));
    assert!(rt.operations.list().is_empty());

    // Lose the connection after recording the batch, before any file can
    // enter the worker's inbox. Both operations belong to the same action.
    let worker = rt.clone();
    s.store().write(move |c| {
        c.commit_hook(Some(move || {
            if worker.operations.list().len() == 2 {
                worker.set_connection_error(Some("connection lost".into()));
            }
            false
        }))?;
        Ok(())
    }).unwrap();
    assert!(call(&mut s, "telegram.send", d.clone()).is_err());
    s.store().write(|c| c.commit_hook(None::<fn() -> bool>)).unwrap();
    assert!(inbox.try_recv().is_err(), "none of the batch was queued");
    for open in [first, second] {
        assert_eq!(field_now(&s, open), "caption");
        assert_eq!(with_chat(&s, open, |c| c.carrying().iter().map(|f| f.path.clone()).collect::<Vec<_>>()), vec![PHOTO.to_string(), document.clone()]);
    }
    rt.set_connection_error(None);
    assert_eq!(call(&mut s, "telegram.send", d).unwrap()["status"], "queued");
    let requests: Vec<Value> = inbox.try_iter().map(|r| serde_json::from_str(&r).unwrap()).collect();
    assert_eq!(requests.len(), 2);
    assert_eq!(requests[0]["input_message_content"]["caption"]["text"], "caption");
    assert_eq!(requests[1]["input_message_content"]["document"]["document"]["path"], document);
    assert_eq!(requests[1]["input_message_content"]["caption"]["text"], "");
}

#[test]
fn one_undo_reverses_every_attachment_in_a_send() {
    use crate::apps::telegram::history as telegram_history;
    for (from_panel, undo_before_delivery) in [(false, false), (false, true), (true, false), (true, true)] {
        let attachments = Attachments::new();
        let document = attachments.file("report.txt", b"the report");
        let mut s = session();
        open_root(&mut s, Chats::id());
        let rt = runtime::of(s.store());
        let inbox = rt.connect();
        let d = draft(&mut s, json!({"chat": VERA, "text": "caption", "files": [PHOTO, document]}));
        let (before, head) = s.history().rows();
        if from_panel { send(&mut s, slot(&d)); }
        else { call(&mut s, "telegram.send", d).unwrap(); }
        let (after, _) = s.history().rows();
        assert_eq!(after.len(), before.len() + 1);
        assert_eq!(after.last().unwrap().label, "send 2 attachments · Vera Kovac");
        let requests: Vec<Value> = inbox.try_iter().map(|r| serde_json::from_str(&r).unwrap()).collect();
        assert_eq!(requests.len(), 2);
        if undo_before_delivery {
            assert!(s.undo());
            assert!(inbox.try_recv().is_err(), "undo waits for Telegram's message ids");
        }
        for (i, request) in requests.iter().enumerate() {
            rt.operations.reply(s.store(), &json!({
                "@type": "message", "chat_id": VERA, "id": 9000 + i,
                "@extra": request["@extra"], "sending_state": null
            }));
        }
        if !undo_before_delivery { assert!(s.undo()); }
        telegram_history::pump(s.store());
        assert_eq!(s.history().head(), head, "one undo returns past the entire send");
        let deletes: Vec<Value> = inbox.try_iter().map(|r| serde_json::from_str(&r).unwrap()).collect();
        assert_eq!(deletes.len(), 2);
        let mut ids = Vec::new();
        for delete in &deletes {
            assert_eq!(delete["@type"], "deleteMessages");
            assert_eq!(delete["chat_id"], VERA);
            assert_eq!(delete["revoke"], true);
            ids.extend(delete["message_ids"].as_array().unwrap().iter().map(|id| id.as_i64().unwrap()));
        }
        ids.sort_unstable();
        assert_eq!(ids, vec![9000, 9001], "the captioned first attachment is undone too");
    }
}

#[test]
fn captionless_attachments_update_and_clear_every_matching_open_composer() {
    for from_panel in [false, true] {
        let mut s = session();
        let first = open_root(&mut s, Chat::id(VERA));
        let message = model::history(s.store(), VERA).last().unwrap().id;
        let second = open_root(&mut s, Chat::at(VERA, message));
        let inbox = runtime::of(s.store()).connect();
        let input = json!({"chat": VERA, "text": "", "files": [PHOTO]});
        let d = draft(&mut s, input.clone());
        assert_eq!(draft(&mut s, input), d, "staging the same files does not duplicate them");
        for open in [first, second] {
            assert_eq!(field_now(&s, open), "");
            assert_eq!(with_chat(&s, open, |c| c.carrying().to_vec()), vec![model::Carried { path: PHOTO.into() }]);
        }
        if from_panel {
            send(&mut s, slot(&d));
        } else {
            call(&mut s, "telegram.send", d.clone()).unwrap();
        }
        let request: Value = serde_json::from_str(&inbox.try_recv().unwrap()).unwrap();
        assert_eq!(request["input_message_content"]["@type"], "inputMessagePhoto");
        assert_eq!(request["input_message_content"]["caption"]["text"], "");
        for open in [first, second] {
            assert!(with_chat(&s, open, |c| c.carrying().is_empty()));
            let mut again = d.clone();
            again["slot"] = json!(open);
            assert!(call(&mut s, "telegram.send", again).is_err());
        }
        assert!(inbox.try_recv().is_err());
    }
}

#[test]
fn agents_can_append_attachments_but_cannot_discard_existing_files() {
    let attachments = Attachments::new();
    let document = attachments.file("report.txt", b"the report");
    let notes = attachments.file("notes \"draft\".txt", b"notes");
    let mut s = session();
    let first = open_root(&mut s, Chat::id(VERA));
    let message = model::history(s.store(), VERA).last().unwrap().id;
    let second = open_root(&mut s, Chat::at(VERA, message));
    with_chat(&s, second, |c| { c.carry(&[PHOTO.into(), notes.clone()]); });
    let error = call(&mut s, "telegram.draft", json!({
        "chat": VERA, "text": "caption", "replace": true, "files": [document]
    })).unwrap_err();
    let mut files: Vec<String> = serde_json::from_str(error.split_once("in this order: ").unwrap().1).unwrap();
    assert_eq!(files, vec![PHOTO.to_string(), notes]);
    assert_eq!(field_now(&s, first), "", "all composers are checked before any is changed");
    assert!(with_chat(&s, first, |c| c.carrying().is_empty()));
    files.push(document);
    let d = draft(&mut s, json!({"chat": VERA, "text": "caption", "files": files}));
    for open in [first, second] {
        assert_eq!(with_chat(&s, open, |c| c.carrying().iter().map(|f| f.path.clone()).collect::<Vec<_>>()), files);
    }
    assert_eq!(d["files"], json!(files));
}

#[test]
fn invalid_attachment_paths_leave_existing_work_intact() {
    let attachments = Attachments::new();
    let empty = attachments.file("empty.txt", b"");
    let missing = attachments.0.join("missing.pdf");
    let mut s = session();
    let chat = open_root(&mut s, Chat::id(VERA));
    with_chat(&s, chat, |c| c.typed("keep me"));
    let inbox = runtime::of(s.store()).connect();
    for files in [
        json!("not an array"), json!([null]), json!([1]), json!([""]),
        json!(["relative.png"]), json!(["https://example.com/photo.png"]),
        json!([PHOTO, PHOTO]), json!([missing]), json!([attachments.0]), json!([empty]),
    ] {
        assert!(call(&mut s, "telegram.draft", json!({
            "chat": VERA, "text": "replacement", "replace": true, "files": files
        })).is_err(), "{files}");
        assert_eq!(field_now(&s, chat), "keep me");
        assert!(with_chat(&s, chat, |c| c.carrying().is_empty()));
    }
    assert!(inbox.try_recv().is_err());
}

#[test]
fn a_send_rechecks_attachment_presence_and_order_before_queueing_any_file() {
    for change in ["added", "removed", "reordered", "omitted", "missing", "empty"] {
        let attachments = Attachments::new();
        let document = attachments.file("report.txt", b"the report");
        let mut s = session();
        open_root(&mut s, Chats::id());
        let inbox = runtime::of(s.store()).connect();
        let mut d = draft(&mut s, json!({"chat": VERA, "text": "caption", "files": [PHOTO, document]}));
        let chat = slot(&d);
        match change {
            "added" => with_chat(&s, chat, |c| { c.carry(&["/tmp/other.pdf".into()]); }),
            "removed" => with_chat(&s, chat, |c| { c.uncarry(0); }),
            "reordered" => with_chat(&s, chat, |c| { c.move_carried(0, 1); }),
            "omitted" => { d.as_object_mut().unwrap().remove("files"); }
            "missing" => std::fs::remove_file(&document).unwrap(),
            "empty" => std::fs::write(&document, b"").unwrap(),
            _ => unreachable!(),
        }
        let before = with_chat(&s, chat, |c| c.carrying().to_vec());
        assert!(call(&mut s, "telegram.send", d).is_err(), "{change}");
        assert!(inbox.try_recv().is_err(), "{change}: no file is queued");
        assert_eq!(field_now(&s, chat), "caption");
        assert_eq!(with_chat(&s, chat, |c| c.carrying().to_vec()), before);
    }
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
        assert!(c.edit((c.peer(), own)));
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
                c.reply((c.peer(), message));
            }),
            "files" => with_chat(&s, chat, |c| {
                c.carry(&["/tmp/keep.pdf".into()]);
            }),
            "edit" => with_chat(&s, chat, |c| {
                c.edit((c.peer(), message));
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
    for (disconnected, files) in [(false, vec![]), (true, vec![]), (false, vec![PHOTO]), (true, vec![PHOTO])] {
        let mut s = session();
        open_root(&mut s, Chats::id());
        let rt = runtime::of(s.store());
        if disconnected {
            drop(rt.connect());
        }
        let d = draft(&mut s, json!({"chat": VERA, "text": "keep me", "files": files}));
        assert!(call(&mut s, "telegram.send", d.clone()).is_err());
        assert_eq!(draft_row(&s, VERA), "keep me");
        assert_eq!(field_now(&s, slot(&d)), "keep me");
        assert_eq!(with_chat(&s, slot(&d), |c| c.carrying().iter().map(|f| f.path.clone()).collect::<Vec<_>>()), files);
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
        .send_draft(&mut s)
        .into_iter().next()
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
