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
    let mut fitted_scale = 0.0;
    let mut zoomed_offset = DVec2::default();
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
        Event::Draw(event) if frame < 13 => {
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
            if frame == 0 { fitted_scale = image.borrow().unwrap().camera.scale; }
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
                    assert_eq!(image.borrow().unwrap().camera.scale, fitted_scale * 2.0);
                    if frame == 0 { zoomed_offset = image.borrow().unwrap().camera.offset; }
                    if frame == 1 { assert_eq!(image.borrow().unwrap().camera.offset, zoomed_offset + dvec2(-50.0, -30.0)); }
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
                    image.command(cx, Command::Fit);
                    image.enable(true);
                }
                5 | 6 => {
                    let span = if frame == 5 { 50.0 } else { 100.0 };
                    let state = if frame == 5 { TouchState::Start } else { TouchState::Move };
                    let center = dvec2(300.0, 200.0);
                    root.handle_event(cx, &touch(&[(1, center - dvec2(span, 0.0), state),
                        (2, center + dvec2(span, 0.0), state)]), &mut scope);
                    assert!(props.grab.touch_claimed());
                    assert_eq!(image.borrow().unwrap().camera.scale, fitted_scale * if frame == 5 { 1.0 } else { 2.0 });
                }
                7 => {
                    root.handle_event(cx, &touch(&[(1, dvec2(230.0, 220.0), TouchState::Move),
                        (2, dvec2(430.0, 220.0), TouchState::Move)]), &mut scope);
                    assert_eq!(image.borrow().unwrap().camera.offset, zoomed_offset + dvec2(30.0, 20.0));
                    root.handle_event(cx, &touch(&[(1, dvec2(230.0, 220.0), TouchState::Stop),
                        (2, dvec2(430.0, 220.0), TouchState::Stop)]), &mut scope);
                    assert!(props.grab.touch_claimed(), "the lift belongs to the same gesture");
                    assert!(image.clicked().is_none(), "a pinch ending on a link must not open it");
                    image.command(cx, Command::Fit);
                }
                8 => {
                    assert_eq!(image.borrow().unwrap().camera.scale, fitted_scale);
                    assert!(image.borrow().unwrap().camera.rect(0).pos.x >= 0.0);
                    assert_eq!(point, dvec2(300.0, 200.0));
                    root.handle_event(cx, &touch(&[(3, point, TouchState::Start)]), &mut scope);
                    root.handle_event(cx, &touch(&[(3, point, TouchState::Stop)]), &mut scope);
                    assert!(props.grab.touch_claimed());
                    assert_eq!(image.clicked(), Some(Target::Page(1)), "a single tap follows the link");
                    let texture = image.borrow().unwrap().surfaces[&0].texture.clone();
                    image.document(cx, vec![(420, 595), (595, 420)]);
                    image.page(0, texture.clone(), Vec::new(), None);
                    image.page(1, texture, Vec::new(), None);
                }
                9 => {
                    let event = scroll(dvec2(300.0, 200.0), dvec2(0.0, 600.0), false);
                    root.handle_event(cx, &event, &mut scope);
                    assert_eq!(image.borrow().unwrap().camera.current(), 1, "ordinary scrolling crosses pages at fit width");
                    let Event::Scroll(event) = event else { unreachable!() };
                    assert!(event.handled_y.get() && event.handled_x.get());
                }
                10 => {
                    image.command(cx, Command::ZoomIn);
                    image.borrow_mut().unwrap().camera.pan(dvec2(-80.0, 40.0));
                    image.command(cx, Command::Fit);
                    let page = image.borrow().unwrap().camera.rect(1);
                    assert!(page.pos.x >= 0.0 && page.pos.y >= 0.0);
                    assert!(page.pos.x + page.size.x <= 600.0 + 1e-6 && page.pos.y + page.size.y <= 400.0 + 1e-6);
                }
                11 => {
                    image.command(cx, Command::ZoomOut);
                    image.command(cx, Command::ZoomOut);
                }
                12 => {
                    assert!(props.hits.by_label("PDF page 1").is_some());
                    assert!(props.hits.by_label("PDF page 2").is_some(), "both pages are drawn together in one continuous surface");
                    image.document(cx, vec![(4096, 4096); 1000]);
                    image.borrow_mut().unwrap().camera.resize(dvec2(600.0, 400.0));
                    image.go_to(cx, 700);
                    assert_eq!(image.request(), Some(700), "a distant jump requests its destination first");
                    let wanted = image.borrow().unwrap().wanted();
                    assert!(wanted.len() <= 3 && wanted.iter().all(|page| page.abs_diff(700) <= 1),
                        "a thousand-page file keeps only a bounded neighborhood resident");
                    seen.set(true);
                }
                _ => unreachable!(),
            }
            frame += 1;
            cx.redraw_all();
        }
        _ => {}
    }))));
    Cx::headless_event_loop_for_draw_cycles(cx, 14);
    assert!(finished.get());
}

