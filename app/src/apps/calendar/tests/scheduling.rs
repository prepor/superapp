use super::*;

fn request(s: &mut Session) -> (i64, i64, availability::Query) {
    let (draft, f) = form(s);
    let day = dates::day(s.now() + 86400.0, &f.zone);
    let q = availability::Query {
        start: format!("{day}T09:00"),
        end: format!("{day}T17:00"),
        zone: f.zone,
        minutes: 30,
        guests: vec!["nora@studio.example".into()],
        draft_guests: None,
    };
    let id = availability::request(s, 1, q, Some(draft)).unwrap();
    (id, draft, availability::load(s.store(), id).unwrap().0)
}

#[test]
fn availability_recheck_completes_with_background_preparation_and_ui_reads() {
    use std::{sync::mpsc, time::{Duration, Instant}};

    let mut initial = paused_session();
    let (id, _, query) = request(&mut initial);
    refresh(&initial);
    let env = kernel::app::Env {
        clock: kernel::caps::ClockSource::virtual_from(initial.now()),
        ..Default::default()
    };
    let store = initial.store().clone();
    let world = kernel::app::world_for(
        APPS, kernel::store::Store::with_db(store.db()).unwrap(), Mode::Fake, &env,
    );
    let mut s = Session::new(
        kernel::app::Apps::new(APPS), std::rc::Rc::new(world),
        kernel::app::Workers::none(store),
    );
    let (notify, woke) = mpsc::channel();
    s.store().attach_ui(move || { let _ = notify.send(()); });
    let slot = open(&mut s, panels::Availability::id(id));
    let instance = s.panel(slot).unwrap();
    let wait = |s: &mut Session| {
        woke.recv_timeout(Duration::from_secs(5)).expect("availability completion wakes the UI");
        s.store().poll_external();
        s.settle();
    };
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        assert!(Instant::now() < deadline, "initial availability loads");
        let ready = instance.borrow_mut().as_any().downcast_mut::<panels::Availability>()
            .unwrap().preview(s.now()).is_ok();
        if ready { break; }
        wait(&mut s);
    }
    let mut search = availability::Search::from_query(&query);
    search.shift(1).unwrap();
    {
        let mut panel = instance.borrow_mut();
        let panel = panel.as_any().downcast_mut::<panels::Availability>().unwrap();
        panel.edit_search(search.clone());
        panel.run("calendar.check", &mut s);
        assert_eq!(panel.request, id, "the request is still preparing");
    }
    let deadline = Instant::now() + Duration::from_secs(5);
    let checked = loop {
        assert!(Instant::now() < deadline, "the accepted check reaches its panel");
        wait(&mut s);
        let mut panel = instance.borrow_mut();
        let panel = panel.as_any().downcast_mut::<panels::Availability>().unwrap();
        if panel.request != id {
            assert!(!panel.dirty);
            assert!(panel.error.is_empty(), "{}", panel.error);
            // Editing before the new request's display snapshot is ready must
            // not leave a temporary loading state as a permanent form error.
            search.minutes = "60".into();
            panel.edit_search(search.clone());
            break panel.request;
        }
    };
    refresh(&s);
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        assert!(Instant::now() < deadline, "the new availability result reaches its panel");
        let preview = {
            let mut panel = instance.borrow_mut();
            let panel = panel.as_any().downcast_mut::<panels::Availability>().unwrap();
            assert_eq!(panel.request, checked);
            panel.preview(s.now())
        };
        if let Ok(preview) = preview {
            assert_eq!(availability::Search::from_query(&preview.query), search);
            assert!(!preview.result.slots.is_empty());
            let mut panel = instance.borrow_mut();
            let panel = panel.as_any().downcast_mut::<panels::Availability>().unwrap();
            assert!(!panel.dirty, "valid controls recover when their request finishes loading");
            assert!(panel.error.is_empty(), "{}", panel.error);
            break;
        }
        wait(&mut s);
    }
    s.shutdown();
}

