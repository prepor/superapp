//! Geometry uses all slots, while dwell previews remain outside session history
//! until one release commits the destination and placement together.

use super::*;
use crate::shell::anim::Anim;
use crate::shell::test_support::{panel, TEST_APP};
use kernel::caps::ClockSource;
use kernel::launcher;
use kernel::layout::{Column, LayoutOpts, Slot};
use kernel::panel::{PanelId, Tag};
use kernel::session::Session;
use makepad_widgets::makepad_platform::event::{TouchPoint, TouchState, TouchUpdateEvent};

static APPS: &[&dyn kernel::app::App] = &[&TEST_APP];

fn layout(columns: &[&[SlotId]]) -> Ws {
    let mut ws = Ws::new();
    for ids in columns {
        ws.columns.push(Column {
            slots: ids.to_vec(),
            ..Default::default()
        });
        for &id in *ids {
            ws.slots.insert(
                id,
                Slot {
                    id,
                    show: PanelId::new(Tag("test"), [id.to_string()]),
                },
            );
        }
    }
    ws
}

fn center(r: Rect) -> DVec2 {
    r.pos + r.size / 2.0
}

#[test]
fn tile_drop_previews_before_and_after_and_places_the_same_row() {
    let tiles = Tiles::new(rect(0.0, 0.0, 400.0, 800.0), dvec2(0.0, 0.0));
    let original = layout(&[&[1], &[2, 3]]);
    let tile = tiles.tile(1, 0);
    for (fraction, row) in [(0.25, 0), (0.75, 1)] {
        let p = tile.pos + dvec2(tile.size.x / 2.0, tile.size.y * fraction);
        let (target, bar) = tiles.target(&original, 1, p).unwrap();
        assert_eq!(target, DropTarget::Into { col: 1, row });
        assert!(bar.size.x > bar.size.y);
        assert!(clipped(bar, tiles.body).is_some());
        let mut ws = original.clone();
        ws.place(1, target);
        assert_eq!(ws.locate(1), Some((0, row)));
        assert_eq!(ws.columns[0].slots.len(), 3);
    }
}

#[test]
fn tile_gap_previews_a_new_column_and_lone_source_center_has_no_target() {
    let tiles = Tiles::new(rect(0.0, 0.0, 800.0, 800.0), dvec2(0.0, 0.0));
    let mut ws = layout(&[&[1, 2], &[3], &[4]]);
    let p = dvec2(tiles.tile(1, 0).pos.x - COL_GAP / 2.0, 250.0);
    let (target, bar) = tiles.target(&ws, 4, p).unwrap();
    assert_eq!(target, DropTarget::Boundary { at: 1 });
    assert!(bar.size.y > bar.size.x);
    assert_eq!(center(bar).x, p.x);
    ws.place(4, target);
    assert_eq!(ws.columns[1].slots, vec![4]);
    assert_eq!(ws.columns[0].slots, vec![1, 2]);
    assert_eq!(ws.columns[2].slots, vec![3]);
    assert!(tiles.target(&ws, 4, center(tiles.tile(1, 0))).is_none());
}

#[test]
fn hidden_tabs_have_separate_tiles_and_accept_drops_between_them() {
    let mut ws = layout(&[&[1], &[2, 3, 4]]);
    ws.columns[1].tabbed = true;
    ws.columns[1].active = 0;
    let scene = ws.scene((400.0, 800.0), LayoutOpts::default());
    let hidden = scene.slots.iter().find(|slot| slot.id == 3).unwrap();
    assert!(!hidden.visible);
    assert_eq!(
        hidden.rect,
        scene.slots.iter().find(|slot| slot.id == 2).unwrap().rect
    );

    let tiles = Tiles::new(rect(0.0, 0.0, 400.0, 800.0), dvec2(0.0, 0.0));
    assert!(tiles.tile(1, 1).pos.y >= tiles.tile(1, 0).pos.y + TILE_H);
    let tile = tiles.tile(1, 1);
    let p = tile.pos + dvec2(tile.size.x / 2.0, tile.size.y * 0.75);
    let (target, _) = tiles.target(&ws, 1, p).unwrap();
    assert_eq!(target, DropTarget::Into { col: 1, row: 2 });
    ws.place(1, target);
    assert_eq!(ws.columns[0].slots, vec![2, 3, 1, 4]);
    assert!(ws.columns[0].tabbed);
    assert_eq!(ws.columns[0].active, 2);
}

