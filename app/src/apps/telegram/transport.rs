//! The worker's TDLib seam. `FakeTd` scripts updates and records sends;
//! `RealTd` binds the optional native engine. Panels use neither transport.
#![cfg_attr(not(feature = "tdlib"), allow(dead_code))]

#[cfg(test)]
use std::collections::VecDeque;
#[cfg(any(feature = "tdlib", test))]
use std::sync::{Arc, Mutex};
#[cfg(test)]
use std::sync::MutexGuard;

#[cfg(any(feature = "tdlib", test))]
use tokio::sync::Notify;

/// An asynchronous update stream. The account processes bounded batches in
/// order and awaits readiness between them; sends never wait for a response.
#[async_trait::async_trait(?Send)]
pub trait Td {
    /// Fire one request — a JSON string in TDLib's type language. Fire and
    /// forget: the reply, if any, returns through [`try_receive`](Td::try_receive).
    fn send(&self, request: &str);

    /// The next ready update or response. This call never blocks.
    fn try_receive(&self) -> Option<String>;

    /// Wait without consuming input. Cancellation leaves queued updates
    /// intact, including when a command wakes the worker first.
    async fn ready(&self);
}

// -- the real transport --------------------------------------------------------

/// A live TDLib client owned by the worker: its own id for [`send`](Td::send),
/// an inbox fed by the process-wide native receive bridge.
#[cfg(feature = "tdlib")]
#[derive(Debug)]
pub struct RealTd {
    client: super::tdjson::Client,
    inbound: Mutex<tokio::sync::mpsc::Receiver<String>>,
    ready: Arc<Notify>,
    closing: std::sync::atomic::AtomicBool,
    closed: tokio::sync::watch::Receiver<bool>,
}

#[cfg(feature = "tdlib")]
impl RealTd {
    /// Opens the single native account client. Only the worker calls this.
    #[allow(clippy::new_without_default)]
    #[must_use]
    pub fn new() -> RealTd {
        // Send TDLib's own log to a file rather than the terminal (its
        // default prints two lines on every `td_receive`), at level 2 — errors and warnings,
        // not the info spam. `execute` is synchronous and needs no client.
        if let Some(home) = std::env::var_os("HOME") {
            let path = std::path::Path::new(&home)
                .join("Library/Application Support/superapp/td.log");
            let stream = serde_json::json!({
                "@type": "setLogStream",
                "log_stream": {
                    "@type": "logStreamFile",
                    "path": path.to_string_lossy(),
                    "max_file_size": 10_485_760,
                    "redirect_stderr": false,
                },
            });
            let _ = super::tdjson::execute(&stream.to_string());
        }
        let _ = super::tdjson::execute(
            r#"{"@type":"setLogVerbosityLevel","new_verbosity_level":2}"#,
        );
        let client = super::tdjson::Client::new();
        let (sender, receiver) = tokio::sync::mpsc::channel(512);
        let ready = Arc::new(Notify::new());
        let (closed, closed_rx) = tokio::sync::watch::channel(false);
        dispatcher().lock().expect("TDLib clients")
            .insert(client.id(), Route { sender, ready: ready.clone(), closed });
        let td = RealTd { client, inbound: Mutex::new(receiver), ready,
            closing: std::sync::atomic::AtomicBool::new(false), closed: closed_rx };
        // The client-id interface emits nothing until it has taken a first
        // request: send a harmless one so TDLib draws its initial
        // `updateAuthorizationState`, which is what moves the sign-in off
        // `closed`. Without this the worker drains an empty queue forever and
        // the panel reads "not started".
        td.client
            .send(r#"{"@type":"getOption","name":"version","@extra":"kick"}"#);
        td
    }

    pub(super) fn start_close(&self) {
        if !self.closing.swap(true, std::sync::atomic::Ordering::AcqRel) {
            self.client.send(r#"{"@type":"close"}"#);
        }
    }

    pub(super) fn is_closed(&self) -> bool { *self.closed.borrow() }

    pub(super) fn has_updates(&self) -> bool {
        !self.inbound.lock().expect("TDLib inbox").is_empty()
    }

    pub(super) async fn closed(&self) {
        let mut closed = self.closed.clone();
        let _ = closed.wait_for(|closed| *closed).await;
    }
}

#[cfg(feature = "tdlib")]
#[async_trait::async_trait(?Send)]
impl Td for RealTd {
    fn send(&self, request: &str) {
        self.client.send(request);
    }

    fn try_receive(&self) -> Option<String> {
        self.inbound.lock().expect("TDLib inbox").try_recv().ok()
    }

    async fn ready(&self) {
        if !self.inbound.lock().expect("TDLib inbox").is_empty() { return; }
        self.ready.notified().await;
    }
}

#[cfg(feature = "tdlib")]
impl Drop for RealTd {
    fn drop(&mut self) {
        dispatcher().lock().expect("TDLib clients").remove(&self.client.id());
        // The bridge continues receiving TDLib's close confirmation after
        // this account's Rust receiver is gone.
        self.start_close();
    }
}

#[cfg(feature = "tdlib")]
type Clients = Mutex<std::collections::HashMap<i32, Route>>;

#[cfg(feature = "tdlib")]
#[derive(Clone)]
struct Route {
    sender: tokio::sync::mpsc::Sender<String>,
    ready: Arc<Notify>,
    closed: tokio::sync::watch::Sender<bool>,
}

/// TDLib exposes one blocking receive queue for the whole process. A single
/// native bridge owns it; account tasks await bounded queues on Tokio. Keep
/// responses and updates together: projecting them out of order corrupts
/// message delivery and history state.
#[cfg(feature = "tdlib")]
fn dispatcher() -> &'static Arc<Clients> {
    static CLIENTS: std::sync::OnceLock<Arc<Clients>> = std::sync::OnceLock::new();
    CLIENTS.get_or_init(|| {
        let clients = Arc::new(Clients::default());
        let routes = clients.clone();
        std::thread::Builder::new().name("tdlib-receive".into()).spawn(move || loop {
            let Some(raw) = super::tdjson::receive(60.0) else { continue };
            let Ok(value) = serde_json::from_str::<serde_json::Value>(&raw) else { continue; };
            let closed = value["@type"] == "updateAuthorizationState"
                && value["authorization_state"]["@type"] == "authorizationStateClosed";
            let route = value["@client_id"].as_i64().and_then(|id| i32::try_from(id).ok())
                .and_then(|id| routes.lock().expect("TDLib clients").get(&id).cloned());
            if let Some(route) = route {
                if route.sender.blocking_send(raw).is_ok() { route.ready.notify_one(); }
                // Signal only after the final packet is queued: awaiting
                // closure may then drain every preceding update in order.
                if closed { route.closed.send_replace(true); }
            }
        }).expect("start TDLib receiver");
        clients
    })
}

// -- the fake transport --------------------------------------------------------

/// The offline transport the state-machine tests drive. `Arc<Mutex<..>>` on
/// purpose: a test and the account (or the worker) each hold a clone of the
/// one shared state, so a [`push`](FakeTd::push) on the test's handle is seen
/// by the account's `receive`, and a `send` from the account shows up in the
/// test's [`sent`](FakeTd::sent).
#[cfg(test)]
#[derive(Debug, Clone, Default)]
pub struct FakeTd {
    inner: Arc<Mutex<Inner>>,
    ready: Arc<Notify>,
}

#[cfg(test)]
#[derive(Debug, Default)]
struct Inner {
    /// Scripted updates `receive` pops, front first; `None` once drained.
    inbound: VecDeque<String>,
    /// Every request the account fired, in order.
    sent: Vec<String>,
}

#[cfg(test)]
impl FakeTd {
    #[must_use]
    pub fn new() -> FakeTd {
        FakeTd::default()
    }

