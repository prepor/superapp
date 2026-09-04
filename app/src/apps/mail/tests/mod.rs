//! Mail, driven through a session with no widget in sight.
//!
//! The accounts and the two panels that configure them are in
//! [`accounts`](self::accounts); the helpers below are shared with it.
//!
//! `Session::fake` gives an in-memory store with mail's schema and seed, its
//! fake servers, and the passes running inline — so a scripted action is
//! followed by its consequences in the same call.

use kernel::app::{App, Apps, Env, Mode};
use kernel::layout::SlotId;
use kernel::nav::Nav;
use kernel::panel::{PanelId, VerbAct};
use kernel::search::{Engine, Go};
use kernel::session::{Action, Session};
use kernel::store::Store;

use super::caps::FakeServers;
use super::model::{self, MailId, Role, Seed};
use super::panels::{AddAccount, Compose, Contact, Mailbox, Message, Settings};
use super::MAIL;

mod accounts;
mod carries;

static APPS: &[&dyn App] = &[&MAIL];

/// A session and the clock it runs on — the send window is the one thing a
/// test has to move time for.
fn session() -> (Session, kernel::caps::ClockSource) {
    let env = Env::default();
    let clock = env.clock.clone();
    (Session::fake_with(APPS, &env), clock)
}

/// This world's servers — what a test plants a mail through, and takes
/// offline.
fn servers(s: &Session) -> FakeServers {
    s.world()
        .caps(|c| c.get::<FakeServers>().map(|f| f.clone()))
        .expect("the fake servers are installed under their own type")
}

/// Opens a root panel, as the launcher would.
fn open_root(s: &mut Session, id: PanelId) -> SlotId {
    let show = id.clone();
    s.act(Action::new("open", format!("open “{id}”")).moving(move |wm| {
        wm.open(show, None, false);
    }));
    s.settle();
    s.focus().expect("the new slot has focus")
}

/// A navigation, settled — what the shell does after every event, and what
/// a test does before it looks at the slots.
fn go(s: &mut Session, n: Nav) {
    s.nav(n);
    s.settle();
}

/// Reaches a mailbox instance.
fn with_mailbox<T>(s: &Session, slot: SlotId, f: impl FnOnce(&mut Mailbox) -> T) -> T {
    let inst = s.panel(slot).expect("a panel in the slot");
    let mut b = inst.borrow_mut();
    f(b.as_any().downcast_mut::<Mailbox>().expect("a mailbox"))
}

/// Runs one of a panel's verbs by id, exactly as the bar does: the bar is
/// pulled again as it fires, and a verb of the panel's own is its own
/// method, with the instance borrowed for the whole of it.
fn verb(s: &mut Session, slot: SlotId, id: &str) {
    let inst = s.panel(slot).expect("a panel in the slot");
    let act = {
        let b = inst.borrow();
        b.verbs().into_iter().find(|v| v.id == id).map(|v| v.act)
    };
    match act {
        Some(VerbAct::Run) => inst.borrow_mut().run(id, s),
        Some(VerbAct::Call(f)) => f(s),
        Some(VerbAct::Go(n)) => s.nav(n),
        None => panic!("no verb {id} on slot {slot}"),
    }
    s.settle();
}

/// The ids on one panel's bar, in the order it wears them.
fn verb_ids(s: &Session, slot: SlotId) -> Vec<&'static str> {
    s.panel(slot)
        .expect("a panel in the slot")
        .borrow()
        .verbs()
        .iter()
        .map(|v| v.id)
        .collect()
}

/// The problems about sends. With the servers down the account cannot reach
/// them either, and that is a standing problem of its own — true, and not
/// what these tests are about.
fn send_problems(s: &Session) -> Vec<kernel::app::Problem> {
    s.problems()
        .into_iter()
        .filter(|p| p.key.starts_with("outbox:"))
        .collect()
}

fn unread(store: &Store, id: MailId) -> bool {
    store
        .conn()
        .query_row("SELECT unread FROM message WHERE id = ?1", [id], |r| {
            r.get(0)
        })
        .unwrap_or(false)
}

fn role_of(store: &Store, id: MailId) -> String {
    model::role_word_of(store, id).unwrap_or_default()
}

fn kinds(s: &Session) -> Vec<String> {
    s.history().rows().0.into_iter().map(|r| r.kind).collect()
}

/// The topics an inbox panel lists, top to bottom.
fn topics(s: &Session, slot: SlotId) -> Vec<String> {
    with_mailbox(s, slot, |m| {
        m.rows(0, 50).into_iter().map(|r| r.topic).collect()
    })
}

// -- the mailbox ---------------------------------------------------------------

/// The inbox lists the demo world's conversations, newest first, and folds a
/// three-message thread into one row.
#[test]
fn the_inbox_lists_the_seeded_threads() {
    let (mut s, _clock) = session();
    let list = open_root(&mut s, Role::Inbox.id());
    assert_eq!(s.panel(list).unwrap().borrow().title(), "inbox");

    // The nine hand-written letters, then the generated tail that makes the
    // inbox overflow — every one of it older, so the top of the list is the
    // demo world proper.
    let rows = with_mailbox(&s, list, |m| m.rows(0, 100));
    assert_eq!(rows.len(), 69, "{:?}", topics(&s, list));
    assert_eq!(rows[0].topic, "Q3 infra budget draft");
    assert!(rows[0].unread);
    assert_eq!(rows[1].topic, "[stelaxis] CI failed on main");

    // Max's two replies and the note they answer are one row, three long,
    // and the row is named after the oldest of them.
    let thread = &rows[2];
    assert_eq!(thread.topic, "superapp panel model");
    assert_eq!(thread.n, 3);
    assert_eq!(thread.who, vec!["Max".to_string(), "me".to_string()]);
    assert_eq!(thread.who_line(), "Max, me · 3");

    // The same list under a filter, materialized: two conversations of it
    // have something unread in them.
    let unread = model::mailbox_filtered(s.store(), Role::Inbox, "@unread");
    assert_eq!(unread.len(), 2);
    assert!(unread.iter().all(|t| t.unread));

    // …and the archive holds the CI runs the inbox's GitHub mail continues,
    // which are one conversation six long.
    let archive = open_root(&mut s, Role::Archive.id());
    assert_eq!(with_mailbox(&s, archive, |m| m.len()), 1);
    let ci = with_mailbox(&s, archive, |m| m.rows(0, 1))[0].clone();
    assert_eq!(ci.n, 6, "five archived runs and the one in the inbox");
}

/// The four mailboxes are four panels over one list, and each shows what its
/// own folder holds.
#[test]
fn the_four_mailboxes_show_their_own_folders() {
    let (mut s, _clock) = session();
    let counts: Vec<(&str, usize)> = model::ROLES
        .into_iter()
        .map(|role| {
            let slot = open_root(&mut s, role.id());
            let n = with_mailbox(&s, slot, |m| m.len());
            (role.as_str(), n)
        })
        .collect();
    assert_eq!(
        counts,
        vec![("inbox", 69), ("archive", 1), ("sent", 1), ("spam", 3)]
    );

    // Sent holds my own note to Max — the conversation read from the other
    // end, so the row is the same three letters.
    let sent = s.showing(&Role::Sent.id())[0];
    let row = with_mailbox(&s, sent, |m| m.rows(0, 1))[0].clone();
    assert_eq!(row.topic, "superapp panel model");
    assert_eq!(row.n, 3);

    // A mailbox opened on a filter is filtered from the first draw, and says
    // so in its title.
    let filtered = open_root(&mut s, Role::Inbox.filtered("vera@kovac.io"));
    assert_eq!(
        s.panel(filtered).unwrap().borrow().title(),
        "inbox · vera@kovac.io"
    );
    assert_eq!(with_mailbox(&s, filtered, |m| m.len()), 1);
    assert_eq!(
        with_mailbox(&s, filtered, |m| m.rows(0, 1))[0].topic,
        "Q3 infra budget draft"
    );
    // The argument is the address; what the field is seeded with is the
    // grammar's own way of saying it, so the filter reads as a filter and
    // can be edited into another one.
    assert_eq!(
        with_mailbox(&s, filtered, |m| m.seed_filter()),
        "@from:vera@kovac.io"
    );
}

