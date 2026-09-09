use super::{availability, dates, edit, model, scoped, snapshot::{Snapshot, State}};
use kernel::{
    layout::SlotId,
    nav::Nav,
    panel::{Opening, Panel, PanelId, PanelKind, Tag, Verb},
    richtable::ListState,
    session::Session,
    store::{Store, Val},
};
use serde_json::Value;
use std::{any::Any, rc::Rc, sync::Arc};
pub static KINDS: &[&dyn PanelKind] = &[
    &TimelineKind,
    &MonthKind,
    &EventKind,
    &EditorKind,
    &AvailabilityKind,
    &SourcesKind,
];
fn open(from: SlotId, id: PanelId) -> Nav {
    Nav::Open {
        from,
        id,
        fresh: false,
    }
}
fn say(s: &mut Session, result: Result<(), String>) {
    if let Err(e) = result {
        s.notify(e, true);
    }
    s.redraw();
}
fn accounts() -> PanelId {
    PanelId::bare(Tag("accounts"))
}
pub fn start_editor(s: &mut Session, from: SlotId, source: i64, event: Option<i64>) {
    edit::submit(s, move |w| edit::create_plan(w, source, event), move |s, result| {
        let result = result.map(|draft| s.nav_within(open(from, Editor::id(draft.id))));
        say(s, result);
    });
}

pub struct Timeline {
    id: PanelId,
    pub slot: SlotId,
    pub store: Rc<Store>,
    pub list: ListState<scoped::Events>,
    pub filter: String,
    pub day: Option<String>,
    pub zone: String,
    more: bool,
    zone_pending: bool,
}
impl Timeline {
    pub const TAG: Tag = Tag("calendar");
    pub fn id() -> PanelId {
        PanelId::bare(Self::TAG)
    }
    pub fn day(day: &str, zone: &str, filter: &str) -> PanelId {
        PanelId::new(Self::TAG, [filter, day, zone])
    }
    pub fn ready(&self) -> bool { !self.zone_pending }
    fn initialize(&mut self, now: f64) -> bool {
        if !self.zone_pending {return true;}
        let Some(sources) = model::display_sources(&self.store).ready() else {return false;};
        self.zone = sources.first().map(|source|source.zone.clone()).unwrap_or("UTC".into());
        let (start,end) = self.day.as_ref().and_then(|day|dates::date(day).ok()).and_then(|day|Some((
            dates::midnight(day,&self.zone).ok()?,Some(dates::midnight(day+chrono::Duration::days(1),&self.zone).ok()?))))
            .unwrap_or((now,None));
        self.list = ListState::new(scoped::Events {start,end,zone:self.zone.clone()},50);
        self.list.set_filter(&self.filter);
        self.zone_pending = false;
        true
    }
    pub fn cover(&mut self, s: &mut Session) {
        if !self.initialize(s.now()) {return;}
        if let Some((start, end)) = self.day.as_ref().and_then(|day| {
            let date = dates::date(day).ok()?;
            Some((
                dates::midnight(date, &self.zone).ok()?,
                dates::midnight(date.succ_opt()?, &self.zone).ok()?,
            ))
        }) {
            model::cover(s, start, end);
        }
    }
    pub fn reached_end(&mut self) {
        self.more = true;
    }
    /// Coalesce edge/scroll demand while a fetch is running. An empty page
    /// does not trigger another year on redraw; further scrolling can do so.
    pub fn prefetch(&mut self, s: &mut Session, near_end: bool, advanced: bool) -> bool {
        if self.day.is_some() || !near_end || !self.list.table().errors().is_empty() {
            self.more = false;
            return false;
        }
        self.more |= advanced;
        if !self.more || model::display_sources(&self.store).ready().is_none_or(|sources|sources.is_empty()) {
            return false;
        }
        let Some(range) = model::display_coverage(&self.store).ready().and_then(|range|range.as_ref().clone()) else {
            return false;
        };
        if range.pending() || range.checked.is_none() || !range.error.is_empty() {
            return false;
        }
        if model::cover(s, range.start, range.end + 366.0 * 86400.0) {
            self.more = false;
            return true;
        }
        false
    }
}
impl Panel for Timeline {
    fn id(&self) -> &PanelId {
        &self.id
    }
    fn title(&self) -> String {
        self.day.clone().unwrap_or("calendar".into())
    }
    fn about(&self) -> String {
        format!("Upcoming Google Calendar occurrences, ordered by start. Date scope: {} in {}. Filter: {}. Each row has source/account identity, invite status and a stable occurrence ID. Browsing near the timeline's end automatically fetches more dates; a day agenda fetches that day. {}. Missing events outside the cached range are not evidence of availability. Use calendar.availability for scheduling.",self.day.as_deref().unwrap_or("upcoming"),self.zone,self.list.table().filter(),model::display_sync_line(&self.store))
    }
    fn wish(&self, _: usize) -> (u32, u32) {
        (5, 6)
    }
    fn placed(&mut self, slot: SlotId) {
        self.slot = slot;
    }
    fn persist(&self) -> PanelId {
        if self.zone_pending {return self.id.clone();}
        PanelId::new(
            Self::TAG,
            [
                self.list.table().filter(),
                self.day.as_deref().unwrap_or(""),
                &self.zone,
            ],
        )
    }
    fn verbs(&self) -> Vec<Verb> {
        vec![
            Verb::run("calendar.new", "new event", Some('n')),
            Verb::go(
                "calendar.month",
                "month",
                Some('m'),
                Nav::Replace {
                    slot: self.slot,
                    id: Month::id(
                        self.day.as_deref().unwrap_or(""),
                        self.list.table().filter(),
                    ),
                },
            ),
            Verb::go(
                "calendar.sources",
                "calendars",
                Some('c'),
                open(self.slot, Sources::id()),
            ),
            Verb::run("calendar.refresh", "refresh", Some('r')),
        ]
    }
    fn run(&mut self, v: &str, s: &mut Session) {
        match v {
            "calendar.refresh" => model::refresh(s),
            "calendar.new" => {
                let slot = self.slot;
                s.prepare_work(move |w|Box::pin(edit::background(w,|w| {
                    model::sources(w.store()).iter().find(|source|source.writable())
                        .map(|source|edit::create_plan(w,source.id,None)).transpose()
                })),move |s,result|match result {
                    Ok(Some(plan))=>s.act_async_result(plan,move |s,result| {
                        let result = result.map(|draft|s.nav_within(open(slot,Editor::id(draft.id)))).map_err(|error|error.to_string());
                        say(s,result);
                    }),
                    Ok(None)=>{s.nav_within(open(slot,accounts()));s.notify("connect Google Calendar in Accounts to add events",false);},
                    Err(error)=>say(s,Err(error)),
                });
            }
            _ => {}
        }
    }
    fn as_any(&mut self) -> &mut dyn Any {
        self
    }
}
pub struct TimelineKind;
impl PanelKind for TimelineKind {
    fn tag(&self) -> Tag {
        Timeline::TAG
    }
    fn open(&self, id: &PanelId, cx: &mut Opening<'_>) -> Box<dyn Panel> {
        let s = cx.session();
        let explicit = id.args.get(2).filter(|zone|dates::zone(zone).is_ok()).cloned();
        let sources = model::display_sources(s.store()).ready();
        let zone_pending = explicit.is_none() && sources.is_none();
        let zone = explicit.or_else(||sources.as_ref().and_then(|sources|sources.first().map(|source|source.zone.clone()))).unwrap_or("UTC".into());
        let day = id.args.get(1).filter(|d| dates::date(d).is_ok()).cloned();
        let (start, end) = day
            .as_ref()
            .and_then(|d| dates::date(d).ok())
            .and_then(|d| {
                Some((
                    dates::midnight(d, &zone).ok()?,
                    Some(dates::midnight(d + chrono::Duration::days(1), &zone).ok()?),
                ))
            })
            .unwrap_or((s.now(), None));
        let filter = id.args.first().cloned().unwrap_or_default();
        let mut list = ListState::new(
            scoped::Events {
                start,
                end,
                zone: zone.clone(),
            },
            50,
        );
        list.set_filter(&filter);
        Box::new(Timeline {
            id: id.clone(),
            slot: 0,
            store: s.store().clone(),
            list,
            filter,
            day,
            zone,
            more: false,
            zone_pending,
        })
    }
}

