//! The sign-in panel: what lets the account holder log in.
//!
//! It owns nothing the store does not. Each draw it reads the one
//! [`tg_session`](super::super::schema) row the worker writes and shows, by
//! the auth state, one line and — where a secret is wanted — one field and
//! one verb: the phone, the login code, the two-factor password. The verb
//! sends what the field holds to TDLib through the [shared
//! transport](super::super::transport::shared), the same client the worker's
//! loop drives; the panel writes nothing itself. TDLib's answer comes back as
//! an update the worker projects into `tg_session`, and the next draw reflects
//! where the flow now stands.
//!
//! Without the `tdlib` feature there is no engine and no worker, so the row
//! stays 'closed' and the verb is an inert toast — the panel still exists and
//! still reads the session, it simply has nothing to send to.

use std::any::Any;
use std::rc::Rc;

use kernel::layout::SlotId;
use kernel::panel::{Opening, Panel, PanelId, PanelKind, Tag, Verb};
use kernel::session::Session;
use kernel::store::Store;

use super::super::schema::{self, Session as TgSession};
use super::super::{config, sync};

/// The verb id the bar and [`run`](SignIn::run) share.
const VERB: &str = "telegram.signin";

/// Which secret the current state asks for — one field, one word for the bar.
/// `None` of these is drawn where the state wants nothing typed (connecting,
/// ready, signing out).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Field {
    /// `wait_phone`: the account's phone number.
    Phone,
    /// `wait_code`: the login code TDLib just delivered.
    Code,
    /// `wait_password`: the two-factor password.
    Password,
}

/// The one sign-in panel.
pub struct SignIn {
    id: PanelId,
    store: Rc<Store>,
    slot: SlotId,
    /// The field's text, kept as the widget hands each change over, so the
    /// bar's verb sends what was typed without reaching for a widget.
    field: String,
}

impl SignIn {
    pub const TAG: Tag = Tag("signin");

    /// The identity of the one sign-in panel.
    #[must_use]
    pub fn id() -> PanelId {
        PanelId::bare(Self::TAG)
    }

    /// The session row as the panel reads it back — the 'closed' default where
    /// no sign-in has begun.
    #[must_use]
    pub fn session(&self) -> TgSession {
        schema::session(self.store.conn())
    }

    /// The auth state, in a word.
    #[must_use]
    pub fn state(&self) -> String {
        self.session().state
    }

    /// The field the current state asks for, if any.
    #[must_use]
    pub fn field_kind(&self) -> Option<Field> {
        match self.state().as_str() {
            "wait_phone" => Some(Field::Phone),
            "wait_code" => Some(Field::Code),
            "wait_password" => Some(Field::Password),
            _ => None,
        }
    }

    /// The line above the field: where the flow stands, in a few words.
    #[must_use]
    pub fn line(&self) -> String {
        let s = self.session();
        match s.state.as_str() {
            "connecting" => "connecting to Telegram…".to_string(),
            "wait_phone" => "enter your phone number".to_string(),
            "wait_code" => "enter the code Telegram sent".to_string(),
            "wait_password" => "enter your password".to_string(),
            "ready" => match s.phone.as_deref() {
                Some(p) => format!("signed in as {p}"),
                None => "signed in".to_string(),
            },
            "logging_out" => "signing out…".to_string(),
            // 'closed', and any state a later phase adds this one does not
            // know: not started, and the worker — where there is one — drives
            // it forward.
            _ => "not started".to_string(),
        }
    }

    /// The hint a code or password step carries, where the worker filed one:
    /// `sms · 5`, a password's own hint. Shown beside the field.
    #[must_use]
    pub fn hint(&self) -> Option<String> {
        match self.field_kind() {
            Some(Field::Code | Field::Password) => self.session().detail,
            _ => None,
        }
    }

