//! What a letter carries, both ways: the parts derived from its `raw`, and
//! the files a draft will carry out.
//!
//! The same session the rest of these tests drive. What is proved here is the
//! walk (a seeded letter's `raw` is a real `multipart/mixed`, and nothing
//! wrote its rows by hand), the narrowing the HTML reading goes through at
//! ingest, and the three refusals an attach makes.

use super::*;

use crate::apps::mail::{carry, panels, parts, reading};

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
        .map(carry::DraftFile::label)
        .collect()
}

/// Reaches the compose instance itself — what the widget does through its
/// own borrow on every keystroke.
fn with_compose<T>(s: &Session, slot: SlotId, f: impl FnOnce(&mut Compose) -> T) -> T {
    let inst = s.panel(slot).expect("a panel in the slot");
    let mut b = inst.borrow_mut();
    f(b.as_any().downcast_mut::<Compose>().expect("a compose"))
}

/// The paths the demo tree has, which is what an attach may actually pick
/// up: one file well inside the limit, one 38 MB disk image well past it,
/// and one directory, which is not a file at all.
const A_FILE: &str = "~/Downloads/report-q3.pdf";
const TOO_BIG: &str = "~/Downloads/superapp-0.1.0.dmg";
const A_DIR: &str = "~/Downloads/2026";

/// *attach* is the files clipboard's other destination: it appears while
/// another app is holding something, adds what it holds as one undoable
/// action, and ignores a path the draft carries already.
#[test]
fn attach_carries_what_the_files_app_holds() {
    let env = Env::default();
    let mut s = Session::fake_with(WITH_FILES, &env);
    let list = open_root(&mut s, Role::Inbox.id());
    go(
        &mut s,
        Nav::Open {
            from: list,
            id: Compose::id(Seed::Blank),
            fresh: true,
        },
    );
    let sheet = s.focus().expect("the compose took focus");

    // Nothing held: the sheet offers the two ways out of it and no more.
    crate::apps::files::FILES.clear();
    observe(&s, sheet);
    assert_eq!(verb_ids(&s, sheet), vec!["mail.send", "mail.discard"]);

    crate::apps::files::FILES.set(
        crate::apps::files::Op::Copy,
        vec![A_FILE.into(), "~/notes.md".into()],
    );
    observe(&s, sheet);
    assert_eq!(
        verb_ids(&s, sheet),
        vec!["mail.send", "mail.discard", "mail.attach"]
    );

    verb(&mut s, sheet, "mail.attach");
    // The line says the name and the size the file was picked at: the row
    // records both, because the send reads the file again and refuses one
    // that has grown past the limit since.
    assert_eq!(
        carrying(&s, sheet),
        vec!["report-q3.pdf · 1.2 MB", "notes.md · 2.1 KB"]
    );
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
    assert_eq!(
        s.notes().last().map(|n| n.msg.clone()),
        Some("already carrying 2 files".to_string())
    );

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
    go(
        &mut alone,
        Nav::Open {
            from: list,
            id: Compose::id(Seed::Blank),
            fresh: true,
        },
    );
    let sheet = alone.focus().expect("the compose took focus");
    observe(&alone, sheet);
    assert_eq!(verb_ids(&alone, sheet), vec!["mail.send", "mail.discard"]);
}

