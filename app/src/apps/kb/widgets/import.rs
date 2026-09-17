//! The import form, drawn: the folder's field, what the press reads, and
//! the line the read answers with.

use kernel::session::Session;
use makepad_widgets::*;

use crate::shell::hosted::PanelProps;

use super::super::panels::Import;

#[derive(Script, ScriptHook, Widget)]
pub struct ImportPanel {
    #[source]
    source: ScriptObjectRef,
    #[deref]
    view: View,
    /// Whether the field has been given the path the panel opened with:
    /// once, before the first draw.
    #[rust]
    primed: bool,
    /// The path the field last showed, so a picked folder is written once.
    #[rust]
    shown: String,
}

impl Widget for ImportPanel {
    fn handle_event(&mut self, cx: &mut Cx, event: &Event, scope: &mut Scope) {
        self.view.handle_event(cx, event, scope);
        let Some(props) = scope.props.get::<PanelProps>().cloned() else {
            return;
        };
        if let Some(s) = scope.data.get_mut::<Session>() {
            if let Some(p) = props.panel.borrow_mut().as_any().downcast_mut::<Import>() {
                p.poll(s);
            }
        }
        let field = self.view.text_input(cx, ids!(path_input));
        if let Event::Actions(actions) = event {
            if field.changed(actions).is_some() || field.returned(actions).is_some() {
                let mut borrow = props.panel.borrow_mut();
                let Some(p) = borrow.as_any().downcast_mut::<Import>() else {
                    return;
                };
                p.path = field.text();
                self.shown.clone_from(&p.path);
                if field.returned(actions).is_some() {
                    if let Some(s) = scope.data.get_mut::<Session>() {
                        p.submit(s);
                    }
                }
                self.view.redraw(cx);
            }
        }
        if let Event::KeyDown(k) = event {
            if k.key_code == KeyCode::Tab {
                field.set_key_focus(cx);
            }
        }
    }

    fn draw_walk(&mut self, cx: &mut Cx2d, scope: &mut Scope, walk: Walk) -> DrawStep {
        let Some(props) = scope.props.get::<PanelProps>().cloned() else {
            return self.view.draw_walk(cx, scope, walk);
        };
        if let Some(s) = scope.data.get_mut::<Session>() {
            if let Some(p) = props.panel.borrow_mut().as_any().downcast_mut::<Import>() {
                p.poll(s);
            }
        }
        if let Some(p) = props.panel.borrow_mut().as_any().downcast_mut::<Import>() {
            if !self.primed || self.shown != p.path {
                self.primed = true;
                self.shown.clone_from(&p.path);
                self.view.text_input(cx, ids!(path_input)).set_text(cx, &p.path);
            }
            self.view.label(cx, ids!(status_lbl)).set_text(cx, &p.status());
        }
        let step = self.view.draw_walk(cx, scope, walk);
        let r = self.view.text_input(cx, ids!(path_input)).area().rect(cx);
        if r.size.x > 0.0 {
            props.hits.add("kb folder", r, MouseCursor::Text, props.slot);
        }
        let label = self.view.label(cx, ids!(status_lbl));
        if !label.text().is_empty() {
            props.hits.add_clipped(label.text(), label.area().rect(cx), self.view.area().rect(cx), MouseCursor::Text, props.slot);
        }
        step
    }
}
