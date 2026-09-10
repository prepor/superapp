use super::*;
use crate::apps::telegram::{seed::BERLIN, topics};
use serde_json::json;

/// History and incoming updates can add unread lines after the opening read.
fn arrive(s: &Session, after: i64) {
    let date = s.now();
    s.store().write(move |c| {
        for id in [after + 1, after + 2] {
            c.execute("INSERT INTO tg_message(chat, id, date, text)
                VALUES(?1, ?2, ?3, 'arrived after opening')", rusqlite::params![STELAXIS, id, date])?;
        }
        c.execute("UPDATE tg_chat SET unread = 2, last_read = ?2 WHERE peer = ?1",
            [STELAXIS, after])?;
        Ok(())
    }).unwrap();
}

#[test]
fn scrolling_reads_ordinary_messages_loaded_after_the_chat_opens() {
    for initially_empty in [false, true] {
        let mut s = session();
        let after = model::history(s.store(), STELAXIS).last().unwrap().id;
        if initially_empty {
            s.store().write(|c| {
                c.execute("DELETE FROM tg_message WHERE chat = ?1", [STELAXIS]).map(|_| ())
            }).unwrap();
        }
        let reader = open_root(&mut s, Chat::id(STELAXIS));
        arrive(&s, after);
        with_chat(&s, reader, |c| c.view_messages(&[], s.now()));
        assert_eq!(unread(&s, STELAXIS).0, 2, "loading alone reads nothing");

        with_chat(&s, reader, |c| c.view_messages(&[(c.peer(), after + 1)], s.now()));
        assert_eq!(unread(&s, STELAXIS).0, 1, "the newer unseen line stays unread");
        assert_eq!(model::peer(s.store(), STELAXIS).unwrap().last_read, Some(after + 1));
        with_chat(&s, reader, |c| c.view_messages(&[(c.peer(), after + 1)], s.now() + 6.0));
        assert_eq!(unread(&s, STELAXIS).0, 1, "a repeated view cannot decrement twice");

        with_chat(&s, reader, |c| c.view_messages(&[(c.peer(), after + 2)], s.now() + 7.0));
        assert_eq!(unread(&s, STELAXIS).0, 0, "reaching the newest line clears the badge");
        assert_eq!(model::peer(s.store(), STELAXIS).unwrap().last_read, Some(after + 2));
    }
}

#[test]
fn viewing_trailing_mentions_advances_the_inbox_but_preserves_unseen_mentions() {
    let mut s = session();
    let after = add_trailing_mentions(&s);
    let list = open_root(&mut s, Chats::id());
    go(&mut s, Nav::Preview { from: list, id: Chat::id(STELAXIS) });
    let reader = s.joined_child(list).unwrap();
    assert_eq!(unread(&s, STELAXIS).0, 2);
    assert_eq!(model::reply_count(s.store()), 4);
    assert!(!with_chat(&s, reader, |c| c.view_messages(&[(c.peer(), after + 1)], s.now())),
        "fixture reads need no retry timer");
    assert_eq!(unread(&s, STELAXIS).0, 1);
    assert_eq!(model::reply_count(s.store()), 3);
    with_chat(&s, reader, |c| c.view_messages(&[(c.peer(), after + 2)], s.now() + 1.0));
    assert_eq!(unread(&s, STELAXIS).0, 0);
    assert_eq!(model::reply_count(s.store()), 2, "older unseen mentions keep their badges");

    let mentions: Vec<_> = model::history(s.store(), STELAXIS).iter()
        .filter(|m| m.unread_mention).map(model::Msg::key).collect();
    assert!(mentions.iter().all(|(_, id)| *id < after));
    with_chat(&s, reader, |c| c.view_messages(&mentions, s.now() + 2.0));
    assert_eq!(model::reply_count(s.store()), 0, "mentions behind the read cursor can still be read");
    assert_eq!(model::peer(s.store(), STELAXIS).unwrap().last_read, Some(after + 2));
}

#[test]
fn live_ordinary_views_retry_until_telegram_acknowledges_the_read() {
    let mut s = session();
    let reader = open_root(&mut s, Chat::id(STELAXIS));
    let after = model::history(s.store(), STELAXIS).last().unwrap().id;
    let inbox = runtime::of(s.store()).connect();
    arrive(&s, after);
    assert!(with_chat(&s, reader, |c| c.view_messages(&[(c.peer(), after + 1)], s.now())));
    let request: serde_json::Value = serde_json::from_str(&inbox.try_recv().unwrap()).unwrap();
    assert_eq!(request["@type"], "viewMessages");
    assert_eq!(request["chat_id"], STELAXIS);
    assert_eq!(request["message_ids"], json!([after + 1]));
    assert_eq!(request["force_read"], true);
    assert_eq!(unread(&s, STELAXIS).0, 2, "queuing is not acknowledgment");
    assert!(with_chat(&s, reader, |c| c.view_messages(&[(c.peer(), after + 1)], s.now() + 1.0)),
        "deduplicated attempts still need a scheduled retry");
    assert!(inbox.try_recv().is_err(), "pending views are deduplicated");
    with_chat(&s, reader, |c| c.view_messages(&[(c.peer(), after + 1)], s.now() + 6.0));
    assert!(inbox.try_recv().is_ok(), "a lost receipt can retry without scrolling");

    let acc = account();
    acc.on_update(s.world(), &json!({"@type": "updateChatReadInbox", "chat_id": STELAXIS,
        "last_read_inbox_message_id": after + 1, "unread_count": 1}).to_string());
    assert!(!with_chat(&s, reader, |c| c.view_messages(&[(c.peer(), after + 1)], s.now() + 12.0)));
    assert!(inbox.try_recv().is_err(), "acknowledged reads stop retrying");
    assert_eq!(unread(&s, STELAXIS).0, 1);

    with_chat(&s, reader, |c| c.view_messages(&[(c.peer(), after + 1), (c.peer(), after + 2)], s.now() + 13.0));
    let request: serde_json::Value = serde_json::from_str(&inbox.try_recv().unwrap()).unwrap();
    assert_eq!(request["message_ids"], json!([after + 2]));
    acc.on_update(s.world(), &json!({"@type": "updateChatReadInbox", "chat_id": STELAXIS,
        "last_read_inbox_message_id": after + 2, "unread_count": 0}).to_string());
    assert_eq!(unread(&s, STELAXIS).0, 0);
    with_chat(&s, reader, |c| c.view_messages(&[(c.peer(), after + 1), (c.peer(), after + 2)], s.now() + 20.0));
    assert!(inbox.try_recv().is_err());
}

#[test]
fn viewing_a_topic_advances_only_its_own_read_position() {
    for live in [false, true] {
        let mut s = session();
        let reader = open_root(&mut s, Chat::topic_at(BERLIN, 2, 2000));
        let parent = model::peer(s.store(), BERLIN).unwrap();
        let inbox = live.then(|| runtime::of(s.store()).connect());
        // The other topic's id is deliberately included; it cannot be read
        // through this transcript even if the widget hands over stale rows.
        with_chat(&s, reader, |c| c.view_messages(&[(c.peer(), 2000), (c.peer(), 4000)], s.now()));
        if let Some(inbox) = inbox {
            let request: serde_json::Value = serde_json::from_str(&inbox.try_recv().unwrap()).unwrap();
            assert_eq!(request["@type"], "viewMessages");
            assert_eq!(request["chat_id"], BERLIN);
            assert_eq!(request["message_ids"], json!([2000]));
            assert_eq!(request["source"]["@type"], "messageSourceForumTopicHistory");
            assert_eq!(topics::get(s.store(), BERLIN, 2).unwrap().unread, 1);
        } else {
            let topic = topics::get(s.store(), BERLIN, 2).unwrap();
            assert_eq!((topic.unread, topic.last_read), (0, Some(2000)));
        }
        assert_eq!(topics::get(s.store(), BERLIN, 4).unwrap().unread, 1);
        let unchanged = model::peer(s.store(), BERLIN).unwrap();
        assert_eq!((unchanged.unread, unchanged.last_read), (parent.unread, parent.last_read));
    }
}

#[test]
fn pending_failed_and_service_rows_cannot_advance_the_read_position() {
    let mut s = session();
    let reader = open_root(&mut s, Chat::id(STELAXIS));
    let after = model::history(s.store(), STELAXIS).last().unwrap().id;
    arrive(&s, after);
    s.store().write(move |c| {
        for (offset, state, service) in [(3, "sending", false), (4, "failed", false), (5, "", true)] {
            c.execute("INSERT INTO tg_message(chat, id, date, text, state, service)
                VALUES(?1, ?2, ?2, 'not a delivered message', ?3, ?4)",
                rusqlite::params![STELAXIS, after + offset, state, service])?;
        }
        Ok(())
    }).unwrap();
    let inbox = runtime::of(s.store()).connect();
    with_chat(&s, reader, |c| c.view_messages(&[(c.peer(), after + 3), (c.peer(), after + 4), (c.peer(), after + 5), (c.peer(), -1)], s.now()));
    assert!(inbox.try_recv().is_err());
    assert_eq!(unread(&s, STELAXIS).0, 2);
    assert_eq!(model::peer(s.store(), STELAXIS).unwrap().last_read, Some(after));
}
