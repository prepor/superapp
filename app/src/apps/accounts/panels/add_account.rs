//! The add-account form, and the Gmail sign-in above it.
//!
//! Two doors to the same row. The four fields are one provider's app
//! password; the button is Google's consent, which cannot be typed into a
//! form at all — Google stopped accepting passwords on IMAP — and which takes
//! as long as a human takes.
//!
//! The consent is split in two on purpose: binding the loopback listener and
//! minting the PKCE pair are instant and must happen before the browser opens
//! (a redirect to a closed port is lost), and waiting for a human is not
//! something the UI thread may do. So the blocking half runs on a thread and
//! drops its answer in [`AddAccount::signin`]; the panel picks it up on the
//! next poll and writes the row on the thread that owns the store.

use std::any::Any;
use std::sync::{Arc, Mutex};

use kernel::caps::SecretSet;
use kernel::layout::SlotId;
use kernel::panel::{Opening, Panel, PanelId, PanelKind, Tag, Verb};
use kernel::session::Session;

use super::Settings;
use crate::identity::accounts;
use crate::identity::oauth::{self, Signed};

/// What a sign-in thread hands back.
type Slot = Arc<Mutex<Option<Result<Signed, String>>>>;

/// The four fields, as the panel holds them.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Form {
    pub email: String,
    pub pass: String,
    pub imap: String,
    pub smtp: String,
}

impl Form {
    /// What a fresh form comes up with: one provider's hosts, because a form
    /// with two empty host fields is a quiz.
    #[must_use]
    pub fn fresh() -> Form {
        Form {
            imap: "imap.fastmail.com".into(),
            smtp: "smtp.fastmail.com".into(),
            ..Form::default()
        }
    }
}

/// The form panel.
pub struct AddAccount {
    id: PanelId,
    slot: SlotId,
    /// The text as the fields have it. The widget hands every change over,
    /// so the bar's *add* has the values without reaching for a widget.
    form: Form,
    /// The one line the Google flow speaks through: what it is waiting for,
    /// who signed in, or why it could not. `true` marks it as a failure.
    google: Option<(String, bool)>,
    /// A sign-in waiting on the browser, if one is out.
    signin: Option<Slot>,
    /// The consent page the widget should open, once.
    open_url: Option<String>,
    /// The bar asked for a sign-in. The bar has no waker to give and cannot
    /// open a browser, so the flow is started by the widget on its next
    /// event — see [`AddAccount::take_google`].
    want_google: bool,
    /// A row was added and the fields should be emptied. Said once, like the
    /// URL: the widget owns the text, and a standing "the form is empty"
    /// would wipe what is being typed into it.
    cleared: bool,
    pub mail: bool,
    pub calendar: bool,
    expected: Option<i64>,
}

impl AddAccount {
    pub fn for_account(id: i64) -> PanelId {
        PanelId::new(Self::TAG, [id.to_string()])
    }
    pub fn for_service(id: i64, calendar: bool) -> PanelId {
        PanelId::new(
            Self::TAG,
            [
                id.to_string(),
                if calendar { "calendar" } else { "mail" }.into(),
            ],
        )
    }
    pub fn services(&mut self, mail: bool, calendar: bool) {
        self.mail = mail;
        self.calendar = calendar;
    }
    pub const TAG: Tag = Tag("add_account");

    /// The identity of the one add-account panel.
    #[must_use]
    pub fn id() -> PanelId {
        PanelId::bare(Self::TAG)
    }

    /// The text the fields show.
    #[must_use]
    pub fn form(&self) -> &Form {
        &self.form
    }

    /// A field changed: the panel keeps the text, so the bar's *add* has it.
    /// Not an action — typing is the future editor's local undo, not the
    /// workspace's — and no row is written until the button is pressed.
    pub fn edited(&mut self, f: Form) {
        self.form = f;
    }

    /// What the Google row says, if anything.
    #[must_use]
    pub fn google_line(&self) -> Option<&(String, bool)> {
        self.google.as_ref()
    }

    /// The consent page to open, taken. The widget is the only thing that can
    /// open a browser, so the panel hands it the URL rather than the act.
    pub fn take_url(&mut self) -> Option<String> {
        self.open_url.take()
    }

