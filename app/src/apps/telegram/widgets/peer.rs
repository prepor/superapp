//! A peer's card, drawn: the name, what it is, a phone number where there
//! is one, and the bio or description under a rule.
//!
//! Everything on it is a cached query on the id the panel carries, so there
//! is nothing to keep between draws. The ways off the card are on the bar,
//! where every navigation is.

use makepad_widgets::*;
use kernel::session::Session;

use crate::shell::hosted::PanelProps;

use super::super::panels::Peer;

/// The widget.
#[derive(Script, ScriptHook, Widget)]
pub struct PeerPanel {
    #[source]
    source: ScriptObjectRef,
    #[deref]
    view: View,
}

impl Widget for PeerPanel {
    fn handle_event(&mut self, cx: &mut Cx, event: &Event, scope: &mut Scope) {
        if let Event::KeyDown(k) = event {
            if k.key_code == KeyCode::Escape {
                if let (Some(props), Some(s)) = (
                    scope.props.get::<PanelProps>(), scope.data.get_mut::<Session>(),
                ) {
                    if s.focus() == Some(props.slot) {
                        props.panel.borrow_mut().run("telegram.cancel", s);
                    }
                }
            }
        }
        self.view.handle_event(cx, event, scope);
    }

    fn draw_walk(&mut self, cx: &mut Cx2d, scope: &mut Scope, walk: Walk) -> DrawStep {
        let Some(props) = scope.props.get::<PanelProps>().cloned() else {
            return self.view.draw_walk(cx, scope, walk);
        };
        let (card, prompt) = {
            let mut borrow = props.panel.borrow_mut();
            borrow.as_any().downcast_mut::<Peer>()
                .map_or((None, None), |p| (p.card(), p.prompt()))
        };
        let Some(card) = card else {
            self.view.label(cx, ids!(name_lbl)).set_text(cx, "nobody");
            self.view
                .label(cx, ids!(kind_lbl))
                .set_text(cx, "no such peer");
            return self.view.draw_walk(cx, scope, walk);
        };
        let kind_line = card.kind_line();
        self.view.label(cx, ids!(name_lbl)).set_text(cx, &card.name);
        self.view.label(cx, ids!(kind_lbl)).set_text(cx, &kind_line);
        let prompt = prompt.unwrap_or_default();
        self.view.label(cx, ids!(prompt_lbl)).set_visible(cx, !prompt.is_empty());
        self.view.label(cx, ids!(prompt_lbl)).set_text(cx, &prompt);
        let phone = card.phone.clone().unwrap_or_default();
        self.view
            .view(cx, ids!(phone_wrap))
            .set_visible(cx, !phone.is_empty());
        self.view
            .text_input(cx, ids!(phone_wrap.phone_txt))
            .set_text(cx, &phone);
        let about = card.about.clone().unwrap_or_default();
        self.view
            .view(cx, ids!(about_wrap))
            .set_visible(cx, !about.is_empty());
        self.view
            .text_input(cx, ids!(about_wrap.about_txt))
            .set_text(cx, &about);
        self.view
            .label(cx, ids!(none_lbl))
            .set_visible(cx, about.is_empty());

        let step = self.view.draw_walk(cx, scope, walk);
        // The kind line is what a script addresses the card by — the name
        // is also the panel's title, and a title is chrome.
        let r = self.view.label(cx, ids!(kind_lbl)).area().rect(cx);
        if r.size.x > 0.0 {
            props
                .hits
                .add(kind_line, r, MouseCursor::Default, props.slot);
        }
        if !prompt.is_empty() {
            let r = self.view.label(cx, ids!(prompt_lbl)).area().rect(cx);
            props.hits.add(prompt, r, MouseCursor::Default, props.slot);
        }
        step
    }
}
