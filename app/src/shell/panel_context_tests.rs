//! Context actions keep the long-pressed panel, and Back unwinds transient
//! surfaces before it changes workspace history.

use super::*;
use crate::shell::anim::Anim;
use crate::shell::test_support::{panel, TEST_APP};
use kernel::app::{world_for, App, Apps, Env, Mode, Workers};
use kernel::caps::{Clipboard, ClockSource, FakeClipboard};
use kernel::launcher;
use kernel::session::Session;
use kernel::store::Store;
use std::rc::Rc;
use std::time::{Duration, Instant};
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
static APPS: &[&dyn App] = &[&TEST_APP, &CONTEXT_TAKER];

fn workspace() -> (Cx, Stage, Shell, SlotId, SlotId) {
    workspace_over(Session::fake(APPS))
}

/// A session whose undo is a walk in flight rather than an inline step:
/// a world with a factory, its store attached to a UI. What the real app
/// has, and what a chord that arrives mid-undo is up against.
fn attached_session() -> Session {
    let apps = Apps::new(APPS);
    let store = Store::open(
        None,
        &apps.schemas(),
        kernel::sync::Device::fake().replicating(apps.replicated()),
    )
    .unwrap();
    let world = Rc::new(world_for(APPS, store, Mode::Fake, &Env::default()));
    let workers = Workers::none(world.store().clone());
    Session::new(apps, world, workers)
}

