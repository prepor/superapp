use super::*;

fn requests_of(td: &FakeTd, kind: &str) -> Vec<Value> {
    td.sent().iter().map(|raw| serde_json::from_str::<Value>(raw).unwrap())
        .filter(|v| v["@type"] == kind).collect()
}

#[test]
fn one_batch_gesture_undoes_and_redoes_all_three_chats() {
    for verb_name in ["telegram.mute", "telegram.pin", "telegram.archive", "telegram.unarchive"] {
        let mut s = session();
        let list = open_root(&mut s, if verb_name == "telegram.unarchive" { Chats::archive() } else { Chats::id() });
        with_chats(&s, list, |c| c.list_mut().marks_mut().extend(
            [VERA, STELAXIS, FAMILY].map(|peer| format!("{peer}:0"))));
        let td = FakeTd::new();
        let acc = connected_reaction_account(&s, td.clone());
        acc.drain(s.world());
        let before = s.history().rows().0.len();
        verb(&mut s, list, verb_name);
        assert_eq!(s.history().rows().0.len(), before + 1);
        assert!(s.history().rows().0.last().unwrap().label.ends_with("3 chats"));
        assert_eq!(with_chats(&s, list, |c| c.list_mut().marks().len()), 0);
        acc.drain(s.world());
        let reads: Vec<_> = requests_of(&td, "getChat").into_iter().filter(|v|
            v["@extra"]["context"].as_str().is_some_and(|s| s.starts_with("undo_snapshot:"))).collect();
        assert_eq!(reads.len(), 3);
        let previous_list = if verb_name == "telegram.unarchive" { "chatListArchive" } else { "chatListMain" };
        for (i, read) in reads.iter().enumerate() {
            acc.on_update(s.world(), &json!({"@type": "chat", "id": read["chat_id"], "@extra": read["@extra"],
                "notification_settings": {"mute_for": 10 + i, "use_default_mute_for": true},
                "positions": [{"list": {"@type": previous_list}, "order": "123", "is_pinned": false}]
            }).to_string());
        }
        let kind = match verb_name {
            "telegram.mute" => "setChatNotificationSettings",
            "telegram.pin" => "toggleChatIsPinned",
            _ => "addChatToList",
        };
        let originals = requests_of(&td, kind);
        assert_eq!(originals.len(), 3);
        assert!(s.undo());
        acc.drain(s.world());
        assert_eq!(requests_of(&td, kind).len(), 3, "undo waits for acknowledgements");
        for (i, original) in originals.iter().enumerate() {
            ok(&acc, &s, original);
            acc.drain(s.world());
            assert_eq!(requests_of(&td, kind).len(), 4 + i, "each chat settles independently");
        }
        let undos = requests_of(&td, kind)[3..].to_vec();
        for undo in &undos {
            let i = reads.iter().position(|read| read["chat_id"] == undo["chat_id"]).unwrap();
            match kind {
                "setChatNotificationSettings" => assert_eq!(undo["notification_settings"],
                    json!({"mute_for": 10 + i, "use_default_mute_for": true})),
                "toggleChatIsPinned" => assert_eq!(undo["is_pinned"], false),
                _ => assert_eq!(undo["chat_list"]["@type"], previous_list),
            }
            ok(&acc, &s, undo);
        }
        assert!(s.redo());
        acc.drain(s.world());
        let redos = requests_of(&td, kind)[6..].to_vec();
        assert_eq!(redos.len(), 3);
        for mut redo in redos {
            let mut original = originals.iter().find(|v| v["chat_id"] == redo["chat_id"]).unwrap().clone();
            original.as_object_mut().unwrap().remove("@extra");
            redo.as_object_mut().unwrap().remove("@extra");
            assert_eq!(redo, original);
        }
        assert_eq!(s.history().rows().0.len(), before + 1);
    }
}

