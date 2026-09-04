//! The device-sync form: where the bucket is, and the key that opens it.
//!
//! Device sync is not an app — it replicates the store itself, every app's
//! tables included — so the form for it is the shell's, drawn by the shell's
//! own app like every other panel here.
//!
//! This is the road a device with no shell and no cable has: a phone is
//! still a device that has to be given a credential, and typing one in is
//! the only way it can be. The secret field is write-only — it is seeded
//! blank even on a configured device, because a key that can be read back
//! off a screen is a key that leaves by a route nobody chose.
//!
//! What goes in that field is the Cloudflare API token's *value*: the key
//! the bucket signs with is its hash, taken on the way to a signature
//! ([`r2::creds`]), and the value is the one credential this device has.

use std::any::Any;

use kernel::panel::{Opening, Panel, PanelId, PanelKind, Tag, Verb};
use kernel::repl::r2;
use kernel::session::Session;
use makepad_widgets::*;

use crate::shell::hosted::PanelProps;
use crate::shell::keys::Letters;

/// The three fields, in the order tab walks them.
const FIELDS: [&[LiveId]; 3] = [ids!(url_input), ids!(key_input), ids!(secret_input)];

/// What a script — and a finger — addresses each by.
const LABELS: [&str; 3] = ["bucket", "key id", "secret"];

/// The device-sync form. The instance owns the text: a panel's fields are
/// its own state, so the bar's *connect* and the field's own enter reach the
/// same three strings.
pub struct Bucket {
    id: PanelId,
    url: String,
    key_id: String,
    /// The Cloudflare API token's value. Write-only: typed in, filed in the
    /// platform's secret store, and cleared. Never read back out for display.
    secret: String,
}

impl Bucket {
    pub const TAG: Tag = Tag("bucket");

    /// The identity of the one device-sync panel.
    #[must_use]
    pub fn id() -> PanelId {
        PanelId::bare(Self::TAG)
    }

    /// What the form comes up showing: the `bucket` file's own url and key
    /// id, so a device that is already configured shows what it is
    /// configured with rather than an empty sheet.
    fn prefilled(id: &PanelId, cx: &Opening<'_>) -> Bucket {
        let dir = cx.session().db_dir();
        Bucket {
            id: id.clone(),
            url: r2::url_from_file(dir).unwrap_or_default(),
            key_id: r2::configured_key_id(dir),
            secret: String::new(),
        }
    }

    /// The two public fields, for the widget to seed itself from once.
    #[must_use]
    pub fn shown(&self) -> (&str, &str) {
        (&self.url, &self.key_id)
    }

    /// What the widget writes back on every keystroke.
    pub fn edited(&mut self, url: String, key_id: String, secret: String) {
        self.url = url;
        self.key_id = key_id;
        self.secret = secret;
    }

    /// Points this device at what the form says. Answers whether the secret
    /// field should be cleared — it is in the keychain now, and a form is
    /// not a place to keep one.
    fn connect(&mut self, s: &mut Session) -> bool {
        match s.connect_bucket(self.url.trim(), self.key_id.trim(), &self.secret) {
            Ok(said) => {
                s.notify(said, false);
                self.secret.clear();
                true
            }
            Err(e) => {
                s.notify(e, true);
                false
            }
        }
    }
}

impl Panel for Bucket {
    fn id(&self) -> &PanelId {
        &self.id
    }

    fn title(&self) -> String {
        "device sync".into()
    }

    /// A form: as wide as an endpoint URL reads, and no taller than its
    /// three rows.
    fn wish(&self, _cols: usize) -> (u32, u32) {
        (5, 3)
    }

    fn verbs(&self) -> Vec<Verb> {
        vec![Verb::run("system.connect", "connect", Some('c'))]
    }

    fn run(&mut self, verb: &str, s: &mut Session) {
        if verb == "system.connect" {
            self.connect(s);
        }
    }

    fn as_any(&mut self) -> &mut dyn Any {
        self
    }
}

/// Its factory.
pub struct BucketKind;

impl PanelKind for BucketKind {
    fn tag(&self) -> Tag {
        Bucket::TAG
    }

    fn open(&self, id: &PanelId, cx: &mut Opening<'_>) -> Box<dyn Panel> {
        Box::new(Bucket::prefilled(id, cx))
    }
}

/// The widget: three fields, seeded once from the instance and written back
/// to it on every keystroke.
#[derive(Script, ScriptHook, Widget)]
pub struct BucketPanel {
    #[source]
    source: ScriptObjectRef,
    #[deref]
    view: View,
    /// Whether the two public fields have been seeded. Once, before the
    /// first draw; after that the text is the operator's, empty included.
    #[rust]
    primed: bool,
}

impl BucketPanel {
    /// What the three fields hold right now.
    fn values(&self, cx: &mut Cx) -> (String, String, String) {
        let [url, key, secret] = FIELDS.map(|p| self.view.text_input(cx, p));
        (url.text(), key.text(), secret.text())
    }

