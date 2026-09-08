use super::runtime;
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

mod topics_tests;
mod reaction_state_tests;
mod startup_tests;
mod navigation_tests;
mod downloads_tests;

/// Protocol tests below start with an already settled viewport. Navigation
/// tests register at the current clock and exercise the delay itself.
fn watch_messages(w: &World, chat: i64, ids: Vec<i64>) -> runtime::MessageView {
    runtime::of(w.store()).watch_messages(chat, None, ids, w.now() - runtime::VIEW_SETTLE)
}

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
/// a download test plants — no client runs). Most protocol tests start
/// authorized; startup tests construct an Account directly or send auth updates.
fn account(td: FakeTd, phone: Option<&str>) -> Account<FakeTd> {
    let account = Account::new(td, 17844, tdlib_dir(), phone.map(str::to_string));
    // Content fixtures begin connected. Startup tests construct a fresh
    // account explicitly or drive its authorization updates.
    account.auth_ready.set(true);
    account
}

fn last_request(td: &FakeTd, kind: &str) -> serde_json::Value {
    td.sent().iter().rev().map(|raw| serde_json::from_str::<serde_json::Value>(raw).unwrap())
        .find(|v| v["@type"] == kind).expect("request was sent")
}

#[test]
fn watching_messages_enables_tdlibs_reaction_polling_until_the_last_view_closes() {
    let w = world();
    let td = FakeTd::new();
    let acc = account(td.clone(), None);
    let view = watch_messages(&w, 7, vec![42]);
    runtime::of(w.store()).set_list_syncing(true);
    acc.drain(&w);
    let online = last_request(&td, "setOption");
    assert_eq!(online["name"], "online");
    assert_eq!(online["value"], json!({"@type": "optionValueBoolean", "value": true}));
    assert!(td.sent_types().contains(&"getMessages".to_string()), "visible rows must not wait for the entire chat list");
    drop(view);
    acc.drain(&w);
    assert_eq!(last_request(&td, "setOption")["value"]["value"], false);
    assert_eq!(td.sent_types().iter().filter(|s| *s == "closeChat").count(), 1);
}

#[test]
fn failed_visible_fetches_retry_without_scrolling_and_retire_timed_out_answers() {
    let clock = FakeClock::default();
    let w = timed_world(&clock);
    let td = FakeTd::new();
    let acc = account(td.clone(), None);
    let _view = watch_messages(&w, 7, vec![42]);
    acc.drain(&w);
    let first = last_request(&td, "getMessages");
    acc.on_update(&w, &json!({"@type": "error", "@extra": first["@extra"],
        "code": 429, "message": "Too Many Requests: retry after 5"}).to_string());
    acc.drain(&w);
    assert_eq!(last_request(&td, "getMessages"), first, "honor the server's wait");
    clock.advance(6.0);
    acc.drain(&w);
    let second = last_request(&td, "getMessages");
    assert_ne!(second["@extra"], first["@extra"], "retry a failed visible fetch");
    clock.advance(31.0);
    acc.drain(&w);
    let third = last_request(&td, "getMessages");
    assert_ne!(third["@extra"], second["@extra"], "retry a missing reply too");
    let response = |request: &serde_json::Value, count| json!({
        "@type": "messages", "@extra": request["@extra"], "messages": [{
            "chat_id": 7, "id": 42, "content": {"@type": "messageText", "text": {"text": "post"}},
            "interaction_info": {"reactions": {"reactions": [
                {"type": {"@type": "reactionTypeEmoji", "emoji": "👍"}, "total_count": count},
            ]}},
        }],
    }).to_string();
    acc.on_update(&w, &response(&third, 9));
    acc.on_update(&w, &response(&second, 1));
    assert_eq!(super::model::line(w.store(), 7, 42).unwrap().reactions.as_deref(), Some("👍 9"));
    assert_eq!(td.sent_types().iter().filter(|s| *s == "openChat").count(), 1, "retrying must not leak chat subscriptions");
}

#[test]
fn reaction_metadata_refreshes_visible_rows_and_snapshots_do_not_undo_live_counts() {
    let clock = FakeClock::default();
    let w = timed_world(&clock);
    let td = FakeTd::new();
    let acc = account(td.clone(), None);
    let _view = watch_messages(&w, 7, vec![42]);
    acc.drain(&w);
    let first = last_request(&td, "getMessages");
    let message = json!({"chat_id": 7, "id": 42,
        "content": {"@type": "messageText", "text": {"text": "post"}}});
    acc.on_update(&w, &json!({"@type": "messages", "@extra": first["@extra"], "messages": [message]}).to_string());
    acc.on_update(&w, &json!({"@type": "updateChatAvailableReactions", "chat_id": 7,
        "available_reactions": {"@type": "chatAvailableReactionsAll"}}).to_string());
    acc.drain(&w);
    let refreshed = last_request(&td, "getMessages");
    assert_ne!(refreshed["@extra"], first["@extra"]);
    acc.on_update(&w, &json!({"@type": "updateMessageInteractionInfo", "chat_id": 7, "message_id": 42,
        "interaction_info": {"reactions": {"reactions": [
            {"type": {"@type": "reactionTypeEmoji", "emoji": "👍"}, "total_count": 8},
        ]}}}).to_string());
    acc.on_update(&w, &json!({"@type": "messages", "@extra": refreshed["@extra"], "messages": [message]}).to_string());
    acc.on_update(&w, &json!({"@type": "updateChatLastMessage", "chat_id": 7, "last_message": message}).to_string());
    assert_eq!(super::model::line(w.store(), 7, 42).unwrap().reactions.as_deref(), Some("👍 8"));
    // Post-add reconciliation advances the counts from the server even
    // when the corresponding interaction update is delayed.
    let (id, _reply) = runtime::of(w.store()).await_reaction();
    acc.send(&w, &super::add_message_reaction(7, 42, "👍", id));
    let added = last_request(&td, "addMessageReaction");
    acc.on_update(&w, &json!({"@type": "ok", "@extra": added["@extra"]}).to_string());
    let fetch = last_request(&td, "getMessage");
    let mut newer = message.clone();
    newer["@type"] = json!("message");
    newer["@extra"] = fetch["@extra"].clone();
    newer["interaction_info"] = json!({"reactions": {"reactions": [
        {"type": {"@type": "reactionTypeEmoji", "emoji": "👍"}, "total_count": 9},
    ]}});
    acc.on_update(&w, &newer.to_string());
    assert_eq!(super::model::line(w.store(), 7, 42).unwrap().reactions.as_deref(), Some("👍 8"),
        "a cached getMessage must not replace the reaction owner");
    clock.advance(2.0);
    acc.drain(&w);
    let search = last_request(&td, "searchChatMessages");
    acc.on_update(&w, &json!({"@type": "foundChatMessages", "@extra": search["@extra"],
        "messages": [newer]}).to_string());
    assert_eq!(super::model::line(w.store(), 7, 42).unwrap().reactions.as_deref(), Some("👍 9"));
    acc.on_update(&w, &json!({"@type": "updateChatLastMessage", "chat_id": 7, "last_message": message}).to_string());
    assert_eq!(super::model::line(w.store(), 7, 42).unwrap().reactions.as_deref(), Some("👍 9"));
    acc.on_update(&w, &json!({"@type": "updateMessageInteractionInfo", "chat_id": 7, "message_id": 42,
        "interaction_info": null}).to_string());
    acc.on_update(&w, &newer.to_string());
    assert_eq!(super::model::line(w.store(), 7, 42).unwrap().reactions.as_deref(), Some("👍 9"));
    clock.advance(2.0);
    reaction_state_tests::confirm_empty(&acc, &td, &w, message);
    assert_eq!(super::model::line(w.store(), 7, 42).unwrap().reactions, None, "removing the last reaction must still clear it");
}