#[test]
fn availability_recheck_displays_the_new_results_with_ui_reads() {
    let mut s = paused_session();
    let (id, draft, _) = request(&mut s);
    refresh(&s);
    let editor = open(&mut s, panels::Editor::id(draft));
    let sheet = open(&mut s, panels::Availability::id(id));
    availability_ui::test_input::recheck(&mut s, editor, sheet);
    s.shutdown();
}

#[test]
fn availability_recheck_revalidates_controls_edited_before_commit() {
    for change_date in [false, true] {
        let mut s = paused_session();
        let (id, _, _) = request(&mut s);
        let slot = open(&mut s, panels::Availability::id(id));
        let instance = s.panel(slot).unwrap();
        s.store().attach_ui(|| {});
        let edited = {
            let mut panel = instance.borrow_mut();
            let panel = panel.as_any().downcast_mut::<panels::Availability>().unwrap();
            panel.check(&mut s);
            let mut search = panel.search.clone();
            search.minutes = "60".into();
            if change_date { search.shift(1).unwrap(); }
            panel.edit_search(search.clone());
            assert_eq!(panel.request, id, "the UI has not consumed the commit yet");
            search
        };
        s.shutdown();
        let mut panel = instance.borrow_mut();
        let panel = panel.as_any().downcast_mut::<panels::Availability>().unwrap();
        assert_ne!(panel.request, id);
        assert_eq!(panel.search, edited, "completion preserves newer controls");
        assert_eq!(panel.dirty, change_date, "only a changed search window needs another check");
        assert_eq!(panel.error.is_empty(), !change_date);
        assert_eq!(availability::load(s.store(), panel.request).unwrap().0.minutes, 30,
            "duration edits do not mutate the accepted request");
    }
}

#[test]
fn changing_duration_reuses_busy_intervals_and_applies_the_new_length() {
    let mut s = paused_session();
    let (id, draft, q) = request(&mut s);
    refresh(&s);
    let before = availability::load(s.store(), id).unwrap().1.unwrap();
    let start = dates::instant(&format!("{}T09:30", &q.start[..10]), &q.zone).unwrap();
    assert!(before.slots.iter().any(|(a, _)| *a == start));
    let slot = open(&mut s, panels::Availability::id(id));
    let instance = s.panel(slot).unwrap();
    let mut borrow = instance.borrow_mut();
    let p = borrow
        .as_any()
        .downcast_mut::<panels::Availability>()
        .unwrap();
    p.select(start, s.now()).unwrap();
    let mut search = p.search.clone();
    search.minutes = "60".into();
    p.edit_search(search.clone());
    assert!(!p.dirty);
    assert_eq!(p.selected, Some(start));
    let (effective, result, _) = availability::preview(s.store(), id, &search, s.now()).unwrap();
    assert_eq!(effective.minutes, 60);
    assert_eq!(result.checked, before.checked);
    assert!(result.slots.iter().all(|(a, b)| b - a == 3600.0));
    assert!(
        !result.slots.iter().any(|(a, _)| *a == start),
        "the longer time now overlaps Planning"
    );
    assert_eq!(availability::load(s.store(), id).unwrap().0.minutes, 30);
    assert_eq!(
        s.store()
            .conn()
            .query_row("SELECT COUNT(*) FROM calendar_availability", [], |r| r
                .get::<_, i64>(0))
            .unwrap(),
        1
    );
    // Changing duration and selecting a time are local UI state until applied.
    let revision = edit::draft(s.store(), draft).unwrap().revision;
    availability::apply_time(&mut s, id, &search, start).unwrap();
    let saved = edit::draft(s.store(), draft).unwrap();
    assert_eq!(saved.revision, revision + 1);
    assert_eq!(saved.form.bounds().unwrap(), (start, start + 3600.0));
    assert_eq!(
        s.store()
            .conn()
            .query_row("SELECT COUNT(*) FROM calendar_change", [], |r| r
                .get::<_, i64>(0))
            .unwrap(),
        0
    );
    drop(borrow);
    s.save();
    let mut restored = Session::new(
        kernel::app::Apps::new(APPS),
        s.world().clone(),
        kernel::app::Workers::none(s.store().clone()),
    );
    assert!(restored.restore());
    let instance = restored.panel(slot).unwrap();
    let mut borrow = instance.borrow_mut();
    let p = borrow
        .as_any()
        .downcast_mut::<panels::Availability>()
        .unwrap();
    assert_eq!(p.search.minutes, "60");
    assert!(!p.dirty);
}

