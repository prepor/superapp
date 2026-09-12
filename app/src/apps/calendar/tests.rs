use super::*;
use kernel::{
    app::App,
    nav::Nav,
    panel::{Panel, PanelId},
    richtable::Datasource,
    session::{Action, Session},
};
use serde_json::{json, Value};
#[path = "tests/loading.rs"]
mod loading;
#[path = "tests/recovery.rs"]
mod recovery;
#[path = "tests/recovery_ui.rs"]
mod recovery_ui;
#[path = "tests/scheduling.rs"]
mod scheduling;
#[path = "tests/tools.rs"]
mod tool_tests;
static APPS: &[&dyn App] = &[
    &crate::apps::mail::MAIL,
    &crate::apps::accounts::ACCOUNTS,
    &CALENDAR,
];
fn session() -> Session {
    let s = Session::fake(APPS);
    refresh(&s);
    s
}
fn refresh(s: &Session) {
    for _ in 0..5 {
        kernel::runtime::block_on(sync::Sync.pass(s.world()));
    }
}
fn event(s: &Session, name: &str) -> model::Event {
    model::EVENTS
        .page(s.store(), None, 0, 100)
        .iter()
        .find(|e| e.title == name)
        .expect(name)
        .clone()
}
fn form(s: &mut Session) -> (i64, edit::Form) {
    let id = edit::create(
        s,
        model::sources(s.store())
            .iter()
            .find(|c| c.writable())
            .unwrap()
            .id,
        None,
    )
    .unwrap();
    let mut f = edit::draft(s.store(), id).unwrap().form;
    f.title = "New project review".into();
    (id, f)
}
fn run(s: &mut Session, name: &str, v: Value) -> Result<Value, String> {
    let t = s.apps().tool(name).unwrap().clone();
    t.check(&v)?;
    if let Some(read) = t.reader {
        return kernel::runtime::block_on(read(&v)(s.world()));
    }
    if let Some(prepare) = t.preparer {
        let prepared = kernel::runtime::block_on(prepare(&v)(s.world()))?;
        let result = std::rc::Rc::new(std::cell::RefCell::new(None));
        let output = result.clone();
        prepared.commit(s, move |_, result| *output.borrow_mut() = Some(result));
        let result = result.borrow_mut().take().expect("fixture tool completed");
        return result;
    }
    (t.run)(s, &v)
}
fn operation(s: &Session, id: i64) -> (String, String) {
    s.store()
        .conn()
        .query_row(
            "SELECT state,error FROM calendar_change WHERE id=?",
            [id],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .unwrap()
}

#[test]
fn queued_editor_typing_coalesces_and_submission_waits_for_the_last_save_after_close() {
    let mut s = paused_session();
    let (draft, mut form) = form(&mut s);
    let slot = open(&mut s, panels::Editor::id(draft));
    let (notify,woke) = std::sync::mpsc::channel();
    s.store().attach_ui(move || {let _ = notify.send(());});
    let deadline = std::time::Instant::now()+std::time::Duration::from_secs(5);
    loop {
        let ready = s.panel(slot).unwrap().borrow_mut().as_any().downcast_mut::<panels::Editor>().unwrap().reading().is_some();
        if ready {break;}
        assert!(std::time::Instant::now()<deadline,"editor snapshot loaded");
        woke.recv_timeout(std::time::Duration::from_secs(5)).unwrap();
        s.store().poll_external();
    }
    let (entered, waiting) = std::sync::mpsc::channel();
    let (release, held) = std::sync::mpsc::channel();
    let _writer = s.store().submit_write(move |_| { entered.send(()).unwrap(); held.recv().unwrap(); Ok(()) }).unwrap();
    waiting.recv_timeout(std::time::Duration::from_secs(5)).unwrap();
    {
        let instance = s.panel(slot).unwrap();
        let mut panel = instance.borrow_mut();
        let editor = panel.as_any().downcast_mut::<panels::Editor>().unwrap();
        for n in 0..40 {
            form.title = format!("Queued draft {n}");
            editor.save(&mut s, 1, 1, form.clone());
        }
        assert_eq!(editor.reading().unwrap().form.title, "Queued draft 39");
        editor.run("calendar.save", &mut s);
        assert_eq!(editor.reading().unwrap().state, "pending", "the reviewed form is frozen while saving");
    }
    s.nav(Nav::Close { slot, label: None });
    s.settle();
    release.send(()).unwrap();
    s.shutdown();
    let saved = edit::draft(s.store(), draft).unwrap();
    assert_eq!(saved.form.title, "Queued draft 39");
    assert_eq!(saved.revision, 3, "one accepted save plus the newest queued form");
    assert_eq!(saved.state, "pending");
    let body: String = s.store().conn().query_row("SELECT body FROM calendar_change WHERE draft=?", [draft], |row| row.get(0)).unwrap();
    assert_eq!(serde_json::from_str::<Value>(&body).unwrap()["form"]["title"], "Queued draft 39");
}

#[test]
fn editor_refresh_keeps_accepted_text_and_accepts_an_undo_to_an_earlier_revision() {
    use std::{sync::{mpsc, Arc}, time::{Duration, Instant}};

    fn fresh(s: &mut Session, slot: kernel::layout::SlotId, woke: &mpsc::Receiver<()>) -> Arc<edit::Draft> {
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            s.store().poll_external();
            s.settle();
            if !s.history_busy() {
                let instance = s.panel(slot).unwrap();
                let mut panel = instance.borrow_mut();
                let editor = panel.as_any().downcast_mut::<panels::Editor>().unwrap();
                if let snapshot::State::Ready(display) = editor.display() {
                    let reading = editor.reading().unwrap();
                    assert_eq!(reading.revision, display.revision);
                    assert_eq!(reading.form.title, display.form.title);
                    assert!(editor.problem().is_empty());
                    return reading;
                }
            }
            assert!(Instant::now() < deadline, "the native display completion must arrive");
            woke.recv_timeout(Duration::from_secs(5)).unwrap();
        }
    }

    let mut s = paused_session();
    let (draft, form) = form(&mut s);
    let other = edit::create(&mut s, 1, None).unwrap();
    let slot = open(&mut s, panels::Editor::id(draft));
    {
        let instance = s.panel(slot).unwrap();
        let mut panel = instance.borrow_mut();
        let editor = panel.as_any().downcast_mut::<panels::Editor>().unwrap();
        assert_eq!(editor.reading().unwrap().revision, 1);
        editor.save(&mut s, 1, 1, form);
        assert_eq!(editor.reading().unwrap().revision, 2);
    }
    let (notify, woke) = mpsc::channel();
    s.store().attach_ui(move || { let _ = notify.send(()); });
    s.store().write(move |tx| {
        tx.execute("UPDATE calendar_draft SET revision=revision+1 WHERE id=?", [other])?;
        Ok(())
    }).unwrap();
    {
        let instance = s.panel(slot).unwrap();
        let mut panel = instance.borrow_mut();
        let editor = panel.as_any().downcast_mut::<panels::Editor>().unwrap();
        let reading = editor.reading().unwrap();
        assert_eq!(reading.revision, 2, "another draft must not restore the old parsed form during refresh");
        assert_eq!(reading.form.title, "New project review");
        assert!(editor.problem().is_empty(), "status can read the accepted form without borrowing it twice");
    }
    assert_eq!(fresh(&mut s, slot, &woke).revision, 2);

    // A recorded data edit restores its previous revision on undo. The fresh
    // answer must win even when its numeric revision is below the cached one.
    let tool = s.apps().tool("sql.write").unwrap().clone();
    let input = json!({
        "sql": "UPDATE calendar_draft SET revision=revision+1,form=json_set(form,'$.title','Later revision') WHERE id=?",
        "params": [draft],
    });
    tool.check(&input).unwrap();
    let prepared = kernel::runtime::block_on(tool.preparer.unwrap()(&input)(s.world())).unwrap();
    let committed = std::rc::Rc::new(std::cell::Cell::new(false));
    let completed = committed.clone();
    prepared.commit(&mut s, move |_, result| {
        assert_eq!(result.unwrap(), json!({"changes": 1}));
        completed.set(true);
    });
    let deadline = Instant::now() + Duration::from_secs(5);
    while !committed.get() {
        s.store().poll_external();
        s.settle();
        if committed.get() { break; }
        assert!(Instant::now() < deadline, "the recorded edit must commit");
        woke.recv_timeout(Duration::from_secs(5)).unwrap();
    }
    assert_eq!(fresh(&mut s, slot, &woke).revision, 3);
    assert!(s.undo());
    let deadline = Instant::now() + Duration::from_secs(5);
    while s.history_busy() {
        s.store().poll_external();
        s.settle();
        if !s.history_busy() { break; }
        assert!(Instant::now() < deadline, "the data reversal must complete");
        woke.recv_timeout(Duration::from_secs(5)).unwrap();
    }
    assert_eq!(edit::draft(s.store(), draft).unwrap().revision, 2, "undo restored the database before refreshing the display");
    let restored = fresh(&mut s, slot, &woke);
    assert_eq!(restored.revision, 2);
    assert_eq!(restored.form.title, "New project review");

    s.store().write(move |tx| {
        tx.execute("UPDATE calendar_draft SET revision=revision+1 WHERE id=?", [other])?;
        Ok(())
    }).unwrap();
    {
        let instance = s.panel(slot).unwrap();
        let mut panel = instance.borrow_mut();
        let editor = panel.as_any().downcast_mut::<panels::Editor>().unwrap();
        assert_eq!(editor.reading().unwrap().revision, 2, "a later refresh cannot resurrect the value that was undone");
    }
    assert_eq!(fresh(&mut s, slot, &woke).revision, 2);
    s.shutdown();
}

