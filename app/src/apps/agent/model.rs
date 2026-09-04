//! The agent's rows, its queries, and the writes a verb composes into an
//! action.
//!
//! The store is the bus. A run has three parties on three threads — the
//! person in the chat panel, the worker talking to the gateway, and the one
//! writer every write goes through — and they meet in these rows and
//! nowhere else. So the writes are `_tx` pieces over a connection, which an
//! action runs inside its one transaction and a worker runs on its own; and
//! the reads come in two forms, the cached one a panel draws from and the
//! `_conn` twin a worker or an effect uses, which holds no cache because it
//! is looking for what another thread has just committed.
//!
//! The actions at the foot are the whole of what a person does to a chat:
//! [`send`], [`stop`], [`retry`], [`delete_chats`]. Every one of them is an
//! ordinary undoable action, which is the rule the whole app is built
//! around.

use std::cell::Cell;
use std::rc::Rc;

use kernel::effect::World;
use kernel::filter::Op;
use kernel::history::Intent;
use kernel::richtable::{Dir, SqlSource, SqlSpec, Suggestion, TagDef, TagSql, TagType, Values};
use kernel::session::{Action, Session};
use kernel::store::{Store, Val, Q};
use rusqlite::Connection;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::wire::{Message, Role, ToolCall, Usage};
use super::MODEL;

/// A conversation's row id — the one argument a `chat` panel carries.
pub type ChatId = i64;
/// One message's row id.
pub type TurnId = i64;
/// One round of the agent working on a chat.
pub type RunId = i64;
/// One use of a tool inside a run.
pub type CallId = i64;

// -- the words a status column takes -------------------------------------------

/// Filed, and nobody has asked the model yet.
pub const PENDING: &str = "pending";
/// The request is out and the answer is arriving.
pub const STREAMING: &str = "streaming";
/// The model asked for tools; the calls are rows, and the chat panel runs
/// them.
pub const WAITING: &str = "waiting";
/// The model said what it had to say.
pub const DONE: &str = "done";
/// The gateway, the network, or the model refused. The chat offers *retry*.
pub const FAILED: &str = "failed";
/// The person stopped it.
pub const STOPPED: &str = "stopped";

/// The statuses a run can still move from — what a worker is wanted for.
pub const LIVE: [&str; 3] = [PENDING, STREAMING, WAITING];

/// A call nobody has run yet.
pub const CALL_PENDING: &str = "pending";
/// A call that answered.
pub const CALL_DONE: &str = "done";
/// A call that refused, whose error is what the model reads.
pub const CALL_FAILED: &str = "failed";

/// What a chat is called before the person has said anything in it.
pub const UNTITLED: &str = "chat";

/// How much of the first line becomes the title.
const TITLE_MAX: usize = 60;

/// Rows per page of the agents list.
pub const CHATS_PAGE: usize = 50;

// -- what a row is -------------------------------------------------------------

/// One conversation.
#[derive(Debug, Clone, PartialEq)]
pub struct Chat {
    pub id: ChatId,
    pub title: String,
    pub model: String,
    pub created: f64,
    pub updated: f64,
}

/// One message, as its row keeps it and as the next request sends it.
///
/// The wire's own message is flattened in, because that is what `body`
/// holds — verbatim, so the next request is built from the rows and a
/// `sqlite3` reader can still read them — with the app's two keys beside
/// it: `chips`, the context a turn carried (empty until phase two), and
/// `finish`, the word the model stopped on.
///
/// The five fields above them are the **row's**, not the body's: serde
/// skips them both ways, and they are filled in by the read.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Turn {
    #[serde(skip)]
    pub id: TurnId,
    #[serde(skip)]
    pub chat: ChatId,
    /// Its place in the conversation, from one.
    #[serde(skip)]
    pub seq: i64,
    /// The run that wrote it, or the run the person's message started.
    #[serde(skip)]
    pub run: Option<RunId>,
    #[serde(skip)]
    pub created: f64,
    #[serde(flatten)]
    pub message: Message,
    /// The context this turn carried, as chips. Reserved for phase two; an
    /// empty array until then, and left out of the JSON while it is empty.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub chips: Vec<Value>,
    /// Why the model stopped, on an agent's turn: `stop`, `length`,
    /// `content_filter`, `stopped`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub finish: Option<String>,
}

impl Turn {
    /// A turn of one message, with no row behind it yet.
    #[must_use]
    pub fn new(message: Message) -> Turn {
        Turn {
            id: 0,
            chat: 0,
            seq: 0,
            run: None,
            created: 0.0,
            message,
            chips: Vec::new(),
            finish: None,
        }
    }

    /// The same, said by a run.
    #[must_use]
    pub fn by(mut self, run: RunId) -> Turn {
        self.run = Some(run);
        self
    }