#[test]
fn refusal_feedback_is_once_per_gesture_and_keeps_batch_marks() {
    for cause in ["lease", "connection", "busy", "disconnected"] {
        for batch in [false, true] {
            let mut s = session();
            let list = open_root(&mut s, Chats::id());
            with_chats(&s, list, |c| c.list_mut().marks_mut().extend(
                [VERA, STELAXIS, FAMILY].map(|peer| format!("{peer}:0"))));
            let rt = runtime::of(s.store());
            let inbox = rt.connect();
            let request = requests::set_chat_muted(VERA, true);
            match cause {
                "lease" => s.store().set_writable(false),
                "connection" => rt.set_connection_error(Some("sign in again".into())),
                "busy" => { history::command(&mut s, &request).unwrap(); receive(&inbox); }
                _ => { drop(inbox); }
            }
            s.take_notes();
            let before = s.history().rows();
            if batch { verb(&mut s, list, "telegram.mute"); }
            else { assert!(!told(&mut s, &request, "mute")); }
            assert_eq!(s.notes().len(), 1, "{cause}, batch={batch}");
            let reason = match cause {
                "lease" => "another device holds the lease",
                "connection" => "sign in again",
                "busy" => "wait for the previous Telegram change",
                _ => "Telegram is not connected",
            };
            assert!(s.notes()[0].err && s.notes()[0].msg.contains(reason), "{:?}", s.notes()[0].msg);
            assert_eq!(s.history().rows(), before, "a refused batch records no partial gesture");
            assert_eq!(with_chats(&s, list, |c| c.list_mut().marks().len()), 3);
            s.store().set_writable(true);
        }
    }
}

#[test]
fn refused_sends_edits_and_both_delete_paths_keep_input_and_report_once() {
    for action in ["send", "files", "edit", "delete", "line delete"] {
        let mut s = session();
        let chat = open_root(&mut s, Chat::id(VERA));
        let m = model::history(s.store(), VERA).iter().find(|m| m.out && !m.service).unwrap().clone();
        with_chat(&s, chat, |c| {
            c.set_cursor((c.peer(), m.id));
            c.set_draft("keep these words");
            if action == "files" { c.carry(&["/tmp/attachment.png".into()]); }
        });
        let card = open_root(&mut s, Line::id(m.chat, m.id));
        if action == "edit" {
            verb(&mut s, chat, "telegram.edit");
            with_chat(&s, chat, |c| c.typed("keep this edit"));
        }
        let rt = runtime::of(s.store());
        let _inbox = rt.connect();
        rt.set_connection_error(Some("sign in again".into()));
        s.take_notes();
        let before = s.history().rows();
        match action {
            "delete" => verb(&mut s, chat, "telegram.delete"),
            "line delete" => verb(&mut s, card, "telegram.delete"),
            _ => send(&mut s, chat),
        }
        assert_eq!(s.notes().len(), 1, "{action}");
        assert!(s.notes()[0].msg.contains("sign in again"), "{action}");
        assert_eq!(s.history().rows(), before);
        assert_eq!(model::line(s.store(), m.chat, m.id).unwrap(), m);
        with_chat(&s, chat, |c| {
            assert_eq!(c.field_text(), if action == "edit" { "keep this edit" } else { "keep these words" });
            if action == "files" { assert_eq!(c.carrying().len(), 1); }
        });
    }
}

#[test]
fn a_refused_reaction_is_reported_only_by_the_picker() {
    let mut s = session();
    let m = model::history(s.store(), VERA).iter().find(|m| !m.service).unwrap().clone();
    let card = open_root(&mut s, Line::id(m.chat, m.id));
    let td = FakeTd::new();
    let acc = connected_reaction_account(&s, td.clone());
    acc.drain(s.world());
    verb(&mut s, card, "telegram.react");
    acc.drain(s.world());
    offer_reactions(&acc, &s, &last_reaction_request(&td, "getMessageAvailableReactions"), &["👍"]);
    poll_reactions(&mut s, card);
    s.store().set_writable(false);
    s.take_notes();
    verb(&mut s, card, "telegram.reaction_0");
    assert!(poll_reactions(&mut s, card));
    assert!(!poll_reactions(&mut s, card));
    assert_eq!(s.notes().len(), 1);
    assert!(s.notes()[0].msg.contains("another device holds the lease"));
    assert!(requests_of(&td, "addMessageReaction").is_empty());
    s.store().set_writable(true);
}