#[test]
fn a_prepared_calendar_edit_rechecks_permissions_and_event_revision_in_the_writer() {
    let mut s = paused_session();
    let (id, form) = form(&mut s);
    let plan = edit::save_plan(s.world(), id, 1, 1, form).unwrap();
    s.store().write(|tx| { tx.execute("UPDATE calendar_source SET role='reader' WHERE id=1", [])?; Ok(()) }).unwrap();
    let before = s.history().head();
    assert!(edit::fixture(&mut s, move |_| Ok(plan)).unwrap_err().contains("read-only"));
    assert_eq!(edit::draft(s.store(), id).unwrap().revision, 1);
    assert_eq!(s.history().head(), before);
    s.store().write(|tx| { tx.execute("UPDATE calendar_source SET role='owner' WHERE id=1", [])?; Ok(()) }).unwrap();
    let event = event(&s, "Design review");
    let plan = edit::command_plan(s.world(), event.id, &event.etag, "delete", "this", "", true).unwrap();
    s.store().write(move |tx| { tx.execute("UPDATE calendar_event SET etag='changed' WHERE id=?", [event.id])?; Ok(()) }).unwrap();
    assert!(edit::fixture(&mut s, move |_| Ok(plan)).unwrap_err().contains("changed"));
    assert_eq!(s.history().head(), before);
}
fn open(s: &mut Session, id: PanelId) -> kernel::layout::SlotId {
    s.act(Action::new("test.open", "open Calendar").moving(move |wm| {
        wm.open(id, None, false);
    }));
    s.settle();
    s.focus().unwrap()
}
fn use_suggestion(s: &mut Session, request: i64, slot: usize) -> Result<i64, String> {
    let (q, r, _, _) = availability::load(s.store(), request).ok_or("request missing")?;
    let start = r.ok_or("still checking")?.slots[slot].0;
    availability::apply_time(s, request, &availability::Search::from_query(&q), start)
}

#[test]
fn timeline_sources_and_month_respect_permissions_and_filters() {
    let mut s = session();
    let slot = open(&mut s, panels::Timeline::id());
    let p = s.panel(slot).unwrap();
    let mut p = p.borrow_mut();
    let p = p.as_any().downcast_mut::<panels::Timeline>().unwrap();
    assert_eq!(p.list.table().filter(), "");
    assert!(p.list.len(s.store()) >= 6);
    p.list.set_filter("@invited");
    assert_eq!(p.list.len(s.store()), 1);
    p.list.set_filter("@with:Nora @meet");
    assert_eq!(p.list.len(s.store()), 8);
    p.list.set_filter("@calendar:Studio");
    assert_eq!(p.list.len(s.store()), 1);
    assert!(p.verbs().iter().all(|v| !v.label.contains("30")));
    assert!(p.verbs().iter().all(|v| v.id != "calendar.later"));
}

/// Drive synchronization explicitly so a request can remain in flight without
/// threads, a renderer or a native event loop.
fn paused_session() -> Session {
    let s = session();
    Session::new(
        kernel::app::Apps::new(APPS),
        s.world().clone(),
        kernel::app::Workers::none(s.store().clone()),
    )
}
fn remote_event(s: &Session, id: &str, at: f64) {
    s.world().caps(|caps| {
        caps.get::<api::Fake>().unwrap().state.lock().unwrap().insert(
            id.into(),
            json!({"id":id,"summary":id,"start":{"dateTime":dates::rfc(at)},"end":{"dateTime":dates::rfc(at+3600.0)},"etag":"\"1\""}),
        );
    });
}

#[test]
fn automatic_ranges_coalesce_without_undo_actions_and_report_loading() {
    let mut s = paused_session();
    let before = model::coverage(s.store()).unwrap();
    let head = s.history().head();
    let end = before.end + 366.0 * 86400.0;
    remote_event(&s, "Beyond the first year", before.end + 86400.0);
    assert!(!model::cover(&mut s, before.start, before.end));
    assert!(model::cover(&mut s, before.start, end));
    for _ in 0..20 {
        assert!(!model::cover(&mut s, before.start, end));
    }
    let pending = model::coverage(s.store()).unwrap();
    assert_eq!(pending.requested, before.requested + 1);
    assert!(pending.pending());
    assert_eq!(s.history().head(), head);
    let status = model::sync_line(s.store());
    assert!(status.contains("loading events"), "{status}");
    assert!(!status.contains("loaded through"), "{status}");

    refresh(&s);
    assert!(!model::coverage(s.store()).unwrap().pending());
    assert!(model::sync_line(s.store()).contains("loaded through"));
    assert_eq!(
        event(&s, "Beyond the first year").start,
        before.end + 86400.0
    );
    assert_eq!(s.history().head(), head);
}

#[test]
fn timeline_prefetches_at_the_edge_and_scrolls_past_empty_filtered_pages() {
    let mut s = paused_session();
    let slot = open(&mut s, panels::Timeline::id());
    let instance = s.panel(slot).unwrap();
    let mut borrow = instance.borrow_mut();
    let p = borrow.as_any().downcast_mut::<panels::Timeline>().unwrap();
    let before = model::coverage(s.store()).unwrap();
    let head = s.history().head();
    remote_event(&s, "Future appointment", before.end + 86400.0);
    assert!(!p.prefetch(&mut s, false, true));
    p.reached_end();
    assert!(p.prefetch(&mut s, true, false));
    assert_eq!(
        model::coverage(s.store()).unwrap().end,
        before.end + 366.0 * 86400.0
    );
    for _ in 0..20 {
        assert!(!p.prefetch(&mut s, true, false));
    }
    refresh(&s);
    assert!(p.list.table().len(s.store()) > 0);
    assert_eq!(event(&s, "Future appointment").start, before.end + 86400.0);
    // Completing the fetch and drawing its rows must not request another year.
    for _ in 0..20 {
        assert!(!p.prefetch(&mut s, true, false));
    }

    p.list.set_filter("@with:nobody@example.com");
    assert_eq!(p.list.len(s.store()), 0);
    assert!(p.prefetch(&mut s, true, true));
    refresh(&s);
    let empty = model::coverage(s.store()).unwrap();
    assert_eq!(empty.end, before.end + 2.0 * 366.0 * 86400.0);
    for _ in 0..20 {
        assert!(!p.prefetch(&mut s, true, false));
    }
    // A fresh forward scroll can explore beyond an empty page, without a button.
    assert!(p.prefetch(&mut s, true, true));
    assert_eq!(
        model::coverage(s.store()).unwrap().requested,
        empty.requested + 1
    );
    assert_eq!(s.history().head(), head);
}

#[test]
fn timeline_prefetch_waits_for_sync_and_does_not_advance_after_failure() {
    struct Offline;
    #[async_trait::async_trait(?Send)]
    impl api::Api for Offline {
        async fn call(&mut self, _: &api::Request) -> Result<Value, String> {
            Err("offline".into())
        }
    }
    let mut s = paused_session();
    let slot = open(&mut s, panels::Timeline::id());
    let instance = s.panel(slot).unwrap();
    let mut borrow = instance.borrow_mut();
    let p = borrow.as_any().downcast_mut::<panels::Timeline>().unwrap();
    let before = model::coverage(s.store()).unwrap();
    assert!(model::cover(&mut s, before.start, before.end + 86400.0));
    let pending = model::coverage(s.store()).unwrap();
    p.reached_end();
    for _ in 0..20 {
        assert!(!p.prefetch(&mut s, true, true));
    }
    assert_eq!(
        model::coverage(s.store()).unwrap().requested,
        pending.requested
    );
    s.world()
        .caps(|caps| caps.insert::<dyn api::Api>(Box::new(Offline)));
    refresh(&s);
    assert!(model::sync_line(s.store()).contains("offline"));
    assert!(!p.prefetch(&mut s, true, true));
    assert_eq!(model::coverage(s.store()).unwrap().end, pending.end);

    // The user moves away while the request is pending: no stale tail demand.
    assert!(!p.prefetch(&mut s, false, false));
    s.world().caps(|caps| {
        let fake = caps.get::<api::Fake>().unwrap().clone();
        caps.insert::<dyn api::Api>(Box::new(fake));
    });
    s.store()
        .write(|c| c.execute("UPDATE calendar_sync SET requested=requested+1", []))
        .unwrap();
    refresh(&s);
    assert!(!p.prefetch(&mut s, true, false));
    assert!(p.prefetch(&mut s, true, true));
}

