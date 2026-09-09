use super::*;

struct HeldReaders(Vec<std::sync::mpsc::Sender<()>>);

impl HeldReaders {
    fn new(s: &Session) -> Self {
        let mut release = Vec::new();
        // Hold the four bounded SQLite readers. A delete must remain a
        // pending gesture while unrelated UI events continue to run.
        for _ in 0..4 {
            let db = s.store().db();
            let (started, observed) = std::sync::mpsc::channel();
            let (resume, held) = std::sync::mpsc::channel();
            release.push(resume);
            kernel::runtime::spawn(async move {
                db.read_async(move |_| {
                    started.send(()).unwrap();
                    held.recv_timeout(Duration::from_secs(5)).expect("reader released");
                    Ok(())
                }).await.unwrap();
            });
            observed.recv_timeout(Duration::from_secs(5)).unwrap();
        }
        Self(release)
    }
}

impl Drop for HeldReaders {
    fn drop(&mut self) {
        for reader in &self.0 { let _ = reader.send(()); }
    }
}

#[test]
fn deletion_prepares_off_ui_and_keeps_the_original_selection() {
    let mut s = session();
    let slot = open_root(&mut s, Chat::id(VERA));
    let mine = model::history(s.store(), VERA).iter().filter(|m| m.out && !m.service)
        .take(3).map(|m| m.key()).collect::<Vec<_>>();
    assert_eq!(mine.len(), 3);
    with_chat(&s, slot, |chat| {
        for &key in &mine[..2] { chat.set_cursor(key); chat.toggle_mark(); }
    });
    let rt = runtime::of(s.store());
    let inbox = rt.connect();
    s.store().attach_ui(|| {});
    wait_transcript(&mut s, slot);
    let readers = HeldReaders::new(&s);
    let head = s.history().head();
    verb(&mut s, slot, "telegram.delete");
    assert_eq!(s.history().head(), head, "the UI returns before the metadata query can run");
    assert!(inbox.try_recv().is_err());
    with_chat(&s, slot, |chat| {
        assert_eq!(chat.marks().len(), 2, "pending preparation retains the original marks");
        chat.set_cursor(mine[2]);
        chat.toggle_mark();
        chat.typed("typed during deletion preparation");
    });
    // Repeating the gesture cannot create a second operation while its
    // first snapshot is pending, even after selecting an additional row.
    verb(&mut s, slot, "telegram.delete");
    assert!(s.notes().iter().any(|note| note.msg.contains("previous Telegram change")),
        "duplicate refusal: {:?}", s.notes());
    assert!(inbox.try_recv().is_err());
    drop(readers);
    let deadline = Instant::now() + Duration::from_secs(5);
    while s.history().head() == head || with_chat(&s, slot, |chat| chat.marks().contains(&mine[0])) {
        s.settle();
        assert!(Instant::now() < deadline, "prepared deletion completed");
        std::thread::sleep(Duration::from_millis(1));
    }
    let request = receive(&inbox);
    assert_eq!(request["@type"], "deleteMessages");
    let expected = mine[..2].iter().map(|key| key.1).collect::<Vec<_>>();
    assert_eq!(request["message_ids"], json!(expected));
    assert!(inbox.try_recv().is_err(), "the reserved gesture executes once");
    assert!(s.history().rows().0.last().unwrap().label.contains("undo resends copies"));
    with_chat(&s, slot, |chat| {
        assert_eq!(chat.cursor(), Some(mine[2]));
        assert_eq!(chat.marks().iter().copied().collect::<Vec<_>>(), vec![mine[2]]);
        assert_eq!(chat.field_text(), "typed during deletion preparation");
    });
    s.shutdown();
}

#[test]
fn a_prepared_deletion_survives_panel_closure_and_rechecks_the_lease() {
    for lost_lease in [false, true] {
        let mut s = session();
        let slot = open_root(&mut s, Chat::id(VERA));
        let key = model::history(s.store(), VERA).iter().find(|m| m.out && !m.service).unwrap().key();
        with_chat(&s, slot, |chat| chat.set_cursor(key));
        let inbox = runtime::of(s.store()).connect();
        s.store().attach_ui(|| {});
        wait_transcript(&mut s, slot);
        let readers = HeldReaders::new(&s);
        verb(&mut s, slot, "telegram.delete");
        go(&mut s, Nav::Close { slot, label: None });
        assert!(s.panel(slot).is_none());
        if lost_lease { s.store().set_writable(false); }
        drop(readers);
        // Shutdown owns accepted preparation even when its originating panel
        // no longer exists, and runs the same admission/error completion.
        s.shutdown();
        if lost_lease {
            assert!(inbox.try_recv().is_err(), "a stale eligibility snapshot cannot bypass the lease");
            assert!(s.notes().iter().any(|note| note.msg.contains("another device holds the lease")));
            assert!(s.history().rows().0.iter().all(|row| row.kind != "delete"));
        } else {
            let request = receive(&inbox);
            assert_eq!(request["message_ids"], json!([key.1]));
            assert!(s.history().rows().0.iter().any(|row| row.kind == "delete"));
        }
    }
}

