//! The comments under a post: the way in, the panel, the composer, and what
//! a post's foot says about what is under it.

use super::*;
use crate::apps::telegram::seed::RUST_WEEKLY_CHAT;
use crate::apps::telegram::threads;
use kernel::panel::Panel;

/// The post the demo world gives eight comments and a reading two short of
/// them: *Issue 612*, the third of the channel's four.
fn busy_post(s: &Session) -> model::MsgId {
    model::history(s.store(), RUST_WEEKLY)[2].id
}

/// The post before it, whose thread nobody has written in.
fn quiet_post(s: &Session) -> model::MsgId {
    model::history(s.store(), RUST_WEEKLY)[1].id
}

#[test]
fn a_posts_foot_counts_its_comments_and_says_when_one_is_new() {
    let s = session();
    let history = model::history(s.store(), RUST_WEEKLY);
    let busy = &history[2];
    assert_eq!(busy.comments, Some(8));
    assert!(busy.comments_new, "two comments arrived after the reading");
    assert_eq!(
        threads::foot(busy.comments, busy.comments_new).as_deref(),
        Some("8 comments · new")
    );
    let quiet = &history[1];
    assert_eq!(
        threads::foot(quiet.comments, quiet.comments_new).as_deref(),
        Some("leave a comment"),
        "a post with a discussion group and nobody in it yet"
    );
    // An ordinary chat's lines have no discussion behind them and no foot.
    let vera = model::history(s.store(), VERA);
    assert!(vera.iter().all(|m| m.comments.is_none()));
    assert_eq!(threads::foot(None, false), None);
}

#[test]
fn comments_open_from_the_post_and_show_the_post_over_them() {
    let mut s = session();
    let post = busy_post(&s);
    let channel = open_root(&mut s, Chat::at(RUST_WEEKLY, post));
    with_chat(&s, channel, |c| c.set_cursor((RUST_WEEKLY, post)));
    verb(&mut s, channel, "telegram.comments");
    let slot = s.focus().expect("the comments panel");
    let id = s.panel(slot).unwrap().borrow().id().clone();
    assert_eq!(id, Chat::comments(RUST_WEEKLY, post));
    assert_eq!(s.panel(slot).unwrap().borrow().title(), "comments · Rust Weekly");

    // The transcript is the group's, scoped to the thread: the post's own
    // copy first, the caption under it, then the comments.
    let (scope, peer, rows) = with_chat(&s, slot, |c| {
        (c.scope(), c.peer(), c.snapshot(s.now()).rows.to_vec())
    });
    assert_eq!(peer, RUST_WEEKLY_CHAT);
    assert!(matches!(scope, Scope::Thread(_)), "a thread, not a whole chat");
    let shape: Vec<String> = rows
        .iter()
        .map(|r| match r {
            Row::Comments { empty } => format!("caption empty={empty}"),
            // The post's copy in the group is an automatic forward from the
            // channel and says so; a comment is somebody's own line.
            Row::Message { msg, .. } => msg
                .fwd_from
                .clone()
                .unwrap_or_else(|| msg.writer().to_string()),
            _ => "other".to_string(),
        })
        .collect();
    assert_eq!(shape.iter().filter(|r| *r == "caption empty=false").count(), 1);
    let caption = shape.iter().position(|r| r.starts_with("caption")).unwrap();
    assert_eq!(
        shape[caption - 1], "Rust Weekly",
        "the post itself stands over its comments"
    );
    assert_eq!(
        rows.iter().filter_map(Row::msg).filter(|m| m.sender.is_some()).count(),
        8,
        "every comment, and nothing from another thread"
    );
}

#[test]
fn an_empty_thread_says_so_and_still_takes_a_comment() {
    let mut s = session();
    let post = quiet_post(&s);
    let slot = open_root(&mut s, Chat::comments(RUST_WEEKLY, post));
    let rows = with_chat(&s, slot, |c| c.snapshot(s.now()).rows.to_vec());
    assert!(rows.iter().any(|r| matches!(r, Row::Comments { empty: true })));
    // The group takes a stranger's line, so the composer stands.
    let card = with_chat(&s, slot, Chat::card).expect("a card");
    assert!(card.can_post());
    assert!(!card.wants_joining());
}

#[test]
fn opening_a_thread_draws_its_unread_line_and_reads_it() {
    let mut s = session();
    let post = busy_post(&s);
    let before = threads::get(s.store(), RUST_WEEKLY, post).unwrap();
    assert_eq!(before.unread, 2, "two comments arrived after the reading");
    // Opened the way a person opens it — a restored panel claims nothing.
    let channel = open_root(&mut s, Chat::at(RUST_WEEKLY, post));
    with_chat(&s, channel, |c| c.set_cursor((RUST_WEEKLY, post)));
    verb(&mut s, channel, "telegram.comments");
    let slot = s.focus().expect("the comments panel");
    let rows = with_chat(&s, slot, |c| c.snapshot(s.now()).rows.to_vec());
    let line = rows.iter().position(|r| matches!(r, Row::Unread)).expect("the unread line");
    assert_eq!(
        rows.len() - line - 1,
        2,
        "it stands above the two comments that came after the reading"
    );
    // Opening reads them, in the thread and not in the group.
    let after = threads::get(s.store(), RUST_WEEKLY, post).unwrap();
    assert_eq!(after.last_read, after.last);
    assert_eq!(after.unread, 0);
    assert_eq!(
        model::peer(s.store(), RUST_WEEKLY_CHAT).unwrap().unread,
        0,
        "the group's own count is untouched"
    );
}

