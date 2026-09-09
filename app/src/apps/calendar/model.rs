use super::dates;
use kernel::{
    filter::Op,
    richtable::{Datasource, Dir, SqlSource, SqlSpec, TagDef, TagSql, TagType, Values},
    store::{Store, Val},
};
use rusqlite::{params, Connection};
use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Source {
    pub id: i64,
    pub account: i64,
    pub email: String,
    pub remote: String,
    pub title: String,
    pub zone: String,
    pub role: String,
    pub meet: bool,
    pub error: String,
    pub checked: Option<f64>,
}
impl Source {
    pub fn writable(&self) -> bool {
        matches!(self.role.as_str(), "owner" | "writer")
    }
}
pub fn sources(store: &Store) -> Vec<Source> {
    store.rows_sql("calendar sources","connected Google calendars, access rights and freshness", "SELECT c.id,c.account,a.email,c.remote,c.title,c.zone,c.role,c.meet,c.error,c.checked FROM calendar_source c JOIN account a ON a.id=c.account WHERE c.active=1 AND a.calendar_enabled=1 ORDER BY a.id,c.title",&[],|r|Ok(Source{id:r.get(0)?,account:r.get(1)?,email:r.get(2)?,remote:r.get(3)?,title:r.get(4)?,zone:r.get(5)?,role:r.get(6)?,meet:r.get(7)?,error:r.get(8)?,checked:r.get(9)?})).as_ref().clone()
}
pub fn source(store: &Store, id: i64) -> Option<Source> {
    sources(store).into_iter().find(|c| c.id == id)
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Event {
    pub id: i64,
    pub source: i64,
    pub calendar: String,
    pub email: String,
    pub title: String,
    pub start: f64,
    pub end: f64,
    pub day: String,
    pub all_day: bool,
    pub location: String,
    pub guests: String,
    pub response: String,
    pub meet: String,
    pub series: String,
    pub zone: String,
    pub remote: String,
    pub etag: String,
}
impl Event {
    pub fn when(&self) -> String {
        if self.all_day {
            let last = dates::day(self.end - 1.0, &self.zone);
            if last == self.day {
                format!("{} · all day", self.day)
            } else {
                format!("{} – {} · all day", self.day, last)
            }
        } else {
            format!(
                "{} – {}",
                dates::label(self.start, &self.zone),
                dates::local(self.end, &self.zone)
                    .split('T')
                    .nth(1)
                    .unwrap_or("")
            )
        }
    }
}
pub static EVENTS:SqlSource<Event,i64>=SqlSource{
 spec:&SqlSpec{id:"calendar events",describe:"cached event occurrences, earliest first; cancelled events and disconnected accounts excluded",select:"e.id,e.source,c.title,a.email,e.title,e.start,e.end,e.day,e.all_day,e.location,e.guests,e.response,e.meet,e.series,c.zone,e.remote,e.etag",from:"calendar_event e JOIN calendar_source c ON c.id=e.source JOIN account a ON a.id=c.account",base:"e.active=1 AND c.active=1 AND a.calendar_enabled=1",text:&["e.title","e.location","e.guests"],index:None,
 tags:&[("calendar",TagSql::Col("c.title")),("source",TagSql::Col("e.source")),("account",TagSql::Col("a.email")),("with",TagSql::Col("e.guests")),("date",TagSql::Col("e.start")),("after",TagSql::Col("e.end")),("before",TagSql::Col("e.start")),("meet",TagSql::Where("e.meet<>''")),("invited",TagSql::Where("e.response='needsAction'")),("all_day",TagSql::Where("e.all_day=1"))],order:&[("e.start",Dir::Asc),("e.id",Dir::Asc)],group:None,key:"e.id",deps:&[]},
 tags:&[
 TagDef{name:"calendar",kind:TagType::Text,ops:&[Op::Eq],describe:"calendar name",values:Values::Dynamic},
 TagDef{name:"source",kind:TagType::Number,ops:&[Op::Eq],describe:"calendar source ID",values:Values::None},
 TagDef{name:"account",kind:TagType::Text,ops:&[Op::Eq],describe:"Google account address",values:Values::Dynamic},
 TagDef{name:"with",kind:TagType::Text,ops:&[Op::Eq],describe:"participant name or address",values:Values::None},
 TagDef{name:"date",kind:TagType::Date,ops:&[Op::Eq,Op::Gt,Op::Gte,Op::Lt,Op::Lte],describe:"start date",values:Values::None},
 TagDef{name:"after",kind:TagType::Number,ops:&[Op::Gt,Op::Gte],describe:"end after Unix timestamp",values:Values::None},
 TagDef{name:"before",kind:TagType::Number,ops:&[Op::Lt],describe:"start before Unix timestamp",values:Values::None},
 TagDef{name:"meet",kind:TagType::Bool,ops:&[],describe:"has a video meeting",values:Values::None},
 TagDef{name:"invited",kind:TagType::Bool,ops:&[],describe:"awaiting your response",values:Values::None},
 TagDef{name:"all_day",kind:TagType::Bool,ops:&[],describe:"all-day events",values:Values::None},
 ],map:|r|Ok(Event{id:r.get(0)?,source:r.get(1)?,calendar:r.get(2)?,email:r.get(3)?,title:r.get(4)?,start:r.get(5)?,end:r.get(6)?,day:r.get(7)?,all_day:r.get(8)?,location:r.get(9)?,guests:r.get(10)?,response:r.get(11)?,meet:r.get(12)?,series:r.get(13)?,zone:r.get(14)?,remote:r.get(15)?,etag:r.get(16)?}),key:|e|e.id,rank:|e|vec![Val::F(e.start),Val::I(e.id)],suggest:|s,tag,typed|sources(s).into_iter().filter_map(|c|match tag{"account"=>Some(c.email),"calendar"=>Some(c.title),_=>None}).filter(|v|v.to_lowercase().contains(&typed.to_lowercase())).map(kernel::richtable::Suggestion::value).collect()
};
pub fn event(s: &Store, id: i64) -> Option<Event> {
    EVENTS.by_key(s, &id)
}
pub fn raw(s: &Store, id: i64) -> Value {
    s.rows_sql(
        "calendar event body",
        "the server event, including attendees, conference, recurrence and ETag",
        "SELECT raw FROM calendar_event WHERE id=?",
        &[Val::I(id)],
        |r| r.get::<_, String>(0),
    )
    .first()
    .and_then(|v| serde_json::from_str(v).ok())
    .unwrap_or(Value::Null)
}
pub fn text<'a>(v: &'a Value, key: &str) -> &'a str {
    v[key].as_str().unwrap_or("")
}
pub fn ingest(c: &Connection, source: i64, zone: &str, v: &Value) -> rusqlite::Result<()> {
    let remote = text(v, "id");
    if remote.is_empty() {
        return Ok(());
    }
    if text(v, "status") == "cancelled" {
        c.execute(
            "UPDATE calendar_event SET active=0 WHERE source=?1 AND remote=?2",
            params![source, remote],
        )?;
        return Ok(());
    }
    let ((start, all_day), (end, _)) =
        match (dates::read(&v["start"], zone), dates::read(&v["end"], zone)) {
            (Ok(a), Ok(b)) => (a, b),
            _ => {
                return Err(rusqlite::Error::InvalidParameterName(
                    "Google event has invalid dates".into(),
                ))
            }
        };
    let guests = v["attendees"]
        .as_array()
        .map(|a| {
            a.iter()
                .map(|g| format!("{} {}", text(g, "displayName"), text(g, "email")))
                .collect::<Vec<_>>()
                .join(", ")
        })
        .unwrap_or_default();
    let response = v["attendees"]
        .as_array()
        .and_then(|a| a.iter().find(|g| g["self"] == true))
        .map(|g| text(g, "responseStatus"))
        .unwrap_or("");
    let meet = v["conferenceData"]["entryPoints"]
        .as_array()
        .and_then(|a| a.iter().find(|p| p["entryPointType"] == "video"))
        .map(|p| text(p, "uri"))
        .unwrap_or_else(|| text(v, "hangoutLink"));
    let day = if all_day {
        text(&v["start"], "date").into()
    } else {
        dates::day(start, zone)
    };
    c.execute("INSERT INTO calendar_event(source,remote,title,start,end,day,all_day,location,guests,response,meet,series,etag,raw) VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14) ON CONFLICT(source,remote) DO UPDATE SET title=excluded.title,start=excluded.start,end=excluded.end,day=excluded.day,all_day=excluded.all_day,location=excluded.location,guests=excluded.guests,response=excluded.response,meet=excluded.meet,series=excluded.series,etag=excluded.etag,raw=excluded.raw,active=1",params![source,remote,text(v,"summary"),start,end,day,all_day,text(v,"location"),guests,response,meet,text(v,"recurringEventId"),text(v,"etag"),v.to_string()])?;
    Ok(())
}
pub fn refresh(s: &mut kernel::session::Session) {
    s.act(kernel::session::Action::writing(
        "calendar.refresh",
        "refresh calendars",
        |c| {
            c.execute(
                "UPDATE calendar_sync SET requested=requested+1 WHERE id=1",
                [],
            )?;
            Ok(())
        },
    ));
}
#[derive(Clone)]
pub struct Coverage {
    pub start: f64,
    pub end: f64,
    pub requested: i64,
    pub completed: i64,
    pub checked: Option<f64>,
    pub error: String,
}
impl Coverage {
    pub fn pending(&self) -> bool {
        self.requested != self.completed
    }
}
pub fn coverage(s: &Store) -> Option<Coverage> {
    s.rows_sql(
        "calendar freshness",
        "requested date coverage, refresh generations and last synchronization",
        "SELECT start,end,requested,completed,checked,error FROM calendar_sync WHERE id=1",
        &[],
        |r| {
            Ok(Coverage {
                start: r.get(0)?,
                end: r.get(1)?,
                requested: r.get(2)?,
                completed: r.get(3)?,
                checked: r.get(4)?,
                error: r.get(5)?,
            })
        },
    )
    .first()
    .cloned()
}

