//! The account worker: authorization, update projection, history and downloads.
//!
//! Panels enqueue commands in the store's runtime. Only this worker uses the
//! transport; `FakeTd` drives the same loop offline. Projection writes go
//! through the store's single writer. Wire JSON is built in `requests` and
//! decoded in `updates`.
#![cfg_attr(not(feature = "tdlib"), allow(dead_code))]

#[cfg(feature = "tdlib")]
use std::path::Path;
use std::path::PathBuf;
use std::time::Duration;

#[cfg(any(feature = "tdlib", test))]
use kernel::app::{Wake, Worker};
use kernel::caps::{Blobs, Secrets};
#[cfg(any(feature = "tdlib", test))]
use kernel::effect::Job;
use kernel::effect::World;
use rusqlite::Connection;
use serde_json::Value;

use super::model::{self, Media, MsgId, PeerId};
use super::project::{
    apply_read_outbox, history_window, project_chats, project_members, project_messages,
    project_peers, trim_chat, IncomingMessage, HISTORY_KEEP,
};
use super::requests::*;
use super::runtime;
use super::schema;
#[cfg(feature = "tdlib")]
use super::transport::RealTd;
use super::transport::Td;
use super::updates;

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
    commands: std::cell::RefCell<Option<runtime::Inbox>>,
    /// The application id — a small positive int Telegram assigns, not a
    /// secret, from the `telegram` file.
    api_id: i32,
    /// TDLib's own binlog directory: its auth keys and update cursors. Local
    /// and un-synced, beside the store and never in it — a session is a
    /// secret and the store replicates.
    tdlib_dir: PathBuf,
    /// The configured phone. If absent, authorization waits for a command
    /// from the sign-in panel.
    phone: Option<String>,
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
    in_flight: std::cell::Cell<Option<(Page, f64)>>,
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

impl<T: Td> Account<T> {
    /// The single outbound boundary for commands and background requests.
    fn send(&self, w: &World, request: &str) {
        let rt = runtime::of(w.store());
        let request = rt.operations.track(request);
        let Ok(v) = serde_json::from_str::<Value>(&request) else {
            rt.operations
                .report(w.store(), "sending request", "Invalid request JSON");
            return;
        };
        if let Err(error) = super::operations::validate_files(&v) {
            if let Some(id) = v["@extra"]["operation"].as_u64() {
                rt.operations.fail(w.store(), id, &error, false);
            }
            return;
        }
        self.log(&format!(
            ">> {} request={} chat={}",
            v["@type"], v["@extra"]["operation"], v["chat_id"]
        ));
        self.td.send(&request);
    }

    fn acknowledged(&self, w: &World, request: &Value) {
        // Profile actions have their own attempt guard in on_reply. In
        // particular, a stale delete-chat reply must not clear any rows here.
        if request["@extra"]["context"]
            .as_str()
            .is_some_and(|context| context.starts_with("peer_action:"))
        {
            return;
        }
        let Some(chat) = request["chat_id"].as_i64() else {
            return;
        };
        let request = request.clone();
        self.filed(
            w,
            "applying confirmed change",
            w.store().write(move |c| match request["@type"].as_str() {
                Some("setChatNotificationSettings") => model::set_muted_tx(
                    c,
                    chat,
                    request["notification_settings"]["mute_for"]
                        .as_i64()
                        .unwrap_or(0)
                        > 0,
                ),
                Some("toggleChatIsPinned") => {
                    model::set_pinned_tx(c, chat, request["is_pinned"] == true)
                }
                Some("addChatToList") => model::set_archived_tx(
                    c,
                    chat,
                    request["chat_list"]["@type"] == "chatListArchive",
                ),
                Some("leaveChat") => model::leave_chat_tx(c, chat),
                Some("deleteChatHistory") => {
                    model::clear_history_tx(c, chat)?;
                    if request["remove_from_chat_list"] == true {
                        model::leave_chat_tx(c, chat)?;
                    }
                    Ok(())
                }
                _ => Ok(()),
            }),
        );
    }