#[test]
fn missing_visible_messages_retry_and_reopening_reconciles_counts_removed_while_away() {
    let clock = FakeClock::default();
    let w = timed_world(&clock);
    let td = FakeTd::new();
    let acc = account(td.clone(), None);
    let view = watch_messages(&w, 7, vec![42]);
    acc.drain(&w);
    let first = last_request(&td, "getMessages");
    acc.on_update(&w, &json!({"@type": "messages", "@extra": first["@extra"], "messages": [null]}).to_string());
    clock.advance(3.0);
    acc.drain(&w);
    let second = last_request(&td, "getMessages");
    assert_ne!(first["@extra"], second["@extra"]);
    let message = json!({"chat_id": 7, "id": 42, "content": {"@type": "messageText", "text": {"text": "post"}}});
    acc.on_update(&w, &json!({"@type": "messages", "@extra": second["@extra"], "messages": [message]}).to_string());
    acc.on_update(&w, &json!({"@type": "updateMessageInteractionInfo", "chat_id": 7, "message_id": 42,
        "interaction_info": {"reactions": {"reactions": [
            {"type": {"@type": "reactionTypeEmoji", "emoji": "👍"}, "total_count": 8},
        ]}}}).to_string());
    acc.send(&w, &super::get_message(7, 42));
    let old_fetch = last_request(&td, "getMessage");
    drop(view);
    acc.drain(&w);
    let _reopened = watch_messages(&w, 7, vec![42]);
    acc.drain(&w);
    let reopened = last_request(&td, "getMessages");
    assert_ne!(second["@extra"], reopened["@extra"]);
    acc.on_update(&w, &json!({"@type": "messages", "@extra": reopened["@extra"], "messages": [message]}).to_string());
    assert_eq!(super::model::line(w.store(), 7, 42).unwrap().reactions.as_deref(), Some("👍 8"));
    reaction_state_tests::confirm_empty(&acc, &td, &w, message.clone());
    assert_eq!(super::model::line(w.store(), 7, 42).unwrap().reactions, None);
    let mut stale = message;
    stale["@type"] = json!("message");
    stale["@extra"] = old_fetch["@extra"].clone();
    stale["interaction_info"] = json!({"reactions": {"reactions": [
        {"type": {"@type": "reactionTypeEmoji", "emoji": "👍"}, "total_count": 8},
    ]}});
    acc.on_update(&w, &stale.to_string());
    assert_eq!(super::model::line(w.store(), 7, 42).unwrap().reactions, None,
        "a request from the previous visit cannot resurrect removed reactions");
}

#[test]
fn visible_messages_refresh_counts_and_share_the_chat_subscription() {
    let w = world();
    let td = FakeTd::new();
    let acc = account(td.clone(), None);
    let chat = watch_messages(&w, -1005, vec![4200, 4300]);
    let card = watch_messages(&w, -1005, vec![4300]);
    acc.drain(&w);
    assert_eq!(td.sent_types(), vec!["setOption", "openChat", "getMessages"]);
    let request = last_request(&td, "getMessages");
    assert_eq!(request["message_ids"], json!([4200, 4300]));
    acc.on_update(&w, &json!({
        "@type": "messages", "@extra": request["@extra"], "messages": [null, {
            "@type": "message", "chat_id": -1005, "id": 4300, "date": 10,
            "content": {"@type": "messageText", "text": {"text": "a post"}},
            "interaction_info": {"view_count": 51, "reply_info": {"reply_count": 8},
                "reactions": {"reactions": [
                    {"type": {"@type": "reactionTypeEmoji", "emoji": "👍"}, "total_count": 7},
                    {"type": {"@type": "reactionTypeEmoji", "emoji": "🔥"}, "total_count": 4},
                ]}},
        }],
    }).to_string());
    let viewed: serde_json::Value = serde_json::from_str(td.sent().last().unwrap()).unwrap();
    assert_eq!(viewed["@type"], "viewMessages");
    assert_eq!(viewed["message_ids"], json!([4300]));
    assert_eq!(viewed["source"]["@type"], "messageSourceOther");
    assert_eq!(viewed["force_read"], false);
    let m = super::model::line(w.store(), -1005, 4300).unwrap();
    assert_eq!((m.views, m.comments, m.reactions.as_deref()), (Some(51), Some(8), Some("👍 7 · 🔥 4")));

    acc.on_update(&w, &json!({
        "@type": "updateMessageInteractionInfo", "chat_id": -1005, "message_id": 4300,
        "interaction_info": {"reactions": {"reactions": [
            {"type": {"@type": "reactionTypeEmoji", "emoji": "👍"}, "total_count": 9},
        ]}},
    }).to_string());
    assert_eq!(super::model::line(w.store(), -1005, 4300).unwrap().reactions.as_deref(), Some("👍 9"));
    let n = td.sent().len();
    acc.drain(&w);
    assert_eq!(td.sent().len(), n, "a quiet viewport does not refetch every pass");
    chat.lock().unwrap().ids = vec![4300, 4400];
    acc.drain(&w);
    let request: serde_json::Value = serde_json::from_str(td.sent().last().unwrap()).unwrap();
    assert_eq!(request["message_ids"], json!([4400]), "only newly visible rows are fetched");
    drop(chat);
    acc.drain(&w);
    assert_eq!(td.sent().len(), n + 1, "the card still owns the open chat");
    drop(card);
    acc.drain(&w);
    assert_eq!(last_request(&td, "closeChat")["chat_id"], -1005);
    assert_eq!(last_request(&td, "setOption")["value"]["value"], false);
    let n = td.sent().len();
    acc.drain(&w);
    assert_eq!(td.sent().len(), n, "close only once");
}

