//! Native text input must be enabled before macOS sees the first keystroke.
//! Sending a scripted TextInput alone cannot exercise that prerequisite.

use super::*;
use crate::shell::hosted::PanelProps;
use kernel::nav::Nav;
use makepad_widgets::*;
use std::cell::{Cell, RefCell};
use std::rc::Rc;

#[test]
fn focused_terminal_registers_native_text_input_without_a_keystroke() {
    check_input_focus(true, false);
}

#[test]
fn inactive_terminal_does_not_activate_native_text_input() {
    check_input_focus(false, false);
}

#[test]
fn pending_terminal_focus_respects_a_new_keyboard_owner() {
    check_input_focus(true, true);
}

fn check_input_focus(initially_active: bool, loses_focus_before_next_frame: bool) {
    static APPS: &[&dyn App] = &[&TERMINAL];
    let mut session = Session::fake(APPS);
    session.nav(Nav::Open {
        from: 0,
        id: PanelId::bare(TAG),
        fresh: true,
    });
    session.settle();
    let slot = session.focus().unwrap();
    let mut props = PanelProps {
        slot,
        panel: session.panel(slot).unwrap(),
        hits: Default::default(),
        keyboard: Default::default(),
        has_keyboard: initially_active,
        grab: Default::default(),
    };
    let checked = Rc::new(Cell::new(false));
    let seen = checked.clone();
    let drawn_area = Rc::new(Cell::new(Area::Empty));
    let area_seen = drawn_area.clone();
    let mut root = WidgetRef::empty();
    let mut pass = None;
    let mut list: Option<DrawList> = None;
    let cx = Rc::new(RefCell::new(Cx::new(Box::new(move |cx, event| {
        match event {
            Event::Startup => {
                root = cx.with_vm(|vm| {
                    makepad_widgets::script_mod(vm);
                    crate::shell::script_mod(vm);
                    super::ui::script_mod(vm);
                    let value = script_eval!(vm, { mod.widgets.TerminalPanel {} });
                    WidgetRef::script_from_value(vm, value)
                });
                makepad_widgets::widget_tree::set_ui_root(cx, &root);
                let p = DrawPass::new(cx);
                p.set_size(cx, dvec2(600.0, 400.0));
                pass = Some(p);
                list = Some(DrawList::new(cx));
                cx.redraw_all();
            }
            Event::Draw(event) => {
                {
                    let mut draw = CxDraw::new(cx, event);
                    let pass = pass.as_ref().unwrap();
                    draw.begin_pass(pass, Some(1.0));
                    let list = list.as_mut().unwrap();
                    list.begin_always(&mut draw);
                    let mut cx = Cx2d::new(&mut draw);
                    cx.begin_root_turtle(dvec2(600.0, 400.0), Layout::default());
                    root.draw_all(&mut cx, &mut Scope::with_data_props(&mut session, &props));
                    cx.end_pass_sized_turtle();
                    list.end(&mut draw);
                    draw.end_pass(pass);
                }
                let area = root.widget(cx, ids!(grid)).area();
                assert!(!area.is_empty());
                assert_eq!(
                    area.rect(cx),
                    root.area().rect(cx),
                    "the grid is the whole body while nothing is being found"
                );
                area_seen.set(area);
                assert_eq!(
                    props.hits.by_label("terminal input").unwrap().rect,
                    area.rect(cx),
                    "blank terminal cells must have a usable click target"
                );
                let expected = if props.has_keyboard {
                    area.rect(cx)
                } else {
                    Rect::default()
                };
                assert_eq!(
                    cx.get_ime_area_rect(),
                    expected,
                    "native text input must follow the shell's keyboard owner before any keystroke"
                );
                if loses_focus_before_next_frame && !seen.get() {
                    // The launcher can take over between drawing the terminal
                    // and its deferred focus request being delivered.
                    props.has_keyboard = false;
                    cx.show_text_ime(Area::Empty, DVec2::default());
                }
                seen.set(true);
            }
            _ => root.handle_event(cx, event, &mut Scope::with_data_props(&mut session, &props)),
        }
    }))));
    Cx::headless_event_loop_for_draw_cycles(cx.clone(), 2);
    assert!(checked.get());
    let expected = if initially_active && !loses_focus_before_next_frame {
        drawn_area.get()
    } else {
        Area::Empty
    };
    assert_eq!(cx.borrow().key_focus(), expected);
}

