//! One worker serializes Calendar writes, availability and bounded refreshes.
//! A full page set is committed atomically; a failed fetch keeps the old cache.
use super::{
    api::{self, Request},
    availability, availability_details, dates, edit, model,
};
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
fn check_availability(
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
        let reply = w.run(&Request::get(&email, "/freeBusy").write(
            "POST",
            availability::wire(&query)?,
            "",
        ));
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
    fn pass(&mut self, w: &World) -> Wake {
        let r = pass(w);
        Wake::After(if matches!(r, Ok(true)) {
            Duration::ZERO
        } else {
            Duration::from_secs(60)
        })
    }
}
fn pass(w: &World) -> Result<bool, String> {
    let change=w.store().conn().query_row("SELECT id,source,kind,body FROM calendar_change WHERE state IN ('pending','processing') ORDER BY id LIMIT 1",[],|r|Ok((r.get::<_,i64>(0)?,r.get::<_,i64>(1)?,r.get::<_,String>(2)?,r.get::<_,String>(3)?))).optional().map_err(|e|e.to_string())?;
    if let Some((id, source, kind, body)) = change {
        w.store()
            .write(move |c| {
                c.execute(
                    "UPDATE calendar_change SET state='processing' WHERE id=?",
                    [id],
                )?;
                Ok(())
            })
            .map_err(|e| e.to_string())?;
        let outcome = model::source(w.store(), source)
            .ok_or("calendar disconnected".into())
            .and_then(|c| {
                change_event(
                    w,
                    id,
                    &c,
                    &kind,
                    &serde_json::from_str(&body).map_err(|_| "corrupt queued change")?,
                )
            });
        let now = w.now();
        w.store().write(move|c|{
   let (state,error,result)=match outcome{Ok(v)=>("done",String::new(),v),Err(e)=>("failed",e,json!({}))};
   c.execute("UPDATE calendar_change SET state=?1,error=?2,result=?3,updated=?4 WHERE id=?5",params![state,error,result.to_string(),now,id])?;
   c.execute("UPDATE calendar_draft SET state=?1,error=?2 WHERE id=(SELECT draft FROM calendar_change WHERE id=?3)",params![state,error,id])?;
   c.execute("UPDATE calendar_sync SET requested=requested+1 WHERE id=1",[])?;
   Ok(())
  }).map_err(|e|e.to_string())?;
        return Ok(true);
    }
    let free=w.store().conn().query_row("SELECT id,account,request FROM calendar_availability WHERE checked IS NULL ORDER BY id LIMIT 1",[],|r|Ok((r.get::<_,i64>(0)?,r.get::<_,i64>(1)?,r.get::<_,String>(2)?))).optional().map_err(|e|e.to_string())?;
    if let Some((id, account, body)) = free {
        let q: availability::Query = serde_json::from_str(&body).map_err(|e| e.to_string())?;
        let result = check_availability(w, account, &q);
        let now = w.now();
        w.store()
            .write(move |c| {
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
            .map_err(|e| e.to_string())?;
        return Ok(true);
    }
    if availability_details::pass(w)? {
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
        let result = refresh(w, account, &email, start, end);
        if let Err(error) = result {
            errors.push(format!("{email}: {error}"));
        }
    }
    let now = w.now();
    let error = errors.join("\n");
    w.store().write(move|c|{c.execute("UPDATE calendar_sync SET start=CASE WHEN start=0 THEN ?1 ELSE MIN(start,?1) END,end=MAX(end,?2),completed=?3,checked=?4,error=?5 WHERE id=1",params![start,end,requested,now,error])?;Ok(())}).map_err(|e|e.to_string())?;
    Ok(false)
}
fn refresh(w: &World, account: i64, email: &str, start: f64, end: f64) -> Result<(), String> {
    let sources = api::pages(
        w,
        Request::get(email, "/users/me/calendarList").query("maxResults", 250),
    )?;
    let now = w.now();
    let sources=w.store().write(move|c|{
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
 }).map_err(|e|e.to_string())?;
    let mut errors = Vec::new();
    for (id, remote, zone) in sources {
        let result = api::pages(
            w,
            Request::get(email, &api::events(&remote))
                .query("singleEvents", true)
                .query("timeMin", dates::rfc(start))
                .query("timeMax", dates::rfc(end))
                .query("maxResults", 2500),
        );
        let error = result.as_ref().err().cloned().unwrap_or_default();
        if !error.is_empty() {
            errors.push(error.clone());
        }
        w.store().write(move|c|{
   let enabled=c.query_row("SELECT a.calendar_enabled AND cs.active FROM calendar_source cs JOIN account a ON a.id=cs.account WHERE cs.id=?",[id],|r|r.get::<_,bool>(0)).optional()?.unwrap_or(false);if !enabled{return Ok(());}
   if let Ok(rows)=result{c.execute("UPDATE calendar_event SET active=0 WHERE source=?1 AND start<?2 AND end>?3",params![id,end,start])?;for v in rows{model::ingest(c,id,&zone,&v)?;}}
   c.execute("UPDATE calendar_source SET checked=?1,error=?2 WHERE id=?3",params![now,error,id])?;Ok(())
  }).map_err(|e|e.to_string())?;
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
fn fetch(w: &World, c: &model::Source, id: &str) -> Result<Value, String> {
    w.run(&Request::get(&c.email, &api::event(&c.remote, id)))
}
fn patch(
    w: &World,
    c: &model::Source,
    id: &str,
    body: Value,
    etag: &str,
    notify: bool,
) -> Result<Value, String> {
    w.run(
        &Request::get(&c.email, &api::event(&c.remote, id))
            .query("conferenceDataVersion", 1)
            .query("sendUpdates", if notify { "all" } else { "none" })
            .write("PATCH", body, etag),
    )
}
fn insert(
    w: &World,
    c: &model::Source,
    mut body: Value,
    id: &str,
    notify: bool,
) -> Result<Value, String> {
    body["id"] = json!(id);
    match w.run(
        &Request::get(&c.email, &api::events(&c.remote))
            .query("conferenceDataVersion", 1)
            .query("sendUpdates", if notify { "all" } else { "none" })
            .write("POST", body, ""),
    ) {
        Err(e) if e.starts_with("HTTP 409") => {
            let v = fetch(w, c, id)?;
            if done_by(&v, id) {
                Ok(v)
            } else {
                Err("event ID collision; create a new draft".into())
            }
        }
        result => result,
    }
}
fn change_event(
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
        return insert(w, c, f.patch(base, &op)?, &op, f.notify);
    }
    let current = match fetch(w, c, remote) {
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
        if let Ok(master) = fetch(w, c, series) {
            if done_by(&master, &op) {
                return Ok(master);
            }
        }
    }
    if model::text(&current, "etag") != model::text(base, "etag") {
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
        );
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
        target = fetch(w, c, series)?;
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
            return split(w, c, &op, kind, &current, &target, form);
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
                let shift = dates::date(form.start.get(..10).unwrap_or(""))?
                    - dates::date(original.start.get(..10).unwrap_or(""))?;
                let day = dates::date(master.start.get(..10).unwrap_or(""))? + shift;
                if form.all_day {
                    let length = dates::date(&form.end)? - dates::date(&form.start)?;
                    form.start = day.to_string();
                    form.end = (day + length).to_string();
                } else {
                    let duration = form.bounds()?.1 - form.bounds()?.0;
                    form.start =
                        format!("{day}T{}", form.start.split('T').nth(1).unwrap_or("00:00"));
                    form.end = dates::local(
                        dates::instant(&form.start, &form.zone)? + duration,
                        &form.zone,
                    );
                }
            } else {
                return Err(
                    "change between timed and all-day events one occurrence at a time".into(),
                );
            }
        }
    }
    if kind == "delete" {
        return match w.run(
            &Request::get(&c.email, &api::event(&c.remote, &target_id))
                .query("sendUpdates", if form.notify { "all" } else { "none" })
                .write("DELETE", Value::Null, model::text(&target, "etag")),
        ) {
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
}
/// Following edits create the replacement first, then trim the original.
/// The deterministic replacement ID makes a retry after either round trip
/// recoverable. A failed trim remains visible as a failed operation.
fn split(
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
        dates::utc(start - 1.0)
            .format("%Y%m%dT%H%M%SZ")
            .to_string()
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
        insert(w, c, new, op, form.notify)?
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
    .map_err(|e| {
        format!("following events: {e}. Retry this operation to finish the series change.")
    })?;
    Ok(result)
}

pub fn retry(s: &mut kernel::session::Session, id: i64) -> Result<(), String> {
    let n=s.act(kernel::session::Action::writing("calendar.retry","retry Calendar operation",move|c|{
  let n=c.execute("UPDATE calendar_change SET state='pending',error='' WHERE id=? AND state='failed'",[id])?;
  c.execute("UPDATE calendar_draft SET state='pending',error='' WHERE id=(SELECT draft FROM calendar_change WHERE id=? AND state='pending')",[id])?;Ok(n)
 }).claiming(vec![Box::new(edit::Submitted)])).unwrap_or(0);
    if n == 0 {
        Err("there is no failed operation to retry".into())
    } else {
        Ok(())
    }
}
