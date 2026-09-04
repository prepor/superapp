//! The agent's panel instances: one conversation, and the list of them.
//!
//! Each owns its own state between draws — the composer's text, the table's
//! cursor and marks — and reads its rows through the store it was opened
//! with. A method the *table* drives answers with a
//! [`Nav`](kernel::nav::Nav) rather than applying one, because a walk
//! decides where it goes and the widget decides when; a verb of the bar is
//! [`Panel::run`](kernel::panel::Panel::run) instead, holding `&mut self`
//! and acting on the session itself.

pub mod agents;
pub mod chat;

pub use agents::{Agents, AgentsKind};
pub use chat::{Chat, ChatKind};
