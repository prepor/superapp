//! The seam between the worker loop and TDLib: two calls, one trait, and a
//! fake that stands in for the native library so the login logic is proven
//! offline.
//!
//! The per-account loop needs exactly two things of the engine — fire a
//! request, and pump the shared queue for the next update. That is the whole
//! of [`Td`]. `execute` — TDLib's synchronous, network-free calls — needs no
//! client and no state, so it stays the [`tdjson`](super::tdjson) free
//! function and is not part of this seam.
//!
//! [`RealTd`] wraps a live client and is compiled only when the `tdlib`
//! feature links the library. [`FakeTd`] is always compiled: it is what makes
//! the authorization state machine testable with no native dependency — a
//! scripted inbound queue `receive` pops, and every `send` recorded for a
//! test to read back.
//!
//! Nothing in this build calls the module yet: the phase-3c worker is its
//! driver and the tests stand in until then, so its items are allowed to read
//! as unused rather than be wired to a caller that does not exist.
#![allow(dead_code)]

use std::collections::VecDeque;
use std::sync::{Arc, Mutex, MutexGuard};

/// The two calls the worker loop makes on the engine. Object-safe, but the
/// account and the worker are generic over it rather than boxed — a real
/// transport is a plain int and a fake is an `Arc`, so there is nothing an
/// allocation would buy.
pub trait Td {
    /// Fire one request — a JSON string in TDLib's type language. Fire and
    /// forget: the reply, if any, returns through [`receive`](Td::receive).
    fn send(&self, request: &str);

    /// The next update or response the engine has ready, or `None` when
    /// `timeout` seconds pass with nothing. The loop calls it with `0.0` and
    /// drains until it answers `None`.
    fn receive(&self, timeout: f64) -> Option<String>;
}

// -- the real transport --------------------------------------------------------

/// A live TDLib client: its own id for [`send`](Td::send), the process-wide
/// queue for [`receive`](Td::receive). As cheap as the int it wraps — the
/// weight is all on the far side of the wire — so it clones freely.
#[cfg(feature = "tdlib")]
#[derive(Debug, Clone, Copy)]
pub struct RealTd {
    client: super::tdjson::Client,
}

#[cfg(feature = "tdlib")]
impl RealTd {
    /// Opens a client. `td_create_client_id` mints an id and nothing more;
    /// the first [`receive`](Td::receive) after it draws TDLib's first
    /// `updateAuthorizationState`, which is what starts the sign-in. The
    /// client is filed in [`SHARED`] as it opens, so the sign-in UI — on the
    /// window thread, not the worker's — sends the account holder's phone,
    /// code and password to the very client the worker's loop drives.
    // A transport is opened through the library, never defaulted — a
    // `Default` that silently minted a client over FFI would be a footgun, so
    // the lint is waived rather than a misleading impl grown, as
    // [`tdjson::Client`](super::tdjson::Client) waives it for the same reason.
    #[allow(clippy::new_without_default)]
    #[must_use]
    pub fn new() -> RealTd {
        // Send TDLib's own log to a file rather than the terminal (its
        // default prints two lines on every `td_receive`, and the worker
        // polls several times a second), at level 2 — errors and warnings,
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
        let td = RealTd {
            client: super::tdjson::Client::new(),
        };
        // The client-id interface emits nothing until it has taken a first
        // request: send a harmless one so TDLib draws its initial
        // `updateAuthorizationState`, which is what moves the sign-in off
        // `closed`. Without this the worker drains an empty queue forever and
        // the panel reads "not started".
        td.client
            .send(r#"{"@type":"getOption","name":"version","@extra":"kick"}"#);
        // Set once: the first client opened is the one the UI answers to. A
        // single account opens exactly one, so a later `set` that finds the
        // cell full is never the wrong client, only the same account again.
        let _ = SHARED.set(td);
        td
    }
}

/// The one account's live transport, filed the moment its client opens. The
/// worker owns the receive side on its own thread; this is the send side the
/// sign-in panel needs, so an answer the account holder types reaches the
/// client the worker is driving. A [`RealTd`] is a `Copy` client id and
/// `td_send` is thread-safe, so the cell hands back a copy and the window
/// thread sends on it directly.
#[cfg(feature = "tdlib")]
static SHARED: std::sync::OnceLock<RealTd> = std::sync::OnceLock::new();

/// The shared transport onto the one account's client, or `None` before the
/// worker has opened it. The sign-in panel sends the phone, code and password
/// through this; with no client open there is nothing yet to send to, and the
/// panel says so rather than guessing.
#[cfg(feature = "tdlib")]
#[must_use]
pub fn shared() -> Option<RealTd> {
    SHARED.get().copied()
}

#[cfg(feature = "tdlib")]
impl Td for RealTd {
    fn send(&self, request: &str) {
        self.client.send(request);
    }

    fn receive(&self, timeout: f64) -> Option<String> {
        super::tdjson::receive(timeout)
    }
}

// -- the fake transport --------------------------------------------------------

/// The offline transport the state-machine tests drive. `Arc<Mutex<..>>` on
/// purpose: a test and the account (or the worker) each hold a clone of the
/// one shared state, so a [`push`](FakeTd::push) on the test's handle is seen
/// by the account's `receive`, and a `send` from the account shows up in the
/// test's [`sent`](FakeTd::sent).
#[derive(Debug, Clone, Default)]
pub struct FakeTd {
    inner: Arc<Mutex<Inner>>,
}

#[derive(Debug, Default)]
struct Inner {
    /// Scripted updates `receive` pops, front first; `None` once drained.
    inbound: VecDeque<String>,
    /// Every request the account fired, in order.
    sent: Vec<String>,
}

impl FakeTd {
    #[must_use]
    pub fn new() -> FakeTd {
        FakeTd::default()
    }

    /// Scripts one update the next `receive` will hand over.
    pub fn push(&self, update: impl Into<String>) {
        self.lock().inbound.push_back(update.into());
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

impl Td for FakeTd {
    fn send(&self, request: &str) {
        self.lock().sent.push(request.to_string());
    }

    /// The timeout is ignored: a fake cannot block, so it answers what it has
    /// and `None` the moment it is empty — which is exactly the drain
    /// condition the loop reads.
    fn receive(&self, _timeout: f64) -> Option<String> {
        self.lock().inbound.pop_front()
    }
}

/// The `@type` of a request or update, or `""` when it carries none — a fake
/// helper for [`sent_types`](FakeTd::sent_types), so a test asserts on the
/// verb.
fn type_of(json: &str) -> String {
    serde_json::from_str::<serde_json::Value>(json)
        .ok()
        .and_then(|v| v["@type"].as_str().map(str::to_string))
        .unwrap_or_default()
}
