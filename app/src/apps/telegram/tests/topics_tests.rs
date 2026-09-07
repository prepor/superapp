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
