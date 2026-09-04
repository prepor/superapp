//! Mail as an agent reads it: what each panel says it is about, the inbox
//! rendered as text, and the data dictionary the system prompt carries.

use std::collections::HashSet;

use kernel::app::App;
use kernel::context;
use kernel::panel::{PanelId, Tag};
use kernel::store::Store as KStore;

use super::*;
use crate::apps::mail::model::MAILBOX_PAGE;
use crate::apps::mail::panels::Card;
use crate::apps::mail::MAIL;

/// A letter of the seed, by its subject.
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

/// One of each panel mail owns, with the arguments a real one carries.
fn one_of_each(s: &Session) -> Vec<PanelId> {
    let budget = mail_named(s.store(), "Q3 infra budget draft");
    let at = crate::apps::mail::parts::attachments(s.store(), budget)[0].at;
    vec![
        Role::Inbox.id(),
        Role::Archive.id(),
        Role::Sent.id(),
        Role::Spam.id(),
        Role::Inbox.filtered("vera@kovac.io"),
        Message::id(budget),
        Compose::id(Seed::Blank),
        Compose::id(Seed::Reply(budget)),
        Contact::id("vera@kovac.io"),
        Card::id(budget, at),
        Settings::id(),
        crate::apps::mail::panels::AddAccount::id(),
    ]
}

/// Every kind mail owns says what it is about in its own words — not the
/// default, which is the title and the identity and tells an agent nothing
/// it could not read off the tag.
#[test]
fn every_mail_panel_says_what_it_is_about() {
    let (mut s, _clock) = session();
    let ids = one_of_each(&s);
    let covered: HashSet<Tag> = ids.iter().map(|id| id.tag).collect();
    for kind in MAIL.kinds() {
        assert!(
            covered.contains(&kind.tag()),
            "no sample panel for the tag {}",
            kind.tag()
        );
    }
    for id in ids {
        let slot = open_root(&mut s, id.clone());
        let (title, about) = {
            let inst = s.panel(slot).expect("the panel");
            let b = inst.borrow();
            (b.title(), b.about())
        };
        assert!(!about.is_empty(), "{id} says nothing");
        assert_ne!(
            about,
            format!("{title} — {id}"),
            "{id} is still on the default"
        );
        // A paragraph, not a label: the point of it is what a tag cannot say.
        assert!(about.len() > 120, "{id}: “{about}”");
        assert!(
            !about.contains("cmd+"),
            "{id} names a chord; keys go on the control"
        );
    }
}

/// The inbox as one block of text for a model: the identity in the
/// attributes, mail's own paragraph, the query that drew it, and the rows as
/// they read now.
#[test]
fn the_inbox_renders_as_the_panel_it_is() {
    let (mut s, _clock) = session();
    let slot = open_root(&mut s, Role::Inbox.id());

    // What the shell does around a draw: open the trace, let the panel read
    // its page, close it. From here on the provenance is the store's.
    s.store().trace_begin(slot);
    let rows = with_mailbox(&s, slot, |m| m.rows(0, MAILBOX_PAGE));
    s.store().trace_end();
    assert!(!rows.is_empty(), "the seed has an inbox");

    let cx = context::of(&s, slot).expect("the inbox is open");
    assert_eq!(cx.id, Role::Inbox.id());
    assert_eq!(cx.title, "inbox");
    assert_eq!(cx.workspace, 1);
    assert_eq!(cx.queries.len(), 1, "one page query drew it");

    let text = context::render(s.store(), &cx, &[]);
    assert!(
        text.starts_with("<panel id=\"inbox\" title=\"inbox\" workspace=\"1\">\n"),
        "{text}"
    );
    assert!(
        text.contains("one row a conversation rather than a letter"),
        "{text}"
    );
    assert!(text.contains("It carries no argument"), "{text}");
    assert!(
        text.contains("### inbox table — the inbox as conversations under the panel's filter"),
        "{text}"
    );
    assert!(text.contains("```sql\nSELECT"), "{text}");
    assert!(
        text.contains("f.role = 'inbox'"),
        "the SQL as it ran: {text}"
    );
    // The columns of the page, and one of the seed's own conversations.
    assert!(
        text.contains("| thread | last | unread | target | who | topic | n |"),
        "{text}"
    );
    assert!(text.contains("| --- |"), "{text}");
    assert!(text.contains("Q3 infra budget draft"), "{text}");
    assert!(
        text.contains(&format!(
            "rows ({} of {}, the panel's own page)",
            rows.len(),
            rows.len()
        )),
        "{text}"
    );
    assert!(text.ends_with("</panel>\n"), "{text}");
    assert!(text.len() < context::CAP, "one page of mail fits a chip");
}

