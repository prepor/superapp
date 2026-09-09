//! The native display reader must distinguish a pending lookup from a
//! confirmed missing conversation when reconciling a person's marks.

use super::*;
use std::sync::mpsc;
use std::time::Instant;

fn draw_until(
    session: &mut Session,
    slot: SlotId,
    wake: &mpsc::Receiver<()>,
    mut ready: impl FnMut(&mut Mailbox, &Store) -> bool,
) {
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        session.store().poll_external();
        session.settle();
        let store = session.store().clone();
        if with_mailbox(session, slot, |mailbox| {
            mailbox.list_mut().sync(&store);
            ready(mailbox, &store)
        }) {
            return;
        }
        let remaining = deadline
            .checked_duration_since(Instant::now())
            .expect("mailbox display completed");
        wake.recv_timeout(remaining)
            .expect("mailbox display woke the UI");
    }
}

fn native_mailbox() -> (Session, SlotId, mpsc::Receiver<()>) {
    let (mut session, _) = session();
    let slot = open_root(&mut session, Role::Inbox.id());
    let (notify, wake) = mpsc::channel();
    session.store().attach_ui(move || {
        let _ = notify.send(());
    });
    draw_until(&mut session, slot, &wake, |mailbox, _| {
        mailbox.len() > 3 && mailbox.rows(0, 3).len() == 3
    });
    (session, slot, wake)
}

#[test]
fn native_mailbox_marks_survive_cold_lookups_preview_and_filter_refresh() {
    let (mut session, slot, wake) = native_mailbox();
    let store = session.store().clone();
    let (keys, preview) = with_mailbox(&session, slot, |mailbox| {
        mailbox.go(0).unwrap();
        assert!(mailbox.toggle_mark(), "space marks the first conversation");
        mailbox.list_mut().mark_range(&store, 1).unwrap();
        let keys = mailbox.list().marks().keys();
        assert_eq!(keys.len(), 2, "shift-down extends the marked set");
        mailbox.list_mut().sync(&store);
        assert_eq!(
            mailbox.list().marks().keys(),
            keys,
            "a cold presence lookup is not deletion"
        );
        assert!(
            mailbox.toggle_mark(),
            "space toggles the second conversation off"
        );
        assert_eq!(mailbox.list().marks().len(), 1);
        assert!(mailbox.toggle_mark(), "and back on");
        (keys, mailbox.go(1).unwrap())
    });
    go(&mut session, preview);
    draw_until(&mut session, slot, &wake, |mailbox, store| {
        assert_eq!(
            mailbox.list().marks().keys(),
            keys,
            "preview/read commits preserve the set"
        );
        mailbox
            .rows(0, 2)
            .iter()
            .any(|row| row.thread == keys[1] && !row.unread)
            && !store.queries_pending()
    });

    with_mailbox(&session, slot, |mailbox| {
        mailbox.list_mut().set_filter("@from:nobody@example.test");
        mailbox.list_mut().sync(&store);
        assert_eq!(
            mailbox.list().marks().keys(),
            keys,
            "a new filter must not clear marks"
        );
    });
    draw_until(&mut session, slot, &wake, |mailbox, store| {
        assert_eq!(mailbox.list().marks().keys(), keys);
        mailbox.list().hidden_rows().len() == 2 && !store.queries_pending()
    });

    let removed = keys[0];
    store
        .write(move |tx| {
            model::file_tx(tx, removed, "archive")?;
            Ok(())
        })
        .unwrap();
    draw_until(&mut session, slot, &wake, |mailbox, store| {
        mailbox.list().marks().keys() == vec![keys[1]] && !store.queries_pending()
    });
    assert_eq!(
        with_mailbox(&session, slot, |mailbox| mailbox.list().hidden_rows().len()),
        1,
        "a confirmed missing conversation is removed from the hidden band too"
    );
    store
        .write(move |tx| {
            model::file_tx(tx, removed, "inbox")?;
            Ok(())
        })
        .unwrap();
    with_mailbox(&session, slot, |mailbox| {
        mailbox.restore_marks(&[removed]);
        mailbox.list_mut().sync(&store);
        assert_eq!(
            mailbox.list().marks().keys(),
            keys,
            "the old empty lookup must not erase a restored mark while its refresh is pending"
        );
    });
    draw_until(&mut session, slot, &wake, |mailbox, store| {
        assert_eq!(mailbox.list().marks().keys(), keys);
        mailbox.list().hidden_rows().len() == 2 && !store.queries_pending()
    });
    session.shutdown();
}

