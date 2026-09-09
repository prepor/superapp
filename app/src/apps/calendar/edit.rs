//! Persistent drafts and the single UI/tool write path.
use super::{dates, model};
use kernel::{
    effect::World,
    history::Intent,
    session::{Edit as SessionEdit, Session},
    store::{Store, Val},
};
use rusqlite::params;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct Form {
    pub title: String,
    pub start: String,
    pub end: String,
    pub zone: String,
    pub all_day: bool,
    pub location: String,
    pub notes: String,
    pub guests: String,
    pub meet: bool,
    pub recurrence: String,
    pub busy: bool,
    pub visibility: String,
    pub reminders: String,
    pub notify: bool,
    pub scope: String,
    pub guests_modify: bool,
    pub guests_invite: bool,
    pub guests_see: bool,
}
impl Default for Form {
    fn default() -> Self {
        Self {
            title: String::new(),
            start: String::new(),
            end: String::new(),
            zone: "UTC".into(),
            all_day: false,
            location: String::new(),
            notes: String::new(),
            guests: String::new(),
            meet: false,
            recurrence: String::new(),
            busy: true,
            visibility: "default".into(),
            reminders: "default".into(),
            notify: true,
            scope: "this".into(),
            guests_modify: false,
            guests_invite: true,
            guests_see: true,
        }
    }
}
impl Form {
    pub fn fresh(now: f64, zone: &str) -> Self {
        let start = (now / 1800.0).ceil() * 1800.0;
        Self {
            start: dates::editor_time(start, zone),
            end: dates::editor_time(start + 3600.0, zone),
            zone: zone.into(),
            ..Self::default()
        }
    }
    pub fn from_event(v: &Value, zone: &str) -> Self {
        let start = dates::read(&v["start"], zone).unwrap_or((0.0, false));
        let end = dates::read(&v["end"], zone).unwrap_or((0.0, false));
        let zone = v["start"]["timeZone"].as_str().unwrap_or(zone);
        Self {
            title: model::text(v, "summary").into(),
            start: if start.1 {
                model::text(&v["start"], "date").into()
            } else {
                dates::editor_time(start.0, zone)
            },
            end: if start.1 {
                model::text(&v["end"], "date").into()
            } else {
                dates::editor_time(end.0, zone)
            },
            zone: zone.into(),
            all_day: start.1,
            location: model::text(v, "location").into(),
            notes: model::text(v, "description").into(),
            guests: v["attendees"]
                .as_array()
                .map(|a| {
                    a.iter()
                        .map(|g| {
                            format!(
                                "{}{}",
                                if g["optional"] == true { "?" } else { "" },
                                model::text(g, "email")
                            )
                        })
                        .collect::<Vec<_>>()
                        .join(", ")
                })
                .unwrap_or_default(),
            meet: !model::text(v, "hangoutLink").is_empty() || v["conferenceData"].is_object(),
            recurrence: v["recurrence"]
                .as_array()
                .map(|r| {
                    r.iter()
                        .filter_map(Value::as_str)
                        .collect::<Vec<_>>()
                        .join("\n")
                })
                .unwrap_or_else(|| {
                    if model::text(v, "recurringEventId").is_empty() {
                        String::new()
                    } else {
                        "unchanged".into()
                    }
                }),
            busy: v["transparency"] != "transparent",
            visibility: v["visibility"].as_str().unwrap_or("default").into(),
            reminders: if v["reminders"]["useDefault"] == true {
                "default".into()
            } else {
                v["reminders"]["overrides"]
                    .as_array()
                    .map(|r| {
                        r.iter()
                            .map(|x| {
                                format!(
                                    "{}:{}",
                                    model::text(x, "method"),
                                    x["minutes"].as_i64().unwrap_or(0)
                                )
                            })
                            .collect::<Vec<_>>()
                            .join(", ")
                    })
                    .unwrap_or_default()
            },
            guests_modify: v["guestsCanModify"] == true,
            guests_invite: v["guestsCanInviteOthers"] != false,
            guests_see: v["guestsCanSeeOtherGuests"] != false,
            ..Self::default()
        }
    }
    pub fn bounds(&self) -> Result<(f64, f64), String> {
        dates::zone(&self.zone)?;
        let start = dates::read(
            &dates::wire(&self.start, &self.zone, self.all_day)?,
            &self.zone,
        )?
        .0;
        let end = dates::read(
            &dates::wire(&self.end, &self.zone, self.all_day)?,
            &self.zone,
        )?
        .0;
        if end <= start {
            return Err("the end must be after the start (all-day end dates are exclusive)".into());
        }
        Ok((start, end))
    }
    pub fn validate(&self) -> Result<(), String> {
        if self.title.trim().is_empty() {
            return Err("give the event a title".into());
        }
        self.bounds()?;
        if !["default", "public", "private", "confidential"].contains(&self.visibility.as_str()) {
            return Err("visibility must be default, public, private or confidential".into());
        }
        if !["this", "all", "following"].contains(&self.scope.as_str()) {
            return Err("choose this event, all events, or following events".into());
        }
        self.attendees(&json!({}))?;
        self.reminder_value()?;
        if !self.recurrence.trim().is_empty() && self.recurrence != "unchanged" {
            recurrence_set(&self.start, &self.zone, self.all_day, &self.recurrence)?;
        }
        Ok(())
    }
    fn attendees(&self, base: &Value) -> Result<Value, String> {
        let mut list = Vec::new();
        let mut seen = std::collections::HashSet::new();
        for part in self
            .guests
            .split([',', ';', '\n'])
            .map(str::trim)
            .filter(|p| !p.is_empty())
        {
            let optional = part.starts_with('?');
            let email = part.trim_start_matches('?');
            if email.chars().any(char::is_whitespace)
                || !email
                    .split_once('@')
                    .is_some_and(|(a, b)| !a.is_empty() && b.contains('.'))
            {
                return Err(format!("invalid guest address: {email}"));
            }
            if !seen.insert(email.to_lowercase()) {
                continue;
            }
            let mut g = base["attendees"]
                .as_array()
                .and_then(|a| {
                    a.iter()
                        .find(|g| model::text(g, "email").eq_ignore_ascii_case(email))
                })
                .cloned()
                .unwrap_or_else(|| json!({"email":email,"responseStatus":"needsAction"}));
            g["optional"] = json!(optional);
            list.push(g);
        }
        Ok(json!(list))
    }
    fn reminder_value(&self) -> Result<Value, String> {
        if self.reminders.trim() == "default" {
            return Ok(json!({"useDefault":true}));
        }
        let mut list = Vec::new();
        for r in self
            .reminders
            .split(',')
            .map(str::trim)
            .filter(|r| !r.is_empty())
        {
            let (method, n) = r.split_once(':').unwrap_or(("popup", r));
            let minutes: i64 = n
                .parse()
                .map_err(|_| "reminders use minutes, for example popup:10, email:60")?;
            if !["popup", "email"].contains(&method) || !(0..=40320).contains(&minutes) {
                return Err("reminders must be popup/email and between 0 and 40320 minutes".into());
            }
            list.push(json!({"method":method,"minutes":minutes}));
        }
        if list.len() > 5 {
            return Err("Google allows at most five reminders".into());
        }
        Ok(json!({"useDefault":false,"overrides":list}))
    }
    pub fn patch(&self, base: &Value, op: &str) -> Result<Value, String> {
        self.validate()?;
        let mut body = json!({"summary":self.title.trim(),"start":dates::wire(&self.start,&self.zone,self.all_day)?,"end":dates::wire(&self.end,&self.zone,self.all_day)?,"location":self.location,"description":self.notes,"attendees":self.attendees(base)?,"transparency":if self.busy{"opaque"}else{"transparent"},"visibility":self.visibility,"reminders":self.reminder_value()?,"guestsCanModify":self.guests_modify,"guestsCanInviteOthers":self.guests_invite,"guestsCanSeeOtherGuests":self.guests_see});
        // PATCH preserves fields this editor does not expose. Arrays we do edit
        // retain the original attendee objects, including response/organizer data.
        if self.recurrence != "unchanged"
            && (model::text(base, "recurringEventId").is_empty() || self.scope != "this")
        {
            body["recurrence"] = json!(self
                .recurrence
                .lines()
                .map(str::trim)
                .filter(|r| !r.is_empty())
                .map(|r| if r.contains(':') {
                    r.to_string()
                } else {
                    format!("RRULE:{r}")
                })
                .collect::<Vec<_>>());
        }
        if self.meet {
            if !base["conferenceData"].is_object() {
                body["conferenceData"] = json!({"createRequest":{"requestId":op,"conferenceSolutionKey":{"type":"hangoutsMeet"}}});
            }
        } else if base["conferenceData"].is_object() {
            body["conferenceData"] = Value::Null;
        }
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
        body["extendedProperties"] = json!(props);
        Ok(body)
    }
}
pub fn recurrence_set(
    start: &str,
    zone: &str,
    all_day: bool,
    rule: &str,
) -> Result<rrule::RRuleSet, String> {
    let start = if all_day {
        format!("{}T000000", start.replace('-', ""))
    } else {
        dates::utc(dates::instant(start, zone)?)
            .with_timezone(&dates::zone(zone)?)
            .format("%Y%m%dT%H%M%S")
            .to_string()
    };
    let rule = rule
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .map(|l| {
            if l.contains(':') {
                l.to_string()
            } else {
                format!("RRULE:{l}")
            }
        })
        .collect::<Vec<_>>()
        .join("\n");
    format!("DTSTART;TZID={zone}:{start}\n{rule}")
        .parse()
        .map_err(|e| format!("invalid recurrence: {e}"))
}
#[derive(Clone, Debug, Serialize)]
pub struct Draft {
    pub id: i64,
    pub event: Option<i64>,
    pub source: i64,
    pub revision: i64,
    pub form: Form,
    pub base: std::sync::Arc<Value>,
    pub state: String,
    pub error: String,
    pub updated: f64,
}
pub fn draft(s: &Store, id: i64) -> Option<Draft> {
    s.rows_sql("calendar draft","persistent event draft, revision and submission state","SELECT id,event,source,revision,form,base,state,error,updated FROM calendar_draft WHERE id=?",&[Val::I(id)],|r|Ok(Draft{id:r.get(0)?,event:r.get(1)?,source:r.get(2)?,revision:r.get(3)?,form:serde_json::from_str(&r.get::<_,String>(4)?).unwrap_or_default(),base:std::sync::Arc::new(serde_json::from_str(&r.get::<_,String>(5)?).unwrap_or(Value::Null)),state:r.get(6)?,error:r.get(7)?,updated:r.get(8)?})).first().cloned()
}
pub fn can_edit(source: &model::Source, v: &Value) -> bool {
    source.writable()
        && (v["organizer"]["self"] == true
            || v["guestsCanModify"] == true
            || model::text(&v["organizer"], "email") == source.email)
}
pub fn prepare(s: &World, source: i64, event: Option<i64>) -> Result<(Form, Value), String> {
    let c = model::source(s.store(), source).ok_or("select a connected calendar")?;
    if !c.writable() {
        return Err("this calendar is read-only".into());
    }
    let base = event
        .map(|id| model::raw(s.store(), id))
        .unwrap_or(json!({}));
    if let Some(id) = event {
        if model::event(s.store(), id).is_none_or(|e| e.source != source) || !can_edit(&c, &base) {
            return Err("you cannot edit this event".into());
        }
    }
    let f = if event.is_some() {
        Form::from_event(&base, &c.zone)
    } else {
        Form::fresh(s.now(), &c.zone)
    };
    Ok((f, base))
}
pub fn create_plan(s: &World, source: i64, event: Option<i64>) -> Result<SessionEdit<Draft>, String> {
    let (form, base) = prepare(s, source, event)?;
    create_form_plan(s, source, event, form, base)
}
pub fn create_form_plan(s: &World, source: i64, event: Option<i64>, form: Form, base: Value) -> Result<SessionEdit<Draft>, String> {
    let now = s.now();
    let text = serde_json::to_string(&form).map_err(|e| e.to_string())?;
    let encoded_base = base.to_string();
    Ok(SessionEdit::writing("calendar.draft", "create event draft", move |tx| {
        check_source(tx, source, true)?;
        tx.execute("INSERT INTO calendar_draft(event,source,form,base,updated) VALUES(?1,?2,?3,?4,?5)",
            params![event, source, text, encoded_base, now])?;
        Ok(Draft { id: tx.last_insert_rowid(), event, source, revision: 1, form, base: std::sync::Arc::new(base),
            state: "draft".into(), error: String::new(), updated: now })
    }))
}