fn wait_transcript(s: &mut Session, slot: SlotId) {
    let deadline = Instant::now() + Duration::from_secs(5);
    while !with_chat(s, slot, |chat| chat.snapshot(0.0).ready) {
        s.settle();
        assert!(Instant::now() < deadline, "initial transcript ready");
        std::thread::sleep(Duration::from_millis(1));
    }
}

fn message(id: i64, text: &str) -> Value {
    json!({"@type": "message", "chat_id": VERA, "id": id, "is_outgoing": true,
        "sender_id": {"@type": "messageSenderUser", "user_id": 1000},
        "content": {"@type": "messageText", "text": {"@type": "formattedText", "text": text, "entities": []}}})
}

fn originals(acc: &sync::Account<FakeTd>, s: &Session, td: &FakeTd, messages: Vec<Value>) {
    let get = last_reaction_request(td, "getMessages");
    acc.on_update(s.world(), &json!({"@type": "messages", "messages": messages,
        "@extra": get["@extra"]}).to_string());
}

fn delivered(acc: &sync::Account<FakeTd>, s: &Session, td: &FakeTd, id: i64) {
    let sent = last_reaction_request(td, "sendMessage");
    acc.on_update(s.world(), &json!({"@type": "message", "chat_id": sent["chat_id"], "id": id,
        "is_outgoing": true, "content": {"@type": "messageText", "text": {"text": "restored"}},
        "@extra": sent["@extra"]}).to_string());
    acc.drain(s.world());
}

#[test]
fn deletion_undo_resends_saved_formatting_and_redo_deletes_the_delivered_copy() {
    let mut s = session();
    let td = FakeTd::new();
    let acc = connected_reaction_account(&s, td.clone());
    acc.drain(s.world());
    history::command(&mut s, &requests::delete_messages(VERA, &[900], true)).unwrap();
    acc.drain(s.world());
    assert!(!td.sent_types().contains(&"deleteMessages".into()));
    assert!(s.history().rows().0.last().unwrap().label.contains("undo resends copies"));
    let mut original = message(900, "keep the bold words");
    original["content"]["text"]["entities"] = json!([
        {"@type": "textEntity", "offset": 9, "length": 4, "type": {"@type": "textEntityTypeBold"}}
    ]);
    originals(&acc, &s, &td, vec![original.clone()]);
    assert!(s.undo());
    acc.drain(s.world());
    assert!(!td.sent_types().contains(&"sendMessage".into()), "the deletion must settle first");
    ok(&acc, &s, &last_reaction_request(&td, "deleteMessages"));
    acc.drain(s.world());
    let send = last_reaction_request(&td, "sendMessage");
    assert_eq!(send["input_message_content"]["text"], original["content"]["text"]);
    assert_eq!(send["input_message_content"]["clear_draft"], false);
    assert!(s.redo());
    acc.on_update(s.world(), &json!({"@type": "message", "chat_id": VERA, "id": 101,
        "sending_state": {"@type": "messageSendingStatePending"}, "@extra": send["@extra"]}).to_string());
    acc.drain(s.world());
    assert_eq!(td.sent_types().iter().filter(|t| *t == "deleteMessages").count(), 1);
    acc.on_update(s.world(), &json!({"@type": "updateMessageSendSucceeded", "old_message_id": 101,
        "message": {"chat_id": VERA, "id": 901}}).to_string());
    acc.drain(s.world());
    acc.drain(s.world());
    let delete = last_reaction_request(&td, "deleteMessages");
    assert_eq!(delete["message_ids"], json!([901]));
    ok(&acc, &s, &delete);
    assert!(s.undo());
    acc.drain(s.world());
    delivered(&acc, &s, &td, 902);
    assert!(s.redo());
    acc.drain(s.world());
    assert_eq!(last_reaction_request(&td, "deleteMessages")["message_ids"], json!([902]));
}

