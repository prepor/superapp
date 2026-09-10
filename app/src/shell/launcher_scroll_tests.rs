//! Keyboard navigation must reveal the complete selected row, including
//! when selection wraps between the ends of a virtualized result list.

use super::*;
use makepad_widgets::makepad_platform::event::{ScrollEvent, ScrollPhase};
use std::cell::{Cell, RefCell};
use std::rc::Rc;

#[test]
fn arrows_reveal_rows_past_the_viewport_edge() {
    check_navigation((0..24).chain((0..24).rev()).map(Step::Select).collect());
}

#[test]
fn wrapping_from_last_result_reveals_the_first() {
    check_navigation(vec![Step::Select(23), Step::Select(0), Step::Select(1)]);
}

#[test]
fn wrapping_from_first_result_reveals_the_last() {
    check_navigation(vec![Step::Select(0), Step::Select(23), Step::Select(22)]);
}

#[test]
fn wheel_scrolling_stays_put_until_selection_changes() {
    check_navigation(vec![Step::Select(0), Step::Wheel, Step::Select(1)]);
}

#[test]
fn a_new_query_reveals_its_first_match_even_when_selection_is_unchanged() {
    check_navigation(vec![
        Step::Select(0),
        Step::Wheel,
        Step::Query("result", 24),
        Step::Select(23),
        Step::Query("result 0", 1),
        Step::Query("no matches", 0),
        Step::Query("", 24),
    ]);
}

#[test]
fn a_shorter_viewport_keeps_the_selected_row_visible() {
    check_navigation(vec![Step::Select(12), Step::Resize(180.0)]);
}

#[derive(Clone, Copy, Debug)]
enum Step {
    Select(usize),
    Wheel,
    Query(&'static str, usize),
    Resize(f64),
}

impl Step {
    fn apply(self, cx: &mut Cx, root: &WidgetRef, props: &mut OverlayProps, size: &mut DVec2) {
        match self {
            Self::Select(selected) => {
                for (i, row) in props.rows.iter_mut().enumerate() {
                    row.current = i == selected;
                }
            }
            Self::Wheel => {
                let viewport = root.widget(cx, ids!(list)).area().rect(cx);
                root.handle_event(
                    cx,
                    &Event::Scroll(ScrollEvent {
                        window_id: CxWindowPool::id_zero(),
                        scroll: dvec2(0.0, 400.0),
                        abs: viewport.pos + viewport.size / 2.0,
                        modifiers: Default::default(),
                        handled_x: Cell::new(false),
                        handled_y: Cell::new(false),
                        is_mouse: true,
                        time: 0.0,
                        phase: ScrollPhase::None,
                    }),
                    &mut Scope::with_props(props),
                );
            }
            Self::Query(query, count) => {
                props.query = query.into();
                props.rows = rows(count);
            }
            Self::Resize(height) => size.y = height,
        }
    }
}

fn rows(count: usize) -> Vec<OverlayRowData> {
    (0..count)
        .map(|i| OverlayRowData {
            main: format!("result {i}"),
            current: i == 0,
            ..Default::default()
        })
        .collect()
}

fn check_navigation(steps: Vec<Step>) {
    const SETTLE_FRAMES: usize = 12;
    let cycles = SETTLE_FRAMES * steps.len() + 1;
    let finished = Rc::new(Cell::new(false));
    let checked = finished.clone();
    let mut root = WidgetRef::empty();
    let mut pass = None;
    let mut list: Option<DrawList> = None;
    let mut size = dvec2(400.0, 270.0);
    let mut frames = 0;
    let mut step = 0;
    let mut wheel_position = None;
    let mut props = OverlayProps {
        rows: rows(24),
        alpha: 1.0,
        has_keyboard: true,
        ..Default::default()
    };
    let cx = Rc::new(RefCell::new(Cx::new(Box::new(move |cx, event| {
        match event {
            Event::Startup => {
                root = cx.with_vm(|vm| {
                    makepad_widgets::script_mod(vm);
                    crate::shell::script_mod(vm);
                    let value = script_eval!(vm, { mod.widgets.LauncherOverlay {} });
                    WidgetRef::script_from_value(vm, value)
                });
                makepad_widgets::widget_tree::set_ui_root(cx, &root);
                pass = Some(DrawPass::new(cx));
                list = Some(DrawList::new(cx));
                steps[0].apply(cx, &root, &mut props, &mut size);
                cx.redraw_all();
            }
            Event::Draw(event) => {
                let mut advance = false;
                {
                    let mut draw = CxDraw::new(cx, event);
                    let pass = pass.as_ref().unwrap();
                    pass.set_size(&mut draw, size);
                    draw.begin_pass(pass, Some(1.0));
                    let list = list.as_mut().unwrap();
                    list.begin_always(&mut draw);
                    let mut cx = Cx2d::new(&mut draw);
                    cx.begin_root_turtle(size, Layout::default());
                    root.draw_all(&mut cx, &mut Scope::with_props(&props));
                    cx.end_pass_sized_turtle();

                    // Measure actual row geometry after allowing scrolling
                    // to settle. A cached or partly clipped row is not enough.
                    frames += 1;
                    if step < steps.len() {
                        let portal = root.widget(&cx, ids!(list)).as_portal_list();
                        if matches!(steps[step], Step::Wheel) && frames == 2 {
                            let inner = portal.borrow().unwrap();
                            assert!(inner.first_id() > 0, "the wheel must move the list");
                            wheel_position = Some((inner.first_id(), inner.first_scroll()));
                        }
                        if frames >= SETTLE_FRAMES {
                            match steps[step] {
                                Step::Wheel => {
                                    let inner = portal.borrow().unwrap();
                                    assert_eq!(Some((inner.first_id(), inner.first_scroll())), wheel_position,
                                        "redraws must not pull a manually scrolled list back to its selection");
                                }
                                _ => {
                                    if let Some(selected) =
                                        props.rows.iter().position(|row| row.current)
                                    {
                                        assert_visible(&cx, &portal, selected);
                                    } else {
                                        assert_eq!(portal.borrow().unwrap().range_end(), 0);
                                    }
                                }
                            }
                            advance = true;
                        }
                    }
                    list.end(&mut draw);
                    draw.end_pass(pass);
                }
                if advance {
                    step += 1;
                    frames = 0;
                    if let Some(next) = steps.get(step) {
                        next.apply(cx, &root, &mut props, &mut size);
                    } else {
                        checked.set(true);
                    }
                }
                if !checked.get() {
                    cx.redraw_all();
                }
            }
            _ => root.handle_event(cx, event, &mut Scope::with_props(&props)),
        }
    }))));
    Cx::headless_no_draw_event_loop_for_draw_cycles(cx, cycles);
    assert!(finished.get(), "the navigation walk did not finish");
}

fn assert_visible(cx: &Cx, portal: &PortalListRef, selected: usize) {
    let viewport = portal.area().rect(cx);
    let inner = portal.borrow().unwrap();
    let row = inner
        .items()
        .iter()
        .find(|(i, _)| **i == selected)
        .map(|(_, item)| item.widget.area().rect(cx))
        .unwrap_or_else(|| {
            panic!(
                "result {selected} was not drawn: {}",
                portal.debug_scroll_state_line()
            )
        });
    assert!(
        row.size.y > 0.0
            && row.pos.y >= viewport.pos.y - 0.5
            && row.pos.y + row.size.y <= viewport.pos.y + viewport.size.y + 0.5,
        "result {selected} must be fully visible: row={row:?}, viewport={viewport:?}, {}",
        portal.debug_scroll_state_line()
    );
}