pub struct MonthLine {pub text: String,pub source: i64}
pub struct MonthCell {pub count: usize,pub lines: Vec<MonthLine>}
pub struct MonthGrid {pub count: usize,pub days: Vec<MonthCell>}
impl MonthGrid {
    fn load(store: &Store,month: &str,zone: &str,filter: &str) -> Result<Self,String> {
        use kernel::richtable::Datasource;
        let days = dates::grid(month);
        let start = dates::midnight(days[0],zone)?;
        let end = dates::midnight(days[0]+chrono::Duration::days(42),zone)?;
        let events = scoped::Events {start,end:Some(end),zone:zone.into()}
            .page(store,kernel::filter::parse(filter).ast.as_ref(),0,10000);
        let days = days.into_iter().map(|day| {
            let a = dates::midnight(day,zone)?;
            let b = dates::midnight(day+chrono::Duration::days(1),zone)?;
            let matching: Vec<_> = events.iter().filter(|event|event.start<b && event.end>a).collect();
            Ok(MonthCell {count:matching.len(),lines:matching.iter().take(3).map(|event|MonthLine {
                text: if event.all_day {event.title.clone()} else {format!("{} {}",dates::utc(event.start).with_timezone(&dates::zone(zone).unwrap_or(chrono_tz::UTC)).format("%H:%M"),event.title)},
                source:event.source,
            }).collect()})
        }).collect::<Result<Vec<_>,String>>()?;
        Ok(Self {count:events.len(),days})
    }
}

