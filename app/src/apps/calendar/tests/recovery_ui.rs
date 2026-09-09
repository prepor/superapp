//! A failed operation can outlive its event. Recovery remains reachable from
//! both the event card and Problems without changing a preserved local draft.
use super::*;
use kernel::{app::ProblemSource, panel::VerbAct};

const CONFLICT: &str = "event changed on Google; reopen the event to review it before trying again";

fn fail(s: &Session, change: i64, error: &str, hide_event: bool) {
    let error = error.to_string();
    s.store()
        .write(move |tx| {
            tx.execute(
                "UPDATE calendar_change SET state='failed',error=? WHERE id=?",
                rusqlite::params![error, change],
            )?;
            tx.execute(
                "UPDATE calendar_draft SET state='failed',error=? WHERE id=(SELECT draft FROM calendar_change WHERE id=?)",
                rusqlite::params![error, change],
            )?;
            if hide_event {
                tx.execute(
                    "UPDATE calendar_event SET active=0 WHERE id=(SELECT event FROM calendar_change WHERE id=?)",
                    [change],
                )?;
            }
            Ok(())
        })
        .unwrap();
}

fn problem(s: &Session, change: i64) -> kernel::app::Problem {
    problems::PROBLEMS
        .list(s.store())
        .into_iter()
        .find(|problem| problem.key == format!("calendar-change:{change}"))
        .expect("failed operation remains visible")
}

fn call(s: &mut Session, problem: &kernel::app::Problem, id: &str) {
    let verb = problem.verbs.iter().find(|verb| verb.id == id).unwrap();
    let VerbAct::Call(call) = &verb.act else {
        panic!("problem recovery is a session action");
    };
    call(s);
    s.settle();
}

#[test]
fn missing_event_keeps_review_and_dismiss_controls_and_clears_the_displayed_error() {
    let mut s = paused_session();
    let e = event(&s, "Planning");
    let change = edit::command(&mut s, e.id, &e.etag, "delete", "this", "", false).unwrap();
    fail(&s, change, CONFLICT, true);
    let slot = open(&mut s, panels::Event::id(e.id));
    let panel = s.panel(slot).unwrap();
    {
        let mut panel = panel.borrow_mut();
        let event = panel.as_any().downcast_mut::<panels::Event>().unwrap();
        assert!(event.reading().is_none());
        let verbs = event.verbs();
        assert!(
            verbs
                .iter()
                .any(|verb| verb.id == "calendar.review" && verb.label == "review latest")
        );
        assert!(
            verbs
                .iter()
                .any(|verb| verb.id == "calendar.dismiss" && verb.label == "dismiss error")
        );
        assert!(!verbs.iter().any(|verb| verb.id == "calendar.retry"));
        event.run("calendar.review", &mut s);
        assert!(event.reading().is_none());
        event.run("calendar.dismiss", &mut s);
        assert!(
            event.operation().is_none(),
            "a dismissed operation cannot leave the old error on the card"
        );
        assert!(event.verbs().is_empty());
    }
    assert_eq!(operation(&s, change), ("dismissed".into(), CONFLICT.into()));
    assert!(
        !problems::PROBLEMS
            .list(s.store())
            .iter()
            .any(|problem| problem.key == format!("calendar-change:{change}"))
    );
    assert!(model::event(s.store(), e.id).is_none());
}

#[test]
fn conflict_problem_reviews_the_event_without_opening_or_saving_its_local_draft() {
    let mut s = paused_session();
    let e = event(&s, "Planning");
    let draft = edit::create(&mut s, e.source, Some(e.id)).unwrap();
    let mut form = edit::draft(s.store(), draft).unwrap().form;
    form.title = "My preserved edit".into();
    edit::save(&mut s, draft, 1, e.source, form).unwrap();
    let change = edit::commit(&mut s, draft, 2).unwrap();
    fail(&s, change, CONFLICT, false);
    let before = serde_json::to_value(edit::draft(s.store(), draft).unwrap()).unwrap();
    let problem = problem(&s, change);
    assert!(!problem.verbs.iter().any(|verb| verb.id == "calendar.retry"));
    call(&mut s, &problem, "calendar.review");
    assert!(!s.showing(&panels::Event::id(e.id)).is_empty());
    assert!(s.showing(&panels::Editor::id(draft)).is_empty());
    assert_eq!(
        serde_json::to_value(edit::draft(s.store(), draft).unwrap()).unwrap(),
        before
    );
    assert_eq!(
        operation(&s, change).0,
        "failed",
        "review is not dismissal or resubmission"
    );
    call(&mut s, &problem, "calendar.dismiss");
    assert_eq!(operation(&s, change).0, "dismissed");
    assert_eq!(
        serde_json::to_value(edit::draft(s.store(), draft).unwrap()).unwrap(),
        before
    );
}

