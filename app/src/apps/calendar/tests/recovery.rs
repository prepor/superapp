//! Recovering a reviewed conflict changes local operation history only when
//! the failed intent is known to be unapplied, or a later matching delete won.
use super::*;
use kernel::app::ProblemSource;
use std::sync::{
    atomic::{AtomicUsize, Ordering},
    Arc,
};

const CONFLICT: &str = "event changed on Google; reopen the event to review it before trying again";
const SERIES_CONFLICT: &str =
    "this series changed since the draft was opened; reopen it before saving";
const LOST_RESPONSE: &str = "network response lost after Google accepted the change";
const PARTIAL: &str =
    "following events: replacement was created; HTTP 412: original series changed";

fn once(s: &Session) {
    kernel::runtime::block_on(sync::Sync.pass(s.world()));
}

fn fake(s: &Session) -> api::Fake {
    s.world()
        .caps(|caps| caps.get::<api::Fake>().unwrap().clone())
}

fn has_problem(s: &Session, id: i64) -> bool {
    problems::PROBLEMS
        .list(s.store())
        .iter()
        .any(|problem| problem.key == format!("calendar-change:{id}"))
}

fn record(s: &Session, e: &model::Event, scope: &str, state: &str, error: &str) -> i64 {
    let body = json!({
        "base": model::raw(s.store(), e.id),
        "scope": scope,
        "reviewed": s.now(),
        "notify": false,
        "operation": format!("fixture-{}-{state}", e.id),
    })
    .to_string();
    let (source, event, now, state, error) = (
        e.source,
        e.id,
        s.now(),
        state.to_string(),
        error.to_string(),
    );
    s.store()
        .write(move |tx| {
            tx.execute(
                "INSERT INTO calendar_change(source,event,kind,body,state,error,updated) VALUES(?1,?2,'delete',?3,?4,?5,?6)",
                rusqlite::params![source, event, body, state, error, now],
            )?;
            Ok(tx.last_insert_rowid())
        })
        .unwrap()
}

fn queued_body(s: &Session, id: i64) -> String {
    s.store()
        .conn()
        .query_row("SELECT body FROM calendar_change WHERE id=?", [id], |r| {
            r.get(0)
        })
        .unwrap()
}

struct NoRequests(Arc<AtomicUsize>);

#[async_trait::async_trait(?Send)]
impl api::Api for NoRequests {
    async fn call(&mut self, _: &api::Request) -> Result<Value, String> {
        self.0.fetch_add(1, Ordering::SeqCst);
        Err("reconciliation must not contact Google".into())
    }
}

fn no_requests(s: &Session) -> Arc<AtomicUsize> {
    let calls = Arc::new(AtomicUsize::new(0));
    s.world().caps(|caps| {
        caps.insert::<dyn api::Api>(Box::new(NoRequests(calls.clone())));
    });
    calls
}

#[test]
fn a_reviewed_delete_retires_the_old_conflict_and_publishes_the_version_to_review() {
    let mut s = paused_session();
    let original = event(&s, "Planning");
    let provider = fake(&s);
    let old = edit::command(
        &mut s,
        original.id,
        &original.etag,
        "delete",
        "this",
        "",
        false,
    )
    .unwrap();
    {
        let mut remote = provider.state.lock().unwrap();
        let event = remote.get_mut(&original.remote).unwrap();
        event["etag"] = json!("\"reviewed-server-version\"");
        event["summary"] = json!("Planning revised on Google");
    }

    once(&s);
    assert_eq!(operation(&s, old), ("failed".into(), CONFLICT.into()));
    assert!(has_problem(&s, old));
    let latest = model::event(s.store(), original.id).unwrap();
    assert_eq!(latest.title, "Planning revised on Google");
    assert_eq!(
        latest.etag, "\"reviewed-server-version\"",
        "the failed preflight must publish the fetched version before another full sync"
    );
    assert_ne!(
        provider.state.lock().unwrap()[&original.remote]["status"],
        "cancelled"
    );
    assert!(
        sync::retry(&mut s, old).is_err(),
        "a blind retry cannot reuse the stale base"
    );

    let reviewed =
        edit::command(&mut s, latest.id, &latest.etag, "delete", "this", "", false).unwrap();
    refresh(&s);
    assert_eq!(operation(&s, reviewed), ("done".into(), String::new()));
    assert_eq!(operation(&s, old), ("superseded".into(), CONFLICT.into()));
    assert!(
        !has_problem(&s, old),
        "the original failed delete must leave Problems"
    );
    assert!(model::event(s.store(), original.id).is_none());
    assert_eq!(
        provider.state.lock().unwrap()[&original.remote]["status"],
        "cancelled"
    );
}

