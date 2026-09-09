//! Exercise the real form and popup with CPU draw lists and platform focus
//! bookkeeping. No window, renderer, operating-system loop or account is used.

use super::*;
use kernel::panel::{Panel, PanelId, Tag};
use makepad_widgets::makepad_platform::studio::StudioToApp;
use std::cell::{Cell, RefCell};
use std::rc::Rc;

struct TestPanel(PanelId);

impl Panel for TestPanel {
    fn id(&self) -> &PanelId {
        &self.0
    }
    fn title(&self) -> String {
        "Select form".into()
    }
    fn as_any(&mut self) -> &mut dyn std::any::Any {
        self
    }
}

struct Host {
    root: WidgetRef,
    props: PanelProps,
}

impl Host {
    fn event(&self, cx: &mut Cx, event: &Event) {
        let select = self.root.widget(cx, ids!(calendar)).as_select();
        let mut data = ();
        let mut scope = Scope::with_data_props(&mut data, &self.props);
        if !handle_open(cx, event, &mut scope, &[select]) {
            self.root.handle_event(cx, event, &mut scope);
        }
    }
}

struct Form {
    cx: Cx,
    host: Rc<RefCell<Host>>,
    select: SelectRef,
    pass: DrawPass,
    list: DrawList,
    frame: u32,
}

impl Form {
    fn new() -> Self {
        let host = Rc::new(RefCell::new(Host {
            root: WidgetRef::empty(),
            props: PanelProps {
                slot: 1,
                panel: Rc::new(RefCell::new(Box::new(TestPanel(PanelId::new(
                    Tag("select-test"),
                    Vec::<String>::new(),
                ))))),
                hits: Default::default(),
                keyboard: Default::default(),
                has_keyboard: true,
                grab: Default::default(),
            },
        }));
        let receiver = host.clone();
        let mut cx = Cx::new(Box::new(move |cx, event| {
            receiver.borrow().event(cx, event)
        }));
        let root = cx.with_vm(|vm| {
            makepad_widgets::script_mod(vm);
            crate::shell::script_mod(vm);
            let value = script_eval!(vm, {
                use mod.prelude.widgets.*
                mod.widgets.View {
                    width: Fill, height: Fill, flow: Down, spacing: 8
                    title := mod.widgets.SField { text: "Event title" }
                    calendar := mod.widgets.SSelect {}
                    covered := mod.widgets.SField { text: "The menu covers this field" }
                }
            });
            WidgetRef::script_from_value(vm, value)
        });
        makepad_widgets::widget_tree::set_ui_root(&mut cx, &root);
        let select = root.widget(&cx, ids!(calendar)).as_select();
        host.borrow_mut().root = root;
        let pass = DrawPass::new(&mut cx);
        pass.set_size(&mut cx, dvec2(340.0, 420.0));
        let list = DrawList::new(&mut cx);
        Self {
            cx,
            host,
            select,
            pass,
            list,
            frame: 0,
        }
    }

    fn options(&mut self, options: Arc<[SelectOption]>, selected: &str) {
        self.select.set_options(
            &mut self.cx,
            "Calendar",
            options,
            selected,
            "Choose a calendar",
        );
    }

