//! Real Makepad focus commits between events, including draws that discover
//! a newly focused panel before any pointer or keyboard input arrives.

use super::*;
use crate::shell::hosted::PanelProps;
use crate::shell::widgets::source_input::SourceInputWidgetRefExt;
use makepad_widgets::makepad_platform::event::{TouchPoint, TouchState, TouchUpdateEvent};
use makepad_widgets::*;
use std::cell::{Cell, RefCell};
use std::rc::Rc;

#[derive(Clone, Copy, PartialEq)]
enum Change {
    Panel,
    Window,
    Background,
    Pause,
    Unavailable,
    Chrome,
    Overlay,
    Pending,
    TouchTap,
    TouchPanel,
    TouchWindow,
    TouchTab,
}

impl Change {
    fn touch(self) -> bool {
        matches!(
            self,
            Self::TouchTap | Self::TouchPanel | Self::TouchWindow | Self::TouchTab
        )
    }

    fn leaving(self) -> Option<Event> {
        match self {
            Self::Window | Self::Unavailable | Self::TouchWindow => {
                Some(Event::WindowLostFocus(WindowId(0, 0)))
            }
            Self::Background => Some(Event::Background),
            Self::Pause => Some(Event::Pause),
            _ => None,
        }
    }

    fn returning(self) -> Option<Event> {
        match self {
            Self::Window | Self::Unavailable | Self::TouchWindow => {
                Some(Event::WindowGotFocus(WindowId(0, 0)))
            }
            Self::Background => Some(Event::Foreground),
            Self::Pause => Some(Event::Resume),
            _ => None,
        }
    }
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
fn foreground_restores_the_focused_notes_editor() {
    check_focus(Change::Background);
}

#[test]
fn resume_restores_the_focused_notes_editor() {
    check_focus(Change::Pause);
}

#[test]
fn unavailable_notes_restore_selection_without_enabling_text_input() {
    check_focus(Change::Unavailable);
}

#[test]
fn an_outside_touch_keeps_the_keyboard_dismissed_until_the_editor_is_tapped() {
    check_focus(Change::TouchTap);
}

#[test]
fn panel_reactivation_restores_focus_after_touch_dismissal() {
    check_focus(Change::TouchPanel);
}

#[test]
fn window_reactivation_restores_focus_after_touch_dismissal() {
    check_focus(Change::TouchWindow);
}

#[test]
fn tab_restores_focus_after_touch_dismissal() {
    check_focus(Change::TouchTab);
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

fn tap(cx: &mut Cx, root: &WidgetRef, session: &mut Session, props: &PanelProps, point: DVec2) {
    cx.fingers.process_tap_count(point, 1.0);
    for (state, time) in [(TouchState::Start, 1.0), (TouchState::Stop, 1.1)] {
        root.handle_event(
            cx,
            &Event::TouchUpdate(TouchUpdateEvent {
                window_id: WindowId(0, 0),
                time,
                modifiers: Default::default(),
                touches: vec![TouchPoint {
                    uid: 1,
                    state,
                    abs: point,
                    time,
                    force: 1.0,
                    radius: DVec2::default(),
                    rotation_angle: 0.0,
                    handled: Cell::new(Area::Empty),
                    sweep_lock: Cell::new(Area::Empty),
                }],
            }),
            &mut Scope::with_data_props(session, props),
        );
    }
    // The platform releases capture after Stop; this fixture has one finger.
    cx.fingers = Default::default();
}

fn check_focus(change: Change) {
    let original = "A thought to return to";
    let mut session = Session::fake(APPS);
    let id = model::create_with_body(&mut session, original.into()).unwrap();
    let other_slot = open(&mut session, NoteList::id());
    let slot = open(&mut session, Editor::note(id));
    if change == Change::Unavailable {
        // Keep the already loaded source, but let observe() mark the deleted
        // note unavailable and drive the real editor's read-only property.
        session
            .store()
            .write(move |tx| tx.execute("UPDATE notes_note SET deleted=1 WHERE id=?1", [id]))
            .unwrap();
    }
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
                            assert_eq!(input.is_read_only(), change == Change::Unavailable);
                            assert_eq!(
                                cx.get_ime_area_rect(),
                                if change == Change::Unavailable {
                                    Rect::default()
                                } else {
                                    input.area().rect(cx)
                                }
                            );
                            root.handle_event(
                                cx,
                                &command(KeyCode::KeyA),
                                &mut Scope::with_data_props(&mut session, &props),
                            );
                            assert_eq!(input.selected_text(), original);
                        }
                        if change.touch() {
                            let status = root.widget(cx, ids!(editor.status_lbl)).area().rect(cx);
                            let point = status.pos + status.size * 0.5;
                            assert!(!input.area().rect(cx).contains(point));
                            tap(cx, &root, &mut session, &props, point);
                        } else if let Some(event) = change.leaving() {
                            root.handle_event(
                                cx,
                                &event,
                                &mut Scope::with_data_props(&mut session, &props),
                            );
                        } else {
                            match change {
                                Change::Panel => {
                                    session.nav(Nav::Focus(other_slot));
                                    props.has_keyboard = false;
                                }
                                Change::Overlay => props.has_keyboard = false,
                                _ => {}
                            }
                        }
                        if !change.touch() && change != Change::Unavailable {
                            other.set_key_focus(cx);
                        }
                    }
                    3 => {
                        assert_eq!(input.text(), original);
                        if change.touch() {
                            assert_eq!(
                                cx.key_focus(),
                                Area::Empty,
                                "touch dismissal must persist across events"
                            );
                            // Probe the next draw: an inactive input must not
                            // register itself with the IME again.
                            cx.show_text_ime(Area::Empty, DVec2::default());
                        } else if change != Change::Chrome && change != Change::Unavailable {
                            assert_eq!(
                                cx.key_focus(),
                                other.area(),
                                "inactive notes must yield focus"
                            );
                        }
                        // After touch dismissal, leave the panel active for
                        // another draw and event cycle before requesting focus.
                        if !change.touch() {
                            if let Some(event) = change.returning() {
                                root.handle_event(
                                    cx,
                                    &event,
                                    &mut Scope::with_data_props(&mut session, &props),
                                );
                            } else {
                                session.nav(Nav::Focus(slot));
                                props.has_keyboard = true;
                                // No event is forwarded after navigation: drawing
                                // must arrange restoration before the next key.
                            }
                        }
                    }
                    4 if change.touch() => {
                        assert_eq!(cx.key_focus(), Area::Empty);
                        assert_eq!(
                            cx.get_ime_area_rect(),
                            Rect::default(),
                            "dismissed keyboard must not reopen on draw"
                        );
                        match change {
                            Change::TouchTap => {
                                let area = input.area().rect(cx);
                                tap(cx, &root, &mut session, &props, area.pos + area.size * 0.5);
                            }
                            Change::TouchPanel => {
                                props.has_keyboard = false;
                                session.nav(Nav::Focus(other_slot));
                                root.handle_event(
                                    cx,
                                    &Event::Signal,
                                    &mut Scope::with_data_props(&mut session, &props),
                                );
                                props.has_keyboard = true;
                                session.nav(Nav::Focus(slot));
                            }
                            Change::TouchWindow => {
                                for event in
                                    [change.leaving().unwrap(), change.returning().unwrap()]
                                {
                                    root.handle_event(
                                        cx,
                                        &event,
                                        &mut Scope::with_data_props(&mut session, &props),
                                    );
                                }
                            }
                            Change::TouchTab => root.handle_event(
                                cx,
                                &Event::KeyDown(KeyEvent {
                                    key_code: KeyCode::Tab,
                                    ..Default::default()
                                }),
                                &mut Scope::with_data_props(&mut session, &props),
                            ),
                            _ => unreachable!(),
                        }
                    }
                    6 => {
                        assert!(
                            input.key_focus(cx),
                            "returning to notes must restore typing"
                        );
                        assert_eq!(
                            cx.get_ime_area_rect(),
                            if change == Change::Unavailable {
                                Rect::default()
                            } else {
                                input.area().rect(cx)
                            }
                        );
                        if matches!(change, Change::Pending | Change::TouchTap) {
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
                        assert_eq!(
                            input.text(),
                            if change == Change::Unavailable {
                                original
                            } else {
                                "Resumed typing"
                            }
                        );
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
    Cx::headless_event_loop_for_draw_cycles(cx, 7);
    assert!(done.get());
}
