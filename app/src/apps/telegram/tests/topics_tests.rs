use super::*;
use crate::apps::telegram::{panels::Topics, seed::BERLIN, topics};

fn with_topics<T>(s: &Session, slot: SlotId, f: impl FnOnce(&mut Topics) -> T) -> T {
    let panel = s.panel(slot).unwrap();
    let mut p = panel.borrow_mut();
    f(p.as_any().downcast_mut::<Topics>().unwrap())
}

fn toggle(s: &mut Session, slot: SlotId, id: i64) {
    let panel = s.panel(slot).unwrap();
    panel
        .borrow_mut()
        .as_any()
        .downcast_mut::<Topics>()
        .unwrap()
        .toggle(id, s);
    s.settle();
}

fn selected_topics(s: &Session, chat: i64) -> Vec<i64> {
    let mut ids: Vec<_> = topics::list(s.store(), chat)
        .iter()
        .filter(|t| t.selected)
        .map(|t| t.id)
        .collect();
    ids.sort_unstable();
    ids
}

#[test]
fn bulk_topic_visibility_round_trips_a_mixed_selection() {
    for (verb_id, shown) in [
        ("telegram.show_topics", vec![1, 2, 3, 4]),
        ("telegram.hide_topics", vec![]),
    ] {
        let mut s = session();
        s.store()
            .write(|c| topics::select_tx(c, BERLIN, &[2, 4], true))
            .unwrap();
        let chats = open_root(&mut s, Chats::id());
        let picker = open_root(&mut s, Topics::id(BERLIN));
        let inbox = runtime::of(s.store()).connect();
        let before = s.history().head();

        verb(&mut s, picker, verb_id);
        assert_eq!(selected_topics(&s, BERLIN), shown);
        assert_eq!(
            with_chats(&s, chats, |p| p.rows(0, 50))
                .iter()
                .filter(|r| r.peer == BERLIN)
                .count(),
            shown.len()
        );
        let action = s.history().head();
        assert_ne!(action, before);
        verb(&mut s, picker, verb_id);
        assert_eq!(
            s.history().head(),
            action,
            "an unchanged selection is not an action"
        );

        s.store()
            .write(|c| {
                c.execute(
                    "UPDATE tg_topic SET draft = 'new draft', unread = 7, pinned = 1,
                     muted = 1, mute_default = 0 WHERE chat = ?1 AND id = 2",
                    [BERLIN],
                )?;
                Ok(())
            })
            .unwrap();
        let after = topics::list(s.store(), BERLIN);
        let mut restored = after.as_ref().clone();
        for topic in &mut restored {
            topic.selected = [2, 4].contains(&topic.id);
        }
        assert!(s.undo());
        assert_eq!(
            s.history().head(),
            before,
            "one undo restores the whole selection"
        );
        assert_eq!(topics::list(s.store(), BERLIN).as_ref(), &restored);
        assert_eq!(
            with_chats(&s, chats, |p| p.rows(0, 50))
                .iter()
                .filter(|r| r.peer == BERLIN)
                .count(),
            2
        );
        assert!(s.redo());
        assert_eq!(topics::list(s.store(), BERLIN), after);
        assert!(
            inbox.try_iter().next().is_none(),
            "selection stays local even when connected"
        );
    }
}

#[test]
fn filtered_topic_selection_undo_keeps_the_original_matches() {
    for (verb_id, mut shown) in [
        ("telegram.show_topics", vec![2, 3, 4]),
        ("telegram.hide_topics", vec![4]),
    ] {
        let mut s = session();
        s.store()
            .write(|c| {
                topics::select_tx(c, BERLIN, &[2, 4], true)?;
                c.execute(
                    "UPDATE tg_topic SET name = 'Match ' || id WHERE chat = ?1 AND id IN (2, 3)",
                    [BERLIN],
                )?;
                c.execute(
                    "INSERT INTO tg_topic(chat, id, name) VALUES(?1, 3, 'Another group')",
                    [HIKE],
                )?;
                Ok(())
            })
            .unwrap();
        let picker = open_root(&mut s, Topics::id(BERLIN));
        with_topics(&s, picker, |p| p.set_filter("match".into()));
        verb(&mut s, picker, verb_id);
        assert_eq!(selected_topics(&s, BERLIN), shown);

        with_topics(&s, picker, |p| p.set_filter("cycling".into()));
        s.store()
            .write(|c| {
                c.execute(
                    "INSERT INTO tg_topic(chat, id, name, selected) VALUES(?1, 5, 'Match 5', 1)",
                    [BERLIN],
                )?;
                Ok(())
            })
            .unwrap();
        assert!(s.undo());
        assert_eq!(selected_topics(&s, BERLIN), vec![2, 4, 5]);
        assert!(selected_topics(&s, HIKE).is_empty());
        assert!(s.redo());
        shown.push(5);
        assert_eq!(selected_topics(&s, BERLIN), shown);
        assert!(selected_topics(&s, HIKE).is_empty());
    }
}

