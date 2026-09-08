use super::*;

#[test]
#[ignore = "manual comparison of selective and broad searches in a 200k-message chat"]
fn large_chat_message_search_timing() {
    use kernel::filter;
    use kernel::richtable::Datasource;
    use std::time::Instant;
    let s = session();
    s.store().write(|c| {
        c.execute("DELETE FROM tg_message", [])?;
        c.execute("WITH RECURSIVE n(i) AS (VALUES(1) UNION ALL SELECT i + 1 FROM n WHERE i < 200000)
            INSERT INTO tg_message(id, chat, date, text)
            SELECT i, ?1, i, 'ordinary cached message' FROM n", [VERA])?;
        c.execute("UPDATE tg_message SET text = text || ' 🦩 selective-needle' WHERE id = 1", [])?;
        Ok(())
    }).unwrap();
    let mut previous = *model::MESSAGES.sql.spec;
    previous.from = "tg_message m INDEXED BY tg_message_search_chat
        JOIN tg_peer p ON p.id = m.chat LEFT JOIN tg_peer s ON s.id = m.sender";
    for (text, n) in [("🦩", 1), ("selective-needle", 1), ("ord", 200000), ("ordinary", 200000)] {
        let query = format!("@chat:vera {text}");
        let ast = filter::parse(&query).ast;
        let q = previous.count(model::MESSAGES.sql.tags, ast.as_ref());
        let start = Instant::now();
        let old_count: i64 = s.store().conn().query_row(&q.sql,
            rusqlite::params_from_iter(&q.params), |r| r.get(0)).unwrap();
        let old_elapsed = start.elapsed();
        let start = Instant::now();
        let count = model::MESSAGES.count(s.store(), ast.as_ref()).unwrap();
        let elapsed = start.elapsed();
        let start = Instant::now();
        let page = model::MESSAGES.page(s.store(), ast.as_ref(), 0, 50);
        let page_elapsed = start.elapsed();
        assert_eq!(old_count as usize, n);
        assert_eq!(count, n);
        assert_eq!(page.len(), n.min(50));
        eprintln!("{query:?}: previous count {old_elapsed:?}; count {elapsed:?}; page {page_elapsed:?}");
    }
}

/// Run against an expendable snapshot; opening it builds the search index.
/// The benchmark prints timings and counts, never cached message text.
#[test]
#[ignore = "manual timing on a copy of a populated store; set SUPERAPP_SEARCH_BENCH_DB"]
fn disk_message_search_timing() {
    use kernel::filter::Ast;
    use kernel::richtable::Datasource;
    use std::time::Instant;
    let path = std::env::var_os("SUPERAPP_SEARCH_BENCH_DB").expect("path to a disposable database copy");
    let start = Instant::now();
    let store = kernel::store::Store::open(Some(std::path::Path::new(&path)), &[&super::super::schema::SCHEMA]).unwrap();
    eprintln!("search index open/build: {:?}", start.elapsed());
    let start = Instant::now();
    let _: i64 = store.conn().query_row("SELECT COUNT(*) FROM tg_message m
        JOIN tg_peer p ON p.id = m.chat LEFT JOIN tg_peer s ON s.id = m.sender
        WHERE m.service = 0 AND casefold(m.text) LIKE casefold(?) ESCAPE '\\'",
        ["%superappsearchprobeabsent%"], |r| r.get(0)).unwrap();
    eprintln!("previous scan count: {:?}", start.elapsed());
    let mut engine = Engine::inline(vec![Box::new(super::super::search::TelegramSearch)]);
    for (i, text) in ["superappsearchprobeabsent", "thermos", "the", "что", "не", "a", "а"].iter().enumerate() {
        let ast = Ast::Text((*text).into());
        let start = Instant::now();
        let count = model::MESSAGES.count(&store, Some(&ast)).unwrap();
        let elapsed = start.elapsed();
        let start = Instant::now();
        let rows = model::MESSAGES.page(&store, Some(&ast), 0, 50);
        eprintln!("{text:?}: count {count} in {elapsed:?}; page {} in {:?}", rows.len(), start.elapsed());
        let start = Instant::now();
        engine.ask(&store, i as u64 + 1, text);
        let hits: usize = engine.collect().iter().map(|a| a.hits.len()).sum();
        eprintln!("{text:?}: provider {hits} hits in {:?}", start.elapsed());
    }
    let chat: String = store.conn().query_row("SELECT p.name FROM tg_message m JOIN tg_peer p ON p.id = m.chat
        GROUP BY m.chat ORDER BY COUNT(*) DESC LIMIT 1", [], |r| r.get(0)).unwrap();
    let ast = Ast::And(vec![Ast::Op { tag: "chat".into(), op: kernel::filter::Op::Eq, value: chat }, Ast::Text("не".into())]);
    let start = Instant::now();
    let count = model::MESSAGES.count(&store, Some(&ast)).unwrap();
    let elapsed = start.elapsed();
    let start = Instant::now();
    let rows = model::MESSAGES.page(&store, Some(&ast), 0, 50);
    eprintln!("scoped short query: count {count} in {elapsed:?}; page {} in {:?}", rows.len(), start.elapsed());
}

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

#[test]
fn typing_coalesces_draft_writes_without_invalidating_the_transcript() {
    let mut s = session();
    let slot = open_root(&mut s, Chat::id(VERA));
    let before = s.store().revision(&["tg_chat"]);
    let rows = with_chat(&s, slot, |c| c.rows(s.now()));
    for text in ["o", "on", "on my", "on my way"] {
        with_chat(&s, slot, |c| c.typed(text));
        assert_eq!(field_now(&s, slot), text);
        assert_eq!(draft_row(&s, VERA), "");
        assert_eq!(s.store().revision(&["tg_chat"]), before);
    }
    with_chat(&s, slot, Chat::save_pending_draft);
    assert_eq!(draft_row(&s, VERA), "on my way");
    assert!(std::sync::Arc::ptr_eq(&rows, &with_chat(&s, slot, |c| c.rows(s.now()))));
    let saved = s.store().revision(&["tg_chat"]);
    with_chat(&s, slot, Chat::save_pending_draft);
    assert_eq!(s.store().revision(&["tg_chat"]), saved);
}

#[test]
fn pending_drafts_survive_remote_updates_and_failed_saves() {
    let mut s = session();
    let slot = open_root(&mut s, Chat::id(VERA));
    write_draft(&s, VERA, "a synced draft");
    assert_eq!(field_now(&s, slot), "a synced draft");
    with_chat(&s, slot, |c| c.typed(""));
    write_draft(&s, VERA, "a later remote draft");
    assert_eq!(field_now(&s, slot), "", "a pending deletion must stay empty");
    with_chat(&s, slot, Chat::save_pending_draft);
    assert_eq!(draft_row(&s, VERA), "");

    s.store().db().set_writable(false);
    with_chat(&s, slot, |c| c.typed("keep these words"));
    assert!(runtime::of(s.store()).operations.list().is_empty(), "typing must not attempt a write");
    with_chat(&s, slot, Chat::save_pending_draft);
    assert_eq!(field_now(&s, slot), "keep these words");
    s.store().db().set_writable(true);
    with_chat(&s, slot, Chat::save_pending_draft);
    assert_eq!(draft_row(&s, VERA), "keep these words");
}

#[test]
fn sending_or_staging_before_the_draft_timer_cannot_restore_old_text() {
    let mut s = session();
    let slot = open_root(&mut s, Chat::id(VERA));
    with_chat(&s, slot, |c| c.typed("send before the timer"));
    send(&mut s, slot);
    with_chat(&s, slot, Chat::save_pending_draft);
    assert_eq!(draft_row(&s, VERA), "");
    assert_eq!(field_now(&s, slot), "");

    with_chat(&s, slot, |c| {
        c.typed("old pending text");
        c.stage_draft("explicit replacement", None, false).unwrap();
        c.save_pending_draft();
    });
    assert_eq!(draft_row(&s, VERA), "explicit replacement");
    assert_eq!(field_now(&s, slot), "explicit replacement");
}

#[test]
fn leaving_and_dropping_a_chat_save_pending_drafts() {
    let mut s = session();
    let slot = open_root(&mut s, Chat::id(VERA));
    with_chat(&s, slot, |c| {
        c.typed("saved on leaving");
        c.flush_draft();
    });
    assert_eq!(draft_row(&s, VERA), "saved on leaving");
    with_chat(&s, slot, |c| c.typed("saved on closing"));
    let world = s.world().clone();
    drop(s);
    assert_eq!(model::peer(world.store(), VERA).unwrap().draft.as_deref(), Some("saved on closing"));
}

#[test]
#[ignore = "manual comparison of large-chat input work and draft writes"]
fn chat_interaction_timing() {
    use std::hint::black_box;
    use std::time::Instant;
    let mut s = session();
    large_history(&s);
    let slot = open_root(&mut s, Chat::id(VERA));
    let snapshot = with_chat(&s, slot, |c| c.snapshot(s.now()));
    let visible: Vec<_> = snapshot.history.iter().rev().take(12).map(|m| m.key()).collect();
    let start = Instant::now();
    for _ in 0..1000 {
        let history = with_chat(&s, slot, |c| c.history());
        black_box(history.iter().filter(|m| m.unread_mention && visible.contains(&m.key())).count());
        black_box(snapshot.rows.iter().position(|r| r.msg().is_some_and(|m| m.key() == visible[0])));
    }
    eprintln!("10,000 messages: previous history scans per input/frame = {:?}", start.elapsed() / 1000);
    let start = Instant::now();
    for _ in 0..1000 {
        with_chat(&s, slot, |c| c.view_messages(&visible, s.now()));
        black_box(snapshot.row_index(visible[0]));
    }
    eprintln!("10,000 messages: indexed input/frame lookups = {:?}", start.elapsed() / 1000);
    let start = Instant::now();
    for i in 0..100 { with_chat(&s, slot, |c| c.set_draft(&format!("per-key save {i}"))); }
    eprintln!("100 synchronous draft writes = {:?}", start.elapsed());
    let start = Instant::now();
    for i in 0..100 { with_chat(&s, slot, |c| c.typed(&format!("coalesced save {i}"))); }
    eprintln!("100 in-memory keystrokes = {:?}", start.elapsed());
    with_chat(&s, slot, Chat::save_pending_draft);
    assert_eq!(draft_row(&s, VERA), "coalesced save 99");
}
