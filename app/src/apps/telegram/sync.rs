//! The login brain of the real client: TDLib's authorization state machine,
//! and the per-account worker that drives it.
//!
//! An account signs in by phone and code, two-factor password where the
//! account has one, driven entirely by `updateAuthorizationState`: TDLib says
//! what it wants next and [`Account`] answers, writing the [`tg_session`] row
//! so the sign-in UI reflects where the flow stands. The state machine and
//! its request builders are pure and testable — [`FakeTd`](super::transport)
//! scripts the updates and records the sends — so this whole path is proven
//! offline, with the real [`RealTd`](super::transport::RealTd) behind the
//! `tdlib` feature.
//!
//! What this phase does **not** do: project content. A signed-in account's
//! dialogs, peers and history become rows in a later phase; the seam is
//! [`Account::on_ready`] and [`Account::handle_update`], left near-empty here
//! so the next agent has one clear place to fill.
//!
//! Nothing in this build calls the module yet: the real worker is not
//! registered until phase 3d, so its items are allowed to read as unused
//! rather than be wired to a caller that does not exist.
#![allow(dead_code)]

use std::path::{Path, PathBuf};
use std::time::Duration;

use kernel::app::{Wake, Worker};
use kernel::caps::{Blobs, Secrets};
use kernel::effect::{Job, World};
use rusqlite::Connection;
use serde_json::{json, Value};

use super::model::{self, Media, MsgId, PeerId};
use super::progress;
use super::project::{
    apply_read_outbox, history_window, project_chats, project_members, project_messages,
    project_peers, trim_chat, IncomingMessage, HISTORY_KEEP,
};
use super::schema;
#[cfg(feature = "tdlib")]
use super::transport::RealTd;
use super::transport::Td;
use super::updates;

/// The version TDLib is told at sign-in: a label the account's own device
/// list shows beside `superapp`, no more. A const so one place carries it.
const APP_VERSION: &str = "0.1.0";

/// How long a *typing…* stands before a pass forgets it, in seconds. The
/// server sends `chatActionCancel` when it feels like it and not otherwise,
/// so every Telegram client expires an action on its own clock; about six
/// seconds is what they all use, and a person still typing re-arms it well
/// inside that.
const TYPING_FOR: f64 = 6.0;

// -- the authorization state machine -------------------------------------------

/// One account's sign-in, driven by `updateAuthorizationState`. It owns the
/// transport it sends on; the worker feeds it the updates it drains.
///
/// Every auth state either fires the request TDLib is waiting for
/// (parameters, phone) or records a state the user must answer to (code,
/// password) and waits for the UI to supply it — never guessing a secret.
pub struct Account<T: Td> {
    td: T,
    /// The application id — a small positive int Telegram assigns, not a
    /// secret, from the `telegram` file.
    api_id: i32,
    /// TDLib's own binlog directory: its auth keys and update cursors. Local
    /// and un-synced, beside the store and never in it — a session is a
    /// secret and the store replicates.
    tdlib_dir: PathBuf,
    /// The configured phone, if the `telegram` file carried one; else set by
    /// the sign-in UI through [`set_phone`](Account::set_phone).
    phone: Option<String>,
    /// Every distinct write failure said on stderr so far — each is said
    /// once, however many lines it refuses.
    seen: std::cell::RefCell<std::collections::HashSet<String>>,
    /// When each chat's *typing…* falls silent, in unix seconds. Telegram's
    /// server does not reliably send `chatActionCancel` — a client that
    /// waited for one would leave a person typing forever — so every client
    /// gives an action about six seconds and then forgets it, which is what
    /// [`TYPING_FOR`] and this map are. A `RefCell` because the worker is one
    /// thread per account and `drain` takes `&self`.
    typing: std::cell::RefCell<std::collections::HashMap<PeerId, f64>>,
    /// History pages waiting their turn, front first: the chat just opened
    /// goes to the front, a walk's next page to the back, so opened chats
    /// take turns. One page is on the wire at a time, a second apart.
    pages: std::cell::RefCell<std::collections::VecDeque<Page>>,
    /// When the page on the wire went, or `None` when none is.
    in_flight: std::cell::Cell<Option<f64>>,
    /// The earliest the next page may go: the pace, or the wait Telegram
    /// asked for.
    not_before: std::cell::Cell<f64>,
}

/// One history page to ask for: the chat, the walk, where from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Page {
    chat: PeerId,
    from: MsgId,
    walk: Walk,
}

/// Seconds between history pages. Telegram throttles `messages.getHistory`
/// well below what a walk asks for when every answer fires the next page at
/// once — the first day's trace showed 140 pages a minute and 2,500 delayed
/// queries with thirty-second waits (2026-09-07). One a second, one chat at
/// a time, is what a client scrolling briskly does.
const PAGE_GAP: f64 = 1.0;

/// How long a page on the wire is waited for before the next may go.
const PAGE_PATIENCE: f64 = 30.0;

/// How many of a chat's newest lines have their pictures fetched as it
/// opens; the rest are the viewer's to ask for.
const FETCH_ON_OPEN: usize = 40;

/// The chats whose transcripts the panels asked to fill, taken by the
/// worker on its next pass. Panels have no handle on the account; this is
/// the seam between the window's thread and the worker's.
static WANTED: std::sync::Mutex<Vec<PeerId>> = std::sync::Mutex::new(Vec::new());

/// Lines the panels asked to have fetched afresh, taken by the worker on its
/// next pass: a line projected before a column it needs existed — a video
/// from before the clip's remote id was kept — is re-projected whole from
/// the engine's copy, and the column fills.
static WANTED_LINES: std::sync::Mutex<Vec<(PeerId, MsgId)>> = std::sync::Mutex::new(Vec::new());

/// Ask for one line to be fetched from the wire again and re-projected, so a
/// column the row lacks — one added after the row landed — is filled in.
pub fn want_line(chat: PeerId, id: MsgId) {
    let mut wanted = WANTED_LINES.lock().unwrap_or_else(|e| e.into_inner());
    if !wanted.contains(&(chat, id)) {
        wanted.push((chat, id));
    }
}

/// Files the panels asked for by their remote ids, taken by the worker on its
/// next pass. A picture is fetched as its line arrives, but only for the
/// newest forty of a chat opened, and the blob cache evicts; so a drawing
/// that finds no bytes says so here and the worker asks the engine for them.
static WANTED_FILES: std::sync::Mutex<Vec<String>> = std::sync::Mutex::new(Vec::new());

/// Ask for one file by the durable remote id the row keeps — a picture whose
/// bytes are not on this device. The worker turns it into a `getRemoteFile`
/// on its next pass and the answer into a download, which lands in the blob
/// cache under the row's own key; the next draw finds it. Deduplicated, since
/// every draw of a row without its picture asks again.
pub fn want_file(rid: &str) {
    let mut wanted = WANTED_FILES.lock().unwrap_or_else(|e| e.into_inner());
    if !wanted.iter().any(|w| w == rid) {
        wanted.push(rid.to_string());
    }
}

/// Ask for a chat's transcript to be filled from the wire — what a chat does
/// as it opens. The worker walks it a page at a time, paced, the chat just
/// opened first; this only queues it. The chat reads as loading until the
/// walk ends ([`progress`]).
pub fn want_history(chat: PeerId) {
    progress::set_loading(chat, true);
    let mut wanted = WANTED.lock().unwrap_or_else(|e| e.into_inner());
    if !wanted.contains(&chat) {
        wanted.push(chat);
    }
}

impl<T: Td> Account<T> {
    #[must_use]
    pub fn new(td: T, api_id: i32, tdlib_dir: PathBuf, phone: Option<String>) -> Account<T> {
        Account {
            td,
            api_id,
            tdlib_dir,
            phone,
            seen: std::cell::RefCell::new(std::collections::HashSet::new()),
            typing: std::cell::RefCell::new(std::collections::HashMap::new()),
            pages: std::cell::RefCell::new(std::collections::VecDeque::new()),
            in_flight: std::cell::Cell::new(None),
            not_before: std::cell::Cell::new(0.0),
        }
    }

    /// Queues the first page of a chat's fill, at the front: the chat just
    /// opened is the one the account holder is looking at.
    pub fn want(&self, chat: PeerId) {
        progress::set_loading(chat, true);
        let mut pages = self.pages.borrow_mut();
        let page = Page {
            chat,
            from: 0,
            walk: Walk::Fill,
        };
        if !pages.contains(&page) {
            pages.push_front(page);
        }
    }

    /// Sends the next history page, if one is due: none on the wire (or one
    /// waited on past patience), the pace kept, the wait Telegram asked for
    /// honoured. The chats the panels wanted since the last pass go to the
    /// front first. One page per pass at most.
    fn pump(&self, w: &World) {
        // Files a drawing found missing go first, and single lines asked for
        // with them: few, cheap, and somebody is looking at each.
        let files: Vec<String> =
            std::mem::take(&mut *WANTED_FILES.lock().unwrap_or_else(|e| e.into_inner()));
        for rid in files {
            self.td.send(&request_file(&rid));
        }
        let lines: Vec<(PeerId, MsgId)> =
            std::mem::take(&mut *WANTED_LINES.lock().unwrap_or_else(|e| e.into_inner()));
        for (chat, id) in lines {
            self.td.send(&get_message(chat, id));
        }
        let wanted: Vec<PeerId> = std::mem::take(&mut *WANTED.lock().unwrap_or_else(|e| e.into_inner()));
        for chat in wanted {
            self.want(chat);
        }
        let now = w.now();
        if let Some(sent) = self.in_flight.get() {
            if now - sent < PAGE_PATIENCE {
                return;
            }
            self.in_flight.set(None);
        }
        if now < self.not_before.get() {
            return;
        }
        let Some(page) = self.pages.borrow_mut().pop_front() else {
            return;
        };
        self.in_flight.set(Some(now));
        self.not_before.set(now + PAGE_GAP);
        self.td.send(&get_chat_history(page.chat, page.from, page.walk));
    }

    /// Files a write's outcome. A refused write goes to the trace and, once
    /// per distinct error, to stderr: a store whose table lacks a column
    /// refuses every line the same way, and two thousand copies of one
    /// sentence bury it. Silence here cost a night — every message upsert
    /// failing on a first-shape store while the sign-in panel said the chats
    /// were syncing (2026-09-07) — so nothing the worker writes is dropped
    /// unheard again.
    fn filed<R, E: std::fmt::Display>(&self, what: &str, r: Result<R, E>) {
        if let Err(e) = r {
            let line = format!("{what}: {e}");
            self.log(&format!("!! {line}"));
            if self.seen.borrow_mut().insert(line.clone()) {
                eprintln!("telegram: a write failed — {line}");
            }
        }
    }

    /// Drains every update the engine has ready — `receive(0.0)` until it
    /// answers `None` — acting on each. Answers how many were consumed.
    /// Called from the worker's pass; it never blocks.
    ///
    /// The pass begins by letting a stale *typing…* go
    /// ([`expire_typing`](Account::expire_typing)): that is a thing the clock
    /// says, not the wire, so it belongs to the pass rather than to any
    /// update.
    pub fn drain(&self, w: &World) -> usize {
        self.expire_typing(w);
        let mut n = 0;
        while let Some(raw) = self.td.receive(0.0) {
            self.on_update(w, &raw);
            n += 1;
        }
        self.pump(w);
        n
    }

    /// One update. An `updateAuthorizationState` drives the sign-in;
    /// everything else is content for [`handle_update`](Account::handle_update),
    /// the phase-3e seam. An unparseable line is dropped, not fatal — the
    /// wire's framing is TDLib's to keep, not ours.
    pub fn on_update(&self, w: &World, raw: &str) {
        let Ok(v) = serde_json::from_str::<Value>(raw) else {
            self.log(&format!("<< UNPARSEABLE {raw}"));
            return;
        };
        match v["@type"].as_str() {
            Some("updateAuthorizationState") => {
                self.log(&format!(
                    "<< auth {}",
                    v["authorization_state"]["@type"].as_str().unwrap_or("?")
                ));
                self.on_auth(w, &v["authorization_state"]);
            }
            // An error object is not an update; the flow ignores it, but a
            // trace must show it — a bad phone, a flood wait, a rejected
            // parameter all arrive this way. A 404 to the chat-list load is
            // the one error that means something: the list is complete.
            Some("error") => {
                self.log(&format!("<< ERROR {v}"));
                self.on_reply(w, &v, true);
            }
            Some("ok") => {
                self.log("<< ok");
                self.on_reply(w, &v, false);
            }
            Some("file") => {
                self.log("<< file");
                self.on_file_answer(&v);
            }
            // A single line fetched afresh ([`want_line`]) answers as the
            // message itself, which lands the way a new line does.
            Some("message") => {
                self.log("<< message");
                self.on_new_message(w, &v);
            }
            other => {
                self.log(&format!("<< {}", other.unwrap_or("?")));
                self.handle_update(w, &v);
            }
        }
    }

