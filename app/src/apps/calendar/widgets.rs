use super::{
    completion::{self, Field as CompletionField},
    completion_ui::Offers,
    dates, edit, model, panels, scoped,
};
use crate::reader::{self, pictures};
use crate::shell::{
    hosted::PanelProps,
    widgets::{
        form::{self, ClickFocus},
        table::{self, RowSpec, TableView},
    },
};
use kernel::{nav::Nav, panel::PanelId, richtable::ListState, session::Session};
use makepad_widgets::*;

struct EventRows;
impl RowSpec for EventRows {
    type Src = scoped::Events;
    type Panel = panels::Timeline;
    fn list(p: &mut Self::Panel) -> &mut ListState<Self::Src> {
        &mut p.list
    }
    fn row_tpl() -> LiveId {
        live_id!(row)
    }
    fn seed_filter(p: &Self::Panel) -> String {
        p.filter.clone()
    }
    fn populate(
        cx: &mut Cx,
        row: &WidgetRef,
        r: &model::Event,
        selected: bool,
        marked: bool,
        _: f64,
    ) {
        let line = table::line(cx, row, selected, marked);

        line.label(cx, ids!(body.title_lbl)).set_text(
            cx,
            if r.title.is_empty() {
                "(untitled event)"
            } else {
                &r.title
            },
        );
        let when = if r.all_day {
            "all day".into()
        } else {
            format!(
                "{} – {}",
                dates::local(r.start, &r.zone)
                    .split('T')
                    .nth(1)
                    .unwrap_or(""),
                dates::local(r.end, &r.zone).split('T').nth(1).unwrap_or("")
            )
        };
        line.label(cx, ids!(body.time_lbl)).set_text(cx, &when);
        line.label(cx, ids!(body.detail_lbl)).set_text(
            cx,
            &format!(
                "{} · {}{}{}{}",
                r.calendar,
                r.email,
                if r.meet.is_empty() { "" } else { " · Meet" },
                if r.series.is_empty() {
                    ""
                } else {
                    " · repeats"
                },
                if r.response == "needsAction" {
                    " · needs reply"
                } else {
                    ""
                }
            ),
        );
    }
    fn section(row: &model::Event, previous: Option<&model::Event>) -> Option<String> {
        previous.is_none_or(|p| p.day != row.day).then(|| {
            dates::date(&row.day)
                .map(|d| d.format("%A %d %B").to_string())
                .unwrap_or(row.day.clone())
        })
    }
    fn label(r: &model::Event, _: f64) -> String {
        r.title.clone()
    }
    fn target(r: &model::Event) -> PanelId {
        panels::Event::id(r.id)
    }
    fn empty_line(p: &Self::Panel, _: &str) -> String {
        if model::sources(&p.store).is_empty() {
            "connect Google Calendar in Accounts to see your events"
        } else {
            "no events under this filter"
        }
        .into()
    }
}
#[derive(Script, ScriptHook, Widget)]
pub struct CalendarTimelinePanel {
    #[source]
    source: ScriptObjectRef,
    #[deref]
    view: View,
    #[live]
    suggest: View,
    #[rust]
    table: TableView<EventRows>,
}
impl Widget for CalendarTimelinePanel {
    fn handle_event(&mut self, cx: &mut Cx, event: &Event, scope: &mut Scope) {
        self.table.handle_event(cx, event, scope, &mut self.view);
    }
    fn draw_walk(&mut self, cx: &mut Cx2d, scope: &mut Scope, walk: Walk) -> DrawStep {
        if let Some(s) = scope.data.get::<Session>() {
            self.view
                .label(cx, ids!(status_lbl))
                .set_text(cx, &model::sync_line(s.store()));
        }
        self.table
            .draw(cx, scope, walk, &mut self.view, &mut self.suggest)
    }
}
fn mine(props: &PanelProps, p: DVec2) -> bool {
    props.hits.at(p).map(|h| h.slot) == Some(Some(props.slot))
}
fn hit(props: &PanelProps, cx: &mut Cx2d, name: impl Into<String>, w: &WidgetRef, clip: Rect) {
    if !w.visible() || !w.area().is_valid(cx) {
        return;
    }
    props.hits.add_clipped(
        name.into(),
        w.area().rect(cx),
        clip,
        MouseCursor::Hand,
        props.slot,
    );
}
fn text_field(cx: &mut Cx, widget: &WidgetRef, path: &[LiveId], text: &str) {
    let t = widget.text_input(cx, path);
    if t.text() != text {
        t.set_text(cx, text);
    }
}