/// Domain preparation runs on an owned blocking world. Both UI actions and
/// tools use these same plans; the session only receives a ready transaction.
pub async fn background<T: Send + 'static>(world: &World,
    work: impl FnOnce(&World) -> Result<T, String> + Send + 'static) -> Result<T, String> {
    if let Some(factory) = world.factory() {
        kernel::runtime::spawn_blocking(move || {
            let world = factory.build().map_err(|error| error.to_string())?;
            work(&world)
        }).await.map_err(|error| error.to_string())?
    } else { work(world) }
}

pub fn submit<T: Send + 'static>(s: &mut Session,
    prepare: impl FnOnce(&World) -> Result<SessionEdit<T>, String> + Send + 'static,
    complete: impl FnOnce(&mut Session, Result<T, String>) + 'static) {
    s.prepare_work(move |world| Box::pin(background(world, prepare)), move |s, result| {
        match result {
            Ok(edit) => s.act_async_result(edit, move |s, result| complete(s, result.map_err(|error| error.to_string()))),
            Err(error) => complete(s, Err(error)),
        }
    });
}

#[cfg(test)]
pub fn fixture<T: Send + 'static>(s: &mut Session,
    prepare: impl FnOnce(&World) -> Result<SessionEdit<T>, String> + Send + 'static) -> Result<T, String> {
    let result = std::rc::Rc::new(std::cell::RefCell::new(None));
    let output = result.clone();
    submit(s, prepare, move |_, result| *output.borrow_mut() = Some(result));
    let result = result.borrow_mut().take().expect("fixture edit completed immediately");
    result
}
#[cfg(test)]
pub fn create(s: &mut Session, source: i64, event: Option<i64>) -> Result<i64, String> {
    fixture(s, move |w| create_plan(w, source, event)).map(|draft| draft.id)
}
pub fn editable(d: &Draft) -> bool {
    d.state == "draft"
        || d.state == "failed"
            && super::sync::rejected(&d.error)
            && !super::sync::needs_review(&d.error)
}
pub fn save_plan(
    s: &World,
    id: i64,
    revision: i64,
    source: i64,
    form: Form,
) -> Result<SessionEdit<Draft>, String> {
    let before = draft(s.store(), id).ok_or("draft not found")?;
    if before.revision != revision {
        return Err("this draft changed; read it again before editing".into());
    }
    if !editable(&before) {
        if super::sync::needs_review(&before.error) {
            return Err("review the latest event and start a new edit; this draft retains the rejected version".into());
        }
        return Err("this operation may already have reached Google; retry it to resolve its status before editing".into());
    }
    if model::source(s.store(), source).is_none_or(|c| !c.writable()) {
        return Err("calendar is disconnected or read-only".into());
    }
    if before.event.is_some() && before.source != source {
        return Err("duplicate the event to change its calendar".into());
    }
    let now = s.now();
    let text = serde_json::to_string(&form).unwrap();
    let mut after = before.clone();
    after.form = form.clone();
    after.source = source;
    after.revision += 1;
    after.updated = now;
    after.state = "draft".into();
    after.error.clear();
    let supersede = before.state == "failed";
    let (state, error) = (before.state.clone(), before.error.clone());
    Ok(SessionEdit::writing("calendar.update_draft", "edit event draft", move |c| {
        check_source(c, source, true)?;
        // Worker completions change state/error without incrementing the form
        // revision. Recheck the reviewed status when this queued edit commits.
        let changed = c.execute("UPDATE calendar_draft SET source=?1,form=?2,revision=revision+1,updated=?3,state='draft',error='' WHERE id=?4 AND revision=?5 AND state=?6 AND error=?7", params![source,text,now,id,revision,state,error])?;
        if changed == 0 { return Err(sql_error("draft changed before it could be saved")); }
        if supersede { c.execute("UPDATE calendar_change SET state='superseded' WHERE draft=? AND state='failed'", [id])?; }
        Ok(after)
    }).claiming(vec![Box::new(DraftEdit { before, after: form, source })]))
}
#[cfg(test)]
pub fn save(s: &mut Session, id: i64, revision: i64, source: i64, form: Form) -> Result<i64, String> {
    fixture(s, move |w| save_plan(w, id, revision, source, form)).map(|draft| draft.revision)
}
fn sql_error(why: &str) -> rusqlite::Error {
    rusqlite::Error::SqliteFailure(rusqlite::ffi::Error::new(rusqlite::ffi::SQLITE_ERROR), Some(why.into()))
}