#[test]
fn scrolling_reaches_last_column_and_row_and_clips_hits_to_body() {
    let ws = layout(&[&[1, 2, 3, 4, 5, 6], &[7], &[8], &[9]]);
    let vp = rect(30.0, 50.0, 400.0, 600.0);
    let initial = Tiles::new(vp, dvec2(0.0, 0.0));
    let limit = initial.limit(&ws);
    assert!(limit.x > 0.0 && limit.y > 0.0);
    let end = Tiles::new(vp, limit);
    let last_column = end.tile(3, 0);
    let last_row = end.tile(0, 5);
    assert_eq!(
        last_column.pos.x + last_column.size.x,
        vp.pos.x + vp.size.x - PAD
    );
    assert_eq!(
        last_row.pos.y + last_row.size.y,
        end.body.pos.y + end.body.size.y
    );

    let scrolled = Tiles::new(vp, dvec2(100.0, 90.0));
    let tile = scrolled.tile(0, 0);
    let hit = clipped(tile, scrolled.body).unwrap();
    assert_eq!(hit.pos, scrolled.body.pos);
    assert!(hit.size.x < tile.size.x && hit.size.y < tile.size.y);
    assert!(scrolled.target(&ws, 9, center(hit)).is_some());
    assert!(scrolled
        .target(&ws, 9, dvec2(vp.pos.x - 1.0, center(hit).y))
        .is_none());
    assert!(scrolled
        .target(&ws, 9, dvec2(center(hit).x, scrolled.body.pos.y - 1.0))
        .is_none());
    assert!(clipped(rect(0.0, 0.0, 5.0, 5.0), scrolled.body).is_none());
}

/// A live join shows between the tiles as it does between the panels: the
/// ═ spans the gap from the parent's tile to its child's, level with the
/// child's title whichever row the parent stands in, and there is none to
/// draw once the tiles are not side by side.
#[test]
fn a_join_bridges_the_gap_from_the_parent_tile_to_its_child() {
    let tiles = Tiles::new(rect(0.0, 0.0, 400.0, 800.0), dvec2(0.0, 0.0));
    let (parent, child) = (tiles.tile(0, 1), tiles.tile(1, 0));
    let bar = bridge(parent, child, 22.0).unwrap();
    assert_eq!(bar.pos.x, parent.pos.x + parent.size.x);
    assert_eq!(bar.size.x, COL_GAP);
    assert_eq!(bar.pos.y + 2.0, child.pos.y + 22.0);
    assert!(
        bar.pos.y < parent.pos.y,
        "level with the child, not the parent"
    );
    // A tile pulled down takes its end of the bridge with it.
    let mut pulled = child;
    pulled.pos.y += 30.0;
    assert_eq!(
        bridge(parent, pulled, 22.0).unwrap().pos.y,
        bar.pos.y + 30.0
    );
    assert!(
        bridge(child, parent, 22.0).is_none(),
        "the parent stands left"
    );
    assert!(bridge(tiles.tile(0, 0), tiles.tile(0, 1), 22.0).is_none());
}

fn workspace() -> (Cx, Stage, Shell, SlotId, SlotId) {
    let mut cx = Cx::new(Box::new(|_, _| {}));
    let stage = cx.with_vm(|vm| {
        makepad_widgets::script_mod(vm);
        crate::shell::script_mod(vm);
        let value = script_eval!(vm, { mod.widgets.Stage {} });
        Stage::script_from_value(vm, value)
    });
    let mut stage = stage;
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
        senses: crate::platform::senses::Senses::new(),
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
    stage.open_overview(&mut cx, &mut sh);
    (cx, stage, sh, first, second)
}

