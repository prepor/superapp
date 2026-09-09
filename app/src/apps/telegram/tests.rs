//! Telegram, driven through a session with no widget in sight.
//!
//! `Session::fake` gives an in-memory store with the app's schema and its
//! demo world, so every test starts from the same ten chats.

use kernel::app::App;
use kernel::layout::SlotId;
use kernel::nav::Nav;
use kernel::panel::{PanelId, VerbAct};
use kernel::search::{Engine, Go};
use kernel::session::{Action, Session};
use kernel::time::{ts, virtual_epoch};

use super::model::{self, PeerKind, RecKind};
use super::panels::chat::rows_of;
use super::panels::signin::Field;
use super::panels::{
    Attach, Chat, Chats, Contacts, Line, Members, Messages, Peer, People, Place, Row, SignIn, Viewer,
};
use super::panels::told;
use super::seed::{
    ANNA, DEV, ELENA, FAMILY, HIKE, IVAN, MAX, OLD_FLAT, RUST_WEEKLY, SELF, STELAXIS, VERA,
};
use super::transport::FakeTd;
use super::{runtime, schema, requests, sync, Telegram, TELEGRAM};

static APPS: &[&dyn App] = &[&TELEGRAM];

mod filter_tests;
mod topics_tests;
mod tools;
mod performance;
mod downloads_tests;
mod history;
mod context_tests;
mod upgrades_tests;
mod reading;

#[test]
fn background_ui_preserves_unread_and_submits_drafts_without_waiting_for_sqlite() {
    use kernel::app::{Apps, Env, Mode, Workers};
    use kernel::store::Store;
    use std::rc::Rc;
    use std::sync::{atomic::{AtomicBool, Ordering}, mpsc, Arc};
    use std::time::{Duration, SystemTime, UNIX_EPOCH};

    let dir = std::env::temp_dir().join(format!("telegram-ui-{}-{}", std::process::id(),
        SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_nanos()));
    std::fs::create_dir_all(&dir).unwrap();
    let apps = Apps::new(APPS);
    let store = Store::open(Some(&dir.join("store.sqlite")), &apps.schemas()).unwrap();
    apps.seed(&store, Mode::Fake).unwrap();
    let card = super::topics::card(&store, VERA, 0).unwrap();
    let unread = model::first_unread_in(&store, VERA, 0, card.last_read.unwrap_or(0));
    let world = Rc::new(apps.world(store, Mode::Fake, &Env::default()));
    let workers = Workers::inline(APPS, world.clone());
    let mut s = Session::new(apps, world, workers, Mode::Fake);
    s.store().attach_ui(|| {});
    let slot = open_root(&mut s, Chat::id(VERA));
    with_chat(&s, slot, |chat| assert_eq!(chat.first_unread(), unread,
        "an initial display snapshot must not erase the opening unread boundary"));
    with_chat(&s, slot, |chat| chat.set_draft("saved draft"));
    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    loop {
        let panel = s.panel(slot).unwrap().clone();
        panel.borrow_mut().as_any().downcast_mut::<Chat>().unwrap().poll(&mut s);
        if super::topics::card(s.store(), VERA, 0).unwrap().draft.as_deref() == Some("saved draft") { break; }
        assert!(std::time::Instant::now() < deadline);
        std::thread::sleep(Duration::from_millis(1));
    }
    with_chat(&s, slot, |chat| { let _ = chat.card(); });

    let (started, observed) = mpsc::channel();
    let (release, held) = mpsc::channel();
    let finished = Arc::new(AtomicBool::new(false));
    let done = finished.clone();
    let _blocker = s.store().submit_write(move |_| {
        started.send(()).unwrap();
        let _ = held.recv_timeout(Duration::from_secs(5));
        done.store(true, Ordering::Release);
        Ok(())
    }).unwrap();
    observed.recv_timeout(Duration::from_secs(5)).unwrap();
    with_chat(&s, slot, |chat| {
        chat.set_draft("");
        let _ = chat.card();
        assert_eq!(chat.draft(), "", "the previous database row must not restore cleared text");
    });
    assert!(!finished.load(Ordering::Acquire), "saving a draft must return while SQLite is busy");
    release.send(()).unwrap();
    s.store().write(|_| Ok(())).unwrap();
    assert_eq!(super::topics::card(s.store(), VERA, 0).unwrap().draft, None);
    drop(s);
    drop(_blocker);
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn a_queued_local_edit_captures_undo_state_inside_its_transaction() {
    use std::sync::mpsc;
    use std::time::{Duration, Instant};
    let mut s = session();
    let msg = model::history(s.store(), VERA).iter().find(|m| m.out).unwrap().clone();
    s.store().attach_ui(|| {});
    let (started, observed) = mpsc::channel();
    let (release, held) = mpsc::channel();
    let id = msg.id;
    let _blocker = s.store().submit_write(move |tx| {
        model::edit_tx(tx, VERA, id, "changed while the composer was open", true, Some(&[]))?;
        started.send(()).unwrap();
        held.recv_timeout(Duration::from_secs(5)).expect("UI submitted the edit without waiting");
        Ok(())
    }).unwrap();
    observed.recv_timeout(Duration::from_secs(5)).unwrap();
    let head = s.history().head();
    super::verbs::edit_line(&mut s, VERA, id, &msg.text, msg.edited, "new edit");
    assert_eq!(s.history().head(), head, "uncommitted work has no undo node");
    release.send(()).unwrap();
    s.store().write(|_| Ok(())).unwrap();
    s.settle();
    assert_eq!(model::line(s.store(), VERA, id).unwrap().text, "new edit");
    assert!(s.undo());
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        s.settle();
        let restored = model::line(s.store(), VERA, id).unwrap();
        if restored.text == "changed while the composer was open" {
            assert!(restored.edited);
            assert_eq!(restored.entities, Some(Vec::new()));
            break;
        }
        assert!(Instant::now() < deadline, "undo finished in the background");
        std::thread::sleep(Duration::from_millis(1));
    }
    s.shutdown();
}

fn session() -> Session {
    Session::fake(APPS)
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

/// A navigation, settled.
fn go(s: &mut Session, n: Nav) {
    s.nav(n);
    s.settle();
}

fn with_chats<T>(s: &Session, slot: SlotId, f: impl FnOnce(&mut Chats) -> T) -> T {
    let inst = s.panel(slot).expect("a panel in the slot");
    let mut b = inst.borrow_mut();
    f(b.as_any().downcast_mut::<Chats>().expect("a chat list"))
}

fn with_chat<T>(s: &Session, slot: SlotId, f: impl FnOnce(&mut Chat) -> T) -> T {
    let inst = s.panel(slot).expect("a panel in the slot");
    let mut b = inst.borrow_mut();
    f(b.as_any().downcast_mut::<Chat>().expect("a chat"))
}

fn with_attach<T>(s: &Session, slot: SlotId, f: impl FnOnce(&mut Attach) -> T) -> T {
    let inst = s.panel(slot).expect("a panel in the slot");
    let mut b = inst.borrow_mut();
    f(b.as_any().downcast_mut::<Attach>().expect("an attach panel"))
}

/// What the widget does at the top of every draw: the attach panel looks
/// through the join at the chat's list.
fn observe(s: &Session, slot: SlotId) {
    with_attach(s, slot, |a| a.observe(s));
}

/// A chat with its attach panel opened from the bar, joined.
fn chat_with_attach(s: &mut Session, peer: i64) -> (SlotId, SlotId) {
    let chat = open_root(s, Chat::id(peer));
    verb(s, chat, "telegram.attach");
    let attach = s.joined_child(chat).expect("the attach panel, joined");
    observe(s, attach);
    (chat, attach)
}

/// Enter in a chat's composer, settled.
fn send(s: &mut Session, slot: SlotId) {
    let inst = s.panel(slot).expect("a panel in the slot");
    {
        let mut b = inst.borrow_mut();
        b.as_any().downcast_mut::<Chat>().expect("a chat").send(s);
    }
    s.settle();
}

/// Runs one of a panel's verbs by id, exactly as the bar does.
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

fn verb_ids(s: &Session, slot: SlotId) -> Vec<&'static str> {
    s.panel(slot)
        .expect("a panel in the slot")
        .borrow()
        .verbs()
        .iter()
        .map(|v| v.id)
        .collect()
}

fn titles(s: &Session, slot: SlotId) -> Vec<String> {
    with_chats(s, slot, |c| c.rows(0, 50).into_iter().map(|r| r.title).collect())
}

fn unread(s: &Session, peer: i64) -> (i64, bool) {
    s.store()
        .conn()
        .query_row(
            "SELECT unread, mention FROM tg_chat WHERE peer = ?1",
            [peer],
            |r| Ok((r.get(0)?, r.get::<_, i64>(1)? != 0)),
        )
        .expect("a chat row")
}

// -- the app ------------------------------------------------------------------------

#[test]
fn the_app_registers_its_tags_and_roots() {
    let s = session();
    let tags: Vec<&str> = s.apps().tags().iter().map(|t| t.as_str()).collect();
    assert_eq!(
        tags,
        vec![
            "attach", "chats", "contacts", "line", "media", "members", "messages", "peer",
            "place", "signin", "telegram-chat", "telegram-topics"
        ]
    );
    let roots: Vec<String> = s.roots().into_iter().map(|r| r.label).collect();
    assert_eq!(roots, vec!["chats", "replies & mentions", "contacts", "saved messages", "sign in"]);
    // No worker without the engine: the demo world runs no background sync,
    // and the sign-in panel reads the 'closed' session row it leaves.
    assert!(s.workers().names().is_empty(), "nothing runs in the background yet");
}

// -- the chat list ---------------------------------------------------------------------

/// Pinned chats lead in their order, then the rest by the last line's
/// time; the archive is its own list.
#[test]
fn the_list_puts_pinned_chats_first_and_the_archive_aside() {
    let mut s = session();
    let list = open_root(&mut s, Chats::id());
    assert_eq!(
        titles(&s, list),
        vec![
            "Vera Kovac",
            "stelaxis",
            "Elena Petrova",
            "Family",
            "Rust Weekly",
            "Hiking Saturday",
            "superapp dev",
            "Max Ivanov",
            "Andrey Rudenko",
            "Anna Schmidt",
        ]
    );
    let archive = open_root(&mut s, Chats::archive());
    assert_eq!(titles(&s, archive), vec!["Old flat"]);
    let row = with_chats(&s, list, |c| c.rows(1, 2).remove(0));
    assert_eq!(row.kind, PeerKind::Group);
    assert!(row.muted && row.unread_mentions == 2 && row.pinned == 2);
    assert_eq!(row.unread, 8);
    assert_eq!(row.preview(s.now()), "Ivan: meeting moved to 15:00");
}

/// The filter's tags: unread, a folder, a kind.
#[test]
fn the_filter_narrows_by_tag() {
    let mut s = session();
    let list = open_root(&mut s, Chats::id());
    let under = |s: &Session, f: &str| -> Vec<String> {
        with_chats(s, list, |c| {
            c.list_mut().set_filter(f);
            c.rows(0, 50).into_iter().map(|r| r.title).collect()
        })
    };
    assert_eq!(
        under(&s, "@unread"),
        vec!["stelaxis", "Elena Petrova", "Family", "Rust Weekly"]
    );
    assert_eq!(
        under(&s, "@folder:work"),
        vec!["stelaxis", "Rust Weekly", "superapp dev", "Max Ivanov"]
    );
    assert_eq!(under(&s, "@kind:channel"), vec!["Rust Weekly", "superapp dev"]);
    assert_eq!(under(&s, "thermos"), vec!["Hiking Saturday"]);
    let src = model::chats(false);
    let folders: Vec<String> = (src.suggest)(s.store(), "folder", "w")
        .into_iter()
        .map(|g| g.value)
        .collect();
    assert_eq!(folders, vec!["work".to_string()]);
}

/// A preview claims the read on the opening node, and undo gives the count
/// back with the panel.
#[test]
fn a_preview_marks_the_chat_read_and_undo_gives_it_back() {
    let mut s = session();
    let list = open_root(&mut s, Chats::id());
    assert_eq!(unread(&s, STELAXIS), (8, true));
    let nav = with_chats(&s, list, |c| c.go(1)).expect("a row");
    go(&mut s, nav);
    let reader = s.joined_child(list).expect("the chat, joined");
    assert_eq!(unread(&s, STELAXIS), (0, true));
    // Where the reading started is kept on the instance for the unread
    // line, and the cursor starts nowhere.
    let (first, cursor) = with_chat(&s, reader, |c| (c.first_unread(), c.cursor().map(|(_, id)| id)));
    let first = first.expect("something was unread");
    assert!(cursor.is_none());
    let hist = model::history(s.store(), STELAXIS);
    let i = hist.iter().position(|m| m.id == first).expect("a line");
    assert_eq!(hist[i].text, "Q3 infra budget draft is ready for review");
    assert_eq!(s.focus(), Some(list), "a preview leaves focus in the list");

    s.undo();
    s.settle();
    assert_eq!(unread(&s, STELAXIS), (8, true));
    assert!(s.joined_child(list).is_none(), "the preview went with it");
}

#[test]
fn an_unread_chat_stays_selected_after_reading_until_another_chat_is_selected() {
    let mut s = session();
    let list = open_root(&mut s, Chats::id());
    with_chats(&s, list, |c| { c.list_mut().set_filter("@unread"); });
    let before = titles(&s, list);
    let nav = with_chats(&s, list, |c| c.go(1)).unwrap();
    go(&mut s, nav);

    assert_eq!(unread(&s, ELENA), (0, false));
    assert_eq!(titles(&s, list), before, "reading keeps the selected chat in place");
    with_chats(&s, list, |c| {
        let l = c.list_mut();
        assert_eq!(l.cursor_index(s.store()), Some(1));
        let row = l.row(s.store(), 1).unwrap();
        assert_eq!(row.peer, ELENA);
        assert_eq!(row.unread, 0, "the retained row shows its current read status");
    });

    let row = with_chats(&s, list, |c| c.list_mut().move_cursor(s.store(), 1)).unwrap();
    assert_eq!(row.peer, FAMILY, "down takes the next chat without skipping it");
    go(&mut s, Nav::Preview { from: list, id: Chats::target(&row) });
    assert_eq!(unread(&s, FAMILY), (0, false));
    assert_eq!(titles(&s, list), vec!["stelaxis", "Family", "Rust Weekly"]);
    with_chats(&s, list, |c| {
        assert_eq!(c.list_mut().cursor_index(s.store()), Some(1));
    });

    let nav = with_chats(&s, list, |c| c.go(0)).unwrap();
    go(&mut s, nav);
    assert_eq!(titles(&s, list), vec!["stelaxis", "Rust Weekly"]);
}

/// The transcript's rows: a day where the day changes, the unread line
/// above the first unread line, a run where one writer goes on.
#[test]
fn the_transcript_is_days_runs_and_the_unread_line() {
    let s = session();
    let hist = model::history(s.store(), VERA);
    let rows = rows_of(&hist, None, virtual_epoch());
    let shape: Vec<String> = rows
        .iter()
        .map(|r| match r {
            Row::Day(d) => format!("day {d}"),
            Row::Unread => "unread".to_string(),
            Row::Service(m) => format!("service {}", m.text),
            Row::Message { msg, run } => {
                format!("{}{}", if *run { "+ " } else { "" }, msg.writer())
            }
        })
        .collect();
    assert_eq!(
        shape,
        vec![
            "day 27 AUG",
            "Vera Kovac",
            "me",
            "day YESTERDAY",
            "Vera Kovac",
            "me",
            "day TODAY",
            "Vera Kovac",
            "+ Vera Kovac",
            "me",
            "Vera Kovac",
            "+ Vera Kovac",
            "me",
        ]
    );
    // The reply's quote names the line it answers.
    let reply = hist.iter().find(|m| m.reply_to.is_some()).expect("a reply");
    assert_eq!(reply.reply_name, "Vera Kovac");
    assert_eq!(reply.reply_text, "see you at 7:30 then?");

    // The group: the unread line stands above the first unread line, and a
    // service line breaks a run.
    let hist = model::history(s.store(), STELAXIS);
    let first = hist
        .iter()
        .find(|m| m.text.starts_with("Q3 infra"))
        .map(|m| m.id);
    let rows = rows_of(&hist, first.map(|id| (STELAXIS, id)), virtual_epoch());
    let at = rows.iter().position(|r| *r == Row::Unread).expect("the line");
    assert!(matches!(&rows[at + 1], Row::Message { msg, run: false } if msg.fwd_from.is_some()));
    assert!(matches!(&rows[0], Row::Day(d) if d == "25 AUG"));
    assert!(matches!(&rows[1], Row::Service(m) if m.text == "Ivan Petrov joined the group"));

    // A line that never left keeps its own header, where `failed` is said,
    // even five minutes after one of mine that did.
    let hist = model::history(s.store(), HIKE);
    let rows = rows_of(&hist, None, virtual_epoch());
    let failed = rows
        .iter()
        .find(|r| matches!(r, Row::Message { msg, .. } if msg.state.as_deref() == Some("failed")))
        .expect("the thermos");
    assert!(matches!(failed, Row::Message { run: false, .. }));
}

/// The cursor walks the lines, marks follow it, and the reply line stands
/// on the cursor's line.
#[test]
fn the_cursor_walks_and_reply_takes_the_line_under_it() {
    let mut s = session();
    let slot = open_root(&mut s, Chat::id(VERA));
    let hist = model::history(s.store(), VERA);
    let last = hist.last().expect("lines").id;
    with_chat(&s, slot, |c| {
        assert_eq!(c.walk(-1).map(|(_, id)| id), Some(last), "from nothing, the newest line");
        assert_eq!(c.walk(-1).map(|(_, id)| id), Some(last - 1));
        c.toggle_mark();
        c.mark_range(-1);
        assert_eq!(c.marks().len(), 2);
    });
    // The batch has the bar while rows are marked — no reply — and the
    // cursor's line has a card to go to; cleared, the cursor's verbs are
    // back.
    let bar = verb_ids(&s, slot);
    assert!(bar.contains(&"telegram.forward") && bar.contains(&"telegram.clear"));
    assert!(bar.contains(&"telegram.line") && !bar.contains(&"telegram.reply"));
    verb(&mut s, slot, "telegram.clear");
    assert!(with_chat(&s, slot, |c| c.marks().is_empty()));
    verb(&mut s, slot, "telegram.reply");
    let line = with_chat(&s, slot, |c| c.reply_line(s.now())).expect("replying");
    assert!(line.starts_with("reply to "), "{line}");
    // A draft is written behind the composer, and the list shows it.
    with_chat(&s, slot, |c| c.set_draft("on my way"));
    let list = open_root(&mut s, Chats::id());
    let row = with_chats(&s, list, |c| c.rows(0, 1).remove(0));
    assert_eq!(row.preview(s.now()), "draft: on my way");
    // A send this round is a toast, and the field empties with it.
    with_chat(&s, slot, |c| c.set_draft("on my way"));
    {
        let inst = s.panel(slot).unwrap();
        let mut b = inst.borrow_mut();
        b.as_any().downcast_mut::<Chat>().unwrap().send(&mut s);
    }
    assert_eq!(with_chat(&s, slot, |c| c.draft().to_string()), "");
    assert!(with_chat(&s, slot, |c| c.reply_to().is_none()));
}

#[test]
fn a_reply_original_has_a_way_back_after_walking_and_marking() {
    let mut s = session();
    let chat = open_root(&mut s, Chat::id(VERA));
    let hist = model::history(s.store(), VERA);
    let reply = hist.iter().find(|m| m.reply_to.is_some()).unwrap();
    let original = reply.reply_to.unwrap();
    assert!(!verb_ids(&s, chat).contains(&"telegram.back"));
    with_chat(&s, chat, |c| {
        c.set_cursor((c.peer(), reply.id));
        c.set_draft("still writing");
    });
    verb(&mut s, chat, "telegram.original");
    with_chat(&s, chat, |c| {
        assert_eq!(c.cursor().map(|(_, id)| id), Some(original));
        assert_eq!(c.take_follow_wish().map(|(_, id)| id), Some(original));
        assert_eq!(c.take_follow_wish().map(|(_, id)| id), None);
        c.walk(-1).map(|(_, id)| id);
        c.toggle_mark();
    });
    let marks = with_chat(&s, chat, |c| c.marks().clone());
    assert!(verb_ids(&s, chat).contains(&"telegram.back"));

    // Another panel on this same conversation has its own reading history.
    let other = open_root(&mut s, Chat::at(VERA, original));
    assert!(!verb_ids(&s, other).contains(&"telegram.back"));
    verb(&mut s, chat, "telegram.back");
    with_chat(&s, chat, |c| {
        assert_eq!(c.cursor().map(|(_, id)| id), Some(reply.id));
        assert_eq!(c.take_follow_wish().map(|(_, id)| id), Some(reply.id));
        assert_eq!(c.take_follow_wish().map(|(_, id)| id), None);
        assert_eq!(c.marks(), &marks);
        assert_eq!(c.draft(), "still writing");
        assert!(!c.take_field_wish(), "returning does not ask for the composer");
    });
    assert!(!verb_ids(&s, chat).contains(&"telegram.back"));
}

#[test]
fn revisiting_a_reply_does_not_duplicate_the_return_point() {
    let mut s = session();
    let chat = open_root(&mut s, Chat::id(VERA));
    let hist = model::history(s.store(), VERA);
    let reply = hist.iter().find(|m| m.reply_to.is_some()).unwrap();
    let original = reply.reply_to.unwrap();

    for _ in 0..2 {
        // Clicking the reply again instead of using Back keeps its return point.
        with_chat(&s, chat, |c| c.set_cursor((c.peer(), reply.id)));
        verb(&mut s, chat, "telegram.original");
        with_chat(&s, chat, |c| {
            assert_eq!(c.cursor().map(|(_, id)| id), Some(original));
            assert_eq!(c.take_follow_wish().map(|(_, id)| id), Some(original));
        });
        assert!(verb_ids(&s, chat).contains(&"telegram.back"));
    }

    verb(&mut s, chat, "telegram.back");
    with_chat(&s, chat, |c| {
        assert_eq!(c.cursor().map(|(_, id)| id), Some(reply.id));
        assert_eq!(c.take_follow_wish().map(|(_, id)| id), Some(reply.id));
    });
    assert!(!verb_ids(&s, chat).contains(&"telegram.back"));
}

