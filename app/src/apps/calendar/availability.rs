//! Free/busy is permission dependent. Missing/error calendars remain unknown.
use super::{dates, edit, model};
use kernel::{
    session::{Action, Session},
    store::{Store, Val},
};
use rusqlite::params;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Query {
    pub start: String,
    pub end: String,
    pub zone: String,
    pub minutes: u32,
    pub guests: Vec<String>,
}
impl Query {
    pub fn validate(&self) -> Result<(f64, f64), String> {
        let a = dates::instant(&self.start, &self.zone)?;
        let b = dates::instant(&self.end, &self.zone)?;
        if b <= a || b - a > 14.0 * 86400.0 {
            return Err("availability needs a range between 15 minutes and 14 days".into());
        }
        if !(15..=480).contains(&self.minutes) {
            return Err("duration must be 15–480 minutes".into());
        }
        if self.guests.len() > 50 {
            return Err("availability supports up to 50 calendars".into());
        }
        Ok((a, b))
    }
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Person {
    pub calendar: String,
    pub known: bool,
    pub busy: Vec<(f64, f64)>,
    pub error: String,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ResultSet {
    pub people: Vec<Person>,
    pub slots: Vec<(f64, f64)>,
    pub complete: bool,
    pub checked: f64,
}
pub fn calculate(q: &Query, answer: &Value, now: f64) -> Result<ResultSet, String> {
    let (a, b) = q.validate()?;
    let mut people = Vec::new();
    for id in &q.guests {
        let c = &answer["calendars"][id];
        let mut valid = c.is_object()
            && c["errors"].as_array().is_none_or(Vec::is_empty)
            && c["busy"].is_array();
        let mut busy = Vec::new();
        for item in c["busy"].as_array().into_iter().flatten() {
            match (
                dates::instant(model::text(item, "start"), "UTC"),
                dates::instant(model::text(item, "end"), "UTC"),
            ) {
                (Ok(start), Ok(end)) if end > start => busy.push((start, end)),
                _ => valid = false,
            }
        }
        people.push(Person {
            calendar: id.clone(),
            known: valid,
            busy,
            error: if valid {
                String::new()
            } else {
                "availability not shared or could not be read".into()
            },
        });
    }
    let mut slots = Vec::new();
    let mut at = (a.max(now) / 900.0).ceil() * 900.0;
    let length = q.minutes as f64 * 60.0;
    while at + length <= b && slots.len() < 100 {
        if people
            .iter()
            .filter(|p| p.known)
            .all(|p| p.busy.iter().all(|(a, b)| at + length <= *a || at >= *b))
        {
            slots.push((at, at + length));
        }
        at += 900.0;
    }
    Ok(ResultSet {
        complete: people.iter().all(|p| p.known),
        people,
        slots,
        checked: now,
    })
}
pub fn request(
    s: &mut Session,
    account: i64,
    mut q: Query,
    draft: Option<i64>,
) -> Result<i64, String> {
    q.validate()?;
    if let Some(id) = draft {
        let d = edit::draft(s.store(), id).ok_or("draft missing")?;
        if !edit::editable(&d) {
            return Err("this draft is already being saved".into());
        }
        if model::source(s.store(), d.source).is_none_or(|source| source.account != account) {
            return Err("use the draft's Google account to check availability".into());
        }
    }
    let sources = model::sources(s.store());
    if !sources.iter().any(|c| c.account == account) {
        return Err("choose a connected Google account".into());
    }
    // Include all connected owned calendars, independent of list filters.
    q.guests
        .extend(sources.into_iter().filter(|c| c.role == "owner").map(|c| {
            if c.remote == "primary" {
                c.email
            } else {
                c.remote
            }
        }));
    q.guests.sort();
    q.guests.dedup();
    if q.guests.len() > 50 {
        return Err("Google allows at most 50 calendars per availability request".into());
    }
    let body = serde_json::to_string(&q).unwrap();
    s.act(Action::writing(
        "calendar.availability",
        "check calendar availability",
        move |c| {
            c.execute(
                "INSERT INTO calendar_availability(account,request,draft) VALUES(?1,?2,?3)",
                params![account, body, draft],
            )?;
            Ok(c.last_insert_rowid())
        },
    ))
    .ok_or("could not request availability".into())
}
pub fn load(s: &Store, id: i64) -> Option<(Query, Option<ResultSet>, String, Option<i64>)> {
    s.rows_sql(
        "calendar availability",
        "free/busy request, per-calendar coverage, suggested slots and freshness",
        "SELECT request,response,error,draft FROM calendar_availability WHERE id=?",
        &[Val::I(id)],
        |r| {
            Ok((
                serde_json::from_str::<Query>(&r.get::<_, String>(0)?).ok(),
                r.get::<_, Option<String>>(1)?
                    .and_then(|v| serde_json::from_str::<ResultSet>(&v).ok()),
                r.get::<_, String>(2)?,
                r.get::<_, Option<i64>>(3)?,
            ))
        },
    )
    .first()
    .and_then(|(q, r, e, d)| q.clone().map(|q| (q, r.clone(), e.clone(), *d)))
}
pub fn apply(s: &mut Session, id: i64, slot: usize) -> Result<i64, String> {
    let (q, r, e, draft) = load(s.store(), id).ok_or("availability request missing")?;
    if !e.is_empty() {
        return Err(e);
    }
    let r = r.ok_or("still checking availability")?;
    let (a, b) = *r.slots.get(slot).ok_or("time slot missing")?;
    let d = edit::draft(s.store(), draft.ok_or("this request has no event draft")?)
        .ok_or("draft missing")?;
    if a < s.now() || s.now() - r.checked > 300.0 {
        return Err("these times are out of date; check availability again".into());
    }
    if d.form
        .guests
        .split([',', ';', '\n'])
        .map(|g| g.trim().trim_start_matches('?'))
        .filter(|g| !g.is_empty())
        .any(|g| !q.guests.iter().any(|held| held.eq_ignore_ascii_case(g)))
    {
        return Err("the guest list changed; check availability for the new guests".into());
    }
    let mut form = d.form;
    form.start = dates::editor_time(a, &q.zone);
    form.end = dates::editor_time(b, &q.zone);
    form.zone = q.zone;
    form.all_day = false;
    edit::save(s, d.id, d.revision, d.source, form)?;
    Ok(d.id)
}
pub fn wire(q: &Query) -> Result<Value, String> {
    let (a, b) = q.validate()?;
    Ok(
        json!({"timeMin":dates::rfc(a),"timeMax":dates::rfc(b),"calendarExpansionMax":50,"items":q.guests.iter().map(|id|json!({"id":id})).collect::<Vec<_>>()}),
    )
}