    /// Appends one line to `tg-debug.log` beside the store — what the worker
    /// received and sent, for diagnosing a live sign-in. Only the real build
    /// writes it; a test (no `tdlib` feature) is silent, so no test spatters
    /// a temp dir with logs.
    #[cfg(feature = "tdlib")]
    pub fn log(&self, line: &str) {
        use std::io::Write as _;
        let path = self
            .tdlib_dir
            .parent()
            .unwrap_or(&self.tdlib_dir)
            .join("tg-debug.log");
        if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(path) {
            let secs = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_secs())
                .unwrap_or(0);
            let _ = writeln!(f, "{secs} {line}");
        }
    }

    #[cfg(not(feature = "tdlib"))]
    fn log(&self, _line: &str) {}

    /// One authorization state: fire what TDLib waits for, or record what the
    /// user must answer to, and write the session row either way.
    fn on_auth(&self, w: &World, st: &Value) {
        let now = w.now();
        // The state row, written on the store's writer thread. Only the owned
        // arguments cross; `w` is not captured, so the closure is `Send`.
        let write = |phone: Option<String>, state: &'static str, detail: Option<String>| {
            self.filed("on_auth", w.store().write(move |c| {
                schema::set_session(c, phone.as_deref(), state, detail.as_deref(), now)
            }));
        };
        match st["@type"].as_str() {
            // Handshake: TDLib wants the application's own parameters before
            // anything else. The api_hash is the secret half, read from the
            // account holder's keychain and never from a file.
            Some("authorizationStateWaitTdlibParameters") => {
                let api_hash = w
                    .with_cap::<dyn Secrets, _>(|s| s.get("tg/api_hash"))
                    .ok()
                    .flatten()
                    .unwrap_or_default();
                self.log(&format!(
                    ">> setTdlibParameters api_id={} api_hash_len={} dir={}",
                    self.api_id,
                    api_hash.len(),
                    self.tdlib_dir.display()
                ));
                self.td.send(&set_tdlib_parameters(
                    self.api_id,
                    &api_hash,
                    &self.tdlib_dir,
                ));
                write(None, "connecting", None);
            }
            // The phone. If the file configured one, send it and move on; if
            // not, wait for the UI to supply it through `set_phone`.
            Some("authorizationStateWaitPhoneNumber") => {
                if let Some(phone) = self.phone.clone() {
                    self.log(&format!(">> setAuthenticationPhoneNumber {phone}"));
                    self.td.send(&set_authentication_phone(&phone));
                    write(Some(phone), "wait_phone", None);
                } else {
                    self.log(">> (no phone configured, waiting for the UI)");
                    write(None, "wait_phone", None);
                }
            }
            // The code, and the password, come from the user through the UI —
            // never auto-sent. The row records the step and the hint; the UI
            // reads it and calls `check_code` / `check_password`.
            Some("authorizationStateWaitCode") => write(None, "wait_code", code_detail(st)),
            Some("authorizationStateWaitPassword") => {
                let hint = st["password_hint"]
                    .as_str()
                    .filter(|s| !s.is_empty())
                    .map(str::to_string);
                write(None, "wait_password", hint);
            }
            // Signed in. The dialog list loads from here — phase 3e's work,
            // begun by `on_ready`.
            Some("authorizationStateReady") => {
                write(None, "ready", None);
                self.on_ready(w);
            }
            Some("authorizationStateLoggingOut") => write(None, "logging_out", None),
            Some("authorizationStateClosing" | "authorizationStateClosed") => {
                write(None, "closed", None);
            }
            // States this phase does not sign in through — WaitRegistration,
            // WaitEmailAddress, WaitOtherDeviceConfirmation — are left for a
            // later phase rather than half-answered here.
            _ => {}
        }
    }

    /// Supplies the phone the sign-in UI collected when the `telegram` file
    /// carried none, and sends it. TDLib answers with the next auth state,
    /// which moves the row on from 'wait_phone'.
    pub fn set_phone(&mut self, phone: &str) {
        self.phone = Some(phone.to_string());
        self.td.send(&set_authentication_phone(phone));
    }

    /// Sends the login code the user entered. TDLib answers with the next
    /// auth state — 'ready', or 'wait_password' for a two-factor account —
    /// which drives the row; the code is never auto-sent from an update.
    pub fn check_code(&self, code: &str) {
        self.td.send(&check_authentication_code(code));
    }

    /// Sends the two-factor password the user entered.
    pub fn check_password(&self, password: &str) {
        self.td.send(&check_authentication_password(password));
    }

    /// Signed in — the seam where the dialog list loads. Asking TDLib to load
    /// the main chat list is enough: each chat then arrives as an
    /// `updateNewChat`, and every peer, chat and message rides its own update
    /// into [`handle_update`](Account::handle_update), which projects it. So
    /// the list fills itself once this one request is out.
    ///
    /// The list comes a page at a time: each `ok` to the load asks for the
    /// next page, the 404 that ends the main list starts the archive's, and
    /// the archive's 404 ends the load ([`on_reply`](Account::on_reply)).
    /// The transcripts themselves fill as chats open ([`backfill`]).
    ///
    /// A line still `sending` from before is a phantom: with no message
    /// database on the engine's side, no pending send survives a restart,
    /// so the echo that stayed behind is cleared here rather than drawn
    /// forever as *sending…*.
    pub fn on_ready(&self, w: &World) {
        self.filed(
            "on_ready",
            w.store().write(|c| {
                c.execute("DELETE FROM tg_message WHERE state = 'sending'", [])
                    .map(|_| ())
            }),
        );
        progress::set_list_syncing(true);
        self.td.send(&load_chats(ChatList::Main));
    }

    /// A plain reply — `ok` or an error — to one of this account's own
    /// requests, told apart by the `@extra` it wore: the chat-list load, a
    /// history page, and a send.
    ///
    /// The list load: `ok` says a page landed and there may be another, an
    /// error (TDLib's 404) that the list is complete — after the main list
    /// the archive is loaded the same way, and after the archive nothing.
    fn on_reply(&self, w: &World, v: &Value, failed: bool) {
        match (v["@extra"].as_str(), failed) {
            (Some("load_chats:main"), false) => self.td.send(&load_chats(ChatList::Main)),
            (Some("load_chats:main"), true) | (Some("load_chats:archive"), false) => {
                self.td.send(&load_chats(ChatList::Archive));
            }
            (Some("load_chats:archive"), true) => progress::set_list_syncing(false),
            // A history page refused. Telegram's *too many requests* says
            // how long to hold off: the page goes back to the front and the
            // whole queue waits that long. Anything else — a chat gone
            // private, not found — ends that chat's walk.
            (Some(extra), true) if extra.starts_with("history:") => {
                self.in_flight.set(None);
                let Some((chat, walk, from)) = parse_history_extra(extra) else {
                    return;
                };
                let wait = (v["code"].as_i64() == Some(429))
                    .then(|| retry_after(v["message"].as_str().unwrap_or("")))
                    .flatten();
                match wait {
                    Some(secs) => {
                        self.not_before.set(w.now() + secs + 1.0);
                        self.pages.borrow_mut().push_front(Page { chat, from, walk });
                    }
                    None => progress::set_loading(chat, false),
                }
            }
            // A send refused — a line past the length limit, an attachment
            // that is not there, a chat one may not write in. The composer
            // was emptied the moment the request was queued, so the words are
            // nowhere but in the `@extra`: they go back on the chat as its
            // draft, and the open transcript takes them into the field on its
            // next draw ([`Chat::card`](crate::apps::telegram::Chat)). Onto a
            // composer somebody has typed in since they do not go — that
            // draft is newer than this one.
            //
            // The line answered and the files carried are not given back:
            // neither is on the row, and a send is the one place the panel
            // lets go of them. What was written is the half that cannot be
            // typed again from what is on the screen.
            (Some(extra), true) if extra.starts_with("send:") => {
                let Some((chat, text)) = parse_send_extra(extra) else {
                    return;
                };
                self.log(&format!(
                    "!! send to {chat} refused: {} {}",
                    v["code"],
                    v["message"].as_str().unwrap_or("")
                ));
                if text.is_empty() {
                    return;
                }
                let text = text.to_string();
                self.filed(
                    "on_reply/send",
                    w.store().write(move |c| {
                        c.execute(
                            "UPDATE tg_chat SET draft = ?2
                             WHERE peer = ?1 AND COALESCE(draft, '') = ''",
                            rusqlite::params![chat, text],
                        )
                        .map(|_| ())
                    }),
                );
            }
            _ => {}
        }
    }

    /// One of the engine's own options. `my_id` is the one that matters here:
    /// it names the account holder, and nothing else in the store does.
    /// Without it *saved messages* opens the demo world's stand-in self
    /// (`seed::SELF`) however signed in the build is, that being the only
    /// self a store with no account has ever had.
    ///
    /// The peer is stubbed first — the option arrives before the user object
    /// it belongs to — and the demo's own self gives the flag up, there being
    /// one account holder and one row that may say so.
    fn on_option(&self, w: &World, u: &Value) {
        if u["name"].as_str() != Some("my_id") {
            return;
        }
        // An int64 crosses TDLib's JSON interface as a number or as a string,
        // by build and by field; read either, the way a chat position's order
        // is read either way.
        let v = &u["value"]["value"];
        let Some(id) = v.as_i64().or_else(|| v.as_str()?.parse().ok()) else {
            return;
        };
        self.log(&format!("<< my_id {id}"));
        self.filed(
            "on_option",
            w.store().write(move |c| {
                ensure_peer(c, id)?;
                c.execute("UPDATE tg_peer SET is_self = 1 WHERE id = ?1", [id])?;
                // One account holder, one row that says so: the demo world's
                // stand-in self gives the flag up, and so would an account
                // signed out of before this one.
                c.execute(
                    "UPDATE tg_peer SET is_self = 0 WHERE is_self = 1 AND id <> ?1",
                    [id],
                )?;
                Ok(())
            }),
        );
    }

    /// A `file` answer to a [`request_file`] — a clip the viewer opened on,
    /// or a picture a drawing found no bytes for. The engine has given the
    /// file a session-local id, and the download is asked for at once, at
    /// the front of the queue — the account holder is looking at it. The
    /// finished file rides `updateFile` into the blob cache like any other,
    /// under the `tg:` key the row already names.
    ///
    /// Both spellings of the `@extra` are ours: `clip:` is what the viewer's
    /// [`request_clip`] wore before a picture could be asked for the same
    /// way, and answers to it are still in flight from a session that began
    /// under it.
    fn on_file_answer(&self, v: &Value) {
        if !v["@extra"]
            .as_str()
            .is_some_and(|x| x.starts_with("file:") || x.starts_with("clip:"))
        {
            return;
        }
        // What the engine knows of the file, in the trace: one that never
        // downloads is diagnosed from here (size, whether it can be
        // downloaded at all, whether a download is already under way).
        self.log(&format!(
            "<< file id={} size={} expected={} can_download={} active={} completed={} path={:?}",
            v["id"],
            v["size"],
            v["expected_size"],
            v["local"]["can_be_downloaded"],
            v["local"]["is_downloading_active"],
            v["local"]["is_downloading_completed"],
            v["local"]["path"].as_str().unwrap_or("")
        ));
        if let Some(id) = v["id"].as_i64().and_then(|n| i32::try_from(n).ok()) {
            self.log(&format!(">> downloadFile {id} priority=32"));
            self.td.send(&download_file(id, 32));
        }
    }

    /// Every non-auth update: new, edited and deleted messages, new chats and
    /// their moving flags, peers, the groups behind them and what those count,
    /// and finished downloads. Each is mapped by [`updates`](super::updates)
    /// and projected through [`project`](super::project); an update this phase
    /// does not know is dropped in silence, the framing being TDLib's to keep.
    pub fn handle_update(&self, w: &World, update: &Value) {
        match update["@type"].as_str() {
            Some("updateNewMessage") => self.on_new_message(w, &update["message"]),
            Some("updateMessageContent") => self.on_message_content(w, update),
            Some("updateMessageEdited") => self.on_message_edited(w, update),
            Some("updateDeleteMessages") => self.on_delete_messages(w, update),
            Some("updateNewChat") => self.on_new_chat(w, &update["chat"]),
            Some("updateChatLastMessage") => self.on_new_message(w, &update["last_message"]),
            Some("updateChatPosition") => self.on_chat_position(w, update),
            Some("updateChatAddedToList") => self.on_chat_listing(w, update, true),
            Some("updateChatRemovedFromList") => self.on_chat_listing(w, update, false),
            Some("updateChatReadInbox") => self.on_chat_read_inbox(w, update),
            Some("updateChatReadOutbox") => self.on_chat_read_outbox(w, update),
            Some("updateChatNotificationSettings") => self.on_chat_notifications(w, update),
            // What a chat says of itself after its object: a new title, the
            // mention badge, a draft from another device, somebody typing.
            Some("updateChatTitle") => self.on_chat_title(w, update),
            Some("updateChatUnreadMentionCount") => self.on_chat_mentions(w, update),
            Some("updateChatDraftMessage") => self.on_chat_draft(w, update),
            Some("updateChatAction") => self.on_chat_action(w, update),
            Some("updateMessageInteractionInfo") => self.on_interaction(w, update),
            Some("updateMessageSendSucceeded" | "updateMessageSendFailed") => {
                self.on_sent(w, update);
            }
            // Who the account holder is: the one thing the engine says about
            // itself that a row depends on.
            Some("updateOption") => self.on_option(w, update),
            Some("updateUser") => self.on_user(w, &update["user"]),
            Some("updateUserStatus") => self.on_user_status(w, update),
            // The groups behind the chats: how many are in one, how many are
            // here now, what it says about itself.
            Some("updateSupergroup") => self.on_counts(w, updates::supergroup(update)),
            Some("updateSupergroupFullInfo") => self.on_counts(w, updates::supergroup_full(update)),
            Some("updateBasicGroup") => self.on_counts(w, updates::basic_group(update)),
            Some("updateBasicGroupFullInfo") => self.on_basic_group_full(w, update),
            Some("updateChatOnlineMemberCount") => self.on_counts(w, updates::chat_online(update)),
            Some("updateFile") => self.on_file(w, &update["file"]),
            Some("messages") => self.on_history(w, update),
            _ => {}
        }
    }

    /// A new line: map it, make sure its chat and its sender stand as peers so
    /// the foreign keys hold whatever order the wire took, project it, then
    /// trim the chat back to its window. All in one write, so a line and its
    /// trim are one step.
    fn on_new_message(&self, w: &World, message: &Value) {
        let Some(msg) = updates::message(message) else {
            return;
        };
        let (chat, sender) = (msg.chat, msg.sender);
        self.filed("on_new_message", w.store().write(move |c| {
            ensure_peer(c, chat)?;
            model::ensure_chat_tx(c, chat)?;
            if let Some(s) = sender {
                ensure_peer(c, s)?;
            }
            project_messages(c, &[msg])?;
            apply_read_outbox(c, chat)?;
            trim_chat(c, chat)?;
            Ok(())
        }));
        // Ask for the media, if any: TDLib downloads it, `updateFile`
        // completes, and [`on_file`](Account::on_file) ingests the bytes into
        // the blob cache under the same tg: key the row names — the next
        // redraw draws the photo.
        self.fetch(w, &message["content"]);
    }

    /// Asks TDLib for the media a line's content carries — unless the blob
    /// cache already holds it. The cache is the one store of media: a
    /// finished download moves into it and the engine is told to forget its
    /// copy ([`on_file`](Account::on_file)), so asking again for a key the
    /// cache has would only pull the same bytes off the server twice — once
    /// per line that names the same photo. Content with no file id, as the
    /// demo world's is, asks for nothing.
    fn fetch(&self, w: &World, content: &Value) {
        let Some(file_id) = updates::download_id(content) else {
            return;
        };
        let cached = updates::download_key(content).is_some_and(|key| {
            w.with_cap::<dyn Blobs, _>(|b| b.contains(&key))
                .unwrap_or(false)
        });
        if !cached {
            self.td.send(&download_file(file_id, 1));
        }
    }

    /// A line's content edited under it — a caption fixed, a photo swapped.
    /// The text and media columns are rewritten in place, and the full-text
    /// index follows through the update trigger.
    fn on_message_content(&self, w: &World, u: &Value) {
        let (Some(chat), Some(id)) = (u["chat_id"].as_i64(), u["message_id"].as_i64()) else {
            return;
        };
        let (text, media) = updates::content(&u["new_content"], 0.0);
        self.filed("on_message_content", w
            .store()
            .write(move |c| set_content(c, chat, id, &text, media.as_ref())));
        // A swapped-in photo or file is fetched the same way a new line's is.
        self.fetch(w, &u["new_content"]);
    }

    /// A line marked edited: the flag alone, the text having ridden its own
    /// `updateMessageContent` where it changed.
    fn on_message_edited(&self, w: &World, u: &Value) {
        let (Some(chat), Some(id)) = (u["chat_id"].as_i64(), u["message_id"].as_i64()) else {
            return;
        };
        let edited = u["edit_date"].as_i64().unwrap_or(0) > 0;
        self.filed("on_message_edited", w.store().write(move |c| {
            c.execute(
                "UPDATE tg_message SET edited = ?3 WHERE chat = ?1 AND id = ?2",
                rusqlite::params![chat, id, edited],
            )
            .map(|_| ())
        }));
    }

    /// What a line has gathered since it was posted: the views a channel
    /// post counts, the comments under it, the reactions as the one line the
    /// transcript draws. Written straight, not coalesced — the update carries
    /// the interaction info whole, so a reaction taken back is an absence
    /// that must reach the row, else the last emoji a post ever wore would
    /// stay on it forever.
    fn on_interaction(&self, w: &World, u: &Value) {
        let Some(i) = updates::interaction(u) else {
            return;
        };
        self.filed(
            "on_interaction",
            w.store().write(move |c| {
                c.execute(
                    "UPDATE tg_message SET views = ?3, comments = ?4, reactions = ?5
                     WHERE chat = ?1 AND id = ?2",
                    rusqlite::params![i.chat, i.id, i.views, i.comments, i.reactions],
                )
                .map(|_| ())
            }),
        );
    }

    /// Lines deleted. Only a permanent delete removes a row — a `from_cache`
    /// delete merely drops TDLib's copy, the message living on — and the delete
    /// trigger strikes each gone line from the index.
    fn on_delete_messages(&self, w: &World, u: &Value) {
        if u["is_permanent"].as_bool() != Some(true) {
            return;
        }
        let Some(chat) = u["chat_id"].as_i64() else {
            return;
        };
        let ids: Vec<MsgId> = u["message_ids"]
            .as_array()
            .map(|a| a.iter().filter_map(Value::as_i64).collect())
            .unwrap_or_default();
        if ids.is_empty() {
            return;
        }
        self.filed("on_delete_messages", w
            .store()
            .write(move |c| model::delete_lines_tx(c, chat, &ids).map(|_| ())));
    }

    /// A chat arrives: its peer where the chat is the only source of one (a
    /// group or a channel), a stub peer where a user update owes the rest (a
    /// private chat), then the chat row. `INSERT OR IGNORE` on the stub never
    /// overwrites a real peer already filed.
    fn on_new_chat(&self, w: &World, chat: &Value) {
        let Some(ch) = updates::chat(chat) else {
            return;
        };
        let peer = ch.peer;
        let derived = updates::chat_peer(chat);
        let read_outbox = chat["last_read_outbox_message_id"].as_i64().filter(|&m| m != 0);
        self.filed("on_new_chat", w.store().write(move |c| {
            if let Some(p) = derived {
                project_peers(c, &[p])?;
            }
            ensure_peer(c, peer)?;
            project_chats(c, &[ch])?;
            if read_outbox.is_some() {
                c.execute(
                    "UPDATE tg_chat SET read_outbox = ?2 WHERE peer = ?1",
                    rusqlite::params![peer, read_outbox],
                )?;
                apply_read_outbox(c, peer)?;
            }
            Ok(())
        }));
        // The chat object carries its last line; a chat loaded from the list
        // gets its one line from here, the way a new line rides
        // `updateChatLastMessage`.
        if chat["last_message"].is_object() {
            self.on_new_message(w, &chat["last_message"]);
        }
    }

    /// The far side read up to a line: every sent line up to it is read —
    /// the second word of the ticks. The cursor is kept on the chat, so a
    /// line that lands later, from a backfill, is marked by it too.
    fn on_chat_read_outbox(&self, w: &World, u: &Value) {
        let (Some(chat), Some(last)) = (
            u["chat_id"].as_i64(),
            u["last_read_outbox_message_id"].as_i64().filter(|&m| m != 0),
        ) else {
            return;
        };
        self.filed(
            "on_chat_read_outbox",
            w.store().write(move |c| {
                c.execute(
                    "UPDATE tg_chat SET read_outbox = ?2 WHERE peer = ?1",
                    rusqlite::params![chat, last],
                )?;
                apply_read_outbox(c, chat)
            }),
        );
    }

    /// A send settled. While it was pending the engine echoed the line under
    /// a temporary id, and that echo is what the transcript drew as
    /// *sending…*; now the message as the server has it — its real id, the
    /// server's date — replaces it, or on a failure the same line comes back
    /// marked failed. Without this the echo stayed a second copy forever,
    /// still *sending…*, beside the line that had in fact gone.
    fn on_sent(&self, w: &World, u: &Value) {
        let Some(msg) = updates::message(&u["message"]) else {
            return;
        };
        let old = u["old_message_id"].as_i64().unwrap_or(0);
        let (chat, sender) = (msg.chat, msg.sender);
        self.filed(
            "on_sent",
            w.store().write(move |c| {
                if old != 0 && old != msg.id {
                    c.execute(
                        "DELETE FROM tg_message WHERE chat = ?1 AND id = ?2",
                        rusqlite::params![chat, old],
                    )?;
                }
                ensure_peer(c, chat)?;
                model::ensure_chat_tx(c, chat)?;
                if let Some(s) = sender {
                    ensure_peer(c, s)?;
                }
                project_messages(c, &[msg])?;
                apply_read_outbox(c, chat)?;
                Ok(())
            }),
        );
    }

    /// A chat moved in a list: pinned or un-pinned in the main list, or in or
    /// out of the archive. A single position speaks for one list, so only its
    /// flag is touched; a missing chat row is a no-op, the chat not loaded yet.
    fn on_chat_position(&self, w: &World, u: &Value) {
        let Some(chat) = u["chat_id"].as_i64() else {
            return;
        };
        let pos = &u["position"];
        let listed = order_present(&pos["order"]);
        let sql = match pos["list"]["@type"].as_str() {
            // In the main list, or out of it — a chat left, or one that was
            // only ever seen through a forward, sits at order 0 — and pinned
            // while it is in.
            Some("chatListMain") => "UPDATE tg_chat SET in_main = ?2, pinned = ?3 WHERE peer = ?1",
            Some("chatListArchive") => "UPDATE tg_chat SET archived = ?2 WHERE peer = ?1",
            _ => return,
        };
        let pinned = i64::from(listed && pos["is_pinned"].as_bool() == Some(true));
        let archive = sql.contains("archived");
        self.filed(
            "on_chat_position",
            w.store().write(move |c| {
                // Each statement binds exactly its own parameters: the
                // archive's has no pinned to take (review, 2026-09-07: every
                // archive position failed on a third parameter).
                if archive {
                    c.execute(sql, rusqlite::params![chat, i64::from(listed)])
                } else {
                    c.execute(sql, rusqlite::params![chat, i64::from(listed), pinned])
                }
                .map(|_| ())
            }),
        );
    }

    /// A chat joined a list or left one — `updateChatAddedToList`,
    /// `updateChatRemovedFromList` — which is the engine saying outright
    /// what a position only implies: whether the chat is one of mine.
    fn on_chat_listing(&self, w: &World, u: &Value, on: bool) {
        let Some(chat) = u["chat_id"].as_i64() else {
            return;
        };
        let col = match u["chat_list"]["@type"].as_str() {
            Some("chatListMain") => "in_main",
            Some("chatListArchive") => "archived",
            _ => return,
        };
        let sql = format!("UPDATE tg_chat SET {col} = ?2 WHERE peer = ?1");
        self.filed(
            "on_chat_listing",
            w.store().write(move |c| {
                c.execute(&sql, rusqlite::params![chat, i64::from(on)])
                    .map(|_| ())
            }),
        );
    }

    /// A chat's read marker moved: the unread count and the last read line,
    /// which is where the unread rule is drawn. A missing chat row is a no-op.
    fn on_chat_read_inbox(&self, w: &World, u: &Value) {
        let Some(chat) = u["chat_id"].as_i64() else {
            return;
        };
        let unread = u["unread_count"].as_i64().unwrap_or(0);
        let last_read = u["last_read_inbox_message_id"].as_i64().filter(|&m| m != 0);
        self.filed("on_chat_read_inbox", w.store().write(move |c| {
            c.execute(
                "UPDATE tg_chat SET unread = ?2, last_read = COALESCE(?3, last_read) WHERE peer = ?1",
                rusqlite::params![chat, unread, last_read],
            )
            .map(|_| ())
        }));
    }

    /// A chat's notification settings changed — muted here, or on another of
    /// the account's devices. TDLib says it in seconds: any `mute_for` at all
    /// is muted, nought is not. A missing chat row is a no-op.
    fn on_chat_notifications(&self, w: &World, u: &Value) {
        let Some(chat) = u["chat_id"].as_i64() else {
            return;
        };
        let muted = u["notification_settings"]["mute_for"].as_i64().unwrap_or(0) > 0;
        self.filed("on_chat_notifications", w.store().write(move |c| {
            c.execute(
                "UPDATE tg_chat SET muted = ?2 WHERE peer = ?1",
                rusqlite::params![chat, muted],
            )
            .map(|_| ())
        }));
    }

    /// A chat renamed. The title is the peer's, a chat and its peer sharing
    /// one id, so this is a targeted `UPDATE` on `tg_peer` rather than a
    /// projection: an upsert would have to invent a kind, and the kind is the
    /// chat object's to say. The row is stubbed first, a title being able to
    /// arrive before the chat that wears it.
    fn on_chat_title(&self, w: &World, u: &Value) {
        let Some((chat, title)) = updates::chat_title(u) else {
            return;
        };
        self.filed(
            "on_chat_title",
            w.store().write(move |c| {
                ensure_peer(c, chat)?;
                c.execute(
                    "UPDATE tg_peer SET name = ?2 WHERE id = ?1",
                    rusqlite::params![chat, title],
                )
                .map(|_| ())
            }),
        );
    }

    /// One of a chat's unread lines names me, or none does any longer: the
    /// badge the list draws beside the count. A missing chat row is a no-op.
    fn on_chat_mentions(&self, w: &World, u: &Value) {
        let Some((chat, mention)) = updates::chat_mentions(u) else {
            return;
        };
        self.filed(
            "on_chat_mentions",
            w.store().write(move |c| {
                c.execute(
                    "UPDATE tg_chat SET mention = ?2 WHERE peer = ?1",
                    rusqlite::params![chat, mention],
                )
                .map(|_| ())
            }),
        );
    }

    /// A draft, from whichever device typed it — the phone's half-written
    /// line shows here, as it does on the client. A null draft is a draft
    /// cleared and writes `NULL`: this is the one field where an absence is
    /// the news.
    fn on_chat_draft(&self, w: &World, u: &Value) {
        let Some((chat, text)) = updates::chat_draft(u) else {
            return;
        };
        self.filed(
            "on_chat_draft",
            w.store().write(move |c| {
                c.execute(
                    "UPDATE tg_chat SET draft = ?2 WHERE peer = ?1",
                    rusqlite::params![chat, text],
                )
                .map(|_| ())
            }),
        );
    }

    /// Somebody is at the keyboard in a chat. The column holds the *name*, as
    /// the store knows it, rather than an id — the header and the list draw a
    /// word, and a sender the store has never heard of is *someone* rather
    /// than a hole.
    ///
    /// A deadline is kept beside the write ([`TYPING_FOR`]), because the
    /// server's `chatActionCancel` is a courtesy and not a promise; the next
    /// pass clears whatever has fallen silent.
    fn on_chat_action(&self, w: &World, u: &Value) {
        let Some(a) = updates::chat_action(u) else {
            return;
        };
        let who = if a.on {
            self.typing.borrow_mut().insert(a.chat, w.now() + TYPING_FOR);
            Some(self.name_of(w, a.who))
        } else {
            self.typing.borrow_mut().remove(&a.chat);
            None
        };
        let chat = a.chat;
        self.filed(
            "on_chat_action",
            w.store().write(move |c| {
                c.execute(
                    "UPDATE tg_chat SET typing = ?2 WHERE peer = ?1",
                    rusqlite::params![chat, who],
                )
                .map(|_| ())
            }),
        );
    }

    /// What the store calls a peer, for the one place a name is wanted
    /// outside a projection. *someone* where the id is unknown or nameless:
    /// an action about a person nobody has been introduced to yet is still
    /// worth drawing.
    fn name_of(&self, w: &World, who: Option<PeerId>) -> String {
        who.and_then(|id| {
            w.store()
                .conn()
                .query_row(
                    "SELECT name FROM tg_peer WHERE id = ?1",
                    [id],
                    |r| r.get::<_, String>(0),
                )
                .ok()
                .filter(|n| !n.is_empty())
        })
        .unwrap_or_else(|| "someone".to_string())
    }

    /// Clears every *typing…* whose six seconds are up, in one write. The
    /// clock is the world's, so a scripted run expires a typing state exactly
    /// where the script says it does.
    fn expire_typing(&self, w: &World) {
        let now = w.now();
        let gone: Vec<PeerId> = {
            let mut deadlines = self.typing.borrow_mut();
            let gone: Vec<PeerId> = deadlines
                .iter()
                .filter(|(_, &until)| until <= now)
                .map(|(&peer, _)| peer)
                .collect();
            for peer in &gone {
                deadlines.remove(peer);
            }
            gone
        };
        if gone.is_empty() {
            return;
        }
        // The ids are the wire's own i64s, so the list is a number list; no
        // string from the wire is ever spelled into SQL.
        let list: Vec<String> = gone.iter().map(PeerId::to_string).collect();
        let sql = format!(
            "UPDATE tg_chat SET typing = NULL WHERE peer IN ({})",
            list.join(", ")
        );
        self.filed(
            "expire_typing",
            w.store().write(move |c| c.execute(&sql, []).map(|_| ())),
        );
    }

    /// A peer, refreshed: name, handle, presence. Upserts, so a later update
    /// with more known about them settles onto the same row.
    fn on_user(&self, w: &World, user: &Value) {
        let Some(p) = updates::peer(user) else {
            return;
        };
        self.filed("on_user", w.store().write(move |c| project_peers(c, &[p])));
    }

    /// A person came online, or went away. The whole of `updateUserStatus`,
    /// which is how a header's *online* and the people list's `@online` move
    /// between the rare refreshes of the user object itself. Written as an
    /// update, the way a group's counts are: the status is all this update
    /// knows, and a peer's name is not a thing to null with it.
    fn on_user_status(&self, w: &World, u: &Value) {
        let Some((id, status, last_seen)) = updates::user_presence(u) else {
            return;
        };
        self.filed(
            "on_user_status",
            w.store().write(move |c| {
                ensure_peer(c, id)?;
                c.execute(
                    "UPDATE tg_peer SET status = ?2, last_seen = COALESCE(?3, last_seen)
                     WHERE id = ?1",
                    rusqlite::params![id, status, last_seen],
                )
                .map(|_| ())
            }),
        );
    }

    /// What a group update counted: its size, how many are here now, what it
    /// says about itself. Written as an update rather than through
    /// [`project_peers`], because a group update is not a peer — it carries no
    /// name and no kind, and those are the chat's to say.
    ///
    /// The row may not stand yet: the group updates come *before* the chats
    /// that hold them, so the stub goes in first ([`ensure_peer`], which never
    /// overwrites) and `updateNewChat` fills in the title behind it. Each
    /// column keeps what it had where the update knew nothing of it, the same
    /// rule the peer upsert follows.
    fn on_counts(&self, w: &World, counts: Option<updates::PeerCounts>) {
        let Some(c) = counts else {
            return;
        };
        self.filed("on_counts", w.store().write(move |conn| {
            ensure_peer(conn, c.id)?;
            conn.execute(
                "UPDATE tg_peer SET members = COALESCE(?2, members),
                                    online = COALESCE(?3, online),
                                    about = COALESCE(?4, about),
                                    admin = COALESCE(?5, admin)
                 WHERE id = ?1",
                rusqlite::params![c.id, c.members, c.online, c.about, c.admin],
            )
            .map(|_| ())
        }));
    }

    /// A basic group's full info: its counts, and — this being the one update
    /// that carries a membership — who is in it. Each member stands as a peer
    /// stub first, the foreign key insisting, and their own `updateUser` names
    /// them.
    ///
    /// A **supergroup's** members arrive through no update at all;
    /// `getSupergroupMembers` pages them and that is a later phase.
    fn on_basic_group_full(&self, w: &World, u: &Value) {
        self.on_counts(w, updates::basic_group_full(u));
        let members = updates::basic_group_members(u);
        if members.is_empty() {
            return;
        }
        self.filed("on_basic_group_full", w.store().write(move |c| {
            for m in &members {
                ensure_peer(c, m.peer)?;
            }
            project_members(c, &members)
        }));
    }

    /// A page of a chat's history, answering a `getChatHistory` this account
    /// sent: its `@extra` names the chat, the walk and the line it was asked
    /// from. The lines are projected as a new one is, all in one write, and
    /// the walk goes on from the page's oldest line:
    ///
    /// - a *fill* walks down from the newest line while pages still bring
    ///   lines the store lacks — the gap an absence left — and on a page it
    ///   already knew whole turns into a tail;
    /// - a *tail* walks down from the oldest line held until the window is
    ///   full ([`HISTORY_KEEP`]) or the chat has nothing older.
    ///
    /// An empty page, or one that brought nothing older than it was asked
    /// from, ends the walk. Media is fetched for the first page only — what
    /// the transcript shows as it opens — never for the thousands beneath.
    fn on_history(&self, w: &World, v: &Value) {
        let Some((chat, walk, from)) = v["@extra"].as_str().and_then(parse_history_extra) else {
            return;
        };
        self.in_flight.set(None);
        let raw = v["messages"].as_array().cloned().unwrap_or_default();
        let batch: Vec<IncomingMessage> = raw.iter().filter_map(updates::message).collect();
        let Some(oldest) = batch.iter().map(|m| m.id).min() else {
            progress::set_loading(chat, false);
            return;
        };
        let newest = batch.iter().map(|m| m.id).max().unwrap_or(oldest);
        let known: i64 = w
            .store()
            .conn()
            .query_row(
                "SELECT COUNT(*) FROM tg_message WHERE chat = ?1 AND id BETWEEN ?2 AND ?3",
                rusqlite::params![chat, oldest, newest],
                |r| r.get(0),
            )
            .unwrap_or(0);
        // Whether lines older than this page were held before it landed:
        // then the page fills a gap above known history, which is worth
        // closing whatever the window holds; else this is the first load,
        // which the cap bounds like a tail.
        let gap = history_window(w.store().conn(), chat)
            .ok()
            .and_then(|(held, _)| held)
            .is_some_and(|held| held < oldest);
        // The pictures of the newest lines only — what the transcript shows
        // as it opens. A whole page's worth queued the engine's downloads
        // behind a hundred thumbnails per chat opened, and a clip the
        // account holder was waiting on behind them all (2026-09-07).
        if from == 0 {
            for m in raw.iter().take(FETCH_ON_OPEN) {
                self.fetch(w, &m["content"]);
            }
        }
        let senders: Vec<PeerId> = batch.iter().filter_map(|m| m.sender).collect();
        let brought = batch.len() as i64;
        self.filed(
            "history",
            w.store().write(move |c| {
                ensure_peer(c, chat)?;
                model::ensure_chat_tx(c, chat)?;
                for s in senders {
                    ensure_peer(c, s)?;
                }
                project_messages(c, &batch)?;
                apply_read_outbox(c, chat)?;
                trim_chat(c, chat)?;
                Ok(())
            }),
        );
        let (held_oldest, count) = history_window(w.store().conn(), chat).unwrap_or((None, 0));
        let progressed = from == 0 || oldest < from;
        // A fill goes on until a page is known whole, whatever the window
        // holds: an absence's gap sits above the lines kept, and the trim
        // keeps the count at the cap while the gap is still open (review,
        // 2026-09-07). Only the tail stops at the cap.
        let room = count < HISTORY_KEEP as i64;
        let next = match walk {
            Walk::Fill if known < brought && progressed && (room || gap) => {
                Some((oldest, Walk::Fill))
            }
            Walk::Fill => held_oldest.filter(|_| room).map(|o| (o, Walk::Tail)),
            Walk::Tail if progressed && room => Some((oldest, Walk::Tail)),
            Walk::Tail => None,
        };
        progress::set_loading(chat, next.is_some());
        if let Some((from, walk)) = next {
            self.pages.borrow_mut().push_back(Page { chat, from, walk });
        }
    }

    /// A file's state changed. On a finished download the bytes are ingested
    /// into the [blob cache](Blobs) under the same `tg:<unique id>` key the
    /// message rows already point at, so a view resolves straight to the cached
    /// file — no row need change, the reference having named the key all along.
    ///
    /// Then the engine is told to forget its copy. The bytes moved, and an
    /// engine still counting the file as downloaded would announce it again,
    /// at a path that is gone, for every later line that names the same photo
    /// — and fetch it off the server afresh the next time it was asked
    /// (`Need to redownload file: Can't find real file path`, in its log).
    /// Forgotten, the file is remote-only on its side, and only a key the
    /// cache lacks is ever asked for again ([`fetch`](Account::fetch)).
    ///
    /// An announcement whose path is already gone when it is read — one the
    /// engine queued before it was told — is nothing to move and no error:
    /// the cache has the bytes.
    ///
    /// **A file that is not the engine's own is never moved.** When the
    /// account holder sends a photo, TDLib takes it as an `inputFileLocal`
    /// and announces it with the holder's own path on disk as the local copy
    /// — and [`ingest`](Blobs::ingest) *moves* what it is handed. Left to it,
    /// sending a picture would take that picture out of the folder it was
    /// chosen from. So a path outside [`tdlib_dir`](Account) is read and
    /// [`put`](Blobs::put) under the same key instead, the file staying where
    /// its owner keeps it, and no `deleteFile` is sent: there is nothing of
    /// the engine's to forget.
    fn on_file(&self, w: &World, file: &Value) {
        let local = &file["local"];
        if local["is_downloading_completed"].as_bool() != Some(true) {
            return;
        }
        let Some(path) = local["path"].as_str().filter(|p| !p.is_empty()) else {
            return;
        };
        let Some(uid) = file["remote"]["unique_id"]
            .as_str()
            .filter(|u| !u.is_empty())
        else {
            return;
        };
        let src = PathBuf::from(path);
        if !src.exists() {
            return;
        }
        let key = format!("tg:{uid}");
        if !src.starts_with(&self.tdlib_dir) {
            // An upload is announced more than once, and the bytes under a
            // remote unique id are those bytes forever: a key the cache holds
            // is not read off the disk again.
            if w.with_cap::<dyn Blobs, _>(|b| b.contains(&key))
                .unwrap_or(false)
            {
                return;
            }
            match std::fs::read(&src) {
                Ok(bytes) => match w.with_cap::<dyn Blobs, _>(|b| b.put(&key, &bytes)) {
                    Ok(Ok(_)) => {}
                    Ok(Err(e)) | Err(e) => {
                        eprintln!("telegram: caching a file of my own failed: {e}");
                    }
                },
                Err(e) => eprintln!("telegram: reading a file of my own failed: {e}"),
            }
            return;
        }
        match w.with_cap::<dyn Blobs, _>(|b| b.ingest(&key, &src)) {
            Ok(Ok(_)) => {
                if let Some(id) = file["id"].as_i64().and_then(|n| i32::try_from(n).ok()) {
                    self.td.send(&delete_file(id));
                }
            }
            Ok(Err(e)) => eprintln!("telegram: caching a download failed: {e}"),
            Err(e) => eprintln!("telegram: {e}"),
        }
    }
}

