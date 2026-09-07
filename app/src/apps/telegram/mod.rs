//! The telegram app: a chat list, conversations, one line as a card and its
//! media as large as the grid allows, what goes with the next message and
//! the place to send, search, the people, and a card per peer.
//!
//! Round one draws the surface over a demo world; nothing reaches Telegram
//! yet. Every verb that would toasts *draft: nothing leaves*, and what is
//! the panel's own — the cursor, the marks, the reply line, the draft, the
//! read claim — is real. The plan is `docs/planning/cr-012-telegram.md`.

use std::any::Any;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, MutexGuard, OnceLock};

use kernel::app::{App, Capabilities, Env, Mode, Root, Schema, Worker};
use kernel::panel::PanelKind;
use kernel::search::Provider;
use kernel::store::Store;

pub mod config;
pub mod model;
pub mod panels;
/// What the engine is busy with, for a panel to say *loading…*.
pub mod progress;
pub mod project;
pub mod scenes;
pub mod schema;
pub mod search;
pub mod seed;
/// The authorization state machine and the per-account worker loop.
pub mod sync;
/// The real engine's C binding, only when the `tdlib` feature links it.
#[cfg(feature = "tdlib")]
pub mod tdjson;
/// The engine seam the loop drives: the real transport, and a fake for tests.
pub mod transport;
pub mod ui;
/// Pure maps from a TDLib update's JSON to the projection's `Incoming*`
/// structs — the wire's shapes in one greppable place.
pub mod updates;
pub mod verbs;
pub mod widgets;

#[cfg(test)]
mod tests;

use model::{MsgId, PeerId};
pub use panels::{Chat, Chats, Contacts, SignIn};
pub use ui::UI;

/// Lines waiting for the chat they are going to: what *forward* took out of
/// a transcript, or off one line's card, and what the chat list's *forward
/// here* sends on.
///
/// The client's forward is a sheet that picks a chat, and here a picker is a
/// panel of its own — so the pick has to outlive the transcript that started
/// it. That makes it the app's, not a panel's.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Forward {
    /// The chat the lines are in now.
    pub from: PeerId,
    pub ids: Vec<MsgId>,
}

/// The app.
pub struct Telegram {
    /// The forward waiting for its chat, where one waits. A mutex because
    /// the app is a `static` every thread can see; only the window's ever
    /// touches this one.
    forward: Mutex<Option<Forward>>,
    /// The account holder's own store — the directory the one real,
    /// unscripted world was built over, filed by [`Telegram::outside`]. One
    /// process holds more than one session: the window's, and a Panels
    /// Library mount's or a test's beside it, each with a store of its own.
    /// The engine belongs to exactly one of them.
    engine_store: OnceLock<PathBuf>,
}

/// The one in this build.
pub static TELEGRAM: Telegram = Telegram {
    forward: Mutex::new(None),
    engine_store: OnceLock::new(),
};

impl Telegram {
    fn waiting() -> MutexGuard<'static, Option<Forward>> {
        TELEGRAM.forward.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Whether a store is the account holder's own: the one a worker may
    /// open a client for, and the one a panel may send from.
    ///
    /// Everything else — the Panels Library's mounts, every test's session —
    /// comes up in memory with the demo rows, and must reach no further than
    /// them. TDLib's receive queue is process-wide and its responses are not
    /// routed by client, so a second client opened beside the real one drains
    /// the signed-in account's updates into a fixture store; and a fixture
    /// verb that reached [`transport::shared`] would send a real message from
    /// a scene. Neither is a thing a library of drawings may do.
    // Both its readers — the worker registration and the panels' `wire` —
    // are the engine's own, so a build linking none has no caller for it.
    // `app`'s app modules are private, and a `pub` alone does not count as
    // one, which is what [`config`] says of its own readers.
    #[cfg_attr(not(feature = "tdlib"), allow(dead_code))]
    #[must_use]
    pub fn engine_store(dir: Option<&Path>) -> bool {
        match (TELEGRAM.engine_store.get(), dir) {
            (Some(engine), Some(dir)) => engine == dir,
            _ => false,
        }
    }

    /// Takes lines out of a transcript to wait for the chat they go to.
    /// A second forward replaces the first: one pick is being made.
    pub fn carry_forward(from: PeerId, ids: Vec<MsgId>) {
        *Self::waiting() = Some(Forward { from, ids });
    }

    /// The forward, and the waiting over — the pick, or the way out of it.
    pub fn take_forward() -> Option<Forward> {
        Self::waiting().take()
    }

    /// What waits, for a bar that offers the pick and a panel that draws it.
    #[must_use]
    pub fn pending_forward() -> Option<Forward> {
        Self::waiting().clone()
    }
}

