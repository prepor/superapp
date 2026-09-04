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
        let (status, said) = outcome(s, call);
        let (id, now) = (call.id, s.now());
        // Bookkeeping, not a node: the tool's own `act` is what history
        // shows and what undo takes back.
        let _ = s
            .store()
            .write(move |c| model::set_call_tx(c, id, status, &said, now));
    }
    s.workers().kick(&model::run_entity(run.id));
    pending.len()
}

/// One call: what it came to, and the word its row takes.
///
/// Three refusals before the tool is reached, each a sentence the model can
/// act on: a name no app in this build offers, arguments the tool's own
/// schema will not have, and a writing tool on a device that may not write
/// — which gets the same words a verb gets, as the error the model reads,
/// and the run goes on.
fn outcome(s: &mut Session, call: &Call) -> (&'static str, String) {
    let Some(tool) = s.apps().tool(&call.tool).cloned() else {
        return (
            model::CALL_FAILED,
            format!("no such tool in this build: {}", call.tool),
        );
    };
    let input = call.input();
    if let Err(why) = tool.check(&input) {
        return (model::CALL_FAILED, why);
    }
    if tool.writes && !s.writable() {
        return (
            model::CALL_FAILED,
            "another device holds the lease — nothing was written".to_string(),
        );
    }
    match (tool.run)(s, &input) {
        Ok(said) => (model::CALL_DONE, said.to_string()),
        Err(why) => (model::CALL_FAILED, why),
    }
}