#[test]
fn native_mailbox_mark_all_finishes_after_loading_and_clear_cancels_it() {
    let (mut session, slot, wake) = native_mailbox();
    let store = session.store().clone();
    let count = with_mailbox(&session, slot, |mailbox| mailbox.len());
    with_mailbox(&session, slot, |mailbox| {
        assert!(
            mailbox.list_mut().mark_all(&store),
            "the cold request is accepted"
        );
    });
    draw_until(&mut session, slot, &wake, |mailbox, store| {
        mailbox.list().marks().len() == count && !store.queries_pending()
    });

    with_mailbox(&session, slot, |mailbox| {
        mailbox.clear_marks();
        mailbox.list_mut().set_filter("@from:vera");
        assert!(mailbox.list_mut().mark_all(&store));
        mailbox.clear_marks();
    });
    draw_until(&mut session, slot, &wake, |mailbox, store| {
        assert!(
            mailbox.list().marks().is_empty(),
            "clear cancels the accepted mark-all request"
        );
        !store.queries_pending()
    });
    with_mailbox(&session, slot, |mailbox| {
        mailbox.list_mut().set_filter("@subject:budget");
        assert!(mailbox.list_mut().mark_all(&store));
        mailbox.list_mut().set_filter("");
    });
    draw_until(&mut session, slot, &wake, |mailbox, store| {
        assert!(
            mailbox.list().marks().is_empty(),
            "changing the filter cancels the old request"
        );
        !store.queries_pending()
    });
    with_mailbox(&session, slot, |mailbox| {
        mailbox.list_mut().set_filter("@subject:CI");
        assert!(mailbox.list_mut().mark_all(&store));
        mailbox.list_mut().retarget(model::threads(Role::Archive));
    });
    draw_until(&mut session, slot, &wake, |mailbox, store| {
        assert!(
            mailbox.list().marks().is_empty(),
            "changing the source cancels the old request"
        );
        !store.queries_pending()
    });
    session.shutdown();
}

fn pump_until(
    session: &mut Session,
    wake: &mpsc::Receiver<()>,
    mut ready: impl FnMut(&Session) -> bool,
) {
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        session.store().poll_external();
        session.settle();
        if ready(session) {
            return;
        }
        let remaining = deadline
            .checked_duration_since(Instant::now())
            .expect("mail completion arrived");
        wake.recv_timeout(remaining)
            .expect("mail completion woke the UI");
    }
}

fn preview_mail(session: &Session, slot: SlotId) -> Option<MailId> {
    session.joined_child(slot).and_then(|child| {
        session
            .panel(child)
            .and_then(|panel| Message::of(panel.borrow().id()))
    })
}

fn hold_writer(store: &Store) -> mpsc::Sender<()> {
    let (entered, waiting) = mpsc::channel();
    let (release, held) = mpsc::channel();
    let _pending = store
        .submit_write(move |_| {
            entered.send(()).unwrap();
            held.recv_timeout(Duration::from_secs(10))
                .expect("the UI accepted input while the writer was held");
            Ok(())
        })
        .unwrap();
    waiting.recv_timeout(Duration::from_secs(5)).unwrap();
    release
}

// Occupy the bounded display-reader pool so filing cannot accidentally use
// a refreshed page. The writer must supply its own committed successor.
struct HeldReaders(Vec<mpsc::Sender<()>>);

impl HeldReaders {
    fn new(store: &Store) -> Self {
        let (entered, waiting) = mpsc::channel();
        let mut releases = Vec::new();
        for _ in 0..4 {
            let db = store.db();
            let entered = entered.clone();
            let (release, held) = mpsc::channel();
            releases.push(release);
            kernel::runtime::spawn(async move {
                db.read_async(move |_| {
                    entered.send(()).unwrap();
                    held.recv_timeout(Duration::from_secs(10))
                        .expect("release display reader");
                    Ok(())
                })
                .await
                .unwrap();
            });
        }
        for _ in 0..4 {
            waiting.recv_timeout(Duration::from_secs(5)).unwrap();
        }
        Self(releases)
    }
}

impl Drop for HeldReaders {
    fn drop(&mut self) {
        for release in self.0.drain(..) {
            let _ = release.send(());
        }
    }
}