#[test]
fn month_and_day_views_fetch_their_civil_dates_on_display_navigation_and_restore() {
    let mut s = paused_session();
    let slot = open(&mut s, panels::Month::id("2030-03-01", ""));
    let at = dates::instant("2030-03-30T10:00", "Europe/Berlin").unwrap();
    remote_event(&s, "Distant month", at);
    let instance = s.panel(slot).unwrap();
    {
        let mut borrow = instance.borrow_mut();
        let p = borrow.as_any().downcast_mut::<panels::Month>().unwrap();
        let (start, end) = p.bounds().unwrap();
        assert_eq!(end - start, 42.0 * 86400.0 - 3600.0);
        p.cover(&mut s);
        assert_eq!(model::coverage(s.store()).unwrap().end, end);
        refresh(&s);
        assert!(p.rows().iter().any(|e| e.title == "Distant month"));
        p.run("calendar.next", &mut s);
        let next = model::coverage(s.store()).unwrap();
        assert_eq!(next.end, p.bounds().unwrap().1);
        p.run("calendar.previous", &mut s);
        p.cover(&mut s);
        assert_eq!(
            model::coverage(s.store()).unwrap().requested,
            next.requested
        );
    }

    // Simulate a saved distant month opened against the initial cache range.
    s.store()
        .write(|c| c.execute("UPDATE calendar_sync SET start=0,end=0", []))
        .unwrap();
    let mut restored = Session::new(
        kernel::app::Apps::new(APPS),
        s.world().clone(),
        kernel::app::Workers::none(s.store().clone()),
    );
    assert!(restored.restore());
    let instance = restored.panel(slot).unwrap();
    let mut borrow = instance.borrow_mut();
    let p = borrow.as_any().downcast_mut::<panels::Month>().unwrap();
    p.cover(&mut restored);
    let covered = model::coverage(restored.store()).unwrap();
    assert!(covered.start <= p.bounds().unwrap().0 && covered.end >= p.bounds().unwrap().1);
    drop(borrow);

    let slot = open(
        &mut restored,
        panels::Timeline::day("2026-03-08", "America/Havana", ""),
    );
    let instance = restored.panel(slot).unwrap();
    let mut borrow = instance.borrow_mut();
    let p = borrow.as_any().downcast_mut::<panels::Timeline>().unwrap();
    p.cover(&mut restored);
    let covered = model::coverage(restored.store()).unwrap();
    let start = dates::instant("2026-03-08T01:00", "America/Havana").unwrap();
    assert_eq!(covered.start, start);
    p.reached_end();
    assert!(
        !p.prefetch(&mut restored, true, true),
        "a day agenda stays bounded"
    );
}

#[test]
fn event_tool_fetches_missing_dates_and_returns_loading_until_synced() {
    let mut s = paused_session();
    let before = model::coverage(s.store()).unwrap();
    let start = before.end + 86400.0;
    remote_event(&s, "Agent requested dates", start + 3600.0);
    let query = json!({"start":dates::rfc(start),"end":dates::rfc(start+86400.0),"zone":"UTC"});
    let result = run(&mut s, "calendar.events", query.clone()).unwrap();
    assert_eq!(result["loading"], true);
    assert_eq!(result["total"], 0);
    let requested = model::coverage(s.store()).unwrap().requested;
    let result = run(&mut s, "calendar.events", query.clone()).unwrap();
    assert_eq!(result["loading"], true);
    assert_eq!(model::coverage(s.store()).unwrap().requested, requested);
    refresh(&s);
    let result = run(&mut s, "calendar.events", query).unwrap();
    assert_eq!(result["loading"], false);
    assert_eq!(result["events"][0]["title"], "Agent requested dates");
}
#[test]
fn create_edit_and_delete_round_trip_through_the_same_queue() {
    let mut s = session();
    let (id, mut f) = form(&mut s);
    f.guests = "nora@studio.example, ?leo@studio.example".into();
    f.meet = true;
    f.reminders = "popup:10,email:60".into();
    edit::save(&mut s, id, 1, 1, f).unwrap();
    let job = edit::commit(&mut s, id, 2).unwrap();
    refresh(&s);
    assert_eq!(operation(&s, job).0, "done");
    let e = event(&s, "New project review");
    assert!(!e.meet.is_empty());
    assert_eq!(
        model::raw(s.store(), e.id)["attendees"][1]["optional"],
        true
    );
    let id = edit::create(&mut s, e.source, Some(e.id)).unwrap();
    let d = edit::draft(s.store(), id).unwrap();
    let mut f = d.form;
    f.title = "Renamed review".into();
    edit::save(&mut s, id, d.revision, e.source, f).unwrap();
    let job = edit::commit(&mut s, id, 2).unwrap();
    refresh(&s);
    assert_eq!(operation(&s, job).0, "done");
    let e = event(&s, "Renamed review");
    let job = edit::command(&mut s, e.id, &e.etag, "delete", "this", "", true).unwrap();
    refresh(&s);
    assert_eq!(operation(&s, job).0, "done");
    assert!(model::event(s.store(), e.id).is_none());
}
#[test]
fn stale_draft_revisions_and_read_only_events_are_rejected() {
    let mut s = session();
    let (id, f) = form(&mut s);
    edit::save(&mut s, id, 1, 1, f.clone()).unwrap();
    assert!(edit::save(&mut s, id, 1, 1, f)
        .unwrap_err()
        .contains("changed"));
    assert!(edit::commit(&mut s, id, 1)
        .unwrap_err()
        .contains("revision"));
    let e = event(&s, "Studio all-hands");
    assert!(edit::create(&mut s, e.source, Some(e.id)).is_err());
    assert!(edit::command(&mut s, e.id, &e.etag, "delete", "all", "", true).is_err());
}
#[test]
fn google_conflicts_keep_the_draft_and_never_overwrite_newer_changes() {
    let mut s = session();
    let e = event(&s, "Planning");
    let id = edit::create(&mut s, e.source, Some(e.id)).unwrap();
    let mut f = edit::draft(s.store(), id).unwrap().form;
    f.title = "My draft title".into();
    edit::save(&mut s, id, 1, e.source, f).unwrap();
    s.world().caps(|c| {
        let fake = c.get::<api::Fake>().unwrap();
        let mut rows = fake.state.lock().unwrap();
        rows.get_mut(&e.remote).unwrap()["etag"] = json!("\"server-new\"");
    });
    let job = edit::commit(&mut s, id, 2).unwrap();
    refresh(&s);
    assert_eq!(operation(&s, job).0, "failed");
    assert!(operation(&s, job).1.contains("changed"));
    assert_eq!(
        edit::draft(s.store(), id).unwrap().form.title,
        "My draft title"
    );
}
#[test]
fn deleting_all_recurrences_and_editing_one_have_distinct_effects() {
    let mut s = session();
    let e = event(&s, "Design review");
    let id = edit::create(&mut s, e.source, Some(e.id)).unwrap();
    let mut f = edit::draft(s.store(), id).unwrap().form;
    f.title = "One special review".into();
    edit::save(&mut s, id, 1, e.source, f).unwrap();
    let job = edit::commit(&mut s, id, 2).unwrap();
    refresh(&s);
    assert_eq!(operation(&s, job).0, "done");
    assert_eq!(
        model::EVENTS
            .page(s.store(), None, 0, 100)
            .iter()
            .filter(|e| e.title == "Design review")
            .count(),
        7
    );
    let e = event(&s, "One special review");
    let job = edit::command(&mut s, e.id, &e.etag, "delete", "all", "", true).unwrap();
    refresh(&s);
    assert_eq!(operation(&s, job).0, "done");
    assert!(model::EVENTS
        .page(s.store(), None, 0, 100)
        .iter()
        .all(|e| e.series != "design"));
}
#[test]
fn following_edits_split_at_the_selected_occurrence() {
    let mut s = session();
    let e = model::EVENTS
        .page(s.store(), None, 0, 100)
        .iter()
        .filter(|e| e.title == "Design review")
        .nth(2)
        .unwrap()
        .clone();
    let id = edit::create(&mut s, e.source, Some(e.id)).unwrap();
    let mut f = edit::draft(s.store(), id).unwrap().form;
    f.title = "New weekly review".into();
    f.scope = "following".into();
    edit::save(&mut s, id, 1, e.source, f).unwrap();
    let job = edit::commit(&mut s, id, 2).unwrap();
    refresh(&s);
    assert_eq!(operation(&s, job), ("done".into(), String::new()));
    let rows = model::EVENTS.page(s.store(), None, 0, 100);
    assert_eq!(
        rows.iter().filter(|e| e.title == "Design review").count(),
        2
    );
    assert_eq!(
        rows.iter()
            .filter(|e| e.title == "New weekly review")
            .count(),
        6
    );
}
#[test]
fn rsvp_updates_only_your_attendee_entry() {
    let mut s = session();
    let e = event(&s, "Research catch-up");
    let job = edit::command(&mut s, e.id, &e.etag, "respond", "this", "accepted", true).unwrap();
    refresh(&s);
    assert_eq!(operation(&s, job).0, "done");
    assert_eq!(event(&s, "Research catch-up").response, "accepted");
}
#[test]
fn date_math_observes_exclusive_ends_leap_days_and_dst() {
    assert!(dates::date("2026-02-29").is_err());
    assert!(dates::date("2028-02-29").is_ok());
    assert!(dates::instant("2026-03-29T02:30", "Europe/Berlin").is_err());
    assert!(dates::instant("2026-10-25T02:30", "Europe/Berlin").is_err());
    assert!(dates::instant("2026-10-25T02:30:00+02:00", "Europe/Berlin").is_ok());
    let mut f = edit::Form {
        title: "All day".into(),
        start: "2026-03-29".into(),
        end: "2026-03-30".into(),
        zone: "Europe/Berlin".into(),
        all_day: true,
        ..Default::default()
    };
    let (a, b) = f.bounds().unwrap();
    assert_eq!(b - a, 23.0 * 3600.0);
    f.end = f.start.clone();
    assert!(f.validate().is_err());
    assert_eq!(dates::grid("2026-09-01").len(), 42);
}

const MIDNIGHT_GAPS: [(&str, &str); 3] = [
    ("America/Santiago", "2026-09-06"),
    ("Asia/Beirut", "2026-03-29"),
    ("America/Havana", "2026-03-08"),
];

