//! A correspondent's card, drawn: a name, an address, a count, and the link
//! to their letters.
//!
//! Everything on it is a cached query on the address the panel carries, so
//! there is nothing to keep between draws.

use makepad_widgets::*;

use crate::shell::dsl::SLinkWidgetRefExt;
use crate::shell::hosted::PanelProps;

use super::super::panels::Contact;

/// The widget.
#[derive(Script, ScriptHook, Widget)]
pub struct ContactPanel {
    #[source]
    source: ScriptObjectRef,
    #[deref]
    view: View,
}

impl Widget for ContactPanel {
    fn handle_event(&mut self, cx: &mut Cx, event: &Event, scope: &mut Scope) {
        self.view.handle_event(cx, event, scope);
    }

    fn draw_walk(&mut self, cx: &mut Cx2d, scope: &mut Scope, walk: Walk) -> DrawStep {
        let Some(props) = scope.props.get::<PanelProps>().cloned() else {
            return self.view.draw_walk(cx, scope, walk);
        };
        let card = {
            let mut borrow = props.panel.borrow_mut();
            borrow.as_any().downcast_mut::<Contact>().map(|c| {
                let (name, n) = c.who();
                (name, n, c.email().to_string(), c.link_label(), c.slot())
            })
        };
        let Some((name, n, email, label, slot)) = card else {
            return self.view.draw_walk(cx, scope, walk);
        };
        self.view.label(cx, ids!(name_lbl)).set_text(cx, &name);
        self.view.label(cx, ids!(email_lbl)).set_text(cx, &email);
        self.view
            .label(cx, ids!(count_lbl))
            .set_text(cx, &format!("{n} message(s) in mail"));
        // The same navigation the bar's verb carries, drawn where a person
        // reading the card looks for it. Its letter is the bar's, so it is
        // not drawn bold twice.
        self.view.widget(cx, ids!(from_link)).as_slink().set(
            cx,
            &label,
            kernel::nav::Nav::Open {
                from: slot,
                id: super::super::model::Role::Inbox.filtered(&email),
                fresh: false,
            },
            false,
            None,
        );

        let step = self.view.draw_walk(cx, scope, walk);
        // The address is what a script addresses the card by — the name is
        // also the panel's title, and a title is chrome.
        let r = self.view.label(cx, ids!(email_lbl)).area().rect(cx);
        if r.size.x > 0.0 {
            props
                .hits
                .add(email, r, MouseCursor::Default, props.slot);
        }
        step
    }
}