#[derive(Script, ScriptHook, Widget)]
pub struct CalendarMonthPanel {
    #[source]
    source: ScriptObjectRef,
    #[deref]
    view: View,
    #[rust]
    days: Vec<(Rect, String)>,
    #[rust]
    mounted: bool,
    #[rust]
    selected_day: String,
}
impl Widget for CalendarMonthPanel {
    fn handle_event(&mut self, cx: &mut Cx, event: &Event, scope: &mut Scope) {
        self.view.handle_event(cx, event, scope);
        let Some(props) = scope.props.get::<PanelProps>().cloned() else {
            return;
        };
        if let Event::Actions(a) = event {
            if self
                .view
                .text_input(cx, ids!(filter_input))
                .changed(a)
                .is_some()
            {
                if let Some(p) = props
                    .panel
                    .borrow_mut()
                    .as_any()
                    .downcast_mut::<panels::Month>()
                {
                    p.filter = self.view.text_input(cx, ids!(filter_input)).text();
                }
                self.view.redraw(cx);
            }
        }
        if let Event::KeyDown(k) = event {
            if k.key_code == KeyCode::Slash {
                self.view
                    .text_input(cx, ids!(filter_input))
                    .set_key_focus(cx);
            }
        }
        if let Event::MouseDown(e) = event {
            if !mine(&props, e.abs) {
                return;
            }
            if let Some((_, day)) = self.days.iter().find(|(r, _)| r.contains(e.abs)) {
                self.selected_day = day.clone();
                let target = props
                    .panel
                    .borrow_mut()
                    .as_any()
                    .downcast_mut::<panels::Month>()
                    .map(|p| panels::Timeline::day(day, &p.zone, &p.filter));
                if let (Some(id), Some(s)) = (target, scope.data.get_mut::<Session>()) {
                    s.nav(Nav::Open {
                        from: props.slot,
                        id,
                        fresh: false,
                    });
                }
            }
        }
    }
    fn draw_walk(&mut self, cx: &mut Cx2d, scope: &mut Scope, walk: Walk) -> DrawStep {
        let Some(props) = scope.props.get::<PanelProps>().cloned() else {
            return self.view.draw_walk(cx, scope, walk);
        };
        let Some((month, zone, filter, rows)) = props
            .panel
            .borrow_mut()
            .as_any()
            .downcast_mut::<panels::Month>()
            .map(|p| (p.month.clone(), p.zone.clone(), p.filter.clone(), p.rows()))
        else {
            return self.view.draw_walk(cx, scope, walk);
        };
        if !self.mounted {
            self.view
                .text_input(cx, ids!(filter_input))
                .set_text(cx, &filter);
            self.mounted = true;
        }
        let errors = kernel::filter::parse(&filter).errors;
        self.view.label(cx, ids!(filter_err_lbl)).set_text(
            cx,
            &errors
                .iter()
                .map(|e| e.message.clone())
                .collect::<Vec<_>>()
                .join("; "),
        );
        let grid = dates::grid(&month);
        self.view
            .label(cx, ids!(month_lbl))
            .set_text(cx, &dates::month(&month, 0).format("%B %Y").to_string());
        self.view
            .label(cx, ids!(month_detail_lbl))
            .set_text(cx, &format!("{} · {} events in view", zone, rows.len()));
        let cell_height = ((cx.peek_walk_turtle(walk).size.y - 152.0) / 6.0).max(94.0);
        let capacity = ((cell_height - 52.0) / 23.0).floor().clamp(1.0, 3.0) as usize;
        let today = scope
            .data
            .get::<Session>()
            .map(|s| dates::day(s.now(), &zone))
            .unwrap_or_default();
        let mut drawn = Vec::new();
        while let Some(item) = self.view.draw_walk(cx, scope, walk).step() {
            let lr = item.as_portal_list();
            let Some(mut list) = lr.borrow_mut() else {
                continue;
            };
            list.set_item_range(cx, 0, 6);
            while let Some(i) = list.next_visible_item(cx) {
                if i >= 6 {
                    continue;
                }
                let week = list.item(cx, i, live_id!(week));
                for (col, path) in [
                    ids!(d0),
                    ids!(d1),
                    ids!(d2),
                    ids!(d3),
                    ids!(d4),
                    ids!(d5),
                    ids!(d6),
                ]
                .iter()
                .enumerate()
                {
                    let date = grid[i * 7 + col];
                    let day = date.to_string();
                    let a = dates::midnight(date, &zone).unwrap_or(0.0);
                    let b = dates::midnight(date + chrono::Duration::days(1), &zone).unwrap_or(a);
                    let events = rows
                        .iter()
                        .filter(|e| e.start < b && e.end > a)
                        .collect::<Vec<_>>();
                    let cell = week.widget(cx, *path);
                    if let Some(mut view) = cell.as_view().borrow_mut() {
                        view.walk.height = Size::Fixed(cell_height);
                        view.draw_bg.set_uniform(
                            cx,
                            live_id!(quiet),
                            &[f32::from(col >= 5 || !day.starts_with(&month[..7]))],
                        );
                        view.draw_bg.set_uniform(
                            cx,
                            live_id!(selected),
                            &[f32::from(
                                day == self.selected_day
                                    || (self.selected_day.is_empty() && day == today),
                            )],
                        );
                    }
                    let label = cell.label(cx, ids!(day_lbl));
                    label.set_text(cx, &date.format("%-d").to_string());
                    label.set_visible(cx, day != today);
                    label.set_text_color(
                        cx,
                        if day.starts_with(&month[..7]) {
                            vec4(0.078, 0.078, 0.078, 1.0)
                        } else {
                            vec4(0.565, 0.565, 0.565, 1.0)
                        },
                    );
                    cell.widget(cx, ids!(today_badge))
                        .set_visible(cx, day == today);
                    cell.label(cx, ids!(today_lbl))
                        .set_text(cx, &date.format("%-d").to_string());
                    for (n, path) in [ids!(e0), ids!(e1), ids!(e2)].into_iter().enumerate() {
                        let row = cell.widget(cx, path);
                        row.set_visible(cx, n < capacity && n < events.len());
                        let Some(e) = events.get(n).filter(|_| n < capacity) else {
                            continue;
                        };
                        let text = if e.all_day {
                            e.title.clone()
                        } else {
                            format!(
                                "{} {}",
                                dates::utc(e.start)
                                    .with_timezone(&dates::zone(&zone).unwrap_or(chrono_tz::UTC))
                                    .format("%H:%M"),
                                e.title
                            )
                        };
                        row.label(cx, ids!(title_lbl)).set_text(cx, &text);
                        if let Some(mut view) = row.as_view().borrow_mut() {
                            view.draw_bg.set_uniform(
                                cx,
                                live_id!(dotted),
                                &[(e.source % 2) as f32],
                            );
                        }
                    }
                    let more = cell.label(cx, ids!(more_lbl));
                    more.set_visible(cx, events.len() > capacity);
                    more.set_text(
                        cx,
                        &format!("+{} more", events.len().saturating_sub(capacity)),
                    );
                    drawn.push((cell, day));
                }
                week.draw_all(cx, scope);
            }
        }
        self.days.clear();
        let clip = self.view.widget(cx, ids!(list)).area().rect(cx);
        for (w, day) in drawn {
            if let Some(r) = props.hits.add_row_clipped(
                format!("day {day}"),
                w.area().rect(cx),
                clip,
                MouseCursor::Hand,
                props.slot,
            ) {
                self.days.push((r, day));
            }
        }
        props.hits.add_clipped(
            "filter",
            self.view.text_input(cx, ids!(filter_input)).area().rect(cx),
            self.view.area().rect(cx),
            MouseCursor::Text,
            props.slot,
        );
        DrawStep::done()
    }
}