#[test]
fn a_comments_draft_belongs_to_its_thread_and_not_to_the_group() {
    let mut s = session();
    let post = busy_post(&s);
    let slot = open_root(&mut s, Chat::comments(RUST_WEEKLY, post));
    with_chat(&s, slot, |c| c.set_draft("a comment of mine"));
    s.settle();
    let thread = threads::get(s.store(), RUST_WEEKLY, post).expect("the thread row");
    assert_eq!(thread.draft.as_deref(), Some("a comment of mine"));
    assert_eq!(
        model::peer(s.store(), RUST_WEEKLY_CHAT).unwrap().draft,
        None,
        "the group's own composer is untouched"
    );
    // And the panel reads it back as its own.
    assert_eq!(
        with_chat(&s, slot, Chat::card).unwrap().draft.as_deref(),
        Some("a comment of mine")
    );
}

#[test]
fn a_post_with_no_thread_yet_waits_rather_than_drawing_the_channel() {
    let mut s = session();
    // A post with a count and no thread this store has been told about:
    // nothing to show, and nothing to write with, until the wire answers.
    let post = busy_post(&s);
    s.store()
        .write(move |c| {
            c.execute("DELETE FROM tg_thread WHERE post = ?1", [post]).map(|_| ())
        })
        .unwrap();
    let slot = open_root(&mut s, Chat::comments(RUST_WEEKLY, post));
    let (seeking, rows, peer) = with_chat(&s, slot, |c| {
        (c.seeking(), c.snapshot(s.now()).rows.len(), c.peer())
    });
    assert!(seeking, "still looking for where the comments are");
    assert_eq!(rows, 0, "the channel's own posts are not the comments");
    assert_eq!(peer, RUST_WEEKLY);
    assert!(!with_chat(&s, slot, |c| c.verbs().iter().any(|v| v.id == "telegram.submit")));
}

#[test]
fn the_channels_card_opens_the_group_its_comments_are_written_in() {
    let mut s = session();
    let card = open_root(&mut s, Peer::id(RUST_WEEKLY));
    verb(&mut s, card, "telegram.discussion");
    let slot = s.focus().expect("the group");
    assert_eq!(
        *s.panel(slot).unwrap().borrow().id(),
        Chat::id(RUST_WEEKLY_CHAT)
    );
}

#[test]
fn a_group_that_wants_members_offers_joining_in_place_of_the_composer() {
    let mut s = session();
    s.store()
        .write(|c| {
            c.execute(
                "UPDATE tg_peer SET join_to_send = 1 WHERE id = ?1",
                [RUST_WEEKLY_CHAT],
            )
            .map(|_| ())
        })
        .unwrap();
    let post = busy_post(&s);
    let slot = open_root(&mut s, Chat::comments(RUST_WEEKLY, post));
    let card = with_chat(&s, slot, Chat::card).expect("a card");
    assert!(card.wants_joining());
    assert!(!card.can_post(), "a stranger's comment needs the joining first");
    let verbs = with_chat(&s, slot, |c| {
        c.verbs().into_iter().map(|v| v.id.to_string()).collect::<Vec<_>>()
    });
    assert!(verbs.contains(&"telegram.join_group".to_string()));
    assert!(!verbs.contains(&"telegram.submit".to_string()));
}

#[test]
fn the_group_side_of_a_thread_opens_the_same_panel() {
    let mut s = session();
    let post = busy_post(&s);
    let thread = threads::get(s.store(), RUST_WEEKLY, post).unwrap();
    let (group, root) = thread.where_it_is().unwrap();
    // The post's copy in the group carries the count too, and the panel it
    // opens is the thread's — reached from the other side.
    let slot = open_root(&mut s, Chat::at(group, root));
    with_chat(&s, slot, |c| c.set_cursor((group, root)));
    let verbs = with_chat(&s, slot, |c| c.verbs().into_iter().collect::<Vec<_>>());
    let comments = verbs.iter().find(|v| v.id == "telegram.comments").expect("the way in");
    match &comments.act {
        VerbAct::Go(Nav::Open { id, .. }) => assert_eq!(
            *id,
            Chat::comments(RUST_WEEKLY, post),
            "both ways in land on the panel the post names"
        ),
        _ => panic!("comments opens a panel"),
    }
    let opened = open_root(&mut s, Chat::comments(group, root));
    assert_eq!(with_chat(&s, opened, |c| c.scope()), Scope::Thread(root));
    assert_eq!(with_chat(&s, opened, |c| c.peer()), group);
}