/// The find bar's field takes the keyboard from the grid and gives it back
/// on Escape, with the shell seeing none of what was typed in between.
#[test]
fn the_find_field_takes_the_keyboard_and_escape_gives_it_back() {
    static APPS: &[&dyn App] = &[&TERMINAL];
    let mut session = Session::fake(APPS);
    session.nav(Nav::Open {
        from: 0,
        id: PanelId::bare(TAG),
        fresh: true,
    });
    session.settle();
    let slot = session.focus().unwrap();
    let props = PanelProps {
        slot,
        panel: session.panel(slot).unwrap(),
        hits: Default::default(),
        keyboard: Default::default(),
        has_keyboard: true,
        grab: Default::default(),
    };
    let finished = Rc::new(Cell::new(false));
    let checked = finished.clone();
    let mut root = WidgetRef::empty();
    let mut pass = None;
    let mut list: Option<DrawList> = None;
    let mut frame = 0;
    let cx = Rc::new(RefCell::new(Cx::new(Box::new(move |cx, event| {
        let prompt_line = |session: &mut Session| {
            let panel = session.panel(slot).unwrap();
            let mut panel = panel.borrow_mut();
            let terminal = panel.as_any().downcast_mut::<TerminalPanel>().unwrap();
            let frame = terminal.engine.as_mut().unwrap().frame().unwrap();
            frame
                .rows
                .iter()
                .map(|row| {
                    row.iter()
                        .map(|c| c.text.as_str())
                        .collect::<String>()
                        .trim_end()
                        .to_string()
                })
                .filter(|line| !line.is_empty())
                .last()
                .unwrap_or_default()
        };
        match event {
            Event::Startup => {
                root = cx.with_vm(|vm| {
                    makepad_widgets::script_mod(vm);
                    crate::shell::script_mod(vm);
                    super::ui::script_mod(vm);
                    let value = script_eval!(vm, { mod.widgets.TerminalPanel {} });
                    WidgetRef::script_from_value(vm, value)
                });
                makepad_widgets::widget_tree::set_ui_root(cx, &root);
                let p = DrawPass::new(cx);
                p.set_size(cx, dvec2(600.0, 400.0));
                pass = Some(p);
                list = Some(DrawList::new(cx));
                cx.redraw_all();
            }
            Event::Draw(event) => {
                props.hits.clear();
                {
                    let mut draw = CxDraw::new(cx, event);
                    let pass = pass.as_ref().unwrap();
                    draw.begin_pass(pass, Some(1.0));
                    let list = list.as_mut().unwrap();
                    list.begin_always(&mut draw);
                    let mut cx = Cx2d::new(&mut draw);
                    cx.begin_root_turtle(dvec2(600.0, 400.0), Layout::default());
                    root.draw_all(&mut cx, &mut Scope::with_data_props(&mut session, &props));
                    cx.end_pass_sized_turtle();
                    list.end(&mut draw);
                    draw.end_pass(pass);
                }
                frame += 1;
                cx.new_next_frame();
            }
            Event::NextFrame(_) if frame > 0 => {
                // The grid asks for the keyboard on the frame after a draw.
                root.handle_event(cx, event, &mut Scope::with_data_props(&mut session, &props));
                let grid = root.widget(cx, ids!(grid));
                let field = root.text_input(cx, ids!(find_input));
                let send = |cx: &mut Cx, session: &mut Session, event: Event| {
                    root.handle_event(cx, &event, &mut Scope::with_data_props(session, &props));
                };
                match frame {
                    2 => {
                        assert_eq!(cx.key_focus(), grid.area());
                        assert!(props.hits.by_label("find in output").is_none());
                        props.panel.borrow_mut().run("terminal.find", &mut session);
                    }
                    3 => {
                        assert_eq!(
                            cx.key_focus(),
                            field.area(),
                            "the verb lands the caret in the field"
                        );
                        assert!(props.hits.by_label("find in output").is_some());
                        send(
                            cx,
                            &mut session,
                            Event::KeyDown(KeyEvent {
                                key_code: KeyCode::KeyD,
                                ..Default::default()
                            }),
                        );
                        send(
                            cx,
                            &mut session,
                            Event::TextInput(TextInputEvent {
                                input: "demo".into(),
                                ..Default::default()
                            }),
                        );
                    }
                    4 => {
                        assert_eq!(field.text(), "demo");
                        assert_eq!(
                            prompt_line(&mut session),
                            "$",
                            "the shell saw nothing of the query"
                        );
                        assert!(
                            props.hits.by_label("1 of 1").is_some(),
                            "the count is on the bar"
                        );
                        send(
                            cx,
                            &mut session,
                            Event::KeyDown(KeyEvent {
                                key_code: KeyCode::ReturnKey,
                                ..Default::default()
                            }),
                        );
                    }
                    5 => {
                        assert_eq!(
                            cx.key_focus(),
                            field.area(),
                            "walking the matches keeps the caret"
                        );
                        assert_eq!(prompt_line(&mut session), "$");
                        send(
                            cx,
                            &mut session,
                            Event::KeyDown(KeyEvent {
                                key_code: KeyCode::Escape,
                                ..Default::default()
                            }),
                        );
                    }
                    6 => {
                        assert_eq!(
                            cx.key_focus(),
                            grid.area(),
                            "escape hands the keyboard back to the grid"
                        );
                        assert!(props.hits.by_label("find in output").is_none());
                        assert_eq!(grid.area().rect(cx), root.area().rect(cx));
                        send(
                            cx,
                            &mut session,
                            Event::KeyDown(KeyEvent {
                                key_code: KeyCode::KeyX,
                                ..Default::default()
                            }),
                        );
                        send(
                            cx,
                            &mut session,
                            Event::TextInput(TextInputEvent {
                                input: "x".into(),
                                ..Default::default()
                            }),
                        );
                    }
                    7 => {
                        assert_eq!(
                            prompt_line(&mut session),
                            "$ x",
                            "typing reaches the shell again"
                        );
                        checked.set(true);
                    }
                    _ => {}
                }
                cx.redraw_all();
            }
            _ => root.handle_event(cx, event, &mut Scope::with_data_props(&mut session, &props)),
        }
    }))));
    Cx::headless_no_draw_event_loop_for_draw_cycles(cx, 8);
    assert!(finished.get());
}
