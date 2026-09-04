//! One conversation: the transcript above, the composer below.
//!
//! The panel is the run's hands. The worker asks the model and writes what
//! comes back, but a call the model asked for runs *here*, on the UI thread
//! with the session — so a chat shown nowhere pauses at its next call, and
//! picks up when it is opened again. The widget calls
//! [`run_pending_calls`](super::super::calls::run_pending_calls) on every
//! event while the run is waiting.
//!
//! A panel opened as `chat(new)` has no row behind it yet: the first send
//! makes the chat and then replaces the slot with `chat(<id>)`, so the slot
//! and the saved session name the real conversation rather than the blank
//! one it started as.

use std::any::Any;
use std::rc::Rc;

use kernel::layout::SlotId;
use kernel::nav::Nav;
use kernel::panel::{Opening, Panel, PanelId, PanelKind, Tag, Verb};
use kernel::session::Session;
use kernel::store::Store;

use super::super::model::{self, ChatId, Run, Turn};
use super::Agents;

/// The argument a chat panel carries when there is no row behind it yet.
const NEW: &str = "new";

/// One chat, open.
pub struct Chat {
    id: PanelId,
    store: Rc<Store>,
    slot: SlotId,
    /// The conversation this panel is of; `None` until the first send makes
    /// one.
    chat: Option<ChatId>,
    /// The composer's text. The widget mirrors its field into it on every
    /// edit, as the compose sheet does with a draft — but nothing is
    /// written down: an unsent message is not a row.
    draft: String,
}

impl Chat {
    pub const TAG: Tag = Tag("chat");

    /// The identity of one conversation's panel.
    #[must_use]
    pub fn id(chat: ChatId) -> PanelId {
        PanelId::new(Self::TAG, [chat.to_string()])
    }

    /// The identity of a chat nobody has said anything in. A root, and what
    /// *new* opens.
    #[must_use]
    pub fn new_id() -> PanelId {
        PanelId::new(Self::TAG, [NEW])
    }

    /// The conversation a `chat` panel is of; `None` for a blank one and
    /// for any other tag.
    #[must_use]
    pub fn of(id: &PanelId) -> Option<ChatId> {
        (id.tag == Self::TAG)
            .then(|| id.arg(0)?.parse().ok())
            .flatten()
    }

    /// The conversation this panel is of.
    #[must_use]
    pub fn chat(&self) -> Option<ChatId> {
        self.chat
    }

    /// What the composer holds.
    #[must_use]
    pub fn draft(&self) -> &str {
        &self.draft
    }

    /// The composer changed. Not an action — typing is the future editor's
    /// local undo, not the workspace's — and not a row either.
    pub fn set_draft(&mut self, text: &str) {
        if self.draft != text {
            self.draft = text.to_string();
        }
    }

    /// The newest round of the agent in this chat, whatever it is doing —
    /// its id, its status, its error and what it cost.
    #[must_use]
    pub fn latest_run(&self) -> Option<Run> {
        self.chat.and_then(|c| model::latest_run(&self.store, c))
    }

    /// That run's word — `streaming`, `waiting`, `failed` — or `None` in a
    /// chat nobody has sent in.
    #[must_use]
    pub fn status(&self) -> Option<String> {
        self.latest_run().map(|r| r.status)
    }

    /// The transcript, in order.
    #[must_use]
    pub fn turns(&self) -> Rc<Vec<Turn>> {
        match self.chat {
            Some(c) => model::turns(&self.store, c),
            None => Rc::new(Vec::new()),
        }
    }

    /// What the round this turn came out of cost, as the muted line under
    /// it: *2.1k in (1.9k cached), 310 out*. `None` while the round has not
    /// said, which is every round until its last chunk.
    #[must_use]
    pub fn usage_line(&self, turn: &Turn) -> Option<String> {
        let usage = model::run(&self.store, turn.run?)?.usage?;
        let cached = if usage.cached > 0 {
            format!(" ({} cached)", tokens(usage.cached))
        } else {
            String::new()
        };
        Some(format!(
            "{} in{cached}, {} out",
            tokens(usage.input),
            tokens(usage.output)
        ))
    }