pub struct Month {
    id: PanelId,
    pub slot: SlotId,
    pub store: Rc<Store>,
    pub month: String,
    pub filter: String,
    pub zone: String,
    zone_pending: bool,
    grid: Snapshot<(String,String,String),MonthGrid>,
}
impl Month {
    pub const TAG: Tag = Tag("calendar-month");
    pub fn id(day: &str, filter: &str) -> PanelId {
        PanelId::new(Self::TAG, [day, filter])
    }
    pub fn bounds(&self) -> Option<(f64, f64)> {
        let first = dates::grid(&self.month)[0];
        Some((
            dates::midnight(first, &self.zone).ok()?,
            dates::midnight(first + chrono::Duration::days(42), &self.zone).ok()?,
        ))
    }
    pub fn ready(&self) -> bool { !self.zone_pending }
    fn initialize(&mut self, now: f64) -> bool {
        if !self.zone_pending {return true;}
        let Some(sources) = model::display_sources(&self.store).ready() else {return false;};
        self.zone = sources.first().map(|source|source.zone.clone()).unwrap_or("UTC".into());
        let day = self.id.args.first().filter(|day|dates::date(day).is_ok()).cloned().unwrap_or_else(||dates::day(now,&self.zone));
        self.month = dates::month(&day,0).to_string();
        self.zone_pending = false;
        true
    }
    pub fn cover(&mut self, s: &mut Session) {
        if !self.initialize(s.now()) {return;}
        if let Some((start, end)) = self.bounds() {
            model::cover(s, start, end);
        }
    }
    pub fn reading(&self) -> State<MonthGrid> {
        if self.zone_pending {return State::Loading;}
        let (month,zone,filter) = (self.month.clone(),self.zone.clone(),self.filter.clone());
        self.grid.get(&self.store,(month.clone(),zone.clone(),filter.clone()),self.store.revision(&["calendar_event","calendar_source","account"]),
            move |store|MonthGrid::load(store,&month,&zone,&filter))
    }
    #[cfg(test)]
    pub fn rows(&self) -> Vec<model::Event> {
        if self.zone_pending {return Vec::new();}
        use kernel::richtable::Datasource;
        let Some((start, end)) = self.bounds() else {
            return Vec::new();
        };
        scoped::Events {
            start,
            end: Some(end),
            zone: self.zone.clone(),
        }
        .page(
            &self.store,
            kernel::filter::parse(&self.filter).ast.as_ref(),
            0,
            10000,
        )
        .as_ref()
        .clone()
    }
}
impl Panel for Month {
    fn id(&self) -> &PanelId {
        &self.id
    }
    fn title(&self) -> String {
        dates::month(&self.month, 0).format("%B %Y").to_string()
    }
    fn about(&self) -> String {
        format!("Calendar month {} in {}, Monday first. Filter: {}. Visible dates, including adjacent-month days, are fetched automatically on display and navigation. Select a day for its agenda, including overlapping multi-day events. {}",self.month,self.zone,self.filter,model::display_sync_line(&self.store))
    }
    fn wish(&self, _: usize) -> (u32, u32) {
        (7, 6)
    }
    fn placed(&mut self, slot: SlotId) {
        self.slot = slot;
    }
    fn persist(&self) -> PanelId {
        if self.zone_pending {return self.id.clone();}
        Self::id(&self.month, &self.filter)
    }
    fn verbs(&self) -> Vec<Verb> {
        vec![
            Verb::run("calendar.previous", "previous", Some('p')),
            Verb::run("calendar.next", "next", Some('n')),
            Verb::run("calendar.today", "today", Some('o')),
            Verb::go(
                "calendar.timeline",
                "timeline",
                Some('v'),
                Nav::Replace {
                    slot: self.slot,
                    id: PanelId::new(Timeline::TAG, [self.filter.clone()]),
                },
            ),
            Verb::go(
                "calendar.sources",
                "calendars",
                Some('c'),
                open(self.slot, Sources::id()),
            ),
        ]
    }
    fn run(&mut self, v: &str, s: &mut Session) {
        if !self.initialize(s.now()) {return;}
        let d = match v {
            "calendar.previous" => dates::month(&self.month, -1),
            "calendar.next" => dates::month(&self.month, 1),
            "calendar.today" => dates::month(&dates::day(s.now(), &self.zone), 0),
            _ => return,
        };
        self.month = d.to_string();
        self.cover(s);
        s.redraw();
    }
    fn as_any(&mut self) -> &mut dyn Any {
        self
    }
}
pub struct MonthKind;
impl PanelKind for MonthKind {
    fn tag(&self) -> Tag {
        Month::TAG
    }
    fn open(&self, id: &PanelId, cx: &mut Opening<'_>) -> Box<dyn Panel> {
        let s = cx.session();
        let sources = model::display_sources(s.store()).ready();
        let zone_pending = sources.is_none();
        let zone = sources.as_ref().and_then(|sources|sources.first()).map(|source|source.zone.clone()).unwrap_or("UTC".into());
        let day = id
            .args
            .first()
            .filter(|d| dates::date(d).is_ok())
            .cloned()
            .unwrap_or_else(|| dates::day(s.now(), &zone));
        Box::new(Month {
            id: id.clone(),
            slot: 0,
            store: s.store().clone(),
            month: dates::month(&day, 0).to_string(),
            filter: id.args.get(1).cloned().unwrap_or_default(),
            zone,
            zone_pending,
            grid: Snapshot::default(),
        })
    }
}

pub struct EventDisplay {
    pub event: model::Event,
    pub raw: Arc<Value>,
    pub can_edit: bool,
    pub about: String,
    pub when: String,
    pub details_html: String,
    pub notes_html: String,
    pub people: String,
}
impl EventDisplay {
    fn load(store: &Store, id: i64) -> Result<Self, String> {
        let event = model::event(store,id).ok_or("event unavailable: deleted or calendar disconnected")?;
        let raw = model::raw(store,id);
        let can_edit = model::source(store,event.source).is_some_and(|source| edit::can_edit(&source,&raw));
        let when = event.when();
        let about = format!("Google event occurrence {} on source {} ({} / {}), revision {}. {}. Recurring series: {}. Organizer: {}. Your response: {}. Use a draft for edits and read the latest revision before committing.",event.id,event.source,event.calendar,event.email,event.etag,when,event.series,raw["organizer"],event.response);
        let meet = raw["conferenceData"]["createRequest"]["status"]["statusCode"].as_str().unwrap_or("");
        let details = format!("{}{}{}\n{} · {}\n{}{}",
            if event.location.is_empty(){""}else{"Location: "},event.location,
            if event.location.is_empty(){""}else{"\n"},
            if raw["transparency"]=="transparent"{"free"}else{"busy"},
            raw["visibility"].as_str().unwrap_or("default visibility"),event.meet,
            match meet {"pending"=>"\nGoogle Meet is being created…","failure"=>"\nGoogle Meet could not be created",_=>""});
        let people = raw["attendees"].as_array().map(|people| people.iter().map(|person| {
            format!("{}{} · {}",model::text(person,"email"),if person["optional"]==true{" (optional)"}else{""},model::text(person,"responseStatus"))
        }).collect::<Vec<_>>().join("\n")).unwrap_or_default();
        Ok(Self { when, about, can_edit,
            details_html: crate::reader::html::linked_text(details.trim()),
            notes_html: crate::reader::html::linked(model::text(&raw,"description")),
            people: format!("Organizer: {}\n{}",model::text(&raw["organizer"],"email"),people),
            event, raw: Arc::new(raw) })
    }
}

