//! The agent's Makepad widgets: one conversation, and the list of them.
//!
//! The list is the shell's rich table over the panel's own list state —
//! four short functions and no draw loop of its own. The chat is the one
//! panel in this build that draws neither a table nor a form: a transcript
//! of items, a composer under it, and a live tail while the model is still
//! writing.

pub mod agents;
pub mod chat;

pub use agents::AgentAgentsPanel;
pub use chat::AgentChatPanel;
