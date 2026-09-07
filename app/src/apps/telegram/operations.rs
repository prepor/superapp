//! Acknowledged Telegram commands and transfers, scoped to one store.
//!
//! Requests remain in memory until they settle. Failed requests retain their
//! exact payload for an explicit retry, including attachments and replies.
//! Credentials are never retained or logged. An uncertain send is never retried
//! automatically: a missing answer does not establish that nothing was sent.

use std::collections::{BTreeMap, VecDeque};
use std::path::Path;
use std::sync::{
    atomic::{AtomicBool, Ordering},
    Mutex,
};
use std::time::{Duration, Instant};

use kernel::app::{Problem, ProblemSource};
use kernel::nav::Nav;
use kernel::panel::Verb;
use kernel::store::Store;
use serde_json::{json, Value};

use super::{model::PeerId, runtime, trace};

const PATIENCE: Duration = Duration::from_secs(120);

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Status {
    Pending,
    Done,
    Failed { error: String, uncertain: bool },
}

#[derive(Clone)]
pub struct Operation {
    pub id: u64,
    pub chat: Option<PeerId>,
    pub label: String,
    pub status: Status,
    request: Option<Value>,
    kind: String,
    messages: Vec<(PeerId, i64)>,
    failed_messages: Vec<(PeerId, i64)>,
    incomplete_response: bool,
    files: BTreeMap<i64, (u64, u64)>,
    changed: Instant,
}

impl Operation {
    pub fn line(&self) -> String {
        match &self.status {
            Status::Pending => {
                let (done, total) = self
                    .files
                    .values()
                    .fold((0, 0), |(d, t), (a, b)| (d + a, t + b));
                if let Some(percent) = done.saturating_mul(100).checked_div(total) {
                    format!("{}… {}%", self.label, percent.min(100))
                } else {
                    format!("{}…", self.label)
                }
            }
            Status::Done => format!("{} — done", self.label),
            Status::Failed { error, .. } => format!("{} failed: {error}", self.label),
        }
    }

    pub fn retryable(&self) -> bool {
        matches!(
            self.status,
            Status::Failed {
                uncertain: false,
                ..
            }
        ) && self.request.is_some()
            // Profile actions must start a fresh guarded attempt from their
            // panel; replaying the old correlation would bypass that flow.
            && !self.context().is_some_and(|c| c.starts_with("peer_action:"))
            && self
                .messages
                .iter()
                .all(|m| self.failed_messages.contains(m))
    }

    pub fn foreground(&self) -> bool {
        !matches!(
            self.kind.as_str(),
            "loadChats"
                | "getChatHistory"
                | "searchChatMessages"
                | "getMessage"
                | "getRemoteFile"
                | "downloadFile"
                | "deleteFile"
                | "viewMessages"
                | "setChatDraftMessage"
        )
    }

    pub fn context(&self) -> Option<&str> {
        self.request.as_ref()?["@extra"]["context"].as_str()
    }

    fn sending(&self) -> bool {
        matches!(
            self.kind.as_str(),
            "sendMessage" | "forwardMessages" | "resendMessages"
        )
    }
}

#[derive(Default)]
pub struct Tracker {
    dirty: AtomicBool,
    state: Mutex<State>,
}

#[derive(Default)]
struct State {
    next: u64,
    operations: BTreeMap<u64, Operation>,
    // TDLib may deliver the final update before the sendMessage response.
    settled: VecDeque<((PeerId, i64), Option<String>)>,
}

