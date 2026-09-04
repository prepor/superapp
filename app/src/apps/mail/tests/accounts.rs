//! The accounts an app is made of, and the two panels that configure them.
//!
//! The same session the other half of these tests drives: an in-memory store
//! with mail's schema and seed, its fake servers, and the passes running
//! inline — so an account added here starts a worker in the same call.

use super::*;

use crate::apps::mail::caps::Auth;
use crate::apps::mail::panels::Form;
use crate::apps::mail::{seed, sync};

/// A correspondent's card: who they are, how much they have written, and the
/// one link off it — the inbox, filtered to their address.
#[test]
fn a_contact_card_links_to_its_letters() {
    let (mut s, _clock) = session();
    let card = open_root(&mut s, Contact::id("max@ivanov.dev"));
    let (title, label, verbs) = {
        let inst = s.panel(card).unwrap();
        let b = inst.borrow();
        (b.title(), b.verbs()[0].label.clone(), b.verbs().len())
    };
    assert_eq!(title, "Max Ivanov");
    assert_eq!(label, "messages from max");
    assert_eq!(verbs, 1);

    verb(&mut s, card, "mail.from");
    let list = s.focus().expect("the filtered inbox took focus");
    assert_eq!(
        s.panel(list).unwrap().borrow().title(),
        "inbox · max@ivanov.dev"
    );
    // Max's three inbox letters, as two conversations: the panel model
    // thread, and the long one that stands alone. The filter is `@from:`,
    // not a bare word, so a letter that merely mentions him would not be
    // here.
    assert_eq!(with_mailbox(&s, list, |m| m.len()), 2);
    let mut topics = with_mailbox(&s, list, |m| {
        m.rows(0, 9).into_iter().map(|r| r.topic).collect::<Vec<_>>()
    });
    topics.sort();
    assert_eq!(
        topics,
        vec![
            "long version: what panels owe their content".to_string(),
            "superapp panel model".to_string()
        ]
    );

    // A card for an address nobody has written from still opens, and says
    // as much.
    let none = open_root(&mut s, Contact::id("nobody@example.org"));
    let inst = s.panel(none).unwrap();
    assert_eq!(inst.borrow().title(), "nobody@example.org");
}

