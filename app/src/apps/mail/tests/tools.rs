//! Mail's tools, run the way a chat runs them: by name, out of the
//! registry, with their arguments read against their own schema first.
//!
//! The point of most of them is that they are the verb, so the tests are
//! comparisons: a session where the button was pressed and a twin where the
//! tool was called end up with the same rows, and one press of `cmd+z`
//! undoes either.

use super::*;

use serde_json::{json, Value};

/// One call, checked and settled — what the chat panel will do per
/// `agent_call` row.
fn call(s: &mut Session, name: &str, input: &Value) -> Result<Value, String> {
    let t = s
        .apps()
        .tool(name)
        .unwrap_or_else(|| panic!("no tool {name}"))
        .clone();
    t.check(input)?;
    let out = (t.run)(s, input);
    s.settle();
    out
}

/// Where every letter of the store is filed — the one reading that says
/// whether two sessions agree about what a verb did.
fn folders(s: &Session) -> Vec<(MailId, String)> {
    let conn = s.store().conn();
    let mut stmt = conn
        .prepare(
            "SELECT m.id, COALESCE(f.role, '') FROM message m
             LEFT JOIN folder f ON f.id = m.folder ORDER BY m.id",
        )
        .expect("the folders query");
    let rows = stmt
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))
        .expect("the folders");
    rows.filter_map(Result::ok).collect()
}

/// Which letters of the store are unread.
fn unreads(s: &Session) -> Vec<MailId> {
    let conn = s.store().conn();
    let mut stmt = conn
        .prepare("SELECT id FROM message WHERE unread = 1 ORDER BY id")
        .expect("the unread query");
    let rows = stmt.query_map([], |r| r.get(0)).expect("the unread");
    rows.filter_map(Result::ok).collect()
}

/// The conversation the top row of a mailbox stands for.
fn top_thread(s: &Session, slot: SlotId) -> i64 {
    with_mailbox(s, slot, |m| m.rows(0, 1))
        .first()
        .expect("a row")
        .thread
}

fn label(s: &Session) -> String {
    s.history()
        .rows()
        .0
        .last()
        .map(|r| r.label.clone())
        .unwrap_or_default()
}

// -- the filing tools are the filing verbs ------------------------------------------

/// The archive tool and the archive button leave the same rows, and one
/// `cmd+z` puts either back: the tool claims the same [`Filed`] intents,
/// because it runs the same filing.
#[test]
fn the_archive_tool_files_exactly_what_the_archive_verb_files() {
    // The button: mark the top row of the inbox, press *archive*.
    let (mut verbed, _c1) = session();
    let list = open_root(&mut verbed, Role::Inbox.id());
    with_mailbox(&verbed, list, |m| {
        m.go(0);
        m.toggle_mark();
    });
    verb(&mut verbed, list, "mail.archive");

    // The tool: the same conversation, by id, on a twin.
    let (mut tooled, _c2) = session();
    let twin = open_root(&mut tooled, Role::Inbox.id());
    let thread = top_thread(&tooled, twin);
    let out = call(&mut tooled, "mail.archive", &json!({ "thread": thread }))
        .expect("the tool archived it");
    assert_eq!(out["letters"], json!(1));
    assert_eq!(out["mailbox"], json!("archive"));

    assert_eq!(
        folders(&verbed),
        folders(&tooled),
        "the same letters moved to the same folders"
    );
    assert_eq!(
        label(&tooled),
        "archive “Q3 infra budget draft”",
        "the node says what a card will say"
    );

    // And the same undo.
    assert!(verbed.undo());
    assert!(tooled.undo());
    assert_eq!(folders(&verbed), folders(&tooled));
    assert_eq!(role_of(tooled.store(), 1), "inbox");
}

