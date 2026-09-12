//! One worker serializes Calendar writes, availability and bounded refreshes.
//! A full page set is committed atomically; a failed fetch keeps the old cache.
use super::{
    api::{self, Request},
    availability, availability_details, dates, edit, model,
};
use chrono::TimeZone;
use kernel::{
    app::{Wake, Worker},
    effect::{Job, World},
};
use rusqlite::{params, OptionalExtension};
use serde_json::{json, Value};
use std::time::Duration;

pub struct Sync;

/// A guest may be shared with a different connected identity than the event's
/// organizer. Retry only unreadable calendars, keeping every successful result.
async fn check_availability(
    w: &World,
    account: i64,
    q: &availability::Query,
) -> Result<availability::ResultSet, String> {
    let mut accounts = w
        .store()
        .rows_sql(
            "availability accounts",
            "connected identities available for free/busy",
            "SELECT id,email FROM account WHERE calendar_enabled=1 ORDER BY id",
            &[],
            |r| Ok((r.get::<_, i64>(0)?, r.get::<_, String>(1)?)),
        )
        .as_ref()
        .clone();
    if !accounts.iter().any(|(id, _)| *id == account) {
        return Err("Google account disconnected".into());
    }
    accounts.sort_by_key(|(id, _)| *id != account);
    let mut people = availability::calculate(q, &json!({}), w.now())?.people;
    for (_, email) in accounts {
        let mut query = q.clone();
        query.guests = people
            .iter()
            .filter(|p| !p.known)
            .map(|p| p.calendar.clone())
            .collect();
        if query.guests.is_empty() {
            break;
        }
        let reply = w
            .run_async(&Request::get(&email, "/freeBusy").write(
                "POST",
                availability::wire(&query)?,
                "",
            ))
            .await;
        let results = match reply {
            Ok(value) => availability::calculate(&query, &value, w.now())?.people,
            Err(error) => query
                .guests
                .iter()
                .map(|id| availability::Person {
                    calendar: id.clone(),
                    known: false,
                    busy: Vec::new(),
                    error: error.clone(),
                    checks: Vec::new(),
                    details: availability::Details::default(),
                })
                .collect(),
        };
        for mut result in results {
            let Some(person) = people
                .iter_mut()
                .find(|p| p.calendar.eq_ignore_ascii_case(&result.calendar))
            else {
                continue;
            };
            let mut checks = std::mem::take(&mut person.checks);
            checks.push(availability::Check {
                account: email.clone(),
                error: result.error.clone(),
            });
            result.checks = checks;
            *person = result;
        }
    }
    for person in &mut people {
        if person.known && !person.busy.is_empty() {
            person.details.state = availability::DetailState::Pending;
        }
    }
    availability::suggest(q, people, w.now())
}