#[test]
fn topic_toggles_undo_individually_and_empty_matches_leave_history_alone() {
    let mut s = session();
    let picker = open_root(&mut s, Topics::id(BERLIN));
    let before = s.history().head();
    with_topics(&s, picker, |p| p.set_filter("no matches".into()));
    verb(&mut s, picker, "telegram.show_topics");
    verb(&mut s, picker, "telegram.hide_topics");
    assert_eq!(s.history().head(), before);

    with_topics(&s, picker, |p| p.set_filter(String::new()));
    toggle(&mut s, picker, 2);
    toggle(&mut s, picker, 4);
    assert!(s.undo());
    assert_eq!(selected_topics(&s, BERLIN), vec![2]);
    assert!(s.undo());
    assert!(selected_topics(&s, BERLIN).is_empty());
    assert!(s.redo());
    assert_eq!(selected_topics(&s, BERLIN), vec![2]);
    assert!(s.redo());
    assert_eq!(selected_topics(&s, BERLIN), vec![2, 4]);
}

#[test]
fn selection_replaces_the_forum_with_independent_chat_rows() {
    let mut s = session();
    let chats = open_root(&mut s, Chats::id());
    assert!(with_chats(&s, chats, |p| p.rows(0, 50))
        .iter()
        .all(|r| r.peer != BERLIN));
    verb(&mut s, chats, "telegram.topics");
    let groups = s.joined_child(chats).unwrap();
    let rows = with_chats(&s, groups, |p| p.rows(0, 50));
    assert_eq!(rows.len(), 1);
    assert_eq!(Chats::target(&rows[0]), Topics::id(BERLIN));
    let picker = open_root(&mut s, Topics::id(BERLIN));
    toggle(&mut s, picker, 2);
    toggle(&mut s, picker, 4);
    let rows = with_chats(&s, chats, |p| p.rows(0, 50));
    let topics: Vec<_> = rows.iter().filter(|r| r.peer == BERLIN).collect();
    assert_eq!(topics.len(), 2);
    assert_ne!(topics[0].key(), topics[1].key());
    let meetup = topics.iter().find(|r| r.topic == 2).unwrap();
    assert_eq!(meetup.title, "Meetups · Вастрик.Берлин");
    assert_eq!(meetup.last_text, "Coffee at the canal on Saturday?");
    assert_eq!(meetup.unread, 1);
    assert_eq!(Chats::target(meetup), Chat::topic(BERLIN, 2));
    toggle(&mut s, picker, 2);
    assert!(with_chats(&s, chats, |p| p.rows(0, 50))
        .iter()
        .all(|r| r.peer != BERLIN || r.topic == 4));
    toggle(&mut s, picker, 4);
    assert_eq!(
        with_chats(&s, groups, |p| p.rows(0, 50)).len(),
        1,
        "the group remains discoverable"
    );
}