pub struct Event {
    id: PanelId,
    pub event: i64,
    pub slot: SlotId,
    pub store: Rc<Store>,
    pub deleting: bool,
    pub scope: String,
    pub notify: bool,
    pub url: Option<String>,
    display: Snapshot<i64, EventDisplay>,
}
impl Event {
    pub const TAG: Tag = Tag("calendar-event");
    pub fn id(id: i64) -> PanelId {
        PanelId::new(Self::TAG, [id.to_string()])
    }
    pub fn display(&self) -> State<EventDisplay> {
        let id = self.event;
        self.display.get(&self.store, id, self.store.revision(&["calendar_event", "calendar_source", "account"]), move |store| EventDisplay::load(store,id))
    }
    pub fn reading(&self) -> Option<Arc<EventDisplay>> { self.display().ready() }
    pub fn status(&self) -> String {
        match self.display() {
            State::Loading => "loading event…".into(),
            State::Failed(error) => error,
            State::Ready(_) | State::Refreshing(_) => String::new(),
        }
    }
    pub fn operation(&self) -> Option<(i64, String, String)> {
        self.store
            .snapshot_rows_sql(
                "calendar operation",
                "latest change for this event",
                "SELECT id,state,error FROM calendar_change WHERE event=? AND state NOT IN ('superseded','dismissed') ORDER BY id DESC LIMIT 1",
                &[Val::I(self.event)],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )
            .first()
            .cloned()
    }
}
impl Panel for Event {
    fn id(&self) -> &PanelId {
        &self.id
    }
    fn title(&self) -> String {
        self.reading()
            .map(|display| display.event.title.clone())
            .unwrap_or_else(|| self.status())
    }
    fn about(&self) -> String {
        self.reading().map(|display| format!("{} Deletion scope: {}.", display.about, self.scope)).unwrap_or_else(|| self.status())
    }
    fn wish(&self, _: usize) -> (u32, u32) {
        (4, 6)
    }
    fn placed(&mut self, slot: SlotId) {
        self.slot = slot;
    }
    fn verbs(&self) -> Vec<Verb> {
        let recovery = match self.operation() {
            Some((_, state, error)) if state == "failed" && super::sync::needs_review(&error) => vec![
                Verb::run("calendar.review", "review latest", Some('r')),
                Verb::run("calendar.dismiss", "dismiss error", None),
            ],
            Some((_, state, _)) if state == "failed" => vec![Verb::run("calendar.retry", "retry", Some('r'))],
            _ => Vec::new(),
        };
        let Some(display) = self.reading() else { return recovery; };
        let (e, raw) = (&display.event, &display.raw);
        if self.deleting {
            return vec![
                Verb::run("calendar.confirm_delete", "delete event", Some('d')),
                Verb::run("calendar.cancel_delete", "cancel", Some('c')),
            ];
        }
        let mut v = vec![Verb::run("calendar.duplicate", "duplicate", Some('p'))];
        if raw["htmlLink"].as_str().is_some_and(|s| !s.is_empty()) {
            v.push(Verb::run("calendar.google", "open in Google", Some('g')));
        }
        if display.can_edit {
            v.push(Verb::run("calendar.edit", "edit", Some('e')));
            v.push(Verb::run("calendar.delete", "delete", Some('d')));
        }
        if !e.response.is_empty() {
            v.extend([
                Verb::run("calendar.yes", "yes", Some('y')),
                Verb::run("calendar.maybe", "maybe", Some('m')),
                Verb::run("calendar.no", "no", Some('n')),
            ]);
        }
        if !e.meet.is_empty() {
            v.push(Verb::run("calendar.join", "join meet", Some('j')));
        }
        v.extend(recovery);
        v
    }
    fn run(&mut self, v: &str, s: &mut Session) {
        // A rejected change still needs recovery after the event has been
        // deleted or its calendar disconnected. These actions own no event
        // mutation and must remain reachable without an EventDisplay.
        match v {
            "calendar.review" => {
                self.deleting = false;
                self.display = Snapshot::default();
                s.redraw();
                return;
            }
            "calendar.dismiss" | "calendar.retry" => {
                if let Some((id, _, _)) = self.operation() {
                    let plan = if v == "calendar.dismiss" { super::sync::dismiss_plan(id) }
                        else { super::sync::retry_plan(id) };
                    s.act_async_result(plan, |s, result| say(s, result.map_err(|error| error.to_string())));
                }
                return;
            }
            _ => {},
        }
        let Some(display) = self.reading() else { return; };
        let (e, raw) = (display.event.clone(), display.raw.clone());
        match v {
            "calendar.edit" => start_editor(s, self.slot, e.source, Some(e.id)),
            "calendar.delete" => {
                self.deleting = true;
                s.redraw();
            }
            "calendar.cancel_delete" => {
                self.deleting = false;
                s.redraw();
            }
            "calendar.confirm_delete" => {
                let (scope, notify) = (self.scope.clone(), self.notify);
                edit::submit(s, move |w| edit::command_plan(w, e.id, &e.etag, "delete", &scope, "", notify), |s, result| say(s, result.map(|_| ())));
                self.deleting = false;
            }
            "calendar.yes" | "calendar.maybe" | "calendar.no" => {
                let response = match v {
                    "calendar.yes" => "accepted",
                    "calendar.maybe" => "tentative",
                    _ => "declined",
                };
                edit::submit(s, move |w| edit::command_plan(w, e.id, &e.etag, "respond", "this", response, true), |s, result| say(s, result.map(|_| ())));
            }
            "calendar.duplicate" => {
                let slot = self.slot;
                edit::submit(s, move |w| {
                    let target = model::sources(w.store()).into_iter().find(|c| c.writable()).ok_or("no writable calendar")?;
                    let mut form = edit::Form::from_event(&model::raw(w.store(), e.id), &target.zone);
                    form.title = format!("{} (copy)", form.title);
                    form.meet = false;
                    edit::create_form_plan(w, target.id, None, form, serde_json::json!({}))
                }, move |s, result| {
                    let result = result.map(|draft| s.nav_within(open(slot, Editor::id(draft.id))));
                    say(s, result);
                });
            }
            "calendar.join" => {
                if url::Url::parse(&e.meet).is_ok_and(|u| u.scheme() == "https") {
                    self.url = Some(e.meet);
                    s.redraw();
                }
            }
            "calendar.google" => {
                if let Some(link) = raw["htmlLink"]
                    .as_str()
                    .filter(|link| url::Url::parse(link).is_ok_and(|u| u.scheme() == "https"))
                {
                    self.url = Some(link.into());
                    s.redraw();
                }
            }
            _ => {}
        }
    }
    fn as_any(&mut self) -> &mut dyn Any {
        self
    }
}
pub struct EventKind;
impl PanelKind for EventKind {
    fn tag(&self) -> Tag {
        Event::TAG
    }
    fn open(&self, id: &PanelId, cx: &mut Opening<'_>) -> Box<dyn Panel> {
        Box::new(Event {
            id: id.clone(),
            event: id.args.first().and_then(|n| n.parse().ok()).unwrap_or(0),
            slot: 0,
            store: cx.session().store().clone(),
            deleting: false,
            scope: "this".into(),
            notify: true,
            url: None,
            display: Snapshot::default(),
        })
    }
}