#[test]
fn startup_reconciles_completed_deletes_locally_using_the_effective_scope() {
    for (title, old_scope, new_scope) in [
        ("Planning", "all", "this"),
        ("Design review", "this", "this"),
    ] {
        let s = paused_session();
        let e = event(&s, title);
        let old = record(&s, &e, old_scope, "failed", CONFLICT);
        let done = record(&s, &e, new_scope, "done", "");
        let old_body = queued_body(&s, old);
        let provider = fake(&s);
        let mut deleted = model::raw(s.store(), e.id);
        deleted["status"] = json!("cancelled");
        provider
            .state
            .lock()
            .unwrap()
            .insert(e.remote.clone(), deleted);
        let id = e.id;
        s.store()
            .write(move |tx| tx.execute("UPDATE calendar_event SET active=0 WHERE id=?", [id]))
            .unwrap();
        let before = provider.state.lock().unwrap().clone();
        let calls = no_requests(&s);

        once(&s);
        assert_eq!(
            operation(&s, old),
            ("superseded".into(), CONFLICT.into()),
            "{title}: {old_scope} -> {new_scope}"
        );
        assert_eq!(operation(&s, done), ("done".into(), String::new()));
        assert_eq!(
            queued_body(&s, old),
            old_body,
            "recovery keeps the original intent for history"
        );
        assert!(!has_problem(&s, old));
        assert_eq!(
            calls.load(Ordering::SeqCst),
            0,
            "startup recovery needs no remote work"
        );
        assert_eq!(*provider.state.lock().unwrap(), before);
    }
}

#[test]
fn reconciliation_requires_a_later_matching_completed_delete_and_a_definite_conflict() {
    for mismatch in [
        "event",
        "source",
        "remote",
        "scope",
        "kind",
        "order",
        "unknown",
        "partial",
        "all",
        "following",
    ] {
        let s = paused_session();
        let e = event(&s, "Design review");
        let error = match mismatch {
            "unknown" => LOST_RESPONSE,
            "partial" => PARTIAL,
            _ => CONFLICT,
        };
        let scope = if matches!(mismatch, "all" | "following") {
            mismatch
        } else {
            "this"
        };
        let (old, done) = if mismatch == "order" {
            let done = record(&s, &e, "this", "done", "");
            (record(&s, &e, "this", "failed", error), done)
        } else {
            let old = record(&s, &e, scope, "failed", error);
            (old, record(&s, &e, scope, "done", ""))
        };
        s.store().write(move |tx| {
            match mismatch {
                "event" => { tx.execute("UPDATE calendar_change SET event=event+10000 WHERE id=?", [done])?; }
                "source" => { tx.execute("UPDATE calendar_change SET source=source+10000 WHERE id=?", [done])?; }
                "remote" => { tx.execute("UPDATE calendar_change SET body=json_set(body,'$.base.id','different-remote-event') WHERE id=?", [done])?; }
                "scope" => { tx.execute("UPDATE calendar_change SET body=json_set(body,'$.scope','all') WHERE id=?", [done])?; }
                "kind" => { tx.execute("UPDATE calendar_change SET kind='respond' WHERE id=?", [done])?; }
                _ => {}
            }
            Ok(())
        }).unwrap();
        let calls = no_requests(&s);

        once(&s);
        assert_eq!(
            operation(&s, old),
            ("failed".into(), error.into()),
            "{mismatch}"
        );
        assert!(
            has_problem(&s, old),
            "{mismatch}: uncertainty must remain visible"
        );
        assert_eq!(calls.load(Ordering::SeqCst), 0, "{mismatch}");
    }
}

