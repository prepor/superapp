//! Raw Android-shaped events through the stage, after real widget layout.

use super::*;
use crate::shell::anim::Anim;
use kernel::panel::{Opening, Panel, PanelId, PanelKind, Tag};
use makepad_widgets::makepad_platform::event::{LongPressEvent, TouchPoint};
use std::{
    cell::{Cell, RefCell},
    rc::Rc,
};

fn panel_id(name: &str) -> PanelId {
    PanelId::new(Tag("touch-test"), vec![name.to_string()])
}

struct TestPanel(PanelId);
impl Panel for TestPanel {
    fn id(&self) -> &PanelId {
        &self.0
    }
    fn title(&self) -> String {
        "touch fixture".into()
    }
    fn as_any(&mut self) -> &mut dyn std::any::Any {
        self
    }
}

struct TestKind;
impl PanelKind for TestKind {
    fn tag(&self) -> Tag {
        Tag("touch-test")
    }
    fn open(&self, id: &PanelId, _: &mut Opening<'_>) -> Box<dyn Panel> {
        Box::new(TestPanel(id.clone()))
    }
}

struct TestApp;
impl kernel::app::App for TestApp {
    fn id(&self) -> &'static str {
        "touch-test"
    }
    fn kinds(&self) -> &'static [&'static dyn PanelKind] {
        &[&TestKind]
    }
    fn as_any(&self) -> &dyn std::any::Any {
        self
    }
}

fn send(cx: &mut Cx, stage: &mut Stage, sh: &mut Shell, state: TouchState, p: DVec2, time: f64) {
    if state == TouchState::Start {
        cx.fingers.process_tap_count(p, time);
    }
    stage.handle_with(
        cx,
        sh,
        &Event::TouchUpdate(TouchUpdateEvent {
            window_id: CxWindowPool::id_zero(),
            time,
            modifiers: Default::default(),
            touches: vec![TouchPoint {
                uid: 1,
                state,
                abs: p,
                time,
                force: 1.0,
                radius: DVec2::default(),
                rotation_angle: 0.0,
                handled: Cell::new(Area::Empty),
                sweep_lock: Cell::new(Area::Empty),
            }],
        }),
    );
    // The platform releases the touch capture after dispatching Stop. Its
    // bookkeeping is private; these tests have one finger and no mouse.
    if state == TouchState::Stop {
        cx.fingers = Default::default();
    }
}

fn flick(cx: &mut Cx, stage: &mut Stage, sh: &mut Shell, release: f64) {
    send(cx, stage, sh, TouchState::Start, dvec2(40.0, 260.0), 1.0);
    for i in 1..=4 {
        send(
            cx,
            stage,
            sh,
            TouchState::Move,
            dvec2(40.0, 260.0 - f64::from(i) * 30.0),
            1.0 + f64::from(i) * 0.02,
        );
    }
    send(cx, stage, sh, TouchState::Stop, dvec2(40.0, 140.0), release);
}

fn text_y(cx: &Cx, root: &WidgetRef, right: bool) -> f64 {
    root.widget(
        cx,
        if right {
            ids!(right.text)
        } else {
            ids!(left.text)
        },
    )
    .area()
    .rect(cx)
    .pos
    .y
}

#[test]
fn a_raw_swipe_scrolls_once_without_selecting_then_coasts_and_can_be_caught() {
    let mut released = 0.0;
    let mut caught = 0.0;
    run(move |cx, stage, sh, root, frame| {
        match frame {
            0 => flick(cx, stage, sh, 1.09),
            1 => {
                released = text_y(cx, root, false);
                assert!(
                    (released + 120.0).abs() < 1.0,
                    "drag must track 1:1: {released}"
                );
                assert!(stage.touch.fling.is_some(), "release must start momentum");
            }
            10 => {
                caught = text_y(cx, root, false);
                assert!(
                    caught < released - 60.0,
                    "content must keep moving after release"
                );
                send(cx, stage, sh, TouchState::Start, dvec2(40.0, 140.0), 1.3);
                send(cx, stage, sh, TouchState::Stop, dvec2(40.0, 140.0), 1.35);
                assert!(stage.touch.fling.is_none());
                assert!(
                    cx.key_focus().is_empty(),
                    "catching a scroll must not focus text"
                );
            }
            20 => {
                assert!(
                    (text_y(cx, root, false) - caught).abs() < 0.1,
                    "caught scroll moved"
                );
                return true;
            }
            _ => {}
        }
        for id in [ids!(left.text), ids!(right.text)] {
            assert!(
                root.widget(cx, id)
                    .borrow::<Html>()
                    .unwrap()
                    .selected_text()
                    .is_empty(),
                "a scroll must never select words"
            );
        }
        false
    });
}

