use super::*;

fn attachment_requests() -> [String; 2] {
    let photo = model::Carried { path: "/tmp/photo.jpg".into() };
    let document = model::Carried { path: "/tmp/report.txt".into() };
    [
        requests::send_file(VERA, Some(42), &photo, "caption"),
        requests::send_file(VERA, None, &document, ""),
    ]
}

fn attachments(s: &mut Session) -> Vec<u64> {
    history::batch(s, &attachment_requests(), "send 2 attachments".into()).unwrap()
}

fn delivered(s: &Session, request: &Value, id: i64) {
    runtime::of(s.store()).operations.reply(s.store(), &json!({
        "@type": "message", "chat_id": VERA, "id": id, "@extra": request["@extra"]
    }));
}

#[test]
fn partially_rejected_sends_undo_delivered_files_without_reversing_the_previous_action() {
    for rejected in 0..2 {
        for settled_before_undo in 0..=2 {
            for uncertain in [false, true] {
                let mut s = session();
                let rt = runtime::of(s.store());
                let inbox = rt.connect();
                history::command(&mut s, &requests::send_message(VERA, "keep this message", None)).unwrap();
                delivered(&s, &receive(&inbox), 800);
                let previous = s.history().head();
                let operations = attachments(&mut s);
                let requests = [receive(&inbox), receive(&inbox)];
                let accepted = 1 - rejected;
                if settled_before_undo > 0 {
                    rt.operations.fail(s.store(), operations[rejected], "upload failed", uncertain);
                }
                if settled_before_undo == 2 { delivered(&s, &requests[accepted], 900); }
                history::pump(s.store());
                assert!(s.undo());
                if settled_before_undo < 2 {
                    assert!(inbox.try_recv().is_err(), "undo waits for delivery");
                    if settled_before_undo == 0 {
                        rt.operations.fail(s.store(), operations[rejected], "upload failed", uncertain);
                    }
                    delivered(&s, &requests[accepted], 900);
                    history::pump(s.store());
                }
                assert_eq!(s.history().head(), previous, "the previous action stays applied");
                assert_eq!(s.history().rows().0.last().unwrap().state, "undone");
                let delete = receive(&inbox);
                assert_eq!(delete["@type"], "deleteMessages");
                assert_eq!(delete["message_ids"], json!([900]));
                assert_eq!(delete["revoke"], true);
                assert!(inbox.try_recv().is_err(), "only the delivered attachment is deleted");

                rt.operations.reply(s.store(), &json!({"@type": "ok", "@extra": delete["@extra"]}));
                history::pump(s.store());
                assert!(s.redo());
                let redo = receive(&inbox);
                assert_eq!(redo["input_message_content"], requests[accepted]["input_message_content"]);
                assert_eq!(redo["reply_to"], requests[accepted]["reply_to"]);
                assert!(inbox.try_recv().is_err(), "redo never retries the failed attachment");
                assert_eq!(rt.operations.list().iter().find(|op| op.id == operations[rejected]).unwrap().status,
                    Status::Failed { error: "upload failed".into(), uncertain });
                delivered(&s, &redo, 901);
                history::pump(s.store());
                assert!(s.undo());
                assert_eq!(s.history().head(), previous);
                assert_eq!(receive(&inbox)["message_ids"], json!([901]));
                assert!(inbox.try_recv().is_err());
            }
        }
    }
}

#[test]
fn late_attachment_delivery_and_explicit_retries_still_follow_the_sends_undo() {
    for (grouped, explicit_retry) in [(false, false), (false, true), (true, false), (true, true)] {
        let mut s = session();
        let rt = runtime::of(s.store());
        let inbox = rt.connect();
        let operations = if grouped { attachments(&mut s) } else {
            vec![history::command(&mut s, &attachment_requests()[0]).unwrap()]
        };
        let requests: Vec<_> = operations.iter().map(|_| receive(&inbox)).collect();
        let rejected = requests.len() - 1;
        if grouped { delivered(&s, &requests[0], 900); }
        rt.operations.fail(s.store(), operations[rejected], "upload failed", !explicit_retry);
        history::pump(s.store());
        assert!(s.undo());
        if grouped {
            let delete = receive(&inbox);
            assert_eq!(delete["message_ids"], json!([900]));
            rt.operations.reply(s.store(), &json!({"@type": "ok", "@extra": delete["@extra"]}));
            history::pump(s.store());
        }
        assert!(inbox.try_recv().is_err());

        let request = if explicit_retry {
            let retry = rt.operations.retry(operations[rejected]).unwrap();
            assert!(rt.send(&retry));
            receive(&inbox)
        } else {
            assert!(rt.operations.retry(operations[rejected]).is_none());
            requests[rejected].clone()
        };
        delivered(&s, &request, 901);
        history::pump(s.store());
        let delete = receive(&inbox);
        assert_eq!(delete["@type"], "deleteMessages");
        assert_eq!(delete["message_ids"], json!([901]));
        assert!(inbox.try_recv().is_err());
        assert_eq!(s.history().rows().0.last().unwrap().state, "undone");
    }
}