/// A mailbox opened on a sender says so, and its SQL carries the filter it
/// came up under — the provenance is the query that actually ran.
#[test]
fn a_filtered_mailbox_renders_its_filter() {
    let (mut s, _clock) = session();
    let slot = open_root(&mut s, Role::Inbox.filtered("vera@kovac.io"));
    s.store().trace_begin(slot);
    // The widget seeds the field from the panel's argument; a test says it.
    let filter = with_mailbox(&s, slot, |m| {
        let seed = m.seed_filter();
        m.list_mut().set_filter(&seed);
        m.rows(0, MAILBOX_PAGE)
    });
    s.store().trace_end();
    assert!(!filter.is_empty(), "vera has written");

    let cx = context::of(&s, slot).expect("the panel");
    let text = context::render(s.store(), &cx, &[]);
    assert!(
        text.contains("Its argument is a sender, vera@kovac.io"),
        "{text}"
    );
    assert!(text.contains("vera@kovac.io"), "{text}");
}

/// The panel context of a message, with the effects filed about it — the
/// three-part shape a chip carries into a chat.
#[test]
fn a_message_carries_what_was_lately_done_to_it() {
    let (mut s, _clock) = session();
    let budget = mail_named(s.store(), "Q3 infra budget draft");
    let slot = open_root(&mut s, Message::id(budget));
    let cx = context::of(&s, slot).expect("the reader");
    assert!(cx.about.contains("Q3 infra budget draft"), "{}", cx.about);
    assert!(cx.about.contains(&budget.to_string()), "{}", cx.about);
    // Nothing has been *done* to it yet. The passes that ran at boot only
    // asked — connect, select, search — and a chip lists what wrote.
    assert!(context::recent_effects(s.store(), &cx.id, context::EFFECTS).is_empty());

    let text = context::render(s.store(), &cx, &[]);
    assert!(
        text.starts_with(&format!(
            "<panel id=\"message({budget})\" title=\"Q3 infra budget draft\" workspace=\"1\">"
        )),
        "{text}"
    );
}

/// Mail's data dictionary names every table its ladder creates. A table
/// added without a sentence about it is a table an agent will guess at.
#[test]
fn mails_own_words_name_every_table_it_keeps() {
    let (s, _clock) = session();
    let describe = MAIL.describe().expect("mail describes itself");

    let mine = mail_tables(s.store());
    assert!(mine.contains("message"), "the tables were read: {mine:?}");
    for table in &mine {
        assert!(
            describe.contains(&format!("`{table}`")),
            "mail's describe never mentions `{table}`; it has: {describe}"
        );
    }
    // And the sentences that keep a model out of the two tables that are not
    // its to write.
    assert!(describe.contains("`server_msg`"));
    assert!(describe.contains("`outbox`"));
    assert!(describe.contains("never be written directly"));
    assert!(describe.lines().count() < 70, "a dictionary, not a dump");
}

/// Every table this store has that a store with no apps in it does not —
/// mail's, and only mail's. The FTS index's shadow tables are the
/// extension's own bookkeeping and belong to no dictionary.
fn mail_tables(store: &Store) -> HashSet<String> {
    let read = |c: &rusqlite::Connection| -> HashSet<String> {
        c.prepare("SELECT name FROM sqlite_master WHERE type = 'table'")
            .and_then(|mut q| {
                q.query_map([], |r| r.get::<_, String>(0))?
                    .collect::<rusqlite::Result<HashSet<String>>>()
            })
            .expect("the table list")
    };
    let bare = KStore::open(None, &[]).expect("a store with no apps in it");
    read(store.conn())
        .difference(&read(bare.conn()))
        .filter(|n| !n.starts_with("sqlite_") && !n.starts_with("message_fts_"))
        .cloned()
        .collect()
}