/// A reader asks for the rows its conversation reads as: the demo world's one
/// long letter opens taller than a two-paragraph note.
#[test]
fn a_reader_asks_for_the_rows_its_letter_needs() {
    let (mut s, _clock) = session();
    let list = open_root(&mut s, Role::Inbox.id());
    let wish = |s: &Session, mail: MailId| {
        let slot = s.showing(&Message::id(mail))[0];
        s.panel(slot).unwrap().borrow().wish(60)
    };

    go(&mut s, Nav::Open {
        from: list,
        id: Message::id(4),
        fresh: true,
    });
    go(&mut s, Nav::Open {
        from: list,
        id: Message::id(9),
        fresh: true,
    });
    let (short, long) = (wish(&s, 4), wish(&s, 9));
    assert_eq!(short, (4, 3), "a short note keeps the floor");
    assert!(long.1 > short.1, "{long:?} vs {short:?}");

    // A wider column reads the same letter in fewer lines.
    let narrow = s.panel(s.showing(&Message::id(9))[0]).unwrap().borrow().wish(30);
    assert!(narrow.1 >= long.1, "{narrow:?} vs {long:?}");
}

/// A reader unfolds from the first unread letter down: the read run above it
/// folds to header lines, and a letter already read *under* an unread one
/// opens with the rest of the catching up.
#[test]
fn a_reader_unfolds_from_the_first_unread_letter() {
    /// One of the CI conversation's letters, by the run it reports.
    fn ci(s: &Session, run: u32) -> MailId {
        s.store()
            .conn()
            .query_row(
                "SELECT id FROM message WHERE message_id = ?1",
                [format!("ci-{run}@github.com")],
                |r| r.get(0),
            )
            .expect("a seeded CI mail")
    }

    let (mut s, _clock) = session();
    // The CI conversation: five archived runs, read, and the inbox
    // notification that continues them, unread. Another client has flagged
    // the third run unread again, so the catching up starts in the middle of
    // the thread rather than at its end.
    let runs: Vec<MailId> = [4116, 4119, 4121, 4124, 4126, 4128]
        .iter()
        .map(|r| ci(&s, *r))
        .collect();
    let third = runs[2];
    s.store()
        .write(move |c| {
            c.execute("UPDATE message SET unread = 1 WHERE id = ?1", [third])
                .map(|_| ())
        })
        .expect("the flag goes back on");

    let list = open_root(&mut s, Role::Inbox.id());
    go(&mut s, Nav::Open {
        from: list,
        id: Message::id(runs[5]),
        fresh: true,
    });
    let reader = s.showing(&Message::id(runs[5]))[0];
    let inst = s.panel(reader).expect("the reader");
    let mut b = inst.borrow_mut();
    let m = b.as_any().downcast_mut::<Message>().expect("a reader");
    let open: Vec<bool> = runs.iter().map(|id| m.is_open(*id)).collect();
    assert_eq!(
        open,
        vec![false, false, true, true, true, true],
        "folded above the first unread letter, open from it down"
    );
}

/// Every bar mail wears: no letter twice, and none of the ones the workspace
/// keeps for itself.
#[test]
fn no_bar_wears_a_letter_twice_or_a_reserved_one() {
    let (mut s, _clock) = session();
    let list = open_root(&mut s, Role::Inbox.id());
    with_mailbox(&s, list, |m| {
        m.go(0);
        m.toggle_mark();
    });
    let nav = with_mailbox(&s, list, |m| m.go(1)).expect("a row");
    go(&mut s, nav);
    let reader = s.joined_child(list).expect("a reader");
    go(&mut s, Nav::Open {
        from: list,
        id: Compose::id(Seed::Blank),
        fresh: true,
    });
    let sheet = s.focus().expect("a compose");
    let archive = open_root(&mut s, Role::Archive.id());

    for slot in [list, reader, sheet, archive] {
        let verbs = s.panel(slot).unwrap().borrow().verbs();
        assert!(!verbs.is_empty(), "slot {slot} wears nothing");
        let mut seen: Vec<char> = Vec::new();
        for v in &verbs {
            let Some(c) = v.accel else { continue };
            let c = c.to_ascii_lowercase();
            assert!(
                !crate::shell::keys::is_reserved(c),
                "{} wears cmd+{c}, which the workspace keeps",
                v.id
            );
            assert!(!seen.contains(&c), "two verbs on one bar wear cmd+{c}");
            seen.push(c);
        }
    }
}

/// The cursor previews, the preview marks the conversation read, and undo
/// gives the flags back with the panel.
#[test]
fn a_preview_marks_the_thread_read_and_undo_gives_it_back() {
    let (mut s, _clock) = session();
    let list = open_root(&mut s, Role::Inbox.id());
    assert!(unread(s.store(), 1));

    let nav = with_mailbox(&s, list, |m| m.walk(1)).expect("a row to walk onto");
    go(&mut s, nav);

    let reader = s.joined_child(list).expect("the preview joined the list");
    assert_eq!(s.focus(), Some(list), "focus stayed on the list");
    assert_eq!(s.panel(reader).unwrap().borrow().title(), "Q3 infra budget draft");
    assert!(!unread(s.store(), 1), "opening it read it");

    assert!(s.undo());
    assert!(unread(s.store(), 1), "and undo gave the flag back");
    assert!(s.panel(reader).is_none(), "with the slot it opened");
}

/// A walk of previews from one slot is one undo node — the whole walk, not
/// one node a row.
#[test]
fn consecutive_previews_coalesce_into_one_node() {
    let (mut s, _clock) = session();
    let list = open_root(&mut s, Role::Inbox.id());
    let before = kinds(&s).len();

    for _ in 0..4 {
        let nav = with_mailbox(&s, list, |m| m.walk(1)).expect("a row");
        go(&mut s, nav);
    }
    let after = kinds(&s);
    assert_eq!(after.len(), before + 1, "{after:?}");
    assert_eq!(after.last().map(String::as_str), Some("read"));

    // One undo closes the whole walk.
    assert!(s.undo());
    assert!(s.joined_child(list).is_none());
}

