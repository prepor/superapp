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
    removes: Vec<(i64, u8, Rect)>,
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
        let Some((id, action, _)) = self
            .removes
            .iter()
            .rev()
            .find(|(_, _, r)| r.contains(e.abs))
        else {
            return;
        };
        let id = *id;
        let action = *action;
        let Some(session) = scope.data.get_mut::<Session>() else {
            return;
        };
        let mut borrow = props.panel.borrow_mut();
        if let Some(s) = borrow.as_any().downcast_mut::<Settings>() {
            match action {
                0 => s.ask_remove(session, id),
                1 => s.service(session, id, false),
                2 => s.service(session, id, true),
                _ => s.reconnect(session, id),
            }
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
                let confirming = props
                    .panel
                    .borrow_mut()
                    .as_any()
                    .downcast_mut::<Settings>()
                    .is_some_and(|p| p.confirming == Some(a.id));
                let (status, err) = if confirming {
                    ("Remove this account, its cached Mail and Calendar data, and unsent drafts? Google events remain in Google. Press confirm remove to continue.".into(),false)
                } else {
                    a.status_line()
                };
                row.button(cx, ids!(remove_btn)).set_text(
                    cx,
                    if confirming {
                        "confirm remove"
                    } else {
                        "remove"
                    },
                );
                let services = scope
                    .data
                    .get::<Session>()
                    .map(|s| crate::identity::services(s.store().conn(), a.id))
                    .unwrap_or_default();
                row.button(cx, ids!(mail_btn))
                    .set_text(cx, if services.0 { "Mail: on" } else { "Mail: off" });
                row.button(cx, ids!(calendar_btn)).set_text(
                    cx,
                    if services.1 {
                        "Calendar: on"
                    } else {
                        "Calendar: off"
                    },
                );
                row.button(cx, ids!(calendar_btn)).set_visible(
                    cx,
                    a.oauth() || services.1 || services.2.contains("calendar"),
                );
                row.button(cx, ids!(reconnect_btn)).set_visible(cx, true);
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
            for path in [
                ids!(email_lbl),
                ids!(host_lbl),
                ids!(status_lbl),
                ids!(status_err_lbl),
            ] {
                let w = row.text_input(cx, path);
                if !w.visible() {
                    continue;
                }
                let r = w.area().rect(cx);
                if r.size.x > 0.0 {
                    props.hits.add_clipped(
                        w.text(),
                        r,
                        self.view.area().rect(cx),
                        MouseCursor::Text,
                        props.slot,
                    );
                }
            }
            for (action, name, path) in [
                (1, "toggle Mail", ids!(mail_btn)),
                (2, "toggle Calendar", ids!(calendar_btn)),
                (3, "reconnect account", ids!(reconnect_btn)),
            ] {
                let w = row.widget(cx, path);
                if w.visible() {
                    let r = w.area().rect(cx);
                    props.hits.add_clipped(
                        name,
                        r,
                        self.view.area().rect(cx),
                        MouseCursor::Hand,
                        props.slot,
                    );
                    self.removes.push((id, action, r));
                }
            }
            let r = row.button(cx, ids!(remove_btn)).area().rect(cx);
            if r.size.x > 0.0 {
                props.hits.add_clipped(
                    row.button(cx, ids!(remove_btn)).text(),
                    r,
                    self.view.area().rect(cx),
                    MouseCursor::Hand,
                    props.slot,
                );
                self.removes.push((id, 0, r));
            }
        }
        DrawStep::done()
    }
}