#[async_trait::async_trait(?Send)]
impl Worker for Sync {
    fn name(&self) -> String {
        "calendar-sync".into()
    }
    fn entity(&self) -> Option<String> {
        Some("calendar-sync".into())
    }
    fn claims(&self, _: &Job) -> bool {
        false
    }
    async fn pass(&mut self, w: &World) -> Wake {
        let r = pass(w).await;
        Wake::After(if matches!(r, Ok(true)) {
            Duration::ZERO
        } else {
            Duration::from_secs(60)
        })
    }
}
async fn pass(w: &World) -> Result<bool, String> {
    // A later reviewed deletion can resolve an older rejected one. Reconcile
    // persisted rows too, including failures recorded by previous app versions.
    if !resolved_deletions(w.store().conn())
        .map_err(|e| e.to_string())?
        .is_empty()
    {
        w.store()
            .write_async(|tx| retire_resolved_deletions(tx))
            .await
            .map_err(|e| e.to_string())?;
    }
    let change = w
        .store()
        .conn()
        .query_row(
            "SELECT id,source,kind,body,state,uncertain FROM calendar_change
         WHERE state IN ('pending','processing') ORDER BY id LIMIT 1",
            [],
            |r| {
                Ok((
                    r.get::<_, i64>(0)?,
                    r.get::<_, i64>(1)?,
                    r.get::<_, String>(2)?,
                    r.get::<_, String>(3)?,
                    r.get::<_, String>(4)?,
                    r.get::<_, bool>(5)?,
                ))
            },
        )
        .optional()
        .map_err(|e| e.to_string())?;
    if let Some((id, source, kind, body, state, uncertain)) = change {
        // A restarted in-flight request, or an earlier ambiguous attempt, may
        // have reached Google. A later rejected preflight cannot undo that fact.
        let uncertain = state == "processing" || uncertain;
        w.store()
            .write_async(move |c| {
                c.execute(
                    "UPDATE calendar_change SET state='processing' WHERE id=?",
                    [id],
                )?;
                Ok(())
            })
            .await
            .map_err(|e| e.to_string())?;
        let outcome = async {
            let source = model::source(w.store(), source).ok_or("calendar disconnected")?;
            let body = serde_json::from_str(&body).map_err(|_| "corrupt queued change")?;
            change_event(w, id, &source, &kind, &body).await
        }
        .await;
        let now = w.now();
        w.store().write_async(move |c| {
            let (state, error, result, uncertain) = match outcome {
                Ok(v) => ("done", String::new(), v, false),
                Err(e) => {
                    let rejected = rejected(&e);
                    let error = if uncertain && rejected {
                        format!("previous attempt may already have reached Google; {e}")
                    } else { e };
                    ("failed", error, json!({}), uncertain || !rejected)
                }
            };
            c.execute(
                "UPDATE calendar_change SET state=?1,error=?2,result=?3,updated=?4,uncertain=?5 WHERE id=?6",
                params![state, error, result.to_string(), now, uncertain, id],
            )?;
            c.execute(
                "UPDATE calendar_draft SET state=?1,error=?2 WHERE id=(SELECT draft FROM calendar_change WHERE id=?3)",
                params![state, error, id],
            )?;
            retire_resolved_deletions(c)?;
            c.execute("UPDATE calendar_sync SET requested=requested+1 WHERE id=1", [])?;
            Ok(())
        }).await.map_err(|e| e.to_string())?;
        return Ok(true);
    }
    let free=w.store().conn().query_row("SELECT id,account,request FROM calendar_availability WHERE checked IS NULL ORDER BY id LIMIT 1",[],|r|Ok((r.get::<_,i64>(0)?,r.get::<_,i64>(1)?,r.get::<_,String>(2)?))).optional().map_err(|e|e.to_string())?;
    if let Some((id, account, body)) = free {
        let q: availability::Query = serde_json::from_str(&body).map_err(|e| e.to_string())?;
        let result = check_availability(w, account, &q).await;
        let now = w.now();
        w.store()
            .write_async(move |c| {
                let (response, error) = match result {
                    Ok(v) => (Some(serde_json::to_string(&v).unwrap()), String::new()),
                    Err(e) => (None, e),
                };
                c.execute(
                    "UPDATE calendar_availability SET response=?1,error=?2,checked=?3 WHERE id=?4",
                    params![response, error, now, id],
                )?;
                Ok(())
            })
            .await
            .map_err(|e| e.to_string())?;
        return Ok(true);
    }
    if availability_details::pass(w).await? {
        return Ok(true);
    }
    let state = w
        .store()
        .conn()
        .query_row(
            "SELECT start,end,requested,completed,checked FROM calendar_sync WHERE id=1",
            [],
            |r| {
                Ok((
                    r.get::<_, f64>(0)?,
                    r.get::<_, f64>(1)?,
                    r.get::<_, i64>(2)?,
                    r.get::<_, i64>(3)?,
                    r.get::<_, Option<f64>>(4)?,
                ))
            },
        )
        .optional()
        .map_err(|e| e.to_string())?;
    let Some((start, end, requested, completed, checked)) = state else {
        return Ok(false);
    };
    if requested == completed && checked.is_some_and(|t| w.now() - t < 300.0) {
        return Ok(false);
    }
    let start = if start == 0.0 {
        w.now() - 90.0 * 86400.0
    } else {
        start.min(w.now() - 90.0 * 86400.0)
    };
    let end = end.max(w.now() + 366.0 * 86400.0);
    let accounts = w
        .store()
        .rows_sql(
            "calendar accounts",
            "accounts with Calendar enabled",
            "SELECT id,email FROM account WHERE calendar_enabled=1",
            &[],
            |r| Ok((r.get::<_, i64>(0)?, r.get::<_, String>(1)?)),
        )
        .as_ref()
        .clone();
    let mut errors = Vec::new();
    for (account, email) in accounts {
        let result = refresh(w, account, &email, start, end).await;
        if let Err(error) = result {
            errors.push(format!("{email}: {error}"));
        }
    }
    let now = w.now();
    let error = errors.join("\n");
    w.store().write_async(move|c|{c.execute("UPDATE calendar_sync SET start=CASE WHEN start=0 THEN ?1 ELSE MIN(start,?1) END,end=MAX(end,?2),completed=?3,checked=?4,error=?5 WHERE id=1",params![start,end,requested,now,error])?;Ok(())}).await.map_err(|e|e.to_string())?;
    Ok(false)
}
async fn refresh(w: &World, account: i64, email: &str, start: f64, end: f64) -> Result<(), String> {
    let sources = api::pages(
        w,
        Request::get(email, "/users/me/calendarList").query("maxResults", 250),
    )
    .await?;
    let now = w.now();
    let sources=w.store().write_async(move|c|{
  let enabled:bool=c.query_row("SELECT calendar_enabled FROM account WHERE id=?",[account],|r|r.get(0)).optional()?.unwrap_or(false);
  if !enabled{return Ok(Vec::new());}
  c.execute("UPDATE calendar_source SET active=0 WHERE account=?",[account])?;
  let mut kept=Vec::new();
  for source in sources{if source["deleted"]==true{continue;}let remote=model::text(&source,"id");if remote.is_empty(){continue;}
   let role=model::text(&source,"accessRole");let zone=source["timeZone"].as_str().unwrap_or("UTC");let title=source["summaryOverride"].as_str().unwrap_or(model::text(&source,"summary"));let meet=source["conferenceProperties"]["allowedConferenceSolutionTypes"].as_array().is_some_and(|a|a.contains(&json!("hangoutsMeet")));
   c.execute("INSERT INTO calendar_source(account,remote,title,zone,role,meet) VALUES(?1,?2,?3,?4,?5,?6) ON CONFLICT(account,remote) DO UPDATE SET title=excluded.title,zone=excluded.zone,role=excluded.role,meet=excluded.meet,active=1",params![account,remote,title,zone,role,meet])?;
   let id=c.query_row("SELECT id FROM calendar_source WHERE account=?1 AND remote=?2",params![account,remote],|r|r.get::<_,i64>(0))?;
   if role!="freeBusyReader"{kept.push((id,remote.to_string(),zone.to_string()));}
  }Ok(kept)
 }).await.map_err(|e|e.to_string())?;
    let mut errors = Vec::new();
    for (id, remote, zone) in sources {
        let result = api::pages(
            w,
            Request::get(email, &api::events(&remote))
                .query("singleEvents", true)
                .query("timeMin", dates::rfc(start))
                .query("timeMax", dates::rfc(end))
                .query("maxResults", 2500),
        )
        .await;
        let error = result.as_ref().err().cloned().unwrap_or_default();
        if !error.is_empty() {
            errors.push(error.clone());
        }
        w.store().write_async(move|c|{
   let enabled=c.query_row("SELECT a.calendar_enabled AND cs.active FROM calendar_source cs JOIN account a ON a.id=cs.account WHERE cs.id=?",[id],|r|r.get::<_,bool>(0)).optional()?.unwrap_or(false);if !enabled{return Ok(());}
   if let Ok(rows)=result{c.execute("UPDATE calendar_event SET active=0 WHERE source=?1 AND start<?2 AND end>?3",params![id,end,start])?;for v in rows{model::ingest(c,id,&zone,&v)?;}}
   c.execute("UPDATE calendar_source SET checked=?1,error=?2 WHERE id=?3",params![now,error,id])?;Ok(())
  }).await.map_err(|e|e.to_string())?;
    }
    if errors.is_empty() {
        Ok(())
    } else {
        Err(errors.join("; "))
    }
}
fn operation_properties(base: &Value, op: &str) -> Value {
    let mut props = base["extendedProperties"]
        .as_object()
        .cloned()
        .unwrap_or_default();
    let mut private = props
        .get("private")
        .and_then(Value::as_object)
        .cloned()
        .unwrap_or_default();
    private.insert("superappOperation".into(), json!(op));
    props.insert("private".into(), json!(private));
    json!(props)
}
fn op_id(id: i64) -> String {
    format!("superapp{:032x}", id)
}
fn done_by(v: &Value, op: &str) -> bool {
    v["extendedProperties"]["private"]["superappOperation"] == op
}
async fn fetch(w: &World, c: &model::Source, id: &str) -> Result<Value, String> {
    w.run_async(&Request::get(&c.email, &api::event(&c.remote, id)))
        .await
}
async fn patch(
    w: &World,
    c: &model::Source,
    id: &str,
    body: Value,
    etag: &str,
    notify: bool,
) -> Result<Value, String> {
    w.run_async(
        &Request::get(&c.email, &api::event(&c.remote, id))
            .query("conferenceDataVersion", 1)
            .query("sendUpdates", if notify { "all" } else { "none" })
            .write("PATCH", body, etag),
    )
    .await
}
async fn insert(
    w: &World,
    c: &model::Source,
    mut body: Value,
    id: &str,
    notify: bool,
) -> Result<Value, String> {
    body["id"] = json!(id);
    match w
        .run_async(
            &Request::get(&c.email, &api::events(&c.remote))
                .query("conferenceDataVersion", 1)
                .query("sendUpdates", if notify { "all" } else { "none" })
                .write("POST", body, ""),
        )
        .await
    {
        Err(e) if e.starts_with("HTTP 409") => {
            let v = fetch(w, c, id).await?;
            if done_by(&v, id) {
                Ok(v)
            } else {
                Err("event ID collision; create a new draft".into())
            }
        }
        result => result,
    }
}
async fn change_event(
    w: &World,
    id: i64,
    c: &model::Source,
    kind: &str,
    body: &Value,
) -> Result<Value, String> {
    let op = body["operation"]
        .as_str()
        .map(str::to_string)
        .unwrap_or_else(|| op_id(id));
    let base = &body["base"];
    let remote = model::text(base, "id");
    if kind == "save" && remote.is_empty() {
        let f: edit::Form =
            serde_json::from_value(body["form"].clone()).map_err(|e| e.to_string())?;
        return insert(w, c, f.patch(base, &op)?, &op, f.notify).await;
    }
    let current = match fetch(w, c, remote).await {
        Err(e) if kind == "delete" && (e.starts_with("HTTP 404") || e.starts_with("HTTP 410")) => {
            return Ok(json!({"deleted":true}))
        }
        r => r?,
    };
    if done_by(&current, &op) {
        return Ok(current);
    }
    let series = model::text(base, "recurringEventId");
    let scope = if kind == "save" {
        body["form"]["scope"].as_str()
    } else {
        body["scope"].as_str()
    }
    .unwrap_or("this");
    if !series.is_empty() && scope != "this" {
        if let Ok(master) = fetch(w, c, series).await {
            if done_by(&master, &op) {
                return Ok(master);
            }
        }
    }
    if model::text(&current, "etag") != model::text(base, "etag") {
        // The rejected operation keeps its original base. Review reads the
        // version we just fetched, without waiting for a complete calendar sync.
        let (source, zone, observed) = (c.id, c.zone.clone(), current.clone());
        w.store()
            .write_async(move |tx| {
                let connected: bool = tx.query_row(
                    "SELECT EXISTS(SELECT 1 FROM calendar_source c JOIN account a ON a.id=c.account
                 WHERE c.id=? AND c.active=1 AND a.calendar_enabled=1)",
                    [source],
                    |r| r.get(0),
                )?;
                if connected {
                    model::ingest(tx, source, &zone, &observed)?;
                }
                Ok(())
            })
            .await
            .map_err(|e| e.to_string())?;
        return Err(
            "event changed on Google; reopen the event to review it before trying again".into(),
        );
    }
    let form: edit::Form = if kind == "save" {
        serde_json::from_value(body["form"].clone()).map_err(|e| e.to_string())?
    } else {
        let mut f = edit::Form::from_event(&current, &c.zone);
        f.scope = body["scope"].as_str().unwrap_or("this").into();
        f.notify = body["notify"] != false;
        f
    };
    if kind == "respond" {
        let mut attendees = current["attendees"]
            .as_array()
            .cloned()
            .ok_or("no attendees")?;
        let me = attendees
            .iter_mut()
            .find(|p| p["self"] == true)
            .ok_or("no invitation for this account")?;
        me["responseStatus"] = body["response"].clone();
        return patch(
            w,
            c,
            remote,
            json!({"attendees":attendees,"extendedProperties":operation_properties(&current,&op)}),
            model::text(&current, "etag"),
            true,
        )
        .await;
    }
    if !edit::can_edit(c, &current) {
        return Err("Google no longer allows you to change this event".into());
    }
    if current["eventType"]
        .as_str()
        .is_some_and(|t| t != "default")
    {
        return Err("this special event type must be edited in Google Calendar".into());
    }
    let series = model::text(base, "recurringEventId");
    let mut target = current.clone();
    let mut target_id = remote.to_string();
    let mut form = form;
    if !series.is_empty() && form.scope != "this" {
        target = fetch(w, c, series).await?;
        target_id = series.into();
        if done_by(&target, &op) {
            return Ok(target);
        }
        if let Some(updated) = target["updated"]
            .as_str()
            .and_then(|d| dates::instant(d, "UTC").ok())
        {
            if updated > body["reviewed"].as_f64().unwrap_or(0.0) {
                return Err(
                    "this series changed since the draft was opened; reopen it before saving"
                        .into(),
                );
            }
        }
        if form.scope == "following" {
            return split(w, c, &op, kind, &current, &target, form).await;
        }
        // Editing all occurrences retains the master's date; moving the selected
        // occurrence shifts the series by the same civil date and time delta.
        if kind == "save" {
            let master = edit::Form::from_event(&target, &c.zone);
            let original = edit::Form::from_event(&current, &c.zone);
            if form.recurrence == "unchanged" {
                form.recurrence = master.recurrence.clone();
            }
            if form.all_day == master.all_day && original.all_day == master.all_day {
                if form.all_day {
                    let shift = dates::date(&form.start)? - dates::date(&original.start)?;
                    let day = dates::date(&master.start)? + shift;
                    let length = dates::date(&form.end)? - dates::date(&form.start)?;
                    form.start = day.to_string();
                    form.end = (day + length).to_string();
                } else {
                    rebase_series_time(&mut form, &original, &master)?;
                }
            } else {
                return Err(
                    "change between timed and all-day events one occurrence at a time".into(),
                );
            }
        }
    }
    if kind == "delete" {
        return match w
            .run_async(
                &Request::get(&c.email, &api::event(&c.remote, &target_id))
                    .query("sendUpdates", if form.notify { "all" } else { "none" })
                    .write("DELETE", Value::Null, model::text(&target, "etag")),
            )
            .await
        {
            Err(e) if e.starts_with("HTTP 404") || e.starts_with("HTTP 410") => {
                Ok(json!({"deleted":true}))
            }
            r => r,
        };
    }
    patch(
        w,
        c,
        &target_id,
        form.patch(&target, &op)?,
        model::text(&target, "etag"),
        form.notify,
    )
    .await
}

