//! Real Makepad focus commits between events, including draws that discover
//! a newly focused panel before any pointer or keyboard input arrives.

use super::*;
use crate::shell::hosted::PanelProps;
use crate::shell::widgets::source_input::SourceInputWidgetRefExt;
use makepad_widgets::*;
use std::cell::{Cell, RefCell};
use std::rc::Rc;

#[derive(Clone, Copy, PartialEq)]
enum Change {
    Panel,
    Window,
    Chrome,
    Overlay,
    Pending,
}

#[test]
fn panel_focus_restores_editing_without_a_click() {
    check_focus(Change::Panel);
}

#[test]
fn window_activation_restores_the_focused_notes_editor() {
    check_focus(Change::Window);
}

#[test]
fn focus_on_note_chrome_returns_to_the_text() {
    check_focus(Change::Chrome);
}

#[test]
fn notes_yield_to_overlays_and_restore_the_selection_afterwards() {
    check_focus(Change::Overlay);
}

#[test]
fn pending_editor_focus_respects_a_new_keyboard_owner() {
    check_focus(Change::Pending);
}

fn command(key_code: KeyCode) -> Event {
    Event::KeyDown(KeyEvent {
        key_code,
        modifiers: KeyModifiers {
            logo: cfg!(target_vendor = "apple"),
            control: cfg!(not(target_vendor = "apple")),
            ..Default::default()
        },
        ..Default::default()
    })
}

fn check_focus(change: Change) {
    let original = "A thought to return to";
    let mut session = Session::fake(APPS);
    let id = model::create_with_body(&mut session, original.into()).unwrap();
    let other_slot = open(&mut session, NoteList::id());
    let slot = open(&mut session, Editor::note(id));
    let mut props = PanelProps {
        slot,
        panel: session.panel(slot).unwrap(),
        hits: Default::default(),
        keyboard: Default::default(),
        has_keyboard: true,
        grab: Default::default(),
    };
    let done = Rc::new(Cell::new(false));
    let seen = done.clone();
    let mut root = WidgetRef::empty();
    let mut pass = None;
    let mut list: Option<DrawList> = None;
    let mut frame = 0;
    let cx = Rc::new(RefCell::new(Cx::new(Box::new(move |cx, event| {
        match event {
            Event::Startup => {
                root = cx.with_vm(|vm| {
                    makepad_widgets::script_mod(vm);
                    crate::shell::script_mod(vm);
                    super::super::ui::script_mod(vm);
                    let value = script_eval!(vm, { use mod.prelude.widgets.*
                        View {
                            width: Fill, height: Fill, flow: Down
                            other := mod.widgets.SField { height: 40 }
                            editor := mod.widgets.NotesEditorPanel {}
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
                    root.draw_all(&mut cx, &mut Scope::with_data_props(&mut session, &props));
                    cx.end_pass_sized_turtle();
                    list.end(&mut draw);
                    draw.end_pass(pass);
                }
                frame += 1;
                if frame == 1 && change == Change::Pending {
                    // The editor has scheduled focus, but an overlay takes
                    // ownership before that request can be committed.
                    props.has_keyboard = false;
                    root.text_input(cx, ids!(other)).set_key_focus(cx);
                }
                cx.new_next_frame();
            }
            Event::NextFrame(_) if frame > 0 => {
                root.handle_event(cx, event, &mut Scope::with_data_props(&mut session, &props));
                let input = root.widget(cx, ids!(editor.body_input)).as_source_input();
                let other = root.text_input(cx, ids!(other));
                match frame {
                    2 => {
                        if change == Change::Pending {
                            assert_eq!(cx.key_focus(), other.area());
                        } else {
                            assert!(input.key_focus(cx), "opening a note must enable typing");
                            assert_eq!(cx.get_ime_area_rect(), input.area().rect(cx));
                            root.handle_event(
                                cx,
                                &command(KeyCode::KeyA),
                                &mut Scope::with_data_props(&mut session, &props),
                            );
                            assert_eq!(input.selected_text(), original);
                        }
                        match change {
                            Change::Panel => {
                                session.nav(Nav::Focus(other_slot));
                                props.has_keyboard = false;
                            }
                            Change::Window => root.handle_event(
                                cx,
                                &Event::WindowLostFocus(WindowId(0, 0)),
                                &mut Scope::with_data_props(&mut session, &props),
                            ),
                            Change::Overlay => props.has_keyboard = false,
                            Change::Chrome | Change::Pending => {}
                        }
                        other.set_key_focus(cx);
                    }
                    3 => {
                        assert_eq!(input.text(), original);
                        if change != Change::Chrome {
                            assert_eq!(
                                cx.key_focus(),
                                other.area(),
                                "inactive notes must yield focus"
                            );
                        }
                        if change == Change::Window {
                            root.handle_event(
                                cx,
                                &Event::WindowGotFocus(WindowId(0, 0)),
                                &mut Scope::with_data_props(&mut session, &props),
                            );
                        } else {
                            session.nav(Nav::Focus(slot));
                            props.has_keyboard = true;
                            // No event is forwarded after navigation: drawing
                            // must arrange restoration before the next key.
                        }
                    }
                    5 => {
                        assert!(
                            input.key_focus(cx),
                            "returning to notes must restore typing"
                        );
                        assert_eq!(cx.get_ime_area_rect(), input.area().rect(cx));
                        if change == Change::Pending {
                            root.handle_event(
                                cx,
                                &command(KeyCode::KeyA),
                                &mut Scope::with_data_props(&mut session, &props),
                            );
                        }
                        assert_eq!(
                            input.selected_text(),
                            original,
                            "focus must retain selection"
                        );
                        root.handle_event(
                            cx,
                            &Event::TextInput(TextInputEvent {
                                input: "Resumed typing".into(),
                                ..Default::default()
                            }),
                            &mut Scope::with_data_props(&mut session, &props),
                        );
                        assert_eq!(input.text(), "Resumed typing");
                        root.handle_event(
                            cx,
                            &command(KeyCode::KeyZ),
                            &mut Scope::with_data_props(&mut session, &props),
                        );
                        assert_eq!(input.text(), original, "restored editing keeps native undo");
                        seen.set(true);
                    }
                    _ => {}
                }
                cx.redraw_all();
            }
            _ => root.handle_event(cx, event, &mut Scope::with_data_props(&mut session, &props)),
        }
    }))));
    Cx::headless_event_loop_for_draw_cycles(cx, 6);
    assert!(done.get());
}