#[test]
fn all_day_bounds_use_the_first_valid_instant_on_midnight_gap_dates() {
    for (zone, day) in MIDNIGHT_GAPS {
        let next = dates::date(day).unwrap().succ_opt().unwrap().to_string();
        let form = edit::Form {
            title: "DST day".into(),
            start: day.into(),
            end: next.clone(),
            zone: zone.into(),
            all_day: true,
            ..Default::default()
        };
        form.validate().unwrap();
        let (a, b) = form.bounds().unwrap();
        assert_eq!(dates::local(a, zone), format!("{day}T01:00"), "{zone}");
        assert_eq!(dates::local(b, zone), format!("{next}T00:00"), "{zone}");
        assert_eq!(b - a, 23.0 * 3600.0, "{zone}");
        let body = form.patch(&json!({}), "test").unwrap();
        assert_eq!(body["start"], json!({"date":day}));
        assert_eq!(body["end"], json!({"date":next}));
        // Timed inputs in the gap still need an explicit offset.
        assert!(dates::instant(&format!("{day}T00:30"), zone).is_err());
    }
    let repeated = dates::midnight(dates::date("2026-11-01").unwrap(), "America/Havana").unwrap();
    assert_eq!(
        repeated,
        dates::instant("2026-11-01T04:00:00Z", "UTC").unwrap()
    );
    assert!(dates::midnight(dates::date("2011-12-30").unwrap(), "Pacific/Apia").is_err());
}

/// Google supplies expanded occurrences. Keep that snapshot independent of
/// the fake's recurrence parser so these tests exercise the sync transaction
/// and the exact outgoing series changes, including Google's DATE fields.
struct CalendarSnapshot {
    zone: String,
    events: Vec<Value>,
    fake: api::Fake,
    reject_trim: bool,
}
#[async_trait::async_trait(?Send)]
impl api::Api for CalendarSnapshot {
    async fn call(&mut self, r: &api::Request) -> Result<Value, String> {
        if r.method == "GET" && r.path == "/users/me/calendarList" {
            return Ok(json!({"items":[{
                "id":"primary", "summary":"Work", "timeZone":self.zone,
                "accessRole":"owner"
            }]}));
        }
        if r.method == "GET" && r.path == api::events("primary") {
            return Ok(json!({"items":self.events}));
        }
        if self.reject_trim && r.method == "PATCH" && r.body["recurrence"].is_array() {
            self.reject_trim = false;
            return Err("HTTP 400: series trim rejected".into());
        }
        api::Api::call(&mut self.fake, r).await
    }
}

#[test]
fn calendar_sync_caches_all_day_events_on_midnight_gap_dates() {
    for (zone, day) in MIDNIGHT_GAPS {
        let mut s = session();
        let next = dates::date(day).unwrap().succ_opt().unwrap().to_string();
        let events = vec![
            json!({"id":"gap", "summary":"DST day", "start":{"date":day}, "end":{"date":next}}),
            json!({"id":"normal", "summary":"Normal event", "start":{"dateTime":format!("{day}T12:00"),"timeZone":zone}, "end":{"dateTime":format!("{day}T13:00"),"timeZone":zone}}),
        ];
        s.world().caps(|caps| {
            caps.insert::<dyn api::Api>(Box::new(CalendarSnapshot {
                zone: zone.into(),
                events,
                fake: api::Fake::default(),
                reject_trim: false,
            }));
        });
        let noon = dates::instant(&format!("{day}T12:00"), zone).unwrap();
        model::cover(&mut s, noon - 86400.0, noon + 86400.0);
        model::refresh(&mut s);
        refresh(&s);
        let rows = model::EVENTS.page(s.store(), None, 0, 100);
        assert_eq!(rows.len(), 2, "{zone}: entire page must be committed");
        let all_day = rows.iter().find(|e| e.remote == "gap").unwrap();
        assert!(all_day.all_day);
        assert_eq!(all_day.day, day);
        assert_eq!(all_day.end - all_day.start, 23.0 * 3600.0);
        let source = model::source(s.store(), all_day.source).unwrap();
        assert!(source.checked.is_some());
        assert!(source.error.is_empty());
    }
}

#[test]
fn all_day_following_edits_and_deletes_use_an_inclusive_date_cutoff() {
    for (kind, reject_trim) in [("save", false), ("delete", false), ("save", true)] {
        let mut s = session();
        let master = json!({
            "id":"allday", "summary":"Daily all day", "etag":"\"1\"",
            "start":{"date":"2026-09-08"}, "end":{"date":"2026-09-09"},
            "recurrence":["RRULE:FREQ=DAILY;COUNT=8"], "organizer":{"self":true}
        });
        // The selected occurrence was already moved; its original date is
        // still where the old series must stop, regardless of the new edit.
        let instance = json!({
            "id":"allday-occurrence", "summary":"Daily all day", "etag":"\"1\"",
            "start":{"date":"2026-09-12"}, "end":{"date":"2026-09-13"},
            "originalStartTime":{"date":"2026-09-10"}, "recurringEventId":"allday",
            "organizer":{"self":true}
        });
        let fake = api::Fake::default();
        {
            let mut state = fake.state.lock().unwrap();
            state.insert("allday".into(), master);
            state.insert("allday-occurrence".into(), instance.clone());
        }
        s.world().caps(|caps| {
            caps.insert::<dyn api::Api>(Box::new(CalendarSnapshot {
                zone: "Europe/Berlin".into(),
                events: vec![instance],
                fake: fake.clone(),
                reject_trim,
            }));
        });
        model::refresh(&mut s);
        refresh(&s);
        let e = event(&s, "Daily all day");
        let job = if kind == "save" {
            let id = edit::create(&mut s, e.source, Some(e.id)).unwrap();
            let mut form = edit::draft(s.store(), id).unwrap().form;
            form.title = "Later days".into();
            form.scope = "following".into();
            form.start = "2026-09-13".into();
            form.end = "2026-09-14".into();
            edit::save(&mut s, id, 1, e.source, form).unwrap();
            edit::commit(&mut s, id, 2).unwrap()
        } else {
            edit::command(&mut s, e.id, &e.etag, "delete", "following", "", false).unwrap()
        };
        refresh(&s);
        if reject_trim {
            assert_eq!(operation(&s, job).0, "failed");
            {
                let state = fake.state.lock().unwrap();
                assert_eq!(
                    state["allday"]["recurrence"],
                    json!(["RRULE:FREQ=DAILY;COUNT=8"])
                );
                assert_eq!(
                    state
                        .values()
                        .filter(|v| v["summary"] == "Later days")
                        .count(),
                    1
                );
            }
            sync::retry(&mut s, job).unwrap();
            refresh(&s);
        }
        assert_eq!(operation(&s, job), ("done".into(), String::new()));
        let state = fake.state.lock().unwrap();
        assert_eq!(
            state["allday"]["recurrence"],
            json!(["RRULE:FREQ=DAILY;UNTIL=20260909"])
        );
        assert_eq!(state["allday"]["start"], json!({"date":"2026-09-08"}));
        let replacements: Vec<_> = state
            .values()
            .filter(|v| v["summary"] == "Later days")
            .collect();
        assert_eq!(replacements.len(), usize::from(kind == "save"));
        if let Some(new) = replacements.first() {
            assert_eq!(new["recurrence"], json!(["RRULE:FREQ=DAILY;COUNT=6"]));
            assert_eq!(new["start"], json!({"date":"2026-09-13"}));
        }
    }
}

#[test]
fn unknown_and_malformed_freebusy_never_become_free() {
    let q = availability::Query {
        start: "2026-09-08T09:00".into(),
        end: "2026-09-08T11:00".into(),
        zone: "UTC".into(),
        minutes: 30,
        guests: vec!["known".into(), "unknown".into()],
        draft_guests: None,
    };
    let result=availability::calculate(&q,&json!({"calendars":{"known":{"busy":[{"start":"2026-09-08T09:00:00Z","end":"2026-09-08T10:00:00Z"}]},"unknown":{"errors":[{"reason":"notFound"}]}}}),0.0).unwrap();
    assert!(!result.complete);
    assert_eq!(result.people.iter().filter(|p| p.known).count(), 1);
    assert_eq!(
        result.slots[0].0,
        dates::instant("2026-09-08T10:00", "UTC").unwrap()
    );
    let result = availability::calculate(
        &q,
        &json!({"calendars":{"known":{"busy":[{"start":"nonsense","end":"bad"}]}}}),
        0.0,
    )
    .unwrap();
    assert!(!result.complete);
    assert!(result.people.iter().all(|p| !p.known));
}
#[test]
fn availability_includes_owner_and_applies_to_persistent_draft() {
    let mut s = session();
    let (id, f) = form(&mut s);
    edit::save(&mut s, id, 1, 1, f.clone()).unwrap();
    let day = &f.start[..10];
    let q = availability::Query {
        start: format!("{day}T09:00"),
        end: format!("{day}T18:00"),
        zone: f.zone,
        minutes: 30,
        guests: vec!["external@example.com".into()],
        draft_guests: None,
    };
    let request = availability::request(&mut s, 1, q, Some(id)).unwrap();
    refresh(&s);
    let (_, r, error, _) = availability::load(s.store(), request).unwrap();
    assert!(error.is_empty());
    let r = r.unwrap();
    assert!(!r.complete);
    assert!(r.people.len() >= 2);
    assert!(!r.slots.is_empty());
    use_suggestion(&mut s, request, 0).unwrap();
    assert_eq!(edit::draft(s.store(), id).unwrap().revision, 3);
}
#[test]
fn raw_google_fields_and_guest_responses_survive_edits() {
    let base = json!({"attachments":[{"fileUrl":"https://drive.google.com/file/d/123"}],"attendees":[{"email":"a@example.com","responseStatus":"accepted","comment":"keep me","resource":true}],"extendedProperties":{"private":{"other":"kept"}},"conferenceData":{"conferenceId":"keep-me"}});
    let f = edit::Form {
        title: "test".into(),
        start: "2026-09-08T10:00".into(),
        end: "2026-09-08T11:00".into(),
        guests: "a@example.com".into(),
        meet: true,
        ..Default::default()
    };
    let patch = f.patch(&base, "test-op").unwrap();
    assert!(patch.get("attachments").is_none());
    assert!(patch.get("conferenceData").is_none());
    assert_eq!(patch["attendees"][0]["comment"], "keep me");
    assert_eq!(patch["attendees"][0]["responseStatus"], "accepted");
    assert_eq!(patch["extendedProperties"]["private"]["other"], "kept");
}
#[test]
fn tools_have_strict_schemas_and_external_writes_ask() {
    let mut s = session();
    for t in tools::all() {
        assert_eq!(t.input["additionalProperties"], false);
        assert!(!t.description.is_empty());
        assert!(t.check(&json!({"undeclared":true})).is_err());
    }
    assert!(run(
        &mut s,
        "calendar.delete",
        json!({"event":1,"etag":"x","scope":"guess","notify":true})
    )
    .is_err());
    for name in [
        "calendar.create",
        "calendar.update",
        "calendar.commit",
        "calendar.delete",
        "calendar.respond",
        "calendar.retry",
    ] {
        assert!(s.apps().tool(name).unwrap().asks);
    }
    assert!(!s.apps().tool("calendar.draft").unwrap().asks);
    let result = run(&mut s, "calendar.calendars", json!({})).unwrap();
    assert_eq!(result.as_array().unwrap().len(), 2);
    let result = run(
        &mut s,
        "calendar.suggest",
        json!({"field":"guests","text":"nor"}),
    )
    .unwrap();
    assert!(result["suggestions"]
        .as_array()
        .unwrap()
        .iter()
        .any(|p| p["value"] == "nora@studio.example"));
    assert!(!s.apps().tool("calendar.suggest").unwrap().asks);
    assert!(run(
        &mut s,
        "calendar.suggest",
        json!({"field":"title","text":"nor"})
    )
    .is_err());
}