#[test]
fn empty_topic_groups_show_the_local_connection_failure_over_shared_session_state() {
    let mut s = session();
    s.store()
        .write(|c| c.execute("UPDATE tg_peer SET is_forum = 0", []).map(|_| ()))
        .unwrap();
    let groups = open_root(&mut s, Chats::forums());
    assert!(with_chats(&s, groups, |p| p.rows(0, 50)).is_empty());
    let runtime = runtime::of(s.store());
    runtime.set_list_syncing(true);
    assert_eq!(
        with_chats(&s, groups, |p| p.empty_line("")),
        "loading groups with topics…"
    );

    let error = "Telegram is open in another app window\nclose it and restart this app";
    runtime.set_connection_error(Some(error.into()));
    // Another process can update the shared account row, but its connection
    // cannot make this process's failed initialization successful.
    set_session(&s, "ready", None, None);
    let signin = open_root(&mut s, SignIn::id());
    assert_eq!(with_chats(&s, groups, |p| p.empty_line("")), error);
    with_signin(&s, signin, |p| {
        assert_eq!(p.line(), error);
        assert!(p.note().is_none());
        assert!(p.field_kind().is_none());
    });
    let picker = open_root(&mut s, Topics::id(BERLIN));
    assert_eq!(with_topics(&s, picker, |p| p.status()), error);

    runtime.set_connection_error(None);
    assert_eq!(
        with_chats(&s, groups, |p| p.empty_line("")),
        "no groups with topics yet"
    );
    assert!(with_signin(&s, signin, |p| p.note()).is_some());
}

#[test]
fn a_topic_list_timeout_can_refresh_without_old_errors_overriding_it() {
    let mut s = session();
    let picker = open_root(&mut s, Topics::id(BERLIN));
    let rt = runtime::of(s.store());
    let _inbox = rt.connect();
    rt.operations
        .track(&requests::get_forum_topics(BERLIN, 10, 20, 2));
    rt.topics_loaded(BERLIN, Ok(true));
    rt.operations.expire(
        s.store(),
        std::time::Instant::now() + std::time::Duration::from_secs(121),
    );
    TELEGRAM.poll(&mut s);
    assert!(with_topics(&s, picker, |p| p.status()).contains("refresh to try again"));

    verb(&mut s, picker, "telegram.refresh_topics");
    rt.operations.changed();
    TELEGRAM.poll(&mut s);
    assert_eq!(rt.topics_status(BERLIN), Ok(true), "the retry is queued");
    assert_eq!(rt.take_wanted().topic_lists, vec![BERLIN]);
    let next = rt
        .operations
        .track(&requests::get_forum_topics(BERLIN, 0, 0, 0));
    TELEGRAM.poll(&mut s);
    assert_eq!(
        rt.topics_status(BERLIN),
        Ok(true),
        "the old page's failure cannot stop the new request"
    );
    let next: serde_json::Value = serde_json::from_str(&next).unwrap();
    rt.operations.reply(
        s.store(),
        &serde_json::json!({"@type": "forumTopics",
        "topics": [], "@extra": next["@extra"]}),
    );
    rt.topics_loaded(BERLIN, Ok(false));
    TELEGRAM.poll(&mut s);
    assert_eq!(
        rt.topics_status(BERLIN),
        Ok(false),
        "a successful refresh stays successful"
    );
    assert_eq!(
        topics::list(s.store(), BERLIN).len(),
        4,
        "the cached catalog is preserved"
    );
}

#[test]
fn topic_history_read_claim_and_drafts_never_cross_topics() {
    let mut s = session();
    let list = open_root(&mut s, Chats::id());
    let preview = Nav::Preview {
        from: list,
        id: Chat::topic(BERLIN, 2),
    };
    go(&mut s, preview.clone());
    let meetups = s.joined_child(list).unwrap();
    assert_eq!(
        with_chat(&s, meetups, |c| c
            .history()
            .iter()
            .map(|m| m.id)
            .collect::<Vec<_>>()),
        vec![2000]
    );
    assert_eq!(topics::get(s.store(), BERLIN, 2).unwrap().unread, 0);
    assert_eq!(topics::get(s.store(), BERLIN, 4).unwrap().unread, 1);
    s.undo();
    s.settle();
    assert_eq!(topics::get(s.store(), BERLIN, 2).unwrap().unread, 1);
    go(&mut s, preview);
    let meetups = s.joined_child(list).unwrap_or(meetups);
    let cycling = open_root(&mut s, Chat::topic(BERLIN, 4));
    with_chat(&s, meetups, |c| c.set_draft("coffee draft"));
    with_chat(&s, cycling, |c| c.set_draft("cycling draft"));
    assert_eq!(
        topics::get(s.store(), BERLIN, 2).unwrap().draft.as_deref(),
        Some("coffee draft")
    );
    assert_eq!(
        topics::get(s.store(), BERLIN, 4).unwrap().draft.as_deref(),
        Some("cycling draft")
    );
    assert_eq!(model::peer(s.store(), BERLIN).unwrap().draft, None);
    let at = Chat::topic_at(BERLIN, 2, 2000);
    assert_eq!(Chat::topic_of(&at), 2);
    assert_eq!(Chat::msg_of(&at), Some(2000));
    let old_link = open_root(&mut s, Chat::at(BERLIN, 2000));
    assert_eq!(with_chat(&s, old_link, |c| c.topic_id()), 2);
}