fn first_reaction(s: &mut Session, td: &FakeTd, acc: &sync::Account<FakeTd>) -> model::Msg {
    let m = model::history(s.store(), VERA).iter().find(|m| !m.service).unwrap().clone();
    history::command(s, &requests::add_message_reaction(m.chat, m.id, "👍", 1)).unwrap();
    acc.drain(s.world());
    snapshot(acc, s, td, json!({"interaction_info": null}));
    assert!(requests_of(td, "addMessageReaction").is_empty());
    m
}

fn confirmed_reactions(acc: &sync::Account<FakeTd>, s: &Session, td: &FakeTd, m: &model::Msg, info: Value) {
    let read = last_reaction_request(td, "searchChatMessages");
    assert_eq!(read["from_message_id"], m.id);
    acc.on_update(s.world(), &json!({"@type": "foundChatMessages", "@extra": read["@extra"], "messages": [
        {"@type": "message", "chat_id": m.chat, "id": m.id, "interaction_info": info}
    ]}).to_string());
}

#[test]
fn a_first_reaction_is_undoable_after_metadata_and_a_confirming_server_read() {
    for info in [Value::Null, json!({}), json!({"view_count": 1, "reactions": null})] {
        let mut s = session();
        let td = FakeTd::new();
        let acc = connected_reaction_account(&s, td.clone());
        acc.drain(s.world());
        let m = first_reaction(&mut s, &td, &acc);
        let metadata = last_reaction_request(&td, "getMessageAvailableReactions");
        offer_reactions(&acc, &s, &metadata, &["👍"]);
        // A duplicate of the original cache response cannot skip confirmation.
        snapshot(&acc, &s, &td, json!({"interaction_info": {"reactions": []}}));
        assert!(requests_of(&td, "addMessageReaction").is_empty());
        confirmed_reactions(&acc, &s, &td, &m, info);
        assert_eq!(requests_of(&td, "addMessageReaction").len(), 1);
        ok(&acc, &s, &last_reaction_request(&td, "addMessageReaction"));
        assert!(s.undo());
        acc.drain(s.world());
        let remove = last_reaction_request(&td, "removeMessageReaction");
        assert_eq!(remove["reaction_type"]["emoji"], "👍");
        ok(&acc, &s, &remove);
        assert!(s.redo());
        acc.drain(s.world());
        assert_eq!(requests_of(&td, "addMessageReaction").len(), 2);
        assert_eq!(model::line(s.store(), m.chat, m.id).unwrap(), m, "snapshots never re-project stale bodies");
    }
}

#[test]
fn a_confirmation_can_reveal_a_reaction_that_was_already_ours() {
    let mut s = session();
    let td = FakeTd::new();
    let acc = connected_reaction_account(&s, td.clone());
    acc.drain(s.world());
    let m = first_reaction(&mut s, &td, &acc);
    offer_reactions(&acc, &s, &last_reaction_request(&td, "getMessageAvailableReactions"), &["👍"]);
    confirmed_reactions(&acc, &s, &td, &m, json!({"reactions": {"reactions": [
        {"type": {"@type": "reactionTypeEmoji", "emoji": "👍"}, "is_chosen": true}
    ]}}));
    ok(&acc, &s, &last_reaction_request(&td, "addMessageReaction"));
    assert!(s.undo());
    acc.drain(s.world());
    assert!(requests_of(&td, "removeMessageReaction").is_empty());
}

