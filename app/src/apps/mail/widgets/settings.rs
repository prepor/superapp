//! Mail's accounts, drawn: one row an account, with the button that removes
//! it.
//!
//! The rows are a cached query on every draw, so an account a worker has just
//! synced changes its own line without anything subscribing.
//!
//! The *remove* buttons are answered here, by the rectangles of the last
//! draw, because a portal-list item's own area goes stale the moment a
//! mid-gesture redraw lands — the pattern the problems panel follows for the
//! same reason.

use kernel::session::Session;
use makepad_widgets::*;

use crate::shell::hosted::PanelProps;

use super::super::panels::Settings;

/// The widget.
#[derive(Script, ScriptHook, Widget)]
pub struct SettingsPanel {
    #[source]
    source: ScriptObjectRef,
    #[deref]
    view: View,
    /// Which remove button is where, as the last draw left it.
    #[rust]
    removes: Vec<(i64, Rect)>,
}

impl Widget for SettingsPanel {
    fn handle_event(&mut self, cx: &mut Cx, event: &Event, scope: &mut Scope) {
        self.view.handle_event(cx, event, scope);
        let Event::MouseDown(e) = event else { return };
        let Some(props) = scope.props.get::<PanelProps>().cloned() else {
            return;
        };
        // Only where nothing was drawn over the button: the hit table settles
        // that, as it does for a human.
        if props.hits.at(e.abs).map(|h| h.slot) != Some(Some(props.slot)) {
            return;
        }
        let Some((id, _)) = self.removes.iter().rev().find(|(_, r)| r.contains(e.abs)) else {
            return;
        };
        let id = *id;
        let Some(session) = scope.data.get_mut::<Session>() else {
            return;
        };
        let mut borrow = props.panel.borrow_mut();
        if let Some(s) = borrow.as_any().downcast_mut::<Settings>() {
            s.remove(session, id);
        }
    }

    fn draw_walk(&mut self, cx: &mut Cx2d, scope: &mut Scope, walk: Walk) -> DrawStep {
        let Some(props) = scope.props.get::<PanelProps>().cloned() else {
            return self.view.draw_walk(cx, scope, walk);
        };
        let accounts = {
            let mut borrow = props.panel.borrow_mut();
            match borrow.as_any().downcast_mut::<Settings>() {
                Some(s) => s.accounts(),
                None => return self.view.draw_walk(cx, scope, walk),
            }
        };
        self.view
            .label(cx, ids!(none_lbl))
            .set_visible(cx, accounts.is_empty());

        self.removes.clear();
        let mut drawn: Vec<(i64, WidgetRef)> = Vec::new();
        while let Some(item) = self.view.draw_walk(cx, scope, walk).step() {
            let list_ref = item.as_portal_list();
            let Some(mut list) = list_ref.borrow_mut() else {
                continue;
            };
            list.set_item_range(cx, 0, accounts.len());
            while let Some(idx) = list.next_visible_item(cx) {
                let Some(a) = accounts.get(idx) else { continue };
                let row = list.item(cx, idx, live_id!(account_row));
                let (status, err) = a.status_line();
                row.text_input(cx, ids!(email_lbl)).set_text(cx, &a.email);
                row.text_input(cx, ids!(host_lbl))
                    .set_text(cx, &a.host_line());
                for (path, mine) in [(ids!(status_lbl), !err), (ids!(status_err_lbl), err)] {
                    let t = row.text_input(cx, path);
                    t.set_text(cx, if mine { &status } else { "" });
                    t.set_visible(cx, mine);
                }
                row.draw_all(cx, scope);
                drawn.push((a.id, row));
            }
        }
        // The controls' rectangles, once the rows have landed: the address
        // and the status line so a script can click into them, and the
        // button, which this widget answers itself.
        for (id, row) in drawn {
            for path in [ids!(email_lbl), ids!(host_lbl), ids!(status_lbl), ids!(status_err_lbl)] {
                let w = row.text_input(cx, path);
                if !w.visible() {
                    continue;
                }
                let r = w.area().rect(cx);
                if r.size.x > 0.0 {
                    props
                        .hits
                        .add(w.text(), r, MouseCursor::Text, props.slot);
                }
            }
            let r = row.button(cx, ids!(remove_btn)).area().rect(cx);
            if r.size.x > 0.0 {
                props.hits.add("remove", r, MouseCursor::Hand, props.slot);
                self.removes.push((id, r));
            }
        }
        DrawStep::done()
    }
}
