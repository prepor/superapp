//! Real pointer events and clipboard requests against every shared text surface.

use super::source_input::{SourceInput, Span, Style};
use makepad_widgets::makepad_draw::text::selection::Cursor;
use makepad_widgets::makepad_platform::studio::StudioToApp;
use makepad_widgets::*;
use std::{
    cell::{Cell, RefCell},
    collections::BTreeMap,
    rc::Rc,
};

struct TextSurface {
    cx: Cx,
    root: WidgetRef,
    pass: DrawPass,
    list: DrawList,
    frame: u32,
    time: f64,
    points: BTreeMap<usize, DVec2>,
}

impl TextSurface {
    fn new(kind: &str, text: &str, indices: &[usize]) -> Self {
        let host = Rc::new(RefCell::new(WidgetRef::empty()));
        let receiver = host.clone();
        let mut cx = Cx::new(Box::new(move |cx, event| {
            receiver
                .borrow()
                .handle_event(cx, event, &mut Scope::empty());
        }));
        let root = cx.with_vm(|vm| {
            makepad_widgets::script_mod(vm);
            crate::shell::script_mod(vm);
            let value = match kind {
                "field" => script_eval!(vm, { mod.widgets.SField {} }),
                "multiline" => script_eval!(vm, {
                    mod.widgets.SField { width: 160, height: 180, is_multiline: true }
                }),
                // Measure its caret positions before making the surface read-only.
                "readonly" => script_eval!(vm, { mod.widgets.SText { is_read_only: false } }),
                "source" => script_eval!(vm, { mod.widgets.SourceInput { width: 160 } }),
                "html" => script_eval!(vm, {
                    use mod.prelude.widgets.*
                    mod.widgets.Html { width: 160, height: Fit, selectable: true }
                }),
                "markdown" => script_eval!(vm, {
                    use mod.prelude.widgets.*
                    mod.widgets.Markdown { width: 160, height: Fit, selectable: true }
                }),
                _ => unreachable!(),
            };
            WidgetRef::script_from_value(vm, value)
        });
        *host.borrow_mut() = root.clone();
        makepad_widgets::widget_tree::set_ui_root(&mut cx, &root);
        root.set_text(&mut cx, text);
        if let Some(mut source) = root.borrow_mut::<SourceInput>() {
            source.set_spans(
                &mut cx,
                vec![Span {
                    range: 6..12,
                    style: Style {
                        bold: true,
                        ..Style::default()
                    },
                }],
            );
        }
        let pass = DrawPass::new(&mut cx);
        pass.set_size(&mut cx, dvec2(420.0, 220.0));
        let list = DrawList::new(&mut cx);
        let mut surface = Self {
            cx,
            root,
            pass,
            list,
            frame: 0,
            time: 0.0,
            points: BTreeMap::new(),
        };
        surface.draw();
        surface.root.set_key_focus(&mut surface.cx);
        surface.settle_focus();
        for &index in indices {
            let cursor = Cursor {
                index,
                prefer_next_row: true,
            };
            let input = if let Some(mut input) = surface.root.borrow_mut::<TextInput>() {
                input.set_cursor(&mut surface.cx, cursor, false);
                true
            } else if let Some(mut input) = surface.root.borrow_mut::<SourceInput>() {
                input.set_cursor(&mut surface.cx, cursor, false);
                true
            } else {
                false
            };
            let point = if input {
                surface.draw();
                let rect = surface
                    .root
                    .borrow::<TextInput>()
                    .and_then(|input| input.cursor_rect_in_absolute(&surface.cx))
                    .or_else(|| {
                        surface
                            .root
                            .borrow::<SourceInput>()
                            .and_then(|input| input.cursor_rect_in_absolute(&surface.cx))
                    })
                    .unwrap();
                rect.pos + rect.size * 0.5
            } else {
                let rect = surface.root.area().rect(&surface.cx);
                (1..rect.size.y as usize)
                    .flat_map(|y| (1..rect.size.x as usize).map(move |x| (x, y)))
                    .map(|(x, y)| rect.pos + dvec2(x as f64, y as f64))
                    .find(|&point| {
                        surface
                            .root
                            .selection_point_to_char_index(&surface.cx, point)
                            == Some(index)
                    })
                    .unwrap_or_else(|| panic!("{kind}: no drawn point for byte {index}"))
            };
            surface.points.insert(index, point);
        }
        if kind == "readonly" {
            surface
                .root
                .borrow_mut::<TextInput>()
                .unwrap()
                .set_is_read_only(&mut surface.cx, true);
            surface.draw();
        }
        surface
    }

    fn draw(&mut self) {
        self.frame += 1;
        self.cx.new_draw_event = DrawEvent::default();
        let event = DrawEvent {
            redraw_all: true,
            time: f64::from(self.frame) / 60.0,
            ..Default::default()
        };
        let mut draw = CxDraw::new(&mut self.cx, &event);
        draw.begin_pass(&self.pass, Some(1.0));
        self.list.begin_always(&mut draw);
        {
            let mut cx = Cx2d::new(&mut draw);
            cx.begin_root_turtle(dvec2(420.0, 220.0), Layout::default());
            self.root.draw_all(&mut cx, &mut Scope::empty());
            cx.end_pass_sized_turtle();
        }
        self.list.end(&mut draw);
        draw.end_pass(&self.pass);
    }

    fn settle_focus(&mut self) {
        self.cx.dispatch_studio_msg(
            StudioToApp::Custom("selection-test-focus".into()),
            CxWindowPool::id_zero(),
            DVec2::default(),
        );
    }

