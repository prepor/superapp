//! The add-account form, drawn: four fields and the line the Google flow
//! speaks through.
//!
//! The text is the instance's — every change is handed straight to
//! [`AddAccount::edited`], so the bar's *add* has the values without reaching
//! for a widget — and the sign-in's two halves meet here: the widget hands
//! the flow a waker and opens the browser, because a panel names no Makepad,
//! and polls the answer on every event.

use std::sync::Arc;

use kernel::panel::Panel;
use kernel::session::Session;
use makepad_widgets::*;

use crate::shell::hosted::PanelProps;
use crate::shell::keys::Letters;

use super::super::panels::{AddAccount, Form};

/// The tab ring, in order.
const FIELDS: [(&str, &[LiveId]); 4] = [
    ("address", ids!(email_input)),
    ("password", ids!(pass_input)),
    ("imap", ids!(imap_input)),
    ("smtp", ids!(smtp_input)),
];

/// The widget.
#[derive(Script, ScriptHook, Widget)]
pub struct AddAccountPanel {
    #[source]
    source: ScriptObjectRef,
    #[deref]
    view: View,
    /// Whether the fields have been seeded. Once, on the first event tick
    /// after the form has been drawn — a field that has never been drawn is
    /// nowhere to put a caret.
    #[rust]
    mounted: bool,
}

impl Widget for AddAccountPanel {
    fn handle_event(&mut self, cx: &mut Cx, event: &Event, scope: &mut Scope) {
        let Some(props) = scope.props.get::<PanelProps>().cloned() else {
            return;
        };
        // A finished sign-in lands here, on the thread that owns the store.
        poll(&props, scope);

        let inputs = self.inputs(cx);
        let focused = inputs.iter().position(|t| t.key_focus(cx));
        // A live field keeps every cmd chord, so no bar's letter is drawn as
        // if it would fire — the promise a bold letter makes is about now.
        if focused.is_some() {
            props.chord.field(Letters::ALL);
        }
        if let Event::KeyDown(k) = event {
            if focused.is_some() && k.modifiers.logo {
                props.chord.take();
            }
        }

        self.view.handle_event(cx, event, scope);
        self.mount(cx, &props);
        wanted(&props, scope);
        self.browser(cx, &props);

        // Tab walks the four fields, wrapping; from the panel's own focus it
        // lands on the address.
        if let Event::KeyDown(k) = event {
            if k.key_code == KeyCode::Tab {
                let d: isize = if k.modifiers.shift { -1 } else { 1 };
                let n = inputs.len() as isize;
                let j = match focused {
                    Some(i) => (i as isize + d).rem_euclid(n),
                    None if d > 0 => 0,
                    None => n - 1,
                };
                land(cx, &inputs, j as usize);
            }
        }

        if let Event::Actions(actions) = event {
            for t in &inputs {
                if t.key_focus_lost(actions) {
                    t.set_cursor(cx, t.cursor(), false);
                }
            }
            // Enter advances; past the last field it submits, which is the
            // bar's own verb.
            for j in 0..3 {
                if inputs[j].returned(actions).is_some() {
                    land(cx, &inputs, j + 1);
                }
            }
            if inputs[3].returned(actions).is_some() {
                self.edited(cx, &props);
                submit(&props, scope);
                return;
            }
            if inputs.iter().any(|t| t.changed(actions).is_some()) {
                self.edited(cx, &props);
            }
        }
    }

    fn draw_walk(&mut self, cx: &mut Cx2d, scope: &mut Scope, walk: Walk) -> DrawStep {
        let Some(props) = scope.props.get::<PanelProps>().cloned() else {
            return self.view.draw_walk(cx, scope, walk);
        };
        let line = {
            let mut borrow = props.panel.borrow_mut();
            borrow
                .as_any()
                .downcast_mut::<AddAccount>()
                .and_then(|a| a.google_line().cloned())
        };
        // The Google row: one label or the other, and nothing at all until
        // the flow has spoken.
        let (said, err) = line.clone().unwrap_or_default();
        for (path, mine) in [(ids!(google_lbl), !err), (ids!(google_err_lbl), err)] {
            let l = self.view.label(cx, path);
            l.set_text(cx, if mine { &said } else { "" });
            l.set_visible(cx, mine && !said.is_empty());
        }

        let step = self.view.draw_walk(cx, scope, walk);
        // The four fields by name — that is all a script needs to put a
        // caret in one — and the Google line, so what the flow said is a hit
        // and not a picture.
        for (label, path) in FIELDS {
            let r = self.view.widget(cx, path).area().rect(cx);
            if r.size.x > 0.0 {
                props.hits.add(label, r, MouseCursor::Text, props.slot);
            }
        }
        if !said.is_empty() {
            let path = if err {
                ids!(google_err_lbl)
            } else {
                ids!(google_lbl)
            };
            let r = self.view.label(cx, path).area().rect(cx);
            if r.size.x > 0.0 {
                props.hits.add(said, r, MouseCursor::Default, props.slot);
            }
        }
        step
    }
}

