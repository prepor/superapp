//! Shared form input policy: Tab/Shift-Tab walk visible fields, including
//! fields whose controls live in a scrolling form rather than a panel root.
use makepad_widgets::*;

/// Hidden, undrawn and swept controls have no usable geometry. In particular,
/// probing an instance area with zero instances logs an error in Makepad.
pub fn drawn_rect(cx: &Cx, area: Area) -> Option<Rect> {
    if !area.is_valid(cx) {
        return None;
    }
    let rect = area.rect(cx);
    (rect.size.x > 0.0 && rect.size.y > 0.0).then_some(rect)
}

pub fn tab(cx: &mut Cx, event: &Event, inputs: &[TextInputRef]) -> Option<usize> {
    if !matches!(event, Event::KeyDown(k) if k.key_code == KeyCode::Tab) {
        return None;
    }
    let controls = inputs
        .iter()
        .map(|input| (**input).clone())
        .collect::<Vec<_>>();
    tab_controls(cx, event, &controls)
}

/// Walk a form's focusable controls in visual order. A ring can mix text
/// inputs, selects and other widgets without giving each field its own Tab
/// policy. Hidden, disabled and undrawn controls cannot receive focus.
pub fn tab_controls(cx: &mut Cx, event: &Event, controls: &[WidgetRef]) -> Option<usize> {
    let Event::KeyDown(k) = event else {
        return None;
    };
    if k.key_code != KeyCode::Tab
        || k.modifiers.logo
        || k.modifiers.control
        || k.modifiers.alt
        || controls.is_empty()
        || cx.key_focus() == Area::Empty
    {
        return None;
    }
    let focused = controls.iter().position(|control| control.key_focus(cx))?;
    let n = controls.len();
    let next = (1..=n)
        .map(|step| {
            if k.modifiers.shift {
                (focused + n - step) % n
            } else {
                (focused + step) % n
            }
        })
        .find(|&i| {
            controls[i].visible()
                && !controls[i].disabled(cx)
                && drawn_rect(cx, controls[i].area()).is_some()
        })?;
    controls[next].set_key_focus(cx);
    if let Some(mut field) = controls[next].borrow_mut::<TextInput>() {
        field.select_all(cx);
    }
    Some(next)
}

/// Keep the focused field visible in a form stored in one portal-list item.
pub fn reveal(cx: &mut Cx, list: &PortalListRef, input: &TextInputRef) {
    reveal_control(cx, list, input);
}

/// Keep any focused form control visible within the scrolling form.
pub fn reveal_control(cx: &mut Cx, list: &PortalListRef, control: &WidgetRef) {
    let Some(viewport) = drawn_rect(cx, list.area()) else {
        return;
    };
    let Some(field) = drawn_rect(cx, control.area()) else {
        return;
    };
    let delta = if field.pos.y < viewport.pos.y {
        viewport.pos.y - field.pos.y + 8.0
    } else if field.pos.y + field.size.y > viewport.pos.y + viewport.size.y {
        viewport.pos.y + viewport.size.y - field.pos.y - field.size.y - 8.0
    } else {
        return;
    };
    if let Some(mut list) = list.borrow_mut() {
        let scroll = list.first_scroll() + delta;
        list.set_first_id_and_scroll(0, scroll);
        list.redraw(cx);
    }
}

/// Keep a click's focus across a redraw between press and release. An older
/// field's outside-release handler must not clear the field just selected.
#[derive(Default)]
pub struct ClickFocus {
    pressed: Option<usize>,
}
impl ClickFocus {
    pub fn handle(
        &mut self,
        cx: &mut Cx,
        event: &Event,
        props: &crate::shell::hosted::PanelProps,
        inputs: &[TextInputRef],
        labels: &[&str],
    ) {
        let position = match event {
            Event::MouseDown(e) => e.abs,
            Event::MouseUp(e) => e.abs,
            _ => return,
        };
        let target = props
            .hits
            .at(position)
            .filter(|h| h.slot == Some(props.slot))
            .and_then(|h| labels.iter().position(|label| *label == h.label));
        if matches!(event, Event::MouseDown(_)) {
            self.pressed = target;
            if let Some(i) = target {
                inputs[i].set_key_focus(cx);
            }
        } else if self.pressed.take() == target {
            if let Some(i) = target {
                inputs[i].set_key_focus(cx);
            }
        }
    }
}

#[cfg(all(test, headless))]
mod tests {
    use super::*;
    use std::cell::{Cell, RefCell};
    use std::rc::Rc;