    /// The same, with the word the model stopped on.
    #[must_use]
    pub fn finishing(mut self, word: impl Into<String>) -> Turn {
        self.finish = Some(word.into());
        self
    }

    /// The text of it, or the empty string where there is none.
    #[must_use]
    pub fn text(&self) -> &str {
        self.message.text()
    }

    /// The body as the column holds it. A turn is strings and the wire's
    /// own JSON, so there is nothing in it serde can refuse.
    #[must_use]
    pub fn body(&self) -> String {
        serde_json::to_string(self).unwrap_or_else(|_| "{}".to_string())
    }
}

/// What a turn cost, as the run's `usage` column keeps it.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Cost {
    #[serde(rename = "in")]
    pub input: u64,
    #[serde(rename = "out")]
    pub output: u64,
    /// How much of the prompt the model already had.
    #[serde(default)]
    pub cached: u64,
}

impl Cost {
    /// What the stream's last chunk said.
    #[must_use]
    pub fn of(usage: &Usage) -> Cost {
        Cost {
            input: usage.prompt_tokens,
            output: usage.completion_tokens,
            cached: usage.cached(),
        }
    }
}

/// One round of the agent working on a chat.
#[derive(Debug, Clone, PartialEq)]
pub struct Run {
    pub id: RunId,
    pub chat: ChatId,
    pub status: String,
    /// The gateway's sentence, on a run that failed.
    pub error: Option<String>,
    pub started: f64,
    pub ended: Option<f64>,
    pub usage: Option<Cost>,
}

impl Run {
    /// Whether this run still has somewhere to go — what a worker is for.
    #[must_use]
    pub fn live(&self) -> bool {
        LIVE.contains(&self.status.as_str())
    }
}

/// One use of a tool inside a run.
#[derive(Debug, Clone, PartialEq)]
pub struct Call {
    pub id: CallId,
    pub run: RunId,
    /// The assistant turn that asked for it.
    pub turn: TurnId,
    /// The wire's own id for the call, which the `tool` message answers by.
    pub tool_call_id: String,
    pub tool: String,
    /// The arguments, as JSON text.
    pub input: String,
    pub status: String,
    /// What it came to, or why it did not — whichever the model reads back.
    pub output: Option<String>,
    pub created: f64,
    pub ended: Option<f64>,
}

impl Call {
    /// The arguments, read. An unreadable row is an empty object rather
    /// than a panic: the column was written from a `Value` and the only way
    /// it is not one is another build's row.
    #[must_use]
    pub fn input(&self) -> Value {
        serde_json::from_str(&self.input).unwrap_or_else(|_| Value::Object(serde_json::Map::new()))
    }

    /// What the `tool` message this call answers with carries: the output
    /// where it worked, the error where it did not.
    #[must_use]
    pub fn said(&self) -> String {
        self.output.clone().unwrap_or_default()
    }
}

/// One row of the agents list: a chat with what its latest run is doing.
#[derive(Debug, Clone, PartialEq)]
pub struct ChatRow {
    pub id: ChatId,
    pub title: String,
    pub model: String,
    pub updated: f64,
    /// The latest run's word, or empty for a chat nobody has sent in.
    pub status: String,
}

// -- the entities a kick and a history node name -------------------------------

/// One chat, in the `action.entity` vocabulary.
#[must_use]
pub fn chat_entity(chat: ChatId) -> String {
    format!("chat:{chat}")
}

/// One run: the worker's kick address, the effect's entity, and what the
/// history node a *stop* records is about — one spelling everywhere.
#[must_use]
pub fn run_entity(run: RunId) -> String {
    format!("run:{run}")
}

/// What a chat is called, off the first thing said in it: its first line,
/// clipped. `chat` for a message with no words in it.
#[must_use]
pub fn title_of(text: &str) -> String {
    let line = text.lines().next().unwrap_or("").trim();
    if line.is_empty() {
        return UNTITLED.to_string();
    }
    match line.char_indices().nth(TITLE_MAX) {
        Some((at, _)) => line[..at].trim_end().to_string(),
        None => line.to_string(),
    }
}

// -- queries -------------------------------------------------------------------

static Q_CHAT: Q = Q {
    id: "chat",
    sql: "SELECT id, title, model, created, updated FROM agent_chat WHERE id = ?1",
    describe: "one conversation: its title, the model that answers in it, and when",
};

static Q_TURNS: Q = Q {
    id: "turns",
    sql: "SELECT id, chat, seq, role, body, run, created
          FROM agent_turn WHERE chat = ?1 ORDER BY seq",
    describe: "a chat's messages in order, each the wire's own message verbatim",
};