/// The reader's *archive* files the conversation, the push pass turns that
/// into a `move` job, and the executor runs it against the server.
#[test]
fn archiving_a_message_files_it_and_reaches_the_server() {
    let (mut s, _clock) = session();
    let list = open_root(&mut s, Role::Inbox.id());
    let nav = with_mailbox(&s, list, |m| m.walk(1)).expect("a row");
    go(&mut s, nav);
    let reader = s.joined_child(list).expect("a reader");

    assert_eq!(role_of(s.store(), 1), "inbox");
    verb(&mut s, reader, "mail.archive");

    assert_eq!(role_of(s.store(), 1), "archive", "the mail moved");
    assert!(s.panel(reader).is_none(), "the reader closed with it");

    // The list moved on: the cursor stands where it stood, which is now the
    // row below, and its preview is joined to the list again.
    let next = s.joined_child(list).expect("the list previewed the next row");
    assert_eq!(
        s.panel(next).unwrap().borrow().title(),
        "[stelaxis] CI failed on main"
    );

    // One `move` job, filed by the push pass and already run — a kick is
    // what every action does, and the inline mount drains what it filed.
    let moves: Vec<_> = s
        .world()
        .jobs()
        .into_iter()
        .filter(|j| j.kind == "move")
        .collect();
    assert_eq!(moves.len(), 1, "{:?}", s.world().jobs());
    assert_eq!(moves[0].status, "done");
    assert_eq!(moves[0].entity.as_deref(), Some("account:1"));

    // …and the server agrees: the letter is out of INBOX and in Archive.
    let n = servers(&s)
        .with(1, |srv| {
            (
                srv.folders["INBOX"].2.len(),
                srv.folders["Archive"].2.len(),
            )
        })
        .expect("the demo account's server");
    assert_eq!(n, (69, 6), "one letter left INBOX for Archive");

    // A tick with nothing left to say files nothing new.
    let before = s.world().jobs().len();
    s.workers().tick();
    assert_eq!(s.world().jobs().len(), before);
}

/// Filing closes the reader the verb ran on and nothing else. A second panel
/// on the same conversation — here on another workspace — is somebody's own
/// window: what it shows has changed folder, which is a fact about the mail
/// and not a reason to take the panel away.
#[test]
fn filing_closes_its_own_slot_and_no_other() {
    let (mut s, _clock) = session();
    let list = open_root(&mut s, Role::Inbox.id());
    let nav = with_mailbox(&s, list, |m| m.walk(1)).expect("a row");
    go(&mut s, nav);
    let reader = s.joined_child(list).expect("a reader");

    // The same conversation, opened for its own sake on workspace 2.
    s.switch(1);
    let other = open_root(&mut s, Message::id(1));
    s.switch(0);

    verb(&mut s, reader, "mail.archive");
    assert_eq!(role_of(s.store(), 1), "archive");
    assert!(s.panel(reader).is_none(), "the reader it ran on closed");
    assert!(
        s.panel(other).is_some(),
        "the panel somebody else opened stayed"
    );
    assert_eq!(
        s.panel(other).unwrap().borrow().title(),
        "Q3 infra budget draft"
    );
}

/// Two marked conversations archived as one action: one node, the walk that
/// follows folded into it, and undo brings the mails and the marks back on
/// one press.
#[test]
fn the_batch_archive_is_one_node_and_undo_restores_the_marks() {
    let (mut s, _clock) = session();
    let list = open_root(&mut s, Role::Inbox.id());
    with_mailbox(&s, list, |m| {
        m.go(0);
        m.toggle_mark();
        m.go(1);
        m.toggle_mark();
    });
    assert_eq!(with_mailbox(&s, list, |m| m.list().marks().len()), 2);

    // The bar says how many, the archive verb is the inbox's alone, and the
    // two verbs about the set itself close it.
    let labels: Vec<String> = s
        .panel(list)
        .unwrap()
        .borrow()
        .verbs()
        .iter()
        .map(|v| v.label.clone())
        .collect();
    assert_eq!(
        labels,
        vec!["sync", "archive 2", "delete 2", "mark all", "clear"]
    );

    let before = kinds(&s).len();
    verb(&mut s, list, "mail.archive");
    // One node for the whole gesture: the batch closes nothing, so what
    // happens to the panel beside the list is a preview — and that preview
    // is the batch arriving at its consequence, folded into its node.
    assert_eq!(kinds(&s).len(), before + 1, "{:?}", kinds(&s));
    assert_eq!(kinds(&s).last().map(String::as_str), Some("file"));
    assert_eq!(role_of(s.store(), 1), "archive");
    assert_eq!(role_of(s.store(), 2), "archive");
    assert_eq!(with_mailbox(&s, list, |m| m.list().marks().len()), 0);
    assert_eq!(with_mailbox(&s, list, |m| m.len()), 67);
    assert_eq!(
        s.panel(s.joined_child(list).expect("the walk previewed a row"))
            .unwrap()
            .borrow()
            .title(),
        "superapp panel model",
        "the row under the two that left, not one further down for each of them"
    );

    // One press takes the whole gesture back — the walk it left behind
    // included.
    assert!(s.undo());
    assert_eq!(role_of(s.store(), 1), "inbox");
    assert_eq!(role_of(s.store(), 2), "inbox");
    assert_eq!(
        with_mailbox(&s, list, |m| m.list().marks().len()),
        2,
        "the marks came back with the mails"
    );
}

/// The archive mailbox has no *archive* verb: there is nowhere for it to go.
#[test]
fn the_archive_has_no_archive_verb() {
    let (mut s, _clock) = session();
    let list = open_root(&mut s, Role::Inbox.id());
    with_mailbox(&s, list, |m| {
        m.go(0);
        m.toggle_mark();
    });
    verb(&mut s, list, "mail.archive");

    let archive = open_root(&mut s, Role::Archive.id());
    with_mailbox(&s, archive, |m| {
        m.go(0);
        m.toggle_mark();
    });
    assert_eq!(with_mailbox(&s, archive, |m| m.list().marks().len()), 1);
    assert_eq!(
        verb_ids(&s, archive),
        vec!["mail.sync", "mail.delete", "mail.all", "mail.clear"]
    );

    // *mark all* takes every row under the filter, and *clear* lets the
    // whole set go.
    verb(&mut s, archive, "mail.all");
    assert_eq!(
        with_mailbox(&s, archive, |m| m.list().marks().len()),
        2,
        "the CI runs and the conversation just filed"
    );
    verb(&mut s, archive, "mail.clear");
    assert_eq!(verb_ids(&s, archive), vec!["mail.sync"]);
}

/// The reader's *archive*, on the other hand, is about the **mail** — so it
/// is on the bar of a letter read out of the archive, and refuses in words
/// rather than recording an action that would move nothing.
#[test]
fn archiving_a_mail_already_in_the_archive_says_so() {
    let (mut s, _clock) = session();
    let list = open_root(&mut s, Role::Archive.id());
    let nav = with_mailbox(&s, list, |m| m.go(0)).expect("the archive's first row");
    go(&mut s, nav);
    let reader = s.joined_child(list).expect("a reader");
    assert!(
        verb_ids(&s, reader).contains(&"mail.archive"),
        "the button is about the mail, so it is there either way"
    );

    let before = kinds(&s).len();
    verb(&mut s, reader, "mail.archive");
    assert_eq!(
        s.notes().last().map(|n| (n.msg.clone(), n.err)),
        Some(("already in the archive".to_string(), true))
    );
    assert_eq!(kinds(&s).len(), before, "a refusal records nothing");
    assert!(s.panel(reader).is_some(), "and closes nothing");
}