fn pick(stage: &mut Stage, sh: &mut Shell, slot: SlotId) {
    let ws = &sh.session.ws().wss[stage.overview_ws(sh)];
    let (col, row) = ws.locate(slot).unwrap();
    let tiles = Tiles::new(stage.overview_vp(sh), stage.overview.scroll);
    let tile = tiles.tile(col, row);
    stage.overview_pick(sh, 1, slot, center(tile), tile);
}

#[test]
fn overview_scroll_keeps_workspace_and_panel_strips_independent_and_clamped() {
    let (_cx, mut stage, mut sh, _, _) = workspace();
    sh.viewport.x = 300.0;
    stage.overview_scroll(&mut sh, dvec2(200.0, 90.0), dvec2(0.0, 150.0));
    assert_eq!(stage.overview.workspace_scroll, 150.0);
    assert_eq!(stage.overview.scroll, dvec2(0.0, 0.0));

    let limit =
        Tiles::new(stage.overview_vp(&sh), stage.overview.scroll).limit(&sh.session.ws().wss[0]);
    assert!(limit.x > 0.0);
    stage.overview_scroll(&mut sh, dvec2(200.0, 300.0), dvec2(10000.0, 10000.0));
    assert_eq!(stage.overview.scroll, limit);
    assert_eq!(stage.overview.workspace_scroll, 150.0);
    stage.overview_scroll(&mut sh, dvec2(200.0, 300.0), dvec2(-10000.0, -10000.0));
    assert_eq!(stage.overview.scroll, dvec2(0.0, 0.0));
}

#[test]
fn workspace_dwell_resets_when_leaving_and_cancel_keeps_layout_and_history() {
    let (_cx, mut stage, mut sh, first, _) = workspace();
    let before = sh.session.ws().snapshot();
    let history = sh.session.history().head();
    pick(&mut stage, &mut sh, first);
    let destination = center(stage.overview_workspace_rect(stage.overview_vp(&sh), 1));
    stage.overview_drag_to(&mut sh, destination);
    stage.overview_tick(&mut sh, DWELL * 0.75);
    assert_eq!(stage.overview_ws(&sh), 0);
    stage.overview_drag_to(&mut sh, dvec2(200.0, 135.0));
    stage.overview_drag_to(&mut sh, destination);
    stage.overview_tick(&mut sh, DWELL * 0.75);
    assert_eq!(
        stage.overview_ws(&sh),
        0,
        "leaving a tile restarts its dwell"
    );
    stage.overview_tick(&mut sh, DWELL * 0.5);
    assert_eq!(stage.overview_ws(&sh), 1);
    assert_eq!(
        sh.session.ws().snapshot(),
        before,
        "dwell only previews the destination"
    );
    assert_eq!(sh.session.history().head(), history);
    assert!(stage.cancel_overview_drag());
    assert!(!stage.cancel_overview_drag());
    assert_eq!(sh.session.ws().snapshot(), before);
    assert_eq!(sh.session.history().head(), history);
}

#[test]
fn releasing_on_workspace_before_dwell_does_not_move_or_record_history() {
    let (_cx, mut stage, mut sh, first, _) = workspace();
    let before = sh.session.ws().snapshot();
    let history = sh.session.history().head();
    pick(&mut stage, &mut sh, first);
    let destination = center(stage.overview_workspace_rect(stage.overview_vp(&sh), 1));
    stage.overview_drag_to(&mut sh, destination);
    stage.overview_tick(&mut sh, DWELL / 2.0);
    stage.overview_drop(&mut sh, destination);
    sh.session.settle();
    assert_eq!(sh.session.ws().snapshot(), before);
    assert_eq!(sh.session.history().head(), history);
}

