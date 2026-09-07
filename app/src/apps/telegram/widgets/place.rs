//! The place to send, drawn: the map around where the device says I am,
//! and the coordinates as a selectable run.

use makepad_widgets::*;

use crate::shell::hosted::PanelProps;
use crate::shell::widgets::map::{self, FakeTiles};
use crate::shell::widgets::media;

use super::super::panels::Place;

/// The widget.
#[derive(Script, ScriptHook, Widget)]
pub struct PlacePanel {
    #[source]
    source: ScriptObjectRef,
    #[deref]
    view: View,
    /// The map is drawn once: the place does not move this round.
    #[rust]
    mapped: bool,
}

impl Widget for PlacePanel {
    fn handle_event(&mut self, cx: &mut Cx, event: &Event, scope: &mut Scope) {
        self.view.handle_event(cx, event, scope);
    }

    fn draw_walk(&mut self, cx: &mut Cx2d, scope: &mut Scope, walk: Walk) -> DrawStep {
        super::tell_now(scope);
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
        if !self.mapped {
            self.mapped = true;
            let snap = map::snapshot(&mut FakeTiles, lat, lon, map::ZOOM, 320, 160);
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