/// The spam list wears *not spam* where the inbox wears *archive*, and it
/// puts the marked conversation back in the inbox: one undoable action, one
/// `move` job, and the server agrees.
#[test]
fn the_spam_list_puts_a_conversation_back_in_the_inbox() {
    let (mut s, _clock) = session();
    let spam = open_root(&mut s, Role::Spam.id());
    let mail = with_mailbox(&s, spam, |m| {
        m.go(0);
        m.toggle_mark();
        m.rows(0, 1)[0].target
    });
    assert_eq!(role_of(s.store(), mail), "spam");

    let labels: Vec<String> = s
        .panel(spam)
        .unwrap()
        .borrow()
        .verbs()
        .iter()
        .map(|v| v.label.clone())
        .collect();
    assert_eq!(
        labels,
        vec!["sync", "not spam 1", "delete 1", "mark all", "clear"],
        "the junk keeps a conversation the way the inbox archives one"
    );

    let before = kinds(&s).len();
    verb(&mut s, spam, "mail.not_spam");
    assert_eq!(role_of(s.store(), mail), "inbox", "it came out of the junk");
    assert_eq!(
        with_mailbox(&s, spam, |m| m.len()),
        2,
        "the list is shorter"
    );
    assert_eq!(with_mailbox(&s, spam, |m| m.list().marks().len()), 0);
    // One node for the whole gesture: the walk it left behind is the batch
    // arriving at its consequence, folded into its node.
    assert_eq!(kinds(&s).len(), before + 1, "{:?}", kinds(&s));
    assert_eq!(kinds(&s).last().map(String::as_str), Some("file"));

    // The push pass turned the move into a job, and the letter is where the
    // list says it is on the server too.
    let moves: Vec<_> = s
        .world()
        .jobs()
        .into_iter()
        .filter(|j| j.kind == "move")
        .collect();
    assert_eq!(moves.len(), 1, "{:?}", s.world().jobs());
    assert_eq!(moves[0].status, "done");
    let n = servers(&s)
        .with(1, |srv| {
            (srv.folders["INBOX"].2.len(), srv.folders["Spam"].2.len())
        })
        .expect("the demo account's server");
    assert_eq!(n, (71, 2), "one letter left Spam for INBOX");

    // One press takes the whole gesture back: the mail goes to the junk and
    // the mark goes back on its row.
    assert!(s.undo());
    assert_eq!(role_of(s.store(), mail), "spam");
    assert_eq!(
        with_mailbox(&s, spam, |m| m.list().marks().len()),
        1,
        "the mark came back with the mail"
    );
}

/// The reader's *not spam* is the one button on it that comes and goes: a
/// letter read out of the junk wears it, one read out of the inbox does not,
/// and pressing it files the conversation and closes the reader like any
/// other filing.
#[test]
fn the_reader_wears_not_spam_over_a_letter_in_the_junk() {
    let (mut s, _clock) = session();
    let inbox = open_root(&mut s, Role::Inbox.id());
    let nav = with_mailbox(&s, inbox, |m| m.go(0)).expect("the inbox's first row");
    go(&mut s, nav);
    let reader = s.joined_child(inbox).expect("a reader");
    assert!(
        !verb_ids(&s, reader).contains(&"mail.not_spam"),
        "nothing in the inbox is junk to begin with"
    );

    let spam = open_root(&mut s, Role::Spam.id());
    let nav = with_mailbox(&s, spam, |m| m.go(1)).expect("the junk's second row");
    go(&mut s, nav);
    let reader = s.joined_child(spam).expect("a reader");
    let mail = {
        let inst = s.panel(reader).unwrap();
        let mut b = inst.borrow_mut();
        b.as_any().downcast_mut::<Message>().unwrap().mail()
    };
    assert_eq!(
        verb_ids(&s, reader),
        vec![
            "mail.archive",
            "mail.not_spam",
            "mail.delete",
            "mail.reply",
            "mail.forward"
        ]
    );

    verb(&mut s, reader, "mail.not_spam");
    assert_eq!(role_of(s.store(), mail), "inbox");
    assert!(s.panel(reader).is_none(), "the reader closed with it");
    assert!(
        s.joined_child(spam).is_some(),
        "and the list it was read from moved on to the next row"
    );
}

// -- the send flow ---------------------------------------------------------------

