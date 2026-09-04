//! The pass that drives one run: ask, write what came back, and — where
//! the model asked for tools — sleep until the chat has run them.
//!
//! One worker per run that is `pending` or `waiting`, derived from the
//! store, so a run starts the moment its row exists and retires when it
//! ends. It claims no queued job: it has none. Everything it learns it
//! writes as rows, because the rows are the bus — the person is on the UI
//! thread and this is not.

use kernel::app::{Wake, Worker};
use kernel::effect::{Job, World};
use kernel::store::Store;

use super::model::{
    self, add_call_tx, add_turn_tx, set_run_status_tx, set_run_usage_tx, ChatId, Cost, RunId, Turn,
};
use super::run::{Complete, Tail};
use super::wire::{Completion, Finish, Message, Role};
use super::AGENT;

/// One run's pass.
pub struct RunWorker {
    run: RunId,
    chat: ChatId,
}

impl RunWorker {
    #[must_use]
    pub fn new(run: RunId, chat: ChatId) -> RunWorker {
        RunWorker { run, chat }
    }
}

impl Worker for RunWorker {
    fn name(&self) -> String {
        format!("agent-run-{}", self.run)
    }

    fn entity(&self) -> Option<String> {
        Some(model::run_entity(self.run))
    }

    /// None: this pass has no queue behind it. The one thing it does is
    /// ask the model, and that is an in-memory effect it runs itself.
    fn claims(&self, _job: &Job) -> bool {
        false
    }

    /// A pending run is asked; a waiting one is asked again once every call
    /// it is holding has answered. Anything else is a run that has ended,
    /// and the worker sleeps until the set is diffed and it retires.
    fn pass(&mut self, w: &World) -> Wake {
        let Some(run) = model::run_conn(w.store().conn(), self.run) else {
            return Wake::OnKick;
        };
        match run.status.as_str() {
            model::PENDING => self.ask(w),
            model::WAITING => self.answered(w),
            _ => Wake::OnKick,
        }
    }
}

impl RunWorker {
    /// One round: the request, and the row its answer becomes.
    fn ask(&mut self, w: &World) -> Wake {
        let (run, chat, now) = (self.run, self.chat, w.now());
        if w.store()
            .write(move |c| set_run_status_tx(c, run, model::STREAMING, None, now))
            .is_err()
        {
            return Wake::OnKick;
        }
        let turn = model::turns_conn(w.store().conn(), chat).len() as i64 + 1;
        let answer = w.run(&Complete { run, chat, turn });
        // Read before it is cleared: a stop keeps what had arrived.
        let tail = AGENT.tail(run);
        AGENT.clear_tail(run);
        match answer {
            Ok(done) => self.landed(w, &done),
            // The one failure with a row to write: the person stopped it,
            // and what the model had said by then is still worth keeping.
            Err(why) if why == model::STOPPED => self.was_stopped(w, tail.as_ref()),
            Err(why) => self.failed(w, &why),
        }
    }

    /// The answer, as rows. A turn either way; calls and a wait where the
    /// model asked for tools, an ending where it did not.
    fn landed(&mut self, w: &World, done: &Completion) -> Wake {
        let (run, chat, now) = (self.run, self.chat, w.now());
        let usage = done.usage.as_ref().map(Cost::of);
        // `tool_calls` with no calls on it is a model contradicting itself;
        // treating it as an ending is what keeps the run from waiting on
        // nothing for ever.
        let wants_tools = done.finish == Finish::ToolCalls && !done.message.tool_calls.is_empty();
        if wants_tools {
            let turn = Turn::new(done.message.clone()).by(run);
            let calls = done.message.tool_calls.clone();
            let _ = w.store().write(move |c| {
                let (turn_id, _) = add_turn_tx(c, chat, &turn, now)?;
                for call in &calls {
                    add_call_tx(c, run, turn_id, call, now)?;
                }
                set_run_status_tx(c, run, model::WAITING, None, now)?;
                if let Some(u) = usage {
                    set_run_usage_tx(c, run, &u)?;
                }
                Ok(())
            });
            return Wake::OnKick;
        }
        let turn = Turn::new(done.message.clone())
            .by(run)
            .finishing(done.finish.word());
        let _ = w.store().write(move |c| {
            add_turn_tx(c, chat, &turn, now)?;
            set_run_status_tx(c, run, model::DONE, None, now)?;
            if let Some(u) = usage {
                set_run_usage_tx(c, run, &u)?;
            }
            Ok(())
        });
        Wake::OnKick
    }

    /// The person cut it: what had arrived becomes a turn of its own,
    /// marked with the word for why it ends there.
    fn was_stopped(&mut self, w: &World, tail: Option<&Tail>) -> Wake {
        let (run, chat, now) = (self.run, self.chat, w.now());
        let mut message = Message::of(Role::Assistant);
        if let Some(t) = tail {
            if !t.text.is_empty() {
                message.content = Some(t.text.clone());
            }
            if !t.reasoning.is_empty() {
                message.reasoning_content = Some(t.reasoning.clone());
            }
        }
        let turn = Turn::new(message).by(run).finishing(model::STOPPED);
        let _ = w.store().write(move |c| {
            add_turn_tx(c, chat, &turn, now)?;
            set_run_status_tx(c, run, model::STOPPED, None, now)
        });
        Wake::OnKick
    }

    /// Nothing came back. The sentence is the gateway's, and the chat
    /// offers *retry*: nothing here retries by itself.
    fn failed(&mut self, w: &World, why: &str) -> Wake {
        let (run, now, why) = (self.run, w.now(), why.to_string());
        let _ = w
            .store()
            .write(move |c| set_run_status_tx(c, run, model::FAILED, Some(&why), now));
        Wake::OnKick
    }

    /// The calls came back: one `tool` turn per result, in the order the
    /// model asked for them, and then round again.
    ///
    /// This round's calls, not the run's: a run that has already been round
    /// once still has the earlier rounds' rows, and those were answered
    /// when they were the latest.
    fn answered(&mut self, w: &World) -> Wake {
        let calls = model::round_calls_conn(w.store().conn(), self.run);
        let settled = !calls.is_empty()
            && calls
                .iter()
                .all(|c| matches!(c.status.as_str(), model::CALL_DONE | model::CALL_FAILED));
        if !settled {
            return Wake::OnKick;
        }
        let (run, chat, now) = (self.run, self.chat, w.now());
        let results: Vec<Turn> = calls
            .iter()
            .map(|c| Turn::new(Message::tool(&c.tool_call_id, c.said())).by(run))
            .collect();
        if w.store()
            .write(move |c| {
                for turn in &results {
                    add_turn_tx(c, chat, turn, now)?;
                }
                Ok(())
            })
            .is_err()
        {
            return Wake::OnKick;
        }
        self.ask(w)
    }
}

/// The passes the agent wants running now: one per run that is `pending` or
/// `waiting`, from one cached query.
///
/// **None at all on a store that may not be written.** A run row replicates
/// like any other, so the device that does not hold the lease would
/// otherwise start a second worker for a run the holder is already paying
/// for — two requests, two answers, and a turn nobody can put back in
/// order.
#[must_use]
pub fn workers(store: &Store) -> Vec<Box<dyn Worker>> {
    if !store.is_writable() {
        return Vec::new();
    }
    model::runs_wanting_workers(store)
        .iter()
        .map(|(run, chat)| Box::new(RunWorker::new(*run, *chat)) as Box<dyn Worker>)
        .collect()
}