#[test]
fn reply_originals_are_retraced_in_order_and_skip_missing_return_points() {
    let mut s = session();
    let chat = open_root(&mut s, Chat::id(VERA));
    let hist = model::history(s.store(), VERA);
    let reply = hist.iter().find(|m| m.reply_to.is_some()).unwrap().id;
    let middle = hist.iter().find(|m| m.id == reply).unwrap().reply_to.unwrap();
    let oldest = hist[0].id;
    s.store().write(move |c| {
        c.execute(
            "UPDATE tg_message SET reply_to = ?1 WHERE chat = ?2 AND id = ?3",
            [oldest, VERA, middle],
        )?;
        Ok(())
    }).unwrap();
    with_chat(&s, chat, |c| c.set_cursor((c.peer(), reply)));
    verb(&mut s, chat, "telegram.original");
    verb(&mut s, chat, "telegram.original");
    assert_eq!(with_chat(&s, chat, |c| c.cursor().map(|(_, id)| id)), Some(oldest));
    for target in [middle, reply] {
        verb(&mut s, chat, "telegram.back");
        with_chat(&s, chat, |c| {
            assert_eq!(c.cursor().map(|(_, id)| id), Some(target));
            assert_eq!(c.take_follow_wish().map(|(_, id)| id), Some(target));
        });
    }
    assert!(!verb_ids(&s, chat).contains(&"telegram.back"));

    // A server update can remove an intermediate reply while we read its
    // original. One back skips it and still reaches the first reply.
    verb(&mut s, chat, "telegram.original");
    verb(&mut s, chat, "telegram.original");
    s.store().write(move |c| {
        c.execute("DELETE FROM tg_message WHERE chat = ?1 AND id = ?2", [VERA, middle])?;
        Ok(())
    }).unwrap();
    verb(&mut s, chat, "telegram.back");
    assert_eq!(with_chat(&s, chat, Chat::take_follow_wish).map(|(_, id)| id), Some(reply));
    assert_eq!(with_chat(&s, chat, |c| c.cursor().map(|(_, id)| id)), Some(reply));
    assert!(!verb_ids(&s, chat).contains(&"telegram.back"));
}

#[test]
fn a_failed_original_jump_does_not_add_a_return_point() {
    let mut s = session();
    let chat = open_root(&mut s, Chat::id(VERA));
    let hist = model::history(s.store(), VERA);
    let reply = hist.iter().find(|m| m.reply_to.is_some()).unwrap().id;
    let original = hist.iter().find(|m| m.id == reply).unwrap().reply_to.unwrap();
    // Neither an unavailable original nor a malformed self-reply moves.
    for target in [-1, reply] {
        s.store().write(move |c| {
            c.execute(
                "UPDATE tg_message SET reply_to = ?1 WHERE chat = ?2 AND id = ?3",
                [target, VERA, reply],
            )?;
            Ok(())
        }).unwrap();
        with_chat(&s, chat, |c| c.set_cursor((c.peer(), reply)));
        verb(&mut s, chat, "telegram.original");
        assert_eq!(with_chat(&s, chat, |c| c.cursor().map(|(_, id)| id)), Some(reply));
        assert_eq!(with_chat(&s, chat, Chat::take_follow_wish).map(|(_, id)| id), None);
        assert!(!verb_ids(&s, chat).contains(&"telegram.back"));
    }
    assert!(s.notes().iter().any(|n| n.msg == "the line it answers is not loaded"));

    // A failed jump also leaves an earlier, successful return point alone.
    s.store().write(move |c| {
        c.execute(
            "UPDATE tg_message SET reply_to = ?1 WHERE chat = ?2 AND id = ?3",
            [original, VERA, reply],
        )?;
        c.execute(
            "UPDATE tg_message SET reply_to = -1 WHERE chat = ?1 AND id = ?2",
            [VERA, original],
        )?;
        Ok(())
    }).unwrap();
    verb(&mut s, chat, "telegram.original");
    assert_eq!(with_chat(&s, chat, Chat::take_follow_wish).map(|(_, id)| id), Some(original));
    verb(&mut s, chat, "telegram.original");
    assert_eq!(with_chat(&s, chat, Chat::take_follow_wish).map(|(_, id)| id), None);
    verb(&mut s, chat, "telegram.back");
    assert_eq!(with_chat(&s, chat, |c| c.cursor().map(|(_, id)| id)), Some(reply));
    assert!(!verb_ids(&s, chat).contains(&"telegram.back"));
}

#[test]
fn deleting_the_last_return_point_clears_the_way_back() {
    for local in [true, false] {
        let mut s = session();
        let chat = open_root(&mut s, Chat::id(VERA));
        let hist = model::history(s.store(), VERA);
        let reply = hist.iter().find(|m| m.reply_to.is_some()).unwrap().id;
        with_chat(&s, chat, |c| c.set_cursor((c.peer(), reply)));
        verb(&mut s, chat, "telegram.original");
        let original = with_chat(&s, chat, Chat::take_follow_wish).map(|(_, id)| id);
        if local {
            with_chat(&s, chat, |c| c.lines_gone(&[(c.peer(), reply)]));
        }
        s.store().write(move |c| {
            c.execute("DELETE FROM tg_message WHERE chat = ?1 AND id = ?2", [VERA, reply])?;
            Ok(())
        }).unwrap();
        if !local {
            verb(&mut s, chat, "telegram.back");
            assert_eq!(s.notes().last().unwrap().msg, "the reply is no longer loaded");
        }
        assert_eq!(with_chat(&s, chat, |c| c.cursor().map(|(_, id)| id)), original);
        assert_eq!(with_chat(&s, chat, Chat::take_follow_wish).map(|(_, id)| id), None);
        assert!(!verb_ids(&s, chat).contains(&"telegram.back"));
    }
}

#[test]
fn reply_back_keeps_its_shortcut_in_a_blocked_conversation() {
    use crate::shell::bar;
    use crate::shell::keys::Letters;

    let mut s = session();
    s.store().write(|c| model::set_blocked_tx(c, VERA, true)).unwrap();
    let chat = open_root(&mut s, Chat::id(VERA));
    let profile = open_root(&mut s, Peer::id(VERA));
    let reply = model::history(s.store(), VERA).iter()
        .find(|m| m.reply_to.is_some()).unwrap().id;
    let check = |s: &Session, returning| {
        let verbs = s.panel(chat).unwrap().borrow().verbs();
        let preview = s.panel(profile).unwrap().borrow().verbs();
        bar::check(&verbs);
        assert_eq!(bar::chord(&verbs, 'b'), returning);
        assert_eq!(bar::chord(&verbs, 'k'), Some("telegram.unblock"));
        assert_eq!(bar::chord(&preview, 'k'), Some("telegram.unblock"));
        let keys = bar::Shortcuts {
            focused: &verbs, focused_keeps: Letters::NONE,
            preview: &preview, preview_keeps: Letters::NONE,
        };
        assert_eq!(keys.route('b'), returning.map(bar::Shortcut::FocusedVerb),
            "an exhausted Back must not fall through to the profile's unblock");
    };
    check(&s, None);
    with_chat(&s, chat, |c| c.set_cursor((c.peer(), reply)));
    verb(&mut s, chat, "telegram.original");
    check(&s, Some("telegram.back"));
    verb(&mut s, chat, "telegram.back");
    assert_eq!(with_chat(&s, chat, |c| c.cursor().map(|(_, id)| id)), Some(reply));
    check(&s, None);
    assert!(model::peer(s.store(), VERA).unwrap().blocked);
}

/// What a chat is with says what the composer does.
#[test]
fn a_channel_i_do_not_run_has_no_composer() {
    let s = session();
    let card = |peer| model::peer(s.store(), peer).expect("a peer");
    assert!(!card(RUST_WEEKLY).can_post());
    assert!(card(DEV).can_post());
    assert_eq!(card(DEV).placeholder(), "broadcast…  ( enter )");
    assert_eq!(card(VERA).placeholder(), "write a message…  ( enter )");
    assert_eq!(card(VERA).status_line(), "online");
    assert_eq!(card(MAX).status_line(), "last seen within a week");
    assert_eq!(card(STELAXIS).status_line(), "7 members, 3 online");
    assert_eq!(card(RUST_WEEKLY).status_line(), "12.4k subscribers");
    assert_eq!(card(SELF).status_line(), "saved messages");
    assert_eq!(card(STELAXIS).kind_line(), "group · 7 members, 3 online");
    assert_eq!(card(VERA).kind_line(), "@vera · online");
    assert_eq!(
        card(RUST_WEEKLY).kind_line(),
        "channel · 12.4k subscribers · @rustweekly"
    );
    // A person with no chat still opens, on nothing.
    let mut s = session();
    let slot = open_root(&mut s, Chat::id(IVAN));
    assert!(with_chat(&s, slot, |c| c.rows(virtual_epoch()).is_empty()));
    assert_eq!(s.panel(slot).unwrap().borrow().title(), "Ivan Petrov");
}

// -- the other lists -----------------------------------------------------------------------

#[test]
fn messages_are_found_everywhere_and_narrowed_to_a_chat() {
    let mut s = session();
    let all = open_root(&mut s, Messages::id());
    let lines = |s: &Session, slot: SlotId, f: &str| -> Vec<String> {
        let inst = s.panel(slot).unwrap();
        let mut b = inst.borrow_mut();
        let m = b.as_any().downcast_mut::<Messages>().unwrap();
        m.list_mut().set_filter(f);
        m.rows(0, 50).into_iter().map(|r| r.line(virtual_epoch())).collect()
    };
    assert_eq!(
        lines(&s, all, "palette"),
        vec!["new palette, what do you think · photo"]
    );
    assert_eq!(
        lines(&s, all, "@from:\"ivan petrov\""),
        vec![
            "meeting moved to 15:00",
            "location 47.0472, 8.3164",
            "in, if the weather holds"
        ]
    );
    assert_eq!(
        lines(&s, all, "@media"),
        vec![
            "video message 0:08",
            "voice 0:42",
            "sticker 🙈",
            "live location 55.7512, 37.6184 · 42 min left",
            "voice 0:12",
            "the numbers · file",
            "the garden today · photo",
            "location 47.0472, 8.3164",
            "the fold in motion · video",
            "new palette, what do you think · photo",
            "👋 Read the project notes · photo",
            "audio Dry Cleaning — Scratchcard Lanyard · 3:41",
            "file ticket-lisbon.pdf · 340 KB"
        ]
    );
    let in_chat = open_root(&mut s, Messages::in_chat(STELAXIS));
    let seed = {
        let inst = s.panel(in_chat).unwrap();
        let b = inst.borrow();
        assert_eq!(b.title(), "messages · stelaxis");
        drop(b);
        let mut b = inst.borrow_mut();
        b.as_any().downcast_mut::<Messages>().unwrap().seed_filter()
    };
    assert_eq!(seed, "@chat:stelaxis ");
    let n = lines(&s, in_chat, &seed).len();
    assert_eq!(n, 13, "the group's lines, service ones left out");
    assert!(verb_ids(&s, all).is_empty(), "a messages list wears no bar");
}

#[test]
fn unread_replies_open_at_the_message_and_only_visible_items_are_read() {
    let mut s = session();
    let chats = open_root(&mut s, Chats::id());
    assert_eq!(model::reply_count(s.store()), 2);
    verb(&mut s, chats, "telegram.replies");
    let inbox = s.joined_child(chats).unwrap();
    let rows = {
        let panel = s.panel(inbox).unwrap();
        let mut panel = panel.borrow_mut();
        let messages = panel.as_any().downcast_mut::<Messages>().unwrap();
        assert!(messages.is_replies());
        messages.rows(0, 50)
    };
    assert_eq!(rows.len(), 2, "a reply to me and a direct mention");
    let reply = &rows[0];
    assert_eq!(reply.text, "I saw it — the inset is 28 dp, it wants 34");
    assert_eq!(model::line(s.store(), STELAXIS, reply.id).unwrap().reply_name, "me");
    go(&mut s, Nav::Open { from: inbox, id: Chat::at(reply.chat, reply.id), fresh: false });
    let reader = s.joined_child(inbox).unwrap();
    with_chat(&s, reader, |c| {
        assert_eq!(c.cursor().map(|(_, id)| id), Some(reply.id));
        assert_eq!(c.take_follow_wish().map(|(_, id)| id), Some(reply.id), "the transcript scrolls to the reply");
        c.view_messages(&[], s.now());
    });
    assert_eq!(model::reply_count(s.store()), 2, "opening alone reads no notification");
    assert_eq!(unread(&s, STELAXIS).0, 8, "a targeted opening does not read the whole chat");
    with_chat(&s, reader, |c| c.view_messages(&[(c.peer(), reply.id)], s.now()));
    assert_eq!(model::reply_count(s.store()), 1);
    assert!(!model::line(s.store(), reply.chat, reply.id).unwrap().unread_mention);
    assert!(model::line(s.store(), rows[1].chat, rows[1].id).unwrap().unread_mention);
    with_chat(&s, reader, |c| c.view_messages(&[(c.peer(), reply.id)], s.now() + 10.0));
    assert_eq!(model::reply_count(s.store()), 1, "repeat views cannot decrement twice");
}

#[test]
fn unread_replies_include_archived_groups_and_exclude_other_conversations() {
    let mut s = session();
    s.store().write(|c| {
        for peer in [OLD_FLAT, ANNA, RUST_WEEKLY] {
            c.execute("UPDATE tg_chat SET mention = 1 WHERE peer = ?1", [peer])?;
            c.execute(
                "INSERT INTO tg_message(id, chat, date, text, unread_mention) VALUES(999999, ?1, 1, 'look here', 1)",
                [peer],
            )?;
        }
        Ok(())
    }).unwrap();
    assert_eq!(model::reply_count(s.store()), 3, "muted stelaxis and the archived group");
    let inbox = open_root(&mut s, Messages::replies(None));
    let panel = s.panel(inbox).unwrap();
    let mut panel = panel.borrow_mut();
    let messages = panel.as_any().downcast_mut::<Messages>().unwrap();
    let rows = messages.rows(0, 50);
    assert_eq!(rows.len(), 3);
    assert!(rows.iter().any(|m| m.chat == OLD_FLAT));
    assert!(rows.iter().all(|m| m.chat == OLD_FLAT || m.chat == STELAXIS));
}

#[test]
fn live_reply_views_wait_for_acknowledgment_and_can_retry() {
    let mut s = session();
    let unread_ids: Vec<_> = model::history(s.store(), STELAXIS).iter()
        .filter(|m| m.unread_mention).map(|m| m.id).collect();
    let reader = open_root(&mut s, Chat::at(STELAXIS, unread_ids[0]));
    let inbox = runtime::of(s.store()).connect();
    with_chat(&s, reader, |c| c.view_messages(&[(c.peer(), unread_ids[0])], s.now()));
    let request: serde_json::Value = serde_json::from_str(&inbox.try_recv().unwrap()).unwrap();
    assert_eq!(request["@type"], "viewMessages");
    assert_eq!(request["message_ids"], serde_json::json!([unread_ids[0]]));
    assert_eq!(model::reply_count(s.store()), 2, "enqueue is not acknowledgment");
    with_chat(&s, reader, |c| c.view_messages(&[(c.peer(), unread_ids[0])], s.now() + 1.0));
    assert!(inbox.try_recv().is_err(), "views are deduplicated while pending");
    with_chat(&s, reader, |c| c.view_messages(&[(c.peer(), unread_ids[0])], s.now() + 6.0));
    assert!(inbox.try_recv().is_ok(), "an unacknowledged view can retry");
}