/// The refusals an attach makes, each in words. A directory is not a file; a
/// disk image is past what any server will take, and the line says the limit;
/// and a build that refused everything records no action at all.
#[test]
fn attach_refuses_a_directory_and_anything_past_the_limit() {
    let env = Env::default();
    let mut s = Session::fake_with(WITH_FILES, &env);
    let list = open_root(&mut s, Role::Inbox.id());
    go(
        &mut s,
        Nav::Open {
            from: list,
            id: Compose::id(Seed::Blank),
            fresh: true,
        },
    );
    let sheet = s.focus().expect("the compose took focus");

    // A directory alone: nothing to carry, and the sheet says what a letter
    // does carry rather than blaming the file.
    crate::apps::files::FILES.set(crate::apps::files::Op::Copy, vec![A_DIR.into()]);
    observe(&s, sheet);
    let before = kinds(&s).len();
    verb(&mut s, sheet, "mail.attach");
    assert_eq!(
        s.notes().last().map(|n| n.msg.clone()),
        Some("nothing to attach — a letter carries files".to_string())
    );
    assert!(carrying(&s, sheet).is_empty());
    assert_eq!(kinds(&s).len(), before, "a refusal records nothing");

    // A 38 MB disk image: refused with the limit spelled out, which is the
    // only useful thing to say about a file that is too big.
    crate::apps::files::FILES.set(crate::apps::files::Op::Copy, vec![TOO_BIG.into()]);
    observe(&s, sheet);
    verb(&mut s, sheet, "mail.attach");
    assert_eq!(
        s.notes().last().map(|n| n.msg.clone()),
        Some("too big to carry: superapp-0.1.0.dmg (the limit is 25 MB)".to_string())
    );
    assert!(carrying(&s, sheet).is_empty());
    assert_eq!(kinds(&s).len(), before);

    // One of each: what can be carried is, and the toast says how many could
    // not — a partial refusal is still an action.
    crate::apps::files::FILES.set(
        crate::apps::files::Op::Copy,
        vec![A_FILE.into(), TOO_BIG.into(), A_DIR.into()],
    );
    observe(&s, sheet);
    verb(&mut s, sheet, "mail.attach");
    assert_eq!(carrying(&s, sheet), vec!["report-q3.pdf · 1.2 MB"]);
    assert_eq!(
        s.notes().last().map(|n| n.msg.clone()),
        Some("carrying “report-q3.pdf” — 1 too big".to_string())
    );
    crate::apps::files::FILES.clear();
}

/// A discard takes the draft's files with it — and undo puts both back: the
/// text the sheet was holding, and the `CARRIES` line under it. The row is
/// written before the files, because they hang off the slot *and* its seed.
#[test]
fn a_discard_and_its_undo_carry_the_files_with_the_text() {
    let env = Env::default();
    let mut s = Session::fake_with(WITH_FILES, &env);
    let list = open_root(&mut s, Role::Inbox.id());
    go(
        &mut s,
        Nav::Open {
            from: list,
            id: Compose::id(Seed::Blank),
            fresh: true,
        },
    );
    let sheet = s.focus().expect("the compose took focus");

    crate::apps::files::FILES.set(crate::apps::files::Op::Copy, vec![A_FILE.into()]);
    observe(&s, sheet);
    verb(&mut s, sheet, "mail.attach");
    with_compose(&s, sheet, |c| {
        c.edited("vera@kovac.io", "the numbers", "attached.");
    });
    assert_eq!(carrying(&s, sheet), vec!["report-q3.pdf · 1.2 MB"]);
    crate::apps::files::FILES.clear();

    // The sheet goes, and the row and its files go with it.
    verb(&mut s, sheet, "mail.discard");
    assert!(s.panel(sheet).is_none(), "the sheet closed behind it");
    assert!(carry::all(s.store().conn(), sheet as i64)
        .expect("the store answers")
        .is_empty());

    // One undo brings the sheet back with all three.
    assert!(s.undo());
    s.settle();
    let back = s.focus().expect("the sheet came back and took focus");
    assert_eq!(
        with_compose(&s, back, |c| c.draft().clone()).subject,
        "the numbers"
    );
    assert_eq!(carrying(&s, back), vec!["report-q3.pdf · 1.2 MB"]);

    // And redo takes all three away again.
    assert!(s.redo());
    s.settle();
    assert!(carry::all(s.store().conn(), sheet as i64)
        .expect("the store answers")
        .is_empty());
}