#[test]
fn composer_files_drafts_and_forwards_target_the_selected_topic() {
    let mut s = session();
    let inbox = runtime::of(s.store()).connect();
    let chat = open_root(&mut s, Chat::topic(BERLIN, 2));
    with_chat(&s, chat, |c| {
        c.set_draft("coffee: Saturday");
        c.flush_draft();
        c.reply(2000);
    });
    send(&mut s, chat);
    with_chat(&s, chat, |c| {
        c.carry(&["/tmp/topic-photo.jpg".into()]);
        c.set_draft("here is the place");
    });
    send(&mut s, chat);
    let sent: Vec<serde_json::Value> = inbox
        .try_iter()
        .map(|s| serde_json::from_str(&s).unwrap())
        .collect();
    let content: Vec<_> = sent
        .iter()
        .filter(|r| r["@type"] == "sendMessage" || r["@type"] == "setChatDraftMessage")
        .collect();
    assert_eq!(content.len(), 3);
    for r in &content {
        assert_eq!(r["chat_id"], BERLIN);
        assert_eq!(r["topic_id"]["@type"], "messageTopicForum");
        assert_eq!(r["topic_id"]["forum_topic_id"], 2);
    }
    assert_eq!(content[1]["reply_to"]["message_id"], 2000);
    s.store()
        .write(|c| topics::select_tx(c, BERLIN, &[2], true))
        .unwrap();
    let chats = open_root(&mut s, Chats::id());
    let index = with_chats(&s, chats, |p| p.rows(0, 50))
        .iter()
        .position(|r| r.peer == BERLIN)
        .unwrap();
    with_chats(&s, chats, |p| p.go(index));
    runtime::of(s.store()).carry_forward(HIKE, vec![100]);
    verb(&mut s, chats, "telegram.forward_here");
    let request: serde_json::Value =
        serde_json::from_str(&inbox.try_iter().last().unwrap()).unwrap();
    assert_eq!(request["@type"], "forwardMessages");
    assert_eq!(request["topic_id"]["forum_topic_id"], 2);
    verb(&mut s, chat, "telegram.attach");
    let attach = s.joined_child(chat).unwrap();
    observe(&s, attach);
    verb(&mut s, attach, "telegram.place");
    let place = s.joined_child(attach).unwrap();
    assert_eq!(
        *s.panel(place).unwrap().borrow().id(),
        Place::in_topic(BERLIN, 2)
    );
    verb(&mut s, place, "telegram.send_place");
    assert!(inbox.try_iter().next().is_none());
    assert!(s
        .notes()
        .last()
        .unwrap()
        .msg
        .contains("Location sharing is not available yet"));
}

#[test]
fn media_navigation_stays_inside_the_topic() {
    let mut s = session();
    s.store()
        .write(|c| {
            for (id, topic) in [(5000, 2), (6000, 3), (7000, 2)] {
                c.execute(
                    "INSERT INTO tg_message(chat, id, topic, date, text, media)
                VALUES(?1, ?2, ?3, ?2, '', 'photo')",
                    rusqlite::params![BERLIN, id, topic],
                )?;
            }
            Ok(())
        })
        .unwrap();
    let viewer = open_root(&mut s, Viewer::id(BERLIN, 5000));
    let p = s.panel(viewer).unwrap();
    let mut p = p.borrow_mut();
    let viewer = p.as_any().downcast_mut::<Viewer>().unwrap();
    assert_eq!(viewer.neighbours(), (None, Some(7000)));
}

