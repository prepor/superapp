use super::{availability, completion, edit, model, panels, scoped, sync};
use kernel::{effect::World, nav::Nav, session::{Edit, Session}, tool::{Prepare, Prepared, Read, Tool}};
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
fn form_schema(required: &[&str]) -> Value {
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
    props.insert("scope".into(), json!({"type":"string","enum":["this","all","following"]}));
    props.insert("visibility".into(), json!({"type":"string","enum":["default","public","private","confidential"]}));
    schema(json!(props), required)
}
fn reading(input: &Value, read: fn(&World, &Value) -> Result<Value, String>) -> Read {
    let input = input.clone();
    Box::new(move |world| Box::pin(edit::background(world, move |world| read(world, &input))))
}
fn preparing(input: &Value, prepare: fn(&World, &Value) -> Result<Prepared, String>) -> Prepare {
    let input = input.clone();
    Box::new(move |world| Box::pin(edit::background(world, move |world| prepare(world, &input))))
}
fn reveal(edit: Edit<Value>, panel: fn(&Value) -> kernel::panel::PanelId) -> Prepared {
    Prepared::Edit(edit.on_commit(move |reply| {
        let id = panel(reply);
        Box::new(move |s: &mut Session| {
            if let Some(from) = s.focus() { s.nav_within(Nav::Open { from, id, fresh: false }); }
        })
    }))
}