/// Makes sure a peer id stands as a row before a chat or a message points at
/// it — a stub the real `updateUser` or `updateNewChat` fills in later.
/// `INSERT OR IGNORE`, so it never overwrites what is already there; the kind
/// is guessed from the id's sign, a group's being negative on the wire.
fn ensure_peer(c: &Connection, id: PeerId) -> rusqlite::Result<()> {
    let kind = if id < 0 { "group" } else { "person" };
    c.execute(
        "INSERT OR IGNORE INTO tg_peer(id, kind, name) VALUES(?1, ?2, '')",
        rusqlite::params![id, kind],
    )?;
    Ok(())
}

/// Rewrites one line's text and media columns in place — an edited content.
/// `None` media clears them. The update trigger keeps the index in step.
fn set_content(
    c: &Connection,
    chat: PeerId,
    id: MsgId,
    text: &str,
    media: Option<&Media>,
) -> rusqlite::Result<()> {
    c.execute(
        "UPDATE tg_message SET
           text = ?3, media = ?4, media_label = ?5, media_ref = ?6, media_rid = ?7,
           media_w = ?8, media_h = ?9, media_secs = ?10,
           media_lat = ?11, media_lon = ?12, media_until = ?13,
           media_clip = ?14, media_clip_rid = ?15
         WHERE chat = ?1 AND id = ?2",
        rusqlite::params![
            chat,
            id,
            text,
            media.map(|m| m.kind.as_str()),
            media.and_then(|m| m.label.as_deref()),
            media.and_then(|m| m.reference.as_deref()),
            media.and_then(|m| m.rid.as_deref()),
            media.and_then(|m| m.w),
            media.and_then(|m| m.h),
            media.and_then(|m| m.secs),
            media.and_then(|m| m.lat),
            media.and_then(|m| m.lon),
            media.and_then(|m| m.until),
            media.and_then(|m| m.clip.as_deref()),
            media.and_then(|m| m.clip_rid.as_deref()),
        ],
    )?;
    Ok(())
}

/// Whether a `chatPosition.order` places the chat in its list: an int64 TDLib
/// carries as a string, `"0"` meaning absent. Read either shape.
fn order_present(order: &Value) -> bool {
    order
        .as_str()
        .map(|s| !s.is_empty() && s != "0")
        .or_else(|| order.as_i64().map(|n| n != 0))
        .unwrap_or(false)
}

// -- the requests --------------------------------------------------------------
//
// Small builders, one per request, so the JSON in the type language lives in
// one greppable place rather than inline in the state machine.

/// The one-time handshake. The message, chat and file databases are **off**
/// on purpose: we project what we want into `tg_*` ourselves, so there is one
/// durable view of a chat, not two (see the CR). The binlog under
/// `database_directory` is the only thing TDLib keeps, and it is local.
fn set_tdlib_parameters(api_id: i32, api_hash: &str, dir: &Path) -> String {
    json!({
        "@type": "setTdlibParameters",
        "database_directory": dir.to_string_lossy().into_owned(),
        "use_message_database": false,
        "use_chat_info_database": false,
        "use_file_database": false,
        "use_secret_chats": false,
        "use_test_dc": false,
        "api_id": api_id,
        "api_hash": api_hash,
        "system_language_code": "en",
        "device_model": "superapp",
        "application_version": APP_VERSION,
        "database_encryption_key": "",
    })
    .to_string()
}

/// The phone number the account signs in with. `pub` so the sign-in panel
/// sends the number the account holder typed through the same builder the
/// state machine uses, rather than spelling the JSON a second time.
#[must_use]
pub fn set_authentication_phone(phone: &str) -> String {
    json!({ "@type": "setAuthenticationPhoneNumber", "phone_number": phone }).to_string()
}

/// The login code, from the user through the sign-in panel — never auto-sent
/// from an update. `pub` for the panel, for the reason above.
#[must_use]
pub fn check_authentication_code(code: &str) -> String {
    json!({ "@type": "checkAuthenticationCode", "code": code }).to_string()
}

/// The two-factor password, from the user through the sign-in panel. `pub`
/// for the panel, for the reason above.
#[must_use]
pub fn check_authentication_password(password: &str) -> String {
    json!({ "@type": "checkAuthenticationPassword", "password": password }).to_string()
}

/// The main chat list, a window at a time — the seam `on_ready` opens.
/// The two lists a chat can sit in, as `loadChats` names them.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChatList {
    Main,
    Archive,
}

impl ChatList {
    fn td_type(self) -> &'static str {
        match self {
            ChatList::Main => "chatListMain",
            ChatList::Archive => "chatListArchive",
        }
    }

    fn word(self) -> &'static str {
        match self {
            ChatList::Main => "main",
            ChatList::Archive => "archive",
        }
    }
}

/// Ask TDLib for the next page of a list's chats: each arrives as its own
/// `updateNewChat` and `updateChatPosition`, and the request answers `ok`
/// while there may be more, an error 404 once the list is complete. The
/// `@extra` names the list so [`Account::on_reply`] knows which it is.
fn load_chats(list: ChatList) -> String {
    json!({
        "@type": "loadChats",
        "chat_list": { "@type": list.td_type() },
        "limit": 200,
        "@extra": format!("load_chats:{}", list.word()),
    })
    .to_string()
}

// -- the content verbs, live ---------------------------------------------------
//
// The phase-4 requests: the composer's send, reply and edit, and a line's
// delete and read. TDLib echoes each back as an update — a sent line as
// `updateNewMessage`, an edit as `updateMessageContent`, a delete as
// `updateDeleteMessages` — which the projection lays into `tg_*`, so the
// transcript reflects the wire through the one path it already draws, with no
// optimistic local write of our own.

/// The `inputMessageText` a send or an edit carries: the text as a
/// `formattedText` with no entities of ours — Telegram parses none unasked, so
/// what the composer holds travels as plain text. Shared so a send and an edit
/// spell the content the one way.
/// `clear_draft` is what a send says and an edit does not: the draft the
/// server holds for the chat is the line being sent, and sending it is what
/// ends it — on every device (review, 2026-09-07).
fn input_text(text: &str, clear_draft: bool) -> Value {
    json!({
        "@type": "inputMessageText",
        "text": { "@type": "formattedText", "text": text },
        "clear_draft": clear_draft,
    })
}