type Saved = Box<dyn FnOnce(&mut Session, Result<Arc<edit::Draft>, String>)>;
#[derive(Default)]
struct DraftEdits {
    pending: bool,
    submitting: bool,
    queued: Option<(i64, edit::Form)>,
    visible: Option<Arc<edit::Draft>>,
    committed: Option<(Vec<u64>, Arc<edit::Draft>)>,
    error: String,
    waiters: Vec<Saved>,
}

/// Coalesce typing behind one accepted draft save. Completion owns this state
/// even after the editor closes; shutdown drains the same callback queue.
fn save_next(s: &mut Session, state: Rc<std::cell::RefCell<DraftEdits>>, id: i64, revision: i64) {
    let Some((source, form)) = state.borrow_mut().queued.take() else { return; };
    state.borrow_mut().pending = true;
    edit::submit(s, move |w| edit::save_plan(w, id, revision, source, form), move |s, result| {
        let (next, waiters) = {
            let mut state = state.borrow_mut();
            state.pending = false;
            match &result {
                Ok(draft) if state.queued.is_some() => {
                    if let Some(visible) = state.visible.as_mut() { Arc::make_mut(visible).revision = draft.revision; }
                    (Some(draft.revision), Vec::new())
                }
                Ok(draft) => {
                    state.committed = Some((s.store().revision(&["calendar_draft"]), Arc::new(draft.clone())));
                    if !state.submitting { state.visible = None; }
                    (None, std::mem::take(&mut state.waiters))
                }
                Err(error) => { state.error = error.clone(); state.queued = None; (None, std::mem::take(&mut state.waiters)) }
            }
        };
        if let Some(revision) = next { save_next(s, state, id, revision); }
        else {
            if let Err(error) = &result { s.notify(error, true); }
            let result = result.map(Arc::new);
            for complete in waiters { complete(s, result.clone()); }
        }
        s.redraw();
    });
}