#[test]
fn incomplete_duration_edits_recover_without_renewing_stale_availability() {
    let mut s = paused_session();
    let (id, draft, _) = request(&mut s);
    refresh(&s);
    let slot = open(&mut s, panels::Availability::id(id));
    let instance = s.panel(slot).unwrap();
    let mut borrow = instance.borrow_mut();
    let p = borrow
        .as_any()
        .downcast_mut::<panels::Availability>()
        .unwrap();
    let start = availability::load(s.store(), id).unwrap().1.unwrap().slots[0].0;
    p.select(start, s.now()).unwrap();
    let mut search = p.search.clone();
    for invalid in ["", "6", "oops", "481"] {
        search.minutes = invalid.into();
        p.edit_search(search.clone());
        assert!(p.dirty);
        assert!(availability::apply_time(&mut s, id, &search, start).is_err());
        assert_eq!(p.selected, Some(start));
    }
    search.minutes = "45".into();
    p.edit_search(search.clone());
    assert!(!p.dirty);
    assert!(p.error.is_empty());
    let (_, r, _, _) = availability::load(s.store(), id).unwrap();
    let mut old = r.unwrap();
    old.checked -= 301.0;
    s.store()
        .write(move |c| {
            c.execute(
                "UPDATE calendar_availability SET response=? WHERE id=?",
                rusqlite::params![serde_json::to_string(&old).unwrap(), id],
            )
        })
        .unwrap();
    let revision = edit::draft(s.store(), draft).unwrap().revision;
    assert!(availability::apply_time(&mut s, id, &search, start)
        .unwrap_err()
        .contains("out of date"));
    assert_eq!(edit::draft(s.store(), draft).unwrap().revision, revision);
    search.shift(1).unwrap();
    p.edit_search(search);
    assert!(p.dirty, "unchecked dates cannot reuse the old range");
}

#[test]
fn dragging_keeps_the_grab_point_snaps_and_clamps_the_full_duration() {
    use makepad_widgets::{dvec2, Rect};
    let mut q = availability::Query {
        start: "2026-09-09T09:00".into(),
        end: "2026-09-09T17:00".into(),
        zone: "Europe/Berlin".into(),
        minutes: 30,
        guests: vec![],
        draft_guests: None,
    };
    let (a, b) = q.validate().unwrap();
    let rect = Rect {
        pos: dvec2(40.0, 80.0),
        size: dvec2(480.0, 34.0),
    };
    let proposed = (a + 3600.0, a + 5400.0);
    let drag = availability_ui::Drag::new(rect, a, b, Some(proposed), 110.0).unwrap();
    assert_eq!(
        drag.at(110.0),
        proposed.0,
        "grabbing the middle must not jump"
    );
    assert_eq!(
        availability::snap(&q, drag.at(138.0), 0.0),
        Some(a + 5400.0)
    );
    assert_eq!(availability::snap(&q, drag.at(-1000.0), 0.0), Some(a));
    assert_eq!(
        availability::snap(&q, drag.at(2000.0), 0.0),
        Some(b - 1800.0)
    );
    q.minutes = 90;
    assert_eq!(
        availability::snap(&q, drag.at(2000.0), 0.0),
        Some(b - 5400.0)
    );
    let click = availability_ui::Drag::new(rect, a, b, Some(proposed), 220.0).unwrap();
    assert_eq!(
        availability::snap(&q, click.at(220.0), 0.0),
        Some(a + 10800.0)
    );
    assert_eq!(availability::snap(&q, a, a + 60.0), Some(a + 900.0));
    assert!(availability::snap(&q, a, b - 60.0).is_none());
    assert!(availability::snap(&q, f64::NAN, 0.0).is_none());

    q.start = "2026-10-25T00:00".into();
    q.end = "2026-10-25T06:00".into();
    let repeated = dates::instant("2026-10-25T02:53:00+02:00", &q.zone).unwrap();
    let selected = availability::snap(&q, repeated, 0.0).unwrap();
    assert_eq!(
        selected,
        dates::instant("2026-10-25T02:00:00+01:00", &q.zone).unwrap()
    );
    assert_eq!(
        dates::editor_time(selected, &q.zone),
        "2026-10-25T02:00:00+01:00"
    );
    assert_eq!(
        availability::selection(&q, selected, 0.0).unwrap().1 - selected,
        5400.0
    );
}

