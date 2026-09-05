//! What the model is told: the system prompt, the tools, and the turns.
//!
//! One function, and nothing in it changes between two requests of the same
//! chat — no clock, no counts, no ids of the moment. That is deliberate:
//! whatever the model and the gateway cache of a repeated prefix, they
//! cache without being asked, and this wire has no `cache_control` to
//! place.
//!
//! The order is the plan's: what superapp is, how the store is reached and
//! why an app's own tool beats a write, each app's data in its own words,
//! the panel in context if there is one, and a short word about style. No
//! chords, no picture of the workspace — the model acts through tools, not
//! through the keyboard.

use kernel::tool::Tool;

use super::model::{Chat, Turn};
use super::wire::{ChatRequest, Message, ToolDef};
use super::REASONING_EFFORT;

/// One request for this chat, as it stands.
///
/// `describes` is each app's data dictionary, by app id, in app-list order;
/// `context` is the panel the chat is looking at, already rendered, or
/// `None` while there is none.
#[must_use]
pub fn request(
    chat: &Chat,
    turns: &[Turn],
    tools: &[Tool],
    describes: &[(&str, &str)],
    context: Option<&str>,
) -> ChatRequest {
    let mut messages = vec![Message::system(system(describes, context))];
    messages.extend(turns.iter().map(|t| t.message.clone()));
    let mut req = ChatRequest::new(chat.model.clone(), messages);
    req.tools = tools.iter().map(ToolDef::from).collect();
    req.reasoning_effort = Some(REASONING_EFFORT.to_string());
    req
}

/// The system prompt.
fn system(describes: &[(&str, &str)], context: Option<&str>) -> String {
    let mut p = String::new();
    p.push_str(PREAMBLE);
    if !describes.is_empty() {
        p.push_str("\n\n## the apps' data\n");
        for (id, describe) in describes {
            p.push_str(&format!("\n### {id}\n\n{}\n", describe.trim()));
        }
    }
    if let Some(panel) = context {
        p.push_str("\n\n## what the person is looking at\n\n");
        p.push_str(panel.trim());
        p.push('\n');
    }
    p.push_str("\n\n");
    p.push_str(STYLE);
    p
}

/// What superapp is, and how it is reached.
const PREAMBLE: &str = "\
You are the assistant inside superapp: one person's workspace, where mail, \
files and everything else are panels over a single SQLite database on their \
own machine. You are talking to that person, in a chat panel beside the \
rest of their work.

Everything here is rows in that one store. `sql.query` reads it and \
`sql.write` writes it, and both are yours. Prefer an app's own tool wherever \
there is one: a tool is the same code the person's own button runs, so it \
keeps what the app promises — sending a letter is an outbox row the mail app \
files, never an INSERT — and a bare write cannot. The schema is `sql.schema` \
when you need more than the summary below.

Every act of yours is an ordinary undoable action: the person takes it back \
with one chord, so most calls simply run. The few that cannot be undone — \
sending a letter, deleting, a bare write — wait for the person's word first, \
and one they will not have answers `refused by the person`. So do what was \
asked, and say plainly what you did and what you could not.";

/// How to answer.
const STYLE: &str = "\
## how to answer

Answer in the language the person wrote in. Name things by what a person \
calls them — a conversation, a letter, a file — rather than by row id. Say \
what you did and what can be undone. Keep it short: this is a panel in a \
workspace, not a page.";
