//! Mail, driven through a session with no widget in sight.
//!
//! `Session::fake` gives an in-memory store with mail's schema and seed, its
//! fake servers, and the passes running inline — so a scripted action is
//! followed by its consequences in the same call.

use kernel::app::{App, Env};
use kernel::layout::SlotId;
use kernel::nav::Nav;
use kernel::panel::{PanelId, VerbAct};
use kernel::search::{Engine, Go};
use kernel::session::{Action, Session};
use kernel::store::Store;

use super::caps::FakeServers;
use super::model::{self, MailId, Role, Seed};
use super::panels::{Compose, Mailbox, Message};
use super::MAIL;

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

    let rows = with_mailbox(&s, list, |m| m.rows(0, 50));
    assert_eq!(rows.len(), 9, "{:?}", topics(&s, list));
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

    // …and the archive is empty until something is filed there.
    let archive = open_root(&mut s, Role::Archive.id());
    assert_eq!(with_mailbox(&s, archive, |m| m.len()), 0);
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
    assert_eq!(n, (9, 1));

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
/// follows is another, and undo brings both the mails and the marks back.
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
    // One node for the batch, and one for the cursor walk that follows it —
    // the batch closes nothing, so what happens to the panel beside the list
    // is a preview like any other.
    assert_eq!(kinds(&s).len(), before + 2, "{:?}", kinds(&s));
    assert_eq!(kinds(&s).last().map(String::as_str), Some("read"));
    assert_eq!(role_of(s.store(), 1), "archive");
    assert_eq!(role_of(s.store(), 2), "archive");
    assert_eq!(with_mailbox(&s, list, |m| m.list().marks().len()), 0);
    assert_eq!(with_mailbox(&s, list, |m| m.len()), 7);
    assert_eq!(
        s.panel(s.joined_child(list).expect("the walk previewed a row"))
            .unwrap()
            .borrow()
            .title(),
        "Sat hike — early start?",
        "the cursor stood still, and the row that took its place is previewed"
    );

    // The walk first, then the batch it followed.
    assert!(s.undo());
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
    assert_eq!(
        verb_ids(&s, archive),
        vec!["mail.sync", "mail.delete", "mail.all", "mail.clear"]
    );

    // *mark all* takes every row under the filter, and *clear* lets the
    // whole set go.
    verb(&mut s, archive, "mail.all");
    assert_eq!(with_mailbox(&s, archive, |m| m.list().marks().len()), 1);
    verb(&mut s, archive, "mail.clear");
    assert_eq!(verb_ids(&s, archive), vec!["mail.sync"]);
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

    let problems = s.problems();
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
    assert!(s.problems().is_empty(), "and the problem cleared with it");

    // With the servers back, the retried letter goes out.
    servers(&s).set_down(None);
    clock.advance(model::send_delay() + 1.0);
    s.workers().kick_all();
    assert_eq!(servers(&s).submitted().len(), 1);
}

// -- what a letter will carry --------------------------------------------------------

/// Both apps in one build, which is the only shape in which `Apps::get_as`
/// answers. The clipboard it reaches is the files app's own — a process-wide
/// value, so this is the one test that touches it.
static WITH_FILES: &[&dyn App] = &[&MAIL, &crate::apps::files::FILES];

/// Hands the compose instance a look at the shell, as its widget does at the
/// top of every draw and event.
fn observe(s: &Session, slot: SlotId) {
    let inst = s.panel(slot).expect("a panel in the slot");
    let mut b = inst.borrow_mut();
    b.as_any()
        .downcast_mut::<Compose>()
        .expect("a compose")
        .observe(s);
}

/// What the sheet says it will carry.
fn carrying(s: &Session, slot: SlotId) -> Vec<String> {
    let inst = s.panel(slot).expect("a panel in the slot");
    let mut b = inst.borrow_mut();
    b.as_any()
        .downcast_mut::<Compose>()
        .expect("a compose")
        .carrying()
        .iter()
        .map(super::carry::DraftFile::label)
        .collect()
}