impl AddAccountPanel {
    fn inputs(&self, cx: &mut Cx) -> [TextInputRef; 4] {
        [
            self.view.text_input(cx, FIELDS[0].1),
            self.view.text_input(cx, FIELDS[1].1),
            self.view.text_input(cx, FIELDS[2].1),
            self.view.text_input(cx, FIELDS[3].1),
        ]
    }

    /// The first look at a live panel: the fields take the form's text and
    /// the address takes the keyboard. Held until the fields have a
    /// rectangle — focus on a field that has never been drawn is focus on
    /// nothing.
    fn mount(&mut self, cx: &mut Cx, props: &PanelProps) {
        if self.mounted {
            return;
        }
        let inputs = self.inputs(cx);
        if inputs[0].area().rect(cx).size.x <= 0.0 {
            return;
        }
        let Some(f) = ({
            let mut borrow = props.panel.borrow_mut();
            borrow
                .as_any()
                .downcast_mut::<AddAccount>()
                .map(|a| a.form().clone())
        }) else {
            return;
        };
        self.mounted = true;
        for (t, s) in inputs
            .iter()
            .zip([&f.email, &f.pass, &f.imap, &f.smtp])
        {
            t.set_text(cx, s);
        }
        land(cx, &inputs, 0);
    }

    /// A field changed: the panel keeps the text.
    fn edited(&mut self, cx: &mut Cx, props: &PanelProps) {
        let [email, pass, imap, smtp] = self.inputs(cx);
        let f = Form {
            email: email.text(),
            pass: pass.text(),
            imap: imap.text(),
            smtp: smtp.text(),
        };
        let mut borrow = props.panel.borrow_mut();
        if let Some(a) = borrow.as_any().downcast_mut::<AddAccount>() {
            a.edited(f);
        }
    }

    /// Opens the consent page, if the panel has one waiting. The one thing
    /// here a panel cannot do itself.
    fn browser(&mut self, cx: &mut Cx, props: &PanelProps) {
        let url = {
            let mut borrow = props.panel.borrow_mut();
            borrow
                .as_any()
                .downcast_mut::<AddAccount>()
                .and_then(AddAccount::take_url)
        };
        if let Some(url) = url {
            cx.open_url(&url, OpenUrlInPlace::No);
        }
        // A row was added: the address and the password go with it, once.
        // Said by the panel rather than derived from it — a standing "the
        // form is empty" would wipe what is being typed into it.
        let cleared = {
            let mut borrow = props.panel.borrow_mut();
            borrow
                .as_any()
                .downcast_mut::<AddAccount>()
                .is_some_and(AddAccount::take_cleared)
        };
        if cleared {
            let inputs = self.inputs(cx);
            for t in [&inputs[0], &inputs[1]] {
                t.set_text(cx, "");
            }
            land(cx, &inputs, 0);
        }
    }
}

/// Picks up a finished sign-in. Read-write on the session, and the borrow
/// ends with the call.
fn poll(props: &PanelProps, scope: &mut Scope) {
    let Some(session) = scope.data.get_mut::<Session>() else {
        return;
    };
    let mut borrow = props.panel.borrow_mut();
    if let Some(a) = borrow.as_any().downcast_mut::<AddAccount>() {
        a.observe(session);
    }
}

/// The sign-in the bar asked for, started here: the widget is the one thing
/// that can hand the thread a waker.
fn wanted(props: &PanelProps, scope: &mut Scope) {
    let Some(session) = scope.data.get_mut::<Session>() else {
        return;
    };
    let mut borrow = props.panel.borrow_mut();
    let Some(a) = borrow.as_any().downcast_mut::<AddAccount>() else {
        return;
    };
    if a.take_google() {
        a.google(session, waker());
    }
}

/// The bar's own verb, from the last field's enter. Pulled through the
/// instance so there is one door into the write.
fn submit(props: &PanelProps, scope: &mut Scope) {
    let Some(session) = scope.data.get_mut::<Session>() else {
        return;
    };
    let mut borrow = props.panel.borrow_mut();
    if let Some(a) = borrow.as_any().downcast_mut::<AddAccount>() {
        a.run("mail.add", session);
    }
}

/// The waker a sign-in thread is given: the UI thread has nothing else to
/// wake it while a human is in the browser.
#[must_use]
pub fn waker() -> Arc<dyn Fn() + Send + Sync> {
    Arc::new(SignalToUI::set_ui_signal)
}

/// Lands in the `j`-th field, with its text selected: typing replaces and
/// backspace clears, as a form's fields do everywhere.
fn land(cx: &mut Cx, inputs: &[TextInputRef; 4], j: usize) {
    inputs[j].set_key_focus(cx);
    if let Some(mut t) = inputs[j].borrow_mut() {
        t.select_all(cx);
    }
}
