use super::super::{Span, Style};
use super::*;
use makepad_widgets::script_eval;
use std::cell::{Cell, RefCell};

#[test]
fn focused_source_input_draws_a_visible_caret() {
    let done = Rc::new(Cell::new(false));
    let seen = done.clone();
    let mut root = WidgetRef::empty();
    let mut pass = None;
    let mut draw_list: Option<DrawList> = None;
    let mut frame = 0;
    let cx = Rc::new(RefCell::new(Cx::new(Box::new(
        move |cx, event| match event {
            Event::Startup => {
                root = cx.with_vm(|vm| {
                    makepad_widgets::script_mod(vm);
                    crate::shell::script_mod(vm);
                    let value = script_eval!(vm, { mod.widgets.SourceInput{} });
                    WidgetRef::script_from_value(vm, value)
                });
                makepad_widgets::widget_tree::set_ui_root(cx, &root);
                let p = DrawPass::new(cx);
                p.set_size(cx, dvec2(260.0, 220.0));
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
                let mut cx = Cx2d::new(&mut draw);
                cx.begin_root_turtle(dvec2(260.0, 220.0), Layout::default());
                root.draw_all(&mut cx, &mut Scope::empty());
                cx.end_turtle();
                if frame > 1 {
                    let input = root.borrow::<SourceInput>().unwrap();
                    assert!(root.key_focus(&cx));
                    let mut focus = [f32::NAN];
                    let mut blink = [f32::NAN];
                    assert!(input.draw_cursor.get_instance_on_area(
                        &cx,
                        live_id!(focus),
                        &mut focus
                    ));
                    assert!(input.draw_cursor.get_instance_on_area(
                        &cx,
                        live_id!(blink),
                        &mut blink
                    ));
                    let expected = if frame == 4 { 0.0 } else { 1.0 };
                    assert!(
                        ((1.0 - blink[0]) * focus[0] - expected).abs() < 0.01,
                        "caret opacity must be {expected}: focus={focus:?}, blink={blink:?}"
                    );
                    let caret = input.draw_cursor.area().rect(&cx);
                    assert!(
                        caret.size.x >= 1.0 && caret.size.y > 8.0,
                        "caret: {caret:?}"
                    );
                    assert!(input
                        .area()
                        .rect(&cx)
                        .contains(caret.pos + caret.size * 0.5));
                    if frame > 2 {
                        assert_eq!(
                            input.text, "A **bold** thought",
                            "animation must retain the source"
                        );
                    }
                    if frame == 5 {
                        seen.set(true);
                    }
                }
                if frame == 1 {
                    root.borrow_mut::<SourceInput>()
                        .unwrap()
                        .take_key_focus(&mut cx);
                } else if frame == 2 {
                    root.set_text(&mut cx, "A **bold** thought");
                } else if frame == 3 {
                    root.borrow_mut::<SourceInput>()
                        .unwrap()
                        .animator_cut(&mut cx, ids!(blink.on));
                } else if frame == 4 {
                    // Focusing an already focused input must reset its blink.
                    root.borrow_mut::<SourceInput>()
                        .unwrap()
                        .take_key_focus(&mut cx);
                }
                if frame < 5 {
                    cx.redraw_all();
                }
                list.end(&mut draw);
                draw.end_pass(pass);
            }
            _ => root.handle_event(cx, event, &mut Scope::empty()),
        },
    ))));
    Cx::headless_event_loop_for_draw_cycles(cx, 5);
    assert!(done.get());
}