/// *attach* is the files clipboard's other destination: it appears while
/// another app is holding something, adds what it holds as one undoable
/// action, and ignores a path the draft carries already.
#[test]
fn attach_carries_what_the_files_app_holds() {
    let env = Env::default();
    let mut s = Session::fake_with(WITH_FILES, &env);
    let list = open_root(&mut s, Role::Inbox.id());
    go(&mut s, Nav::Open {
        from: list,
        id: Compose::id(Seed::Blank),
        fresh: true,
    });
    let sheet = s.focus().expect("the compose took focus");

    // Nothing held: the sheet offers the two ways out of it and no more.
    crate::apps::files::FILES.clear();
    observe(&s, sheet);
    assert_eq!(verb_ids(&s, sheet), vec!["mail.send", "mail.discard"]);

    crate::apps::files::FILES.set(
        crate::apps::files::Op::Copy,
        vec!["~/Downloads/report.pdf".into(), "~/notes.md".into()],
    );
    observe(&s, sheet);
    assert_eq!(
        verb_ids(&s, sheet),
        vec!["mail.send", "mail.discard", "mail.attach"]
    );

    verb(&mut s, sheet, "mail.attach");
    assert_eq!(carrying(&s, sheet), vec!["report.pdf", "notes.md"]);
    assert_eq!(
        s.notes().last().map(|n| n.msg.clone()),
        Some("carrying 2 files".to_string())
    );

    // The same clipboard again adds nothing and records nothing: a path the
    // draft already carries was not this action's to add.
    let before = kinds(&s).len();
    verb(&mut s, sheet, "mail.attach");
    assert_eq!(kinds(&s).len(), before, "{:?}", kinds(&s));
    assert_eq!(carrying(&s, sheet).len(), 2);

    // One undo takes exactly what the action added back off.
    assert!(s.undo());
    assert!(carrying(&s, sheet).is_empty());
    assert!(s.redo());
    assert_eq!(carrying(&s, sheet).len(), 2);

    // A build without the files app holds nothing, and the verb is not
    // there to be found — which is the whole of "works when the answer is
    // `None`".
    crate::apps::files::FILES.clear();
    let mut alone = Session::fake_with(APPS, &env);
    let list = open_root(&mut alone, Role::Inbox.id());
    go(&mut alone, Nav::Open {
        from: list,
        id: Compose::id(Seed::Blank),
        fresh: true,
    });
    let sheet = alone.focus().expect("the compose took focus");
    observe(&alone, sheet);
    assert_eq!(verb_ids(&alone, sheet), vec!["mail.send", "mail.discard"]);
}

// -- search ------------------------------------------------------------------------

/// The launcher's mail source finds a letter by a word of its subject, and
/// the hit opens the panel that reads it.
#[test]
fn the_search_provider_finds_a_mail_by_its_subject() {
    let (s, _clock) = session();
    let mut engine = Engine::inline(s.apps().providers());
    assert_eq!(engine.slots(), 1, "mail supplies exactly one source");

    engine.ask(s.store(), 1, "airport");
    let hits: Vec<_> = engine.collect().into_iter().flat_map(|a| a.hits).collect();
    assert_eq!(hits.len(), 1, "{hits:?}");
    assert_eq!(hits[0].label, "that airport book");
    assert!(hits[0].detail.starts_with("Dmitry Orlov"));
    assert_eq!(hits[0].go, Go::Open(Message::id(8)));

    // A sender's name reaches their letters too, and a word nobody wrote
    // reaches nothing.
    engine.ask(s.store(), 2, "kovac");
    let hits: Vec<_> = engine.collect().into_iter().flat_map(|a| a.hits).collect();
    assert_eq!(hits.len(), 1);
    assert_eq!(hits[0].label, "Q3 infra budget draft");

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
    assert_eq!(tags, vec!["archive", "compose", "inbox", "message"]);

    let roots: Vec<String> = s.roots().into_iter().map(|r| r.label).collect();
    assert_eq!(roots, vec!["inbox", "archive", "new mail"]);

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
        vec!["move", "seen", "submit"]
    );
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
