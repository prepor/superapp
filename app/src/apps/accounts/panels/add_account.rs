//! The add-account form, and the Gmail sign-in above it.
//!
//! Two doors to the same row. The four fields are one provider's app
//! password; the button is Google's consent, which cannot be typed into a
//! form at all — Google stopped accepting passwords on IMAP — and which takes
//! as long as a human takes.
//!
//! Listener setup and keychain access run on the blocking pool; browser consent
//! waits on Tokio. Registration holds one coordinator permit through credential
//! validation and the SQLite commit, so overlapping reconnects cannot downgrade
//! a working grant. The UI only submits work and observes completion.

use std::any::Any;
use std::sync::{Arc, Mutex, OnceLock};

use kernel::caps::SecretSet;
use kernel::layout::SlotId;
use kernel::panel::{Opening, Panel, PanelId, PanelKind, Tag, Verb};
use kernel::session::Session;

use crate::identity::accounts;
use crate::identity::oauth::{self, Signed};

/// What the consent task hands back.
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

/// Inputs stay outside SQL; the prepared result no longer holds a password.
enum Registration {
    Google {
        signed: Signed,
        expected: Option<i64>,
        mail: bool,
        calendar: bool,
    },
    Password(Form),
}

/// Native registration owns this permit until its SQLite commit is delivered.
/// Waiting for another registration uses Tokio, without occupying a worker.
struct PreparedRegistration {
    input: Registration,
    permit: Option<tokio::sync::OwnedMutexGuard<()>>,
}

struct CommittedRegistration {
    result: Result<Connected, String>,
    _permit: Option<tokio::sync::OwnedMutexGuard<()>>,
}

fn registrations() -> Arc<tokio::sync::Mutex<()>> {
    static SERIAL: OnceLock<Arc<tokio::sync::Mutex<()>>> = OnceLock::new();
    SERIAL
        .get_or_init(|| Arc::new(tokio::sync::Mutex::new(())))
        .clone()
}

struct GoogleAccess {
    existing: Option<i64>,
    before: (bool, bool),
    after: (bool, bool),
}

struct Connected {
    id: i64,
    email: String,
    imap: String,
    smtp: String,
    auth: String,
    access: GoogleAccess,
    line: String,
    password: bool,
}

type RegistrationResult = Arc<Mutex<Option<(String, bool, bool)>>>;

struct GoogleAccount {
    id: i64,
    auth: Option<String>,
    subject: Option<String>,
    mail: bool,
    calendar: bool,
    scopes: String,
}

fn google_access(
    db: &rusqlite::Connection,
    signed: &Signed,
    expected: Option<i64>,
    mail: bool,
    calendar: bool,
) -> Result<GoogleAccess, String> {
    use rusqlite::OptionalExtension;
    let held: Option<GoogleAccount> = db
        .query_row(
            "SELECT id, auth, google_sub, mail_enabled, calendar_enabled, scopes FROM account
         WHERE google_sub=?1 OR email=?2 COLLATE NOCASE ORDER BY google_sub=?1 DESC LIMIT 1",
            rusqlite::params![signed.subject, signed.email],
            |r| {
                Ok(GoogleAccount {
                    id: r.get(0)?,
                    auth: r.get(1)?,
                    subject: r.get(2)?,
                    mail: r.get(3)?,
                    calendar: r.get(4)?,
                    scopes: r.get(5)?,
                })
            },
        )
        .optional()
        .map_err(|e| e.to_string())?;
    if expected.is_some_and(|id| held.as_ref().is_none_or(|a| a.id != id)) {
        return Err(
            "signed into a different account; reconnect with the selected Google account".into(),
        );
    }
    let mut before = (false, false);
    if let Some(GoogleAccount {
        auth,
        subject,
        mail: old_mail,
        calendar: old_calendar,
        scopes,
        ..
    }) = &held
    {
        if auth.as_deref() != Some(oauth::GOOGLE.name) {
            return Err(format!("{} is already a password account", signed.email));
        }
        if subject.as_ref().is_some_and(|sub| sub != &signed.subject) {
            return Err("Google identity does not match the existing account".into());
        }
        let required =
            crate::identity::required_scopes(*old_mail || mail, *old_calendar || calendar);
        if required
            .split_whitespace()
            .chain(scopes.split_whitespace())
            .filter(|s| s.starts_with("https://"))
            .any(|scope| !signed.scopes.split_whitespace().any(|s| s == scope))
        {
            return Err("the new grant is missing access already used by this account; reconnect with all existing services enabled".into());
        }
        before = (*old_mail, *old_calendar);
    }
    let after = (mail || before.0, calendar || before.1);
    if crate::identity::required_scopes(after.0, after.1)
        .split_whitespace()
        .filter(|scope| scope.starts_with("https://"))
        .any(|scope| !signed.scopes.split_whitespace().any(|held| held == scope))
    {
        return Err("Google did not grant access to the selected services".into());
    }
    Ok(GoogleAccess {
        existing: held.map(|a| a.id),
        before,
        after,
    })
}