    /// Whether the bar asked for a sign-in since the last look, taken.
    pub fn take_google(&mut self) -> bool {
        std::mem::take(&mut self.want_google)
    }

    /// Whether the address and the password should be emptied, taken.
    pub fn take_cleared(&mut self) -> bool {
        std::mem::take(&mut self.cleared)
    }

    /// Puts one line on the Google row.
    fn say(&mut self, line: impl Into<String>, err: bool) {
        self.google = Some((line.into(), err));
    }

    /// Picks up a finished sign-in, if there is one: the grant goes to the
    /// keychain and the account row is written here, on the thread that owns
    /// the store. Called by the widget on every event, which is what makes a
    /// consent that took two minutes land without a restart.
    pub fn observe(&mut self, s: &mut Session) {
        let Some(slot) = self.signin.as_ref() else {
            return;
        };
        let Some(done) = slot.lock().ok().and_then(|mut g| g.take()) else {
            return;
        };
        self.signin = None;
        let (line, err) = self.finish(s, done);
        self.say(line, err);
        s.redraw();
    }

    /// What one finished sign-in comes to.
    fn finish(&mut self, s: &mut Session, done: Result<Signed, String>) -> (String, bool) {
        let signed = match done {
            Ok(v) => v,
            Err(e) => {
                s.notify(e.clone(), true);
                return (e, true);
            }
        };
        let held = s
            .store()
            .conn()
            .query_row(
                "SELECT id FROM account WHERE google_sub=?1",
                [&signed.subject],
                |r| r.get::<_, i64>(0),
            )
            .ok()
            .and_then(|id| {
                accounts::accounts(s.store())
                    .iter()
                    .find(|a| a.id == id)
                    .cloned()
            })
            .or_else(|| accounts::account_for(s.store(), &signed.email));
        if self
            .expected
            .is_some_and(|id| held.as_ref().is_none_or(|a| a.id != id))
        {
            return (
                "signed into a different account; reconnect with the selected Google account"
                    .into(),
                true,
            );
        }
        if let Some(a) = &held {
            if !a.oauth() {
                return (
                    format!("{} is already a password account", signed.email),
                    true,
                );
            }
            let subject = s
                .store()
                .conn()
                .query_row("SELECT google_sub FROM account WHERE id=?", [a.id], |r| {
                    r.get::<_, Option<String>>(0)
                })
                .ok()
                .flatten();
            if subject.is_some_and(|sub| sub != signed.subject) {
                return (
                    "Google identity does not match the existing account".into(),
                    true,
                );
            }
            let (mail, calendar, scopes) = crate::identity::services(s.store().conn(), a.id);
            let required =
                crate::identity::required_scopes(mail || self.mail, calendar || self.calendar);
            if required
                .split_whitespace()
                .chain(scopes.split_whitespace())
                .filter(|x| x.starts_with("https://"))
                .any(|scope| !signed.scopes.split_whitespace().any(|s| s == scope))
            {
                return ("the new grant is missing access already used by this account; reconnect with all existing services enabled".into(),true);
            }
        }
        if !s.writable() {
            return (
                "this device must hold the write lease before connecting an account".into(),
                true,
            );
        }
        let existing = held.as_ref().map(|a| a.id);
        let (was_mail, was_calendar, _) = existing
            .map(|id| crate::identity::services(s.store().conn(), id))
            .unwrap_or_default();
        let mail = self.mail || was_mail;
        let calendar = self.calendar || was_calendar;
        if crate::identity::required_scopes(mail, calendar)
            .split_whitespace()
            .filter(|scope| scope.starts_with("https://"))
            .any(|scope| !signed.scopes.split_whitespace().any(|held| held == scope))
        {
            return (
                "Google did not grant access to the selected services".into(),
                true,
            );
        }
        if s.world()
            .run(&SecretSet {
                key: &oauth::refresh_key(&signed.email),
                secret: &signed.refresh,
            })
            .is_err()
        {
            return ("storing the Google grant failed".into(), true);
        }
        let g = oauth::GOOGLE;
        let email = signed.email.clone();
        let subject = signed.subject;
        let scopes = signed.scopes;
        // Service flags and identity land together, before any Mail worker
        // can start against a newly connected Calendar-only account.
        let Some(id) = s.act(kernel::session::Action::writing(
            "accounts.connect",
            "connect Google services",
            move |c| {
                let id = match existing {
                    Some(id) => id,
                    None => accounts::add_account_tx(c, &email, g.imap, g.smtp, g.name)?,
                };
                c.execute(
                    "UPDATE account SET email=?1,google_sub=?2,scopes=?3 WHERE id=?4",
                    rusqlite::params![email, subject, scopes, id],
                )?;
                crate::identity::set_services(c, id, mail, calendar)?;
                Ok(id)
            },
        )) else {
            return ("could not save Google service access".into(), true);
        };
        if existing.is_none() {
            s.claim(Box::new(crate::identity::history::AccountAdded {
                id,
                email: signed.email.clone(),
                imap: g.imap.into(),
                smtp: g.smtp.into(),
                auth: g.name.into(),
                services: Mutex::new(None),
            }));
        } else if (mail, calendar) != (was_mail, was_calendar) {
            s.claim(Box::new(crate::identity::ServicesChanged {
                account: id,
                before: (was_mail, was_calendar),
                after: (mail, calendar),
            }));
        }
        s.workers().kick_all();
        s.notify(format!("{} connected", signed.email), false);
        (
            format!(
                "connected as {} · {}",
                signed.email,
                match (mail, calendar) {
                    (true, true) => "Mail + Calendar",
                    (true, false) => "Mail",
                    _ => "Calendar",
                }
            ),
            false,
        )
    }

