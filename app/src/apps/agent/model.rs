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
/// A call of a tool that [asks](kernel::tool::Tool::asks), waiting for the
/// person's word: its card wears *allow* and *refuse*, and the calls behind
/// it in the round wait with it.
pub const CALL_ASKED: &str = "asked";
/// A call that answered.
pub const CALL_DONE: &str = "done";
/// A call the *tool* would not have — a name no app offers, arguments the
/// schema refuses, a verb that could not do it — whose error is what the
/// model reads. What the *person* would not have is [`CALL_REFUSED`].
pub const CALL_FAILED: &str = "failed";
/// A call the person would not have. It never ran, and what the model reads
/// back is [`REFUSED_SAID`].
pub const CALL_REFUSED: &str = "refused";
/// A call the round was stopped out from under. Nobody refused it and
/// nobody ran it; the next request settles it so the transcript it carries
/// is one the wire will take, and what the model reads back is
/// [`CANCELLED_SAID`].
pub const CALL_CANCELLED: &str = "cancelled";

/// What a refused call answers the model with — the whole of the sentence,
/// because nothing was done and there is nothing else to say about it.
pub const REFUSED_SAID: &str = "refused by the person";
/// The same for a call the stop caught before anybody had a word about it.
pub const CANCELLED_SAID: &str = "cancelled: the round was stopped";

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

/// What a turn carries besides its words.
///
/// Two readings of one thing: `chips` is how the transcript draws the
/// context back, and `context` is how the model was told it. They are kept
/// apart because different halves read them — a widget the first, a request
/// the second — and because a chip renders through a session, which the
/// thread that builds a request has not got.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Carried {
    pub chips: Vec<Value>,
    pub context: Option<String>,
}

