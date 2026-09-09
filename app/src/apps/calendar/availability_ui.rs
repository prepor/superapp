//! The scheduling sheet: shared time axis, participant tracks, then slot selection.
use super::{
    availability::{self, Search},
    completion::{self, Field},
    completion_ui::Offers,
    dates, model, panels,
};
use crate::shell::{
    draw::{rect, DrawFlat},
    hosted::PanelProps,
    widgets::form::{self, ClickFocus},
};
use kernel::{richtable::Suggestion, session::Session};
use makepad_widgets::*;

#[derive(Clone, Default)]
pub struct Track {
    pub busy: Vec<(f64, f64)>,
    pub proposed: Option<(f64, f64)>,
    pub unknown: bool,
    pub request: i64,
    pub query: Option<availability::Query>,
    pub people: Vec<availability::Person>,
    pub interactive: bool,
    pub conflict: bool,
}
impl Track {
    fn new(
        request: i64,
        query: &availability::Query,
        people: Vec<availability::Person>,
        selected: Option<(f64, f64)>,
        interactive: bool,
    ) -> Self {
        let (a, b) = query.validate().unwrap_or((0.0, 1.0));
        Self {
            busy: people
                .iter()
                .flat_map(|p| &p.busy)
                .filter_map(|(s, e)| fraction(*s, *e, a, b))
                .collect(),
            proposed: selected.and_then(|(s, e)| fraction(s, e, a, b)),
            unknown: people.iter().any(|p| !p.known),
            conflict: selected.is_some_and(|(s, e)| {
                people
                    .iter()
                    .any(|p| p.known && p.busy.iter().any(|(a, b)| s < *b && e > *a))
            }),
            request,
            query: Some(query.clone()),
            people,
            interactive,
        }
    }
}

/// Preserve the grab point within an existing proposal; clicking empty space
/// starts a new proposal at the pointer. Snapping happens in the panel model.
#[derive(Clone, Debug)]
pub struct Drag {
    left: f64,
    width: f64,
    start: f64,
    span: f64,
    offset: f64,
    previous: Option<f64>,
}
impl Drag {
    pub fn new(
        rect: Rect,
        start: f64,
        end: f64,
        proposed: Option<(f64, f64)>,
        x: f64,
    ) -> Option<Self> {
        if rect.size.x <= 0.0 || end <= start {
            return None;
        }
        let at = start + (x - rect.pos.x) / rect.size.x * (end - start);
        let offset = proposed
            .filter(|(a, b)| at >= *a && at <= *b)
            .map_or(0.0, |(a, _)| at - a);
        Some(Self {
            left: rect.pos.x,
            width: rect.size.x,
            start,
            span: end - start,
            offset,
            previous: proposed.map(|(a, _)| a),
        })
    }
    pub fn at(&self, x: f64) -> f64 {
        self.start + (x - self.left) / self.width * self.span - self.offset
    }
}

#[derive(Clone, Debug, Default)]
enum TrackAction {
    #[default]
    None,
    Select {
        request: i64,
        start: f64,
    },
    Cancel {
        request: i64,
        previous: Option<f64>,
    },
    Hover(DVec2),
    Leave,
}