pub struct Editor {
    id: PanelId,
    pub draft: i64,
    pub slot: SlotId,
    pub store: Rc<Store>,
    pub error: String,
    edits: Rc<std::cell::RefCell<DraftEdits>>,
    display: Snapshot<i64, edit::Draft>,
}
impl Editor {
    pub const TAG: Tag = Tag("calendar-edit");
    pub fn id(id: i64) -> PanelId {
        PanelId::new(Self::TAG, [id.to_string()])
    }
    pub fn display(&self) -> State<edit::Draft> {
        let id = self.draft;
        self.display.get(&self.store, id, self.store.revision(&["calendar_draft"]), move |store| edit::draft(store,id).ok_or("event draft unavailable".into()))
    }
    pub fn reading(&self) -> Option<Arc<edit::Draft>> {
        let revision = self.store.revision(&["calendar_draft"]);
        let mut edits = self.edits.borrow_mut();
        if let Some(draft) = edits.visible.as_ref() { return Some(draft.clone()); }
        if let Some((_, draft)) = edits.committed.as_ref().filter(|(saved, _)| *saved == revision) {
            return Some(draft.clone());
        }
        match self.display() {
            State::Ready(draft) => {
                // A fresh read is authoritative, including a lower revision
                // restored by undo. Retain it for the next background refresh.
                edits.committed = Some((revision, draft.clone()));
                Some(draft)
            }
            State::Refreshing(draft) => {
                // A local save can be newer than the last parsed snapshot.
                // Other drafts changing must not briefly restore older text.
                Some(edits.committed.as_ref().map(|(_, saved)| saved.clone()).unwrap_or(draft))
            }
            State::Loading => edits.committed.as_ref().map(|(_, draft)| draft.clone()),
            State::Failed(_) => { edits.committed = None; None },
        }
    }
    pub fn problem(&self) -> String {
        {
            let edits = self.edits.borrow();
            if !edits.error.is_empty() { return edits.error.clone(); }
        }
        if !self.error.is_empty() { return self.error.clone(); }
        if self.reading().is_some() { return String::new(); }
        match self.display() { State::Loading=>"Loading event draft…".into(), State::Failed(error)=>error, State::Ready(_) | State::Refreshing(_)=>String::new() }
    }
    pub fn save(&mut self, s: &mut Session, _revision: i64, source: i64, form: edit::Form) {
        let Some(draft) = self.reading() else { return; };
        let mut draft = (*draft).clone();
        if self.edits.borrow().submitting { return; }
        let revision = draft.revision;
        draft.form = form.clone();
        draft.source = source;
        let start = {
            let mut edits = self.edits.borrow_mut();
            edits.visible = Some(Arc::new(draft));
            edits.queued = Some((source, form));
            edits.error.clear();
            !edits.pending
        };
        self.error.clear();
        if start { save_next(s, self.edits.clone(), self.draft, revision); }
        s.redraw();
    }
    fn saved(&mut self, s: &mut Session, complete: impl FnOnce(&mut Session, Result<Arc<edit::Draft>, String>) + 'static) {
        if self.edits.borrow().pending {
            self.edits.borrow_mut().waiters.push(Box::new(complete));
        } else {
            complete(s, self.reading().ok_or("event draft is loading".into()));
        }
    }
    pub fn availability(&mut self, s: &mut Session) {
        let slot = self.slot;
        self.saved(s, move |s, result| {
            let draft = match result { Ok(draft) => draft, Err(error) => { say(s, Err(error)); return; } };
            edit::submit(s, move |w| {
                let (a, b) = draft.form.bounds()?;
                let mut day = dates::day(a.max(w.now()), &draft.form.zone);
                if dates::instant(&format!("{day}T17:00"), &draft.form.zone)? <= w.now() {
                    day = (dates::date(&day)? + chrono::Duration::days(1)).to_string();
                }
                let q = availability::Query { start: format!("{day}T09:00"), end: format!("{day}T17:00"),
                    zone: draft.form.zone.clone(), minutes: if draft.form.all_day { 30 } else { ((b-a)/60.0).clamp(15.0,480.0) as u32 },
                    guests: availability::guests(&draft.form), draft_guests: None };
                let source = model::source(w.store(), draft.source).ok_or("calendar disconnected")?;
                availability::request_plan(w, source.account, q, Some(draft.id))
            }, move |s, result| {
                let result = result.map(|id| s.nav_within(open(slot, Availability::id(id))));
                say(s, result);
            });
        });
    }
}

impl Panel for Editor {
    fn id(&self) -> &PanelId {
        &self.id
    }
    fn persist(&self) -> PanelId {
        Self::id(self.draft)
    }
    fn title(&self) -> String {
        self.reading()
            .map(|d| {
                if d.form.title.is_empty() {
                    "new event".into()
                } else {
                    format!("edit · {}", d.form.title)
                }
            })
            .unwrap_or("event draft".into())
    }
    fn about(&self) -> String {
        self.reading().map(|d|format!("Persistent Google Calendar draft {} revision {} on source {}. State: {}. {}. It has not been sent until calendar.commit succeeds. Read calendar.update_draft for the complete current form. Find a time opens participant tracks beside this draft.", d.id,d.revision,d.source,d.state,d.error)).unwrap_or_else(|| "Loading event draft…".into())
    }
    fn wish(&self, _: usize) -> (u32, u32) {
        (5, 6)
    }
    fn placed(&mut self, slot: SlotId) {
        self.slot = slot;
    }
    fn verbs(&self) -> Vec<Verb> {
        let Some(d) = self.reading() else {
            return vec![];
        };
        if d.state == "pending" {
            return vec![];
        }
        if d.state == "done" {
            return vec![Verb::go(
                "calendar.timeline",
                "calendar",
                Some('c'),
                open(self.slot, Timeline::id()),
            )];
        }
        if d.state == "failed" && super::sync::needs_review(&d.error) {
            return vec![match d.event {
                Some(event) => Verb::go(
                    "calendar.review",
                    "review latest",
                    Some('r'),
                    open(self.slot, Event::id(event)),
                ),
                None => Verb::go(
                    "calendar.timeline",
                    "calendar",
                    Some('c'),
                    open(self.slot, Timeline::id()),
                ),
            }];
        }
        let mut v = if edit::editable(&d) {
            vec![
                Verb::run("calendar.save", "save event", Some('s')),
                Verb::run("calendar.find", "find a time", Some('f')),
            ]
        } else {
            vec![]
        };
        if d.state == "failed" {
            v.push(Verb::run("calendar.retry", "retry", Some('r')));
        }
        v
    }
    fn run(&mut self, v: &str, s: &mut Session) {
        match v {
            "calendar.find" => self.availability(s),
            "calendar.save" => {
                if self.edits.borrow().submitting { return; }
                self.edits.borrow_mut().submitting = true;
                if let Some(draft) = self.reading() {
                    let mut draft = (*draft).clone(); draft.state = "pending".into();
                    self.edits.borrow_mut().visible = Some(Arc::new(draft));
                }
                let edits = self.edits.clone();
                self.saved(s, move |s, result| {
                    let draft = match result {
                        Ok(draft) => draft,
                        Err(error) => { let mut edits = edits.borrow_mut(); edits.submitting = false; edits.error = error; return; }
                    };
                    edit::submit(s, move |w| edit::commit_plan(w, draft.id, draft.revision), move |s, result| {
                        edits.borrow_mut().submitting = false;
                        edits.borrow_mut().visible = None;
                        match result {
                            Ok(_) => s.notify("event queued for Google Calendar", false),
                            Err(error) => { edits.borrow_mut().error = error.clone(); s.notify(error, true); }
                        }
                        s.redraw();
                    });
                });
            }
            "calendar.retry" => {
                s.act_async_result(super::sync::retry_draft_plan(self.draft),|s,result|say(s,result.map_err(|error|error.to_string())));
            }
            _ => {}
        }
    }
    fn as_any(&mut self) -> &mut dyn Any {
        self
    }
}
pub struct EditorKind;
impl PanelKind for EditorKind {
    fn tag(&self) -> Tag {
        Editor::TAG
    }
    fn open(&self, id: &PanelId, cx: &mut Opening<'_>) -> Box<dyn Panel> {
        let store = cx.session().store().clone();
        let draft = id.args.first().and_then(|id| id.parse().ok()).unwrap_or(0);
        let error = String::new();
        Box::new(Editor {
            id: id.clone(),
            draft,
            slot: 0,
            store,
            error,
            edits: Rc::default(),
            display: Snapshot::default(),
        })
    }
}