#[test]
fn ui_opening_creates_and_restores_the_same_draft() {
    let mut s = session();
    let from = open(&mut s, panels::Timeline::id());
    panels::start_editor(&mut s, from, 1, None);
    for _ in 0..5 {
        s.settle();
    }
    let slot = s.focus().unwrap();
    let id = s.panel(slot).unwrap().borrow().persist();
    let draft = id.args[0].parse::<i64>().unwrap();
    assert!(edit::draft(s.store(), draft).is_some());
    s.nav(Nav::Close { slot, label: None });
    s.settle();
    let reopened = open(&mut s, id);
    assert_eq!(
        s.panel(reopened).unwrap().borrow().persist().args[0],
        draft.to_string()
    );
}
#[test]
fn shared_service_switches_preserve_other_services_and_cached_events() {
    let mut s = session();
    let before = model::EVENTS.count(s.store(), None).unwrap();
    s.act(Action::writing("test.service", "disable Calendar", |c| {
        crate::identity::set_services(c, 1, true, false)
    }));
    assert_eq!(model::EVENTS.count(s.store(), None), Some(0));
    assert!(crate::identity::services(s.store().conn(), 1).0);
    s.act(Action::writing("test.service", "enable Calendar", |c| {
        crate::identity::set_services(c, 1, true, true)
    }));
    assert_eq!(model::EVENTS.count(s.store(), None), Some(before));
    let sources = run(&mut s, "accounts.list", json!({})).unwrap();
    assert!(!sources.to_string().contains("refresh_token"));
}
#[test]
fn draft_edits_are_undoable_without_sending() {
    let mut s = session();
    let (id, f) = form(&mut s);
    edit::save(&mut s, id, 1, 1, f).unwrap();
    assert_eq!(
        edit::draft(s.store(), id).unwrap().form.title,
        "New project review"
    );
    assert!(s.undo());
    assert_eq!(edit::draft(s.store(), id).unwrap().form.title, "");
    assert!(s.redo());
    assert_eq!(
        edit::draft(s.store(), id).unwrap().form.title,
        "New project review"
    );
    assert_eq!(
        s.store()
            .conn()
            .query_row("SELECT COUNT(*) FROM calendar_change", [], |r| r
                .get::<_, i64>(0))
            .unwrap(),
        0
    );
}

#[test]
fn every_native_bar_respects_workspace_shortcuts() {
    let mut s = session();
    let event = event(&s, "Design review");
    let (draft, _) = form(&mut s);
    for id in [
        panels::Timeline::id(),
        panels::Month::id("", ""),
        panels::Event::id(event.id),
        panels::Editor::id(draft),
        panels::Sources::id(),
    ] {
        let slot = open(&mut s, id);
        crate::shell::bar::check(&s.panel(slot).unwrap().borrow().verbs());
    }
}

struct LostResponse {
    fake: api::Fake,
    on_post: bool,
    on_trim: bool,
}
#[async_trait::async_trait(?Send)]
impl api::Api for LostResponse {
    async fn call(&mut self, request: &api::Request) -> Result<Value, String> {
        let answer = api::Api::call(&mut self.fake, request).await?;
        if self.on_post && request.method == "POST" && request.path.ends_with("/events") {
            self.on_post = false;
            return Err("network response lost after Google accepted the event".into());
        }
        if self.on_trim && request.method == "PATCH" && request.body["recurrence"].is_array() {
            self.on_trim = false;
            return Err("network response lost after Google trimmed the series".into());
        }
        Ok(answer)
    }
}
fn lose_response(s: &Session, on_post: bool, on_trim: bool) {
    s.world().caps(|caps| {
        let fake = caps.get::<api::Fake>().unwrap().clone();
        caps.insert::<dyn api::Api>(Box::new(LostResponse {
            fake,
            on_post,
            on_trim,
        }));
    });
}
#[test]
fn retry_recovers_a_lost_create_response_without_another_event() {
    let mut s = session();
    let (id, f) = form(&mut s);
    edit::save(&mut s, id, 1, 1, f).unwrap();
    lose_response(&s, true, false);
    let job = edit::commit(&mut s, id, 2).unwrap();
    refresh(&s);
    assert_eq!(operation(&s, job).0, "failed");
    let d = edit::draft(s.store(), id).unwrap();
    assert!(!edit::editable(&d));
    assert!(edit::save(&mut s, id, d.revision, 1, d.form).is_err());
    sync::retry(&mut s, job).unwrap();
    refresh(&s);
    assert_eq!(operation(&s, job).0, "done");
    assert_eq!(
        model::EVENTS
            .page(s.store(), None, 0, 100)
            .iter()
            .filter(|e| e.title == "New project review")
            .count(),
        1
    );
}
#[test]
fn retry_finishes_a_split_after_the_final_response_was_lost() {
    let mut s = session();
    let e = model::EVENTS
        .page(s.store(), None, 0, 100)
        .iter()
        .filter(|e| e.title == "Design review")
        .nth(2)
        .unwrap()
        .clone();
    let id = edit::create(&mut s, 1, Some(e.id)).unwrap();
    let mut f = edit::draft(s.store(), id).unwrap().form;
    f.scope = "following".into();
    f.title = "Split review".into();
    edit::save(&mut s, id, 1, 1, f).unwrap();
    lose_response(&s, false, true);
    let job = edit::commit(&mut s, id, 2).unwrap();
    refresh(&s);
    assert_eq!(operation(&s, job).0, "failed");
    sync::retry(&mut s, job).unwrap();
    refresh(&s);
    assert_eq!(operation(&s, job).0, "done");
    let rows = model::EVENTS.page(s.store(), None, 0, 100);
    assert_eq!(
        rows.iter().filter(|e| e.title == "Design review").count(),
        2
    );
    assert_eq!(rows.iter().filter(|e| e.title == "Split review").count(), 6);
}
#[test]
fn editing_an_existing_repeated_hour_keeps_its_instant() {
    let event = json!({"summary":"Clock change", "start":{"dateTime":"2026-10-25T02:10:00+02:00","timeZone":"Europe/Berlin"},"end":{"dateTime":"2026-10-25T02:40:00+02:00","timeZone":"Europe/Berlin"}});
    let f = edit::Form::from_event(&event, "Europe/Berlin");
    let (start, end) = f.bounds().unwrap();
    assert_eq!(end - start, 1800.0);
    assert_eq!(
        start,
        dates::instant("2026-10-25T00:10:00Z", "UTC").unwrap()
    );
}
#[test]
fn availability_cannot_apply_an_old_guest_list() {
    let mut s = session();
    let (id, f) = form(&mut s);
    edit::save(&mut s, id, 1, 1, f.clone()).unwrap();
    let day = &f.start[..10];
    let q = availability::Query {
        start: format!("{day}T09:00"),
        end: format!("{day}T18:00"),
        zone: f.zone,
        minutes: 30,
        guests: vec![],
        draft_guests: None,
    };
    let request = availability::request(&mut s, 1, q, Some(id)).unwrap();
    refresh(&s);
    let d = edit::draft(s.store(), id).unwrap();
    let mut form = d.form;
    form.guests = "new-person@example.com".into();
    edit::save(&mut s, id, d.revision, 1, form).unwrap();
    assert!(use_suggestion(&mut s, request, 0)
        .unwrap_err()
        .contains("guest list changed"));
}

