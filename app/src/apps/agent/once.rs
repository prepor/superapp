//! One question to the model, with no chat and no panel.
//!
//! A chat is the app's whole shape — a row, a run, a worker, a transcript —
//! and there are questions that want none of it: what a word means, what a
//! sentence should have said. [`Agent::ask_once`] is that question. A system
//! line and one user line go out as an ordinary [`ChatRequest`] with no
//! tools; the text of the assembled message comes back. Nothing is written
//! down, nothing is retried, and nobody watches the stream.
//!
//! It goes through the one door all the same. [`Ask`] is an in-memory
//! effect, as [`Complete`](super::run::Complete) is: a request costs money,
//! so the effects log shows every one of them with its sentence and its
//! error — *ask the model once: …* — beside the mail reads.
//!
//! [`json_object`] is the other half of asking once: a question whose
//! answer is a shape rather than prose asks for JSON, and a model that was
//! asked for JSON wraps it in a sentence or a fence about half the time.

use kernel::effect::{AsyncEffect as Effect, Ctx, World};
use serde_json::Value;

use super::gateway::{Flow, Gateway};
use super::wire::{ChatRequest, Chunk, Message};
use super::Agent;

/// How much of a question the effects log shows before the ellipsis.
const DESCRIBED: usize = 60;

/// One request to the model outside any chat: a system line, one question,
/// no tools.
pub struct Ask {
    pub model: String,
    pub system: String,
    pub user: String,
}

impl Ask {
    /// What goes out. Streamed like every other request — the stream is the
    /// gateway's one shape, and the fake answers it the same way — but with
    /// no tools on it and no reasoning effort: a one-shot question is asked
    /// while somebody waits, and it has nothing to call.
    fn request(&self) -> ChatRequest {
        ChatRequest::new(
            self.model.clone(),
            vec![
                Message::system(self.system.clone()),
                Message::user(self.user.clone()),
            ],
        )
    }
}

#[async_trait::async_trait(?Send)]
impl Effect for Ask {
    const KIND: &'static str = "ask once";
    type Reply = String;

    fn describe(&self) -> String {
        format!("ask the model once: {}", line(&self.user))
    }

    /// A request costs money, so the log's `@wrote` view should show it —
    /// the same answer a chat's request gives.
    fn writes(&self) -> bool {
        true
    }

    async fn perform(&self, cx: &mut Ctx<'_>) -> Result<String, String> {
        let req = self.request();
        let gateway = cx.cap::<dyn Gateway>()?;
        // Nothing is drawn while this one is in flight, so every chunk is
        // waved through and the assembled message is the whole answer.
        let mut on = |_: &Chunk| Flow::Go;
        let done = gateway
            .complete(&req, &mut on)
            .await
            .map_err(|failure| failure.message)?;
        Ok(done.message.text().to_string())
    }
}

/// One line of a question, short enough for a row.
fn line(text: &str) -> String {
    let one = text.split_whitespace().collect::<Vec<_>>().join(" ");
    match one.char_indices().nth(DESCRIBED) {
        Some((at, _)) => format!("{}…", &one[..at]),
        None => one,
    }
}

impl Agent {
    /// One question to the model, answered in words.
    ///
    /// `world` is whichever world the caller is on — a worker's, where the
    /// question was asked from a panel through
    /// [`prepare_work`](kernel::session::Session::prepare_work), so the
    /// person carries on while it is out. The gateway is that world's: the
    /// real one on a window's own run, the scripted fake in every test,
    /// every suite and every library mount.
    ///
    /// # Errors
    ///
    /// If this world has no gateway — a build with the agent app denied its
    /// outside — or if the request never came to an answer.
    pub async fn ask_once(
        world: &World,
        model: &str,
        system: &str,
        user: &str,
    ) -> Result<String, String> {
        world
            .run_async(&Ask {
                model: model.to_string(),
                system: system.to_string(),
                user: user.to_string(),
            })
            .await
    }
}

/// The first `{ … }` of a reply, read as JSON.
///
/// A model asked for one object answers with one object, a fenced one, or
/// one with a sentence in front of it. All three are the same object, so
/// this is what a caller that wanted a shape reads: the first brace, its
/// match — counted, with strings and their escapes skipped, so a `}` inside
/// a value ends nothing — and `None` where what is between them is not
/// JSON.
#[must_use]
pub fn json_object(text: &str) -> Option<Value> {
    let start = text.find('{')?;
    let mut depth = 0usize;
    let mut in_string = false;
    let mut escaped = false;
    for (at, c) in text[start..].char_indices() {
        if in_string {
            match c {
                _ if escaped => escaped = false,
                '\\' => escaped = true,
                '"' => in_string = false,
                _ => {}
            }
            continue;
        }
        match c {
            '"' => in_string = true,
            '{' => depth += 1,
            '}' => {
                depth = depth.checked_sub(1)?;
                if depth == 0 {
                    let end = start + at + c.len_utf8();
                    return serde_json::from_str(&text[start..end]).ok();
                }
            }
            _ => {}
        }
    }
    None
}