pub struct Availability {
    id: PanelId,
    pub request: i64,
    pub slot: SlotId,
    pub store: Rc<Store>,
    pub error: String,
    pub search: availability::Search,
    pub dirty: bool,
    pub selected: Option<f64>,
    preview: availability::PreviewCache,
    checking: bool,
    initialized: bool,
    persisted: Option<String>,
    request_display: Snapshot<i64,availability::RequestDisplay>,
}
impl Availability {
    pub const TAG: Tag = Tag("calendar-availability");
    pub fn id(id: i64) -> PanelId {
        PanelId::new(Self::TAG, [id.to_string()])
    }
    pub fn request_view(&self) -> State<availability::RequestDisplay> {
        let (id,persisted) = (self.request,self.persisted.clone());
        self.request_display.get(&self.store,id,self.store.revision(&["calendar_availability"]),move |store|availability::RequestDisplay::load(store,id,persisted))
    }
    pub fn initialize(&mut self) -> bool {
        if self.initialized { return true; }
        if let Some(display) = self.request_view().ready() {
            self.search = display.initial.clone();
            self.dirty = display.dirty;
            self.initialized = true;
            true
        } else { false }
    }
    pub fn edit_search(&mut self, search: availability::Search) {
        if !self.initialize() { return; }
        self.search = search;
        let validation = match self.request_view() {
            State::Ready(display) | State::Refreshing(display) => self.search.reuse(&display.query).map(|_|()),
            State::Loading => Err("loading availability…".into()),
            State::Failed(error) => Err(error),
        };
        self.dirty = validation.is_err();
        self.error = validation.err().unwrap_or_default();
    }
    pub fn preview(&mut self, now: f64) -> Result<Arc<availability::Preview>, String> {
        if !self.initialize() { return Err("loading availability…".into()); }
        self.preview.get(&self.store,self.request,&self.search,now)
    }
    pub fn select(&mut self, start: f64, now: f64) -> Result<bool, String> {
        let preview = self.preview(now)?;
        if preview.draft.is_none() || !preview.result.people.iter().any(|p| p.known) {
            return Err("a draft and checked availability are needed to choose a time".into());
        }
        let selected =
            Some(availability::snap(&preview.query, start, now).ok_or("no time fits this window")?);
        let changed = self.selected != selected || !self.error.is_empty();
        self.selected = selected;
        self.error.clear();
        Ok(changed)
    }
    pub fn check(&mut self, s: &mut Session) {
        if !self.initialize() { return; }
        if self.checking { return; }
        self.checking = true;
        let (request, search, slot) = (self.request, self.search.clone(), self.slot);
        let sent = search.clone();
        let inline = Rc::new(std::cell::RefCell::new(None));
        let output = inline.clone();
        let returned = Rc::new(std::cell::Cell::new(false));
        let deferred = returned.clone();
        edit::submit(s, move |w| availability::recheck_plan(w, request, &search), move |s, result| {
            if !deferred.get() { *output.borrow_mut() = Some(result); return; }
            if let Some(panel) = s.panel(slot) {
                let mut panel = panel.borrow_mut();
                if let Some(panel) = panel.as_any().downcast_mut::<Availability>().filter(|p| p.request == request) {
                    panel.checked(result, &sent);
                }
            }
            s.redraw();
        });
        returned.set(true);
        let result = inline.borrow_mut().take();
        if let Some(result) = result { self.checked(result, &self.search.clone()); }
        s.redraw();
    }
    fn checked(&mut self, result: Result<i64, String>, sent: &availability::Search) {
        self.checking = false;
        match result {
            Ok(id) => { self.request = id; self.dirty = &self.search != sent; self.selected = None; self.error.clear(); }
            Err(error) => self.error = error,
        }
    }
    pub fn apply(&mut self, s: &mut Session, start: f64) {
        if self.dirty {
            self.error = "check availability for these settings first".into();
            s.redraw();
            return;
        }
        let (request, search, slot) = (self.request, self.search.clone(), self.slot);
        let error = Rc::new(std::cell::RefCell::new(None));
        let reported = error.clone();
        edit::submit(s, move |w| availability::apply_time_plan(w, request, &search, start), move |s, result| {
            match result {
                Ok(draft) => {
                    let parent = s.join_parent_of(slot).filter(|parent| s.panel(*parent).is_some_and(|p| p.borrow().persist() == Editor::id(draft.id)));
                    if let Some(parent) = parent {
                        s.nav_within(Nav::Close { slot, label: Some("find a time".into()) });
                        s.nav(Nav::Focus(parent));
                    } else { s.nav_within(Nav::Replace { slot, id: Editor::id(draft.id) }); }
                }
                Err(error) => { *reported.borrow_mut() = Some(error.clone()); s.notify(error, true); }
            }
            s.redraw();
        });
        if let Some(error) = error.borrow_mut().take() { self.error = error; }
        s.redraw();
    }
}
impl Panel for Availability {
    fn id(&self) -> &PanelId {
        &self.id
    }
    fn persist(&self) -> PanelId {
        let mut id = Self::id(self.request);
        if self.initialized { id.args.push(serde_json::to_string(&self.search).unwrap()); }
        else if let Some(persisted) = &self.persisted { id.args.push(persisted.clone()); }
        id
    }
    fn title(&self) -> String {
        "find a time".into()
    }
    fn about(&self) -> String {
        format!("Google free/busy request {}. Search controls: {}. Unchecked edits: {}. Selected start: {:?}. Unknown calendars are never free. Read calendar.availability_result for the complete checked intervals and shared event details. Choosing a time updates only the original local draft.",self.request,serde_json::to_string(&self.search).unwrap(),self.dirty,self.selected)
    }
    fn wish(&self, _: usize) -> (u32, u32) {
        (6, 6)
    }
    fn placed(&mut self, slot: SlotId) {
        self.slot = slot;
    }
    fn verbs(&self) -> Vec<Verb> {
        let mut verbs = Vec::new();
        if self.initialized && !self.dirty && self.selected.is_some() {
            verbs.push(Verb::run("calendar.use_time", "use this time", Some('s')));
        }
        if self.initialized { verbs.push(Verb::run("calendar.check", "check availability", Some('c'))); }
        verbs.push(Verb::go(
            "calendar.cancel_time",
            "cancel",
            None,
            Nav::Close {
                slot: self.slot,
                label: Some("find a time".into()),
            },
        ));
        verbs
    }
    fn run(&mut self, v: &str, s: &mut Session) {
        match v {
            "calendar.check" => self.check(s),
            "calendar.use_time" => {
                if let Some(i) = self.selected {
                    self.apply(s, i);
                }
            }
            _ => {}
        }
    }
    fn as_any(&mut self) -> &mut dyn Any {
        self
    }
}
pub struct AvailabilityKind;
impl PanelKind for AvailabilityKind {
    fn tag(&self) -> Tag {
        Availability::TAG
    }
    fn open(&self, id: &PanelId, cx: &mut Opening<'_>) -> Box<dyn Panel> {
        let store = cx.session().store().clone();
        let request = id.args.first().and_then(|n| n.parse().ok()).unwrap_or(0);
        let persisted = id.args.get(1).cloned();
        let search = availability::Search::default();
        let dirty = true;
        let mut panel = Availability {
            id: id.clone(),
            request,
            slot: 0,
            store,
            error: String::new(),
            search,
            dirty,
            selected: None,
            preview: availability::PreviewCache::default(),
            checking: false,
            initialized: false,
            persisted,
            request_display: Snapshot::default(),
        };
        panel.initialize();
        Box::new(panel)
    }
}

