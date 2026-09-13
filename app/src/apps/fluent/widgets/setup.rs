//! The setup form, drawn: six fields and the line that says what is wrong.
//!
//! Every change is handed straight to the instance, so the bar's *save* has
//! the values without reaching for a widget. Tab walks the ring, enter goes
//! to the next field, and enter in the last one saves — which is the bar's
//! own verb, pulled through the instance so there is one door into the
//! write.

use kernel::panel::Panel;
use kernel::session::Session;
use makepad_widgets::*;

use crate::shell::hosted::PanelProps;
use crate::shell::widgets::form::{self, ClickFocus};

use super::super::panels::Setup;

/// The tab ring, in order, each under the name a script clicks it by.
const FIELDS: [(&str, &[LiveId]); 6] = [
    ("name", ids!(name_input)),
    ("native language", ids!(native_input)),
    ("target language", ids!(target_input)),
    ("level", ids!(level_input)),
    ("goal", ids!(goal_input)),
    ("daily minutes", ids!(minutes_input)),
];

#[derive(Script, ScriptHook, Widget)]
pub struct SetupPanel {
    #[source]
    source: ScriptObjectRef,
    #[deref]
    view: View,
    /// Whether the fields have been seeded. Once, on the first event tick
    /// after the form has been drawn — a field that has never been drawn is
    /// nowhere to put a caret.
    #[rust]
    mounted: bool,
    #[rust]
    click_focus: ClickFocus,
}

impl Widget for SetupPanel {
    fn handle_event(&mut self, cx: &mut Cx, event: &Event, scope: &mut Scope) {
        let Some(props) = scope.props.get::<PanelProps>().cloned() else {
            return;
        };
        let inputs = self.inputs(cx);
        self.view.handle_event(cx, event, scope);
        self.mount(cx, &props);
        form::tab(cx, event, &inputs);
        self.click_focus.handle(
            cx,
            event,
            &props,
            &inputs,
            &FIELDS.iter().map(|(name, _)| *name).collect::<Vec<_>>(),
        );
        let Event::Actions(actions) = event else {
            return;
        };
        if inputs.iter().any(|t| t.changed(actions).is_some()) {
            self.edited(cx, &props);
        }
        for j in 0..FIELDS.len() - 1 {
            if inputs[j].returned(actions).is_some() {
                land(cx, &inputs, j + 1);
            }
        }
        if inputs[FIELDS.len() - 1].returned(actions).is_some() {
            self.edited(cx, &props);
            save(&props, scope);
        }
    }

    fn draw_walk(&mut self, cx: &mut Cx2d, scope: &mut Scope, walk: Walk) -> DrawStep {
        let Some(props) = scope.props.get::<PanelProps>().cloned() else {
            return self.view.draw_walk(cx, scope, walk);
        };
        let error = props
            .panel
            .borrow_mut()
            .as_any()
            .downcast_mut::<Setup>()
            .map(|p| p.error.clone())
            .unwrap_or_default();
        let line = self.view.label(cx, ids!(error_lbl));
        line.set_text(cx, &error);
        line.set_visible(cx, !error.is_empty());

        let step = self.view.draw_walk(cx, scope, walk);
        let clip = self.view.area().rect(cx);
        for (label, path) in FIELDS {
            let r = self.view.widget(cx, path).area().rect(cx);
            if r.size.x > 0.0 {
                props.hits.add_clipped(label, r, clip, MouseCursor::Text, props.slot);
            }
        }
        if !error.is_empty() {
            let r = line.area().rect(cx);
            if r.size.x > 0.0 {
                props.hits.add_clipped(error, r, clip, MouseCursor::Default, props.slot);
            }
        }
        step
    }
}

impl SetupPanel {
    fn inputs(&self, cx: &mut Cx) -> [TextInputRef; 6] {
        [
            self.view.text_input(cx, FIELDS[0].1),
            self.view.text_input(cx, FIELDS[1].1),
            self.view.text_input(cx, FIELDS[2].1),
            self.view.text_input(cx, FIELDS[3].1),
            self.view.text_input(cx, FIELDS[4].1),
            self.view.text_input(cx, FIELDS[5].1),
        ]
    }

    /// The first look at a live panel: the fields take what the row said
    /// and the name takes the keyboard.
    fn mount(&mut self, cx: &mut Cx, props: &PanelProps) {
        if self.mounted {
            return;
        }
        let inputs = self.inputs(cx);
        if inputs[0].area().rect(cx).size.x <= 0.0 {
            return;
        }
        let Some(said) = ({
            let mut borrow = props.panel.borrow_mut();
            borrow.as_any().downcast_mut::<Setup>().map(|p| {
                [
                    p.name.clone(),
                    p.native.clone(),
                    p.target.clone(),
                    p.level.clone(),
                    p.goal.clone(),
                    p.minutes.clone(),
                ]
            })
        }) else {
            return;
        };
        self.mounted = true;
        for (t, s) in inputs.iter().zip(&said) {
            t.set_text(cx, s);
        }
        land(cx, &inputs, 0);
    }

    /// A field changed: the panel keeps the text, because the bar's verb
    /// reads the panel and not the widget.
    fn edited(&mut self, cx: &mut Cx, props: &PanelProps) {
        let [name, native, target, level, goal, minutes] = self.inputs(cx);
        let mut borrow = props.panel.borrow_mut();
        let Some(p) = borrow.as_any().downcast_mut::<Setup>() else {
            return;
        };
        p.name = name.text();
        p.native = native.text();
        p.target = target.text();
        p.level = level.text();
        p.goal = goal.text();
        p.minutes = minutes.text();
        p.error.clear();
    }
}

/// The bar's own verb, from the last field's enter.
fn save(props: &PanelProps, scope: &mut Scope) {
    let Some(session) = scope.data.get_mut::<Session>() else {
        return;
    };
    let mut borrow = props.panel.borrow_mut();
    if let Some(p) = borrow.as_any().downcast_mut::<Setup>() {
        p.run("fluent.save", session);
    }
}

/// Lands in the `j`-th field, with its text selected: typing replaces and
/// backspace clears, as a form's fields do everywhere.
fn land(cx: &mut Cx, inputs: &[TextInputRef; 6], j: usize) {
    inputs[j].set_key_focus(cx);
    if let Some(mut t) = inputs[j].borrow_mut() {
        t.select_all(cx);
    }
}
