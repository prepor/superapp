//! Running the calls a run is waiting on, on the UI thread.
//!
//! A tool is the whole behaviour of a verb over ids instead of over a
//! cursor, and it runs with the session — so it runs *here*, where the
//! session is, and not on the worker's thread. The chat panel calls this on
//! every event while its run is waiting; a run whose chat is shown nowhere
//! pauses at its next call and picks up when the chat is opened again.
//!
//! Every call runs as soon as it arrives; nothing asks first. Undo is the
//! net, and it is one chord away — each tool files its own action, so what
//! `cmd+z` takes back is the thing the tool did and not this bookkeeping.

use kernel::history::NodeId;
use kernel::session::Session;

use super::model::{self, Call, ChatId};

/// Runs every call the chat's waiting run is holding, in the order the
/// model asked for them, and kicks the run when they are done. Answers how
/// many ran — nought for a chat with no run waiting, which is the common
/// case and costs one cached query.
pub fn run_pending_calls(s: &mut Session, chat: ChatId) -> usize {
    let Some(run) = model::latest_run(s.store(), chat) else {
        return 0;
    };
    if run.status != model::WAITING {
        return 0;
    }
    let pending = model::pending_calls(s.store(), run.id);
    if pending.is_empty() {
        return 0;
    }
    for call in &pending {
        let (status, said, label) = outcome(s, call);
        let (id, now) = (call.id, s.now());
        // Bookkeeping, not a node: the tool's own `act` is what history
        // shows and what undo takes back.
        let _ = s
            .store()
            .write(move |c| model::set_call_tx(c, id, status, &said, label.as_deref(), now));
    }
    s.workers().kick(&model::run_entity(run.id));
    pending.len()
}

/// One call: what it came to, the word its row takes, and the sentence its
/// card says.
///
/// Three refusals before the tool is reached, each a sentence the model can
/// act on: a name no app in this build offers, arguments the tool's own
/// schema will not have, and a writing tool on a device that may not write
/// — which gets the same words a verb gets, as the error the model reads,
/// and the run goes on. A refusal has no sentence: nothing was done.
fn outcome(s: &mut Session, call: &Call) -> (&'static str, String, Option<String>) {
    let Some(tool) = s.apps().tool(&call.tool).cloned() else {
        return (
            model::CALL_FAILED,
            format!("no such tool in this build: {}", call.tool),
            None,
        );
    };
    let input = call.input();
    if let Err(why) = tool.check(&input) {
        return (model::CALL_FAILED, why, None);
    }
    if tool.writes && !s.writable() {
        return (
            model::CALL_FAILED,
            "another device holds the lease — nothing was written".to_string(),
            None,
        );
    }
    let before = s.history().head();
    match (tool.run)(s, &input) {
        Ok(said) => (
            model::CALL_DONE,
            said.to_string(),
            tool.writes.then(|| filed(s, before)).flatten(),
        ),
        Err(why) => (model::CALL_FAILED, why, None),
    }
}

/// The sentence the tool's own node wears: *rename “README.txt” to
/// “readme-renamed.txt”*, which is what the card says it did and what
/// `cmd+z` says it is taking back — one thing said once, in the app's own
/// words rather than in the model's arguments.
///
/// A writing tool files exactly one undoable action, so the node it filed is
/// the head. Read only where the head **moved**: a tool that refused before
/// its write filed nothing, and a burst that coalesced into the node before
/// it left no sentence of its own. Then the card keeps the tool's own line.
fn filed(s: &Session, before: NodeId) -> Option<String> {
    let (nodes, head) = s.history().rows();
    if head == before {
        return None;
    }
    nodes.into_iter().find(|n| n.id == head).map(|n| n.label)
}