impl Tracker {
    pub fn changed(&self) {
        self.dirty.store(true, Ordering::Relaxed);
    }
    pub fn take_changed(&self) -> bool {
        self.dirty.swap(false, Ordering::Relaxed)
    }
    /// Add correlation without putting content or credentials in @extra.
    pub fn track(&self, request: &str) -> String {
        self.dirty.store(true, Ordering::Relaxed);
        let Ok(mut v) = serde_json::from_str::<Value>(request) else {
            return request.to_string();
        };
        if v["@extra"]["operation"].as_u64().is_some() {
            return request.to_string();
        }
        if let Some(context) = v["@extra"].as_str().filter(|s| s.starts_with("history:")) {
            if let Some(op) = self
                .list()
                .into_iter()
                .find(|o| o.context() == Some(context) && o.retryable())
            {
                if let Some(request) = self.retry(op.id) {
                    return request;
                }
            }
        }
        let kind = v["@type"].as_str().unwrap_or("request").to_string();
        let mut state = self.state.lock().unwrap();
        state.next += 1;
        let id = state.next;
        let context = v["@extra"].take();
        v["@extra"] = json!({"operation": id, "context": context});
        let secret = matches!(
            kind.as_str(),
            "setTdlibParameters"
                | "setAuthenticationPhoneNumber"
                | "checkAuthenticationCode"
                | "checkAuthenticationPassword"
        );
        state.operations.insert(
            id,
            Operation {
                id,
                chat: v["chat_id"].as_i64(),
                label: label(&v),
                status: Status::Pending,
                request: (!secret).then(|| v.clone()),
                kind,
                messages: Vec::new(),
                failed_messages: Vec::new(),
                incomplete_response: false,
                files: BTreeMap::new(),
                changed: Instant::now(),
            },
        );
        v.to_string()
    }

    pub fn list(&self) -> Vec<Operation> {
        self.state
            .lock()
            .unwrap()
            .operations
            .values()
            .cloned()
            .collect()
    }

    pub fn dismiss(&self, id: u64) {
        self.dirty.store(true, Ordering::Relaxed);
        let mut state = self.state.lock().unwrap();
        if state
            .operations
            .get(&id)
            .is_some_and(|o| o.status != Status::Pending)
        {
            state.operations.remove(&id);
        }
    }

    pub fn fail_context(&self, store: &Store, context: &str, error: &str) {
        for op in self
            .list()
            .into_iter()
            .filter(|o| o.status == Status::Pending && o.context() == Some(context))
        {
            self.fail(store, op.id, error, false);
        }
    }

    /// Reuse the operation id on retry so disconnection cannot lose the
    /// request or leave a second pending record behind.
    pub fn retry(&self, id: u64) -> Option<String> {
        let mut state = self.state.lock().unwrap();
        let op = state.operations.get_mut(&id)?;
        if !op.retryable() {
            return None;
        }
        let mut req = op.request.clone()?;
        let previous = op.messages.clone();
        if op.sending() && !previous.is_empty() {
            req = json!({"@type": "resendMessages", "chat_id": op.chat,
                "message_ids": previous.iter().map(|(_, id)| id).collect::<Vec<_>>(),
                "@extra": {"operation": id, "context": null}});
        }
        op.kind = req["@type"].as_str()?.to_string();
        op.request = Some(req.clone());
        op.messages.clear();
        op.failed_messages.clear();
        op.incomplete_response = false;
        op.files.clear();
        op.status = Status::Pending;
        op.changed = Instant::now();
        for key in previous {
            state.settled.retain(|(saved, _)| *saved != key);
        }
        self.dirty.store(true, Ordering::Relaxed);
        Some(req.to_string())
    }

    pub fn forget_payload(&self, id: u64) {
        if let Some(op) = self.state.lock().unwrap().operations.get_mut(&id) {
            op.request = None;
        }
    }

    pub fn fail(&self, store: &Store, id: u64, error: &str, uncertain: bool) {
        self.fail_at(store.dir(), id, error, uncertain);
    }

    fn fail_at(&self, dir: Option<&Path>, id: u64, error: &str, uncertain: bool) {
        self.dirty.store(true, Ordering::Relaxed);
        let mut state = self.state.lock().unwrap();
        if let Some(op) = state.operations.get_mut(&id) {
            if matches!(&op.status, Status::Failed { error: old, uncertain: u } if old == error && *u == uncertain)
            {
                return;
            }
            op.status = Status::Failed {
                error: error.to_string(),
                uncertain,
            };
            op.changed = Instant::now();
            trace::error(
                dir,
                &format!("request {id} {} chat={:?}: {error}", op.kind, op.chat),
            );
        }
    }

    /// Errors without a request (projection, decoding, native player, disk).
    pub fn report(&self, store: &Store, what: &str, error: &str) {
        self.dirty.store(true, Ordering::Relaxed);
        let mut state = self.state.lock().unwrap();
        if state.operations.values().any(|o| {
            o.label == what && matches!(&o.status, Status::Failed { error: e, .. } if e == error)
        }) {
            return;
        }
        state.next += 1;
        let id = state.next;
        state.operations.insert(
            id,
            Operation {
                id,
                chat: None,
                label: what.to_string(),
                kind: what.to_string(),
                status: Status::Failed {
                    error: error.to_string(),
                    uncertain: false,
                },
                request: None,
                messages: Vec::new(),
                failed_messages: Vec::new(),
                incomplete_response: false,
                files: BTreeMap::new(),
                changed: Instant::now(),
            },
        );
        trace::error(store.dir(), &format!("{what}: {error}"));
    }