#[test]
fn holding_before_release_stops_without_momentum() {
    run(|cx, stage, sh, root, frame| {
        if frame == 0 {
            flick(cx, stage, sh, 1.5);
        }
        if frame == 10 {
            assert!(stage.touch.fling.is_none());
            assert!((text_y(cx, root, false) + 120.0).abs() < 1.0);
            return true;
        }
        false
    });
}

#[test]
fn leaving_the_panel_keeps_the_scroll_with_its_original_content() {
    run(|cx, stage, sh, root, frame| {
        if frame == 0 {
            send(cx, stage, sh, TouchState::Start, dvec2(40.0, 260.0), 1.0);
            send(cx, stage, sh, TouchState::Move, dvec2(40.0, 230.0), 1.02);
            send(cx, stage, sh, TouchState::Move, dvec2(400.0, 200.0), 1.04);
            send(cx, stage, sh, TouchState::Stop, dvec2(400.0, 180.0), 1.06);
        }
        if frame == 1 {
            assert!(
                (text_y(cx, root, false) + 80.0).abs() < 1.0,
                "include movement on release"
            );
        }
        if frame == 10 {
            assert!(text_y(cx, root, false) < -80.0);
            assert!(
                text_y(cx, root, true).abs() < 0.1,
                "neighbor must not scroll"
            );
            return true;
        }
        false
    });
}

#[test]
fn taps_and_long_press_selection_still_reach_text() {
    run_fixture(false, true, |cx, stage, sh, root, frame| {
        let text = root.text_input(cx, ids!(left.text));
        let p = dvec2(15.0, 8.0);
        match frame {
            0 => {
                send(cx, stage, sh, TouchState::Start, p, 1.0);
                assert!(
                    cx.key_focus().is_empty(),
                    "undecided touch must not focus content"
                );
                send(cx, stage, sh, TouchState::Stop, p, 1.1);
                assert!(
                    text.borrow().unwrap().selected_text().is_empty(),
                    "one tap became a double click"
                );
            }
            1 => {
                assert!(text.key_focus(cx));
                send(cx, stage, sh, TouchState::Start, p, 2.0);
                stage.handle_with(
                    cx,
                    sh,
                    &Event::LongPress(LongPressEvent {
                        window_id: CxWindowPool::id_zero(),
                        uid: 1,
                        abs: p,
                        time: 2.5,
                    }),
                );
                assert_eq!(text.borrow().unwrap().selected_text(), "scrollable");
                assert!(matches!(stage.touch.mode, Mode::Content { .. }));
                send(cx, stage, sh, TouchState::Move, p + dvec2(70.0, 0.0), 2.6);
                send(cx, stage, sh, TouchState::Move, p + dvec2(100.0, 0.0), 2.7);
                send(cx, stage, sh, TouchState::Stop, p + dvec2(100.0, 0.0), 2.8);
                assert!(text.borrow().unwrap().selected_text().len() > "scrollable".len());
                assert!(stage.touch.fling.is_none());
                assert!(text_y(cx, root, false).abs() < 0.1);
                return true;
            }
            _ => {}
        }
        false
    });
}

#[test]
fn selectable_lists_scroll_and_fling_without_selecting_rows() {
    let mut released = 0.0;
    run_fixture(true, false, move |cx, stage, sh, root, frame| {
        let list = root.widget(cx, ids!(left)).as_portal_list();
        let position = {
            let list = list.borrow().unwrap();
            list.first_id() as f64 * 32.0 - list.first_scroll()
        };
        match frame {
            0 => flick(cx, stage, sh, 1.09),
            1 => {
                released = position;
                assert!(
                    (released - 120.0).abs() < 1.0,
                    "list drag must track 1:1: {released}"
                );
            }
            10 => {
                assert!(position > released + 60.0);
                return true;
            }
            _ => {}
        }
        assert!(
            !list.borrow().unwrap().has_selection(),
            "scroll selected list text"
        );
        false
    });
}

fn run(check: impl FnMut(&mut Cx, &mut Stage, &mut Shell, &WidgetRef, usize) -> bool + 'static) {
    run_fixture(false, false, check);
}