#[test]
fn transient_failure_remains_retryable_after_the_event_disappears() {
    let mut s = paused_session();
    let e = event(&s, "Planning");
    let change = edit::command(&mut s, e.id, &e.etag, "delete", "this", "", false).unwrap();
    fail(&s, change, "connection lost while waiting for Google", true);
    let problem = problem(&s, change);
    assert!(problem.verbs.iter().any(|verb| verb.id == "calendar.retry"));
    assert!(
        !problem
            .verbs
            .iter()
            .any(|verb| verb.id == "calendar.dismiss")
    );
    let slot = open(&mut s, panels::Event::id(e.id));
    let panel = s.panel(slot).unwrap();
    {
        let mut panel = panel.borrow_mut();
        let event = panel.as_any().downcast_mut::<panels::Event>().unwrap();
        assert!(event.reading().is_none());
        assert!(event.verbs().iter().any(|verb| verb.id == "calendar.retry"));
        assert!(
            !event
                .verbs()
                .iter()
                .any(|verb| verb.id == "calendar.dismiss")
        );
        event.run("calendar.retry", &mut s);
    }
    assert_eq!(operation(&s, change), ("pending".into(), String::new()));
}

#[test]
fn conflicted_editor_reviews_the_latest_event_and_preserves_its_original_draft() {
    let mut s = paused_session();
    let e = event(&s, "Planning");
    let draft = edit::create(&mut s, e.source, Some(e.id)).unwrap();
    let mut form = edit::draft(s.store(), draft).unwrap().form;
    form.title = "My local title before the conflict".into();
    edit::save(&mut s, draft, 1, e.source, form).unwrap();
    let change = edit::commit(&mut s, draft, 2).unwrap();
    fail(&s, change, CONFLICT, false);
    let original = edit::draft(s.store(), draft).unwrap();
    let before = serde_json::to_value(&original).unwrap();
    let mut latest = model::raw(s.store(), e.id);
    latest["summary"] = json!("Planning revised on Google");
    latest["etag"] = json!("\"new-server-version\"");
    s.store()
        .write(move |tx| model::ingest(tx, e.source, &e.zone, &latest))
        .unwrap();

    let slot = open(&mut s, panels::Editor::id(draft));
    let panel = s.panel(slot).unwrap();
    let mut verbs = panel.borrow().verbs();
    assert_eq!(
        verbs.len(),
        1,
        "a stale draft cannot be saved or blindly retried"
    );
    let review = verbs.remove(0);
    assert_eq!(review.id, "calendar.review");
    assert_eq!(review.label, "review latest");
    assert!(
        !edit::editable(&original),
        "the form must disable autosave for its rejected base"
    );
    let VerbAct::Go(nav) = review.act else {
        panic!("review only navigates");
    };
    s.nav(nav);
    s.settle();
    assert_eq!(s.showing(&panels::Editor::id(draft)), vec![slot]);
    let event_slot = s.showing(&panels::Event::id(e.id))[0];
    assert_eq!(
        s.panel(event_slot).unwrap().borrow().title(),
        "Planning revised on Google"
    );
    assert_eq!(
        serde_json::to_value(edit::draft(s.store(), draft).unwrap()).unwrap(),
        before
    );
    assert_eq!(operation(&s, change), ("failed".into(), CONFLICT.into()));
}

#[test]
fn conflicted_create_draft_without_an_event_does_not_offer_an_invalid_review_target() {
    let mut s = paused_session();
    let (draft, form) = form(&mut s);
    edit::save(&mut s, draft, 1, 1, form).unwrap();
    let change = edit::commit(&mut s, draft, 2).unwrap();
    fail(&s, change, "HTTP 412: create rejected", false);
    let slot = open(&mut s, panels::Editor::id(draft));
    let verbs = s.panel(slot).unwrap().borrow().verbs();
    assert_eq!(verbs.len(), 1);
    assert_eq!(verbs[0].id, "calendar.timeline");
    assert!(s.showing(&panels::Event::id(0)).is_empty());
}
