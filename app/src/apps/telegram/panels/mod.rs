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
//! Telegram sends through [`wire`] when the store has a worker connected.
//! Offline actions use fixture intents or a toast describing the request.

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
pub mod topics;

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
pub use topics::Topics;

/// Queue a request for this store's worker. False means no worker is
/// connected; true means queued, not acknowledged by Telegram.
#[must_use]
pub fn wire(store: &Store, request: &str) -> bool {
    let rt = super::runtime::of(store);
    if !live(store) {
        return false;
    }
    let tracked = rt.operations.track(request);
    let sent = rt.send(&tracked);
    if !sent {
        if let Ok(v) = serde_json::from_str::<serde_json::Value>(&tracked) {
            if let Some(id) = v["@extra"]["operation"].as_u64() {
                rt.operations.fail(
                    store,
                    id,
                    "Telegram is not connected. Your request was not sent.",
                    false,
                );
                // The composer still owns these; its Enter is the retry.
                if v["@type"] == "sendMessage"
                    || v["@type"] == "editMessageText"
                    || v["@type"] == "editMessageCaption"
                {
                    rt.operations.forget_payload(id);
                }
            }
        }
    }
    sent
}

pub fn live(store: &Store) -> bool {
    super::runtime::of(store).has_worker() || super::Telegram::engine_store(store.dir())
}

/// Queue a live verb, or explain why it did not go. Local changes wait for
/// the worker's acknowledgement; fixture actions use the offline toast.
pub fn told(s: &mut Session, request: &str, what: &str) -> bool {
    if wire(s.store(), request) {
        s.redraw();
        return true;
    }
    if live(s.store()) {
        s.notify(format!("{what} failed: Telegram is not connected"), true);
    } else {
        s.notify(super::draft_toast(what), false);
    }
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
        super::runtime::of(store)
            .operations
            .report(store, "saving change", &e.to_string());
    }
}
