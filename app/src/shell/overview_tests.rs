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
