use super::*;

/// A realistic cached history, including message bodies large enough to make
/// accidental transcript copies visible. No account, network or private data.
fn large_history(s: &Session) {
    s.store().write(|tx| {
        tx.execute("DELETE FROM tg_message WHERE chat = ?1", [VERA])?;
        let mut insert = tx.prepare(
            "INSERT INTO tg_message(id, chat, sender, date, text, out)
             VALUES(?1, ?2, ?2, ?3, ?4, 0)",
        )?;
        let text = "A cached message with enough text to exercise transcript allocation. ".repeat(16);
        for id in 1..=10_000 {
            insert.execute(rusqlite::params![id, VERA, 1_700_000_000.0 + f64::from(id), text])?;
        }
        Ok(())
    }).unwrap();
}

#[test]
#[ignore = "manual timing of chat navigation and repeated transcript draws"]
fn chat_switch_timing() {
    let mut s = session();
    large_history(&s);
    let list = open_root(&mut s, Chats::id());
    let start = std::time::Instant::now();
    go(&mut s, Nav::Preview { from: list, id: Chat::id(VERA) });
    let slot = s.showing(&Chat::id(VERA))[0];
    with_chat(&s, slot, |c| { std::hint::black_box(c.rows(s.now())); });
    eprintln!("10,000 messages: open + first transcript = {:?}", start.elapsed());
    let start = std::time::Instant::now();
    for _ in 0..100 {
        with_chat(&s, slot, |c| { std::hint::black_box(c.rows(s.now())); });
    }
    eprintln!("10,000 messages: average warm transcript = {:?}", start.elapsed() / 100);
}

#[test]
#[ignore = "manual timing of disk-backed navigation and background loading"]
fn disk_chat_switch_timing() {
    use kernel::app::{Apps, Env, Mode, Workers};
    use kernel::effect::World;
    use kernel::store::Store;
    use std::rc::Rc;
    use std::time::{Duration, Instant};

    let dir = std::env::temp_dir().join(format!("superapp-chat-timing-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let apps = Apps::new(APPS);
    let store = Rc::new(Store::open(Some(&dir.join("store.sqlite")), &apps.schemas()).unwrap());
    apps.seed(&store, Mode::Fake).unwrap();
    let world = Rc::new(World::new(store, apps.capabilities(Mode::Fake, &Env::default()), apps.registry()));
    let workers = Workers::inline(APPS, world.clone());
    let mut s = Session::new(apps, world, workers, Mode::Fake);
    large_history(&s);
    let list = open_root(&mut s, Chats::id());
    let start = Instant::now();
    go(&mut s, Nav::Preview { from: list, id: Chat::id(VERA) });
    let slot = s.showing(&Chat::id(VERA))[0];
    with_chat(&s, slot, |c| { std::hint::black_box(c.rows(s.now())); });
    eprintln!("disk / 10,000 messages: switch + first draw preparation = {:?}", start.elapsed());
    while !with_chat(&s, slot, |c| c.transcript_ready()) {
        assert!(start.elapsed() < Duration::from_secs(5));
        std::thread::sleep(Duration::from_millis(1));
    }
    eprintln!("disk / 10,000 messages: background transcript ready = {:?}", start.elapsed());
    let start = Instant::now();
    for _ in 0..1000 {
        with_chat(&s, slot, |c| { std::hint::black_box(c.rows(s.now())); });
    }
    eprintln!("disk / 10,000 messages: average warm transcript = {:?}", start.elapsed() / 1000);
    drop(s);
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn unchanged_transcripts_share_rows_and_refresh_message_edits() {
    let mut s = session();
    let slot = open_root(&mut s, Chat::id(VERA));
    let rows = with_chat(&s, slot, |c| c.rows(s.now()));
    for _ in 0..10 {
        let again = with_chat(&s, slot, |c| c.rows(s.now()));
        assert!(std::sync::Arc::ptr_eq(&rows, &again));
    }
    let id = rows.iter().find_map(Row::msg).unwrap().id;
    s.store().write(move |tx| tx.execute(
        "UPDATE tg_message SET text='changed' WHERE chat=?1 AND id=?2", [VERA, id],
    ).map(|_| ())).unwrap();
    let after = with_chat(&s, slot, |c| c.rows(s.now()));
    assert_eq!(after.iter().find_map(|r| r.msg().filter(|m| m.id == id)).unwrap().text, "changed");
    assert!(!std::sync::Arc::ptr_eq(&rows, &after));
    let tomorrow = with_chat(&s, slot, |c| c.rows(s.now() + 86400.0));
    assert!(!std::sync::Arc::ptr_eq(&after, &tomorrow), "day captions advance even without a database change");
}
