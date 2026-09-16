//! The comments under a post, over the wire: where they are, the page that
//! brings them, the receipt that reads them, and the counts on the post.

use super::*;
use crate::apps::telegram::{model, model::Scope, requests, threads};

/// A channel, and the group its comments are written in.
const CHANNEL: i64 = -1_000_000_000_042;
const GROUP: i64 = -1_000_000_000_043;
const POST: i64 = 500;
const ROOT: i64 = 900;

/// The discussion group, announced as the supergroup it is.
fn group(acc: &Account<FakeTd>, w: &World) {
    acc.on_update(
        w,
        &json!({"@type": "updateSupergroup", "supergroup": {
            "id": 43, "member_count": 3400
        }})
        .to_string(),
    );
    acc.on_update(
        w,
        &json!({"@type": "updateNewChat", "chat": {
            "@type": "chat", "id": GROUP,
            "type": {"@type": "chatTypeSupergroup", "supergroup_id": 43, "is_channel": false},
            "title": "Rust Weekly chat", "unread_count": 0, "positions": [],
        }})
        .to_string(),
    );
}

fn channel(acc: &Account<FakeTd>, w: &World) {
    acc.on_update(
        w,
        &json!({"@type": "updateSupergroup", "supergroup": {
            "id": 42, "is_channel": true, "member_count": 12800, "has_linked_chat": true
        }})
        .to_string(),
    );
    acc.on_update(
        w,
        &chat_object(CHANNEL, "Rust Weekly", json!([
            {"list": {"@type": "chatListMain"}, "order": "42"}
        ])),
    );
}

/// The post, with the reply info a channel with a discussion group carries.
fn post(count: i64, last: i64, read: i64) -> serde_json::Value {
    json!({"id": POST, "chat_id": CHANNEL, "date": 500,
        "content": {"@type": "messageText", "text": {"text": "Issue 612"}},
        "interaction_info": {"view_count": 4300, "reply_info": {
            "reply_count": count, "last_message_id": last,
            "last_read_inbox_message_id": read}}})
}

/// The post's own copy in the discussion group: an automatic forward the
/// channel itself is the sender of, which is what makes it the root.
fn root_copy() -> serde_json::Value {
    json!({"id": ROOT, "chat_id": GROUP, "date": 900,
        "sender_id": {"@type": "messageSenderChat", "chat_id": CHANNEL},
        "forward_info": {
            "origin": {"@type": "messageOriginChannel", "chat_id": CHANNEL, "message_id": POST},
            // The wire fills this in for a copy in a discussion group and
            // for almost nothing else.
            "source": {"chat_id": CHANNEL, "message_id": POST}},
        "interaction_info": {"reply_info": {"reply_count": 2,
            "last_message_id": 902, "last_read_inbox_message_id": 901}},
        "content": {"@type": "messageText", "text": {"text": "Issue 612"}}})
}

/// The wire's answer to *where are this post's comments*.
fn thread_info(extra: &serde_json::Value) -> String {
    json!({"@type": "messageThreadInfo", "chat_id": GROUP, "message_thread_id": ROOT,
        "reply_info": {"reply_count": 2, "last_message_id": 902, "last_read_inbox_message_id": 901},
        "unread_message_count": 1,
        "messages": [root_copy()],
        "draft_message": {"input_message_text": {"text": {"text": "half a comment"}}},
        "@extra": extra.clone()})
    .to_string()
}

#[test]
fn a_posts_comments_are_asked_after_once_and_written_down_where_they_are() {
    let td = FakeTd::new();
    let acc = account(td.clone(), None);
    let w = world();
    channel(&acc, &w);
    acc.on_update(&w, &json!({"@type": "updateNewMessage", "message": post(2, 902, 0)}).to_string());
    assert_eq!(model::line(w.store(), CHANNEL, POST).unwrap().comments, Some(2));

    let rt = runtime::of(w.store());
    let _inbox = rt.connect();
    rt.want_thread((CHANNEL, POST));
    rt.want_thread((CHANNEL, POST));
    acc.drain(&w);
    let asks = td.sent().iter().filter(|s| s.contains("getMessageThread")).count();
    assert_eq!(asks, 1, "one ask covers a panel drawing over and over");
    let ask = last_request(&td, "getMessageThread");
    assert_eq!((ask["chat_id"].as_i64(), ask["message_id"].as_i64()), (Some(CHANNEL), Some(POST)));

    acc.on_update(&w, &thread_info(&ask["@extra"]));
    let thread = threads::get(w.store(), CHANNEL, POST).expect("the thread row");
    assert_eq!(thread.where_it_is(), Some((GROUP, ROOT)));
    assert_eq!((thread.count, thread.last, thread.last_read), (2, Some(902), Some(901)));
    // The root came with the answer and belongs to the thread, so a panel
    // has the post to draw over the comments before any page lands.
    let root = model::line(w.store(), GROUP, ROOT).expect("the root");
    assert_eq!(root.thread, ROOT);
    assert_eq!(model::history_in(w.store(), GROUP, Scope::Thread(ROOT)).len(), 1);
    // And the same row is found from the group's side.
    assert_eq!(threads::of_root(w.store(), GROUP, ROOT).map(|t| t.post), Some(POST));
}

