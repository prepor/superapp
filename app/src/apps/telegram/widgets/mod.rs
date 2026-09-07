//! Telegram's Makepad half: one widget per panel kind.
//!
//! Each borrows its instance from the scope and calls the instance's own
//! methods; nothing here keeps state the panel could keep instead. The
//! three lists are the shared rich table with a row body of their own; the
//! transcript is a list of its own kind, with the composer under it; the
//! card is a card.
//!
//! The templates they are built from are in [`ui`](super::ui).

pub mod attach;
pub mod chat;
pub mod chats;
pub mod line;
pub mod media;
pub mod messages;
pub mod peer;
pub mod people;
pub mod place;
mod text;

pub use attach::AttachPanel;
pub use chat::ChatPanel;
pub use chats::ChatsPanel;
pub use line::LinePanel;
pub use media::ViewerPanel;
pub use messages::MessagesPanel;
pub use peer::PeerPanel;
pub use people::PeoplePanel;
pub use place::PlacePanel;

use kernel::session::Session;
use makepad_widgets::Scope;

/// The current event or draw's clock; a fixture without a session is inert.
pub fn now(scope: &mut Scope) -> f64 {
    scope.data.get_mut::<Session>().map_or(0.0, |s| s.now())
}

/// Inputs a transcript row needs from its owning session. Passed explicitly
/// so nested library draws cannot change another panel's clock or media root.
#[derive(Default)]
pub struct RenderContext {
    pub now: f64,
    pub store_dir: Option<std::path::PathBuf>,
}

impl RenderContext {
    pub fn from_scope(scope: &mut Scope) -> Self {
        scope.data.get_mut::<Session>().map_or_else(Self::default, |s| Self {
            now: s.now(),
            store_dir: s.store().dir().map(std::path::Path::to_path_buf),
        })
    }
}