/// No replay timer or stream of input events may hide a stalled initial load.
#[test]
fn metadata_sizes_the_panel_and_loading_schedules_its_own_draws() {
    use super::super::{Controller, FileViewerWidgetRefExt, Preview, Measure};
    use kernel::{app::App, panel::{Opening, PanelKind}, session::{Action, Session}};
    struct Owner(PanelId, Controller);
    impl Panel for Owner {
        fn id(&self) -> &PanelId { &self.0 }
        fn title(&self) -> String { "document".into() }
        fn wish(&self, cols: usize) -> (u32, u32) { self.1.measure().wish(cols, 7) }
        fn as_any(&mut self) -> &mut dyn std::any::Any { self }
    }
    struct Kind;
    impl PanelKind for Kind {
        fn tag(&self) -> Tag { Tag("viewer-test") }
        fn open(&self, id: &PanelId, _: &mut Opening<'_>) -> Box<dyn Panel> {
            Box::new(Owner(id.clone(), Controller::default()))
        }
    }
    struct TestApp;
    impl App for TestApp {
        fn id(&self) -> &'static str { "viewer-test" }
        fn kinds(&self) -> &'static [&'static dyn PanelKind] { &[&Kind] }
        fn as_any(&self) -> &dyn std::any::Any { self }
    }
    let mut session = Session::fake(&[&TestApp]);
    let id = PanelId::new(Tag("viewer-test"), Vec::<String>::new());
    let open = id.clone();
    session.act(Action::new("open", "open document").moving(move |wm| { wm.open(open, None, false); }));
    session.settle();
    let slot = session.focus().unwrap();
    let panel = session.panel(slot).unwrap();
    let control = panel.borrow_mut().as_any().downcast_mut::<Owner>().unwrap().1.clone();
    assert_eq!(panel.borrow().wish(60), (4, 3));
    session.take_dirty();
    let props = PanelProps { slot, panel, hits: Default::default(), keyboard: Default::default(), grab: Default::default() };
    let finished = Rc::new(Cell::new(false));
    let seen = finished.clone();
    let mut root = WidgetRef::empty();
    let mut pass = None;
    let mut list: Option<DrawList> = None;
    let mut frames = 0;
    let mut copying = false;
    let cx = Rc::new(RefCell::new(Cx::new(Box::new(move |cx, event| match event {
        Event::Startup => {
            root = cx.with_vm(|vm| {
                makepad_widgets::script_mod(vm);
                crate::shell::script_mod(vm);
                let value = script_eval!(vm, { mod.widgets.FileViewer {} });
                WidgetRef::script_from_value(vm, value)
            });
            makepad_widgets::widget_tree::set_ui_root(cx, &root);
            let viewer = root.as_file_viewer();
            viewer.bind(control.clone());
            viewer.show(cx, Preview::Pdf(kernel::caps::demo::PDF.to_vec()));
            let p = DrawPass::new(cx);
            p.set_size(cx, dvec2(600.0, 700.0));
            pass = Some(p);
            list = Some(DrawList::new(cx));
            cx.redraw_all();
        }
        Event::Draw(event) => {
            frames += 1;
            let mut draw = CxDraw::new(cx, event);
            let pass = pass.as_ref().unwrap();
            draw.begin_pass(pass, Some(1.0));
            let list = list.as_mut().unwrap();
            list.begin_always(&mut draw);
            let mut cx = Cx2d::new(&mut draw);
            cx.begin_root_turtle(dvec2(600.0, 700.0), Layout::default());
            props.hits.clear();
            root.draw_all(&mut cx, &mut Scope::with_data_props(&mut session, &props));
            cx.end_pass_sized_turtle();
            assert_eq!(control.measure(), Measure::Pdf(420, 595));
            assert_eq!(session.ws().wish_of(&id), (5, 6), "the opening measures itself without another input");
            let image = root.widget(&cx, ids!(image_box.image)).as_viewer_image();
            if frames == 2 { cx.set_key_focus(image.area()); }
            if frames == 1 {
                assert!(session.take_dirty().layout);
                assert!(cx.new_draw_event.draw_lists.contains(&root.area().draw_list_id().unwrap()),
                    "metadata and page requests must schedule a draw from inside drawing");
            }
            if image.borrow().unwrap().surfaces.len() == 2 && !copying {
                assert!(frames <= 3, "both visible pages must render before extracting text");
                assert!(image.borrow().unwrap().selection.pages.is_empty(), "text extraction waits for page bitmaps");
                assert!(control.verbs().iter().any(|verb| verb.id == "viewer.fit"));
                root.handle_event(&mut cx, &Event::KeyDown(KeyEvent { key_code: KeyCode::KeyA,
                    modifiers: KeyModifiers { logo: true, ..Default::default() }, ..Default::default() }),
                    &mut Scope::with_data_props(&mut session, &props));
                let response = Rc::new(RefCell::new(None));
                root.handle_event(&mut cx, &Event::TextCopy(TextClipboardEvent { response: response.clone() }),
                    &mut Scope::with_data_props(&mut session, &props));
                assert!(response.borrow().is_none());
                assert!(image.status().text_pending);
                copying = true;
            }
            if copying && !image.status().text_pending {
                assert!(image.borrow().unwrap().selection.text(2).unwrap().contains("Back to page 1"));
                seen.set(true);
            }
            list.end(&mut draw);
            draw.end_pass(pass);
        }
        _ => {}
    }))));
    Cx::headless_event_loop_for_draw_cycles(cx, 6);
    assert!(finished.get(), "initial loading or pending copy stalled with no more user input");
}