/// Apply the occurrence's civil date shift and requested clock time to the
/// master. Its date may use a different UTC offset, so resolve it in the IANA
/// zone instead of copying the occurrence's RFC3339 suffix.
fn rebase_series_time(
    form: &mut edit::Form,
    original: &edit::Form,
    master: &edit::Form,
) -> Result<(), String> {
    let (from, until) = form.bounds()?;
    let zone = dates::zone(&form.zone)?;
    let requested = dates::utc(from).with_timezone(&zone);
    let original = dates::utc(original.bounds()?.0).with_timezone(&dates::zone(&original.zone)?);
    let master = dates::utc(master.bounds()?.0).with_timezone(&dates::zone(&master.zone)?);
    let shift = requested.date_naive() - original.date_naive();
    let day = master.date_naive().checked_add_signed(shift)
        .ok_or("you cannot move this series outside the supported date range")?;
    let local = day.and_time(requested.time());
    let start = if local == requested.naive_local() {
        // The selected first occurrence already identifies an exact instant,
        // including an explicit offset in a repeated hour.
        requested
    } else if zone == master.timezone() && local == master.naive_local() {
        master
    } else {
        zone.from_local_datetime(&local).single().ok_or(
            "you cannot move this series into a skipped or repeated local time; edit its first occurrence with an explicit UTC offset",
        )?
    }.timestamp() as f64;
    form.start = dates::rfc(start);
    form.end = dates::rfc(start + (until - from));
    Ok(())
}

