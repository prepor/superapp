use super::{availability, completion, edit, model, panels, scoped, sync};
use kernel::{nav::Nav, session::Session, tool::Tool};
use serde_json::{json, Value};
fn schema(properties: Value, required: &[&str]) -> Value {
    json!({"type":"object","properties":properties,"required":required,"additionalProperties":false})
}
fn number() -> Value {
    json!({"type":"integer"})
}
fn string() -> Value {
    json!({"type":"string"})
}
fn id(v: &Value, k: &str) -> Result<i64, String> {
    v[k].as_i64()
        .filter(|n| *n > 0)
        .ok_or(format!("{k} must be a positive ID"))
}
fn reveal(s: &mut Session, id: kernel::panel::PanelId) {
    if let Some(from) = s.focus() {
        s.nav(Nav::Open {
            from,
            id,
            fresh: false,
        });
    }
}
fn form_schema() -> Value {
    let mut props = serde_json::Map::new();
    for field in [
        "title",
        "start",
        "end",
        "zone",
        "location",
        "notes",
        "guests",
        "recurrence",
        "visibility",
        "reminders",
        "scope",
    ] {
        props.insert(field.into(), string());
    }
    for field in [
        "all_day",
        "meet",
        "busy",
        "notify",
        "guests_modify",
        "guests_invite",
        "guests_see",
    ] {
        props.insert(field.into(), json!({"type":"boolean"}));
    }
    schema(json!(props), &[])
}
pub fn all() -> Vec<Tool> {
    vec![
 Tool::new("calendar.calendars","List connected Google calendars with source IDs, account addresses, access roles, Meet support and last refresh.",schema(json!({}),&[]),false,|s,_|Ok(json!(model::sources(s.store())))),
 Tool::new("calendar.suggest","Suggest event field values from the same completion source as the editor. Guests match names or addresses in cached events, non-spam Mail correspondents and connected accounts; existing guests are excluded. Locations are recent event locations. Also offers IANA zones and readable recurrence, reminder, duration and time presets. No contact lookup or Google write occurs.",schema(json!({"field":{"type":"string","enum":["guests","zone","location","recurrence","reminders","duration","time"]},"text":string(),"cursor":{"type":"integer","minimum":0}}),&["field","text"]),false,|s,v|{
  use kernel::richtable::Completion;
  let kind=match v["field"].as_str().unwrap_or(""){ "guests"=>completion::Field::Guests,"zone"=>completion::Field::Zone,"location"=>completion::Field::Location,"recurrence"=>completion::Field::Repeat,"reminders"=>completion::Field::Reminders,"duration"=>completion::Field::Duration,"time"=>completion::Field::Time,_=>return Err("unknown completion field".into()) };
  let text=v["text"].as_str().unwrap_or("");let cursor=v["cursor"].as_u64().map(|n|n as usize).unwrap_or(text.len());
  let choices=kind.context(text,cursor).map(|ctx|kind.offer(s.store(),&ctx)).unwrap_or_default();
  Ok(json!({"suggestions":choices.iter().map(|c|json!({"label":c.label,"value":c.value,"description":c.describe})).collect::<Vec<_>>()}))}),
 Tool::new("calendar.events","Read occurrences in an explicit time range, automatically requesting missing dates in the background. Returns current cached events and loading=true while synchronization is pending; query again after it finishes and inspect coverage for errors. start/end are RFC3339 or local ISO datetimes plus an IANA zone. This is not an availability check. Filter supports @calendar, @account, @with, @invited and @meet.",schema(json!({"start":string(),"end":string(),"zone":string(),"filter":string(),"offset":number(),"limit":number()}),&["start","end","zone"]),false,|s,v|{
  use kernel::richtable::Datasource;
  let start=super::dates::instant(v["start"].as_str().unwrap_or(""),v["zone"].as_str().unwrap_or(""))?;let end=super::dates::instant(v["end"].as_str().unwrap_or(""),v["zone"].as_str().unwrap_or(""))?;if end<=start{return Err("end must be after start".into());}
  let parsed=kernel::filter::parse(v["filter"].as_str().unwrap_or(""));if !parsed.errors.is_empty(){return Err(format!("invalid filter: {:?}",parsed.errors));}
  model::cover(s,start,end);
  let source=scoped::Events{start,end:Some(end),zone:v["zone"].as_str().unwrap_or("UTC").into()};let limit=v["limit"].as_u64().unwrap_or(100).clamp(1,500) as usize;let offset=v["offset"].as_u64().unwrap_or(0) as usize;let rows=source.page(s.store(),parsed.ast.as_ref(),offset,limit);let total=source.count(s.store(),parsed.ast.as_ref()).unwrap_or(0);
  Ok(json!({"events":rows.as_ref(),"total":total,"truncated":offset+rows.len()<total,"loading":model::coverage(s.store()).is_some_and(|c|c.pending()),"coverage":model::sync_line(s.store())}))}),
 Tool::new("calendar.event","Read one event's current cached revision, raw Google fields, source and effective editing rights. Use this before editing, deleting or responding.",schema(json!({"event":number()}),&["event"]),false,|s,v|{let id=id(v,"event")?;let e=model::event(s.store(),id).ok_or("event not found")?;let raw=model::raw(s.store(),id);let can_edit=model::source(s.store(),e.source).is_some_and(|c|edit::can_edit(&c,&raw));Ok(json!({"event":e,"raw":raw,"can_edit":can_edit}))}),
 Tool::new("calendar.draft","Create a persistent local draft on an explicit writable source. Optional event edits that occurrence. Opens the same native editor used by the person; does not send invitations.",schema(json!({"source":number(),"event":number()}),&["source"]),true,|s,v|{let draft=edit::create(s,id(v,"source")?,v["event"].as_i64())?;reveal(s,panels::Editor::id(draft));Ok(json!(edit::draft(s.store(),draft)))}),
 Tool::new("calendar.update_draft","Read or update a local event draft using its current revision. Omit changes to read it. start/end use YYYY-MM-DDTHH:MM; all-day uses dates with an EXCLUSIVE end. guests is comma-separated email addresses (prefix ? for optional), recurrence is RFC5545 RRULE lines, reminders is default or popup:10,email:60. scope is this/all/following. No Google write occurs.",schema(json!({"draft":number(),"revision":number(),"source":number(),"changes":form_schema()}),&["draft"]),true,|s,v|{let draft=id(v,"draft")?;let d=edit::draft(s.store(),draft).ok_or("draft not found")?;if let Some(changes)=v["changes"].as_object(){let revision=id(v,"revision")?;let mut value=serde_json::to_value(&d.form).unwrap();for(k,v)in changes{value[k]=v.clone();}let form=serde_json::from_value(value).map_err(|e|format!("invalid draft: {e}"))?;edit::save(s,draft,revision,v["source"].as_i64().unwrap_or(d.source),form)?;}Ok(json!(edit::draft(s.store(),draft)))}),
 Tool::new("calendar.commit","Submit the exact reviewed draft revision to Google. May send invitations, updates and create Meet. Returns a queued operation; inspect calendar.operation until done or failed. Requires approval.",schema(json!({"draft":number(),"revision":number()}),&["draft","revision"]),true,|s,v|Ok(json!({"operation":edit::commit(s,id(v,"draft")?,id(v,"revision")?)?}))).asking(),
 Tool::new("calendar.delete","Delete an event or recurring scope on Google. May send cancellations. Requires the latest event ETag and explicit scope (this/all/following).",schema(json!({"event":number(),"etag":string(),"scope":{"type":"string","enum":["this","all","following"]},"notify":{"type":"boolean"}}),&["event","etag","scope","notify"]),true,|s,v|Ok(json!({"operation":edit::command(s,id(v,"event")?,v["etag"].as_str().unwrap_or(""),"delete",v["scope"].as_str().unwrap_or(""),"",v["notify"]==true)?}))).asking(),
 Tool::new("calendar.respond","Send your RSVP for an invitation. Requires the latest event ETag and accepted/tentative/declined response.",schema(json!({"event":number(),"etag":string(),"response":{"type":"string","enum":["accepted","tentative","declined"]}}),&["event","etag","response"]),true,|s,v|Ok(json!({"operation":edit::command(s,id(v,"event")?,v["etag"].as_str().unwrap_or(""),"respond","this",v["response"].as_str().unwrap_or(""),true)?}))).asking(),
 Tool::new("calendar.operation","Read the status of an event write. A pending/processing operation has not completed; failures retain the draft and a readable error.",schema(json!({"operation":number()}),&["operation"]),false,|s,v|{let rows=s.store().rows_sql("calendar tool operation","queued Calendar write status","SELECT id,state,error,result FROM calendar_change WHERE id=?",&[kernel::store::Val::I(id(v,"operation")?)],|r|Ok(json!({"id":r.get::<_,i64>(0)?,"state":r.get::<_,String>(1)?,"error":r.get::<_,String>(2)?,"result":serde_json::from_str::<Value>(&r.get::<_,String>(3)?).unwrap_or(Value::Null)})));rows.first().cloned().ok_or("operation not found".into())}),
 Tool::new("calendar.retry","Retry a failed Google write with its original idempotency key. May send invitations or cancellations; approval is required.",schema(json!({"operation":number()}),&["operation"]),true,|s,v|{sync::retry(s,id(v,"operation")?)?;Ok(json!({"queued":true}))}).asking(),
 Tool::new("calendar.availability","Queue an immutable Google free/busy request in an explicit range (up to 14 days) using a connected account. Includes the linked draft's current guests and owned calendars even when hidden by filters. Tries unreadable guests through the other connected Calendar accounts and records each attempt. Opens the participant timeline and slot picker. Missing sharing permission stays unknown; suggestions may have partial coverage, and none are offered if no calendars are readable. Query result with calendar.availability_result; applying a time only updates a local draft.",schema(json!({"account":number(),"draft":number(),"start":string(),"end":string(),"zone":string(),"minutes":number(),"guests":{"type":"array","items":string()}}),&["account","start","end","zone","minutes","guests"]),false,|s,v|{let q=availability::Query{start:v["start"].as_str().unwrap_or("").into(),end:v["end"].as_str().unwrap_or("").into(),zone:v["zone"].as_str().unwrap_or("").into(),minutes:v["minutes"].as_u64().unwrap_or(0) as u32,guests:v["guests"].as_array().into_iter().flatten().filter_map(Value::as_str).map(str::to_string).collect(),draft_guests:None};let id=availability::request(s,id(v,"account")?,q,v["draft"].as_i64())?;reveal(s,panels::Availability::id(id));Ok(json!({"request":id}))}),
 Tool::new("calendar.availability_result","Read checked free/busy, account attempts and candidate slots. Each person's details contain state (pending/ready/unavailable) and shared event titles, exact bounds, locations and the account that supplied them; unreadable details do not change busy status. Optional minutes recalculates candidates from the same fresh checked intervals without another Google request or renewed freshness. Unknown is not free; no slot is reserved.",schema(json!({"request":number(),"minutes":{"type":"integer","minimum":15,"maximum":480}}),&["request"]),false,|s,v|{
  let request=id(v,"request")?;let(q,r,error,_)=availability::load(s.store(),request).ok_or("request not found")?;
  if let Some(minutes)=v.get("minutes"){let mut search=availability::Search::from_query(&q);search.minutes=minutes.to_string();let(q,r,_)=availability::preview(s.store(),request,&search,s.now())?;return Ok(json!({"query":q,"result":r,"error":""}));}
  Ok(json!({"query":q,"result":r,"error":error}))}),
 Tool::new("calendar.use_time","Apply a proposed time to the availability request's original local draft, like dragging and using this time in the scheduling sheet. start is RFC3339 or local ISO in the checked zone; snaps to 15 minutes and clamps the full duration to the checked window. Requires fresh results and unchanged guests/account. A manually selected time may overlap busy events: returns conflicts and unknown calendars. This only updates the local draft; calendar.commit separately submits it to Google.",schema(json!({"request":number(),"start":string(),"minutes":{"type":"integer","minimum":15,"maximum":480}}),&["request","start","minutes"]),true,|s,v|{
  let request=id(v,"request")?;let(q,_,_,_)=availability::load(s.store(),request).ok_or("request not found")?;let mut search=availability::Search::from_query(&q);search.minutes=v["minutes"].to_string();let(q,r,_)=availability::preview(s.store(),request,&search,s.now())?;
  let start=super::dates::instant(v["start"].as_str().unwrap_or(""),&q.zone)?;let start=availability::snap(&q,start,s.now()).ok_or("no time fits this window")?;let end=start+f64::from(q.minutes)*60.0;
  let conflicts:Vec<_>=r.people.iter().filter(|p|p.known && p.busy.iter().any(|(a,b)|start<*b && end>*a)).map(|p|p.calendar.clone()).collect();let unknown:Vec<_>=r.people.iter().filter(|p|!p.known).map(|p|p.calendar.clone()).collect();
  let draft=availability::apply_time(s,request,&search,start)?;reveal(s,panels::Editor::id(draft));Ok(json!({"draft":edit::draft(s.store(),draft),"conflicts":conflicts,"unknown":unknown}))}),
]
}
