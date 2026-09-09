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
    /// The draft's participants when this request was queued. Extra calendars
    /// supplied by an agent are independent of the event's invitation list.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub draft_guests: Option<Vec<String>>,
}
impl Query {
    pub fn validate(&self) -> Result<(f64, f64), String> {
        dates::zone(&self.zone)?;
        let a = dates::instant(&self.start, &self.zone)?;
        let b = dates::instant(&self.end, &self.zone)?;
        if b <= a || b - a > 14.0 * 86400.0 {
            return Err("availability needs a range between 15 minutes and 14 days".into());
        }
        if !(15..=480).contains(&self.minutes) {
            return Err("duration must be 15–480 minutes".into());
        }
        if b - a < f64::from(self.minutes) * 60.0 {
            return Err("the meeting duration is longer than the search window".into());
        }
        if self.guests.len() > 50 {
            return Err("availability supports up to 50 calendars".into());
        }
        Ok((a, b))
    }
}

/// Editable controls are separate from an immutable, possibly in-flight query.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Search {
    pub day: String,
    pub end_day: String,
    pub from: String,
    pub until: String,
    pub minutes: String,
    pub zone: String,
}
impl Search {
    pub fn from_query(q: &Query) -> Self {
        let local = |s: &str| {
            dates::instant(s, &q.zone)
                .map(|t| dates::editor_time(t, &q.zone))
                .unwrap_or_else(|_| s.into())
        };
        let a = local(&q.start);
        let b = local(&q.end);
        Self {
            day: a.get(..10).unwrap_or("").into(),
            end_day: b.get(..10).unwrap_or("").into(),
            from: a.get(11..).unwrap_or("").into(),
            until: b.get(11..).unwrap_or("").into(),
            minutes: q.minutes.to_string(),
            zone: q.zone.clone(),
        }
    }
    pub fn query(&self, guests: Vec<String>) -> Result<Query, String> {
        dates::date(&self.day)?;
        dates::date(&self.end_day)?;
        let q = Query {
            start: format!("{}T{}", self.day, self.from),
            end: format!("{}T{}", self.end_day, self.until),
            zone: self.zone.clone(),
            minutes: self
                .minutes
                .parse()
                .map_err(|_| "duration must be a number of minutes")?,
            guests,
            draft_guests: None,
        };
        q.validate()?;
        Ok(q)
    }
    pub fn shift(&mut self, days: i64) -> Result<(), String> {
        let a = dates::date(&self.day)? + chrono::Duration::days(days);
        let b = dates::date(&self.end_day)? + chrono::Duration::days(days);
        self.day = a.to_string();
        self.end_day = b.to_string();
        Ok(())
    }
}

pub fn guests(form: &edit::Form) -> Vec<String> {
    form.guests
        .split([',', ';', '\n'])
        .map(|g| g.trim().trim_start_matches('?').trim().to_string())
        .filter(|g| !g.is_empty())
        .collect()
}

fn normalized(guests: &[String]) -> Vec<String> {
    let mut guests: Vec<_> = guests
        .iter()
        .map(|g| g.trim().to_lowercase())
        .filter(|g| !g.is_empty())
        .collect();
    guests.sort();
    guests.dedup();
    guests
}

pub fn draft_error(store: &Store, request: i64, q: &Query, draft: Option<i64>) -> Option<String> {
    let draft = draft?;
    let Some(d) = edit::draft(store, draft) else {
        return Some("The event draft is missing.".into());
    };
    if !edit::editable(&d) {
        return Some("This draft is already being saved.".into());
    }
    let account: Option<i64> = store
        .conn()
        .query_row(
            "SELECT account FROM calendar_availability WHERE id=?",
            [request],
            |r| r.get(0),
        )
        .ok();
    if model::source(store, d.source).is_none_or(|c| Some(c.account) != account) {
        return Some("The event's Google account changed. Check availability again.".into());
    }
    let current = normalized(&guests(&d.form));
    let changed = q.draft_guests.as_ref().map_or_else(
        || {
            current
                .iter()
                .any(|g| !q.guests.iter().any(|held| held.eq_ignore_ascii_case(g)))
        },
        |held| current != normalized(held),
    );
    changed
        .then(|| "The guest list changed. Check availability again for the current guests.".into())
}