/// Following edits create the replacement first, then trim the original.
/// The deterministic replacement ID makes a retry after either round trip
/// recoverable. A failed trim remains visible as a failed operation.
async fn split(
    w: &World,
    c: &model::Source,
    op: &str,
    kind: &str,
    instance: &Value,
    master: &Value,
    mut form: edit::Form,
) -> Result<Value, String> {
    let rules = master["recurrence"]
        .as_array()
        .ok_or("series has no recurrence")?;
    if rules.len() != 1
        || !model::text(instance, "recurringEventId").is_empty()
            && instance["originalStartTime"].is_null()
    {
        return Err("this series has complex exceptions; edit it in Google Calendar".into());
    }
    let rule = rules[0].as_str().ok_or("invalid series rule")?;
    if !rule.starts_with("RRULE:") {
        return Err("following changes require a single recurrence rule".into());
    }
    let start = dates::read(&instance["originalStartTime"], &c.zone)?.0;
    let master_form = edit::Form::from_event(master, &c.zone);
    if start <= master_form.bounds()?.0 {
        return Err("this is the first occurrence; choose all events".into());
    }
    let mut pieces = rule
        .trim_start_matches("RRULE:")
        .split(';')
        .filter(|p| !p.starts_with("UNTIL=") && !p.starts_with("COUNT="))
        .map(str::to_string)
        .collect::<Vec<_>>();
    // UNTIL is inclusive and must have the original DTSTART's value type.
    // Use the occurrence's original civil date even if this edit moves it.
    let until = if master_form.all_day {
        dates::date(model::text(&instance["originalStartTime"], "date"))?
            .pred_opt()
            .ok_or("cannot trim before the first supported date")?
            .format("%Y%m%d")
            .to_string()
    } else {
        dates::utc(start - 1.0).format("%Y%m%dT%H%M%SZ").to_string()
    };
    pieces.push(format!("UNTIL={until}"));
    let trimmed = format!("RRULE:{}", pieces.join(";"));
    let result = if kind == "save" {
        if form.recurrence == "unchanged" {
            form.recurrence = rule.into();
            if let Some(total) = rule.split(';').find_map(|p| {
                p.strip_prefix("COUNT=")
                    .and_then(|n| n.parse::<usize>().ok())
            }) {
                let set = edit::recurrence_set(
                    &master_form.start,
                    &master_form.zone,
                    master_form.all_day,
                    rule,
                )?;
                let before = set
                    .before(dates::utc(start - 1.0).with_timezone(&rrule::Tz::UTC))
                    .all(10000)
                    .dates
                    .len();
                let remaining = total
                    .checked_sub(before)
                    .filter(|n| *n > 0)
                    .ok_or("could not calculate remaining occurrences")?;
                form.recurrence = rule
                    .split(';')
                    .map(|p| {
                        if p.starts_with("COUNT=") {
                            format!("COUNT={remaining}")
                        } else {
                            p.into()
                        }
                    })
                    .collect::<Vec<_>>()
                    .join(";");
            }
        }
        let mut base = master.clone();
        base.as_object_mut().unwrap().remove("recurringEventId");
        let mut new = form.patch(&base, op)?;
        if form.meet && base["conferenceData"].is_object() {
            new["conferenceData"] = base["conferenceData"].clone();
        }
        insert(w, c, new, op, form.notify).await?
    } else {
        json!({"deleted":true})
    };
    let id = model::text(master, "id");
    patch(
        w,
        c,
        id,
        json!({"recurrence":[trimmed],"extendedProperties":operation_properties(master,op)}),
        model::text(master, "etag"),
        form.notify,
    )
    .await
    .map_err(|e| {
        format!("following events: {e}. Retry this operation to finish the series change.")
    })?;
    Ok(result)
}

