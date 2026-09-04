//! The agent app: a chat over the store, with the apps as its hands.
//!
//! An agent acts only through what a person could do, and every one of its
//! acts is an ordinary undoable action. That rule is what the app is built
//! around: a chat is rows like any other, a tool is a verb's own code path
//! over ids, and `cmd+z` is the answer to a call that should not have run.
//!
//! **The store is the bus.** A run has three parties on three threads — the
//! person in the chat panel, the [`worker`] talking to the gateway, and the
//! one writer every write goes through — and they meet in [`model`]'s rows.
//! A person sends; the worker asks the model and streams the answer into
//! the live tail on this app's own static; where the model asks for tools,
//! the worker files a call row apiece and sleeps, and the chat panel runs
//! them ([`calls`]) and kicks it awake. Nothing in the kernel schedules
//! anything for this app.
//!
//! What draws is two panels — a conversation and the list of them — and
//! what leaves the process is one in-memory effect, [`run::Complete`],
//! behind one capability, [`Gateway`]: the real one over `kernel::http` on
//! a window's own run, the scripted [`FakeGateway`] in every test, every
//! suite and every library mount.

use std::any::Any;
use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

use kernel::app::{App, Apps, Capabilities, Env, Mode, ProblemSource, Root, Schema, Worker};
use kernel::layout::SlotId;
use kernel::nav::Nav;
use kernel::panel::PanelKind;
use kernel::session::Session;
use kernel::store::Store;
use kernel::tool::Tool;

use chip::Chip;

pub mod calls;
pub mod chip;
pub mod completion;
pub mod fake;
pub mod gateway;
pub mod model;
pub mod panels;
pub mod problems;
pub mod prompt;
pub mod real;
pub mod run;
pub mod scenes;
pub mod schema;
pub mod ui;
pub mod widgets;
pub mod wire;
pub mod worker;

#[cfg(test)]
mod fixtures;
#[cfg(test)]
mod tests;

pub use fake::FakeGateway;
pub use gateway::{Gateway, Provider};
pub use model::RunId;
pub use panels::{Agents, Chat};
pub use real::RealGateway;
pub use ui::UI;

/// The model behind every chat here: one of the models Cloudflare hosts
/// itself, which the gateway reaches as the `workers-ai` provider. A
/// million tokens of context, function calling, streaming and a reasoning
/// mode, priced so that a day of chatting over one's own mail costs cents.
/// Changing it is a commit, not a setting; a chat's row records the model
/// it ran on, so a chat that predates the change still says what answered
/// it.
pub const MODEL: &str = "@cf/zai-org/glm-5.3-flash";

/// How hard the model thinks before it answers. Beside the model, and a
/// commit for the same reason.
pub const REASONING_EFFORT: &str = "medium";

/// The gateway's name, made once in the Cloudflare dashboard. Where it is —
/// the account — is read off the R2 bucket's host, so a device that syncs
/// has a gateway and one that does not has neither.
pub const GATEWAY: &str = "superapp";

/// Where requests go. A second provider — Cloudflare's REST route, or
/// another model on the same wire — is a second const behind the same
/// capability, not a second module.
pub static PROVIDER: Provider = Provider {
    name: "workers-ai",
    path: "/workers-ai/v1/chat/completions",
    model: MODEL,
    reasoning_effort: REASONING_EFFORT,
};

/// The app, and the little it keeps in memory.
///
/// Four things, and each is here because a row would be the wrong place for
/// it. A **tail** is what has arrived of an answer still being written: a
/// token a row would be a thousand writes a turn. The **wake** hook is the
/// shell's way of asking for a frame, which the kernel has no word for. The
/// **tools** and the **describes** are copied out of the registry in
/// [`App::attach`], because a request is built on a worker's thread with no
/// registry in reach. And the **offered** chip is what `cmd+shift+a` leaves
/// for the chat it opens, a navigation carrying an identity and nothing
/// else.
pub struct Agent {
    /// What is arriving, per run in flight. Ordered by run id, which costs
    /// nothing and is what a `static` can be built with.
    tails: Mutex<BTreeMap<RunId, run::Tail>>,
    tools: Mutex<Vec<Tool>>,
    describes: Mutex<Vec<(&'static str, &'static str)>>,
    wake: Mutex<Option<Arc<dyn Fn() + Send + Sync>>>,
    offered: Mutex<Option<Chip>>,
}

/// The one in this build.
pub static AGENT: Agent = Agent {
    tails: Mutex::new(BTreeMap::new()),
    tools: Mutex::new(Vec::new()),
    describes: Mutex::new(Vec::new()),
    wake: Mutex::new(None),
    offered: Mutex::new(None),
};

static CHAT_KIND: panels::ChatKind = panels::ChatKind;
static AGENTS_KIND: panels::AgentsKind = panels::AgentsKind;
static KINDS: &[&dyn PanelKind] = &[&CHAT_KIND, &AGENTS_KIND];

static GATEWAY_PROBLEMS: problems::GatewayProblems = problems::GatewayProblems;
static SOURCES: &[&dyn ProblemSource] = &[&GATEWAY_PROBLEMS];