#[test]
fn an_existing_store_can_sync_calendar_without_demo_seeding_or_mail() {
    static SHARED: &[&dyn App] = &[&crate::apps::accounts::ACCOUNTS, &CALENDAR];
    let store =
        kernel::store::Store::open(None, &[&crate::identity::SCHEMA, &schema::SCHEMA], kernel::sync::Device::fake()).unwrap();
    store
        .write(|c| {
            let id =
                crate::identity::accounts::add_account_tx(c, "me@prepor.dev", "", "", "google")?;
            crate::identity::set_services(c, id, false, true)
        })
        .unwrap();
    let world = kernel::app::world_for(SHARED, store, Mode::Fake, &Env::default());
    kernel::runtime::block_on(sync::Sync.pass(&world));
    assert_eq!(model::sources(world.store()).len(), 2);
    assert!(model::EVENTS.count(world.store(), None).unwrap() > 0);
    let start = world
        .store()
        .conn()
        .query_row("SELECT start FROM calendar_sync WHERE id=1", [], |r| {
            r.get::<_, f64>(0)
        })
        .unwrap();
    assert_eq!(start, world.now() - 90.0 * 86400.0);
}

#[test]
fn rechecking_availability_keeps_the_latest_request_on_screen_and_restore() {
    let mut s = session();
    let (draft, f) = form(&mut s);
    let day = &f.start[..10];
    let q = availability::Query {
        start: format!("{day}T09:00"),
        end: format!("{day}T18:00"),
        zone: f.zone,
        minutes: 30,
        guests: vec![],
        draft_guests: None,
    };
    let old = availability::request(&mut s, 1, q, Some(draft)).unwrap();
    refresh(&s);
    let slot = open(&mut s, panels::Availability::id(old));
    s.panel(slot)
        .unwrap()
        .borrow_mut()
        .run("calendar.check", &mut s);
    for _ in 0..5 {
        s.settle();
    }
    let latest = s.panel(slot).unwrap().borrow().persist();
    assert_ne!(latest.args[0], old.to_string());
    refresh(&s);
    let new = latest.args[0].parse().unwrap();
    assert!(availability::load(s.store(), new).unwrap().1.is_some());
    s.nav(Nav::Close { slot, label: None });
    s.settle();
    let restored = open(&mut s, latest);
    assert_eq!(
        s.panel(restored).unwrap().borrow().persist().args[0],
        new.to_string()
    );
}

#[test]
fn guests_complete_names_and_emails_without_repeating_existing_guests() {
    use kernel::richtable::Completion;
    let s = session();
    let field = completion::Field::Guests;
    let ctx = field.context("nor", 3).unwrap();
    let choices = field.offer(s.store(), &ctx);
    let nora = choices
        .iter()
        .find(|v| v.value == "nora@studio.example")
        .unwrap();
    assert_eq!(nora.label, "Nora");
    assert_eq!(
        field.splice("nor", 3, &ctx, nora),
        ("nora@studio.example, ".into(), 21)
    );
    let text = "leo@studio.example; ?nora-old@example.com, somebody@example.com";
    let at = text.find("nora").unwrap() + 2;
    let ctx = field.context(text, at).unwrap();
    let (line, cursor) = field.splice(text, at, &ctx, nora);
    assert_eq!(
        line,
        "leo@studio.example; ?nora@studio.example, somebody@example.com"
    );
    assert_eq!(&line[..cursor], "leo@studio.example; ?nora@studio.example");
    let already = "?NORA@studio.example; no";
    let ctx = field.context(already, already.len()).unwrap();
    assert!(field
        .offer(s.store(), &ctx)
        .iter()
        .all(|v| v.value != "nora@studio.example"));
    let ctx = field.context("Kovac", 5).unwrap();
    assert!(field
        .offer(s.store(), &ctx)
        .iter()
        .any(|v| v.label.contains("Kovac")));
    let text = "léa@example.com, ?nor";
    let ctx = field.context(text, text.len()).unwrap();
    assert_eq!(
        field.splice(text, text.len(), &ctx, nora).0,
        "léa@example.com, ?nora@studio.example, "
    );
}

#[test]
fn calendar_only_stores_offer_guests_and_useful_field_presets() {
    use kernel::richtable::Completion;
    static APPS: &[&dyn App] = &[&crate::apps::accounts::ACCOUNTS, &CALENDAR];
    let s = Session::fake(APPS);
    refresh(&s);
    let field = completion::Field::Guests;
    assert!(field
        .offer(s.store(), &field.context("nor", 3).unwrap())
        .iter()
        .any(|v| v.value == "nora@studio.example"));
    for (field, text, expected) in [
        (completion::Field::Zone, "berlin", "Europe/Berlin"),
        (
            completion::Field::Location,
            "meeting-room",
            "https://example.com/meeting-room",
        ),
        (
            completion::Field::Repeat,
            "weekday",
            "RRULE:FREQ=WEEKLY;BYDAY=MO,TU,WE,TH,FR",
        ),
        (completion::Field::Reminders, "10", "popup:10"),
        (completion::Field::Reminders, "No reminders", ""),
        (completion::Field::Duration, "45", "45"),
    ] {
        let ctx = field.context(text, text.len()).unwrap();
        let choices = field.offer(s.store(), &ctx);
        assert!(
            choices.iter().any(|v| v.value == expected),
            "{field:?}: {choices:?}"
        );
    }
    let field = completion::Field::Repeat;
    let choices = field.offer(s.store(), &field.context("RRULE:FREQ=WEEKLY", 17).unwrap());
    assert_eq!(choices[0].value, "RRULE:FREQ=WEEKLY");
    assert!(choices.iter().all(|v| v.describe.is_empty()));
}

#[test]
fn edited_search_settings_do_not_change_pending_queries_and_rechecks_use_current_guests() {
    let mut s = session();
    let (draft, mut f) = form(&mut s);
    f.guests = "nora@studio.example".into();
    edit::save(&mut s, draft, 1, 1, f.clone()).unwrap();
    let day = &f.start[..10];
    let q = availability::Query {
        start: format!("{day}T09:00"),
        end: format!("{day}T18:00"),
        zone: f.zone.clone(),
        minutes: 30,
        guests: availability::guests(&f),
        draft_guests: None,
    };
    let request = availability::request(&mut s, 1, q, Some(draft)).unwrap();
    let slot = open(&mut s, panels::Availability::id(request));
    let panel = s.panel(slot).unwrap();
    {
        let mut panel = panel.borrow_mut();
        let p = panel
            .as_any()
            .downcast_mut::<panels::Availability>()
            .unwrap();
        let mut search = p.search.clone();
        search.minutes = "oops".into();
        p.edit_search(search);
        p.check(&mut s);
        assert_eq!(p.request, request);
        assert!(!p.error.is_empty());
    }
    refresh(&s);
    let (old, result, _, _) = availability::load(s.store(), request).unwrap();
    assert_eq!(old.minutes, 30);
    assert!(result.is_some());
    let d = edit::draft(s.store(), draft).unwrap();
    let mut form = d.form;
    form.guests = "leo@studio.example".into();
    edit::save(&mut s, draft, d.revision, 1, form).unwrap();
    assert!(
        availability::draft_error(s.store(), request, &old, Some(draft))
            .unwrap()
            .contains("guest list changed")
    );
    {
        let mut panel = panel.borrow_mut();
        let p = panel
            .as_any()
            .downcast_mut::<panels::Availability>()
            .unwrap();
        p.search.minutes = "45".into();
        p.check(&mut s);
        assert_ne!(p.request, request);
        assert!(!p.dirty);
        assert!(p.error.is_empty());
        let (new, _, _, _) = availability::load(s.store(), p.request).unwrap();
        assert_eq!(new.minutes, 45);
        assert!(new.guests.contains(&"leo@studio.example".into()));
        assert!(!new.guests.contains(&"nora@studio.example".into()));
    }
    assert_eq!(
        availability::load(s.store(), request).unwrap().0.minutes,
        30
    );
}

#[test]
fn availability_tracks_removed_guests_and_account_changes_but_allows_title_edits() {
    let mut s = session();
    let (draft, mut f) = form(&mut s);
    f.guests = "nora@studio.example".into();
    edit::save(&mut s, draft, 1, 1, f.clone()).unwrap();
    let search = availability::Search {
        day: f.start[..10].into(),
        end_day: f.start[..10].into(),
        from: "09:00".into(),
        until: "17:00".into(),
        minutes: "30".into(),
        zone: f.zone.clone(),
    };
    // Draft guests must be checked even if a tool only supplies extra calendars.
    let request =
        availability::request(&mut s, 1, search.query(vec![]).unwrap(), Some(draft)).unwrap();
    refresh(&s);
    let (q, result, _, _) = availability::load(s.store(), request).unwrap();
    assert!(q.guests.contains(&"nora@studio.example".into()));
    assert!(!result.unwrap().slots.is_empty());
    let d = edit::draft(s.store(), draft).unwrap();
    f.title = "Renamed review".into();
    edit::save(&mut s, draft, d.revision, 1, f.clone()).unwrap();
    assert!(availability::draft_error(s.store(), request, &q, Some(draft)).is_none());
    f.guests.clear();
    let d = edit::draft(s.store(), draft).unwrap();
    edit::save(&mut s, draft, d.revision, 1, f.clone()).unwrap();
    assert!(use_suggestion(&mut s, request, 0)
        .unwrap_err()
        .contains("guest list changed"));
    let new = availability::recheck(&mut s, request, &search).unwrap();
    let (q, _, _, _) = availability::load(s.store(), new).unwrap();
    assert!(!q.guests.contains(&"nora@studio.example".into()));
    let source = s.store()
        .write(|c| {
            let account = crate::identity::accounts::add_account_tx(c, "other@example.com", "", "", "google")?;
            crate::identity::set_services(c, account, false, true)?;
            c.execute("INSERT INTO calendar_source(account,remote,title,role) VALUES(?,'other@example.com','Other','owner')", [account])?;
            Ok(c.last_insert_rowid())
        })
        .unwrap();
    let d = edit::draft(s.store(), draft).unwrap();
    edit::save(&mut s, draft, d.revision, source, f).unwrap();
    assert!(availability::draft_error(s.store(), new, &q, Some(draft))
        .unwrap()
        .contains("account changed"));
}