    /// The form's own door: one row from four fields, with the password in
    /// the keychain and never in the store.
    fn add(&mut self, s: &mut Session) {
        let f = self.form.clone();
        let email = f.email.trim().to_string();
        if email.is_empty() {
            s.notify("no address", true);
            return;
        }
        if accounts::account_for(s.store(), &email).is_some() {
            s.notify(format!("{email} is already here"), true);
            return;
        }
        if !f.pass.is_empty()
            && s.world()
                .run(&SecretSet {
                    key: &email,
                    secret: &f.pass,
                })
                .is_err()
        {
            s.notify("the keychain refused the password", true);
            return;
        }
        let id = Settings::add(s, &email, f.imap.trim(), f.smtp.trim(), oauth::PASSWORD);
        if id != 0 {
            // The password is gone from the panel with the row it made; the
            // hosts stay, because the next account is usually the same
            // provider.
            self.form.email.clear();
            self.form.pass.clear();
            self.cleared = true;
            s.notify(format!("{email} added — syncing"), false);
        }
    }

    /// Opens the browser on Google's consent page and puts the flow's
    /// blocking half on a thread. `wake` is what tells the UI thread that the
    /// answer has landed — the panel names no Makepad, so the widget supplies
    /// it.
    fn start_google(&mut self, s: &mut Session, wake: Arc<dyn Fn() + Send + Sync>) {
        // A script never leaves for a browser: the consent round trip needs
        // Google and a human, and a suite that opened Safari would be neither
        // headless nor reproducible. What a script *can* prove is everything
        // up to that door, so the refusal speaks on the same line a real
        // failure would. A world whose servers are the fake ones is exactly
        // that world.
        if !s.world().caps(|c| c.has::<super::super::GoogleConsent>()) {
            self.say("sign-in needs a real run, not a script", true);
            s.redraw();
            return;
        }
        // One at a time. A second press would orphan the first listener and
        // burn the consent it is still waiting for.
        if self.signin.is_some() {
            self.say("a sign-in is already waiting", false);
            s.redraw();
            return;
        }
        if !self.mail && !self.calendar {
            self.say("choose Mail or Calendar before signing in", true);
            return;
        }
        let mut scopes = crate::identity::required_scopes(self.mail, self.calendar);
        if let Some(id) = self.expected {
            let (mail, calendar, held) = crate::identity::services(s.store().conn(), id);
            scopes = crate::identity::required_scopes(mail || self.mail, calendar || self.calendar);
            for scope in held.split_whitespace() {
                if !scopes.split_whitespace().any(|s| s == scope) {
                    scopes.push(' ');
                    scopes.push_str(scope);
                }
            }
        }
        // Where the client registration lives: beside the store, which is
        // also where the keychain fallback writes.
        let started = s
            .db_dir()
            .ok_or_else(|| "no store file — accounts need one".to_string())
            .and_then(oauth::Client::load)
            .and_then(|c| oauth::Flow::start(c, oauth::GOOGLE))
            .map(|f| f.scopes(scopes));
        let flow = match started {
            Ok(f) => f,
            Err(e) => {
                s.notify(e.clone(), true);
                self.say(e, true);
                s.redraw();
                return;
            }
        };
        let url = flow.url();
        let slot: Slot = Arc::new(Mutex::new(None));
        let into = slot.clone();
        if let Err(e) = std::thread::Builder::new()
            .name("google-signin".into())
            .spawn(move || {
                let r = flow.wait();
                if let Ok(mut g) = into.lock() {
                    *g = Some(r);
                }
                wake();
            })
        {
            let line = format!("could not start the sign-in: {e}");
            s.notify(line.clone(), true);
            self.say(line, true);
            s.redraw();
            return;
        }
        self.signin = Some(slot);
        self.open_url = Some(url);
        self.say("waiting for google in the browser…", false);
        s.redraw();
    }