#[test]
fn a_visible_thread_walks_its_own_history() {
    let td = FakeTd::new();
    let acc = account(td.clone(), None);
    let w = world();
    channel(&acc, &w);
    acc.on_update(&w, &chat_object(GROUP, "Rust Weekly chat", json!([])));
    let _view = runtime::of(w.store()).watch_messages(
        GROUP, Some(Scope::Thread(ROOT)), vec![], w.now() - runtime::VIEW_SETTLE);
    acc.drain(&w);
    let page = last_request(&td, "getMessageThreadHistory");
    assert_eq!(page["chat_id"], GROUP);
    assert_eq!(page["message_id"], ROOT);
    assert_eq!(page["from_message_id"], 0);

    // The lines a thread's page brings are the thread's, whether or not
    // each one repeats the name of it.
    let answer = json!({"@type": "messages", "total_count": 2, "messages": [
        {"id": 902, "chat_id": GROUP, "date": 902, "sender_id": {"@type": "messageSenderUser", "user_id": 7},
         "content": {"@type": "messageText", "text": {"text": "and the second"}}},
        {"id": 901, "chat_id": GROUP, "date": 901, "sender_id": {"@type": "messageSenderUser", "user_id": 7},
         "topic_id": {"@type": "messageTopicThread", "message_thread_id": ROOT},
         "content": {"@type": "messageText", "text": {"text": "the first comment"}}}],
        "@extra": page["@extra"]});
    acc.on_update(&w, &answer.to_string());
    let lines = model::history_in(w.store(), GROUP, Scope::Thread(ROOT));
    assert_eq!(lines.len(), 2);
    assert!(lines.iter().all(|m| m.thread == ROOT));
    assert!(model::history_in(w.store(), GROUP, Scope::Whole).len() >= 2,
        "the group's own transcript still holds them");
}

#[test]
fn a_thread_is_read_by_its_own_source_and_never_backwards() {
    let td = FakeTd::new();
    let acc = account(td.clone(), None);
    let w = world();
    channel(&acc, &w);
    acc.on_update(&w, &chat_object(GROUP, "Rust Weekly chat", json!([])));
    acc.on_update(&w, &json!({"@type": "updateNewMessage", "message": post(2, 902, 0)}).to_string());
    let rt = runtime::of(w.store());
    let _inbox = rt.connect();
    rt.want_thread((CHANNEL, POST));
    acc.drain(&w);
    let ask = last_request(&td, "getMessageThread");
    acc.on_update(&w, &thread_info(&ask["@extra"]));
    // The comment being read is one the transcript has: a receipt is sent
    // for what was drawn, and what was drawn came from the store.
    acc.on_update(&w, &json!({"@type": "updateNewMessage", "message":
        {"id": 902, "chat_id": GROUP, "date": 902,
         "sender_id": {"@type": "messageSenderUser", "user_id": 7},
         "topic_id": {"@type": "messageTopicThread", "message_thread_id": ROOT},
         "content": {"@type": "messageText", "text": {"text": "a comment"}}}})
        .to_string());

    let receipt = requests::in_scope(
        requests::view_messages(GROUP, &[902]), Scope::Thread(ROOT));
    let sent: serde_json::Value = serde_json::from_str(&receipt).unwrap();
    assert_eq!(sent["source"]["@type"], "messageSourceMessageThreadHistory");
    assert!(sent["topic_id"].is_null(), "a receipt names no destination");
    acc.send(&w, &receipt);
    let echo: serde_json::Value = serde_json::from_str(td.sent().last().unwrap()).unwrap();
    acc.on_update(&w, &json!({"@type": "ok", "@extra": echo["@extra"]}).to_string());
    assert_eq!(threads::get(w.store(), CHANNEL, POST).unwrap().last_read, Some(902));

    // A snapshot that lags this device's own reading cannot rewind it.
    acc.on_update(&w, &json!({"@type": "updateMessageInteractionInfo",
        "chat_id": CHANNEL, "message_id": POST,
        "interaction_info": {"view_count": 4400, "reply_info": {
            "reply_count": 3, "last_message_id": 903, "last_read_inbox_message_id": 900}}})
        .to_string());
    let thread = threads::get(w.store(), CHANNEL, POST).unwrap();
    assert_eq!((thread.count, thread.last, thread.last_read), (3, Some(903), Some(902)));
    let line = model::line(w.store(), CHANNEL, POST).unwrap();
    assert_eq!((line.comments, line.views), (Some(3), Some(4400)));
    assert!(line.comments_new, "a comment has arrived since the reading");
}