/// A text line to a chat, optionally answering one of its messages. The reply
/// is the modern *input* form — `inputMessageReplyToMessage` by message id, the
/// `InputMessageReplyTo` that `sendMessage` takes — not the received-message
/// `messageReplyToMessage`, which is a different, output-only type carrying its
/// own `chat_id`. Left off entirely when nothing is replied to.
#[must_use]
pub fn send_message(chat_id: PeerId, text: &str, reply_to: Option<MsgId>) -> String {
    let mut req = json!({
        "@type": "sendMessage",
        "chat_id": chat_id,
        "input_message_content": input_text(text, true),
    });
    if let Some(id) = reply_to {
        req["reply_to"] = json!({
            "@type": "inputMessageReplyToMessage",
            "message_id": id,
        });
    }
    req["@extra"] = json!(send_extra(chat_id, text));
    req.to_string()
}

/// What a send wears so a refusal can be answered for: the chat it was going
/// to, and the words the composer held. TDLib echoes an `@extra` back on the
/// response, error or not, and there is nothing else on a refusal that says
/// which send it refused.
///
/// The words ride in it because the composer is emptied the moment the
/// request is queued — a send is not a thing one waits on — so by the time
/// the engine says *no* they are nowhere else. The text is last, and read
/// back with a limit, so a line with colons in it comes home whole
/// ([`parse_send_extra`]).
fn send_extra(chat_id: PeerId, text: &str) -> String {
    format!("send:{chat_id}:{text}")
}

/// The chat a refused send was going to and the words it carried, or `None`
/// for any other answer's `@extra`.
fn parse_send_extra(extra: &str) -> Option<(PeerId, &str)> {
    let mut parts = extra.splitn(3, ':');
    if parts.next()? != "send" {
        return None;
    }
    let chat = parts.next()?.parse().ok()?;
    Some((chat, parts.next().unwrap_or_default()))
}

/// The line a send answers, put on a request the way [`send_message`] puts it:
/// the *input* form, by message id, absent entirely when nothing is answered.
/// Shared by the sends that carry something other than text, so all of them
/// spell a reply the one way.
fn with_reply(req: &mut Value, reply_to: Option<MsgId>) {
    if let Some(id) = reply_to {
        req["reply_to"] = json!({
            "@type": "inputMessageReplyToMessage",
            "message_id": id,
        });
    }
}

/// One carried file to a chat, as a message of its own: what
/// [`Carried::kind`](model::Carried::kind) says it is — a picture as a photo,
/// a moving one as a video, a sound as audio, anything else as a document —
/// wrapped in an `inputFileLocal`, which is TDLib's *the bytes are at this
/// path on this machine*; the upload is the engine's.
///
/// The words in the composer ride as the `caption`, so a picture with
/// something written under it is one message and not two, the way the client
/// sends it. That makes the caption the *first* file's alone: the rest go
/// bare, an empty caption being an empty `formattedText` rather than no field
/// — TDLib takes the content object whole.
#[must_use]
pub fn send_file(
    chat_id: PeerId,
    reply_to: Option<MsgId>,
    file: &model::Carried,
    caption: &str,
) -> String {
    // The kind's `@type`, and the field it names its file by: each input
    // content spells its own, `photo` for a photo and so on down.
    let (kind, names_it) = match file.kind() {
        "photo" => ("inputMessagePhoto", "photo"),
        "video" => ("inputMessageVideo", "video"),
        "audio" => ("inputMessageAudio", "audio"),
        _ => ("inputMessageDocument", "document"),
    };
    let mut content = json!({
        "@type": kind,
        "caption": { "@type": "formattedText", "text": caption },
    });
    // The files app spells a path the way it shows it — `~/Downloads/x` —
    // and the engine reads a path as the disk has it (review, 2026-09-07).
    content[names_it] = json!({
        "@type": "inputFileLocal",
        "path": kernel::caps::real_path(&file.path).to_string_lossy(),
    });
    let mut req = json!({
        "@type": "sendMessage",
        "chat_id": chat_id,
        "input_message_content": content,
        "@extra": send_extra(chat_id, caption),
    });
    with_reply(&mut req, reply_to);
    req.to_string()
}

/// Where the device says I am, to a chat. `live_period` nought is the one-off
/// share — a location for an hour is a live one, and that is a later phase —
/// and with it the two fields that only a live location moves, the heading and
/// the radius an alert would fire at, are nought too. The accuracy is nought
/// for *not measured*, this round's place being a constant rather than a
/// reading.
#[must_use]
pub fn send_location(chat_id: PeerId, reply_to: Option<MsgId>, lat: f64, lon: f64) -> String {
    let mut req = json!({
        "@type": "sendMessage",
        "chat_id": chat_id,
        "input_message_content": {
            "@type": "inputMessageLocation",
            "location": {
                "@type": "location",
                "latitude": lat,
                "longitude": lon,
                "horizontal_accuracy": 0,
            },
            "live_period": 0,
            "heading": 0,
            "proximity_alert_radius": 0,
        },
        "@extra": send_extra(chat_id, ""),
    });
    with_reply(&mut req, reply_to);
    req.to_string()
}

/// Lines out of one chat and into another — the pick the client's forward
/// sheet makes. `send_copy` false is the forward proper: each line arrives
/// wearing the *forwarded from* the transcript already draws, rather than as
/// a fresh line of mine, and so `remove_caption` has nothing to remove.
/// `options` null takes the account's own defaults for the send.
///
/// Nothing local is written for it: every copy comes back as its own
/// `updateNewMessage`, the way a send's echo does.
#[must_use]
pub fn forward_messages(chat_id: PeerId, from_chat_id: PeerId, message_ids: &[MsgId]) -> String {
    json!({
        "@type": "forwardMessages",
        "chat_id": chat_id,
        "message_thread_id": 0,
        "from_chat_id": from_chat_id,
        "message_ids": message_ids,
        "options": null,
        "send_copy": false,
        "remove_caption": false,
    })
    .to_string()
}

/// New text over one of my lines — the same `inputMessageText` a send carries.
/// TDLib answers with an `updateMessageContent` the projection rewrites in
/// place, so no local edit is wanted once this is out.
#[must_use]
pub fn edit_message_text(chat_id: PeerId, message_id: MsgId, text: &str) -> String {
    json!({
        "@type": "editMessageText",
        "chat_id": chat_id,
        "message_id": message_id,
        "input_message_content": input_text(text, false),
    })
    .to_string()
}

/// Edits the caption under a line's media — a photo's, a file's — which is
/// its own request: `editMessageText` is for a text line and is refused on
/// a media one (review, 2026-09-07).
#[must_use]
pub fn edit_message_caption(chat_id: PeerId, message_id: MsgId, text: &str) -> String {
    json!({
        "@type": "editMessageCaption",
        "chat_id": chat_id,
        "message_id": message_id,
        "caption": { "@type": "formattedText", "text": text },
    })
    .to_string()
}

/// Deletes lines. `revoke` true is the client's *delete for everyone* — the
/// message goes from every side, not merely my own copy. TDLib answers with an
/// `updateDeleteMessages` the projection strikes the rows on.
#[must_use]
pub fn delete_messages(chat_id: PeerId, message_ids: &[MsgId], revoke: bool) -> String {
    json!({
        "@type": "deleteMessages",
        "chat_id": chat_id,
        "message_ids": message_ids,
        "revoke": revoke,
    })
    .to_string()
}

/// Marks lines seen. `force_read` reads them at the server — the chat's unread
/// count clears, not merely that the lines were shown — so a chat read locally
/// is read on Telegram too. The newest line stands for the whole chat.
#[must_use]
pub fn view_messages(chat_id: PeerId, message_ids: &[MsgId]) -> String {
    json!({
        "@type": "viewMessages",
        "chat_id": chat_id,
        "message_ids": message_ids,
        "force_read": true,
    })
    .to_string()
}

// -- the verbs about a chat ----------------------------------------------------
//
// Mute, pin, archive and leave: what the peer's card and the list's batch do.
// Each is answered by an update of its own — `updateChatNotificationSettings`,
// `updateChatPosition`, `updateChatLastMessage` for a group one has left — so
// the store converges on the engine's word; the panel's own flip is only what
// keeps the bar honest between the press and that answer.

/// How long a muted chat stays muted: the far end of an int32, which is what
/// every Telegram client means by *forever*.
const MUTE_FOREVER: i64 = 2_147_483_647;

/// Mutes a chat, or lets it speak again. TDLib takes the settings object
/// whole rather than a patch, so every field is spelled: `mute_for` is ours —
/// with `use_default_mute_for` off, a default being exactly what would
/// override it — and the rest say *use the account's own*, which is what they
/// said before we touched them.
#[must_use]
pub fn set_chat_muted(chat_id: PeerId, muted: bool) -> String {
    json!({
        "@type": "setChatNotificationSettings",
        "chat_id": chat_id,
        "notification_settings": {
            "@type": "chatNotificationSettings",
            "use_default_mute_for": false,
            "mute_for": if muted { MUTE_FOREVER } else { 0 },
            "use_default_sound": true,
            "sound_id": 0,
            "use_default_show_preview": true,
            "show_preview": false,
            "use_default_mute_stories": true,
            "mute_stories": false,
            "use_default_story_sound": true,
            "story_sound_id": 0,
            "use_default_show_story_sender": true,
            "show_story_sender": false,
            "use_default_disable_pinned_message_notifications": true,
            "disable_pinned_message_notifications": false,
            "use_default_disable_mention_notifications": true,
            "disable_mention_notifications": false,
        },
    })
    .to_string()
}

/// Pins a chat to the top of the main list, or lets it down. The archive
/// keeps its own pins; this one names the list it means. TDLib answers with
/// the `updateChatPosition` [`on_chat_position`](Account::on_chat_position)
/// already lays on the row.
#[must_use]
pub fn toggle_chat_pinned(chat_id: PeerId, pinned: bool) -> String {
    json!({
        "@type": "toggleChatIsPinned",
        "chat_list": {"@type": "chatListMain"},
        "chat_id": chat_id,
        "is_pinned": pinned,
    })
    .to_string()
}

/// Archives a chat, or brings it back. One call does both ways: a chat
/// belongs to the list it is added to, and adding it to the main list is what
/// taking it out of the archive means.
#[must_use]
pub fn add_chat_to_list(chat_id: PeerId, archived: bool) -> String {
    json!({
        "@type": "addChatToList",
        "chat_id": chat_id,
        "chat_list": {
            "@type": if archived { "chatListArchive" } else { "chatListMain" },
        },
    })
    .to_string()
}

/// Leaves a group or a channel. Only the membership goes: the conversation
/// stays on Telegram until it is deleted, and the peer stays known, which is
/// why the local half keeps the `tg_peer` row and takes only the chat
/// ([`model::leave_chat_tx`]).
#[must_use]
pub fn leave_chat(chat_id: PeerId) -> String {
    json!({
        "@type": "leaveChat",
        "chat_id": chat_id,
    })
    .to_string()
}

/// Joins a group or a channel the account merely knows of — one a line was
/// forwarded from, one an invite named. Nothing is written locally: joining
/// is the engine's to confirm, and it does, with the `updateChatPosition`
/// and `updateChatAddedToList` that put the chat in my list
/// ([`on_chat_listing`](Account::on_chat_listing)) — where *leave* has to
/// flip at once because what follows it is an absence.
#[must_use]
pub fn join_chat(chat_id: PeerId) -> String {
    json!({
        "@type": "joinChat",
        "chat_id": chat_id,
    })
    .to_string()
}

/// Deletes my side of a conversation — the messages, and the chat from my
/// list — and nothing of the other person's. `deleteChatHistory` with
/// `revoke` false is the request that means that; TDLib's `deleteChat`
/// deletes for every member wherever it may, which no *delete chat* on a
/// card should quietly do (review, 2026-09-07).
#[must_use]
pub fn delete_chat(chat_id: PeerId) -> String {
    delete_chat_history(chat_id, true)
}

/// Clears my side of a conversation's history and keeps the chat in the
/// list, as the client's *clear history* does.
#[must_use]
pub fn clear_history(chat_id: PeerId) -> String {
    delete_chat_history(chat_id, false)
}

fn delete_chat_history(chat_id: PeerId, remove_from_list: bool) -> String {
    json!({
        "@type": "deleteChatHistory",
        "chat_id": chat_id,
        "remove_from_chat_list": remove_from_list,
        "revoke": false,
    })
    .to_string()
}

/// The draft, sent to the server as a chat is left, so the half-written line
/// is on the phone too — which is the other half of
/// [`on_chat_draft`](Account::on_chat_draft), one client's composer being
/// every client's.
///
/// An empty text sends `draft_message: null`, which is how the type language
/// spells *there is no draft*: clearing one and never having had one are the
/// same request. `message_thread_id` is nought — a thread's own draft is a
/// later phase — and the draft answers no message and carries no date of its
/// own, the server dating it as it arrives.
#[must_use]
pub fn set_chat_draft(chat_id: PeerId, text: Option<&str>) -> String {
    let draft = match text.map(str::trim).filter(|t| !t.is_empty()) {
        Some(t) => json!({
            "@type": "draftMessage",
            "reply_to": null,
            "date": 0,
            "input_message_text": input_text(t, false),
        }),
        None => Value::Null,
    };
    json!({
        "@type": "setChatDraftMessage",
        "chat_id": chat_id,
        "message_thread_id": 0,
        "draft_message": draft,
    })
    .to_string()
}

/// Ask TDLib to fetch a file by its session-local id. Not synchronous — the
/// worker learns it finished from an `updateFile` and ingests the bytes into
/// the blob cache; a low `priority` keeps a photo behind whatever the account
/// holder is looking at. The file lands under the same `tg:` key the row
/// already names, so no row changes and the next redraw resolves the picture.
#[must_use]
pub fn download_file(file_id: i32, priority: i32) -> String {
    json!({
        "@type": "downloadFile",
        "file_id": file_id,
        "priority": priority,
        "synchronous": false,
    })
    .to_string()
}

/// Tell TDLib to forget a file's local copy — the bytes having moved into the
/// blob cache, the one place media lives. Left believing it still has the
/// file, the engine announces it at a path that is gone for every line that
/// names it, and fetches it off the server again the next time it is asked.
/// Forgotten, it is remote-only on the engine's side, and a later
/// `downloadFile` — one the cache did not answer — is a fresh fetch.
#[must_use]
pub fn delete_file(file_id: i32) -> String {
    json!({
        "@type": "deleteFile",
        "file_id": file_id,
    })
    .to_string()
}

/// The most lines one `getChatHistory` asks for — TDLib's own ceiling.
pub const HISTORY_PAGE: i64 = 100;

/// The two walks a history backfill makes, named in the request's `@extra`
/// so the page that answers knows which it belongs to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Walk {
    /// Down from the newest line, while pages bring lines the store lacks.
    Fill,
    /// Down from the oldest line held, until the window is full.
    Tail,
}

impl Walk {
    fn word(self) -> &'static str {
        match self {
            Walk::Fill => "fill",
            Walk::Tail => "tail",
        }
    }

    fn parse(word: &str) -> Option<Walk> {
        match word {
            "fill" => Some(Walk::Fill),
            "tail" => Some(Walk::Tail),
            _ => None,
        }
    }
}

/// Ask TDLib for one page of `chat`'s lines older than `from` — `0` for the
/// newest — as the account's `messages` answer. Its `@extra` carries the
/// chat, the walk and `from`, which is how [`Account::on_history`] knows
/// whose page it reads and whether the page moved at all.
#[must_use]
pub fn get_chat_history(chat: PeerId, from: MsgId, walk: Walk) -> String {
    json!({
        "@type": "getChatHistory",
        "chat_id": chat,
        "from_message_id": from,
        "offset": 0,
        "limit": HISTORY_PAGE,
        "only_local": false,
        "@extra": format!("history:{chat}:{}:{from}", walk.word()),
    })
    .to_string()
}

/// Ask TDLib for one line by its ids — the answer is the `message` itself,
/// re-projected whole. What a viewer sends for a video line that predates the
/// clip's remote id being kept on the row.
#[must_use]
pub fn get_message(chat: PeerId, id: MsgId) -> String {
    json!({
        "@type": "getMessage",
        "chat_id": chat,
        "message_id": id,
        "@extra": format!("line:{chat}:{id}"),
    })
    .to_string()
}

/// The seconds a *Too Many Requests: retry after N* asks for, or `None`
/// for any other message.
fn retry_after(message: &str) -> Option<f64> {
    message
        .rsplit_once("retry after ")
        .and_then(|(_, n)| n.trim().parse::<f64>().ok())
}

/// Ask TDLib for a file by its remote id — the persistent name a file
/// carries, kept on the row as `media_rid` for the picture and `clip_rid`
/// for the clip behind it — so it can be downloaded on demand: the answer is
/// a `file` with a session-local id, on which the worker fires `downloadFile`
/// ([`Account::on_file_answer`]), and the bytes land in the blob cache under
/// the key the row already names. The viewer sends this through the shared
/// transport; a drawing that finds no bytes says [`want_file`] instead and
/// the worker sends it on its next pass.
#[must_use]
pub fn request_file(remote_id: &str) -> String {
    json!({
        "@type": "getRemoteFile",
        "remote_file_id": remote_id,
        // Unknown on purpose: the remote id itself says what the file is —
        // a photo, a video, an animation, a video note — and a wrong type
        // here is a refusal.
        "file_type": null,
        "@extra": format!("file:{remote_id}"),
    })
    .to_string()
}

/// The clip, by the same request: kept as its own name because the viewer
/// asks for a clip and nothing else, and the word says which of a moving
/// picture's two files is meant.
#[must_use]
pub fn request_clip(remote_id: &str) -> String {
    request_file(remote_id)
}

/// The chat, walk and origin a history page's `@extra` names, or `None` for
/// any other answer's.
fn parse_history_extra(extra: &str) -> Option<(PeerId, Walk, MsgId)> {
    let mut parts = extra.split(':');
    if parts.next()? != "history" {
        return None;
    }
    let chat = parts.next()?.parse().ok()?;
    let walk = Walk::parse(parts.next()?)?;
    let from = parts.next()?.parse().ok()?;
    Some((chat, walk, from))
}

/// The hint a `authorizationStateWaitCode` carries, as one line the sign-in
/// panel shows beside the field: the delivery kind and, where TDLib gives it,
/// the code's length — `sms · 5`, `telegramMessage`. `None` when the update
/// carries no type at all.
fn code_detail(st: &Value) -> Option<String> {
    let ty = &st["code_info"]["type"];
    let kind = ty["@type"].as_str()?;
    let short = kind
        .strip_prefix("authenticationCodeType")
        .unwrap_or(kind)
        .to_lowercase();
    Some(match ty["length"].as_i64() {
        Some(n) => format!("{short} · {n}"),
        None => short,
    })
}

// -- the worker ----------------------------------------------------------------

/// How long the worker sleeps between drains. Short, because a TDLib push
/// lands on the shared queue and the only way this pass hears of it is to
/// look — the kernel's pass model fits a brief poll, not a blocking receive
/// that would hold the thread for a minute.
const POLL: Duration = Duration::from_millis(300);

/// The one account this build signs in, in the `action.entity` vocabulary.
/// A single account, as the `telegram` file describes; several accounts are a
/// later phase.
const ACCOUNT_ENTITY: &str = "telegram";

/// One account's worker: it owns the account's transport and drives its
/// authorization state machine. Constructed with the real transport only
/// under the `tdlib` feature — without it there is no worker and Telegram
/// stays the demo world.
///
/// Not registered in [`App::workers`](kernel::app::App::workers) yet: that is
/// phase 3d. This phase defines the type and proves its loop offline.
pub struct TgWorker<T: Td> {
    account: Account<T>,
}