/// Deleting takes the copies in whichever mailbox holds the conversation,
/// and undo puts them back where they were.
#[test]
fn the_delete_tool_trashes_a_conversation_and_undo_restores_it() {
    let (mut s, _clock) = session();
    let list = open_root(&mut s, Role::Inbox.id());
    let thread = top_thread(&s, list);
    call(&mut s, "mail.delete", &json!({ "thread": thread })).expect("the tool deleted it");
    assert_eq!(role_of(s.store(), 1), "trash");
    assert_eq!(label(&s), "delete “Q3 infra budget draft”");
    assert!(s.undo());
    assert_eq!(role_of(s.store(), 1), "inbox");
}

/// *not spam* is the same move in the other direction, and it says so when
/// the conversation is not in the junk.
#[test]
fn the_not_spam_tool_takes_a_conversation_out_of_the_junk() {
    let (mut s, _clock) = session();
    let spam = open_root(&mut s, Role::Spam.id());
    let thread = top_thread(&s, spam);
    call(&mut s, "mail.not_spam", &json!({ "thread": thread })).expect("out of the junk");
    assert_eq!(
        model::role_word_of(s.store(), thread).as_deref(),
        Some("inbox")
    );
    assert!(s.undo());
    assert_eq!(
        model::role_word_of(s.store(), thread).as_deref(),
        Some("spam")
    );

    // And a conversation that is not in the spam has nothing to be taken
    // out of it.
    let inbox = open_root(&mut s, Role::Inbox.id());
    let other = top_thread(&s, inbox);
    let e = call(&mut s, "mail.not_spam", &json!({ "thread": other })).expect_err("not junk");
    assert!(e.contains("not in the spam"), "{e}");
}

/// A conversation the inbox does not hold cannot be archived out of it, and
/// the sentence says which mailbox was looked in. Which mailbox's copies a
/// filing takes is the mailbox verb's rule, and *archive* is the inbox's
/// alone — a conversation that is in the archive *and* in the inbox still
/// has an inbox copy to archive, which is why this asks the junk.
#[test]
fn archiving_something_that_is_not_in_the_inbox_says_so() {
    let (mut s, _clock) = session();
    let spam = open_root(&mut s, Role::Spam.id());
    let thread = top_thread(&s, spam);
    let e = call(&mut s, "mail.archive", &json!({ "thread": thread })).expect_err("not in it");
    assert!(e.contains("not in the inbox"), "{e}");
}

// -- reading ---------------------------------------------------------------------------

/// The index the launcher reads, answering ids instead of labels.
#[test]
fn the_search_tool_finds_a_seeded_letter() {
    let (mut s, _clock) = session();
    let out = call(&mut s, "mail.search", &json!({"query": "budget"})).expect("the search ran");
    let letters = out["letters"].as_array().expect("letters").clone();
    assert!(!letters.is_empty(), "the seed has a budget letter");
    let hit = letters
        .iter()
        .find(|l| {
            l["subject"]
                .as_str()
                .is_some_and(|s| s.contains("Q3 infra budget"))
        })
        .expect("the Q3 letter");
    assert!(hit["from"].as_str().is_some_and(|w| w.contains('@')));
    assert!(hit["date"].as_str().is_some_and(|d| !d.is_empty()));
    // The conversation it answers is the one the filing tools take.
    let thread = hit["thread"].as_i64().expect("its conversation");
    call(&mut s, "mail.archive", &json!({ "thread": thread })).expect("and it can be filed");

    // A question with no word in it asks the index nothing.
    let empty = call(&mut s, "mail.search", &json!({"query": "  "})).expect("nothing to look for");
    assert_eq!(empty["letters"], json!([]));
}

/// A conversation read as text: every letter of it, oldest first, in the
/// reading a person sees.
#[test]
fn the_thread_tool_reads_a_conversation_out() {
    let (mut s, _clock) = session();
    let list = open_root(&mut s, Role::Inbox.id());
    // The three-message conversation of the seed.
    let thread = with_mailbox(&s, list, |m| m.rows(0, 10))
        .into_iter()
        .find(|r| r.n == 3)
        .expect("Max's thread")
        .thread;
    let out = call(&mut s, "mail.thread", &json!({ "thread": thread })).expect("the conversation");
    assert_eq!(out["topic"], json!("superapp panel model"));
    let letters = out["letters"].as_array().expect("letters").clone();
    assert_eq!(letters.len(), 3);
    assert_eq!(out["truncated"], json!(false));
    for l in &letters {
        assert!(l["from"].as_str().is_some_and(|w| w.contains('@')), "{l}");
        assert!(l["text"].as_str().is_some_and(|t| !t.is_empty()), "{l}");
        assert!(l["date"].as_str().is_some_and(|d| !d.is_empty()), "{l}");
    }
    assert!(
        call(&mut s, "mail.thread", &json!({"thread": 9999})).is_err(),
        "a conversation nobody has"
    );
}