#[test]
fn dismissing_a_known_conflict_keeps_the_draft_intent_and_google_event_unchanged() {
    let mut s = paused_session();
    let e = event(&s, "Planning");
    let draft = edit::create(&mut s, e.source, Some(e.id)).unwrap();
    let mut form = edit::draft(s.store(), draft).unwrap().form;
    form.title = "Keep my unsent title".into();
    edit::save(&mut s, draft, 1, e.source, form).unwrap();
    let provider = fake(&s);
    provider.state.lock().unwrap().get_mut(&e.remote).unwrap()["etag"] = json!("\"server-new\"");
    let change = edit::commit(&mut s, draft, 2).unwrap();
    once(&s);
    assert_eq!(operation(&s, change).0, "failed");
    assert!(edit::fixture(&mut s, move |_| Ok(sync::retry_draft_plan(draft))).is_err());
    let before = serde_json::to_value(edit::draft(s.store(), draft).unwrap()).unwrap();
    let before_remote = provider.state.lock().unwrap().clone();
    let body = queued_body(&s, change);

    edit::fixture(&mut s, move |_| Ok(sync::dismiss_plan(change))).unwrap();
    assert_eq!(operation(&s, change), ("dismissed".into(), CONFLICT.into()));
    assert!(!has_problem(&s, change));
    assert_eq!(queued_body(&s, change), body);
    assert_eq!(
        serde_json::to_value(edit::draft(s.store(), draft).unwrap()).unwrap(),
        before
    );
    assert_eq!(*provider.state.lock().unwrap(), before_remote);
    assert!(model::event(s.store(), e.id).is_some());
    assert!(
        edit::fixture(&mut s, move |_| Ok(sync::dismiss_plan(change))).is_err(),
        "dismissal is guarded against stale actions"
    );
}

#[test]
fn dismissal_revalidates_operation_state_and_does_not_hide_ambiguous_failures() {
    for error in [LOST_RESPONSE, PARTIAL, "HTTP 500: server unavailable"] {
        let mut s = paused_session();
        let e = event(&s, "Planning");
        let failed = record(&s, &e, "this", "failed", error);
        let before = fake(&s).state.lock().unwrap().clone();
        assert!(
            edit::fixture(&mut s, move |_| Ok(sync::dismiss_plan(failed))).is_err(),
            "{error}"
        );
        assert_eq!(operation(&s, failed), ("failed".into(), error.into()));
        assert!(has_problem(&s, failed));
        assert_eq!(*fake(&s).state.lock().unwrap(), before);
    }

    let mut s = paused_session();
    let e = event(&s, "Planning");
    let id = record(&s, &e, "this", "failed", CONFLICT);
    let prepared = sync::dismiss_plan(id);
    s.store()
        .write(move |tx| {
            tx.execute(
                "UPDATE calendar_change SET state='pending' WHERE id=?",
                [id],
            )
        })
        .unwrap();
    assert!(
        edit::fixture(&mut s, move |_| Ok(prepared)).is_err(),
        "a state change after preparing dismissal must be checked in the writer transaction"
    );
    assert_eq!(operation(&s, id), ("pending".into(), CONFLICT.into()));
}

