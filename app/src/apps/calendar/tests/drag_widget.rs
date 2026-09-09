//! Memory-only pointer routing: no OS initialization, rendering, window or loop.
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