    /// Scripts one update the next `receive` will hand over.
    pub fn push(&self, update: impl Into<String>) {
        self.lock().inbound.push_back(update.into());
        self.ready.notify_one();
    }

    /// Every request the account sent, in order — the raw JSON.
    #[must_use]
    pub fn sent(&self) -> Vec<String> {
        self.lock().sent.clone()
    }

    /// The `@type` of each sent request, so an assertion reads the verb it
    /// expected rather than re-parsing the JSON at the call site.
    #[must_use]
    pub fn sent_types(&self) -> Vec<String> {
        self.lock().sent.iter().map(|s| type_of(s)).collect()
    }

    /// A poisoned lock means a panic mid-write; the captured requests are
    /// still readable, and a test learns more from the assertion that follows
    /// than from a second panic here.
    fn lock(&self) -> MutexGuard<'_, Inner> {
        self.inner.lock().unwrap_or_else(|e| e.into_inner())
    }
}

#[cfg(test)]
#[async_trait::async_trait(?Send)]
impl Td for FakeTd {
    fn send(&self, request: &str) {
        self.lock().sent.push(request.to_string());
    }

    fn try_receive(&self) -> Option<String> {
        self.lock().inbound.pop_front()
    }

    async fn ready(&self) {
        if !self.lock().inbound.is_empty() { return; }
        self.ready.notified().await;
    }
}

/// The `@type` of a request or update, or `""` when it carries none — a fake
/// helper for [`sent_types`](FakeTd::sent_types), so a test asserts on the
/// verb.
#[cfg(test)]
fn type_of(json: &str) -> String {
    serde_json::from_str::<serde_json::Value>(json)
        .ok()
        .and_then(|v| v["@type"].as_str().map(str::to_string))
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test(start_paused = true)]
    async fn cancelling_readiness_keeps_updates_in_wire_order() {
        let td = FakeTd::new();
        // A worker can lose select! to a command while waiting for TDLib.
        tokio::select! {
            _ = td.ready() => panic!("an empty transport is not ready"),
            _ = tokio::task::yield_now() => {},
        }
        td.push("first response");
        td.push("following update");
        tokio::time::timeout(std::time::Duration::from_secs(1), td.ready()).await.unwrap();
        assert_eq!(td.try_receive().as_deref(), Some("first response"));
        assert_eq!(td.try_receive().as_deref(), Some("following update"));
        assert!(td.try_receive().is_none());
    }

    #[cfg(feature = "tdlib")]
    #[tokio::test]
    async fn a_native_client_closes_without_account_credentials() {
        let td = RealTd::new();
        td.start_close();
        tokio::time::timeout(std::time::Duration::from_secs(10), td.closed()).await
            .expect("TDLib acknowledged closing its unauthenticated client");
        assert!(td.is_closed());
        let mut last = None;
        while let Some(update) = td.try_receive() { last = Some(update); }
        let final_update: serde_json::Value = serde_json::from_str(&last.unwrap()).unwrap();
        assert_eq!(final_update["authorization_state"]["@type"], "authorizationStateClosed");
    }
}
