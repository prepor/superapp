//! The editor, drawn: the shell's `SourceInput` over the document, with
//! the Markdown spans, and the line that says where the draft stands.

use kernel::session::Session;
use makepad_widgets::*;

use crate::shell::hosted::PanelProps;
use crate::shell::keys::Letters;
use crate::shell::widgets::source_input::{self, SourceInputWidgetRefExt};

use super::super::markdown;
use super::super::panels::Edit;

#[derive(Script, ScriptHook, Widget)]
pub struct EditPanel {
    #[source]
    source: ScriptObjectRef,
    #[deref]
    view: View,
    #[rust]
    shown: Option<u64>,
    #[rust]
    background: bool,
    #[rust]
    focus_next_frame: Option<NextFrame>,
}

impl EditPanel {
    fn input(&self, cx: &mut Cx) -> source_input::SourceInputRef {
        self.view.widget(cx, ids!(body_input)).as_source_input()
    }
}

impl Widget for EditPanel {
    fn handle_event(&mut self, cx: &mut Cx, event: &Event, scope: &mut Scope) {
        let Some(props) = scope.props.get::<PanelProps>().cloned() else {
            return;
        };
        match event {
            Event::WindowLostFocus(_) | Event::Background | Event::Pause => self.background = true,
            Event::WindowGotFocus(_) | Event::Foreground | Event::Resume => self.background = false,
            _ => {}
        }
        let input = self.input(cx);
        let active = props.has_keyboard && !self.background;
        let activated = input.borrow_mut().is_some_and(|mut input| input.set_focus_active(cx, active));
        self.view.handle_event(cx, event, scope);
        if active
            && self.shown.is_some()
            && !input.area().is_empty()
            && (!input.focus_dismissed_by_touch() || matches!(event, Event::KeyDown(k) if k.key_code == KeyCode::Tab))
            && (activated || self.focus_next_frame.is_some() || !input.key_focus(cx))
        {
            input.take_key_focus(cx);
        }
        self.focus_next_frame = None;
        if let Event::Actions(actions) = event {
            if let Some(text) = input.changed(actions) {
                let now = scope.data.get_mut::<Session>().map_or(0.0, |s| s.now());
                if let Some(p) = props.panel.borrow_mut().as_any().downcast_mut::<Edit>() {
                    let spans = markdown::spans(&text);
                    p.edited(text, now);
                    if let Some(mut input) = input.borrow_mut() {
                        input.set_spans(cx, spans);
                    }
                }
                if let Some(s) = scope.data.get_mut::<Session>() {
                    s.redraw();
                }
                self.view.redraw(cx);
            }
        }
    }

    fn draw_walk(&mut self, cx: &mut Cx2d, scope: &mut Scope, walk: Walk) -> DrawStep {
        let Some(props) = scope.props.get::<PanelProps>().cloned() else {
            return self.view.draw_walk(cx, scope, walk);
        };
        let input = self.input(cx);
        if let Some(p) = props.panel.borrow_mut().as_any().downcast_mut::<Edit>() {
            if self.shown != Some(p.revision) {
                input.set_text(cx, &p.text);
                if let Some(mut input) = input.borrow_mut() {
                    input.set_spans(cx, markdown::spans(&p.text));
                }
                self.shown = Some(p.revision);
            }
            self.view.label(cx, ids!(status_lbl)).set_text(cx, &p.status());
            self.view.label(cx, ids!(error_lbl)).set_text(cx, &p.error);
            self.view.widget(cx, ids!(error_lbl)).set_visible(cx, !p.error.is_empty());
            // The save chord is the bar's while the caret is in the field.
            props.keyboard.keep(&self.view.widget(cx, ids!(body_input)), Letters::ALL.minus(Letters::of(&['s'])));
        }
        let active = props.has_keyboard && !self.background;
        let activated = input.borrow_mut().is_some_and(|mut input| input.set_focus_active(cx, active));
        self.view.draw_walk_all(cx, scope, walk);
        if active && !input.focus_dismissed_by_touch() && (activated || !input.key_focus(cx)) {
            if self.focus_next_frame.is_none() {
                self.focus_next_frame = Some(cx.new_next_frame());
            }
        } else if !active || input.focus_dismissed_by_touch() {
            self.focus_next_frame = None;
        }
        props.hits.add("editor", input.area().rect(cx), MouseCursor::Text, props.slot);
        for (label, path) in [("editor status", ids!(status_lbl)), ("editor error", ids!(error_lbl))] {
            let widget = self.view.widget(cx, path);
            if widget.visible() {
                props.hits.add(label, widget.area().rect(cx), MouseCursor::Default, props.slot);
            }
        }
        DrawStep::done()
    }
}