/// A reply written, sent after its window, handed to the submission server,
/// and back in the conversation it answered.
#[test]
fn a_reply_goes_out_and_the_sent_copy_joins_the_thread() {
    let (mut s, clock) = session();
    let list = open_root(&mut s, Role::Inbox.id());

    // The reader, then the reply — the two links a message panel wears.
    let nav = with_mailbox(&s, list, |m| m.go(2)).expect("the conversation's row");
    go(&mut s, nav);
    let reader = s.joined_child(list).expect("a reader");
    let mail = {
        let inst = s.panel(reader).unwrap();
        let mut b = inst.borrow_mut();
        b.as_any().downcast_mut::<Message>().unwrap().mail()
    };
    verb(&mut s, reader, "mail.reply");
    let sheet = s.focus().expect("the compose took focus");
    {
        let inst = s.panel(sheet).unwrap();
        let mut b = inst.borrow_mut();
        let c = b.as_any().downcast_mut::<Compose>().expect("a compose");
        assert_eq!(c.seed(), Seed::Reply(mail));
        assert_eq!(c.draft().to, "max@ivanov.dev");
        assert!(c.draft().subject.starts_with("Re: superapp panel model"));
        assert!(c.draft().body.contains("wrote:"), "{}", c.draft().body);
        c.edited(&c.draft().to.clone(), &c.draft().subject.clone(), "Agreed.");
    }

    verb(&mut s, sheet, "mail.send");
    assert!(s.panel(sheet).is_none(), "the sheet closed behind the send");
    let (status, after): (String, f64) = s
        .store()
        .conn()
        .query_row(
            "SELECT status, send_after FROM outbox WHERE id = ?1",
            [sheet as i64],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .expect("an outbox row under the compose's own slot");
    assert_eq!(status, "pending");
    assert!(after > s.now(), "the window has not run out yet");
    assert!(servers(&s).submitted().is_empty(), "nothing has left yet");

    // The window runs out, the sender claims the row, and the letter goes.
    clock.advance(model::send_delay() + 1.0);
    s.workers().kick_all();
    let sent = servers(&s).submitted();
    assert_eq!(sent.len(), 1);
    assert_eq!(sent[0].to, "max@ivanov.dev");
    assert_eq!(sent[0].body, "Agreed.");
    assert!(sent[0].in_reply_to.is_some(), "it says what it answers");
    let done: String = s
        .store()
        .conn()
        .query_row("SELECT status FROM outbox WHERE id = ?1", [sheet as i64], |r| {
            r.get(0)
        })
        .unwrap();
    assert_eq!(done, "sent");

    // The copy the transport filed to Sent comes back on the next pass, and
    // threads with the letter it answered.
    s.workers().kick_all();
    let thread = model::thread(s.store(), mail);
    assert!(
        thread.iter().any(|t| t.role == "sent" && t.mail.body == "Agreed."),
        "{:?}",
        thread.iter().map(|t| (&t.role, &t.mail.body)).collect::<Vec<_>>()
    );
}

/// With the servers down, a send that failed stands as a problem — and the
/// problem's own *retry* files it again.
#[test]
fn a_failing_send_is_a_problem_that_retry_refiles() {
    let (mut s, clock) = session();
    let list = open_root(&mut s, Role::Inbox.id());
    let sheet = {
        go(&mut s, Nav::Open {
            from: list,
            id: Compose::id(Seed::Blank),
            fresh: true,
        });
        s.focus().expect("the compose took focus")
    };
    {
        let inst = s.panel(sheet).unwrap();
        let mut b = inst.borrow_mut();
        b.as_any()
            .downcast_mut::<Compose>()
            .unwrap()
            .edited("vera@kovac.io", "the numbers", "checked, they hold.");
    }
    verb(&mut s, sheet, "mail.send");

    servers(&s).set_down(Some("no route to host"));
    clock.advance(model::send_delay() + 1.0);
    s.workers().kick_all();
    assert!(servers(&s).submitted().is_empty(), "nothing left the process");

    let problems = send_problems(&s);
    assert_eq!(problems.len(), 1, "{problems:?}");
    assert_eq!(problems[0].key, format!("outbox:{sheet}"));
    assert_eq!(problems[0].label, "send “the numbers”");
    assert_eq!(problems[0].line, "no route to host");
    assert!(problems[0].detail.contains("to vera@kovac.io"));
    let ids: Vec<&str> = problems[0].verbs.iter().map(|v| v.id).collect();
    assert_eq!(ids, vec!["mail.retry", "mail.reopen"]);

    // *retry* files the send again, on a fresh window — so the condition the
    // problem was derived from is gone.
    let retry = problems[0]
        .verbs
        .iter()
        .find(|v| v.id == "mail.retry")
        .map(|v| match &v.act {
            VerbAct::Call(f) => f.clone(),
            VerbAct::Run | VerbAct::Go(_) => panic!("retry is a button of its own"),
        })
        .expect("a retry verb");
    retry(&mut s);
    let status: String = s
        .store()
        .conn()
        .query_row("SELECT status FROM outbox WHERE id = ?1", [sheet as i64], |r| {
            r.get(0)
        })
        .unwrap();
    assert_eq!(status, "pending");
    assert!(
        send_problems(&s).is_empty(),
        "and the problem cleared with it"
    );

    // With the servers back, the retried letter goes out.
    servers(&s).set_down(None);
    clock.advance(model::send_delay() + 1.0);
    s.workers().kick_all();
    assert_eq!(servers(&s).submitted().len(), 1);
}

// -- search ------------------------------------------------------------------------

/// The launcher's mail source finds a letter by a word of its subject, and
/// the hit opens the panel that reads it.
#[test]
fn the_search_provider_finds_a_mail_by_its_subject() {
    let (s, _clock) = session();
    let mut engine = Engine::inline(s.apps().providers());
    assert_eq!(engine.slots(), 1, "mail supplies exactly one source");

    // A word out of a letter's subject reaches the letter — and its sender
    // reaches a card, which is what the launcher offers a contact as.
    engine.ask(s.store(), 1, "airport");
    let hits: Vec<_> = engine.collect().into_iter().flat_map(|a| a.hits).collect();
    assert_eq!(hits.len(), 1, "{hits:?}");
    assert_eq!(hits[0].label, "that airport book");
    assert_eq!(hits[0].detail, "Dmitry Orlov");
    assert_eq!(hits[0].go, Go::Open(Message::id(8)));

    // A sender's name reaches their card first, then their letters.
    engine.ask(s.store(), 2, "kovac");
    let hits: Vec<_> = engine.collect().into_iter().flat_map(|a| a.hits).collect();
    assert_eq!(hits.len(), 2, "{hits:?}");
    assert_eq!(hits[0].label, "Vera Kovac");
    assert_eq!(hits[0].go, Go::Open(Contact::id("vera@kovac.io")));
    assert_eq!(hits[1].label, "Q3 infra budget draft");

    // The index is type-ahead: a prefix of two words finds the letter on the
    // fourth keystroke, and the words may come in any order.
    engine.ask(s.store(), 4, "budg q3");
    let hits: Vec<_> = engine.collect().into_iter().flat_map(|a| a.hits).collect();
    assert_eq!(hits.len(), 1, "{hits:?}");
    assert_eq!(hits[0].label, "Q3 infra budget draft");

    // A word only the *body* carries reaches it too — the index reads the
    // letter, which `LIKE` over the headers never did.
    engine.ask(s.store(), 5, "thermos");
    let hits: Vec<_> = engine.collect().into_iter().flat_map(|a| a.hits).collect();
    assert_eq!(hits.len(), 1, "{hits:?}");
    assert_eq!(hits[0].label, "Sat hike — early start?");

    // Nothing the launcher offers came out of the junk folder.
    engine.ask(s.store(), 6, "crypt0");
    let hits: Vec<_> = engine.collect().into_iter().flat_map(|a| a.hits).collect();
    assert!(
        hits.iter().all(|h| h.go != Go::Open(Contact::id("no-reply@crypt0-rewards.biz"))),
        "{hits:?}"
    );

    engine.ask(s.store(), 3, "zzz");
    assert!(engine
        .collect()
        .into_iter()
        .all(|a| a.hits.is_empty()));
}

// -- the shape of the app ------------------------------------------------------------

/// The tags mail owns, the workers it asks for, and the roots it offers.
#[test]
fn the_app_registers_its_tags_workers_and_roots() {
    let (mut s, _clock) = session();
    let tags: Vec<&str> = s.apps().tags().iter().map(|t| t.as_str()).collect();
    assert_eq!(
        tags,
        vec![
            "add_account",
            "archive",
            "attachment",
            "compose",
            "contact",
            "inbox",
            "message",
            "sent",
            "settings",
            "spam"
        ]
    );

    let roots: Vec<String> = s.roots().into_iter().map(|r| r.label).collect();
    assert_eq!(
        roots,
        vec!["inbox", "archive", "sent", "spam", "new mail", "settings"]
    );

    // The passes follow the store: one account is configured, so one sync
    // worker runs beside the sender.
    open_root(&mut s, Role::Inbox.id());
    assert_eq!(
        s.workers().names(),
        vec!["sender".to_string(), "sync-1".to_string()]
    );

    // Every deferred effect this build can read back.
    assert_eq!(
        s.world().registry().kinds(),
        vec!["forwarded", "move", "seen", "submit"]
    );
}

/// A real run's demo account is the same letters with no hosts: there is no
/// `imap.demo` out there, so nothing syncs for it and the sender is the only
/// pass. Every other mode keeps the hosts, which is what the test above
/// syncs against.
#[test]
fn a_real_seed_leaves_the_demo_account_without_hosts() {
    let apps = Apps::new(APPS);
    let store = Store::open(None, &apps.schemas()).expect("in-memory store");
    apps.seed(&store, Mode::Real).expect("the demo rows");

    let hosts: (Option<String>, Option<String>) = store
        .conn()
        .query_row("SELECT imap_host, smtp_host FROM account", [], |r| {
            Ok((r.get(0)?, r.get(1)?))
        })
        .expect("the demo account is there all the same");
    assert_eq!(hosts, (None, None));

    let mails: i64 = store
        .conn()
        .query_row("SELECT COUNT(*) FROM message", [], |r| r.get(0))
        .unwrap();
    assert!(mails > 0, "the demo mail is in every fresh store");

    let names: Vec<String> = super::sync::workers(&store)
        .iter()
        .map(|w| w.name())
        .collect();
    assert_eq!(names, vec!["sender".to_string()], "no pass for a hostless account");
}

/// A compose's identity round-trips through its arguments, which is the one
/// spelling of them there is.
#[test]
fn a_compose_names_what_it_started_from() {
    for seed in [Seed::Blank, Seed::Reply(42), Seed::Forward(7)] {
        assert_eq!(Compose::of(&Compose::id(seed)), Some(seed));
    }
    assert_eq!(Compose::id(Seed::Reply(42)).to_string(), "compose(reply, 42)");
    assert_eq!(Compose::id(Seed::Blank).to_string(), "compose");
    assert_eq!(Message::of(&Message::id(42)), Some(42));
    assert_eq!(Message::of(&Role::Inbox.id()), None);
    // An argument this build cannot read is a blank sheet, which is the one
    // seed that cannot be wrong.
    assert_eq!(
        Compose::of(&PanelId::new(Compose::TAG, ["reply", "not a number"])),
        Some(Seed::Blank)
    );
}

/// What a conversation is called, whichever of its letters you read it off.
#[test]
fn a_topic_is_the_subject_without_its_prefixes() {
    assert_eq!(model::topic_of("Re: superapp panel model"), "superapp panel model");
    assert_eq!(model::topic_of("RE[2]: Fwd: q3"), "q3");
    assert_eq!(model::topic_of("re (3): hike"), "hike");
    assert_eq!(model::topic_of("Re:"), "Re:", "nothing left but the prefix");
    assert_eq!(model::topic_of("  spaced  "), "spaced");
}

// -- the four roles, and the card off one -------------------------------------

/// A mailbox's `@from:` completes against the people who wrote *to it*: the
/// spam list offers its own senders and nobody else's, and the other three
/// offer the correspondents.
#[test]
fn a_mailbox_completes_against_its_own_senders() {
    let (s, _clock) = session();
    let of = |role: Role, typed: &str| -> Vec<String> {
        let src = model::threads(role);
        (src.suggest)(s.store(), "from", typed)
            .into_iter()
            .map(|g| g.value)
            .collect()
    };
    assert_eq!(of(Role::Inbox, "kov"), vec!["vera@kovac.io".to_string()]);
    assert!(
        of(Role::Inbox, "crypt").is_empty(),
        "a spammer is not a correspondent"
    );
    assert_eq!(
        of(Role::Spam, "crypt"),
        vec!["no-reply@crypt0-rewards.biz".to_string()]
    );
    assert!(
        of(Role::Spam, "kov").is_empty(),
        "and the spam list offers nobody else"
    );
}


// -- the compose sheet's recipients ----------------------------------------------

/// The TO field completes the token under the caret against the senders the
/// store knows, by name or by address; a pick lands the bare address over
/// that token and leaves the rest of the line alone.
#[test]
fn recipients_complete_the_token_under_the_caret() {
    use kernel::richtable::{Completion, Suggestion};

    use super::recipients::Recipients;

    let (session, _clock) = session();
    let store = session.store();
    let r = Recipients;
    let ctx = |text: &str| r.context(text, text.len());
    let labels = |v: Vec<Suggestion>| v.into_iter().map(|g| g.label).collect::<Vec<_>>();

    // An empty token is nothing to complete: landing in the field, or typing
    // the comma for the next address, opens no box.
    assert_eq!(ctx(""), None);
    assert_eq!(ctx("vera@kovac.io, "), None);

    // Name or address, as a substring — the way `@from:` matches.
    let c = ctx("kov").expect("a token");
    assert_eq!((c.start, c.partial.as_str()), (0, "kov"));
    assert_eq!(labels(r.offer(store, &c)), vec!["Vera Kovac"]);
    assert_eq!(
        labels(r.offer(store, &ctx("ELENA").expect("a token"))),
        vec!["Elena Petrova"]
    );
    let vera = r.offer(store, &c).remove(0);
    assert_eq!(
        (vera.value.as_str(), vera.describe.as_str()),
        ("vera@kovac.io", "vera@kovac.io"),
        "the row shows the name and says the address"
    );
    assert_eq!(
        r.splice("kov", 3, &c, &vera),
        ("vera@kovac.io".to_string(), 13)
    );

    // A second recipient: the token starts after the comma and the space
    // after it, the address already in the line is not offered again, and
    // the splice keeps it.
    let text = "vera@kovac.io, v";
    let c = ctx(text).expect("a token");
    assert_eq!((c.start, c.partial.as_str()), (15, "v"));
    assert_eq!(c.taken, vec!["vera@kovac.io".to_string()]);
    let offer = r.offer(store, &c);
    assert!(
        offer.iter().all(|g| g.value != "vera@kovac.io"),
        "{offer:?}"
    );
    let max = offer
        .iter()
        .find(|g| g.label == "Max Ivanov")
        .expect("Ivanov has a v in it");
    assert_eq!(
        r.splice(text, text.len(), &c, max),
        ("vera@kovac.io, max@ivanov.dev".to_string(), 29)
    );

    // Typed out in full, an address needs no completing.
    assert!(r
        .offer(store, &ctx("vera@kovac.io").expect("a token"))
        .is_empty());

    // The caret in the middle of the line completes the token it is in and
    // leaves what follows alone.
    let text = "ele, max@ivanov.dev";
    let c = r.context(text, 3).expect("a token");
    assert_eq!((c.start, c.partial.as_str()), (0, "ele"));
    assert_eq!(c.taken, vec!["max@ivanov.dev".to_string()]);
    let elena = r.offer(store, &c).remove(0);
    assert_eq!(
        r.splice(text, 3, &c, &elena),
        ("elena.p@gmail.com, max@ivanov.dev".to_string(), 17)
    );

    // A `Name <addr>` token counts by its address.
    let c = ctx("Vera Kovac <vera@kovac.io>, ver").expect("a token");
    assert_eq!(c.taken, vec!["vera@kovac.io".to_string()]);
    assert!(r.offer(store, &c).is_empty());

    // Spam is not in the list: nothing a compose offers came out of the junk.
    assert!(r.offer(store, &ctx("crypt").expect("a token")).is_empty());
}

// -- forwarding ------------------------------------------------------------------

/// A forward is a link like a reply — opening a sheet claims nothing — and
/// the `$Forwarded` keyword lands when the letter has actually left. The push
/// pass then tells the server, which is where every other client reads its
/// arrow from.
#[test]
fn a_forward_marks_the_letter_once_it_has_left() {
    let (mut s, clock) = session();
    let list = open_root(&mut s, Role::Inbox.id());
    let nav = with_mailbox(&s, list, |m| m.go(0)).expect("the first row");
    go(&mut s, nav);
    let reader = s.joined_child(list).expect("a reader");

    let forwarded = |s: &Session| model::mail(s.store(), 1).expect("the mail").forwarded;
    assert!(!forwarded(&s));

    verb(&mut s, reader, "mail.forward");
    let sheet = s.focus().expect("the sheet took focus");
    let (seed, subject) = {
        let inst = s.panel(sheet).unwrap();
        let mut b = inst.borrow_mut();
        let c = b.as_any().downcast_mut::<Compose>().unwrap();
        (c.seed(), c.draft().subject.clone())
    };
    assert_eq!(seed, Seed::Forward(1));
    assert_eq!(subject, "Fwd: Q3 infra budget draft");
    assert!(!forwarded(&s), "opening a sheet claims nothing");

    {
        let inst = s.panel(sheet).unwrap();
        let mut b = inst.borrow_mut();
        let c = b.as_any().downcast_mut::<Compose>().unwrap();
        let (subject, body) = (c.draft().subject.clone(), c.draft().body.clone());
        c.edited("max@ivanov.dev", &subject, &body);
    }
    verb(&mut s, sheet, "mail.send");
    clock.advance(model::send_delay() + 1.0);
    s.workers().kick_all();
    assert_eq!(servers(&s).submitted().len(), 1, "the letter left");
    assert!(forwarded(&s), "and the one it passed on wears the keyword");

    // The push pass tells the server, and the server agrees.
    s.workers().kick_all();
    let marked = servers(&s)
        .with(1, |srv| {
            srv.folders["INBOX"].2.iter().filter(|m| m.forwarded).count()
        })
        .expect("the demo account's server");
    assert_eq!(marked, 2, "the seeded one, and the one just passed on");
}

/// A server that keeps no keywords is never told: the mark stays local
/// truth, and its silence is not read back as another client clearing it.
#[test]
fn a_keywordless_server_is_never_told_about_the_mark() {
    let (mut s, clock) = session();
    servers(&s).with(1, |srv| srv.keywords = false);
    // The folder rows learn it on the next pass.
    s.workers().kick_all();
    let keeps: bool = s
        .store()
        .conn()
        .query_row(
            "SELECT keywords FROM folder WHERE account = 1 AND role = 'inbox'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert!(!keeps, "the folder row records what the server said");

    let list = open_root(&mut s, Role::Inbox.id());
    let nav = with_mailbox(&s, list, |m| m.go(0)).expect("the first row");
    go(&mut s, nav);
    let reader = s.joined_child(list).expect("a reader");
    verb(&mut s, reader, "mail.forward");
    let sheet = s.focus().expect("the sheet took focus");
    {
        let inst = s.panel(sheet).unwrap();
        let mut b = inst.borrow_mut();
        let c = b.as_any().downcast_mut::<Compose>().unwrap();
        let (subject, body) = (c.draft().subject.clone(), c.draft().body.clone());
        c.edited("max@ivanov.dev", &subject, &body);
    }
    verb(&mut s, sheet, "mail.send");
    clock.advance(model::send_delay() + 1.0);
    s.workers().kick_all();
    s.workers().kick_all();

    assert!(
        model::mail(s.store(), 1).expect("the mail").forwarded,
        "the mark stands locally"
    );
    assert!(
        !s.world().jobs().iter().any(|j| j.kind == "forwarded"),
        "and nothing was queued for it: {:?}",
        s.world().jobs()
    );
}

// -- the accounts -----------------------------------------------------------------

// -- reopening a failed send -------------------------------------------------------

/// A send that failed, reopened as a sheet: the letter that failed is the
/// letter in the sheet, and one undo puts the failure back.
#[test]
fn a_failed_send_reopens_with_its_own_text() {
    let (mut s, clock) = session();
    servers(&s).set_down(Some("no route to host"));

    // A letter that cannot leave.
    let sheet = open_root(&mut s, Compose::id(Seed::Blank));
    {
        let inst = s.panel(sheet).unwrap();
        let mut b = inst.borrow_mut();
        let c = b.as_any().downcast_mut::<Compose>().unwrap();
        c.edited("vera@kovac.io", "the one that failed", "and its body");
    }
    verb(&mut s, sheet, "mail.send");
    clock.advance(model::send_delay() + 1.0);
    for _ in 0..8 {
        s.workers().kick_all();
        clock.advance(600.0);
    }
    let problems = send_problems(&s);
    assert_eq!(problems.len(), 1, "{problems:?}");
    assert_eq!(problems[0].key, format!("outbox:{sheet}"));

    // *reopen*: a fresh sheet on the same letter.
    let reopen = problems[0]
        .verbs
        .iter()
        .find(|v| v.id == "mail.reopen")
        .map(|v| match &v.act {
            VerbAct::Call(f) => f.clone(),
            _ => panic!("reopen is a call"),
        })
        .expect("the reopen verb");
    reopen(&mut s);
    s.settle();
    let fresh = s.focus().expect("the sheet took focus");
    assert_ne!(fresh, sheet);
    let draft = {
        let inst = s.panel(fresh).unwrap();
        let mut b = inst.borrow_mut();
        b.as_any().downcast_mut::<Compose>().unwrap().draft().clone()
    };
    assert_eq!(draft.to, "vera@kovac.io");
    assert_eq!(draft.subject, "the one that failed");
    assert_eq!(draft.body, "and its body");
    assert!(
        send_problems(&s).is_empty(),
        "the failed row went with it: {:?}",
        s.problems()
    );

    // One undo: the sheet closes and the failure is back, with its error.
    assert!(s.undo());
    s.settle();
    assert!(s.panel(fresh).is_none());
    let back = send_problems(&s);
    assert_eq!(back.len(), 1, "{back:?}");
    assert!(back[0].line.contains("no route to host"), "{:?}", back[0]);
}

/// *retry* files the send again, and giving it back puts the failure back —
/// the row and the job — so the letter stays reachable.
#[test]
fn a_retry_is_taken_back_with_its_failure() {
    let (mut s, clock) = session();
    servers(&s).set_down(Some("no route to host"));
    let sheet = open_root(&mut s, Compose::id(Seed::Blank));
    {
        let inst = s.panel(sheet).unwrap();
        let mut b = inst.borrow_mut();
        b.as_any()
            .downcast_mut::<Compose>()
            .unwrap()
            .edited("vera@kovac.io", "retried", "body");
    }
    verb(&mut s, sheet, "mail.send");
    clock.advance(model::send_delay() + 1.0);
    for _ in 0..8 {
        s.workers().kick_all();
        clock.advance(600.0);
    }
    assert_eq!(send_problems(&s).len(), 1);

    let retry = send_problems(&s)[0]
        .verbs
        .iter()
        .find(|v| v.id == "mail.retry")
        .map(|v| match &v.act {
            VerbAct::Call(f) => f.clone(),
            _ => panic!("retry is a call"),
        })
        .expect("the retry verb");
    retry(&mut s);
    s.settle();
    let status: String = s
        .store()
        .conn()
        .query_row("SELECT status FROM outbox WHERE id = ?1", [sheet as i64], |r| {
            r.get(0)
        })
        .unwrap();
    assert_eq!(status, "pending", "filed again, on a fresh window");
    assert!(send_problems(&s).is_empty());

    assert!(s.undo());
    s.settle();
    let back = send_problems(&s);
    assert_eq!(back.len(), 1, "{back:?}");
    assert!(back[0].line.contains("no route to host"));
}

// -- the search index --------------------------------------------------------------

/// The index is maintained by triggers in the database, not by this build:
/// a letter that arrives through a plain insert is findable without anything
/// re-indexing it.
#[test]
fn the_index_follows_a_write() {
    let (s, _clock) = session();
    let hits = |q: &str| {
        let mut engine = Engine::inline(s.apps().providers());
        engine.ask(s.store(), 1, q);
        engine
            .collect()
            .into_iter()
            .flat_map(|a| a.hits)
            .map(|h| h.label)
            .collect::<Vec<_>>()
    };
    assert!(hits("bathysphere").is_empty());
    s.store()
        .write(|c| {
            c.execute(
                "INSERT INTO message(account, folder, from_name, from_email, subject,
                                     date, unread, body, topic)
                 SELECT 1, id, 'Nobody', 'nobody@example.org', 'the bathysphere',
                        0, 0, 'down it went', 'the bathysphere'
                   FROM folder WHERE account = 1 AND role = 'inbox'",
                [],
            )
            .map(|_| ())
        })
        .expect("a plain insert");
    assert_eq!(hits("bathysphere"), vec!["the bathysphere".to_string()]);
}

/// What the launcher asks the index, out of what a person typed: every word
/// its own quoted prefix term, and nothing that could pass for an operator.
#[test]
fn the_match_string_quotes_what_was_typed() {
    assert_eq!(
        model::fts_match("q3 budget").as_deref(),
        Some("\"q3\"* AND \"budget\"*")
    );
    assert_eq!(
        model::fts_match("vera@kovac.io").as_deref(),
        Some("\"vera\"* AND \"kovac\"* AND \"io\"*")
    );
    assert_eq!(model::fts_match("  "), None);
    assert_eq!(model::fts_match("*"), None);
    // An operator typed into the box is a word, not an operator.
    let m = model::fts_match("a OR b").expect("three words");
    assert!(m.starts_with('"'), "{m}");
    assert_eq!(m.matches(" AND ").count(), 2, "{m}");
}

// -- the bars ----------------------------------------------------------------------

/// Every bar mail wears: no duplicate letter, and none of the workspace's
/// own. The shell asserts this in a debug build; each app tests its own.
#[test]
fn every_bar_wears_its_letters_once() {
    let (mut s, _clock) = session();
    let mut slots = vec![
        open_root(&mut s, Settings::id()),
        open_root(&mut s, AddAccount::id()),
        open_root(&mut s, Contact::id("max@ivanov.dev")),
        open_root(&mut s, Compose::id(Seed::Blank)),
    ];
    for role in model::ROLES {
        let slot = open_root(&mut s, role.id());
        // With marks, which is when a list wears its most.
        with_mailbox(&s, slot, |m| {
            m.go(0);
            m.toggle_mark();
        });
        slots.push(slot);
    }
    let list = s.showing(&Role::Inbox.id())[0];
    let nav = with_mailbox(&s, list, |m| m.go(0)).expect("a row");
    go(&mut s, nav);
    slots.push(s.joined_child(list).expect("a reader"));

    for slot in slots {
        let inst = s.panel(slot).expect("a panel");
        let verbs = inst.borrow().verbs();
        crate::shell::bar::check(&verbs);
        let mut seen: Vec<char> = Vec::new();
        for v in &verbs {
            if let Some(c) = v.accel {
                assert!(!seen.contains(&c), "{} wears cmd+{c} twice", v.id);
                seen.push(c);
            }
        }
    }
    assert_eq!(super::panels::settings::ACCEL_ADD_ACCOUNT, 'd');
}


/// A draft row written by something other than the panel — a device-sync
/// pass materializing the other machine's half-written letter — is read back
/// on the next look, so the sheet shows what is in the store rather than what
/// this device last typed into it.
#[test]
fn a_compose_rereads_a_draft_written_under_it() {
    let (mut s, _clock) = session();
    let sheet = open_root(&mut s, Compose::id(Seed::Blank));
    let draft = |s: &Session| {
        let inst = s.panel(sheet).unwrap();
        let mut b = inst.borrow_mut();
        b.as_any().downcast_mut::<Compose>().unwrap().draft().clone()
    };
    assert_eq!(draft(&s), model::Draft::default());

    // The other device's row lands under this slot's id.
    let key = sheet as i64;
    let text = model::Draft {
        to: "vera@kovac.io".into(),
        subject: "from over there".into(),
        body: "half written on the other machine".into(),
    };
    let planted = text.clone();
    s.store()
        .write(move |c| model::upsert_draft_tx(c, key, Seed::Blank, &planted, 0.0))
        .expect("the replicated row");

    // Nothing has told the panel; the next look is what does.
    let moved = {
        let inst = s.panel(sheet).unwrap();
        let mut b = inst.borrow_mut();
        b.as_any().downcast_mut::<Compose>().unwrap().reread()
    };
    assert!(moved, "the row moved under it");
    assert_eq!(draft(&s), text);

    // …and a second look changes nothing, so a widget re-seeds its fields
    // once rather than on every draw.
    let again = {
        let inst = s.panel(sheet).unwrap();
        let mut b = inst.borrow_mut();
        b.as_any().downcast_mut::<Compose>().unwrap().reread()
    };
    assert!(!again);
}

/// Every id a mail claims to belong to is one row, and the pair is the
/// table's key — which is what lets device sync record it at all.
#[test]
fn a_reference_is_one_row_per_id() {
    let (s, _clock) = session();
    let refs = |id: MailId| -> Vec<String> {
        s.store()
            .conn()
            .prepare("SELECT mid FROM reference WHERE message = ?1 ORDER BY mid")
            .and_then(|mut q| {
                q.query_map([id], |r| r.get::<_, String>(0))?
                    .collect::<rusqlite::Result<Vec<_>>>()
            })
            .unwrap_or_default()
    };
    // Max's second reply names two conversations.
    let pm2 = s
        .store()
        .conn()
        .query_row(
            "SELECT id FROM message WHERE message_id = 'pm-2@ivanov.dev'",
            [],
            |r| r.get::<_, MailId>(0),
        )
        .expect("the seeded reply");
    assert_eq!(
        refs(pm2),
        vec!["pm-0@prepor.dev".to_string(), "pm-1@ivanov.dev".to_string()]
    );

    // The same id twice is one row, not two: threading asks "is it named",
    // never "how often".
    s.store()
        .write(move |c| {
            model::thread_tx(
                c,
                1,
                pm2,
                "pm-2@ivanov.dev",
                &[
                    "pm-0@prepor.dev".to_string(),
                    "pm-0@prepor.dev".to_string(),
                    "pm-1@ivanov.dev".to_string(),
                ],
            )
        })
        .expect("threading again");
    assert_eq!(
        refs(pm2),
        vec!["pm-0@prepor.dev".to_string(), "pm-1@ivanov.dev".to_string()]
    );

    // And the table has the key device sync records it by.
    let pk: i64 = s
        .store()
        .conn()
        .query_row(
            "SELECT COUNT(*) FROM pragma_table_info('reference') WHERE pk > 0",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(pk, 2, "message and mid together");
}

/// A delete from the reader is **one** undo, not two. The filing and the
/// cursor walk it leaves behind are one gesture, so they are one node: the
/// press that follows puts the mail back, reopens the reader on it, and
/// takes the walk's own claim — the next conversation marked read — back
/// with it.
#[test]
fn deleting_from_a_reader_is_one_undo() {
    let (mut s, _clock) = session();
    let list = open_root(&mut s, Role::Inbox.id());
    let nav = with_mailbox(&s, list, |m| m.go(0)).expect("a row");
    go(&mut s, nav);
    let reader = s.joined_child(list).expect("a reader");
    let rows = with_mailbox(&s, list, |m| m.len());
    let before = kinds(&s).len();

    verb(&mut s, reader, "mail.delete");
    assert_eq!(role_of(s.store(), 1), "trash");
    assert!(s.panel(reader).is_none(), "the reader closed with it");
    assert_eq!(kinds(&s).len(), before + 1, "{:?}", kinds(&s));
    assert_eq!(kinds(&s).last().map(String::as_str), Some("file"));
    // The walk that followed marked what it landed on read.
    let next = s.joined_child(list).expect("the walk previewed a row");
    let landed = Message::of(s.panel(next).unwrap().borrow().id()).expect("a mail");
    assert!(!unread(s.store(), landed));

    assert!(s.undo());
    assert_eq!(role_of(s.store(), 1), "inbox", "one press, and it is back");
    assert_eq!(with_mailbox(&s, list, |m| m.len()), rows);
    assert!(unread(s.store(), landed), "the walk's own claim came back");
    assert_eq!(
        s.joined_child(list)
            .and_then(|c| s.panel(c))
            .map(|p| p.borrow().title()),
        Some("Q3 infra budget draft".to_string()),
        "the reader is open on the mail again"
    );

    // And redo replays the whole gesture, walk included.
    assert!(s.redo());
    assert_eq!(role_of(s.store(), 1), "trash");
    assert_eq!(
        s.joined_child(list)
            .and_then(|c| s.panel(c))
            .map(|p| p.borrow().title()),
        Some("[stelaxis] CI failed on main".to_string()),
    );
}

/// Filing the first conversation from its reader leaves the list previewing
/// the row that took its place — whichever row that is.
#[test]
fn the_walk_after_a_filing_previews_whatever_it_lands_on() {
    for row in 0..3usize {
        let (mut s, _clock) = session();
        let list = open_root(&mut s, Role::Inbox.id());
        let nav = with_mailbox(&s, list, |m| m.go(row)).expect("a row");
        go(&mut s, nav);
        let reader = s.joined_child(list).expect("a reader");
        // A click on the bar focuses its panel first, which is the state the
        // verb actually runs in.
        go(&mut s, Nav::Focus(reader));
        verb(&mut s, reader, "mail.archive");
        let next = s
            .joined_child(list)
            .and_then(|c| s.panel(c))
            .map(|p| p.borrow().title());
        assert!(next.is_some(), "row {row}: the walk previewed nothing");
    }
}
