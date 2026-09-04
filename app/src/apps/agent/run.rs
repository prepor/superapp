//! The request as an effect, and the live tail it streams into.
//!
//! [`Complete`] is in-memory and never [`Deferred`](kernel::effect::Deferred):
//! a request costs money, nobody would retry one blindly, and the run's own
//! row is its state. It goes through the one door all the same, so the
//! effect log shows every request with its sentence and its error beside
//! the mail reads, and says `writes: true` because a request costs
//! something.
//!
//! What arrives before the answer is whole lives here, on the app's static,
//! and not in a row: a token a row would be a thousand writes a turn. The
//! chat panel draws [`Agent::tail`] under the last turn while the run is
//! going, and the engine rings [`Agent::wake`] after every chunk so the
//! frame that draws it happens.

use std::sync::Arc;

use kernel::effect::{Ctx, Effect};

use super::gateway::{Flow, Gateway};
use super::model::{self, ChatId, RunId};
use super::prompt;
use super::wire::{Chunk, Completion};
use super::AGENT;

/// What has arrived of an answer that is still being written.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Tail {
    pub text: String,
    /// The model's own reasoning, where it sends any: drawn folded and
    /// muted.
    pub reasoning: String,
    /// Moves on every chunk, so a widget can tell "nothing new" from "the
    /// same words again" without comparing strings.
    pub version: u64,
}

impl super::Agent {
    /// What has arrived of this run's answer, or `None` once it is over —
    /// the tail is cleared when the turn becomes a row.
    ///
    /// # Panics
    ///
    /// If a previous holder panicked with the tails locked.
    #[must_use]
    pub fn tail(&self, run: RunId) -> Option<Tail> {
        self.tails
            .lock()
            .expect("the agent's tails")
            .get(&run)
            .cloned()
    }

    /// One chunk's worth, added.
    ///
    /// # Panics
    ///
    /// As [`Agent::tail`](super::Agent::tail).
    pub(super) fn append(&self, run: RunId, text: &str, reasoning: &str) {
        let mut tails = self.tails.lock().expect("the agent's tails");
        let tail = tails.entry(run).or_default();
        tail.text.push_str(text);
        tail.reasoning.push_str(reasoning);
        tail.version += 1;
    }

    /// The run is over: what it was writing is a row now, or it is nothing.
    ///
    /// # Panics
    ///
    /// As [`Agent::tail`](super::Agent::tail).
    pub(super) fn clear_tail(&self, run: RunId) {
        self.tails.lock().expect("the agent's tails").remove(&run);
    }

    /// How the engine asks for a frame. The shell's half sets this to
    /// makepad's `SignalToUI::set_ui_signal`, the way the kernel's workers
    /// wake the window; a build with no window — a test, a library mount —
    /// leaves it unset and nothing is woken.
    ///
    /// # Panics
    ///
    /// If a previous holder panicked with the hook locked.
    pub fn set_wake(&self, f: impl Fn() + Send + Sync + 'static) {
        *self.wake.lock().expect("the agent's wake hook") = Some(Arc::new(f));
    }

    /// Rings it. Called after every chunk, from whatever thread the run is
    /// on.
    ///
    /// # Panics
    ///
    /// As [`Agent::set_wake`](super::Agent::set_wake).
    pub fn wake(&self) {
        // Cloned out and the lock let go before the call: a hook that asks
        // for a frame has no business waiting on this mutex, and a hook
        // that rang back into it would deadlock.
        let hook = self.wake.lock().expect("the agent's wake hook").clone();
        if let Some(f) = hook {
            f();
        }
    }

    /// What this build offers an agent, and what each app says its data is
    /// — copied out of the registry in
    /// [`App::attach`](kernel::app::App::attach), because a request is
    /// built on a worker thread with no registry in reach.
    ///
    /// # Panics
    ///
    /// If a previous holder panicked with the registry locked.
    pub(super) fn learn(
        &self,
        tools: Vec<kernel::tool::Tool>,
        describes: Vec<(&'static str, &'static str)>,
    ) {
        *self.tools.lock().expect("the agent's tools") = tools;
        *self.describes.lock().expect("the agent's describes") = describes;
    }

    /// The tools every request carries.
    ///
    /// # Panics
    ///
    /// As [`Agent::learn`](super::Agent::learn).
    #[must_use]
    pub fn tools(&self) -> Vec<kernel::tool::Tool> {
        self.tools.lock().expect("the agent's tools").clone()
    }

    /// Each app's data in its own words, by app id, in app-list order.
    ///
    /// # Panics
    ///
    /// As [`Agent::learn`](super::Agent::learn).
    #[must_use]
    pub fn describes(&self) -> Vec<(&'static str, &'static str)> {
        self.describes
            .lock()
            .expect("the agent's describes")
            .clone()
    }
}

/// One request to the model, for one run.
///
/// The run is what it is about; the other two are for the log's sentence
/// alone, because an effect's [`describe`](Effect::describe) has no store
/// to look them up in.
pub struct Complete {
    pub run: RunId,
    pub chat: ChatId,
    /// Which turn of the chat this answer will be.
    pub turn: i64,
}

impl Effect for Complete {
    const KIND: &'static str = "complete";
    type Reply = Completion;

    fn describe(&self) -> String {
        format!("ask the model for chat {}, turn {}", self.chat, self.turn)
    }

    /// A request costs money, so the log's `@wrote` view should show it.
    fn writes(&self) -> bool {
        true
    }

    fn entity(&self) -> Option<String> {
        Some(model::run_entity(self.run))
    }

    /// Reads the chat and its turns off the reader it is handed, builds the
    /// request, and streams the answer into the live tail.
    ///
    /// The failure is the [`Failure`](super::gateway::Failure)'s **sentence**
    /// and not its display form: the run's `error` is what the problem
    /// source reads, and it keys on a `gateway: ` at the front of it.
    fn perform(&self, cx: &mut Ctx<'_>) -> Result<Completion, String> {
        // Copied out before the capability bag is borrowed: the stream's
        // callback reads the run's status per chunk, and `cap` wants `cx`
        // to itself.
        let db = cx.db;
        let chat =
            model::chat_conn(db, self.chat).ok_or_else(|| format!("chat {} is gone", self.chat))?;
        let turns = model::turns_conn(db, self.chat);
        let tools = AGENT.tools();
        let describes = AGENT.describes();
        let req = prompt::request(&chat, &turns, &tools, &describes, None);
        let run = self.run;
        let gateway = cx.cap::<dyn Gateway>()?;
        let mut on = |chunk: &Chunk| {
            let (text, reasoning) = deltas(chunk);
            AGENT.append(run, &text, &reasoning);
            AGENT.wake();
            // One cheap read a chunk: *stop* is the run's status, and it
            // cuts the stream at the next one rather than at some safe
            // point later.
            if model::is_stopped(db, run) {
                Flow::Stop
            } else {
                Flow::Go
            }
        };
        gateway.complete(&req, &mut on).map_err(|f| f.message)
    }
}

/// What one chunk adds: its text, and its reasoning.
fn deltas(chunk: &Chunk) -> (String, String) {
    let mut text = String::new();
    let mut reasoning = String::new();
    for choice in &chunk.choices {
        if let Some(t) = &choice.delta.content {
            text.push_str(t);
        }
        if let Some(t) = &choice.delta.reasoning_content {
            reasoning.push_str(t);
        }
    }
    (text, reasoning)
}