static Q_LATEST_RUN: Q = Q {
    id: "latest run",
    sql: "SELECT id, chat, status, error, started, ended, usage
          FROM agent_run WHERE chat = ?1 ORDER BY id DESC LIMIT 1",
    describe: "the newest round of the agent working on a chat",
};

static Q_RUN: Q = Q {
    id: "run",
    sql: "SELECT id, chat, status, error, started, ended, usage FROM agent_run WHERE id = ?1",
    describe: "one round of the agent working on a chat, and what it cost",
};

static Q_CHAT_RUNS: Q = Q {
    id: "chat runs",
    sql: "SELECT id, chat, status, error, started, ended, usage
          FROM agent_run WHERE chat = ?1 ORDER BY id",
    describe: "every round of the agent working on a chat, oldest first",
};

static Q_RUN_CALLS: Q = Q {
    id: "run calls",
    sql: "SELECT id, run, turn, tool_call_id, tool, input, status, output, created, ended
          FROM agent_call WHERE run = ?1 ORDER BY id",
    describe: "every tool call a run asked for, in the order the model asked",
};

static Q_LIVE_RUNS: Q = Q {
    id: "live runs",
    sql: "SELECT id, chat FROM agent_run WHERE status IN ('pending', 'waiting') ORDER BY id",
    describe: "the runs that want a worker: one not asked for yet, one holding calls",
};

static Q_CHAT_MODELS: Q = Q {
    id: "chat models",
    sql: "SELECT DISTINCT model FROM agent_chat ORDER BY model",
    describe: "the models chats have run on, for the list's `@model:` completion",
};

fn chat_row(r: &rusqlite::Row) -> rusqlite::Result<Chat> {
    Ok(Chat {
        id: r.get(0)?,
        title: r.get(1)?,
        model: r.get(2)?,
        created: r.get(3)?,
        updated: r.get(4)?,
    })
}

fn turn_row(r: &rusqlite::Row) -> rusqlite::Result<Turn> {
    let role: String = r.get(3)?;
    let body: String = r.get(4)?;
    let mut turn = decode(&body, &role);
    turn.id = r.get(0)?;
    turn.chat = r.get(1)?;
    turn.seq = r.get(2)?;
    turn.run = r.get(5)?;
    turn.created = r.get(6)?;
    Ok(turn)
}

fn run_row(r: &rusqlite::Row) -> rusqlite::Result<Run> {
    let usage: Option<String> = r.get(6)?;
    Ok(Run {
        id: r.get(0)?,
        chat: r.get(1)?,
        status: r.get(2)?,
        error: r.get(3)?,
        started: r.get(4)?,
        ended: r.get(5)?,
        usage: usage.and_then(|t| serde_json::from_str(&t).ok()),
    })
}

fn call_row(r: &rusqlite::Row) -> rusqlite::Result<Call> {
    Ok(Call {
        id: r.get(0)?,
        run: r.get(1)?,
        turn: r.get(2)?,
        tool_call_id: r.get(3)?,
        tool: r.get(4)?,
        input: r.get(5)?,
        status: r.get(6)?,
        output: r.get(7)?,
        created: r.get(8)?,
        ended: r.get(9)?,
    })
}

/// A turn's body, read. A row another build wrote in some other shape is
/// not a panic: the role column still says who spoke, and an empty message
/// is a truer answer than a crash in a draw pass.
fn decode(body: &str, role: &str) -> Turn {
    serde_json::from_str(body).unwrap_or_else(|_| Turn::new(Message::of(role_named(role))))
}

/// The role a column spells. Anything else is the model's own, and reads as
/// the assistant's.
#[must_use]
pub fn role_named(word: &str) -> Role {
    match word {
        "system" => Role::System,
        "user" => Role::User,
        "tool" => Role::Tool,
        _ => Role::Assistant,
    }
}

/// The word the `role` column keeps.
#[must_use]
pub fn role_word(role: Role) -> &'static str {
    match role {
        Role::System => "system",
        Role::User => "user",
        Role::Assistant => "assistant",
        Role::Tool => "tool",
    }
}

// -- the reads a panel draws from ----------------------------------------------

/// One conversation, or `None` for a chat that is not there — a deleted
/// one, or a panel restored from another device's session.
#[must_use]
pub fn chat(store: &Store, id: ChatId) -> Option<Chat> {
    store
        .rows(&Q_CHAT, &[Val::I(id)], chat_row)
        .first()
        .cloned()
}

/// A chat's messages, in order.
#[must_use]
pub fn turns(store: &Store, chat: ChatId) -> Rc<Vec<Turn>> {
    store.rows(&Q_TURNS, &[Val::I(chat)], turn_row)
}

