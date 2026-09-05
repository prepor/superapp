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

use super::super::calls;
use super::super::chip::Chip;
use super::super::model::{self, Call, Carried, ChatId, Run, Turn};

/// The argument a chat panel carries when there is no row behind it yet.
const NEW: &str = "new";

/// What *continue* says, as the person's own turn. A word rather than a
/// flag on the request: the wire has nothing for *finish what you were
/// saying*, and a model that reads its own cut answer above this needs
/// nothing more.
const CONTINUE: &str = "Continue.";

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
    /// What the composer is carrying beside its words. They go with the
    /// next send and leave the composer with it; like the draft, they are
    /// not a row until then.
    chips: Vec<Chip>,
    /// The *add panel* field, while it is up: what has been typed into it.
    /// `None` is the field put away, which is where it is until the verb
    /// asks for it.
    picking: Option<String>,
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

    /// The context the composer is holding, in the order it was given.
    #[must_use]
    pub fn chips(&self) -> &[Chip] {
        &self.chips
    }

    /// One more — unless this panel is already carrying it. A chip is a
    /// reference, and two references to one panel say the same thing twice;
    /// which of them is which is settled by what a turn would keep of them,
    /// so no kind of chip is named here.
    pub fn add_chip(&mut self, chip: Chip) {
        let json = chip.to_json();
        if self.chips.iter().any(|c| c.to_json() == json) {
            return;
        }
        self.chips.push(chip);
    }

    /// The `i`-th taken off — the × on a pill.
    pub fn remove_chip(&mut self, i: usize) {
        if i < self.chips.len() {
            self.chips.remove(i);
        }
    }

    /// The *add panel* field's text, while it is up; `None` while it is
    /// away. The widget mirrors its own field into it, the way the composer
    /// mirrors the draft.
    #[must_use]
    pub fn picking(&self) -> Option<&str> {
        self.picking.as_deref()
    }

    /// Raises the field, changes what is in it, or — with `None` — puts it
    /// away. `esc` and a pick both put it away.
    pub fn set_picking(&mut self, text: Option<&str>) {
        self.picking = text.map(ToString::to_string);
    }

    /// The panels a pick is offered: every slot on every workspace by its
    /// title, this chat left out — asking a chat about itself is a mirror,
    /// and the chip it would make is the one thing already in the room.
    #[must_use]
    pub fn pickable(&self, s: &Session) -> Vec<(SlotId, String)> {
        s.panels()
            .into_iter()
            .filter(|(slot, _)| *slot != self.slot)
            .map(|(slot, inst)| {
                let title = inst.borrow().title();
                (slot, title)
            })
            .filter(|(_, title)| !title.trim().is_empty())
            .collect()
    }

    /// The slot a typed line names, matched against the titles as they
    /// read: the whole title first, and then the one title that begins with
    /// what was typed, so *inb* is the inbox where nothing else starts that
    /// way.
    #[must_use]
    pub fn pick(&self, s: &Session, typed: &str) -> Option<SlotId> {
        let typed = typed.trim().to_lowercase();
        if typed.is_empty() {
            return None;
        }
        let open = self.pickable(s);
        open.iter()
            .find(|(_, t)| t.to_lowercase() == typed)
            .or_else(|| {
                open.iter()
                    .find(|(_, t)| t.to_lowercase().starts_with(&typed))
            })
            .map(|(slot, _)| *slot)
    }

    /// A pick taken: the panel's chip into the composer, and the field away.
    /// Answers whether one was found — a spelling that names nothing leaves
    /// the field where it is.
    pub fn add_panel(&mut self, s: &Session, typed: &str) -> bool {
        let Some(slot) = self.pick(s, typed) else {
            return false;
        };
        let Some(chip) = Chip::panel(s, slot) else {
            return false;
        };
        self.add_chip(chip);
        self.picking = None;
        true
    }

    /// Whether the last thing said in this chat is an answer the model ran
    /// out of room for — which is what puts *continue* on the bar.
    #[must_use]
    pub fn cut_short(&self) -> bool {
        self.turns()
            .last()
            .is_some_and(|t| t.finish.as_deref() == Some("length"))
    }

    /// *continue*: the person asking for the rest, as the person's own turn,
    /// so a cut answer is finished by a round like any other rather than by
    /// a second kind of request.
    pub fn carry_on(&mut self, s: &mut Session) {
        let Some(chat) = self.chat else {
            return;
        };
        model::send(s, Some(chat), CONTINUE, Carried::default());
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

    /// The call standing at its card waiting for the person's word, if the
    /// round has one: a tool that cannot be undone does not run until it is
    /// allowed, and the calls behind it wait with it. The first, because
    /// the walk stops at one.
    #[must_use]
    pub fn asked_call(&self) -> Option<Call> {
        // Only while the round is still waiting for it: a run the person
        // stopped strands whatever it was holding, and a bar must not offer
        // a word that would do nothing.
        let run = self.latest_run().filter(|r| r.status == model::WAITING)?;
        model::asked_calls(&self.store, run.id).into_iter().next()
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

    /// Sends what is in the composer: the words, and the chips rendered for
    /// the model as they stand now.
    ///
    /// A blank chat's send makes the row and then replaces the slot with
    /// the real conversation's identity, so the layout the session saves
    /// names it. A refused write leaves the words in the field: they are
    /// the only copy.
    pub fn send(&mut self, s: &mut Session) {
        let said = std::mem::take(&mut self.draft);
        let chips = std::mem::take(&mut self.chips);
        let carried = Carried {
            chips: chips.iter().map(Chip::to_json).collect(),
            context: (!chips.is_empty()).then(|| {
                chips
                    .iter()
                    .map(|c| c.render(s))
                    .collect::<Vec<_>>()
                    .join("\n")
            }),
        };
        let Some((chat, _)) = model::send(s, self.chat, &said, carried) else {
            self.draft = said;
            self.chips = chips;
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
        // A call standing at its card is the one thing about this panel a
        // reader has to act on, so it is said and named.
        let asked = self.asked_call().map_or_else(String::new, |c| {
            format!(
                " A call of {} is waiting for the person's word before it runs: \
                 the card wears *allow* and *refuse*, and the round stands still \
                 until one of them is pressed.",
                c.tool
            )
        });
        format!(
            "{}: one conversation with the assistant, running on {model_name}, \
             {n} turn{} so far. The person writes at the foot and the agent answers \
             above; the agent reads and changes this workspace through the tools \
             this build offers, and every act of its own is an ordinary undoable \
             action.{asked}",
            self.title(),
            if n == 1 { "" } else { "s" }
        )
    }

    /// Four by six: a third of the desktop's width reads as a conversation
    /// should, and leaves room for what the chat opens beside it.
    fn wish(&self, _cols: usize) -> (u32, u32) {
        (4, 6)
    }

    fn placed(&mut self, slot: SlotId) {
        self.slot = slot;
    }

    /// *send* while there is something to send and nothing going, *stop*
    /// while something is, *retry* on a round that came to nothing,
    /// *continue* on an answer that ran out of room, *allow* and *refuse*
    /// while a call is waiting to be one or the other — then *add panel*,
    /// the one that is always there. A fresh chat and the list of them are
    /// the agents panel's business, not a conversation's.
    fn verbs(&self) -> Vec<Verb> {
        let run = self.latest_run();
        let going = run.as_ref().is_some_and(Run::live);
        let mut v = Vec::new();
        if !going && (!self.draft.trim().is_empty() || !self.chips.is_empty()) {
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
        if !going && self.cut_short() {
            v.push(Verb::run("agent.continue", "continue", Some('o')));
        }
        // The word on a call that cannot be undone. *allow* wears no
        // letter: `a` is the caret's own select-all, `l` and `w` are the
        // workspace's, and the composer is live nearly always — so a letter
        // of that word would be a promise the bar could not keep.
        //
        // Closures rather than `Run` verbs: allowing a call runs the tool,
        // and a tool reaches every panel there is — files' own refresh
        // borrows each of them — while `Panel::run` still holds this one.
        if let (Some(chat), Some(call)) = (self.chat, self.asked_call()) {
            let id = call.id;
            v.push(Verb::call("agent.allow", "allow", None, move |s| {
                calls::allow(s, chat, id);
            }));
            v.push(Verb::call("agent.refuse", "refuse", Some('f'), move |s| {
                calls::refuse(s, chat, id);
            }));
        }
        // The phone's way into the context a chord opens on the desktop, and
        // harmless where the chord is there: a field over the panels that
        // are open, one pick apiece.
        v.push(Verb::run("agent.add_panel", "add panel", Some('p')));
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
            "agent.continue" => self.carry_on(s),
            "agent.add_panel" => self.picking = Some(String::new()),
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
            // What `cmd+shift+a` left for it: the panel it was opened
            // about, offered on the app's own static because a navigation
            // carries an identity and nothing else.
            chips: super::super::AGENT.take_offered().into_iter().collect(),
            picking: None,
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