    /// Return the acknowledged request so the worker can apply its local half
    /// only after Telegram accepted it.
    pub fn reply(&self, store: &Store, v: &Value) -> Option<Value> {
        let id = v["@extra"]["operation"].as_u64()?;
        self.dirty.store(true, Ordering::Relaxed);
        if v["@type"] == "error" {
            // loadChats uses 404 as its normal end-of-list sentinel.
            let end = v["code"] == 404
                && v["@extra"]["context"]
                    .as_str()
                    .is_some_and(|x| x.starts_with("load_chats:"));
            if !end {
                self.fail(store, id, &error_text(v), false);
                return None;
            }
        }
        let mut state = self.state.lock().unwrap();
        let settled = state.settled.clone();
        let op = state.operations.get_mut(&id)?;
        if op.sending() {
            let messages: Vec<&Value> = if v["@type"] == "messages" {
                v["messages"].as_array()?.iter().collect()
            } else {
                vec![v]
            };
            let expected = op
                .request
                .as_ref()
                .and_then(|r| r["message_ids"].as_array())
                .map_or(1, Vec::len);
            op.incomplete_response = messages.len() != expected;
            let mut failed = op.incomplete_response.then(|| {
                "Telegram did not confirm every message. Check the chat before sending again."
                    .to_string()
            });
            for msg in messages {
                let (Some(chat), Some(mid)) = (msg["chat_id"].as_i64(), msg["id"].as_i64()) else {
                    op.incomplete_response = true;
                    failed = Some("Telegram did not confirm every message. Check the chat before sending again.".to_string());
                    continue;
                };
                match settled
                    .iter()
                    .find(|(key, _)| *key == (chat, mid))
                    .map(|(_, error)| error)
                {
                    Some(Some(error)) => {
                        failed = Some(error.clone());
                        op.messages.push((chat, mid));
                        op.failed_messages.push((chat, mid));
                    }
                    Some(None) => {}
                    None if msg["sending_state"]["@type"] == "messageSendingStateFailed" => {
                        failed = Some(error_text(&msg["sending_state"]["error"]));
                        op.messages.push((chat, mid));
                        op.failed_messages.push((chat, mid));
                    }
                    None if !msg["sending_state"].is_null() => op.messages.push((chat, mid)),
                    None => {}
                }
                collect_files(&msg["content"], &mut op.files, true);
            }
            if let Some(error) = failed {
                let uncertain = op.incomplete_response;
                drop(state);
                self.fail(store, id, &error, uncertain);
                return None;
            }
            if !op.messages.is_empty() {
                return None;
            }
        } else if op.kind == "downloadFile" {
            collect_files(v, &mut op.files, false);
            // Even a completed file must enter the blob cache before done.
            return None;
        }
        op.status = Status::Done;
        op.changed = Instant::now();
        op.request.take()
    }

    pub fn sent(&self, store: &Store, update: &Value) {
        self.dirty.store(true, Ordering::Relaxed);
        let msg = &update["message"];
        let (Some(chat), Some(old)) = (msg["chat_id"].as_i64(), update["old_message_id"].as_i64())
        else {
            return;
        };
        let error =
            (update["@type"] == "updateMessageSendFailed").then(|| error_text(&update["error"]));
        let mut state = self.state.lock().unwrap();
        // Refresh duplicates and evict by arrival order, never by chat id.
        state.settled.retain(|(key, _)| *key != (chat, old));
        state.settled.push_back(((chat, old), error.clone()));
        while state.settled.len() > 256 {
            state.settled.pop_front();
        }
        let mut failures = Vec::new();
        for op in state
            .operations
            .values_mut()
            .filter(|o| o.messages.contains(&(chat, old)))
        {
            if let Some(error) = &error {
                op.failed_messages.push((chat, old));
                failures.push((op.id, error.clone(), op.incomplete_response));
            } else {
                op.messages.retain(|m| *m != (chat, old));
                if op.messages.is_empty() && !op.incomplete_response {
                    op.status = Status::Done;
                    op.request = None;
                    op.changed = Instant::now();
                }
            }
        }
        let awaiting_response = state
            .operations
            .values()
            .any(|o| o.sending() && o.chat == Some(chat) && o.status == Status::Pending);
        drop(state);
        for (id, error, uncertain) in &failures {
            self.fail(store, *id, error, *uncertain);
        }
        if failures.is_empty() && !awaiting_response {
            if let Some(error) = error {
                self.report(store, "sending message", &error);
            }
        }
    }

