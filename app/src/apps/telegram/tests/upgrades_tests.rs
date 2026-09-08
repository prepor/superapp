use super::*;
use crate::apps::telegram::upgrades;
use serde_json::{json, Value};

const OLD: i64 = -123;
const NEW: i64 = -1_000_000_000_456;

fn histories(s: &Session, linked: bool) {
    s.store().write(move |c| {
        for chat in [OLD, NEW] {
            c.execute("INSERT INTO tg_peer(id, kind, name) VALUES(?1, 'group', 'Upgraded group')", [chat])?;
            model::ensure_chat_tx(c, chat)?;
        }
        for (chat, id, date, text, reply, source) in [
            (OLD, 42, 1, "before upgrade", None, None),
            (OLD, 43, 2, "old reply", Some(42), None),
            (NEW, 42, 3, "after upgrade", None, None),
            (NEW, 43, 4, "new reply to old message", Some(42), Some(OLD)),
        ] {
            c.execute("INSERT INTO tg_message(chat,id,date,text,out,reply_to,reply_chat)
                VALUES(?1,?2,?3,?4,1,?5,?6)", rusqlite::params![chat,id,date,text,reply,source])?;
        }
        if linked { upgrades::record(c, OLD, NEW)?; }
        Ok(())
    }).unwrap();
}

#[test]
fn linking_cached_histories_refreshes_open_transcripts_without_colliding_ids() {
    let mut s = session();
    histories(&s, false);
    let slot = open_root(&mut s, Chat::id(NEW));
    let before = with_chat(&s, slot, |c| c.snapshot(s.now()));
    assert_eq!(before.history.len(), 2);
    s.store().write(|c| upgrades::record(c, OLD, NEW)).unwrap();
    let after = with_chat(&s, slot, |c| c.snapshot(s.now()));
    assert_eq!(after.history.iter().map(model::Msg::key).collect::<Vec<_>>(),
        [(OLD, 42), (OLD, 43), (NEW, 42), (NEW, 43)]);
    assert_eq!(before.history.len(), 2, "a draw retains its immutable snapshot");
    assert_ne!(after.row_index((OLD, 42)), after.row_index((NEW, 42)));
    assert_eq!(after.message((OLD, 42)).unwrap().text, "before upgrade");
    assert_eq!(after.message((NEW, 42)).unwrap().text, "after upgrade");
    for key in [(OLD, 43), (NEW, 43)] {
        assert_eq!(after.message(key).unwrap().reply_text, "before upgrade");
    }
    assert_eq!(model::line(s.store(), NEW, 42).unwrap().text, "after upgrade");
    assert_eq!(model::history(s.store(), OLD).len(), 2, "the original identity stays readable");
    assert!(model::history_in(s.store(), NEW, 7).is_empty(), "old messages do not leak into forum topics");

    let rows = rows_of(&after.history, Some((NEW, 42)), s.now());
    let divider = rows.iter().position(|r| *r == Row::Unread).unwrap();
    assert_eq!(rows[divider + 1].msg().unwrap().key(), (NEW, 42));
}

#[test]
fn replies_cards_and_deletion_keep_the_original_message_identity() {
    let mut s = session();
    histories(&s, true);
    let slot = open_root(&mut s, Chat::id(NEW));
    with_chat(&s, slot, |c| c.set_cursor((NEW, 43)));
    verb(&mut s, slot, "telegram.original");
    assert_eq!(with_chat(&s, slot, |c| c.cursor()), Some((OLD, 42)));
    verb(&mut s, slot, "telegram.line");
    let card = s.joined_child(slot).unwrap();
    assert_eq!(s.panel(card).unwrap().borrow().id(), &Line::id(OLD, 42));
    verb(&mut s, card, "telegram.reply");
    assert!(with_chat(&s, slot, |c| c.reply_line(s.now()).unwrap()).contains("before upgrade"));
    verb(&mut s, slot, "telegram.back");
    assert_eq!(with_chat(&s, slot, |c| c.cursor()), Some((NEW, 43)));

    with_chat(&s, slot, |c| { c.set_cursor((OLD, 42)); c.toggle_mark(); });
    with_chat(&s, slot, |c| { c.set_cursor((NEW, 42)); c.toggle_mark(); });
    assert_eq!(with_chat(&s, slot, |c| c.marks().len()), 2);
    verb(&mut s, slot, "telegram.clear");
    with_chat(&s, slot, |c| c.set_cursor((OLD, 42)));
    verb(&mut s, slot, "telegram.delete");
    assert!(model::line(s.store(), OLD, 42).is_none());
    assert!(model::line(s.store(), NEW, 42).is_some(), "deleting an old line cannot delete the new line with that id");
}

#[test]
fn replying_to_an_old_message_sends_to_the_new_group_with_an_external_reply() {
    let mut s = session();
    histories(&s, true);
    let slot = open_root(&mut s, Chat::id(NEW));
    let inbox = runtime::of(s.store()).connect();
    let td = FakeTd::new();
    let _acc = connected_reaction_account(&s, td);
    with_chat(&s, slot, |c| { c.reply((OLD, 42)); c.set_draft("answer"); });
    send(&mut s, slot);
    let request: Value = serde_json::from_str(&inbox.try_recv().unwrap()).unwrap();
    assert_eq!(request["chat_id"], NEW);
    assert_eq!(request["reply_to"], json!({"@type": "inputMessageReplyToExternalMessage",
        "chat_id": OLD, "message_id": 42}));
}

#[test]
fn forwarding_a_selection_across_the_upgrade_keeps_both_source_chats() {
    let mut s = session();
    histories(&s, true);
    let slot = open_root(&mut s, Chat::id(NEW));
    with_chat(&s, slot, |c| {
        c.set_cursor((OLD, 42)); c.toggle_mark();
        c.set_cursor((NEW, 42)); c.toggle_mark();
    });
    verb(&mut s, slot, "telegram.forward");
    let pending = runtime::of(s.store()).pending_forward().unwrap();
    let groups = model::message_groups(pending.messages);
    assert_eq!(groups[&OLD], [42]);
    assert_eq!(groups[&NEW], [42]);
}

#[test]
fn forwarding_across_the_upgrade_sends_old_messages_first() {
    let mut s = session();
    histories(&s, true);
    let inbox = runtime::of(s.store()).connect();
    let slot = open_root(&mut s, Chat::id(NEW));
    with_chat(&s, slot, |c| {
        c.set_cursor((NEW, 42)); c.toggle_mark();
        c.set_cursor((OLD, 42)); c.toggle_mark();
    });
    verb(&mut s, slot, "telegram.forward");
    let list = s.joined_child(slot).unwrap();
    with_chats(&s, list, |c| { c.go(0); });
    verb(&mut s, list, "telegram.forward_here");
    let requests: Vec<Value> = std::iter::from_fn(|| inbox.try_recv().ok())
        .map(|raw| serde_json::from_str(&raw).unwrap())
        .filter(|v: &Value| v["@type"] == "forwardMessages").collect();
    assert_eq!(requests.len(), 2);
    assert_eq!(requests[0]["from_chat_id"], OLD);
    assert_eq!(requests[1]["from_chat_id"], NEW);
    assert_eq!(requests[0]["message_ids"], json!([42]));
    assert_eq!(requests[1]["message_ids"], json!([42]));
    assert!(runtime::of(s.store()).pending_forward().is_none());
}

#[test]
fn an_agent_cannot_confuse_an_old_reply_with_the_same_id_in_the_new_group() {
    let mut s = session();
    histories(&s, true);
    let inbox = runtime::of(s.store()).connect();
    let slot = open_root(&mut s, Chat::id(NEW));
    with_chat(&s, slot, |c| { c.reply((OLD, 42)); c.set_draft("answer"); });
    let input = json!({"slot": slot, "chat": NEW, "text": "answer", "reply_to": 42});
    for name in ["telegram.draft", "telegram.send"] {
        let tool = s.apps().tool(name).unwrap().clone();
        assert!((tool.run)(&mut s, &input).is_err(), "{name} must compare the source chat too");
    }
    assert!(inbox.try_recv().is_err());
    assert_eq!(with_chat(&s, slot, |c| c.reply_key()), Some((OLD, 42)));
}