    #[must_use]
    pub fn new(td: T, api_id: i32, tdlib_dir: PathBuf, phone: Option<String>) -> Account<T> {
        Account {
            td,
            commands: std::cell::RefCell::new(None),
            api_id,
            tdlib_dir,
            phone,
            typing: std::cell::RefCell::new(std::collections::HashMap::new()),
            pages: std::cell::RefCell::new(std::collections::VecDeque::new()),
            in_flight: std::cell::Cell::new(None),
            not_before: std::cell::Cell::new(0.0),
        }
    }

    /// Queues the first page of a chat's fill, at the front: the chat just
    /// opened is the one the account holder is looking at.
    pub fn want(&self, w: &World, chat: PeerId) {
        runtime::of(w.store()).set_loading(chat, true);
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
        if super::Telegram::engine_store(w.store().dir())
            && schema::session(w.store().conn()).state != "ready"
        {
            return;
        }
        // Files a drawing found missing go first, and single lines asked for
        // with them: few, cheap, and somebody is looking at each.
        let wanted = runtime::of(w.store()).take_wanted();
        for rid in wanted.files {
            self.send(w, &request_file(&rid));
        }
        for (chat, id) in wanted.lines {
            self.send(w, &get_message(chat, id));
        }
        for chat in wanted.chats {
            self.want(w, chat);
        }
        let now = w.now();
        if let Some((page, sent)) = self.in_flight.get() {
            if now - sent < PAGE_PATIENCE {
                return;
            }
            self.in_flight.set(None);
            runtime::of(w.store()).set_loading(page.chat, false);
            runtime::of(w.store()).operations.fail_context(
                w.store(),
                &format!("history:{}:{}:{}", page.chat, page.walk.word(), page.from),
                "Telegram did not return the history page. Try again.",
            );
        }
        if now < self.not_before.get() {
            return;
        }
        let Some(page) = self.pages.borrow_mut().pop_front() else {
            return;
        };
        self.in_flight.set(Some((page, now)));
        self.not_before.set(now + PAGE_GAP);
        self.send(w, &get_chat_history(page.chat, page.from, page.walk));
    }

