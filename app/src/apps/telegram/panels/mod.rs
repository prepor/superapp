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

use kernel::panel::{PanelId, Verb};
use kernel::session::Session;
use kernel::store::Store;

use super::model::{PeerId, Scope};

use crate::shell::widgets::map;

pub mod attach;
pub mod call;
pub mod chat;
pub mod chats;
pub mod line;
pub mod media;
pub mod messages;
pub mod peer;
pub mod people;
pub mod place;
pub mod playback;
mod reactions;
mod reaction_authors;
pub mod signin;
pub mod topics;

pub use attach::Attach;
pub use call::Call;
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

/// Where a forwarded line came from, as a panel to open: the post itself
/// where a channel named one, else the conversation with whoever wrote it.
///
/// `None` on a line nobody forwarded, and on one whose origin hid itself —
/// there is nothing to open then, only a name. Lines cached before the
/// origin was kept as a peer also read `None`, and gain the way back as
/// they are re-projected.
#[must_use]
pub fn came_from(store: &Store, m: &super::model::Msg) -> Option<kernel::panel::PanelId> {
    let peer = m.fwd_peer?;
    // A forward out of this very conversation goes nowhere: the line is
    // already here.
    if peer == m.chat {
        return None;
    }
    Some(match m.fwd_key() {
        Some((peer, msg)) if super::model::peer(store, peer).is_some() => Chat::at(peer, msg),
        _ => Chat::id(peer),
    })
}

/// The arguments a panel standing in one part of a chat carries after the
/// chat itself: nothing for the chat, the topic's id for a forum topic, and
/// the thread's root behind a word for a post's comments — the word being
/// what tells the two kinds apart, both being message ids.
#[must_use]
pub fn scope_args(chat: PeerId, scope: Scope) -> Vec<String> {
    let mut args = vec![chat.to_string()];
    match scope {
        Scope::Whole => {}
        Scope::Topic(topic) => args.push(topic.to_string()),
        Scope::Thread(root) => {
            args.push("thread".into());
            args.push(root.to_string());
        }
    }
    args
}

/// The same, read back.
#[must_use]
pub fn scope_of(id: &PanelId) -> Scope {
    if id.arg(1) == Some("thread") {
        return Scope::of_thread(id.arg(2).and_then(|s| s.parse().ok()).unwrap_or(0));
    }
    Scope::of_topic(id.arg(1).and_then(|s| s.parse().ok()).unwrap_or(0))
}

/// A place's ways out: somebody else's map, at the point. Apple Maps only
/// where there is one to open — a phone opens `maps.apple.com` at nothing —
/// Google Maps and OpenStreetMap everywhere. The line's card and the viewer
/// wear the same three.
#[must_use]
pub fn place_verbs() -> Vec<Verb> {
    let apple = cfg!(target_os = "macos")
        .then(|| Verb::run("telegram.maps", "maps", Some('m')));
    apple
        .into_iter()
        .chain([
            Verb::run("telegram.google", "google maps", Some('g')),
            Verb::run("telegram.browser", "browser", Some('b')),
        ])
        .collect()
}

/// Which map one of those verbs asks for, at the point; `None` for any
/// other verb.
#[must_use]
pub fn place_url(verb: &str, lat: f64, lon: f64) -> Option<String> {
    match verb {
        "telegram.maps" => Some(map::maps_url(lat, lon)),
        "telegram.google" => Some(map::google_url(lat, lon)),
        "telegram.browser" => Some(map::osm_url(lat, lon)),
        _ => None,
    }
}

/// What one of those verbs asks for, ready to be opened — or, in a world
/// that is nobody's, said as a draft toast and opened nowhere.
///
/// A fixture is a scene of a panel or a scripted run, and neither may reach
/// the machine's browser: a suite that opened Google Maps would leave a
/// window behind on whoever ran it. The test is the same one a send is
/// weighed by — a world that delivers nothing opens nothing — and not
/// whether there is a store on disk, because a suite is given one.
///
/// Everywhere else the URL goes back to the panel, whose widget hands it to
/// the system on its next draw: a panel has no `Cx` and cannot open anything
/// itself.
#[must_use]
pub fn map_wish(s: &mut Session, verb: &str, lat: f64, lon: f64) -> Option<String> {
    let url = place_url(verb, lat, lon)?;
    let outside = s
        .world()
        .with_cap::<super::runtime::Delivery, _>(|d| *d == super::runtime::Delivery::Live)
        .unwrap_or(false);
    if !outside {
        s.notify(super::draft_toast(&format!("open {url}")), false);
        return None;
    }
    s.redraw();
    Some(url)
}

/// Queue a request for this store's worker. False means no worker is
/// connected; true means queued, not acknowledged by Telegram.
#[must_use]
pub fn wire(store: &Store, request: &str) -> bool {
    queue(store, request).is_some()
}

/// The same queue boundary, returning this request's operation id. The
/// worker can register other operations concurrently, so a caller must not
/// infer its send's id by reading the tracker's newest entry afterwards.
pub fn queue(store: &Store, request: &str) -> Option<u64> {
    let rt = super::runtime::of(store);
    if !live(store) {
        return None;
    }
    let tracked = rt.operations.track(request);
    let v: serde_json::Value = serde_json::from_str(&tracked).ok()?;
    let id = v["@extra"]["operation"].as_u64()?;
    let sent = rt.send(&tracked);
    if !sent {
        rt.operations.fail(
            store,
            id,
            "Telegram is not connected. Your request was not sent.",
            false,
        );
        // The composer still owns these; its Enter is the retry.
        if v["@type"] == "sendMessage"
            || v["@type"] == "sendMessageAlbum"
            || v["@type"] == "editMessageText"
            || v["@type"] == "editMessageCaption"
        {
            rt.operations.forget_payload(id);
        }
    }
    sent.then_some(id)
}

/// Whether a call can be made from this store. A live account needs an
/// engine linked into the build to carry one; a demo world has the fake,
/// which carries nothing and is what every other verb draws on there.
#[must_use]
pub fn can_call(store: &Store) -> bool {
    !live(store) || super::calls::available()
}

pub fn live(store: &Store) -> bool {
    super::runtime::of(store).has_worker() || super::Telegram::engine_store(store.dir())
}

/// Queue a live verb, or explain why it did not go. Local changes wait for
/// the worker's acknowledgement; fixture actions use the offline toast.
pub fn told(s: &mut Session, request: &str, what: &str) -> bool {
    match super::history::command(s, request) {
        Ok(_) => { s.redraw(); true }
        Err(error) => { error.notify(s, what); false }
    }
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
    if store.ui_attached() {
        match store.submit_write(what) {
            Ok(pending) => super::runtime::of(store).track_write(pending, "saving change"),
            Err(error) => super::runtime::of(store).notice(format!("saving change: {error}"), true),
        }
        return;
    }
    if let Err(e) = store.write(what) {
        super::runtime::of(store)
            .operations
            .report(store, "saving change", &e.to_string());
    }
}
