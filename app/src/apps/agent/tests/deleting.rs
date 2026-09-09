//! A committed deletion chooses its preview from the writer's new rows.
//! Display queries deliberately remain blocked with their old snapshots.

use super::*;
use std::sync::mpsc;
use std::time::Duration;

struct HeldReaders(Vec<mpsc::Sender<()>>);

impl HeldReaders {
    fn new(s: &Session) -> Self {
        let (started, observed) = mpsc::channel();
        let mut releases = Vec::new();
        for _ in 0..4 {
            let db = s.store().db();
            let started = started.clone();
            let (release, held) = mpsc::channel();
            releases.push(release);
            kernel::runtime::spawn(async move {
                db.read_async(move |_| {
                    started.send(()).unwrap();
                    held.recv_timeout(Duration::from_secs(10)).expect("release display reader");
                    Ok(())
                }).await.unwrap();
            });
        }
        for _ in 0..4 {
            observed.recv_timeout(Duration::from_secs(5)).expect("all display readers are held");
        }
        Self(releases)
    }
}

impl Drop for HeldReaders {
    fn drop(&mut self) {
        for release in self.0.drain(..) { let _ = release.send(()); }
    }
}

fn selected(s: &Session, slot: SlotId) -> Option<ChatId> {
    with_agents(s, slot, |a| a.list_mut().cursor_key().copied())
}

fn mark_newest(s: &mut Session) -> (SlotId, [ChatId; 3]) {
    let one = send_new(s, "the first");
    let two = send_new(s, "the second");
    let three = send_new(s, "the third");
    let slot = open_root(s, Agents::id());
    let preview = with_agents(s, slot, |a| {
        let preview = a.go(0).unwrap();
        assert!(a.toggle_mark());
        preview
    });
    s.nav(preview);
    s.settle();
    assert_eq!(selected(s, slot), Some(three));
    (slot, [one, two, three])
}

#[test]
fn deleting_a_chat_advances_before_display_refresh_and_the_next_edit_completion() {
    let mut s = session();
    let (slot, [_, successor, deleted]) = mark_newest(&mut s);
    let held = HeldReaders::new(&s);
    s.store().attach_ui(|| {});
    s.panel(slot).unwrap().borrow_mut().run("agent.delete", &mut s);
    let observed = std::rc::Rc::new(std::cell::Cell::new(false));
    let completed = observed.clone();
    s.act_async(kernel::session::Edit::writing("test.followup", "later edit", |_| Ok(()))
        .wake_if(|_| false), move |s, result| {
            assert!(result.is_some());
            completed.set(!s.showing(&Chat::id(successor)).is_empty());
        });
    kernel::runtime::block_on(s.store().flush_async()).unwrap();
    let displayed = with_agents(&s, slot, |a| a.list_mut().table().row(s.store(), 0).unwrap().id);
    assert_eq!(displayed, deleted, "the display still contains the removed row");
    s.settle();
    assert_eq!(selected(&s, slot), Some(successor));
    assert!(s.showing(&Chat::id(deleted)).is_empty());
    assert!(!s.showing(&Chat::id(successor)).is_empty());
    assert!(observed.get(), "the deletion's preview lands before the next edit owns history");
    let (rows, head) = s.history().rows();
    assert_eq!(rows.iter().find(|row| row.id == head).unwrap().label, "later edit");
    drop(held);
    s.shutdown();
}

#[test]
fn deleting_a_chat_preserves_intervening_cursor_filter_and_source_changes() {
    for change in ["cursor", "filter", "source"] {
        let mut s = session();
        let (slot, [chosen, _, deleted]) = mark_newest(&mut s);
        // The chats have finished their scripted turns. Native navigation
        // only wakes async workers; fixture workers would execute whole
        // passes inline and wait for this deliberately held writer.
        kernel::runtime::block_on(s.workers().shutdown());
        let held_readers = HeldReaders::new(&s);
        s.store().attach_ui(|| {});
        let (started, observed) = mpsc::channel();
        let (release, held) = mpsc::channel();
        let _pending = s.store().submit_write(move |_| {
            started.send(()).unwrap();
            held.recv_timeout(Duration::from_secs(10)).expect("the UI kept accepting input");
            Ok(())
        }).unwrap();
        observed.recv_timeout(Duration::from_secs(5)).unwrap();
        let before = std::time::Instant::now();
        verb(&mut s, slot, "agent.delete");
        let preview = with_agents(&s, slot, |a| match change {
            "cursor" => a.go(2),
            "filter" => { a.list_mut().set_filter("the first"); None }
            "source" => { a.list_mut().retarget(&model::CHATS); None }
            _ => unreachable!(),
        });
        if let Some(preview) = preview { s.nav(preview); }
        assert!(before.elapsed() < Duration::from_millis(200), "{change} waited for the writer");
        release.send(()).unwrap();
        kernel::runtime::block_on(s.store().flush_async()).unwrap();
        s.settle();
        assert!(model::chat(s.store(), deleted).is_none());
        assert_eq!(selected(&s, slot), (change == "cursor").then_some(chosen), "{change}");
        if change == "cursor" {
            assert!(!s.showing(&Chat::id(chosen)).is_empty(), "the user's newer preview wins");
        }
        drop(held_readers);
        s.shutdown();
    }
}
