//! Pointer routing and draw-list construction without OS initialization,
//! rendering, windows or an event loop.
use super::*;
use std::cell::Cell;

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
pub fn draw_panels(s: &mut Session, editor: kernel::layout::SlotId, sheet: kernel::layout::SlotId) {
    use makepad_widgets::makepad_platform::makepad_error_log::{self, LogLevel};
    use std::sync::Mutex;
    static ERRORS: Mutex<Vec<String>> = Mutex::new(Vec::new());
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
    widgets[1]
        .borrow_mut::<CalendarAvailabilityPanel>()
        .unwrap()
        .directory = vec![Suggestion::labeled(
        "Nora with a long display name that wraps the overlap warning onto another line",
        "nora@studio.example",
    )];
    let (window_id, button) = (WindowId(0, 0), MouseButton::PRIMARY);
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