/// Settings lists the accounts; the form adds one and the row's button takes
/// it away again, with everything it brought.
#[test]
fn the_form_adds_an_account_and_the_row_removes_it() {
    let (mut s, _clock) = session();
    let settings = open_root(&mut s, Settings::id());
    assert_eq!(s.panel(settings).unwrap().borrow().title(), "settings");
    assert_eq!(
        verb_ids(&s, settings),
        vec!["mail.add_account"],
        "the form is a link; the remove buttons belong to their rows"
    );
    let rows = |s: &Session| {
        let inst = s.panel(settings).unwrap();
        let mut b = inst.borrow_mut();
        b.as_any().downcast_mut::<Settings>().unwrap().accounts()
    };
    assert_eq!(rows(&s).len(), 1);
    assert_eq!(rows(&s)[0].email, "me@prepor.dev");
    assert_eq!(rows(&s)[0].host_line(), "imap.demo");
    // The demo account syncs against the fake server, so its line is the
    // pass's own — never an error.
    assert!(!rows(&s)[0].status_line().1, "{:?}", rows(&s)[0]);

    // The form, reached by the link the bar wears.
    verb(&mut s, settings, "mail.add_account");
    let form = s.focus().expect("the form took focus");
    assert_eq!(s.panel(form).unwrap().borrow().title(), "add account");
    assert_eq!(verb_ids(&s, form), vec!["mail.add", "mail.google"]);
    {
        let inst = s.panel(form).unwrap();
        let mut b = inst.borrow_mut();
        let a = b.as_any().downcast_mut::<AddAccount>().unwrap();
        assert_eq!(a.form().imap, "imap.fastmail.com");
        a.edited(Form {
            email: "andrey@fastmail.test".into(),
            pass: "app-pass-123".into(),
            imap: "imap.invalid".into(),
            smtp: "smtp.invalid".into(),
        });
    }
    verb(&mut s, form, "mail.add");
    assert_eq!(rows(&s).len(), 2);
    let added = rows(&s)[1].clone();
    assert_eq!(added.email, "andrey@fastmail.test");
    assert_eq!(added.imap_host.as_deref(), Some("imap.invalid"));
    assert_eq!(added.auth.as_deref(), Some("password"));
    assert!(!added.oauth());

    // The password went to the keychain, never to the store.
    assert_eq!(
        s.world()
            .run(&kernel::caps::SecretGet("andrey@fastmail.test"))
            .expect("the keychain answered")
            .as_deref(),
        Some("app-pass-123")
    );
    let stored: i64 = s
        .store()
        .conn()
        .query_row(
            "SELECT COUNT(*) FROM account WHERE email LIKE '%app-pass%'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(stored, 0);

    // A new account is a new pass and a new watch, asked for after the
    // action that made it.
    assert!(s.workers().names().contains(&"sync-2".to_string()));
    assert!(s.workers().names().contains(&"watch-2".to_string()));

    // The password is gone from the form with the row it made; the hosts
    // stay, because the next account is usually the same provider.
    {
        let inst = s.panel(form).unwrap();
        let mut b = inst.borrow_mut();
        let a = b.as_any().downcast_mut::<AddAccount>().unwrap();
        assert_eq!(a.form().email, "");
        assert_eq!(a.form().pass, "");
        assert_eq!(a.form().imap, "imap.invalid");
        // The same address twice is refused: two rows for one mailbox would
        // be two workers fetching the same mail into the same store.
        a.edited(Form {
            email: "andrey@fastmail.test".into(),
            ..a.form().clone()
        });
    }
    verb(&mut s, form, "mail.add");
    assert_eq!(rows(&s).len(), 2);
    assert_eq!(
        s.notes().last().map(|n| n.msg.as_str()),
        Some("andrey@fastmail.test is already here")
    );

    // Undo takes an account that is still empty back off.
    assert!(s.undo());
    assert_eq!(rows(&s).len(), 1);
    assert!(s.redo());
    assert_eq!(rows(&s).len(), 2);

    // …and the row's own button removes it with its mail. Nothing brings
    // that back, and the node says so.
    {
        let inst = s.panel(settings).unwrap();
        let mut b = inst.borrow_mut();
        let id = added.id;
        b.as_any().downcast_mut::<Settings>().unwrap().remove(&mut s, id);
    }
    s.settle();
    assert_eq!(rows(&s).len(), 1);
    assert!(!s.workers().names().contains(&"sync-2".to_string()));
    assert!(!s.workers().names().contains(&"watch-2".to_string()));

    // Undo cannot bring an account's mail back and does not pretend to: the
    // node goes expired and the walk steps past it, so the account stays
    // gone whatever else the walk reaches.
    s.undo();
    s.settle();
    assert_eq!(rows(&s).len(), 1, "the account did not come back");
    let states: Vec<String> = s.history().rows().0.into_iter().map(|r| r.state).collect();
    assert!(
        states.iter().any(|st| st == "expired"),
        "the removal is expired: {states:?}"
    );
}

/// The Google button refuses a scripted run and says so on the panel's own
/// line — the flow wants a browser and a human, and a suite is neither.
#[test]
fn the_google_button_refuses_a_scripted_run() {
    let (mut s, _clock) = session();
    let form = open_root(&mut s, AddAccount::id());
    verb(&mut s, form, "mail.google");
    // The bar only asks; the widget starts the flow, and this test is the
    // widget. Each borrow ends before the next: the flow acts on the
    // session, and the session holds the same instance.
    let inst = s.panel(form).expect("the form");
    let asked = {
        let mut b = inst.borrow_mut();
        b.as_any().downcast_mut::<AddAccount>().unwrap().take_google()
    };
    assert!(asked, "the bar asked for a sign-in");
    {
        let mut b = inst.borrow_mut();
        let a = b.as_any().downcast_mut::<AddAccount>().unwrap();
        a.google(&mut s, std::sync::Arc::new(|| {}));
    }
    let (line, url) = {
        let mut b = inst.borrow_mut();
        let a = b.as_any().downcast_mut::<AddAccount>().unwrap();
        (a.google_line().cloned(), a.take_url())
    };
    assert_eq!(
        line,
        Some(("sign-in needs a real run, not a script".into(), true))
    );
    assert!(url.is_none(), "no browser was opened");
}

/// The credentials one session opens with: an app password out of the
/// keychain, or a bearer token out of the grant. One picker, reading the
/// account row's own column.
#[test]
fn credentials_come_from_the_keychain_or_the_grant() {
    let (s, _clock) = session();
    let servers = servers(&s);
    servers.grant("g@gmail.test", "ya29.fake");

    let creds = |email: &str, bearer: bool| {
        sync::creds(s.world(), email, "imap.example", bearer)
    };
    let pw = creds("me@prepor.dev", false).expect("the demo password");
    assert_eq!(pw.host, "imap.example");
    assert_eq!(pw.user, "me@prepor.dev");
    assert_eq!(pw.auth, Auth::Password(seed::PASSWORD.into()));
    assert!(!pw.auth.is_bearer());

    let tok = creds("g@gmail.test", true).expect("the planted grant");
    assert_eq!(tok.auth, Auth::Bearer("ya29.fake".into()));
    assert!(tok.auth.is_bearer());

    // And the two failures say what to do about them.
    assert_eq!(
        creds("nobody@example.org", false).unwrap_err(),
        "no password in the keychain"
    );
    assert!(creds("nobody@example.org", true)
        .unwrap_err()
        .contains("no google grant"));

    // A secret never prints, whichever mechanism it is.
    assert!(!format!("{pw:?}").contains(seed::PASSWORD));
    assert!(!format!("{tok:?}").contains("ya29.fake"));
}

/// An account whose last pass failed is a standing problem, with the two
/// things there are to do about it. A pass that succeeds clears the row by
/// itself: there is nothing to reset and nothing to store.
#[test]
fn a_failing_account_is_a_problem_until_it_syncs() {
    let (mut s, _clock) = session();
    let set = |s: &Session, status: &str| {
        let status = status.to_string();
        s.store()
            .write(move |c| {
                c.execute(
                    "UPDATE account SET status = ?1, synced = NULL WHERE id = 1",
                    [status],
                )
                .map(|_| ())
            })
            .expect("the status");
    };

    set(&s, "error: no route to host");
    let problems = s.problems();
    assert_eq!(problems.len(), 1, "{problems:?}");
    assert_eq!(problems[0].key, "account:1");
    assert_eq!(problems[0].label, "me@prepor.dev");
    assert_eq!(problems[0].line, "no route to host");
    assert_eq!(problems[0].detail, "never synced");
    let verbs: Vec<String> = problems[0].verbs.iter().map(|v| v.label.clone()).collect();
    assert_eq!(verbs, vec!["sync me".to_string(), "settings".to_string()]);

    // Its link opens the panel the account is configured on.
    let nav = problems[0]
        .verbs
        .iter()
        .find(|v| v.id == "mail.settings")
        .map(|v| match &v.act {
            VerbAct::Go(n) => n.clone(),
            _ => panic!("settings is a link"),
        })
        .expect("the settings link");
    go(&mut s, nav);
    let slot = s.focus().expect("settings took focus");
    assert_eq!(s.panel(slot).unwrap().borrow().title(), "settings");

    // A pass that succeeds takes the row away.
    set(&s, "ok · sep 01 12:00");
    assert!(s.problems().is_empty(), "{:?}", s.problems());
}