/// One round, by id — what the muted line under a turn reads its cost off.
#[must_use]
pub fn run(store: &Store, id: RunId) -> Option<Run> {
    store.rows(&Q_RUN, &[Val::I(id)], run_row).first().cloned()
}

/// Every round of a chat, oldest first.
#[must_use]
pub fn runs(store: &Store, chat: ChatId) -> Rc<Vec<Run>> {
    store.rows(&Q_CHAT_RUNS, &[Val::I(chat)], run_row)
}

/// The newest round of the agent working on this chat — what the bar reads
/// to know whether anything is going on.
#[must_use]
pub fn latest_run(store: &Store, chat: ChatId) -> Option<Run> {
    store
        .rows(&Q_LATEST_RUN, &[Val::I(chat)], run_row)
        .first()
        .cloned()
}

/// Every call a run asked for, in the order the model asked.
#[must_use]
pub fn calls(store: &Store, run: RunId) -> Rc<Vec<Call>> {
    store.rows(&Q_RUN_CALLS, &[Val::I(run)], call_row)
}

/// The ones nobody has run yet.
#[must_use]
pub fn pending_calls(store: &Store, run: RunId) -> Vec<Call> {
    calls(store, run)
        .iter()
        .filter(|c| c.status == CALL_PENDING)
        .cloned()
        .collect()
}

/// The runs that want a worker, each with its chat: one that has not been
/// asked for yet, and one holding calls the chat panel will run.
#[must_use]
pub fn runs_wanting_workers(store: &Store) -> Rc<Vec<(RunId, ChatId)>> {
    store.rows(&Q_LIVE_RUNS, &[], |r| Ok((r.get(0)?, r.get(1)?)))
}

// -- the reads a worker and an effect use --------------------------------------
//
// Uncached, on a bare connection: a worker is looking for what the UI thread
// has just committed, and an effect is handed a `Ctx` with nothing but the
// reader on it.

/// One conversation, off a connection.
#[must_use]
pub fn chat_conn(c: &Connection, id: ChatId) -> Option<Chat> {
    c.query_row(Q_CHAT.sql, [id], chat_row).ok()
}

/// A chat's messages, off a connection.
#[must_use]
pub fn turns_conn(c: &Connection, chat: ChatId) -> Vec<Turn> {
    let Ok(mut stmt) = c.prepare(Q_TURNS.sql) else {
        return Vec::new();
    };
    stmt.query_map([chat], turn_row)
        .map(|it| it.filter_map(Result::ok).collect())
        .unwrap_or_default()
}

/// One run, off a connection.
#[must_use]
pub fn run_conn(c: &Connection, run: RunId) -> Option<Run> {
    c.query_row(
        "SELECT id, chat, status, error, started, ended, usage FROM agent_run WHERE id = ?1",
        [run],
        run_row,
    )
    .ok()
}

/// Every call of a run, off a connection.
#[must_use]
pub fn calls_conn(c: &Connection, run: RunId) -> Vec<Call> {
    let Ok(mut stmt) = c.prepare(Q_RUN_CALLS.sql) else {
        return Vec::new();
    };
    stmt.query_map([run], call_row)
        .map(|it| it.filter_map(Result::ok).collect())
        .unwrap_or_default()
}

/// The calls of the run's newest assistant turn: the round it is waiting on,
/// off a connection.
///
/// A run goes round as many times as the model asks for tools, and every
/// round's calls hang off the same run — so *which* calls a waiting run is
/// holding is the ones of its latest turn. The earlier rounds' were
/// answered when they were the latest, and answering them again would put a
/// second `tool` message in the conversation for a call the model has long
/// since read.
#[must_use]
pub fn round_calls_conn(c: &Connection, run: RunId) -> Vec<Call> {
    let Ok(mut stmt) = c.prepare(
        "SELECT id, run, turn, tool_call_id, tool, input, status, output, created, ended
         FROM agent_call
         WHERE run = ?1 AND turn = (SELECT MAX(turn) FROM agent_call WHERE run = ?1)
         ORDER BY id",
    ) else {
        return Vec::new();
    };
    stmt.query_map([run], call_row)
        .map(|it| it.filter_map(Result::ok).collect())
        .unwrap_or_default()
}

/// Whether the person has asked for this run to stop — read between chunks,
/// which is how *stop* cuts a stream at its next one.
///
/// A run whose row is gone counts as stopped: an undo took the send back
/// out from under it, and there is nowhere left to write the answer.
#[must_use]
pub fn is_stopped(c: &Connection, run: RunId) -> bool {
    match c.query_row("SELECT status FROM agent_run WHERE id = ?1", [run], |r| {
        r.get::<_, String>(0)
    }) {
        Ok(status) => status == STOPPED,
        Err(_) => true,
    }
}

// -- the writes ----------------------------------------------------------------