    pub fn file_progress(&self, file: &Value) {
        let Some(id) = file["id"].as_i64() else {
            return;
        };
        let mut state = self.state.lock().unwrap();
        for op in state
            .operations
            .values_mut()
            .filter(|o| o.status == Status::Pending)
        {
            if op.files.contains_key(&id)
                || (op.kind == "downloadFile"
                    && op.request.as_ref().is_some_and(|r| r["file_id"] == id))
            {
                let previous = op.files.get(&id).copied();
                let value = progress(file, op.sending());
                op.files.insert(id, value);
                if previous != Some(value) {
                    op.changed = Instant::now();
                    self.dirty.store(true, Ordering::Relaxed);
                }
            }
        }
    }

    pub fn file_finished(&self, store: &Store, file: &Value, error: Option<&str>) {
        self.dirty.store(true, Ordering::Relaxed);
        let Some(id) = file["id"].as_i64() else {
            return;
        };
        let mut state = self.state.lock().unwrap();
        let mut failures = Vec::new();
        for op in state.operations.values_mut().filter(|o| {
            o.kind == "downloadFile" && o.request.as_ref().is_some_and(|r| r["file_id"] == id)
        }) {
            if let Some(error) = error {
                failures.push((op.id, error.to_string()));
            } else {
                op.status = Status::Done;
                op.request = None;
                op.changed = Instant::now();
            }
        }
        drop(state);
        for (id, error) in failures {
            self.fail(store, id, &error, false);
        }
    }

    pub fn expire(&self, store: &Store, now: Instant) {
        let pending: Vec<_> = self
            .list()
            .into_iter()
            .filter(|o| {
                o.status == Status::Pending && now.saturating_duration_since(o.changed) >= PATIENCE
            })
            .collect();
        for op in pending {
            self.fail(
                store,
                op.id,
                if op.sending() {
                    "No delivery confirmation. Check the chat before sending again."
                } else {
                    "Telegram did not respond. Try again."
                },
                op.sending(),
            );
        }
        self.state.lock().unwrap().operations.retain(|_, op| {
            let keep = op.status != Status::Done
                || now.saturating_duration_since(op.changed) < Duration::from_secs(5);
            if !keep {
                self.dirty.store(true, Ordering::Relaxed);
            }
            keep
        });
    }
}

pub fn error_text(v: &Value) -> String {
    let text = v["message"]
        .as_str()
        .unwrap_or("Telegram refused the operation");
    match v["code"].as_i64() {
        Some(code) => format!("{text} ({code})"),
        None => text.to_string(),
    }
}

fn label(v: &Value) -> String {
    match v["@type"].as_str().unwrap_or("") {
        "sendMessage" => {
            let mut paths = Vec::new();
            local_paths(v, &mut paths);
            paths.first().map_or_else(
                || "sending message".to_string(),
                |p| {
                    format!(
                        "sending {}",
                        std::path::Path::new(p)
                            .file_name()
                            .unwrap_or_default()
                            .to_string_lossy()
                    )
                },
            )
        }
        "forwardMessages" => "forwarding messages".into(),
        "resendMessages" => "resending messages".into(),
        "editMessageText" | "editMessageCaption" => "saving edit".into(),
        "deleteMessages" => "deleting messages".into(),
        "setChatNotificationSettings" => "updating notifications".into(),
        "toggleChatIsPinned" => "updating pin".into(),
        "addChatToList" => "moving chat".into(),
        "leaveChat" => "leaving chat".into(),
        "joinChat" => "joining chat".into(),
        "deleteChatHistory" => "deleting history".into(),
        "setChatDraftMessage" => "saving draft".into(),
        "viewMessages" => "marking read".into(),
        "searchChatMessages" => "loading replies and mentions".into(),
        "setTdlibParameters" => "connecting to Telegram".into(),
        "setAuthenticationPhoneNumber"
        | "checkAuthenticationCode"
        | "checkAuthenticationPassword" => "signing in".into(),
        "loadChats" => "loading chats".into(),
        "getChatHistory" => "loading messages".into(),
        "getMessage" => "loading message".into(),
        "getRemoteFile" | "downloadFile" => "downloading media".into(),
        "deleteFile" => "updating media cache".into(),
        other => other.to_string(),
    }
}