#[test]
fn rejected_attachment_redo_leaves_its_delivered_sibling_undoable() {
    for rejected in 0..2 {
        let mut s = session();
        let rt = runtime::of(s.store());
        let inbox = rt.connect();
        attachments(&mut s);
        for id in [900, 901] { delivered(&s, &receive(&inbox), id); }
        assert!(s.undo());
        for _ in 0..2 {
            let delete = receive(&inbox);
            rt.operations.reply(s.store(), &json!({"@type": "ok", "@extra": delete["@extra"]}));
        }
        history::pump(s.store());
        assert!(s.redo());
        let requests = [receive(&inbox), receive(&inbox)];
        let failed = requests[rejected]["@extra"]["operation"].as_u64().unwrap();
        rt.operations.fail(s.store(), failed, "upload failed", false);
        delivered(&s, &requests[1 - rejected], 902);
        history::pump(s.store());
        assert!(s.undo());
        assert_eq!(s.history().rows().0.last().unwrap().state, "undone");
        assert_eq!(receive(&inbox)["message_ids"], json!([902]));
        assert!(inbox.try_recv().is_err());
    }
}

#[test]
fn a_failed_attachment_deletion_blocks_redo_before_any_sibling_is_resent() {
    for rejected in 0..2 {
        let mut s = session();
        let rt = runtime::of(s.store());
        let inbox = rt.connect();
        attachments(&mut s);
        for id in [900, 901] { delivered(&s, &receive(&inbox), id); }
        assert!(s.undo());
        let deletes = [receive(&inbox), receive(&inbox)];
        let failed = deletes[rejected]["@extra"]["operation"].as_u64().unwrap();
        rt.operations.fail(s.store(), failed, "MESSAGE_DELETE_FORBIDDEN", false);
        rt.operations.reply(s.store(), &json!({"@type": "ok", "@extra": deletes[1 - rejected]["@extra"]}));
        history::pump(s.store());
        s.redo();
        history::pump(s.store());
        assert!(inbox.try_recv().is_err(), "a failed reversal cannot produce a partial resend");
        assert_eq!(s.history().rows().0.last().unwrap().state, "expired");
    }
}

#[test]
fn rejected_and_uncertain_sends_consume_their_own_undo_without_retrying() {
    let [photo, document] = attachment_requests();
    for requests in [
        vec![requests::send_message(VERA, "once", None)],
        vec![photo.clone()], vec![document.clone()], vec![photo, document],
    ] {
        for uncertain in [false, true] {
            for undo_before_failure in [false, true] {
                let mut s = session();
                let rt = runtime::of(s.store());
                let inbox = rt.connect();
                history::command(&mut s, &requests::send_message(VERA, "keep this message", None)).unwrap();
                delivered(&s, &receive(&inbox), 800);
                let previous = s.history().head();
                let operations = if let [request] = requests.as_slice() {
                    vec![history::command(&mut s, request).unwrap()]
                } else {
                    history::batch(&mut s, &requests, "send attachments".into()).unwrap()
                };
                let sent = s.history().head();
                for _ in &operations { assert_eq!(receive(&inbox)["@type"], "sendMessage"); }
                if undo_before_failure { assert!(s.undo()); }
                for operation in &operations {
                    rt.operations.fail(s.store(), *operation, "send failed", uncertain);
                }
                history::pump(s.store());
                if !undo_before_failure { assert!(s.undo()); }
                assert_eq!(s.history().head(), previous, "the failed send consumes exactly its own undo step");
                assert_eq!(s.history().rows().0.last().unwrap().state, "undone");
                assert!(inbox.try_recv().is_err(), "the previous message stays delivered");

                assert!(s.redo());
                assert_eq!(s.history().head(), sent);
                assert_eq!(s.history().rows().0.last().unwrap().state, "applied");
                assert!(s.undo());
                assert_eq!(s.history().head(), previous);
                assert!(inbox.try_recv().is_err(), "redo never retries a rejected or uncertain send");
                for operation in &operations {
                    assert_eq!(rt.operations.list().iter().find(|op| op.id == *operation).unwrap().status,
                        Status::Failed { error: "send failed".into(), uncertain });
                }
                assert!(s.undo(), "a separate undo can still reverse the previous action");
                assert_eq!(receive(&inbox)["message_ids"], json!([800]));
                assert!(inbox.try_recv().is_err());
            }
        }
    }
}