/// These failures definitively rejected this attempt. In particular, a
/// `following events:` failure may have created a replacement already and must
/// retain its original operation key for recovery, even if its final trim
/// received HTTP 412. Earlier uncertain attempts wrap later rejection messages.
pub fn needs_review(error: &str) -> bool {
    error.starts_with("HTTP 412:")
        || error.starts_with("event changed on Google;")
        || error.starts_with("this series changed since the draft was opened;")
}

/// Legacy errors are persisted as text. Only top-level rejection messages prove
/// no mutation occurred; nested errors from partial writes do not.
pub fn rejected(error: &str) -> bool {
    needs_review(error)
        || [
            "HTTP 400:",
            "HTTP 401:",
            "HTTP 403:",
            "HTTP 404:",
            "HTTP 410:",
            "you cannot ",
        ]
        .iter()
        .any(|prefix| error.starts_with(prefix))
}

fn resolved_deletions(c: &rusqlite::Connection) -> rusqlite::Result<Vec<i64>> {
    // Older workers treated a missing occurrence as a completed deletion even
    // for a series scope. Only a matching single-event deletion is proof here.
    let mut query = c.prepare(
        "SELECT failed.id,failed.error FROM calendar_change failed
         WHERE failed.state='failed' AND failed.kind='delete' AND failed.uncertain=0 AND EXISTS(
           SELECT 1 FROM calendar_change done
           WHERE done.state='done' AND done.kind='delete' AND done.id>failed.id
             AND done.source=failed.source AND done.event=failed.event
             AND CASE WHEN json_valid(done.body) THEN json_extract(done.body,'$.base.id') END
               = CASE WHEN json_valid(failed.body) THEN json_extract(failed.body,'$.base.id') END
             AND CASE WHEN json_valid(done.body) THEN
                   CASE WHEN COALESCE(json_extract(done.body,'$.base.recurringEventId'),'')=''
                     THEN 'this' ELSE COALESCE(json_extract(done.body,'$.scope'),'this') END END = 'this'
             AND CASE WHEN json_valid(failed.body) THEN
                   CASE WHEN COALESCE(json_extract(failed.body,'$.base.recurringEventId'),'')=''
                     THEN 'this' ELSE COALESCE(json_extract(failed.body,'$.scope'),'this') END END = 'this'
         )")?;
    let rows = query.query_map([], |r| Ok((r.get::<_, i64>(0)?, r.get::<_, String>(1)?)))?;
    let mut resolved = Vec::new();
    for row in rows {
        let (id, error) = row?;
        if needs_review(&error) {
            resolved.push(id);
        }
    }
    Ok(resolved)
}

fn retire_resolved_deletions(c: &rusqlite::Connection) -> rusqlite::Result<()> {
    for id in resolved_deletions(c)? {
        c.execute(
            "UPDATE calendar_change SET state='superseded' WHERE id=? AND state='failed'",
            [id],
        )?;
    }
    Ok(())
}

/// Dismiss only an attempt known to have been rejected. This acknowledges its
/// error locally; it never changes Google or discards the stored draft/body.
pub fn dismiss_plan(id: i64) -> kernel::session::Edit<()> {
    kernel::session::Edit::writing("calendar.dismiss", "dismiss Calendar conflict", move |c| {
        let error = c
            .query_row(
                "SELECT error FROM calendar_change WHERE id=? AND state='failed' AND uncertain=0",
                [id],
                |r| r.get::<_, String>(0),
            )
            .optional()?;
        if !error.as_deref().is_some_and(needs_review) {
            return Err(rusqlite::Error::InvalidParameterName(
                "only a rejected Calendar conflict can be dismissed".into(),
            ));
        }
        c.execute(
            "UPDATE calendar_change SET state='dismissed' WHERE id=? AND state='failed'",
            [id],
        )?;
        Ok(())
    })
}

fn retry_tx(c: &rusqlite::Connection, id: i64) -> rusqlite::Result<()> {
    let (error, uncertain) = c
        .query_row(
            "SELECT error,uncertain FROM calendar_change WHERE id=? AND state='failed'",
            [id],
            |r| Ok((r.get::<_, String>(0)?, r.get::<_, bool>(1)?)),
        )
        .optional()?
        .ok_or_else(|| {
            rusqlite::Error::InvalidParameterName("there is no failed operation to retry".into())
        })?;
    if !uncertain && needs_review(&error) {
        return Err(rusqlite::Error::InvalidParameterName(
            "review the latest event and submit a new change; retry would reuse the rejected version".into()));
    }
    let uncertain = uncertain || !rejected(&error);
    let n = c.execute("UPDATE calendar_change SET state='pending',error='',uncertain=?1 WHERE id=?2 AND state='failed'", params![uncertain, id])?;
    if n == 0 {
        return Err(rusqlite::Error::InvalidParameterName(
            "there is no failed operation to retry".into(),
        ));
    }
    c.execute("UPDATE calendar_draft SET state='pending',error='' WHERE id=(SELECT draft FROM calendar_change WHERE id=? AND state='pending')", [id])?;
    Ok(())
}
pub fn retry_plan(id: i64) -> kernel::session::Edit<()> {
    kernel::session::Edit::writing("calendar.retry", "retry Calendar operation", move |c| {
        retry_tx(c, id)
    })
    .claiming(vec![Box::new(edit::Submitted)])
}
pub fn retry_draft_plan(draft: i64) -> kernel::session::Edit<()> {
    kernel::session::Edit::writing("calendar.retry", "retry Calendar operation", move |c| {
        let id = c.query_row(
            "SELECT id FROM calendar_change WHERE draft=? ORDER BY id DESC LIMIT 1",
            [draft],
            |r| r.get(0),
        )?;
        retry_tx(c, id)
    })
    .claiming(vec![Box::new(edit::Submitted)])
}
#[cfg(test)]
pub fn retry(s: &mut kernel::session::Session, id: i64) -> Result<(), String> {
    edit::fixture(s, move |_| Ok(retry_plan(id)))
}