#[test]
fn batch_restore_keeps_topics_and_replies_to_the_new_parent_and_redoes_the_whole_batch() {
    let mut s = session();
    let td = FakeTd::new();
    let acc = connected_reaction_account(&s, td.clone());
    acc.drain(s.world());
    history::command(&mut s, &requests::delete_messages(VERA, &[901, 900], true)).unwrap();
    acc.drain(s.world());
    let mut parent = message(900, "parent");
    parent["topic_id"] = json!({"@type": "messageTopicForum", "forum_topic_id": 42});
    let mut reply = message(901, "reply");
    reply["topic_id"] = parent["topic_id"].clone();
    reply["reply_to"] = json!({"@type": "messageReplyToMessage", "chat_id": VERA, "message_id": 900,
        "quote": {"@type": "textQuote", "text": {"text": "parent"}, "position": 0, "is_manual": true}});
    originals(&acc, &s, &td, vec![reply, parent.clone()]);
    ok(&acc, &s, &last_reaction_request(&td, "deleteMessages"));
    assert!(s.undo());
    acc.drain(s.world());
    let sent = last_reaction_request(&td, "sendMessage");
    assert_eq!(sent["input_message_content"]["text"]["text"], "parent");
    assert_eq!(sent["topic_id"], parent["topic_id"]);
    delivered(&acc, &s, &td, 1000);
    acc.drain(s.world());
    let sent = last_reaction_request(&td, "sendMessage");
    assert_eq!(sent["reply_to"]["message_id"], 1000);
    assert_eq!(sent["reply_to"]["quote"]["@type"], "inputTextQuote");
    assert!(sent["reply_to"]["quote"]["is_manual"].is_null());
    delivered(&acc, &s, &td, 1001);
    assert!(s.redo());
    acc.drain(s.world());
    assert_eq!(last_reaction_request(&td, "deleteMessages")["message_ids"], json!([1001, 1000]));
}

#[test]
fn restoring_a_deleted_send_keeps_all_of_its_replacement_ids_linked() {
    let mut s = session();
    let td = FakeTd::new();
    let acc = connected_reaction_account(&s, td.clone());
    acc.drain(s.world());
    history::command(&mut s, &requests::send_message(VERA, "original", None)).unwrap();
    acc.drain(s.world());
    delivered(&acc, &s, &td, 900);
    history::command(&mut s, &requests::delete_messages(VERA, &[900], true)).unwrap();
    acc.drain(s.world());
    originals(&acc, &s, &td, vec![message(900, "original")]);
    ok(&acc, &s, &last_reaction_request(&td, "deleteMessages"));
    assert!(s.undo());
    acc.drain(s.world());
    delivered(&acc, &s, &td, 901);
    assert!(s.undo());
    acc.drain(s.world());
    let delete = last_reaction_request(&td, "deleteMessages");
    assert_eq!(delete["message_ids"], json!([901]));
    ok(&acc, &s, &delete);
    assert!(s.redo());
    acc.drain(s.world());
    delivered(&acc, &s, &td, 902);
    assert!(s.redo());
    acc.drain(s.world());
    let delete = last_reaction_request(&td, "deleteMessages");
    assert_eq!(delete["message_ids"], json!([902]));
    ok(&acc, &s, &delete);
    assert!(s.undo());
    acc.drain(s.world());
    delivered(&acc, &s, &td, 903);
    assert!(s.undo());
    acc.drain(s.world());
    assert_eq!(last_reaction_request(&td, "deleteMessages")["message_ids"], json!([903]));
}

#[test]
fn incomplete_or_ineligible_snapshots_keep_every_original_and_disable_retry() {
    for invalid in [Value::Null, json!({"is_outgoing": false}), json!({"chat_id": STELAXIS}),
        json!({"can_be_saved": false}), json!({"content": {"@type": "messagePoll"}})] {
        let mut s = session();
        let td = FakeTd::new();
        let acc = connected_reaction_account(&s, td.clone());
        acc.drain(s.world());
        let id = history::command(&mut s, &requests::delete_messages(VERA, &[900], true)).unwrap();
        acc.drain(s.world());
        let mut original = message(900, "keep me");
        if let Some(fields) = invalid.as_object() {
            original.as_object_mut().unwrap().extend(fields.clone());
        } else { original = Value::Null; }
        originals(&acc, &s, &td, vec![original]);
        acc.drain(s.world());
        let rt = runtime::of(s.store());
        assert!(!td.sent_types().contains(&"deleteMessages".into()));
        assert!(rt.operations.retry(id).is_none());
        assert!(rt.operations.list().iter().any(|op| op.id == id && op.line().contains("nothing was deleted")));
    }
}