/// A new conversation. Answers its id.
///
/// # Errors
///
/// If the store refuses the write.
pub fn new_chat_tx(c: &Connection, title: &str, model: &str, now: f64) -> rusqlite::Result<ChatId> {
    c.execute(
        "INSERT INTO agent_chat(title, model, created, updated) VALUES(?1, ?2, ?3, ?3)",
        rusqlite::params![title, model, now],
    )?;
    Ok(c.last_insert_rowid())
}

/// One message, at the end of its chat: the next seq, and the chat's
/// `updated` moved to match. Answers the row's id and the seq it took.
///
/// # Errors
///
/// If the store refuses the write.
pub fn add_turn_tx(
    c: &Connection,
    chat: ChatId,
    turn: &Turn,
    now: f64,
) -> rusqlite::Result<(TurnId, i64)> {
    let seq: i64 = c.query_row(
        "SELECT COALESCE(MAX(seq), 0) + 1 FROM agent_turn WHERE chat = ?1",
        [chat],
        |r| r.get(0),
    )?;
    c.execute(
        "INSERT INTO agent_turn(chat, seq, role, body, run, created)
         VALUES(?1, ?2, ?3, ?4, ?5, ?6)",
        rusqlite::params![
            chat,
            seq,
            role_word(turn.message.role),
            turn.body(),
            turn.run,
            now
        ],
    )?;
    let id = c.last_insert_rowid();
    c.execute(
        "UPDATE agent_chat SET updated = ?2 WHERE id = ?1",
        rusqlite::params![chat, now],
    )?;
    Ok((id, seq))
}

/// A run nobody has asked for yet. Answers its id.
///
/// # Errors
///
/// If the store refuses the write.
pub fn new_run_tx(c: &Connection, chat: ChatId, now: f64) -> rusqlite::Result<RunId> {
    c.execute(
        "INSERT INTO agent_run(chat, status, started) VALUES(?1, ?2, ?3)",
        rusqlite::params![chat, PENDING, now],
    )?;
    Ok(c.last_insert_rowid())
}

/// Where a run stands. An ending status stamps `ended`; the ones on the way
/// leave it alone.
///
/// # Errors
///
/// If the store refuses the write.
pub fn set_run_status_tx(
    c: &Connection,
    run: RunId,
    status: &str,
    error: Option<&str>,
    now: f64,
) -> rusqlite::Result<()> {
    let over = matches!(status, DONE | FAILED | STOPPED);
    c.execute(
        "UPDATE agent_run SET status = ?2, error = ?3, ended = CASE WHEN ?4 THEN ?5 ELSE ended END
         WHERE id = ?1",
        rusqlite::params![run, status, error, over, now],
    )?;
    Ok(())
}

/// What the round cost, off the stream's last chunk.
///
/// # Errors
///
/// If the store refuses the write.
pub fn set_run_usage_tx(c: &Connection, run: RunId, usage: &Cost) -> rusqlite::Result<()> {
    let text = serde_json::to_string(usage).unwrap_or_else(|_| "{}".to_string());
    c.execute(
        "UPDATE agent_run SET usage = ?2 WHERE id = ?1",
        rusqlite::params![run, text],
    )?;
    Ok(())
}

/// One call the model asked for, pending. Answers its id.
///
/// # Errors
///
/// If the store refuses the write.
pub fn add_call_tx(
    c: &Connection,
    run: RunId,
    turn: TurnId,
    call: &ToolCall,
    now: f64,
) -> rusqlite::Result<CallId> {
    c.execute(
        "INSERT INTO agent_call(run, turn, tool_call_id, tool, input, status, created)
         VALUES(?1, ?2, ?3, ?4, ?5, ?6, ?7)",
        rusqlite::params![
            run,
            turn,
            call.id,
            call.function.name,
            call.function.arguments,
            CALL_PENDING,
            now
        ],
    )?;
    Ok(c.last_insert_rowid())
}

/// What a call came to, or why it did not.
///
/// # Errors
///
/// If the store refuses the write.
pub fn set_call_tx(
    c: &Connection,
    call: CallId,
    status: &str,
    output: &str,
    now: f64,
) -> rusqlite::Result<()> {
    c.execute(
        "UPDATE agent_call SET status = ?2, output = ?3, ended = ?4 WHERE id = ?1",
        rusqlite::params![call, status, output, now],
    )?;
    Ok(())
}