#[test]
fn choosing_a_time_updates_the_original_editor_and_closes_the_scheduling_sheet() {
    let mut s = session();
    let (draft, _) = form(&mut s);
    let editor = open(&mut s, panels::Editor::id(draft));
    s.panel(editor)
        .unwrap()
        .borrow_mut()
        .run("calendar.find", &mut s);
    s.settle();
    refresh(&s);
    let slot = s.focus().unwrap();
    assert_ne!(slot, editor);
    assert_eq!(s.join_parent_of(slot), Some(editor));
    let panel = s.panel(slot).unwrap();
    {
        let mut panel = panel.borrow_mut();
        let p = panel
            .as_any()
            .downcast_mut::<panels::Availability>()
            .unwrap();
        let (_, r, _, _) = availability::load(s.store(), p.request).unwrap();
        let start = r.unwrap().slots[0].0;
        p.select(start, s.now()).unwrap();
        crate::shell::bar::check(&p.verbs());
        p.run("calendar.use_time", &mut s);
    }
    s.settle();
    assert_eq!(s.focus(), Some(editor));
    assert!(s.panel(slot).is_none());
    assert_eq!(
        s.panel(editor).unwrap().borrow().persist(),
        panels::Editor::id(draft)
    );
    assert_eq!(edit::draft(s.store(), draft).unwrap().revision, 2);
}

#[test]
fn unavailable_calendars_offer_no_unchecked_times_and_tracks_clip_to_the_window() {
    let q = availability::Query {
        start: "2026-09-08T09:00".into(),
        end: "2026-09-08T17:00".into(),
        zone: "Europe/Berlin".into(),
        minutes: 30,
        guests: vec!["private@example.com".into()],
        draft_guests: None,
    };
    let r = availability::calculate(&q, &json!({"calendars":{}}), 0.0).unwrap();
    assert!(!r.complete);
    assert!(r.slots.is_empty());
    assert_eq!(
        availability_ui::fraction(8.0, 10.0, 9.0, 17.0),
        Some((0.0, 0.125))
    );
    assert_eq!(
        availability_ui::fraction(16.0, 19.0, 9.0, 17.0),
        Some((0.875, 1.0))
    );
    assert_eq!(availability_ui::fraction(6.0, 8.0, 9.0, 17.0), None);
    assert_eq!(availability_ui::fraction(12.0, 11.0, 9.0, 17.0), None);
    let mut search = availability::Search::from_query(&q);
    search.shift(1).unwrap();
    let moved = search.query(q.guests).unwrap();
    assert_eq!(moved.start, "2026-09-09T09:00");
    search.minutes = "480".into();
    search.until = "10:00".into();
    assert!(search.query(vec![]).is_err());
    let repeated = availability::Query {
        start: "2026-10-25T02:15:00+02:00".into(),
        end: "2026-10-25T02:15:00+01:00".into(),
        zone: "Europe/Berlin".into(),
        minutes: 30,
        guests: vec![],
        draft_guests: None,
    };
    let restored = availability::Search::from_query(&repeated)
        .query(vec![])
        .unwrap();
    assert_eq!(restored.validate().unwrap(), repeated.validate().unwrap());
}

#[cfg(headless)]
#[test]
fn scheduling_templates_load_without_a_window_or_event_loop() {
    use makepad_widgets::*;
    let mut cx = Cx::new(Box::new(|_, _| {}));
    let (editor, availability, track, month, timeline) = cx.with_vm(|vm| {
        makepad_widgets::script_mod(vm);
        crate::shell::script_mod(vm);
        crate::reader::ui::script_mod(vm);
        super::ui::script_mod(vm);
        let editor = script_eval!(vm,{mod.widgets.CalendarEditorPanel{}});
        let availability = script_eval!(vm,{mod.widgets.CalendarAvailabilityPanel{}});
        let track = script_eval!(vm,{mod.widgets.CalendarTimeTrack{}});
        let month = script_eval!(vm,{mod.widgets.CalendarMonthPanel{}});
        let timeline = script_eval!(vm,{mod.widgets.CalendarTimelinePanel{}});
        (
            WidgetRef::script_from_value(vm, editor),
            WidgetRef::script_from_value(vm, availability),
            WidgetRef::script_from_value(vm, track),
            WidgetRef::script_from_value(vm, month),
            WidgetRef::script_from_value(vm, timeline),
        )
    });
    assert!(editor
        .borrow::<super::widgets::CalendarEditorPanel>()
        .is_some());
    assert!(availability
        .borrow::<super::availability_ui::CalendarAvailabilityPanel>()
        .is_some());
    assert!(track
        .borrow::<super::availability_ui::CalendarTimeTrack>()
        .is_some());
    assert!(month
        .borrow::<super::widgets::CalendarMonthPanel>()
        .is_some());
    assert!(timeline
        .borrow::<super::widgets::CalendarTimelinePanel>()
        .is_some());
}

struct FreeBusyAccounts {
    replies: std::collections::HashMap<String, Result<Value, String>>,
    calls: std::sync::Arc<std::sync::Mutex<Vec<api::Request>>>,
}
#[async_trait::async_trait(?Send)]
impl api::Api for FreeBusyAccounts {
    async fn call(&mut self, request: &api::Request) -> Result<Value, String> {
        use kernel::effect::AsyncEffect;
        assert!(!request.writes());
        if request.path != "/freeBusy" {
            return Err("Keep the cached calendar catalog for this availability test".into());
        }
        assert_eq!(request.method, "POST");
        self.calls.lock().unwrap().push(request.clone());
        self.replies
            .get(&request.email)
            .expect("unexpected account")
            .clone()
    }
}
fn availability_account(s: &Session, email: &str, role: &str, enabled: bool) -> i64 {
    let (email, role) = (email.to_owned(), role.to_owned());
    s.store()
        .write(move |c| {
            let id = crate::identity::accounts::add_account_tx(c, &email, "", "", "google")?;
            crate::identity::set_services(c, id, false, enabled)?;
            c.execute(
                "INSERT INTO calendar_source(account,remote,title,role) VALUES(?1,?2,?2,?3)",
                rusqlite::params![id, email, role],
            )?;
            Ok(id)
        })
        .unwrap()
}

#[test]
fn availability_retries_guests_with_connected_accounts_and_preserves_successful_checks() {
    let mut s = session();
    let personal = model::source(s.store(), 1).unwrap().email;
    let work = "me@work.example";
    let other = "me@other.example";
    let (draft, mut f) = form(&mut s);
    f.guests = "colleague@work.example, teammate@work.example".into();
    edit::save(&mut s, draft, 1, 1, f.clone()).unwrap();
    // Install these catalogs after draft actions have kicked the default fake
    // worker; only the account-specific availability fixture should see them.
    // Access to colleagues does not require an owned calendar on the account.
    availability_account(&s, work, "reader", true);
    availability_account(&s, other, "owner", true);
    availability_account(&s, "disabled@example.com", "owner", false);
    let day = dates::day(s.now() + 86400.0, "UTC");
    let q = availability::Query {
        start: format!("{day}T09:00"),
        end: format!("{day}T17:00"),
        zone: "UTC".into(),
        minutes: 30,
        guests: vec![],
        draft_guests: None,
    };
    let busy = |a: &str, b: &str| json!({"busy":[{"start":format!("{day}T{a}:00Z"),"end":format!("{day}T{b}:00Z")}]});
    let denied = json!({"errors":[{"reason":"notFound"}]});
    let calls = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
    s.world().caps(|caps| caps.insert::<dyn api::Api>(Box::new(FreeBusyAccounts {
        replies: [
            (personal.clone(),Ok(json!({"calendars":{personal.clone():busy("09:00","10:00"),"colleague@work.example":denied,"teammate@work.example":denied,other:denied}}))),
            (work.into(),Ok(json!({"calendars":{personal.clone():denied,"colleague@work.example":busy("10:00","11:00"),"TEAMMATE@work.example":{"busy":[]},other:denied}}))),
            // An unsolicited failure for an already-readable calendar must not
            // overwrite the successful result from its previous account.
            (other.into(),Ok(json!({"calendars":{personal.clone():denied,"colleague@work.example":denied,"teammate@work.example":denied,other:busy("12:00","13:00")}}))),
        ].into_iter().collect(), calls:calls.clone(),
    })));
    let request = availability::request(&mut s, 1, q, Some(draft)).unwrap();
    kernel::runtime::block_on(sync::Sync.pass(s.world()));
    let (q, result, error, _) = availability::load(s.store(), request).unwrap();
    assert!(error.is_empty(), "{error}");
    let result = result.unwrap();
    assert!(
        result.complete,
        "{:#?}; calls: {:#?}",
        result.people,
        calls
            .lock()
            .unwrap()
            .iter()
            .map(|r| (&r.email, &r.body))
            .collect::<Vec<_>>()
    );
    assert_eq!(result.people.len(), 4);
    let colleague = result
        .people
        .iter()
        .find(|p| p.calendar == "colleague@work.example")
        .unwrap();
    assert_eq!(colleague.via(), Some(work));
    assert_eq!(colleague.checks.len(), 2);
    assert!(colleague.checks[0].error.contains("notFound"));
    let personal_result = result
        .people
        .iter()
        .find(|p| p.calendar == personal)
        .unwrap();
    assert_eq!(personal_result.checks.len(), 1);
    assert_eq!(personal_result.via(), Some(personal.as_str()));
    assert_eq!(
        result.slots[0].0,
        dates::instant(&format!("{day}T11:00"), "UTC").unwrap()
    );
    for (start, end) in &result.slots {
        assert!(result
            .people
            .iter()
            .all(|p| p.busy.iter().all(|(a, b)| end <= a || start >= b)));
    }
    let calls = calls.lock().unwrap();
    assert_eq!(
        calls.iter().map(|r| r.email.as_str()).collect::<Vec<_>>(),
        vec![personal.as_str(), work, other]
    );
    assert!(!calls[1].body["items"]
        .as_array()
        .unwrap()
        .iter()
        .any(|item| item["id"] == personal));
    assert_eq!(calls[2].body["items"], json!([{"id":other}]));
    assert!(!q.guests.contains(&"disabled@example.com".to_string()));
    let saved = edit::draft(s.store(), draft).unwrap();
    assert_eq!(saved.source, 1);
    assert_eq!(saved.form.guests, f.guests);
    let reply = run(
        &mut s,
        "calendar.availability_result",
        json!({"request":request}),
    )
    .unwrap();
    assert!(reply["result"]["people"]
        .as_array()
        .unwrap()
        .iter()
        .any(|p| p["checks"]
            .as_array()
            .unwrap()
            .iter()
            .any(|check| check["account"] == work && check["error"] == "")));
}

