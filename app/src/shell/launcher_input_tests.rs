//! Exercise Makepad's focus transitions between real input events. Bulk
//! scripted typing cannot show a focus request stealing the next keystroke.

use super::*;
use std::cell::{Cell, RefCell};
use std::rc::Rc;

#[test]
fn launcher_keeps_typing_and_undo_through_competing_focus_requests() {
    check_focus(true);
}

#[test]
fn inactive_launcher_leaves_the_keyboard_with_the_active_ui() {
    check_focus(false);
}

fn check_focus(initially_active: bool) {
    let finished = Rc::new(Cell::new(false));
    let checked = finished.clone();
    let mut root = WidgetRef::empty();
    let mut pass = None;
    let mut list: Option<DrawList> = None;
    let mut frame = 0;
    let mut props = OverlayProps {
        alpha: 1.0,
        has_keyboard: initially_active,
        ..Default::default()
    };
    let cx = Rc::new(RefCell::new(Cx::new(Box::new(move |cx, event| {
        match event {
            Event::Startup => {
                root = cx.with_vm(|vm| {
                    makepad_widgets::script_mod(vm);
                    crate::shell::script_mod(vm);
                    let value = script_eval!(vm, {
                        mod.widgets.View {
                            width: Fill, height: Fill, flow: Down
                            other := mod.widgets.SField { width: Fill, height: 40 }
                            launcher := mod.widgets.LauncherOverlay {}
                        }
                    });
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
                    root.draw_all(&mut cx, &mut Scope::with_props(&props));
                    cx.end_pass_sized_turtle();
                    list.end(&mut draw);
                    draw.end_pass(pass);
                }
                frame += 1;
                cx.new_next_frame();
            }
            Event::NextFrame(_) if frame > 0 => {
                let launcher = root.widget(cx, ids!(launcher));
                let query = launcher.text_input(cx, ids!(query_input));
                let other = root.text_input(cx, ids!(other));
                assert!(!query.area().is_empty());
                assert!(!other.area().is_empty());
                match frame {
                    1 => {
                        other.set_key_focus(cx);
                        launcher.handle_event(cx, &Event::Signal, &mut Scope::with_props(&props));
                    }
                    2 | 3 if initially_active => {
                        assert_eq!(cx.key_focus(), query.area());
                        let key = KeyEvent {
                            key_code: if frame == 2 {
                                KeyCode::KeyA
                            } else {
                                KeyCode::KeyB
                            },
                            ..Default::default()
                        };
                        launcher.handle_event(
                            cx,
                            &Event::KeyDown(key),
                            &mut Scope::with_props(&props),
                        );
                        launcher.handle_event(
                            cx,
                            &Event::TextInput(TextInputEvent {
                                input: if frame == 2 { "a" } else { "b" }.into(),
                                ..Default::default()
                            }),
                            &mut Scope::with_props(&props),
                        );
                        // A previewed panel requests its caret on key release.
                        other.set_key_focus(cx);
                        launcher.handle_event(
                            cx,
                            &Event::KeyUp(key),
                            &mut Scope::with_props(&props),
                        );
                        assert_eq!(query.text(), if frame == 2 { "a" } else { "ab" });
                        if frame == 3 {
                            let command = |key_code| {
                                Event::KeyDown(KeyEvent {
                                    key_code,
                                    modifiers: KeyModifiers {
                                        logo: cfg!(target_vendor = "apple"),
                                        control: cfg!(not(target_vendor = "apple")),
                                        ..Default::default()
                                    },
                                    ..Default::default()
                                })
                            };
                            launcher.handle_event(
                                cx,
                                &command(KeyCode::KeyA),
                                &mut Scope::with_props(&props),
                            );
                            launcher.handle_event(
                                cx,
                                &Event::TextInput(TextInputEvent {
                                    input: "replacement".into(),
                                    ..Default::default()
                                }),
                                &mut Scope::with_props(&props),
                            );
                            assert_eq!(query.text(), "replacement");
                            launcher.handle_event(
                                cx,
                                &command(KeyCode::KeyZ),
                                &mut Scope::with_props(&props),
                            );
                            assert_eq!(
                                query.text(),
                                "ab",
                                "retaining focus must preserve text undo"
                            );
                        }
                    }
                    4 => {
                        // The library covers this stage, or another mount is
                        // entered, before its next background signal arrives.
                        props.has_keyboard = false;
                        other.set_key_focus(cx);
                        launcher.handle_event(cx, &Event::Signal, &mut Scope::with_props(&props));
                    }
                    5 => {
                        assert_eq!(cx.key_focus(), other.area());
                        assert_eq!(query.text(), if initially_active { "ab" } else { "" });
                        checked.set(true);
                    }
                    _ => {}
                }
                cx.redraw_all();
            }
            _ => {
                let launcher = root.widget(cx, ids!(launcher));
                launcher.handle_event(cx, event, &mut Scope::with_props(&props));
            }
        }
    }))));
    Cx::headless_no_draw_event_loop_for_draw_cycles(cx, 6);
    assert!(finished.get());
}