#[test]
fn pdf_selection_uses_real_input_and_survives_zoom_and_bitmap_eviction() {
    let props = PanelProps {
        slot: 1, panel: Rc::new(RefCell::new(Box::new(TestPanel(PanelId::new(Tag("viewer-test"), Vec::<String>::new()))))),
        hits: Default::default(), keyboard: Default::default(), grab: Default::default(),
    };
    let done = Rc::new(Cell::new(false));
    let finished = done.clone();
    let mut root = WidgetRef::empty();
    let mut pass = None;
    let mut list: Option<DrawList> = None;
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
            let pdf = pdf::Document::open(kernel::caps::demo::PDF.to_vec()).unwrap();
            let image = root.as_viewer_image();
            image.document(cx, pdf.sizes());
            image.text_page(cx, 0, pdf.text(0));
            image.text_page(cx, 1, pdf.text(1));
            for page in 0..2 {
                let page = pdf.render(page).unwrap();
                let texture = super::super::texture(cx, page.width, page.height, page.pixels);
                image.page(page.number, Some(texture), page.links, None);
            }
            image.enable(true);
            let p = DrawPass::new(cx);
            p.set_size(cx, dvec2(600.0, 700.0));
            pass = Some(p);
            list = Some(DrawList::new(cx));
            cx.redraw_all();
        }
        Event::Draw(event) if frame < 7 => {
            props.hits.clear();
            {
                let mut draw = CxDraw::new(cx, event);
                let pass = pass.as_ref().unwrap();
                draw.begin_pass(pass, Some(1.0));
                let list = list.as_mut().unwrap();
                list.begin_always(&mut draw);
                let mut cx = Cx2d::new(&mut draw);
                cx.begin_root_turtle(dvec2(600.0, 700.0), Layout::default());
                root.draw_all(&mut cx, &mut Scope::with_data_props(&mut (), &props));
                cx.end_pass_sized_turtle();
                list.end(&mut draw);
                draw.end_pass(pass);
            }
            let image = root.as_viewer_image();
            let send = |cx: &mut Cx, event: Event| {
                crate::shell::pointer::pointer_before(cx, &event);
                root.handle_event(cx, &event, &mut Scope::with_data_props(&mut (), &props));
                crate::shell::pointer::pointer_after(cx, &event);
            };
            let point = |page: usize, byte: usize, fraction: f64| {
                let image = image.borrow().unwrap();
                let glyph = image.selection.pages[&page].glyphs.iter().find(|g| g.range.start == byte).unwrap();
                let q = glyph.quad.map(|p| dvec2(p[0], p[1]));
                let p = q[0] + (q[1] - q[0]) * fraction + (q[3] - q[0]) * 0.5;
                let rect = image.page_rect(image.viewport(cx), page);
                rect.pos + p * rect.size
            };
            match frame {
                0 => {
                    let a = point(0, 0, 0.1);
                    let b = point(0, "A shared PDF viewe".len(), 0.9);
                    let offset = image.borrow().unwrap().camera.offset;
                    send(cx, mouse(a, true));
                    send(cx, Event::MouseMove(MouseMoveEvent {
                        abs: b, lock_delta: DVec2::default(), window_id: CxWindowPool::id_zero(),
                        modifiers: Default::default(), time: 0.1, handled: Cell::new(Area::Empty),
                    }));
                    send(cx, mouse(b, false));
                    assert_eq!(image.borrow().unwrap().selection.text(2).unwrap(), "A shared PDF viewer");
                    assert_eq!(image.borrow().unwrap().camera.offset, offset, "selecting text must not pan");
                    assert!(image.clicked().is_none());
                }
                1 => {
                    assert!(props.keyboard.kept(cx, &root).has('c'), "copy belongs to the PDF selection, not a file verb");
                    let response = Rc::new(RefCell::new(None));
                    send(cx, Event::TextCopy(TextClipboardEvent { response: response.clone() }));
                    assert_eq!(response.borrow().as_deref(), Some("A shared PDF viewer"));
                    image.command(cx, Command::ZoomIn);
                }
                2 => {
                    let surfaces = std::mem::take(&mut image.borrow_mut().unwrap().surfaces);
                    let response = Rc::new(RefCell::new(None));
                    send(cx, Event::TextCopy(TextClipboardEvent { response: response.clone() }));
                    assert_eq!(response.borrow().as_deref(), Some("A shared PDF viewer"));
                    send(cx, Event::KeyDown(KeyEvent { key_code: KeyCode::KeyA,
                        modifiers: KeyModifiers { logo: true, ..Default::default() }, ..Default::default() }));
                    assert!(image.borrow().unwrap().selection.text(2).unwrap().contains("Back to page 1"));
                    // The same native copy command must complete after a text
                    // page has left the cache, without a second key press.
                    image.borrow_mut().unwrap().selection.pages.remove(&1);
                    let response = Rc::new(RefCell::new(None));
                    send(cx, Event::TextCopy(TextClipboardEvent { response: response.clone() }));
                    assert!(response.borrow().is_none(), "no partial clipboard while text is missing");
                    let span = image.copy_request().unwrap();
                    let mut worker = super::super::worker::Worker::start(super::super::Preview::Pdf(kernel::caps::demo::PDF.to_vec())).unwrap();
                    assert!(matches!(worker.poll(), Some(Ok(super::super::worker::Ready::PdfInfo(_)))));
                    worker.request(super::super::worker::Request::Copy(Some(span))).unwrap();
                    // A verb-bar press moves keyboard focus to the shell before
                    // its command runs. Keep the in-flight copy through both.
                    for command in [Command::Fit, Command::FitWidth, Command::ZoomIn, Command::ZoomOut] {
                        send(cx, Event::KeyFocus(KeyFocusEvent { prev: image.area(), focus: Area::Empty }));
                        assert_eq!(image.copy_request(), Some(span), "{command:?}: toolbar focus must preserve a pending copy");
                        image.command(cx, command);
                        assert_eq!(image.copy_request(), Some(span), "{command:?}: changing the view must preserve a pending copy");
                    }
                    let Some(Ok(super::super::worker::Ready::Copied(span, result))) = worker.poll() else { panic!("copy answered") };
                    image.copied(cx, span, result);
                    assert!(image.borrow().unwrap().selection.text(2).unwrap().contains("Back to page 1"),
                        "the complete copy is available without reloading the page geometry");
                    assert!(!image.status().text_pending);
                    image.borrow_mut().unwrap().surfaces = surfaces;
                    image.command(cx, Command::Fit);
                }
                3 => {
                    let p = point(0, 3, 0.5);
                    send(cx, touch(&[(1, p, TouchState::Start)]));
                    let timer_id = image.borrow().unwrap().touch_hold.0;
                    send(cx, Event::Timer(TimerEvent { timer_id, time: Some(0.45) }));
                    send(cx, touch(&[(1, p, TouchState::Stop)]));
                    assert_eq!(image.borrow().unwrap().selection.text(2).unwrap(), "shared");
                    assert!(image.clicked().is_none(), "long-press selection never activates a link");
                }
                4 => {
                    send(cx, Event::KeyDown(KeyEvent { key_code: KeyCode::KeyA,
                        modifiers: KeyModifiers { logo: true, ..Default::default() }, ..Default::default() }));
                    send(cx, Event::TextCopy(TextClipboardEvent { response: Rc::new(RefCell::new(None)) }));
                    let span = image.copy_request().expect("copy waits for the evicted page");
                    send(cx, Event::Background);
                    assert!(image.copy_request().is_none(), "leaving the app still cancels a pending copy");
                    image.copied(cx, span, Ok("obsolete copy".into()));
                    assert!(image.borrow().unwrap().selection.text(2).is_none(), "a canceled result must not be accepted");
                    image.enable(false);
                    assert!(!image.borrow().unwrap().has_selection());
                }
                5 => {
                    assert!(props.hits.is_empty());
                    image.set(cx, None, dvec2(600.0, 400.0), false, Vec::new());
                    image.enable(true);
                    send(cx, Event::KeyDown(KeyEvent { key_code: KeyCode::KeyA,
                        modifiers: KeyModifiers { logo: true, ..Default::default() }, ..Default::default() }));
                    assert!(image.borrow().unwrap().text_selection().is_none(), "images do not take text-selection chords");
                    assert!(!image.borrow().unwrap().has_selection());
                    let pdf = pdf::Document::open(pdf::dense_fixture(1, 50, 80)).unwrap();
                    image.document(cx, pdf.sizes());
                    let text = pdf.text(0);
                    assert_eq!(text.glyphs.len(), 4000);
                    image.text_page(cx, 0, text);
                    let page = pdf.render(0).unwrap();
                    let texture = super::super::texture(cx, page.width, page.height, page.pixels);
                    image.page(0, Some(texture), page.links, None);
                    image.borrow_mut().unwrap().camera.resize(dvec2(600.0, 700.0));
                    image.command(cx, Command::Fit);
                }
                6 => {
                    assert_eq!(props.hits.len(), 52,
                        "4,000 rendered characters register 50 line hits plus two page hits");
                    let p = point(0, 0, 0.5);
                    assert_eq!(props.hits.at(p).unwrap().cursor, MouseCursor::Text);
                    send(cx, mouse(p, true));
                    send(cx, mouse(p, false));
                    assert!(image.borrow().unwrap().press.is_none());
                    finished.set(true);
                }
                _ => unreachable!(),
            }
            frame += 1;
            cx.redraw_all();
        }
        Event::KeyFocus(_) => root.handle_event(cx, event, &mut Scope::with_data_props(&mut (), &props)),
        _ => {}
    }))));
    Cx::headless_event_loop_for_draw_cycles(cx, 8);
    assert!(done.get());
}
