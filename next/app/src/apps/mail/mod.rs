//! The mail app: mailboxes, messages, and drafts over the demo seed and a
//! fake server.
//!
//! What it adds to the shell is two halves and nothing else: the kernel's
//! [`App`] — four panel kinds, a schema ladder, a demo seed, three deferred
//! effects, two capabilities of its own, a search source, a problem source, a
//! worker per account plus the sender, and its roots for the launcher — and
//! the shell's [`AppUi`](crate::shell::app_ui::AppUi), which is
//! [`ui`] and the widgets under [`widgets`].
//!
//! The prototype has neither a real IMAP nor a real SMTP, so
//! [`caps::FakeServers`] is what every mode gets — see
//! [`caps::install`].

// Mail is written against its own interfaces, not against this build's use
// of them: the model answers questions no panel asks yet (the accounts a
// settings panel would list, the reopen a failed send will want), and the
// port is what reaches them. Told once here rather than one `pub` at a time.
#![allow(dead_code)]

use std::any::Any;

use kernel::app::{App, Capabilities, Env, Mode, ProblemSource, Root, Schema, Worker};
use kernel::effect::Registry;
use kernel::panel::PanelKind;
use kernel::store::Store;

pub mod caps;
pub mod carry;
pub mod effects;
pub mod model;
pub mod panels;
pub mod problems;
pub mod scenes;
pub mod schema;
pub mod search;
pub mod seed;
pub mod sync;
pub mod ui;
pub mod widgets;

#[cfg(test)]
mod tests;

pub use model::{MailId, Role, Seed};
pub use panels::{Compose, Mailbox, Message};
pub use ui::UI;

/// The app.
pub struct Mail;

/// The one in this build.
pub static MAIL: Mail = Mail;

static INBOX_KIND: panels::MailboxKind = panels::MailboxKind(Role::Inbox);
static ARCHIVE_KIND: panels::MailboxKind = panels::MailboxKind(Role::Archive);
static MESSAGE_KIND: panels::MessageKind = panels::MessageKind;
static COMPOSE_KIND: panels::ComposeKind = panels::ComposeKind;

static KINDS: &[&dyn PanelKind] = &[
    &INBOX_KIND,
    &ARCHIVE_KIND,
    &MESSAGE_KIND,
    &COMPOSE_KIND,
];

static FAILING_SENDS: problems::FailingSends = problems::FailingSends;
static SOURCES: &[&dyn ProblemSource] = &[&FAILING_SENDS];

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

    fn seed(&self, store: &Store) -> rusqlite::Result<()> {
        seed::seed_if_empty(store)
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

    /// The mailboxes lead, and a blank sheet closes: the launcher's order for
    /// mail, whatever else is in the build.
    fn roots(&self) -> Vec<Root> {
        vec![
            Root::new(Role::Inbox.id(), "inbox", "mail letters unread"),
            Root::new(Role::Archive.id(), "archive", "mail filed kept"),
            Root::new(Compose::id(Seed::Blank), "new mail", "compose write send"),
        ]
    }

    fn as_any(&self) -> &dyn Any {
        self
    }
}

impl Mail {
    /// One mailbox's panel.
    #[must_use]
    pub fn mailbox(&self, role: Role) -> kernel::panel::PanelId {
        Mailbox::id(role)
    }

    /// The panel that reads a mail's conversation — what another app links
    /// to when it has a mail id and nothing else.
    #[must_use]
    pub fn message(&self, mail: MailId) -> kernel::panel::PanelId {
        Message::id(mail)
    }

    /// A sheet on this source, blank or answering something.
    #[must_use]
    pub fn compose(&self, seed: Seed) -> kernel::panel::PanelId {
        Compose::id(seed)
    }
}
