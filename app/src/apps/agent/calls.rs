//! Dispatching session calls in the order the model requested them.
//!
//! Expensive preparation runs on a service world. The session commits the
//! resulting transaction with its tool reply, or accepts a native command
//! whose service owns completion and compensation. A pending invocation holds
//! its place across UI events and survives the chat panel closing. A run with
//! no visible chat pauses before starting its next session call.
//! Background reads belong to the run's worker; this walk stops at them so
//! later session calls cannot overtake a download.
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
use kernel::session::{Edit, Session};
use kernel::tool::{Prepare, Prepared, Tool};
use std::cell::Cell;
use std::collections::HashSet;
use std::rc::Rc;

use super::model::{self, Call, CallId, ChatId, RunId};

/// Dispatch belongs to this session's world, not a process-global catalogue
/// or a panel that may close while a call is preparing. One active call holds
/// its run's place until the result transaction has committed.
#[derive(Default)]
pub(super) struct ActiveCalls(HashSet<RunId>);

type Outcome = (&'static str, String, Option<String>);

fn active(s: &Session, run: RunId) -> bool {
    s.world().with_cap::<ActiveCalls, _>(|calls| calls.0.contains(&run)).unwrap_or(false)
}
fn begin(s: &Session, run: RunId) {
    s.world().with_cap::<ActiveCalls, _>(|calls| calls.0.insert(run)).expect("agent dispatch state");
}
fn finished(s: &mut Session, run: RunId, resume: bool) {
    s.world().with_cap::<ActiveCalls, _>(|calls| calls.0.remove(&run)).expect("agent dispatch state");
    if resume {
        if let Some(run) = model::run(s.store(), run) {
            run_pending_calls(s, run.chat);
        }
    }
    s.workers().kick_all();
}

/// Starts ordered calls up to an approval, a background read, or a pending
/// preparation/commit. Further UI events cannot dispatch the same call twice.
pub fn run_pending_calls(s: &mut Session, chat: ChatId) -> usize {
    let Some(run) = waiting_run(s, chat) else { return 0; };
    if active(s, run) || !model::asked_calls(s.store(), run).is_empty() { return 0; }
    let mut moved = 0;
    for call in model::pending_calls(s.store(), run) {
        let tool = s.apps().tool(&call.tool);
        if tool.is_some_and(|tool| tool.reader.is_some() && !tool.asks) { break; }
        moved += 1;
        if tool.is_some_and(|tool| tool.asks) {
            begin(s, run);
            let id = call.id;
            s.act_async(Edit::writing("agent.ask", "ask to run a tool", move |tx| {
                if pending(tx, id, run)? { model::ask_call_tx(tx, id)?; }
                Ok(())
            }).record_if(|_| false), move |s, _| finished(s, run, false));
            break;
        }
        if ran(s, &call) { break; }
    }
    if moved > 0 { s.workers().kick_all(); }
    moved
}

pub fn allow(s: &mut Session, chat: ChatId, call: CallId) -> bool {
    let Some(call) = asked(s, chat, call) else { return false; };
    ran(s, &call);
    run_pending_calls(s, chat);
    s.workers().kick_all();
    true
}

pub fn refuse(s: &mut Session, chat: ChatId, call: CallId) -> bool {
    let Some(call) = asked(s, chat, call) else { return false; };
    begin(s, call.run);
    record(s, &call, (model::CALL_REFUSED, String::new(), None), Rc::new(Cell::new(true)));
    true
}

/// True while a call still owns its position. Fixtures complete in this turn,
/// so their walk can continue and keep reporting the number of calls moved.
fn ran(s: &mut Session, call: &Call) -> bool {
    begin(s, call.run);
    let deferred = Rc::new(Cell::new(false));
    let tool = s.apps().tool(&call.tool).cloned();
    match tool {
        Some(tool) if tool.preparer.is_some() || tool.reader.is_some() || tool.stager.is_some() => {
            {
                let call = call.clone();
                let resume = deferred.clone();
                if let Some(stage) = tool.stager {
                    let input = call.input.clone();
                    s.prepare_work(move |world| Box::pin(decode_call(world, tool, input)), move |s, input| {
                        if !pending(s.store().conn(), call.id, call.run).unwrap_or(false) {
                            finished(s, call.run, resume.get());
                            return;
                        }
                        let prepare = input.and_then(|(_, input)| stage(s, &input));
                        match prepare {
                            Ok(prepare) => s.prepare_tool(prepare, move |s, result| prepared(s, call, resume, result)),
                            Err(error) => prepared(s, call, resume, Err(error)),
                        }
                    });
                } else {
                    let prepare = prepare_call(tool, call.input.clone());
                    s.prepare_tool(prepare, move |s, result| prepared(s, call, resume, result));
                }
            }
        }
        _ => {
            let answer = outcome(s, call);
            record(s, call, answer, deferred.clone());
        }
    }
    deferred.set(true);
    active(s, call.run)
}

fn prepared(s: &mut Session, call: Call, resume: Rc<Cell<bool>>, result: Result<Prepared, String>) {
    let run = call.run;
                    // An explicit Stop or deletion while preparation was in
                    // flight withdraws this invocation before it changes data.
                    if !pending(s.store().conn(), call.id, run).unwrap_or(false) {
                        finished(s, run, resume.get());
                        return;
                    }
                    match result {
                        Ok(Prepared::Edit(edit)) => {
                            let (id, now) = (call.id, s.now());
                            let label = edit.label().to_owned();
                            // Both the edit and its reply commit atomically.
                            // A Stop queued ahead of this transaction rolls the
                            // edit back, and a crash cannot replay a completed edit.
                            let edit = edit.then_write(move |tx, reply| {
                                if !pending(tx, id, run)? {
                                    return Err(rusqlite::Error::SqliteFailure(
                                        rusqlite::ffi::Error::new(rusqlite::ffi::SQLITE_ABORT),
                                        Some("the tool call was cancelled".into()),
                                    ));
                                }
                                model::set_call_tx(tx, id, model::CALL_DONE, &reply.to_string(), Some(&label), now)
                            });
                            s.act_async_result(edit, move |s, result| match result {
                                Ok(_) => finished(s, run, resume.get()),
                                Err(error) => record(s, &call, (model::CALL_FAILED, error.to_string(), None), resume),
                            });
                        }
                        Ok(Prepared::Reply(reply)) => record(s, &call, (model::CALL_DONE, reply.to_string(), None), resume),
                        Ok(Prepared::Command(command)) => {
                            let before = s.history().head();
                            command.commit(s, Box::new(move |s, result| {
                                let answer = match result {
                                    Ok(reply) => (model::CALL_DONE, reply.to_string(), filed(s, before)),
                                    Err(error) => (model::CALL_FAILED, error, None),
                                };
                                record_command(s, call, answer, resume);
                            }));
                        }
                        Err(error) => record(s, &call, (model::CALL_FAILED, error, None), resume),
                    }
}

/// Parsing a large tool input and checking its schema are preparation too.
fn prepare_call(tool: Tool, input: String) -> Prepare {
    Box::new(move |world| Box::pin(async move {
        let (tool, input) = decode_call(world, tool, input).await?;
        if let Some(prepare) = tool.preparer {
            prepare(&input)(world).await
        } else {
            tool.reader.expect("a background reader")(&input)(world).await.map(Prepared::Reply)
        }
    }))
}

async fn decode_call(world: &kernel::effect::World, tool: Tool, input: String) -> Result<(Tool, serde_json::Value), String> {
    let decode = move || {
        let input = serde_json::from_str(&input).map_err(|error| error.to_string())?;
        tool.check(&input)?;
        Ok((tool, input))
    };
    if world.factory().is_some() {
        kernel::runtime::spawn_blocking(decode).await.map_err(|error| error.to_string())?
    } else { decode() }
}

/// Bookkeeping changes no user data and therefore files no undo node. The
/// predicate preserves a Stop's cancellation result if it arrived meanwhile.
fn record(s: &mut Session, call: &Call, (status, said, label): Outcome, resume: Rc<Cell<bool>>) {
    let (id, run, now) = (call.id, call.run, s.now());
    s.act_async(Edit::writing("agent.result", "record a tool result", move |tx| {
        if pending(tx, id, run)? {
            model::set_call_tx(tx, id, status, &said, label.as_deref(), now)?;
        }
        Ok(())
    }).record_if(|_| false), move |s, _| finished(s, run, resume.get()));
}

/// A native operation owns its completion once accepted. A later Stop cannot
/// undo the filesystem transaction or discard its undo claim. If Stop already
/// closed the tool round, update that response to the actual completed result.
fn record_command(s: &mut Session, call: Call, (status, said, label): Outcome, resume: Rc<Cell<bool>>) {
    let (run, now) = (call.run, s.now());
    s.act_async(Edit::writing("agent.result", "record a native tool result", move |tx| {
        model::set_call_tx(tx, call.id, status, &said, label.as_deref(), now)?;
        tx.execute("UPDATE agent_turn SET body=json_set(body,'$.content',?1)
            WHERE run=?2 AND role='tool' AND json_extract(body,'$.tool_call_id')=?3
            AND seq>(SELECT seq FROM agent_turn WHERE id=?4)
            AND seq<COALESCE((SELECT MIN(next.seq) FROM agent_turn next
                WHERE next.run=?2 AND next.role='assistant'
                AND next.seq>(SELECT seq FROM agent_turn WHERE id=?4)),9223372036854775807)",
            rusqlite::params![said, run, call.tool_call_id, call.turn])?;
        Ok(())
    }).record_if(|_| false), move |s, _| finished(s, run, resume.get()));
}

fn pending(c: &rusqlite::Connection, call: CallId, run: RunId) -> rusqlite::Result<bool> {
    c.query_row(
        "SELECT EXISTS(SELECT 1 FROM agent_call c JOIN agent_run r ON r.id=c.run
         WHERE c.id=?1 AND c.run=?2 AND c.status IN (?3,?4) AND r.status=?5)",
        rusqlite::params![call, run, model::CALL_PENDING, model::CALL_ASKED, model::WAITING],
        |row| row.get(0),
    )
}