/// One message, as its row keeps it and as the next request sends it.
///
/// The wire's own message is flattened in, because that is what `body`
/// holds — verbatim, so the next request is built from the rows and a
/// `sqlite3` reader can still read them — with the app's own keys beside
/// it: `chips` and `context`, what a turn carried and what it rendered to,
/// and `finish`, the word the model stopped on.
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
    /// The context this turn carried, as chips — what the transcript draws
    /// back as pills, one [`Chip::to_json`](super::chip::Chip::to_json)
    /// apiece.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub chips: Vec<Value>,
    /// What those chips rendered to for the model, as the request carries
    /// it. Kept on the turn rather than rebuilt per request, because a chip
    /// renders through a [`Session`] — the open panels, the effect registry
    /// — and the thread that builds a request has neither: it has a reader.
    /// So the render happens once, on the UI thread, at send time, with the
    /// rows the panel was showing then.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub context: Option<String>,
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
            context: None,
            finish: None,
        }
    }

    /// The same, carrying context: the chips as the transcript reads them
    /// back, and what they rendered to for the model.
    #[must_use]
    pub fn carrying(mut self, carried: &Carried) -> Turn {
        self.chips.clone_from(&carried.chips);
        self.context.clone_from(&carried.context);
        self
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
    /// The sentence the history shows for what this call did, on a writing
    /// tool that filed a node — *rename “README.txt” to
    /// “readme-renamed.txt”*. `None` for a reading tool, a refusal, and a
    /// writing tool that changed nothing. It is the card's line and nothing
    /// else: what the model reads back is the tool's own JSON.
    pub label: Option<String>,
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
    /// where it worked, the error where it did not, and — for a call the
    /// person would not have — [`REFUSED_SAID`], so the model reads that it
    /// was refused rather than that nothing happened.
    #[must_use]
    pub fn said(&self) -> String {
        match self.status.as_str() {
            CALL_REFUSED => REFUSED_SAID.to_string(),
            CALL_CANCELLED => CANCELLED_SAID.to_string(),
            _ => self.output.clone().unwrap_or_default(),
        }
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
    sql: "SELECT id, run, turn, tool_call_id, tool, input, status, output, label, created, ended
          FROM agent_call WHERE run = ?1 ORDER BY id",
    describe: "every tool call a run asked for, in the order the model asked",
};

static Q_LIVE_RUNS: Q = Q {
    id: "live runs",
    sql: "SELECT id, chat FROM agent_run
          WHERE status IN ('pending', 'streaming', 'waiting') ORDER BY id",
    describe: "the runs that want a worker: one not asked for yet, one being answered, \
               one holding calls",
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
        label: r.get(8)?,
        created: r.get(9)?,
        ended: r.get(10)?,
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

/// The ones waiting on the person: a tool that asks, standing at its card
/// until it is allowed or refused. One at a time in practice — the walk
/// stops at the first — but a list, because the round is what the panel
/// reads.
#[must_use]
pub fn asked_calls(store: &Store, run: RunId) -> Vec<Call> {
    calls(store, run)
        .iter()
        .filter(|c| c.status == CALL_ASKED)
        .cloned()
        .collect()
}

/// The runs that want a worker, each with its chat: one that has not been
/// asked for yet, one whose answer is arriving, and one holding calls the
/// chat panel will run — [`LIVE`], in one query.
///
/// A `streaming` run is in the list because the set is diffed after **every**
/// action: any write at all while the gateway streams would otherwise retire
/// the very worker that is reading it, closing the kick channel it needs to
/// be woken by when its calls come back.
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
        "SELECT id, run, turn, tool_call_id, tool, input, status, output, label, created, ended
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

/// Whether the run a worker started for is still there, still this chat's,
/// and its chat still there too — asked **inside** the worker's own write,
/// which is the only place the answer cannot go stale between the asking
/// and the writing.
///
/// A worker is inside the gateway for as long as an answer takes, and an
/// undo or a *delete* on the UI thread can take its rows away while it is.
/// Without this the answer lands anyway: an assistant turn for a run that no
/// longer exists, in a chat that may not either. The id is enough to say so
/// because a run id is `AUTOINCREMENT` and never comes back.
///
/// # Errors
///
/// If the read fails.
pub fn run_alive_tx(c: &Connection, run: RunId, chat: ChatId) -> rusqlite::Result<bool> {
    c.query_row(
        "SELECT EXISTS(SELECT 1 FROM agent_run r JOIN agent_chat c ON c.id = r.chat
                        WHERE r.id = ?1 AND r.chat = ?2)",
        rusqlite::params![run, chat],
        |r| r.get::<_, i64>(0),
    )
    .map(|n| n == 1)
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

/// A call put to the person: the status alone moves.
///
/// Nothing else about the row does, because nothing else has happened —
/// it has run nothing, claimed nothing, and has not ended: `ended` is the
/// moment the person answers, which [`set_call_tx`] writes then.
///
/// # Errors
///
/// If the store refuses the write.
pub fn ask_call_tx(c: &Connection, call: CallId) -> rusqlite::Result<()> {
    c.execute(
        "UPDATE agent_call SET status = ?2 WHERE id = ?1",
        rusqlite::params![call, CALL_ASKED],
    )?;
    Ok(())
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
    label: Option<&str>,
    now: f64,
) -> rusqlite::Result<()> {
    c.execute(
        "UPDATE agent_call SET status = ?2, output = ?3, label = ?4, ended = ?5 WHERE id = ?1",
        rusqlite::params![call, status, output, label, now],
    )?;
    Ok(())
}

/// The round the last request left open, closed off — one `tool` turn per
/// call the newest assistant turn asked for, in the order it asked.
///
/// A stop lands between the calls being written and their results being
/// written, and there is nothing to pick the round back up: the assistant
/// turn with its `tool_calls` stays, and the `tool` turns that answer them
/// never come. The next request would then carry a `tool_calls` message with
/// no `tool` messages after it, which is invalid on the wire — and the
/// results of the calls that *did* run would be lost from the model's
/// context, so it would ask for them again.
///
/// So every request a person starts settles first. A call that ran answers
/// with what it came to; a call that never got a word — `pending` or
/// `asked` at the card — is [cancelled](CALL_CANCELLED), row and message
/// both, because a person's stop is a fact the model should read rather than
/// a silence it has to guess at.
///
/// Nothing to settle is the ordinary case and costs one walk of the turns.
///
/// # Errors
///
/// If the store refuses the write.
pub fn settle_round_tx(c: &Connection, chat: ChatId, now: f64) -> rusqlite::Result<()> {
    let turns = turns_conn(c, chat);
    let Some(at) = turns
        .iter()
        .rposition(|t| t.message.role == Role::Assistant && !t.message.tool_calls.is_empty())
    else {
        return Ok(());
    };
    let asked = &turns[at].message.tool_calls;
    let answered: Vec<&str> = turns[at + 1..]
        .iter()
        .filter_map(|t| t.message.tool_call_id.as_deref())
        .collect();
    let run = turns[at].run;
    for call in asked {
        if answered.contains(&call.id.as_str()) {
            continue;
        }
        // The row is where the outcome is: what a call that ran came to,
        // and — for one nobody answered — the word for that, written now so
        // the card and the message say the same thing. By the turn as well
        // as the id, because a `tool_call_id` is the provider's own and two
        // chats are handed the same one soon enough.
        let row: Option<Call> = c
            .query_row(
                "SELECT id, run, turn, tool_call_id, tool, input, status, output, label,
                        created, ended
                 FROM agent_call WHERE turn = ?1 AND tool_call_id = ?2",
                rusqlite::params![turns[at].id, call.id],
                call_row,
            )
            .ok();
        let said = match &row {
            Some(r) if matches!(r.status.as_str(), CALL_PENDING | CALL_ASKED) => {
                set_call_tx(c, r.id, CALL_CANCELLED, "", None, now)?;
                CANCELLED_SAID.to_string()
            }
            Some(r) => r.said(),
            // No row at all: the model asked and nothing was ever filed for
            // it. The cancellation is still the truth of it.
            None => CANCELLED_SAID.to_string(),
        };
        let mut turn = Turn::new(Message::tool(&call.id, said));
        if let Some(run) = run {
            turn = turn.by(run);
        }
        add_turn_tx(c, chat, &turn, now)?;
    }
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
/// `carried` is the context the composer held — the chips, and the text
/// they rendered to at this moment, which is what the request carries.
///
/// Answers the chat and the run, or `None` for a message with neither words
/// nor context in it, and for a device that may not write.
pub fn send(
    s: &mut Session,
    chat: Option<ChatId>,
    text: &str,
    carried: Carried,
) -> Option<(ChatId, RunId)> {
    let said = text.trim().to_string();
    if said.is_empty() && carried.chips.is_empty() {
        return None;
    }
    let now = s.now();
    let title = title_of(&said);
    let label = format!("send “{title}”");
    let (model, body) = (MODEL.to_string(), said.clone());
    let held = carried.clone();
    let mut act = Action::writing("agent.send", label, move |tx| {
        let chat = match chat {
            Some(id) => id,
            None => new_chat_tx(tx, &title, &model, now)?,
        };
        // Whatever the last round left open is closed before this one
        // starts: a request must not carry a `tool_calls` message with no
        // answers behind it.
        settle_round_tx(tx, chat, now)?;
        // The run before the turn, so the person's message records which
        // round it started — which is what the transcript groups by.
        let run = new_run_tx(tx, chat, now)?;
        let turn = Turn::new(Message::user(body)).carrying(&held).by(run);
        let (_, seq) = add_turn_tx(tx, chat, &turn, now)?;
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
        carried,
        run: Cell::new(run),
    }));
    Some((chat, run))
}

/// *stop*: the run's status, which the worker reads between chunks and the
/// stream answers at its next one.
///
/// It claims [`Stopped`], which refuses: a stop is a node the history shows
/// and undo walks past, because there is no request left to un-stop.
pub fn stop(s: &mut Session, run: RunId) {
    let now = s.now();
    s.act(
        Action::writing("agent.stop", "stop the agent", move |tx| {
            set_run_status_tx(tx, run, STOPPED, None, now)
        })
        .about(run_entity(run))
        .claiming(vec![Box::new(Stopped { run }) as Box<dyn Intent>]),
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
    let run = s.act(
        Action::writing("agent.retry", "ask again", move |tx| {
            // As a send does, and for the same reason: the round the stop
            // or the failure left open is closed before this one is filed.
            settle_round_tx(tx, chat, now)?;
            new_run_tx(tx, chat, now)
        })
        .about(chat_entity(chat)),
    )?;
    s.claim(Box::new(Retried {
        chat,
        run: Cell::new(run),
    }));
    Some(run)
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
    /// What the composer held when it went. Redo files the same turn,
    /// context and all: rendering the chips again would ask the model about
    /// a workspace that has moved on since.
    carried: Carried,
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
        let carried = self.carried.clone();
        let run = w
            .store()
            .write(move |c| {
                let run = new_run_tx(c, chat, now)?;
                let turn = Turn::new(Message::user(text)).carrying(&carried).by(run);
                add_turn_tx(c, chat, &turn, now)?;
                Ok(run)
            })
            .map_err(|e| e.to_string())?;
        self.run.set(run);
        Ok(())
    }
}

/// A *stop*, which refuses to be given back.
///
/// The request is gone: the stream was cut, the gateway has been paid, and
/// there is no way to ask for the rest of an answer it already sent. So the
/// node is honest about it rather than silent — it claims something whose
/// [`blocked`](Intent::blocked) always answers, the way mail's `Sent`
/// refuses once the letter has left. `cmd+z` walks past it to the send
/// underneath and says what it really undid; the history still shows that
/// the round was stopped, which is worth reading.
///
/// The alternative was no node at all. This is the better one: a person who
/// pressed *stop* did something, and an action that leaves no trace in the
/// tree is the tree lying by omission.
struct Stopped {
    run: RunId,
}

impl Intent for Stopped {
    fn describe(&self) -> String {
        format!("run:{} stopped", self.run)
    }

    fn blocked(&self, _w: &World) -> Option<String> {
        Some("a stopped round cannot be resumed — retry asks again".to_string())
    }

    /// Never reached: `blocked` answers first, for every walk in either
    /// direction, and a blocked node is expired rather than reversed.
    fn reverse(&self, _w: &World) -> Result<(), String> {
        Ok(())
    }

    fn reapply(&self, _w: &World) -> Result<(), String> {
        Ok(())
    }
}

/// A *retry*: the round it filed, and everything that round went on to say.
///
/// Mirrors [`Sent`], because it is the same act with the question left out.
/// Undoing it is *the chat as it was before you asked again* — the run goes,
/// and with it the turns it wrote and the calls it asked for; redo files a
/// fresh pending run, whose id is a new one, so the cell is what the next
/// undo takes back.
///
/// What [`settle_round_tx`] wrote in the same transaction stays, exactly as
/// it does under an undone send: those turns belong to the round *before*
/// this one and answer for it, and taking them back would leave the
/// transcript in the shape no request may carry.
struct Retried {
    chat: ChatId,
    run: Cell<RunId>,
}

impl Intent for Retried {
    fn describe(&self) -> String {
        format!("chat:{} asked again", self.chat)
    }

    fn reverse(&self, w: &World) -> Result<(), String> {
        let run = self.run.get();
        // Two writes, as a send's are: the stop has to be visible to a
        // worker mid-stream on another thread, and a status set and deleted
        // inside one transaction never was.
        w.store()
            .write(move |c| {
                c.execute(
                    "UPDATE agent_run SET status = ?2 WHERE id = ?1
                       AND status IN ('pending', 'streaming', 'waiting')",
                    rusqlite::params![run, STOPPED],
                )
                .map(|_| ())
            })
            .map_err(|e| e.to_string())?;
        w.store()
            .write(move |c| {
                c.execute("DELETE FROM agent_call WHERE run = ?1", [run])?;
                c.execute("DELETE FROM agent_turn WHERE run = ?1", [run])?;
                c.execute("DELETE FROM agent_run WHERE id = ?1", [run])?;
                Ok(())
            })
            .map_err(|e| e.to_string())
    }

    fn reapply(&self, w: &World) -> Result<(), String> {
        let (chat, now) = (self.chat, w.now());
        let run = w
            .store()
            .write(move |c| new_run_tx(c, chat, now))
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
                    // A run that was streaming when the chat went comes
                    // back stopped. Its worker retired with the row and
                    // there is no asking the gateway for the rest of an
                    // answer it already sent, so *streaming* would be a
                    // word about nobody — the chat offers *retry* instead.
                    // `pending` and `waiting` come back as they were: the
                    // walk's own `kick_all` asks for their workers again.
                    // The stamp is the run's own start, the only moment
                    // this restore can honestly name.
                    let streaming = r.status == STREAMING;
                    let status: &str = if streaming {
                        STOPPED
                    } else {
                        r.status.as_str()
                    };
                    let ended = if streaming {
                        r.ended.or(Some(r.started))
                    } else {
                        r.ended
                    };
                    c.execute(
                        "INSERT OR REPLACE INTO
                           agent_run(id, chat, status, error, started, ended, usage)
                         VALUES(?1, ?2, ?3, ?4, ?5, ?6, ?7)",
                        rusqlite::params![r.id, r.chat, status, r.error, r.started, ended, usage],
                    )?;
                }
                for call in &calls {
                    c.execute(
                        "INSERT OR REPLACE INTO agent_call(id, run, turn, tool_call_id, tool,
                                                           input, status, output, label,
                                                           created, ended)
                         VALUES(?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)",
                        rusqlite::params![
                            call.id,
                            call.run,
                            call.turn,
                            call.tool_call_id,
                            call.tool,
                            call.input,
                            call.status,
                            call.output,
                            call.label,
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