    /// The widget's own door to the sign-in, because only it can hand over a
    /// waker and open a browser.
    pub fn google(&mut self, s: &mut Session, wake: Arc<dyn Fn() + Send + Sync>) {
        self.start_google(s, wake);
    }
}

impl Panel for AddAccount {
    fn id(&self) -> &PanelId {
        &self.id
    }

    fn title(&self) -> String {
        "add account".into()
    }

    /// The two doors to one row.
    fn about(&self) -> String {
        "The form that adds an account, and Google's consent above it: two \
         doors to one row of `account`. The four fields are a label, an \
         address and the two host fields — prefilled, because a form with two \
         empty host fields is a quiz — and *add* writes the row and files the \
         password to the keychain, refusing a blank address or one already \
         present. It takes no arguments and reads nothing; *sign in with \
         google* does the same thing through an OAuth grant instead, which a \
         scripted run refuses in one line."
            .into()
    }

    /// The form is compact: four labelled fields and the Google row above
    /// them.
    fn wish(&self, _cols: usize) -> (u32, u32) {
        (4, 3)
    }

    fn placed(&mut self, slot: SlotId) {
        self.slot = slot;
    }

    /// Two buttons: the form's own, and Google's. Both act on what the panel
    /// shows, so both are buttons rather than links.
    fn verbs(&self) -> Vec<Verb> {
        vec![
            Verb::run("mail.add", "add", Some('a')),
            Verb::run("mail.google", "sign in with google", Some('g')),
        ]
    }

    fn run(&mut self, verb: &str, s: &mut Session) {
        match verb {
            "mail.add" => self.add(s),
            // The bar has no waker to give and cannot open a browser, so a
            // press or a chord only asks: the widget starts the flow on its
            // next event, with a waker of its own.
            "mail.google" => {
                self.want_google = true;
                s.redraw();
            }
            _ => {}
        }
    }

    fn as_any(&mut self) -> &mut dyn Any {
        self
    }
}

/// Its factory.
pub struct AddAccountKind;

impl PanelKind for AddAccountKind {
    fn tag(&self) -> Tag {
        AddAccount::TAG
    }

