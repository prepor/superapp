//! The agent's entries for the panels library.
//!
//! Every node is a live chat over the scripted gateway — the same
//! [`FakeGateway`](super::FakeGateway) every suite and every test gets — so a
//! node is a conversation actually held rather than a fixture posed to look
//! like one: the script types a message, the answer arrives through the
//! assembler, and what the picture shows is what the widget drew for it.
//!
//! Which answer comes back is chosen by a word in what is typed, which is why
//! each of these states is one line of script and no setup at all: *fail* is
//! refused, *cut* runs out of room, *rename* asks for a tool, and *delete*
//! asks for one that waits to be allowed.
//!
//! The list is the other way round — its rows are seeded through the scene's
//! own `open`, because two chats with two different rounds behind them are
//! rows, and holding two conversations to get them would be a slower way of
//! writing the same four inserts.

use kernel::scene::Scene;
use kernel::store::Store;
use kernel::time::ts;

use crate::shell::app_ui::Setup;
use crate::shell::catalog::{panel_fake, workspace_on};

use super::model;
use super::panels::{Agents, Chat};
use super::MODEL;

/// The agent's scenes, in canvas order.
#[must_use]
pub fn scenes() -> Vec<Scene<Setup>> {
    vec![chat(), agents()]
}

/// One conversation, in each of the states a round can leave it.
fn chat() -> Scene<Setup> {
    let said = |script: &str| panel_fake(|_| Chat::new_id(), script);
    // The caret first: a mount is not the window's own stage, so the
    // composer is given the keyboard by a press the way a person would.
    let ask = |words: &str| {
        format!("click \"ask\"\nwait 300\ntype \"{words}\"\nwait 400\nkey enter\nwait 900")
    };
    Scene::new("agent chat", (560.0, 640.0))
        .note("A chat: the transcript above, the composer below. The person's turns are washed blocks on the right, the agent's plain text on the left.")
        .note("Every node here is a real round through the scripted gateway — a word in the message picks which answer comes back.")
        .note("Live — enter a node and write in it; the answer arrives while you watch.")
        .node("empty", said(""))
        .about("a chat nobody has said anything in: the bar has nothing to send")
        .node("answered", said(&ask("hello")))
        .about("what the round cost is the muted line under the answer")
        .node("cut short", said(&ask("cut it short")))
        .about("the model ran out of room: the mark beside the cost, and *continue* on the bar")
        .node("failed", said(&ask("please fail")))
        .about("the gateway's own sentence where the answer would have been, and *retry*")
        .node("a call", said(&ask("rename the readme")))
        .about("a tool call is a card, and it says the sentence its own undo node wears")
        .node("a call that asks", said(&ask("delete the readme")))
        .about("a call undo cannot take back waits at its card: *allow* or *refuse*")
        .node(
            "chip",
            workspace_on(|_| Agents::id(), "key cmd+shift+a\nwait 900"),
        )
        .sized((1200.0, 700.0))
        .about("shift+cmd+a on a panel: a chat joined to it, carrying its chip")
        .node(
            "add panel",
            workspace_on(
                |_| Agents::id(),
                "key cmd 2\nwait 400\ntype \"new chat\"\nwait 400\nkey enter\nwait 700\n\
                 click \"add panel\"\nwait 500\nclick \"panel\"\nwait 300\ntype \"ag\"\nwait 600",
            ),
        )
        .sized((1200.0, 700.0))
        .about("the phone's way to the same thing: the open panels, one pick apiece")
        .edge("empty", "answered", "hello")
        .edge("empty", "cut short", "cut it short")
        .edge("empty", "failed", "please fail")
        .edge("empty", "a call", "rename the readme")
        .edge("empty", "a call that asks", "delete the readme")
        .edge("chip", "add panel", "the same chip, without the chord")
}

/// The chats, as the list shows them.
fn agents() -> Scene<Setup> {
    Scene::new("agents", (560.0, 420.0))
        .note("Every chat, newest first: its title, the model that answered, what its newest round is doing, and when it last moved.")
        .note("The word for a round that is still going is muted; a round that failed is in the ink, because that is the row this list is for.")
        .node("two chats", panel_fake(seed, ""))
        .about("one round that answered, one the gateway refused")
}

/// Two conversations, put into the mount's own store: a scene's `open` is the
/// one place a node may seed rows the demo world does not carry.
///
/// Both rounds are over — nothing here is `pending` or `waiting`, because a
/// live run wants a worker and this store has one: a mount is a picture of a
/// state, not a race with it.
fn seed(store: &Store) -> kernel::panel::PanelId {
    let _ = store.write(|c| {
        if c.query_row("SELECT count(*) FROM agent_chat", [], |r| {
            r.get::<_, i64>(0)
        })? > 0
        {
            return Ok(());
        }
        let asked = model::new_chat_tx(
            c,
            "what is in my inbox today",
            MODEL,
            ts(2026, 8, 31, 9, 40),
        )?;
        let run = model::new_run_tx(c, asked, ts(2026, 8, 31, 9, 40))?;
        model::set_run_status_tx(c, run, model::DONE, None, ts(2026, 8, 31, 9, 41))?;

        let refused = model::new_chat_tx(c, "rename the readme", MODEL, ts(2026, 8, 30, 18, 12))?;
        let run = model::new_run_tx(c, refused, ts(2026, 8, 30, 18, 12))?;
        model::set_run_status_tx(
            c,
            run,
            model::FAILED,
            Some("gateway: unauthorized — the token is not this account's"),
            ts(2026, 8, 30, 18, 12),
        )?;
        Ok(())
    });
    Agents::id()
}