impl<T: Td> TgWorker<T> {
    /// Wraps an account in a worker. [`real`](TgWorker::real) is the
    /// production construction; a test builds one over a
    /// [`FakeTd`](super::transport::FakeTd) account.
    #[must_use]
    pub fn new(account: Account<T>) -> TgWorker<T> {
        TgWorker { account }
    }
}

#[cfg(feature = "tdlib")]
/// The real worker. It reads the api_id and phone from the `telegram` file
/// beside the store and keeps TDLib's binlog in a local `tdlib` directory
/// next to it — never in the store, which replicates.
///
/// Its TDLib client is opened **lazily, on the first pass, exactly once** —
/// never at construction. The kernel rebuilds the worker set on every
/// reconcile to diff [`Worker::name`]s (that is why `App::workers` must be
/// cheap), so opening a client in the constructor would mint a fresh client
/// on every reconcile, and their authorization states would race on TDLib's
/// one shared receive queue — the phone submitted against one, the code
/// prompt against another, and no code ever arriving. Only the single
/// running worker's first pass opens the one client that drives the sign-in.
#[cfg(feature = "tdlib")]
pub struct RealWorker {
    api_id: i32,
    phone: Option<String>,
    tdlib_dir: PathBuf,
    account: Option<Account<RealTd>>,
}

#[cfg(feature = "tdlib")]
impl RealWorker {
    #[must_use]
    pub fn new(db_dir: Option<&Path>) -> RealWorker {
        RealWorker {
            api_id: super::config::api_id(db_dir).unwrap_or(0),
            phone: super::config::phone(db_dir),
            tdlib_dir: db_dir.map_or_else(|| PathBuf::from("tdlib"), |d| d.join("tdlib")),
            account: None,
        }
    }
}

#[cfg(feature = "tdlib")]
impl Worker for RealWorker {
    fn name(&self) -> String {
        ACCOUNT_ENTITY.to_string()
    }

    fn entity(&self) -> Option<String> {
        Some(ACCOUNT_ENTITY.to_string())
    }

    fn claims(&self, job: &Job) -> bool {
        job.entity.as_deref() == Some(ACCOUNT_ENTITY)
    }

    fn pass(&mut self, w: &World) -> Wake {
        // The one client is opened here, once — the first pass of the single
        // running worker, never a reconcile's throwaway construction.
        let (api_id, phone, dir) = (self.api_id, self.phone.clone(), self.tdlib_dir.clone());
        let account = self
            .account
            .get_or_insert_with(|| Account::new(RealTd::new(), api_id, dir, phone));
        account.drain(w);
        Wake::After(POLL)
    }
}

impl<T: Td + Send + 'static> Worker for TgWorker<T> {
    fn name(&self) -> String {
        ACCOUNT_ENTITY.to_string()
    }

    fn entity(&self) -> Option<String> {
        Some(ACCOUNT_ENTITY.to_string())
    }

    /// This account's own jobs — none are filed yet (the verbs are phase 3),
    /// but the shape is right: the thread holding the live session is the one
    /// that runs its work.
    fn claims(&self, job: &Job) -> bool {
        job.entity.as_deref() == Some(ACCOUNT_ENTITY)
    }

    /// One pass: drain every update TDLib has ready, then ask to be woken in
    /// [`POLL`] so a fresh push is picked up promptly. The drain never blocks,
    /// so the thread is handed back at once.
    fn pass(&mut self, w: &World) -> Wake {
        self.account.drain(w);
        Wake::After(POLL)
    }
}

#[cfg(test)]
mod tests {
    use super::{download_file, Account, TgWorker, Walk, HISTORY_PAGE, PAGE_GAP, POLL};
    use crate::apps::telegram::project;
    use crate::apps::telegram::schema::{self, SCHEMA};
    use crate::apps::telegram::transport::{FakeTd, Td};
    use kernel::app::{Capabilities, Env, Mode, Wake, Worker};
    use kernel::caps::{Blobs, ClockSource, FakeClock};
    use kernel::effect::{Registry, World};
    use kernel::store::Store;
    use serde_json::json;
    use std::rc::Rc;

    /// A world over a fresh telegram store — the schema only, no demo seed —
    /// with a fake api_hash planted where the parameters step reads it, so no
    /// real keychain is ever touched.
    fn world() -> World {
        timed_world(&FakeClock::default())
    }

    /// The same, over a clock the caller holds — for what happens *later*: a
    /// typing state falling silent six seconds after the action that set it.
    fn timed_world(clock: &FakeClock) -> World {
        let env = Env {
            clock: ClockSource::Virtual(clock.clone()),
            ..Env::default()
        };
        env.secrets.plant("tg/api_hash", "FAKEHASH");
        let store = Store::open(None, &[&SCHEMA]).expect("a telegram store");
        let mut caps = Capabilities::default();
        kernel::caps::install(Mode::Fake, &env, &mut caps);
        World::new(Rc::new(store), caps, Registry::new())
    }

    /// The engine's own directory, which is what tells a file it downloaded
    /// from one of mine it was merely handed ([`Account::on_file`]).
    fn tdlib_dir() -> std::path::PathBuf {
        std::env::temp_dir().join("superapp-tg-p3c-tdlib")
    }

    /// An account over a fake transport, with the api_id the demo file uses
    /// and a scratch tdlib directory (nothing is written to it but the files
    /// a download test plants — no client runs).
    fn account(td: FakeTd, phone: Option<&str>) -> Account<FakeTd> {
        Account::new(td, 17844, tdlib_dir(), phone.map(str::to_string))
    }

    /// A finished download on disk where the engine keeps them, named for
    /// the test that planted it.
    fn engine_file(name: &str) -> std::path::PathBuf {
        let dir = tdlib_dir();
        std::fs::create_dir_all(&dir).expect("the engine's directory");
        let path = dir.join(format!("{name}-{}.bin", std::process::id()));
        std::fs::write(&path, b"downloaded photo bytes").expect("a downloaded file");
        path
    }

    /// The `updateAuthorizationState` for a bare state, its `@type` and no
    /// more.
    fn auth(state: &str) -> String {
        format!(
            r#"{{"@type":"updateAuthorizationState","authorization_state":{{"@type":"{state}"}}}}"#
        )
    }

    fn state(w: &World) -> String {
        schema::session(w.store().conn()).state
    }

    /// The handshake makes the account send `setTdlibParameters`, and the
    /// api_id and api_hash it was built from ride along in the request.
    #[test]
    fn wait_tdlib_parameters_sends_the_parameters() {
        let td = FakeTd::new();
        let acc = account(td.clone(), None);
        let w = world();
        acc.on_update(&w, &auth("authorizationStateWaitTdlibParameters"));

        assert_eq!(td.sent_types(), vec!["setTdlibParameters".to_string()]);
        let sent: serde_json::Value = serde_json::from_str(&td.sent()[0]).expect("valid JSON");
        assert_eq!(sent["api_id"], 17844);
        assert_eq!(sent["api_hash"], "FAKEHASH");
        // The three databases are off, as the CR insists: we keep the one
        // durable view of a chat.
        assert_eq!(sent["use_message_database"], false);
        assert_eq!(sent["use_file_database"], false);
        assert_eq!(state(&w), "connecting");
    }

    /// A configured phone is sent the moment TDLib asks for one.
    #[test]
    fn wait_phone_number_sends_the_configured_phone() {
        let td = FakeTd::new();
        let acc = account(td.clone(), Some("+4915150525562"));
        let w = world();
        acc.on_update(&w, &auth("authorizationStateWaitPhoneNumber"));

        assert_eq!(
            td.sent_types(),
            vec!["setAuthenticationPhoneNumber".to_string()]
        );
        let sent: serde_json::Value = serde_json::from_str(&td.sent()[0]).unwrap();
        assert_eq!(sent["phone_number"], "+4915150525562");
        let s = schema::session(w.store().conn());
        assert_eq!(s.state, "wait_phone");
        assert_eq!(s.phone.as_deref(), Some("+4915150525562"));
    }

    /// With no phone configured, the flow stops at 'wait_phone' and sends
    /// nothing — the UI must supply one. `set_phone` then sends it.
    #[test]
    fn no_phone_waits_then_set_phone_sends_it() {
        let td = FakeTd::new();
        let mut acc = account(td.clone(), None);
        let w = world();
        acc.on_update(&w, &auth("authorizationStateWaitPhoneNumber"));
        assert!(td.sent().is_empty(), "nothing sent without a phone");
        assert_eq!(state(&w), "wait_phone");

        acc.set_phone("+4915150525562");
        assert_eq!(
            td.sent_types(),
            vec!["setAuthenticationPhoneNumber".to_string()]
        );
        let sent: serde_json::Value = serde_json::from_str(&td.sent()[0]).unwrap();
        assert_eq!(sent["phone_number"], "+4915150525562");
    }