    fn draw(&mut self) {
        self.frame += 1;
        self.cx.new_draw_event = DrawEvent::default();
        let host = self.host.borrow();
        host.props.hits.clear();
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
            let bounds = Rect {
                pos: DVec2::default(),
                size: dvec2(340.0, 420.0),
            };
            let mut data = ();
            let mut scope = Scope::with_data_props(&mut data, &host.props);
            cx.begin_root_turtle(bounds.size, Layout::default());
            host.root.draw_all(&mut cx, &mut scope);
            draw_open(
                &mut cx,
                &mut scope,
                bounds,
                &host.props,
                std::slice::from_ref(&self.select),
            );
            cx.end_pass_sized_turtle();
        }
        self.list.end(&mut draw);
        draw.end_pass(&self.pass);
    }

    fn settle_focus(&mut self) {
        // Custom dispatch runs Makepad's normal post-event focus transition
        // without attempting to activate or create an operating-system window.
        self.cx.dispatch_studio_msg(
            StudioToApp::Custom("select-test-focus".into()),
            CxWindowPool::id_zero(),
            DVec2::default(),
        );
    }

    fn send(&mut self, event: Event) -> Vec<String> {
        if matches!(event, Event::MouseDown(_)) {
            self.cx
                .fingers
                .mouse_down(MouseButton::PRIMARY, CxWindowPool::id_zero());
        }
        let actions = self
            .cx
            .capture_actions(|cx| self.host.borrow().event(cx, &event));
        if matches!(event, Event::MouseUp(_)) {
            self.cx.fingers.mouse_up(MouseButton::PRIMARY);
        }
        self.settle_focus();
        actions
            .filter_widget_actions(self.select.widget_uid())
            .filter_map(|action| match action.cast::<SelectAction>() {
                SelectAction::Changed(value) => Some(value),
                SelectAction::None => None,
            })
            .collect()
    }

    fn key(&mut self, code: KeyCode) -> Vec<String> {
        self.send(Event::KeyDown(KeyEvent {
            key_code: code,
            modifiers: Default::default(),
            is_repeat: false,
            time: 0.0,
        }))
    }

    fn mouse(&mut self, at: DVec2, down: bool) -> Vec<String> {
        self.send(if down {
            Event::MouseDown(MouseDownEvent {
                abs: at,
                window_id: CxWindowPool::id_zero(),
                button: MouseButton::PRIMARY,
                modifiers: Default::default(),
                handled: Cell::new(Area::Empty),
                time: 0.0,
            })
        } else {
            Event::MouseUp(MouseUpEvent {
                abs: at,
                window_id: CxWindowPool::id_zero(),
                button: MouseButton::PRIMARY,
                modifiers: Default::default(),
                time: 0.0,
            })
        })
    }

    fn face_point(&self) -> DVec2 {
        let face = self.select.borrow().unwrap().face(&self.cx);
        let rect = form::drawn_rect(&self.cx, face.area()).expect("the select face was drawn");
        rect.pos + rect.size * 0.5
    }

    fn option_point(&self, label: &str) -> DVec2 {
        let hit = self
            .host
            .borrow()
            .props
            .hits
            .by_label(&format!("Calendar: {label}"))
            .expect("the option was drawn and registered after the form");
        hit.rect.pos + hit.rect.size * 0.5
    }

    fn open_with_pointer(&mut self) {
        let at = self.face_point();
        assert!(self.mouse(at, true).is_empty());
        assert!(self.select.borrow().unwrap().choices.open);
        assert!(
            self.select.key_focus(&self.cx),
            "opening takes focus from the old text field"
        );
        self.draw();
        assert!(self.mouse(at, false).is_empty());
        assert!(
            self.select.borrow().unwrap().choices.open,
            "opening release must not choose an option"
        );
        assert!(
            self.select.key_focus(&self.cx),
            "outside-release text handlers must not clear select focus"
        );
        assert!(!self
            .cx
            .fingers
            .is_area_captured(self.select.borrow().unwrap().face(&self.cx).area()));
    }
}

fn options() -> Arc<[SelectOption]> {
    Arc::from([
        SelectOption::new("work-id", "Work"),
        SelectOption::new("personal-id", "Personal"),
        SelectOption::new("team-id", "Team"),
    ])
}

#[test]
fn select_pointer_and_keyboard_preserve_focus_and_commit_only_explicit_choices() {
    let mut form = Form::new();
    form.options(options(), "work-id");
    form.draw();
    form.host
        .borrow()
        .root
        .widget(&form.cx, ids!(title))
        .set_key_focus(&mut form.cx);
    form.settle_focus();
    form.open_with_pointer();

    let personal = form.option_point("Personal");
    assert!(form.mouse(personal, true).is_empty());
    form.draw();
    assert_eq!(form.mouse(personal, false), ["personal-id"]);
    assert!(!form.select.borrow().unwrap().choices.open);
    assert!(form.select.key_focus(&form.cx));
    form.options(options(), "personal-id");
    assert_eq!(form.select.text(), "Personal");

    assert!(form.key(KeyCode::Space).is_empty());
    assert!(form.key(KeyCode::ArrowDown).is_empty());
    assert_eq!(
        form.select.borrow().unwrap().choices.highlighted.as_deref(),
        Some("team-id")
    );
    assert!(form.key(KeyCode::Escape).is_empty());
    assert!(!form.select.borrow().unwrap().choices.open);
    assert_eq!(
        form.select.borrow().unwrap().choices.selected,
        "personal-id"
    );
    assert!(form.select.key_focus(&form.cx));

    form.open_with_pointer();
    let other = form.host.borrow().root.widget(&form.cx, ids!(title));
    let rect = form::drawn_rect(&form.cx, other.area()).unwrap();
    let outside = rect.pos + rect.size * 0.5;
    assert!(form.mouse(outside, true).is_empty());
    assert!(!form.select.borrow().unwrap().choices.open);
    // The shell broadcasts pointer events: another panel can take focus
    // after this menu dismisses on the same outside press.
    other.set_key_focus(&mut form.cx);
    form.settle_focus();
    assert!(form.mouse(outside, false).is_empty());
    assert!(other.key_focus(&form.cx), "dismissal release must not steal another panel's focus");

    form.select.set_disabled(&mut form.cx, true);
    assert!(form.key(KeyCode::ReturnKey).is_empty());
    let at = form.face_point();
    assert!(form.mouse(at, true).is_empty());
    assert!(form.mouse(at, false).is_empty());
    assert!(!form.select.borrow().unwrap().choices.open);
    assert_eq!(
        form.select.borrow().unwrap().choices.selected,
        "personal-id"
    );
}

