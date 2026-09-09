use super::*;

fn create_input(s: &Session) -> Value {
    json!({"source":1,"form":{
        "title":"Agent meeting","start":dates::rfc(s.world().now()+3607.0),
        "end":dates::rfc(s.world().now()+7209.0),"zone":"Europe/Berlin","notify":false
    }})
}

fn update_input(e: &model::Event) -> Value {
    json!({"event":e.id,"etag":e.etag,
        "changes":{"title":"Agent update","scope":"this","notify":false}})
}

fn writes(s: &Session) -> (i64, i64) {
    s.store()
        .conn()
        .query_row(
            "SELECT (SELECT COUNT(*) FROM calendar_draft),(SELECT COUNT(*) FROM calendar_change)",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .unwrap()
}

fn prepare(s: &Session, name: &str, input: &Value) -> kernel::session::Edit<Value> {
    let tool = s.apps().tool(name).unwrap();
    tool.check(input).unwrap();
    let prepared = kernel::runtime::block_on(tool.preparer.unwrap()(input)(s.world())).unwrap();
    let kernel::tool::Prepared::Edit(edit) = prepared else {
        panic!("expected a prepared edit")
    };
    edit
}

#[test]
fn agent_tools_create_modify_and_remove_events_through_the_google_queue() {
    let mut s = paused_session();
    let mut input = create_input(&s);
    input["form"]["guests"] = json!("nora@studio.example, ?leo@studio.example");
    input["form"]["notes"] = json!("Keep these notes");
    input["form"]["location"] = json!("Room 2");
    input["form"]["meet"] = json!(true);
    input["form"]["reminders"] = json!("popup:10,email:60");
    let created = run(&mut s, "calendar.create", input).unwrap();
    assert_eq!(created["queued"], true);
    assert_eq!(writes(&s), (1, 1));
    let draft = created["draft"].as_i64().unwrap();
    assert_eq!(edit::draft(s.store(), draft).unwrap().state, "pending");
    let status = run(
        &mut s,
        "calendar.operation",
        json!({"operation":created["operation"]}),
    )
    .unwrap();
    assert_eq!(status["state"], "pending");
    assert!(model::EVENTS
        .page(s.store(), None, 0, 100)
        .iter()
        .all(|e| e.title != "Agent meeting"));
    refresh(&s);
    assert_eq!(
        operation(&s, created["operation"].as_i64().unwrap()).0,
        "done"
    );
    assert_eq!(edit::draft(s.store(), draft).unwrap().state, "done");
    let e = event(&s, "Agent meeting");
    assert!(!e.meet.is_empty());

    // Provider fields and attendee responses survive an update that only
    // changes the title and clears the location.
    let mut raw = model::raw(s.store(), e.id);
    raw["attendees"][0]["responseStatus"] = json!("accepted");
    raw["attendees"][0]["comment"] = json!("See you there");
    raw["attachments"] = json!([{"fileUrl":"https://example.com/agenda"}]);
    raw["extendedProperties"]["private"]["custom"] = json!("keep");
    s.world().caps(|caps| {
        caps.get::<api::Fake>()
            .unwrap()
            .state
            .lock()
            .unwrap()
            .insert(e.remote.clone(), raw.clone());
    });
    let cached = raw.clone();
    s.store()
        .write(move |tx| model::ingest(tx, e.source, &e.zone, &cached))
        .unwrap();
    let read = run(&mut s, "calendar.event", json!({"event":e.id})).unwrap();
    assert_eq!(read["can_edit"], true);
    let updated = run(
        &mut s,
        "calendar.update",
        json!({"event":e.id,"etag":read["event"]["etag"],
        "changes":{"title":"Agent update","location":"","scope":"this","notify":false}}),
    )
    .unwrap();
    assert_eq!(updated["queued"], true);
    assert_eq!(
        operation(&s, updated["operation"].as_i64().unwrap()).0,
        "pending"
    );
    refresh(&s);
    assert_eq!(
        operation(&s, updated["operation"].as_i64().unwrap()).0,
        "done"
    );
    let updated_event = event(&s, "Agent update");
    let after = model::raw(s.store(), updated_event.id);
    for field in [
        "start",
        "end",
        "attendees",
        "description",
        "reminders",
        "attachments",
        "conferenceData",
    ] {
        assert_eq!(after[field], raw[field], "preserve {field}");
    }
    assert_eq!(after["extendedProperties"]["private"]["custom"], "keep");
    assert_eq!(after["location"], "");
    assert!(run(
        &mut s,
        "calendar.delete",
        json!({"event":e.id,"etag":e.etag,"scope":"this","notify":false})
    )
    .is_err());
    let deleted = run(
        &mut s,
        "calendar.delete",
        json!({"event":updated_event.id,
        "etag":updated_event.etag,"scope":"this","notify":false}),
    )
    .unwrap();
    refresh(&s);
    let status = run(
        &mut s,
        "calendar.operation",
        json!({"operation":deleted["operation"]}),
    )
    .unwrap();
    assert_eq!(status["state"], "done");
    assert!(run(&mut s, "calendar.event", json!({"event":updated_event.id})).is_err());
}

#[test]
fn direct_tools_require_event_details_reviewed_versions_and_notification_choices() {
    let s = paused_session();
    let input = create_input(&s);
    let create = s.apps().tool("calendar.create").unwrap();
    for field in ["title", "start", "end", "zone", "notify"] {
        let mut missing = input.clone();
        missing["form"].as_object_mut().unwrap().remove(field);
        assert!(create.check(&missing).unwrap_err().contains(field));
    }
    for fields in [
        json!({"title":42}),
        json!({"notify":"yes"}),
        json!({"guests":["a@example.com"]}),
        json!({"invented":true}),
    ] {
        let mut invalid = input.clone();
        invalid["form"]
            .as_object_mut()
            .unwrap()
            .extend(fields.as_object().unwrap().clone());
        assert!(create.check(&invalid).is_err(), "{invalid}");
    }
    let update = s.apps().tool("calendar.update").unwrap();
    let input = update_input(&event(&s, "Planning"));
    for field in ["event", "etag", "changes"] {
        let mut missing = input.clone();
        missing.as_object_mut().unwrap().remove(field);
        assert!(update.check(&missing).unwrap_err().contains(field));
    }
    for field in ["scope", "notify"] {
        let mut missing = input.clone();
        missing["changes"].as_object_mut().unwrap().remove(field);
        assert!(update.check(&missing).unwrap_err().contains(field));
    }
    let mut invalid = input;
    invalid["changes"]["source"] = json!(2);
    assert!(
        update.check(&invalid).is_err(),
        "an update cannot move calendars"
    );
    for tool in [create, update, s.apps().tool("calendar.delete").unwrap()] {
        assert!(tool.writes && tool.asks);
        assert!(tool.preparer.is_some());
    }
}

#[test]
fn invalid_forms_and_unreviewed_or_read_only_events_leave_no_drafts_or_operations() {
    let mut s = paused_session();
    let input = create_input(&s);
    for fields in [
        json!({"title":" "}),
        json!({"end":input["form"]["start"]}),
        json!({"zone":"not/a-zone"}),
        json!({"guests":"invalid"}),
        json!({"recurrence":"RRULE:FREQ=INVALID"}),
        json!({"reminders":"popup:-1"}),
        json!({"visibility":"unknown"}),
        json!({"scope":"guess"}),
    ] {
        let mut invalid = input.clone();
        invalid["form"]
            .as_object_mut()
            .unwrap()
            .extend(fields.as_object().unwrap().clone());
        assert!(
            run(&mut s, "calendar.create", invalid.clone()).is_err(),
            "{invalid}"
        );
    }
    let read_only = event(&s, "Studio all-hands");
    for source in [0, -1, i64::MAX, read_only.source] {
        let mut invalid = input.clone();
        invalid["source"] = json!(source);
        assert!(run(&mut s, "calendar.create", invalid).is_err());
    }
    assert!(run(&mut s, "calendar.update", update_input(&read_only)).is_err());
    let e = event(&s, "Planning");
    for etag in ["", "stale"] {
        let mut invalid = update_input(&e);
        invalid["etag"] = json!(etag);
        assert!(run(&mut s, "calendar.update", invalid)
            .unwrap_err()
            .contains("ETag"));
    }
    let mut invalid = update_input(&e);
    invalid["event"] = json!(i64::MAX);
    assert!(run(&mut s, "calendar.update", invalid)
        .unwrap_err()
        .contains("not found"));
    let mut invalid = update_input(&e);
    invalid["changes"]["end"] = json!(dates::rfc(e.start));
    assert!(run(&mut s, "calendar.update", invalid)
        .unwrap_err()
        .contains("end"));
    s.store()
        .write(|tx| {
            tx.execute("UPDATE calendar_source SET meet=0 WHERE id=1", [])?;
            Ok(())
        })
        .unwrap();
    let mut invalid = input;
    invalid["form"]["meet"] = json!(true);
    assert!(run(&mut s, "calendar.create", invalid)
        .unwrap_err()
        .contains("Meet"));
    assert_eq!(writes(&s), (0, 0));
}

#[test]
fn direct_submissions_prepare_without_writes_and_rollback_if_permissions_change() {
    for name in ["calendar.create", "calendar.update"] {
        for disconnected in [false, true] {
            let mut s = paused_session();
            let input = if name == "calendar.create" {
                create_input(&s)
            } else {
                update_input(&event(&s, "Planning"))
            };
            let plan = prepare(&s, name, &input);
            assert_eq!(
                writes(&s),
                (0, 0),
                "preparation cannot create a draft or send an invitation"
            );
            s.store()
                .write(move |tx| {
                    tx.execute(
                        if disconnected {
                            "UPDATE account SET calendar_enabled=0"
                        } else {
                            "UPDATE calendar_source SET role='reader'"
                        },
                        [],
                    )?;
                    Ok(())
                })
                .unwrap();
            let head = s.history().head();
            assert!(edit::fixture(&mut s, move |_| Ok(plan))
                .unwrap_err()
                .contains("read-only"));
            assert_eq!(
                writes(&s),
                (0, 0),
                "the new draft must roll back with the refused operation"
            );
            assert_eq!(s.history().head(), head);
        }
    }
}

#[test]
fn prepared_updates_rollback_when_the_cached_event_changes_or_is_deleted() {
    for deleted in [false, true] {
        let mut s = paused_session();
        let e = event(&s, "Planning");
        let plan = prepare(&s, "calendar.update", &update_input(&e));
        s.store()
            .write(move |tx| {
                tx.execute(
                    if deleted {
                        "UPDATE calendar_event SET active=0 WHERE id=?"
                    } else {
                        "UPDATE calendar_event SET etag='changed' WHERE id=?"
                    },
                    [e.id],
                )?;
                Ok(())
            })
            .unwrap();
        let head = s.history().head();
        assert!(edit::fixture(&mut s, move |_| Ok(plan))
            .unwrap_err()
            .contains("event changed"));
        assert_eq!(writes(&s), (0, 0));
        assert_eq!(s.history().head(), head);
    }
}

#[test]
fn direct_updates_keep_the_failed_draft_when_google_has_a_newer_event() {
    let mut s = paused_session();
    let e = event(&s, "Planning");
    let result = run(&mut s, "calendar.update", update_input(&e)).unwrap();
    s.world().caps(|caps| {
        let fake = caps.get::<api::Fake>().unwrap();
        let mut rows = fake.state.lock().unwrap();
        let current = rows.get_mut(&e.remote).unwrap();
        current["etag"] = json!("\"newer-version\"");
        current["summary"] = json!("Changed on Google");
    });
    refresh(&s);
    let status = run(
        &mut s,
        "calendar.operation",
        json!({"operation":result["operation"]}),
    )
    .unwrap();
    assert_eq!(status["state"], "failed");
    assert!(status["error"].as_str().unwrap().contains("changed"));
    let draft = edit::draft(s.store(), result["draft"].as_i64().unwrap()).unwrap();
    assert_eq!(draft.state, "failed");
    assert_eq!(draft.form.title, "Agent update");
    assert_eq!(draft.base["etag"], e.etag);
    assert_eq!(event(&s, "Changed on Google").id, e.id);
}

#[test]
fn direct_create_supports_all_day_events_with_an_exclusive_end() {
    let mut s = paused_session();
    let day = dates::date(&dates::day(s.world().now() + 86400.0, "Europe/Berlin")).unwrap();
    let result = run(&mut s, "calendar.create", json!({"source":1,"form":{
        "title":"Agent all day","start":day.to_string(),"end":day.succ_opt().unwrap().to_string(),
        "zone":"Europe/Berlin","all_day":true,"notify":false
    }})).unwrap();
    refresh(&s);
    assert_eq!(
        operation(&s, result["operation"].as_i64().unwrap()).0,
        "done"
    );
    let e = event(&s, "Agent all day");
    assert!(e.all_day);
    let raw = model::raw(s.store(), e.id);
    assert_eq!(raw["start"]["date"], day.to_string());
    assert_eq!(raw["end"]["date"], day.succ_opt().unwrap().to_string());
    assert!(raw["start"].get("dateTime").is_none());
}

#[test]
fn direct_updates_apply_the_explicit_recurring_scope() {
    for (scope, original, changed) in [("this", 7, 1), ("all", 0, 8), ("following", 2, 6)] {
        let mut s = paused_session();
        let e = model::EVENTS
            .page(s.store(), None, 0, 100)
            .iter()
            .filter(|e| e.title == "Design review")
            .nth(2)
            .unwrap()
            .clone();
        let mut input = update_input(&e);
        input["changes"]["scope"] = json!(scope);
        let result = run(&mut s, "calendar.update", input).unwrap();
        refresh(&s);
        assert_eq!(
            operation(&s, result["operation"].as_i64().unwrap()).0,
            "done",
            "{scope}"
        );
        let events = model::EVENTS.page(s.store(), None, 0, 100);
        assert_eq!(
            events.iter().filter(|e| e.title == "Design review").count(),
            original,
            "{scope}"
        );
        assert_eq!(
            events.iter().filter(|e| e.title == "Agent update").count(),
            changed,
            "{scope}"
        );
    }
}

fn weekly_series(s: &mut Session, start: &str, end: &str) -> Vec<model::Event> {
    let created = run(
        s,
        "calendar.create",
        json!({"source":1,"form":{
            "title":"DST series","start":start,"end":end,"zone":"Europe/Berlin",
            "recurrence":"RRULE:FREQ=WEEKLY;COUNT=20","notify":false
        }}),
    )
    .unwrap();
    refresh(s);
    assert_eq!(
        operation(s, created["operation"].as_i64().unwrap()).0,
        "done"
    );
    let events: Vec<_> = model::EVENTS
        .page(s.store(), None, 0, 100)
        .iter()
        .filter(|e| e.title == "DST series")
        .cloned()
        .collect();
    assert_eq!(events.len(), 20);
    events
}

#[test]
fn direct_all_scope_title_edits_preserve_instants_across_dst() {
    for (start, end, selected_day) in [
        (
            "2026-09-16T15:00:07+02:00",
            "2026-09-16T16:00:09+02:00",
            "2026-11-04",
        ),
        (
            "2027-02-17T15:00:07+01:00",
            "2027-02-17T16:00:09+01:00",
            "2027-04-07",
        ),
    ] {
        let mut s = paused_session();
        let before = weekly_series(&mut s, start, end);
        let occurrence = before.iter().find(|e| e.day == selected_day).unwrap();
        let mut input = update_input(occurrence);
        input["changes"]["scope"] = json!("all");
        let result = run(&mut s, "calendar.update", input).unwrap();
        refresh(&s);
        assert_eq!(
            operation(&s, result["operation"].as_i64().unwrap()).0,
            "done"
        );
        let events = model::EVENTS.page(s.store(), None, 0, 100);
        let after: Vec<_> = events
            .iter()
            .filter(|e| e.series == occurrence.series)
            .collect();
        assert_eq!(after.len(), before.len());
        for (before, after) in before.iter().zip(after) {
            assert_eq!(after.title, "Agent update");
            assert_eq!(
                after.start, before.start,
                "start on {} after editing {selected_day}",
                before.day
            );
            assert_eq!(
                after.end, before.end,
                "end on {} after editing {selected_day}",
                before.day
            );
        }
    }
}

#[test]
fn direct_all_scope_time_edits_use_local_dates_and_keep_seconds() {
    let mut s = paused_session();
    let before = weekly_series(
        &mut s,
        "2026-09-16T15:00:07+02:00",
        "2026-09-16T16:00:09+02:00",
    );
    let occurrence = before.iter().find(|e| e.day == "2026-11-04").unwrap();
    // UTC input represents the next civil day in Berlin. The master's
    // September date still uses summer time when applying that day shift.
    let result = run(
        &mut s,
        "calendar.update",
        json!({"event":occurrence.id,"etag":occurrence.etag,
        "changes":{"scope":"all","notify":false,
            "start":"2026-11-04T23:15:11Z","end":"2026-11-05T00:00:13Z"}}),
    )
    .unwrap();
    refresh(&s);
    assert_eq!(
        operation(&s, result["operation"].as_i64().unwrap()).0,
        "done"
    );
    let events = model::EVENTS.page(s.store(), None, 0, 100);
    let after: Vec<_> = events
        .iter()
        .filter(|e| e.series == occurrence.series)
        .collect();
    assert_eq!(after.len(), before.len());
    for (before, after) in before.iter().zip(after) {
        let next_day = dates::date(&before.day).unwrap().succ_opt().unwrap();
        let expected = next_day
            .and_hms_opt(0, 15, 11)
            .unwrap()
            .and_local_timezone(dates::zone("Europe/Berlin").unwrap())
            .single()
            .unwrap()
            .timestamp() as f64;
        assert_eq!(after.start, expected, "start on {next_day}");
        assert_eq!(after.end, expected + 2702.0, "end on {next_day}");
    }
}

#[test]
fn direct_all_scope_edits_reject_new_skipped_or_repeated_master_times() {
    for (start, end, changed_start, changed_end, selected_day) in [
        (
            "2026-10-25T01:15:07+02:00",
            "2026-10-25T01:45:09+02:00",
            "2026-11-08T02:30:11+01:00",
            "2026-11-08T03:00:13+01:00",
            "2026-11-08",
        ),
        (
            "2027-03-28T01:15:07+01:00",
            "2027-03-28T01:45:09+01:00",
            "2027-04-11T02:30:11+02:00",
            "2027-04-11T03:00:13+02:00",
            "2027-04-11",
        ),
    ] {
        let mut s = paused_session();
        let before = weekly_series(&mut s, start, end);
        let occurrence = before.iter().find(|e| e.day == selected_day).unwrap();
        let result = run(
            &mut s,
            "calendar.update",
            json!({"event":occurrence.id,"etag":occurrence.etag,
            "changes":{"scope":"all","notify":false,"start":changed_start,"end":changed_end}}),
        )
        .unwrap();
        refresh(&s);
        let (state, error) = operation(&s, result["operation"].as_i64().unwrap());
        assert_eq!(state, "failed");
        assert!(error.contains("skipped or repeated"), "{error}");
        assert!(edit::editable(
            &edit::draft(s.store(), result["draft"].as_i64().unwrap()).unwrap()
        ));
        let events = model::EVENTS.page(s.store(), None, 0, 100);
        let after: Vec<_> = events
            .iter()
            .filter(|e| e.series == occurrence.series)
            .collect();
        assert_eq!(after.len(), before.len());
        for (before, after) in before.iter().zip(after) {
            assert_eq!((after.start, after.end), (before.start, before.end));
        }
    }
}
