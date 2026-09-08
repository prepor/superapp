use super::*;
use kernel::{
    app::App,
    nav::Nav,
    panel::{Panel, PanelId},
    richtable::Datasource,
    session::{Action, Session},
};
use serde_json::{json, Value};
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
        sync::Sync.pass(s.world());
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
fn open(s: &mut Session, id: PanelId) -> kernel::layout::SlotId {
    s.act(Action::new("test.open", "open Calendar").moving(move |wm| {
        wm.open(id, None, false);
    }));
    s.settle();
    s.focus().unwrap()
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
impl api::Api for CalendarSnapshot {
    fn call(&mut self, r: &api::Request) -> Result<Value, String> {
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
        api::Api::call(&mut self.fake, r)
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
    };
    let request = availability::request(&mut s, 1, q, Some(id)).unwrap();
    refresh(&s);
    let (_, r, error, _) = availability::load(s.store(), request).unwrap();
    assert!(error.is_empty());
    let r = r.unwrap();
    assert!(!r.complete);
    assert!(r.people.len() >= 2);
    assert!(!r.slots.is_empty());
    availability::apply(&mut s, request, 0).unwrap();
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
impl api::Api for LostResponse {
    fn call(&mut self, request: &api::Request) -> Result<Value, String> {
        let answer = api::Api::call(&mut self.fake, request)?;
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
    };
    let request = availability::request(&mut s, 1, q, Some(id)).unwrap();
    refresh(&s);
    let d = edit::draft(s.store(), id).unwrap();
    let mut form = d.form;
    form.guests = "new-person@example.com".into();
    edit::save(&mut s, id, d.revision, 1, form).unwrap();
    assert!(availability::apply(&mut s, request, 0)
        .unwrap_err()
        .contains("guest list changed"));
}

#[test]
fn an_existing_store_can_sync_calendar_without_demo_seeding_or_mail() {
    static SHARED: &[&dyn App] = &[&crate::apps::accounts::ACCOUNTS, &CALENDAR];
    let store =
        kernel::store::Store::open(None, &[&crate::identity::SCHEMA, &schema::SCHEMA]).unwrap();
    store
        .write(|c| {
            let id =
                crate::identity::accounts::add_account_tx(c, "me@prepor.dev", "", "", "google")?;
            crate::identity::set_services(c, id, false, true)
        })
        .unwrap();
    let world = kernel::app::world_for(SHARED, store, Mode::Fake, &Env::default());
    sync::Sync.pass(&world);
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
