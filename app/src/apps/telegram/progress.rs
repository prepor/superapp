//! What the engine is busy with right now, for a panel to say so.
//!
//! A transcript being filled from the wire, the chat list still coming
//! down: each is a walk the worker drives from its own thread, and the
//! panel that started it draws on the window's. What they share is a flag
//! in memory — not the store, which replicates and where "loading" on one
//! device means nothing on another. The panel reads it each draw and shows
//! *loading…* until the walk ends; the worker clears it as the last page
//! lands (Andrey, 2026-09-07: there should always be a clean visual
//! indicator that something is loading).

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Mutex, MutexGuard};

use super::model::PeerId;

/// The chats whose history walk is in flight — a handful at most, so a
/// vector is the set.
static LOADING: Mutex<Vec<PeerId>> = Mutex::new(Vec::new());

/// Whether the chat list is still being loaded, page by page.
static LIST_SYNCING: AtomicBool = AtomicBool::new(false);

fn loading_set() -> MutexGuard<'static, Vec<PeerId>> {
    LOADING.lock().unwrap_or_else(|e| e.into_inner())
}

/// Marks a chat's history walk as under way, or over.
pub fn set_loading(chat: PeerId, on: bool) {
    let mut set = loading_set();
    if on {
        if !set.contains(&chat) {
            set.push(chat);
        }
    } else {
        set.retain(|c| *c != chat);
    }
}

/// Whether a chat's transcript is still being filled from the wire.
#[must_use]
pub fn loading(chat: PeerId) -> bool {
    loading_set().contains(&chat)
}

/// Marks the chat-list load as under way, or over.
pub fn set_list_syncing(on: bool) {
    LIST_SYNCING.store(on, Ordering::Relaxed);
}

/// A line whose viewer should play as it opens — the transcript's play
/// button on a real clip, which the row cannot play itself: the viewer is
/// opened and told, through here, to start.
static PLAY_NEXT: Mutex<Option<(PeerId, i64)>> = Mutex::new(None);

/// Asks the next viewer opened on this line to play at once.
pub fn play_on_open(chat: PeerId, id: i64) {
    *PLAY_NEXT.lock().unwrap_or_else(|e| e.into_inner()) = Some((chat, id));
}

/// Whether a viewer opening on this line was asked to play; the wish is
/// spent by the asking.
#[must_use]
pub fn take_play_on_open(chat: PeerId, id: i64) -> bool {
    let mut wish = PLAY_NEXT.lock().unwrap_or_else(|e| e.into_inner());
    if *wish == Some((chat, id)) {
        *wish = None;
        true
    } else {
        false
    }
}

/// One line into the account's trace — `tg-debug.log` beside the store —
/// from the window's thread: what a panel saw of the player, so a clip that
/// does not play is diagnosed from the file rather than from a description.
pub fn note(store_dir: Option<&std::path::Path>, line: &str) {
    use std::io::Write as _;
    let Some(dir) = store_dir else { return };
    if let Ok(mut f) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(dir.join("tg-debug.log"))
    {
        let secs = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);
        let _ = writeln!(f, "{secs} {line}");
    }
}

/// Whether the chat list is still coming down.
#[must_use]
pub fn list_syncing() -> bool {
    LIST_SYNCING.load(Ordering::Relaxed)
}

#[cfg(test)]
mod tests {
    /// A flag is per chat and per state: set, seen, cleared — and the list's
    /// own is one bool.
    #[test]
    fn flags_are_set_seen_and_cleared() {
        super::set_loading(-777_001, true);
        assert!(super::loading(-777_001));
        assert!(!super::loading(-777_002));
        super::set_loading(-777_001, false);
        assert!(!super::loading(-777_001));
        super::set_list_syncing(true);
        assert!(super::list_syncing());
        super::set_list_syncing(false);
    }
}
