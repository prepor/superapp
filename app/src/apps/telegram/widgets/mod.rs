//! Telegram's Makepad half: one widget per panel kind.
//!
//! Each borrows its instance from the scope and calls the instance's own
//! methods; nothing here keeps state the panel could keep instead. The
//! three lists are the shared rich table with a row body of their own; the
//! transcript is a list of its own kind, with the composer under it; the
//! card is a card.
//!
//! Every draw starts by telling the [model](super::model) what time it is,
//! because a row spells its time relative to now and is filled by a
//! function with no session in reach.
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

/// Tells the model what time it is, off the session in the scope. Called
/// at the top of every telegram widget's draw.
pub fn tell_now(scope: &mut Scope) {
    if let Some(s) = scope.data.get_mut::<Session>() {
        super::model::set_now(s.now());
    }
}