#[test]
fn workspace_transfer_and_row_placement_commit_as_one_undoable_action() {
    let (mut cx, mut stage, mut sh, first, second) = workspace();
    sh.session.switch(1);
    stage.open_root(&mut sh, panel("first"));
    sh.session.settle();
    let existing = sh.session.focus().unwrap();
    sh.session.switch(0);
    assert_eq!(sh.session.focus(), Some(second));
    stage.open_overview(&mut cx, &mut sh);
    let before = sh.session.ws().snapshot();
    let history = sh.session.history().head();
    let rows = sh.session.history().rows().0.len();
    pick(&mut stage, &mut sh, first);
    let destination = center(stage.overview_workspace_rect(stage.overview_vp(&sh), 1));
    stage.overview_drag_to(&mut sh, destination);
    stage.overview_tick(&mut sh, DWELL + 0.01);
    assert_eq!(stage.overview_ws(&sh), 1);
    assert_eq!(sh.session.ws().snapshot(), before);

    let tile = Tiles::new(stage.overview_vp(&sh), stage.overview.scroll).tile(0, 0);
    let below = tile.pos + dvec2(tile.size.x / 2.0, tile.size.y * 0.75);
    stage.overview_drop(&mut sh, below);
    sh.session.settle();
    assert_eq!(sh.session.ws().active, 1);
    assert_eq!(sh.session.ws().columns[0].slots, vec![existing, first]);
    assert_eq!(sh.session.focus(), Some(first));
    assert_eq!(sh.session.ws().wss[0].focus, Some(second));
    assert_eq!(sh.session.history().rows().0.len(), rows + 1);
    let after = sh.session.ws().snapshot();

    assert!(sh.session.undo());
    sh.session.settle();
    assert_eq!(sh.session.history().head(), history);
    assert_eq!(sh.session.ws().snapshot(), before);
    assert!(sh.session.redo());
    sh.session.settle();
    assert_eq!(sh.session.ws().snapshot(), after);
}

/// A tile pulled down its column is the phone's close: short of half its
/// height a lift springs it back and nothing happens; past it the tile
/// runs off the strip and the panel closes when it has gone — one undoable
/// action, with overview still up.
#[test]
fn pulling_a_tile_down_closes_its_panel_once_it_has_gone() {
    let (_cx, mut stage, mut sh, first, second) = workspace();
    let tile_of = |stage: &Stage, sh: &Shell, slot| {
        let ws = &sh.session.ws().wss[stage.overview_ws(sh)];
        let (col, row) = ws.locate(slot).unwrap();
        Tiles::new(stage.overview_vp(sh), stage.overview.scroll).tile(col, row)
    };
    let land = |stage: &mut Stage, sh: &mut Shell| {
        for _ in 0..600 {
            if !stage.overview_tick(sh, 1.0 / 60.0) {
                break;
            }
            stage.settle_tile_swipe(sh);
        }
        stage.settle_tile_swipe(sh);
        sh.session.settle();
    };
    let history = sh.session.history().head();

    // Short of the threshold: back where it stood, nothing closed.
    let tile = tile_of(&stage, &sh, first);
    stage.overview_swipe_start(&mut sh, first, tile, 10.0);
    stage.overview_swipe_to(&mut sh, SWIPE_CLOSE - 5.0);
    assert!(!stage.overview.swipe.as_ref().unwrap().armed());
    stage.overview_swipe_release(&mut sh);
    land(&mut stage, &mut sh);
    assert!(stage.overview.swipe.is_none());
    assert!(sh.session.panel(first).is_some());
    assert_eq!(sh.session.history().head(), history);

    // Past it: the tile is sent off, and the close lands with it.
    stage.overview_swipe_start(&mut sh, first, tile, 10.0);
    stage.overview_swipe_to(&mut sh, SWIPE_CLOSE + 5.0);
    assert!(stage.overview.swipe.as_ref().unwrap().armed());
    stage.overview_swipe_release(&mut sh);
    assert!(stage.overview.swipe.as_ref().unwrap().commit);
    assert!(sh.session.panel(first).is_some(), "not before the tile has gone");
    land(&mut stage, &mut sh);
    assert!(stage.overview.swipe.is_none());
    assert!(sh.session.panel(first).is_none());
    assert!(sh.session.panel(second).is_some());
    assert_eq!(sh.overlay, Overlay::Overview, "overview stays up");
    assert_ne!(sh.session.history().head(), history);
    assert!(sh.session.undo());
    sh.session.settle();
    assert!(sh.session.panel(first).is_some());

    // Overview put away under a committed pull: the close still runs.
    let tile = tile_of(&stage, &sh, second);
    stage.overview_swipe_start(&mut sh, second, tile, SWIPE_CLOSE + 20.0);
    stage.overview_swipe_release(&mut sh);
    sh.overlay = Overlay::None;
    stage.overview_tick(&mut sh, 1.0 / 60.0);
    sh.session.settle();
    assert!(sh.session.panel(second).is_none());
    assert!(stage.overview.swipe.is_none());
}