#[test]
fn hover_shows_the_exact_event_at_boundaries_and_keeps_private_events_busy() {
    let q = availability::Query {
        start: "2026-09-09T09:00".into(),
        end: "2026-09-09T17:00".into(),
        zone: "Europe/Berlin".into(),
        minutes: 30,
        guests: vec!["nora@example.com".into()],
        draft_guests: None,
    };
    let (a, _) = q.validate().unwrap();
    let mut person = availability::calculate(&q, &json!({"calendars":{"nora@example.com":{"busy":[{"start":dates::rfc(a),"end":dates::rfc(a+7200.0)}]}}}), 0.0).unwrap().people.remove(0);
    let raw = |id: &str, start, end| json!({"id":id,"start":{"dateTime":dates::rfc(start)},"end":{"dateTime":dates::rfc(end)}});
    let mut first = raw("first", a, a + 3600.0);
    first["summary"] = json!("Roadmap review");
    first["location"] = json!("Room 4");
    person.details.state = availability::DetailState::Ready;
    person.details.events = vec![
        availability_details::event(&first, &q.zone, "work@example.com").unwrap(),
        availability_details::event(
            &raw("private", a + 3600.0, a + 7200.0),
            &q.zone,
            "work@example.com",
        )
        .unwrap(),
    ];
    let text = availability_ui::hover_text(&person, a + 3599.0, &q.zone).unwrap();
    assert!(text.contains("Roadmap review") && text.contains("Room 4"));
    assert!(text.contains("09:00") && text.contains("10:00"));
    let private = availability_ui::hover_text(&person, a + 3600.0, &q.zone).unwrap();
    assert!(private.contains("Busy") && !private.contains("Roadmap review"));
    assert!(availability_ui::hover_text(&person, a + 7200.0, &q.zone).is_none());
    first["transparency"] = json!("transparent");
    assert!(availability_details::event(&first, &q.zone, "work@example.com").is_none());
    first["transparency"] = json!("opaque");
    first["attendees"] = json!([{"self":true,"responseStatus":"declined"}]);
    assert!(availability_details::event(&first, &q.zone, "work@example.com").is_none());
    for (zone, day) in MIDNIGHT_GAPS {
        let next = dates::date(day).unwrap().succ_opt().unwrap();
        let raw = json!({"id":"all-day","summary":"Day off","start":{"date":day},"end":{"date":next.to_string()}});
        let event = availability_details::event(&raw, zone, "work@example.com").unwrap();
        assert!(event.all_day);
        assert_eq!(event.end - event.start, 23.0 * 3600.0);
    }
}

struct DetailReplies {
    fake: api::Fake,
    replies: std::collections::HashMap<(String, String), Value>,
    calls: std::sync::Arc<std::sync::Mutex<Vec<api::Request>>>,
}
#[async_trait::async_trait(?Send)]
impl api::Api for DetailReplies {
    async fn call(&mut self, r: &api::Request) -> Result<Value, String> {
        if !r.query.iter().any(|(key, _)| key == "fields") {
            return self.fake.call(r).await;
        }
        use kernel::effect::AsyncEffect;
        assert!(!r.writes());
        self.calls.lock().unwrap().push(r.clone());
        if r.path != api::events("nora@studio.example") {
            return Err("HTTP 403: details not shared".into());
        }
        let token = r
            .query
            .iter()
            .find(|(key, _)| key == "pageToken")
            .map(|(_, v)| v.clone())
            .unwrap_or_default();
        self.replies
            .get(&(r.email.clone(), token))
            .cloned()
            .ok_or("HTTP 403: details not shared".into())
    }
}