// -- marking ----------------------------------------------------------------------------

/// Marking read is what opening a reader claims, without opening one; and
/// marking unread is the mirror of it.
#[test]
fn the_read_and_unread_tools_are_one_undo_each() {
    let (mut s, _clock) = session();
    let list = open_root(&mut s, Role::Inbox.id());
    let thread = top_thread(&s, list);
    let before = unreads(&s);
    assert!(unread(s.store(), 1), "the top conversation is unread");

    let out = call(&mut s, "mail.read", &json!({ "thread": thread })).expect("marked read");
    assert_eq!(out["letters"], json!(1));
    assert!(!unread(s.store(), 1));
    assert_eq!(label(&s), "read “Q3 infra budget draft”");
    assert!(
        call(&mut s, "mail.read", &json!({ "thread": thread })).is_err(),
        "and again is nothing to do"
    );
    assert!(s.undo());
    assert_eq!(unreads(&s), before, "exactly the letters that moved");

    // The other way: a conversation put back where a person would find it.
    call(&mut s, "mail.read", &json!({ "thread": thread })).expect("read again");
    let out = call(&mut s, "mail.unread", &json!({ "thread": thread })).expect("and unread");
    assert_eq!(out["letters"], json!(1));
    assert!(unread(s.store(), 1));
    assert_eq!(label(&s), "unread “Q3 infra budget draft”");
    assert!(s.undo(), "which undo takes back too");
    assert!(!unread(s.store(), 1));
}

// -- writing a letter ---------------------------------------------------------------------