/// A workspace tile switches the stack behind overview at once: nothing is
/// left sliding to show through the fade when a panel tile is tapped next.
#[test]
fn a_workspace_tile_lands_the_stack_without_a_slide() {
    let (_cx, mut stage, mut sh, _, _) = workspace();
    sh.anim.slide().retarget(0.0);
    stage.overview_workspace(&mut sh, 2);
    assert_eq!(sh.session.ws().active, 2);
    assert!(sh.anim.slide().is_done());
    assert!((sh.anim.slide().value() - 2.0).abs() < f64::EPSILON);
    assert!(sh.anim.camera().is_done());
    assert_eq!(sh.overlay, Overlay::Overview);
}

/// A tile still on its way off the strip when the next one is pulled
/// closes its panel then and there: a quick second pull must not forget
/// the first.
#[test]
fn a_second_pull_does_not_forget_a_close_still_in_flight() {
    let (_cx, mut stage, mut sh, first, second) = workspace();
    let tile_of = |stage: &Stage, sh: &Shell, slot| {
        let ws = &sh.session.ws().wss[stage.overview_ws(sh)];
        let (col, row) = ws.locate(slot).unwrap();
        Tiles::new(stage.overview_vp(sh), stage.overview.scroll).tile(col, row)
    };
    let tile = tile_of(&stage, &sh, first);
    stage.overview_swipe_start(&mut sh, first, tile, SWIPE_CLOSE + 20.0);
    stage.overview_swipe_release(&mut sh);
    assert!(stage.overview.swipe.as_ref().unwrap().commit);
    stage.overview_tick(&mut sh, 1.0 / 60.0);
    assert!(!stage.overview.swipe.as_ref().unwrap().dy.is_done(), "still flying");

    let tile = tile_of(&stage, &sh, second);
    stage.overview_swipe_start(&mut sh, second, tile, 10.0);
    sh.session.settle();
    assert!(sh.session.panel(first).is_none(), "the first close landed");
    assert_eq!(stage.overview.swipe.as_ref().unwrap().slot, second);
    assert!(!stage.overview.swipe.as_ref().unwrap().commit);
    assert!(sh.session.panel(second).is_some());
}

/// A second finger landing and lifting mid-pull changes nothing: the
/// finger that holds the tile keeps it, and its lift decides.
#[test]
fn a_bystander_finger_cannot_cancel_a_pull() {
    let (mut cx, mut stage, mut sh, first, _) = workspace();
    let ws = &sh.session.ws().wss[stage.overview_ws(&sh)];
    let (col, row) = ws.locate(first).unwrap();
    let tile = Tiles::new(stage.overview_vp(&sh), stage.overview.scroll).tile(col, row);
    // The hit table is the draw's; stand in for it.
    let mut hit = Hit::act("first", tile, MouseCursor::Hand, Act::OverviewPanel(first));
    hit.unclipped = Some(tile);
    stage.hits.push(hit);
    let c = center(tile);
    stage.touch_start(1, c);
    stage.touch_move(&mut cx, &mut sh, 1, c + dvec2(0.0, SWIPE_CLOSE + 30.0));
    assert!(matches!(stage.touch.mode, Mode::TileSwipe { uid: 1 }));
    assert!(stage.overview.swipe.as_ref().unwrap().armed());

    let elsewhere = dvec2(30.0, 700.0);
    stage.touch_start(2, elsewhere);
    stage.touch_stop(&mut cx, &mut sh, 2, elsewhere);
    assert!(matches!(stage.touch.mode, Mode::TileSwipe { uid: 1 }), "still held");
    assert!(stage.overview.swipe.is_some());

    stage.touch_stop(&mut cx, &mut sh, 1, c + dvec2(0.0, SWIPE_CLOSE + 30.0));
    assert!(stage.overview.swipe.as_ref().unwrap().commit);
    for _ in 0..600 {
        if !stage.overview_tick(&mut sh, 1.0 / 60.0) {
            break;
        }
    }
    stage.settle_tile_swipe(&mut sh);
    sh.session.settle();
    assert!(sh.session.panel(first).is_none());
}