#[test]
fn availability_account_failures_do_not_block_other_connected_accounts() {
    let mut s = session();
    let personal = model::source(s.store(), 1).unwrap().email;
    let work = "me@work.example";
    availability_account(&s, work, "owner", true);
    let day = dates::day(s.now() + 86400.0, "UTC");
    let q = availability::Query {
        start: format!("{day}T09:00"),
        end: format!("{day}T17:00"),
        zone: "UTC".into(),
        minutes: 30,
        guests: vec!["colleague@work.example".into()],
        draft_guests: None,
    };
    s.world().caps(|caps| caps.insert::<dyn api::Api>(Box::new(FreeBusyAccounts {
        replies:[
            (personal.clone(),Err("HTTP 401: reconnect this Google account in Accounts".into())),
            (work.into(),Ok(json!({"calendars":{work:{"busy":[]},"colleague@work.example":{"busy":[]},personal.clone():{"errors":[{"reason":"notFound"}]}}}))),
        ].into_iter().collect(),calls:Default::default(),
    })));
    let request = availability::request(&mut s, 1, q, None).unwrap();
    kernel::runtime::block_on(sync::Sync.pass(s.world()));
    let (_, result, error, _) = availability::load(s.store(), request).unwrap();
    assert!(error.is_empty());
    let result = result.unwrap();
    assert!(!result.complete);
    assert!(!result.slots.is_empty());
    assert_eq!(result.people.iter().filter(|p| p.known).count(), 2);
    let unknown = result.people.iter().find(|p| !p.known).unwrap();
    assert_eq!(unknown.calendar, personal);
    assert!(unknown.failure().contains("HTTP 401"));
    assert!(unknown.failure().contains("notFound"));
    assert!(unknown.failure().contains(work));
    assert_eq!(unknown.via(), None);
}

#[test]
fn availability_retains_google_error_reasons_and_reads_older_results() {
    let q = availability::Query {
        start: "2026-09-09T09:00".into(),
        end: "2026-09-09T17:00".into(),
        zone: "UTC".into(),
        minutes: 30,
        guests: vec!["guest@example.com".into()],
        draft_guests: None,
    };
    for reason in ["notFound", "forbidden", "internalError", "futureError"] {
        let r = availability::calculate(
            &q,
            &json!({"calendars":{"guest@example.com":{"errors":[{"reason":reason}],"busy":[]}}}),
            0.0,
        )
        .unwrap();
        assert!(!r.complete);
        assert!(r.slots.is_empty());
        assert!(r.people[0].error.contains(reason));
    }
    let r = availability::calculate(
        &q,
        &json!({"calendars":{"guest@example.com":{"errors":"invalid","busy":[]}}}),
        0.0,
    )
    .unwrap();
    assert!(!r.people[0].known);
    assert!(r.people[0].error.contains("invalid availability"));
    let old: availability::ResultSet = serde_json::from_value(json!({"people":[{"calendar":"guest@example.com","known":true,"busy":[],"error":""}],"slots":[],"complete":true,"checked":0.0})).unwrap();
    assert!(old.people[0].known);
    assert!(old.people[0].checks.is_empty());
}

#[test]
fn guest_suggestions_arrive_without_typing_again_in_calendar_only_and_full_stores() {
    use kernel::richtable::Completion;
    use std::{sync::mpsc,time::Duration};
    static CALENDAR_ONLY: &[&dyn App] = &[&crate::apps::accounts::ACCOUNTS,&CALENDAR];
    for apps in [CALENDAR_ONLY,APPS] {
        let mut s = Session::fake(apps);
        refresh(&s);
        let field = completion::Field::Guests;
        let context = field.context("nor",3).unwrap();
        let expected = field.offer(s.store(),&context);
        let (notify,woke) = mpsc::channel();
        s.store().attach_ui(move || {let _ = notify.send(());});
        assert!(field.offer(s.store(),&context).is_empty());
        let before = s.store().display_revision();
        let deadline = std::time::Instant::now()+Duration::from_secs(5);
        let answer = loop {
            assert!(std::time::Instant::now()<deadline,"guest snapshots completed");
            let answer = field.offer(s.store(),&context);
            if !s.store().queries_pending() && !answer.is_empty() {break answer;}
            woke.recv_timeout(Duration::from_secs(5)).unwrap();
            s.store().poll_external();
        };
        assert_ne!(before,s.store().display_revision(),"unchanged caret observes completion");
        assert_eq!(answer.iter().map(|s|(&s.label,&s.value)).collect::<Vec<_>>(),expected.iter().map(|s|(&s.label,&s.value)).collect::<Vec<_>>());
        assert!(answer.len()<=kernel::richtable::MAX_SUGGESTIONS);
        s.shutdown();
    }
}

#[test]
fn month_snapshots_wait_for_timezone_and_keep_boundary_counts_with_three_display_rows() {
    use std::{sync::mpsc,time::Duration};
    let mut s = paused_session();
    s.store().write(|tx| {
        for (id,start,end) in [("bridge","2030-02-28","2030-03-02"),("ends-before","2030-02-28","2030-03-01")] {
            model::ingest(tx,1,"Europe/Berlin",&json!({"id":id,"summary":id,"start":{"date":start},"end":{"date":end}}))?;
        }
        for n in 0..10 {
            model::ingest(tx,1,"Europe/Berlin",&json!({"id":format!("dense-{n}"),"summary":format!("dense-{n}"),"start":{"date":"2030-03-01"},"end":{"date":"2030-03-02"}}))?;
        }
        Ok(())
    }).unwrap();
    let (notify,woke) = mpsc::channel();
    s.store().attach_ui(move || {let _ = notify.send(());});
    let requested = panels::Month::id("2030-03-01","");
    let slot = open(&mut s,requested.clone());
    let instance = s.panel(slot).unwrap();
    let mut borrow = instance.borrow_mut();
    let panel = borrow.as_any().downcast_mut::<panels::Month>().unwrap();
    assert!(!panel.ready(),"a cold source snapshot never implies UTC");
    assert_eq!(panel.persist(),requested,"loading does not overwrite restored state");
    let deadline = std::time::Instant::now()+Duration::from_secs(5);
    let grid = loop {
        assert!(std::time::Instant::now()<deadline,"month preparation completed");
        panel.cover(&mut s);
        match panel.reading() {
            snapshot::State::Ready(grid)=>break grid,
            snapshot::State::Loading | snapshot::State::Refreshing(_)=>{
                woke.recv_timeout(Duration::from_secs(5)).unwrap();
                s.store().poll_external();
            }
            snapshot::State::Failed(error)=>panic!("{error}"),
        }
    };
    assert_eq!(panel.zone,"Europe/Berlin");
    assert_eq!(grid.count,12);
    assert_eq!(grid.days.len(),42);
    let dates = dates::grid("2030-03-01");
    let day = |date: &str| dates.iter().position(|day|day.to_string()==date).unwrap();
    assert_eq!(grid.days[day("2030-02-28")].count,2);
    assert_eq!(grid.days[day("2030-03-01")].count,11,"an exclusive midnight end never spills into the next day");
    assert_eq!(grid.days[day("2030-03-01")].lines.len(),3);
    assert_eq!(grid.days[day("2030-03-02")].count,0);
    assert!(grid.days.iter().all(|day|day.lines.len()<=3));
    panel.filter = "bridge".into();
    assert!(matches!(panel.reading(),snapshot::State::Loading),"changed filters never show stale month entries");
    let filtered = loop {
        match panel.reading() {
            snapshot::State::Ready(grid)=>break grid,
            snapshot::State::Loading | snapshot::State::Refreshing(_)=>{
                woke.recv_timeout(Duration::from_secs(5)).unwrap();
                s.store().poll_external();
            }
            snapshot::State::Failed(error)=>panic!("{error}"),
        }
    };
    assert_eq!(filtered.count,1);
    assert_eq!(filtered.days[day("2030-03-01")].count,1);
    drop(borrow);
    s.shutdown();
}