pub fn all() -> Vec<Tool> {
    let mut availability = Tool::preparing("calendar.availability", "Queue immutable Google free/busy for a range up to 14 days using a connected account. Includes the linked draft's current guests and all owned calendars; unknown is not free. Opens the slot picker. Read calendar.availability_result for completion.",
        schema(json!({"account":number(),"draft":number(),"start":string(),"end":string(),"zone":string(),"minutes":number(),"guests":{"type":"array","items":string()}}), &["account","start","end","zone","minutes","guests"]), |v| preparing(v, request));
    availability.writes = false;
    vec![
        Tool::reading("calendar.calendars", "List connected Google calendars with source IDs, accounts, access roles, Meet support and refresh times.", schema(json!({}), &[]), |v| reading(v, |w,_| Ok(json!(model::sources(w.store()))))),
        Tool::reading("calendar.suggest", "Suggest editor values from cached guests, Mail correspondents, locations, IANA zones and time presets. Does not query Google.", schema(json!({"field":{"type":"string","enum":["guests","zone","location","recurrence","reminders","duration","time"]},"text":string(),"cursor":{"type":"integer","minimum":0}}), &["field","text"]), |v| reading(v, suggest)),
        Tool::reading("calendar.events", "Read cached occurrences in an explicit range and request missing dates. loading=true means synchronization is pending; query again and inspect coverage. start/end are RFC3339 or local ISO plus IANA zone. This is not availability.", schema(json!({"start":string(),"end":string(),"zone":string(),"filter":string(),"offset":number(),"limit":number()}), &["start","end","zone"]), events_reader),
        Tool::reading("calendar.event", "Read one cached event revision, raw Google fields and effective editing rights before editing, deleting or responding.", schema(json!({"event":number()}), &["event"]), |v| reading(v, event)),
        Tool::preparing("calendar.create", "Create an event on an explicit writable Google calendar. form requires title, start, end, IANA zone and notify. Times use local ISO or RFC3339; all_day uses exclusive end dates. Guests are comma-separated emails (? prefix for optional), recurrence uses RRULE lines, reminders default or popup:10,email:60. Supports the other editor fields, including Meet. Returns draft and queued operation IDs; read calendar.operation until done or failed. May send invitations; requires approval.", schema(json!({"source":number(),"form":form_schema(&["title","start","end","zone","notify"])}), &["source","form"]), |v| preparing(v, create_event)).asking(),
        Tool::preparing("calendar.update", "Modify an event using the current ETag from calendar.event. changes uses the same form fields as calendar.create; omitted fields are preserved. Requires explicit changes.scope (this, all, following) and changes.notify. Returns draft and queued operation IDs; read calendar.operation until done or failed. May notify guests; requires approval.", schema(json!({"event":number(),"etag":string(),"changes":form_schema(&["scope","notify"])}), &["event","etag","changes"]), |v| preparing(v, update_event)).asking(),
        Tool::preparing("calendar.draft", "Create a persistent local draft on an explicit writable source. Optional event edits that occurrence. Opens the native editor; sends no invitations.", schema(json!({"source":number(),"event":number()}), &["source"]), |v| preparing(v, draft)),
        Tool::preparing("calendar.update_draft", "Read or update a local draft using its revision. Omit changes to read. start/end use local ISO (all-day exclusive end dates), guests comma-separated emails (? prefix for optional), recurrence RRULE lines, reminders default or popup:10,email:60. No Google write.", schema(json!({"draft":number(),"revision":number(),"source":number(),"changes":form_schema(&[])}), &["draft"]), |v| preparing(v, update)),
        Tool::preparing("calendar.commit", "Submit the exact reviewed draft revision to Google. May send invitations and create Meet. Inspect calendar.operation until done or failed. Requires approval.", schema(json!({"draft":number(),"revision":number()}), &["draft","revision"]), |v| preparing(v, |w,v| Ok(Prepared::Edit(edit::commit_plan(w,id(v,"draft")?,id(v,"revision")?)?.map(|id| json!({"operation":id})))))).asking(),
        Tool::preparing("calendar.delete", "Delete an event or recurring scope on Google using the current ETag from calendar.event and explicit scope and notify choices. Returns a queued operation ID; read calendar.operation until done or failed. May send cancellations; requires approval.", schema(json!({"event":number(),"etag":string(),"scope":{"type":"string","enum":["this","all","following"]},"notify":{"type":"boolean"}}), &["event","etag","scope","notify"]), |v| preparing(v, |w,v| command(w,v,"delete"))).asking(),
        Tool::preparing("calendar.respond", "Send RSVP using the latest event ETag and accepted/tentative/declined response.", schema(json!({"event":number(),"etag":string(),"response":{"type":"string","enum":["accepted","tentative","declined"]}}), &["event","etag","response"]), |v| preparing(v, |w,v| command(w,v,"respond"))).asking(),
        Tool::reading("calendar.operation", "Read queued Google write status. Pending/processing has not completed; failures retain the draft and error.", schema(json!({"operation":number()}), &["operation"]), |v| reading(v, operation)),
        Tool::preparing("calendar.retry", "Retry a failed Google write with its original idempotency key. May send invitations or cancellations; requires approval.", schema(json!({"operation":number()}), &["operation"]), |v| preparing(v, |_,v| Ok(Prepared::Edit(sync::retry_plan(id(v,"operation")?).map(|()| json!({"queued":true})))))).asking(),
        availability,
        Tool::reading("calendar.availability_result", "Read checked free/busy, account attempts, shared event details and slots. Optional minutes recalculates candidates without refreshing data. Unknown is not free; no slot is reserved.", schema(json!({"request":number(),"minutes":{"type":"integer","minimum":15,"maximum":480}}), &["request"]), |v| reading(v, availability_result)),
        Tool::preparing("calendar.use_time", "Apply a proposed time to the original local draft. Snaps to 15 minutes inside fresh checked bounds; requires unchanged guests/account. Returns conflicts and unknown calendars. Use calendar.commit to submit this draft to Google.", schema(json!({"request":number(),"start":string(),"minutes":{"type":"integer","minimum":15,"maximum":480}}), &["request","start","minutes"]), |v| preparing(v, use_time)),
    ]
}