#[test]
fn errors_missing_messages_and_malformed_confirmation_never_mean_empty_reactions() {
    for reply in [json!({"@type": "error", "code": 400, "message": "MESSAGE_NOT_FOUND"}),
        json!({"@type": "foundChatMessages", "messages": []}),
        json!({"@type": "foundChatMessages", "messages": [{"@type": "message", "chat_id": 999, "id": 999}]}),
        json!({"@type": "availableReactions", "top_reactions": []})] {
        let mut s = session();
        let td = FakeTd::new();
        let acc = connected_reaction_account(&s, td.clone());
        acc.drain(s.world());
        first_reaction(&mut s, &td, &acc);
        let kind = if reply["@type"] == "availableReactions" { "getMessageAvailableReactions" } else {
            offer_reactions(&acc, &s, &last_reaction_request(&td, "getMessageAvailableReactions"), &["👍"]);
            "searchChatMessages"
        };
        let mut reply = reply;
        reply["@extra"] = last_reaction_request(&td, kind)["@extra"].clone();
        acc.on_update(s.world(), &reply.to_string());
        ok(&acc, &s, &last_reaction_request(&td, "addMessageReaction"));
        s.undo();
        acc.drain(s.world());
        assert_eq!(s.history().rows().0.last().unwrap().state, "expired");
        assert!(requests_of(&td, "removeMessageReaction").is_empty());
    }
}

#[test]
fn reaction_confirmation_restarts_across_metadata_changes_and_newer_counts() {
    for metadata in [false, true] {
        let mut s = session();
        let td = FakeTd::new();
        let acc = connected_reaction_account(&s, td.clone());
        acc.drain(s.world());
        let m = first_reaction(&mut s, &td, &acc);
        offer_reactions(&acc, &s, &last_reaction_request(&td, "getMessageAvailableReactions"), &["👍"]);
        let update = if metadata { json!({"@type": "updateChatAvailableReactions", "chat_id": m.chat}) }
            else { json!({"@type": "updateMessageInteractionInfo", "chat_id": m.chat, "message_id": m.id,
                "interaction_info": {"reactions": {"reactions": []}}}) };
        acc.on_update(s.world(), &update.to_string());
        confirmed_reactions(&acc, &s, &td, &m, Value::Null);
        assert!(requests_of(&td, "addMessageReaction").is_empty());
        assert_eq!(requests_of(&td, "getMessageAvailableReactions").len(), 2);
        offer_reactions(&acc, &s, &last_reaction_request(&td, "getMessageAvailableReactions"), &["👍"]);
        confirmed_reactions(&acc, &s, &td, &m, Value::Null);
        ok(&acc, &s, &last_reaction_request(&td, "addMessageReaction"));
        assert!(s.undo());
        acc.drain(s.world());
        assert_eq!(requests_of(&td, "removeMessageReaction").len(), 1);
    }
}

#[test]
fn reaction_preparation_timeouts_release_the_picker_and_ignore_late_confirmation() {
    use kernel::app::Env;
    use kernel::caps::{ClockSource, FakeClock};
    let clock = FakeClock::at(virtual_epoch());
    let mut s = Session::fake_with(APPS, &Env { clock: ClockSource::Virtual(clock.clone()), ..Env::default() });
    let m = model::history(s.store(), VERA).iter().find(|m| !m.service).unwrap().clone();
    let card = open_root(&mut s, Line::id(m.chat, m.id));
    let td = FakeTd::new();
    let acc = connected_reaction_account(&s, td.clone());
    acc.drain(s.world());
    verb(&mut s, card, "telegram.react");
    acc.drain(s.world());
    offer_reactions(&acc, &s, &last_reaction_request(&td, "getMessageAvailableReactions"), &["👍"]);
    poll_reactions(&mut s, card);
    verb(&mut s, card, "telegram.reaction_0");
    acc.drain(s.world());
    snapshot(&acc, &s, &td, json!({"interaction_info": null}));
    offer_reactions(&acc, &s, &last_reaction_request(&td, "getMessageAvailableReactions"), &["👍"]);
    s.take_notes();
    clock.advance(31.0);
    acc.drain(s.world());
    assert!(poll_reactions(&mut s, card));
    assert!(verb_ids(&s, card).contains(&"telegram.reactions_retry"));
    assert_eq!(s.notes().len(), 1);
    assert!(s.notes()[0].err);
    confirmed_reactions(&acc, &s, &td, &m, Value::Null);
    acc.drain(s.world());
    assert!(requests_of(&td, "addMessageReaction").is_empty());
}