#[test]
fn a_post_with_a_discussion_and_nobody_in_it_still_offers_the_way_in() {
    let td = FakeTd::new();
    let acc = account(td.clone(), None);
    let w = world();
    channel(&acc, &w);
    acc.on_update(&w, &json!({"@type": "updateNewMessage", "message": post(0, 0, 0)}).to_string());
    assert_eq!(model::line(w.store(), CHANNEL, POST).unwrap().comments, Some(0));
    // A line with no reply info at all has no comments to open.
    acc.on_update(&w, &json!({"@type": "updateNewMessage", "message": {
        "id": 501, "chat_id": CHANNEL, "date": 501,
        "content": {"@type": "messageText", "text": {"text": "no discussion"}}}})
        .to_string());
    assert_eq!(model::line(w.store(), CHANNEL, 501).unwrap().comments, None);
}

#[test]
fn only_the_channels_own_copy_says_where_a_posts_comments_are() {
    let td = FakeTd::new();
    let acc = account(td.clone(), None);
    let w = world();
    channel(&acc, &w);
    group(&acc, &w);

    // Somebody forwarding another channel's post into this group makes a
    // line that can be commented on too — and those comments are its own.
    let other = -1_000_000_000_077_i64;
    acc.on_update(&w, &json!({"@type": "updateNewMessage", "message": {
        "id": 800, "chat_id": GROUP, "date": 800,
        "sender_id": {"@type": "messageSenderUser", "user_id": 7},
        "forward_info": {"origin": {"@type": "messageOriginChannel",
            "chat_id": other, "message_id": 42}},
        "interaction_info": {"reply_info": {"reply_count": 1, "last_message_id": 801}},
        "content": {"@type": "messageText", "text": {"text": "look at this"}}}})
        .to_string());
    assert!(
        threads::get(w.store(), other, 42).is_none(),
        "the other channel's post keeps its own comments, wherever they are"
    );
    assert_eq!(
        threads::get(w.store(), GROUP, 800).and_then(|t| t.where_it_is()),
        Some((GROUP, 800)),
        "the forwarded line is the root of its own thread, here"
    );

    // Nor does one forwarded by hand *as the channel* — an anonymous
    // admin's forward wears the channel as its sender and the post as its
    // origin, and the wire gives it no source at all, which is the whole
    // difference between it and the copy below.
    acc.on_update(&w, &json!({"@type": "updateNewMessage", "message": {
        "id": 810, "chat_id": GROUP, "date": 810,
        "sender_id": {"@type": "messageSenderChat", "chat_id": CHANNEL},
        "forward_info": {"origin": {"@type": "messageOriginChannel",
            "chat_id": CHANNEL, "message_id": POST}},
        "interaction_info": {"reply_info": {"reply_count": 0}},
        "content": {"@type": "messageText", "text": {"text": "worth reading again"}}}})
        .to_string());
    assert!(
        threads::get(w.store(), CHANNEL, POST).is_none(),
        "a hand's forward says nothing about where the post's comments are"
    );

    // The channel's own automatic copy is the one that joins the two.
    acc.on_update(&w, &json!({"@type": "updateNewMessage", "message": root_copy()}).to_string());
    assert_eq!(
        threads::get(w.store(), CHANNEL, POST).and_then(|t| t.where_it_is()),
        Some((GROUP, ROOT))
    );
}