/// One finger on a strip, at speed and at a crawl.
///
/// A flick throws the strip: it carries on after the finger and slows to a
/// stop by itself, as a panel's body does. A slow drag places the strip
/// instead — the same distance travelled, no speed at the lift, and it
/// stays exactly where it was let go.
#[test]
fn a_flicked_overview_strip_coasts_and_a_placed_one_stays() {
    for (moves, step, dt, coasts) in [(5, 40.0, 1.0 / 60.0, true), (20, 3.0, 0.1, false)] {
        let (mut cx, mut stage, mut sh, _, _) = workspace();
        // The workspace row above the tiles: nine spaces, so there is far
        // more strip than viewport whatever the panels do.
        let (mut x, y, mut time) = (300.0, 90.0, 1.0);
        touch(&mut cx, &mut stage, &mut sh, TouchState::Start, dvec2(x, y), time);
        for _ in 0..moves {
            time += dt;
            x -= step;
            touch(&mut cx, &mut stage, &mut sh, TouchState::Move, dvec2(x, y), time);
        }
        assert!(matches!(stage.touch.mode, Mode::OverviewScroll { uid: 1, .. }));
        touch(&mut cx, &mut stage, &mut sh, TouchState::Stop, dvec2(x, y), time);
        let released = stage.overview.workspace_scroll;
        assert!(released > 0.0, "the drag itself moved the strip");

        let mut ticks = 0;
        while stage.touch_tick(&mut cx, &mut sh, 1.0 / 60.0) && ticks < 600 {
            ticks += 1;
        }
        let settled = stage.overview.workspace_scroll;
        if coasts {
            assert!(settled > released, "{settled} <= {released}: no inertia");
            assert!(ticks > 0 && ticks < 600, "the coast ran and then stopped");
        } else {
            assert_eq!(settled, released, "a placed strip does not drift");
        }
    }
}

/// A tap that lands on a coasting strip stops it where it is, and is not
/// also a press on the tile underneath.
#[test]
fn a_press_catches_a_coasting_strip() {
    let (mut cx, mut stage, mut sh, _, _) = workspace();
    let (mut x, y, mut time) = (300.0, 90.0, 1.0);
    touch(&mut cx, &mut stage, &mut sh, TouchState::Start, dvec2(x, y), time);
    for _ in 0..5 {
        time += 1.0 / 60.0;
        x -= 40.0;
        touch(&mut cx, &mut stage, &mut sh, TouchState::Move, dvec2(x, y), time);
    }
    touch(&mut cx, &mut stage, &mut sh, TouchState::Stop, dvec2(x, y), time);
    stage.touch_tick(&mut cx, &mut sh, 1.0 / 60.0);
    stage.touch_tick(&mut cx, &mut sh, 1.0 / 60.0);
    let caught_at = stage.overview.workspace_scroll;

    time += 1.0;
    touch(&mut cx, &mut stage, &mut sh, TouchState::Start, dvec2(x, y), time);
    assert!(stage.touch.caught, "the press took the strip, not the tile");
    for _ in 0..30 {
        stage.touch_tick(&mut cx, &mut sh, 1.0 / 60.0);
    }
    assert_eq!(stage.overview.workspace_scroll, caught_at, "the coast stopped");
}

fn touch(
    cx: &mut Cx,
    stage: &mut Stage,
    sh: &mut Shell,
    state: TouchState,
    p: DVec2,
    time: f64,
) {
    stage.touch_update(
        cx,
        sh,
        &TouchUpdateEvent {
            window_id: CxWindowPool::id_zero(),
            time,
            modifiers: Default::default(),
            touches: vec![TouchPoint {
                uid: 1,
                state,
                abs: p,
                time,
                force: 1.0,
                radius: DVec2::default(),
                rotation_angle: 0.0,
                handled: std::cell::Cell::new(Area::Empty),
                sweep_lock: std::cell::Cell::new(Area::Empty),
            }],
        },
    );
}