/// Avoid parsing unchanged descriptions during redraws, and preserve the
/// reader's text selection when only surrounding event state changes.
#[derive(Default)]
struct EventText {
    source: String,
    html: String,
}
impl EventText {
    fn get(&mut self, source: &str, literal: bool) -> &str {
        if self.source != source {
            self.html = if literal {
                reader::html::linked_text(source)
            } else {
                reader::html::linked(source)
            };
            self.source = source.into();
        }
        &self.html
    }
}

#[derive(Script, ScriptHook, Widget)]
pub struct CalendarEventPanel {
    #[source]
    source: ScriptObjectRef,
    #[deref]
    view: View,
    #[rust]
    content: WidgetRef,
    #[rust]
    details: EventText,
    #[rust]
    notes: EventText,
}
impl Widget for CalendarEventPanel {
    fn handle_event(&mut self, cx: &mut Cx, event: &Event, scope: &mut Scope) {
        reader::handle_links(&mut self.view, cx, event, scope);
        match event {
            Event::Actions(a) if pictures::landed(cx, a) => self.view.redraw(cx),
            Event::NetworkResponses(r) if pictures::arrived(cx, r) => self.view.redraw(cx),
            _ => {}
        }
        let Some(props) = scope.props.get::<PanelProps>().cloned() else {
            return;
        };
        if let Event::Actions(a) = event {
            let mut p = props.panel.borrow_mut();
            if let Some(p) = p.as_any().downcast_mut::<panels::Event>() {
                if self.content.button(cx, ids!(scope_btn)).clicked(a) {
                    p.scope = next_scope(&p.scope).into();
                    self.view.redraw(cx);
                }
                if self.content.button(cx, ids!(notify_btn)).clicked(a) {
                    p.notify = !p.notify;
                    self.view.redraw(cx);
                }
            }
        }
    }
    fn draw_walk(&mut self, cx: &mut Cx2d, scope: &mut Scope, walk: Walk) -> DrawStep {
        let Some(props) = scope.props.get::<PanelProps>().cloned() else {
            return self.view.draw_walk(cx, scope, walk);
        };
        let Some((reading, deleting, edit_scope, notify, operation, url)) = props
            .panel
            .borrow_mut()
            .as_any()
            .downcast_mut::<panels::Event>()
            .map(|p| {
                (
                    p.reading(),
                    p.deleting,
                    p.scope.clone(),
                    p.notify,
                    p.operation(),
                    p.url.take(),
                )
            })
        else {
            return self.view.draw_walk(cx, scope, walk);
        };
        if let Some(url) = url {
            cx.open_url(&url, OpenUrlInPlace::No);
        }
        let mut drawn = None;
        while let Some(item) = self.view.draw_walk(cx, scope, walk).step() {
            let lr = item.as_portal_list();
            let Some(mut list) = lr.borrow_mut() else {
                continue;
            };
            list.set_item_range(cx, 0, 1);
            while let Some(i) = list.next_visible_item(cx) {
                if i != 0 {
                    continue;
                }
                let w = list.item(cx, 0, live_id!(content));
                if let Some((e, raw)) = &reading {
                    w.label(cx, ids!(title_lbl)).set_text(cx, &e.title);
                    w.label(cx, ids!(when_lbl)).set_text(cx, &e.when());
                    w.label(cx, ids!(source_lbl))
                        .set_text(cx, &format!("{} · {}", e.calendar, e.email));
                    let meet_status = raw["conferenceData"]["createRequest"]["status"]
                        ["statusCode"]
                        .as_str()
                        .unwrap_or("");
                    let details = format!(
                        "{}{}{}\n{} · {}\n{}{}",
                        if e.location.is_empty() {
                            ""
                        } else {
                            "Location: "
                        },
                        e.location,
                        if e.location.is_empty() { "" } else { "\n" },
                        if raw["transparency"] == "transparent" {
                            "free"
                        } else {
                            "busy"
                        },
                        raw["visibility"].as_str().unwrap_or("default visibility"),
                        if e.meet.is_empty() { "" } else { &e.meet },
                        match meet_status {
                            "pending" => "\nGoogle Meet is being created…",
                            "failure" => "\nGoogle Meet could not be created",
                            _ => "",
                        }
                    );
                    let details_view = w.html(cx, ids!(details_html));
                    reader::set_html(cx, details_view, self.details.get(details.trim(), true));
                    let people = raw["attendees"]
                        .as_array()
                        .map(|a| {
                            a.iter()
                                .map(|g| {
                                    format!(
                                        "{}{} · {}",
                                        model::text(g, "email"),
                                        if g["optional"] == true {
                                            " (optional)"
                                        } else {
                                            ""
                                        },
                                        model::text(g, "responseStatus")
                                    )
                                })
                                .collect::<Vec<_>>()
                                .join("\n")
                        })
                        .unwrap_or_default();
                    text_field(
                        cx,
                        &w,
                        ids!(people_lbl),
                        &format!(
                            "Organizer: {}\n{}",
                            model::text(&raw["organizer"], "email"),
                            people
                        ),
                    );
                    let notes = model::text(raw, "description");
                    let notes_view = w.html(cx, ids!(notes_html));
                    reader::set_html(cx, notes_view, self.notes.get(notes, false));
                    w.widget(cx, ids!(notes_html))
                        .set_visible(cx, !notes.trim().is_empty());
                    w.view(cx, ids!(delete_view)).set_visible(cx, deleting);
                    w.label(cx,ids!(delete_lbl)).set_text(cx,"Delete this event from Google Calendar? Guest notifications follow the setting below.");
                    w.button(cx, ids!(scope_btn))
                        .set_visible(cx, !e.series.is_empty());
                    w.button(cx, ids!(scope_btn))
                        .set_text(cx, &format!("scope: {edit_scope}"));
                    w.button(cx, ids!(notify_btn)).set_text(
                        cx,
                        if notify {
                            "notify guests: yes"
                        } else {
                            "notify guests: no"
                        },
                    );
                } else {
                    w.label(cx, ids!(title_lbl))
                        .set_text(cx, "event unavailable");
                    w.label(cx, ids!(when_lbl))
                        .set_text(cx, "deleted or calendar disconnected");
                    w.view(cx, ids!(delete_view)).set_visible(cx, false);
                    let details_view = w.html(cx, ids!(details_html));
                    reader::set_html(cx, details_view, "");
                    let notes_view = w.html(cx, ids!(notes_html));
                    reader::set_html(cx, notes_view, "");
                    text_field(cx, &w, ids!(people_lbl), "");
                }
                w.label(cx, ids!(state_lbl)).set_text(
                    cx,
                    &operation
                        .as_ref()
                        .map(|(_, state, error)| format!("{state} {error}"))
                        .unwrap_or_default(),
                );
                w.draw_all(cx, scope);
                drawn = Some(w);
            }
        }
        let pics = pictures::link_rects(cx);
        if let Some(w) = drawn {
            let clip = self.view.widget(cx, ids!(list)).area().rect(cx);
            for (path, label, link_label) in [
                (ids!(details_html), "event details", "event details link"),
                (
                    ids!(notes_html),
                    "event description",
                    "event description link",
                ),
            ] {
                let widget = w.widget(cx, path);
                if !widget.visible() || !widget.area().is_valid(cx) {
                    continue;
                }
                let area = widget.area().rect(cx);
                props
                    .hits
                    .add_clipped(label, area, clip, MouseCursor::Text, props.slot);
                for rect in reader::link_runs(cx, &w, path, area, &pics) {
                    props
                        .hits
                        .add_clipped(link_label, rect, clip, MouseCursor::Hand, props.slot);
                }
            }
            for path in [
                ids!(title_lbl),
                ids!(when_lbl),
                ids!(source_lbl),
                ids!(state_lbl),
            ] {
                let l = w.label(cx, path);
                if !l.text().is_empty() {
                    hit(&props, cx, l.text(), &w.widget(cx, path), clip);
                }
            }
            if deleting {
                for path in [ids!(scope_btn), ids!(notify_btn)] {
                    hit(
                        &props,
                        cx,
                        w.button(cx, path).text(),
                        &w.widget(cx, path),
                        clip,
                    );
                }
            }
            self.content = w;
        }
        DrawStep::done()
    }
}
fn next_scope(s: &str) -> &'static str {
    match s {
        "this" => "all",
        "all" => "following",
        _ => "this",
    }
}
const FIELDS: [(&str, &[LiveId]); 9] = [
    ("event title", ids!(title_input)),
    ("event start", ids!(start_input)),
    ("event end", ids!(end_input)),
    ("event time zone", ids!(zone_input)),
    ("event guests", ids!(guests_input)),
    ("event location", ids!(location_input)),
    ("event recurrence", ids!(recurrence_input)),
    ("event reminders", ids!(reminders_input)),
    ("event notes", ids!(notes_input)),
];
fn editor_offers(cx: &mut Cx, form: &WidgetRef) -> Vec<(CompletionField, TextInputRef)> {
    [
        (CompletionField::Guests, ids!(guests_input)),
        (CompletionField::Zone, ids!(zone_input)),
        (CompletionField::Location, ids!(location_input)),
        (CompletionField::Repeat, ids!(recurrence_input)),
        (CompletionField::Reminders, ids!(reminders_input)),
    ]
    .into_iter()
    .map(|(kind, path)| (kind, form.text_input(cx, path)))
    .collect()
}

