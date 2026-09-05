//! Running the calls a run is waiting on, on the UI thread.
//!
//! A tool is the whole behaviour of a verb over ids instead of over a
//! cursor, and it runs with the session — so it runs *here*, where the
//! session is, and not on the worker's thread. The chat panel calls this on
//! every event while its run is waiting; a run whose chat is shown nowhere
//! pauses at its next call and picks up when the chat is opened again.
//!
//! Nearly every call runs as soon as it arrives: undo is the net, and it is
//! one chord away — each tool files its own action, so what `cmd+z` takes
//! back is the thing the tool did and not this bookkeeping. The exception
//! is a tool that [asks](kernel::tool::Tool::asks): what cannot be undone
//! or has left the machine stops the walk at a card that waits, and
//! [`allow`] or [`refuse`] is what starts it again. The walk stops there
//! rather than running on, because order can matter — a draft before its
//! send.

use kernel::history::NodeId;
use kernel::session::Session;

use super::model::{self, Call, CallId, ChatId, RunId};

/// Runs every call the chat's waiting run is holding, in the order the
/// model asked for them, up to the first that asks, and kicks the run when
/// they are done. Answers how many rows moved — nought for a chat with no
/// run waiting, which is the common case and costs one cached query, and
/// nought again while a call stands asked.
pub fn run_pending_calls(s: &mut Session, chat: ChatId) -> usize {
    let Some(run) = waiting_run(s, chat) else {
        return 0;
    };
    // A round stopped at a question stays stopped: the calls behind an
    // asked one are its own to release.
    if !model::asked_calls(s.store(), run).is_empty() {
        return 0;
    }
    let moved = walk(s, run);
    if moved > 0 {
        // `kick_all`, not `kick`: a worker whose channel was closed while
        // the set was diffed answers no address at all, and only re-asking
        // the apps brings it back. The run's own worker is among the ones
        // woken either way.
        s.workers().kick_all();
    }
    moved
}

/// The person's word, on the call that was waiting for it: it runs, exactly
/// as an arriving call would have, and the round goes on to the next one.
/// Answers whether there was such a call to allow.
pub fn allow(s: &mut Session, chat: ChatId, call: CallId) -> bool {
    let Some(call) = asked(s, chat, call) else {
        return false;
    };
    ran(s, &call);
    // The rest of the round, up to the next question — and the kick, which
    // the worker needs whether anything else moved or not.
    run_pending_calls(s, chat);
    s.workers().kick_all();
    true
}

/// The other word: the call never runs, and *refused by the person* is what
/// the model reads back, so it can say what it could not do. The walk goes
/// on — the next call may itself be one that asks.
pub fn refuse(s: &mut Session, chat: ChatId, call: CallId) -> bool {
    let Some(call) = asked(s, chat, call) else {
        return false;
    };
    let (id, now) = (call.id, s.now());
    let _ = s
        .store()
        .write(move |c| model::set_call_tx(c, id, model::CALL_REFUSED, "", None, now));
    run_pending_calls(s, chat);
    s.workers().kick_all();
    true
}

/// The round's pending calls in the model's own order, each run where its
/// tool runs on arrival and asked where it does not — and there the walk
/// stops, with everything behind it left pending. Answers how many rows
/// moved.
fn walk(s: &mut Session, run: RunId) -> usize {
    let mut moved = 0;
    for call in model::pending_calls(s.store(), run) {
        moved += 1;
        if s.apps().tool(&call.tool).is_some_and(|t| t.asks) {
            let id = call.id;
            let _ = s.store().write(move |c| model::ask_call_tx(c, id));
            break;
        }
        ran(s, &call);
    }
    moved
}

/// One call, run and written down. Bookkeeping, not a node: the tool's own
/// `act` is what history shows and what undo takes back.
fn ran(s: &mut Session, call: &Call) {
    let (status, said, label) = outcome(s, call);
    let (id, now) = (call.id, s.now());
    let _ = s
        .store()
        .write(move |c| model::set_call_tx(c, id, status, &said, label.as_deref(), now));
}

/// The chat's newest run, if it is one holding calls.
fn waiting_run(s: &Session, chat: ChatId) -> Option<RunId> {
    model::latest_run(s.store(), chat)
        .filter(|r| r.status == model::WAITING)
        .map(|r| r.id)
}

/// The call an *allow* or a *refuse* is about: one of this chat's own, and
/// one that is really waiting to be answered. A word for anything else is
/// a word for a card that has moved on.
fn asked(s: &Session, chat: ChatId, call: CallId) -> Option<Call> {
    let run = waiting_run(s, chat)?;
    model::asked_calls(s.store(), run)
        .into_iter()
        .find(|c| c.id == call)
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