#[test]
fn partial_restore_failure_never_automatically_resends_already_restored_messages() {
    let mut s = session();
    let td = FakeTd::new();
    let acc = connected_reaction_account(&s, td.clone());
    acc.drain(s.world());
    history::command(&mut s, &requests::delete_messages(VERA, &[900, 901], true)).unwrap();
    acc.drain(s.world());
    originals(&acc, &s, &td, vec![message(900, "first"), message(901, "second")]);
    ok(&acc, &s, &last_reaction_request(&td, "deleteMessages"));
    assert!(s.undo());
    acc.drain(s.world());
    delivered(&acc, &s, &td, 1000);
    acc.drain(s.world());
    let sent = last_reaction_request(&td, "sendMessage");
    acc.on_update(s.world(), &json!({"@type": "error", "code": 400, "message": "CHAT_WRITE_FORBIDDEN",
        "@extra": sent["@extra"]}).to_string());
    acc.drain(s.world());
    s.redo();
    s.undo();
    acc.drain(s.world());
    assert_eq!(td.sent_types().iter().filter(|t| *t == "sendMessage").count(), 2);
    assert_eq!(s.history().rows().0.last().unwrap().state, "expired");
}

#[test]
fn attachment_bytes_are_saved_before_deletion_and_survive_the_source_disappearing() {
    for download in [false, true] {
        let mut s = session();
        let td = FakeTd::new();
        let acc = connected_reaction_account(&s, td.clone());
        acc.drain(s.world());
        let path = std::env::temp_dir().join(format!("telegram-delete-test-{}-{download}.pdf", std::process::id()));
        std::fs::write(&path, b"original document bytes").unwrap();
        let file = json!({"@type": "file", "id": 77, "size": 23,
            "remote": {"id": "delete-document", "unique_id": "delete-document"},
            "local": {"path": path, "is_downloading_completed": true}});
        let mut original = message(900, "");
        original["content"] = json!({"@type": "messageDocument", "caption": {"text": "the caption", "entities": []},
            "document": {"file_name": "../../report.pdf", "document": file}});
        if download { original["content"]["document"]["document"]["local"] = Value::Null; }
        history::command(&mut s, &requests::delete_messages(VERA, &[900], true)).unwrap();
        acc.drain(s.world());
        originals(&acc, &s, &td, vec![original]);
        if download {
            assert!(!td.sent_types().contains(&"deleteMessages".into()));
            let get = last_reaction_request(&td, "downloadFile");
            assert_eq!(get["file_id"], 77);
            assert_eq!(get["synchronous"], true);
            let rt = runtime::of(s.store());
            let other: Value = serde_json::from_str(&rt.operations.track(&requests::download_file(77, 1))).unwrap();
            let mut file = file.clone();
            file["@extra"] = get["@extra"].clone();
            acc.on_update(s.world(), &file.to_string());
            assert!(rt.operations.pending(other["@extra"]["operation"].as_u64().unwrap()),
                "an undo backup does not complete an unrelated cache download");
        }
        ok(&acc, &s, &last_reaction_request(&td, "deleteMessages"));
        std::fs::remove_file(&path).unwrap();
        assert!(s.undo());
        acc.drain(s.world());
        let sent = last_reaction_request(&td, "sendMessage");
        let backup = std::path::PathBuf::from(sent["input_message_content"]["document"]["document"]["path"].as_str().unwrap());
        assert_ne!(backup, path);
        assert_eq!(backup.file_name().unwrap(), "report.pdf");
        assert_eq!(std::fs::read(&backup).unwrap(), b"original document bytes");
        assert_eq!(sent["input_message_content"]["caption"]["text"], "the caption");
        delivered(&acc, &s, &td, 901);
        drop(acc);
        drop(s);
        let deadline = Instant::now() + Duration::from_secs(5);
        while backup.exists() {
            assert!(Instant::now() < deadline, "discarded history releases its saved attachment bytes in the pool");
            std::thread::sleep(Duration::from_millis(1));
        }
    }
}

#[test]
fn a_timed_out_attachment_backup_never_deletes_on_a_late_reply() {
    let mut s = session();
    let td = FakeTd::new();
    let acc = connected_reaction_account(&s, td.clone());
    acc.drain(s.world());
    let mut original = message(900, "");
    original["content"] = json!({"@type": "messageDocument", "document": {"document": {"@type": "file", "id": 77}}});
    let id = history::command(&mut s, &requests::delete_messages(VERA, &[900], true)).unwrap();
    acc.drain(s.world());
    originals(&acc, &s, &td, vec![original]);
    let get = last_reaction_request(&td, "downloadFile");
    let rt = runtime::of(s.store());
    rt.operations.fail(s.store(), get["@extra"]["operation"].as_u64().unwrap(), "download timed out", false);
    acc.on_update(s.world(), &json!({"@type": "file", "id": 77, "@extra": get["@extra"],
        "local": {"is_downloading_completed": true, "path": "/nonexistent/late-file"}}).to_string());
    acc.drain(s.world());
    assert!(matches!(rt.operations.outcome(id).unwrap().status, Status::Failed { .. }));
    assert!(rt.operations.retry(id).is_none());
    assert!(!td.sent_types().contains(&"deleteMessages".into()));
}