/// A channel can post something a person wrote, and the copy in the group is
/// still the post's own: the origin is the person, the source is the post.
#[test]
fn a_post_of_somebody_elses_words_still_finds_its_comments() {
    let td = FakeTd::new();
    let acc = account(td.clone(), None);
    let w = world();
    channel(&acc, &w);
    group(&acc, &w);
    acc.on_update(&w, &json!({"@type": "updateNewMessage", "message": {
        "id": ROOT, "chat_id": GROUP, "date": 900,
        "sender_id": {"@type": "messageSenderChat", "chat_id": CHANNEL},
        "forward_info": {
            "origin": {"@type": "messageOriginUser", "sender_user_id": 7},
            "source": {"chat_id": CHANNEL, "message_id": POST}},
        "interaction_info": {"reply_info": {"reply_count": 1, "last_message_id": 901}},
        "content": {"@type": "messageText", "text": {"text": "somebody's good thread"}}}})
        .to_string());
    assert_eq!(
        threads::get(w.store(), CHANNEL, POST).and_then(|t| t.where_it_is()),
        Some((GROUP, ROOT)),
        "the post and its copy are joined however the words got there"
    );
}

#[test]
fn a_reposted_post_keeps_its_own_comments_and_leaves_the_old_ones_alone() {
    let td = FakeTd::new();
    let acc = account(td.clone(), None);
    let w = world();
    channel(&acc, &w);
    group(&acc, &w);
    // The old post's copy, and the thread under it.
    acc.on_update(&w, &json!({"@type": "updateNewMessage", "message": root_copy()}).to_string());
    assert_eq!(
        threads::get(w.store(), CHANNEL, POST).and_then(|t| t.where_it_is()),
        Some((GROUP, ROOT))
    );
    // The channel reposts it: a new post, whose copy in the group still
    // carries the *first* post as its origin and the new one as its source.
    acc.on_update(&w, &json!({"@type": "updateNewMessage", "message": {
        "id": 950, "chat_id": GROUP, "date": 950,
        "sender_id": {"@type": "messageSenderChat", "chat_id": CHANNEL},
        "forward_info": {
            "origin": {"@type": "messageOriginChannel", "chat_id": CHANNEL, "message_id": POST},
            "source": {"chat_id": CHANNEL, "message_id": 600}},
        "interaction_info": {"reply_info": {"reply_count": 0}},
        "content": {"@type": "messageText", "text": {"text": "Issue 612, again"}}}})
        .to_string());
    assert_eq!(
        threads::get(w.store(), CHANNEL, 600).and_then(|t| t.where_it_is()),
        Some((GROUP, 950)),
        "the repost's own comments"
    );
    assert_eq!(
        threads::get(w.store(), CHANNEL, POST).and_then(|t| t.where_it_is()),
        Some((GROUP, ROOT)),
        "and the first post's are where they always were"
    );
}

#[test]
fn a_thread_answer_never_takes_back_what_is_typed_here() {
    let td = FakeTd::new();
    let acc = account(td.clone(), None);
    let w = world();
    channel(&acc, &w);
    group(&acc, &w);
    acc.on_update(&w, &json!({"@type": "updateNewMessage", "message": post(2, 902, 0)}).to_string());
    acc.on_update(&w, &json!({"@type": "updateNewMessage", "message": root_copy()}).to_string());
    // Typed here while the ask was on the wire.
    let rt = runtime::of(w.store());
    let _inbox = rt.connect();
    rt.want_thread((CHANNEL, POST));
    acc.drain(&w);
    let ask = last_request(&td, "getMessageThread");
    // Typed here while that ask was on the wire.
    let typed_at = w.now() + 10.0;
    w.store()
        .write(move |c| threads::draft_tx(c, GROUP, ROOT, "mine, half written", typed_at))
        .unwrap();
    acc.on_update(&w, &thread_info(&ask["@extra"]));
    assert_eq!(
        threads::get(w.store(), CHANNEL, POST).unwrap().draft.as_deref(),
        Some("mine, half written"),
        "the answer is a snapshot from before it was typed"
    );

    // Nor may a stale answer put back what was cleared here since — a
    // comment sent, say — and a clear made elsewhere, which is newer than
    // anything here, lands.
    let sent_at = typed_at + 10.0;
    w.store()
        .write(move |c| threads::draft_tx(c, GROUP, ROOT, "", sent_at))
        .unwrap();
    acc.on_update(&w, &thread_info(&ask["@extra"]));
    assert_eq!(threads::get(w.store(), CHANNEL, POST).unwrap().draft, None);
    let cleared = json!({"@type": "messageThreadInfo", "chat_id": GROUP,
        "message_thread_id": ROOT, "messages": [], "draft_message": null,
        "@extra": format!("thread:{CHANNEL}:{POST}:{}", sent_at + 10.0)});
    w.store()
        .write(move |c| threads::draft_tx(c, GROUP, ROOT, "typed again", sent_at + 5.0))
        .unwrap();
    acc.on_update(&w, &cleared.to_string());
    assert_eq!(
        threads::get(w.store(), CHANNEL, POST).unwrap().draft,
        None,
        "a clear from after the last word typed here"
    );
}