#[test]
fn definite_version_conflicts_require_review_but_unknown_responses_remain_retryable() {
    for error in [
        CONFLICT,
        SERIES_CONFLICT,
        "HTTP 412: event changed on Google; reopen it",
    ] {
        let mut s = paused_session();
        let e = event(&s, "Design review");
        let id = record(&s, &e, "all", "failed", error);
        assert!(sync::retry(&mut s, id).is_err(), "{error}");
        assert_eq!(operation(&s, id), ("failed".into(), error.into()));
    }

    let mut s = paused_session();
    let e = event(&s, "Planning");
    let id = record(&s, &e, "this", "failed", LOST_RESPONSE);
    sync::retry(&mut s, id).unwrap();
    assert_eq!(operation(&s, id), ("pending".into(), String::new()));
}

struct PartialWrite {
    fake: api::Fake,
    lost_insert: bool,
    reject_trim: bool,
}

#[async_trait::async_trait(?Send)]
impl api::Api for PartialWrite {
    async fn call(&mut self, request: &api::Request) -> Result<Value, String> {
        if self.reject_trim && request.method == "PATCH" && request.body["recurrence"].is_array() {
            self.reject_trim = false;
            return Err("HTTP 400: original series trim rejected".into());
        }
        let response = api::Api::call(&mut self.fake, request).await?;
        if self.lost_insert && request.method == "POST" && request.path.ends_with("/events") {
            self.lost_insert = false;
            return Err(LOST_RESPONSE.into());
        }
        Ok(response)
    }
}

#[test]
fn a_later_preflight_conflict_does_not_erase_a_following_edits_partial_write() {
    for lost_insert in [false, true] {
        let mut s = paused_session();
        let e = model::EVENTS
            .page(s.store(), None, 0, 100)
            .iter()
            .filter(|event| event.title == "Design review")
            .nth(2)
            .unwrap()
            .clone();
        let draft = edit::create(&mut s, e.source, Some(e.id)).unwrap();
        let mut form = edit::draft(s.store(), draft).unwrap().form;
        form.title = "Replacement pending recovery".into();
        form.scope = "following".into();
        edit::save(&mut s, draft, 1, e.source, form).unwrap();
        let provider = fake(&s);
        s.world().caps(|caps| {
            caps.insert::<dyn api::Api>(Box::new(PartialWrite {
                fake: provider.clone(),
                lost_insert,
                reject_trim: !lost_insert,
            }));
        });
        let change = edit::commit(&mut s, draft, 2).unwrap();
        once(&s);
        let (state, error) = operation(&s, change);
        assert_eq!(state, "failed");
        assert!(
            !sync::needs_review(&error),
            "the first attempt may have created a replacement: {error}"
        );
        assert_eq!(
            provider
                .state
                .lock()
                .unwrap()
                .values()
                .filter(|event| event["summary"] == "Replacement pending recovery")
                .count(),
            1
        );

        let mut changed = model::raw(s.store(), e.id);
        changed["etag"] = json!("\"changed-after-partial-write\"");
        provider.state.lock().unwrap().insert(e.remote.clone(), changed);
        let before_retry = provider.state.lock().unwrap().clone();
        sync::retry(&mut s, change).unwrap();
        once(&s);
        let (state, error) = operation(&s, change);
        assert_eq!(state, "failed");
        assert!(!sync::needs_review(&error),
            "a later rejected preflight cannot prove the earlier write was unapplied (lost insert={lost_insert}): {error}");
        assert!(
            !edit::editable(&edit::draft(s.store(), draft).unwrap()),
            "an uncertain partial write cannot become a new editable submission"
        );
        assert!(edit::fixture(&mut s, move |_| Ok(sync::dismiss_plan(change))).is_err());
        assert!(has_problem(&s, change));
        assert_eq!(
            *provider.state.lock().unwrap(),
            before_retry,
            "retry must not create a second replacement"
        );
    }
}

