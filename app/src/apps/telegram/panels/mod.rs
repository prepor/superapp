//! Telegram's panel instances: the chat list, one conversation, one line
//! of it as a card, the viewer over a line's media, what goes with the next
//! message, the place to send, the messages a filter finds, the people, and
//! a peer's card.
//!
//! Each owns its own state between draws — a table with its cursor and
//! marks, a transcript's cursor and reply line, a draft — and reads through
//! the store it was opened with. The widget that draws one borrows it from
//! the scope and calls its methods; a verb of the bar is
//! [`Panel::run`](kernel::panel::Panel::run), and a verb that reaches
//! Telegram sends through [`wire`] where the build is signed in and says
//! what it *would* have sent, in a toast, where it is not.

use kernel::session::Session;
use kernel::store::Store;

pub mod attach;
pub mod chat;
pub mod chats;
pub mod line;
pub mod media;
pub mod messages;
pub mod peer;
pub mod people;
pub mod place;
pub mod signin;

pub use attach::Attach;
pub use chat::{Chat, Row};
pub use chats::Chats;
pub use line::Line;
pub use media::Viewer;
pub use messages::Messages;
pub use peer::Peer;
pub use people::{Contacts, Members, People};
pub use place::Place;
pub use signin::SignIn;

/// Fires one request at the signed-in account's client, answering whether it
/// went. `true` only where this build links the engine (the `tdlib` feature),
/// an account has signed in, and the panel asking reads the account holder's
/// own store; a caller keeps the demo path — the toast saying what would have
/// left — on `false`, so a build with no engine is behaviourally unchanged.
/// Mirrors the sign-in panel's own `deliver`.
///
/// The store is what says whose panel this is. The one client is filed in a
/// process-wide cell, so a Panels Library mount drawn beside a signed-in
/// window would otherwise reach it: a scene pressing *mute* would mute a real
/// chat, and a fixture's send would be a message somebody receives
/// ([`Telegram::engine_store`](super::Telegram::engine_store)).
#[cfg(feature = "tdlib")]
#[must_use]
pub fn wire(store: &Store, request: &str) -> bool {
    use super::transport::{self, Td};
    if !super::Telegram::engine_store(store.dir()) {
        return false;
    }
    match transport::shared() {
        Some(td) => {
            td.send(request);
            true
        }
        None => false,
    }
}

/// Without the engine there is nothing to send to: every verb keeps the demo
/// path, so this answers `false` and never reaches for a client.
#[cfg(not(feature = "tdlib"))]
#[must_use]
pub fn wire(_store: &Store, _request: &str) -> bool {
    false
}

/// One verb, told to Telegram: the request goes where the build is signed in;
/// where it is not, the toast says what *would* have left. Answers whether it
/// went — which is what tells a caller whether the flip beside it is a thing
/// the engine will presently confirm, or a change nobody asked for.
pub fn told(s: &mut Session, request: &str, what: &str) -> bool {
    if wire(s.store(), request) {
        return true;
    }
    s.notify(super::draft_toast(what), false);
    false
}

/// Writes a verb's local half straight through the store. Not an action: the
/// engine's own update settles the flag a moment later, and a flip the wire
/// will restate is not a thing to give back, so there is nothing here for
/// undo to hold. A refused write is said once, on stderr, and the next draw
/// simply reads what the store still holds.
pub fn flip(
    store: &Store,
    what: impl FnOnce(&rusqlite::Transaction) -> rusqlite::Result<()> + Send + 'static,
) {
    if let Err(e) = store.write(what) {
        eprintln!("telegram: the flip was not written: {e}");
    }
}