pub fn hover_text(person: &availability::Person, at: f64, zone: &str) -> Option<String> {
    let (start, end) = *person.busy.iter().find(|(a, b)| at >= *a && at < *b)?;
    let events: Vec<_> = person
        .details
        .events
        .iter()
        .filter(|e| at >= e.start && at < e.end)
        .collect();
    let time = |a, b| format!("{} – {}", dates::label(a, zone), dates::label(b, zone));
    let body = if events.is_empty() {
        format!(
            "Busy\n{}\n{}",
            time(start, end),
            match person.details.state {
                availability::DetailState::Pending => "Loading event details…",
                availability::DetailState::Ready | availability::DetailState::Unavailable =>
                    "Event details unavailable",
            }
        )
    } else {
        events
            .iter()
            .map(|e| {
                format!(
                    "{}\n{}{}{}",
                    if e.title.is_empty() {
                        "Busy · details unavailable"
                    } else {
                        &e.title
                    },
                    if e.all_day { "All day · " } else { "" },
                    time(e.start, e.end),
                    if e.location.is_empty() {
                        String::new()
                    } else {
                        format!("\n{}", e.location)
                    }
                )
            })
            .collect::<Vec<_>>()
            .join("\n\n")
    };
    Some(format!("{}\n{}", person.calendar, body))
}
/// Fractions shared by the painted busy blocks and the selected proposal.
pub fn fraction(a: f64, b: f64, start: f64, end: f64) -> Option<(f64, f64)> {
    if end <= start || b <= a {
        return None;
    }
    let left = (a.max(start) - start) / (end - start);
    let right = (b.min(end) - start) / (end - start);
    (right > left).then_some((left, right))
}
#[derive(Script, ScriptHook, Widget)]
pub struct CalendarTimeTrack {
    #[source]
    source: ScriptObjectRef,
    #[deref]
    view: View,
    #[live]
    fill: DrawFlat,
    #[rust]
    pub track: Track,
    #[rust]
    drag: Option<(i64, Drag)>,
}
impl CalendarTimeTrack {
    fn block(&mut self, cx: &mut Cx2d, r: Rect, color: Vec4f) {
        self.fill.color = color;
        self.fill.draw_abs(cx, r);
    }
}
impl Widget for CalendarTimeTrack {
    fn is_interactive(&self) -> bool {
        true
    }
    fn handle_event(&mut self, cx: &mut Cx, event: &Event, scope: &mut Scope) {
        if matches!(event, Event::KeyDown(k) if k.key_code == KeyCode::Escape) {
            if let Some((request, drag)) = self.drag.take() {
                cx.widget_action(
                    self.widget_uid(),
                    TrackAction::Cancel {
                        request,
                        previous: drag.previous,
                    },
                );
            }
        }
        match event.hits(cx, self.view.area()) {
            Hit::FingerDown(e) if e.is_primary_hit() && self.track.interactive => {
                if let Some((a, b)) = self.track.query.as_ref().and_then(|q| q.validate().ok()) {
                    let proposed = self
                        .track
                        .proposed
                        .map(|(s, e)| (a + s * (b - a), a + e * (b - a)));
                    if let Some(drag) = Drag::new(e.rect, a, b, proposed, e.abs.x) {
                        cx.set_key_focus(self.view.area());
                        cx.widget_action(
                            self.widget_uid(),
                            TrackAction::Select {
                                request: self.track.request,
                                start: drag.at(e.abs.x),
                            },
                        );
                        self.drag = Some((self.track.request, drag));
                        cx.set_cursor(MouseCursor::Grabbing);
                    }
                }
            }
            Hit::FingerMove(e) => {
                if let Some((request, drag)) = &self.drag {
                    cx.widget_action(
                        self.widget_uid(),
                        TrackAction::Select {
                            request: *request,
                            start: drag.at(e.abs.x),
                        },
                    );
                    cx.set_cursor(MouseCursor::Grabbing);
                }
            }
            Hit::FingerUp(e) => {
                if let Some((request, drag)) = self.drag.take() {
                    cx.widget_action(
                        self.widget_uid(),
                        TrackAction::Select {
                            request,
                            start: drag.at(e.abs.x),
                        },
                    );
                }
            }
            Hit::FingerHoverIn(e) | Hit::FingerHoverOver(e) => {
                cx.set_cursor(if self.track.interactive {
                    MouseCursor::Grab
                } else {
                    MouseCursor::Default
                });
                cx.widget_action(self.widget_uid(), TrackAction::Hover(e.abs));
            }
            Hit::FingerHoverOut(_) => {
                cx.widget_action(self.widget_uid(), TrackAction::Leave);
            }
            _ => {}
        }
        self.view.handle_event(cx, event, scope);
    }
    fn draw_walk(&mut self, cx: &mut Cx2d, scope: &mut Scope, walk: Walk) -> DrawStep {
        if self
            .drag
            .as_ref()
            .is_some_and(|(id, _)| *id != self.track.request)
        {
            self.drag = None;
        }
        self.view
            .draw_bg
            .set_uniform(cx, live_id!(unknown), &[f32::from(self.track.unknown)]);
        self.view.draw_walk_all(cx, scope, walk);
        let r = self.view.area().rect(cx);
        let (x, y, w, h) = (r.pos.x, r.pos.y, r.size.x, r.size.y);
        if w <= 0.0 || h <= 0.0 {
            return DrawStep::done();
        }
        let rule = vec4(0.86, 0.86, 0.86, 1.0);
        for i in 0..=8 {
            self.block(cx, rect(x + (w - 1.0) * i as f64 / 8.0, y, 1.0, h), rule);
        }
        self.block(cx, rect(x, y, w, 1.0), rule);
        self.block(cx, rect(x, y + h - 1.0, w, 1.0), rule);
        for (a, b) in self.track.busy.clone() {
            self.block(
                cx,
                rect(x + a * w, y + 4.0, (b - a) * w, h - 8.0),
                vec4(0.74, 0.74, 0.74, 1.0),
            );
            self.block(
                cx,
                rect(x + a * w, y + 4.0, 2.0, h - 8.0),
                vec4(0.49, 0.49, 0.49, 1.0),
            );
        }
        if let Some((a, b)) = self.track.proposed {
            let (left, width) = (x + a * w, ((b - a) * w).max(1.0));
            let ink = if self.track.conflict {
                vec4(0.68, 0.18, 0.08, 1.0)
            } else {
                vec4(0.078, 0.078, 0.078, 1.0)
            };
            self.block(cx, rect(left, y, width, h), vec4(0.078, 0.078, 0.078, 0.07));
            for r in [
                rect(left, y, 1.0, h),
                rect(left + width - 1.0, y, 1.0, h),
                rect(left, y, width, 1.0),
                rect(left, y + h - 1.0, width, 1.0),
            ] {
                self.block(cx, r, ink);
            }
        }
        DrawStep::done()
    }
}