/// A letter's parts are derived from its `raw`, and the bytes come back out
/// of it: the seed's two carriers are the walk's proof, since their `raw` is
/// a real `multipart/mixed` and nothing wrote the rows by hand.
#[test]
fn a_letter_lists_its_parts_and_yields_their_bytes() {
    let (s, _clock) = session();
    let store = s.store();

    // Vera's budget draft: one part, described by its row.
    let budget = mail_named(store, "Q3 infra budget draft");
    let carried = parts::attachments(store, budget);
    assert_eq!(carried.len(), 1);
    let a = &carried[0];
    assert_eq!(a.name, "q3-budget.csv");
    assert_eq!(a.mime, "text/csv");
    assert_eq!(a.label(), "q3-budget.csv · 140 B");
    assert_eq!(a.kind(), kernel::caps::FileKind::Text);
    assert_eq!(
        a.panel(),
        kernel::panel::PanelId::new(panels::Card::TAG, ["1".to_string(), a.at.to_string()])
    );

    // The bytes are not stored twice: they come back out of the letter.
    let bytes = parts::part(store, a).expect("the part is in the letter");
    assert!(
        String::from_utf8_lossy(&bytes).starts_with("line,aug,sep,delta"),
        "{:?}",
        String::from_utf8_lossy(&bytes)
    );
    assert_eq!(bytes.len() as u64, a.size);

    // One part, by the letter and its place in it — the identity the panel
    // persists.
    assert_eq!(parts::attachment(store, budget, a.at).as_ref(), Some(a));
    assert_eq!(parts::attachment(store, budget, 99), None);

    // The invoice carries a pdf, so `open` has something to hand to the OS.
    let invoice = mail_named(store, "invoice 2026-08 — €46.20");
    let pdf = parts::attachments(store, invoice);
    assert_eq!(pdf.len(), 1);
    assert_eq!(pdf[0].mime, "application/pdf");
    assert_eq!(pdf[0].kind(), kernel::caps::FileKind::Pdf);

    // Everything else carries nothing, and says so once: a mail with no raw
    // is walked and marked, not walked again on every pass.
    let plain = mail_named(store, "Sat hike — early start?");
    assert!(parts::attachments(store, plain).is_empty());

    // What the reader's wish counts a line for.
    let carriers = parts::thread_carriers(store, budget);
    assert_eq!(carriers.iter().copied().collect::<Vec<_>>(), vec![budget]);
}

/// The seed's HTML letter is narrowed on the way in, exactly as a synced one
/// is: what the store holds is what the widget draws, and the `@html` tag
/// sifts on it.
#[test]
fn the_seeded_html_letter_is_narrowed_at_ingest() {
    let (s, _clock) = session();
    let store = s.store();
    let ci = mail_named(store, "[stelaxis] CI failed on main");
    let m = model::mail(store, ci).expect("the letter");
    let html = m.html.as_deref().expect("it was sent as HTML");

    // What survives the narrowing is the letter: the link a reader could
    // have meant, the emphasis, the picture it can show.
    assert!(
        html.contains("<a href=\"https://github.com/x/stelaxis\">"),
        "{html}"
    );
    assert!(html.contains("data:image/png;base64,"), "{html}");
    assert!(html.contains("the build badge"), "{html}");
    // And what does not: the stylesheet, the `javascript:` href, the pixel
    // counting the open, and the surrogate pair a composer wrote an emoji as.
    assert!(!html.contains("javascript:"), "{html}");
    assert!(!html.contains("<style"), "{html}");
    assert!(!html.contains("pixel.gif"), "{html}");
    assert!(html.contains('🚀'), "{html}");

    // `@html` sifts on the column, so the tag finds the letters that arrived
    // as HTML and no others: this one, and Max's reply from a composer that
    // writes its quote as a `<blockquote>`.
    let html_rows = model::mailbox_filtered(store, Role::Inbox, "@html");
    let targets: Vec<MailId> = html_rows.iter().map(|t| t.target).collect();
    assert_eq!(html_rows.len(), 2, "{targets:?}");
    assert!(targets.contains(&ci), "{targets:?}");

    // Max's, folded: what he wrote, then the letter he wrote it over.
    let max = store
        .conn()
        .query_row(
            "SELECT id FROM message WHERE message_id = 'pm-2@ivanov.dev'",
            [],
            |r| r.get::<_, MailId>(0),
        )
        .expect("Max's second reply");
    let m = model::mail(store, max).expect("the letter");
    let (own, quote) = reading::split_quote_html(m.html.as_deref().expect("HTML"));
    assert!(own.contains("keeps a preview honest"), "{own}");
    assert!(!own.contains("blockquote"), "{own}");
    let quote = quote.expect("a composer's quote is a blockquote");
    assert!(quote.starts_with("<p>On Sun, 30 Aug 2026"), "{quote}");
    assert!(quote.contains("<blockquote>"), "{quote}");

    // The quote fold reads the HTML the same way it reads the text.
    let (own, quote) = reading::split_quote_html(
        "<p>Agreed.</p><p>On Sun, Max wrote:</p><blockquote>the note</blockquote>",
    );
    assert_eq!(own, "<p>Agreed.</p>");
    assert_eq!(
        quote.as_deref(),
        Some("<p>On Sun, Max wrote:</p><blockquote>the note</blockquote>")
    );
    // A letter that is all quote stays whole.
    assert_eq!(
        reading::split_quote_html("<blockquote>x</blockquote>").1,
        None
    );
}

