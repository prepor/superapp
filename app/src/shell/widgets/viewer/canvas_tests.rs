//! Drive the actual widget after layout, including raw mobile touch updates.

use super::*;
use std::{cell::{Cell, RefCell}, rc::Rc};
use makepad_widgets::makepad_platform::event::{ScrollEvent, ScrollPhase, TouchPoint};
use kernel::panel::{Panel, PanelId, Tag};

struct TestPanel(PanelId);
impl Panel for TestPanel {
    fn id(&self) -> &PanelId { &self.0 }
    fn title(&self) -> String { "viewer".into() }
    fn as_any(&mut self) -> &mut dyn std::any::Any { self }
}

fn scroll(point: DVec2, delta: DVec2, zoom: bool) -> Event {
    Event::Scroll(ScrollEvent {
        window_id: CxWindowPool::id_zero(), abs: point, scroll: delta,
        modifiers: KeyModifiers { logo: zoom, ..Default::default() },
        handled_x: Cell::new(false), handled_y: Cell::new(false),
        is_mouse: false, time: 0.0, phase: ScrollPhase::Changed,
    })
}

fn mouse(point: DVec2, down: bool) -> Event {
    if down { Event::MouseDown(MouseDownEvent {
        window_id: CxWindowPool::id_zero(), abs: point, button: MouseButton::PRIMARY,
        modifiers: Default::default(), handled: Cell::new(Area::Empty), time: 0.0,
    }) } else { Event::MouseUp(MouseUpEvent {
        window_id: CxWindowPool::id_zero(), abs: point, button: MouseButton::PRIMARY,
        modifiers: Default::default(), time: 0.0,
    }) }
}

fn touch(points: &[(u64, DVec2, TouchState)]) -> Event {
    Event::TouchUpdate(TouchUpdateEvent {
        time: 0.0, window_id: CxWindowPool::id_zero(), modifiers: Default::default(),
        touches: points.iter().map(|&(uid, abs, state)| TouchPoint {
            uid, abs, state, time: 0.0, rotation_angle: 0.0, force: 1.0, radius: DVec2::default(),
            handled: Cell::new(Area::Empty), sweep_lock: Cell::new(Area::Empty),
        }).collect(),
    })
}