impl App for Agent {
    fn id(&self) -> &'static str {
        "agent"
    }

    fn kinds(&self) -> &'static [&'static dyn PanelKind] {
        KINDS
    }

    fn schema(&self) -> Option<&'static Schema> {
        Some(&schema::SCHEMA)
    }

    /// The list, and a blank sheet: the two ways into a chat, in the order
    /// the launcher offers them.
    fn roots(&self) -> Vec<Root> {
        vec![
            Root::new(Agents::id(), "agents", "chats assistant model"),
            Root::new(Chat::new_id(), "new chat", "ask assistant agent chat"),
        ]
    }

    /// The model, for one world.
    ///
    /// A window's own run, replaying nothing and on the wall clock, reaches
    /// the real gateway; everything else — a scripted run, a test, a
    /// library mount — gets [`FakeGateway`], registered under the trait
    /// *and* under its own type, so a test can reach `get::<FakeGateway>()`
    /// to plant a script or read what the model was told.
    fn outside(&self, mode: Mode, env: &Env, caps: &mut Capabilities) {
        if mode == Mode::Deny {
            return;
        }
        if mode == Mode::Real && !env.scripted && !env.clock.is_virtual() {
            caps.insert::<dyn Gateway>(Box::new(RealGateway::new(env)));
            return;
        }
        let fake = FakeGateway::default_script();
        caps.insert::<dyn Gateway>(Box::new(fake.clone()));
        caps.insert::<FakeGateway>(Box::new(fake));
    }

    /// `cmd+shift+a`, taken: a chat that carries the panel as context.
    ///
    /// In a chat already, the chord adds the chip of the panel the chat is
    /// joined from — asking about what one is looking at, from where one is
    /// asking. Anywhere else it opens a chat joined to the panel, with that
    /// panel's chip in its composer.
    fn ask(&self, s: &mut Session, about: SlotId) -> bool {
        if self.add_to_chat(s, about) {
            return true;
        }
        let Some(chip) = Chip::panel(s, about) else {
            return false;
        };
        self.offer(chip);
        s.nav(Nav::Open {
            from: about,
            id: Chat::new_id(),
            fresh: false,
        });
        true
    }

    fn problems(&self) -> &'static [&'static dyn ProblemSource] {
        SOURCES
    }

    fn workers(&self, store: &Store) -> Vec<Box<dyn Worker>> {
        worker::workers(store)
    }

    /// What every request carries: the tools this build offers, and each
    /// app's data in its own words. Copied here, at the one moment the
    /// finished registry exists, because the thread that builds a request
    /// has no registry to ask.
    fn attach(&self, apps: &Apps) {
        let (tools, describes) = Agent::registry_of(apps);
        self.learn(tools, describes);
    }

    fn as_any(&self) -> &dyn Any {
        self
    }
}

impl Agent {
    /// The chip `cmd+shift+a` made, kept until the chat it opened takes it.
    /// One at a time: the chord opens one chat, and the tick that opens it
    /// takes the offer with it.
    ///
    /// # Panics
    ///
    /// If a previous holder panicked with the offer locked.
    pub fn offer(&self, chip: Chip) {
        *self.offered.lock().expect("the agent's offered chip") = Some(chip);
    }

    /// The offer, taken — so a chat opened by any other road starts empty.
    ///
    /// # Panics
    ///
    /// As [`Agent::offer`].
    #[must_use]
    pub fn take_offered(&self) -> Option<Chip> {
        self.offered.lock().expect("the agent's offered chip").take()
    }

    /// The chord inside a chat: the panel the chat is joined from, added to
    /// its composer. Answers whether this slot was a chat at all.
    fn add_to_chat(&self, s: &mut Session, about: SlotId) -> bool {
        let Some(inst) = s.panel(about) else {
            return false;
        };
        if inst.borrow_mut().as_any().downcast_mut::<Chat>().is_none() {
            return false;
        }
        // Made before the chat is borrowed to write: a chip reads the panel
        // it points at, and this walk starts at one that is open.
        let chip = s.join_parent_of(about).and_then(|p| Chip::panel(s, p));
        match chip {
            Some(chip) => {
                if let Some(c) = inst.borrow_mut().as_any().downcast_mut::<Chat>() {
                    c.add_chip(chip);
                }
                s.redraw();
            }
            // A chat standing on its own has no panel to speak of, and says
            // so rather than opening a second chat about itself.
            None => s.notify("this chat is joined to nothing", false),
        }
        true
    }

    /// What a request carries, read off the finished registry: every tool
    /// this build offers, in app-list order, and each app's data in its own
    /// words under the app's id — apps that describe nothing say nothing.
    ///
    /// Split out of [`App::attach`] so it can be read without the static:
    /// the copy is the app's, and a test that wanted to prove the copy
    /// would otherwise be racing every other test that builds a registry.
    #[must_use]
    fn registry_of(apps: &Apps) -> (Vec<Tool>, Vec<(&'static str, &'static str)>) {
        let describes = apps
            .list()
            .iter()
            .filter_map(|a| Some((a.id(), a.describe()?)))
            .collect();
        (apps.tools().to_vec(), describes)
    }
}