    fn send(&mut self, event: Event) {
        self.root
            .handle_event(&mut self.cx, &event, &mut Scope::empty());
        self.settle_focus();
    }

    fn press(&mut self, index: usize, shift: bool, taps: u32) {
        let abs = self.points[&index];
        self.time += 1.0;
        self.cx
            .fingers
            .mouse_down(MouseButton::PRIMARY, CxWindowPool::id_zero());
        for tap in 0..taps {
            self.cx
                .fingers
                .process_tap_count(abs, self.time + f64::from(tap) * 0.05);
        }
        self.send(Event::MouseDown(MouseDownEvent {
            abs,
            window_id: CxWindowPool::id_zero(),
            button: MouseButton::PRIMARY,
            modifiers: KeyModifiers {
                shift,
                ..Default::default()
            },
            handled: Cell::new(Area::Empty),
            time: self.time,
        }));
    }

    fn release(&mut self, index: usize) {
        // Releasing Shift before the mouse must still keep the selection.
        self.send(Event::MouseUp(MouseUpEvent {
            abs: self.points[&index],
            window_id: CxWindowPool::id_zero(),
            button: MouseButton::PRIMARY,
            modifiers: Default::default(),
            time: self.time + 0.2,
        }));
        self.cx.fingers.mouse_up(MouseButton::PRIMARY);
        self.draw();
    }

    fn click(&mut self, index: usize, shift: bool) {
        self.press(index, shift, 1);
        self.release(index);
    }

    fn copy(&mut self) -> String {
        let response = Rc::new(RefCell::new(None));
        self.send(Event::TextCopy(TextClipboardEvent {
            response: response.clone(),
        }));
        let text = response.borrow().clone().unwrap_or_default();
        text
    }
}

#[test]
fn shift_click_extends_shared_text_surfaces_from_the_original_anchor() {
    for kind in [
        "field",
        "multiline",
        "readonly",
        "source",
        "html",
        "markdown",
    ] {
        let text = "alpha bravó charlie delta";
        let mut surface = TextSurface::new(kind, text, &[0, 6, 7, 13, 21, 26]);
        surface.click(6, false);
        surface.click(26, true);
        assert_eq!(surface.copy(), &text[6..], "{kind}: extend from a caret");

        surface.click(13, true);
        assert_eq!(
            surface.copy(),
            &text[6..13],
            "{kind}: shrink inside a selection"
        );
        surface.click(0, true);
        assert_eq!(surface.copy(), &text[..6], "{kind}: cross the anchor");
        surface.click(21, true);
        assert_eq!(
            surface.copy(),
            &text[6..21],
            "{kind}: retain the anchor on repeated clicks"
        );

        // The drag begins inside the selection and crosses its original anchor.
        surface.press(13, true, 1);
        surface.draw();
        surface.send(Event::MouseMove(MouseMoveEvent {
            abs: surface.points[&0],
            window_id: CxWindowPool::id_zero(),
            lock_delta: DVec2::default(),
            modifiers: Default::default(),
            time: surface.time + 0.1,
            handled: Cell::new(Area::Empty),
        }));
        surface.release(0);
        assert_eq!(
            surface.copy(),
            &text[..6],
            "{kind}: drag from the original anchor"
        );

        surface.click(21, true);
        surface.click(13, false);
        assert_eq!(
            surface.copy(),
            "",
            "{kind}: plain click still collapses a selection"
        );
        surface.click(26, true);
        assert_eq!(
            surface.copy(),
            &text[13..],
            "{kind}: plain click starts a new anchor"
        );

        surface.press(7, false, 2);
        surface.release(7);
        assert_eq!(
            surface.copy(),
            "bravó",
            "{kind}: double click still takes a word"
        );
        surface.press(7, true, 2);
        surface.release(7);
        assert_eq!(
            surface.copy(),
            "b",
            "{kind}: Shift overrides a repeated click's word selection"
        );

        surface.send(Event::KeyDown(KeyEvent {
            key_code: KeyCode::KeyA,
            modifiers: KeyModifiers {
                logo: true,
                control: true,
                ..Default::default()
            },
            time: surface.time,
            is_repeat: false,
        }));
        surface.click(13, true);
        assert_eq!(
            surface.copy(),
            &text[..13],
            "{kind}: extend a keyboard selection"
        );
    }
}

#[test]
fn shift_click_extends_across_lines_and_inline_links() {
    for kind in ["multiline", "source", "html", "markdown"] {
        let text = match kind {
            "html" => "alpha bravó<br/>charlie <a href=\"https://example.com\">delta</a>",
            "markdown" => "alpha bravó\n\ncharlie [delta](https://example.com)",
            _ => "alpha bravó\ncharlie delta",
        };
        let mut surface = TextSurface::new(kind, text, &[0, 6]);
        let rendered = if kind == "html" || kind == "markdown" {
            surface.root.selection_get_full_text()
        } else {
            text.into()
        };
        let end = rendered.find("delta").unwrap() + 3;
        // Measure the endpoint using the rendered text's byte offsets.
        surface = TextSurface::new(kind, text, &[0, 6, end]);
        surface.click(6, false);
        surface.click(end, true);
        assert_eq!(
            surface.copy(),
            &rendered[6..end],
            "{kind}: select across lines and into a link"
        );
        surface.click(0, true);
        assert_eq!(
            surface.copy(),
            &rendered[..6],
            "{kind}: return across the anchor"
        );
    }
}