    /// Copies the fields into the instance, so the bar's *connect* acts on
    /// what is on screen.
    fn write_back(&self, cx: &mut Cx, props: &PanelProps) {
        let (url, key, secret) = self.values(cx);
        if let Some(b) = props.panel.borrow_mut().as_any().downcast_mut::<Bucket>() {
            b.edited(url, key, secret);
        }
    }

    /// Runs the panel's own *connect*, and clears the secret if it took.
    fn submit(&mut self, cx: &mut Cx, props: &PanelProps, scope: &mut Scope) {
        self.write_back(cx, props);
        let Some(session) = scope.data.get_mut::<Session>() else {
            return;
        };
        let cleared = props
            .panel
            .borrow_mut()
            .as_any()
            .downcast_mut::<Bucket>()
            .is_some_and(|b| b.connect(session));
        if cleared {
            self.view.text_input(cx, FIELDS[2]).set_text(cx, "");
        }
        self.view.redraw(cx);
    }
}

impl Widget for BucketPanel {
    fn handle_event(&mut self, cx: &mut Cx, event: &Event, scope: &mut Scope) {
        let Some(props) = scope.props.get::<PanelProps>().cloned() else {
            self.view.handle_event(cx, event, scope);
            return;
        };
        let fields = FIELDS.map(|p| self.view.text_input(cx, p));
        let focused = fields.iter().any(|f| f.key_focus(cx));
        // A live field keeps every cmd chord — the caret's own `cmd+a`
        // included — so no bar may promise one while it blinks.
        if focused {
            props.chord.field(Letters::ALL);
            if matches!(event, Event::KeyDown(k) if k.modifiers.logo) {
                props.chord.take();
            }
        }

        self.view.handle_event(cx, event, scope);

        if let Event::Actions(actions) = event {
            // A blurred field keeps no selection (the frameworks' norm).
            for f in &fields {
                if f.key_focus_lost(actions) {
                    f.set_cursor(cx, f.cursor(), false);
                }
            }
            // Enter walks the form; past the last field it connects.
            if fields[0].returned(actions).is_some() {
                focus_field(cx, &fields[1]);
            } else if fields[1].returned(actions).is_some() {
                focus_field(cx, &fields[2]);
            } else if fields[2].returned(actions).is_some() {
                self.submit(cx, &props, scope);
                return;
            }
            if fields.iter().any(|f| f.changed(actions).is_some()) {
                self.write_back(cx, &props);
            }
        }

        // Tab walks the three, wrapping.
        if let Event::KeyDown(k) = event {
            if k.key_code == KeyCode::Tab {
                let at = fields.iter().position(|f| f.key_focus(cx));
                let n = fields.len();
                let next = match (at, k.modifiers.shift) {
                    (Some(i), false) => (i + 1) % n,
                    (Some(i), true) => (i + n - 1) % n,
                    (None, _) => 0,
                };
                focus_field(cx, &fields[next]);
                self.view.redraw(cx);
            }
        }
    }

    fn draw_walk(&mut self, cx: &mut Cx2d, scope: &mut Scope, walk: Walk) -> DrawStep {
        let Some(props) = scope.props.get::<PanelProps>().cloned() else {
            return self.view.draw_walk(cx, scope, walk);
        };
        if !self.primed {
            self.primed = true;
            let (url, key_id) = props
                .panel
                .borrow_mut()
                .as_any()
                .downcast_ref::<Bucket>()
                .map(|b| {
                    let (u, k) = b.shown();
                    (u.to_string(), k.to_string())
                })
                .unwrap_or_default();
            self.view.text_input(cx, FIELDS[0]).set_text(cx, &url);
            self.view.text_input(cx, FIELDS[1]).set_text(cx, &key_id);
        }
        // A field that has the keyboard says so again on every draw: the bar
        // is drawn before this widget, and the promise is about now.
        let fields = FIELDS.map(|p| self.view.text_input(cx, p));
        if fields.iter().any(|f| f.key_focus(cx)) {
            props.chord.field(Letters::ALL);
        }

        let step = self.view.draw_walk(cx, scope, walk);

        // The fields, by the name a script calls them, once they have
        // landed.
        for (p, label) in FIELDS.into_iter().zip(LABELS) {
            let r = self.view.text_input(cx, p).area().rect(cx);
            if r.size.x > 0.0 {
                props.hits.add(label, r, MouseCursor::Text, props.slot);
            }
        }
        step
    }
}

/// Puts the caret in a field and selects what is in it, so typing replaces
/// rather than appends — what a form's field does.
fn focus_field(cx: &mut Cx, field: &TextInputRef) {
    field.set_key_focus(cx);
    if let Some(mut t) = field.borrow_mut() {
        t.select_all(cx);
    }
}
