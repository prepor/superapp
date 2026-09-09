use super::{dates, model};
use kernel::{app::Mode, store::Store};
use rusqlite::{params, OptionalExtension};
use serde_json::{json, Value};

pub fn events(now: f64) -> Vec<Value> {
    let day = dates::day(now, "Europe/Berlin");
    let date = dates::date(&day).unwrap();
    let timed = |id: &str, title: &str, days: i64, time: &str, minutes: i64| {
        let d = date + chrono::Duration::days(days);
        let at = dates::instant(&format!("{d}T{time}"), "Europe/Berlin").unwrap();
        json!({"id":id,"summary":title,"start":{"dateTime":dates::rfc(at),"timeZone":"Europe/Berlin"},"end":{"dateTime":dates::rfc(at+minutes as f64*60.0),"timeZone":"Europe/Berlin"},"organizer":{"email":"me@demo.invalid","self":true},"etag":"\"1\"","status":"confirmed","reminders":{"useDefault":true},"eventType":"default"})
    };
    let mut design = timed("design", "Design review", 0, "14:00", 45);
    design["recurrence"] = json!(["RRULE:FREQ=WEEKLY;COUNT=8"]);
    design["description"] = json!("<p>Review the calendar timeline and upcoming work.</p><p><a href=\"https://example.com/agenda\">Review agenda</a><br>Project notes: https://example.com/notes</p>");
    design["location"] = json!("https://example.com/meeting-room");
    design["attendees"] = json!([{"email":"nora@studio.example","displayName":"Nora","responseStatus":"accepted"},{"email":"leo@studio.example","displayName":"Leo","responseStatus":"tentative"}]);
    design["conferenceData"] = json!({"entryPoints":[{"entryPointType":"video","uri":"https://meet.google.com/demo-review"}]});
    let mut invitation = timed("research", "Research catch-up", 0, "15:30", 30);
    invitation["organizer"] = json!({"email":"nora@studio.example"});
    invitation["attendees"] =
        json!([{"email":"me@demo.invalid","self":true,"responseStatus":"needsAction"}]);
    let mut allhands = timed("allhands", "Studio all-hands", 2, "11:00", 60);
    allhands["organizer"] = json!({"email":"studio@studio.example"});
    let next = date + chrono::Duration::days(5);
    vec![
        design,
        invitation,
        timed("planning", "Planning", 1, "10:00", 60),
        timed("walk", "A walk by the river", 1, "18:00", 60),
        allhands,
        timed("dentist", "Dentist", 7, "09:00", 45),
        json!({"id":"trip","summary":"Weekend away","start":{"date":next.to_string()},"end":{"date":(next+chrono::Duration::days(2)).to_string()},"organizer":{"self":true,"email":"me@demo.invalid"},"etag":"\"1\""}),
    ]
}
pub fn seed(s: &Store, mode: Mode) -> rusqlite::Result<()> {
    let now = kernel::time::virtual_epoch();
    s.write(move|c|{
 if mode!=Mode::Fake{return Ok(());}
 c.execute("UPDATE calendar_sync SET start=?1,end=?2 WHERE id=1 AND start=0 AND end=0",params![now-90.0*86400.0,now+366.0*86400.0])?;
 if c.query_row("SELECT COUNT(*) FROM calendar_source",[],|r|r.get::<_,i64>(0))?>0{return Ok(());}
 let account=match c.query_row("SELECT id FROM account ORDER BY id LIMIT 1",[],|r|r.get::<_,i64>(0)).optional()? { Some(id)=>id, None=>crate::identity::accounts::add_account_tx(c,"me@prepor.dev","","","google")? };
 c.execute("UPDATE account SET calendar_enabled=1,scopes=?1 WHERE id=?2",params![crate::identity::required_scopes(true,true),account])?;
 for (remote,title,role,meet) in [("primary","Work","owner",true),("studio","Studio","reader",false)]{c.execute("INSERT INTO calendar_source(account,remote,title,zone,role,meet) VALUES(?1,?2,?3,'Europe/Berlin',?4,?5)",params![account,remote,title,role,meet])?;let source=c.last_insert_rowid();for v in events(now){if (v["id"]=="allhands")== (remote=="studio") && v["recurrence"].is_null(){model::ingest(c,source,"Europe/Berlin",&v)?;}}}
 Ok(())})
}
