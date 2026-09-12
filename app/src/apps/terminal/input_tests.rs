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
    check_input_focus(true, false, false);
}

#[test]
fn inactive_terminal_does_not_activate_native_text_input() {
    check_input_focus(false, false, false);
}

#[test]
fn pending_terminal_focus_respects_a_new_keyboard_owner() {
    check_input_focus(true, true, false);
}

#[test]
fn embedded_terminal_hit_tracks_its_position_after_fill_layout() {
    check_input_focus(false, false, true);
}

fn check_input_focus(initially_active: bool, loses_focus_before_next_frame: bool, embedded: bool) {
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
                    let value = if embedded {
                        script_eval!(vm, { use mod.prelude.widgets.*
                            View {
                            width:Fill,height:Fill,flow:Down
                            View {width:Fill,height:Fill}
                            terminal := mod.widgets.TerminalPanel {height:140}
                        } })
                    } else {
                        script_eval!(vm, { mod.widgets.TerminalPanel {} })
                    };
                    WidgetRef::script_from_value(vm, value)
                });
                if embedded {
                    let handle =
                        create_session(session.store(), Mode::Fake, Path::new(".")).unwrap();
                    root.widget(cx, ids!(terminal))
                        .as_terminal_view()
                        .borrow_mut()
                        .unwrap()
                        .bind_session(handle);
                }
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
                    if embedded {
                        root.widget(&cx, ids!(terminal))
                            .as_terminal_view()
                            .borrow()
                            .unwrap()
                            .register_hits(&cx, &props);
                    }
                    cx.end_pass_sized_turtle();
                    list.end(&mut draw);
                    draw.end_pass(pass);
                }
                let area = if embedded {
                    let area = root.widget(cx, ids!(terminal)).area();
                    assert!(
                        area.rect(cx).pos.y > 200.0,
                        "the Fill sibling must move the terminal down"
                    );
                    area
                } else {
                    root.area()
                };
                assert!(!area.is_empty());
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