fn run_fixture(
    selectable_list: bool,
    text_input: bool,
    mut check: impl FnMut(&mut Cx, &mut Stage, &mut Shell, &WidgetRef, usize) -> bool + 'static,
) {
    static APPS: &[&dyn kernel::app::App] = &[&TestApp];
    let mut session = kernel::session::Session::fake(APPS);
    session.act(Action::new("open", "touch fixtures").moving(|wm| {
        wm.open(panel_id("left"), None, false);
        wm.open(panel_id("right"), None, false);
    }));
    session.settle();
    let left = session.showing(&panel_id("left"))[0];
    let right = session.showing(&panel_id("right"))[0];
    let mut sh = Shell {
        session,
        anim: Anim::default(),
        viewport: dvec2(600.0, 320.0),
        last_frame: None,
        hover: None,
        toasts: Vec::new(),
        overlay: Overlay::None,
        overlay_last: Overlay::None,
        launcher: kernel::launcher::Search::new(),
        clock: Default::default(),
        virtual_time: false,
        grid: None,
    };
    let done = Rc::new(Cell::new(false));
    let seen = done.clone();
    let mut root = WidgetRef::empty();
    let mut stage = None;
    let mut pass = None;
    let mut draw_list: Option<DrawList> = None;
    let mut frame = 0;
    let cx = Rc::new(RefCell::new(Cx::new(Box::new(
        move |cx, event| match event {
            Event::Startup => {
                root = cx.with_vm(|vm| {
                makepad_widgets::script_mod(vm);
                crate::shell::script_mod(vm);
                stage = Some(Stage::script_new(vm));
                let value = script_eval!(vm, {
                    use mod.prelude.widgets.*
                    let text = if #(text_input) {
                        mod.widgets.SProseText {}
                    } else {
                        mod.widgets.Html { width: Fill, height: Fit, selectable: true }
                    }
                    let content = if #(selectable_list) {
                        mod.widgets.SList {
                            width: 300, height: Fill, selectable: true
                            row := mod.widgets.SText { height: 32, text: "scrollable words in a row" }
                        }
                    } else {
                        mod.widgets.ScrollYView {
                            width: 300, height: Fill
                            text := text {}
                        }
                    }
                    mod.widgets.View {
                        width: Fill, height: Fill, flow: Right
                        left := content {}
                        right := mod.widgets.ScrollYView {
                            width: 300, height: Fill
                            text := text {}
                        }
                    }
                });
                WidgetRef::script_from_value(vm, value)
            });
                makepad_widgets::widget_tree::set_ui_root(cx, &root);
                for id in [ids!(left.text), ids!(right.text)] {
                    root.widget(cx, id).set_text(
                        cx,
                        &if text_input {
                            "scrollable words across many lines\n"
                        } else {
                            "<p>scrollable words across many lines</p>"
                        }
                        .repeat(150),
                    );
                }
                let stage = stage.as_mut().unwrap();
                stage.hosted.insert(left, root.widget(cx, ids!(left)));
                stage.hosted.insert(right, root.widget(cx, ids!(right)));
                let p = DrawPass::new(cx);
                p.set_size(cx, dvec2(600.0, 320.0));
                pass = Some(p);
                draw_list = Some(DrawList::new(cx));
                cx.redraw_all();
            }
            Event::Draw(event) if !seen.get() => {
                {
                    let mut draw = CxDraw::new(cx, event);
                    let pass = pass.as_ref().unwrap();
                    draw.begin_pass(pass, Some(1.0));
                    let list = draw_list.as_mut().unwrap();
                    list.begin_always(&mut draw);
                    let mut cx = Cx2d::new(&mut draw);
                    cx.begin_root_turtle(dvec2(600.0, 320.0), Layout::default());
                    while root
                        .draw_walk(&mut cx, &mut Scope::empty(), Walk::fill())
                        .is_step()
                    {
                        let portal = root.widget(&cx, ids!(left)).as_portal_list();
                        let mut portal = portal.borrow_mut().unwrap();
                        portal.set_item_range(&mut cx, 0, 150);
                        while let Some(index) = portal.next_visible_item(&mut cx) {
                            portal
                                .item(&mut cx, index, live_id!(row))
                                .draw_all(&mut cx, &mut Scope::empty());
                        }
                    }
                    cx.end_pass_sized_turtle();
                    list.end(&mut draw);
                    draw.end_pass(pass);
                }
                let stage = stage.as_mut().unwrap();
                stage.area = root.area();
                stage.hits.clear();
                for (slot, id) in [(left, ids!(left)), (right, ids!(right))] {
                    stage.hits.add(
                        "content",
                        root.widget(cx, id).area().rect(cx),
                        MouseCursor::Text,
                        slot,
                    );
                }
                if check(cx, stage, &mut sh, &root, frame) {
                    seen.set(true);
                } else {
                    if frame > 0 {
                        stage.touch_tick(cx, &mut sh, 1.0 / 60.0);
                    }
                    frame += 1;
                    cx.redraw_all();
                }
            }
            Event::Draw(_) => {}
            _ => root.handle_event(cx, event, &mut Scope::empty()),
        },
    ))));
    Cx::headless_no_draw_event_loop_for_draw_cycles(cx, 40);
    assert!(done.get(), "touch walk did not finish");
}
