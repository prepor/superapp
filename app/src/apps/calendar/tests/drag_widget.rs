//! Pointer routing and draw-list construction without OS initialization,
//! rendering, windows or an event loop.
use super::*;
use std::cell::Cell;

#[cfg(headless)]
pub fn exercise(s: &mut Session, slot: kernel::layout::SlotId, q: &availability::Query) {
    let mut cx = Cx::new(Box::new(|_, _| {}));
    let (sheet, track) = cx.with_vm(|vm| {
        makepad_widgets::script_mod(vm);
        crate::shell::script_mod(vm);
        crate::reader::ui::script_mod(vm);
        super::super::ui::script_mod(vm);
        let sheet = script_eval!(vm, {mod.widgets.CalendarAvailabilityPanel{}});
        let track = script_eval!(vm, {mod.widgets.CalendarTimeTrack{}});
        (
            WidgetRef::script_from_value(vm, sheet),
            WidgetRef::script_from_value(vm, track),
        )
    });
    // Seed only an in-memory hit rectangle, without a draw pass or renderer.
    let list = cx.draw_lists.alloc();
    let rect = rect(40.0, 80.0, 480.0, 34.0);
    cx.draw_lists[list.id()].rect_areas.push(CxRectArea {
        rect,
        draw_clip: (rect.pos, rect.pos + rect.size),
    });
    let area = Area::Rect(RectArea {
        draw_list_id: list.id(),
        rect_id: 0,
        redraw_id: 0,
    });
    assert_eq!(form::drawn_rect(&cx, area), Some(rect));
    let empty = Area::Instance(InstanceArea {
        draw_list_id: list.id(),
        draw_item_id: 0,
        instance_offset: 0,
        instance_count: 0,
        redraw_id: 0,
    });
    assert!(form::drawn_rect(&cx, empty).is_none());
    let stale = Area::Rect(RectArea {
        draw_list_id: list.id(),
        rect_id: 0,
        redraw_id: 1,
    });
    assert!(form::drawn_rect(&cx, stale).is_none());

    let panel = s.panel(slot).unwrap();
    let (request, checked) = {
        let mut p = panel.borrow_mut();
        let p = p.as_any().downcast_mut::<panels::Availability>().unwrap();
        (p.request, p.preview(s.now()).unwrap())
    };
    let (a, b) = q.validate().unwrap();
    track.borrow_mut::<CalendarTimeTrack>().unwrap().track = Track::new(
        request,
        q,
        checked.result.people.clone(),
        Some((a, a + f64::from(q.minutes) * 60.0)),
        true,
    );
    sheet
        .borrow_mut::<CalendarAvailabilityPanel>()
        .unwrap()
        .tracks
        .push((track.clone(), rect));
    let props = PanelProps {
        slot,
        panel: panel.clone(),
        hits: Default::default(),
        keyboard: Default::default(),
        grab: Default::default(),
    };
    let window_id = WindowId(0, 0); // An event identifier only; no window exists.
    let button = MouseButton::PRIMARY;
    cx.fingers.mouse_down(button, window_id);
    let down = Event::MouseDown(MouseDownEvent {
        abs: dvec2(40.0, 96.0),
        button,
        window_id,
        modifiers: Default::default(),
        handled: Cell::default(),
        time: 0.0,
    });
    let send = |cx: &mut Cx, s: &mut Session, event: &Event| {
        let actions = cx.capture_actions(|cx| {
            track
                .borrow_mut::<CalendarTimeTrack>()
                .unwrap()
                .pointer(cx, event, area, s.now());
        });
        let count = actions
            .filter_widget_actions(track.widget_uid())
            .filter(|a| matches!(a.cast::<TrackAction>(), TrackAction::Select { .. }))
            .count();
        if !actions.is_empty() {
            sheet.handle_event(
                cx,
                &Event::Actions(actions),
                &mut Scope::with_data_props(s, &props),
            );
        }
        count
    };
    send(&mut cx, s, &down);
    s.take_dirty();
    s.store().trace_begin(9002);
    let mut changes = 0;
    let started = std::time::Instant::now();
    for i in 0..10_000 {
        let event = Event::MouseMove(MouseMoveEvent {
            abs: dvec2(40.0 + f64::from(i) / 10_000.0 * 480.0, 96.0),
            lock_delta: dvec2(0.0, 0.0),
            window_id,
            modifiers: Default::default(),
            time: f64::from(i) / 1000.0,
            handled: Cell::default(),
        });
        changes += send(&mut cx, s, &event);
    }
    let up = Event::MouseUp(MouseUpEvent {
        abs: dvec2(900.0, 200.0),
        button,
        window_id,
        modifiers: Default::default(),
        time: 11.0,
    });
    changes += send(&mut cx, s, &up);
    cx.fingers.mouse_up(button);
    s.store().trace_end();
    eprintln!(
        "10,000 pointer moves: {changes} snapped updates in {:?}",
        started.elapsed()
    );
    assert_eq!(
        changes, 30,
        "only the 30 later quarter-hour starts should emit actions"
    );
    assert!(
        !s.take_dirty().any(),
        "a drag must not refresh macOS menus and other panels"
    );
    assert!(
        s.store().trace_of(9002).is_empty(),
        "drag actions must reuse checked data"
    );
    let mut p = panel.borrow_mut();
    let p = p.as_any().downcast_mut::<panels::Availability>().unwrap();
    assert_eq!(p.selected, Some(b - 1800.0));
    assert!(track.borrow::<CalendarTimeTrack>().unwrap().drag.is_none());
    assert!(!cx.fingers.any_areas_captured());

    // A swept area must be ignored before asking Makepad for its rectangle.
    cx.draw_lists[list.id()].rect_areas.clear();
    assert!(form::drawn_rect(&cx, area).is_none());
}