#[test]
fn native_filing_carries_the_committed_successor_and_undo_restores_one_gesture() {
    for (role, batch) in [
        (Role::Inbox, false),
        (Role::Inbox, true),
        (Role::Spam, true),
    ] {
        let (mut session, _) = session();
        let slot = open_root(&mut session, role.id());
        let (notify, wake) = mpsc::channel();
        session.store().attach_ui(move || {
            let _ = notify.send(());
        });
        draw_until(&mut session, slot, &wake, |mailbox, _| {
            mailbox.rows(0, 3).len() == 3
        });
        let (rows, nav) = with_mailbox(&session, slot, |mailbox| {
            let rows = mailbox.rows(0, 3);
            if batch {
                mailbox.go(0).unwrap();
                assert!(mailbox.toggle_mark());
                mailbox.go(1).unwrap();
                assert!(mailbox.toggle_mark());
            }
            (rows, mailbox.go(1).unwrap())
        });
        go(&mut session, nav);
        draw_until(&mut session, slot, &wake, |mailbox, store| {
            mailbox.rows(0, 3).len() == 3
                && !store.queries_pending()
                && !unread(store, rows[1].target)
        });
        kernel::runtime::block_on(session.workers().shutdown());
        let held_readers = HeldReaders::new(session.store());
        let reader = session.joined_child(slot).unwrap();
        let before = session.history().head();
        let release = hold_writer(session.store());
        let verb_id = if role == Role::Spam {
            "mail.not_spam"
        } else {
            "mail.archive"
        };
        verb(&mut session, if batch { slot } else { reader }, verb_id);
        assert_eq!(
            session.history().head(),
            before,
            "the UI accepted filing while the writer was held"
        );
        assert_eq!(preview_mail(&session, slot), Some(rows[1].target));
        release.send(()).unwrap();
        pump_until(&mut session, &wake, |session| {
            preview_mail(session, slot) == Some(rows[2].target)
        });
        assert_eq!(
            with_mailbox(&session, slot, |mailbox| mailbox
                .list()
                .cursor_key()
                .copied()),
            Some(rows[2].thread)
        );
        assert_eq!(
            with_mailbox(&session, slot, |mailbox| mailbox
                .list()
                .table()
                .row(session.store(), 1)
                .unwrap()
                .thread),
            rows[1].thread,
            "display snapshots still contain the filed row"
        );
        drop(held_readers);
        assert_eq!(
            session
                .history()
                .rows()
                .0
                .iter()
                .filter(|row| row.kind == "file")
                .count(),
            1
        );
        assert!(session.undo());
        pump_until(&mut session, &wake, |session| {
            session.history().head() == before && !session.history_busy()
        });
        assert_eq!(role_of(session.store(), rows[1].target), role.as_str());
        assert_eq!(
            preview_mail(&session, slot),
            Some(rows[1].target),
            "one undo restores the previous reader"
        );
        assert_eq!(
            with_mailbox(&session, slot, |mailbox| mailbox.list().marks().len()),
            if batch { 2 } else { 0 }
        );
        session.shutdown();
    }
}

#[test]
fn native_filing_does_not_override_a_cursor_moved_while_the_writer_was_held() {
    let (mut session, slot, wake) = native_mailbox();
    let (original, next) = with_mailbox(&session, slot, |mailbox| {
        mailbox.go(0).unwrap();
        mailbox.toggle_mark();
        let rows = mailbox.rows(0, 3);
        (rows[0].clone(), rows[2].clone())
    });
    kernel::runtime::block_on(session.workers().shutdown());
    let release = hold_writer(session.store());
    verb(&mut session, slot, "mail.archive");
    let nav = with_mailbox(&session, slot, |mailbox| mailbox.go(2).unwrap());
    go(&mut session, nav);
    release.send(()).unwrap();
    pump_until(&mut session, &wake, |session| {
        role_of(session.store(), original.target) == "archive"
            && session
                .history()
                .rows()
                .0
                .iter()
                .any(|row| row.kind == "file")
    });
    assert_eq!(preview_mail(&session, slot), Some(next.target));
    assert_eq!(
        with_mailbox(&session, slot, |mailbox| mailbox
            .list()
            .cursor_key()
            .copied()),
        Some(next.thread)
    );
    session.shutdown();
}

#[test]
fn native_batch_filing_keeps_a_surviving_selected_read_conversation() {
    let (mut session, slot, wake) = native_mailbox();
    with_mailbox(&session, slot, |mailbox| {
        mailbox.list_mut().set_filter("@unread")
    });
    draw_until(&mut session, slot, &wake, |mailbox, _| {
        mailbox.rows(0, 2).len() == 2
    });
    let (selected, nav) = with_mailbox(&session, slot, |mailbox| {
        mailbox.go(0).unwrap();
        mailbox.toggle_mark();
        let selected = mailbox.rows(1, 2)[0].clone();
        (selected, mailbox.go(1).unwrap())
    });
    go(&mut session, nav);
    draw_until(&mut session, slot, &wake, |mailbox, store| {
        let _ = mailbox.rows(0, 3);
        !unread(store, selected.target) && !store.queries_pending()
    });
    let before = session.history().head();
    verb(&mut session, slot, "mail.archive");
    pump_until(&mut session, &wake, |session| {
        session.history().head() != before
    });
    assert_eq!(preview_mail(&session, slot), Some(selected.target));
    assert_eq!(
        with_mailbox(&session, slot, |mailbox| mailbox
            .list()
            .cursor_key()
            .copied()),
        Some(selected.thread)
    );
    session.shutdown();
}