    /// The code step records its state and its hint and sends nothing; the
    /// code arrives from the user through `check_code`, which sends it.
    #[test]
    fn wait_code_records_the_state_then_check_code_sends() {
        let td = FakeTd::new();
        let acc = account(td.clone(), Some("+4915150525562"));
        let w = world();
        let update = r#"{"@type":"updateAuthorizationState","authorization_state":{
            "@type":"authorizationStateWaitCode",
            "code_info":{"type":{"@type":"authenticationCodeTypeSms","length":5}}}}"#;
        acc.on_update(&w, update);

        assert!(td.sent().is_empty(), "a code is never auto-sent");
        let s = schema::session(w.store().conn());
        assert_eq!(s.state, "wait_code");
        assert_eq!(s.detail.as_deref(), Some("sms · 5"), "the hint for the UI");

        acc.check_code("12345");
        assert_eq!(td.sent_types(), vec!["checkAuthenticationCode".to_string()]);
        let sent: serde_json::Value = serde_json::from_str(&td.sent()[0]).unwrap();
        assert_eq!(sent["code"], "12345");
    }

    /// The password step records its state and sends nothing; `check_password`
    /// sends it.
    #[test]
    fn wait_password_records_the_state_then_check_password_sends() {
        let td = FakeTd::new();
        let acc = account(td.clone(), None);
        let w = world();
        acc.on_update(&w, &auth("authorizationStateWaitPassword"));
        assert!(td.sent().is_empty(), "a password is never auto-sent");
        assert_eq!(state(&w), "wait_password");

        acc.check_password("hunter2");
        assert_eq!(
            td.sent_types(),
            vec!["checkAuthenticationPassword".to_string()]
        );
        let sent: serde_json::Value = serde_json::from_str(&td.sent()[0]).unwrap();
        assert_eq!(sent["password"], "hunter2");
    }

    /// Ready writes 'ready' and opens the load-chats seam.
    #[test]
    fn ready_writes_the_ready_state() {
        let td = FakeTd::new();
        let acc = account(td.clone(), None);
        let w = world();
        acc.on_update(&w, &auth("authorizationStateReady"));
        assert_eq!(state(&w), "ready");
        // The seam fired: phase 3e widens this into a full projection.
        assert_eq!(td.sent_types(), vec!["loadChats".to_string()]);
    }

    /// A content update projects rows but never writes the session row or
    /// fires a request: auth and content stay each other's business.
    #[test]
    fn a_content_update_leaves_the_session_and_sends_nothing() {
        let td = FakeTd::new();
        let acc = account(td.clone(), None);
        let w = world();
        acc.on_update(&w, &new_message(7, 1, Some(1), "a line"));
        assert!(td.sent().is_empty(), "content fires no request");
        assert_eq!(state(&w), "closed", "the session row is auth's alone");
        // But the line itself did land — content is projected now.
        assert_eq!(num(&w, "SELECT COUNT(*) FROM tg_message WHERE id = 7"), 1);
    }

    /// The worker's one pass drains every queued update — three pushed, one
    /// pass consumes all three — and asks to be woken on the short poll.
    #[test]
    fn a_pass_drains_every_queued_update() {
        let td = FakeTd::new();
        let acc = account(td.clone(), Some("+4915150525562"));
        let mut worker = TgWorker::new(acc);
        let w = world();

        td.push(auth("authorizationStateWaitTdlibParameters"));
        td.push(auth("authorizationStateWaitPhoneNumber"));
        td.push(auth("authorizationStateWaitCode"));

        let wake = worker.pass(&w);
        assert_eq!(wake, Wake::After(POLL));
        assert!(
            td.receive(0.0).is_none(),
            "the queue drained in the one pass"
        );

        // All three were acted on: parameters and the phone were sent, and
        // the last update won the state row.
        let types = td.sent_types();
        assert!(types.contains(&"setTdlibParameters".to_string()));
        assert!(types.contains(&"setAuthenticationPhoneNumber".to_string()));
        assert_eq!(state(&w), "wait_code");

        // And the worker names itself and claims its own account's jobs.
        assert_eq!(worker.name(), "telegram");
        assert_eq!(worker.entity().as_deref(), Some("telegram"));
    }

    // -- phase 3e: the content projection --------------------------------------

    /// One integer off the store — a count or a single column — for an
    /// assertion that reads what an update wrote.
    fn num(w: &World, sql: &str) -> i64 {
        w.store().conn().query_row(sql, [], |r| r.get(0)).unwrap()
    }

    /// An `updateUser` for a person, as TDLib frames one.
    fn user_update(id: i64, first: &str, last: &str, username: &str) -> String {
        json!({
            "@type": "updateUser",
            "user": {
                "@type": "user",
                "id": id,
                "first_name": first,
                "last_name": last,
                "username": username,
                "phone_number": "",
                "status": {"@type": "userStatusOnline", "expires": 0},
                "is_contact": true,
                "type": {"@type": "userTypeRegular"},
            },
        })
        .to_string()
    }

    /// An `updateNewChat` for a private chat, its id the user's own.
    fn private_chat_update(id: i64, title: &str, unread: i64) -> String {
        json!({
            "@type": "updateNewChat",
            "chat": {
                "@type": "chat",
                "id": id,
                "title": title,
                "type": {"@type": "chatTypePrivate", "user_id": id},
                "unread_count": unread,
                "notification_settings": {"mute_for": 0},
                "positions": [{"list": {"@type": "chatListMain"}, "order": "42", "is_pinned": false}],
            },
        })
        .to_string()
    }

    /// An `updateNewMessage` carrying a text line.
    fn new_message(id: i64, chat: i64, sender: Option<i64>, text: &str) -> String {
        let sender_id = sender.map_or(
            json!(null),
            |s| json!({"@type": "messageSenderUser", "user_id": s}),
        );
        json!({
            "@type": "updateNewMessage",
            "message": {
                "@type": "message",
                "id": id,
                "chat_id": chat,
                "sender_id": sender_id,
                "date": 1_725_000_000,
                "is_outgoing": false,
                "content": {"@type": "messageText", "text": {"text": text}},
            },
        })
        .to_string()
    }

    /// An `updateNewMessage` carrying a photo whose file has the given remote
    /// unique id — the blob-cache key the row must resolve through.
    fn photo_message(id: i64, chat: i64, sender: i64, uid: &str) -> String {
        json!({
            "@type": "updateNewMessage",
            "message": {
                "@type": "message",
                "id": id,
                "chat_id": chat,
                "sender_id": {"@type": "messageSenderUser", "user_id": sender},
                "date": 1_725_000_000,
                "is_outgoing": false,
                "content": {
                    "@type": "messagePhoto",
                    "caption": {"text": "the garden today"},
                    "photo": {"sizes": [
                        {"width": 320, "height": 200, "photo": {"id": 501, "remote": {"unique_id": "thumb"}}},
                        {"width": 1600, "height": 1000, "photo": {"id": 502, "remote": {"unique_id": uid}}},
                    ]},
                },
            },
        })
        .to_string()
    }

    /// An `updateFile` for a finished download sitting at `path`, its bytes
    /// keyed by `uid` on the wire.
    fn file_update(path: &str, uid: &str) -> String {
        json!({
            "@type": "updateFile",
            "file": {
                "@type": "file",
                "id": 55,
                "local": {"@type": "localFile", "path": path, "is_downloading_completed": true},
                "remote": {"@type": "remoteFile", "unique_id": uid},
            },
        })
        .to_string()
    }

    /// An `updateDeleteMessages`, permanent, for the listed ids.
    fn delete_update(chat: i64, ids: &[i64]) -> String {
        json!({
            "@type": "updateDeleteMessages",
            "chat_id": chat,
            "message_ids": ids,
            "is_permanent": true,
            "from_cache": false,
        })
        .to_string()
    }

    /// An `updateChatReadInbox` moving the read marker.
    fn read_inbox_update(chat: i64, unread: i64, last_read: i64) -> String {
        json!({
            "@type": "updateChatReadInbox",
            "chat_id": chat,
            "last_read_inbox_message_id": last_read,
            "unread_count": unread,
        })
        .to_string()
    }

    /// A user, then its chat, then a line in it: a peer, a chat and a message
    /// row all land, the line is findable through the full-text index, and a
    /// later read marker clears the unread count.
    #[test]
    fn a_user_a_chat_and_a_message_project_and_index() {
        let td = FakeTd::new();
        let acc = account(td.clone(), None);
        let w = world();

        td.push(user_update(2, "Vera", "Kovac", "vera"));
        td.push(private_chat_update(2, "Vera Kovac", 1));
        td.push(new_message(4001, 2, Some(2), "quokka rendezvous at seven"));
        assert_eq!(acc.drain(&w), 3, "three updates consumed in the one drain");

        // The peer, with what the user update carried.
        let (name, contact): (String, i64) = w
            .store()
            .conn()
            .query_row(
                "SELECT name, is_contact FROM tg_peer WHERE id = 2",
                [],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap();
        assert_eq!(name, "Vera Kovac");
        assert_eq!(contact, 1, "a contact");
        // The chat, with the unread the update carried.
        assert_eq!(num(&w, "SELECT unread FROM tg_chat WHERE peer = 2"), 1);
        // The message.
        let text: String = w
            .store()
            .conn()
            .query_row("SELECT text FROM tg_message WHERE id = 4001", [], |r| {
                r.get(0)
            })
            .unwrap();
        assert_eq!(text, "quokka rendezvous at seven");
        // And it is findable through the index the projection fed.
        let hits = project::search_local(w.store().conn(), Some(2), "quokka");
        assert_eq!(hits.len(), 1, "the word reaches the line");
        assert_eq!(hits[0].id, 4001);

        // A read marker clears the count.
        acc.on_update(&w, &read_inbox_update(2, 0, 4001));
        assert_eq!(num(&w, "SELECT unread FROM tg_chat WHERE peer = 2"), 0);
    }

    /// A photo message lands with the `photo` kind and its `media_ref` the
    /// blob-cache key `tg:<unique id>`, even with no peer update before it: the
    /// chat and sender are stubbed so the line is never lost to ordering.
    #[test]
    fn a_photo_message_carries_its_kind_and_blob_key() {
        let td = FakeTd::new();
        let acc = account(td.clone(), None);
        let w = world();
        acc.on_update(&w, &photo_message(4100, 2, 2, "garden_uniq_1"));

        let (kind, mref, wdt): (String, String, i64) = w
            .store()
            .conn()
            .query_row(
                "SELECT media, media_ref, media_w FROM tg_message WHERE id = 4100",
                [],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )
            .unwrap();
        assert_eq!(kind, "photo");
        assert_eq!(mref, "tg:garden_uniq_1", "the largest size's remote id");
        assert_eq!(wdt, 1600, "the largest size's width");
    }

    /// One of my lines, as the engine sends it: outgoing, in the given
    /// sending state (`None` once it is on the server), under `kind` —
    /// `updateNewMessage` for the echo, or the settled `message` inside an
    /// `updateMessageSendSucceeded` / `updateMessageSendFailed`.
    fn my_line(id: i64, chat: i64, state: Option<&str>) -> serde_json::Value {
        let sending_state = state.map_or(json!(null), |s| json!({"@type": s}));
        json!({
            "@type": "message",
            "id": id,
            "chat_id": chat,
            "sender_id": {"@type": "messageSenderUser", "user_id": 1},
            "date": 1_725_000_000 + id,
            "is_outgoing": true,
            "sending_state": sending_state,
            "content": {"@type": "messageText", "text": {"text": "Yoooo! New TG client is here!"}},
        })
    }

    /// The (id, state) of every line of mine in a chat, oldest first.
    fn my_lines(w: &World, chat: i64) -> Vec<(i64, String)> {
        w.store()
            .conn()
            .prepare("SELECT id, COALESCE(state, '') FROM tg_message WHERE chat = ?1 AND out = 1 ORDER BY id")
            .unwrap()
            .query_map([chat], |r| Ok((r.get(0)?, r.get(1)?)))
            .unwrap()
            .collect::<Result<_, _>>()
            .unwrap()
    }

    /// The echo of a pending send is one line reading `sending`; when the
    /// send succeeds the settled message replaces it under its real id — one
    /// line, `sent`, never two — and a failure leaves the one line `failed`.
    #[test]
    fn a_pending_echo_is_replaced_when_the_send_settles() {
        let td = FakeTd::new();
        let acc = account(td.clone(), None);
        let w = world();

        let echo = json!({"@type": "updateNewMessage", "message": my_line(900, 2, Some("messageSendingStatePending"))});
        acc.on_update(&w, &echo.to_string());
        assert_eq!(my_lines(&w, 2), vec![(900, "sending".to_string())]);

        let settled = json!({
            "@type": "updateMessageSendSucceeded",
            "message": my_line(1000, 2, None),
            "old_message_id": 900,
        });
        acc.on_update(&w, &settled.to_string());
        assert_eq!(my_lines(&w, 2), vec![(1000, "sent".to_string())], "one line, the server's");
        // The list's own echo of the settled line changes nothing.
        let last = json!({"@type": "updateChatLastMessage", "chat_id": 2, "last_message": my_line(1000, 2, None)});
        acc.on_update(&w, &last.to_string());
        assert_eq!(my_lines(&w, 2), vec![(1000, "sent".to_string())]);

        let echo = json!({"@type": "updateNewMessage", "message": my_line(901, 2, Some("messageSendingStatePending"))});
        acc.on_update(&w, &echo.to_string());
        let failed = json!({
            "@type": "updateMessageSendFailed",
            "message": my_line(901, 2, Some("messageSendingStateFailed")),
            "old_message_id": 901,
            "error": {"@type": "error", "code": 400, "message": "CHAT_WRITE_FORBIDDEN"},
        });
        acc.on_update(&w, &failed.to_string());
        assert_eq!(
            my_lines(&w, 2),
            vec![(901, "failed".to_string()), (1000, "sent".to_string())],
        );
    }

    /// The far side's read cursor marks my sent lines read up to it — those
    /// already there when it moves, and those a backfill brings after, the
    /// cursor being kept on the chat. A pending line is nobody's to read.
    #[test]
    fn the_outbox_cursor_marks_sent_lines_read() {
        let td = FakeTd::new();
        let acc = account(td.clone(), None);
        let w = world();
        for id in [10, 20] {
            let u = json!({"@type": "updateNewMessage", "message": my_line(id, 2, None)});
            acc.on_update(&w, &u.to_string());
        }
        let u = json!({"@type": "updateNewMessage", "message": my_line(30, 2, Some("messageSendingStatePending"))});
        acc.on_update(&w, &u.to_string());

        let read = json!({"@type": "updateChatReadOutbox", "chat_id": 2, "last_read_outbox_message_id": 30});
        acc.on_update(&w, &read.to_string());
        assert_eq!(
            my_lines(&w, 2),
            vec![(10, "read".to_string()), (20, "read".to_string()), (30, "sending".to_string())]
        );

        // A page from the past lands already read, the cursor being past it.
        let page = json!({"@type": "messages", "messages": [my_line(5, 2, None)], "@extra": "history:2:tail:10"});
        acc.on_update(&w, &page.to_string());
        assert_eq!(my_lines(&w, 2)[0], (5, "read".to_string()));

        // A chat object carries the cursor too.
        let chat = json!({"@type": "updateNewChat", "chat": {
            "@type": "chat", "id": 3, "type": {"@type": "chatTypePrivate", "user_id": 3},
            "title": "Vera", "unread_count": 0, "last_read_outbox_message_id": 40,
            "last_message": my_line(40, 3, None),
        }});
        acc.on_update(&w, &chat.to_string());
        assert_eq!(my_lines(&w, 3), vec![(40, "read".to_string())], "the chat's last line, read");
    }

    /// Signed in, the list is loaded a page at a time: each `ok` asks for the
    /// next page, the 404 that ends the main list starts the archive's, and
    /// the archive's 404 ends the load. A phantom `sending` line from before
    /// the restart is cleared on the way.
    #[test]
    fn the_chat_list_is_paged_then_the_archive_and_phantoms_cleared() {
        let td = FakeTd::new();
        let acc = account(td.clone(), None);
        let w = world();
        let u = json!({"@type": "updateNewMessage", "message": my_line(900, 2, Some("messageSendingStatePending"))});
        acc.on_update(&w, &u.to_string());

        acc.on_ready(&w);
        assert!(my_lines(&w, 2).is_empty(), "no pending send survives a restart");
        let lists = |td: &FakeTd| -> Vec<String> {
            td.sent()
                .iter()
                .filter_map(|r| serde_json::from_str::<serde_json::Value>(r).ok())
                .filter(|v| v["@type"] == "loadChats")
                .map(|v| v["chat_list"]["@type"].as_str().unwrap_or("").to_string())
                .collect()
        };
        assert_eq!(lists(&td), vec!["chatListMain"]);

        acc.on_update(&w, &json!({"@type": "ok", "@extra": "load_chats:main"}).to_string());
        assert_eq!(lists(&td), vec!["chatListMain", "chatListMain"], "ok: another page");
        let done = json!({"@type": "error", "code": 404, "message": "Not Found", "@extra": "load_chats:main"});
        acc.on_update(&w, &done.to_string());
        assert_eq!(lists(&td).last().map(String::as_str), Some("chatListArchive"), "404: the archive next");
        acc.on_update(&w, &json!({"@type": "ok", "@extra": "load_chats:archive"}).to_string());
        assert_eq!(lists(&td).len(), 4);
        let done = json!({"@type": "error", "code": 404, "message": "Not Found", "@extra": "load_chats:archive"});
        acc.on_update(&w, &done.to_string());
        assert_eq!(lists(&td).len(), 4, "the archive's 404 ends the load");
    }

    /// A chat object as the engine announces it, with the positions given —
    /// none for a chat merely seen, a main-list one for a chat of mine.
    fn chat_object(id: i64, title: &str, positions: serde_json::Value) -> String {
        json!({"@type": "updateNewChat", "chat": {
            "@type": "chat", "id": id,
            "type": {"@type": "chatTypeSupergroup", "supergroup_id": 5, "is_channel": true},
            "title": title, "unread_count": 0, "positions": positions,
        }})
        .to_string()
    }

    /// Whether a chat sits in the main list, as the store has it.
    fn in_main(w: &World, chat: i64) -> i64 {
        num(w, &format!("SELECT in_main FROM tg_chat WHERE peer = {chat}"))
    }

    /// The engine announces every chat it learns of the same way; only a
    /// position in the main list makes it one of mine. A position that goes
    /// to nought, or the engine saying it was removed from the list, takes
    /// it out; a position back, or the engine saying it was added, puts it
    /// back.
    #[test]
    fn only_a_chat_with_a_place_in_the_list_is_listed() {
        let td = FakeTd::new();
        let acc = account(td.clone(), None);
        let w = world();

        // A channel a line was forwarded from: known, not mine.
        acc.on_update(&w, &chat_object(-1005, "дядя сэм", json!([])));
        assert_eq!(in_main(&w, -1005), 0, "no position: not in the list");
        // One of mine, with its place.
        let mine = json!([{"list": {"@type": "chatListMain"}, "order": "9000", "is_pinned": false}]);
        acc.on_update(&w, &chat_object(-1006, "stelaxis", mine));
        assert_eq!(in_main(&w, -1006), 1);

        // Left (or never joined, after all): the position goes to nought.
        let gone = json!({"@type": "updateChatPosition", "chat_id": -1006,
            "position": {"list": {"@type": "chatListMain"}, "order": "0", "is_pinned": false}});
        acc.on_update(&w, &gone.to_string());
        assert_eq!(in_main(&w, -1006), 0);
        // And back, pinned this time.
        let back = json!({"@type": "updateChatPosition", "chat_id": -1006,
            "position": {"list": {"@type": "chatListMain"}, "order": "9001", "is_pinned": true}});
        acc.on_update(&w, &back.to_string());
        assert_eq!(in_main(&w, -1006), 1);
        assert_eq!(num(&w, "SELECT pinned FROM tg_chat WHERE peer = -1006"), 1);

        // The engine's own word on it.
        let removed = json!({"@type": "updateChatRemovedFromList", "chat_id": -1006, "chat_list": {"@type": "chatListMain"}});
        acc.on_update(&w, &removed.to_string());
        assert_eq!(in_main(&w, -1006), 0);
        let added = json!({"@type": "updateChatAddedToList", "chat_id": -1005, "chat_list": {"@type": "chatListMain"}});
        acc.on_update(&w, &added.to_string());
        assert_eq!(in_main(&w, -1005), 1);
        let archived = json!({"@type": "updateChatAddedToList", "chat_id": -1005, "chat_list": {"@type": "chatListArchive"}});
        acc.on_update(&w, &archived.to_string());
        assert_eq!(num(&w, "SELECT archived FROM tg_chat WHERE peer = -1005"), 1);
    }

    /// A line wanted afresh is asked for on the next pass by its ids, and the
    /// `message` that answers lands as a row — the way an old video line gets
    /// the clip id it was projected without.
    #[test]
    fn a_wanted_line_is_fetched_and_lands() {
        let td = FakeTd::new();
        let acc = account(td.clone(), None);
        let w = world();
        super::want_line(-9_010, 4242);
        acc.drain(&w);
        let req: serde_json::Value = serde_json::from_str(td.sent().last().unwrap()).unwrap();
        assert_eq!(req["@type"], "getMessage");
        assert_eq!(req["chat_id"], -9_010);
        assert_eq!(req["message_id"], 4242);
        assert_eq!(req["@extra"], "line:-9010:4242");

        let mut answer = my_line(4242, -9_010, None);
        answer["@extra"] = json!("line:-9010:4242");
        answer["content"] = json!({"@type": "messageVideo", "caption": {"text": "clip"},
            "video": {"duration": 3, "width": 640, "height": 360,
                      "video": {"id": 9, "remote": {"id": "RID_V", "unique_id": "VU"}},
                      "thumbnail": {"file": {"id": 8, "remote": {"id": "RID_T", "unique_id": "TU"}}}}});
        acc.on_update(&w, &answer.to_string());
        let rid: String = w.store().conn().query_row(
            "SELECT media_clip_rid FROM tg_message WHERE id = 4242", [], |r| r.get(0)).unwrap();
        assert_eq!(rid, "RID_V");
    }

    /// The review's small findings, each pinned: an archive position lands
    /// (it bound a third parameter and failed every time), a content change
    /// carries the clip, a send clears the server's draft while an edit does
    /// not, a media line's edit is a caption edit, a `~/` path is the disk's
    /// by the time the engine reads it, and a fill goes on past a full
    /// window while its pages still bring unknown lines.
    #[test]
    fn the_reviews_small_findings_hold() {
        let td = FakeTd::new();
        let acc = account(td.clone(), None);
        let w = world();

        // The archive position.
        acc.on_update(&w, &chat_object(-1008, "old times", json!([])));
        let shelved = json!({"@type": "updateChatPosition", "chat_id": -1008,
            "position": {"list": {"@type": "chatListArchive"}, "order": "5", "is_pinned": false}});
        acc.on_update(&w, &shelved.to_string());
        assert_eq!(num(&w, "SELECT archived FROM tg_chat WHERE peer = -1008"), 1);

        // A content change carries the clip.
        acc.on_update(&w, &new_message(70, -1008, Some(1), "was text"));
        let swapped = json!({"@type": "updateMessageContent", "chat_id": -1008, "message_id": 70,
            "new_content": {"@type": "messageVideo", "caption": {"text": "now a clip"},
                "video": {"duration": 4, "width": 640, "height": 360,
                    "video": {"id": 91, "remote": {"id": "RID_B", "unique_id": "VB"}},
                    "thumbnail": {"file": {"id": 90, "remote": {"id": "RID_TB", "unique_id": "TB"}}}}}});
        acc.on_update(&w, &swapped.to_string());
        let (clip, rid): (String, String) = w.store().conn().query_row(
            "SELECT media_clip, media_clip_rid FROM tg_message WHERE chat = -1008 AND id = 70",
            [], |r| Ok((r.get(0)?, r.get(1)?))).unwrap();
        assert_eq!((clip.as_str(), rid.as_str()), ("tg:VB", "RID_B"));

        // The draft flag, the caption edit, the path.
        let v = |s: String| serde_json::from_str::<serde_json::Value>(&s).unwrap();
        assert_eq!(v(super::send_message(2, "hi", None))["input_message_content"]["clear_draft"], true);
        assert_eq!(v(super::edit_message_text(2, 5, "hi"))["input_message_content"]["clear_draft"], false);
        let cap = v(super::edit_message_caption(2, 5, "a caption"));
        assert_eq!(cap["@type"], "editMessageCaption");
        assert_eq!(cap["caption"]["text"], "a caption");
        let file = crate::apps::telegram::model::Carried { path: "~/Downloads/report.pdf".into() };
        let sent = v(super::send_file(2, None, &file, ""));
        let path = sent["input_message_content"]["document"]["path"].as_str().unwrap().to_string();
        assert!(!path.starts_with('~') && path.ends_with("/Downloads/report.pdf"), "{path}");
    }

    /// A first load stops at the cap: with nothing held below the pages, a
    /// fill is a tail by another name and ends when the window is full,
    /// rather than fetching pages the trim strikes on landing.
    #[test]
    fn a_first_load_stops_at_the_cap() {
        let td = FakeTd::new();
        let acc = account(td.clone(), None);
        let w = world();
        let keep = super::HISTORY_KEEP as i64;
        // Pages of a hundred, newest first, down to the cap exactly.
        let mut from = 0;
        for page in 0..(keep / 100) {
            let top = 30_000 - page * 100;
            let ids: Vec<i64> = ((top - 99)..=top).rev().collect();
            acc.on_update(&w, &history_page(-1010, &ids, &format!("history:-1010:fill:{from}")));
            from = top - 99;
        }
        assert_eq!(num(&w, "SELECT COUNT(*) FROM tg_message WHERE chat = -1010"), keep);
        assert!(
            !crate::apps::telegram::progress::loading(-1010),
            "the window full and nothing older held: the walk ends"
        );
    }

    /// A window already full still closes an absence's gap: the fill walks
    /// on while pages bring unknown lines, and only the tail stops at the
    /// cap.
    #[test]
    fn a_full_window_still_closes_its_gap() {
        let td = FakeTd::new();
        let acc = account(td.clone(), None);
        let clock = FakeClock::at(7_000.0);
        let w = timed_world(&clock);
        // A chat holding the cap already: lines 1..=KEEP, written straight in
        // (pages would each queue a next page of their own).
        w.store()
            .write(|c| {
                super::ensure_peer(c, -1009)?;
                crate::apps::telegram::model::ensure_chat_tx(c, -1009)?;
                let mut ins = c.prepare(
                    "INSERT INTO tg_message(id, chat, date, text) VALUES(?1, -1009, ?1, 'line')",
                )?;
                for id in 1..=super::HISTORY_KEEP as i64 {
                    ins.execute([id])?;
                }
                Ok(())
            })
            .unwrap();
        assert_eq!(num(&w, "SELECT COUNT(*) FROM tg_message WHERE chat = -1009"), super::HISTORY_KEEP as i64);
        // Newer lines the absence missed: a fill page of unknown ones.
        let newer: Vec<i64> = (10_101..=10_200).rev().collect();
        acc.on_update(&w, &history_page(-1009, &newer, "history:-1009:fill:0"));
        clock.advance(PAGE_GAP + 0.1);
        acc.drain(&w);
        assert_eq!(
            last_history_request(&td),
            Some((10_101, "history:-1009:fill:10101".to_string())),
            "the fill goes on below the page, the window being full or not"
        );
    }

    /// A `messages` answer to a history request: the lines it carries, ids
    /// newest first as TDLib sends them, under the `@extra` the request wore.
    fn history_page(chat: i64, ids: &[i64], extra: &str) -> String {
        let messages: Vec<serde_json::Value> = ids
            .iter()
            .map(|id| {
                json!({
                    "@type": "message",
                    "id": id,
                    "chat_id": chat,
                    "sender_id": {"@type": "messageSenderUser", "user_id": 1},
                    "date": 1_725_000_000 + id,
                    "is_outgoing": false,
                    "content": {"@type": "messageText", "text": {"text": format!("line {id}")}},
                })
            })
            .collect();
        json!({"@type": "messages", "total_count": ids.len(), "messages": messages, "@extra": extra})
            .to_string()
    }

    /// The request the next page rides, as (`from_message_id`, `@extra`).
    fn last_history_request(td: &FakeTd) -> Option<(i64, String)> {
        let last = td.sent().last()?.clone();
        let v: serde_json::Value = serde_json::from_str(&last).ok()?;
        (v["@type"] == "getChatHistory").then(|| {
            (
                v["from_message_id"].as_i64().unwrap_or(-1),
                v["@extra"].as_str().unwrap_or("").to_string(),
            )
        })
    }

    /// A chat wanted goes to the front of the queue and its first page — a
    /// fill from the newest line, asking for TDLib's ceiling of lines,
    /// naming itself in `@extra` — goes on the next pass; nothing more goes
    /// until it is answered, and then not within the pace.
    #[test]
    fn a_wanted_chat_gets_its_first_page_on_the_next_pass_and_no_more() {
        let td = FakeTd::new();
        let acc = account(td.clone(), None);
        let clock = FakeClock::at(1_000.0);
        let w = timed_world(&clock);

        super::want_history(-9_001);
        acc.drain(&w);
        let req: serde_json::Value = serde_json::from_str(&td.sent()[0]).expect("valid JSON");
        assert_eq!(req["@type"], "getChatHistory");
        assert_eq!(req["chat_id"], -9_001);
        assert_eq!(req["from_message_id"], 0);
        assert_eq!(req["limit"], HISTORY_PAGE);
        assert_eq!(req["only_local"], false);
        assert_eq!(req["@extra"], "history:-9001:fill:0");
        assert_eq!(super::parse_history_extra("history:2:fill:0"), Some((2, Walk::Fill, 0)));
        assert_eq!(super::parse_history_extra("history:-100:tail:77"), Some((-100, Walk::Tail, 77)));
        assert_eq!(super::parse_history_extra("kick"), None);

        // A second chat wanted while the first page is on the wire waits.
        acc.want(-9_002);
        acc.drain(&w);
        assert_eq!(td.sent().len(), 1, "one page on the wire at a time");
        // Answered, the next goes — but only once the pace allows.
        acc.on_update(&w, &history_page(-9_001, &[], "history:-9001:fill:0"));
        acc.drain(&w);
        assert_eq!(td.sent().len(), 1, "not within the pace of the last");
        clock.advance(PAGE_GAP);
        acc.drain(&w);
        assert_eq!(last_history_request(&td), Some((0, "history:-9002:fill:0".to_string())));
        assert_eq!(super::retry_after("Too Many Requests: retry after 30"), Some(30.0));
        assert_eq!(super::retry_after("Chat not found"), None);
    }

    /// Telegram refusing a page with *retry after N* holds the whole queue
    /// off for N seconds and puts the page back at the front; a page waited
    /// on past patience is given up on so the walk goes on.
    #[test]
    fn a_refused_page_waits_out_the_retry_and_goes_again() {
        let td = FakeTd::new();
        let acc = account(td.clone(), None);
        let clock = FakeClock::at(5_000.0);
        let w = timed_world(&clock);
        acc.want(-9_003);
        acc.drain(&w);
        assert_eq!(td.sent().len(), 1);
        let refused = json!({"@type": "error", "code": 429, "message": "Too Many Requests: retry after 30",
            "@extra": "history:-9003:fill:0"});
        acc.on_update(&w, &refused.to_string());
        clock.advance(10.0);
        acc.drain(&w);
        assert_eq!(td.sent().len(), 1, "held off");
        clock.advance(22.0);
        acc.drain(&w);
        assert_eq!(td.sent().len(), 2, "gone again after the wait");
        assert_eq!(last_history_request(&td), Some((0, "history:-9003:fill:0".to_string())));
        assert!(crate::apps::telegram::progress::loading(-9_003));

        // A page never answered: patience runs out and the next goes.
        acc.want(-9_004);
        clock.advance(super::PAGE_PATIENCE + 1.0);
        acc.drain(&w);
        assert_eq!(td.sent().len(), 3);
        // Any other refusal ends that chat's walk.
        let gone = json!({"@type": "error", "code": 400, "message": "Chat not found", "@extra": "history:-9004:fill:0"});
        acc.on_update(&w, &gone.to_string());
        assert!(!crate::apps::telegram::progress::loading(-9_004));
    }

    /// A page of unknown lines lands as rows and the fill walks on from the
    /// page's oldest line; a page the store already knew whole turns the walk
    /// into a tail from the oldest line held.
    #[test]
    fn a_history_page_lands_and_the_walk_goes_on() {
        let td = FakeTd::new();
        let acc = account(td.clone(), None);
        let clock = FakeClock::at(100.0);
        let w = timed_world(&clock);
        // The next page goes on the pass after the answer, the pace kept.
        let step = |w: &World| {
            clock.advance(PAGE_GAP + 0.1);
            acc.drain(w);
        };

        // The first page: three lines nobody held.
        acc.on_update(&w, &history_page(2, &[300, 200, 100], "history:2:fill:0"));
        assert_eq!(num(&w, "SELECT COUNT(*) FROM tg_message WHERE chat = 2"), 3);
        step(&w);
        assert_eq!(
            last_history_request(&td),
            Some((100, "history:2:fill:100".to_string())),
            "the fill asks for the page before its oldest line"
        );

        // A page the store knew whole: the gap is closed, the walk turns to
        // the tail, from the oldest line held.
        acc.on_update(&w, &history_page(2, &[300, 200, 100], "history:2:fill:100"));
        step(&w);
        assert_eq!(
            last_history_request(&td),
            Some((100, "history:2:tail:100".to_string())),
            "a known page turns the fill into a tail"
        );

        // The tail brings older lines and walks on from them.
        acc.on_update(&w, &history_page(2, &[100, 90, 80], "history:2:tail:100"));
        assert_eq!(num(&w, "SELECT COUNT(*) FROM tg_message WHERE chat = 2"), 5);
        step(&w);
        assert_eq!(last_history_request(&td), Some((80, "history:2:tail:80".to_string())));
        assert_eq!(td.sent_types().iter().filter(|t| *t == "getChatHistory").count(), 3);
    }

    /// The loading flag a panel reads follows the walk: on from the request,
    /// off when the walk ends — and the list's own from `ready` to the
    /// archive's 404.
    #[test]
    fn the_loading_flags_follow_the_walks() {
        use crate::apps::telegram::progress;
        let td = FakeTd::new();
        let acc = account(td.clone(), None);
        let w = world();
        acc.want(-555);
        assert!(progress::loading(-555), "on from the request");
        acc.on_update(&w, &history_page(-555, &[300, 200], "history:-555:fill:0"));
        assert!(progress::loading(-555), "still on while pages come");
        acc.on_update(&w, &history_page(-555, &[], "history:-555:fill:200"));
        assert!(!progress::loading(-555), "off at the chat's beginning");

        acc.on_ready(&w);
        assert!(progress::list_syncing());
        let done = json!({"@type": "error", "code": 404, "message": "Not Found", "@extra": "load_chats:main"});
        acc.on_update(&w, &done.to_string());
        assert!(progress::list_syncing(), "the archive is still to come");
        let done = json!({"@type": "error", "code": 404, "message": "Not Found", "@extra": "load_chats:archive"});
        acc.on_update(&w, &done.to_string());
        assert!(!progress::list_syncing());
    }

    /// A file is asked for by its remote id — a clip the viewer opened on,
    /// or a picture a drawing found no bytes for; the `file` that answers is
    /// downloaded at once, at the front of the queue, by the id it carries.
    /// The older `clip:` spelling is honoured too: an answer to it may still
    /// be in flight.
    #[test]
    fn a_file_request_is_answered_with_a_download() {
        let td = FakeTd::new();
        let acc = account(td.clone(), None);
        let w = world();
        let req: serde_json::Value = serde_json::from_str(&super::request_file("RID_1")).unwrap();
        assert_eq!(req["@type"], "getRemoteFile");
        assert_eq!(req["remote_file_id"], "RID_1");
        assert_eq!(req["@extra"], "file:RID_1");
        assert_eq!(super::request_clip("RID_1"), super::request_file("RID_1"));
        let answer = json!({"@type": "file", "id": 77, "@extra": "file:RID_1",
            "local": {"is_downloading_completed": false}, "remote": {"unique_id": "u1"}});
        acc.on_update(&w, &answer.to_string());
        assert_eq!(td.sent_types(), vec!["downloadFile".to_string()]);
        let sent: serde_json::Value = serde_json::from_str(&td.sent()[0]).unwrap();
        assert_eq!(sent["file_id"], 77);
        assert_eq!(sent["priority"], 32);
        let older = json!({"@type": "file", "id": 78, "@extra": "clip:RID_0",
            "local": {"is_downloading_completed": false}, "remote": {"unique_id": "u0"}});
        acc.on_update(&w, &older.to_string());
        assert_eq!(td.sent().len(), 2, "a session that began under the old word");
        // Any other file answer is nobody's request here.
        acc.on_update(&w, &json!({"@type": "file", "id": 79, "@extra": "kick"}).to_string());
        assert_eq!(td.sent().len(), 2);
    }

    /// A picture a drawing found missing is asked for on the worker's next
    /// pass, by the durable remote id the row keeps — and once, however many
    /// draws ask for it.
    #[test]
    fn a_wanted_file_is_asked_for_on_the_next_pass() {
        let td = FakeTd::new();
        let acc = account(td.clone(), None);
        let w = world();
        super::want_file("RID_PIC");
        super::want_file("RID_PIC");
        acc.drain(&w);
        // Other tests share the queue this rides on, so the request is
        // picked out by the id it names rather than by its place.
        let asked = || -> Vec<serde_json::Value> {
            td.sent()
                .iter()
                .filter_map(|s| serde_json::from_str::<serde_json::Value>(s).ok())
                .filter(|v| v["remote_file_id"] == "RID_PIC")
                .collect()
        };
        let mine = asked();
        assert_eq!(mine.len(), 1, "asked once");
        assert_eq!(mine[0]["@type"], "getRemoteFile");
        assert_eq!(mine[0]["@extra"], "file:RID_PIC");
        // And the queue is spent: a second pass does not ask again.
        acc.drain(&w);
        assert_eq!(asked().len(), 1);
    }

    /// The walk ends at the chat's beginning: an empty page, or a tail page
    /// that brought nothing older than it was asked from, sends no more.
    #[test]
    fn a_history_walk_ends_where_the_chat_does() {
        let td = FakeTd::new();
        let acc = account(td.clone(), None);
        let w = world();

        acc.on_update(&w, &history_page(2, &[], "history:2:tail:80"));
        assert!(td.sent().is_empty(), "an empty page ends the walk");

        acc.on_update(&w, &history_page(2, &[80], "history:2:tail:80"));
        assert_eq!(num(&w, "SELECT COUNT(*) FROM tg_message WHERE chat = 2"), 1);
        assert!(td.sent().is_empty(), "a page that moved nowhere ends the walk");

        // Any other answer's `@extra` is not a page: nothing is read into it.
        acc.on_update(&w, &history_page(2, &[70], "kick"));
        assert_eq!(num(&w, "SELECT COUNT(*) FROM tg_message WHERE chat = 2"), 1);
    }

    /// A finished download is ingested into the blob cache under its
    /// `tg:<unique id>` key, and the source file moves in rather than being
    /// left behind.
    #[test]
    fn a_completed_download_is_ingested_into_the_cache() {
        let td = FakeTd::new();
        let acc = account(td.clone(), None);
        let w = world();

        // A finished download sitting where the engine puts them.
        let src = engine_file("download");

        acc.on_update(&w, &file_update(&src.to_string_lossy(), "garden_uniq_1"));

        let cached = w
            .with_cap::<dyn Blobs, _>(|b| b.contains("tg:garden_uniq_1"))
            .expect("the world has a blob cache");
        assert!(cached, "the download was ingested under its key");
        assert!(!src.exists(), "the source moved into the cache, not left");

        // And the engine is told to forget its copy, by the file's own id.
        assert_eq!(td.sent_types(), vec!["deleteFile".to_string()]);
        let sent: serde_json::Value = serde_json::from_str(&td.sent()[0]).unwrap();
        assert_eq!(sent["file_id"], 55);
    }

    /// The engine announces a finished file more than once — again for each
    /// later line that names it. After the first ingest the path it names is
    /// gone: the repeat is left alone — no error, no second request — and
    /// the cache still holds the bytes.
    #[test]
    fn a_file_announced_again_after_it_moved_is_left_alone() {
        let td = FakeTd::new();
        let acc = account(td.clone(), None);
        let w = world();
        let src = engine_file("again");
        let announce = file_update(&src.to_string_lossy(), "garden_uniq_2");

        acc.on_update(&w, &announce);
        assert!(!src.exists(), "moved in on the first announcement");
        acc.on_update(&w, &announce);
        acc.on_update(&w, &announce);

        assert_eq!(
            td.sent_types(),
            vec!["deleteFile".to_string()],
            "one forget for the one move; the repeats send nothing"
        );
        let cached = w
            .with_cap::<dyn Blobs, _>(|b| b.contains("tg:garden_uniq_2"))
            .unwrap();
        assert!(cached, "the bytes stayed cached");
    }

    /// A photo the cache already holds is not fetched again — the cache, not
    /// the engine's copy, is where media lives — while one it lacks is asked
    /// for by the largest size's own file id.
    #[test]
    fn a_photo_the_cache_holds_is_not_fetched_again() {
        // A fresh key: the request goes out, naming the largest size.
        let td = FakeTd::new();
        let acc = account(td.clone(), None);
        let w = world();
        acc.on_update(&w, &photo_message(4200, 2, 2, "fresh_uniq"));
        assert_eq!(td.sent_types(), vec!["downloadFile".to_string()]);
        let sent: serde_json::Value = serde_json::from_str(&td.sent()[0]).unwrap();
        assert_eq!(sent["file_id"], 502, "the largest size's id, not the thumbnail's");

        // The same photo named again — a forward, a re-announced last
        // message — once the cache has it: nothing goes out.
        let td = FakeTd::new();
        let acc = account(td.clone(), None);
        let w = world();
        w.with_cap::<dyn Blobs, _>(|b| b.put("tg:held_uniq", b"jpeg bytes"))
            .unwrap()
            .unwrap();
        acc.on_update(&w, &photo_message(4201, 2, 2, "held_uniq"));
        assert!(td.sent().is_empty(), "a cached photo is not fetched again");
        // The row still points at the key it always did.
        let mref: String = w
            .store()
            .conn()
            .query_row("SELECT media_ref FROM tg_message WHERE id = 4201", [], |r| r.get(0))
            .unwrap();
        assert_eq!(mref, "tg:held_uniq");
    }

    /// The download request names the file by its session-local id and its
    /// priority, and asks for the asynchronous form — the worker learns it
    /// finished from an `updateFile`, not from the call.
    #[test]
    fn download_file_names_the_file_and_is_asynchronous() {
        let req: serde_json::Value =
            serde_json::from_str(&download_file(55, 1)).expect("valid JSON");
        assert_eq!(req["@type"], "downloadFile");
        assert_eq!(req["file_id"], 55);
        assert_eq!(req["priority"], 1);
        assert_eq!(req["synchronous"], false);
    }

    /// A permanent delete removes the row and, through the delete trigger, its
    /// full-text entry; a `from_cache` delete leaves the row standing.
    #[test]
    fn a_permanent_delete_removes_the_row_and_its_index() {
        let td = FakeTd::new();
        let acc = account(td.clone(), None);
        let w = world();
        acc.on_update(&w, &new_message(4200, 3, Some(3), "narwhal to be deleted"));
        assert_eq!(
            num(&w, "SELECT COUNT(*) FROM tg_message WHERE id = 4200"),
            1
        );
        assert_eq!(
            project::search_local(w.store().conn(), None, "narwhal").len(),
            1
        );

        // A cache-only delete is ignored — the message still exists.
        acc.on_update(
            &w,
            &json!({
                "@type": "updateDeleteMessages", "chat_id": 3,
                "message_ids": [4200], "is_permanent": false, "from_cache": true,
            })
            .to_string(),
        );
        assert_eq!(
            num(&w, "SELECT COUNT(*) FROM tg_message WHERE id = 4200"),
            1,
            "a from_cache delete keeps the row"
        );

        // The permanent one removes it, and the index follows.
        acc.on_update(&w, &delete_update(3, &[4200]));
        assert_eq!(
            num(&w, "SELECT COUNT(*) FROM tg_message WHERE id = 4200"),
            0
        );
        assert!(
            project::search_local(w.store().conn(), None, "narwhal").is_empty(),
            "the index lost the deleted line"
        );
    }

    /// Malformed or unknown updates never panic and never write: a bad line,
    /// an unknown @type, a message missing its ids, a delete of nothing.
    #[test]
    fn a_malformed_or_unknown_update_writes_nothing() {
        let td = FakeTd::new();
        let acc = account(td.clone(), None);
        let w = world();

        acc.on_update(&w, "this is not json {");
        acc.on_update(
            &w,
            &json!({"@type": "updateSomethingNew", "x": 1}).to_string(),
        );
        acc.on_update(
            &w,
            &json!({"@type": "updateNewMessage", "message": {"@type": "message"}}).to_string(),
        );
        acc.on_update(&w, &json!({"@type": "updateFile", "file": {}}).to_string());
        acc.on_update(&w, &json!({"@type": "updateUser", "user": {}}).to_string());

        assert!(td.sent().is_empty(), "nothing was sent");
        assert_eq!(num(&w, "SELECT COUNT(*) FROM tg_peer"), 0, "no peers");
        assert_eq!(num(&w, "SELECT COUNT(*) FROM tg_chat"), 0, "no chats");
        assert_eq!(num(&w, "SELECT COUNT(*) FROM tg_message"), 0, "no messages");

        // The group updates too: no id, no row.
        acc.on_update(&w, &json!({"@type": "updateSupergroup", "supergroup": {}}).to_string());
        acc.on_update(&w, &json!({"@type": "updateBasicGroupFullInfo"}).to_string());
        acc.on_update(&w, &json!({"@type": "updateChatOnlineMemberCount"}).to_string());
        acc.on_update(&w, &json!({"@type": "updateChatNotificationSettings"}).to_string());
        assert_eq!(num(&w, "SELECT COUNT(*) FROM tg_peer"), 0, "still no peers");
    }

    // -- the counts, and the verbs about a chat --------------------------------

    /// A supergroup's own updates fill the counts a chat cannot carry, under
    /// the chat id its group id stands for — and the chat that arrives after
    /// them, which knows only a title, leaves them standing.
    #[test]
    fn a_supergroups_counts_land_and_a_later_chat_leaves_them() {
        let td = FakeTd::new();
        let acc = account(td.clone(), None);
        let w = world();
        let chat = -1_000_000_001_234_i64;

        acc.on_update(
            &w,
            &json!({"@type": "updateSupergroup",
                    "supergroup": {"id": 1234, "member_count": 812, "is_channel": true}})
            .to_string(),
        );
        assert_eq!(num(&w, "SELECT members FROM tg_peer WHERE id = -1000000001234"), 812);

        acc.on_update(
            &w,
            &json!({"@type": "updateSupergroupFullInfo", "supergroup_id": 1234,
                    "supergroup_full_info": {"member_count": 815, "description": "the letter"}})
            .to_string(),
        );
        acc.on_update(
            &w,
            &json!({"@type": "updateChatOnlineMemberCount",
                    "chat_id": chat, "online_member_count": 12})
            .to_string(),
        );
        let (members, online, about): (i64, i64, String) = w
            .store()
            .conn()
            .query_row(
                "SELECT members, online, about FROM tg_peer WHERE id = ?1",
                [chat],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )
            .unwrap();
        assert_eq!((members, online, about.as_str()), (815, 12, "the letter"));

        // Now the chat itself: a title, a kind, and nothing about the size.
        acc.on_update(
            &w,
            &json!({"@type": "updateNewChat", "chat": {
                "id": chat, "title": "Quokka Weekly",
                "type": {"@type": "chatTypeSupergroup", "is_channel": true},
                "notification_settings": {"mute_for": 0},
            }})
            .to_string(),
        );
        let (kind, name, members, online): (String, String, i64, i64) = w
            .store()
            .conn()
            .query_row(
                "SELECT kind, name, members, online FROM tg_peer WHERE id = ?1",
                [chat],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
            )
            .unwrap();
        assert_eq!(kind, "channel", "the chat is what says what it is");
        assert_eq!(name, "Quokka Weekly");
        assert_eq!((members, online), (815, 12), "the counts stand");
        assert!(td.sent().is_empty(), "a count asks the wire for nothing");
    }

    /// A basic group's full info brings its membership with it: every member
    /// stands as a peer, the creator and the administrators run it, and the
    /// list is the count where none is spelled.
    #[test]
    fn a_basic_groups_full_info_lands_its_members() {
        let td = FakeTd::new();
        let acc = account(td.clone(), None);
        let w = world();

        acc.on_update(
            &w,
            &json!({"@type": "updateBasicGroupFullInfo", "basic_group_id": 77,
                    "basic_group_full_info": {
                        "description": "the hike",
                        "members": [
                            {"member_id": {"@type": "messageSenderUser", "user_id": 2},
                             "status": {"@type": "chatMemberStatusCreator"}},
                            {"member_id": {"@type": "messageSenderUser", "user_id": 3},
                             "status": {"@type": "chatMemberStatusMember"}},
                        ],
                    }})
            .to_string(),
        );
        let (members, about): (i64, String) = w
            .store()
            .conn()
            .query_row("SELECT members, about FROM tg_peer WHERE id = -77", [], |r| {
                Ok((r.get(0)?, r.get(1)?))
            })
            .unwrap();
        assert_eq!((members, about.as_str()), (2, "the hike"));
        assert_eq!(num(&w, "SELECT COUNT(*) FROM tg_member WHERE chat = -77"), 2);
        assert_eq!(
            num(&w, "SELECT admin FROM tg_member WHERE chat = -77 AND peer = 2"),
            1,
            "the creator runs it"
        );
        assert_eq!(
            num(&w, "SELECT admin FROM tg_member WHERE chat = -77 AND peer = 3"),
            0
        );
        // Each member stands as a peer of its own, the foreign key insisting;
        // their own `updateUser` names them later.
        assert_eq!(num(&w, "SELECT COUNT(*) FROM tg_peer WHERE id IN (2, 3)"), 2);
    }

    /// The mute follows the engine's word, whichever device set it.
    #[test]
    fn the_notification_settings_move_the_mute() {
        let td = FakeTd::new();
        let acc = account(td.clone(), None);
        let w = world();
        acc.on_update(&w, &private_chat_update(2, "Vera Kovac", 0));
        assert_eq!(num(&w, "SELECT muted FROM tg_chat WHERE peer = 2"), 0);

        acc.on_update(
            &w,
            &json!({"@type": "updateChatNotificationSettings", "chat_id": 2,
                    "notification_settings": {"mute_for": 2_147_483_647}})
            .to_string(),
        );
        assert_eq!(num(&w, "SELECT muted FROM tg_chat WHERE peer = 2"), 1);

        acc.on_update(
            &w,
            &json!({"@type": "updateChatNotificationSettings", "chat_id": 2,
                    "notification_settings": {"mute_for": 0}})
            .to_string(),
        );
        assert_eq!(num(&w, "SELECT muted FROM tg_chat WHERE peer = 2"), 0);
    }

    /// The four requests a card and a list send about a chat, in the type
    /// language: the whole settings object with our `mute_for` in it, the pin
    /// naming the list it means, the archive as an add to one list or the
    /// other, and the leave.
    #[test]
    fn the_chat_verbs_spell_their_requests() {
        let req: serde_json::Value =
            serde_json::from_str(&super::set_chat_muted(-1001, true)).expect("valid JSON");
        assert_eq!(req["@type"], "setChatNotificationSettings");
        assert_eq!(req["chat_id"], -1001);
        let st = &req["notification_settings"];
        assert_eq!(st["@type"], "chatNotificationSettings");
        assert_eq!(st["use_default_mute_for"], false);
        assert_eq!(st["mute_for"], 2_147_483_647_i64, "muted for as long as int32 lasts");
        assert_eq!(st["use_default_sound"], true);
        assert_eq!(st["use_default_show_preview"], true);
        assert_eq!(st["use_default_disable_mention_notifications"], true);
        let un: serde_json::Value =
            serde_json::from_str(&super::set_chat_muted(-1001, false)).expect("valid JSON");
        assert_eq!(un["notification_settings"]["mute_for"], 0);

        let req: serde_json::Value =
            serde_json::from_str(&super::toggle_chat_pinned(-1001, true)).expect("valid JSON");
        assert_eq!(req["@type"], "toggleChatIsPinned");
        assert_eq!(req["chat_list"]["@type"], "chatListMain");
        assert_eq!(req["is_pinned"], true);

        let req: serde_json::Value =
            serde_json::from_str(&super::add_chat_to_list(-1001, true)).expect("valid JSON");
        assert_eq!(req["@type"], "addChatToList");
        assert_eq!(req["chat_list"]["@type"], "chatListArchive");
        let back: serde_json::Value =
            serde_json::from_str(&super::add_chat_to_list(-1001, false)).expect("valid JSON");
        assert_eq!(back["chat_list"]["@type"], "chatListMain");

        let req: serde_json::Value =
            serde_json::from_str(&super::leave_chat(-1001)).expect("valid JSON");
        assert_eq!(req["@type"], "leaveChat");
        assert_eq!(req["chat_id"], -1001);

        // The read a list's batch sends: the newest line stands for the chat,
        // and `force_read` clears the count at the server.
        let req: serde_json::Value =
            serde_json::from_str(&super::view_messages(-1001, &[4200])).expect("valid JSON");
        assert_eq!(req["@type"], "viewMessages");
        assert_eq!(req["message_ids"], json!([4200]));
        assert_eq!(req["force_read"], true);
    }

    /// What a send that is not text spells: a file by the kind its name gives
    /// it — the content's `@type` and the field it names its `inputFileLocal`
    /// by — with the composer's words as the caption and the reply on it; a
    /// place, one-off, so every field a live location moves is nought; and a
    /// forward, which keeps the *forwarded from* rather than sending a copy.
    #[test]
    fn the_media_sends_spell_their_requests() {
        let v = |s: String| serde_json::from_str::<serde_json::Value>(&s).expect("valid JSON");
        let file = |p: &str| crate::apps::telegram::model::Carried { path: p.to_string() };

        // The first file of a send: the words ride under it as its caption,
        // and the line it answers rides with it.
        let req = v(super::send_file(-1001, Some(42), &file("~/a.png"), "here"));
        assert_eq!(req["@type"], "sendMessage");
        assert_eq!(req["chat_id"], -1001);
        let c = &req["input_message_content"];
        assert_eq!(c["@type"], "inputMessagePhoto");
        assert_eq!(c["photo"]["@type"], "inputFileLocal");
        // The files app's `~/` spelling is the disk's by the time the engine
        // reads it.
        let path = c["photo"]["path"].as_str().unwrap();
        assert!(!path.starts_with('~') && path.ends_with("/a.png"), "{path}");
        assert_eq!(c["caption"]["@type"], "formattedText");
        assert_eq!(c["caption"]["text"], "here");
        assert_eq!(req["reply_to"]["@type"], "inputMessageReplyToMessage");
        assert_eq!(req["reply_to"]["message_id"], 42);

        // The rest, one per kind: a bare file carries an empty caption rather
        // than none — TDLib takes the content whole — and answers nothing.
        for (path, kind, names_it) in [
            ("~/clip.mp4", "inputMessageVideo", "video"),
            ("~/track.m4a", "inputMessageAudio", "audio"),
            ("~/report-q3.pdf", "inputMessageDocument", "document"),
        ] {
            let req = v(super::send_file(-1001, None, &file(path), ""));
            let c = &req["input_message_content"];
            assert_eq!(c["@type"], kind, "{path}");
            assert_eq!(c[names_it]["@type"], "inputFileLocal", "{path}");
            // Spelled as the disk has it, whatever the files app showed.
            assert_eq!(
                c[names_it]["path"].as_str().unwrap(),
                kernel::caps::real_path(path).to_string_lossy().as_ref(),
                "{path}"
            );
            assert_eq!(c["caption"]["text"], "");
            assert!(req.get("reply_to").is_none(), "{path} answers nothing");
        }

        // A place: the trailhead, shared once rather than for an hour.
        let req = v(super::send_location(-1001, None, 47.0472, 8.3164));
        let c = &req["input_message_content"];
        assert_eq!(c["@type"], "inputMessageLocation");
        assert_eq!(c["location"]["@type"], "location");
        assert_eq!(c["location"]["latitude"], 47.0472);
        assert_eq!(c["location"]["longitude"], 8.3164);
        assert_eq!(c["location"]["horizontal_accuracy"], 0);
        assert_eq!(c["live_period"], 0);
        assert_eq!(c["heading"], 0);
        assert_eq!(c["proximity_alert_radius"], 0);

        // A forward: the lines by id, out of the chat holding them and into
        // the one picked, as forwards and not as fresh lines of mine.
        let req = v(super::forward_messages(7, -1001, &[1, 2, 3]));
        assert_eq!(req["@type"], "forwardMessages");
        assert_eq!(req["chat_id"], 7);
        assert_eq!(req["from_chat_id"], -1001);
        assert_eq!(req["message_ids"], json!([1, 2, 3]));
        assert_eq!(req["message_thread_id"], 0);
        assert!(req["options"].is_null(), "the account's own send options");
        assert_eq!(req["send_copy"], false);
        assert_eq!(req["remove_caption"], false);
    }

    // -- presence, typing, drafts and joining ----------------------------------

    /// A status arriving on its own lands on the peer: the word the header
    /// draws and the moment it says *last seen*. The row need not stand yet
    /// — a status may reach us before the user object does.
    #[test]
    fn a_status_of_its_own_lands_on_the_peer() {
        let td = FakeTd::new();
        let acc = account(td.clone(), None);
        let w = world();

        acc.on_update(
            &w,
            &json!({"@type": "updateUserStatus", "user_id": 2,
                    "status": {"@type": "userStatusOnline", "expires": 1_725_000_600}})
            .to_string(),
        );
        let (status, seen): (String, Option<f64>) = w
            .store()
            .conn()
            .query_row("SELECT status, last_seen FROM tg_peer WHERE id = 2", [], |r| {
                Ok((r.get(0)?, r.get(1)?))
            })
            .unwrap();
        assert_eq!((status.as_str(), seen), ("online", Some(1_725_000_600.0)));

        // The user itself, after: the name lands and the status is not lost.
        acc.on_update(&w, &user_update(2, "Vera", "Kovac", "vera"));
        acc.on_update(
            &w,
            &json!({"@type": "updateUserStatus", "user_id": 2,
                    "status": {"@type": "userStatusOffline", "was_online": 1_725_000_000}})
            .to_string(),
        );
        let (name, status): (String, String) = w
            .store()
            .conn()
            .query_row("SELECT name, status FROM tg_peer WHERE id = 2", [], |r| {
                Ok((r.get(0)?, r.get(1)?))
            })
            .unwrap();
        assert_eq!((name.as_str(), status.as_str()), ("Vera Kovac", "offline"));

        // A status with no word for it leaves the last one standing.
        acc.on_update(
            &w,
            &json!({"@type": "updateUserStatus", "user_id": 2, "status": {"@type": "userStatusEmpty"}})
                .to_string(),
        );
        assert_eq!(
            w.store()
                .conn()
                .query_row("SELECT status FROM tg_peer WHERE id = 2", [], |r| r
                    .get::<_, String>(0))
                .unwrap(),
            "offline",
            "a value once known stays known"
        );
        assert!(td.sent().is_empty(), "presence asks the wire for nothing");
    }

    /// An `updateChatAction` for a chat, from a user.
    fn action_update(chat: i64, sender: i64, kind: &str) -> String {
        json!({
            "@type": "updateChatAction",
            "chat_id": chat,
            "sender_id": {"@type": "messageSenderUser", "user_id": sender},
            "action": {"@type": kind},
        })
        .to_string()
    }

    /// Who the store says is typing in a chat.
    fn typing(w: &World, chat: i64) -> Option<String> {
        w.store()
            .conn()
            .query_row("SELECT typing FROM tg_chat WHERE peer = ?1", [chat], |r| {
                r.get::<_, Option<String>>(0)
            })
            .unwrap()
    }

    /// Typing is the sender's name as the store knows it, cleared by the
    /// cancel — and, because the server's cancel is a courtesy rather than a
    /// promise, by the pass that finds its six seconds up.
    #[test]
    fn typing_wears_a_name_and_the_pass_lets_it_go() {
        let clock = FakeClock::default();
        let w = timed_world(&clock);
        let td = FakeTd::new();
        let acc = account(td.clone(), None);
        acc.on_update(&w, &user_update(2, "Vera", "Kovac", "vera"));
        acc.on_update(&w, &private_chat_update(2, "Vera Kovac", 0));

        acc.on_update(&w, &action_update(2, 2, "chatActionTyping"));
        assert_eq!(typing(&w, 2).as_deref(), Some("Vera Kovac"));
        // Recording a voice note, sending a photo: all one word to a header.
        acc.on_update(&w, &action_update(2, 2, "chatActionRecordingVoiceNote"));
        assert_eq!(typing(&w, 2).as_deref(), Some("Vera Kovac"));

        // A pass inside the six seconds leaves it standing.
        clock.advance(5.0);
        acc.drain(&w);
        assert_eq!(typing(&w, 2).as_deref(), Some("Vera Kovac"), "still typing");
        // Past them, the pass clears it, the cancel never having come.
        clock.advance(1.5);
        acc.drain(&w);
        assert_eq!(typing(&w, 2), None, "six seconds of silence is not typing");

        // Where the cancel does come it is at once, and no later pass
        // rewrites the row.
        acc.on_update(&w, &action_update(2, 2, "chatActionTyping"));
        assert!(typing(&w, 2).is_some());
        acc.on_update(&w, &action_update(2, 2, "chatActionCancel"));
        assert_eq!(typing(&w, 2), None, "the cancel stops it");

        // A stranger typing in a group is *someone* rather than a hole.
        acc.on_update(&w, &chat_object(-1006, "stelaxis", json!([])));
        acc.on_update(&w, &action_update(-1006, 909, "chatActionTyping"));
        assert_eq!(typing(&w, -1006).as_deref(), Some("someone"));
        assert!(td.sent().is_empty(), "an action asks the wire for nothing");
    }

    /// The one-field updates a chat sends after its object: a title, the
    /// mention badge, and what a line has gathered — which is written whole,
    /// so a reaction taken back leaves the row.
    #[test]
    fn a_chats_later_updates_land_the_title_the_mention_and_the_counts() {
        let td = FakeTd::new();
        let acc = account(td.clone(), None);
        let w = world();
        acc.on_update(&w, &chat_object(-1006, "stelaxis", json!([])));

        acc.on_update(
            &w,
            &json!({"@type": "updateChatTitle", "chat_id": -1006, "title": "stelaxis · v2"})
                .to_string(),
        );
        assert_eq!(
            w.store()
                .conn()
                .query_row("SELECT name FROM tg_peer WHERE id = -1006", [], |r| r
                    .get::<_, String>(0))
                .unwrap(),
            "stelaxis · v2"
        );

        acc.on_update(
            &w,
            &json!({"@type": "updateChatUnreadMentionCount", "chat_id": -1006,
                    "unread_mention_count": 2})
            .to_string(),
        );
        assert_eq!(num(&w, "SELECT mention FROM tg_chat WHERE peer = -1006"), 1);
        acc.on_update(
            &w,
            &json!({"@type": "updateChatUnreadMentionCount", "chat_id": -1006,
                    "unread_mention_count": 0})
            .to_string(),
        );
        assert_eq!(num(&w, "SELECT mention FROM tg_chat WHERE peer = -1006"), 0);

        // What a post has gathered, and what it has lost.
        acc.on_update(&w, &new_message(4300, -1006, None, "the release is out"));
        acc.on_update(
            &w,
            &json!({"@type": "updateMessageInteractionInfo", "chat_id": -1006, "message_id": 4300,
                    "interaction_info": {
                        "view_count": 1_204,
                        "reply_info": {"reply_count": 7},
                        "reactions": [{"reaction": "👍", "total_count": 3}],
                    }})
            .to_string(),
        );
        let gathered = |w: &World| -> (Option<i64>, Option<i64>, Option<String>) {
            w.store()
                .conn()
                .query_row(
                    "SELECT views, comments, reactions FROM tg_message WHERE id = 4300",
                    [],
                    |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
                )
                .unwrap()
        };
        assert_eq!(gathered(&w), (Some(1_204), Some(7), Some("👍 3".to_string())));
        acc.on_update(
            &w,
            &json!({"@type": "updateMessageInteractionInfo", "chat_id": -1006,
                    "message_id": 4300, "interaction_info": null})
            .to_string(),
        );
        assert_eq!(gathered(&w), (None, None, None), "the last reaction, taken back");
        assert!(td.sent().is_empty());
    }

    /// A draft typed on the phone shows here, and a draft cleared there
    /// clears here: the null draft is the news, not an absence of it.
    #[test]
    fn a_draft_from_another_device_lands_and_a_null_one_clears_it() {
        let td = FakeTd::new();
        let acc = account(td.clone(), None);
        let w = world();
        acc.on_update(&w, &private_chat_update(2, "Vera Kovac", 0));
        let draft = |w: &World| -> Option<String> {
            w.store()
                .conn()
                .query_row("SELECT draft FROM tg_chat WHERE peer = 2", [], |r| {
                    r.get::<_, Option<String>>(0)
                })
                .unwrap()
        };

        acc.on_update(
            &w,
            &json!({"@type": "updateChatDraftMessage", "chat_id": 2, "draft_message": {
                "@type": "draftMessage",
                "input_message_text": {"@type": "inputMessageText", "text": {"text": "on my way"}},
            }})
            .to_string(),
        );
        assert_eq!(draft(&w).as_deref(), Some("on my way"));

        acc.on_update(
            &w,
            &json!({"@type": "updateChatDraftMessage", "chat_id": 2, "draft_message": null})
                .to_string(),
        );
        assert_eq!(draft(&w), None, "the draft was sent, or thrown away");
        assert!(td.sent().is_empty());
    }

    /// The three requests this round adds: the way into a chat, the way out
    /// of a person's, and the draft the composer leaves behind.
    #[test]
    fn joining_deleting_and_the_draft_spell_their_requests() {
        let v = |s: String| serde_json::from_str::<serde_json::Value>(&s).expect("valid JSON");

        let req = v(super::join_chat(-1006));
        assert_eq!(req["@type"], "joinChat");
        assert_eq!(req["chat_id"], -1006);

        let req = v(super::delete_chat(2));
        assert_eq!(req["@type"], "deleteChatHistory", "never deleteChat: that is everyone's");
        assert_eq!(req["chat_id"], 2);
        assert_eq!(req["remove_from_chat_list"], true);
        assert_eq!(req["revoke"], false, "the other side keeps theirs");
        let req = v(super::clear_history(2));
        assert_eq!(req["@type"], "deleteChatHistory");
        assert_eq!(req["remove_from_chat_list"], false);
        assert_eq!(req["revoke"], false);
        assert_eq!(req["chat_id"], 2);

        let req = v(super::set_chat_draft(2, Some("on my way")));
        assert_eq!(req["@type"], "setChatDraftMessage");
        assert_eq!(req["chat_id"], 2);
        assert_eq!(req["message_thread_id"], 0);
        let d = &req["draft_message"];
        assert_eq!(d["@type"], "draftMessage");
        assert!(d["reply_to"].is_null(), "a draft answers nothing");
        assert_eq!(d["date"], 0);
        assert_eq!(d["input_message_text"]["@type"], "inputMessageText");
        assert_eq!(d["input_message_text"]["text"]["text"], "on my way");

        // Nothing typed is no draft at all, which is how the type language
        // spells *cleared*.
        for empty in [None, Some(""), Some("   ")] {
            let req = v(super::set_chat_draft(2, empty));
            assert!(req["draft_message"].is_null(), "{empty:?} is no draft");
        }
    }

    /// A file that is not the engine's own — the account holder's picture,
    /// which TDLib names as the local copy of the photo it is sending — is
    /// copied into the cache and left exactly where it was. Moving it would
    /// take the picture out of the folder it was chosen from, and there is
    /// no engine copy to tell anyone to forget.
    #[test]
    fn a_file_of_my_own_is_copied_in_and_left_where_it_is() {
        let td = FakeTd::new();
        let acc = account(td.clone(), None);
        let w = world();
        let mine = std::env::temp_dir().join(format!(
            "superapp-tg-my-own-{}.png",
            std::process::id()
        ));
        std::fs::write(&mine, b"my own picture").unwrap();

        let announce = file_update(&mine.to_string_lossy(), "sent_uniq_1");
        acc.on_update(&w, &announce);
        // An upload is announced more than once; the repeat reads nothing.
        acc.on_update(&w, &announce);

        assert!(mine.exists(), "a file of mine is never moved");
        let cached = w
            .with_cap::<dyn Blobs, _>(|b| b.contains("tg:sent_uniq_1"))
            .expect("the world has a blob cache");
        assert!(cached, "its bytes went in under the same tg: key");
        assert!(td.sent().is_empty(), "no engine copy to forget");
        let held = w
            .with_cap::<dyn Blobs, _>(|b| b.get("tg:sent_uniq_1"))
            .unwrap()
            .expect("the cached file");
        assert_eq!(std::fs::read(held).unwrap(), b"my own picture");
        std::fs::remove_file(&mine).unwrap();
    }

}