    /// The note under the line once signed in: what the store holds so far,
    /// as counts — `syncing · 210 chats · 4 812 lines`. Counts rather than a
    /// sentence, so a sync that fills the list and never a transcript shows
    /// as what it is (2026-09-07: a night of *your chats are syncing* over
    /// zero lines).
    #[must_use]
    pub fn note(&self) -> Option<String> {
        if self.state() != "ready" {
            return None;
        }
        let c = self.store.conn();
        let count = |sql: &str| c.query_row(sql, [], |r| r.get::<_, i64>(0)).unwrap_or(0);
        Some(format!(
            "syncing · {} chats · {} lines",
            count("SELECT COUNT(*) FROM tg_chat"),
            count("SELECT COUNT(*) FROM tg_message")
        ))
    }

    /// The bar's verb for the current field, if any — the word it sends.
    #[must_use]
    pub fn action(&self) -> Option<(&'static str, &'static str)> {
        match self.field_kind()? {
            Field::Phone => Some((VERB, "send phone")),
            Field::Code => Some((VERB, "send code")),
            Field::Password => Some((VERB, "send password")),
        }
    }

    /// What the field comes up with when a step first shows it. The phone is
    /// prefilled from the `telegram` file or the session row where one is
    /// known; a code or a password starts empty — a secret is never guessed.
    #[must_use]
    pub fn field_seed(&self) -> String {
        match self.field_kind() {
            Some(Field::Phone) => config::phone(self.store.dir())
                .or_else(|| self.session().phone)
                .unwrap_or_default(),
            _ => String::new(),
        }
    }

    /// The widget hands every change over, so the bar's verb has the text.
    pub fn edited(&mut self, text: String) {
        self.field = text;
    }

    /// Sends what the field holds to TDLib, by the current state: the phone,
    /// the code, or the password, each through [`sync`]'s own builder so the
    /// wire's JSON lives in one place. The panel writes nothing to the store —
    /// TDLib's answer rides an update the worker projects into `tg_session`,
    /// and the next draw reflects it.
    fn send(&mut self, s: &mut Session) {
        let Some(kind) = self.field_kind() else {
            return;
        };
        let text = self.field.trim().to_string();
        if text.is_empty() {
            s.notify("nothing to send", true);
            return;
        }
        let req = match kind {
            Field::Phone => sync::set_authentication_phone(&text),
            Field::Code => sync::check_authentication_code(&text),
            Field::Password => sync::check_authentication_password(&text),
        };
        deliver(s, &req);
    }
}

/// Fires one request at the shared client, where a build has an engine. The
/// redraw carries the panel forward to the next state once the worker projects
/// TDLib's answer.
#[cfg(feature = "tdlib")]
fn deliver(s: &mut Session, req: &str) {
    use super::super::transport::{self, Td};
    match transport::shared() {
        Some(td) => {
            td.send(req);
            s.redraw();
        }
        None => s.notify("telegram is not connected yet", true),
    }
}

/// Without the feature there is no engine: the send is an inert toast, and the
/// row stays where it was.
#[cfg(not(feature = "tdlib"))]
fn deliver(s: &mut Session, _req: &str) {
    s.notify("this build has no Telegram engine", true);
}

impl Panel for SignIn {
    fn id(&self) -> &PanelId {
        &self.id
    }

    fn title(&self) -> String {
        "sign in".into()
    }

    /// A short form: a line, a field, a note.
    fn wish(&self, _cols: usize) -> (u32, u32) {
        (4, 3)
    }

    fn placed(&mut self, slot: SlotId) {
        self.slot = slot;
    }

    /// One verb, where a secret is wanted; none while connecting, signed in,
    /// or signing out — the worker drives those.
    fn verbs(&self) -> Vec<Verb> {
        match self.action() {
            Some((id, label)) => vec![Verb::run(id, label, Some('s'))],
            None => Vec::new(),
        }
    }

    fn run(&mut self, verb: &str, s: &mut Session) {
        if verb == VERB {
            self.send(s);
        }
    }

    fn as_any(&mut self) -> &mut dyn Any {
        self
    }
}

/// Its factory.
pub struct SignInKind;

impl PanelKind for SignInKind {
    fn tag(&self) -> Tag {
        SignIn::TAG
    }

    fn open(&self, id: &PanelId, cx: &mut Opening<'_>) -> Box<dyn Panel> {
        Box::new(SignIn {
            id: id.clone(),
            store: cx.session().store().clone(),
            slot: 0,
            field: String::new(),
        })
    }
}