fn workspace_over(session: Session) -> (Cx, Stage, Shell, SlotId, SlotId) {
    let mut cx = Cx::new(Box::new(|_, _| {}));
    let mut stage = cx.with_vm(|vm| {
        makepad_widgets::script_mod(vm);
        crate::shell::script_mod(vm);
        let value = script_eval!(vm, { mod.widgets.Stage {} });
        Stage::script_from_value(vm, value)
    });
    let mut sh = Shell {
        session,
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
    stage.open_root(&mut sh, panel("first"));
    sh.session.settle();
    let first = sh.session.focus().unwrap();
    stage.open_root(&mut sh, panel("second"));
    sh.session.settle();
    let second = sh.session.focus().unwrap();
    (cx, stage, sh, first, second)
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
    let (mut cx, mut stage, mut sh, first, second) = workspace();
    sh.overlay = Overlay::PanelContext(first);
    assert_eq!(sh.session.focus(), Some(second));
    let before = sh.session.history().head();
    stage.resolve(&mut cx, &mut sh, Act::PanelToggleTabs(first), false);
    sh.session.settle();
    assert_eq!(sh.overlay, Overlay::None);
    assert_eq!(sh.session.focus(), Some(first));
    assert!(is_tabbed(&sh, first));
    assert!(!is_tabbed(&sh, second));
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
    assert!(!is_tabbed(&sh, first));
}

/// The unjoin row is offered only to a panel in a join, names the bridge it
/// would break, acts on the long-pressed panel rather than the focused one,
/// closes nothing, and is one undo step.
#[test]
fn context_unjoin_breaks_the_long_pressed_panels_bridge_and_undo_restores_it() {
    use crate::shell::panel_context::context_rows;
    let (mut cx, mut stage, mut sh, first, second) = workspace();
    let rows = context_rows(&sh, first);
    assert_eq!(rows.len(), 4, "a panel in no join has nothing to unjoin");
    assert!(rows.iter().all(|r| r.act != Act::PanelUnjoin(first)));

    sh.session.nav(Nav::Open {
        from: first,
        id: panel("third"),
        fresh: false,
    });
    sh.session.settle();
    let third = sh.session.focus().unwrap();
    assert_eq!(sh.session.joined_child(first), Some(third));
    sh.session.nav(Nav::Focus(second));
    sh.session.settle();

    // Offered before the close, so the row that takes the panel away stays
    // last.
    let rows = context_rows(&sh, third);
    assert_eq!(rows.len(), 5);
    assert_eq!(rows[3].act, Act::PanelUnjoin(third));
    assert_eq!(rows[3].label, "unjoin panel");
    assert_eq!(rows[3].detail, "“first” ═ “third”");
    assert_eq!(rows[4].act, Act::PanelClose(third));

    let before = sh.session.history().head();
    sh.overlay = Overlay::PanelContext(third);
    stage.resolve(&mut cx, &mut sh, Act::PanelUnjoin(third), false);
    sh.session.settle();
    assert_eq!(sh.overlay, Overlay::None);
    assert_eq!(sh.session.focus(), Some(third));
    assert_eq!(sh.session.joined_child(first), None);
    assert!(sh.session.panel(third).is_some(), "the panel stays, on its own");
    assert_ne!(sh.session.history().head(), before);

    assert!(sh.session.undo());
    sh.session.settle();
    assert_eq!(sh.session.history().head(), before);
    assert_eq!(sh.session.joined_child(first), Some(third));
}

/// An unjoin asked while an undo is in flight reads the layout the undo
/// restores, not the one on screen: here the walk is bringing the bridge
/// back, and a chord that read the stale layout would have said *not
/// joined* and dropped the ask.
#[test]
fn an_unjoin_asked_during_an_undo_reads_the_layout_the_undo_restores() {
    let (_cx, mut stage, mut sh, first, _) = workspace_over(attached_session());
    sh.session.nav(Nav::Open {
        from: first,
        id: panel("third"),
        fresh: false,
    });
    sh.session.settle();
    let third = sh.session.focus().unwrap();
    stage.unjoin_slot(&mut sh, third);
    sh.session.settle();
    assert_eq!(sh.session.joined_child(first), None);
    let unjoined = sh.session.history().head();

    let (wake, woke) = std::sync::mpsc::channel();
    sh.session.store().attach_ui(move || {
        let _ = wake.send(());
    });
    assert!(sh.session.undo());
    assert!(sh.session.history_busy(), "the undo is a walk in flight");
    assert_eq!(sh.session.joined_child(first), None, "on screen the bridge is still gone");
    stage.unjoin_slot(&mut sh, third);
    assert!(
        !sh.session.notes().iter().any(|n| n.msg.contains("not joined")),
        "the stale layout is not what the chord reads"
    );

    // The UI's loop: a wake, the store's completions, a settle. The walk
    // lands on one wake; the save of the layout it restored completes on a
    // later one, and only then do the queued commands run.
    let deadline = Instant::now() + Duration::from_secs(5);
    while sh.session.history_busy() || sh.session.joined_child(first).is_some() {
        assert!(Instant::now() < deadline, "the undo must land and the queued unjoin run");
        woke.recv_timeout(Duration::from_secs(5)).unwrap();
        sh.session.store().poll_external();
        sh.session.settle();
    }
    // The undo put the bridge back, and the queued unjoin then took it away
    // again as a node of its own past the one the undo stepped over.
    assert_eq!(sh.session.joined_child(first), None);
    let head = sh.session.history().head();
    assert_ne!(head, unjoined);
    let (rows, _) = sh.session.history().rows();
    let node = rows.iter().find(|r| r.id == head).unwrap();
    assert_eq!((node.kind.as_str(), node.label.as_str()), ("unjoin", "unjoin “third”"));
    assert!(sh.session.panel(third).is_some());
    sh.session.shutdown();
}

#[test]
fn context_agent_receives_the_long_pressed_panel_after_focus_changes() {
    let (mut cx, mut stage, mut sh, first, second) = workspace();
    sh.overlay = Overlay::PanelContext(first);
    assert_eq!(sh.session.focus(), Some(second));
    stage.resolve(&mut cx, &mut sh, Act::PanelAsk(first), false);
    assert_eq!(sh.overlay, Overlay::None);
    assert_eq!(sh.session.focus(), Some(first));
    assert!(sh
        .session
        .notes()
        .iter()
        .any(|note| note.msg == format!("asked about {first}")));
}

#[test]
fn a_dismissed_context_menu_cannot_repeat_its_action_before_the_next_draw() {
    let (mut cx, mut stage, mut sh, first, _) = workspace();
    sh.overlay = Overlay::PanelContext(first);
    stage.resolve(&mut cx, &mut sh, Act::PanelToggleTabs(first), false);
    sh.session.settle();
    assert!(is_tabbed(&sh, first));
    let before = sh.session.history().head();
    sh.overlay = Overlay::Overview;
    stage.resolve(&mut cx, &mut sh, Act::PanelToggleTabs(first), false);
    assert_eq!(sh.overlay, Overlay::Overview);
    assert!(is_tabbed(&sh, first));
    assert_eq!(sh.session.history().head(), before);
}

#[test]
fn context_copy_uses_the_long_pressed_panel_and_creates_no_history_step() {
    let (mut cx, mut stage, mut sh, first, second) = workspace();
    let clipboard = FakeClipboard::new();
    sh.session.world().caps(|caps| {
        caps.insert::<dyn Clipboard>(Box::new(clipboard.clone()));
    });
    sh.overlay = Overlay::PanelContext(first);
    assert_eq!(sh.session.focus(), Some(second));
    let before = sh.session.history().head();
    stage.resolve(&mut cx, &mut sh, Act::PanelCopyContext(first), false);
    sh.session.settle();
    assert_eq!(sh.overlay, Overlay::None);
    assert_eq!(sh.session.focus(), Some(first));
    assert_eq!(sh.session.history().head(), before);
    let copies = clipboard.taken();
    assert_eq!(copies.len(), 1);
    assert!(copies[0].starts_with(&kernel::context::header_line(&panel("first"))));
}

#[test]
fn the_menu_names_its_actions_and_says_which_way_the_column_goes() {
    use crate::shell::panel_context::context_rows;
    let (_cx, mut stage, mut sh, first, _) = workspace();
    let rows = context_rows(&sh, first);
    let labels: Vec<&str> = rows.iter().map(|r| r.label.as_str()).collect();
    assert_eq!(
        labels,
        [
            "start agent with panel context",
            "copy panel context",
            "switch column tab mode",
            "close panel",
        ]
    );
    let acts: Vec<Act> = rows.iter().map(|r| r.act.clone()).collect();
    assert_eq!(
        acts,
        [
            Act::PanelAsk(first),
            Act::PanelCopyContext(first),
            Act::PanelToggleTabs(first),
            Act::PanelClose(first),
        ]
    );
    // Only the column switch says what it would do, and it says it for
    // the column as it stands.
    assert_eq!(rows[2].detail, "show panels as tabs");
    assert!(rows.iter().enumerate().all(|(i, r)| i == 2 || r.detail.is_empty()));
    stage.toggle_column_tabs(&mut sh, first);
    sh.session.settle();
    assert!(is_tabbed(&sh, first));
    assert_eq!(context_rows(&sh, first)[2].detail, "show stacked panels");
}

#[test]
fn context_close_takes_the_pressed_panel_and_the_menu_with_it() {
    let (mut cx, mut stage, mut sh, first, second) = workspace();
    sh.overlay = Overlay::PanelContext(first);
    assert_eq!(sh.session.focus(), Some(second));
    stage.resolve(&mut cx, &mut sh, Act::PanelClose(first), false);
    sh.session.settle();
    assert_eq!(sh.overlay, Overlay::None);
    assert!(sh.session.panel(first).is_none(), "the pressed panel closes");
    assert!(sh.session.panel(second).is_some(), "the focused one stays");

    // A hit left over from a dismissed menu closes nothing.
    sh.overlay = Overlay::Overview;
    stage.resolve(&mut cx, &mut sh, Act::PanelClose(second), false);
    sh.session.settle();
    assert!(sh.session.panel(second).is_some());
    assert_eq!(sh.overlay, Overlay::Overview);
}

#[test]
fn the_sheet_hangs_from_the_header_and_stays_on_the_screen() {
    use crate::shell::panel_context::{context_layout, context_rows, MAX_W, MIN_W};
    let (_cx, _stage, sh, first, _) = workspace();
    let rows = context_rows(&sh, first);
    let phone = rect(0.0, 0.0, 380.0, 780.0);

    // From a header at the top of a phone: the sheet's head is that
    // header, and the rows follow it, each a finger's target.
    let head = rect(8.0, 8.0, 364.0, theme::HEAD_H);
    let lay = context_layout(phone, Some(head), &rows);
    assert_eq!(lay.head, head);
    assert_eq!(lay.sheet.pos, head.pos);
    assert_eq!(lay.sheet.size.x, head.size.x);
    assert_eq!(lay.rows.len(), rows.len());
    assert!(lay.rows.iter().all(|r| r.size.y >= 44.0));
    let mut y = head.pos.y + theme::HEAD_H;
    for r in &lay.rows {
        assert_eq!(r.pos.y, y, "rows are contiguous");
        assert_eq!(r.pos.x, head.pos.x + 1.0, "inside the border");
        y += r.size.y;
    }
    assert_eq!(lay.sheet.pos.y + lay.sheet.size.y, y + 1.0);

    // A header near the foot of the screen: lifted until the sheet fits.
    let low = rect(8.0, 740.0, 364.0, theme::HEAD_H);
    let lay = context_layout(phone, Some(low), &rows);
    assert!(lay.sheet.pos.y + lay.sheet.size.y <= phone.size.y);
    assert_eq!(lay.sheet.pos.x, 8.0);

    // A panel across a desktop does not make a menu across a desktop,
    // and a narrow one at the far edge still holds its labels, on screen.
    let desktop = rect(0.0, 0.0, 1440.0, 900.0);
    let wide = rect(8.0, 8.0, 1424.0, theme::HEAD_H);
    assert_eq!(context_layout(desktop, Some(wide), &rows).sheet.size.x, MAX_W);
    let narrow = rect(1300.0, 8.0, 132.0, theme::HEAD_H);
    let lay = context_layout(desktop, Some(narrow), &rows);
    assert_eq!(lay.sheet.size.x, MIN_W);
    assert!(lay.sheet.pos.x + lay.sheet.size.x <= desktop.size.x);

    // Nothing to hang from: centred, as the other sheets are.
    let lay = context_layout(phone, None, &rows);
    let mid = lay.sheet.pos.x + lay.sheet.size.x / 2.0;
    assert!((mid - phone.size.x / 2.0).abs() < 1.0);
}

#[test]
fn back_closes_context_menu_without_undoing_and_respects_consumed_events() {
    let (mut cx, mut stage, mut sh, first, _) = workspace();
    let before = sh.session.history().head();
    sh.overlay = Overlay::PanelContext(first);
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
    let (mut cx, mut stage, mut sh, first, _) = workspace();
    let bounds = rect(0.0, 0.0, 400.0, 300.0);
    let point = dvec2(380.0, 10.0);
    stage.hits.push(Hit::act(
        "first",
        bounds,
        MouseCursor::Default,
        Act::Focus(first),
    ));
    stage.hits.push(Hit::act(
        "close",
        rect(370.0, 0.0, 30.0, theme::HEAD_H),
        MouseCursor::Hand,
        Act::Close(first),
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
    assert_eq!(sh.overlay, Overlay::PanelContext(first));
    assert_eq!(sh.session.focus(), Some(first));
    stage.handle_with(
        &mut cx,
        &mut sh,
        &native_touch(&[(1, point, TouchState::Stop)]),
    );
    assert_eq!(sh.overlay, Overlay::PanelContext(first));
    assert!(sh.session.panel(first).is_some());
    assert_eq!(sh.session.history().head(), before);
}

#[test]
fn native_overview_swipe_preempts_raw_touch_owner_and_releases_its_contacts() {
    let (mut cx, mut stage, mut sh, first, _) = workspace();
    let content = cx.with_vm(|vm| WidgetRef::new_with_inner(Box::new(TouchOwner::script_new(vm))));
    stage.hosted.insert(first, content.clone());
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

/// The soft keyboard's *go* is the launcher's enter: it takes the selected
/// hit, and the launcher comes down with the keyboard.
#[test]
fn the_keyboards_go_takes_the_launchers_selected_hit() {
    use makepad_widgets::makepad_platform::event::{ImeAction, ImeActionEvent};
    let (mut cx, mut stage, mut sh, first, second) = workspace();
    assert_eq!(sh.session.focus(), Some(second));
    stage.open_launcher(&mut cx, &mut sh);
    stage.settle(&mut cx, &mut sh);
    assert!(stage.launcher_up);
    let (windows, roots) = (sh.session.windows(), sh.session.roots());
    sh.launcher.ask(&windows, &roots, "first");
    stage.handle_with(
        &mut cx,
        &mut sh,
        &Event::ImeAction(ImeActionEvent {
            action: ImeAction::Go,
        }),
    );
    stage.settle(&mut cx, &mut sh);
    assert_eq!(sh.overlay, Overlay::None);
    assert_eq!(sh.session.focus(), Some(first));
    assert!(!stage.launcher_up, "the launcher's coming down was seen");
}

/// A soft keyboard put away under the launcher stays away: the query keeps
/// the caret but stops asking for the keyboard, until the launcher is
/// raised again or the keyboard comes back on its own.
#[test]
fn a_keyboard_put_away_under_the_launcher_stays_away_until_it_is_raised_again() {
    let (mut cx, mut stage, mut sh, _, _) = workspace();
    let hide = Event::VirtualKeyboard(VirtualKeyboardEvent::DidHide { time: 1.0 });
    let show = Event::VirtualKeyboard(VirtualKeyboardEvent::DidShow {
        height: 300.0,
        time: 2.0,
    });
    // With no launcher up, a keyboard going down is nobody's dismissal.
    stage.handle_with(&mut cx, &mut sh, &hide);
    assert!(!stage.kb_dismissed);

    stage.open_launcher(&mut cx, &mut sh);
    stage.handle_with(&mut cx, &mut sh, &show);
    assert!(!stage.kb_dismissed);
    stage.handle_with(&mut cx, &mut sh, &hide);
    assert!(stage.kb_dismissed, "put away under the launcher");
    assert_eq!(sh.overlay, Overlay::Launcher, "the launcher stays up");

    // Tapping the field raises it afresh, as does raising the launcher.
    stage.open_launcher(&mut cx, &mut sh);
    assert!(!stage.kb_dismissed);
    stage.handle_with(&mut cx, &mut sh, &hide);
    assert!(stage.kb_dismissed);
    stage.handle_with(&mut cx, &mut sh, &show);
    assert!(!stage.kb_dismissed, "back on its own");

    // A floating keyboard is up at no height at all: not a dismissal.
    let floating = Event::VirtualKeyboard(VirtualKeyboardEvent::DidShow {
        height: 0.0,
        time: 3.0,
    });
    stage.handle_with(&mut cx, &mut sh, &floating);
    assert!(!stage.kb_dismissed);
    assert_eq!(stage.kb_h, 0.0);
}