    #[test]
    fn mixed_tab_ring_skips_unavailable_controls_and_selects_only_input_text() {
        let done = Rc::new(Cell::new(false));
        let seen = done.clone();
        let mut root = WidgetRef::empty();
        let mut undrawn = WidgetRef::empty();
        let mut pass = None;
        let mut draw_list: Option<DrawList> = None;
        let mut frame = 0;
        let cx = Rc::new(RefCell::new(Cx::new(Box::new(
            move |cx, event| match event {
                Event::Startup => {
                    (root, undrawn) = cx.with_vm(|vm| {
                        makepad_widgets::script_mod(vm);
                        let form = script_eval!(vm, {
                            use mod.prelude.widgets.*
                            mod.widgets.View {
                                width: Fill, height: Fit, flow: Down
                                first := mod.widgets.TextInput { text: "Event title" }
                                disabled := mod.widgets.Button { text: "Disabled" }
                                hidden := mod.widgets.Button { text: "Hidden", visible: false }
                                last := mod.widgets.Button { text: "Calendar" }
                            }
                        });
                        let undrawn = script_eval!(vm, { mod.widgets.Button { text: "Undrawn" } });
                        (
                            WidgetRef::script_from_value(vm, form),
                            WidgetRef::script_from_value(vm, undrawn),
                        )
                    });
                    makepad_widgets::widget_tree::set_ui_root(cx, &root);
                    root.widget(cx, ids!(disabled)).set_disabled(cx, true);
                    let p = DrawPass::new(cx);
                    p.set_size(cx, dvec2(300.0, 250.0));
                    pass = Some(p);
                    draw_list = Some(DrawList::new(cx));
                    cx.redraw_all();
                }
                Event::Draw(event) => {
                    frame += 1;
                    let mut draw = CxDraw::new(cx, event);
                    let pass = pass.as_ref().unwrap();
                    draw.begin_pass(pass, Some(1.0));
                    let list = draw_list.as_mut().unwrap();
                    list.begin_always(&mut draw);
                    {
                        let mut cx = Cx2d::new(&mut draw);
                        cx.begin_root_turtle(dvec2(300.0, 250.0), Layout::default());
                        root.draw_all(&mut cx, &mut Scope::empty());
                        cx.end_turtle();
                        let controls = [
                            root.widget(&mut cx, ids!(first)),
                            root.widget(&mut cx, ids!(disabled)),
                            root.widget(&mut cx, ids!(hidden)),
                            undrawn.clone(),
                            root.widget(&mut cx, ids!(last)),
                        ];
                        let key = |shift, logo| {
                            Event::KeyDown(KeyEvent {
                                key_code: KeyCode::Tab,
                                modifiers: KeyModifiers {
                                    shift,
                                    logo,
                                    ..Default::default()
                                },
                                is_repeat: false,
                                time: 0.0,
                            })
                        };
                        match frame {
                            1 => controls[0].set_key_focus(&mut cx),
                            2 => {
                                assert!(controls[0].key_focus(&cx));
                                assert_eq!(
                                    tab_controls(&mut cx, &key(false, false), &controls),
                                    Some(4)
                                );
                                assert_eq!(controls[4].text(), "Calendar");
                            }
                            3 => {
                                assert!(controls[4].key_focus(&cx));
                                assert_eq!(
                                    tab_controls(&mut cx, &key(true, false), &controls),
                                    Some(0)
                                );
                                assert_eq!(
                                    controls[0].borrow::<TextInput>().unwrap().selected_text(),
                                    "Event title"
                                );
                            }
                            4 => {
                                assert!(controls[0].key_focus(&cx));
                                assert_eq!(
                                    tab_controls(&mut cx, &key(true, false), &controls),
                                    Some(4)
                                );
                            }
                            5 => {
                                assert!(controls[4].key_focus(&cx));
                                assert_eq!(
                                    tab_controls(&mut cx, &key(false, false), &controls),
                                    Some(0)
                                );
                            }
                            6 => {
                                assert!(controls[0].key_focus(&cx));
                                assert_eq!(
                                    tab_controls(&mut cx, &key(false, true), &controls),
                                    None
                                );
                                seen.set(true);
                            }
                            _ => unreachable!(),
                        }
                        if frame < 6 {
                            cx.redraw_all();
                        }
                    }
                    list.end(&mut draw);
                    draw.end_pass(pass);
                }
                _ => root.handle_event(cx, event, &mut Scope::empty()),
            },
        ))));
        Cx::headless_event_loop_for_draw_cycles(cx, 6);
        assert!(done.get());
    }
}