    fn open(&self, id: &PanelId, _cx: &mut Opening<'_>) -> Box<dyn Panel> {
        let expected = id.args.first().and_then(|id| id.parse().ok());
        let (mail, calendar) = expected
            .map(|id| {
                let (m, c, _) = crate::identity::services(_cx.session().store().conn(), id);
                (m, c)
            })
            .unwrap_or((true, true));
        Box::new(AddAccount {
            id: id.clone(),
            slot: 0,
            form: Form::fresh(),
            google: None,
            signin: None,
            open_url: None,
            want_google: false,
            cleared: false,
            mail: mail || id.args.get(1).is_some_and(|a| a == "mail"),
            calendar: calendar || id.args.get(1).is_some_and(|a| a == "calendar"),
            expected,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use kernel::{app::App, caps::SecretGet, session::Action};
    static APPS: &[&dyn App] = &[&crate::apps::accounts::ACCOUNTS];
    fn connect(
        s: &mut Session,
        expected: Option<i64>,
        mail: bool,
        calendar: bool,
        signed: Signed,
    ) -> (String, bool) {
        let id = expected
            .map(AddAccount::for_account)
            .unwrap_or_else(AddAccount::id);
        s.act(Action::new("test.open", "open account").moving(move |wm| {
            wm.open(id, None, true);
        }));
        s.settle();
        let p = s.panel(s.focus().unwrap()).unwrap();
        let mut p = p.borrow_mut();
        let p = p.as_any().downcast_mut::<AddAccount>().unwrap();
        p.services(mail, calendar);
        p.finish(s, Ok(signed))
    }
    fn signed(email: &str, subject: &str, mail: bool, calendar: bool, token: &str) -> Signed {
        Signed {
            email: email.into(),
            subject: subject.into(),
            scopes: crate::identity::required_scopes(mail, calendar),
            refresh: token.into(),
        }
    }
    #[test]
    fn calendar_only_connection_and_reconnect_preserve_identity_and_services() {
        let mut s = Session::fake(APPS);
        let result = connect(
            &mut s,
            None,
            false,
            true,
            signed("me@example.com", "google-1", false, true, "first-grant"),
        );
        assert!(!result.1, "{}", result.0);
        let account = accounts::account_for(s.store(), "me@example.com").unwrap();
        assert!(!crate::identity::services(s.store().conn(), account.id).0);
        assert!(crate::identity::services(s.store().conn(), account.id).1);
        let result = connect(
            &mut s,
            Some(account.id),
            true,
            true,
            signed("renamed@example.com", "google-1", true, true, "next-grant"),
        );
        assert!(!result.1, "{}", result.0);
        let next = accounts::account_for(s.store(), "renamed@example.com").unwrap();
        assert_eq!(account.id, next.id);
        assert_eq!(accounts::accounts(s.store()).len(), 1);
        assert_eq!(
            s.world()
                .run(&SecretGet(&oauth::refresh_key(&next.email)))
                .unwrap()
                .as_deref(),
            Some("next-grant")
        );
    }
    #[test]
    fn rejected_identity_and_partial_consent_do_not_replace_a_working_grant() {
        let mut s = Session::fake(APPS);
        assert!(
            !connect(
                &mut s,
                None,
                true,
                true,
                signed("me@example.com", "google-1", true, true, "keep-me")
            )
            .1
        );
        let id = accounts::account_for(s.store(), "me@example.com")
            .unwrap()
            .id;
        assert!(
            connect(
                &mut s,
                Some(id),
                true,
                true,
                signed("other@example.com", "google-2", true, true, "wrong-person")
            )
            .1
        );
        assert!(
            connect(
                &mut s,
                Some(id),
                false,
                true,
                signed("me@example.com", "google-1", false, true, "partial-grant")
            )
            .1
        );
        assert_eq!(
            s.world()
                .run(&SecretGet(&oauth::refresh_key("me@example.com")))
                .unwrap()
                .as_deref(),
            Some("keep-me")
        );
        assert_eq!(accounts::accounts(s.store()).len(), 1);
    }
    #[test]
    fn adding_shared_account_undo_redo_keeps_scope_and_google_subject() {
        let mut s = Session::fake(APPS);
        assert!(
            !connect(
                &mut s,
                None,
                false,
                true,
                signed("me@example.com", "google-1", false, true, "grant")
            )
            .1
        );
        let id = accounts::account_for(s.store(), "me@example.com")
            .unwrap()
            .id;
        assert!(s.undo());
        assert!(accounts::accounts(s.store()).is_empty());
        assert!(s.redo());
        let next = accounts::account_for(s.store(), "me@example.com").unwrap();
        assert_eq!(id, next.id);
        let (mail, calendar, scopes) = crate::identity::services(s.store().conn(), id);
        assert!(!mail && calendar);
        assert!(scopes.contains("calendar.events"));
        assert_eq!(
            s.store()
                .conn()
                .query_row("SELECT google_sub FROM account WHERE id=?", [id], |r| {
                    r.get::<_, String>(0)
                })
                .unwrap(),
            "google-1"
        );
    }
}
