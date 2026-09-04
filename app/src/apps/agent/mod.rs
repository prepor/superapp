//! The agent app: a chat over the store, with the apps as its hands.
//!
//! An agent acts only through what a person could do, and every one of its
//! acts is an ordinary undoable action. That rule is what the app is built
//! around: a chat is rows like any other, a tool is a verb's own code path
//! over ids, and `cmd+z` is the answer to a call that should not have run.
//!
//! This phase is the floor, and nothing draws yet. What is here is the
//! outside of a chat: the [`wire`] types the chat-completions API speaks,
//! the [`Gateway`] capability the model sits behind, the scripted
//! [`FakeGateway`] every test and every suite gets instead, and the consts
//! that say which model answers and where it is. Phase 1 adds the schema,
//! the two panels, the worker and the token; phase 3 adds the tools, which
//! is when [`App::tools`](kernel::app::App::tools) here stops being empty.

// Until a chat draws, the tests are the only caller of any of this: the app
// registers a capability and no panel, and `apps` is a private module, so
// every type below reads as dead to a build with no `cfg(test)`. Phase 1
// takes this line out with the first widget that uses them.
#![allow(dead_code)]

use std::any::Any;

use kernel::app::{App, Capabilities, Env, Mode};
use kernel::panel::PanelKind;

pub mod fake;
pub mod gateway;
pub mod wire;

#[cfg(test)]
mod fixtures;
#[cfg(test)]
mod tests;

pub use fake::FakeGateway;
pub use gateway::{Gateway, Provider};

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

/// The app.
pub struct Agent;

/// The one in this build.
pub static AGENT: Agent = Agent;

impl App for Agent {
    fn id(&self) -> &'static str {
        "agent"
    }

    /// None yet: phase 1 registers `chat`, over one conversation, and
    /// `agents`, the list of them.
    fn kinds(&self) -> &'static [&'static dyn PanelKind] {
        &[]
    }

    /// The model, for one world.
    ///
    /// A scripted run, a test and a library mount get [`FakeGateway`],
    /// registered under the trait *and* under its own type, so a test can
    /// reach `get::<FakeGateway>()` to plant a script or read what the
    /// model was told.
    fn outside(&self, mode: Mode, _env: &Env, caps: &mut Capabilities) {
        match mode {
            // phase 0 follow-up: the real gateway over `kernel::http` and
            // `kernel::sse`, behind `PROVIDER`, with the Cloudflare token
            // device sync already holds. Until it lands a real run has no
            // gateway, and a request through one fails in words.
            Mode::Real | Mode::Deny => {}
            Mode::Fake => {
                let fake = FakeGateway::default_script();
                caps.insert::<dyn Gateway>(Box::new(fake.clone()));
                caps.insert::<FakeGateway>(Box::new(fake));
            }
        }
    }

    fn as_any(&self) -> &dyn Any {
        self
    }
}
