//! Telegram's panels over a local SQLite projection of TDLib updates.
//!
//! `runtime` owns transient coordination for each store; `sync` owns the
//! account worker, `requests` builds outgoing JSON, and `updates` normalizes
//! incoming JSON for `project`. Panels own interaction state and enqueue
//! commands; widgets render it. Native TDLib is optional; fixtures stay offline.

use std::any::Any;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use kernel::app::{App, Capabilities, Env, Mode, Root, Schema, Worker};
use kernel::panel::PanelKind;
use kernel::search::Provider;
use kernel::store::Store;

pub mod config;
pub mod model;
pub mod operations;
pub mod panels;
pub mod project;
pub mod requests;
pub mod runtime;
pub mod scenes;
pub mod schema;
pub mod search;
pub mod seed;
/// The authorization state machine and the per-account worker loop.
pub mod sync;
pub mod text;
/// The real engine's C binding, only when the `tdlib` feature links it.
#[cfg(feature = "tdlib")]
pub mod tdjson;
pub mod trace;
pub mod topics;
pub mod tools;
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

pub use panels::{Chat, Chats, Contacts, SignIn};
pub use ui::UI;

/// The app.
pub struct Telegram {
    /// The account holder's own store — the directory the one real,
    /// unscripted world was built over, filed by [`Telegram::outside`]. One
    /// process holds more than one session: the window's, and a Panels
    /// Library mount's or a test's beside it, each with a store of its own.
    /// The engine belongs to exactly one of them.
    engine_store: OnceLock<PathBuf>,
}

/// The one in this build.
pub static TELEGRAM: Telegram = Telegram {
    engine_store: OnceLock::new(),
};

impl Telegram {
    /// The boot store admitted to the single native receive loop. This is
    /// registration policy only; panels send through their own runtime.
    #[must_use]
    pub fn engine_store(dir: Option<&Path>) -> bool {
        match (TELEGRAM.engine_store.get(), dir) {
            (Some(engine), Some(dir)) => engine == dir,
            _ => false,
        }
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
static TOPICS_KIND: panels::topics::TopicsKind = panels::topics::TopicsKind;
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
    &TOPICS_KIND,
];

impl App for Telegram {
    fn id(&self) -> &'static str {
        "telegram"
    }

    fn problems(&self) -> &'static [&'static dyn kernel::app::ProblemSource] {
        &[&operations::Failures]
    }

    fn poll(&self, s: &mut kernel::session::Session) {
        let rt = runtime::of(s.store());
        rt.operations.expire(s.store(), std::time::Instant::now());
        if rt.operations.take_changed() {
            let operations = rt.operations.list();
            for op in &operations {
                if matches!(op.status, operations::Status::Failed { .. }) {
                    if let Some(context) = op.context() {
                        if context.starts_with("load_chats:") {
                            rt.set_list_syncing(false);
                        }
                        if let Some(chat) = context.strip_prefix("topics:")
                            .and_then(|s| s.split(':').next()?.parse().ok())
                        {
                            let pending = operations.iter().any(|op| op.chat == Some(chat)
                                && op.status == operations::Status::Pending
                                && op.context().is_some_and(|c| c.starts_with("topics:")));
                            if rt.topics_status(chat) == Ok(true) && !rt.topic_list_queued(chat) && !pending {
                                rt.topics_loaded(chat, Err("could not load topics · refresh to try again".into()));
                            }
                        }
                    }
                }
            }
            s.redraw();
        }
        for (text, error) in rt.take_notices() {
            s.notify(text, error);
            s.redraw();
        }
    }

    fn kinds(&self) -> &'static [&'static dyn PanelKind] {
        KINDS
    }

    fn tools(&self) -> Vec<kernel::tool::Tool> {
        tools::all()
    }

    fn describe(&self) -> Option<&'static str> {
        Some(tools::DESCRIBE)
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

    /// Admit the real, unscripted boot store. Library mounts and tests do not
    /// open native clients; TDLib's process-wide receive queue has one owner.
    fn outside(&self, mode: Mode, env: &Env, caps: &mut Capabilities) {
        let live = mode == Mode::Real && !env.scripted;
        caps.insert(Box::new(if live { runtime::Delivery::Live } else { runtime::Delivery::Demo }));
        if live {
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
            Root::new(panels::Messages::replies(None), "replies & mentions", "telegram unread groups"),
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

/// The feedback for a command in an offline fixture.
#[must_use]
pub fn draft_toast(what: &str) -> String {
    format!("draft: nothing leaves — {what}")
}