pub fn recheck(s: &mut Session, id: i64, search: &Search) -> Result<i64, String> {
    let (old, _, _, draft) = load(s.store(), id).ok_or("availability request missing")?;
    let (account, guests) = if let Some(id) = draft {
        let d = edit::draft(s.store(), id).ok_or("draft missing")?;
        let source = model::source(s.store(), d.source).ok_or("calendar disconnected")?;
        (source.account, guests(&d.form))
    } else {
        (
            s.store()
                .conn()
                .query_row(
                    "SELECT account FROM calendar_availability WHERE id=?",
                    [id],
                    |r| r.get(0),
                )
                .map_err(|e| e.to_string())?,
            old.guests,
        )
    };
    request(s, account, search.query(guests)?, draft)
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Check {
    pub account: String,
    pub error: String,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Person {
    pub calendar: String,
    pub known: bool,
    pub busy: Vec<(f64, f64)>,
    pub error: String,
    #[serde(default)]
    pub checks: Vec<Check>,
}
impl Person {
    pub fn via(&self) -> Option<&str> {
        self.checks
            .iter()
            .find(|c| c.error.is_empty())
            .map(|c| c.account.as_str())
    }
    pub fn failure(&self) -> String {
        if self.checks.is_empty() {
            return self.error.clone();
        }
        self.checks
            .iter()
            .filter(|c| !c.error.is_empty())
            .map(|c| format!("{}: {}", c.account, c.error))
            .collect::<Vec<_>>()
            .join("; ")
    }
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ResultSet {
    pub people: Vec<Person>,
    pub slots: Vec<(f64, f64)>,
    pub complete: bool,
    pub checked: f64,
}
pub fn calculate(q: &Query, answer: &Value, now: f64) -> Result<ResultSet, String> {
    let mut people = Vec::new();
    for id in &q.guests {
        let c = answer["calendars"]
            .as_object()
            .and_then(|calendars| {
                calendars.get(id).or_else(|| {
                    calendars
                        .iter()
                        .find(|(k, _)| k.eq_ignore_ascii_case(id))
                        .map(|(_, v)| v)
                })
            })
            .unwrap_or(&Value::Null);
        let mut valid = c.is_object()
            && (c["errors"].is_null() || c["errors"].as_array().is_some_and(Vec::is_empty))
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
                calendar_error(c)
            },
            checks: Vec::new(),
        });
    }
    suggest(q, people, now)
}

fn calendar_error(calendar: &Value) -> String {
    let mut errors = Vec::new();
    for error in calendar["errors"].as_array().into_iter().flatten() {
        let reason: String = model::text(error, "reason")
            .chars()
            .filter(|c| c.is_ascii_alphanumeric() || *c == '_')
            .take(80)
            .collect();
        let explanation = match reason.as_str() {
            "notFound" => "Calendar not found or not accessible to this account (notFound)".into(),
            "forbidden" => "Google refused calendar access for this account (forbidden)".into(),
            "internalError" => {
                "Google could not check this calendar; try again (internalError)".into()
            }
            "groupTooBig" | "tooManyCalendarsRequested" => {
                format!("Google's availability limit was exceeded ({reason})")
            }
            "" => "Google could not check this calendar".into(),
            _ => format!("Google could not check this calendar ({reason})"),
        };
        if !errors.contains(&explanation) {
            errors.push(explanation);
        }
    }
    if !errors.is_empty() {
        return errors.join("; ");
    }
    if calendar.is_null() {
        "Google did not return availability for this calendar".into()
    } else {
        "Google returned incomplete or invalid availability".into()
    }
}

pub fn suggest(q: &Query, people: Vec<Person>, now: f64) -> Result<ResultSet, String> {
    let (a, b) = q.validate()?;
    let mut slots = Vec::new();
    let mut at = (a.max(now) / 900.0).ceil() * 900.0;
    let length = q.minutes as f64 * 60.0;
    while people.iter().any(|p| p.known) && at + length <= b && slots.len() < 100 {
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
        complete: !people.is_empty() && people.iter().all(|p| p.known),
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
        q.draft_guests = Some(normalized(&guests(&d.form)));
        q.guests.extend(guests(&d.form));
    } else {
        q.draft_guests = None;
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
    q.guests.retain(|g| !g.trim().is_empty());
    for guest in &mut q.guests {
        *guest = guest.trim().to_string();
    }
    q.guests.sort_by_key(|g| g.to_lowercase());
    q.guests.dedup_by(|a, b| a.eq_ignore_ascii_case(b));
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
    if let Some(error) = draft_error(s.store(), id, &q, draft) {
        return Err(error);
    }
    let r = r.ok_or("still checking availability")?;
    let (a, b) = *r.slots.get(slot).ok_or("time slot missing")?;
    let d = edit::draft(s.store(), draft.ok_or("this request has no event draft")?)
        .ok_or("draft missing")?;
    if a < s.now() || s.now() - r.checked > 300.0 {
        return Err("these times are out of date; check availability again".into());
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
