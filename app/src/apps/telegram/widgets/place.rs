//! The place to send, drawn: the map around where the device says I am, the
//! coordinates and the accuracy as a selectable run, and — until the
//! receiver answers — the waiting, or its refusal.

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
        let Some((here, line, refusal, share)) = ({
            let mut borrow = props.panel.borrow_mut();
            borrow.as_any().downcast_mut::<Place>().map(|p| {
                (
                    p.fix().map(|f| (f.lat, f.lon)),
                    p.where_line(),
                    p.refusal(),
                    p.share_line(),
                )
            })
        }) else {
            return self.view.draw_walk(cx, scope, walk);
        };
        // A world with no store on disk is a fixture — a scene of this
        // panel — and draws the kernel's street grid, as the transcript's
        // maps do.
        let fixture = scope
            .data
            .get_mut::<Session>()
            .is_none_or(|s| s.store().dir().is_none());
        // No fix, no map: a pin at nowhere would be a place the person is
        // not, and the line says what is being waited for instead.
        self.view
            .widget(cx, ids!(map))
            .set_visible(cx, here.is_some());
        if let Some((lat, lon)) = here {
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
        } else {
            self.drawn = None;
        }
        self.view
            .text_input(cx, ids!(coords_txt))
            .set_text(cx, &line);
        let refused = self.view.label(cx, ids!(refused_lbl));
        refused.set_visible(cx, refusal.is_some());
        refused.set_text(cx, refusal.as_deref().unwrap_or(""));
        let sharing = self.view.label(cx, ids!(share_lbl));
        sharing.set_visible(cx, share.is_some());
        sharing.set_text(cx, share.as_deref().unwrap_or(""));
        let step = self.view.draw_walk(cx, scope, walk);
        // What the panel says is what a script addresses it by: the
        // coordinates, the refusal, and the share that is running.
        for (words, path, cursor) in [
            (line, ids!(coords_txt), MouseCursor::Text),
            (
                refusal.unwrap_or_default(),
                ids!(refused_lbl),
                MouseCursor::Default,
            ),
            (
                share.unwrap_or_default(),
                ids!(share_lbl),
                MouseCursor::Default,
            ),
        ] {
            if words.is_empty() {
                continue;
            }
            let r = self.view.widget(cx, path).area().rect(cx);
            if r.size.x > 0.0 && r.size.y > 0.0 {
                props.hits.add(words, r, cursor, props.slot);
            }
        }
        step
    }
}