#[test]
fn filtering_and_archiving_change_only_the_named_topics() {
    let mut s = session();
    let picker = open_root(&mut s, Topics::id(BERLIN));
    with_topics(&s, picker, |p| p.set_filter("meet".into()));
    verb(&mut s, picker, "telegram.show_topics");
    assert!(topics::get(s.store(), BERLIN, 2).unwrap().selected);
    assert!(!topics::get(s.store(), BERLIN, 4).unwrap().selected);
    let chats = open_root(&mut s, Chats::id());
    let i = with_chats(&s, chats, |p| p.rows(0, 50))
        .iter()
        .position(|r| r.peer == BERLIN)
        .unwrap();
    with_chats(&s, chats, |p| {
        p.go(i);
        p.toggle_mark();
    });
    verb(&mut s, chats, "telegram.archive");
    assert!(!model::peer(s.store(), BERLIN).unwrap().archived);
    assert!(topics::get(s.store(), BERLIN, 2).unwrap().archived);
    let archive = open_root(&mut s, Chats::archive());
    assert!(with_chats(&s, archive, |p| p.rows(0, 50))
        .iter()
        .any(|r| r.peer == BERLIN && r.topic == 2));
    with_topics(&s, picker, |p| p.set_filter(String::new()));
    verb(&mut s, picker, "telegram.hide_topics");
    assert!(topics::list(s.store(), BERLIN).iter().all(|t| !t.selected));
}

#[test]
fn a_read_batch_keeps_unloaded_conversations_unread_and_marked() {
    let mut s = session();
    let inbox = runtime::of(s.store()).connect();
    s.store()
        .write(|c| {
            c.execute(
                "DELETE FROM tg_message WHERE chat = ?1 OR (chat = ?2 AND topic = 2)",
                rusqlite::params![ANNA, BERLIN],
            )?;
            c.execute(
                "UPDATE tg_chat SET unread = 3, mention = 1 WHERE peer = ?1",
                [ANNA],
            )?;
            c.execute(
                "UPDATE tg_topic SET selected = 1, unread = 5, mention = 1
            WHERE chat = ?1 AND id = 2",
                [BERLIN],
            )?;
            Ok(())
        })
        .unwrap();
    let last = model::newest_ordinary_line(s.store(), STELAXIS).unwrap();
    let before = unread(&s, STELAXIS);
    let list = open_root(&mut s, Chats::id());
    for (peer, topic) in [(STELAXIS, 0), (ANNA, 0), (BERLIN, 2)] {
        let index = with_chats(&s, list, |p| p.rows(0, 50))
            .iter()
            .position(|row| row.peer == peer && row.topic == topic)
            .unwrap();
        with_chats(&s, list, |p| {
            p.go(index);
            p.toggle_mark();
        });
    }

    verb(&mut s, list, "telegram.read");

    assert_eq!(unread(&s, ANNA), (3, 1));
    assert_eq!(unread(&s, STELAXIS), before);
    let meetup = topics::get(s.store(), BERLIN, 2).unwrap();
    assert_eq!(meetup.unread, 5);
    assert_eq!(meetup.unread_mentions, 1);
    assert_eq!(topics::get(s.store(), BERLIN, 4).unwrap().unread, 1);
    assert_eq!(with_chats(&s, list, |p| p.list_mut().marks().len()), 2);
    let sent: Vec<serde_json::Value> = inbox
        .try_iter()
        .map(|r| serde_json::from_str(&r).unwrap())
        .collect();
    assert_eq!(
        sent.len(),
        1,
        "only the chat with a message can send a read request"
    );
    assert_eq!(sent[0]["@type"], "viewMessages");
    assert_eq!(sent[0]["chat_id"], STELAXIS);
    assert_eq!(sent[0]["message_ids"], serde_json::json!([last]));
}

#[test]
fn selections_survive_reopening_the_database() {
    use kernel::store::Store;
    let dir = std::env::temp_dir().join(format!("superapp-topic-selection-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("store.db");
    {
        let store = Store::open(Some(&path), &[&schema::SCHEMA]).unwrap();
        crate::apps::telegram::seed::seed_if_empty(&store).unwrap();
        store
            .write(|c| topics::select_tx(c, BERLIN, &[2], true))
            .unwrap();
    }
    {
        let store = Store::open(Some(&path), &[&schema::SCHEMA]).unwrap();
        assert!(topics::get(&store, BERLIN, 2).unwrap().selected);
        assert!(!topics::get(&store, BERLIN, 4).unwrap().selected);
    }
    std::fs::remove_dir_all(dir).unwrap();
}
