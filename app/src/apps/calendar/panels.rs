use super::{availability, dates, edit, model, scoped};
use kernel::{
    layout::SlotId,
    nav::Nav,
    panel::{Opening, Panel, PanelId, PanelKind, Tag, Verb},
    richtable::ListState,
    session::Session,
    store::{Store, Val},
};
use serde_json::Value;
use std::{any::Any, rc::Rc};
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
    let result = edit::create(s, source, event).map(|draft| {
        s.nav_within(open(from, Editor::id(draft)));
    });
    say(s, result);
}

pub struct Timeline {
    id: PanelId,
    pub slot: SlotId,
    pub store: Rc<Store>,
    pub list: ListState<scoped::Events>,
    pub filter: String,
    pub day: Option<String>,
    pub zone: String,
}
impl Timeline {
    pub const TAG: Tag = Tag("calendar");
    pub fn id() -> PanelId {
        PanelId::bare(Self::TAG)
    }
    pub fn day(day: &str, zone: &str, filter: &str) -> PanelId {
        PanelId::new(Self::TAG, [filter, day, zone])
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
        format!("Upcoming Google Calendar occurrences, ordered by start. Date scope: {} in {}. Filter: {}. Each row has source/account identity, invite status and a stable occurrence ID. {}. Missing events outside the cached range are not evidence of availability. Use calendar.availability for scheduling.",self.day.as_deref().unwrap_or("upcoming"),self.zone,self.list.table().filter(),model::sync_line(&self.store))
    }
    fn wish(&self, _: usize) -> (u32, u32) {
        (5, 6)
    }
    fn placed(&mut self, slot: SlotId) {
        self.slot = slot;
    }
    fn persist(&self) -> PanelId {
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
            Verb::run("calendar.later", "load later", Some('o')),
        ]
    }
    fn run(&mut self, v: &str, s: &mut Session) {
        match v {
            "calendar.refresh" => model::refresh(s),
            "calendar.later" => {
                let end = self
                    .store
                    .conn()
                    .query_row("SELECT end FROM calendar_sync WHERE id=1", [], |r| {
                        r.get::<_, f64>(0)
                    })
                    .unwrap_or(s.now());
                model::cover(s, s.now(), end + 366.0 * 86400.0);
            }
            "calendar.new" => {
                if let Some(c) = model::sources(s.store()).iter().find(|c| c.writable()) {
                    start_editor(s, self.slot, c.id, None);
                } else {
                    s.nav_within(open(self.slot, accounts()));
                    s.notify("connect Google Calendar in Accounts to add events", false);
                }
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
        let zone = id
            .args
            .get(2)
            .filter(|s| dates::zone(s).is_ok())
            .cloned()
            .unwrap_or_else(|| {
                model::sources(s.store())
                    .first()
                    .map(|c| c.zone.clone())
                    .unwrap_or("UTC".into())
            });
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
        })
    }
}