#[test]
fn source_styles_preserve_wrapping_caret_geometry_and_cached_layout() {
    let done = Rc::new(Cell::new(false));
    let seen = done.clone();
    let mut root = WidgetRef::empty();
    let mut pass = None;
    let mut draw_list: Option<DrawList> = None;
    let mut previous: Option<Rc<LaidoutText>> = None;
    let mut frame = 0;
    let text = "Plain **bold café words wrapping over a narrow editor**\nAn *italic résumé* and `**code**`.";
    let bold = text.find("**").unwrap()..text.rfind("**\n").unwrap() + 2;
    let italic = text.find("*italic").unwrap()..text.find("* and").unwrap() + 1;
    let cx = Rc::new(RefCell::new(Cx::new(Box::new(
        move |cx, event| match event {
            Event::Startup => {
                root = cx.with_vm(|vm| {
                    makepad_widgets::script_mod(vm);
                    // Shared inputs must exist before any app UI is registered.
                    crate::shell::script_mod(vm);
                    let value = script_eval!(vm, { mod.widgets.SourceInput{} });
                    WidgetRef::script_from_value(vm, value)
                });
                makepad_widgets::widget_tree::set_ui_root(cx, &root);
                root.set_text(cx, text);
                root.borrow_mut::<SourceInput>().unwrap().set_spans(
                    cx,
                    vec![
                        Span {
                            range: bold.clone(),
                            style: Style {
                                bold: true,
                                ..Style::default()
                            },
                        },
                        Span {
                            range: italic.clone(),
                            style: Style {
                                italic: true,
                                ..Style::default()
                            },
                        },
                    ],
                );
                let p = DrawPass::new(cx);
                p.set_size(cx, dvec2(260.0, 220.0));
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
                let mut cx = Cx2d::new(&mut draw);
                cx.begin_root_turtle(dvec2(260.0, 220.0), Layout::default());
                root.draw_all(&mut cx, &mut Scope::empty());
                cx.end_turtle();
                {
                    let input = root.borrow::<SourceInput>().unwrap();
                    let styled = input.laidout_text.as_ref().unwrap();
                    let plain = input.draw_text.layout(
                        &mut cx,
                        0.0,
                        0.0,
                        input.laidout_width,
                        true,
                        Align::default(),
                        text,
                    );
                    assert!(styled.rows.len() > 2, "the long emphasis must wrap");
                    assert_eq!(styled.rows.len(), plain.rows.len());
                    let mut offset = 0;
                    let mut emphasized = 0;
                    for (styled, plain) in styled.rows.iter().zip(&plain.rows) {
                        assert_eq!(styled.text, plain.text);
                        assert_eq!(styled.origin_in_lpxs, plain.origin_in_lpxs);
                        assert_eq!(styled.width_in_lpxs, plain.width_in_lpxs);
                        for (glyph, original) in styled.glyphs.iter().zip(&plain.glyphs) {
                            assert_eq!(glyph.cluster, original.cluster);
                            assert_eq!(glyph.origin_in_lpxs, original.origin_in_lpxs);
                            let inside = bold.contains(&(offset + glyph.cluster))
                                || italic.contains(&(offset + glyph.cluster));
                            assert_eq!(
                                !Rc::ptr_eq(&glyph.font, &original.font),
                                inside,
                                "source offset {}",
                                offset + glyph.cluster
                            );
                            emphasized += usize::from(inside);
                        }
                        offset += plain.text.len() + usize::from(plain.newline);
                    }
                    assert!(emphasized > 10);
                    for sample in ["ab\tX", "\t\t**café**\n\tend", "0123456789\tY", "\t"] {
                        for width in [240.0, 67.0] {
                            let tabs = super::super::source_layout(
                                &mut cx,
                                &input.draw_text,
                                Some(width),
                                true,
                                Align::default(),
                                sample,
                            );
                            assert_eq!(&*tabs.text, sample);
                            for row in &tabs.rows {
                                assert!(sample.is_char_boundary(row.text.start_in_parent()));
                                assert!(sample.is_char_boundary(row.text.end_in_parent()));
                                for glyph in &row.glyphs {
                                    assert!(glyph.cluster < row.text.len());
                                    assert!(row.text.is_char_boundary(glyph.cluster));
                                }
                            }
                            for (index, _) in sample.grapheme_indices(true) {
                                let position = tabs.cursor_to_position(Cursor {
                                    index,
                                    prefer_next_row: true,
                                });
                                assert!(position.x_in_lpxs.is_finite());
                                assert_eq!(
                                    tabs.position_to_cursor(position).index,
                                    index,
                                    "{sample:?} at {index}"
                                );
                            }
                        }
                    }
                    let tab = super::super::source_layout(
                        &mut cx,
                        &input.draw_text,
                        None,
                        false,
                        Align::default(),
                        "ab\tX",
                    );
                    let spaces = input.draw_text.layout(
                        &mut cx,
                        0.0,
                        0.0,
                        None,
                        false,
                        Align::default(),
                        "ab  X",
                    );
                    assert_eq!(
                        tab.rows[0].width_in_lpxs, spaces.rows[0].width_in_lpxs,
                        "tab stops occupy four columns"
                    );
                    assert_eq!(input.filter_input("a\r\nb", false), "a\nb");
                    if let Some(previous) = &previous {
                        assert!(
                            Rc::ptr_eq(previous, styled),
                            "an idle redraw must reuse the styled layout"
                        );
                        seen.set(true);
                    }
                    previous = Some(styled.clone());
                }
                if frame < 2 {
                    cx.redraw_all();
                }
                list.end(&mut draw);
                draw.end_pass(pass);
            }
            _ => {}
        },
    ))));
    Cx::headless_event_loop_for_draw_cycles(cx, 2);
    assert!(done.get());
}
