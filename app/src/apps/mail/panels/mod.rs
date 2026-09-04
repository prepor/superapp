//! Mail's panel instances: the four mailboxes, the reader, the compose
//! sheet, the card over a part of a letter, a correspondent's card, and the
//! two that configure an account.
//!
//! Each owns its own state between draws — a table with its cursor and marks,
//! which messages of a thread are open, the text of a draft — and writes
//! through the store it was opened with. The widget that draws one borrows it
//! from the scope and calls its methods; a method the *table* drives answers
//! with a [`Nav`](kernel::nav::Nav) rather than applying one, because a walk
//! decides where it goes and the widget decides when. A verb of the bar is
//! [`Panel::run`](kernel::panel::Panel::run) instead: the instance holds
//! `&mut self` and acts on the session itself.

pub mod add_account;
pub mod card;
pub mod compose;
pub mod contact;
pub mod mailbox;
pub mod message;
pub mod settings;

pub use add_account::{AddAccount, AddAccountKind, Form};
pub use card::{Card, CardKind};
pub use compose::{Compose, ComposeKind};
pub use contact::{Contact, ContactKind};
pub use mailbox::{Mailbox, MailboxKind};
pub use message::{Message, MessageKind};
pub use settings::{Settings, SettingsKind};
