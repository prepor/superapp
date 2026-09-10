//! Context actions keep the long-pressed panel, and Back unwinds transient
//! surfaces before it changes workspace history.

use super::*;
use crate::shell::anim::Anim;
use crate::shell::system::{About, Help, SYSTEM};
use kernel::app::App;
use kernel::caps::{Clipboard, ClockSource, FakeClipboard};
use kernel::launcher;
use kernel::session::Session;
use makepad_widgets::makepad_platform::event::{
    LongPressEvent, TouchPoint, TouchState, TouchUpdateEvent,
};
use std::collections::BTreeSet;

#[derive(Script, ScriptHook, Widget)]
struct TouchOwner {
    #[uid]
    uid: WidgetUid,
    #[walk]
    walk: Walk,
    #[redraw]
    #[rust]
    area: Area,
    #[rust]
    contacts: BTreeSet<u64>,
    #[rust]
    updates: usize,
}

impl Widget for TouchOwner {
    fn handle_event(&mut self, _cx: &mut Cx, event: &Event, scope: &mut Scope) {
        if let Event::TouchUpdate(event) = event {
            self.updates += 1;
            for point in &event.touches {
                match point.state {
                    TouchState::Start => {
                        self.contacts.insert(point.uid);
                    }
                    TouchState::Stop => {
                        self.contacts.remove(&point.uid);
                    }
                    _ => {}
                }
            }
            scope
                .props
                .get::<crate::shell::hosted::PanelProps>()
                .unwrap()
                .grab
                .claim_touch();
        }
    }

    fn draw_walk(&mut self, _cx: &mut Cx2d, _scope: &mut Scope, _walk: Walk) -> DrawStep {
        DrawStep::done()
    }
}

fn native_touch(points: &[(u64, DVec2, TouchState)]) -> Event {
    Event::TouchUpdate(TouchUpdateEvent {
        time: 1.0,
        window_id: CxWindowPool::id_zero(),
        modifiers: KeyModifiers::default(),
        touches: points
            .iter()
            .map(|&(uid, abs, state)| TouchPoint {
                uid,
                abs,
                state,
                time: 1.0,
                rotation_angle: 0.0,
                force: 1.0,
                radius: DVec2::default(),
                handled: std::cell::Cell::new(Area::Empty),
                sweep_lock: std::cell::Cell::new(Area::Empty),
            })
            .collect(),
    })
}

struct ContextTaker;

impl App for ContextTaker {
    fn id(&self) -> &'static str {
        "context-taker"
    }
    fn kinds(&self) -> &'static [&'static dyn kernel::panel::PanelKind] {
        &[]
    }
    fn ask(&self, session: &mut Session, about: SlotId) -> bool {
        session.notify(format!("asked about {about}"), false);
        true
    }
    fn as_any(&self) -> &dyn std::any::Any {
        self
    }
}

static CONTEXT_TAKER: ContextTaker = ContextTaker;
static APPS: &[&dyn App] = &[&SYSTEM, &CONTEXT_TAKER];

fn workspace() -> (Cx, Stage, Shell, SlotId, SlotId) {
    let mut cx = Cx::new(Box::new(|_, _| {}));
    let mut stage = cx.with_vm(|vm| {
        makepad_widgets::script_mod(vm);
        crate::shell::script_mod(vm);
        let value = script_eval!(vm, { mod.widgets.Stage {} });
        Stage::script_from_value(vm, value)
    });
    let mut sh = Shell {
        session: Session::fake(APPS),
        anim: Anim::default(),
        viewport: dvec2(400.0, 800.0),
        last_frame: None,
        hover: None,
        toasts: Vec::new(),
        overlay: Overlay::None,
        overlay_last: Overlay::None,
        launcher: launcher::Search::new(),
        clock: ClockSource::virtual_from(0.0),
        virtual_time: true,
        grid: None,
    };
    stage.open_root(&mut sh, Help::id());
    sh.session.settle();
    let help = sh.session.focus().unwrap();
    stage.open_root(&mut sh, About::id());
    sh.session.settle();
    let about = sh.session.focus().unwrap();
    (cx, stage, sh, help, about)
}