/// Browsing expands the cache in the background, without recording an undo
/// action. Check before writing: a visible month asks on every draw.
pub fn cover(s: &mut kernel::session::Session, start: f64, end: f64) -> bool {
    if !start.is_finite() || !end.is_finite() || end <= start || !s.store().is_writable() {
        return false;
    }
    let Some(range) = coverage(s.store()) else {
        return false;
    };
    if range.start != 0.0 && range.start <= start && range.end >= end {
        return false;
    }
    let result = s.store().write(move |c| {
        c.execute(
            "UPDATE calendar_sync SET start=CASE WHEN start=0 THEN ?1 ELSE MIN(start,?1) END,end=MAX(end,?2),requested=requested+1 WHERE id=1 AND (start=0 OR start>?1 OR end<?2)",
            params![start, end],
        )
    });
    match result {
        Ok(n) if n > 0 => {
            s.workers().kick("calendar-sync");
            s.redraw();
            true
        }
        Err(e) => {
            s.notify(format!("could not load calendar dates: {e}"), true);
            false
        }
        _ => false,
    }
}
pub fn sync_line(s: &Store) -> String {
    coverage(s)
        .map(|range| {
            if range.end <= 0.0 {
                return "waiting for first Calendar sync".into();
            }
            if range.pending() {
                format!("loading events through {}…", dates::day(range.end, "UTC"))
            } else if !range.error.is_empty() {
                range.error
            } else {
                format!(
                    "{} · loaded through {}",
                    range
                        .checked
                        .map(|t| format!("updated {}", dates::label(t, "UTC")))
                        .unwrap_or("waiting for sync".into()),
                    dates::day(range.end, "UTC")
                )
            }
        })
        .unwrap_or_default()
}