const FIELDS: [(&str, &[LiveId]); 6] = [
    ("availability date", ids!(date_input)),
    ("availability through date", ids!(end_date_input)),
    ("availability from", ids!(from_input)),
    ("availability until", ids!(until_input)),
    ("availability duration", ids!(duration_input)),
    ("availability time zone", ids!(zone_input)),
];
fn offers(cx: &mut Cx, w: &WidgetRef) -> Vec<(Field, TextInputRef)> {
    [
        (Field::Time, ids!(from_input)),
        (Field::Time, ids!(until_input)),
        (Field::Duration, ids!(duration_input)),
        (Field::Zone, ids!(zone_input)),
    ]
    .into_iter()
    .map(|(f, p)| (f, w.text_input(cx, p)))
    .collect()
}
#[derive(Clone, PartialEq)]
enum Pick {
    Slot(usize),
    More,
    Previous,
    Next,
}
#[derive(Script, ScriptHook, Widget)]
pub struct CalendarAvailabilityPanel {
    #[source]
    source: ScriptObjectRef,
    #[deref]
    view: View,
    #[live]
    suggest: View,
    #[live]
    tooltip: View,
    #[rust]
    controls: WidgetRef,
    #[rust]
    shown: Search,
    #[rust]
    offers: Offers,
    #[rust]
    click_focus: ClickFocus,
    #[rust]
    picks: Vec<(Rect, Pick)>,
    #[rust]
    pressed: Option<(DVec2, Pick)>,
    #[rust]
    expanded: bool,
    #[rust]
    request: i64,
    #[rust]
    directory: Vec<Suggestion>,
    #[rust]
    tracks: Vec<(WidgetRef, Rect)>,
    #[rust]
    hover: Option<(WidgetUid, DVec2)>,
}
impl Widget for CalendarAvailabilityPanel {
    fn handle_event(&mut self, cx: &mut Cx, event: &Event, scope: &mut Scope) {
        let Some(props) = scope.props.get::<PanelProps>().cloned() else {
            return;
        };
        let fields = offers(cx, &self.controls);
        let completed = self.offers.handle(cx, event, &props, &fields);
        if completed == Some(false) {
            self.view.redraw(cx);
            return;
        }
        if completed.is_none() {
            self.view.handle_event(cx, event, scope);
        }
        let inputs: Vec<_> = FIELDS
            .iter()
            .map(|(_, p)| self.controls.text_input(cx, p))
            .collect();
        if completed.is_none() {
            let ring: Vec<_> = inputs
                .iter()
                .enumerate()
                .filter(|(i, _)| *i != 1 || self.shown.day != self.shown.end_day)
                .map(|(_, t)| t.clone())
                .collect();
            if let Some(i) = form::tab(cx, event, &ring) {
                form::reveal(cx, &self.view.portal_list(cx, ids!(list)), &ring[i]);
            }
        }
        self.click_focus
            .handle(cx, event, &props, &inputs, &FIELDS.map(|(name, _)| name));
        let changed = completed == Some(true)
            || matches!(event, Event::Actions(a) if inputs.iter().any(|t|t.changed(a).is_some()));
        if changed {
            let mut p = props.panel.borrow_mut();
            if let Some(p) = p.as_any().downcast_mut::<panels::Availability>() {
                let day = inputs[0].text();
                let end_day = if p.search.end_day == p.search.day && day != p.search.day {
                    day.clone()
                } else {
                    inputs[1].text()
                };
                p.edit_search(Search {
                    day,
                    end_day,
                    from: inputs[2].text(),
                    until: inputs[3].text(),
                    minutes: inputs[4].text(),
                    zone: inputs[5].text(),
                });
                self.expanded = false;
            }
            self.view.redraw(cx);
        }
        if let Event::Actions(actions) = event {
            for (track, _) in &self.tracks {
                for action in actions.filter_widget_actions(track.widget_uid()) {
                    match action.cast::<TrackAction>() {
                        TrackAction::Select { request, start } => {
                            if let Some(s) = scope.data.get_mut::<Session>() {
                                if let Some(p) = props
                                    .panel
                                    .borrow_mut()
                                    .as_any()
                                    .downcast_mut::<panels::Availability>()
                                {
                                    if p.request == request {
                                        if let Err(error) = p.select(start, s.now()) {
                                            p.error = error;
                                        }
                                        s.redraw();
                                    }
                                }
                            }
                            self.hover = None;
                        }
                        TrackAction::Cancel { request, previous } => {
                            if let Some(p) = props
                                .panel
                                .borrow_mut()
                                .as_any()
                                .downcast_mut::<panels::Availability>()
                            {
                                if p.request == request {
                                    p.selected = previous;
                                }
                            }
                            if let Some(s) = scope.data.get_mut::<Session>() {
                                s.redraw();
                            }
                        }
                        TrackAction::Hover(at) => {
                            if props.hits.at(at).is_some_and(|h| {
                                h.slot == Some(props.slot)
                                    && h.label.starts_with("availability track ")
                            }) {
                                self.hover = Some((track.widget_uid(), at));
                            }
                        }
                        TrackAction::Leave
                            if self.hover.is_some_and(|(id, _)| id == track.widget_uid()) =>
                        {
                            self.hover = None
                        }
                        _ => {}
                    }
                    self.view.redraw(cx);
                }
            }
        }
        if matches!(event, Event::Scroll(_) | Event::MouseDown(_)) {
            self.hover = None;
        }
        if let Event::MouseDown(e) = event {
            self.pressed = props
                .hits
                .at(e.abs)
                .filter(|h| h.slot == Some(props.slot))
                .and_then(|_| self.picks.iter().find(|(r, _)| r.contains(e.abs)))
                .map(|(_, p)| (e.abs, p.clone()));
        }
        if let Event::MouseUp(e) = event {
            if let Some((at, pick)) = self.pressed.take() {
                let delta = e.abs - at;
                if delta.x.abs() > 5.0
                    || delta.y.abs() > 5.0
                    || props
                        .hits
                        .at(e.abs)
                        .is_none_or(|h| h.slot != Some(props.slot))
                    || !self
                        .picks
                        .iter()
                        .any(|(r, p)| *p == pick && r.contains(e.abs))
                {
                    return;
                }
                let mut p = props.panel.borrow_mut();
                let Some(p) = p.as_any().downcast_mut::<panels::Availability>() else {
                    return;
                };
                match pick {
                    Pick::Slot(i) => {
                        if let Some(s) = scope.data.get_mut::<Session>() {
                            let selected =
                                availability::preview(&p.store, p.request, &p.search, s.now())
                                    .and_then(|(_, r, _)| {
                                        r.slots
                                            .get(i)
                                            .map(|(a, _)| *a)
                                            .ok_or("time slot missing".into())
                                    })
                                    .and_then(|start| p.select(start, s.now()));
                            if let Err(error) = selected {
                                p.error = error;
                            }
                            s.redraw();
                        }
                    }
                    Pick::More => self.expanded = true,
                    Pick::Previous | Pick::Next => {
                        let mut search = p.search.clone();
                        match search.shift(if pick == Pick::Previous { -1 } else { 1 }) {
                            Ok(()) => {
                                p.edit_search(search);
                                if let Some(s) = scope.data.get_mut::<Session>() {
                                    p.check(s);
                                }
                            }
                            Err(e) => p.error = e,
                        }
                    }
                }
                self.view.redraw(cx);
            }
        }
    }
    fn draw_walk(&mut self, cx: &mut Cx2d, scope: &mut Scope, walk: Walk) -> DrawStep {
        let Some(props) = scope.props.get::<PanelProps>().cloned() else {
            return self.view.draw_walk(cx, scope, walk);
        };
        let now = scope.data.get::<Session>().map(|s| s.now()).unwrap_or(0.0);
        let Some((id, search, dirty, error, q, result, selected, interactive, store)) = ({
            let mut borrow = props.panel.borrow_mut();
            borrow
                .as_any()
                .downcast_mut::<panels::Availability>()
                .and_then(|p| {
                    let (stored, cached, remote_error, draft) =
                        availability::load(&p.store, p.request)?;
                    let resolved = availability::preview(&p.store, p.request, &p.search, now);
                    let (q, result, error) = match resolved {
                        Ok((q, result, _)) => (q, Some(result), p.error.clone()),
                        Err(error) => {
                            let q = p.search.query(stored.guests.clone()).unwrap_or(stored);
                            let error = if !p.error.is_empty() {
                                p.error.clone()
                            } else if cached.is_none() && remote_error.is_empty() && !p.dirty {
                                String::new()
                            } else {
                                error
                            };
                            (q, None, error)
                        }
                    };
                    let interactive = draft.is_some()
                        && result
                            .as_ref()
                            .is_some_and(|r| r.people.iter().any(|p| p.known));
                    if let Some(r) = &result {
                        if interactive {
                            p.selected = p
                                .selected
                                .and_then(|start| availability::snap(&q, start, now))
                                .or_else(|| r.slots.first().map(|(a, _)| *a));
                        } else {
                            p.selected = None;
                        }
                    } else if !p.dirty {
                        p.selected = None;
                    }
                    Some((
                        p.request,
                        p.search.clone(),
                        p.dirty,
                        error,
                        q,
                        result,
                        p.selected,
                        interactive,
                        p.store.clone(),
                    ))
                })
        }) else {
            return self.view.draw_walk(cx, scope, walk);
        };
        if self.request != id {
            self.request = id;
            self.expanded = false;
            self.directory = completion::people(&store);
            self.hover = None;
        }
        let (a, b) = q.validate().unwrap_or((0.0, 1.0));
        let chosen = result
            .as_ref()
            .and_then(|_| selected.and_then(|start| availability::selection(&q, start, now)));
        let sources = model::sources(&store);
        let own: Vec<_> = sources
            .iter()
            .filter(|c| c.role == "owner")
            .map(|c| {
                if c.remote == "primary" {
                    c.email.to_lowercase()
                } else {
                    c.remote.to_lowercase()
                }
            })
            .collect();
        let name = |email: &str| {
            self.directory
                .iter()
                .find(|s| s.value.eq_ignore_ascii_case(email))
                .map(|s| s.label.clone())
                .unwrap_or_else(|| email.into())
        };
        let people = result
            .as_ref()
            .map(|r| r.people.clone())
            .unwrap_or_else(|| {
                q.guests
                    .iter()
                    .map(|g| availability::Person {
                        calendar: g.clone(),
                        known: false,
                        busy: vec![],
                        error: String::new(),
                        checks: vec![],
                        details: availability::Details::default(),
                    })
                    .collect()
            });
        let mine: Vec<_> = people
            .iter()
            .filter(|p| own.contains(&p.calendar.to_lowercase()))
            .collect();
        let mut rows = Vec::new();
        if !mine.is_empty() {
            rows.push((
                "You".into(),
                format!(
                    "{} own calendar{}",
                    mine.len(),
                    if mine.len() == 1 { "" } else { "s" }
                ),
                if result.is_none() {
                    "not checked"
                } else if mine.iter().any(|p| !p.known) {
                    "some calendars unavailable"
                } else {
                    "own calendars checked"
                }
                .to_string(),
                Track::new(
                    id,
                    &q,
                    mine.iter().map(|p| (*p).clone()).collect(),
                    chosen,
                    interactive,
                ),
            ));
        }
        for person in people
            .iter()
            .filter(|p| !own.contains(&p.calendar.to_lowercase()))
        {
            rows.push((
                name(&person.calendar),
                person.calendar.clone(),
                if result.is_none() {
                    "not checked".into()
                } else if person.known {
                    person
                        .via()
                        .map(|email| format!("via {email}"))
                        .unwrap_or_else(|| "free / busy shared".into())
                } else {
                    person.error.clone()
                },
                Track::new(id, &q, vec![person.clone()], chosen, interactive),
            ));
        }
        let unknown = people.iter().filter(|p| !p.known).count();
        let conflicts: Vec<_> = rows
            .iter()
            .filter(|(_, _, _, track)| track.conflict)
            .map(|(name, ..)| name.as_str())
            .collect();
        let selection = chosen
            .map(|(start, end)| {
                format!(
                    "{} – {} · {} min{}",
                    dates::label(start, &q.zone),
                    dates::local(end, &q.zone).get(11..).unwrap_or(""),
                    q.minutes,
                    if conflicts.is_empty() {
                        String::new()
                    } else {
                        format!("\nOverlaps busy time: {}", conflicts.join(", "))
                    }
                )
            })
            .unwrap_or_else(|| {
                if interactive {
                    "Drag on a track to choose a time".into()
                } else {
                    String::new()
                }
            });
        let status = if !error.is_empty() {
            error
        } else if dirty {
            "Settings changed. Check availability to find times.".into()
        } else if let Some(r) = &result {
            format!(
                "{} · {}/{} calendars checked\n{} candidate times",
                if r.complete {
                    "All availability checked"
                } else {
                    "Partial availability"
                },
                people.len() - unknown,
                people.len(),
                r.slots.len()
            )
        } else {
            "Checking availability…".into()
        };
        let notice = if result.is_some() && unknown > 0 {
            format!("{} calendar{} unavailable. Suggested times only consider the calendars we can check.\n\n{}",unknown,if unknown==1{" is"}else{"s are"},people.iter().filter(|p|!p.known).map(|p|format!("{} — {}",name(&p.calendar),p.failure())).collect::<Vec<_>>().join("\n\n"))
        } else {
            String::new()
        };
        let slots = result.as_ref().map(|r| r.slots.as_slice()).unwrap_or(&[]);
        let shown = if self.expanded {
            slots.len()
        } else {
            slots.len().min(5)
        };
        let more = slots.len() > shown;
        let heading = if unknown == 0 && result.is_some() {
            "Times that work for everyone"
        } else {
            "Suggested times"
        };
        let footer = if let Some(r) = &result {
            if slots.is_empty() {
                if people.iter().all(|p| !p.known) {
                    "No calendars could be checked. Verify access, then try again.".into()
                } else {
                    "No opening in this window. Try another date, a shorter meeting, or wider hours.".into()
                }
            } else {
                format!("{} candidate times · checked {}. Choose a time, then use this time to update your draft.",slots.len(),dates::label(r.checked,&q.zone))
            }
        } else {
            String::new()
        };
        let mut drawn = Vec::new();
        let mut tracks = Vec::new();
        let mut controls = None;
        while let Some(item) = self.view.draw_walk(cx, scope, walk).step() {
            let lr = item.as_portal_list();
            let Some(mut list) = lr.borrow_mut() else {
                continue;
            };
            let notice_i = 1 + rows.len();
            let heading_i = notice_i + 1;
            let first_slot = heading_i + 1;
            let more_i = first_slot + shown;
            let footer_i = more_i + usize::from(more);
            list.set_item_range(cx, 0, footer_i + 1);
            while let Some(i) = list.next_visible_item(cx) {
                if i == 0 {
                    let w = list.item(cx, i, live_id!(controls));
                    if self.shown != search || self.controls.widget_uid() != w.widget_uid() {
                        for ((_, path), value) in FIELDS.iter().zip([
                            &search.day,
                            &search.end_day,
                            &search.from,
                            &search.until,
                            &search.minutes,
                            &search.zone,
                        ]) {
                            w.text_input(cx, path).set_text(cx, value);
                        }
                        self.shown = search.clone();
                    }
                    w.widget(cx, ids!(range_row))
                        .set_visible(cx, search.day != search.end_day);
                    w.label(cx, ids!(count_lbl))
                        .set_text(cx, &format!("{} people", rows.len()));
                    w.label(cx, ids!(status_lbl)).set_text(cx, &status);
                    w.label(cx, ids!(selection_lbl)).set_text(cx, &selection);
                    w.label(cx, ids!(selection_lbl)).set_text_color(
                        cx,
                        if conflicts.is_empty() {
                            vec4(0.078, 0.078, 0.078, 1.0)
                        } else {
                            vec4(0.68, 0.18, 0.08, 1.0)
                        },
                    );
                    w.widget(cx, ids!(drag_hint)).set_visible(cx, interactive);
                    for (i, path) in [ids!(t0), ids!(t1), ids!(t2), ids!(t3), ids!(t4)]
                        .iter()
                        .enumerate()
                    {
                        let at = a + (b - a) * i as f64 / 4.0;
                        let label = dates::utc(at)
                            .with_timezone(&dates::zone(&q.zone).unwrap_or(chrono_tz::UTC))
                            .format(if search.day == search.end_day {
                                "%H:%M"
                            } else {
                                "%d %b"
                            })
                            .to_string();
                        w.label(cx, *path).set_text(cx, &label);
                    }
                    w.draw_all(cx, scope);
                    controls = Some(w);
                } else if let Some((title, detail, sharing, track)) = rows.get(i - 1) {
                    let w = list.item(cx, i, live_id!(person));
                    w.label(cx, ids!(name_lbl)).set_text(cx, title);
                    w.label(cx, ids!(detail_lbl)).set_text(cx, detail);
                    w.label(cx, ids!(sharing_lbl)).set_text(cx, sharing);
                    if let Some(mut widget) =
                        w.widget(cx, ids!(track)).borrow_mut::<CalendarTimeTrack>()
                    {
                        widget.track = track.clone();
                    }
                    w.draw_all(cx, scope);
                    tracks.push((
                        w.widget(cx, ids!(track)),
                        format!("availability track {title}"),
                    ));
                } else if i == notice_i {
                    let w = list.item(cx, i, live_id!(notice));
                    w.set_visible(cx, !notice.is_empty());
                    w.label(cx, ids!(body_lbl)).set_text(cx, &notice);
                    w.draw_all(cx, scope);
                } else if i == heading_i {
                    let w = list.item(cx, i, live_id!(heading));
                    w.label(cx, ids!(title_lbl)).set_text(cx, heading);
                    w.draw_all(cx, scope);
                } else if i >= first_slot && i < more_i {
                    let n = i - first_slot;
                    let (start, end) = slots[n];
                    let w = list.item(cx, i, live_id!(slot));
                    for (path, on) in [
                        (ids!(normal), selected != Some(start)),
                        (ids!(selected), selected == Some(start)),
                    ] {
                        let row = w.widget(cx, path);
                        row.set_visible(cx, on);
                        let time = |t| {
                            dates::utc(t)
                                .with_timezone(&dates::zone(&q.zone).unwrap_or(chrono_tz::UTC))
                                .format("%H:%M")
                                .to_string()
                        };
                        row.label(cx, ids!(time_lbl))
                            .set_text(cx, &format!("{} — {}", time(start), time(end)));
                        row.label(cx, ids!(date_lbl)).set_text(
                            cx,
                            &format!(
                                "{} · {}",
                                dates::utc(start)
                                    .with_timezone(&dates::zone(&q.zone).unwrap_or(chrono_tz::UTC))
                                    .format("%a %d %B"),
                                if unknown == 0 {
                                    "everyone available"
                                } else {
                                    "check with unavailable guests"
                                }
                            ),
                        );
                    }
                    w.draw_all(cx, scope);
                    drawn.push((w, Pick::Slot(n), format!("select time {}", n + 1)));
                } else if more && i == more_i {
                    let w = list.item(cx, i, live_id!(more));
                    w.draw_all(cx, scope);
                    drawn.push((w, Pick::More, "show more times".into()));
                } else if i == footer_i {
                    let w = list.item(cx, i, live_id!(footer));
                    w.label(cx, ids!(body_lbl)).set_text(cx, &footer);
                    w.draw_all(cx, scope);
                }
            }
        }
        self.picks.clear();
        let clip = self.view.widget(cx, ids!(list)).area().rect(cx);
        self.tracks.clear();
        for (track, label) in tracks {
            if let Some(r) = props.hits.add_clipped(
                label,
                track.area().rect(cx),
                clip,
                if interactive {
                    MouseCursor::Grab
                } else {
                    MouseCursor::Default
                },
                props.slot,
            ) {
                self.tracks.push((track, r));
            }
        }
        for (w, pick, label) in drawn {
            if let Some(r) = props.hits.add_clipped(
                label,
                w.area().rect(cx),
                clip,
                MouseCursor::Hand,
                props.slot,
            ) {
                self.picks.push((r, pick));
            }
        }
        if let Some(w) = controls {
            for (name, path) in FIELDS {
                props.hits.add_clipped(
                    name,
                    w.widget(cx, path).area().rect(cx),
                    clip,
                    MouseCursor::Text,
                    props.slot,
                );
            }
            for (name, path, pick) in [
                (
                    "previous availability date",
                    ids!(previous_btn),
                    Pick::Previous,
                ),
                ("next availability date", ids!(next_btn), Pick::Next),
            ] {
                if let Some(r) = props.hits.add_clipped(
                    name,
                    w.widget(cx, path).area().rect(cx),
                    clip,
                    MouseCursor::Hand,
                    props.slot,
                ) {
                    self.picks.push((r, pick));
                }
            }
            props.hits.add_clipped(
                status,
                w.label(cx, ids!(status_lbl)).area().rect(cx),
                clip,
                MouseCursor::Default,
                props.slot,
            );
            self.controls = w;
        }
        let fields = offers(cx, &self.controls);
        let bounds = self.view.area().rect(cx);
        self.offers
            .draw(cx, scope, &props, &fields, &mut self.suggest, bounds);
        if let Some((uid, at)) = self.hover {
            let text = self
                .tracks
                .iter()
                .find(|(track, r)| track.widget_uid() == uid && r.contains(at))
                .and_then(|(widget, _)| {
                    let track = widget.borrow::<CalendarTimeTrack>()?;
                    let rect = track.area().rect(cx);
                    let time = a + (at.x - rect.pos.x) / rect.size.x * (b - a);
                    let texts: Vec<_> = track
                        .track
                        .people
                        .iter()
                        .filter_map(|p| hover_text(p, time, &q.zone))
                        .collect();
                    (!texts.is_empty()).then(|| texts.join("\n\n"))
                });
            if let Some(text) = text {
                let width = 330.0f64.min(bounds.size.x);
                let lines = text
                    .lines()
                    .map(|line| {
                        (line.chars().count() as f64 * 6.5 / (width - 24.0).max(1.0))
                            .ceil()
                            .max(1.0)
                    })
                    .sum::<f64>()
                    .min(12.0);
                let height = (24.0 + 18.0 * lines).min(bounds.size.y);
                let x = (at.x + 12.0).clamp(bounds.pos.x, bounds.pos.x + bounds.size.x - width);
                let y = if at.y + 18.0 + height <= bounds.pos.y + bounds.size.y {
                    at.y + 18.0
                } else {
                    (at.y - height - 12.0).max(bounds.pos.y)
                };
                self.tooltip.label(cx, ids!(body_lbl)).set_text(cx, &text);
                self.tooltip.draw_walk_all(
                    cx,
                    scope,
                    Walk {
                        abs_pos: Some(dvec2(x, y)),
                        width: Size::Fixed(width),
                        height: Size::Fixed(height),
                        ..Walk::fit()
                    },
                );
            }
        }
        DrawStep::done()
    }
}