#[test]
fn gestures_and_links_use_drawn_coordinates() {
    let mut props = PanelProps {
        slot: 1, panel: Rc::new(RefCell::new(Box::new(TestPanel(PanelId::new(Tag("viewer-test"), Vec::<String>::new()))))),
        hits: Default::default(), keyboard: Default::default(), grab: Default::default(),
    };
    let finished = Rc::new(Cell::new(false));
    let seen = finished.clone();
    let mut root = WidgetRef::empty();
    let mut pass = None;
    let mut draw_list: Option<DrawList> = None;
    let mut frame = 0;
    let cx = Rc::new(RefCell::new(Cx::new(Box::new(move |cx, event| match event {
        Event::Startup => {
            root = cx.with_vm(|vm| {
                makepad_widgets::script_mod(vm);
                crate::shell::script_mod(vm);
                let value = script_eval!(vm, { mod.widgets.ViewerImage {} });
                WidgetRef::script_from_value(vm, value)
            });
            makepad_widgets::widget_tree::set_ui_root(cx, &root);
            let texture = Texture::new_with_format(cx, TextureFormat::VecBGRAu8_32 {
                width: 1, height: 1, data: Some(vec![0xffffffff]), updated: TextureUpdated::Full,
            });
            let image = root.as_viewer_image();
            image.set(cx, Some(texture), dvec2(1200.0, 800.0), true,
                vec![Link { rect: [0.4, 0.4, 0.6, 0.6], target: Target::Page(1) }]);
            image.enable(true);
            let p = DrawPass::new(cx);
            p.set_size(cx, dvec2(600.0, 400.0));
            pass = Some(p);
            draw_list = Some(DrawList::new(cx));
            cx.redraw_all();
        }
        Event::Draw(event) if frame < 9 => {
            props.hits.clear();
            props.grab = Default::default();
            {
                let mut draw = CxDraw::new(cx, event);
                let pass = pass.as_ref().unwrap();
                draw.begin_pass(pass, Some(1.0));
                let list = draw_list.as_mut().unwrap();
                list.begin_always(&mut draw);
                let mut cx = Cx2d::new(&mut draw);
                cx.begin_root_turtle(dvec2(600.0, 400.0), Layout::default());
                root.draw_all(&mut cx, &mut Scope::with_data_props(&mut (), &props));
                cx.end_pass_sized_turtle();
                list.end(&mut draw);
                draw.end_pass(pass);
            }
            let image = root.as_viewer_image();
            let hit = props.hits.by_label("go to page 2");
            let point = hit.as_ref().map(|h| h.rect.pos + h.rect.size * 0.5).unwrap_or(dvec2(300.0, 200.0));
            let mut data = ();
            let mut scope = Scope::with_data_props(&mut data, &props);
            match frame {
                0 | 1 => {
                    let event = if frame == 0 { scroll(point, dvec2(0.0, -100.0), true) }
                        else { scroll(point, dvec2(50.0, 30.0), false) };
                    root.handle_event(cx, &event, &mut scope);
                    let Event::Scroll(event) = event else { unreachable!() };
                    assert!(event.handled_x.get() && event.handled_y.get(),
                        "frame {frame}: gesture at {point:?}, active {}, valid {}, rect {:?}, clip {:?}, hits {:?}",
                        image.borrow().unwrap().active, image.borrow().unwrap().area.is_valid(cx),
                        image.borrow().unwrap().viewport(cx), image.borrow().unwrap().area.clipped_rect(cx), props.hits.labels());
                    assert_eq!(image.zoom(), 2.0);
                    if frame == 1 { assert_eq!(image.borrow().unwrap().camera.offset, dvec2(-50.0, -30.0)); }
                }
                2 | 3 => {
                    assert_eq!(hit.unwrap().cursor, MouseCursor::Hand);
                    root.handle_event(cx, &mouse(point, true), &mut scope);
                    let end = if frame == 2 { point + dvec2(60.0, 20.0) } else { point };
                    if frame == 2 {
                        root.handle_event(cx, &Event::MouseMove(MouseMoveEvent {
                            abs: end, lock_delta: DVec2::default(), window_id: CxWindowPool::id_zero(),
                            modifiers: Default::default(), time: 0.0, handled: Cell::new(Area::Empty),
                        }), &mut scope);
                    }
                    root.handle_event(cx, &mouse(end, false), &mut scope);
                    assert_eq!(image.clicked(), (frame == 3).then_some(Target::Page(1)), "dragging must not follow a link");
                    if frame == 3 { image.enable(false); }
                }
                4 => {
                    assert!(props.hits.is_empty(), "hidden content must leave no stale links or image hits");
                    root.handle_event(cx, &mouse(point, true), &mut scope);
                    root.handle_event(cx, &mouse(point, false), &mut scope);
                    assert!(image.clicked().is_none());
                    image.zoom_to(cx, 1.0);
                    image.enable(true);
                }
                5 | 6 => {
                    let span = if frame == 5 { 50.0 } else { 100.0 };
                    let state = if frame == 5 { TouchState::Start } else { TouchState::Move };
                    let center = dvec2(300.0, 200.0);
                    root.handle_event(cx, &touch(&[(1, center - dvec2(span, 0.0), state),
                        (2, center + dvec2(span, 0.0), state)]), &mut scope);
                    assert!(props.grab.touch_claimed());
                    assert_eq!(image.zoom(), if frame == 5 { 1.0 } else { 2.0 });
                }
                7 => {
                    root.handle_event(cx, &touch(&[(1, dvec2(230.0, 220.0), TouchState::Move),
                        (2, dvec2(430.0, 220.0), TouchState::Move)]), &mut scope);
                    assert_eq!(image.borrow().unwrap().camera.offset, dvec2(30.0, 20.0));
                    root.handle_event(cx, &touch(&[(1, dvec2(230.0, 220.0), TouchState::Stop),
                        (2, dvec2(430.0, 220.0), TouchState::Stop)]), &mut scope);
                    assert!(props.grab.touch_claimed(), "the lift belongs to the same gesture");
                    assert!(image.clicked().is_none(), "a pinch ending on a link must not open it");
                    image.zoom_to(cx, 1.0);
                }
                8 => {
                    assert_eq!(image.zoom(), 1.0);
                    assert_eq!(image.borrow().unwrap().camera.offset, DVec2::default());
                    assert_eq!(point, dvec2(300.0, 200.0));
                    root.handle_event(cx, &touch(&[(3, point, TouchState::Start)]), &mut scope);
                    root.handle_event(cx, &touch(&[(3, point, TouchState::Stop)]), &mut scope);
                    assert!(props.grab.touch_claimed());
                    assert_eq!(image.clicked(), Some(Target::Page(1)), "a single tap follows the link");
                    seen.set(true);
                }
                _ => unreachable!(),
            }
            frame += 1;
            cx.redraw_all();
        }
        _ => {}
    }))));
    Cx::headless_event_loop_for_draw_cycles(cx, 10);
    assert!(finished.get());
}
