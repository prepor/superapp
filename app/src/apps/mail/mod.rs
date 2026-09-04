//! The mail app: mailboxes, messages, and drafts over the demo seed and a
//! fake server.
//!
//! What it adds to the shell is two halves and nothing else: the kernel's
//! [`App`] — ten panel kinds, a schema ladder, a demo seed, four deferred
//! effects, three capabilities of its own, a search source, a problem source,
//! a worker per account plus the sender, and its roots for the launcher — and
//! the shell's [`AppUi`](crate::shell::app_ui::AppUi), which is [`ui`] and the
//! widgets under [`widgets`].
//!
//! A window's own run reaches real servers ([`real`]); every scripted run,
//! every test and every library mount gets [`caps::FakeServers`] — see
//! [`caps::install`].

use std::any::Any;

use kernel::app::{App, Capabilities, Env, Mode, ProblemSource, Root, Schema, Worker};
use kernel::effect::Registry;
use kernel::panel::PanelKind;
use kernel::store::Store;

pub mod accounts;
pub mod caps;
pub mod carry;
pub mod effects;
pub mod html;
pub mod model;
pub mod panels;
pub mod oauth;
pub mod parts;
pub mod problems;
pub mod real;
pub mod reading;
pub mod recipients;
pub mod scenes;
pub mod schema;
pub mod search;
pub mod seed;
pub mod sync;
pub mod ui;
pub mod widgets;

#[cfg(test)]
mod tests;

pub use model::{Role, Seed};
pub use panels::{Compose, Settings};
pub use ui::UI;

/// The app.
pub struct Mail;

/// The one in this build.
pub static MAIL: Mail = Mail;

static INBOX_KIND: panels::MailboxKind = panels::MailboxKind(Role::Inbox);
static ARCHIVE_KIND: panels::MailboxKind = panels::MailboxKind(Role::Archive);
static SENT_KIND: panels::MailboxKind = panels::MailboxKind(Role::Sent);
static SPAM_KIND: panels::MailboxKind = panels::MailboxKind(Role::Spam);
static MESSAGE_KIND: panels::MessageKind = panels::MessageKind;
static COMPOSE_KIND: panels::ComposeKind = panels::ComposeKind;
static CONTACT_KIND: panels::ContactKind = panels::ContactKind;
static SETTINGS_KIND: panels::SettingsKind = panels::SettingsKind;
static ADD_ACCOUNT_KIND: panels::AddAccountKind = panels::AddAccountKind;
static CARD_KIND: panels::CardKind = panels::CardKind;

static KINDS: &[&dyn PanelKind] = &[
    &INBOX_KIND,
    &ARCHIVE_KIND,
    &SENT_KIND,
    &SPAM_KIND,
    &MESSAGE_KIND,
    &COMPOSE_KIND,
    &CONTACT_KIND,
    &SETTINGS_KIND,
    &ADD_ACCOUNT_KIND,
    &CARD_KIND,
];

static FAILING_ACCOUNTS: problems::FailingAccounts = problems::FailingAccounts;
static FAILING_SENDS: problems::FailingSends = problems::FailingSends;
/// Accounts first, then sends: an account that cannot reach its server is
/// why its letters are not moving.
static SOURCES: &[&dyn ProblemSource] = &[&FAILING_ACCOUNTS, &FAILING_SENDS];

impl App for Mail {
    fn id(&self) -> &'static str {
        "mail"
    }

    fn kinds(&self) -> &'static [&'static dyn PanelKind] {
        KINDS
    }

    fn schema(&self) -> Option<&'static Schema> {
        Some(&schema::SCHEMA)
    }

    fn seed(&self, store: &Store, mode: Mode) -> rusqlite::Result<()> {
        seed::seed_if_empty(store, mode)
    }

    fn effects(&self, reg: &mut Registry) {
        effects::register(reg);
    }

    fn outside(&self, mode: Mode, env: &Env, caps: &mut Capabilities) {
        caps::install(mode, env, caps);
    }

    fn search_providers(&self) -> Vec<Box<dyn kernel::search::Provider>> {
        vec![Box::new(search::MailSearch)]
    }

    fn problems(&self) -> &'static [&'static dyn ProblemSource] {
        SOURCES
    }

    fn workers(&self, store: &Store) -> Vec<Box<dyn Worker>> {
        sync::workers(store)
    }

    /// The four mailboxes lead, then a blank sheet, then the accounts: the
    /// launcher's order for mail, whatever else is in the build.
    fn roots(&self) -> Vec<Root> {
        vec![
            Root::new(Role::Inbox.id(), "inbox", "mail letters unread"),
            Root::new(Role::Archive.id(), "archive", "mail filed kept"),
            Root::new(Role::Sent.id(), "sent", "mail outgoing wrote"),
            Root::new(Role::Spam.id(), "spam", "mail junk"),
            Root::new(Compose::id(Seed::Blank), "new mail", "compose write send"),
            Root::new(Settings::id(), "settings", "accounts mail imap smtp"),
        ]
    }

    fn as_any(&self) -> &dyn Any {
        self
    }
}