/// How long a letter reads, and how many rows the reader asks for. The
/// measure is taken off the reading the panel *draws*: an HTML letter is
/// measured as the lines its narrowing plains to, a plain one as its text.
#[test]
fn a_letter_is_measured_by_the_reading_the_panel_draws() {
    let (s, _clock) = session();
    let store = s.store();

    // Vera's two paragraphs, and the same letter in a narrower column.
    let vera = model::mail(store, mail_named(store, "Q3 infra budget draft")).expect("the letter");
    assert_eq!(reading::reading_lines(&vera, 1000), 3);
    assert!(reading::reading_lines(&vera, 40) > reading::reading_lines(&vera, 80));

    // The HTML one is measured off its narrowing, picture and list included,
    // so it is longer than the two lines its `body` column holds.
    let ci = model::mail(store, mail_named(store, "[stelaxis] CI failed on main")).expect("it");
    assert!(
        reading::reading_lines(&ci, 1000) >= 5,
        "{}",
        reading::reading_lines(&ci, 1000)
    );

    // A conversation's wish: a closed message is a row and a half, an open
    // one its text plus the chrome around it — and one line more when the
    // letter carries something, since its parts are listed on their own.
    let budget = mail_named(store, "Q3 infra budget draft");
    let msgs = model::thread(store, budget);
    let open: std::collections::BTreeSet<MailId> = msgs.iter().map(|t| t.mail.head.id).collect();
    let none = std::collections::BTreeSet::new();
    let carries = parts::thread_carriers(store, budget);
    assert_eq!(
        reading::thread_lines(&msgs, &open, &carries, 1000),
        reading::thread_lines(&msgs, &open, &none, 1000) + 1,
        "the parts line is worth a line"
    );
    // A closed letter lists nothing, so it costs nothing either way.
    assert_eq!(
        reading::thread_lines(&msgs, &none, &carries, 1000),
        reading::thread_lines(&msgs, &none, &none, 1000)
    );
}

/// One mail by subject — the seed's letters are addressed that way in a test
/// rather than by an id the order could move.
fn mail_named(store: &Store, subject: &str) -> MailId {
    store
        .conn()
        .query_row(
            "SELECT id FROM message WHERE subject = ?1",
            [subject],
            |r| r.get(0),
        )
        .unwrap_or_else(|e| panic!("no seeded mail “{subject}”: {e}"))
}