pub fn local_paths<'a>(v: &'a Value, out: &mut Vec<&'a str>) {
    if v["@type"] == "inputFileLocal" {
        if let Some(p) = v["path"].as_str() {
            out.push(p);
        }
    } else if let Some(object) = v.as_object() {
        for child in object.values() {
            local_paths(child, out);
        }
    } else if let Some(array) = v.as_array() {
        for child in array {
            local_paths(child, out);
        }
    }
}

/// Validation happens on the worker, before a file request reaches TDLib.
pub fn validate_files(v: &Value) -> Result<(), String> {
    let mut paths = Vec::new();
    local_paths(v, &mut paths);
    for path in paths {
        let metadata = std::fs::metadata(path).map_err(|e| format!("Cannot open {path}: {e}"))?;
        if !metadata.is_file() {
            return Err(format!("{path} is not a regular file"));
        }
        if metadata.len() == 0 {
            return Err(format!("{path} is empty"));
        }
        std::fs::File::open(path).map_err(|e| format!("Cannot read {path}: {e}"))?;
    }
    Ok(())
}

fn progress(v: &Value, upload: bool) -> (u64, u64) {
    let done = if upload {
        &v["remote"]["uploaded_size"]
    } else {
        &v["local"]["downloaded_size"]
    };
    (
        done.as_u64().unwrap_or(0),
        v["size"]
            .as_u64()
            .filter(|n| *n > 0)
            .or_else(|| v["expected_size"].as_u64())
            .unwrap_or(0),
    )
}

fn collect_files(v: &Value, files: &mut BTreeMap<i64, (u64, u64)>, upload: bool) {
    if v["@type"] == "file"
        || (v["id"].is_i64() && v["remote"].is_object() && v["local"].is_object())
    {
        if let Some(id) = v["id"].as_i64() {
            files.insert(id, progress(v, upload));
        }
    } else if let Some(object) = v.as_object() {
        for child in object.values() {
            collect_files(child, files, upload);
        }
    } else if let Some(array) = v.as_array() {
        for child in array {
            collect_files(child, files, upload);
        }
    }
}

pub struct Failures;
impl ProblemSource for Failures {
    fn list(&self, store: &Store) -> Vec<Problem> {
        runtime::of(store)
            .operations
            .list()
            .into_iter()
            .filter_map(|op| {
                let Status::Failed { ref error, .. } = op.status else {
                    return None;
                };
                let id = op.id;
                let mut verbs = Vec::new();
                if op.retryable() {
                    verbs.push(Verb::call("telegram.retry", "retry", None, move |s| {
                        retry(s.store(), id);
                        s.redraw();
                    }));
                }
                if let Some(chat) = op.chat {
                    verbs.push(Verb::go(
                        "telegram.check_chat",
                        "open chat",
                        None,
                        Nav::Open {
                            from: 0,
                            id: super::panels::Chat::id(chat),
                            fresh: false,
                        },
                    ));
                }
                verbs.push(Verb::call("telegram.dismiss", "dismiss", None, move |s| {
                    runtime::of(s.store()).operations.dismiss(id);
                    s.redraw();
                }));
                Some(
                    Problem::new(
                        format!("telegram:request:{id}"),
                        &op.label,
                        error,
                        op.chat.map_or_else(
                            || "Telegram".into(),
                            |chat| {
                                super::model::peer(store, chat)
                                    .map_or_else(|| format!("chat {chat}"), |p| p.name)
                            },
                        ),
                    )
                    .announcing(op.line())
                    .with_verbs(verbs),
                )
            })
            .collect()
    }
}

pub fn retry(store: &Store, id: u64) {
    let rt = runtime::of(store);
    if let Some(request) = rt.operations.retry(id) {
        if !rt.send(&request) {
            rt.operations.fail(
                store,
                id,
                "Telegram is not connected; try again after reconnecting",
                false,
            );
        }
    }
}