#[test]
fn media_restores_upload_the_full_saved_file_with_the_original_metadata() {
    let path = std::env::temp_dir().join(format!("telegram-delete-media-{}.bin", std::process::id()));
    std::fs::write(&path, b"full attachment").unwrap();
    let file = json!({"@type": "file", "id": 77, "local": {"path": path, "is_downloading_completed": true}});
    let cases = [
        (json!({"@type": "messagePhoto", "photo": {"sizes": [
            {"width": 20, "height": 20, "photo": {"id": 99}},
            {"width": 1000, "height": 800, "photo": file}
        ]}, "has_spoiler": true, "show_caption_above_media": true}), "photo", "inputPhoto"),
        (json!({"@type": "messageVideo", "video": {"video": file, "duration": 15, "width": 640, "height": 480,
            "supports_streaming": true, "file_name": "clip.mp4"}}), "video", "inputVideo"),
        (json!({"@type": "messageAnimation", "animation": {"animation": file, "duration": 3,
            "width": 100, "height": 120}}), "animation", "inputAnimation"),
        (json!({"@type": "messageAudio", "audio": {"audio": file, "duration": 15, "performer": "Artist",
            "title": "Song", "file_name": "song.flac"}}), "audio", "inputAudio"),
        (json!({"@type": "messageVoiceNote", "voice_note": {"voice": file, "duration": 4,
            "waveform": "AAAA"}}), "voice_note", "inputVoiceNote"),
        (json!({"@type": "messageVideoNote", "video_note": {"video": file, "duration": 4, "length": 200}}),
            "video_note", "inputVideoNote"),
        (json!({"@type": "messageSticker", "sticker": {"sticker": file, "width": 512, "height": 512,
            "emoji": "👍", "format": {"@type": "stickerFormatWebp"}}}), "sticker", "inputSticker"),
    ];
    for (content, field, kind) in cases {
        let mut s = session();
        let td = FakeTd::new();
        let acc = connected_reaction_account(&s, td.clone());
        acc.drain(s.world());
        let mut original = message(900, "");
        original["content"] = content.clone();
        history::command(&mut s, &requests::delete_messages(VERA, &[900], true)).unwrap();
        acc.drain(s.world());
        originals(&acc, &s, &td, vec![original]);
        assert!(!td.sent_types().contains(&"downloadFile".into()), "the original bytes are available");
        ok(&acc, &s, &last_reaction_request(&td, "deleteMessages"));
        assert!(s.undo());
        acc.drain(s.world());
        let sent = last_reaction_request(&td, "sendMessage");
        let input = &sent["input_message_content"];
        assert_eq!(input[field]["@type"], kind);
        let backup = input[field][field]["path"].as_str().unwrap();
        assert_eq!(std::fs::read(backup).unwrap(), b"full attachment");
        if field == "photo" {
            assert_eq!(input[field]["width"], 1000);
            assert_eq!(input["has_spoiler"], true);
            assert_eq!(input["show_caption_above_media"], true);
        } else {
            for key in ["width", "height", "duration", "length", "waveform", "title", "performer", "supports_streaming"] {
                assert_eq!(input[field][key], content[field][key]);
            }
        }
        delivered(&acc, &s, &td, 901);
    }
    std::fs::remove_file(path).unwrap();
}

#[test]
fn losing_the_lease_while_saving_originals_cancels_deletion() {
    let mut s = session();
    let td = FakeTd::new();
    let acc = connected_reaction_account(&s, td.clone());
    acc.drain(s.world());
    let id = history::command(&mut s, &requests::delete_messages(VERA, &[900], true)).unwrap();
    acc.drain(s.world());
    s.store().set_writable(false);
    originals(&acc, &s, &td, vec![message(900, "keep me")]);
    acc.drain(s.world());
    assert!(!td.sent_types().contains(&"deleteMessages".into()));
    assert!(matches!(runtime::of(s.store()).operations.outcome(id).unwrap().status, Status::Failed { .. }));
}