/// Leave an ordinary line before two unread mentions at the end of the group.
fn add_trailing_mentions(s: &Session) -> i64 {
    let history = model::history(s.store(), STELAXIS);
    let ordinary = &history[history.len() - 3];
    assert!(!ordinary.unread_mention);
    let last = ordinary.id;
    s.store().write(move |c| {
        c.execute("UPDATE tg_message SET unread_mention = 1
            WHERE chat = ?1 AND id > ?2", [STELAXIS, last])?;
        c.execute("UPDATE tg_chat SET mention =
            (SELECT COUNT(*) FROM tg_message WHERE chat = ?1 AND unread_mention = 1)
            WHERE peer = ?1", [STELAXIS])?;
        Ok(())
    }).unwrap();
    last
}

#[test]
fn preview_reads_ordinary_messages_with_unread_mentions_at_the_end() {
    let mut s = session();
    let last = add_trailing_mentions(&s);
    let list = open_root(&mut s, Chats::id());
    let inbox = runtime::of(s.store()).connect();
    go(&mut s, Nav::Preview { from: list, id: Chat::id(STELAXIS) });
    assert_eq!(s.focus(), Some(list), "the transcript has not been focused");
    assert_eq!(unread(&s, STELAXIS).0, 2);
    assert_eq!(model::peer(s.store(), STELAXIS).unwrap().last_read, Some(last));
    assert_eq!(model::reply_count(s.store()), 4);
    #[cfg(feature = "tdlib")]
    {
        let request: serde_json::Value = serde_json::from_str(
            &inbox.try_recv().expect("the preview sends a read for the preceding ordinary message")
        ).unwrap();
        assert_eq!(request["@type"], "viewMessages");
        assert_eq!(request["chat_id"], STELAXIS);
        assert_eq!(request["message_ids"], serde_json::json!([last]));
        assert_eq!(request["force_read"], true);
    }
    assert!(inbox.try_recv().is_err(), "unseen mentions have no acknowledgment");

    account().on_update(s.world(), &serde_json::json!({
        "@type": "updateNewChat",
        "chat": {
            "id": STELAXIS, "title": "stelaxis",
            "type": {"@type": "chatTypeSupergroup", "is_channel": false},
            "positions": [{"list": {"@type": "chatListMain"}, "order": "100"}],
            "unread_count": 2, "last_read_inbox_message_id": last,
            "unread_mention_count": 4
        }
    }).to_string());
    assert_eq!(unread(&s, STELAXIS).0, 2, "the server confirms the same remaining count");
    assert_eq!(model::peer(s.store(), STELAXIS).unwrap().last_read, Some(last));
}

#[test]
fn batch_reads_ordinary_messages_with_unread_mentions_at_the_end() {
    let mut s = session();
    let last = add_trailing_mentions(&s);
    let family_last = model::history(s.store(), FAMILY).last().unwrap().id;
    let list = open_root(&mut s, Chats::id());
    with_chats(&s, list, |c| c.list_mut().marks_mut().extend([STELAXIS, FAMILY].map(|peer| format!("{peer}:0"))));
    let inbox = runtime::of(s.store()).connect();
    let before = s.history().rows();
    verb(&mut s, list, "telegram.read");
    assert_eq!(s.history().rows(), before, "read receipts do not enter undo history");
    let requests: Vec<serde_json::Value> = inbox.try_iter()
        .map(|raw| serde_json::from_str(&raw).unwrap()).collect();
    assert_eq!(requests.len(), 2, "one read for each marked group");
    assert_eq!(unread(&s, STELAXIS).0, 8, "a queued read waits for confirmation");
    for (peer, last, remaining) in [(STELAXIS, last, 2), (FAMILY, family_last, 0)] {
        let request = requests.iter().find(|r| r["chat_id"] == peer).unwrap();
        assert_eq!(request["@type"], "viewMessages");
        assert_eq!(request["message_ids"], serde_json::json!([last]));
        assert_eq!(request["force_read"], true);
        account().on_update(s.world(), &serde_json::json!({
            "@type": "ok", "@extra": request["@extra"]
        }).to_string());
        assert_eq!(unread(&s, peer).0, remaining);
        assert_eq!(model::peer(s.store(), peer).unwrap().last_read, Some(last));
    }
    assert_eq!(model::reply_count(s.store()), 4, "read n preserves every mention");
    assert_eq!(with_chats(&s, list, |c| c.list_mut().marks().len()), 0);
}

#[test]
fn ordinary_reads_with_a_stale_target_preserve_the_server_read_state() {
    for batch in [false, true] {
        for read_ahead in [0, 1] {
            let mut s = session();
            let last = add_trailing_mentions(&s);
            let read_before = last + read_ahead;
            s.store().write(move |c| {
                // Some of these unread messages aren't cached yet. Neither
                // an equal nor an older target can change the server count.
                c.execute("UPDATE tg_chat SET last_read = ?2, unread = 5 WHERE peer = ?1",
                    [STELAXIS, read_before])?;
                Ok(())
            }).unwrap();
            let list = open_root(&mut s, Chats::id());
            let inbox = runtime::of(s.store()).connect();
            if batch {
                with_chats(&s, list, |c| c.list_mut().marks_mut().add(format!("{STELAXIS}:0")));
                verb(&mut s, list, "telegram.read");
            } else {
                go(&mut s, Nav::Preview { from: list, id: Chat::id(STELAXIS) });
            }
            assert_eq!(unread(&s, STELAXIS).0, 5);
            assert_eq!(model::peer(s.store(), STELAXIS).unwrap().last_read, Some(read_before));
            for raw in inbox.try_iter() {
                let request: serde_json::Value = serde_json::from_str(&raw).unwrap();
                assert_eq!(request["message_ids"], serde_json::json!([last]));
                account().on_update(s.world(), &serde_json::json!({
                    "@type": "ok", "@extra": request["@extra"]
                }).to_string());
            }
            assert_eq!(unread(&s, STELAXIS).0, 5);
            assert_eq!(model::peer(s.store(), STELAXIS).unwrap().last_read, Some(read_before));
        }
    }
}

#[test]
fn ordinary_reads_preserve_unread_messages_missing_from_the_cache() {
    let mut s = session();
    let last = add_trailing_mentions(&s);
    s.store().write(move |c| {
        c.execute("DELETE FROM tg_message WHERE chat = ?1 AND id = ?2", [STELAXIS, last + 2])?;
        Ok(())
    }).unwrap();
    let list = open_root(&mut s, Chats::id());
    go(&mut s, Nav::Preview { from: list, id: Chat::id(STELAXIS) });
    assert_eq!(unread(&s, STELAXIS).0, 2, "one cached and one uncached message remain unread");
    assert_eq!(model::peer(s.store(), STELAXIS).unwrap().last_read, Some(last));
}

#[test]
fn redoing_an_ordinary_read_keeps_the_original_receipt_boundary() {
    let mut s = session();
    let last = add_trailing_mentions(&s);
    let before = model::peer(s.store(), STELAXIS).unwrap();
    let list = open_root(&mut s, Chats::id());
    let inbox = runtime::of(s.store()).connect();
    go(&mut s, Nav::Preview { from: list, id: Chat::id(STELAXIS) });
    assert_eq!(unread(&s, STELAXIS).0, 2);
    let _ = inbox.try_iter().count();
    assert!(s.undo());
    assert_eq!(unread(&s, STELAXIS).0, before.unread);
    assert_eq!(model::peer(s.store(), STELAXIS).unwrap().last_read, before.last_read);
    let arrived_at = model::history(s.store(), STELAXIS).last().unwrap().date + 1.0;
    s.store().write(move |c| {
        for (offset, out, service) in [(3, false, false), (4, true, false), (5, false, true)] {
            c.execute("INSERT INTO tg_message(id, chat, date, text, out, service)
                VALUES(?1, ?2, ?3, 'arrived while undone', ?4, ?5)",
                rusqlite::params![last + offset, STELAXIS, arrived_at, out, service])?;
        }
        c.execute("UPDATE tg_chat SET unread = unread + 1 WHERE peer = ?1", [STELAXIS])?;
        Ok(())
    }).unwrap();
    assert!(s.redo());
    assert_eq!(unread(&s, STELAXIS).0, 3, "the two mentions and the new incoming line stay unread");
    assert_eq!(model::peer(s.store(), STELAXIS).unwrap().last_read, Some(last));
    let reader = s.joined_child(list).unwrap();
    assert_eq!(with_chat(&s, reader, |c| c.first_unread()), Some(last + 1));
    assert!(inbox.try_recv().is_err(), "restoring the panel sends no new read receipt");
}

#[test]
fn preview_without_an_ordinary_line_keeps_the_chat_unread() {
    for keep_mentions in [true, false] {
        let mut s = session();
        // A newly discovered group may have only its unread replies cached,
        // or no messages at all while its first history request is pending.
        s.store().write(move |c| {
            c.execute("DELETE FROM tg_message
                WHERE chat = ?1 AND (unread_mention = 0 OR ?2 = 0)",
                rusqlite::params![STELAXIS, keep_mentions])?;
            Ok(())
        }).unwrap();
        let before = model::peer(s.store(), STELAXIS).unwrap();
        let list = open_root(&mut s, Chats::id());
        let inbox = runtime::of(s.store()).connect();
        go(&mut s, Nav::Preview { from: list, id: Chat::id(STELAXIS) });
        assert_eq!(s.focus(), Some(list));
        assert_eq!(unread(&s, STELAXIS).0, before.unread,
            "a preview cannot claim a read with no ordinary message to name");
        assert_eq!(model::peer(s.store(), STELAXIS).unwrap().last_read, before.last_read);
        assert_eq!(model::reply_count(s.store()), before.unread_mentions);
        assert!(inbox.try_recv().is_err(), "unseen mentions are not acknowledged");

        s.undo();
        s.settle();
        assert!(s.redo());
        s.settle();
        assert_eq!(unread(&s, STELAXIS).0, before.unread,
            "reopening has no deferred local read claim either");
        assert_eq!(model::peer(s.store(), STELAXIS).unwrap().last_read, before.last_read);
        assert!(inbox.try_recv().is_err());
    }
}

#[test]
fn batch_without_an_ordinary_line_keeps_the_skipped_chat_unread_and_marked() {
    let mut s = session();
    s.store().write(|c| {
        c.execute("DELETE FROM tg_message WHERE chat = ?1 AND unread_mention = 0", [STELAXIS])?;
        Ok(())
    }).unwrap();
    let before = model::peer(s.store(), STELAXIS).unwrap();
    let family_last = model::history(s.store(), FAMILY).last().unwrap().id;
    let list = open_root(&mut s, Chats::id());
    with_chats(&s, list, |c| c.list_mut().marks_mut().extend([STELAXIS, FAMILY].map(|peer| format!("{peer}:0"))));
    let inbox = runtime::of(s.store()).connect();
    verb(&mut s, list, "telegram.read");
    let request: serde_json::Value = serde_json::from_str(&inbox.try_recv().unwrap()).unwrap();
    assert_eq!(request["@type"], "viewMessages");
    assert_eq!(request["chat_id"], FAMILY);
    assert_eq!(request["message_ids"], serde_json::json!([family_last]));
    assert!(inbox.try_recv().is_err(), "there is no read for the group holding only mentions");
    account().on_update(s.world(), &serde_json::json!({
        "@type": "ok", "@extra": request["@extra"]
    }).to_string());
    assert_eq!(unread(&s, FAMILY).0, 0);
    assert_eq!(unread(&s, STELAXIS).0, before.unread);
    assert_eq!(model::peer(s.store(), STELAXIS).unwrap().last_read, before.last_read);
    assert_eq!(model::reply_count(s.store()), before.unread_mentions);
    assert_eq!(with_chats(&s, list, |c| c.list_mut().marks().keys()), vec![format!("{STELAXIS}:0")],
        "the skipped group stays marked for a later retry");
}

#[test]
fn the_people_are_the_address_book_and_a_group() {
    let mut s = session();
    let contacts = open_root(&mut s, Contacts::id());
    let names = |s: &Session, slot: SlotId, f: &str| -> Vec<String> {
        let inst = s.panel(slot).unwrap();
        let mut b = inst.borrow_mut();
        let p = b.as_any().downcast_mut::<People>().unwrap();
        p.list_mut().set_filter(f);
        p.rows(0, 50).into_iter().map(|r| r.name).collect()
    };
    assert_eq!(
        names(&s, contacts, ""),
        vec![
            "Elena Petrova",
            "Irina Rudenko",
            "Ivan Petrov",
            "Max Ivanov",
            "Olga Novak",
            "Sergey Rudenko",
            "Vera Kovac"
        ]
    );
    assert_eq!(
        names(&s, contacts, "@online"),
        vec!["Irina Rudenko", "Ivan Petrov", "Vera Kovac"]
    );
    let members = open_root(&mut s, Members::id(STELAXIS));
    assert_eq!(
        s.panel(members).unwrap().borrow().title(),
        "members · stelaxis"
    );
    let seed = {
        let inst = s.panel(members).unwrap();
        let mut b = inst.borrow_mut();
        b.as_any().downcast_mut::<People>().unwrap().seed_filter()
    };
    assert_eq!(names(&s, members, &seed).len(), 7);
    assert_eq!(
        names(&s, members, &format!("{seed}@admin")),
        vec!["Andrey Rudenko", "Vera Kovac"]
    );
    // A member's row says where it opens, and what it says under the name.
    let member = {
        let inst = s.panel(members).unwrap();
        let mut b = inst.borrow_mut();
        b.as_any().downcast_mut::<People>().unwrap().rows(0, 1).remove(0)
    };
    assert_eq!(member.group, Some(STELAXIS));
    assert_eq!(member.detail(), "online · @prepor · admin");
    names(&s, contacts, "");
    let contact = {
        let inst = s.panel(contacts).unwrap();
        let mut b = inst.borrow_mut();
        b.as_any().downcast_mut::<People>().unwrap().rows(0, 1).remove(0)
    };
    assert_eq!(contact.group, None);
    assert_eq!(contact.detail(), "last seen recently · @elena_p");
}

// -- the bars ------------------------------------------------------------------------------

/// Every bar telegram wears: no letter twice, and none of the ones the
/// workspace keeps for itself.
#[test]
fn no_bar_wears_a_letter_twice_or_a_reserved_one() {
    let mut s = session();
    let list = open_root(&mut s, Chats::id());
    with_chats(&s, list, |c| {
        c.go(0);
        c.toggle_mark();
    });
    let archive = open_root(&mut s, Chats::archive());
    with_chats(&s, archive, |c| {
        c.go(0);
        c.toggle_mark();
    });
    let mut slots = vec![list, archive];
    for id in [
        Chat::id(VERA),
        Chat::id(STELAXIS),
        Chat::id(RUST_WEEKLY),
        Chat::id(DEV),
        Peer::id(VERA),
        Peer::id(STELAXIS),
        Peer::id(RUST_WEEKLY),
        Peer::id(OLD_FLAT),
    ] {
        slots.push(open_root(&mut s, id));
    }
    // The chat with the cursor on my own line, marked, wears *edit* and the
    // batch twins too.
    let mine = open_root(&mut s, Chat::id(VERA));
    with_chat(&s, mine, |c| {
        c.walk(-1).map(|(_, id)| id);
        c.toggle_mark();
    });
    slots.push(mine);
    // The line's card over each kind of line, the viewer, and the place.
    let hist = model::history(s.store(), STELAXIS);
    for m in hist.iter().filter(|m| !m.service) {
        slots.push(open_root(&mut s, Line::id(STELAXIS, m.id)));
        if m.media.is_some() {
            slots.push(open_root(&mut s, Viewer::id(STELAXIS, m.id)));
        }
    }
    let hike = model::history(s.store(), HIKE);
    let mine_line = hike.iter().find(|m| m.out).expect("my line");
    slots.push(open_root(&mut s, Line::id(HIKE, mine_line.id)));
    let place_line = hike.iter().find(|m| m.media.as_ref().is_some_and(|md| md.kind == "location")).expect("a place");
    slots.push(open_root(&mut s, Line::id(HIKE, place_line.id)));
    slots.push(open_root(&mut s, Place::id(VERA)));
    // The attach panel in each of its states: empty; carrying three with
    // the cursor between them, so both trades are on the bar; with files
    // held; recording.
    let (_, empty) = chat_with_attach(&mut s, VERA);
    slots.push(empty);
    let (host, full) = chat_with_attach(&mut s, ELENA);
    with_chat(&s, host, |c| {
        c.carry(&["~/a.pdf".to_string(), "~/b.png".to_string(), "~/c.mp4".to_string()]);
    });
    observe(&s, full);
    with_attach(&s, full, |a| a.set_cursor(1));
    slots.push(full);
    let (_, held) = chat_with_attach(&mut s, MAX);
    with_attach(&s, held, |a| a.set_held(vec!["~/notes.md".to_string()]));
    slots.push(held);
    let (_, rec) = chat_with_attach(&mut s, ANNA);
    verb(&mut s, rec, "telegram.voice");
    slots.push(rec);

    for slot in slots {
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
            assert!(
                !seen.contains(&c),
                "two verbs on slot {slot} wear cmd+{c}: {:?}",
                verbs.iter().map(|v| v.id).collect::<Vec<_>>()
            );
            seen.push(c);
            assert!(
                v.label.to_lowercase().contains(c),
                "{}'s label {:?} does not carry its letter {c}",
                v.id,
                v.label
            );
        }
    }
    assert_eq!(
        verb_ids(&s, mine),
        vec![
            "telegram.attach",
            "telegram.line",
            "telegram.about",
            "telegram.forward",
            "telegram.delete",
            "telegram.clear"
        ]
    );
    assert_eq!(
        verb_ids(&s, full),
        vec![
            "telegram.browse",
            "telegram.remove",
            "telegram.earlier",
            "telegram.later",
            "telegram.voice",
            "telegram.video",
            "telegram.place"
        ]
    );
    assert_eq!(verb_ids(&s, rec), vec!["telegram.send_rec", "telegram.discard"]);
    // The rare verbs are on the line's card, edit only over mine.
    let mine_card = open_root(&mut s, Line::id(HIKE, mine_line.id));
    assert_eq!(
        verb_ids(&s, mine_card),
        vec![
            "telegram.reply",
            "telegram.edit",
            "telegram.forward",
            "telegram.copy",
            "telegram.react",
            "telegram.delete",
            "telegram.pin"
        ]
    );
    let place_card = open_root(&mut s, Line::id(HIKE, place_line.id));
    assert!(verb_ids(&s, place_card).ends_with(&["telegram.maps", "telegram.browser"]));
}

/// The attach panel edits what the composer carries, through the join:
/// what the files app holds is added in order, a row is traded with its
/// neighbours and put down, and the chat's bar counts what waits. Its
/// recordings run against the clock and end in a toast; its place opens
/// joined to it. Away from its chat it says so.
#[test]
fn the_attach_panel_edits_what_the_composer_carries() {
    let mut s = session();
    let chat = open_root(&mut s, Chat::id(VERA));
    assert_eq!(verb_ids(&s, chat), vec!["telegram.attach", "telegram.about"]);
    verb(&mut s, chat, "telegram.attach");
    let attach = s.joined_child(chat).expect("the attach panel, joined");
    assert_eq!(s.panel(attach).unwrap().borrow().id(), &Attach::id(VERA));
    observe(&s, attach);
    assert!(with_attach(&s, attach, |a| a.joined()));
    // Nothing held and no rows: the link to the files panel, the two
    // recordings and the place.
    assert_eq!(
        verb_ids(&s, attach),
        vec!["telegram.browse", "telegram.voice", "telegram.video", "telegram.place"]
    );

    // Held: `add` puts them on the chat's list in order, the cursor on the
    // last one, and the chat's bar counts them.
    let names = |s: &Session| {
        with_chat(s, chat, |c| c.carrying().iter().map(model::Carried::label).collect::<Vec<_>>())
    };
    with_attach(&s, attach, |a| {
        a.set_held(vec![
            "~/Downloads/report-q3.pdf".to_string(),
            "~/Downloads/screenshot-2026-08-30.png".to_string(),
        ]);
    });
    assert!(verb_ids(&s, attach).contains(&"telegram.add"));
    verb(&mut s, attach, "telegram.add");
    assert_eq!(names(&s), vec!["report-q3.pdf · file", "screenshot-2026-08-30.png · photo"]);
    assert_eq!(with_attach(&s, attach, |a| a.cursor()), Some(1));
    let labels: Vec<String> = s.panel(chat).unwrap().borrow().verbs().iter().map(|v| v.label.clone()).collect();
    assert!(labels.contains(&"attach 2".to_string()), "{labels:?}");
    // The same path again is passed over.
    with_attach(&s, attach, |a| a.set_held(vec!["~/Downloads/report-q3.pdf".to_string()]));
    verb(&mut s, attach, "telegram.add");
    assert_eq!(names(&s).len(), 2);

    // The cursor on the last row: `earlier` and not `later`; the trade
    // moves the row and the cursor with it, and the bar follows.
    assert_eq!(
        verb_ids(&s, attach),
        vec![
            "telegram.browse",
            "telegram.remove",
            "telegram.earlier",
            "telegram.voice",
            "telegram.video",
            "telegram.place"
        ]
    );
    verb(&mut s, attach, "telegram.earlier");
    assert_eq!(names(&s)[0], "screenshot-2026-08-30.png · photo");
    assert_eq!(with_attach(&s, attach, |a| a.cursor()), Some(0));
    let bar = verb_ids(&s, attach);
    assert!(!bar.contains(&"telegram.earlier") && bar.contains(&"telegram.later"));
    verb(&mut s, attach, "telegram.later");
    assert_eq!(names(&s)[1], "screenshot-2026-08-30.png · photo");
    assert_eq!(with_attach(&s, attach, |a| a.cursor()), Some(1));

    // `remove` puts the cursor's row down; the cursor stays in range, and
    // goes when the list does.
    verb(&mut s, attach, "telegram.remove");
    assert_eq!(names(&s), vec!["report-q3.pdf · file"]);
    assert_eq!(with_attach(&s, attach, |a| a.cursor()), Some(0));
    verb(&mut s, attach, "telegram.remove");
    assert!(names(&s).is_empty());
    assert_eq!(with_attach(&s, attach, |a| a.cursor()), None);
    assert!(!verb_ids(&s, attach).contains(&"telegram.remove"));

    // The composer sends the files with the text, and both go.
    with_attach(&s, attach, |a| a.set_held(vec!["~/notes.md".to_string()]));
    verb(&mut s, attach, "telegram.add");
    with_chat(&s, chat, |c| c.set_draft("here"));
    {
        let inst = s.panel(chat).unwrap();
        let mut b = inst.borrow_mut();
        b.as_any().downcast_mut::<Chat>().unwrap().send(&mut s);
    }
    assert!(with_chat(&s, chat, |c| c.carrying().is_empty() && c.draft().is_empty()));

    // A recording runs against the clock; while it runs the bar is its two
    // ways out; enter sends it, `discard` throws the other away.
    let t0 = s.now();
    verb(&mut s, attach, "telegram.voice");
    let rec = with_attach(&s, attach, |a| a.recording()).expect("recording");
    assert_eq!(rec.kind, RecKind::Voice);
    assert!(rec.line(t0 + 3.2).starts_with("recording voice 0:03"));
    assert_eq!(verb_ids(&s, attach), vec!["telegram.send_rec", "telegram.discard"]);
    {
        let inst = s.panel(attach).unwrap();
        let mut b = inst.borrow_mut();
        b.as_any().downcast_mut::<Attach>().unwrap().send_recording(&mut s, t0 + 3.2);
    }
    assert!(with_attach(&s, attach, |a| a.recording().is_none()));
    verb(&mut s, attach, "telegram.video");
    assert_eq!(with_attach(&s, attach, |a| a.recording().map(|r| r.kind)), Some(RecKind::Video));
    verb(&mut s, attach, "telegram.discard");
    assert!(with_attach(&s, attach, |a| a.recording().is_none()));

    // The place opens joined to the attach panel, with its two ways to send.
    verb(&mut s, attach, "telegram.place");
    let place = s.joined_child(attach).expect("the place, joined");
    assert_eq!(verb_ids(&s, place), vec!["telegram.send_place", "telegram.send_live"]);
    verb(&mut s, place, "telegram.send_place");

    // Away from its chat: no list, and `add` says so rather than adding.
    let alone = open_root(&mut s, Attach::id(VERA));
    observe(&s, alone);
    assert!(!with_attach(&s, alone, |a| a.joined()));
    with_attach(&s, alone, |a| a.set_held(vec!["~/notes.md".to_string()]));
    verb(&mut s, alone, "telegram.add");
    assert!(with_chat(&s, chat, |c| c.carrying().is_empty()));
}

/// A reply asks for the caret: from the bar over the cursor, and from the
/// line's card through the join, which takes the chat's focus too. The
/// wish is answered once.
#[test]
fn a_reply_asks_for_the_caret() {
    let mut s = session();
    let chat = open_root(&mut s, Chat::id(STELAXIS));
    assert!(!with_chat(&s, chat, Chat::take_field_wish));
    with_chat(&s, chat, |c| {
        c.walk(-1).map(|(_, id)| id);
    });
    verb(&mut s, chat, "telegram.reply");
    assert!(with_chat(&s, chat, |c| c.reply_to().is_some()));
    assert!(with_chat(&s, chat, Chat::take_field_wish));
    assert!(!with_chat(&s, chat, Chat::take_field_wish), "answered once");
    // No cursor: no reply on the bar at all.
    let fresh = open_root(&mut s, Chat::id(VERA));
    assert!(!verb_ids(&s, fresh).contains(&"telegram.reply"));
    // From the card: the chat is told and focused.
    verb(&mut s, chat, "telegram.line");
    let card = s.joined_child(chat).expect("the card, joined");
    assert_eq!(s.focus(), Some(card));
    verb(&mut s, card, "telegram.reply");
    assert_eq!(s.focus(), Some(chat));
    assert!(with_chat(&s, chat, Chat::take_field_wish));
}

/// The cursor's verbs: `reply` over any line, `edit` and `delete` over
/// mine, none without a cursor. Edit and delete are real on the store and
/// undone whole; the marks' `delete n` only while every marked line is
/// mine; the card edits through the join and deletes on its own.
#[test]
fn my_lines_are_edited_and_deleted_and_undone() {
    let mut s = session();
    let chat = open_root(&mut s, Chat::id(VERA));
    assert_eq!(verb_ids(&s, chat), vec!["telegram.attach", "telegram.about"]);
    let hist = model::history(s.store(), VERA);
    let hers = hist.iter().find(|m| !m.out && !m.service).expect("her line").id;
    let mine = hist.iter().rev().find(|m| m.out).expect("my line").clone();
    with_chat(&s, chat, |c| c.set_cursor((c.peer(), hers)));
    assert_eq!(
        verb_ids(&s, chat),
        vec![
            "telegram.reply",
            "telegram.copy",
            "telegram.react",
            "telegram.attach",
            "telegram.line",
            "telegram.about"
        ]
    );
    with_chat(&s, chat, |c| c.set_cursor((c.peer(), mine.id)));
    assert_eq!(
        verb_ids(&s, chat),
        vec![
            "telegram.reply",
            "telegram.edit",
            "telegram.delete",
            "telegram.copy",
            "telegram.react",
            "telegram.attach",
            "telegram.line",
            "telegram.about"
        ]
    );

    // Edit: the field takes the text, the line above says which, the caret
    // is asked for; enter writes it, and the draft comes back empty. Undo
    // gives the old text and the old flag back.
    with_chat(&s, chat, |c| c.set_draft("half a thought"));
    verb(&mut s, chat, "telegram.edit");
    assert_eq!(with_chat(&s, chat, |c| c.field_text().to_string()), mine.text);
    let above = with_chat(&s, chat, |c| c.above_line(s.now())).expect("the editing line");
    assert!(above.starts_with("editing: "), "{above}");
    assert!(with_chat(&s, chat, Chat::take_field_wish));
    with_chat(&s, chat, |c| c.typed("then we start at 7:00 sharp"));
    send(&mut s, chat);
    let text_of = |s: &Session| {
        model::history(s.store(), VERA)
            .iter()
            .find(|m| m.id == mine.id)
            .map(|m| (m.text.clone(), m.edited))
    };
    assert_eq!(text_of(&s), Some(("then we start at 7:00 sharp".to_string(), true)));
    assert!(with_chat(&s, chat, |c| c.editing().is_none()));
    assert_eq!(with_chat(&s, chat, |c| c.field_text().to_string()), "half a thought");
    s.undo();
    s.settle();
    assert_eq!(text_of(&s), Some((mine.text.clone(), mine.edited)));
    // An edit that changes nothing, or empties the line, writes nothing.
    verb(&mut s, chat, "telegram.edit");
    send(&mut s, chat);
    verb(&mut s, chat, "telegram.edit");
    with_chat(&s, chat, |c| c.typed("   "));
    send(&mut s, chat);
    assert_eq!(text_of(&s), Some((mine.text.clone(), mine.edited)));

    // Delete: the line goes, the cursor steps to the line before it, and
    // undo puts it back whole — the same id, the same date.
    let len = hist.len();
    with_chat(&s, chat, |c| c.set_cursor((c.peer(), mine.id)));
    verb(&mut s, chat, "telegram.delete");
    let after = model::history(s.store(), VERA);
    assert_eq!(after.len(), len - 1);
    assert!(after.iter().all(|m| m.id != mine.id));
    let at = hist.iter().position(|m| m.id == mine.id).unwrap();
    let before = hist[..at].iter().rev().find(|m| !m.service).unwrap().id;
    assert_eq!(with_chat(&s, chat, |c| c.cursor().map(|(_, id)| id)), Some(before));
    s.undo();
    s.settle();
    let back = model::history(s.store(), VERA);
    assert_eq!(back.len(), len);
    let restored = back.iter().find(|m| m.id == mine.id).expect("the line, back");
    assert_eq!((restored.text.as_str(), restored.date), (mine.text.as_str(), mine.date));

    // Marks: the batch has the bar, and `delete n` only while every marked
    // line is mine.
    with_chat(&s, chat, |c| {
        c.set_cursor((c.peer(), mine.id));
        c.toggle_mark();
    });
    let bar = verb_ids(&s, chat);
    assert!(bar.contains(&"telegram.delete") && !bar.contains(&"telegram.reply"));
    with_chat(&s, chat, |c| {
        c.set_cursor((c.peer(), hers));
        c.toggle_mark();
    });
    let bar = verb_ids(&s, chat);
    assert!(bar.contains(&"telegram.forward") && !bar.contains(&"telegram.delete"));

    // From the card: edit through the join takes the chat's focus; delete
    // on the card itself, the chat's cursor stepping off the line.
    with_chat(&s, chat, |c| {
        c.clear_marks();
        c.set_cursor((c.peer(), mine.id));
    });
    verb(&mut s, chat, "telegram.line");
    let card = s.joined_child(chat).expect("the card, joined");
    assert_eq!(
        verb_ids(&s, card),
        vec![
            "telegram.reply",
            "telegram.edit",
            "telegram.forward",
            "telegram.copy",
            "telegram.react",
            "telegram.delete",
            "telegram.pin"
        ]
    );
    verb(&mut s, card, "telegram.edit");
    assert_eq!(s.focus(), Some(chat));
    assert!(with_chat(&s, chat, |c| c.editing().is_some()));
    with_chat(&s, chat, Chat::cancel_edit);
    verb(&mut s, card, "telegram.delete");
    assert!(model::history(s.store(), VERA).iter().all(|m| m.id != mine.id));
    assert_eq!(with_chat(&s, chat, |c| c.cursor().map(|(_, id)| id)), Some(before));
    // Another's line's card wears neither.
    let hers_card = open_root(&mut s, Line::id(VERA, hers));
    assert_eq!(
        verb_ids(&s, hers_card),
        vec!["telegram.reply", "telegram.forward", "telegram.copy", "telegram.react", "telegram.pin"]
    );
}

#[test]
fn editing_and_deleting_a_link_restore_its_destination_on_undo() {
    use super::text::{Entity, EntityKind};

    let mut s = session();
    let mine = model::history(s.store(), VERA).iter().find(|m| m.out).unwrap().clone();
    let id = mine.id;
    let entities = vec![Entity { offset: 0, length: 4,
        kind: EntityKind::TextUrl { url: "https://example.org".into() } }];
    let original = entities.clone();
    s.store().write(move |c| model::edit_tx(c, VERA, id, "read this", false, Some(&original))).unwrap();
    super::verbs::edit_line(&mut s, VERA, id, "read this", false, "new text");
    s.settle();
    let current = |s: &Session| model::history(s.store(), VERA).iter().find(|m| m.id == id).unwrap().clone();
    assert!(current(&s).entities.is_none());
    s.undo();
    s.settle();
    assert_eq!(current(&s).entities, Some(entities.clone()));
    assert_eq!(current(&s).text, "read this");
    super::verbs::delete_lines(&mut s, VERA, vec![id]);
    s.settle();
    s.undo();
    s.settle();
    assert_eq!(current(&s).entities, Some(entities));
}

#[test]
fn undo_restores_a_received_empty_entity_list_without_enabling_detection() {
    let mut s = session();
    let id = model::history(s.store(), VERA).iter().find(|m| m.out).unwrap().id;
    let text = "main.rs https://example.org";
    s.store().write(move |c| model::edit_tx(c, VERA, id, text, false, Some(&[]))).unwrap();
    super::verbs::edit_line(&mut s, VERA, id, text, false, "new text");
    s.settle();
    s.undo();
    s.settle();
    let history = model::history(s.store(), VERA);
    let restored = history.iter().find(|m| m.id == id).unwrap();
    assert_eq!(restored.entities, Some(vec![]));
    assert!(!super::text::html(&restored.text, restored.entities.as_deref()).contains("<a "));
}

/// The viewer walks the chat's media in place; a line's card replies on
/// the chat it hangs under and plays against the clock.
#[test]
fn the_viewer_walks_and_the_card_replies_and_plays() {
    let mut s = session();
    let hist = model::history(s.store(), STELAXIS);
    let media: Vec<i64> = hist.iter().filter(|m| m.media.is_some()).map(|m| m.id).collect();
    let viewer = open_root(&mut s, Viewer::id(STELAXIS, media[0]));
    assert_eq!(verb_ids(&s, viewer), vec!["telegram.next", "telegram.open"]);
    verb(&mut s, viewer, "telegram.next");
    assert_eq!(s.panel(viewer).unwrap().borrow().id(), &Viewer::id(STELAXIS, media[1]));
    assert!(verb_ids(&s, viewer).starts_with(&["telegram.previous", "telegram.next"]));

    // A line's card, joined to its chat, replies on the chat's composer.
    let chat = open_root(&mut s, Chat::id(STELAXIS));
    with_chat(&s, chat, |c| {
        c.walk(-1).map(|(_, id)| id);
    });
    verb(&mut s, chat, "telegram.line");
    let card = s.joined_child(chat).expect("the card, joined");
    verb(&mut s, card, "telegram.reply");
    let line = with_chat(&s, chat, |c| c.reply_line(s.now())).expect("replying");
    assert!(line.starts_with("reply to Ivan Petrov"), "{line}");
    // Its player runs against the clock.
    let voice = hist.iter().find(|m| m.media.as_ref().is_some_and(|md| md.kind == "voice")).unwrap();
    let vcard = open_root(&mut s, Line::id(STELAXIS, voice.id));
    let now = s.now();
    verb(&mut s, vcard, "telegram.play");
    let inst = s.panel(vcard).unwrap();
    let mut b = inst.borrow_mut();
    let l = b.as_any().downcast_mut::<Line>().unwrap();
    let st = l.player_state(voice, now + 10.0).unwrap();
    assert!(st.playing && (st.position - 10.0).abs() < 1e-6 && st.length == 42.0);
}

/// A clip nobody can play yet is still the round-one line.
///
/// The one thing a build with no engine can prove about video: a row that
/// names a clip, with no file on the device and nothing to ask for it with,
/// keeps its poster and its fake timeline — so the panels library, the demo
/// world and the suites draw exactly what they drew before. What replaces
/// them, the platform's player over the cached file, needs a signed-in run.
#[test]
fn a_clip_with_nothing_behind_it_keeps_the_poster_and_the_timeline() {
    let mut s = session();
    let hist = model::history(s.store(), STELAXIS);
    let video = hist
        .iter()
        .find(|m| m.media.as_ref().is_some_and(|md| md.kind == "video"))
        .expect("a video line in the demo world")
        .clone();
    // Name the clip on it, the way the live projection would; nothing else
    // about the row changes.
    let id = video.id;
    s.store()
        .write(move |c| {
            c.execute(
                "UPDATE tg_message SET media_clip = 'tg:reef', media_clip_rid = 'RID' WHERE id = ?1",
                [id],
            )
        })
        .expect("name the clip");

    let slot = open_root(&mut s, Viewer::id(STELAXIS, id));
    let inst = s.panel(slot).expect("the viewer");
    let now = s.now();
    let line = model::line(s.store(), STELAXIS, id).expect("the line");
    {
        let mut b = inst.borrow_mut();
        let v = b.as_any().downcast_mut::<Viewer>().unwrap();
        assert_eq!(line.media.as_ref().unwrap().clip.as_deref(), Some("tg:reef"));
        // Asking is a no-op with no engine, so the clip is never this
        // panel's to play and there is no file to point a player at.
        v.ask_for_clip(&line);
        assert!(!v.plays_clip(&line), "nothing to play it with");
        assert_eq!(v.download_note(&line), None, "and nothing to wait for");
        assert!(v.clip_file(&line).is_none(), "the store is in memory");
    }
    // So `play` runs the timeline against the clock, as it always has.
    verb(&mut s, slot, "telegram.play");
    let mut b = inst.borrow_mut();
    let v = b.as_any().downcast_mut::<Viewer>().unwrap();
    let st = v.player_state(&line, now + 4.0).expect("the fake timeline");
    assert!(st.playing, "so the button reads pause");
    assert!((st.position - 4.0).abs() < 1e-6, "four seconds in");
}

/// Inline play requests the clip without navigating or pretending the
/// native video has advanced while its download is still pending.
#[test]
fn inline_video_play_downloads_on_demand_and_stays_in_the_chat() {
    use crate::shell::widgets::media::PlayerState;

    let mut s = session();
    let mut video = model::history(s.store(), STELAXIS).iter()
        .find(|m| m.media.as_ref().is_some_and(|md| md.kind == "video")).unwrap().clone();
    // No poster: the full clip alone must still identify a real video.
    let md = video.media.as_mut().unwrap();
    md.reference = None;
    md.clip = Some("tg:inline-clip".into());
    md.secs = None;
    let chat = open_root(&mut s, Chat::id(STELAXIS));
    let inbox = runtime::of(s.store()).connect();
    let now = s.now();
    assert!(inbox.try_recv().is_err(), "opening a chat must not download its videos");
    with_chat(&s, chat, |c| {
        assert!(c.player_state(&video, now).is_some(), "play remains available without a duration");
        c.toggle_play(&video, now);
        let player = c.playback((c.peer(), video.id)).unwrap();
        assert!(player.plays_clip(&video));
        assert!(player.running());
        assert_eq!(player.player_state(&video, now + 8.0).unwrap().position, 0.0);
        assert!(player.download_note(&video).is_some());
    });
    assert_eq!(s.focus(), Some(chat));
    assert_eq!(s.panel(chat).unwrap().borrow().id(), &Chat::id(STELAXIS));
    let request: serde_json::Value = serde_json::from_str(&inbox.try_recv().unwrap()).unwrap();
    assert_eq!(request["@type"], "getMessage");
    assert_eq!(request["message_id"], video.id);
    with_chat(&s, chat, |c| {
        c.toggle_play(&video, now + 1.0);
        assert!(!c.playback((c.peer(), video.id)).unwrap().running(), "pause while downloading must stick");
        c.toggle_play(&video, now + 2.0);
        c.playback((c.peer(), video.id)).unwrap().set_native_state(PlayerState {
            playing: true, position: 3.5, length: 14.0,
        });
        c.set_cursor((c.peer(), video.id));
        let state = c.player_state(&video, now + 20.0).unwrap();
        assert_eq!(state.position, 3.5, "the native frame owns the clock, even after selection");
        c.pause(now + 20.0);
        assert!(!c.player_state(&video, now + 30.0).unwrap().playing);
        assert_eq!(c.player_state(&video, now + 30.0).unwrap().position, 3.5);
    });
    assert!(inbox.try_recv().is_err(), "toggles reuse the pending download");

    let voice = model::history(s.store(), STELAXIS).iter()
        .find(|m| m.media.as_ref().is_some_and(|md| md.kind == "voice")).unwrap().clone();
    with_chat(&s, chat, |c| {
        c.toggle_play(&voice, now + 30.0);
        assert!(c.player_state(&voice, now + 31.0).unwrap().playing);
        assert!(!c.player_state(&video, now + 31.0).unwrap().playing);
        assert!(c.playback((c.peer(), video.id)).is_none(), "another message takes over the player");
    });
}

/// A card opened through the bar must hand playback off even while its chat
/// remains visible. Starting again in the chat or viewer transfers it back.
#[test]
fn telegram_panels_transfer_playback_without_overlapping() {
    use crate::shell::widgets::media::PlayerState;

    for native in [false, true] {
        let mut s = session();
        let mut video = model::history(s.store(), STELAXIS).iter()
            .find(|m| m.media.as_ref().is_some_and(|md| md.kind == "video")).unwrap().clone();
        if native {
            let id = video.id;
            s.store().write(move |c| c.execute(
                "UPDATE tg_message SET media_clip = 'tg:handoff' WHERE id = ?1", [id],
            )).unwrap();
            video = model::line(s.store(), STELAXIS, id).unwrap();
        }
        let chat = open_root(&mut s, Chat::id(STELAXIS));
        let _inbox = native.then(|| runtime::of(s.store()).connect());
        let now = s.now();
        with_chat(&s, chat, |c| {
            c.set_cursor((c.peer(), video.id));
            c.toggle_play(&video, now);
            if native {
                c.playback((c.peer(), video.id)).unwrap().set_native_state(PlayerState {
                    playing: true, position: 3.5, length: 14.0,
                });
            }
        });
        verb(&mut s, chat, "telegram.line");
        let card = s.focus().unwrap();
        assert_eq!(s.panel(card).unwrap().borrow().id(), &Line::id(STELAXIS, video.id));
        verb(&mut s, card, "telegram.play");
        with_chat(&s, chat, |c| {
            assert!(!c.player_state(&video, now).unwrap().playing);
            if native {
                let p = c.playback((c.peer(), video.id)).unwrap();
                // A late native update cannot reclaim another panel's turn.
                p.set_native_state(PlayerState { playing: true, position: 3.5, length: 14.0 });
                assert!(!p.running());
                assert_eq!(p.player_state(&video, now).unwrap().position, 3.5);
            }
        });
        let card_panel = s.panel(card).unwrap();
        {
            let mut b = card_panel.borrow_mut();
            let l = b.as_any().downcast_mut::<Line>().unwrap();
            assert!(l.player_state(&video, now).unwrap().playing);
        }
        with_chat(&s, chat, |c| c.toggle_play(&video, now));
        {
            let mut b = card_panel.borrow_mut();
            let l = b.as_any().downcast_mut::<Line>().unwrap();
            assert!(!l.player_state(&video, now).unwrap().playing,
                "returning to the transcript must also pause the card");
        }
        verb(&mut s, card, "telegram.open");
        let viewer = s.focus().unwrap();
        verb(&mut s, viewer, "telegram.play");
        with_chat(&s, chat, |c| assert!(!c.player_state(&video, now).unwrap().playing));
        let viewer_panel = s.panel(viewer).unwrap();
        let mut b = viewer_panel.borrow_mut();
        assert!(b.as_any().downcast_mut::<Viewer>().unwrap().player_state(&video, now).unwrap().playing);

        // An isolated fixture/session with the same message id owns its own audio.
        let other = session();
        let mut other_player = super::panels::playback::Playback::new(other.store().clone(), video.key());
        other_player.toggle_play(&video, now);
        assert!(b.as_any().downcast_mut::<Viewer>().unwrap().player_state(&video, now).unwrap().playing);
    }
}

/// The viewer reads live byte counts for its own file, including before the
/// first bytes arrive and when Telegram only knows an approximate size.
#[test]
fn the_viewer_shows_downloaded_and_total_bytes_until_the_file_lands() {
    use kernel::app::{Apps, Env, Mode, Workers};
    use kernel::caps::{BlobCache, ClockSource, FakeClock, BLOB_BUDGET_DEFAULT};
    use kernel::effect::World;
    use kernel::store::Store;
    use serde_json::json;
    use std::rc::Rc;

    let dir = std::env::temp_dir()
        .join(format!("superapp-tg-viewer-progress-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let engine = dir.join("tdlib");
    std::fs::create_dir_all(&engine).unwrap();
    let apps = Apps::new(APPS);
    let store = Rc::new(Store::open(Some(&dir.join("store.sqlite")), &apps.schemas()).unwrap());
    apps.seed(&store, Mode::Fake).unwrap();
    let clock = FakeClock::default();
    let env = Env {
        blobs: BlobCache::at(dir.join("blobs"), BLOB_BUDGET_DEFAULT),
        clock: ClockSource::Virtual(clock.clone()),
        ..Env::default()
    };
    let world = Rc::new(World::new(store, apps.capabilities(Mode::Fake, &env), apps.registry()));
    let workers = Workers::inline(APPS, world.clone());
    let mut s = Session::new(apps, world, workers, Mode::Fake);
    let td = FakeTd::new();
    let acc = sync::Account::new(td.clone(), 17844, engine.clone(), None);
    acc.drain(s.world());

    let history = model::history(s.store(), STELAXIS);
    let video = history.iter().find(|m| m.media.as_ref().is_some_and(|md| md.kind == "video")).unwrap().id;
    let photo = history.iter().find(|m| m.media.as_ref().is_some_and(|md| md.kind == "photo")).unwrap().id;
    s.store().write(move |c| {
        c.execute(
            "UPDATE tg_message SET media_ref = 'tg:poster', media_rid = 'RID_POSTER', \
             media_clip = 'tg:clip', media_clip_rid = 'RID_CLIP' WHERE chat = ?1 AND id = ?2",
            [STELAXIS, video],
        )?;
        c.execute(
            "UPDATE tg_message SET media_ref = 'tg:poster', media_rid = 'RID_POSTER' \
             WHERE chat = ?1 AND id = ?2",
            [STELAXIS, photo],
        )
    }).unwrap();
    let viewer = open_root(&mut s, Viewer::id(STELAXIS, video));
    let picture = open_root(&mut s, Viewer::id(STELAXIS, photo));
    let signin = open_root(&mut s, SignIn::id());
    let note = |s: &Session, slot| {
        let inst = s.panel(slot).unwrap();
        let mut borrow = inst.borrow_mut();
        let v = borrow.as_any().downcast_mut::<Viewer>().unwrap();
        let m = v.msg().unwrap();
        v.ask_for_clip(&m);
        v.ask_for_picture(&m);
        v.download_note(&m)
    };
    assert_eq!(note(&s, viewer).as_deref(), Some("connecting to Telegram…"));
    assert_eq!(note(&s, picture).as_deref(), Some("connecting to Telegram…"));
    runtime::of(s.store()).want_history(STELAXIS);
    acc.drain(s.world());
    assert!(td.sent().is_empty(), "downloads and history must wait for authorization");

    let auth = |state| json!({
        "@type": "updateAuthorizationState", "authorization_state": {"@type": state}
    }).to_string();
    acc.on_update(s.world(), &auth("authorizationStateWaitTdlibParameters"));
    let parameters: serde_json::Value = serde_json::from_str(&td.sent()[0]).unwrap();
    acc.on_update(s.world(), &json!({
        "@type": "error", "code": 400, "message": "Can't lock file: already in use",
        "@extra": parameters["@extra"]
    }).to_string());
    let blocked = "Telegram is open in another app instance · close it to continue";
    assert_eq!(note(&s, viewer).as_deref(), Some(blocked));
    assert_eq!(note(&s, picture).as_deref(), Some(blocked));
    assert_eq!(with_signin(&s, signin, |p| p.note()).as_deref(), Some(blocked));

    clock.advance(4.0);
    acc.drain(s.world());
    assert_eq!(td.sent_types(), vec!["setTdlibParameters"]);
    clock.advance(1.0);
    acc.drain(s.world());
    acc.drain(s.world());
    assert_eq!(td.sent_types(), vec!["setTdlibParameters", "setTdlibParameters"]);

    // When the lock is released, the retry succeeds. The same open viewers
    // receive their queued files without needing another play or reopen.
    acc.on_update(s.world(), &auth("authorizationStateReady"));
    acc.drain(s.world());
    assert_eq!(td.sent_types(), vec![
        "setTdlibParameters", "setTdlibParameters", "loadChats",
    ]);
    assert_eq!(note(&s, viewer).as_deref(), Some("loading media details…"));
    assert_eq!(note(&s, picture).as_deref(), Some("loading media details…"));
    // Authorization precedes chat restoration. Neither viewer may lose its
    // request to a premature "Chat not found" response.
    acc.on_update(s.world(), &json!({"@type": "updateNewChat", "chat": {
        "id": STELAXIS, "title": "fixture", "type": {"@type": "chatTypeSupergroup"}
    }}).to_string());
    acc.drain(s.world());
    assert!(!td.sent_types().iter().any(|t| t == "getChatHistory"), "history waits for the chat lists");
    let source_extra = |id| td.sent().iter()
        .map(|r| serde_json::from_str::<serde_json::Value>(r).unwrap())
        .find(|r| r["@type"] == "getMessage" && r["message_id"] == id).unwrap()["@extra"].clone();
    let video_extra = source_extra(video);
    let photo_extra = source_extra(photo);
    assert!(!td.sent_types().iter().any(|t| t == "getRemoteFile" || t == "downloadFile"));
    for list in ["main", "archive"] {
        acc.on_update(s.world(), &json!({"@type": "error", "code": 404,
            "@extra": format!("load_chats:{list}")}).to_string());
    }
    acc.drain(s.world());
    assert!(td.sent_types().iter().any(|t| t == "getChatHistory"), "history starts after the chat lists load");

    let mb = 1024 * 1024;
    let mut file = json!({
        "@type": "file", "id": 77,
        "size": 48 * mb, "expected_size": 60 * mb,
        "local": {"is_downloading_active": false, "is_downloading_completed": false, "downloaded_size": 0},
        "remote": {"unique_id": "clip", "id": "REFRESHED_CLIP"}
    });
    acc.on_update(s.world(), &json!({"@type": "message", "id": video, "chat_id": STELAXIS,
        "@extra": video_extra, "content": {"@type": "messageVideo", "video": {
            "width": 640, "height": 360, "duration": 30, "video": file
        }}}).to_string());
    assert_eq!(note(&s, viewer).as_deref(), Some("downloading · 0 B / 48 MB"));
    let download: serde_json::Value = serde_json::from_str(td.sent().last().unwrap()).unwrap();
    assert_eq!(download["@type"], "downloadFile");
    assert_eq!(download["synchronous"], true, "later failures must reach the viewer");
    assert_eq!(download["file_id"], 77);

    file.as_object_mut().unwrap().remove("@extra");
    file["local"]["is_downloading_active"] = json!(true);
    file["local"]["downloaded_size"] = json!(12 * mb);
    file["local"]["downloaded_prefix_size"] = json!(mb);
    // An asynchronous downloadFile reply also carries a current snapshot.
    acc.on_update(s.world(), &file.to_string());
    assert_eq!(note(&s, viewer).as_deref(), Some("downloading · 12 MB / 48 MB"));

    let mut poster = json!({
        "@type": "file", "id": 78, "size": 2 * mb,
        "local": {"is_downloading_active": true, "downloaded_size": mb},
        "remote": {"unique_id": "poster"}
    });
    acc.on_update(s.world(), &json!({"@type": "message", "id": photo, "chat_id": STELAXIS,
        "@extra": photo_extra, "content": {"@type": "messagePhoto", "photo": {"sizes": [{
            "width": 640, "height": 480, "photo": poster
        }]}}}).to_string());
    let update = |file: &serde_json::Value| json!({"@type": "updateFile", "file": file}).to_string();
    acc.on_update(s.world(), &update(&poster));
    assert_eq!(note(&s, picture).as_deref(), Some("downloading · 1.0 MB / 2.0 MB"));
    assert_eq!(note(&s, viewer).as_deref(), Some("downloading · 12 MB / 48 MB"));

    // Some Telegram videos carry no thumbnail. Their clip still owns the
    // progress display and must become playable when its bytes arrive.
    s.store().write(move |c| {
        c.execute(
            "UPDATE tg_message SET media_ref = NULL, media_rid = NULL WHERE chat = ?1 AND id = ?2",
            [STELAXIS, video],
        )
    }).unwrap();
    assert_eq!(note(&s, viewer).as_deref(), Some("downloading · 12 MB / 48 MB"));

    file["size"] = json!(0);
    file["expected_size"] = json!(0);
    acc.on_update(s.world(), &update(&file));
    assert_eq!(note(&s, viewer).as_deref(), Some("downloading · 12 MB · total unknown"));
    file["expected_size"] = json!(48 * mb);
    acc.on_update(s.world(), &update(&file));
    assert_eq!(note(&s, viewer).as_deref(), Some("downloading · 12 MB / ~48 MB"));

    // The real failure sequence: bytes stop, then the completion response
    // fails. Late file snapshots must not replace the error with a spinner.
    file["local"]["is_downloading_active"] = json!(false);
    acc.on_update(s.world(), &update(&file));
    acc.on_update(s.world(), &json!({"@type": "error", "code": 400,
        "message": "File download has failed or was canceled", "@extra": download["@extra"]
    }).to_string());
    assert!(note(&s, viewer).unwrap().contains("failed or was canceled"));
    acc.on_update(s.world(), &update(&file));
    assert!(note(&s, viewer).unwrap().contains("failed or was canceled"));
    let sent = td.sent().len();
    acc.drain(s.world());
    assert_eq!(td.sent().len(), sent, "failed downloads do not retry in a loop");
    super::operations::retry(s.store(), download["@extra"]["operation"].as_u64().unwrap());
    acc.drain(s.world());
    file["local"]["is_downloading_active"] = json!(true);
    acc.on_update(s.world(), &update(&file));
    assert_eq!(note(&s, viewer).as_deref(), Some("downloading · 12 MB / ~48 MB"));

    let clip_path = engine.join("clip.mp4");
    std::fs::write(&clip_path, b"mp4 bytes").unwrap();
    file["size"] = json!(48 * mb);
    file["local"] = json!({
        "path": clip_path, "is_downloading_active": false,
        "is_downloading_completed": true, "downloaded_size": 48 * mb
    });
    acc.on_update(s.world(), &update(&file));
    assert_eq!(note(&s, viewer), None, "the clip is ready, even while its poster downloads");
    assert_eq!(runtime::of(s.store()).download("tg:clip"), None);

    let photo_path = engine.join("photo.png");
    std::fs::write(&photo_path, super::seed::demo_bytes("demo:palette").unwrap()).unwrap();
    poster["local"] = json!({"path": photo_path, "is_downloading_completed": true});
    acc.on_update(s.world(), &update(&poster));
    assert_eq!(note(&s, picture), None);
    assert_eq!(runtime::of(s.store()).download("tg:poster"), None);

    // TDLib then forgets its local copy. That update must not revive progress.
    poster["local"] = json!({"is_downloading_active": false, "downloaded_size": 0});
    acc.on_update(s.world(), &update(&poster));
    assert_eq!(runtime::of(s.store()).download("tg:poster"), None);
    drop(s);
    drop(env);
    std::fs::remove_dir_all(dir).unwrap();
}

/// The links off a card go where they say.
#[test]
fn the_card_links_to_the_chat_its_messages_and_the_members() {
    let mut s = session();
    let card = open_root(&mut s, Peer::id(STELAXIS));
    assert_eq!(
        verb_ids(&s, card),
        vec![
            "telegram.mute",
            "telegram.pin",
            "telegram.archive",
            "telegram.chat",
            "telegram.search",
            "telegram.members",
            "telegram.leave"
        ]
    );
    // `search` is the card's and not the chat's, since 2026-09-05.
    let chat = open_root(&mut s, Chat::id(STELAXIS));
    assert!(!verb_ids(&s, chat).contains(&"telegram.search"));
    verb(&mut s, card, "telegram.search");
    assert_eq!(
        s.panel(s.joined_child(card).expect("the messages, joined")).unwrap().borrow().id(),
        &Messages::in_chat(STELAXIS)
    );
    verb(&mut s, card, "telegram.members");
    let members = s.joined_child(card).expect("the members, joined");
    assert_eq!(
        s.panel(members).unwrap().borrow().id(),
        &Members::id(STELAXIS)
    );
    // A person's card has no members and nothing to leave; a muted chat's
    // says unmute; the archived one says unarchive.
    let person = open_root(&mut s, Peer::id(ANNA));
    assert!(!verb_ids(&s, person).contains(&"telegram.members"));
    assert!(!verb_ids(&s, person).contains(&"telegram.leave"), "one leaves a group");
    // A channel is left too, though nobody is listed in it.
    let channel = open_root(&mut s, Peer::id(RUST_WEEKLY));
    assert!(!verb_ids(&s, channel).contains(&"telegram.members"));
    assert!(verb_ids(&s, channel).contains(&"telegram.leave"));
    let group = open_root(&mut s, Peer::id(STELAXIS));
    let labels: Vec<String> = s.panel(group).unwrap().borrow().verbs().iter().map(|v| v.label.clone()).collect();
    assert_eq!(labels[0], "unmute");
    assert_eq!(labels[1], "unpin");
    let old = open_root(&mut s, Peer::id(OLD_FLAT));
    assert!(verb_ids(&s, old).contains(&"telegram.unarchive"));
}

#[test]
fn blocked_lines_cannot_start_a_reply_or_edit() {
    let mut s = session();
    let chat = open_root(&mut s, Chat::id(VERA));
    let mine = model::history(s.store(), VERA).iter().find(|m| m.out).unwrap().id;
    with_chat(&s, chat, |c| {
        c.set_cursor((c.peer(), mine));
        c.set_draft("keep this draft");
    });
    verb(&mut s, chat, "telegram.line");
    let line = s.joined_child(chat).unwrap();
    assert!(verb_ids(&s, line).contains(&"telegram.reply"));
    assert!(verb_ids(&s, line).contains(&"telegram.edit"));

    s.store().write(|c| model::set_blocked_tx(c, VERA, true)).unwrap();
    for slot in [chat, line] {
        for action in ["telegram.reply", "telegram.edit"] {
            assert!(!verb_ids(&s, slot).contains(&action));
            // A click already queued before the block must be refused too.
            s.panel(slot).unwrap().borrow_mut().run(action, &mut s);
            s.settle();
        }
    }
    assert_eq!(s.focus(), Some(line), "a refused action never focuses the hidden composer");
    with_chat(&s, chat, |c| {
        c.reply((c.peer(), mine));
        assert!(!c.edit((c.peer(), mine)), "direct callers cannot bypass the block");
        assert_eq!(c.reply_to(), None);
        assert!(c.editing().is_none());
        assert_eq!(c.field_text(), "keep this draft");
        assert!(!c.take_field_wish());
        assert!(c.above_line(s.now()).is_none());
    });

    s.store().write(|c| model::set_blocked_tx(c, VERA, false)).unwrap();
    verb(&mut s, line, "telegram.reply");
    assert_eq!(with_chat(&s, chat, |c| c.reply_to()), Some(mine));
    verb(&mut s, line, "telegram.edit");
    assert_eq!(with_chat(&s, chat, |c| c.editing().unwrap().msg.1), mine);
}

#[test]
fn blocking_hides_existing_replies_and_edits_until_unblocked() {
    for editing in [false, true] {
        let mut s = session();
        let chat = open_root(&mut s, Chat::id(VERA));
        let mine = model::history(s.store(), VERA).iter().find(|m| m.out).unwrap().id;
        with_chat(&s, chat, |c| {
            c.set_draft("keep this draft");
            if editing {
                assert!(c.edit((c.peer(), mine)));
                c.typed("keep this edit");
            } else {
                c.reply((c.peer(), mine));
            }
        });
        let text = field_now(&s, chat);
        let above = with_chat(&s, chat, |c| c.above_line(s.now()));
        assert!(above.is_some());
        s.store().write(|c| model::set_blocked_tx(c, VERA, true)).unwrap();
        assert!(with_chat(&s, chat, |c| c.above_line(s.now()).is_none()));
        send(&mut s, chat);
        assert_eq!(field_now(&s, chat), text, "blocking keeps unsent work");
        assert_eq!(draft_row(&s, VERA), "keep this draft");
        s.store().write(|c| model::set_blocked_tx(c, VERA, false)).unwrap();
        assert_eq!(with_chat(&s, chat, |c| c.above_line(s.now())), above);
        assert_eq!(field_now(&s, chat), text);
    }
}

#[test]
fn ended_sessions_release_pending_peer_actions_and_restore_the_bars() {
    for state in ["authorizationStateLoggingOut", "authorizationStateClosing", "authorizationStateClosed"] {
        let mut s = session();
        let profile = open_root(&mut s, Peer::id(VERA));
        let chat = open_root(&mut s, Chat::id(VERA));
        let runtime = runtime::of(s.store());
        let inbox = runtime.connect();
        let acc = account();
        verb(&mut s, profile, "telegram.block");
        verb(&mut s, profile, "telegram.confirm");
        let _request = inbox.try_recv().unwrap();
        // Telegram can announce the block before acknowledging the request.
        s.store().write(|c| model::set_blocked_tx(c, VERA, true)).unwrap();
        assert!(verb_ids(&s, profile).is_empty());
        assert!(!verb_ids(&s, chat).contains(&"telegram.unblock"));

        acc.on_update(s.world(), &serde_json::json!({
            "@type": "updateAuthorizationState", "authorization_state": { "@type": state },
        }).to_string());
        s.settle();
        assert!(!runtime.peer_action_pending(VERA), "{state}");
        assert!(verb_ids(&s, profile).contains(&"telegram.unblock"));
        assert!(verb_ids(&s, chat).contains(&"telegram.unblock"));
        assert!(s.notes().last().unwrap().msg.contains("disconnected"));
        assert!(model::peer(s.store(), VERA).unwrap().blocked, "an interrupted request has no assumed result");
        verb(&mut s, chat, "telegram.unblock");
        assert!(!runtime.peer_action_pending(VERA), "a closed session cannot queue another action");

        let reconnected = runtime.connect();
        verb(&mut s, chat, "telegram.unblock");
        assert!(runtime.peer_action_pending(VERA));
        assert!(reconnected.try_recv().unwrap().contains("setMessageSenderBlockList"));
    }
}

#[test]
fn blocking_waits_for_confirmation_and_acknowledgement() {
    use serde_json::{json, Value};
    let mut s = session();
    let profile = open_root(&mut s, Peer::id(VERA));
    let chat = open_root(&mut s, Chat::id(VERA));
    let inbox = runtime::of(s.store()).connect();
    let acc = account();

    verb(&mut s, profile, "telegram.block");
    assert_eq!(verb_ids(&s, profile), vec!["telegram.confirm", "telegram.cancel"]);
    assert!(inbox.try_recv().is_err());
    verb(&mut s, profile, "telegram.cancel");
    assert!(!model::peer(s.store(), VERA).unwrap().blocked);

    verb(&mut s, profile, "telegram.block");
    verb(&mut s, profile, "telegram.confirm");
    let req: Value = serde_json::from_str(&inbox.try_recv().unwrap()).unwrap();
    assert_eq!(req["@type"], "setMessageSenderBlockList");
    assert_eq!(req["sender_id"], json!({"@type": "messageSenderUser", "user_id": VERA}));
    assert_eq!(req["block_list"], json!({"@type": "blockListMain"}));
    assert!(!model::peer(s.store(), VERA).unwrap().blocked, "queueing is not success");
    assert!(verb_ids(&s, profile).is_empty(), "no duplicate action while waiting");
    acc.on_update(s.world(), &json!({
        "@type": "error", "code": 400, "message": "USER_ID_INVALID", "@extra": req["@extra"],
    }).to_string());
    s.settle();
    assert!(!model::peer(s.store(), VERA).unwrap().blocked);
    assert!(s.notes().last().unwrap().msg.contains("USER_ID_INVALID"));

    verb(&mut s, profile, "telegram.block");
    verb(&mut s, profile, "telegram.confirm");
    let req: Value = serde_json::from_str(&inbox.try_recv().unwrap()).unwrap();
    acc.on_update(s.world(), &json!({"@type": "ok", "@extra": req["@extra"]}).to_string());
    s.settle();
    let card = model::peer(s.store(), VERA).unwrap();
    assert!(card.blocked && card.is_contact && card.in_main);
    assert_eq!(card.status_line(), "blocked");
    assert!(!card.can_post());
    assert!(verb_ids(&s, profile).contains(&"telegram.unblock"));
    assert!(verb_ids(&s, chat).contains(&"telegram.unblock"));
    assert!(!verb_ids(&s, chat).contains(&"telegram.attach"));

    with_chat(&s, chat, |c| c.set_draft("keep these words"));
    send(&mut s, chat);
    assert_eq!(draft_row(&s, VERA), "keep these words");
    assert!(inbox.try_recv().is_err(), "a blocked composer sends nothing");
    let place = open_root(&mut s, Place::id(VERA));
    verb(&mut s, place, "telegram.send_place");
    assert!(inbox.try_recv().is_err(), "an open location panel cannot bypass the block");
    let list = open_root(&mut s, Chats::id());
    with_chats(&s, list, |c| {
        let i = c.rows(0, 100).iter().position(|r| r.peer == VERA).unwrap();
        c.go(i);
    });
    runtime::of(s.store()).carry_forward(STELAXIS, vec![1]);
    verb(&mut s, list, "telegram.forward_here");
    assert!(inbox.try_recv().is_err(), "forwarding to a blocked user sends nothing");
    assert!(runtime::of(s.store()).pending_forward().is_some());
    verb(&mut s, chat, "telegram.unblock");
    let req: Value = serde_json::from_str(&inbox.try_recv().unwrap()).unwrap();
    assert_eq!(req["@type"], "setMessageSenderBlockList");
    assert!(req["block_list"].is_null());
    assert!(model::peer(s.store(), VERA).unwrap().blocked);
    acc.on_update(s.world(), &json!({"@type": "ok", "@extra": req["@extra"]}).to_string());
    s.settle();
    assert!(model::peer(s.store(), VERA).unwrap().can_post());
    assert_eq!(field_now(&s, chat), "keep these words");
}

#[test]
fn deleting_a_contact_keeps_the_conversation_and_deleting_the_chat_keeps_the_block() {
    use serde_json::{json, Value};
    let mut s = session();
    let profile = open_root(&mut s, Peer::id(VERA));
    let contacts = open_root(&mut s, Contacts::id());
    let history = model::history(s.store(), VERA);
    let inbox = runtime::of(s.store()).connect();
    let acc = account();
    s.store().write(|c| model::set_blocked_tx(c, VERA, true)).unwrap();

    verb(&mut s, profile, "telegram.delete_contact");
    assert!(inbox.try_recv().is_err());
    verb(&mut s, profile, "telegram.confirm");
    let req: Value = serde_json::from_str(&inbox.try_recv().unwrap()).unwrap();
    assert_eq!(req["@type"], "removeContacts");
    assert_eq!(req["user_ids"], json!([VERA]));
    assert!(model::peer(s.store(), VERA).unwrap().is_contact);
    acc.on_update(s.world(), &json!({"@type": "ok", "@extra": req["@extra"]}).to_string());
    s.settle();
    let card = model::peer(s.store(), VERA).unwrap();
    assert!(!card.is_contact && card.blocked && card.in_main);
    assert_eq!(model::history(s.store(), VERA), history);
    assert!(!verb_ids(&s, profile).contains(&"telegram.delete_contact"));
    let inst = s.panel(contacts).unwrap();
    let mut people = inst.borrow_mut();
    assert!(!people.as_any().downcast_mut::<People>().unwrap().rows(0, 100).iter().any(|p| p.id == VERA));
    drop(people);

    // A failed deletion must leave the chat, messages and search index intact.
    for fail in [true, false] {
        verb(&mut s, profile, "telegram.delete");
        verb(&mut s, profile, "telegram.confirm");
        let req: Value = serde_json::from_str(&inbox.try_recv().unwrap()).unwrap();
        assert_eq!(req["@type"], "deleteChatHistory");
        assert_eq!(req["chat_id"], VERA);
        assert_eq!(req["remove_from_chat_list"], true);
        assert_eq!(req["revoke"], false);
        assert_eq!(model::history(s.store(), VERA), history);
        acc.on_update(s.world(), &json!({
            "@type": if fail { "error" } else { "ok" }, "@extra": req["@extra"],
            "code": 400, "message": "CHAT_DELETE_FAILED",
        }).to_string());
        s.settle();
        if fail {
            assert_eq!(model::history(s.store(), VERA), history);
            assert!(model::peer(s.store(), VERA).unwrap().in_main);
            assert!(s.notes().last().unwrap().msg.contains("CHAT_DELETE_FAILED"));
        }
    }
    let card = model::peer(s.store(), VERA).unwrap();
    assert!(card.blocked && !card.in_main && !card.archived);
    assert!(model::history(s.store(), VERA).is_empty());
    assert!(super::project::search_local(s.store().conn(), Some(VERA), "thermos").is_empty());
    assert!(verb_ids(&s, profile).contains(&"telegram.unblock"));
    assert!(!verb_ids(&s, profile).contains(&"telegram.delete"));
    let mut engine = Engine::inline(s.apps().providers());
    engine.ask(s.store(), 1, "vera");
    let hits: Vec<_> = engine.collect().into_iter().flat_map(|a| a.hits).collect();
    let hit = hits.iter().find(|h| h.label == "Vera Kovac").unwrap();
    assert_eq!(hit.detail, "blocked");
    assert_eq!(hit.go, Go::Open(Peer::id(VERA)), "the user can still find and unblock them");
}

#[test]
fn user_actions_exclude_self_and_groups_and_offline_actions_do_not_claim_success() {
    let mut s = session();
    for peer in [SELF, STELAXIS, RUST_WEEKLY] {
        let profile = open_root(&mut s, Peer::id(peer));
        for action in ["telegram.block", "telegram.unblock", "telegram.delete_contact"] {
            assert!(!verb_ids(&s, profile).contains(&action));
            s.panel(profile).unwrap().borrow_mut().run(action, &mut s);
            assert!(!runtime::of(s.store()).peer_action_pending(peer));
        }
        assert!(!model::peer(s.store(), peer).unwrap().blocked);
    }
    let profile = open_root(&mut s, Peer::id(VERA));
    for action in ["telegram.block", "telegram.delete_contact", "telegram.delete"] {
        verb(&mut s, profile, action);
        verb(&mut s, profile, "telegram.confirm");
        assert!(s.notes().last().unwrap().msg.starts_with("draft: nothing leaves"));
    }
    let card = model::peer(s.store(), VERA).unwrap();
    assert!(!card.blocked && card.is_contact && card.in_main);
}

#[test]
fn a_contact_without_a_chat_fetches_their_block_state_on_open() {
    use serde_json::{json, Value};
    let mut s = session();
    let inbox = runtime::of(s.store()).connect();
    let profile = open_root(&mut s, Peer::id(IVAN));
    let req: Value = serde_json::from_str(&inbox.try_recv().unwrap()).unwrap();
    assert_eq!(req["@type"], "getUserFullInfo");
    assert_eq!(req["user_id"], IVAN);
    account().on_update(s.world(), &json!({
        "@type": "userFullInfo", "@extra": req["@extra"],
        "block_list": {"@type": "blockListMain"},
    }).to_string());
    assert!(verb_ids(&s, profile).contains(&"telegram.unblock"));
    assert!(!verb_ids(&s, profile).contains(&"telegram.delete"));
}

/// Off the wire the verbs about a chat say what would have left and change
/// nothing — the demo world is what a store with no account shows. Leaving
/// is the one that acts anyway: the conversation goes from the store, the
/// peer stays, and the card closes behind it.
#[test]
fn the_chat_verbs_toast_off_the_wire_and_leaving_still_leaves() {
    let mut s = session();
    let card = open_root(&mut s, Peer::id(STELAXIS));
    let muted = |s: &Session| model::peer(s.store(), STELAXIS).expect("the card").muted;
    let was = muted(&s);
    verb(&mut s, card, "telegram.mute");
    assert_eq!(muted(&s), was, "nothing left the machine, so nothing flipped");
    let said = |s: &Session| s.notes().last().map(|n| n.msg.clone()).unwrap_or_default();
    assert!(
        said(&s).contains("unmute"),
        "the toast says what would have gone: {:?}",
        said(&s)
    );

    // The batch verbs of the list say the same, over the count they name,
    // and the marks stay for the retry that a signed-in build would make.
    let list = open_root(&mut s, Chats::id());
    with_chats(&s, list, |c| {
        c.go(0);
        c.toggle_mark();
        c.go(1);
        c.toggle_mark();
    });
    verb(&mut s, list, "telegram.read");
    assert!(said(&s).contains("read 2 chats"), "{:?}", said(&s));
    assert_eq!(unread(&s, STELAXIS).0, 8, "nothing was read: nothing left");
    verb(&mut s, list, "telegram.archive");
    assert!(said(&s).contains("archive 2 chats"), "{:?}", said(&s));
    assert_eq!(with_chats(&s, list, |c| c.list_mut().marks().len()), 2, "the marks stay");

    // Leaving: the lines, the membership and the chat row go; the peer, and
    // everything that points at its name, stays.
    let lines = |s: &Session| model::history(s.store(), STELAXIS).len();
    assert!(lines(&s) > 0, "the group has a transcript to lose");
    verb(&mut s, card, "telegram.leave");
    s.settle();
    assert_eq!(lines(&s), 0, "the transcript is gone");
    let card_after = model::peer(s.store(), STELAXIS).expect("the peer stays known");
    assert_eq!(card_after.name, "stelaxis");
    assert_eq!(card_after.unread, 0, "a chat that is gone has no flags");
    assert!(s.panel(card).is_none(), "the card closed behind it");
    // The list has let it go, and nothing crashes on the way past.
    assert!(!titles(&s, list).contains(&"stelaxis".to_string()));
    let chat = open_root(&mut s, Chat::id(STELAXIS));
    assert_eq!(s.panel(chat).unwrap().borrow().title(), "stelaxis");
}

// -- the search source ---------------------------------------------------------------------

#[test]
fn the_search_source_finds_chats_people_and_lines() {
    let s = session();
    let mut engine = Engine::inline(s.apps().providers());
    assert_eq!(engine.slots(), 1, "telegram supplies exactly one source");

    // A name reaches the chat with them; a word reaches the lines that
    // carry it, each opening its chat at that line.
    engine.ask(s.store(), 1, "vera");
    let hits: Vec<_> = engine.collect().into_iter().flat_map(|a| a.hits).collect();
    assert!(hits.len() >= 2, "{hits:?}");
    assert_eq!(hits[0].label, "Vera Kovac");
    assert_eq!(hits[0].detail, "online");
    assert_eq!(hits[0].go, Go::Open(Chat::id(VERA)));
    assert!(hits[1].label.contains("Vera"), "{:?}", hits[1]);

    engine.ask(s.store(), 2, "thermos");
    let hits: Vec<_> = engine.collect().into_iter().flat_map(|a| a.hits).collect();
    assert_eq!(hits.len(), 1, "{hits:?}");
    assert_eq!(hits[0].label, "and the thermos");
    assert_eq!(hits[0].detail, "me in Hiking Saturday");
    let hist = model::history(s.store(), HIKE);
    let id = hist.last().expect("lines").id;
    assert_eq!(hits[0].go, Go::Open(Chat::at(HIKE, id)));

    // A kind is a word too: `channel` finds both.
    engine.ask(s.store(), 3, "channel");
    let hits: Vec<_> = engine.collect().into_iter().flat_map(|a| a.hits).collect();
    let labels: Vec<&str> = hits.iter().map(|h| h.label.as_str()).collect();
    assert!(labels.contains(&"Rust Weekly") && labels.contains(&"superapp dev"), "{labels:?}");

    // Nobody is offered whose page would be empty.
    engine.ask(s.store(), 4, "elena");
    let hits: Vec<_> = engine.collect().into_iter().flat_map(|a| a.hits).collect();
    assert_eq!(hits[0].go, Go::Open(Chat::id(ELENA)));
}

/// The seed's clock: everything sits around the virtual epoch, so the list
/// spells today's lines by the hour and yesterday's by the weekday.
#[test]
fn the_seed_sits_around_the_virtual_epoch() {
    let mut s = session();
    let list = open_root(&mut s, Chats::id());
    let rows = with_chats(&s, list, |c| c.rows(0, 50));
    let when: Vec<String> = rows
        .iter()
        .map(|r| model::when(r.last, virtual_epoch()))
        .collect();
    assert_eq!(
        when,
        vec!["11:52", "11:40", "10:03", "09:30", "08:16", "mon", "mon", "sat", "thu", "12.08"]
    );
    assert_eq!(rows[0].last, ts(2026, 9, 1, 11, 52));
}

// -- signing in ----------------------------------------------------------------------------

/// Writes the one session row the sign-in panel reads back, on the store's
/// own writer — the seam the worker uses, exercised here with no worker.
fn set_session(s: &Session, state: &str, phone: Option<&str>, detail: Option<&str>) {
    let state = state.to_string();
    let phone = phone.map(str::to_string);
    let detail = detail.map(str::to_string);
    s.store()
        .write(move |c| schema::set_session(c, phone.as_deref(), &state, detail.as_deref(), 0.0))
        .expect("write the session row");
}

fn with_signin<T>(s: &Session, slot: SlotId, f: impl FnOnce(&mut SignIn) -> T) -> T {
    let inst = s.panel(slot).expect("a panel in the slot");
    let mut b = inst.borrow_mut();
    f(b.as_any().downcast_mut::<SignIn>().expect("a sign-in panel"))
}

/// The panel maps each session state to the field it shows and the verb it
/// wears: connecting has neither, each wait-state its own field and word, and
/// ready says who signed in. The panel reads the row live, so one open reflects
/// every state written under it.
#[test]
fn signin_maps_each_state_to_its_field_and_verb() {
    let mut s = session();
    let slot = open_root(&mut s, SignIn::id());
    let labels = |s: &Session| -> Vec<String> {
        s.panel(slot)
            .unwrap()
            .borrow()
            .verbs()
            .iter()
            .map(|v| v.label.clone())
            .collect()
    };

    // Closed by default — a store no account has touched.
    with_signin(&s, slot, |p| {
        assert_eq!(p.state(), "closed");
        assert_eq!(p.field_kind(), None);
        assert_eq!(p.line(), "not started");
    });
    assert!(labels(&s).is_empty(), "nothing to send while closed");

    // Connecting: a line, still nothing to type.
    set_session(&s, "connecting", None, None);
    with_signin(&s, slot, |p| {
        assert_eq!(p.field_kind(), None);
        assert_eq!(p.line(), "connecting to Telegram…");
    });
    assert!(labels(&s).is_empty());

    // The phone.
    set_session(&s, "wait_phone", None, None);
    with_signin(&s, slot, |p| assert_eq!(p.field_kind(), Some(Field::Phone)));
    assert_eq!(labels(&s), vec!["send phone"]);

    // The code, with the delivery hint beside the field.
    set_session(&s, "wait_code", Some("+4915150525562"), Some("sms · 5"));
    with_signin(&s, slot, |p| {
        assert_eq!(p.field_kind(), Some(Field::Code));
        assert_eq!(p.hint().as_deref(), Some("sms · 5"));
    });
    assert_eq!(labels(&s), vec!["send code"]);

    // The two-factor password, with its own hint.
    set_session(&s, "wait_password", None, Some("your cat"));
    with_signin(&s, slot, |p| {
        assert_eq!(p.field_kind(), Some(Field::Password));
        assert_eq!(p.hint().as_deref(), Some("your cat"));
    });
    assert_eq!(labels(&s), vec!["send password"]);

    // Signed in: who, and that the chats are coming down. The phone the code
    // step filed rides through the later states that carry none.
    set_session(&s, "ready", None, None);
    with_signin(&s, slot, |p| {
        assert_eq!(p.field_kind(), None);
        assert_eq!(p.line(), "signed in as +4915150525562");
        // The note counts what the store holds — the demo world's rows here.
        let note = p.note().expect("a note once signed in");
        assert!(!note.starts_with("syncing"), "{note}");
        assert!(note.ends_with(" lines") && note.contains(" chats · "), "{note}");
        assert!(!note.contains("· 0 lines"), "the demo world has lines: {note}");
    });
    assert!(labels(&s).is_empty());
}

/// The send writes nothing to the store itself: TDLib's answer is the worker's
/// to project, so a press in 'wait_phone' leaves the session row exactly where
/// it stood. Without the engine the send is an inert toast; with it, it reaches
/// the shared client — neither touches `tg_session`.
#[test]
fn signin_send_leaves_the_session_row_to_the_worker() {
    let mut s = session();
    let slot = open_root(&mut s, SignIn::id());
    set_session(&s, "wait_phone", None, None);
    with_signin(&s, slot, |p| p.edited("+4915150525562".to_string()));
    verb(&mut s, slot, "telegram.signin");
    // The panel advanced nothing — the state is still the worker's to move.
    assert_eq!(schema::session(s.store().conn()).state, "wait_phone");
}

/// The three request builders the panel shares with the state machine carry
/// the right `@type` and the value they were handed — so the panel reuses the
/// JSON rather than spelling it a second time.
#[test]
fn the_auth_builders_carry_their_type_and_value() {
    let phone: serde_json::Value =
        serde_json::from_str(&requests::set_authentication_phone("+4915150525562")).unwrap();
    assert_eq!(phone["@type"], "setAuthenticationPhoneNumber");
    assert_eq!(phone["phone_number"], "+4915150525562");

    let code: serde_json::Value =
        serde_json::from_str(&requests::check_authentication_code("12345")).unwrap();
    assert_eq!(code["@type"], "checkAuthenticationCode");
    assert_eq!(code["code"], "12345");

    let pw: serde_json::Value =
        serde_json::from_str(&requests::check_authentication_password("hunter2")).unwrap();
    assert_eq!(pw["@type"], "checkAuthenticationPassword");
    assert_eq!(pw["password"], "hunter2");
}

// -- the live verbs, off the wire ----------------------------------------------

/// The phase-4 wire builders carry the right `@type` and fields: a send with
/// and without the line it answers, an edit, a delete with its revoke flag, and
/// a read. The reply rides the *input* form `inputMessageReplyToMessage` — the
/// `InputMessageReplyTo` that `sendMessage` takes — not the received-message
/// `messageReplyToMessage`, and is absent entirely when nothing is answered.
#[test]
fn the_wire_builders_carry_their_type_and_fields() {
    let v = |s: String| serde_json::from_str::<serde_json::Value>(&s).unwrap();

    // A plain send: the text as a formattedText inside an inputMessageText, and
    // no reply_to at all.
    let send = v(requests::send_message(7, "on my way", None));
    assert_eq!(send["@type"], "sendMessage");
    assert_eq!(send["chat_id"], 7);
    assert_eq!(send["input_message_content"]["@type"], "inputMessageText");
    assert_eq!(send["input_message_content"]["text"]["@type"], "formattedText");
    assert_eq!(send["input_message_content"]["text"]["text"], "on my way");
    assert!(send.get("reply_to").is_none(), "no reply, no reply_to field");

    // A reply: the input reply form, by message id.
    let reply = v(requests::send_message(7, "yes", Some(42)));
    assert_eq!(reply["reply_to"]["@type"], "inputMessageReplyToMessage");
    assert_eq!(reply["reply_to"]["message_id"], 42);

    // An edit reuses the same inputMessageText over a chat/message pair.
    let edit = v(requests::edit_message_text(7, 42, "fixed"));
    assert_eq!(edit["@type"], "editMessageText");
    assert_eq!(edit["chat_id"], 7);
    assert_eq!(edit["message_id"], 42);
    assert_eq!(edit["input_message_content"]["text"]["text"], "fixed");

    // A delete for everyone: the ids as an array, revoke true; and revoke false
    // is the delete-for-me alone.
    let del = v(requests::delete_messages(7, &[1, 2, 3], true));
    assert_eq!(del["@type"], "deleteMessages");
    assert_eq!(del["chat_id"], 7);
    assert_eq!(del["message_ids"], serde_json::json!([1, 2, 3]));
    assert_eq!(del["revoke"], true);
    assert_eq!(v(requests::delete_messages(7, &[1], false))["revoke"], false);

    // A read: force_read, so the chat's count clears, not merely that the line
    // was shown.
    let view = v(requests::view_messages(7, &[99]));
    assert_eq!(view["@type"], "viewMessages");
    assert_eq!(view["chat_id"], 7);
    assert_eq!(view["message_ids"], serde_json::json!([99]));
    assert_eq!(view["force_read"], true);
}

/// Off the wire — the default build links no engine — a send is the demo toast
/// and empties the composer just the same: the draft, the line it answered and
/// anything carried are all let go, and a plain send never panics. The wire
/// gating never gets in the way of the clearing the demo has always done.
#[test]
fn a_send_clears_the_composer_with_no_engine() {
    let mut s = session();
    let chat = open_root(&mut s, Chat::id(VERA));

    // A reply with a draft: both the reply line and the text are set.
    let hers = model::history(s.store(), VERA)
        .iter()
        .find(|m| !m.out && !m.service)
        .expect("her line")
        .id;
    with_chat(&s, chat, |c| {
        c.reply((c.peer(), hers));
        c.set_draft("see you at seven");
    });
    assert_eq!(with_chat(&s, chat, |c| c.reply_to()), Some(hers));

    send(&mut s, chat);

    // The demo clearing, wire or no wire: nothing left in the composer.
    with_chat(&s, chat, |c| {
        assert_eq!(c.draft(), "");
        assert_eq!(c.reply_to(), None);
        assert!(c.carrying().is_empty());
    });

    // Carrying files, the files are the message and the words their caption
    // — so off the wire it is one toast counting them, and the list is put
    // down with the draft.
    with_chat(&s, chat, |c| {
        c.carry(&["~/a.png".to_string(), "~/clip.mp4".to_string()]);
        c.set_draft("here");
    });
    send(&mut s, chat);
    let said = s.notes().last().map(|n| n.msg.clone()).unwrap_or_default();
    assert!(said.contains("send with 2 files"), "{said}");
    with_chat(&s, chat, |c| {
        assert_eq!(c.draft(), "");
        assert!(c.carrying().is_empty());
    });
}

/// Forwarding is a pick of the chat, as the client's forward sheet is: the
/// verb takes the marked lines out of the transcript and opens the list,
/// which wears *forward here* only while lines wait for it. The pick spends
/// them — off the wire nothing leaves and the toast says what would have,
/// but a pick made is a pick made — and *clear* is the way out of one.
#[test]
fn forward_waits_for_a_chat_and_the_list_picks_it() {
    let mut s = session();
    let chat = open_root(&mut s, Chat::id(VERA));
    let list = open_root(&mut s, Chats::id());
    assert!(
        !verb_ids(&s, list).contains(&"telegram.forward_here"),
        "nothing waits for a chat yet"
    );

    // Two lines marked, and forward: the marks have done their work, the
    // lines wait on the app, and the list offers the pick.
    let hist = model::history(s.store(), VERA);
    let ids: Vec<i64> = hist.iter().filter(|m| !m.service).take(2).map(|m| m.id).collect();
    let mark = |s: &Session| {
        with_chat(s, chat, |c| {
            for id in &ids {
                c.set_cursor((c.peer(), *id));
                c.toggle_mark();
            }
        });
    };
    mark(&s);
    verb(&mut s, chat, "telegram.forward");
    assert!(with_chat(&s, chat, |c| c.marks().is_empty()));
    let waiting = runtime::of(s.store()).pending_forward().expect("the lines wait for a chat");
    assert_eq!(waiting.messages, ids.iter().map(|&id| (VERA, id)).collect::<Vec<_>>());
    assert!(verb_ids(&s, list).contains(&"telegram.forward_here"));

    // The pick: the chat under the cursor takes them, and the waiting ends.
    with_chats(&s, list, |c| {
        c.go(0);
    });
    verb(&mut s, list, "telegram.forward_here");
    let said = s.notes().last().map(|n| n.msg.clone()).unwrap_or_default();
    assert!(said.contains("forward 2 lines to Vera Kovac"), "{said}");
    assert!(runtime::of(s.store()).take_forward().is_none(), "the pick spent the forward");
    assert!(!verb_ids(&s, list).contains(&"telegram.forward_here"));

    // And the way out of one: *clear* lets the lines go without sending
    // them anywhere.
    mark(&s);
    verb(&mut s, chat, "telegram.forward");
    assert!(runtime::of(s.store()).pending_forward().is_some());
    verb(&mut s, list, "telegram.clear");
    assert!(runtime::of(s.store()).pending_forward().is_none());
}

/// *copy* puts the line's words on the clipboard, and a line with none —
/// a picture — puts what the transcript says it is. It goes through the
/// effect, so the log keeps the row and a world that may not touch a
/// human's clipboard would refuse it out loud.
#[test]
fn copy_takes_the_line_through_the_clipboard() {
    let mut s = session();
    let copied = |s: &mut Session, count| {
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        while s.notes().iter().filter(|note| note.msg == "copied").count() < count {
            s.settle();
            assert!(std::time::Instant::now() < deadline, "the clipboard completion arrived: {:?}", s.notes());
            std::thread::sleep(std::time::Duration::from_millis(1));
        }
    };
    let chat = open_root(&mut s, Chat::id(STELAXIS));
    let hist = model::history(s.store(), STELAXIS);
    let words = hist
        .iter()
        .find(|m| !m.service && !m.text.is_empty())
        .expect("a line with words")
        .clone();
    with_chat(&s, chat, |c| c.set_cursor((c.peer(), words.id)));
    verb(&mut s, chat, "telegram.copy");
    copied(&mut s, 1);
    assert_eq!(s.notes().last().map(|n| n.msg.clone()).unwrap_or_default(), "copied");
    // The ring the log reads in-memory effects out of: nothing is filed for
    // one, so what it kept is the sentence the effect described itself with.
    let clipped = |s: &Session| {
        let ring: Vec<serde_json::Value> =
            serde_json::from_str(&s.store().mem().json()).expect("the effect ring");
        let last = ring.last().expect("the copy is in the ring").clone();
        assert_eq!(last["kind"], "clip");
        last["what"].as_str().unwrap_or_default().to_string()
    };
    assert_eq!(clipped(&s), format!("copy the line ({} bytes)", words.text.len()));

    // A line with no words says what it is instead: the card's own copy,
    // over the recording the transcript draws as *voice 0:12*.
    let voice = model::history(s.store(), FAMILY)
        .iter()
        .find(|m| m.text.is_empty() && m.media.is_some())
        .expect("a line with nothing written on it")
        .clone();
    let card = open_root(&mut s, Line::id(FAMILY, voice.id));
    verb(&mut s, card, "telegram.copy");
    copied(&mut s, 2);
    let label = voice.media.expect("the recording").line(s.now());
    assert_eq!(label, "voice 0:12");
    assert_eq!(clipped(&s), format!("copy the line ({} bytes)", label.len()));
}


// -- whose session, whose engine ------------------------------------------------

/// An account over a fake transport: what the *worker* does with an answer
/// from the engine, which no panel can make happen. The api_id is the demo
/// file's and the directory a scratch one — nothing opens a client.
fn account() -> sync::Account<FakeTd> {
    sync::Account::new(
        FakeTd::new(),
        17844,
        std::env::temp_dir().join("superapp-tg-panel-tests"),
        None,
    )
}

fn poll_reactions(s: &mut Session, slot: SlotId) -> bool {
    let panel = s.panel(slot).unwrap();
    let mut panel = panel.borrow_mut();
    if let Some(chat) = panel.as_any().downcast_mut::<Chat>() {
        return chat.poll(s);
    }
    panel.as_any().downcast_mut::<Line>().unwrap().poll_reactions(s)
}

fn last_reaction_request(td: &FakeTd, kind: &str) -> serde_json::Value {
    td.sent().iter().rev()
        .map(|raw| serde_json::from_str::<serde_json::Value>(raw).unwrap())
        .find(|v| v["@type"] == kind)
        .expect("the worker sent the reaction request")
}

/// Picker behavior tests begin after the fake client has restored its lists.
/// Startup ordering is exercised separately by the worker's protocol tests.
fn connected_reaction_account(s: &Session, td: FakeTd) -> sync::Account<FakeTd> {
    let acc = sync::Account::new(td, 17844, std::env::temp_dir(), None);
    acc.on_ready(s.world());
    for list in ["main", "archive"] {
        acc.on_update(s.world(), &serde_json::json!({"@type": "error", "code": 404,
            "@extra": format!("load_chats:{list}")}).to_string());
    }
    acc
}


fn answer_reaction_snapshot(acc: &sync::Account<FakeTd>, s: &Session, td: &FakeTd) {
    let request = last_reaction_request(td, "getMessage");
    assert!(request["@extra"]["context"].as_str().unwrap().starts_with("undo_snapshot:"));
    acc.on_update(s.world(), &serde_json::json!({
        "@type": "message", "chat_id": request["chat_id"], "id": request["message_id"],
        "@extra": request["@extra"], "interaction_info": {"reactions": {"reactions": []}},
    }).to_string());
}

fn offer_reactions(acc: &sync::Account<FakeTd>, s: &Session, request: &serde_json::Value, emojis: &[&str]) {
    acc.on_update(s.world(), &serde_json::json!({
        "@type": "availableReactions", "@extra": request["@extra"],
        "top_reactions": emojis.iter().map(|emoji| serde_json::json!({
            "type": {"@type": "reactionTypeEmoji", "emoji": emoji}, "needs_premium": false,
        })).collect::<Vec<_>>(),
    }).to_string());
}

#[test]
fn a_first_empty_reaction_answer_recovers_when_metadata_arrives_without_reopening() {
    let mut s = session();
    let m = model::history(s.store(), VERA).iter().find(|m| !m.service).unwrap().clone();
    let card = open_root(&mut s, Line::id(VERA, m.id));
    let td = FakeTd::new();
    let acc = connected_reaction_account(&s, td.clone());
    acc.drain(s.world());
    verb(&mut s, card, "telegram.react");
    acc.drain(s.world());
    let first = last_reaction_request(&td, "getMessageAvailableReactions");
    offer_reactions(&acc, &s, &first, &[]);
    let labels: Vec<_> = s.panel(card).unwrap().borrow().verbs().into_iter().map(|v| v.label).collect();
    assert_eq!(labels, vec!["loading reactions…", "cancel"], "an early empty cache is not a definitive answer");
    acc.on_update(s.world(), &serde_json::json!({"@type": "updateChatAvailableReactions", "chat_id": VERA + 1}).to_string());
    acc.drain(s.world());
    assert_eq!(last_reaction_request(&td, "getMessageAvailableReactions"), first, "ignore other chats");
    for kind in ["updateChatAvailableReactions", "updateActiveEmojiReactions"] {
        acc.on_update(s.world(), &serde_json::json!({"@type": kind, "chat_id": VERA}).to_string());
    }
    acc.drain(s.world());
    let second = last_reaction_request(&td, "getMessageAvailableReactions");
    assert_ne!(second["@extra"], first["@extra"]);
    offer_reactions(&acc, &s, &second, &["👍", "🔥"]);
    assert!(poll_reactions(&mut s, card));
    assert!(verb_ids(&s, card).contains(&"telegram.reaction_1"));
    offer_reactions(&acc, &s, &first, &[]);
    assert!(verb_ids(&s, card).contains(&"telegram.reaction_1"), "late answers cannot replace new choices");
    acc.on_update(s.world(), &serde_json::json!({"@type": "updateMessageInteractionInfo",
        "chat_id": VERA, "message_id": m.id, "interaction_info": null}).to_string());
    acc.drain(s.world());
    let third = last_reaction_request(&td, "getMessageAvailableReactions");
    offer_reactions(&acc, &s, &third, &["❤️"]);
    assert!(poll_reactions(&mut s, card), "a picker redraws more than its first response");
    let labels: Vec<_> = s.panel(card).unwrap().borrow().verbs().into_iter().map(|v| v.label).collect();
    assert_eq!(labels[0], "❤️");
}

#[test]
fn empty_reaction_choices_keep_retrying_on_the_worker_clock() {
    use kernel::app::Env;
    use kernel::caps::{ClockSource, FakeClock};
    let clock = FakeClock::at(virtual_epoch());
    let mut s = Session::fake_with(APPS, &Env { clock: ClockSource::Virtual(clock.clone()), ..Env::default() });
    let m = model::history(s.store(), VERA).iter().find(|m| !m.service).unwrap().clone();
    let card = open_root(&mut s, Line::id(VERA, m.id));
    let td = FakeTd::new();
    let acc = connected_reaction_account(&s, td.clone());
    acc.drain(s.world());
    verb(&mut s, card, "telegram.react");
    acc.drain(s.world());
    let first = last_reaction_request(&td, "getMessageAvailableReactions");
    offer_reactions(&acc, &s, &first, &[]);
    clock.advance(1.0);
    acc.drain(s.world());
    let retry = last_reaction_request(&td, "getMessageAvailableReactions");
    assert_ne!(first["@extra"], retry["@extra"], "retry an empty cache even without metadata events");
    offer_reactions(&acc, &s, &retry, &["👍"]);
    assert!(verb_ids(&s, card).contains(&"telegram.reaction_0"));
    verb(&mut s, card, "telegram.reactions_cancel");
    verb(&mut s, card, "telegram.react");
    for _ in 0..5 {
        acc.drain(s.world());
        offer_reactions(&acc, &s, &last_reaction_request(&td, "getMessageAvailableReactions"), &[]);
        clock.advance(4.0);
    }
    let labels: Vec<_> = s.panel(card).unwrap().borrow().verbs().into_iter().map(|v| v.label).collect();
    assert_eq!(labels, vec!["waiting for reactions…", "retry", "cancel"]);
    let count = td.sent().len();
    clock.advance(60.0);
    acc.drain(s.world());
    assert_eq!(td.sent().len(), count + 1, "an empty cache keeps retrying with a capped delay");
    offer_reactions(&acc, &s, &last_reaction_request(&td, "getMessageAvailableReactions"), &["🔥"]);
    assert!(verb_ids(&s, card).contains(&"telegram.reaction_0"), "late metadata recovers without a new update or reopening");
}

#[test]
fn reaction_request_timeouts_release_the_picker_and_never_resend_an_unconfirmed_add() {
    use kernel::app::Env;
    use kernel::caps::{ClockSource, FakeClock};
    let clock = FakeClock::at(virtual_epoch());
    let mut s = Session::fake_with(APPS, &Env { clock: ClockSource::Virtual(clock.clone()), ..Env::default() });
    let m = model::history(s.store(), VERA).iter().find(|m| !m.service).unwrap().clone();
    let card = open_root(&mut s, Line::id(VERA, m.id));
    let td = FakeTd::new();
    let acc = connected_reaction_account(&s, td.clone());
    acc.drain(s.world());
    verb(&mut s, card, "telegram.react");
    acc.drain(s.world());
    let timed_out = last_reaction_request(&td, "getMessageAvailableReactions");
    clock.advance(31.0);
    acc.drain(s.world());
    assert!(poll_reactions(&mut s, card));
    assert!(s.notes().last().unwrap().msg.contains("could not load reactions"));
    assert!(!poll_reactions(&mut s, card));
    verb(&mut s, card, "telegram.reactions_retry");
    acc.drain(s.world());
    offer_reactions(&acc, &s, &timed_out, &["🔥"]);
    assert!(!verb_ids(&s, card).contains(&"telegram.reaction_0"));
    offer_reactions(&acc, &s, &last_reaction_request(&td, "getMessageAvailableReactions"), &["👍"]);
    verb(&mut s, card, "telegram.reaction_0");
    acc.drain(s.world());
    answer_reaction_snapshot(&acc, &s, &td);
    let add = last_reaction_request(&td, "addMessageReaction");
    clock.advance(31.0);
    acc.drain(s.world());
    assert!(poll_reactions(&mut s, card));
    assert!(s.notes().last().unwrap().msg.contains("could not add reaction"));
    assert!(!runtime::of(s.store()).operations.pending_context(add["@extra"]["context"].as_str().unwrap()),
        "shared progress must stop waiting when the picker times out");
    assert_eq!(td.sent_types().iter().filter(|t| *t == "addMessageReaction").count(), 1);
    acc.on_update(s.world(), &serde_json::json!({"@type": "ok", "@extra": add["@extra"]}).to_string());
    assert_eq!(last_reaction_request(&td, "getMessage")["message_id"], m.id, "late success still refreshes the actual message");
    assert!(verb_ids(&s, card).contains(&"telegram.reactions_retry"), "a retired add cannot finish a newer picker state");
}

#[test]
fn reaction_permission_changes_during_loading_are_coalesced_and_explained() {
    let mut s = session();
    let m = model::history(s.store(), VERA).iter().find(|m| !m.service).unwrap().clone();
    let card = open_root(&mut s, Line::id(VERA, m.id));
    let td = FakeTd::new();
    let acc = connected_reaction_account(&s, td.clone());
    acc.drain(s.world());
    verb(&mut s, card, "telegram.react");
    acc.drain(s.world());
    let first = last_reaction_request(&td, "getMessageAvailableReactions");
    for _ in 0..4 {
        acc.on_update(s.world(), &serde_json::json!({"@type": "updateChatAvailableReactions", "chat_id": VERA}).to_string());
        acc.drain(s.world());
    }
    assert_eq!(last_reaction_request(&td, "getMessageAvailableReactions"), first);
    offer_reactions(&acc, &s, &first, &["👍"]);
    acc.drain(s.world());
    assert_eq!(td.sent_types().iter().filter(|t| *t == "getMessageAvailableReactions").count(), 2);
    let retry = last_reaction_request(&td, "getMessageAvailableReactions");
    acc.on_update(s.world(), &serde_json::json!({"@type": "availableReactions", "@extra": retry["@extra"],
        "unavailability_reason": {"@type": "reactionUnavailabilityReasonGuest"}}).to_string());
    let labels: Vec<_> = s.panel(card).unwrap().borrow().verbs().into_iter().map(|v| v.label).collect();
    assert_eq!(labels, vec!["join this chat to react", "cancel"]);
    verb(&mut s, card, "telegram.reactions_cancel");
    let n = td.sent().len();
    acc.on_update(s.world(), &serde_json::json!({"@type": "updateActiveEmojiReactions"}).to_string());
    acc.drain(s.world());
    assert_eq!(td.sent().len(), n, "closing the panel retires its metadata subscription");
}

#[test]
fn an_empty_picker_cache_keeps_retrying_and_cannot_erase_loaded_choices() {
    use kernel::app::Env;
    use kernel::caps::{ClockSource, FakeClock};
    let clock = FakeClock::at(virtual_epoch());
    let mut s = Session::fake_with(APPS, &Env { clock: ClockSource::Virtual(clock.clone()), ..Env::default() });
    let m = model::history(s.store(), VERA).iter().find(|m| !m.service).unwrap().clone();
    let card = open_root(&mut s, Line::id(VERA, m.id));
    let td = FakeTd::new();
    let acc = connected_reaction_account(&s, td.clone());
    acc.drain(s.world());
    verb(&mut s, card, "telegram.react");
    acc.drain(s.world());
    offer_reactions(&acc, &s, &last_reaction_request(&td, "getMessageAvailableReactions"), &[]);
    clock.advance(11.0);
    acc.drain(s.world());
    offer_reactions(&acc, &s, &last_reaction_request(&td, "getMessageAvailableReactions"), &[]);
    let labels: Vec<_> = s.panel(card).unwrap().borrow().verbs().into_iter().map(|v| v.label).collect();
    assert_eq!(labels, ["waiting for reactions…", "retry", "cancel"]);
    clock.advance(3.0);
    acc.drain(s.world());
    offer_reactions(&acc, &s, &last_reaction_request(&td, "getMessageAvailableReactions"), &["👍"]);
    assert!(verb_ids(&s, card).contains(&"telegram.reaction_0"), "recover without reopening the picker");
    acc.on_update(s.world(), &serde_json::json!({"@type": "updateActiveEmojiReactions"}).to_string());
    acc.drain(s.world());
    offer_reactions(&acc, &s, &last_reaction_request(&td, "getMessageAvailableReactions"), &[]);
    assert!(verb_ids(&s, card).contains(&"telegram.reaction_0"), "metadata invalidation must not flash away known choices");
}

#[test]
fn a_reaction_uses_the_messages_available_emoji_and_the_server_updates_its_count() {
    let mut s = session();
    let chat = open_root(&mut s, Chat::id(RUST_WEEKLY));
    let m = model::history(s.store(), RUST_WEEKLY).iter().find(|m| !m.service).unwrap().clone();
    with_chat(&s, chat, |c| c.set_cursor((c.peer(), m.id)));
    let td = FakeTd::new();
    let acc = connected_reaction_account(&s, td.clone());
    acc.drain(s.world());

    // Channels may accept reactions even when the composer is read-only.
    verb(&mut s, chat, "telegram.react");
    assert_eq!(verb_ids(&s, chat), vec!["telegram.reaction_status", "telegram.reactions_cancel"]);
    acc.drain(s.world());
    let request = last_reaction_request(&td, "getMessageAvailableReactions");
    assert_eq!(request["chat_id"], RUST_WEEKLY);
    assert_eq!(request["message_id"], m.id);
    assert_eq!(request["row_size"], 6);
    assert!(!poll_reactions(&mut s, chat));
    let emoji = |text: &str| serde_json::json!({"type": {"@type": "reactionTypeEmoji", "emoji": text}});
    acc.on_update(s.world(), &serde_json::json!({
        "@type": "availableReactions", "@extra": request["@extra"],
        "top_reactions": [emoji("👍"), emoji("❤️")],
        "recent_reactions": [emoji("👍"), emoji("🔥"),
            {"type": {"@type": "reactionTypeCustomEmoji", "custom_emoji_id": "123"}},
            {"type": {"@type": "reactionTypePaid"}}],
        "popular_reactions": [emoji("👏"),
            {"type": {"@type": "reactionTypeEmoji", "emoji": "⭐"}, "needs_premium": true}],
    }).to_string());
    let labels: Vec<_> = s.panel(chat).unwrap().borrow().verbs().into_iter().map(|v| v.label).collect();
    assert_eq!(labels, vec!["👍", "❤️", "🔥", "👏", "cancel"]);
    assert!(poll_reactions(&mut s, chat));
    assert!(!poll_reactions(&mut s, chat));

    verb(&mut s, chat, "telegram.reaction_1");
    assert!(!verb_ids(&s, chat).contains(&"telegram.reaction_1"), "no duplicate send while waiting");
    acc.drain(s.world());
    answer_reaction_snapshot(&acc, &s, &td);
    let request = last_reaction_request(&td, "addMessageReaction");
    assert_eq!(request["chat_id"], RUST_WEEKLY);
    assert_eq!(request["message_id"], m.id);
    assert_eq!(request["reaction_type"], serde_json::json!({"@type": "reactionTypeEmoji", "emoji": "❤️"}));
    assert_eq!(request["is_big"], false);
    assert_eq!(request["update_recent_reactions"], true);
    assert_eq!(model::line(s.store(), m.chat, m.id).unwrap().reactions, m.reactions);

    acc.on_update(s.world(), &serde_json::json!({"@type": "ok", "@extra": request["@extra"]}).to_string());
    let refresh = last_reaction_request(&td, "getMessage");
    assert_eq!(refresh["chat_id"], m.chat);
    assert_eq!(refresh["message_id"], m.id);
    assert!(poll_reactions(&mut s, chat));
    assert!(verb_ids(&s, chat).contains(&"telegram.react"));
    // A restored line may have no push update. The confirmed snapshot must
    // update the footer on its own, without reopening the chat.
    acc.on_update(s.world(), &serde_json::json!({
        "@type": "message", "@extra": refresh["@extra"], "chat_id": m.chat, "id": m.id,
        "date": m.date, "content": {"@type": "messageText", "text": {"text": m.text}},
        "interaction_info": {"reactions": {"reactions": [
            {"type": {"@type": "reactionTypeEmoji", "emoji": "❤️"}, "total_count": 2},
        ]}},
    }).to_string());
    assert_eq!(model::line(s.store(), m.chat, m.id).unwrap().reactions.as_deref(), Some("❤️ 2"));
    acc.on_update(s.world(), &serde_json::json!({
        "@type": "updateMessageInteractionInfo", "chat_id": m.chat, "message_id": m.id,
        "interaction_info": {"reactions": {"@type": "messageReactions", "reactions": [
            {"type": {"@type": "reactionTypeEmoji", "emoji": "❤️"}, "total_count": 1, "is_chosen": true},
        ]}},
    }).to_string());
    assert_eq!(model::line(s.store(), m.chat, m.id).unwrap().reactions.as_deref(), Some("❤️ 1"));
}

#[test]
fn reaction_paging_reaches_every_choice_in_chat_and_card() {
    let mut s = session();
    let m = model::history(s.store(), VERA).iter().find(|m| !m.service).unwrap().clone();
    let chat = open_root(&mut s, Chat::id(VERA));
    with_chat(&s, chat, |c| c.set_cursor((c.peer(), m.id)));
    let card = open_root(&mut s, Line::id(m.chat, m.id));
    let td = FakeTd::new();
    let acc = connected_reaction_account(&s, td.clone());
    acc.drain(s.world());
    let choices = ["👍", "❤️", "🔥", "😂", "😮", "🙏", "🎉", "👏", "🤔", "🤯", "😢", "💯", "🦄", "🌚"];
    for slot in [chat, card] {
        verb(&mut s, slot, "telegram.react");
        acc.drain(s.world());
        offer_reactions(&acc, &s, &last_reaction_request(&td, "getMessageAvailableReactions"), &choices);
        for page in 0..3 {
            let shown: Vec<_> = s.panel(slot).unwrap().borrow().verbs().into_iter()
                .filter(|v| v.style == kernel::panel::VerbStyle::Glyph).map(|v| v.label).collect();
            assert_eq!(shown, choices[page * 6..choices.len().min((page + 1) * 6)]);
            if page < 2 {
                verb(&mut s, slot, "telegram.reactions_more");
            }
        }
        assert!(!verb_ids(&s, slot).contains(&"telegram.reactions_more"));
        verb(&mut s, slot, "telegram.reactions_back");
        verb(&mut s, slot, "telegram.reactions_more");
        verb(&mut s, slot, "telegram.reaction_1");
        acc.drain(s.world());
        answer_reaction_snapshot(&acc, &s, &td);
        let add = last_reaction_request(&td, "addMessageReaction");
        assert_eq!(add["reaction_type"]["emoji"], "🌚");
        verb(&mut s, slot, "telegram.reactions_cancel");
        acc.on_update(s.world(), &serde_json::json!({"@type": "ok", "@extra": add["@extra"]}).to_string());
        let refresh = last_reaction_request(&td, "getMessage");
        assert_eq!((refresh["chat_id"].as_i64(), refresh["message_id"].as_i64()), (Some(m.chat), Some(m.id)));
    }
}

#[test]
fn reaction_failures_are_reported_once_and_can_be_inspected_and_retried() {
    let mut s = session();
    let m = model::history(s.store(), VERA).iter().find(|m| !m.service).unwrap().clone();
    let card = open_root(&mut s, Line::id(VERA, m.id));
    let td = FakeTd::new();
    let acc = connected_reaction_account(&s, td.clone());
    acc.drain(s.world());
    verb(&mut s, card, "telegram.react");
    acc.drain(s.world());
    let query = last_reaction_request(&td, "getMessageAvailableReactions");
    acc.on_update(s.world(), &serde_json::json!({
        "@type": "error", "@extra": query["@extra"], "code": 400, "message": "MESSAGE_NOT_FOUND",
    }).to_string());
    let notes = s.notes().len();
    assert!(poll_reactions(&mut s, card));
    assert_eq!(s.notes().last().unwrap().msg, "could not load reactions: MESSAGE_NOT_FOUND");
    assert!(!poll_reactions(&mut s, card));
    assert_eq!(s.notes().len(), notes + 1, "the failure is reported only once");
    assert!(verb_ids(&s, card).contains(&"telegram.reactions_retry"));
    verb(&mut s, card, "telegram.reactions_retry");
    acc.drain(s.world());
    let query = last_reaction_request(&td, "getMessageAvailableReactions");
    offer_reactions(&acc, &s, &query, &["👍"]);
    verb(&mut s, card, "telegram.reaction_0");
    acc.drain(s.world());
    answer_reaction_snapshot(&acc, &s, &td);
    let request = last_reaction_request(&td, "addMessageReaction");
    acc.on_update(s.world(), &serde_json::json!({
        "@type": "error", "@extra": request["@extra"], "code": 400, "message": "REACTION_INVALID",
    }).to_string());
    assert_eq!(model::line(s.store(), m.chat, m.id).unwrap(), m);
    assert!(poll_reactions(&mut s, card));
    assert_eq!(s.notes().last().unwrap().msg, "could not add reaction: REACTION_INVALID");
    verb(&mut s, card, "telegram.reaction_status");
    assert!(s.notes().last().unwrap().msg.contains("REACTION_INVALID"));
    verb(&mut s, card, "telegram.reactions_retry");
    acc.drain(s.world());
    let query = last_reaction_request(&td, "getMessageAvailableReactions");
    offer_reactions(&acc, &s, &query, &[]);
    let labels: Vec<_> = s.panel(card).unwrap().borrow().verbs().into_iter().map(|v| v.label).collect();
    assert_eq!(labels, vec!["loading reactions…", "cancel"]);
    verb(&mut s, card, "telegram.reactions_cancel");
    assert!(verb_ids(&s, card).contains(&"telegram.react"));

    verb(&mut s, card, "telegram.react");
    acc.drain(s.world());
    let query = last_reaction_request(&td, "getMessageAvailableReactions");
    offer_reactions(&acc, &s, &query, &["👍"]);
    drop(acc);
    assert!(!verb_ids(&s, card).contains(&"telegram.reaction_0"), "disconnect retires the choice subscription");
    verb(&mut s, card, "telegram.reaction_status");
    assert!(s.notes().last().unwrap().msg.contains("disconnected"));
    verb(&mut s, card, "telegram.reactions_retry");
    assert!(verb_ids(&s, card).contains(&"telegram.reactions_retry"), "a disconnected live picker stays live");
    assert_eq!(model::line(s.store(), m.chat, m.id).unwrap(), m);
}

#[test]
fn a_reaction_reports_its_clients_startup_failure_until_authorization_recovers() {
    let mut s = session();
    let m = model::history(s.store(), VERA).iter().find(|m| !m.service).unwrap().clone();
    let chat = open_root(&mut s, Chat::at(VERA, m.id));
    let signin = open_root(&mut s, SignIn::id());
    let td = FakeTd::new();
    let acc = sync::Account::new(td.clone(), 17844, std::env::temp_dir(), None);
    td.push(serde_json::json!({
        "@type": "updateAuthorizationState",
        "authorization_state": {"@type": "authorizationStateWaitTdlibParameters"},
    }).to_string());
    acc.drain(s.world());
    let parameters = last_reaction_request(&td, "setTdlibParameters");
    acc.on_update(s.world(), &serde_json::json!({
        "@type": "error", "@extra": parameters["@extra"], "code": 400,
        "message": "Can't lock file \"/tmp/tdlib/td.binlog\", because it is already in use; check for another program instance running",
    }).to_string());
    let reason = "Telegram is open in another app window\nclose it and restart this app";
    with_signin(&s, signin, |p| {
        assert_eq!(p.state(), "error");
        assert_eq!(p.line(), reason);
        assert_eq!(p.note().as_deref(), Some("Telegram is open in another app instance · close it to continue"));
        assert_eq!(p.field_kind(), None);
    });

    // A second process can write the shared session row. The error must
    // still describe the client that actually handles this reaction.
    set_session(&s, "ready", None, None);
    verb(&mut s, chat, "telegram.react");
    acc.drain(s.world());
    assert!(!td.sent_types().iter().any(|t| t == "getMessageAvailableReactions"),
        "a known connection failure is reported without sending a request");
    assert!(poll_reactions(&mut s, chat));
    assert_eq!(s.notes().last().unwrap().msg, format!("could not load reactions: {reason}"));
    let labels: Vec<_> = s.panel(chat).unwrap().borrow().verbs().into_iter().map(|v| v.label).collect();
    assert_eq!(labels, vec!["could not load reactions", "retry", "cancel"]);

    // A recovered connection must not keep reporting the previous failure.
    acc.on_update(s.world(), &serde_json::json!({
        "@type": "updateAuthorizationState",
        "authorization_state": {"@type": "authorizationStateReady"},
    }).to_string());
    acc.on_update(s.world(), &serde_json::json!({"@type": "updateNewChat", "chat": {
        "@type": "chat", "id": VERA, "title": "Vera",
        "type": {"@type": "chatTypePrivate", "user_id": VERA},
    }}).to_string());
    verb(&mut s, chat, "telegram.reactions_retry");
    acc.drain(s.world());
    let query = last_reaction_request(&td, "getMessageAvailableReactions");
    acc.on_update(s.world(), &serde_json::json!({
        "@type": "error", "@extra": query["@extra"], "code": 400, "message": "MESSAGE_NOT_FOUND",
    }).to_string());
    assert!(poll_reactions(&mut s, chat));
    assert_eq!(s.notes().last().unwrap().msg, "could not load reactions: MESSAGE_NOT_FOUND");
    verb(&mut s, chat, "telegram.reactions_retry");
    acc.drain(s.world());
    let query = last_reaction_request(&td, "getMessageAvailableReactions");
    offer_reactions(&acc, &s, &query, &["👍"]);
    assert!(verb_ids(&s, chat).contains(&"telegram.reaction_0"));
    assert_eq!(model::line(s.store(), m.chat, m.id).unwrap(), m);
}

#[test]
fn reaction_pickers_ignore_late_answers_and_keep_panels_and_stores_separate() {
    let mut s = session();
    let m = model::history(s.store(), VERA).iter().find(|m| !m.service).unwrap().clone();
    let chat = open_root(&mut s, Chat::at(VERA, m.id));
    let card = open_root(&mut s, Line::id(VERA, m.id));
    let td = FakeTd::new();
    let acc = connected_reaction_account(&s, td.clone());
    acc.drain(s.world());
    verb(&mut s, chat, "telegram.react");
    acc.drain(s.world());
    let first = last_reaction_request(&td, "getMessageAvailableReactions");
    verb(&mut s, card, "telegram.react");
    acc.drain(s.world());
    let second = last_reaction_request(&td, "getMessageAvailableReactions");
    assert_ne!(first["@extra"], second["@extra"]);
    verb(&mut s, chat, "telegram.reactions_cancel");
    verb(&mut s, chat, "telegram.react");
    acc.drain(s.world());
    let third = last_reaction_request(&td, "getMessageAvailableReactions");
    offer_reactions(&acc, &s, &first, &["🔥"]);
    assert!(!verb_ids(&s, chat).contains(&"telegram.reaction_0"));
    offer_reactions(&acc, &s, &second, &["❤️"]);
    assert!(verb_ids(&s, card).contains(&"telegram.reaction_0"));
    offer_reactions(&acc, &session(), &third, &["👍"]);
    assert!(!verb_ids(&s, chat).contains(&"telegram.reaction_0"));
    offer_reactions(&acc, &s, &third, &["👍"]);
    with_chat(&s, chat, |c| { c.walk(1).map(|(_, id)| id); });
    assert!(!verb_ids(&s, chat).contains(&"telegram.reaction_0"));
    assert!(verb_ids(&s, card).contains(&"telegram.reaction_0"));
}

#[test]
fn a_live_reaction_picker_never_falls_back_to_demo_when_the_worker_is_missing() {
    let mut s = session();
    s.world().caps(|caps| caps.insert(Box::new(runtime::Delivery::Live)));
    set_session(&s, "ready", None, None);
    let m = model::history(s.store(), VERA).iter().find(|m| !m.service).unwrap().clone();
    let chat = open_root(&mut s, Chat::at(VERA, m.id));
    let card = open_root(&mut s, Line::id(VERA, m.id));
    for slot in [chat, card] {
        verb(&mut s, slot, "telegram.react");
        assert!(!verb_ids(&s, slot).contains(&"telegram.reaction_0"));
        assert!(poll_reactions(&mut s, slot));
        assert!(s.notes().last().unwrap().msg.contains("not connected"));
    }

    // A worker that starts later makes retry useful, without reopening the panel.
    let td = FakeTd::new();
    let acc = connected_reaction_account(&s, td.clone());
    acc.drain(s.world());
    verb(&mut s, chat, "telegram.reactions_retry");
    acc.drain(s.world());
    let query = last_reaction_request(&td, "getMessageAvailableReactions");
    offer_reactions(&acc, &s, &query, &["👍"]);
    assert!(verb_ids(&s, chat).contains(&"telegram.reaction_0"));
    verb(&mut s, card, "telegram.reactions_retry");
    verb(&mut s, chat, "telegram.reaction_0");
    acc.drain(s.world());
    drop(acc);
    for slot in [chat, card] {
        assert!(poll_reactions(&mut s, slot), "both loading and adding stop on disconnect");
        assert!(s.notes().last().unwrap().msg.contains("disconnected"));
        verb(&mut s, slot, "telegram.reactions_cancel");
    }

    // Closing the picker must not forget that this is a live account.
    verb(&mut s, chat, "telegram.react");
    assert!(!verb_ids(&s, chat).contains(&"telegram.reaction_0"));
    assert_eq!(model::line(s.store(), m.chat, m.id).unwrap(), m);
}

#[test]
fn demo_reactions_page_and_cancel_without_touching_the_composer_or_service_messages() {
    let mut s = session();
    let chat = open_root(&mut s, Chat::id(STELAXIS));
    let hist = model::history(s.store(), STELAXIS);
    let m = hist.iter().find(|m| !m.service).unwrap();
    let id = m.id;
    s.store().write(move |c| {
        c.execute("UPDATE tg_message SET reactions = '👍 3 · ❤️ 1' WHERE chat = ?1 AND id = ?2",
            [STELAXIS, id]).map(|_| ())
    }).unwrap();
    with_chat(&s, chat, |c| { c.set_cursor((c.peer(), m.id)); c.set_draft("a draft"); c.reply((c.peer(), m.id)); });
    verb(&mut s, chat, "telegram.react");
    verb(&mut s, chat, "telegram.reactions_more");
    assert!(!verb_ids(&s, chat).contains(&"telegram.reactions_more"));
    verb(&mut s, chat, "telegram.reactions_back");
    verb(&mut s, chat, "telegram.reaction_0");
    let mut expected = m.clone();
    expected.reactions = Some("👍 4 · ❤️ 1".into());
    assert_eq!(model::line(s.store(), m.chat, m.id).as_ref(), Some(&expected));
    assert!(!s.notes().iter().any(|n| n.msg.contains("draft: nothing leaves")));
    let card = open_root(&mut s, Line::id(STELAXIS, m.id));
    verb(&mut s, card, "telegram.react");
    verb(&mut s, card, "telegram.reaction_0");
    assert_eq!(model::line(s.store(), m.chat, m.id).as_ref(), Some(&expected), "one reaction per emoji, across panels");
    verb(&mut s, card, "telegram.react");
    verb(&mut s, card, "telegram.reaction_2");
    expected.reactions = Some("👍 4 · ❤️ 1 · 🔥 1".into());
    assert_eq!(model::line(s.store(), m.chat, m.id).as_ref(), Some(&expected));
    with_chat(&s, chat, |c| {
        assert_eq!(c.field_text(), "a draft");
        assert_eq!(c.reply_to(), Some(m.id));
    });
    verb(&mut s, chat, "telegram.react");
    assert!(with_chat(&s, chat, Chat::cancel_reactions));
    assert!(verb_ids(&s, chat).contains(&"telegram.react"));
    with_chat(&s, chat, Chat::toggle_mark);
    assert!(!verb_ids(&s, chat).contains(&"telegram.react"));
    with_chat(&s, chat, Chat::clear_marks);
    let service = hist.iter().find(|m| m.service).unwrap();
    with_chat(&s, chat, |c| c.set_cursor((c.peer(), service.id)));
    assert!(!verb_ids(&s, chat).contains(&"telegram.react"));
    let card = open_root(&mut s, Line::id(STELAXIS, service.id));
    assert!(!verb_ids(&s, card).contains(&"telegram.react"));
    let missing = open_root(&mut s, Line::id(STELAXIS, i64::MAX));
    assert!(!verb_ids(&s, missing).contains(&"telegram.react"));

    // Unsent lines cannot receive a reaction, and a stale picker cannot
    // send after its message disappears.
    with_chat(&s, chat, |c| c.set_cursor((c.peer(), m.id)));
    let card = open_root(&mut s, Line::id(STELAXIS, m.id));
    for state in ["sending", "failed"] {
        let id = m.id;
        s.store().write(move |c| {
            c.execute("UPDATE tg_message SET state = ?3 WHERE chat = ?1 AND id = ?2",
                rusqlite::params![STELAXIS, id, state]).map(|_| ())
        }).unwrap();
        assert!(!verb_ids(&s, chat).contains(&"telegram.react"));
        assert!(!verb_ids(&s, card).contains(&"telegram.react"));
    }
    let id = m.id;
    s.store().write(move |c| {
        c.execute("UPDATE tg_message SET state = NULL WHERE chat = ?1 AND id = ?2",
            rusqlite::params![STELAXIS, id]).map(|_| ())
    }).unwrap();
    verb(&mut s, card, "telegram.react");
    s.store().write(move |c| model::delete_lines_tx(c, STELAXIS, &[id])).unwrap();
    verb(&mut s, card, "telegram.reaction_0");
    assert!(s.notes().last().unwrap().err);
    assert!(!verb_ids(&s, card).contains(&"telegram.reaction_0"));
}

/// One chat's draft, as the row holds it.
fn draft_row(s: &Session, peer: i64) -> String {
    s.store()
        .conn()
        .query_row(
            "SELECT COALESCE(draft, '') FROM tg_chat WHERE peer = ?1",
            [peer],
            |r| r.get(0),
        )
        .expect("a chat row")
}

/// A draft written under an open panel — what `updateChatDraftMessage` does
/// when the line was half-typed on the phone.
fn write_draft(s: &Session, peer: i64, text: &str) {
    let text = text.to_string();
    s.store()
        .write(move |c| model::set_draft_tx(c, peer, &text))
        .expect("the draft written");
}

/// What the widget does at the top of every draw: read the card, which is
/// where the draft is reconciled, then the field.
fn field_now(s: &Session, slot: SlotId) -> String {
    with_chat(s, slot, |c| {
        let _ = c.card();
        c.field_text().to_string()
    })
}

/// A session of fixtures — every test's, and the Panels Library's — is not
/// the account holder's, whatever this build links: no worker of ours runs
/// in it, so no second TDLib client opens to drain the engine's process-wide
/// queue into a store of demo rows; and no verb of its panels reaches the
/// wire, so a scene pressing *mute* mutes nothing but its own fixture.
#[test]
fn nothing_of_a_fixture_session_reaches_the_engine() {
    let mut s = session();
    assert!(s.db_dir().is_none(), "a fixture session is in memory");
    assert!(!Telegram::engine_store(s.db_dir()));
    assert!(
        TELEGRAM.workers(s.store()).is_empty(),
        "no engine worker for a store that is not the account holder's"
    );
    assert!(s.workers().names().is_empty(), "and none running");

    // The toast is the whole of it: nothing went, and the local flip a
    // caller would make on `true` is not made.
    assert!(!told(&mut s, &requests::set_chat_muted(VERA, true), "mute"));
    let said = s.notes().last().map(|n| n.msg.clone()).unwrap_or_default();
    assert_eq!(said, "draft: nothing leaves — mute");
}

/// A refused live send keeps its input in the operation tracker. Correlation
/// contains no text, and the failure never overwrites a newer composer draft.
#[test]
fn a_refused_send_keeps_its_input_without_overwriting_the_composer() {
    let mut s = session();
    let chat = open_root(&mut s, Chat::id(VERA));
    let rt = runtime::of(s.store());
    let inbox = rt.connect();

    // Builders leave correlation to the tracker at the queue boundary.
    let v = |s: String| serde_json::from_str::<serde_json::Value>(&s).unwrap();
    assert!(v(requests::send_message(VERA, "17:00, or 18:00?", None))["@extra"].is_null());
    let file = model::Carried {
        path: "~/Pictures/trail.png".to_string(),
    };
    assert!(v(requests::send_file(VERA, None, &file, "under it"))["@extra"].is_null());
    assert!(v(requests::send_location(VERA, None, 48.1, 11.5))["@extra"].is_null());

    with_chat(&s, chat, |c| c.set_draft("17:00, or 18:00?"));
    send(&mut s, chat);
    assert_eq!(draft_row(&s, VERA), "");
    assert_eq!(with_chat(&s, chat, |c| c.draft().to_string()), "");
    let sent = v(inbox.try_recv().unwrap());
    assert!(sent["@extra"]["operation"].is_u64());
    assert!(sent["@extra"]["context"].is_null());
    with_chat(&s, chat, |c| c.set_draft("never mind"));

    let refusal = serde_json::json!({
        "@type": "error",
        "code": 400,
        "message": "MESSAGE_TOO_LONG",
        "@extra": sent["@extra"],
    });
    account().on_update(s.world(), &refusal.to_string());
    assert_eq!(draft_row(&s, VERA), "never mind");
    assert_eq!(field_now(&s, chat), "never mind");
    let op = rt.operations.list().remove(0);
    assert!(op.line().contains("MESSAGE_TOO_LONG"));
    assert!(op.retryable());
    let retry = v(rt.operations.retry(op.id).unwrap());
    assert_eq!(retry["input_message_content"], sent["input_message_content"]);
}

/// `my_id` is the one thing that says who the account holder is. Until it
/// arrives, *saved messages* is the demo world's stand-in self — the only
/// self a store with no account has — and after it, the real one's chat,
/// with the stand-in's flag given up. The value crosses as a number or as a
/// string, by build; both are read.
#[test]
fn my_id_names_the_account_holder_and_saved_messages_follows() {
    let mut s = session();
    assert_eq!(model::self_peer(s.store()), None, "nobody has signed in");
    let saved = open_root(&mut s, Chat::id(SELF));
    assert_eq!(with_chat(&s, saved, |c| c.peer()), SELF);

    let option = |value: serde_json::Value| {
        serde_json::json!({
            "@type": "updateOption",
            "name": "my_id",
            "value": { "@type": "optionValueInteger", "value": value },
        })
        .to_string()
    };
    account().on_update(s.world(), &option(serde_json::json!(7_771)));
    assert_eq!(model::self_peer(s.store()), Some(7_771));
    assert!(
        !model::peer(s.store(), SELF).expect("the demo self").is_self,
        "one account holder, one row that says so"
    );
    // The string shape, and a second account over the same store.
    account().on_update(s.world(), &option(serde_json::json!("7772")));
    assert_eq!(model::self_peer(s.store()), Some(7_772));

    let saved = open_root(&mut s, Chat::id(SELF));
    assert_eq!(
        with_chat(&s, saved, |c| c.peer()),
        7_772,
        "the notes-to-self open on the account holder's own chat"
    );
}

/// A draft written under an open panel — the same line half-typed on the
/// phone, arriving as `updateChatDraftMessage` — reaches the composer, which
/// until now read the row once and never again. What has been typed here
/// since is not the other device's to overwrite.
#[test]
fn a_draft_from_another_device_reaches_an_untouched_composer() {
    let mut s = session();
    let chat = open_root(&mut s, Chat::id(VERA));
    assert_eq!(field_now(&s, chat), "");

    write_draft(&s, VERA, "on the train, back at six");
    assert_eq!(field_now(&s, chat), "on the train, back at six");

    // Typed here: the local words stand, whatever the row says next — and
    // whatever draws happen between, since typing writes the row too and
    // a draw reads it back (review, 2026-09-07: a redraw between made the
    // words look untouched and a phone's draft overwrote them).
    with_chat(&s, chat, |c| c.typed("bringing the maps"));
    with_chat(&s, chat, |c| {
        let _ = c.card();
    });
    write_draft(&s, VERA, "and the thermos");
    assert_eq!(field_now(&s, chat), "bringing the maps");

    // And the first read is what it always was: a chat opened on a draft
    // shows it, reconciled or not.
    let hike = open_root(&mut s, Chat::id(HIKE));
    assert_eq!(field_now(&s, hike), "I'll bring the thermos and");
}

#[test]
fn forwarding_and_signin_stay_with_the_session_that_started_them() {
    let mut a = session();
    let mut b = session();
    let a_list = open_root(&mut a, Chats::id());
    let b_list = open_root(&mut b, Chats::id());
    runtime::of(a.store()).carry_forward(VERA, vec![42]);
    assert!(verb_ids(&a, a_list).contains(&"telegram.forward_here"));
    assert!(!verb_ids(&b, b_list).contains(&"telegram.forward_here"));
    assert!(runtime::of(b.store()).take_forward().is_none());
    assert!(runtime::of(a.store()).pending_forward().is_some());

    let inbox = runtime::of(a.store()).connect();
    let a_signin = open_root(&mut a, SignIn::id());
    let b_signin = open_root(&mut b, SignIn::id());
    set_session(&a, "wait_code", None, None);
    set_session(&b, "wait_code", None, None);
    with_signin(&a, a_signin, |p| p.edited("11111".to_string()));
    with_signin(&b, b_signin, |p| p.edited("22222".to_string()));
    verb(&mut b, b_signin, "telegram.signin");
    assert!(inbox.try_recv().is_err(), "a fixture cannot answer another session's login");
    verb(&mut a, a_signin, "telegram.signin");
    let request: serde_json::Value = serde_json::from_str(&inbox.try_recv().unwrap()).unwrap();
    assert_eq!(request["@type"], "checkAuthenticationCode");
    assert_eq!(request["code"], "11111");
}

#[test]
fn player_verbs_follow_their_own_clock_without_a_widget_draw() {
    use kernel::app::Env;
    use kernel::caps::{ClockSource, FakeClock};
    let clock_a = FakeClock::at(virtual_epoch());
    let clock_b = FakeClock::at(virtual_epoch());
    let make_session = |clock: &FakeClock| Session::fake_with(APPS, &Env {
        clock: ClockSource::Virtual(clock.clone()),
        ..Env::default()
    });
    let mut a = make_session(&clock_a);
    let mut b = make_session(&clock_b);
    let voice = model::history(a.store(), STELAXIS).iter()
        .find(|m| m.media.as_ref().is_some_and(|md| md.kind == "voice"))
        .expect("a voice note in the fixture").id;
    let a_line = open_root(&mut a, Line::id(STELAXIS, voice));
    let b_line = open_root(&mut b, Line::id(STELAXIS, voice));
    let a_viewer = open_root(&mut a, Viewer::id(STELAXIS, voice));
    verb(&mut a, a_line, "telegram.play");
    verb(&mut b, b_line, "telegram.play");
    let label = |s: &Session, slot| {
        s.panel(slot).unwrap().borrow().verbs().into_iter()
            .find(|v| v.id == "telegram.play").unwrap().label
    };
    assert_eq!(label(&a, a_line), "pause");
    clock_a.advance(2.0);
    verb(&mut a, a_viewer, "telegram.play");
    assert_eq!(label(&a, a_line), "play", "the viewer takes over without a widget draw");
    assert_eq!(label(&a, a_viewer), "pause");
    clock_a.advance(3600.0);
    assert_eq!(label(&a, a_line), "play");
    assert_eq!(label(&a, a_viewer), "play");
    assert_eq!(label(&b, b_line), "pause");
    {
        let panel = a.panel(a_line).unwrap();
        let mut b = panel.borrow_mut();
        let line = b.as_any().downcast_mut::<Line>().unwrap();
        assert_eq!(line.player_state(&line.msg().unwrap(), a.now()).unwrap().position, 2.0,
            "the old timeline must stop at the handoff, not keep advancing behind its play label");
    }
    verb(&mut a, a_line, "telegram.play");
    assert_eq!(label(&a, a_line), "pause");
    clock_a.advance(3600.0);
    assert_eq!(label(&a, a_line), "play");
    assert_eq!(label(&b, b_line), "pause");
}

#[test]
fn dropping_files_stages_them_once_and_rejects_directories() {
    let mut s = session();
    let slot = open_root(&mut s, Chat::id(VERA));
    let file = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("resources/icon_32.png")
        .to_string_lossy()
        .into_owned();
    let paths = vec![
        file.clone(),
        file.clone(),
        std::env::temp_dir().to_string_lossy().into_owned(),
    ];
    let panel = s.panel(slot).unwrap().clone();
    let mut panel = panel.borrow_mut();
    let chat = panel.as_any().downcast_mut::<Chat>().unwrap();
    chat.drop_files(&mut s, &paths);
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    while !chat.poll(&mut s) {
        assert!(std::time::Instant::now() < deadline, "attachment validation did not finish");
        std::thread::sleep(std::time::Duration::from_millis(1));
    }
    assert_eq!(chat.carrying(), &[model::Carried { path: file }]);
    assert!(s
        .notes()
        .iter()
        .any(|n| n.err && n.msg.contains("not a regular file")));
    assert!(s.notes().iter().any(|n| n.msg.contains("attached 1 file")));
    s.take_notes();
    assert!(runtime::of(s.store()).operations.list().iter().any(|operation|
        operation.label == "attaching file" && operation.line().contains("not a regular file")),
        "a rejected attachment remains visible after its transient toast disappears");
}

#[test]
fn a_disconnected_worker_does_not_clear_the_composer_or_fake_a_send() {
    let mut s = session();
    let slot = open_root(&mut s, Chat::id(VERA));
    let rt = runtime::of(s.store());
    drop(rt.connect());
    with_chat(&s, slot, |c| {
        c.set_draft("keep this caption");
        c.carry(&["/tmp/keep-this.png".into()]);
    });
    send(&mut s, slot);
    with_chat(&s, slot, |c| {
        assert_eq!(c.draft(), "keep this caption");
        assert_eq!(c.carrying().len(), 1);
    });
    assert!(s.notes().last().unwrap().err);
    assert!(rt
        .operations
        .list()
        .iter()
        .all(|o| o.status != super::operations::Status::Pending));
}