static CHATS_KIND: panels::chats::ChatsKind = panels::chats::ChatsKind;
static CHAT_KIND: panels::chat::ChatKind = panels::chat::ChatKind;
static MESSAGES_KIND: panels::messages::MessagesKind = panels::messages::MessagesKind;
static CONTACTS_KIND: panels::people::ContactsKind = panels::people::ContactsKind;
static MEMBERS_KIND: panels::people::MembersKind = panels::people::MembersKind;
static PEER_KIND: panels::peer::PeerKind = panels::peer::PeerKind;
static LINE_KIND: panels::line::LineKind = panels::line::LineKind;
static VIEWER_KIND: panels::media::ViewerKind = panels::media::ViewerKind;
static PLACE_KIND: panels::place::PlaceKind = panels::place::PlaceKind;
static ATTACH_KIND: panels::attach::AttachKind = panels::attach::AttachKind;
static SIGNIN_KIND: panels::signin::SignInKind = panels::signin::SignInKind;
static KINDS: &[&dyn PanelKind] = &[
    &CHATS_KIND,
    &CHAT_KIND,
    &MESSAGES_KIND,
    &CONTACTS_KIND,
    &MEMBERS_KIND,
    &PEER_KIND,
    &LINE_KIND,
    &VIEWER_KIND,
    &PLACE_KIND,
    &ATTACH_KIND,
    &SIGNIN_KIND,
];

impl App for Telegram {
    fn id(&self) -> &'static str {
        "telegram"
    }

    fn kinds(&self) -> &'static [&'static dyn PanelKind] {
        KINDS
    }

    fn schema(&self) -> Option<&'static Schema> {
        Some(&schema::SCHEMA)
    }

    /// The demo world, whatever the outside: until an account signs in
    /// there is nothing else to show, and a real run's rows are the same
    /// rows.
    fn seed(&self, store: &Store, _mode: Mode) -> rusqlite::Result<()> {
        seed::seed_if_empty(store)
    }

    /// Telegram installs no backend of its own — the engine is a worker's,
    /// not a capability — so what it takes from a world being built is which
    /// store the account belongs to: the one a real run that nobody is
    /// scripting opened. A mount and a test build theirs [`Mode::Fake`], so a
    /// fixture's store is never mistaken for the account holder's.
    ///
    /// The clock is deliberately no part of it, unlike mail's own
    /// `real_run`: a headless run against a copy of the session is how the
    /// live wire is diagnosed, and it is the real account for every purpose
    /// here.
    fn outside(&self, mode: Mode, env: &Env, _caps: &mut Capabilities) {
        if mode == Mode::Real && !env.scripted {
            // An empty parent is no directory, which is what the store makes
            // of one too — so both sides of the comparison mean the same
            // thing by *in memory*.
            let dir = env.db_dir.as_deref().filter(|d| !d.as_os_str().is_empty());
            if let Some(dir) = dir {
                // Set once: one run has one account holder, and a second
                // world over the same store — a worker's own — files the
                // same directory.
                let _ = TELEGRAM.engine_store.set(dir.to_path_buf());
            }
        }
    }

    fn search_providers(&self) -> Vec<Box<dyn Provider>> {
        vec![Box::new(search::TelegramSearch)]
    }

    /// The list, the people, the chat with oneself, and the door an account
    /// signs in through.
    fn roots(&self) -> Vec<Root> {
        vec![
            Root::new(Chats::id(), "chats", "telegram"),
            Root::new(Contacts::id(), "contacts", "telegram people"),
            Root::new(Chat::id(seed::SELF), "saved messages", "telegram notes"),
            Root::new(SignIn::id(), "sign in", "telegram account login phone"),
        ]
    }

    /// The one account's worker, where the engine is linked: it opens the
    /// TDLib client beside the store and drives the authorization flow, so the
    /// sign-in panel has something to answer. Without the `tdlib` feature there
    /// is no engine and no worker — Telegram stays the demo world.
    ///
    /// For the account holder's store and no other
    /// ([`engine_store`](Telegram::engine_store)). A library mount and a test
    /// run their passes inline, so registering this for every store would open
    /// a client per fixture session, and every one of them would drain the
    /// engine's process-wide queue — the signed-in account's updates landing
    /// in a store of demo rows.
    #[cfg(feature = "tdlib")]
    fn workers(&self, store: &Store) -> Vec<Box<dyn Worker>> {
        if !Self::engine_store(store.dir()) {
            return Vec::new();
        }
        vec![Box::new(sync::RealWorker::new(store.dir()))]
    }

    #[cfg(not(feature = "tdlib"))]
    fn workers(&self, _store: &Store) -> Vec<Box<dyn Worker>> {
        Vec::new()
    }

    fn as_any(&self) -> &dyn Any {
        self
    }
}

/// What every verb that would reach Telegram says instead, this round.
#[must_use]
pub fn draft_toast(what: &str) -> String {
    format!("draft: nothing leaves — {what}")
}