fn is_tabbed(sh: &Shell, slot: SlotId) -> bool {
    let wm = sh.session.ws();
    let ws = &wm.wss[wm.ws_of(slot).unwrap()];
    ws.columns[ws.locate(slot).unwrap().0].tabbed
}

fn back(cx: &mut Cx, stage: &mut Stage, sh: &mut Shell) {
    let handled = std::cell::Cell::new(false);
    let event = Event::BackPressed { handled };
    stage.handle_with(cx, sh, &event);
    assert!(matches!(event, Event::BackPressed { handled } if handled.get()));
    sh.session.settle();
}

#[test]
fn panel_context_targets_its_slot_after_focus_changes_and_back_undoes_it() {
    let (mut cx, mut stage, mut sh, help, about) = workspace();
    sh.overlay = Overlay::PanelContext(help);
    assert_eq!(sh.session.focus(), Some(about));
    let before = sh.session.history().head();
    stage.resolve(&mut cx, &mut sh, Act::PanelToggleTabs(help), false);
    sh.session.settle();
    assert_eq!(sh.overlay, Overlay::None);
    assert_eq!(sh.session.focus(), Some(help));
    assert!(is_tabbed(&sh, help));
    assert!(!is_tabbed(&sh, about));
    assert_ne!(sh.session.history().head(), before);

    let after = sh.session.history().head();
    sh.overlay = Overlay::Overview;
    back(&mut cx, &mut stage, &mut sh);
    assert_eq!(sh.overlay, Overlay::None);
    assert_eq!(
        sh.session.history().head(),
        after,
        "dismissing overview must preserve the completed action"
    );
    back(&mut cx, &mut stage, &mut sh);
    assert_eq!(sh.session.history().head(), before);
    assert!(!is_tabbed(&sh, help));
}

#[test]
fn context_agent_receives_the_long_pressed_panel_after_focus_changes() {
    let (mut cx, mut stage, mut sh, help, about) = workspace();
    sh.overlay = Overlay::PanelContext(help);
    assert_eq!(sh.session.focus(), Some(about));
    stage.resolve(&mut cx, &mut sh, Act::PanelAsk(help), false);
    assert_eq!(sh.overlay, Overlay::None);
    assert_eq!(sh.session.focus(), Some(help));
    assert!(sh
        .session
        .notes()
        .iter()
        .any(|note| note.msg == format!("asked about {help}")));
}

#[test]
fn a_dismissed_context_menu_cannot_repeat_its_action_before_the_next_draw() {
    let (mut cx, mut stage, mut sh, help, _) = workspace();
    sh.overlay = Overlay::PanelContext(help);
    stage.resolve(&mut cx, &mut sh, Act::PanelToggleTabs(help), false);
    sh.session.settle();
    assert!(is_tabbed(&sh, help));
    let before = sh.session.history().head();
    sh.overlay = Overlay::Overview;
    stage.resolve(&mut cx, &mut sh, Act::PanelToggleTabs(help), false);
    assert_eq!(sh.overlay, Overlay::Overview);
    assert!(is_tabbed(&sh, help));
    assert_eq!(sh.session.history().head(), before);
}

#[test]
fn context_copy_uses_the_long_pressed_panel_and_creates_no_history_step() {
    let (mut cx, mut stage, mut sh, help, about) = workspace();
    let clipboard = FakeClipboard::new();
    sh.session.world().caps(|caps| {
        caps.insert::<dyn Clipboard>(Box::new(clipboard.clone()));
    });
    sh.overlay = Overlay::PanelContext(help);
    assert_eq!(sh.session.focus(), Some(about));
    let before = sh.session.history().head();
    stage.resolve(&mut cx, &mut sh, Act::PanelCopyContext(help), false);
    sh.session.settle();
    assert_eq!(sh.overlay, Overlay::None);
    assert_eq!(sh.session.focus(), Some(help));
    assert_eq!(sh.session.history().head(), before);
    let copies = clipboard.taken();
    assert_eq!(copies.len(), 1);
    assert!(copies[0].starts_with(&kernel::context::header_line(&Help::id())));
}