#[test]
fn restarting_a_processing_change_preserves_uncertainty_and_its_retry_identity() {
    let mut s = paused_session();
    let e = event(&s, "Planning");
    let draft = edit::create(&mut s, e.source, Some(e.id)).unwrap();
    let mut form = edit::draft(s.store(), draft).unwrap().form;
    form.title = "Potentially submitted before restart".into();
    edit::save(&mut s, draft, 1, e.source, form).unwrap();
    let change = edit::commit(&mut s, draft, 2).unwrap();
    let original_body = queued_body(&s, change);

    // A crash could occur after sending the request but before recording its
    // outcome. Another writer's newer ETag does not reveal what happened to it.
    s.store()
        .write(move |tx| {
            tx.execute(
                "UPDATE calendar_change SET state='processing' WHERE id=?",
                [change],
            )
        })
        .unwrap();
    let provider = fake(&s);
    {
        let mut remote = provider.state.lock().unwrap();
        let event = remote.get_mut(&e.remote).unwrap();
        event["etag"] = json!("\"changed-while-app-was-closed\"");
        event["summary"] = json!("The current server version");
    }
    let remote_before = provider.state.lock().unwrap().clone();

    once(&s);
    let (state, error) = operation(&s, change);
    assert_eq!(state, "failed");
    assert!(
        !sync::needs_review(&error),
        "a restarted in-flight request has no definitive rejection: {error}"
    );
    assert!(!edit::editable(&edit::draft(s.store(), draft).unwrap()));
    assert!(edit::fixture(&mut s, move |_| Ok(sync::dismiss_plan(change))).is_err());
    assert!(has_problem(&s, change));
    assert_eq!(*provider.state.lock().unwrap(), remote_before);

    sync::retry(&mut s, change).unwrap();
    assert_eq!(operation(&s, change).0, "pending");
    assert_eq!(
        queued_body(&s, change),
        original_body,
        "recovery must retry the same operation key and original intent"
    );
    assert!(!edit::editable(&edit::draft(s.store(), draft).unwrap()));
}

#[test]
fn a_prepared_draft_save_cannot_overwrite_a_later_conflict_or_uncertain_outcome() {
    for later_error in [CONFLICT, LOST_RESPONSE] {
        let mut s = paused_session();
        let e = event(&s, "Planning");
        let draft = edit::create(&mut s, e.source, Some(e.id)).unwrap();
        let change = edit::commit(&mut s, draft, 1).unwrap();
        s.store()
            .write(move |tx| {
                tx.execute(
                    "UPDATE calendar_change SET state='failed',error='HTTP 400: invalid field' WHERE id=?",
                    [change],
                )?;
                tx.execute(
                    "UPDATE calendar_draft SET state='failed',error='HTTP 400: invalid field' WHERE id=?",
                    [draft],
                )?;
                Ok(())
            })
            .unwrap();
        let editable = edit::draft(s.store(), draft).unwrap();
        assert!(edit::editable(&editable));
        let mut form = editable.form.clone();
        form.title = "Prepared before the operation changed".into();
        let prepared =
            edit::save_plan(s.world(), draft, editable.revision, e.source, form).unwrap();

        // Retrying a queued operation updates the outcome without incrementing
        // the draft revision. A background form preparation can span that work.
        s.store()
            .write(move |tx| {
                tx.execute(
                    "UPDATE calendar_change SET error=?1,uncertain=?2 WHERE id=?3",
                    rusqlite::params![later_error, later_error == LOST_RESPONSE, change],
                )?;
                tx.execute(
                    "UPDATE calendar_draft SET error=?1 WHERE id=?2",
                    rusqlite::params![later_error, draft],
                )?;
                Ok(())
            })
            .unwrap();
        let immutable = edit::draft(s.store(), draft).unwrap();
        assert_eq!(immutable.revision, editable.revision);
        assert!(!edit::editable(&immutable));
        assert!(
            edit::fixture(&mut s, move |_| Ok(prepared)).is_err(),
            "writer validation must recheck the operation outcome: {later_error}"
        );
        assert_eq!(operation(&s, change), ("failed".into(), later_error.into()));
        assert_eq!(
            serde_json::to_value(edit::draft(s.store(), draft).unwrap()).unwrap(),
            serde_json::to_value(immutable).unwrap()
        );
    }
}
