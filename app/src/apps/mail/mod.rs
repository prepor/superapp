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

    /// Mail's tables, in mail's own words. Read into an agent's system
    /// prompt, so it is prose rather than a schema dump.
    fn describe(&self) -> Option<&'static str> {
        Some(MAIL_DESCRIBE)
    }

    fn as_any(&self) -> &dyn Any {
        self
    }
}

/// The data dictionary an agent is given for mail: what a row is, the
/// columns that matter, the values a column takes, and — the part that keeps
/// a model honest — what must never be written directly.
///
/// It is prose and not `sqlite_master`: the schema is one tool call away,
/// and what a model cannot read off the schema is which of two columns is
/// the intent and which is the record of a server.
const MAIL_DESCRIBE: &str = "\
mail keeps its own tables in the one database every app shares. `message` \
records what the person wants; `server_msg` records what the server last \
said; the difference between them is what the sync pass turns into work.

`account` — one row per mailbox this build syncs: `label`, `email`, \
`imap_host`, `smtp_host`, and `auth`, which is NULL or 'password' for an app \
password and 'google' for an OAuth grant. `status` and `synced` are what the \
last sync pass wrote. No secret is ever in this table, or in any other: an \
app password lives in the keychain and a refresh token under its own key.

`folder` — one row per folder on a server: `account`, `name`, and `role`, \
which is how the app files by meaning rather than by name: 'inbox', \
'archive', 'sent', 'spam', 'trash', or NULL for a folder that is none of \
them and is not mirrored. Only the first four have panels; the trash is a \
role, not a list. `uidvalidity` and `uidnext` are the sync pass's own.

`message` — one row per letter, and the state the person wants it in: \
`folder` (where it should be), `unread`, `forwarded`, with `from_name`, \
`from_email`, `subject`, `date`, `body`, `html`, `message_id`, `topic` (the \
subject with its reply prefixes stripped) and `thread` — the smallest id in \
its conversation, decided at ingest, which is what a mailbox groups by. \
`raw` is the whole MIME letter as a blob and sits last on purpose: a select \
that does not need it should not name it.

`reference` — `(message, mid)`, one row per id a letter claims to answer. \
Threading is three lookups over this table and no subject guessing.

`server_msg` — the server's last word about a letter: `folder`, `uid`, \
`seen`, `forwarded`, one row per message. It is a record, not an intent.

`attachment` and `attachment_scan` — derived from `message.raw`: a part's \
`name`, `mime`, `size`, `cid`, and the `part` index its bytes are read back \
by. The bytes are never copied out of the letter; `attachment_scan` records \
which walk made a letter's rows.

`draft` and `draft_attachment` — a compose panel's unsent text and the \
paths it will carry, both keyed by that panel's slot (`panel`), which is why \
half-written text survives a restart.

`outbox` — one row per send: `account`, `send_after` (the window before it \
leaves), `status` ('pending', 'sent', 'failed') and `error`. Its id is the \
compose panel's slot, so there is one pending send per sheet.

`message_fts` — an FTS5 index over subject, sender and body, kept up to \
date by triggers inside the database. Search it as `message_fts MATCH ?`.

What must never be written directly:
— a send is an `outbox` row filed through mail's own send, never an INSERT \
  and never a letter handed to a server from anywhere else;
— a move between folders is `message.folder`, through mail's tools, and the \
  push pass converges the server from the difference against `server_msg`; \
  writing `server_msg` by hand tells the app a change has already been \
  pushed when it has not;
— marking a letter read is `message.unread`, not `server_msg.seen`;
— `attachment`, `attachment_scan` and `message_fts` are derived from \
  `message` and are rebuilt from it; a row written into them is lost at the \
  next walk.";