#[test]
fn a_thread_keeps_the_draft_another_device_left_in_it() {
    let td = FakeTd::new();
    let acc = account(td.clone(), None);
    let w = world();
    channel(&acc, &w);
    acc.on_update(&w, &json!({"@type": "updateNewMessage", "message": post(2, 902, 0)}).to_string());
    let rt = runtime::of(w.store());
    let _inbox = rt.connect();
    rt.want_thread((CHANNEL, POST));
    acc.drain(&w);
    let ask = last_request(&td, "getMessageThread");
    acc.on_update(&w, &thread_info(&ask["@extra"]));
    assert_eq!(
        threads::get(w.store(), CHANNEL, POST).unwrap().draft.as_deref(),
        Some("half a comment")
    );
    // A later line in the thread knows nothing about drafts and says
    // nothing about them.
    acc.on_update(&w, &json!({"@type": "updateNewMessage", "message":
        {"id": 903, "chat_id": GROUP, "date": 903,
         "sender_id": {"@type": "messageSenderUser", "user_id": 7},
         "topic_id": {"@type": "messageTopicThread", "message_thread_id": ROOT},
         "content": {"@type": "messageText", "text": {"text": "one more"}}}})
        .to_string());
    assert_eq!(
        threads::get(w.store(), CHANNEL, POST).unwrap().draft.as_deref(),
        Some("half a comment")
    );
}

#[test]
fn a_refused_ask_is_said_once_and_can_be_made_again() {
    let td = FakeTd::new();
    let acc = account(td.clone(), None);
    let w = world();
    channel(&acc, &w);
    let rt = runtime::of(w.store());
    let _inbox = rt.connect();
    rt.want_thread((CHANNEL, POST));
    acc.drain(&w);
    let ask = last_request(&td, "getMessageThread");
    acc.on_update(&w, &json!({"@type": "error", "code": 400, "message": "MSG_ID_INVALID",
        "@extra": ask["@extra"]}).to_string());
    assert!(rt.thread_trouble((CHANNEL, POST)).is_some_and(|t| t.contains("retry")));
    rt.want_thread((CHANNEL, POST));
    acc.drain(&w);
    assert_eq!(td.sent().iter().filter(|s| s.contains("getMessageThread")).count(), 1,
        "a refusal stands until it is retried");
    rt.retry_thread((CHANNEL, POST));
    assert!(rt.thread_trouble((CHANNEL, POST)).is_none());
    rt.want_thread((CHANNEL, POST));
    acc.drain(&w);
    assert_eq!(td.sent().iter().filter(|s| s.contains("getMessageThread")).count(), 2);
}

#[test]
fn a_supergroup_says_what_is_linked_to_it_and_whether_it_wants_members() {
    let td = FakeTd::new();
    let acc = account(td.clone(), None);
    let w = world();
    channel(&acc, &w);
    acc.on_update(&w, &json!({"@type": "updateSupergroup", "supergroup": {
        "id": 43, "member_count": 3400, "join_to_send_messages": true}}).to_string());
    acc.on_update(&w, &chat_object(GROUP, "Rust Weekly chat", json!([])));
    assert!(model::peer(w.store(), GROUP).unwrap().join_to_send);
    acc.on_update(&w, &json!({"@type": "updateSupergroupFullInfo", "supergroup_id": 42,
        "supergroup_full_info": {"member_count": 12800, "linked_chat_id": GROUP}}).to_string());
    assert_eq!(model::peer(w.store(), CHANNEL).unwrap().linked, Some(GROUP));
    // A link taken away is an answer too.
    acc.on_update(&w, &json!({"@type": "updateSupergroupFullInfo", "supergroup_id": 42,
        "supergroup_full_info": {"member_count": 12800, "linked_chat_id": 0}}).to_string());
    assert_eq!(model::peer(w.store(), CHANNEL).unwrap().linked, None);
}