#[test]
fn visible_messages_are_replayed_after_sign_in_and_isolated_between_accounts() {
    let a = world();
    let b = world();
    let td = FakeTd::new();
    let acc = account(td.clone(), None);
    let view = watch_messages(&a, 7, vec![42]);
    let td_b = FakeTd::new();
    let acc_b = account(td_b.clone(), None);
    acc_b.drain(&b);
    assert!(td_b.sent().is_empty(), "a fixture cannot subscribe another store's worker");
    acc.drain(&a);
    acc.on_update(&a, &auth("authorizationStateWaitTdlibParameters"));
    acc.on_update(&a, &auth("authorizationStateReady"));
    acc.drain(&a);
    assert_eq!(td.sent_types().iter().filter(|s| *s == "openChat").count(), 1,
        "reauthorization must wait for this client's chat announcement too");
    acc.on_update(&a, &chat_object(7, "restored chat", json!([])));
    acc.drain(&a);
    assert_eq!(td.sent_types().iter().filter(|s| *s == "openChat").count(), 2,
        "visible rows resume as soon as their chat is ready");
    for list in ["main", "archive"] {
        acc.on_update(&a, &json!({"@type": "error", "code": 404,
            "@extra": format!("load_chats:{list}")}).to_string());
    }
    acc.drain(&a);
    assert_eq!(td.sent_types().iter().filter(|s| *s == "openChat").count(), 2);
    assert_eq!(td.sent_types().iter().filter(|s| *s == "getMessages").count(), 2);
    drop(view);
    let n = td.sent().len();
    acc.on_update(&a, &json!({
        "@type": "messages", "@extra": "visible:7", "messages": [{
            "chat_id": 7, "id": 42, "content": {"@type": "messageText", "text": {"text": "late"}},
        }],
    }).to_string());
    assert_eq!(td.sent().len(), n, "a late snapshot must not resubscribe a closed view");
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

#[test]
fn a_locked_telegram_session_reports_the_connection_failure_until_auth_resumes() {
    let td = FakeTd::new();
    let acc = account(td.clone(), None);
    let w = world();
    acc.drain(&w);
    acc.on_update(&w, &auth("authorizationStateWaitTdlibParameters"));
    let parameters: serde_json::Value = serde_json::from_str(&td.sent()[0]).unwrap();
    let extra = parameters["@extra"].clone();
    assert!(extra["operation"].is_u64(), "parameters are correlated");
    acc.on_update(&w, &json!({"@type": "error", "code": 400,
        "message": "Can't lock file \"td.binlog\", because it is already in use; check for another program instance running",
        "@extra": extra,
    }).to_string());

    let runtime = runtime::of(w.store());
    let error = runtime.connection_error().expect("a visible failure");
    assert!(error.contains("another app window"));
    assert!(error.contains("restart"));
    assert_eq!(runtime.topics_status(42), Err(error));
    assert!(!runtime.list_syncing());
    assert!(!runtime.send(&super::get_forum_topics(42, 0, 0, 0)));
    runtime.want_history(42);
    acc.drain(&w);
    assert_eq!(td.sent_types(), vec!["setTdlibParameters"]);
    assert_eq!(
        state(&w), "connecting",
        "a failure is local to this process"
    );
    assert!(runtime::of(world().store()).connection_error().is_none());

    acc.on_update(&w, &auth("authorizationStateReady"));
    acc.drain(&w);
    assert!(runtime.connection_error().is_none());
    assert!(runtime.list_syncing());
    assert_eq!(
        td.sent_types(),
        vec!["setTdlibParameters", "loadChats"]
    );
    for list in ["main", "archive"] {
        acc.on_update(&w, &json!({"@type": "error", "code": 404,
            "@extra": format!("load_chats:{list}")}).to_string());
    }
    acc.drain(&w);
    assert_eq!(td.sent_types(), vec!["setTdlibParameters", "loadChats", "loadChats", "getChatHistory"]);
    acc.on_update(
        &w,
        &json!({"@type": "error", "code": 404,
            "message": "Chat not found", "@extra": "unrelated"}).to_string(),
    );
    assert!(runtime.connection_error().is_none());
}

/// A retry belongs only to the parameters step. Advancing sign-in cancels
/// it, and a late failure must not put that step back over the phone prompt.
#[test]
fn initialization_retries_stop_when_signin_advances() {
    let td = FakeTd::new();
    let acc = Account::new(td.clone(), 17844, tdlib_dir(), None);
    let clock = FakeClock::default();
    let w = timed_world(&clock);
    acc.on_update(&w, &auth("authorizationStateWaitTdlibParameters"));
    let parameters: serde_json::Value = serde_json::from_str(&td.sent()[0]).unwrap();
    let error = json!({
        "@type": "error", "code": 400, "message": "Can't lock file: already in use",
        "@extra": parameters["@extra"],
    }).to_string();
    acc.on_update(&w, &error);
    acc.on_update(&w, &auth("authorizationStateWaitPhoneNumber"));
    acc.on_update(&w, &error);
    clock.advance(super::PARAMETERS_RETRY);
    acc.drain(&w);
    assert_eq!(td.sent_types(), vec!["setTdlibParameters"]);
    assert_eq!(state(&w), "wait_phone");
    assert_eq!(runtime::of(w.store()).connection_note().as_deref(),
        Some("sign in to Telegram to download media"));

    acc.on_update(&w, &auth("authorizationStateReady"));
    acc.on_update(&w, &error);
    clock.advance(super::PARAMETERS_RETRY);
    acc.drain(&w);
    assert_eq!(td.sent_types(), vec!["setTdlibParameters", "loadChats"]);
    assert_eq!(runtime::of(w.store()).connection_note(), None);
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

fn mention_group(acc: &Account<FakeTd>, w: &World, chat: i64, count: i64) {
    acc.on_update(w, &json!({
        "@type": "updateNewChat",
        "chat": {
            "id": chat, "title": "Unread group",
            "type": {"@type": "chatTypeSupergroup", "is_channel": false},
            "positions": [{"list": {"@type": "chatListMain"}, "order": "100"}],
            "unread_count": 0, "last_read_inbox_message_id": 10000,
            "unread_mention_count": count,
            "notification_settings": {"mute_for": 3600}
        }
    }).to_string());
}

fn last_mention_request(td: &FakeTd) -> serde_json::Value {
    td.sent().iter().rev().filter_map(|raw| serde_json::from_str::<serde_json::Value>(raw).ok())
        .find(|v| v["@type"] == "searchChatMessages").expect("an unread search request")
}

fn mention_page(chat: i64, ids: &[i64], request: &serde_json::Value, next: Option<i64>) -> String {
    let mut page: serde_json::Value = serde_json::from_str(
        &history_page(chat, ids, "")
    ).unwrap();
    page["@extra"] = request["@extra"].clone();
    for message in page["messages"].as_array_mut().unwrap() {
        message["contains_unread_mention"] = json!(true);
        // The original isn't in the local history; Telegram still knows
        // that this message answers me.
        message["reply_to"] = json!({"@type": "messageReplyToMessage", "chat_id": chat, "message_id": 42});
    }
    if let Some(next) = next {
        page["@type"] = json!("foundChatMessages");
        page["next_from_message_id"] = json!(next);
    }
    page.to_string()
}

#[test]
fn unread_mentions_are_loaded_without_history_and_paginate_short_pages() {
    let clock = FakeClock::default();
    let w = timed_world(&clock);
    let td = FakeTd::new();
    let acc = account(td.clone(), None);
    mention_group(&acc, &w, -9001, 3);
    acc.drain(&w);
    let first = last_mention_request(&td);
    assert_eq!(first["filter"]["@type"], "searchMessagesFilterUnreadMention");
    assert_eq!(first["from_message_id"], 0);
    assert_eq!(first["query"], "");
    assert!(runtime::of(w.store()).mentions_status(None).0);

    acc.on_update(&w, &mention_page(-9001, &[100], &first, Some(90)));
    assert_eq!(num(&w, "SELECT COUNT(*) FROM tg_message WHERE unread_mention = 1"), 1);
    assert_eq!(num(&w, "SELECT unread FROM tg_chat WHERE peer = -9001"), 0);
    assert_eq!(super::model::reply_count(w.store()), 3, "count isn't bounded by downloaded history");
    clock.advance(PAGE_GAP + 0.1);
    acc.drain(&w);
    let second = last_mention_request(&td);
    assert_eq!(second["from_message_id"], 90, "a short page must follow the server continuation");
    acc.on_update(&w, &mention_page(-9001, &[80, 70], &second, Some(0)));
    assert_eq!(num(&w, "SELECT COUNT(*) FROM tg_message WHERE unread_mention = 1"), 3);
    assert_eq!(runtime::of(w.store()).mentions_status(None), (false, false));
}

#[test]
fn legacy_mention_pages_progress_without_repeating_the_boundary() {
    let clock = FakeClock::default();
    let w = timed_world(&clock);
    let td = FakeTd::new();
    let acc = account(td.clone(), None);
    mention_group(&acc, &w, -9001, 2);
    acc.drain(&w);
    let first = last_mention_request(&td);
    acc.on_update(&w, &mention_page(-9001, &[100], &first, None));
    clock.advance(PAGE_GAP + 0.1);
    acc.drain(&w);
    let second = last_mention_request(&td);
    assert_eq!(second["from_message_id"], 99);
    acc.on_update(&w, &mention_page(-9001, &[], &second, None));
    assert!(!runtime::of(w.store()).mentions_status(None).0);
}

#[test]
fn mention_reads_from_another_device_clear_only_that_chat_and_reject_stale_pages() {
    let clock = FakeClock::default();
    let w = timed_world(&clock);
    let td = FakeTd::new();
    let acc = account(td.clone(), None);
    mention_group(&acc, &w, -9001, 2);
    acc.drain(&w);
    let first = last_mention_request(&td);
    acc.on_update(&w, &mention_page(-9001, &[100, 90], &first, Some(80)));
    clock.advance(PAGE_GAP + 0.1);
    acc.drain(&w);
    let stale = last_mention_request(&td);
    mention_group(&acc, &w, -9002, 1);
    let mut same_id: serde_json::Value = serde_json::from_str(&mention_page(-9002, &[100], &first, None)).unwrap();
    same_id = same_id["messages"][0].clone();
    acc.on_update(&w, &same_id.to_string());
    acc.on_update(&w, &json!({"@type": "updateMessageMentionRead", "chat_id": -9001,
        "message_id": 100, "unread_mention_count": 1}).to_string());
    assert_eq!(num(&w, "SELECT unread_mention FROM tg_message WHERE chat = -9001 AND id = 100"), 0);
    assert_eq!(num(&w, "SELECT unread_mention FROM tg_message WHERE chat = -9002 AND id = 100"), 1);
    assert_eq!(num(&w, "SELECT mention FROM tg_chat WHERE peer = -9001"), 1);
    acc.on_update(&w, &mention_page(-9001, &[100, 90, 70], &stale, Some(0)));
    assert_eq!(num(&w, "SELECT COUNT(*) FROM tg_message WHERE chat = -9001"), 2, "stale scan discarded");
    let delayed: serde_json::Value = serde_json::from_str(&mention_page(-9001, &[100], &first, None)).unwrap();
    acc.on_update(&w, &delayed["messages"][0].to_string());
    assert_eq!(num(&w, "SELECT unread_mention FROM tg_message WHERE chat = -9001 AND id = 100"), 0,
        "a delayed ordinary message response cannot undo the read either");
    acc.on_update(&w, &json!({"@type": "updateChatUnreadMentionCount", "chat_id": -9001,
        "unread_mention_count": 0}).to_string());
    assert_eq!(num(&w, "SELECT COUNT(*) FROM tg_message WHERE chat = -9001 AND unread_mention = 1"), 0);
    assert_eq!(super::model::reply_count(w.store()), 1, "the other group is still unread");
}

#[test]
fn unread_search_errors_and_timeouts_leave_the_count_and_allow_refresh() {
    let clock = FakeClock::default();
    let w = timed_world(&clock);
    let td = FakeTd::new();
    let acc = account(td.clone(), None);
    mention_group(&acc, &w, -9001, 2);
    acc.drain(&w);
    let first = last_mention_request(&td);
    acc.on_update(&w, &json!({"@type": "error", "code": 429, "message": "Too Many Requests: retry after 5",
        "@extra": first["@extra"]}).to_string());
    let sent = td.sent().len();
    clock.advance(5.0);
    acc.drain(&w);
    assert_eq!(td.sent().len(), sent, "the flood wait is honored");
    clock.advance(1.1);
    acc.drain(&w);
    assert_eq!(td.sent().len(), sent + 1);
    acc.on_update(&w, &json!({"@type": "error", "code": 500, "message": "unavailable",
        "@extra": last_mention_request(&td)["@extra"]}).to_string());
    assert_eq!(runtime::of(w.store()).mentions_status(None), (false, true));
    assert_eq!(super::model::reply_count(w.store()), 2);
    runtime::of(w.store()).want_mentions(-9001);
    clock.advance(PAGE_GAP + 0.1);
    acc.drain(&w);
    assert_ne!(last_mention_request(&td)["@extra"], first["@extra"]);
    clock.advance(super::PAGE_PATIENCE + 0.1);
    acc.drain(&w);
    assert_eq!(runtime::of(w.store()).mentions_status(None), (false, true));
    assert_eq!(super::model::reply_count(w.store()), 2);
}

#[test]
fn a_completed_rescan_reconciles_old_notifications_and_preserves_new_arrivals() {
    let clock = FakeClock::default();
    let w = timed_world(&clock);
    let td = FakeTd::new();
    let acc = account(td.clone(), None);
    mention_group(&acc, &w, -9001, 2);
    acc.drain(&w);
    let first = last_mention_request(&td);
    acc.on_update(&w, &mention_page(-9001, &[100, 90], &first, Some(0)));
    // The server only announces a smaller count: the completed search
    // must identify which locally cached notification disappeared.
    acc.on_update(&w, &json!({"@type": "updateChatUnreadMentionCount", "chat_id": -9001,
        "unread_mention_count": 1}).to_string());
    clock.advance(PAGE_GAP + 0.1);
    acc.drain(&w);
    let second = last_mention_request(&td);
    let incoming: serde_json::Value = serde_json::from_str(&mention_page(-9001, &[110], &first, None)).unwrap();
    acc.on_update(&w, &incoming["messages"][0].to_string());
    acc.on_update(&w, &mention_page(-9001, &[90], &second, Some(0)));
    assert_eq!(num(&w, "SELECT unread_mention FROM tg_message WHERE id = 100"), 0);
    assert_eq!(num(&w, "SELECT unread_mention FROM tg_message WHERE id = 90"), 1);
    assert_eq!(num(&w, "SELECT unread_mention FROM tg_message WHERE id = 110"), 1,
        "a message received during the scan is not part of its snapshot");
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

#[test]
fn links_survive_projection_and_follow_content_edits_in_their_own_chat() {
    use crate::apps::telegram::{model, text};

    let td = FakeTd::new();
    let acc = account(td, None);
    let w = world();
    let mut update: serde_json::Value = serde_json::from_str(&new_message(70, -1008, None, "👋 read this")).unwrap();
    let entities = json!([{"offset": 3, "length": 9,
        "type": {"@type": "textEntityTypeTextUrl", "url": "https://example.org/first"}}]);
    update["message"]["content"]["text"]["entities"] = entities.clone();
    acc.on_update(&w, &update.to_string());
    // The same Telegram message id may belong to another chat too.
    update["message"]["chat_id"] = json!(-1009);
    acc.on_update(&w, &update.to_string());
    let reading = |chat| {
        let history = model::history(w.store(), chat);
        let message = &history[0];
        text::html(&message.text, message.entities.as_deref())
    };
    assert!(reading(-1008).contains("href=\"https://example.org/first\""));

    // A resync replaces the target even when the visible text is identical.
    update["message"]["chat_id"] = json!(-1008);
    update["message"]["content"]["text"]["entities"][0]["type"]["url"] = json!("https://example.org/second");
    acc.on_update(&w, &update.to_string());
    assert!(reading(-1008).contains("href=\"https://example.org/second\""));

    // Switching from text to a media caption retains the new caption entities.
    let mut edit = json!({"@type": "updateMessageContent", "chat_id": -1008, "message_id": 70,
        "new_content": {"@type": "messagePhoto", "caption": {"text": "👋 read this", "entities": entities}, "photo": {"sizes": []}}});
    acc.on_update(&w, &edit.to_string());
    assert!(reading(-1008).contains("href=\"https://example.org/first\""));
    edit["new_content"]["caption"]["entities"] = json!([]);
    edit["new_content"]["caption"]["text"] = json!("main.rs notes.md report.pdf Dr.Smith it.Then https://example.org me@example.org");
    acc.on_update(&w, &edit.to_string());
    assert!(!reading(-1008).contains("<a "));
    assert_eq!(model::history(w.store(), -1008)[0].entities, Some(vec![]));
    assert!(reading(-1009).contains("href=\"https://example.org/first\""));

    // A fresh message with an empty entity array also forbids local guessing.
    update["message"]["content"]["text"] = edit["new_content"]["caption"].clone();
    acc.on_update(&w, &update.to_string());
    assert!(!reading(-1008).contains("<a "));
    assert_eq!(model::history(w.store(), -1008)[0].entities, Some(vec![]));
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
fn the_chat_list_is_paged_and_unconfirmed_sends_stay_visible() {
    let td = FakeTd::new();
    let acc = account(td.clone(), None);
    let w = world();
    let u = json!({"@type": "updateNewMessage", "message": my_line(900, 2, Some("messageSendingStatePending"))});
    acc.on_update(&w, &u.to_string());

    acc.on_ready(&w);
    assert_eq!(
        my_lines(&w, 2),
        vec![(900, "failed".to_string())],
        "an unconfirmed send must not disappear"
    );
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
    acc.drain(&w);
    assert!(runtime::of(w.store()).send(&super::request_media(-9_010, 4242, true)));
    acc.drain(&w);
    let req: serde_json::Value = serde_json::from_str(td.sent().last().unwrap()).unwrap();
    assert_eq!(req["@type"], "getMessage");
    assert_eq!(req["chat_id"], -9_010);
    assert_eq!(req["message_id"], 4242);
    assert_eq!(req["@extra"]["context"], "media:-9010:4242:true");

    let mut answer = my_line(4242, -9_010, None);
    answer["@extra"] = req["@extra"].clone();
    answer["content"] = json!({"@type": "messageVideo", "caption": {"text": "clip"},
        "video": {"duration": 3, "width": 640, "height": 360,
                  "video": {"id": 9, "remote": {"id": "RID_V", "unique_id": "VU"}},
                  "thumbnail": {"file": {"id": 8, "remote": {"id": "RID_T", "unique_id": "TU"}}}}});
    acc.on_update(&w, &answer.to_string());
    let rid: String = w.store().conn().query_row(
        "SELECT media_clip_rid FROM tg_message WHERE id = 4242", [], |r| r.get(0)).unwrap();
    assert_eq!(rid, "RID_V");
    let download: serde_json::Value = serde_json::from_str(td.sent().last().unwrap()).unwrap();
    assert_eq!(download["file_id"], 9, "download the refreshed clip, not its poster");
    assert_eq!(download["synchronous"], true);
    assert_eq!(download["@extra"]["context"], req["@extra"]["context"]);
}

#[test]
fn media_source_errors_and_timeouts_are_visible_without_automatic_retries() {
    let td = FakeTd::new();
    let acc = account(td.clone(), None);
    let w = world();
    let rt = runtime::of(w.store());
    let context = super::media_context(7, 42, true);
    for (code, message) in [(404, "Message not found"), (429, "Too Many Requests: retry after 60")] {
        acc.send(&w, &super::request_media(7, 42, true));
        let request: serde_json::Value = serde_json::from_str(td.sent().last().unwrap()).unwrap();
        acc.on_update(&w, &json!({"@type": "error", "code": code, "message": message,
            "@extra": request["@extra"]}).to_string());
        assert!(rt.operations.media_note(&context).unwrap().contains(message));
        let count = td.sent().len();
        for _ in 0..10 { acc.drain(&w); }
        assert_eq!(td.sent().len(), count, "respect failures and Telegram's rate limit");
    }
    acc.send(&w, &super::request_media(7, 42, true));
    assert_eq!(rt.operations.media_note(&context).as_deref(), Some("loading media details…"));
    rt.operations.expire(w.store(), std::time::Instant::now() + std::time::Duration::from_secs(121));
    assert!(rt.operations.media_note(&context).unwrap().contains("did not respond"));
}

#[test]
fn a_message_without_the_requested_media_finishes_with_an_error() {
    let td = FakeTd::new();
    let acc = account(td.clone(), None);
    let w = world();
    acc.send(&w, &super::request_media(7, 42, true));
    let request: serde_json::Value = serde_json::from_str(td.sent().last().unwrap()).unwrap();
    let mut answer = my_line(42, 7, None);
    answer["@extra"] = request["@extra"].clone();
    acc.on_update(&w, &answer.to_string());
    let note = runtime::of(w.store()).operations.media_note(&super::media_context(7, 42, true)).unwrap();
    assert!(note.contains("no longer has downloadable media"));
    assert_eq!(td.sent_types(), vec!["getMessage"]);
}

#[test]
fn a_media_request_that_times_out_waiting_for_its_chat_requires_a_retry() {
    let td = FakeTd::new();
    let acc = account(td.clone(), None);
    let w = world();
    acc.drain(&w);
    acc.on_ready(&w);
    acc.send(&w, &super::request_media(7, 42, true));
    assert_eq!(td.sent_types(), vec!["loadChats"]);
    let rt = runtime::of(w.store());
    let op = rt.operations.list().last().unwrap().id;
    rt.operations.expire(w.store(), std::time::Instant::now() + std::time::Duration::from_secs(121));
    acc.loading_chats.set(false);
    acc.drain(&w);
    assert_eq!(td.sent_types(), vec!["loadChats"], "an expired queued request stays stopped");
    crate::apps::telegram::operations::retry(w.store(), op);
    acc.drain(&w);
    assert_eq!(td.sent_types(), vec!["loadChats", "getMessage"]);
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
    let path = sent["input_message_content"]["document"]["document"]["path"]
        .as_str()
        .unwrap()
        .to_string();
    assert!(
        !path.starts_with('~') && path.ends_with("/Downloads/report.pdf"),
        "{path}"
    );
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
        !runtime::of(w.store()).loading(-1010),
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
            v["@extra"]["context"].as_str().unwrap_or("").to_string(),
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

    runtime::of(w.store()).want_history(-9_001);
    acc.drain(&w);
    let req: serde_json::Value = serde_json::from_str(&td.sent()[0]).expect("valid JSON");
    assert_eq!(req["@type"], "getChatHistory");
    assert_eq!(req["chat_id"], -9_001);
    assert_eq!(req["from_message_id"], 0);
    assert_eq!(req["limit"], HISTORY_PAGE);
    assert_eq!(req["only_local"], false);
    assert_eq!(req["@extra"]["context"], "history:-9001:fill:0");
    assert_eq!(super::parse_history_extra("history:2:fill:0"), Some((2, Walk::Fill, 0)));
    assert_eq!(super::parse_history_extra("history:-100:tail:77"), Some((-100, Walk::Tail, 77)));
    assert_eq!(super::parse_history_extra("kick"), None);

    // A second chat wanted while the first page is on the wire waits.
    acc.want(&w, -9_002);
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
    acc.want(&w, -9_003);
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
    assert!(runtime::of(w.store()).loading(-9_003));

    // A page never answered: patience runs out and the next goes.
    acc.want(&w, -9_004);
    clock.advance(super::PAGE_PATIENCE + 1.0);
    acc.drain(&w);
    assert_eq!(td.sent().len(), 3);
    // Any other refusal ends that chat's walk.
    let gone = json!({"@type": "error", "code": 400, "message": "Chat not found", "@extra": "history:-9004:fill:0"});
    acc.on_update(&w, &gone.to_string());
    assert!(!runtime::of(w.store()).loading(-9_004));
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
    let td = FakeTd::new();
    let acc = account(td.clone(), None);
    let w = world();
    acc.want(&w, -555);
    assert!(runtime::of(w.store()).loading(-555), "on from the request");
    acc.on_update(&w, &history_page(-555, &[300, 200], "history:-555:fill:0"));
    assert!(runtime::of(w.store()).loading(-555), "still on while pages come");
    acc.on_update(&w, &history_page(-555, &[], "history:-555:fill:200"));
    assert!(!runtime::of(w.store()).loading(-555), "off at the chat's beginning");

    acc.on_ready(&w);
    assert!(runtime::of(w.store()).list_syncing());
    let done = json!({"@type": "error", "code": 404, "message": "Not Found", "@extra": "load_chats:main"});
    acc.on_update(&w, &done.to_string());
    assert!(runtime::of(w.store()).list_syncing(), "the archive is still to come");
    let done = json!({"@type": "error", "code": 404, "message": "Not Found", "@extra": "load_chats:archive"});
    acc.on_update(&w, &done.to_string());
    assert!(!runtime::of(w.store()).list_syncing());
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

/// getRemoteFile can already hold the finished file. Its reply is ingested
/// directly, without starting another download or leaving progress behind.
#[test]
fn a_file_request_that_is_already_complete_is_cached() {
    let td = FakeTd::new();
    let acc = account(td.clone(), None);
    let w = world();
    let src = engine_file("completed-answer");
    let update: serde_json::Value =
        serde_json::from_str(&file_update(&src.to_string_lossy(), "already_here")).unwrap();
    let mut answer = update["file"].clone();
    answer["@extra"] = json!("file:RID_HERE");

    acc.on_update(&w, &answer.to_string());
    assert!(w.with_cap::<dyn Blobs, _>(|b| b.contains("tg:already_here")).unwrap());
    assert!(!src.exists());
    assert_eq!(td.sent_types(), vec!["deleteFile"]);
    assert_eq!(runtime::of(w.store()).download("tg:already_here"), None);
}

/// A picture a drawing found missing is asked for on the worker's next
/// pass, by the durable remote id the row keeps — and once, however many
/// draws ask for it.
#[test]
fn a_wanted_file_is_asked_for_on_the_next_pass() {
    let td = FakeTd::new();
    let acc = account(td.clone(), None);
    let w = world();
    runtime::of(w.store()).want_file("RID_PIC");
    runtime::of(w.store()).want_file("RID_PIC");
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
    assert_eq!(mine[0]["@extra"]["context"], "file:RID_PIC");
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
    assert_eq!(c["photo"]["@type"], "inputPhoto");
    assert_eq!(c["photo"]["photo"]["@type"], "inputFileLocal");
    // The files app's `~/` spelling is the disk's by the time the engine
    // reads it.
    let path = c["photo"]["photo"]["path"].as_str().unwrap();
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
        assert_eq!(c[names_it][names_it]["@type"], "inputFileLocal", "{path}");
        // Spelled as the disk has it, whatever the files app showed.
        assert_eq!(
            c[names_it][names_it]["path"].as_str().unwrap(),
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
    assert_eq!(num(&w, "SELECT mention FROM tg_chat WHERE peer = -1006"), 2);
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
    assert_eq!(gathered(&w), (None, None, Some("👍 3".into())), "null metadata awaits confirmation");
    assert!(td.sent().is_empty());
    let _view = watch_messages(&w, -1006, vec![4300]);
    reaction_state_tests::confirm_empty(&acc, &td, &w, json!({"chat_id": -1006, "id": 4300}));
    assert_eq!(gathered(&w), (None, None, None), "the confirmed last removal clears it");
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

#[test]
fn block_state_survives_user_refreshes_and_tracks_chats_profiles_and_remote_unblocks() {
    let acc = account(FakeTd::new(), None);
    let w = world();
    let blocked = || num(&w, "SELECT blocked FROM tg_peer WHERE id = 2");
    let update = |kind: &str, list: serde_json::Value| json!({
        "@type": kind, "chat_id": 2, "block_list": list,
    }).to_string();

    // The block can arrive before a person's name or any conversation.
    acc.on_update(&w, &update("updateChatBlockList", json!({"@type": "blockListMain"})));
    assert_eq!(blocked(), 1);
    acc.on_update(&w, &user_update(2, "Vera", "Kovac", "vera"));
    assert_eq!(blocked(), 1, "ordinary user updates do not carry block state");
    assert_eq!(num(&w, "SELECT COUNT(*) FROM tg_chat"), 0);

    let mut chat: serde_json::Value = serde_json::from_str(&private_chat_update(2, "Vera", 0)).unwrap();
    chat["chat"]["block_list"] = json!({"@type": "blockListMain"});
    acc.on_update(&w, &chat.to_string());
    assert_eq!(blocked(), 1, "initial chat snapshots include existing blocks");
    acc.on_update(&w, &update("updateChatBlockList", json!(null)));
    assert_eq!(blocked(), 0, "unblocking on another device is visible here");

    acc.on_update(&w, &json!({
        "@type": "updateUserFullInfo", "user_id": 2,
        "user_full_info": {"@type": "userFullInfo", "block_list": {"@type": "blockListMain"}},
    }).to_string());
    assert_eq!(blocked(), 1);
    acc.on_update(&w, &json!({
        "@type": "userFullInfo", "@extra": "user_full_info:2",
        "block_list": {"@type": "blockListStories"},
    }).to_string());
    assert_eq!(blocked(), 0, "hiding stories does not block messages");

    // Removing a contact elsewhere updates the address book but keeps the peer/chat.
    let mut user: serde_json::Value = serde_json::from_str(&user_update(2, "Vera", "Kovac", "vera")).unwrap();
    user["user"]["is_contact"] = json!(false);
    acc.on_update(&w, &user.to_string());
    assert_eq!(num(&w, "SELECT is_contact FROM tg_peer WHERE id = 2"), 0);
    assert_eq!(num(&w, "SELECT COUNT(*) FROM tg_chat WHERE peer = 2"), 1);
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

#[test]
fn peer_actions_ignore_replies_from_an_ended_session_after_retrying() {
    use super::PeerAction;
    let w = world();
    let td = FakeTd::new();
    let acc = account(td.clone(), None);
    let runtime = runtime::of(w.store());
    acc.drain(&w);
    assert!(runtime.send_peer_action(7, PeerAction::Block));
    acc.drain(&w);
    let first: serde_json::Value = serde_json::from_str(&td.sent()[0]).unwrap();
    acc.on_update(&w, &auth("authorizationStateClosed"));
    assert!(!runtime.peer_action_pending(7));
    acc.drain(&w);
    assert!(!runtime.send_peer_action(7, PeerAction::Block), "a closed account stays disconnected");

    let replacement = account(td.clone(), None);
    replacement.drain(&w);
    assert!(runtime.send_peer_action(7, PeerAction::Block));
    replacement.drain(&w);
    let retry: serde_json::Value = serde_json::from_str(&td.sent()[1]).unwrap();
    assert_ne!(first["@extra"], retry["@extra"]);
    runtime.take_notices();
    for kind in ["ok", "error"] {
        replacement.on_update(&w, &json!({
            "@type": kind, "@extra": first["@extra"], "code": 400, "message": "stale error",
        }).to_string());
        assert!(runtime.peer_action_pending(7), "an old reply cannot complete the retry");
        assert_eq!(num(&w, "SELECT count(*) FROM tg_peer WHERE blocked = 1"), 0);
        assert!(runtime.take_notices().is_empty());
    }
    replacement.on_update(&w, &json!({"@type": "ok", "@extra": retry["@extra"]}).to_string());
    assert!(!runtime.peer_action_pending(7));
    assert_eq!(num(&w, "SELECT count(*) FROM tg_peer WHERE blocked = 1"), 1);
}

#[test]
fn stopping_a_worker_releases_its_pending_peer_actions() {
    use super::PeerAction;
    let a = world();
    let b = world();
    let account_a = account(FakeTd::new(), None);
    let account_b = account(FakeTd::new(), None);
    account_a.drain(&a);
    account_b.drain(&b);
    let runtime_a = runtime::of(a.store());
    let runtime_b = runtime::of(b.store());
    assert!(runtime_a.send_peer_action(7, PeerAction::Block));
    assert!(runtime_a.send_peer_action(8, PeerAction::DeleteContact));
    assert!(runtime_b.send_peer_action(7, PeerAction::Block));
    drop(account_a);
    assert!(!runtime_a.peer_action_pending(7));
    assert!(!runtime_a.peer_action_pending(8));
    assert!(!runtime_a.send_peer_action(7, PeerAction::Block));
    assert!(runtime_b.peer_action_pending(7), "another store's worker stays connected");
    let replacement = account(FakeTd::new(), None);
    replacement.drain(&a);
    assert!(runtime_a.send_peer_action(7, PeerAction::Block));
    assert!(runtime_a.send_peer_action(8, PeerAction::DeleteContact));
}

#[test]
fn tracked_profile_deletes_keep_their_own_retry_and_stale_reply_guards() {
    use super::PeerAction;
    let w = world();
    let td = FakeTd::new();
    let acc = account(td.clone(), None);
    let rt = runtime::of(w.store());
    acc.on_update(&w, &chat_object(7, "keep until confirmed", json!([])));
    acc.drain(&w);
    let last = || serde_json::from_str::<serde_json::Value>(td.sent().last().unwrap()).unwrap();

    assert!(rt.send_peer_action(7, PeerAction::DeleteChat));
    acc.drain(&w);
    let refused = last();
    acc.on_update(&w, &json!({"@type": "error", "code": 403, "message": "forbidden",
        "@extra": refused["@extra"]}).to_string());
    assert!(!rt.operations.list()[0].retryable(), "retry must create a new profile-action attempt");
    assert!(!rt.peer_action_pending(7));

    assert!(rt.send_peer_action(7, PeerAction::DeleteChat));
    acc.drain(&w);
    let retired = last();
    rt.disconnect();
    let replacement = account(td.clone(), None);
    replacement.drain(&w);
    assert!(rt.send_peer_action(7, PeerAction::DeleteChat));
    replacement.drain(&w);
    let current = last();
    replacement.on_update(&w, &json!({"@type": "ok", "@extra": retired["@extra"]}).to_string());
    assert_eq!(num(&w, "SELECT COUNT(*) FROM tg_chat WHERE peer = 7"), 1);
    assert!(rt.peer_action_pending(7), "a stale reply cannot finish the current attempt");

    replacement.on_update(&w, &json!({"@type": "ok", "@extra": current["@extra"]}).to_string());
    assert_eq!(num(&w, "SELECT COUNT(*) FROM tg_chat WHERE peer = 7"), 0);
    assert!(!rt.peer_action_pending(7));
}

#[test]
fn workers_consume_only_their_own_stores_commands_and_download_requests() {
    let a = world();
    let b = world();
    let td_a = FakeTd::new();
    let td_b = FakeTd::new();
    let account_a = account(td_a.clone(), None);
    let account_b = account(td_b.clone(), None);
    account_a.drain(&a);
    account_b.drain(&b);

    let state_a = runtime::of(a.store());
    let state_b = runtime::of(b.store());
    assert!(state_a.send(&super::send_message(7, "for A", None)));
    assert!(state_b.send(&super::send_message(7, "for B", None)));
    state_a.want_file("A-photo");
    assert!(state_a.send(&super::request_media(7, 42, true)));
    state_a.want_history(7);
    account_b.drain(&b);
    assert_eq!(td_b.sent_types(), vec!["sendMessage"]);
    assert!(td_b.sent()[0].contains("for B"));
    assert!(td_a.sent().is_empty());
    assert!(!state_b.loading(7));

    account_a.drain(&a);
    assert_eq!(td_a.sent_types(), vec!["sendMessage", "getMessage", "getRemoteFile", "getChatHistory"]);
    assert!(td_a.sent()[0].contains("for A"));
    drop(account_a);
    assert!(!state_a.send(&super::send_message(7, "after shutdown", None)));
    assert!(state_b.send(&super::send_message(7, "still connected", None)));
}

#[test]
fn an_image_rejected_by_the_worker_is_visible_and_recoverable() {
    use crate::apps::telegram::{
        model::Carried,
        operations::{Failures, Status},
        panels,
    };
    use kernel::app::ProblemSource;
    let td = FakeTd::new();
    let acc = account(td.clone(), None);
    let w = world();
    acc.drain(&w);
    assert!(panels::wire(
        w.store(),
        &super::send_file(
            7,
            Some(42),
            &Carried {
                path: "/nonexistent/photo.png".into()
            },
            ""
        )
    ));
    acc.drain(&w);
    assert!(td.sent().is_empty(), "invalid files never reach TDLib");
    let rt = runtime::of(w.store());
    let op = rt.operations.list().remove(0);
    assert!(matches!(
        op.status,
        Status::Failed {
            uncertain: false,
            ..
        }
    ));
    assert!(op.retryable());
    assert!(Failures.list(w.store())[0]
        .announce
        .as_ref()
        .unwrap()
        .contains("photo.png"));
    let retry: serde_json::Value =
        serde_json::from_str(&rt.operations.retry(op.id).unwrap()).unwrap();
    assert_eq!(retry["reply_to"]["message_id"], 42);
    assert_eq!(
        retry["input_message_content"]["photo"]["photo"]["path"],
        "/nonexistent/photo.png"
    );
}

#[test]
fn chat_mutations_are_applied_only_after_successful_acknowledgement() {
    use crate::apps::telegram::{model, panels};
    let td = FakeTd::new();
    let acc = account(td.clone(), None);
    let w = world();
    acc.on_update(&w, &chat_object(7, "test", json!([])));
    acc.drain(&w);
    assert!(panels::wire(w.store(), &super::set_chat_muted(7, true)));
    acc.drain(&w);
    assert!(!model::peer(w.store(), 7).unwrap().muted);
    let req: serde_json::Value = serde_json::from_str(td.sent().last().unwrap()).unwrap();
    acc.on_update(
        &w,
        &json!({"@type": "error", "code": 403, "message": "forbidden", "@extra": req["@extra"]})
            .to_string(),
    );
    assert!(!model::peer(w.store(), 7).unwrap().muted);
    assert!(panels::wire(w.store(), &super::set_chat_muted(7, true)));
    acc.drain(&w);
    let req: serde_json::Value = serde_json::from_str(td.sent().last().unwrap()).unwrap();
    acc.on_update(
        &w,
        &json!({"@type": "ok", "@extra": req["@extra"]}).to_string(),
    );
    assert!(model::peer(w.store(), 7).unwrap().muted);
}

#[test]
fn acknowledgements_without_local_changes_never_enter_the_writer() {
    let acc = account(FakeTd::new(), None);
    let w = world();
    // Even an empty write would fail at this gate and create a problem.
    w.store().db().set_writable(false);
    for kind in [
        "getChatHistory",
        "getMessage",
        "viewMessages",
        "deleteMessages",
        "setChatDraftMessage",
        "sendMessage",
    ] {
        acc.acknowledged(&w, &json!({"@type": kind, "chat_id": 7}));
        assert!(runtime::of(w.store()).operations.list().is_empty(), "{kind}");
    }
    // A mutation still reaches that gate and reports its refusal.
    acc.acknowledged(
        &w,
        &json!({"@type": "toggleChatIsPinned", "chat_id": 7, "is_pinned": true}),
    );
    assert!(runtime::of(w.store()).operations.list()[0].line().contains("read-only"));
}

#[test]
fn a_completed_download_response_is_cached_without_waiting_for_another_update() {
    use crate::apps::telegram::{operations::Status, panels};
    let td = FakeTd::new();
    let acc = account(td.clone(), None);
    let w = world();
    acc.drain(&w);
    assert!(panels::wire(w.store(), &download_file(77, 32)));
    acc.drain(&w);
    let req: serde_json::Value = serde_json::from_str(td.sent().last().unwrap()).unwrap();
    let path = engine_file("response-completion");
    let reply = json!({"@type": "file", "id": 77, "size": 20, "remote": {"unique_id": "complete-response"},
        "local": {"is_downloading_completed": true, "path": path.to_string_lossy()}, "@extra": req["@extra"]});
    acc.on_update(&w, &reply.to_string());
    assert!(w
        .with_cap::<dyn Blobs, _>(|b| b.contains("tg:complete-response"))
        .unwrap());
    assert_eq!(
        runtime::of(w.store()).operations.list()[0].status,
        Status::Done
    );
}

#[test]
fn list_errors_are_not_mistaken_for_the_end_of_the_list() {
    let td = FakeTd::new();
    let acc = account(td.clone(), None);
    let w = world();
    acc.on_ready(&w);
    let req: serde_json::Value = serde_json::from_str(td.sent().last().unwrap()).unwrap();
    acc.on_update(&w, &json!({"@type": "error", "code": 429, "message": "Too Many Requests: retry after 30", "@extra": req["@extra"]}).to_string());
    assert_eq!(td.sent_types(), vec!["loadChats"]);
    assert!(!runtime::of(w.store()).list_syncing());
    assert!(runtime::of(w.store()).operations.list()[0]
        .line()
        .contains("429"));
}

#[test]
fn a_late_history_reply_cannot_release_another_chats_request() {
    let td = FakeTd::new();
    let acc = account(td.clone(), None);
    let clock = FakeClock::default();
    let w = timed_world(&clock);
    acc.want(&w, 7);
    acc.drain(&w);
    let first: serde_json::Value = serde_json::from_str(td.sent().last().unwrap()).unwrap();
    acc.want(&w, 8);
    clock.advance(31.0);
    acc.drain(&w);
    assert!(!runtime::of(w.store()).loading(7));
    assert!(runtime::of(w.store()).loading(8));
    acc.on_update(
        &w,
        &json!({"@type": "messages", "messages": [], "@extra": first["@extra"]}).to_string(),
    );
    acc.want(&w, 9);
    clock.advance(2.0);
    acc.drain(&w);
    assert_eq!(td.sent_types(), vec!["getChatHistory", "getChatHistory"]);
    assert!(runtime::of(w.store()).loading(8));
}
