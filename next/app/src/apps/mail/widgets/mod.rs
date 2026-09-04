//! Mail's Makepad half: one widget per panel kind.
//!
//! Each borrows its instance from the scope and calls the instance's own
//! methods; nothing here keeps state the panel could keep instead. The
//! mailbox is the shared rich table with a row body of its own, the reader
//! draws the conversation as rows that open in place, and the compose sheet
//! is three fields over a draft.
//!
//! The templates they are built from are in [`ui`](super::ui).

pub mod compose;
pub mod mailbox;
pub mod message;

pub use compose::ComposePanel;
pub use mailbox::MailboxPanel;
pub use message::MessagePanel;