pub struct Sources {
    id: PanelId,
    pub slot: SlotId,
    pub store: Rc<Store>,
}
impl Sources {
    pub const TAG: Tag = Tag("calendar-sources");
    pub fn id() -> PanelId {
        PanelId::bare(Self::TAG)
    }
}
impl Panel for Sources {
    fn id(&self) -> &PanelId {
        &self.id
    }
    fn title(&self) -> String {
        "calendars".into()
    }
    fn about(&self) -> String {
        format!("Connected Google calendars with explicit source IDs, account identities and access rights. {}",model::display_sync_line(&self.store))
    }
    fn wish(&self, _: usize) -> (u32, u32) {
        (4, 5)
    }
    fn placed(&mut self, slot: SlotId) {
        self.slot = slot;
    }
    fn verbs(&self) -> Vec<Verb> {
        vec![
            Verb::go(
                "calendar.accounts",
                "accounts",
                Some('a'),
                open(self.slot, accounts()),
            ),
            Verb::run("calendar.refresh", "refresh", Some('r')),
        ]
    }
    fn run(&mut self, v: &str, s: &mut Session) {
        if v == "calendar.refresh" {
            model::refresh(s);
        }
    }
    fn as_any(&mut self) -> &mut dyn Any {
        self
    }
}
pub struct SourcesKind;
impl PanelKind for SourcesKind {
    fn tag(&self) -> Tag {
        Sources::TAG
    }
    fn open(&self, id: &PanelId, cx: &mut Opening<'_>) -> Box<dyn Panel> {
        Box::new(Sources {
            id: id.clone(),
            slot: 0,
            store: cx.session().store().clone(),
        })
    }
}