/// Run blocking local work outside the UI thread, using the same visible
/// progress and errors as wire operations. Only the runtime and log directory
/// cross threads; the store's reader stays on its owning thread.
pub fn run_local(
    store: &Store,
    chat: Option<PeerId>,
    what: &str,
    run: impl FnOnce() -> Result<(), String> + Send + 'static,
) -> Option<std::thread::JoinHandle<()>> {
    let rt = runtime::of(store);
    let id = {
        let mut state = rt.operations.state.lock().unwrap();
        state.next += 1;
        let id = state.next;
        state.operations.insert(
            id,
            Operation {
                id,
                chat,
                label: what.to_string(),
                kind: what.to_string(),
                status: Status::Pending,
                request: None,
                messages: Vec::new(),
                failed_messages: Vec::new(),
                incomplete_response: false,
                files: BTreeMap::new(),
                changed: Instant::now(),
            },
        );
        id
    };
    rt.operations.changed();
    let worker = rt.clone();
    let dir = store.dir().map(Path::to_path_buf);
    match std::thread::Builder::new()
        .name(format!("telegram-{what}"))
        .spawn(move || {
            match run() {
                Ok(()) => {
                    let mut state = worker.operations.state.lock().unwrap();
                    if let Some(op) = state.operations.get_mut(&id) {
                        op.status = Status::Done;
                        op.changed = Instant::now();
                    }
                }
                Err(error) => worker.operations.fail_at(dir.as_deref(), id, &error, false),
            }
            worker.operations.changed();
        }) {
        Ok(thread) => Some(thread),
        Err(error) => {
            rt.operations.fail(store, id, &error.to_string(), false);
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::super::{model::Carried, requests};
    use super::*;

    fn store() -> Store {
        Store::open(None, &[]).unwrap()
    }
    fn tracked(t: &Tracker, request: String) -> Value {
        serde_json::from_str(&t.track(&request)).unwrap()
    }
    fn refusal(v: &Value) -> Value {
        json!({"@type": "error", "code": 400, "message": "InputFile is not specified", "@extra": v["@extra"]})
    }

    #[test]
    fn rejected_captionless_image_keeps_the_file_and_reply_for_retry() {
        let t = Tracker::default();
        let s = store();
        let req = tracked(
            &t,
            requests::send_file(
                7,
                Some(42),
                &Carried {
                    path: "/tmp/photo.png".into(),
                },
                "",
            ),
        );
        let id = req["@extra"]["operation"].as_u64().unwrap();
        assert!(t.list()[0].line().contains("sending photo.png…"));
        assert!(t.reply(&s, &refusal(&req)).is_none());
        let op = t.list().remove(0);
        assert!(op.line().contains("InputFile is not specified (400)"));
        assert!(op.retryable());
        let retry: Value = serde_json::from_str(&t.retry(id).unwrap()).unwrap();
        assert_eq!(retry["input_message_content"], req["input_message_content"]);
        assert_eq!(retry["reply_to"], req["reply_to"]);
    }

    #[test]
    fn same_caption_sends_are_correlated_individually() {
        let t = Tracker::default();
        let s = store();
        let a = tracked(&t, requests::send_message(7, "same", None));
        let b = tracked(&t, requests::send_message(7, "same", Some(42)));
        assert_ne!(a["@extra"], b["@extra"]);
        t.reply(&s, &refusal(&a));
        assert!(matches!(t.list()[0].status, Status::Failed { .. }));
        assert_eq!(t.list()[1].status, Status::Pending);
    }

    fn pending(req: &Value) -> Value {
        json!({"@type": "message", "chat_id": 7, "id": 100,
            "sending_state": {"@type": "messageSendingStatePending"},
            "content": {"file": {"@type": "file", "id": 5, "size": 1000,
                "local": {}, "remote": {"uploaded_size": 0}}}, "@extra": req["@extra"]})
    }

    #[test]
    fn upload_progress_waits_for_delivery_then_clears_the_payload() {
        let t = Tracker::default();
        let s = store();
        let req = tracked(&t, requests::send_message(7, "hi", None));
        t.reply(&s, &pending(&req));
        t.file_progress(&json!({"id": 5, "size": 1000, "remote": {"uploaded_size": 650}}));
        assert!(t.list()[0].line().contains("65%"));
        assert_eq!(t.list()[0].status, Status::Pending);
        t.sent(
            &s,
            &json!({"@type": "updateMessageSendSucceeded", "old_message_id": 100,
            "message": {"chat_id": 7, "id": 200}}),
        );
        let op = t.list().remove(0);
        assert_eq!(op.status, Status::Done);
        assert!(op.request.is_none());
    }

    #[test]
    fn later_failure_retries_the_tdlib_message_instead_of_creating_a_copy() {
        let t = Tracker::default();
        let s = store();
        let req = tracked(&t, requests::send_message(7, "hi", None));
        t.reply(&s, &pending(&req));
        t.sent(&s, &json!({"@type": "updateMessageSendFailed", "old_message_id": 100,
            "message": {"chat_id": 7, "id": 100}, "error": {"code": 403, "message": "CHAT_WRITE_FORBIDDEN"}}));
        let op = t.list().remove(0);
        assert!(op.line().contains("CHAT_WRITE_FORBIDDEN"));
        let req: Value = serde_json::from_str(&t.retry(op.id).unwrap()).unwrap();
        assert_eq!(req["@type"], "resendMessages");
        assert_eq!(req["message_ids"], json!([100]));
    }

    #[test]
    fn final_update_before_response_does_not_leave_a_send_pending() {
        let t = Tracker::default();
        let s = store();
        let req = tracked(&t, requests::send_message(7, "hi", None));
        t.sent(
            &s,
            &json!({"@type": "updateMessageSendSucceeded", "old_message_id": 100,
            "message": {"chat_id": 7, "id": 200}}),
        );
        t.reply(&s, &pending(&req));
        assert_eq!(t.list()[0].status, Status::Done);
    }

    #[test]
    fn newest_confirmation_survives_a_full_cache_in_a_lower_id_chat() {
        for failed in [false, true] {
            let t = Tracker::default();
            let s = store();
            let chat = -1_000_123_456_789_i64;
            let req = tracked(&t, requests::send_message(chat, "hi", None));
            let mut update = json!({"@type": "updateMessageSendSucceeded", "old_message_id": 100,
                "message": {"chat_id": 7, "id": 200}});
            for id in 0..256 {
                update["old_message_id"] = json!(id);
                t.sent(&s, &update);
            }
            // Repeated updates refresh their age without consuming capacity.
            update["old_message_id"] = json!(0);
            t.sent(&s, &update);
            update["message"]["chat_id"] = json!(chat);
            update["old_message_id"] = json!(100);
            if failed {
                update["@type"] = json!("updateMessageSendFailed");
                update["error"] = json!({"code": 403, "message": "CHAT_WRITE_FORBIDDEN"});
            }
            t.sent(&s, &update);
            {
                let state = t.state.lock().unwrap();
                assert_eq!(state.settled.len(), 256);
                assert!(state.settled.iter().any(|(key, _)| *key == (7, 0)));
                assert!(!state.settled.iter().any(|(key, _)| *key == (7, 1)));
            }
            let mut reply = pending(&req);
            reply["chat_id"] = json!(chat);
            t.reply(&s, &reply);
            let op = t.list().remove(0);
            if failed {
                assert!(op.retryable());
                assert!(op.line().contains("CHAT_WRITE_FORBIDDEN"));
            } else {
                assert_eq!(op.status, Status::Done);
            }
        }
    }

    #[test]
    fn local_work_returns_while_pending_then_reports_its_actual_outcome() {
        use std::sync::mpsc;
        use std::thread;

        let s = store();
        let rt = runtime::of(&s);
        for result in [Ok(()), Err("handler refused the file".to_string())] {
            let (started, observed) = mpsc::channel();
            let (release, wait) = mpsc::channel();
            let outcome = result.clone();
            let worker = run_local(&s, Some(7), "opening media", move || {
                started.send(thread::current().id()).unwrap();
                wait.recv_timeout(Duration::from_secs(5)).unwrap();
                outcome
            })
            .unwrap();
            let thread_id = observed.recv_timeout(Duration::from_secs(5)).unwrap();
            assert_ne!(thread_id, thread::current().id());
            let op = rt.operations.list().pop().unwrap();
            assert_eq!(op.status, Status::Pending);
            assert!(op.line().contains("opening media…"));
            rt.operations.take_changed();
            release.send(()).unwrap();
            worker.join().unwrap();
            assert!(rt.operations.take_changed());
            let op = rt.operations.list().pop().unwrap();
            match result {
                Ok(()) => assert_eq!(op.status, Status::Done),
                Err(error) => {
                    assert_eq!(
                        op.status,
                        Status::Failed {
                            error,
                            uncertain: false
                        }
                    );
                    assert!(
                        !op.retryable(),
                        "local work never goes to the TDLib retry queue"
                    );
                    assert!(Failures.list(&s)[0]
                        .announce
                        .as_ref()
                        .unwrap()
                        .contains("handler refused"));
                }
            }
        }
    }

    #[test]
    fn uncertain_send_times_out_without_offering_a_duplicate_send() {
        let t = Tracker::default();
        let s = store();
        let req = tracked(&t, requests::send_message(7, "hi", None));
        t.expire(&s, Instant::now() + PATIENCE);
        let op = t.list().remove(0);
        assert!(matches!(
            op.status,
            Status::Failed {
                uncertain: true,
                ..
            }
        ));
        assert!(!op.retryable());
        // A late, authoritative answer still settles it.
        let mut reply = pending(&req);
        reply["sending_state"] = Value::Null;
        t.reply(&s, &reply);
        assert_eq!(t.list()[0].status, Status::Done);
    }

    #[test]
    fn login_secrets_are_not_retained_or_added_to_correlation() {
        let t = Tracker::default();
        let req = tracked(&t, requests::check_authentication_password("very-secret"));
        assert!(!req["@extra"].to_string().contains("very-secret"));
        assert!(t.list()[0].request.is_none());
    }

    #[test]
    fn download_finishes_only_after_the_cache_accepts_the_bytes() {
        let t = Tracker::default();
        let s = store();
        let req = tracked(&t, requests::download_file(8, 32));
        let file = json!({"@type": "file", "id": 8, "size": 100,
            "local": {"is_downloading_completed": true, "downloaded_size": 100},
            "@extra": req["@extra"]});
        t.reply(&s, &file);
        assert_eq!(t.list()[0].status, Status::Pending);
        t.file_finished(&s, &file, Some("disk full"));
        assert!(t.list()[0].line().contains("disk full"));
        assert!(t.list()[0].retryable());
    }

    #[test]
    fn absent_files_and_directories_are_rejected_before_upload() {
        let req = |path: String| {
            serde_json::from_str::<Value>(&requests::send_file(7, None, &Carried { path }, ""))
                .unwrap()
        };
        assert!(
            validate_files(&req("/nonexistent/telegram-test.png".into()))
                .unwrap_err()
                .contains("Cannot open")
        );
        assert!(
            validate_files(&req(std::env::temp_dir().to_string_lossy().into_owned()))
                .unwrap_err()
                .contains("regular file")
        );
    }
    #[test]
    fn partial_forward_confirmation_never_retries_already_sent_messages() {
        let t = Tracker::default();
        let s = store();
        let req = tracked(&t, requests::forward_messages(7, 8, &[1, 2]));
        t.reply(&s, &json!({"@type": "messages", "messages": [null, pending(&req)], "@extra": req["@extra"]}));
        assert!(!t.list()[0].retryable());
        t.sent(
            &s,
            &json!({"@type": "updateMessageSendSucceeded", "old_message_id": 100,
            "message": {"chat_id": 7, "id": 200}}),
        );
        assert!(matches!(
            t.list()[0].status,
            Status::Failed {
                uncertain: true,
                ..
            }
        ));
    }

    #[test]
    fn retry_after_disconnection_keeps_the_only_copy_of_the_request() {
        let s = store();
        let rt = runtime::of(&s);
        drop(rt.connect());
        let req = tracked(
            &rt.operations,
            requests::send_message(7, "keep this", Some(42)),
        );
        rt.operations.reply(&s, &refusal(&req));
        let id = rt.operations.list()[0].id;
        super::retry(&s, id);
        let ops = rt.operations.list();
        assert_eq!(ops.len(), 1);
        assert!(ops[0].retryable());
        assert_eq!(
            ops[0].request.as_ref().unwrap()["reply_to"]["message_id"],
            42
        );
    }
}