    /// Files a write's outcome. A refused write goes to the trace and, once
    /// per distinct error, to stderr: a store whose table lacks a column
    /// refuses every line the same way, and two thousand copies of one
    /// sentence bury it. Silence here cost a night — every message upsert
    /// failing on a first-shape store while the sign-in panel said the chats
    /// were syncing (2026-09-07) — so nothing the worker writes is dropped
    /// unheard again.
    fn filed<R, E: std::fmt::Display>(&self, w: &World, what: &str, r: Result<R, E>) {
        if let Err(e) = r {
            runtime::of(w.store())
                .operations
                .report(w.store(), what, &e.to_string());
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
        // The receiver belongs to this account. Its lifetime is the send
        // permission: a stopped worker cannot leave a live sender behind.
        let mut commands = self.commands.borrow_mut();
        let inbox = commands.get_or_insert_with(|| runtime::of(w.store()).connect());
        for request in inbox.try_iter() {
            self.send(w, &request);
        }
        drop(commands);
        runtime::of(w.store())
            .operations
            .expire(w.store(), std::time::Instant::now());
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
    /// which projects content. An unparseable line is dropped, not fatal — the
    /// wire's framing is TDLib's to keep, not ours.
    pub fn on_update(&self, w: &World, raw: &str) {
        let Ok(mut v) = serde_json::from_str::<Value>(raw) else {
            runtime::of(w.store()).operations.report(
                w.store(),
                "reading Telegram response",
                "Invalid JSON from TDLib",
            );
            return;
        };
        let rt = runtime::of(w.store());
        let tracked = v["@extra"]["operation"].is_u64();
        if let Some(request) = rt.operations.reply(w.store(), &v) {
            self.acknowledged(w, &request);
        }
        if tracked {
            v["@extra"] = v["@extra"]["context"].clone();
        }
        match v["@type"].as_str() {
            Some("updateConnectionState") => {
                rt.set_connection(v["state"]["@type"].as_str().unwrap_or(""));
                // The session poll needs a redraw even without a projection write.
                rt.operations.changed();
            }
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
                let end = v["code"] == 404
                    && v["@extra"]
                        .as_str()
                        .is_some_and(|x| x.starts_with("load_chats:"));
                if !tracked && !end {
                    rt.operations.report(
                        w.store(),
                        "Telegram request",
                        &super::operations::error_text(&v),
                    );
                }
                self.on_reply(w, &v, true);
            }
            Some("ok") => {
                self.log("<< ok");
                self.on_reply(w, &v, false);
            }
            Some("file") => {
                self.log("<< file");
                self.on_file_answer(w, &v);
                self.on_file(w, &v);
            }
            // A single line fetched afresh ([`runtime::Runtime::want_line`]) answers as the
            // message itself, which lands the way a new line does.
            Some("message") => {
                self.log("<< message");
                self.on_new_message(w, &v);
            }
            Some("messages") if tracked && v["@extra"].is_null() => {
                if let Some(messages) = v["messages"].as_array() {
                    for message in messages {
                        self.on_new_message(w, message);
                    }
                }
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
        super::trace::note(self.tdlib_dir.parent(), line);
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
            self.filed(
                w,
                "on_auth",
                w.store().write(move |c| {
                    schema::set_session(c, phone.as_deref(), state, detail.as_deref(), now)
                }),
            );
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
                self.send(
                    w,
                    &set_tdlib_parameters(self.api_id, &api_hash, &self.tdlib_dir),
                );
                write(None, "connecting", None);
            }
            // The phone. If the file configured one, send it and move on; if
            // not, wait for the UI to supply it through `set_phone`.
            Some("authorizationStateWaitPhoneNumber") => {
                if let Some(phone) = self.phone.clone() {
                    self.log(">> setAuthenticationPhoneNumber");
                    self.send(w, &set_authentication_phone(&phone));
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
            // Signed in. Start loading the dialog list,
            // begun by `on_ready`.
            Some("authorizationStateReady") => {
                write(None, "ready", None);
                self.on_ready(w);
            }
            Some("authorizationStateLoggingOut") => {
                runtime::of(w.store()).disconnect();
                write(None, "logging_out", None);
            }
            Some("authorizationStateClosing" | "authorizationStateClosed") => {
                runtime::of(w.store()).disconnect();
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
    #[cfg(test)]
    pub fn set_phone(&mut self, phone: &str) {
        self.phone = Some(phone.to_string());
        self.td.send(&set_authentication_phone(phone));
    }

    /// Sends the login code the user entered. TDLib answers with the next
    /// auth state — 'ready', or 'wait_password' for a two-factor account —
    /// which drives the row; the code is never auto-sent from an update.
    #[cfg(test)]
    pub fn check_code(&self, code: &str) {
        self.td.send(&check_authentication_code(code));
    }

    /// Sends the two-factor password the user entered.
    #[cfg(test)]
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
    /// The transcripts fill as panels request history through the runtime.
    ///
    /// Keep unconfirmed messages from a previous run visible. The projection
    /// cannot establish whether Telegram delivered them while we were away.
    pub fn on_ready(&self, w: &World) {
        match w.store().write(|c| c.execute("UPDATE tg_message SET state = 'failed' WHERE state = 'sending'", [])) {
            Ok(n) if n > 0 => runtime::of(w.store()).operations.report(w.store(), "previous sends",
                "Delivery was not confirmed before reconnecting. Check the chats before sending again."),
            Ok(_) => {},
            Err(error) => runtime::of(w.store()).operations.report(w.store(), "recovering sends", &error.to_string()),
        }
        runtime::of(w.store()).set_list_syncing(true);
        self.send(w, &load_chats(ChatList::Main));
    }

    /// A plain reply — `ok` or an error — to one of this account's own
    /// requests, told apart by the `@extra` it wore: the chat-list load, a
    /// history page, and a send.
    ///
    /// The list load: `ok` says a page landed and there may be another, an
    /// error (TDLib's 404) that the list is complete — after the main list
    /// the archive is loaded the same way, and after the archive nothing.
    fn on_reply(&self, w: &World, v: &Value, failed: bool) {
        if let Some((action, peer, id)) = PeerAction::from_reply(v) {
            let runtime = runtime::of(w.store());
            if !runtime.finish_peer_action(peer, action, id) {
                return;
            }
            let name = model::peer(w.store(), peer).map_or_else(|| peer.to_string(), |p| p.name);
            let result = if failed {
                Err(v["message"].as_str().unwrap_or("request failed").to_string())
            } else {
                w.store().write(move |c| match action {
                    PeerAction::Block | PeerAction::Unblock => {
                        ensure_peer(c, peer)?;
                        model::set_blocked_tx(c, peer, action == PeerAction::Block)
                    }
                    PeerAction::DeleteContact => model::delete_contact_tx(c, peer),
                    PeerAction::DeleteChat => model::leave_chat_tx(c, peer),
                }).map_err(|e| e.to_string())
            };
            match result {
                Ok(()) => runtime.notice(format!("{} · {name}: done", action.word()), false),
                Err(error) => runtime.notice(format!("{} · {name}: {error}", action.word()), true),
            }
            return;
        }
        if failed
            && v["code"] != 404
            && v["@extra"]
                .as_str()
                .is_some_and(|e| e.starts_with("load_chats:"))
        {
            runtime::of(w.store()).set_list_syncing(false);
            return;
        }
        match (v["@extra"].as_str(), failed) {
            (Some("load_chats:main"), false) => self.send(w, &load_chats(ChatList::Main)),
            (Some("load_chats:main"), true) | (Some("load_chats:archive"), false) => {
                self.send(w, &load_chats(ChatList::Archive));
            }
            (Some("load_chats:archive"), true) => runtime::of(w.store()).set_list_syncing(false),
            // A history page refused. Telegram's *too many requests* says
            // how long to hold off: the page goes back to the front and the
            // whole queue waits that long. Anything else — a chat gone
            // private, not found — ends that chat's walk.
            (Some(extra), true) if extra.starts_with("history:") => {
                let Some((chat, walk, from)) = parse_history_extra(extra) else {
                    return;
                };
                let page = Page { chat, walk, from };
                if self
                    .in_flight
                    .get()
                    .is_some_and(|(current, _)| current != page)
                {
                    return;
                }
                self.in_flight.set(None);
                let wait = (v["code"].as_i64() == Some(429))
                    .then(|| retry_after(v["message"].as_str().unwrap_or("")))
                    .flatten();
                match wait {
                    Some(secs) => {
                        self.not_before.set(w.now() + secs + 1.0);
                        self.pages.borrow_mut().push_front(Page { chat, from, walk });
                    }
                    None => runtime::of(w.store()).set_loading(chat, false),
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
                    w,
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
            w,
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
    /// Accept the older viewer's `clip:` correlation prefix as well as the
    /// current `file:` prefix.
    fn on_file_answer(&self, w: &World, v: &Value) {
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
            self.send(w, &download_file(id, 32));
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
            Some("updateChatBlockList") => {
                if let Some(peer) = update["chat_id"].as_i64() {
                    self.on_blocked(w, peer, updates::blocked(update));
                }
            }
            Some("updateUserFullInfo") if update["user_full_info"].is_object() => {
                if let Some(peer) = update["user_id"].as_i64() {
                    self.on_blocked(w, peer, updates::blocked(&update["user_full_info"]));
                }
            }
            Some("userFullInfo") => {
                if let Some(peer) = update["@extra"].as_str()
                    .and_then(|s| s.strip_prefix("user_full_info:"))
                    .and_then(|s| s.parse().ok())
                {
                    self.on_blocked(w, peer, updates::blocked(update));
                }
            }
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
        self.filed(
            w,
            "on_new_message",
            w.store().write(move |c| {
                ensure_peer(c, chat)?;
                model::ensure_chat_tx(c, chat)?;
                if let Some(s) = sender {
                    ensure_peer(c, s)?;
                }
                project_messages(c, &[msg])?;
                apply_read_outbox(c, chat)?;
                trim_chat(c, chat)?;
                Ok(())
            }),
        );
        // Ask for the media, if any: TDLib downloads it, `updateFile`
        // completes, and [`on_file`](Account::on_file) ingests the bytes into
        // the blob cache under the same tg: key the row names — the next
        // redraw draws the photo.
        if message["sending_state"].is_null() {
            self.fetch(w, &message["content"]);
        }
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
            self.send(w, &download_file(file_id, 1));
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
        let entities = updates::content_entities(&u["new_content"]);
        self.filed(
            w,
            "on_message_content",
            w.store()
                .write(move |c| set_content(c, chat, id, &text, media.as_ref(), &entities)),
        );
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
        self.filed(
            w,
            "on_message_edited",
            w.store().write(move |c| {
                c.execute(
                    "UPDATE tg_message SET edited = ?3 WHERE chat = ?1 AND id = ?2",
                    rusqlite::params![chat, id, edited],
                )
                .map(|_| ())
            }),
        );
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
            w,
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
        self.filed(
            w,
            "on_delete_messages",
            w.store()
                .write(move |c| model::delete_lines_tx(c, chat, &ids).map(|_| ())),
        );
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
        let blocked = updates::blocked(chat);
        let read_outbox = chat["last_read_outbox_message_id"]
            .as_i64()
            .filter(|&m| m != 0);
        self.filed(
            w,
            "on_new_chat",
            w.store().write(move |c| {
                if let Some(p) = derived {
                    project_peers(c, &[p])?;
                }
                ensure_peer(c, peer)?;
                model::set_blocked_tx(c, peer, blocked)?;
                project_chats(c, &[ch])?;
                if read_outbox.is_some() {
                    c.execute(
                        "UPDATE tg_chat SET read_outbox = ?2 WHERE peer = ?1",
                        rusqlite::params![peer, read_outbox],
                    )?;
                    apply_read_outbox(c, peer)?;
                }
                Ok(())
            }),
        );
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
            w,
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
        runtime::of(w.store()).operations.sent(w.store(), u);
        let Some(msg) = updates::message(&u["message"]) else {
            return;
        };
        let old = u["old_message_id"].as_i64().unwrap_or(0);
        let (chat, sender) = (msg.chat, msg.sender);
        self.filed(
            w,
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
            w,
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
            w,
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
        self.filed(w, "on_chat_read_inbox", w.store().write(move |c| {
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
        self.filed(
            w,
            "on_chat_notifications",
            w.store().write(move |c| {
                c.execute(
                    "UPDATE tg_chat SET muted = ?2 WHERE peer = ?1",
                    rusqlite::params![chat, muted],
                )
                .map(|_| ())
            }),
        );
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
            w,
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
            w,
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
            w,
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
            w,
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
            w,
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
        self.filed(
            w,
            "on_user",
            w.store().write(move |c| project_peers(c, &[p])),
        );
    }

    fn on_blocked(&self, w: &World, peer: PeerId, blocked: bool) {
        self.filed(w, "on_blocked", w.store().write(move |c| {
            ensure_peer(c, peer)?;
            model::set_blocked_tx(c, peer, blocked)
        }));
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
            w,
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
        self.filed(
            w,
            "on_counts",
            w.store().write(move |conn| {
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
        self.filed(
            w,
            "on_basic_group_full",
            w.store().write(move |c| {
                for m in &members {
                    ensure_peer(c, m.peer)?;
                }
                project_members(c, &members)
            }),
        );
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
        let stale = self
            .in_flight
            .get()
            .is_some_and(|(page, _)| page != Page { chat, walk, from });
        if !stale {
            self.in_flight.set(None);
        }
        let raw = v["messages"].as_array().cloned().unwrap_or_default();
        let batch: Vec<IncomingMessage> = raw.iter().filter_map(updates::message).collect();
        let Some(oldest) = batch.iter().map(|m| m.id).min() else {
            if !stale {
                runtime::of(w.store()).set_loading(chat, false);
            }
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
            w,
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
        if stale {
            return;
        }
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
        runtime::of(w.store()).set_loading(chat, next.is_some());
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
        runtime::of(w.store()).operations.file_progress(file);
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
        let key = format!("tg:{uid}");
        let rt = runtime::of(w.store());
        if w.with_cap::<dyn Blobs, _>(|b| b.contains(&key))
            .unwrap_or(false)
        {
            rt.operations.file_finished(w.store(), file, None);
            return;
        }
        if !src.exists() {
            let error = "Downloaded file is missing from disk";
            rt.operations.file_finished(w.store(), file, Some(error));
            rt.operations.report(w.store(), "caching media", error);
            return;
        }
        // Upload sources belong to the user. Never move or delete them, even
        // when the file update arrives while the upload is still in progress.
        let result = if !src.starts_with(&self.tdlib_dir) {
            std::fs::read(&src)
                .map_err(|e| e.to_string())
                .and_then(|bytes| {
                    w.with_cap::<dyn Blobs, _>(|b| b.put(&key, &bytes))
                        .and_then(|r| r)
                        .map(|_| ())
                })
        } else {
            w.with_cap::<dyn Blobs, _>(|b| b.ingest(&key, &src))
                .and_then(|r| r)
                .map(|_| ())
        };
        match result {
            Ok(()) => {
                rt.operations.file_finished(w.store(), file, None);
                if src.starts_with(&self.tdlib_dir) {
                    if let Some(id) = file["id"].as_i64().and_then(|n| i32::try_from(n).ok()) {
                        self.send(w, &delete_file(id));
                    }
                }
            }
            Err(error) => {
                rt.operations.file_finished(w.store(), file, Some(&error));
                rt.operations.report(w.store(), "caching media", &error);
            }
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
    entities: &[super::text::Entity],
) -> rusqlite::Result<()> {
    c.execute(
        "UPDATE tg_message SET
           text = ?3, media = ?4, media_label = ?5, media_ref = ?6, media_rid = ?7,
           media_w = ?8, media_h = ?9, media_secs = ?10,
           media_lat = ?11, media_lon = ?12, media_until = ?13,
           media_clip = ?14, media_clip_rid = ?15, entities = ?16, entities_known = 1
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
            serde_json::to_string(entities).expect("text entities serialize"),
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

/// Delivery method and code length shown by the sign-in panel.
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

/// Worker adapter used by the offline protocol tests.
#[cfg(test)]
pub struct TgWorker<T: Td> {
    account: Account<T>,
}

#[cfg(test)]
impl<T: Td> TgWorker<T> {
    /// Runs a fake account through the kernel worker interface.
    #[must_use]
    pub fn new(account: Account<T>) -> TgWorker<T> {
        TgWorker { account }
    }
}

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
    tdlib_dir: PathBuf,
    account: Option<Account<RealTd>>,
}

#[cfg(feature = "tdlib")]
impl RealWorker {
    #[must_use]
    pub fn new(db_dir: Option<&Path>) -> RealWorker {
        RealWorker {
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
        // Reconciliation creates disposable worker descriptions. Open the
        // config and native client only in the retained worker's first pass.
        let account = self.account.get_or_insert_with(|| {
            Account::new(
                RealTd::new(),
                super::config::api_id(w.store().dir()).unwrap_or(0),
                self.tdlib_dir.clone(),
                super::config::phone(w.store().dir()),
            )
        });
        account.drain(w);
        Wake::After(POLL)
    }
}

#[cfg(test)]
impl<T: Td + Send + 'static> Worker for TgWorker<T> {
    fn name(&self) -> String {
        ACCOUNT_ENTITY.to_string()
    }

    fn entity(&self) -> Option<String> {
        Some(ACCOUNT_ENTITY.to_string())
    }

    /// This account's own jobs run on the thread holding its transport.
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
mod tests;
