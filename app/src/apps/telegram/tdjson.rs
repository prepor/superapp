//! Safe wrappers for TDLib's modern JSON C interface, behind `tdlib`.
//!
//! TDLib owns the returned C strings until the next call on the same thread;
//! these wrappers copy them before returning. `receive` serves a process-wide
//! queue, so this build admits only one real account worker.

use std::ffi::{c_char, c_double, c_int, CStr, CString};

// The modern JSON interface (`td/telegram/td_json_client.h`). We bind these
// four and nothing else — the deprecated `td_json_client_*` calls are not
// ours. `td_receive`/`td_execute` return a C string TDLib owns only until the
// next call on the same thread, so the wrappers below copy it at once.
extern "C" {
    fn td_create_client_id() -> c_int;
    fn td_send(client_id: c_int, request: *const c_char);
    fn td_receive(timeout: c_double) -> *const c_char;
    fn td_execute(request: *const c_char) -> *const c_char;
}

/// A handle on one TDLib client. The id is a plain int the library mints, so a
/// `Client` is as cheap as the int it wraps — the weight is all on the far
/// side of the wire. The receive side is deliberately not a method here:
/// `td_receive` drains one queue shared across every client in the process, so
/// it is the free [`receive`] function, pumped from a single thread.
#[derive(Debug, Clone, Copy)]
pub struct Client {
    id: i32,
}

impl Client {
    /// Open a client. `td_create_client_id` mints an id and nothing more — no
    /// socket opens until the first [`send`](Client::send) — so this is cheap
    /// and never blocks. Its answers arrive on the shared queue, so pair it
    /// with [`receive`], not a method on the handle.
    // A client is opened through the library, never defaulted: a `Default`
    // that silently minted one over FFI would be a footgun, so waive the lint
    // rather than grow a misleading impl.
    #[allow(clippy::new_without_default)]
    #[must_use]
    pub fn new() -> Client {
        // SAFETY: td_create_client_id takes no arguments and returns a plain
        // int; there is nothing to pass wrong and nothing to free.
        Client {
            id: unsafe { td_create_client_id() },
        }
    }

    /// Send one request — a JSON string in TDLib's type language — to this
    /// client. Fire and forget: the reply, if any, comes back through
    /// [`receive`] carrying the `@extra` this request set.
    pub fn send(&self, request: &str) {
        let Ok(req) = CString::new(request) else {
            // A request with an interior NUL cannot be a C string. Valid JSON
            // in TDLib's type language never carries one, so this is a caller
            // bug, not a wire state — drop it rather than truncate or panic.
            return;
        };
        // SAFETY: `req` is a NUL-terminated C string that outlives the call;
        // td_send borrows it for the duration and copies what it keeps.
        unsafe { td_send(self.id, req.as_ptr()) };
    }
}

/// Pump the shared queue: the next update or response TDLib has ready, or
/// `None` when `timeout` seconds pass with nothing. There is one queue for the
/// whole process, so this is a free function and it must be pumped from a
/// single thread — the worker's — never two at once.
#[must_use]
pub fn receive(timeout: f64) -> Option<String> {
    // SAFETY: td_receive returns NULL or a C string TDLib owns only until the
    // next td_receive/td_execute on this thread; ptr_to_string copies it into
    // an owned String at once, before this thread touches the queue again.
    unsafe { ptr_to_string(td_receive(timeout)) }
}

/// Run a synchronous, network-free request — `getTextEntities`, `getOption`
/// and the like — and get its answer at once, or `None` for a null return or
/// an input that cannot be a C string. Thread-safe and account-free, so it is
/// the call the offline test leans on.
#[must_use]
pub fn execute(request: &str) -> Option<String> {
    let req = CString::new(request).ok()?;
    // SAFETY: `req` is a valid NUL-terminated C string alive across the call;
    // td_execute returns NULL or a string owned only until the next call on
    // this thread, which ptr_to_string copies at once.
    unsafe { ptr_to_string(td_execute(req.as_ptr())) }
}

/// Copy a C string TDLib just returned into an owned `String`, `None` for
/// NULL. In one place because both [`receive`] and [`execute`] get a pointer
/// valid only until the next call — the copy has to happen here and now,
/// before anything else on the thread touches the queue.
///
/// # Safety
/// `ptr` is NULL or a valid NUL-terminated C string that stays live for the
/// duration of this call.
unsafe fn ptr_to_string(ptr: *const c_char) -> Option<String> {
    if ptr.is_null() {
        return None;
    }
    // SAFETY: non-null and NUL-terminated per the contract above; TDLib's
    // output is UTF-8 JSON, and the lossy read owns its bytes before we return
    // so nothing outlives the pointer's borrow.
    Some(CStr::from_ptr(ptr).to_string_lossy().into_owned())
}

#[cfg(all(test, feature = "tdlib"))]
mod tests {
    use super::*;

    /// The binding links and calls. `getTextEntities` is TDLib's synchronous,
    /// fully-local parse — no account, no socket — so an answer proves
    /// [`execute`] reaches the library and returns. We check the shape only;
    /// the parse itself is TDLib's to be right about.
    #[test]
    fn execute_parses_text_entities_offline() {
        let req = r#"{"@type":"getTextEntities","text":"@telegram /start https://telegram.org","@extra":"probe"}"#;
        let out = execute(req).expect("td_execute returned a string");
        assert!(
            out.contains(r#""@type":"textEntities""#),
            "unexpected reply: {out}"
        );
        // And it is real JSON, not just a substring match.
        let v: serde_json::Value = serde_json::from_str(&out).expect("reply parses as JSON");
        assert_eq!(v["@type"], "textEntities");
    }

    /// Probe the linked library's JSON schema without opening an account.
    /// Its parser must reach the nested InputFile path in our actual builder:
    /// replacing that string with an array must produce a parse error. The
    /// old, flat media request ignores that path on modern TDLib and fails
    /// this test. No send is executed by the synchronous API.
    #[test]
    fn media_builders_match_the_native_json_decoder() {
        use crate::apps::telegram::{model::Carried, requests};
        use serde_json::{json, Value};
        fn invalidate_paths(v: &mut Value) {
            if v["@type"] == "inputFileLocal" {
                v["path"] = json!([]);
            } else if let Some(object) = v.as_object_mut() {
                for child in object.values_mut() {
                    invalidate_paths(child);
                }
            }
        }
        for path in [
            "/tmp/probe.png",
            "/tmp/probe.jpg",
            "/tmp/probe.mp4",
            "/tmp/probe.gif",
            "/tmp/probe.mp3",
            "/tmp/probe.pdf",
        ] {
            let request = requests::send_file(0, None, &Carried { path: path.into() }, "");
            let mut request: Value = serde_json::from_str(&request).unwrap();
            invalidate_paths(&mut request);
            let reply: Value =
                serde_json::from_str(&execute(&request.to_string()).unwrap()).unwrap();
            assert!(
                reply["message"]
                    .as_str()
                    .unwrap_or("")
                    .contains("Expected String"),
                "{path}: {reply}"
            );
        }
    }

    /// A freshly minted client id is a small positive int — the library made
    /// it, no network touched.
    #[test]
    fn new_client_has_a_positive_id() {
        assert!(Client::new().id > 0);
    }
}