fn prepare_registration(
    world: &kernel::effect::World,
    mut input: Registration,
) -> Result<Registration, String> {
    if !world.store().is_writable() {
        return Err("this device must hold the write lease before connecting an account".into());
    }
    match &mut input {
        Registration::Google {
            signed,
            expected,
            mail,
            calendar,
        } => {
            google_access(world.store().conn(), signed, *expected, *mail, *calendar)?;
            world
                .run(&SecretSet {
                    key: &oauth::refresh_key(&signed.email),
                    secret: &signed.refresh,
                })
                .map_err(|_| "storing the Google grant failed")?;
            signed.refresh.clear();
        }
        Registration::Password(form) => {
            if accounts::account_for(world.store(), &form.email).is_some() {
                return Err(format!("{} is already here", form.email));
            }
            if !form.pass.is_empty() {
                world
                    .run(&SecretSet {
                        key: &form.email,
                        secret: &form.pass,
                    })
                    .map_err(|_| "the keychain refused the password")?;
            }
            form.pass.clear();
        }
    }
    Ok(input)
}

/// Rechecks identity and the enabled services in the committing transaction.
/// Another connection completed during keychain I/O cannot be overwritten.
fn commit_registration(
    db: &rusqlite::Transaction<'_>,
    input: Registration,
) -> rusqlite::Result<Result<Connected, String>> {
    match input {
        Registration::Google {
            signed,
            expected,
            mail,
            calendar,
        } => {
            let access = match google_access(db, &signed, expected, mail, calendar) {
                Ok(v) => v,
                Err(e) => return Ok(Err(e)),
            };
            let g = oauth::GOOGLE;
            let id = match access.existing {
                Some(id) => id,
                None => accounts::add_account_tx(db, &signed.email, g.imap, g.smtp, g.name)?,
            };
            db.execute(
                "UPDATE account SET email=?1,google_sub=?2,scopes=?3 WHERE id=?4",
                rusqlite::params![signed.email, signed.subject, signed.scopes, id],
            )?;
            crate::identity::set_services(db, id, access.after.0, access.after.1)?;
            let services = match access.after {
                (true, true) => "Mail + Calendar",
                (true, false) => "Mail",
                _ => "Calendar",
            };
            let line = format!("connected as {} · {services}", signed.email);
            Ok(Ok(Connected {
                id,
                email: signed.email,
                imap: g.imap.into(),
                smtp: g.smtp.into(),
                auth: g.name.into(),
                access,
                line,
                password: false,
            }))
        }
        Registration::Password(form) => {
            let exists: bool = db.query_row(
                "SELECT EXISTS(SELECT 1 FROM account WHERE email=?1 COLLATE NOCASE)",
                [&form.email],
                |r| r.get(0),
            )?;
            if exists {
                return Ok(Err(format!("{} is already here", form.email)));
            }
            let id =
                accounts::add_account_tx(db, &form.email, &form.imap, &form.smtp, oauth::PASSWORD)?;
            let line = format!("{} added — syncing", form.email);
            Ok(Ok(Connected {
                id,
                email: form.email,
                imap: form.imap,
                smtp: form.smtp,
                auth: oauth::PASSWORD.into(),
                access: GoogleAccess {
                    existing: None,
                    before: (false, false),
                    after: (true, false),
                },
                line,
                password: true,
            }))
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
    starting: Option<tokio::sync::oneshot::Receiver<Result<oauth::Flow, String>>>,
    signin_wake: Option<Arc<dyn Fn() + Send + Sync>>,
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
    registration_result: RegistrationResult,
    saving: bool,
    notify: Option<Arc<dyn Fn() + Send + Sync>>,
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

    /// Observes consent, credential preparation and SQLite commit results.
    /// Called by the widget on every event; no native work happens here.
    pub fn observe(&mut self, s: &mut Session) {
        if let Some((line, error)) = self.take_registration_result() {
            self.say(line, error);
            s.redraw();
        }
        if let Some(starting) = &mut self.starting {
            match starting.try_recv() {
                Ok(Ok(flow)) => {
                    self.starting = None;
                    let wake = self.signin_wake.take().expect("sign-in waker");
                    self.begin_flow(flow, wake);
                    s.redraw();
                }
                Ok(Err(error)) => {
                    self.starting = None;
                    self.signin_wake = None;
                    self.say(error.clone(), true);
                    s.notify(error, true);
                    s.redraw();
                }
                Err(tokio::sync::oneshot::error::TryRecvError::Empty) => return,
                Err(tokio::sync::oneshot::error::TryRecvError::Closed) => {
                    self.starting = None;
                    self.signin_wake = None;
                    self.say("could not prepare Google sign-in", true);
                    s.redraw();
                }
            }
        }
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

    pub fn set_waker(&mut self, wake: Arc<dyn Fn() + Send + Sync>) {
        self.notify = Some(wake);
    }

    fn finish(&mut self, s: &mut Session, done: Result<Signed, String>) -> (String, bool) {
        match done {
            Ok(signed) => self.register(
                s,
                Registration::Google {
                    signed,
                    expected: self.expected,
                    mail: self.mail,
                    calendar: self.calendar,
                },
            ),
            Err(error) => {
                s.notify(error.clone(), true);
                (error, true)
            }
        }
    }

    fn add(&mut self, s: &mut Session) {
        let mut form = self.form.clone();
        form.email = form.email.trim().to_owned();
        form.imap = form.imap.trim().to_owned();
        form.smtp = form.smtp.trim().to_owned();
        if form.email.is_empty() {
            s.notify("no address", true);
            return;
        }
        let (line, error) = self.register(s, Registration::Password(form));
        self.say(line, error);
    }

    fn register(&mut self, s: &mut Session, input: Registration) -> (String, bool) {
        if self.saving {
            return ("an account is already connecting".into(), false);
        }
        if !s.writable() {
            return (
                "this device must hold the write lease before connecting an account".into(),
                true,
            );
        }
        self.saving = true;
        if s.store().ui_attached() {
            let Some(factory) = s.world().factory() else {
                self.saving = false;
                return (
                    "this world cannot prepare an account connection".into(),
                    true,
                );
            };
            let result = self.registration_result.clone();
            s.prepare_work(
                move |_| {
                    Box::pin(async move {
                        let permit = registrations().lock_owned().await;
                        kernel::runtime::spawn_blocking(move || {
                            let input = factory
                                .build()
                                .map_err(|e| e.to_string())
                                .and_then(|world| prepare_registration(&world, input))?;
                            Ok(PreparedRegistration {
                                input,
                                permit: Some(permit),
                            })
                        })
                        .await
                        .unwrap_or_else(|e| Err(format!("account preparation stopped: {e}")))
                    })
                },
                move |s, prepared| match prepared {
                    Ok(plan) => Self::commit(s, plan, result),
                    Err(error) => {
                        *result.lock().expect("account result") =
                            Some((error.clone(), true, false));
                        s.notify(error, true);
                        s.redraw();
                    }
                },
            );
            ("connecting account…".into(), false)
        } else {
            match prepare_registration(s.world(), input) {
                Ok(input) => {
                    Self::commit(
                        s,
                        PreparedRegistration {
                            input,
                            permit: None,
                        },
                        self.registration_result.clone(),
                    );
                    self.take_registration_result()
                        .unwrap_or_else(|| ("connecting account…".into(), false))
                }
                Err(error) => {
                    self.saving = false;
                    s.notify(error.clone(), true);
                    (error, true)
                }
            }
        }
    }

    fn commit(s: &mut Session, plan: PreparedRegistration, result: RegistrationResult) {
        let (kind, label) = match &plan.input {
            Registration::Google { .. } => {
                ("accounts.connect", "connect Google services".to_string())
            }
            Registration::Password(form) => ("account", format!("add account {}", form.email)),
        };
        s.act_async(
            kernel::session::Edit::writing(kind, label, move |tx| {
                let result = commit_registration(tx, plan.input)?;
                Ok(CommittedRegistration {
                    result,
                    _permit: plan.permit,
                })
            })
            .record_if(|done| done.result.is_ok()),
            move |s, committed| {
                // Keep the permit through history and result publication as well.
                let (committed, _permit) = match committed {
                    Some(done) => (Some(done.result), done._permit),
                    None => (None, None),
                };
                let outcome = match committed {
                    Some(Ok(done)) => {
                        if done.access.existing.is_none() {
                            s.claim(Box::new(crate::identity::history::AccountAdded {
                                id: done.id,
                                email: done.email.clone(),
                                imap: done.imap,
                                smtp: done.smtp,
                                auth: done.auth,
                                services: Mutex::new(None),
                            }));
                        } else if done.access.before != done.access.after {
                            s.claim(Box::new(crate::identity::ServicesChanged {
                                account: done.id,
                                before: done.access.before,
                                after: done.access.after,
                            }));
                        }
                        s.notify(
                            if done.password {
                                done.line.clone()
                            } else {
                                format!("{} connected", done.email)
                            },
                            false,
                        );
                        (done.line, false, done.password)
                    }
                    Some(Err(error)) => {
                        s.notify(error.clone(), true);
                        (error, true, false)
                    }
                    None => ("could not save account access".into(), true, false),
                };
                *result.lock().expect("account result") = Some(outcome);
                s.redraw();
            },
        );
    }

    fn take_registration_result(&mut self) -> Option<(String, bool)> {
        let result = self
            .registration_result
            .lock()
            .ok()
            .and_then(|mut r| r.take());
        result.map(|(line, error, clear)| {
            self.saving = false;
            if clear {
                self.form.email.clear();
                self.form.pass.clear();
                self.cleared = true;
            }
            (line, error)
        })
    }

    /// Prepares Google's listener off the UI before opening the browser.
    /// The widget supplies the waker used to publish each async completion.
    fn start_google(&mut self, s: &mut Session, wake: Arc<dyn Fn() + Send + Sync>) {
        self.notify = Some(wake.clone());
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
        if self.signin.is_some() || self.starting.is_some() || self.saving {
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
        let directory = s.db_dir().map(std::path::Path::to_path_buf);
        let (send, receive) = tokio::sync::oneshot::channel();
        self.starting = Some(receive);
        self.signin_wake = Some(wake.clone());
        kernel::runtime::spawn_blocking(move || {
            let started = directory
                .as_deref()
                .ok_or_else(|| "no store file — accounts need one".to_string())
                .and_then(oauth::Client::load)
                .and_then(|client| oauth::Flow::start(client, oauth::GOOGLE))
                .map(|flow| flow.scopes(scopes));
            let _ = send.send(started);
            wake();
        });
        self.say("preparing Google sign-in…", false);
        s.redraw();
    }

    fn begin_flow(&mut self, flow: oauth::Flow, wake: Arc<dyn Fn() + Send + Sync>) {
        self.open_url = Some(flow.url());
        let slot: Slot = Arc::new(Mutex::new(None));
        let into = slot.clone();
        kernel::runtime::spawn_local(move || async move {
            let result = flow.wait().await;
            if let Ok(mut value) = into.lock() {
                *value = Some(result);
            }
            wake();
        });
        self.signin = Some(slot);
        self.say("waiting for google in the browser…", false);
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
            starting: None,
            signin_wake: None,
            open_url: None,
            want_google: false,
            cleared: false,
            mail: mail || id.args.get(1).is_some_and(|a| a == "mail"),
            calendar: calendar || id.args.get(1).is_some_and(|a| a == "calendar"),
            expected,
            registration_result: Arc::new(Mutex::new(None)),
            saving: false,
            notify: None,
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
    fn native_registration_keeps_ui_free_and_serializes_overlapping_grants() {
        use kernel::app::{world_for, Apps, Env, Mode, Workers};
        use kernel::caps::{MemSecrets, Secrets, SecretsFactory};
        use kernel::store::Store;
        use std::rc::Rc;
        use std::sync::{
            atomic::{AtomicBool, Ordering},
            mpsc, Condvar,
        };
        use std::time::{Duration, Instant};

        struct SlowSecrets {
            values: MemSecrets,
            armed: Arc<AtomicBool>,
            started: mpsc::Sender<std::thread::ThreadId>,
            gate: Arc<(Mutex<bool>, Condvar)>,
        }
        impl Secrets for SlowSecrets {
            fn get(&mut self, key: &str) -> Option<String> {
                self.values.get(key)
            }
            fn set(&mut self, key: &str, value: &str) -> bool {
                if self.armed.swap(false, Ordering::AcqRel) {
                    let _ = self.started.send(std::thread::current().id());
                    let (lock, changed) = &*self.gate;
                    let released = changed
                        .wait_timeout_while(lock.lock().unwrap(), Duration::from_secs(3), |open| {
                            !*open
                        })
                        .unwrap();
                    if !*released.0 {
                        return false;
                    }
                }
                self.values.set(key, value)
            }
        }
        let values = MemSecrets::new();
        let armed = Arc::new(AtomicBool::new(false));
        let gate = Arc::new((Mutex::new(false), Condvar::new()));
        let (started, starts) = mpsc::channel();
        let backend = {
            let values = values.clone();
            let armed = armed.clone();
            let gate = gate.clone();
            SecretsFactory::new(move || {
                Box::new(SlowSecrets {
                    values: values.clone(),
                    armed: armed.clone(),
                    started: started.clone(),
                    gate: gate.clone(),
                })
            })
        };
        let env = Env {
            secrets_backend: Some(backend),
            ..Env::default()
        };
        let apps = Apps::new(APPS);
        let store = Store::open(None, &apps.schemas()).unwrap();
        let world = Rc::new(world_for(APPS, store, Mode::Fake, &env));
        let workers = Workers::none(world.store().clone());
        let mut s = Session::new(apps, world, workers, Mode::Fake);
        assert!(
            !connect(
                &mut s,
                None,
                false,
                true,
                signed("me@example.com", "g1", false, true, "initial")
            )
            .1
        );
        let account = accounts::account_for(s.store(), "me@example.com")
            .unwrap()
            .id;
        let mut panels = Vec::new();
        let mut slots = Vec::new();
        for id in [
            AddAccount::for_account(account),
            AddAccount::for_service(account, false),
        ] {
            s.act(Action::new("test.open", "open account").moving(move |wm| {
                wm.open(id, None, true);
            }));
            s.settle();
            panels.push(s.panel(s.focus().unwrap()).unwrap());
            slots.push(s.focus().unwrap());
        }
        s.store().attach_ui(|| {});
        armed.store(true, Ordering::Release);
        let start = Instant::now();
        {
            let mut panel = panels[0].borrow_mut();
            let form = panel.as_any().downcast_mut::<AddAccount>().unwrap();
            form.services(true, true);
            assert!(
                !form
                    .finish(
                        &mut s,
                        Ok(signed("me@example.com", "g1", true, true, "full-grant"))
                    )
                    .1
            );
        }
        assert!(
            start.elapsed() < Duration::from_secs(1),
            "the UI waited for the keychain"
        );
        assert_ne!(
            starts.recv_timeout(Duration::from_secs(2)).unwrap(),
            std::thread::current().id()
        );
        {
            let mut panel = panels[1].borrow_mut();
            let form = panel.as_any().downcast_mut::<AddAccount>().unwrap();
            form.services(false, true);
            assert!(
                !form
                    .finish(
                        &mut s,
                        Ok(signed("me@example.com", "g1", false, true, "stale-grant"))
                    )
                    .1
            );
        }
        {
            // Accepted registration belongs to the session even after both
            // forms close. Shutdown must deliver A's edit before waiting for
            // B, whose narrower grant waits for the same coordinator permit.
            for slot in slots {
                s.nav(kernel::nav::Nav::Close { slot, label: None });
            }
            let (lock, changed) = &*gate;
            *lock.lock().unwrap() = true;
            changed.notify_all();
        }
        s.shutdown();
        for panel in &panels {
            let mut panel = panel.borrow_mut();
            let form = panel.as_any().downcast_mut::<AddAccount>().unwrap();
            form.observe(&mut s);
            assert!(!form.saving, "shutdown did not finish registration");
        }
        assert_eq!(
            s.world()
                .run(&SecretGet(&oauth::refresh_key("me@example.com")))
                .unwrap()
                .as_deref(),
            Some("full-grant")
        );
        let (mail, calendar, _) = crate::identity::services(s.store().conn(), account);
        assert!(mail && calendar);
        let mut rejected = panels[1].borrow_mut();
        let rejected = rejected.as_any().downcast_mut::<AddAccount>().unwrap();
        assert!(rejected.google_line().unwrap().1);
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