#[derive(Script, ScriptHook, Widget)]
pub struct CalendarEditorPanel {
    #[source]
    source: ScriptObjectRef,
    #[deref]
    view: View,
    #[live]
    suggest: View,
    #[rust]
    offers: Offers,
    #[rust]
    form: WidgetRef,
    #[rust]
    click_focus: ClickFocus,
}
impl Widget for CalendarEditorPanel {
    fn handle_event(&mut self, cx: &mut Cx, event: &Event, scope: &mut Scope) {
        let Some(props) = scope.props.get::<PanelProps>().cloned() else {
            return;
        };
        let inputs = FIELDS
            .iter()
            .map(|(_, p)| self.form.text_input(cx, p))
            .collect::<Vec<_>>();
        let fields = editor_offers(cx, &self.form);
        let completion = self.offers.handle(cx, event, &props, &fields);
        if completion == Some(false) {
            self.view.redraw(cx);
            return;
        }
        if completion.is_none() {
            self.view.handle_event(cx, event, scope);
        }
        if completion.is_none() {
            if let Some(i) = form::tab(cx, event, &inputs) {
                form::reveal(cx, &self.view.portal_list(cx, ids!(list)), &inputs[i]);
            }
        }
        // Portal items may redraw between press and release. Resolve form
        // controls from the last visible rectangles, as the Accounts form does.
        let pressed = if let Event::MouseDown(e) = event {
            props
                .hits
                .at(e.abs)
                .filter(|h| h.slot == Some(props.slot))
                .map(|h| h.label)
        } else {
            None
        };
        self.click_focus.handle(
            cx,
            event,
            &props,
            &inputs,
            &FIELDS.iter().map(|(name, _)| *name).collect::<Vec<_>>(),
        );
        let actions = if let Event::Actions(actions) = event {
            Some(actions)
        } else {
            None
        };
        if actions.is_none() && pressed.is_none() && completion != Some(true) {
            return;
        }
        let mut borrow = props.panel.borrow_mut();
        let Some(p) = borrow.as_any().downcast_mut::<panels::Editor>() else {
            return;
        };
        let Some(d) = p.reading() else { return };
        if !edit::editable(&d) {
            return;
        }
        let mut f = d.form.clone();
        let mut source = d.source;
        let mut changed = completion == Some(true)
            || actions.is_some_and(|a| inputs.iter().any(|t| t.changed(a).is_some()));
        if changed {
            f.title = inputs[0].text();
            f.start = inputs[1].text();
            f.end = inputs[2].text();
            f.zone = inputs[3].text();
            f.guests = inputs[4].text();
            f.location = inputs[5].text();
            f.recurrence = inputs[6].text();
            f.reminders = inputs[7].text();
            f.notes = inputs[8].text();
        }
        macro_rules! clicked {
            ($label:literal) => {
                pressed.as_deref() == Some($label)
            };
        }
        if clicked!("event calendar") && d.event.is_none() {
            let calendars = model::sources(&p.store)
                .into_iter()
                .filter(|c| c.writable())
                .collect::<Vec<_>>();
            if !calendars.is_empty() {
                let i = calendars.iter().position(|c| c.id == d.source).unwrap_or(0);
                source = calendars[(i + 1) % calendars.len()].id;
                changed = true;
            }
        }
        if clicked!("event all day") {
            f.all_day = !f.all_day;
            if f.all_day {
                f.start = f.start.get(..10).unwrap_or("").into();
                f.end = f.end.get(..10).unwrap_or("").into();
                if f.end <= f.start {
                    if let Ok(d) = dates::date(&f.start) {
                        f.end = (d + chrono::Duration::days(1)).to_string();
                    }
                }
            } else {
                f.start = format!("{}T09:00", f.start);
                f.end = format!("{}T10:00", f.start.get(..10).unwrap_or(""));
            }
            changed = true;
        }
        if clicked!("event Meet") {
            f.meet = !f.meet;
            changed = true;
        }
        if clicked!("event busy") {
            f.busy = !f.busy;
            changed = true;
        }
        if clicked!("notify guests") {
            f.notify = !f.notify;
            changed = true;
        }
        if clicked!("guests can edit") {
            f.guests_modify = !f.guests_modify;
            changed = true;
        }
        if clicked!("guests can invite") {
            f.guests_invite = !f.guests_invite;
            changed = true;
        }
        if clicked!("guests can see") {
            f.guests_see = !f.guests_see;
            changed = true;
        }
        if clicked!("event scope") {
            f.scope = next_scope(&f.scope).into();
            changed = true;
        }
        if clicked!("event visibility") {
            f.visibility = match f.visibility.as_str() {
                "default" => "private",
                "private" => "public",
                _ => "default",
            }
            .into();
            changed = true;
        }
        if clicked!("event repeat") {
            f.recurrence = match f.recurrence.as_str() {
                "" => "RRULE:FREQ=DAILY",
                "RRULE:FREQ=DAILY" => "RRULE:FREQ=WEEKLY",
                "RRULE:FREQ=WEEKLY" => "RRULE:FREQ=MONTHLY",
                "RRULE:FREQ=MONTHLY" => "RRULE:FREQ=YEARLY",
                _ => "",
            }
            .into();
            changed = true;
        }
        if changed {
            if let Some(s) = scope.data.get_mut::<Session>() {
                p.save(s, d.revision, source, f);
                self.view.redraw(cx);
            }
        }
    }
    fn draw_walk(&mut self, cx: &mut Cx2d, scope: &mut Scope, walk: Walk) -> DrawStep {
        let Some(props) = scope.props.get::<PanelProps>().cloned() else {
            return self.view.draw_walk(cx, scope, walk);
        };
        let Some((d, error, sources)) = props
            .panel
            .borrow_mut()
            .as_any()
            .downcast_mut::<panels::Editor>()
            .map(|p| (p.reading(), p.error.clone(), model::sources(&p.store)))
        else {
            return self.view.draw_walk(cx, scope, walk);
        };
        self.view.label(cx, ids!(error_lbl)).set_text(cx, &error);
        self.view.label(cx, ids!(status_lbl)).set_text(
            cx,
            &d.as_ref()
                .map(|d| {
                    format!(
                        "{}{}{}",
                        match d.state.as_str() {
                            "draft" => "draft saved locally",
                            "pending" => "saving to Google Calendar…",
                            "done" => "saved to Google Calendar",
                            _ => "could not save to Google Calendar",
                        },
                        if d.error.is_empty() { "" } else { " · " },
                        d.error
                    )
                })
                .unwrap_or_default(),
        );
        let mut drawn = None;
        while let Some(item) = self.view.draw_walk(cx, scope, walk).step() {
            let lr = item.as_portal_list();
            let Some(mut list) = lr.borrow_mut() else {
                continue;
            };
            list.set_item_range(cx, 0, usize::from(d.is_some()));
            while let Some(i) = list.next_visible_item(cx) {
                if i != 0 {
                    continue;
                }
                let w = list.item(cx, 0, live_id!(form));
                if let Some(d) = &d {
                    let f = &d.form;
                    for ((_, path), value) in FIELDS.iter().zip([
                        &f.title,
                        &f.start,
                        &f.end,
                        &f.zone,
                        &f.guests,
                        &f.location,
                        &f.recurrence,
                        &f.reminders,
                        &f.notes,
                    ]) {
                        text_field(cx, &w, path, value);
                    }
                    let source = sources.iter().find(|c| c.id == d.source);
                    w.button(cx, ids!(source_btn)).set_text(
                        cx,
                        &source
                            .map(|c| format!("{} · {}", c.title, c.email))
                            .unwrap_or("calendar disconnected".into()),
                    );
                    for (path, text) in [
                        (ids!(all_day_btn), if f.all_day { "yes" } else { "no" }),
                        (ids!(meet_btn), if f.meet { "on" } else { "off" }),
                        (ids!(busy_btn), if f.busy { "busy" } else { "free" }),
                        (ids!(scope_btn), &f.scope),
                        (ids!(visibility_btn), &f.visibility),
                        (
                            ids!(notify_btn),
                            if f.notify {
                                "notify guests: yes"
                            } else {
                                "notify guests: no"
                            },
                        ),
                        (
                            ids!(modify_btn),
                            if f.guests_modify {
                                "edit: yes"
                            } else {
                                "edit: no"
                            },
                        ),
                        (
                            ids!(invite_btn),
                            if f.guests_invite {
                                "invite others: yes"
                            } else {
                                "invite others: no"
                            },
                        ),
                        (
                            ids!(see_btn),
                            if f.guests_see {
                                "see guest list: yes"
                            } else {
                                "see guest list: no"
                            },
                        ),
                    ] {
                        w.button(cx, path).set_text(cx, text);
                    }
                    w.button(cx, ids!(repeat_btn))
                        .set_text(cx, completion::repeat_label(&f.recurrence));
                    w.label(cx, ids!(day_hint)).set_visible(cx, f.all_day);
                    w.widget(cx, ids!(scope_row))
                        .set_visible(cx, !model::text(&d.base, "recurringEventId").is_empty());
                }
                w.draw_all(cx, scope);
                drawn = Some(w);
            }
        }
        if let Some(w) = drawn {
            let clip = self.view.widget(cx, ids!(list)).area().rect(cx);
            for (name, path) in FIELDS {
                props.hits.add_clipped(
                    name,
                    w.widget(cx, path).area().rect(cx),
                    clip,
                    MouseCursor::Text,
                    props.slot,
                );
            }
            for (name, path) in [
                ("event calendar", ids!(source_btn)),
                ("event all day", ids!(all_day_btn)),
                ("event Meet", ids!(meet_btn)),
                ("event repeat", ids!(repeat_btn)),
                ("event scope", ids!(scope_btn)),
                ("event busy", ids!(busy_btn)),
                ("event visibility", ids!(visibility_btn)),
                ("notify guests", ids!(notify_btn)),
                ("guests can edit", ids!(modify_btn)),
                ("guests can invite", ids!(invite_btn)),
                ("guests can see", ids!(see_btn)),
            ] {
                hit(&props, cx, name, &w.widget(cx, path), clip);
            }
            self.form = w;
        }
        for path in [ids!(error_lbl), ids!(status_lbl)] {
            let l = self.view.label(cx, path);
            if !l.text().is_empty() {
                props.hits.add_clipped(
                    l.text(),
                    l.area().rect(cx),
                    self.view.area().rect(cx),
                    MouseCursor::Default,
                    props.slot,
                );
            }
        }
        let fields = editor_offers(cx, &self.form);
        let clip = self.view.area().rect(cx);
        self.offers
            .draw(cx, scope, &props, &fields, &mut self.suggest, clip);
        DrawStep::done()
    }
}

