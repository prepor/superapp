//! Mail's Makepad half: one widget per panel kind.
//!
//! Each borrows its instance from the scope and calls the instance's own
//! methods; nothing here keeps state the panel could keep instead. The
//! mailbox is the shared rich table with a row body of its own, the reader
//! draws the conversation as rows that open in place, the compose sheet is
//! three fields over a draft, the card over a part of a letter is the shell's
//! own, and the three that configure an account are a card, a list of rows
//! and a form.
//!
//! [`pictures`] is the exception: it is not a panel's widget but the letter's
//! images — a cache on `Cx`, a reader thread, and the `<img>` item the `Html`
//! widget mints from a template.
//!
//! The templates they are built from are in [`ui`](super::ui).

pub mod add_account;
pub mod card;
pub mod compose;
pub mod contact;
pub mod mailbox;
pub mod message;
pub mod pictures;
pub mod settings;

pub use add_account::AddAccountPanel;
pub use card::AttachmentPanel;
pub use compose::ComposePanel;
pub use contact::ContactPanel;
pub use mailbox::MailboxPanel;
pub use message::MessagePanel;
pub use pictures::HtmlImage;
pub use settings::SettingsPanel;