fn check_source(c: &rusqlite::Connection, source: i64, writable: bool) -> rusqlite::Result<()> {
    let valid: bool = c.query_row("SELECT EXISTS(SELECT 1 FROM calendar_source c JOIN account a ON a.id=c.account
        WHERE c.id=?1 AND c.active=1 AND a.calendar_enabled=1 AND (?2=0 OR c.role IN ('owner','writer')))",
        params![source, writable], |row| row.get(0))?;
    if valid { Ok(()) } else { Err(sql_error("calendar is disconnected or read-only")) }
}

fn check_event(c: &rusqlite::Connection, event: i64, source: i64, etag: &str) -> rusqlite::Result<()> {
    let valid: bool = c.query_row("SELECT EXISTS(SELECT 1 FROM calendar_event WHERE id=?1 AND source=?2 AND etag=?3 AND active=1)",
        params![event, source, etag], |row| row.get(0))?;
    if valid { Ok(()) } else { Err(sql_error("event changed; read the latest version before continuing")) }
}
struct DraftEdit {
    before: Draft,
    after: Form,
    source: i64,
}
impl DraftEdit {
    fn put(&self, w: &World, before: bool) -> Result<(), String> {
        let id = self.before.id;
        let source = if before {
            self.before.source
        } else {
            self.source
        };
        let text = serde_json::to_string(if before {
            &self.before.form
        } else {
            &self.after
        })
        .unwrap();
        w.store().write(move|c|{let n=c.execute("UPDATE calendar_draft SET form=?1,source=?2,revision=revision+1 WHERE id=?3 AND state='draft'",params![text,source,id])?;if n==0{return Err(rusqlite::Error::InvalidParameterName("submitted drafts cannot be undone".into()));}Ok(())}).map_err(|e|e.to_string())
    }
}
impl Intent for DraftEdit {
    fn describe(&self) -> String {
        "event draft edit".into()
    }
    fn reverse(&self, w: &World) -> Result<(), String> {
        self.put(w, true)
    }
    fn reapply(&self, w: &World) -> Result<(), String> {
        self.put(w, false)
    }
}
pub fn operation_id() -> Result<String, String> {
    use ring::rand::SecureRandom;
    let mut bytes = [0u8; 16];
    ring::rand::SystemRandom::new()
        .fill(&mut bytes)
        .map_err(|_| "cannot generate an event ID")?;
    Ok(format!(
        "superapp{}",
        bytes.iter().map(|b| format!("{b:02x}")).collect::<String>()
    ))
}
pub fn commit_plan(s: &World, id: i64, revision: i64) -> Result<SessionEdit<i64>, String> {
    let d = draft(s.store(), id).ok_or("draft not found")?;
    if d.revision != revision {
        return Err("draft revision changed; review the latest draft".into());
    }
    if d.state != "draft" {
        return Err(
            "this draft has already been submitted; inspect its result before retrying".into(),
        );
    }
    let c = model::source(s.store(), d.source).ok_or("calendar disconnected")?;
    if !c.writable() || d.event.is_some() && !can_edit(&c, &d.base) {
        return Err("you cannot edit this event".into());
    }
    if d.form.meet && !c.meet && !d.base["conferenceData"].is_object() {
        return Err("Google Meet is unavailable on this calendar".into());
    }
    d.form.validate()?;
    if !model::text(&d.base, "recurringEventId").is_empty()
        && d.form.scope == "this"
        && d.form.recurrence != "unchanged"
    {
        return Err(
            "changing repeat requires all events or following events as the edit scope".into(),
        );
    }
    if let Some(event) = d.event {
        if model::event(s.store(), event).is_none_or(|e| e.etag != model::text(&d.base, "etag")) {
            return Err("event changed on Google; reopen it before saving".into());
        }
    }
    let now = s.now();
    let source = d.source;
    let event = d.event;
    let etag = model::text(&d.base, "etag").to_string();
    let body =
        json!({"form":d.form,"base":d.base,"reviewed":d.updated,"operation":operation_id()?})
            .to_string();
    Ok(SessionEdit::writing("calendar.commit","save event to Google Calendar",move|tx|{
 check_source(tx, source, true)?;
 if let Some(event) = event { check_event(tx, event, source, &etag)?; }
 let n=tx.execute("UPDATE calendar_draft SET state='pending',error='' WHERE id=?1 AND revision=?2 AND state='draft'",params![id,revision])?;if n!=1{return Err(sql_error("draft revision changed; review the latest draft"));}
 tx.execute("INSERT INTO calendar_change(draft,source,event,kind,body,updated) VALUES(?1,?2,?3,'save',?4,?5)",params![id,source,event,body,now])?;Ok(tx.last_insert_rowid())}).claiming(vec![Box::new(Submitted)]))
}
pub fn command_plan(
    s: &World,
    id: i64,
    etag: &str,
    kind: &str,
    scope: &str,
    response: &str,
    notify: bool,
) -> Result<SessionEdit<i64>, String> {
    let e = model::event(s.store(), id).ok_or("event not found")?;
    let source = model::source(s.store(), e.source).ok_or("calendar disconnected")?;
    let base = model::raw(s.store(), id);
    if e.etag != etag {
        return Err("event changed; read the latest version before continuing".into());
    }
    if !["this", "all", "following"].contains(&scope) {
        return Err("invalid recurrence scope".into());
    }
    if kind == "delete" && !can_edit(&source, &base) {
        return Err("you cannot delete this event".into());
    }
    if kind == "respond"
        && (!["accepted", "tentative", "declined"].contains(&response)
            || !base["attendees"]
                .as_array()
                .is_some_and(|a| a.iter().any(|g| g["self"] == true)))
    {
        return Err("this event has no invitation you can answer".into());
    }
    let now = s.now();
    let body =
        json!({"base":base,"scope":scope,"response":response,"notify":notify,"reviewed":now,"operation":operation_id()?})
            .to_string();
    let kind = kind.to_string();
    let source = e.source;
    let etag = etag.to_string();
    Ok(SessionEdit::writing("calendar.change","update Google event",move|c|{
        check_source(c, source, kind == "delete")?;
        check_event(c, id, source, &etag)?;
        c.execute("INSERT INTO calendar_change(source,event,kind,body,updated) VALUES(?1,?2,?3,?4,?5)",params![source,id,kind,body,now])?;Ok(c.last_insert_rowid())}).claiming(vec![Box::new(Submitted)]))
}

#[cfg(test)]
pub fn commit(s: &mut Session, id: i64, revision: i64) -> Result<i64, String> {
    fixture(s, move |w| commit_plan(w, id, revision))
}
#[cfg(test)]
pub fn command(s: &mut Session, id: i64, etag: &str, kind: &str, scope: &str, response: &str, notify: bool) -> Result<i64, String> {
    let (etag, kind, scope, response) = (etag.to_string(), kind.to_string(), scope.to_string(), response.to_string());
    fixture(s, move |w| command_plan(w, id, &etag, &kind, &scope, &response, notify))
}

/// A provider write can already have reached Google when history is opened.
/// Returning through workspace history must never imply that it was cancelled.
pub struct Submitted;
impl Intent for Submitted {
    fn describe(&self) -> String {
        "Google Calendar change".into()
    }
    fn blocked(&self, _: &World) -> Option<String> {
        Some("Google Calendar changes require an explicit edit or deletion; workspace undo cannot recall them".into())
    }
    fn reverse(&self, _: &World) -> Result<(), String> {
        Ok(())
    }
    fn reapply(&self, _: &World) -> Result<(), String> {
        Ok(())
    }
}