/// Everything of this chat from `seq` on: the later turns, and the runs and
/// calls that came with them.
///
/// # Errors
///
/// If the store refuses the write.
fn cut_back_tx(c: &Connection, chat: ChatId, seq: i64, run: RunId) -> rusqlite::Result<()> {
    c.execute(
        "DELETE FROM agent_call WHERE run IN
           (SELECT id FROM agent_run WHERE chat = ?1 AND id >= ?2)",
        rusqlite::params![chat, run],
    )?;
    c.execute(
        "DELETE FROM agent_run WHERE chat = ?1 AND id >= ?2",
        rusqlite::params![chat, run],
    )?;
    c.execute(
        "DELETE FROM agent_turn WHERE chat = ?1 AND seq >= ?2",
        rusqlite::params![chat, seq],
    )?;
    Ok(())
}

// -- the actions ----------------------------------------------------------------

/// The person says something: the chat if there is not one yet, their turn,
/// and a run nobody has asked for. One action, and [`Session::act`] kicks
/// the workers, which is what starts it.
///
/// Answers the chat and the run, or `None` for an empty message and for a
/// device that may not write.
pub fn send(s: &mut Session, chat: Option<ChatId>, text: &str) -> Option<(ChatId, RunId)> {
    let said = text.trim().to_string();
    if said.is_empty() {
        return None;
    }
    let now = s.now();
    let title = title_of(&said);
    let label = format!("send “{title}”");
    let (model, body) = (MODEL.to_string(), said.clone());
    let mut act = Action::writing("agent.send", label, move |tx| {
        let chat = match chat {
            Some(id) => id,
            None => new_chat_tx(tx, &title, &model, now)?,
        };
        // The run before the turn, so the person's message records which
        // round it started — which is what the transcript groups by.
        let run = new_run_tx(tx, chat, now)?;
        let (_, seq) = add_turn_tx(tx, chat, &Turn::new(Message::user(body)).by(run), now)?;
        Ok((chat, run, seq))
    });
    if let Some(id) = chat {
        act = act.about(chat_entity(id));
    }
    let (chat, run, seq) = s.act(act)?;
    s.claim(Box::new(Sent {
        chat,
        seq,
        text: said,
        run: Cell::new(run),
    }));
    Some((chat, run))
}

/// *stop*: the run's status, which the worker reads between chunks and the
/// stream answers at its next one.
pub fn stop(s: &mut Session, run: RunId) {
    let now = s.now();
    s.act(
        Action::writing("agent.stop", "stop the agent", move |tx| {
            set_run_status_tx(tx, run, STOPPED, None, now)
        })
        .about(run_entity(run)),
    );
}

/// *retry*: another round of the same chat, from the turns it already has.
/// The failed run stays where it is — what went wrong is worth reading —
/// and a fresh pending one is filed beside it.
///
/// Answers the new run, or `None` for a chat with nothing to retry.
pub fn retry(s: &mut Session, chat: ChatId) -> Option<RunId> {
    let last = latest_run(s.store(), chat)?;
    if !matches!(last.status.as_str(), FAILED | STOPPED) {
        return None;
    }
    let now = s.now();
    s.act(
        Action::writing("agent.retry", "ask again", move |tx| {
            new_run_tx(tx, chat, now)
        })
        .about(chat_entity(chat)),
    )
}

/// *delete n*: the chats with their turns, their runs and their calls, one
/// node, and the rows kept on it so undo puts them back exactly as they
/// were.
///
/// Answers whether anything was written — a device that may not write says
/// so with a toast and changes nothing.
pub fn delete_chats(s: &mut Session, ids: &[ChatId]) -> bool {
    if ids.is_empty() {
        return false;
    }
    let kept = Kept::of(s.store(), ids);
    let label = match ids {
        [_] => "delete 1 chat".to_string(),
        many => format!("delete {} chats", many.len()),
    };
    let gone = ids.to_vec();
    let done = s.act(
        Action::writing("agent.delete", label, move |tx| {
            for chat in &gone {
                delete_chat_tx(tx, *chat)?;
            }
            Ok(())
        })
        .claiming(vec![Box::new(kept) as Box<dyn Intent>]),
    );
    if done.is_none() {
        return false;
    }
    s.notify(
        match ids {
            [_] => "deleted 1 chat".to_string(),
            many => format!("deleted {} chats", many.len()),
        },
        false,
    );
    true
}

/// One chat and everything that hangs off it.
///
/// # Errors
///
/// If the store refuses the write.
fn delete_chat_tx(c: &Connection, chat: ChatId) -> rusqlite::Result<()> {
    c.execute(
        "DELETE FROM agent_call WHERE run IN (SELECT id FROM agent_run WHERE chat = ?1)",
        [chat],
    )?;
    c.execute("DELETE FROM agent_run WHERE chat = ?1", [chat])?;
    c.execute("DELETE FROM agent_turn WHERE chat = ?1", [chat])?;
    c.execute("DELETE FROM agent_chat WHERE id = ?1", [chat])?;
    Ok(())
}

// -- what an action claimed of the world ---------------------------------------