#[test]
fn back_closes_context_menu_without_undoing_and_respects_consumed_events() {
    let (mut cx, mut stage, mut sh, help, _) = workspace();
    let before = sh.session.history().head();
    sh.overlay = Overlay::PanelContext(help);
    back(&mut cx, &mut stage, &mut sh);
    assert_eq!(sh.overlay, Overlay::None);
    assert_eq!(sh.session.history().head(), before);

    stage.handle_with(
        &mut cx,
        &mut sh,
        &Event::BackPressed {
            handled: std::cell::Cell::new(true),
        },
    );
    assert_eq!(sh.session.history().head(), before);
}

#[test]
fn native_header_hold_opens_context_and_the_release_cannot_close_the_panel() {
    let (mut cx, mut stage, mut sh, help, _) = workspace();
    let bounds = rect(0.0, 0.0, 400.0, 300.0);
    let point = dvec2(380.0, 10.0);
    stage.hits.push(Hit::act(
        "help",
        bounds,
        MouseCursor::Default,
        Act::Focus(help),
    ));
    stage.hits.push(Hit::act(
        "close",
        rect(370.0, 0.0, 30.0, theme::HEAD_H),
        MouseCursor::Hand,
        Act::Close(help),
    ));
    let before = sh.session.history().head();
    stage.handle_with(
        &mut cx,
        &mut sh,
        &native_touch(&[(1, point, TouchState::Start)]),
    );
    stage.handle_with(
        &mut cx,
        &mut sh,
        &Event::LongPress(LongPressEvent {
            window_id: CxWindowPool::id_zero(),
            uid: 1,
            abs: point,
            time: 1.5,
        }),
    );
    assert_eq!(sh.overlay, Overlay::PanelContext(help));
    assert_eq!(sh.session.focus(), Some(help));
    stage.handle_with(
        &mut cx,
        &mut sh,
        &native_touch(&[(1, point, TouchState::Stop)]),
    );
    assert_eq!(sh.overlay, Overlay::PanelContext(help));
    assert!(sh.session.panel(help).is_some());
    assert_eq!(sh.session.history().head(), before);
}

#[test]
fn native_overview_swipe_preempts_raw_touch_owner_and_releases_its_contacts() {
    let (mut cx, mut stage, mut sh, help, _) = workspace();
    let content = cx.with_vm(|vm| WidgetRef::new_with_inner(Box::new(TouchOwner::script_new(vm))));
    stage.hosted.insert(help, content.clone());
    let a = dvec2(100.0, 300.0);
    let b = dvec2(200.0, 300.0);
    stage.handle_with(
        &mut cx,
        &mut sh,
        &native_touch(&[(1, a, TouchState::Start), (2, b, TouchState::Start)]),
    );
    assert_eq!(content.borrow::<TouchOwner>().unwrap().contacts.len(), 2);
    assert!(
        stage.touch.pts.is_empty(),
        "content claimed the initial touches"
    );
    stage.handle_with(
        &mut cx,
        &mut sh,
        &native_touch(&[
            (1, a - dvec2(0.0, 50.0), TouchState::Move),
            (2, b - dvec2(0.0, 50.0), TouchState::Move),
        ]),
    );
    assert_eq!(sh.overlay, Overlay::Overview);
    assert!(
        content.borrow::<TouchOwner>().unwrap().contacts.is_empty(),
        "the viewer must not retain contacts hidden behind overview"
    );
    let updates = content.borrow::<TouchOwner>().unwrap().updates;
    stage.handle_with(
        &mut cx,
        &mut sh,
        &native_touch(&[
            (1, a - dvec2(0.0, 50.0), TouchState::Stop),
            (2, b - dvec2(0.0, 50.0), TouchState::Stop),
        ]),
    );
    assert_eq!(sh.overlay, Overlay::Overview);
    assert_eq!(
        content.borrow::<TouchOwner>().unwrap().updates,
        updates,
        "the consumed releases must not reenter content"
    );
}
