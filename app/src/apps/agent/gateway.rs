//! The model behind a chat, as a capability — and the two pure halves of
//! reaching one: what a request is made of, and how a stream is read.
//!
//! An app defines its own capabilities and supplies them in
//! [`App::outside`](kernel::app::App::outside). The agent's is [`Gateway`]:
//! one implementation per world — the real gateway on a window's run, the
//! scripted [`FakeGateway`](super::FakeGateway) everywhere else — so no
//! test, no suite and no library mount ever reaches a network.
//!
//! Between the trait and the socket sit two functions with no world in
//! them. [`request_parts`] makes the URL, the headers and the body a
//! request goes out as, so what this app sends is testable without sending
//! it; [`stream_completion`] is the loop over already-framed events, so the
//! reading of a stream is testable without one. What is left for the real
//! gateway — a follow-up to this phase, over `kernel::http` and
//! `kernel::sse` — is the connection between them.

use std::fmt;

use super::wire::{Assembler, ChatRequest, Chunk, Completion};

/// Whether a stream goes on. What the chat panel answers with while a run
/// is going: *stop* is the run's status, read between events, and it cuts
/// the stream at its next chunk rather than at some safe point later.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Flow {
    Go,
    Stop,
}

/// A request that did not come to an answer, in words a person reads.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Failure {
    /// The HTTP status, where there was one — a 401 is the token, a 403 the
    /// account, and the run's row should say which.
    pub status: Option<u16>,
    pub message: String,
}

impl Failure {
    /// A failure with nothing but its sentence: a stream that broke, a
    /// chunk that would not parse, a stop.
    #[must_use]
    pub fn new(message: impl Into<String>) -> Failure {
        Failure {
            status: None,
            message: message.into(),
        }
    }
}

impl fmt::Display for Failure {
    /// The status leads when there is one, because that is the word the
    /// problem row keys on.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.status {
            Some(code) => write!(f, "{code}: {}", self.message),
            None => f.write_str(&self.message),
        }
    }
}

/// The model behind a chat. One implementation per world: the real gateway
/// on a window's run, the scripted fake everywhere else.
pub trait Gateway {
    /// One chat-completions request, streamed. `on` is called per chunk as
    /// it arrives and answers whether to go on, which is how *stop* cuts a
    /// stream at its next chunk; the answer is the assembled message —
    /// text, `tool_calls`, `finish_reason`, `usage` — or the failure in
    /// words. Never retries: the gateway retries, and the run's row is what
    /// a person retries from.
    ///
    /// # Errors
    ///
    /// If the request never got through, if the gateway refused it, if the
    /// stream broke or said it failed, or if the caller stopped it.
    fn complete(
        &mut self,
        req: &ChatRequest,
        on: &mut dyn FnMut(&Chunk) -> Flow,
    ) -> Result<Completion, Failure>;
}

/// Where requests go and what answers them. One const in the app; a second
/// provider — Cloudflare's REST route, or another model on the same wire —
/// is a second const behind the same capability, not a second module.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Provider {
    /// What the gateway calls it in its own logs and in its URL.
    pub name: &'static str,
    /// The path under the gateway's URL, leading slash and all.
    pub path: &'static str,
    pub model: &'static str,
    pub reasoning_effort: &'static str,
}

/// One request, made ready to send: everything the transport needs and
/// nothing it has to decide.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Parts {
    pub url: String,
    pub headers: Vec<(&'static str, String)>,
    pub body: Vec<u8>,
}

/// What a request goes out as, with the app's logging policy read from the
/// environment.
///
/// `SUPERAPP_AGENT_LOG_PAYLOAD=1` is the agent's one environment knob, read
/// by the app itself as mail reads its own: argv belongs to the shell.
#[must_use]
pub fn request_parts(
    p: &Provider,
    account: &str,
    gateway: &str,
    token: &str,
    req: &ChatRequest,
) -> Parts {
    request_parts_with(p, account, gateway, token, req, log_payload())
}

/// The same, with the policy said — which is what a test says.
///
/// One Cloudflare token opens both doors: it is the provider's key
/// (`authorization`) and the gateway's (`cf-aig-authorization`) at once, so
/// there is nothing to store in the gateway and nothing to alias.
///
/// `cf-aig-collect-log-payload: false` is the app's own policy: a chat
/// carries the person's mail, and the gateway keeps counts and status, not
/// prose. With `log_payload` the header is left off and the gateway's own
/// setting decides.
#[must_use]
pub fn request_parts_with(
    p: &Provider,
    account: &str,
    gateway: &str,
    token: &str,
    req: &ChatRequest,
    log_payload: bool,
) -> Parts {
    let bearer = format!("Bearer {token}");
    let mut headers = vec![
        ("authorization", bearer.clone()),
        ("cf-aig-authorization", bearer),
        ("content-type", "application/json".to_string()),
        ("accept", "text/event-stream".to_string()),
    ];
    if !log_payload {
        headers.push(("cf-aig-collect-log-payload", "false".to_string()));
    }
    Parts {
        url: format!(
            "https://gateway.ai.cloudflare.com/v1/{account}/{gateway}{}",
            p.path
        ),
        headers,
        // The request is strings and schemas: there is nothing in it
        // serde can refuse.
        body: serde_json::to_vec(req).unwrap_or_default(),
    }
}

/// Whether this run asked the gateway to keep what it sends.
fn log_payload() -> bool {
    std::env::var("SUPERAPP_AGENT_LOG_PAYLOAD").is_ok_and(|v| v.trim() == "1")
}

/// One stream, read to its answer.
///
/// The events are already framed — `kernel::sse` hands the data of one
/// event at a time — so this is the whole of what a gateway does with a
/// body: parse each event as a [`Chunk`], add it to the [`Assembler`], and
/// ask the caller whether to go on.
///
/// # Errors
///
/// If the stream broke, if an event is not a chunk (quoted, because a
/// gateway that answers HTML to a bad account should say so in the run's
/// row), if a chunk carries the gateway's own error, if the caller stopped
/// it, or if the assembled call's arguments are not JSON.
pub fn stream_completion(
    events: impl Iterator<Item = std::io::Result<String>>,
    on: &mut dyn FnMut(&Chunk) -> Flow,
) -> Result<Completion, Failure> {
    let mut assembler = Assembler::new();
    for event in events {
        let event = event.map_err(|e| Failure::new(format!("the stream broke: {e}")))?;
        let data = event.trim();
        // The stream's own full stop, and the blank events around it.
        if data.is_empty() || data == "[DONE]" {
            continue;
        }
        let chunk: Chunk = serde_json::from_str(data)
            .map_err(|e| Failure::new(format!("the gateway sent no chunk — {e}: {data}")))?;
        assembler.push(&chunk).map_err(Failure::new)?;
        if on(&chunk) == Flow::Stop {
            return Err(Failure::new("stopped"));
        }
    }
    assembler.finish().map_err(Failure::new)
}