/// The agent never sends what nobody read: `mail.draft` opens a sheet with
/// the letter in it, and `mail.send` files the outbox row the sheet's own
/// *send* files — which the sender pass then takes.
#[test]
fn a_drafted_letter_opens_as_a_panel_and_sends_from_it() {
    let (mut s, clock) = session();
    let list = open_root(&mut s, Role::Inbox.id());

    let out = call(
        &mut s,
        "mail.draft",
        &json!({
            "to": "max@ivanov.dev",
            "subject": "the panel model",
            "body": "Agreed — Thursday works."
        }),
    )
    .expect("the sheet opened");
    let sheet = out["slot"].as_u64().expect("its slot");
    assert_eq!(
        s.focus(),
        Some(list),
        "the person's focus stays where it was"
    );
    assert_eq!(s.joined_child(list), Some(sheet), "joined to what was read");

    // The panel is the point: the sheet shows the letter the agent wrote.
    {
        let inst = s.panel(sheet).expect("a compose panel");
        let mut b = inst.borrow_mut();
        let c = b.as_any().downcast_mut::<Compose>().expect("a compose");
        assert_eq!(c.seed(), Seed::Blank);
        assert_eq!(c.draft().to, "max@ivanov.dev");
        assert_eq!(c.draft().subject, "the panel model");
        assert_eq!(c.draft().body, "Agreed — Thursday works.");
    }
    assert!(
        s.store()
            .conn()
            .query_row("SELECT COUNT(*) FROM outbox", [], |r| r.get::<_, i64>(0))
            .expect("the outbox")
            == 0,
        "and nothing has been sent"
    );

    call(&mut s, "mail.send", &json!({ "slot": sheet })).expect("the send was filed");
    assert!(s.panel(sheet).is_none(), "the sheet closed behind it");
    let (status, after): (String, f64) = s
        .store()
        .conn()
        .query_row(
            "SELECT status, send_after FROM outbox WHERE id = ?1",
            [sheet as i64],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .expect("an outbox row under the sheet's own slot");
    assert_eq!(status, "pending");
    assert!(after > s.now(), "the window has not run out yet");

    // The window runs out and the sender takes the row, exactly as it does
    // for a letter a person pressed send on.
    clock.advance(model::send_delay() + 1.0);
    s.workers().kick_all();
    let sent = servers(&s).submitted();
    assert_eq!(sent.len(), 1);
    assert_eq!(sent[0].to, "max@ivanov.dev");
    assert_eq!(sent[0].body, "Agreed — Thursday works.");
}

/// A reply threads: the sheet is seeded from the letter it answers, so the
/// send carries the headers that make it part of the conversation.
#[test]
fn a_draft_can_answer_a_letter() {
    let (mut s, clock) = session();
    let list = open_root(&mut s, Role::Inbox.id());
    let thread = with_mailbox(&s, list, |m| m.rows(0, 10))
        .into_iter()
        .find(|r| r.n == 3)
        .expect("Max's thread")
        .target;

    let out = call(
        &mut s,
        "mail.draft",
        &json!({
            "to": "max@ivanov.dev",
            "subject": "Re: superapp panel model",
            "body": "Agreed.",
            "re": thread
        }),
    )
    .expect("the sheet opened");
    let sheet = out["slot"].as_u64().expect("its slot");
    {
        let inst = s.panel(sheet).expect("a compose panel");
        let mut b = inst.borrow_mut();
        let c = b.as_any().downcast_mut::<Compose>().expect("a compose");
        assert_eq!(c.seed(), Seed::Reply(thread));
    }
    call(&mut s, "mail.send", &json!({ "slot": sheet })).expect("filed");
    clock.advance(model::send_delay() + 1.0);
    s.workers().kick_all();
    let sent = servers(&s).submitted();
    assert_eq!(sent.len(), 1);
    assert!(sent[0].in_reply_to.is_some(), "it says what it answers");

    // A letter nobody has cannot be answered.
    assert!(
        call(
            &mut s,
            "mail.draft",
            &json!({"to": "a@b.c", "subject": "x", "body": "y", "re": 9999})
        )
        .is_err(),
        "no letter at 9999"
    );
}

/// A slot with no draft in it is not a letter, and there is nothing to send.
#[test]
fn sending_a_slot_that_holds_no_draft_is_refused() {
    let (mut s, _clock) = session();
    let list = open_root(&mut s, Role::Inbox.id());
    let e = call(&mut s, "mail.send", &json!({ "slot": list })).expect_err("no draft there");
    assert!(e.contains("no draft"), "{e}");
    assert_eq!(
        s.store()
            .conn()
            .query_row("SELECT COUNT(*) FROM outbox", [], |r| r.get::<_, i64>(0))
            .expect("the outbox"),
        0
    );
}

/// A draft has to go somewhere: with nothing on screen there is nowhere to
/// put the sheet, and the tool says so instead of opening one nobody sees.
#[test]
fn a_draft_with_no_panel_open_says_where_it_would_go() {
    let (mut s, _clock) = session();
    let e = call(
        &mut s,
        "mail.draft",
        &json!({"to": "a@b.c", "subject": "x", "body": "y"}),
    )
    .expect_err("nowhere to put it");
    assert!(e.contains("no panel has focus"), "{e}");
}

/// Every schema is read before `run` sees the arguments.
#[test]
fn the_tools_read_their_arguments_first() {
    let (mut s, _clock) = session();
    assert_eq!(
        call(&mut s, "mail.archive", &json!({})).expect_err("no conversation"),
        "missing `thread`"
    );
    assert_eq!(
        call(&mut s, "mail.archive", &json!({"thread": "1"})).expect_err("not an id"),
        "`thread` must be an integer"
    );
    assert_eq!(
        call(
            &mut s,
            "mail.draft",
            &json!({"to": "a@b.c", "subject": "x"})
        )
        .expect_err("no letter"),
        "missing `body`"
    );
}