#[test]
fn refreshed_select_options_cannot_commit_the_new_occupant_of_a_pressed_row() {
    let mut form = Form::new();
    form.options(options(), "work-id");
    form.draw();
    form.open_with_pointer();
    let personal = form.option_point("Personal");
    assert!(form.mouse(personal, true).is_empty());
    form.options(
        Arc::from([
            SelectOption::new("personal-id", "Personal"),
            SelectOption::new("work-id", "Work"),
            SelectOption::new("team-id", "Team"),
        ]),
        "work-id",
    );
    form.draw();
    assert!(
        form.mouse(personal, false).is_empty(),
        "refresh must not select Work under the old Personal press"
    );
    assert_eq!(form.select.borrow().unwrap().choices.selected, "work-id");

    let personal = form.option_point("Personal");
    assert!(form.mouse(personal, true).is_empty());
    assert_eq!(form.mouse(personal, false), ["personal-id"]);

    form.options(options(), "work-id");
    form.draw();
    form.open_with_pointer();
    let team = form.option_point("Team");
    assert!(form.mouse(team, true).is_empty());
    form.options(Arc::from([SelectOption::new("work-id", "Work")]), "work-id");
    assert!(
        form.mouse(team, false).is_empty(),
        "removed choices cannot be picked before the next draw"
    );
    assert_eq!(form.select.borrow().unwrap().choices.selected, "work-id");
}

#[test]
fn select_end_reveals_a_long_lists_final_option_before_committing() {
    let mut form = Form::new();
    form.options(
        (0..80)
            .map(|index| {
                SelectOption::new(format!("calendar-{index}"), format!("Calendar {index}"))
            })
            .collect(),
        "calendar-0",
    );
    form.draw();
    form.open_with_pointer();
    assert!(form.key(KeyCode::End).is_empty());
    for _ in 0..3 {
        form.draw();
    }
    assert_eq!(
        form.select.borrow().unwrap().choices.highlighted.as_deref(),
        Some("calendar-79")
    );
    let last = form
        .host
        .borrow()
        .props
        .hits
        .by_label("Calendar: Calendar 79")
        .expect("End must reveal the final option in the scrolling menu");
    assert!(
        last.rect.size.y >= 31.0,
        "the highlighted option should be fully visible: {:?}",
        last.rect
    );
    {
        let select = form.select.borrow().unwrap();
        assert!(
            select.rows.len() <= 10,
            "the 288px viewport should expose at most ten 32px choices: {:?}",
            select.rows
        );
    }
    assert_eq!(form.key(KeyCode::ReturnKey), ["calendar-79"]);
    assert!(!form.select.borrow().unwrap().choices.open);
}

#[test]
fn select_scrollbar_drag_scrolls_without_picking_the_row_underneath() {
    let mut form = Form::new();
    form.options(
        (0..80)
            .map(|index| {
                SelectOption::new(format!("calendar-{index}"), format!("Calendar {index}"))
            })
            .collect(),
        "calendar-0",
    );
    form.draw();
    form.open_with_pointer();
    form.draw();
    let list = form
        .select
        .borrow()
        .unwrap()
        .menu
        .portal_list(&form.cx, ids!(list));
    let rect = form::drawn_rect(&form.cx, list.area()).unwrap();
    let first = list.borrow().unwrap().first_id();
    // The drawn vertical track is ten pixels wide; start inside its top
    // handle, then drag far enough to move through several calendar choices.
    let from = dvec2(rect.pos.x + rect.size.x - 5.0, rect.pos.y + 14.0);
    let to = from + dvec2(0.0, 160.0);
    assert!(form.mouse(from, true).is_empty());
    assert!(
        form.cx.fingers.any_areas_captured(),
        "the scrollbar owns the press"
    );
    assert!(
        form.select.borrow().unwrap().pressed.is_none(),
        "scrollbar presses cannot arm a choice"
    );
    assert!(form
        .send(Event::MouseMove(MouseMoveEvent {
            abs: to,
            lock_delta: Default::default(),
            window_id: CxWindowPool::id_zero(),
            modifiers: Default::default(),
            time: 0.1,
            handled: Cell::new(Area::Empty),
        }))
        .is_empty());
    form.draw();
    assert!(form.mouse(to, false).is_empty());
    form.draw();
    assert!(
        list.borrow().unwrap().first_id() > first,
        "drag must scroll the menu: {}",
        list.debug_scroll_state_line()
    );
    assert!(
        !form.cx.fingers.any_areas_captured(),
        "release clears the scrollbar capture"
    );
    assert!(form.select.borrow().unwrap().choices.open);
    assert_eq!(form.select.borrow().unwrap().choices.selected, "calendar-0");
    assert!(form.select.key_focus(&form.cx));
}