#[test]
fn details_load_after_freebusy_with_pagination_and_connected_account_fallback() {
    for shared in [true, false] {
        let mut s = paused_session();
        availability_account(&s, "work@example.com", "reader", true);
        let (id, _, q) = request(&mut s);
        kernel::runtime::block_on(sync::Sync.pass(s.world()));
        let before = availability::load(s.store(), id).unwrap().1.unwrap();
        let nora = before
            .people
            .iter()
            .find(|p| p.calendar == "nora@studio.example")
            .unwrap();
        assert!(nora.known && !before.slots.is_empty());
        assert_eq!(nora.details.state, availability::DetailState::Pending);
        let (a, b) = nora.busy[0];
        let account = nora.via().unwrap().to_owned();
        let masked = json!({"id":"meeting","start":{"dateTime":dates::rfc(a)},"end":{"dateTime":dates::rfc(b)}});
        let mut event = masked.clone();
        event["summary"] = json!("Shared roadmap review");
        event["location"] = json!("Room 4");
        let replies = if shared {
            std::collections::HashMap::from([
                (
                    (account.clone(), String::new()),
                    json!({"items":[masked],"timeZone":q.zone,"nextPageToken":"next"}),
                ),
                (
                    (account.clone(), "next".into()),
                    json!({"items":[],"timeZone":q.zone}),
                ),
                (
                    ("work@example.com".into(), String::new()),
                    json!({"items":[event],"timeZone":q.zone}),
                ),
            ])
        } else {
            std::collections::HashMap::new()
        };
        let calls = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        s.world().caps(|caps| {
            let fake = caps.get::<api::Fake>().unwrap().clone();
            caps.insert::<dyn api::Api>(Box::new(DetailReplies {
                fake,
                replies,
                calls: calls.clone(),
            }));
        });
        refresh(&s);
        let result = availability::load(s.store(), id).unwrap().1.unwrap();
        assert_eq!(result.checked, before.checked);
        assert_eq!(result.slots, before.slots);
        let person = result
            .people
            .iter()
            .find(|p| p.calendar == "nora@studio.example")
            .unwrap();
        assert!(person.known);
        assert_eq!(person.busy, nora.busy);
        if shared {
            assert_eq!(person.details.state, availability::DetailState::Ready);
            assert_eq!(person.details.events.len(), 1);
            assert_eq!(person.details.events[0].title, "Shared roadmap review");
            assert_eq!(person.details.events[0].account, "work@example.com");
            assert_eq!(person.via(), Some(account.as_str()));
            let calls = calls.lock().unwrap();
            let calls: Vec<_> = calls
                .iter()
                .filter(|r| r.path == api::events("nora@studio.example"))
                .collect();
            assert_eq!(calls.len(), 3);
            assert!(calls[1]
                .query
                .contains(&("pageToken".into(), "next".into())));
            assert!(calls
                .iter()
                .all(|r| r.query.contains(&("singleEvents".into(), "true".into()))));
        } else {
            assert_eq!(person.details.state, availability::DetailState::Unavailable);
            assert!(availability_ui::hover_text(person, a, &q.zone)
                .unwrap()
                .contains("Busy"));
            assert!(person.details.events.is_empty());
        }
        // Old persisted replies without optional details remain readable.
        let mut old = serde_json::to_value(&before).unwrap();
        for person in old["people"].as_array_mut().unwrap() {
            person.as_object_mut().unwrap().remove("details");
        }
        let old: availability::ResultSet = serde_json::from_value(old).unwrap();
        assert!(old
            .people
            .iter()
            .all(|p| p.details.state == availability::DetailState::Unavailable));
    }
}