/// Execute widget layout and draw-list construction directly. There is no OS
/// initialization, event loop, window or rendering backend.
#[cfg(headless)]
pub fn draw_panels(s: &mut Session, editor: kernel::layout::SlotId, sheet: kernel::layout::SlotId) {
    draw_panels_case(s, editor, sheet, None);
}

pub fn refresh_during_drag(s: &mut Session, editor: kernel::layout::SlotId, sheet: kernel::layout::SlotId) {
    draw_panels_case(s, editor, sheet, Some(DrawCase::Refresh));
}

pub fn hover_during_motion(s: &mut Session, editor: kernel::layout::SlotId, sheet: kernel::layout::SlotId) {
    draw_panels_case(s, editor, sheet, Some(DrawCase::Hover));
}

pub fn recheck(s: &mut Session, editor: kernel::layout::SlotId, sheet: kernel::layout::SlotId) {
    draw_panels_case(s, editor, sheet, Some(DrawCase::Recheck));
}

#[derive(Clone, Copy)]
enum DrawCase { Refresh, Hover, Recheck }

fn draw_panels_case(s: &mut Session, editor: kernel::layout::SlotId, sheet: kernel::layout::SlotId, case: Option<DrawCase>) {
    // Participant labels come from the same stored directory the real widget
    // reads. Keep the long-label/wrapping coverage after its async migration.
    s.store().write(|tx| tx.execute(
        "UPDATE calendar_event SET raw=json_set(raw,'$.organizer',json_object('email','nora@studio.example','displayName',?1))",
        ["Nora with a long display name that wraps the overlap warning onto another line"],
    )).unwrap();
    use makepad_widgets::makepad_platform::makepad_error_log::{self, LogLevel};
    use std::sync::Mutex;
    static DRAW_TAP: Mutex<()> = Mutex::new(());
    static ERRORS: Mutex<Vec<String>> = Mutex::new(Vec::new());
    let _draw_tap = DRAW_TAP.lock().unwrap_or_else(|error| error.into_inner());
    fn capture(message: &str, _: LogLevel) {
        if message.contains("get_rect called on instance_count") {
            let mut errors = ERRORS.lock().unwrap();
            if errors.is_empty() {
                errors.push(format!(
                    "{message}\n{}",
                    std::backtrace::Backtrace::force_capture()
                ));
            }
        }
    }
    struct Tap;
    impl Drop for Tap {
        fn drop(&mut self) {
            makepad_error_log::set_log_tap(None);
        }
    }
    ERRORS.lock().unwrap().clear();
    makepad_error_log::set_log_tap(Some(capture));
    let _tap = Tap;
    let mut cx = Cx::new(Box::new(|_, _| {}));
    let widgets = cx.with_vm(|vm| {
        makepad_widgets::script_mod(vm);
        crate::shell::script_mod(vm);
        crate::reader::ui::script_mod(vm);
        super::super::ui::script_mod(vm);
        let editor = script_eval!(vm, {mod.widgets.CalendarEditorPanel{}});
        let sheet = script_eval!(vm, {mod.widgets.CalendarAvailabilityPanel{}});
        [
            WidgetRef::script_from_value(vm, editor),
            WidgetRef::script_from_value(vm, sheet),
        ]
    });
    let props = [editor, sheet].map(|slot| PanelProps {
        slot,
        panel: s.panel(slot).unwrap(),
        hits: Default::default(),
        keyboard: Default::default(),
        grab: Default::default(),
    });
    let size = dvec2(1200.0, 740.0);
    let pass = DrawPass::new(&mut cx);
    pass.set_size(&mut cx, size);
    let mut list = DrawList::new(&mut cx);
    let mut frame = 0;
    let mut timings = Vec::new();
    let mut redraw = |cx: &mut Cx, s: &mut Session, size: DVec2| {
        let started = std::time::Instant::now();
        frame += 1;
        for props in &props {
            props.hits.clear();
        }
        cx.new_draw_event = DrawEvent::default();
        pass.set_size(cx, size);
        let event = DrawEvent {
            redraw_all: true,
            time: frame as f64 / 60.0,
            ..Default::default()
        };
        let mut draw = CxDraw::new(cx, &event);
        draw.begin_pass(&pass, Some(1.0));
        list.begin_always(&mut draw);
        {
            let mut cx = Cx2d::new(&mut draw);
            cx.begin_root_turtle(
                size,
                Layout {
                    flow: Flow::right(),
                    ..Layout::default()
                },
            );
            for (widget, props) in widgets.iter().zip(&props) {
                widget.draw_walk_all(
                    &mut cx,
                    &mut Scope::with_data_props(s, props),
                    Walk {
                        width: Size::Fixed(size.x / 2.0),
                        height: Size::fill(),
                        ..Walk::default()
                    },
                );
            }
            cx.end_pass_sized_turtle();
        }
        list.end(&mut draw);
        draw.end_pass(&pass);
        drop(draw);
        timings.push(started.elapsed());
    };
    let send = |cx: &mut Cx, s: &mut Session, event: Event| {
        let mut event = event;
        for _ in 0..8 {
            let actions = cx.capture_actions(|cx| {
                for (widget, props) in widgets.iter().zip(&props) {
                    widget.handle_event(cx, &event, &mut Scope::with_data_props(s, props));
                }
            });
            if actions.is_empty() {
                return;
            }
            event = Event::Actions(actions);
        }
        panic!("widget actions did not settle");
    };
    // Cover empty, populated, then cleared messages, including text clipped out
    // of a short viewport. The old hit collector logged once on every frame.
    for error in ["", "Enter a valid date", "", "   "] {
        props[0]
            .panel
            .borrow_mut()
            .as_any()
            .downcast_mut::<panels::Editor>()
            .unwrap()
            .error = error.into();
        for size in [size, dvec2(960.0, 40.0), size] {
            redraw(&mut cx, s, size);
            assert_eq!(
                widgets[0].label(&cx, ids!(error_lbl)).visible(),
                !error.trim().is_empty()
            );
        }
    }
    let (window_id, button) = (WindowId(0, 0), MouseButton::PRIMARY);
    if matches!(case, Some(DrawCase::Recheck)) {
        let (notify, woke) = std::sync::mpsc::channel();
        s.store().attach_ui(move || { let _ = notify.send(()); });
        let request = {
            let mut panel = props[1].panel.borrow_mut();
            let panel = panel.as_any().downcast_mut::<panels::Availability>().unwrap();
            let mut search = panel.search.clone();
            search.shift(1).unwrap();
            panel.edit_search(search);
            panel.check(s);
            panel.request
        };
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        let mut checked = false;
        loop {
            s.store().poll_external();
            s.settle();
            let current = props[1].panel.borrow_mut().as_any().downcast_mut::<panels::Availability>().unwrap().request;
            if current != request && !checked {
                {
                    let mut panel = props[1].panel.borrow_mut();
                    let panel = panel.as_any().downcast_mut::<panels::Availability>().unwrap();
                    let mut search = panel.search.clone();
                    search.minutes = "60".into();
                    panel.edit_search(search);
                }
                use kernel::app::Worker;
                kernel::runtime::block_on(super::super::sync::Sync.pass(s.world()));
                checked = true;
            }
            redraw(&mut cx, s, size);
            let status = widgets[1].borrow::<CalendarAvailabilityPanel>().unwrap().controls.label(&cx, ids!(status_lbl)).text();
            if checked && status.contains("availability checked") { break; }
            assert!(std::time::Instant::now() < deadline, "the checked result must reach the widget: {status}");
            woke.recv_timeout(std::time::Duration::from_secs(5)).expect(&status);
        }
        let tracks = widgets[1].borrow::<CalendarAvailabilityPanel>().unwrap().tracks.clone();
        assert_eq!(tracks.len(), 1, "rechecks use the draft's current guests");
        assert!(tracks.iter().all(|(track, _)| track.borrow::<CalendarTimeTrack>().unwrap().track.interactive));
    } else if matches!(case, Some(DrawCase::Hover)) {
        let tracks = widgets[1].borrow::<CalendarAvailabilityPanel>().unwrap().tracks.clone();
        let (track, _) = tracks.first().expect("own calendar track is drawn");
        let old_query = track.borrow::<CalendarTimeTrack>().unwrap().track.query.clone().unwrap();
        let (request, events) = {
            let mut panel = props[1].panel.borrow_mut();
            let panel = panel.as_any().downcast_mut::<panels::Availability>().unwrap();
            (panel.request, panel.preview(s.now()).unwrap().result.people.iter()
                .flat_map(|person| &person.details.events).cloned().collect::<Vec<_>>())
        };
        let (notify, woke) = std::sync::mpsc::channel();
        s.store().attach_ui(move || { let _ = notify.send(()); });
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        // First finish the participant refresh caused by establishing the
        // native reader's baseline. Hover then runs with native async policy.
        loop {
            s.store().poll_external();
            redraw(&mut cx, s, size);
            let query = track.borrow::<CalendarTimeTrack>().unwrap().track.query.clone().unwrap();
            if !Arc::ptr_eq(&query, &old_query) { break; }
            assert!(std::time::Instant::now() < deadline, "native participant snapshot completed");
            woke.recv_timeout(std::time::Duration::from_secs(5)).unwrap();
        }
        let (rect, query) = {
            let track = track.borrow::<CalendarTimeTrack>().unwrap();
            (form::drawn_rect(&cx, track.area()).unwrap(), track.track.query.clone().unwrap())
        };
        let first = events.iter().find(|event| event.title == "Planning").unwrap();
        let second = events.iter().find(|event| event.title == "Afternoon check-in").unwrap();
        let (start, end) = query.validate().unwrap();
        let point = |at| rect.pos + dvec2((at - start) / (end - start) * rect.size.x, rect.size.y / 2.0);
        let motion = |cx: &mut Cx, s: &mut Session, at: DVec2, time| {
            s.store().trace_begin(9003);
            send(cx, s, Event::MouseMove(MouseMoveEvent {
                abs: at, lock_delta: Default::default(), window_id,
                modifiers: Default::default(), time, handled: Cell::default(),
            }));
            s.store().trace_end();
            assert!(s.store().trace_of(9003).is_empty(), "hover input must not query the store");
            // Event routing above owns the widgets. The no-op Cx handler lets
            // the public platform dispatcher finish its normal hover cycle,
            // so subsequent moves produce HoverOver and finally HoverOut.
            use makepad_widgets::makepad_platform::studio::{RemoteMouseMove, StudioToApp};
            cx.dispatch_studio_msg(StudioToApp::MouseMove(RemoteMouseMove {
                x: at.x, y: at.y, time, ..Default::default()
            }), window_id, DVec2::default());
        };
        let painted = |cx: &Cx| {
            let panel = widgets[1].borrow::<CalendarAvailabilityPanel>().unwrap();
            let area = panel.tooltip.area();
            (area.redraw_id() == Some(cx.redraw_id) && form::drawn_rect(cx, area).is_some())
                .then(|| panel.tooltip.label(cx, ids!(body_lbl)).text())
        };
        let pos = point(first.start + (first.end - first.start) / 2.0);
        motion(&mut cx, s, pos, 0.0);
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        let mut expected = loop {
            redraw(&mut cx, s, size);
            if let Some(text) = painted(&cx) { break text; }
            assert!(std::time::Instant::now() < deadline, "first hover details completed");
            woke.recv_timeout(std::time::Duration::from_secs(5)).unwrap();
            s.store().poll_external();
        };
        assert!(expected.contains("Planning") && !expected.contains("Afternoon check-in"));
        s.take_dirty();
        for i in 1..120 {
            let at = first.start + (first.end - first.start) * f64::from(i) / 120.0;
            motion(&mut cx, s, point(at), f64::from(i) / 60.0);
            redraw(&mut cx, s, size);
            assert_eq!(painted(&cx).as_deref(), Some(expected.as_str()),
                "moving inside one event must paint the same tooltip on every frame (move {i})");
        }
        assert!(!s.take_dirty().any(), "hover stays local to the scheduling widget");
        // Event enrichment arriving under a stationary pointer replaces the
        // details in place; the old content remains painted during preparation.
        s.store().write(move |tx| {
            let raw: String = tx.query_row("SELECT response FROM calendar_availability WHERE id=?1", [request], |row| row.get(0))?;
            let mut result: availability::ResultSet = serde_json::from_str(&raw).unwrap();
            for person in &mut result.people {
                for event in &mut person.details.events {
                    if event.title == "Planning" { event.title = "Planning updated".into(); }
                }
            }
            tx.execute("UPDATE calendar_availability SET response=?1 WHERE id=?2",
                rusqlite::params![serde_json::to_string(&result).unwrap(), request])?;
            Ok(())
        }).unwrap();
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        loop {
            redraw(&mut cx, s, size);
            let text = painted(&cx).expect("refresh under the pointer must keep the tooltip painted");
            if text.contains("Planning updated") { expected = text; break; }
            assert_eq!(text, expected, "only the old or the refreshed event may appear");
            assert!(std::time::Instant::now() < deadline, "updated hover details completed");
            woke.recv_timeout(std::time::Duration::from_secs(5)).unwrap();
            s.store().poll_external();
        }
        motion(&mut cx, s, point(second.start + 60.0), 3.0);
        redraw(&mut cx, s, size);
        let text = painted(&cx).expect("entering a different event paints its ready details immediately");
        assert!(text.contains("Afternoon check-in") && !text.contains("Planning"));
        motion(&mut cx, s, point(second.end + 60.0), 4.0);
        redraw(&mut cx, s, size);
        assert!(painted(&cx).is_none(), "free time has no event tooltip");
        motion(&mut cx, s, pos, 5.0);
        redraw(&mut cx, s, size);
        assert_eq!(painted(&cx).as_deref(), Some(expected.as_str()));
        motion(&mut cx, s, rect.pos - dvec2(2.0, 2.0), 6.0);
        redraw(&mut cx, s, size);
        assert!(widgets[1].borrow::<CalendarAvailabilityPanel>().unwrap().hover.is_none(), "mouse-out clears hover");
        assert!(painted(&cx).is_none(), "mouse-out does not leave the old tooltip painted");
        assert!(!cx.fingers.any_areas_captured());
    } else if matches!(case, Some(DrawCase::Refresh)) {
        let tracks = widgets[1].borrow::<CalendarAvailabilityPanel>().unwrap().tracks.clone();
        assert_eq!(tracks.len(), 2, "warm both participant tracks before attaching async I/O");
        let identities: Vec<_> = tracks.iter().map(|(track, _)| track.widget_uid()).collect();
        let (track, rect) = &tracks[0];
        let query = track.borrow::<CalendarTimeTrack>().unwrap().track.query.clone().unwrap();
        let proposed = track.borrow::<CalendarTimeTrack>().unwrap().track.proposed.unwrap();
        let old_preview = props[1].panel.borrow_mut().as_any().downcast_mut::<panels::Availability>()
            .unwrap().preview(s.now()).unwrap();
        let mut pos = rect.pos + dvec2((proposed.0 + proposed.1) / 2.0 * rect.size.x, rect.size.y / 2.0);
        cx.fingers.mouse_down(button, window_id);
        send(&mut cx, s, Event::MouseDown(MouseDownEvent {
            abs: pos, button, window_id, modifiers: Default::default(), handled: Cell::default(), time: 0.0,
        }));
        assert!(track.borrow::<CalendarTimeTrack>().unwrap().drag.is_some());
        assert!(cx.fingers.is_area_captured(track.area()));
        let mut selected = props[1].panel.borrow_mut().as_any().downcast_mut::<panels::Availability>().unwrap().selected;
        assert!(selected.is_some());

        let (notify, woke) = std::sync::mpsc::channel();
        s.store().attach_ui(move || { let _ = notify.send(()); });
        // Attaching a native reader establishes its foreign-commit baseline
        // once. Consume that invalidation before measuring later writes.
        while s.store().query_revision() == 0 {
            woke.recv_timeout(std::time::Duration::from_secs(5)).unwrap();
            s.store().poll_external();
        }
        let before = s.store().revision(&["calendar_source"]);
        s.store().write(|tx| tx.execute("UPDATE calendar_source SET checked=COALESCE(checked,0)+1", [])).unwrap();
        assert_ne!(s.store().revision(&["calendar_source"]), before);
        let edits = s.store().revision(&["calendar_draft", "calendar_change"]);
        let intact = |cx: &Cx, s: &Session, selected| {
            let current = widgets[1].borrow::<CalendarAvailabilityPanel>().unwrap().tracks.clone();
            assert_eq!(current.iter().map(|(track, _)| track.widget_uid()).collect::<Vec<_>>(), identities,
                "a data-only refresh must keep the visible track widgets");
            for ((track, _), (_, rect)) in current.iter().zip(&tracks) {
                assert!(track.visible());
                assert_eq!(form::drawn_rect(cx, track.area()), Some(*rect), "refresh must not move tracks");
                assert!(track.area().is_valid(cx));
            }
            assert!(cx.fingers.is_area_captured(track.area()), "the current track area retains pointer capture");
            assert!(track.borrow::<CalendarTimeTrack>().unwrap().drag.is_some());
            let mut panel = props[1].panel.borrow_mut();
            let panel = panel.as_any().downcast_mut::<panels::Availability>().unwrap();
            assert_eq!(panel.selected, selected, "background refresh must not reset the current proposal");
            assert_eq!(panel.search, availability::Search::from_query(&query));
            assert_eq!(s.store().revision(&["calendar_draft", "calendar_change"]), edits);
        };
        // The first async get returns pending even if its task finishes quickly.
        // This is the frame that used to erase the tracks and their capture.
        redraw(&mut cx, s, size);
        intact(&cx, s, selected);
        pos.x += rect.size.x / 16.0;
        let expected = availability::snap(&query, track.borrow::<CalendarTimeTrack>().unwrap()
            .drag.as_ref().unwrap().1.at(pos.x), s.now()).unwrap();
        assert_ne!(Some(expected), selected);
        send(&mut cx, s, Event::MouseMove(MouseMoveEvent {
            abs: pos, lock_delta: dvec2(0.0, 0.0), window_id, modifiers: Default::default(),
            time: 0.5, handled: Cell::default(),
        }));
        selected = Some(expected);
        redraw(&mut cx, s, size);
        intact(&cx, s, selected);
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        loop {
            assert!(std::time::Instant::now() < deadline, "background tracks completed");
            woke.recv_timeout(std::time::Duration::from_secs(5)).unwrap();
            s.store().poll_external();
            redraw(&mut cx, s, size);
            intact(&cx, s, selected);
            let preview = props[1].panel.borrow_mut().as_any().downcast_mut::<panels::Availability>()
                .unwrap().preview(s.now()).unwrap();
            let current_query = track.borrow::<CalendarTimeTrack>().unwrap().track.query.clone().unwrap();
            if !Arc::ptr_eq(&preview, &old_preview) && !Arc::ptr_eq(&current_query, &query) { break; }
        }
        // Continue the same gesture after publication, without another press.
        pos.x += rect.size.x / 8.0;
        let expected = availability::snap(&query, track.borrow::<CalendarTimeTrack>().unwrap()
            .drag.as_ref().unwrap().1.at(pos.x), s.now()).unwrap();
        assert_ne!(Some(expected), selected);
        send(&mut cx, s, Event::MouseMove(MouseMoveEvent {
            abs: pos, lock_delta: dvec2(0.0, 0.0), window_id, modifiers: Default::default(),
            time: 1.0, handled: Cell::default(),
        }));
        redraw(&mut cx, s, size);
        intact(&cx, s, Some(expected));
        send(&mut cx, s, Event::MouseUp(MouseUpEvent {
            abs: pos, button, window_id, modifiers: Default::default(), time: 2.0,
        }));
        cx.fingers.mouse_up(button);
        assert!(track.borrow::<CalendarTimeTrack>().unwrap().drag.is_none());
        assert!(!cx.fingers.any_areas_captured());
    } else {
    for (minutes, width) in [("30", 1200.0), ("60", 960.0), ("90", 1200.0)] {
        let size = dvec2(width, size.y);
        {
            let mut panel = props[1].panel.borrow_mut();
            let p = panel
                .as_any()
                .downcast_mut::<panels::Availability>()
                .unwrap();
            let mut search = p.search.clone();
            search.minutes = minutes.into();
            p.edit_search(search);
        }
        redraw(&mut cx, s, size);
        let tracks = widgets[1]
            .borrow::<CalendarAvailabilityPanel>()
            .unwrap()
            .tracks
            .clone();
        assert_eq!(tracks.len(), 2, "both participants must be drawn");
        let (track, _) = &tracks[0];
        let rect = form::drawn_rect(&cx, track.area()).unwrap();
        let (q, proposal) = {
            let track = track.borrow::<CalendarTimeTrack>().unwrap();
            (
                track.track.query.clone().unwrap(),
                track.track.proposed.unwrap(),
            )
        };
        assert_eq!(q.minutes.to_string(), minutes);
        let mut pos = rect.pos
            + dvec2(
                (proposal.0 + proposal.1) / 2.0 * rect.size.x,
                rect.size.y / 2.0,
            );
        assert!(track.area().clipped_rect(&cx).contains(pos));
        cx.fingers.mouse_down(button, window_id);
        send(
            &mut cx,
            s,
            Event::MouseDown(MouseDownEvent {
                abs: pos,
                button,
                window_id,
                modifiers: Default::default(),
                handled: Cell::default(),
                time: 0.0,
            }),
        );
        assert!(track.borrow::<CalendarTimeTrack>().unwrap().drag.is_some());
        let revision = s.store().revision(&["calendar_draft", "calendar_change"]);
        s.take_dirty();
        // Retain the actual captured area through repeated mark/sweep cycles,
        // crossing busy periods and changing the warning/selection text.
        for i in 0..240 {
            let step = if i % 120 < 60 { i % 60 } else { 60 - i % 60 };
            pos.x = rect.pos.x + f64::from(step) / 60.0 * rect.size.x;
            send(
                &mut cx,
                s,
                Event::MouseMove(MouseMoveEvent {
                    abs: pos,
                    lock_delta: dvec2(0.0, 0.0),
                    window_id,
                    modifiers: Default::default(),
                    time: f64::from(i) / 60.0,
                    handled: Cell::default(),
                }),
            );
            redraw(&mut cx, s, size);
            let mut panel = props[1].panel.borrow_mut();
            let selected = panel
                .as_any()
                .downcast_mut::<panels::Availability>()
                .unwrap()
                .selected
                .unwrap();
            assert_eq!(availability::snap(&q, selected, s.now()), Some(selected));
            assert!(track.borrow::<CalendarTimeTrack>().unwrap().drag.is_some());
            assert!(track.area().is_valid(&cx), "capture must survive redraw");
            for (track, rect) in &tracks {
                assert_eq!(
                    form::drawn_rect(&cx, track.area()),
                    Some(*rect),
                    "overlap warnings must not move any track"
                );
            }
        }
        send(
            &mut cx,
            s,
            Event::MouseUp(MouseUpEvent {
                abs: pos + dvec2(1500.0, 900.0),
                button,
                window_id,
                modifiers: Default::default(),
                time: 5.0,
            }),
        );
        cx.fingers.mouse_up(button);
        assert!(track.borrow::<CalendarTimeTrack>().unwrap().drag.is_none());
        assert!(!cx.fingers.any_areas_captured());
        assert!(
            !s.take_dirty().any(),
            "dragging must stay local to the widget"
        );
        assert_eq!(
            s.store().revision(&["calendar_draft", "calendar_change"]),
            revision
        );
    }
    }
    timings.sort();
    eprintln!(
        "{} editor + scheduling draw-list builds: median {:?}, p99 {:?}, max {:?}",
        timings.len(),
        timings[timings.len() / 2],
        timings[timings.len() * 99 / 100],
        timings.last().unwrap()
    );
    let errors = ERRORS.lock().unwrap();
    assert!(errors.is_empty(), "{}", errors.join("\n"));
}