pub struct Month {
    id: PanelId,
    pub slot: SlotId,
    pub store: Rc<Store>,
    pub month: String,
    pub filter: String,
    pub zone: String,
}
impl Month {
    pub const TAG: Tag = Tag("calendar-month");
    pub fn id(day: &str, filter: &str) -> PanelId {
        PanelId::new(Self::TAG, [day, filter])
    }
    pub fn rows(&self) -> Vec<model::Event> {
        use kernel::richtable::Datasource;
        let first = dates::grid(&self.month)[0];
        let start = dates::midnight(first, &self.zone).unwrap_or(0.0);
        let end = dates::midnight(first + chrono::Duration::days(42), &self.zone).unwrap_or(start);
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
        format!("Calendar month {} in {}, Monday first. Filter: {}. Select a day for its agenda, including overlapping multi-day events. {}",self.month,self.zone,self.filter,model::sync_line(&self.store))
    }
    fn wish(&self, _: usize) -> (u32, u32) {
        (7, 6)
    }
    fn placed(&mut self, slot: SlotId) {
        self.slot = slot;
    }
    fn persist(&self) -> PanelId {
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
        let d = match v {
            "calendar.previous" => dates::month(&self.month, -1),
            "calendar.next" => dates::month(&self.month, 1),
            "calendar.today" => dates::month(&dates::day(s.now(), &self.zone), 0),
            _ => return,
        };
        self.month = d.to_string();
        let start = dates::midnight(d - chrono::Duration::days(7), &self.zone).unwrap_or(s.now());
        model::cover(s, start, start + 49.0 * 86400.0);
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
        let zone = model::sources(s.store())
            .first()
            .map(|c| c.zone.clone())
            .unwrap_or("UTC".into());
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
        })
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
}
impl Event {
    pub const TAG: Tag = Tag("calendar-event");
    pub fn id(id: i64) -> PanelId {
        PanelId::new(Self::TAG, [id.to_string()])
    }
    pub fn reading(&self) -> Option<(model::Event, Value)> {
        model::event(&self.store, self.event).map(|e| (e, model::raw(&self.store, self.event)))
    }
    pub fn operation(&self) -> Option<(i64, String, String)> {
        self.store
            .rows_sql(
                "calendar operation",
                "latest change for this event",
                "SELECT id,state,error FROM calendar_change WHERE event=? ORDER BY id DESC LIMIT 1",
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
            .map(|(e, _)| e.title)
            .unwrap_or("event unavailable".into())
    }
    fn about(&self) -> String {
        self.reading().map(|(e,v)|format!("Google event occurrence {} on source {} ({} / {}), revision {}. {}. Recurring series: {}. Organizer: {}. Your response: {}. Deletion scope: {}. Use a draft for edits, and read the latest revision before committing.",e.id,e.source,e.calendar,e.email,e.etag,e.when(),e.series,v["organizer"],e.response,self.scope)).unwrap_or("This event was deleted or its calendar is disconnected.".into())
    }
    fn wish(&self, _: usize) -> (u32, u32) {
        (4, 6)
    }
    fn placed(&mut self, slot: SlotId) {
        self.slot = slot;
    }
    fn verbs(&self) -> Vec<Verb> {
        let Some((e, raw)) = self.reading() else {
            return vec![];
        };
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
        if model::source(&self.store, e.source).is_some_and(|c| edit::can_edit(&c, &raw)) {
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
        if self.operation().is_some_and(|(_, s, _)| s == "failed") {
            v.push(Verb::run("calendar.retry", "retry", Some('r')));
        }
        v
    }
    fn run(&mut self, v: &str, s: &mut Session) {
        let Some((e, raw)) = self.reading() else {
            return;
        };
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
                let r = edit::command(s, e.id, &e.etag, "delete", &self.scope, "", self.notify)
                    .map(|_| ());
                self.deleting = false;
                say(s, r);
            }
            "calendar.yes" | "calendar.maybe" | "calendar.no" => {
                let response = match v {
                    "calendar.yes" => "accepted",
                    "calendar.maybe" => "tentative",
                    _ => "declined",
                };
                let r =
                    edit::command(s, e.id, &e.etag, "respond", "this", response, true).map(|_| ());
                say(s, r);
            }
            "calendar.duplicate" => {
                let target = model::sources(s.store()).into_iter().find(|c| c.writable());
                if let Some(target) = target {
                    let r = edit::create(s, target.id, None).and_then(|id| {
                        let d = edit::draft(s.store(), id).unwrap();
                        let mut f =
                            edit::Form::from_event(&model::raw(s.store(), e.id), &target.zone);
                        f.title = format!("{} (copy)", f.title);
                        f.meet = false;
                        edit::save(s, id, d.revision, target.id, f)?;
                        s.nav_within(open(self.slot, Editor::id(id)));
                        Ok(())
                    });
                    say(s, r);
                }
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
            "calendar.retry" => {
                if let Some((id, _, _)) = self.operation() {
                    let r = super::sync::retry(s, id);
                    say(s, r);
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
        })
    }
}

pub struct Editor {
    id: PanelId,
    pub draft: i64,
    pub slot: SlotId,
    pub store: Rc<Store>,
    pub error: String,
}
impl Editor {
    pub const TAG: Tag = Tag("calendar-edit");
    pub fn id(id: i64) -> PanelId {
        PanelId::new(Self::TAG, [id.to_string()])
    }
    pub fn reading(&self) -> Option<edit::Draft> {
        edit::draft(&self.store, self.draft)
    }
    pub fn save(&mut self, s: &mut Session, revision: i64, source: i64, form: edit::Form) {
        self.error = edit::save(s, self.draft, revision, source, form)
            .err()
            .unwrap_or_default();
        s.redraw();
    }
    pub fn availability(&mut self, s: &mut Session) {
        let Some(d) = self.reading() else { return };
        let r = (|| {
            let (a, _) = d.form.bounds()?;
            let day = dates::day(a, &d.form.zone);
            let q = availability::Query {
                start: format!("{day}T09:00"),
                end: format!("{day}T18:00"),
                zone: d.form.zone.clone(),
                minutes: ((d.form.bounds()?.1 - a) / 60.0).clamp(15.0, 480.0) as u32,
                guests: d
                    .form
                    .guests
                    .split([',', ';', '\n'])
                    .map(|e| e.trim().trim_start_matches('?').to_string())
                    .filter(|e| !e.is_empty())
                    .collect(),
            };
            let c = model::source(s.store(), d.source).ok_or("calendar disconnected")?;
            let id = availability::request(s, c.account, q, Some(d.id))?;
            s.nav_within(open(self.slot, Availability::id(id)));
            Ok(())
        })();
        say(s, r);
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
        self.reading().map(|d|format!("Persistent Google Calendar draft {} revision {} on source {}. State: {}. {}. It has not been sent until calendar.commit succeeds. Form: {}",d.id,d.revision,d.source,d.state,d.error,serde_json::to_string(&d.form).unwrap())).unwrap_or(self.error.clone())
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
                if let Some(d) = self.reading() {
                    self.error = edit::commit(s, d.id, d.revision).err().unwrap_or_default();
                    if self.error.is_empty() {
                        s.notify("event queued for Google Calendar", false);
                    }
                    s.redraw();
                }
            }
            "calendar.retry" => {
                let id = self
                    .store
                    .conn()
                    .query_row(
                        "SELECT id FROM calendar_change WHERE draft=? ORDER BY id DESC LIMIT 1",
                        [self.draft],
                        |r| r.get::<_, i64>(0),
                    )
                    .ok();
                if let Some(id) = id {
                    let r = super::sync::retry(s, id);
                    say(s, r);
                }
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
        let error = if edit::draft(&store, draft).is_some() {
            String::new()
        } else {
            "event draft unavailable".into()
        };
        Box::new(Editor {
            id: id.clone(),
            draft,
            slot: 0,
            store,
            error,
        })
    }
}

pub struct Availability {
    id: PanelId,
    pub request: i64,
    pub slot: SlotId,
    pub store: Rc<Store>,
    pub error: String,
}
impl Availability {
    pub const TAG: Tag = Tag("calendar-availability");
    pub fn id(id: i64) -> PanelId {
        PanelId::new(Self::TAG, [id.to_string()])
    }
    pub fn apply(&mut self, s: &mut Session, index: usize) {
        let r = availability::apply(s, self.request, index).map(|id| {
            s.nav_within(open(self.slot, Editor::id(id)));
        });
        say(s, r);
    }
}
impl Panel for Availability {
    fn id(&self) -> &PanelId {
        &self.id
    }
    fn persist(&self) -> PanelId {
        Self::id(self.request)
    }
    fn title(&self) -> String {
        "find a time".into()
    }
    fn about(&self) -> String {
        format!("Google free/busy request {}. Unknown calendars are never treated as free. Suggestions are not reservations and may have partial coverage. Result: {}",self.request,availability::load(&self.store,self.request).and_then(|(_,r,_,_)|r).map(|r|serde_json::to_string(&r).unwrap()).unwrap_or("pending".into()))
    }
    fn wish(&self, _: usize) -> (u32, u32) {
        (5, 6)
    }
    fn placed(&mut self, slot: SlotId) {
        self.slot = slot;
    }
    fn verbs(&self) -> Vec<Verb> {
        vec![Verb::run("calendar.check", "check availability", Some('c'))]
    }
    fn run(&mut self, v: &str, s: &mut Session) {
        if v == "calendar.check" {
            if let Some((q, _, _, draft)) = availability::load(&self.store, self.request) {
                let account = self
                    .store
                    .conn()
                    .query_row(
                        "SELECT account FROM calendar_availability WHERE id=?",
                        [self.request],
                        |r| r.get::<_, i64>(0),
                    )
                    .unwrap_or(0);
                match availability::request(s, account, q, draft) {
                    Ok(id) => {
                        self.request = id;
                        self.error.clear();
                    }
                    Err(e) => self.error = e,
                }
                s.redraw();
            }
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
        Box::new(Availability {
            id: id.clone(),
            request: id.args.first().and_then(|n| n.parse().ok()).unwrap_or(0),
            slot: 0,
            store: cx.session().store().clone(),
            error: String::new(),
        })
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
        format!("Connected Google calendars with explicit source IDs, account identities and access rights. {}",model::sync_line(&self.store))
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