#[test]
fn scheduling_tools_recalculate_and_use_snapped_times_without_google_writes() {
    let mut s = paused_session();
    let (id, draft, q) = request(&mut s);
    refresh(&s);
    let result = run(
        &mut s,
        "calendar.availability_result",
        json!({"request":id,"minutes":45}),
    )
    .unwrap();
    assert_eq!(result["query"]["minutes"], 45);
    assert_eq!(availability::load(s.store(), id).unwrap().0.minutes, 30);
    let start = format!("{}T09:28", &q.start[..10]);
    let result = run(
        &mut s,
        "calendar.use_time",
        json!({"request":id,"start":start,"minutes":60}),
    )
    .unwrap();
    let expected = dates::instant(&format!("{}T09:30", &q.start[..10]), &q.zone).unwrap();
    assert_eq!(
        edit::draft(s.store(), draft)
            .unwrap()
            .form
            .bounds()
            .unwrap(),
        (expected, expected + 3600.0)
    );
    assert!(!result["conflicts"].as_array().unwrap().is_empty());
    assert!(!s.apps().tool("calendar.use_time").unwrap().asks);
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
fn dragging_reuses_the_checked_preview_and_invalidates_changed_inputs() {
    let mut s = paused_session();
    let (id, draft, q) = request(&mut s);
    refresh(&s);
    let slot = open(&mut s, panels::Availability::id(id));
    let instance = s.panel(slot).unwrap();
    let mut borrow = instance.borrow_mut();
    let p = borrow
        .as_any()
        .downcast_mut::<panels::Availability>()
        .unwrap();
    let preview = p.preview(s.now()).unwrap();
    let start = preview.result.slots[0].0;
    s.store().trace_begin(9001);
    for i in 0..10_000 {
        p.select(start + f64::from(i % 400), s.now()).unwrap();
        assert!(std::sync::Arc::ptr_eq(&preview, &p.preview(s.now()).unwrap()));
    }
    s.store().trace_end();
    assert!(
        s.store().trace_of(9001).is_empty(),
        "moving the proposal must not read the database or rebuild candidates"
    );

    let mut search = p.search.clone();
    search.minutes = "60".into();
    p.edit_search(search);
    let longer = p.preview(s.now()).unwrap();
    assert!(!std::sync::Arc::ptr_eq(&preview, &longer));
    assert_eq!(longer.result.checked, preview.result.checked);
    assert!(longer.result.slots.iter().all(|(a, b)| b - a == 3600.0));
    assert!(p.preview(preview.result.checked + 301.0).is_err());
    let longer = p.preview(s.now()).unwrap();

    // A background detail update invalidates the snapshot without renewing it.
    let mut updated = (*preview.result).clone();
    updated.people[0].details.state = availability::DetailState::Ready;
    updated.people[0]
        .details
        .events
        .push(availability::BusyEvent {
            id: "enriched".into(),
            title: "Shared event".into(),
            location: String::new(),
            start,
            end: start + 1800.0,
            all_day: false,
            account: "work@example.com".into(),
        });
    s.store()
        .write(move |c| {
            c.execute(
                "UPDATE calendar_availability SET response=? WHERE id=?",
                rusqlite::params![serde_json::to_string(&updated).unwrap(), id],
            )
        })
        .unwrap();
    let enriched = p.preview(s.now()).unwrap();
    assert!(!std::sync::Arc::ptr_eq(&longer, &enriched));
    assert!(enriched.result.people[0]
        .details
        .events
        .iter()
        .any(|e| e.title == "Shared event"));
    assert_eq!(enriched.result.checked, preview.result.checked);

    let d = edit::draft(s.store(), draft).unwrap();
    let mut form = d.form;
    form.guests = "new-guest@example.com".into();
    edit::save(&mut s, draft, d.revision, d.source, form).unwrap();
    assert!(p
        .preview(s.now())
        .err()
        .unwrap()
        .contains("guest list changed"));
    assert!(
        availability::apply_time(&mut s, id, &availability::Search::from_query(&q), start).is_err()
    );
}

#[test]
fn cached_candidates_expire_when_the_clock_passes_a_start_time() {
    let mut s = paused_session();
    let (id, _, q) = request(&mut s);
    refresh(&s);
    let (a, _) = q.validate().unwrap();
    let mut result = availability::load(s.store(), id).unwrap().1.unwrap();
    result.checked = a + 890.0;
    s.store()
        .write(move |c| {
            c.execute(
                "UPDATE calendar_availability SET response=? WHERE id=?",
                rusqlite::params![serde_json::to_string(&result).unwrap(), id],
            )
        })
        .unwrap();
    let mut cache = availability::PreviewCache::default();
    let search = availability::Search::from_query(&q);
    let before = cache.get(s.store(), id, &search, a + 890.0).unwrap();
    assert_eq!(before.result.slots[0].0, a + 900.0);
    let after = cache.get(s.store(), id, &search, a + 901.0).unwrap();
    assert!(!std::sync::Arc::ptr_eq(&before, &after));
    assert!(after
        .result
        .slots
        .iter()
        .all(|(start, _)| *start > a + 901.0));
    assert_eq!(after.result.checked, before.result.checked);
}

#[cfg(headless)]
#[test]
fn dense_pointer_events_do_not_query_or_redraw_the_workspace() {
    let mut s = paused_session();
    let (id, _, q) = request(&mut s);
    refresh(&s);
    let slot = open(&mut s, panels::Availability::id(id));
    availability_ui::test_input::exercise(&mut s, slot, &q);
}

#[cfg(headless)]
#[test]
fn drag_redraws_keep_tracks_stable_and_editor_areas_valid() {
    let mut s = paused_session();
    let (id, draft, _) = request(&mut s);
    refresh(&s);
    let editor = open(&mut s, panels::Editor::id(draft));
    let sheet = open(&mut s, panels::Availability::id(id));
    availability_ui::test_input::draw_panels(&mut s, editor, sheet);
}

#[test]
fn background_calendar_refresh_preserves_active_drag_and_track_identity() {
    let mut s = paused_session();
    let (id, draft, _) = request(&mut s);
    refresh(&s);
    let editor = open(&mut s, panels::Editor::id(draft));
    let sheet = open(&mut s, panels::Availability::id(id));
    availability_ui::test_input::refresh_during_drag(&mut s, editor, sheet);
    s.shutdown();
}

#[test]
fn native_hover_motion_keeps_event_details_visible_without_queries() {
    let mut s = paused_session();
    let (id, draft, query) = request(&mut s);
    let (start, _) = query.validate().unwrap();
    remote_event(&s, "Afternoon check-in", start + 4.0 * 3600.0);
    refresh(&s);
    let editor = open(&mut s, panels::Editor::id(draft));
    let sheet = open(&mut s, panels::Availability::id(id));
    availability_ui::test_input::hover_during_motion(&mut s, editor, sheet);
    s.shutdown();
}

#[test]
fn availability_candidates_load_as_a_snapshot_and_new_controls_replace_them() {
    use std::{sync::mpsc,time::Duration};
    let mut s = paused_session();
    let (id,_,query) = request(&mut s);
    refresh(&s);
    let search = availability::Search::from_query(&query);
    let expected = availability::preview(s.store(),id,&search,s.now()).unwrap().1;
    let (notify,woke) = mpsc::channel();
    s.store().attach_ui(move || {let _ = notify.send(());});
    let mut cache = availability::PreviewCache::default();
    assert!(cache.get(s.store(),id,&search,s.now()).err().unwrap().contains("preparing"));
    let wait = |cache: &mut availability::PreviewCache, search: &availability::Search| {
        let deadline = std::time::Instant::now()+Duration::from_secs(5);
        loop {
            assert!(std::time::Instant::now()<deadline,"availability preparation completed");
            match cache.get(s.store(),id,search,s.now()) {
                Ok(preview)=>break preview,
                Err(error) if error.contains("preparing")=>{
                    woke.recv_timeout(Duration::from_secs(5)).unwrap();
                    s.store().poll_external();
                }
                Err(error)=>panic!("{error}"),
            }
        }
    };
    let ready = wait(&mut cache,&search);
    assert_eq!(ready.result.slots,expected.slots);
    let mut longer = search.clone(); longer.minutes = "60".into();
    assert!(cache.get(s.store(),id,&longer,s.now()).is_err(),"old candidates never appear under changed controls");
    let longer = wait(&mut cache,&longer);
    assert_eq!(longer.query.minutes,60);
    assert!(longer.result.slots.iter().all(|(start,end)|end-start==3600.0));
    assert_eq!(longer.result.checked,ready.result.checked);
    s.shutdown();
}
