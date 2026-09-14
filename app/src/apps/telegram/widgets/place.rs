//! The place to send, drawn: the map around where the device says I am,
//! and the coordinates as a selectable run.

use kernel::caps::{tiles, FakeTiles};
use kernel::session::Session;
use makepad_widgets::*;

use crate::shell::hosted::PanelProps;
use crate::shell::widgets::map;
use crate::shell::widgets::media;

use super::super::panels::Place;

/// The widget.
#[derive(Script, ScriptHook, Widget)]
pub struct PlacePanel {
    #[source]
    source: ScriptObjectRef,
    #[deref]
    view: View,
    /// The place the map was drawn around: it is drawn again when the
    /// device says I have moved.
    #[rust]
    drawn: Option<(f64, f64)>,
    /// Whether that map had ground where a tile goes — so the tile that
    /// lands is worth another picture.
    #[rust]
    waiting: bool,
}

impl Widget for PlacePanel {
    fn handle_event(&mut self, cx: &mut Cx, event: &Event, scope: &mut Scope) {
        if self.waiting && crate::shell::tiles::landed(event) {
            self.drawn = None;
            self.view.redraw(cx);
        }
        self.view.handle_event(cx, event, scope);
    }

    fn draw_walk(&mut self, cx: &mut Cx2d, scope: &mut Scope, walk: Walk) -> DrawStep {
        let Some(props) = scope.props.get::<PanelProps>().cloned() else {
            return self.view.draw_walk(cx, scope, walk);
        };
        let here = {
            let mut borrow = props.panel.borrow_mut();
            borrow.as_any().downcast_mut::<Place>().map(|p| p.here())
        };
        let Some((lat, lon)) = here else {
            return self.view.draw_walk(cx, scope, walk);
        };
        // A world with no store on disk is a fixture — a scene of this
        // panel — and draws the kernel's street grid, as the transcript's
        // maps do.
        let fixture = scope
            .data
            .get_mut::<Session>()
            .is_none_or(|s| s.store().dir().is_none());
        if self.drawn != Some((lat, lon)) {
            let snap = if fixture {
                map::snapshot(&FakeTiles, lat, lon, map::ZOOM, 320, 160)
            } else {
                map::snapshot(&*tiles::source(), lat, lon, map::ZOOM, 320, 160)
            };
            self.waiting = !snap.complete;
            self.drawn = Some((lat, lon));
            let map_w = self.view.widget(cx, ids!(map));
            media::fill_map(cx, &map_w, Some(&snap));
        }
        let coords = format!("{lat:.4}, {lon:.4}");
        self.view
            .text_input(cx, ids!(coords_txt))
            .set_text(cx, &coords);
        let step = self.view.draw_walk(cx, scope, walk);
        let r = self.view.widget(cx, ids!(coords_txt)).area().rect(cx);
        if r.size.x > 0.0 {
            props
                .hits
                .add(coords, r, MouseCursor::Text, props.slot);
        }
        step
    }
}