/// A send: the person's turn, and the run it started.
///
/// Undoing a send is *the chat as it was before you sent* — the turn goes,
/// and so does everything that came after it, since an answer to a question
/// nobody asked is not worth keeping. A run still in flight is told to stop
/// before its rows go out from under it: the worker reads that status
/// between chunks.
///
/// Redo files the turn again and a fresh pending run, which the kick after
/// a walk sets going. The run's id changes each time, so the cell is what
/// the next undo takes back.
struct Sent {
    chat: ChatId,
    seq: i64,
    text: String,
    run: Cell<RunId>,
}

impl Intent for Sent {
    fn describe(&self) -> String {
        format!("chat:{} said “{}”", self.chat, title_of(&self.text))
    }

    fn reverse(&self, w: &World) -> Result<(), String> {
        let (chat, seq, run) = (self.chat, self.seq, self.run.get());
        // Two writes on purpose: the stop has to be *visible* to a worker
        // that is mid-stream on another thread, and a status set and
        // deleted inside one transaction never was.
        w.store()
            .write(move |c| {
                c.execute(
                    "UPDATE agent_run SET status = ?3 WHERE chat = ?1 AND id >= ?2
                       AND status IN ('pending', 'streaming', 'waiting')",
                    rusqlite::params![chat, run, STOPPED],
                )
                .map(|_| ())
            })
            .map_err(|e| e.to_string())?;
        w.store()
            .write(move |c| cut_back_tx(c, chat, seq, run))
            .map_err(|e| e.to_string())
    }

    fn reapply(&self, w: &World) -> Result<(), String> {
        let (chat, text, now) = (self.chat, self.text.clone(), w.now());
        let run = w
            .store()
            .write(move |c| {
                let run = new_run_tx(c, chat, now)?;
                add_turn_tx(c, chat, &Turn::new(Message::user(text)).by(run), now)?;
                Ok(run)
            })
            .map_err(|e| e.to_string())?;
        self.run.set(run);
        Ok(())
    }
}

/// The chats a *delete* took, whole: the rows themselves, so undo puts them
/// back under the ids the turns and the calls still name.
struct Kept {
    chats: Vec<Chat>,
    turns: Vec<Turn>,
    runs: Vec<Run>,
    calls: Vec<Call>,
}

impl Kept {
    /// Everything hanging off these chats, read before the write that takes
    /// it away.
    fn of(store: &Store, ids: &[ChatId]) -> Kept {
        let mut kept = Kept {
            chats: Vec::new(),
            turns: Vec::new(),
            runs: Vec::new(),
            calls: Vec::new(),
        };
        for id in ids {
            if let Some(c) = chat(store, *id) {
                kept.chats.push(c);
            }
            kept.turns.extend(turns(store, *id).iter().cloned());
            for r in runs(store, *id).iter() {
                kept.calls.extend(calls(store, r.id).iter().cloned());
                kept.runs.push(r.clone());
            }
        }
        kept
    }
}

impl Intent for Kept {
    fn describe(&self) -> String {
        match self.chats.as_slice() {
            [one] => format!("chat:{} deleted", one.id),
            many => format!("{} chats deleted", many.len()),
        }
    }

    fn reverse(&self, w: &World) -> Result<(), String> {
        let (chats, turns) = (self.chats.clone(), self.turns.clone());
        let (runs, calls) = (self.runs.clone(), self.calls.clone());
        w.store()
            .write(move |c| {
                for chat in &chats {
                    c.execute(
                        "INSERT OR REPLACE INTO agent_chat(id, title, model, created, updated)
                         VALUES(?1, ?2, ?3, ?4, ?5)",
                        rusqlite::params![
                            chat.id,
                            chat.title,
                            chat.model,
                            chat.created,
                            chat.updated
                        ],
                    )?;
                }
                for t in &turns {
                    c.execute(
                        "INSERT OR REPLACE INTO agent_turn(id, chat, seq, role, body, run, created)
                         VALUES(?1, ?2, ?3, ?4, ?5, ?6, ?7)",
                        rusqlite::params![
                            t.id,
                            t.chat,
                            t.seq,
                            role_word(t.message.role),
                            t.body(),
                            t.run,
                            t.created
                        ],
                    )?;
                }
                for r in &runs {
                    let usage = r.usage.as_ref().and_then(|u| serde_json::to_string(u).ok());
                    c.execute(
                        "INSERT OR REPLACE INTO
                           agent_run(id, chat, status, error, started, ended, usage)
                         VALUES(?1, ?2, ?3, ?4, ?5, ?6, ?7)",
                        rusqlite::params![
                            r.id, r.chat, r.status, r.error, r.started, r.ended, usage
                        ],
                    )?;
                }
                for call in &calls {
                    c.execute(
                        "INSERT OR REPLACE INTO agent_call(id, run, turn, tool_call_id, tool,
                                                           input, status, output, created, ended)
                         VALUES(?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
                        rusqlite::params![
                            call.id,
                            call.run,
                            call.turn,
                            call.tool_call_id,
                            call.tool,
                            call.input,
                            call.status,
                            call.output,
                            call.created,
                            call.ended
                        ],
                    )?;
                }
                Ok(())
            })
            .map_err(|e| e.to_string())
    }

