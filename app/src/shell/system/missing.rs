//! The card a slot gets when no app in this build owns its tag.
//!
//! The slot is kept, not dropped: another build has the app, and the
//! session is shared. The instance is the kernel's
//! [`Missing`](kernel::panel::Missing); this only draws it.

use kernel::panel::Missing;
use makepad_widgets::*;

use crate::shell::hosted::PanelProps;

/// The widget: the tag it could not open, and the one line that says why.
#[derive(Script, ScriptHook, Widget)]
pub struct MissingPanel {
    #[source]
    source: ScriptObjectRef,
    #[deref]
    view: View,
}

impl Widget for MissingPanel {
    fn handle_event(&mut self, cx: &mut Cx, event: &Event, scope: &mut Scope) {
        self.view.handle_event(cx, event, scope);
    }

    fn draw_walk(&mut self, cx: &mut Cx2d, scope: &mut Scope, walk: Walk) -> DrawStep {
        let tag = scope
            .props
            .get::<PanelProps>()
            .map(|p| p.panel.borrow().id().to_string())
            .unwrap_or_default();
        self.view.label(cx, ids!(tag_lbl)).set_text(cx, &tag);
        let _ = Missing::line();
        self.view.draw_walk(cx, scope, walk)
    }
}