#[derive(Clone)]
enum SourceAction {
    Open(PanelId),
    New(i64),
}

#[derive(Script, ScriptHook, Widget)]
pub struct CalendarSourcesPanel {
    #[source]
    source: ScriptObjectRef,
    #[deref]
    view: View,
    #[rust]
    links: Vec<(Rect, SourceAction)>,
}
impl Widget for CalendarSourcesPanel {
    fn handle_event(&mut self, cx: &mut Cx, event: &Event, scope: &mut Scope) {
        self.view.handle_event(cx, event, scope);
        let Some(props) = scope.props.get::<PanelProps>().cloned() else {
            return;
        };
        if let Event::MouseDown(e) = event {
            if !mine(&props, e.abs) {
                return;
            }
            if let Some((_, action)) = self.links.iter().find(|(r, _)| r.contains(e.abs)) {
                if let Some(s) = scope.data.get_mut::<Session>() {
                    match action {
                        SourceAction::New(source) => {
                            panels::start_editor(s, props.slot, *source, None)
                        }
                        SourceAction::Open(id) => s.nav(Nav::Open {
                            from: props.slot,
                            id: id.clone(),
                            fresh: false,
                        }),
                    }
                }
            }
        }
    }
    fn draw_walk(&mut self, cx: &mut Cx2d, scope: &mut Scope, walk: Walk) -> DrawStep {
        let Some(props) = scope.props.get::<PanelProps>().cloned() else {
            return self.view.draw_walk(cx, scope, walk);
        };
        let (sources, status) = scope
            .data
            .get::<Session>()
            .map(|s| (model::sources(s.store()), model::sync_line(s.store())))
            .unwrap_or_default();
        self.view.label(cx, ids!(status_lbl)).set_text(
            cx,
            if sources.is_empty() {
                "connect Google Calendar in Accounts"
            } else {
                &status
            },
        );
        let mut drawn = Vec::new();
        while let Some(item) = self.view.draw_walk(cx, scope, walk).step() {
            let lr = item.as_portal_list();
            let Some(mut list) = lr.borrow_mut() else {
                continue;
            };
            list.set_item_range(cx, 0, sources.len());
            while let Some(i) = list.next_visible_item(cx) {
                let Some(c) = sources.get(i) else { continue };
                let w = list.item(cx, i, live_id!(source));
                w.label(cx, ids!(title_lbl)).set_text(cx, &c.title);
                w.label(cx, ids!(detail_lbl)).set_text(
                    cx,
                    &format!(
                        "{} · {}\n{}{}\n{}",
                        c.email,
                        c.role,
                        c.zone,
                        if c.meet { " · Google Meet" } else { "" },
                        c.error
                    ),
                );
                w.button(cx, ids!(new_btn)).set_visible(cx, c.writable());
                w.draw_all(cx, scope);
                drawn.push((w, c.clone()));
            }
        }
        self.links.clear();
        let clip = self.view.widget(cx, ids!(list)).area().rect(cx);
        for (w, c) in drawn {
            for (path, id, name) in [
                (
                    ids!(open_btn),
                    SourceAction::Open(PanelId::new(
                        panels::Timeline::TAG,
                        [format!("@source:{}", c.id)],
                    )),
                    format!("show {} events", c.title),
                ),
                (
                    ids!(new_btn),
                    SourceAction::New(c.id),
                    format!("new event in {}", c.title),
                ),
            ] {
                let b = w.widget(cx, path);
                if b.visible() {
                    if let Some(r) = props.hits.add_row_clipped(
                        name,
                        b.area().rect(cx),
                        clip,
                        MouseCursor::Hand,
                        props.slot,
                    ) {
                        self.links.push((r, id));
                    }
                }
            }
        }
        DrawStep::done()
    }
}