fn waiting_run(s: &Session, chat: ChatId) -> Option<RunId> {
    model::latest_run(s.store(), chat).filter(|run| run.status == model::WAITING).map(|run| run.id)
}

fn asked(s: &Session, chat: ChatId, call: CallId) -> Option<Call> {
    let run = waiting_run(s, chat)?;
    if active(s, run) { return None; }
    model::asked_calls(s.store(), run).into_iter().find(|row| row.id == call)
}

fn outcome(s: &mut Session, call: &Call) -> Outcome {
    let Some(tool) = s.apps().tool(&call.tool).cloned() else {
        return (model::CALL_FAILED, format!("no such tool in this build: {}", call.tool), None);
    };
    let input = call.input();
    if let Err(why) = tool.check(&input) { return (model::CALL_FAILED, why, None); }
    let before = s.history().head();
    match (tool.run)(s, &input) {
        Ok(said) => (model::CALL_DONE, said.to_string(), tool.writes.then(|| filed(s, before)).flatten()),
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

#[cfg(test)]
mod tests {
    use super::*;
    use kernel::app::{App, Capabilities, Env, Mode};
    use serde_json::json;
    use std::sync::{Arc, atomic::{AtomicBool, AtomicUsize, Ordering}};
    use std::time::Duration;

    #[derive(Clone, Default)]
    struct Gate { started: Arc<AtomicUsize>, release: Arc<tokio::sync::Notify>, native: Arc<AtomicBool> }
    struct TestTools;
    static APPS: &[&dyn App] = &[&super::super::AGENT, &TestTools];

    fn delayed() -> Tool {
        Tool::preparing("test.prepared", "prepare a change", json!({"type":"object"}), |_| {
            Box::new(|world| Box::pin(async move {
                let gate = world.with_cap::<Gate, _>(|gate| gate.clone())?;
                gate.started.fetch_add(1, Ordering::SeqCst);
                gate.release.notified().await;
                Ok(Prepared::Edit(Edit::writing("test.prepared", "prepared change", |tx| {
                    tx.execute("INSERT INTO meta(key,value) VALUES('prepared',1)", [])?;
                    Ok(json!({"prepared": true}))
                })))
            }))
        })
    }
    impl App for TestTools {
        fn id(&self) -> &'static str { "prepared-test" }
        fn kinds(&self) -> &'static [&'static dyn kernel::panel::PanelKind] { &[] }
        fn as_any(&self) -> &dyn std::any::Any { self }
        fn outside(&self, _: Mode, _: &Env, caps: &mut Capabilities) {
            caps.insert::<Gate>(Box::default());
        }
        fn tools(&self) -> Vec<Tool> {
            let mut approved = delayed().asking();
            approved.name = "test.approved";
            vec![delayed(), approved, Tool::new("test.after", "the next call", json!({"type":"object"}), false, |_, _| Ok(json!({"after": true}))),
                Tool::preparing("test.native", "a native command", json!({"type":"object"}), |_| Box::new(|_| Box::pin(async { Ok(Prepared::Command(Box::new(Native))) })))]
        }
    }
    struct Native;
    struct NativeUndo(Arc<AtomicBool>);
    impl kernel::history::Intent for NativeUndo {
        fn describe(&self) -> String { "native change".into() }
        fn reverse(&self, _: &kernel::effect::World) -> Result<(), String> { self.0.store(false, Ordering::SeqCst); Ok(()) }
        fn reapply(&self, _: &kernel::effect::World) -> Result<(), String> { self.0.store(true, Ordering::SeqCst); Ok(()) }
    }
    impl kernel::tool::Command for Native {
        fn commit(self: Box<Self>, s: &mut Session, complete: kernel::tool::CommandComplete) {
            s.prepare_work(|world| Box::pin(async move {
                let gate = world.with_cap::<Gate, _>(|gate| gate.clone())?;
                gate.started.fetch_add(1, Ordering::SeqCst);
                gate.release.notified().await;
                gate.native.store(true, Ordering::SeqCst);
                Ok(gate.native)
            }), move |s, result| match result {
                Ok(changed) => {
                    s.act(kernel::session::Action::new("test.native", "native change").claiming(vec![Box::new(NativeUndo(changed))]));
                    complete(s, Ok(json!({"native":true})));
                }
                Err(error) => complete(s, Err(error)),
            });
        }
    }

    fn seed(s: &Session, tools: &[&str]) -> (ChatId, RunId, Vec<CallId>) {
        let tools: Vec<String> = tools.iter().map(|tool| (*tool).to_string()).collect();
        s.store().write(move |tx| {
            let chat = model::new_chat_tx(tx, "prepared tools", super::super::MODEL, 1.)?;
            tx.execute("INSERT INTO agent_run(chat,status,started) VALUES(?1,'waiting',1)", [chat])?;
            let run = tx.last_insert_rowid();
            tx.execute("INSERT INTO agent_turn(chat,seq,role,body,run,created) VALUES(?1,1,'assistant','{}',?2,1)", rusqlite::params![chat, run])?;
            let turn = tx.last_insert_rowid();
            let mut calls = Vec::new();
            for (index, tool) in tools.into_iter().enumerate() {
                calls.push(model::add_call_tx(tx, run, turn, &super::super::wire::ToolCall {
                    id: format!("call-{index}"), r#type: "function".into(),
                    function: super::super::wire::FunctionCall { name: tool, arguments: "{}".into() },
                }, 1.)?);
            }
            Ok((chat, run, calls))
        }).unwrap()
    }
    fn gate(s: &Session) -> Gate { s.world().with_cap::<Gate, _>(|gate| gate.clone()).unwrap() }
    fn edited(s: &Session) -> bool {
        s.store().conn().query_row("SELECT EXISTS(SELECT 1 FROM meta WHERE key='prepared')", [], |row| row.get(0)).unwrap()
    }

    #[test]
    fn a_pending_preparation_runs_once_and_holds_later_calls() {
        let mut s = Session::fake(APPS);
        let (chat, run, _) = seed(&s, &["test.prepared", "test.after"]);
        let gate = gate(&s);
        assert_eq!(run_pending_calls(&mut s, chat), 1);
        for _ in 0..5 { assert_eq!(run_pending_calls(&mut s, chat), 0); }
        assert_eq!(gate.started.load(Ordering::SeqCst), 1);
        assert!(!edited(&s));
        assert!(model::calls(s.store(), run).iter().all(|call| call.status == model::CALL_PENDING));
        gate.release.notify_one();
        s.settle();
        let calls = model::calls(s.store(), run);
        assert!(calls.iter().all(|call| call.status == model::CALL_DONE));
        assert_eq!(calls[0].label.as_deref(), Some("prepared change"));
        assert!(calls[1].label.is_none());
        assert!(edited(&s));
    }

    #[test]
    fn approval_starts_preparation_once_and_stop_withdraws_it_before_commit() {
        let mut s = Session::fake(APPS);
        let (chat, run, ids) = seed(&s, &["test.approved"]);
        let gate = gate(&s);
        assert_eq!(run_pending_calls(&mut s, chat), 1);
        assert_eq!(gate.started.load(Ordering::SeqCst), 0, "approval precedes preparation");
        assert!(allow(&mut s, chat, ids[0]));
        assert!(!allow(&mut s, chat, ids[0]));
        assert!(!refuse(&mut s, chat, ids[0]));
        assert_eq!(gate.started.load(Ordering::SeqCst), 1);
        s.store().write(move |tx| {
            model::set_run_status_tx(tx, run, model::STOPPED, None, 2.)?;
            model::set_call_tx(tx, ids[0], model::CALL_CANCELLED, "", None, 2.)
        }).unwrap();
        let before = s.history().head();
        gate.release.notify_one();
        s.settle();
        assert!(!edited(&s));
        assert_eq!(s.history().head(), before);
        assert_eq!(model::calls(s.store(), run)[0].status, model::CALL_CANCELLED);
        assert!(!active(&s, run));
    }

    #[test]
    fn a_prepared_edit_and_its_result_commit_before_the_ui_completion() {
        let mut s = Session::fake(APPS);
        let (chat, run, _) = seed(&s, &["test.prepared"]);
        s.store().attach_ui(|| {});
        let gate = gate(&s);
        let (entered, waiting) = std::sync::mpsc::channel();
        let (release, held) = std::sync::mpsc::channel();
        let _writer = s.store().submit_write(move |_| { entered.send(()).unwrap(); held.recv().unwrap(); Ok(()) }).unwrap();
        waiting.recv_timeout(Duration::from_secs(5)).unwrap();
        let before = s.history().head();
        assert_eq!(run_pending_calls(&mut s, chat), 1);
        gate.release.notify_one();
        s.settle();
        assert!(!edited(&s));
        assert_eq!(run_pending_calls(&mut s, chat), 0);
        release.send(()).unwrap();
        kernel::runtime::block_on(s.store().flush_async()).unwrap();
        assert!(edited(&s));
        assert_eq!(model::calls(s.store(), run)[0].status, model::CALL_DONE);
        assert_eq!(s.history().head(), before, "completion has not polled yet");
        s.settle();
        assert!(s.history().head() > before);
        assert!(!active(&s, run));
        s.shutdown();
    }

    #[test]
    fn stop_after_a_native_command_started_keeps_its_result_and_undo() {
        let mut s = Session::fake(APPS);
        let (chat, run, ids) = seed(&s, &["test.native"]);
        let gate = gate(&s);
        assert_eq!(run_pending_calls(&mut s, chat), 1);
        assert_eq!(gate.started.load(Ordering::SeqCst), 1);
        s.store().write(move |tx| {
            model::set_run_status_tx(tx, run, model::STOPPED, None, 2.)?;
            model::set_call_tx(tx, ids[0], model::CALL_CANCELLED, "", None, 2.)?;
            let turn = model::Turn::new(super::super::wire::Message::tool("call-0", "cancelled")).by(run);
            model::add_turn_tx(tx, chat, &turn, 2.)?;
            Ok(())
        }).unwrap();
        gate.release.notify_one();
        s.settle();
        assert!(gate.native.load(Ordering::SeqCst));
        let call = &model::calls(s.store(), run)[0];
        assert_eq!(call.status, model::CALL_DONE);
        assert_eq!(call.label.as_deref(), Some("native change"));
        assert!(model::turns(s.store(), chat).last().unwrap().message.text().contains("native"));
        assert!(s.undo());
        assert!(!gate.native.load(Ordering::SeqCst), "the accepted native operation retained its undo");
        assert_eq!(model::run(s.store(), run).unwrap().status, model::STOPPED);
    }

    #[test]
    fn a_stop_queued_before_the_prepared_transaction_rolls_back_the_edit_and_reply() {
        let mut s = Session::fake(APPS);
        let (chat, run, ids) = seed(&s, &["test.prepared"]);
        s.store().attach_ui(|| {});
        let gate = gate(&s);
        let (entered, waiting) = std::sync::mpsc::channel();
        let (release, held) = std::sync::mpsc::channel();
        let _writer = s.store().submit_write(move |_| { entered.send(()).unwrap(); held.recv().unwrap(); Ok(()) }).unwrap();
        waiting.recv_timeout(Duration::from_secs(5)).unwrap();
        assert_eq!(run_pending_calls(&mut s, chat), 1);
        let _stop = s.store().submit_write(move |tx| {
            model::set_run_status_tx(tx, run, model::STOPPED, None, 2.)?;
            model::set_call_tx(tx, ids[0], model::CALL_CANCELLED, "", None, 2.)
        }).unwrap();
        gate.release.notify_one();
        s.settle();
        let before = s.history().head();
        release.send(()).unwrap();
        s.shutdown();
        assert!(!edited(&s));
        assert_eq!(s.history().head(), before);
        assert_eq!(model::calls(s.store(), run)[0].status, model::CALL_CANCELLED);
        assert!(!active(&s, run));
    }
}