    /// Sends what is in the composer.
    ///
    /// A blank chat's send makes the row and then replaces the slot with
    /// the real conversation's identity, so the layout the session saves
    /// names it. A refused write leaves the words in the field: they are
    /// the only copy.
    pub fn send(&mut self, s: &mut Session) {
        let said = std::mem::take(&mut self.draft);
        let Some((chat, _)) = model::send(s, self.chat, &said) else {
            self.draft = said;
            return;
        };
        let was_blank = self.chat.is_none();
        self.chat = Some(chat);
        if was_blank {
            // Folded into the send's own node: opening the conversation one
            // has just started is the send arriving at its consequence, not
            // a second gesture.
            s.nav_within(Nav::Replace {
                slot: self.slot,
                id: Chat::id(chat),
            });
        }
    }
}

impl Panel for Chat {
    fn id(&self) -> &PanelId {
        &self.id
    }

    /// The chat's own title — the first line of the first thing said in it
    /// — and `chat` before anything has been.
    fn title(&self) -> String {
        self.chat
            .and_then(|c| model::chat(&self.store, c))
            .map(|c| c.title)
            .filter(|t| !t.trim().is_empty())
            .unwrap_or_else(|| model::UNTITLED.to_string())
    }

    fn about(&self) -> String {
        let n = self.turns().len();
        let model_name = self
            .chat
            .and_then(|c| model::chat(&self.store, c))
            .map_or_else(|| super::super::MODEL.to_string(), |c| c.model);
        format!(
            "{}: one conversation with the assistant, running on {model_name}, \
             {n} turn{} so far. The person writes at the foot and the agent answers \
             above; the agent reads and changes this workspace through the tools \
             this build offers, and every act of its own is an ordinary undoable \
             action.",
            self.title(),
            if n == 1 { "" } else { "s" }
        )
    }

    /// Six by six: a conversation wants a column of its own and room to
    /// read one.
    fn wish(&self, _cols: usize) -> (u32, u32) {
        (6, 6)
    }

    fn placed(&mut self, slot: SlotId) {
        self.slot = slot;
    }

    /// *send* while there is something to send and nothing going, *stop*
    /// while something is, *retry* on a round that came to nothing — then
    /// the two that are always there: a fresh chat, and the list.
    fn verbs(&self) -> Vec<Verb> {
        let run = self.latest_run();
        let going = run.as_ref().is_some_and(Run::live);
        let mut v = Vec::new();
        if !going && !self.draft.trim().is_empty() {
            v.push(Verb::run("agent.send", "send", Some('s')));
        }
        if going {
            v.push(Verb::run("agent.stop", "stop", Some('k')));
        }
        if run
            .as_ref()
            .is_some_and(|r| matches!(r.status.as_str(), model::FAILED | model::STOPPED))
        {
            v.push(Verb::run("agent.retry", "retry", Some('r')));
        }
        v.push(Verb::go(
            "agent.new",
            "new",
            Some('n'),
            Nav::Open {
                from: self.slot,
                id: Chat::new_id(),
                fresh: true,
            },
        ));
        // The one link on this bar, and it wears no letter: the composer is
        // a text field, and a letter here would take a chord out of it.
        v.push(Verb::go(
            "agent.agents",
            "agents",
            None,
            Nav::Replace {
                slot: self.slot,
                id: Agents::id(),
            },
        ));
        v
    }

    fn run(&mut self, verb: &str, s: &mut Session) {
        match verb {
            "agent.send" => self.send(s),
            "agent.stop" => {
                if let Some(r) = self.latest_run().filter(Run::live) {
                    model::stop(s, r.id);
                }
            }
            "agent.retry" => {
                if let Some(chat) = self.chat {
                    model::retry(s, chat);
                }
            }
            _ => {}
        }
    }

    fn as_any(&mut self) -> &mut dyn Any {
        self
    }
}

/// Its factory.
pub struct ChatKind;

impl PanelKind for ChatKind {
    fn tag(&self) -> Tag {
        Chat::TAG
    }

    fn open(&self, id: &PanelId, cx: &mut Opening<'_>) -> Box<dyn Panel> {
        Box::new(Chat {
            chat: Chat::of(id),
            id: id.clone(),
            store: cx.session().store().clone(),
            slot: 0,
            draft: String::new(),
        })
    }
}

/// A token count as the muted line says it: plain under a thousand, and one
/// decimal of a thousand above.
fn tokens(n: u64) -> String {
    if n < 1000 {
        return n.to_string();
    }
    #[allow(clippy::cast_precision_loss)]
    let k = n as f64 / 1000.0;
    format!("{k:.1}k")
}