    fn reapply(&self, w: &World) -> Result<(), String> {
        let ids: Vec<ChatId> = self.chats.iter().map(|c| c.id).collect();
        w.store()
            .write(move |c| {
                for id in &ids {
                    delete_chat_tx(c, *id)?;
                }
                Ok(())
            })
            .map_err(|e| e.to_string())
    }
}

// -- the agents list as a rich table --------------------------------------------

/// The chats as a rich table: the title, the model, when it last moved, and
/// what its newest run is doing — so the list is where one finds the chat
/// that stopped short.
///
/// The run's word is a correlated subquery rather than a join, because a
/// chat has as many runs as it has had questions and the list wants one
/// word: the newest. Free text reads the title and every turn's body, which
/// is where the words a person remembers actually are.
static CHATS_SPEC: SqlSpec = SqlSpec {
    id: "chats",
    describe: "the chats under the panel's filter, latest first, one page at a time",
    select: "c.id, c.title, c.model, c.updated,
             COALESCE((SELECT r.status FROM agent_run r
                        WHERE r.chat = c.id ORDER BY r.id DESC LIMIT 1), '')",
    from: "agent_chat c",
    base: "",
    text: &[
        "c.title",
        "COALESCE((SELECT GROUP_CONCAT(t.body, ' ') FROM agent_turn t WHERE t.chat = c.id), '')",
    ],
    tags: &[
        (
            "waiting",
            TagSql::Where(
                "(SELECT r.status FROM agent_run r
                   WHERE r.chat = c.id ORDER BY r.id DESC LIMIT 1) = 'waiting'",
            ),
        ),
        (
            "failed",
            TagSql::Where(
                "(SELECT r.status FROM agent_run r
                   WHERE r.chat = c.id ORDER BY r.id DESC LIMIT 1) = 'failed'",
            ),
        ),
        ("model", TagSql::Col("c.model")),
        ("date", TagSql::Col("c.updated")),
    ],
    order: &[("c.updated", Dir::Desc), ("c.id", Dir::Desc)],
    group: None,
    key: "c.id",
    deps: &[],
};

const DATE_OPS: &[Op] = &[Op::Eq, Op::Gt, Op::Gte, Op::Lt, Op::Lte];

/// What `@` offers in the agents list.
static CHATS_TAGS: &[TagDef] = &[
    TagDef {
        name: "waiting",
        kind: TagType::Bool,
        ops: &[],
        describe: "holding a call the chat will run when it is next shown",
        values: Values::None,
    },
    TagDef {
        name: "failed",
        kind: TagType::Bool,
        ops: &[],
        describe: "the last round did not come to an answer",
        values: Values::None,
    },
    TagDef {
        name: "model",
        kind: TagType::Text,
        ops: &[Op::Eq],
        describe: "which model answered",
        values: Values::Dynamic,
    },
    TagDef {
        name: "date",
        kind: TagType::Date,
        ops: DATE_OPS,
        describe: "a day, 30.08.2026",
        values: Values::None,
    },
];

/// The values `@model:` completes against: the models chats have actually
/// run on, which is a handful of rows and one cached query.
fn suggest_chats(store: &Store, tag: &str, typed: &str) -> Vec<Suggestion> {
    if tag != "model" {
        return Vec::new();
    }
    store
        .rows(&Q_CHAT_MODELS, &[], |r| r.get::<_, String>(0))
        .iter()
        .filter(|m| m.to_lowercase().contains(typed))
        .map(|m| Suggestion::value(m.clone()))
        .collect()
}

/// The datasource the agents list pages through.
pub static CHATS: SqlSource<ChatRow, i64> = SqlSource {
    spec: &CHATS_SPEC,
    tags: CHATS_TAGS,
    map: chat_list_row,
    key: |c| c.id,
    rank: |c| vec![Val::F(c.updated), Val::I(c.id)],
    suggest: suggest_chats,
};

fn chat_list_row(r: &rusqlite::Row) -> rusqlite::Result<ChatRow> {
    Ok(ChatRow {
        id: r.get(0)?,
        title: r.get(1)?,
        model: r.get(2)?,
        updated: r.get(3)?,
        status: r.get(4)?,
    })
}