fn suggest(w: &World, v: &Value) -> Result<Value,String> {
    use kernel::richtable::Completion;
    let kind = match v["field"].as_str().unwrap_or("") {
        "guests"=>completion::Field::Guests,"zone"=>completion::Field::Zone,"location"=>completion::Field::Location,
        "recurrence"=>completion::Field::Repeat,"reminders"=>completion::Field::Reminders,"duration"=>completion::Field::Duration,"time"=>completion::Field::Time,
        _=>return Err("unknown completion field".into()),
    };
    let text=v["text"].as_str().unwrap_or("");
    let cursor=v["cursor"].as_u64().map(|n|n as usize).unwrap_or(text.len());
    let choices=kind.context(text,cursor).map(|ctx|kind.offer(w.store(),&ctx)).unwrap_or_default();
    Ok(json!({"suggestions":choices.iter().map(|c|json!({"label":c.label,"value":c.value,"description":c.describe})).collect::<Vec<_>>()}))
}
fn event_range(v: &Value) -> Result<(f64, f64), String> {
    let zone=v["zone"].as_str().unwrap_or("");
    let start=super::dates::instant(v["start"].as_str().unwrap_or(""),zone)?;
    let end=super::dates::instant(v["end"].as_str().unwrap_or(""),zone)?;
    if end<=start { return Err("end must be after start".into()); }
    let parsed=kernel::filter::parse(v["filter"].as_str().unwrap_or(""));
    if !parsed.errors.is_empty(){return Err(format!("invalid filter: {:?}",parsed.errors));}
    Ok((start, end))
}
fn events_reader(v: &Value) -> Read {
    let v = v.clone();
    Box::new(move |world| Box::pin(async move {
        let input = v.clone();
        let (start, end) = edit::background(world, move |_| event_range(&input)).await?;
        if world.store().is_writable() {
            world.store().write_async(move |tx| model::cover_tx(tx, start, end)).await.map_err(|e| e.to_string())?;
        }
        edit::background(world, move |world| events(world, &v)).await
    }))
}
fn events(w: &World, v: &Value) -> Result<Value,String> {
    use kernel::richtable::Datasource;
    let zone=v["zone"].as_str().unwrap_or("");
    let (start, end) = event_range(v)?;
    let parsed=kernel::filter::parse(v["filter"].as_str().unwrap_or(""));
    let source=scoped::Events{start,end:Some(end),zone:zone.into()};
    let limit=v["limit"].as_u64().unwrap_or(100).clamp(1,500) as usize;
    let offset=v["offset"].as_u64().unwrap_or(0) as usize;
    let rows=source.page(w.store(),parsed.ast.as_ref(),offset,limit);
    let total=source.count(w.store(),parsed.ast.as_ref()).unwrap_or(0);
    Ok(json!({"events":rows.as_ref(),"total":total,"truncated":offset+rows.len()<total,"loading":model::coverage(w.store()).is_some_and(|c|c.pending()),"coverage":model::sync_line(w.store())}))
}
fn event(w:&World,v:&Value)->Result<Value,String>{
    let event=id(v,"event")?;let e=model::event(w.store(),event).ok_or("event not found")?;
    let raw=model::raw(w.store(),event);
    let can_edit=model::source(w.store(),e.source).is_some_and(|c|edit::can_edit(&c,&raw));
    Ok(json!({"event":e,"raw":raw,"can_edit":can_edit}))
}
fn merge_form(form: &edit::Form, changes: &Value) -> Result<edit::Form, String> {
    let changes = changes.as_object().ok_or("event fields must be an object")?;
    let mut value = serde_json::to_value(form).map_err(|e| e.to_string())?;
    for (key, value_in) in changes { value[key] = value_in.clone(); }
    serde_json::from_value(value).map_err(|e| format!("invalid event fields: {e}"))
}
fn create_event(w: &World, v: &Value) -> Result<Prepared, String> {
    let source = id(v, "source")?;
    let (form, base) = edit::prepare(w, source, None)?;
    let form = merge_form(&form, &v["form"])?;
    submit_event(w, source, None, form, base)
}
fn update_event(w: &World, v: &Value) -> Result<Prepared, String> {
    let event = id(v, "event")?;
    let source = model::event(w.store(), event).ok_or("event not found")?.source;
    let (form, base) = edit::prepare(w, source, Some(event))?;
    if v["etag"].as_str().filter(|etag| !etag.is_empty()) != Some(model::text(&base, "etag")) {
        return Err("event changed; read calendar.event for the latest ETag before updating".into());
    }
    let form = merge_form(&form, &v["changes"])?;
    submit_event(w, source, Some(event), form, base)
}
fn submit_event(w: &World, source: i64, event: Option<i64>, form: edit::Form, base: Value) -> Result<Prepared, String> {
    Ok(Prepared::Edit(edit::submit_form_plan(w, source, event, form, base)?
        .map(|(draft, operation)| json!({"draft":draft,"operation":operation,"queued":true}))))
}
fn draft(w:&World,v:&Value)->Result<Prepared,String>{
    Ok(reveal(edit::create_plan(w,id(v,"source")?,v["event"].as_i64())?.map(|draft|json!(draft)), |v|panels::Editor::id(v["id"].as_i64().expect("draft ID"))))
}
fn update(w:&World,v:&Value)->Result<Prepared,String>{
    let id=id(v,"draft")?;
    let draft=edit::draft(w.store(),id).ok_or("draft not found")?;
    if !v["changes"].is_object() {return Ok(Prepared::Reply(json!(draft)));}
    let revision=super::tools::id(v,"revision")?;
    let form=merge_form(&draft.form,&v["changes"])?;
    Ok(Prepared::Edit(edit::save_plan(w,id,revision,v["source"].as_i64().unwrap_or(draft.source),form)?.map(|draft|json!(draft))))
}
fn command(w:&World,v:&Value,kind:&str)->Result<Prepared,String>{
    Ok(Prepared::Edit(edit::command_plan(w,id(v,"event")?,v["etag"].as_str().unwrap_or(""),kind,
        if kind=="respond"{"this"}else{v["scope"].as_str().unwrap_or("")},v["response"].as_str().unwrap_or(""),kind=="respond"||v["notify"]==true)?.map(|id|json!({"operation":id}))))
}
fn operation(w:&World,v:&Value)->Result<Value,String>{
    w.store().rows_sql("calendar tool operation","queued Calendar write status","SELECT id,state,error,result FROM calendar_change WHERE id=?",&[kernel::store::Val::I(id(v,"operation")?)],|r|Ok(json!({"id":r.get::<_,i64>(0)?,"state":r.get::<_,String>(1)?,"error":r.get::<_,String>(2)?,"result":serde_json::from_str::<Value>(&r.get::<_,String>(3)?).unwrap_or(Value::Null)}))).first().cloned().ok_or("operation not found".into())
}
fn request(w:&World,v:&Value)->Result<Prepared,String>{
    let q=availability::Query{start:v["start"].as_str().unwrap_or("").into(),end:v["end"].as_str().unwrap_or("").into(),zone:v["zone"].as_str().unwrap_or("").into(),minutes:v["minutes"].as_u64().unwrap_or(0) as u32,guests:v["guests"].as_array().into_iter().flatten().filter_map(Value::as_str).map(str::to_string).collect(),draft_guests:None};
    Ok(reveal(availability::request_plan(w,id(v,"account")?,q,v["draft"].as_i64())?.map(|id|json!({"request":id})),|v|panels::Availability::id(v["request"].as_i64().expect("request ID"))))
}
fn availability_result(w:&World,v:&Value)->Result<Value,String>{
    let request=id(v,"request")?;
    let(q,r,error,_)=availability::load(w.store(),request).ok_or("request not found")?;
    if let Some(minutes)=v.get("minutes") {
        let mut search=availability::Search::from_query(&q);search.minutes=minutes.to_string();
        let(q,r,_)=availability::preview(w.store(),request,&search,w.now())?;
        return Ok(json!({"query":q,"result":r,"error":""}));
    }
    Ok(json!({"query":q,"result":r,"error":error}))
}
fn use_time(w:&World,v:&Value)->Result<Prepared,String>{
    let request=id(v,"request")?;
    let(q,_,_,_)=availability::load(w.store(),request).ok_or("request not found")?;
    let mut search=availability::Search::from_query(&q);search.minutes=v["minutes"].to_string();
    let(q,r,_)=availability::preview(w.store(),request,&search,w.now())?;
    let start=super::dates::instant(v["start"].as_str().unwrap_or(""),&q.zone)?;
    let start=availability::snap(&q,start,w.now()).ok_or("no time fits this window")?;
    let end=start+f64::from(q.minutes)*60.0;
    let conflicts:Vec<_>=r.people.iter().filter(|p|p.known && p.busy.iter().any(|(a,b)|start<*b && end>*a)).map(|p|p.calendar.clone()).collect();
    let unknown:Vec<_>=r.people.iter().filter(|p|!p.known).map(|p|p.calendar.clone()).collect();
    Ok(reveal(availability::apply_time_plan(w,request,&search,start)?.map(move |draft|json!({"draft":draft,"conflicts":conflicts,"unknown":unknown})),|v|panels::Editor::id(v["draft"]["id"].as_i64().expect("draft ID"))))
}
