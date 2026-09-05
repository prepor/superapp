//! The agent's tables, from version one: a chat, its turns, its runs, and
//! the calls a run asked for.
//!
//! Every one of them has an `INTEGER PRIMARY KEY`, because device sync
//! records a table by its primary key and a table without one replicates
//! nothing — a chat continued on the phone is the same chat only if its
//! turns actually travel. A run's key is `AUTOINCREMENT` as well, which is
//! the one place in this tree where the extra row of bookkeeping is worth
//! it: a plain `INTEGER PRIMARY KEY` hands the highest deleted id out
//! again, and a run id is what a worker still inside the gateway compares
//! itself against.
//!
//! A turn's `body` is the wire's message, stored as text: the next request
//! is built from the rows verbatim, and `sqlite3` can still read them. The
//! app's own two keys ride in the same object — `chips`, empty until phase
//! two, and `finish`, the word the model stopped on.
//!
//! Nothing about the gateway is a row. Its token is device sync's, and a
//! secret never goes in the store: it is the one thing that must not
//! replicate.

use kernel::app::{Schema, Step};

/// The agent's ladder. One rung of tables, a sweep that runs at every open
/// — a run that was streaming when the process died has no worker coming
/// back for it and no job in the queue, so this is the only moment anybody
/// can say so — and the rung that put the runs on a key that never comes
/// back.
///
/// **A new rung goes at the foot, after the sweep, never before it.** The
/// sweep holds a place on the ladder like any other rung and the counter
/// records it: a store that has climbed to 2 would skip a step inserted at
/// 2 for ever. [`Step::Always`] says as much — *a step added after it is
/// still a step this store has not climbed*.
pub static SCHEMA: Schema = Schema {
    app: "agent",
    steps: &[Step::Sql(V1), Step::Always(sweep), Step::Sql(V2)],
};

const V1: &str = "
CREATE TABLE agent_chat(
  id      INTEGER PRIMARY KEY,
  -- The first line of the first thing the person said, until they rename
  -- it; `chat` before that.
  title   TEXT NOT NULL DEFAULT '',
  -- Which model answered. The const is a commit, not a setting, so a chat
  -- that predates a change still says what answered it.
  model   TEXT NOT NULL,
  created REAL NOT NULL,
  updated REAL NOT NULL
);

-- One message: the wire's own, plus which run wrote it.
CREATE TABLE agent_turn(
  id      INTEGER PRIMARY KEY,
  chat    INTEGER NOT NULL,
  -- Its place in the conversation, from one. Unique with the chat, which
  -- is what makes `seq` the thing an undo cuts back to.
  seq     INTEGER NOT NULL,
  -- `user`, `assistant`, `tool` — the wire's word, kept out of the JSON as
  -- a column too, so a query can ask without parsing.
  role    TEXT NOT NULL,
  -- The wire's `Message`, verbatim, plus `chips` and `finish`.
  body    TEXT NOT NULL,
  -- The run that wrote it, or the run the person's message started.
  run     INTEGER,
  created REAL NOT NULL,
  UNIQUE(chat, seq)
);
CREATE INDEX idx_agent_turn_chat ON agent_turn(chat, seq);

-- One round of the agent working on a chat.
CREATE TABLE agent_run(
  id      INTEGER PRIMARY KEY,
  chat    INTEGER NOT NULL,
  -- pending · streaming · waiting · done · failed · stopped
  status  TEXT NOT NULL,
  -- The gateway's sentence, on a run that failed.
  error   TEXT,
  started REAL NOT NULL,
  ended   REAL,
  -- What the turn cost, as JSON: {in, out, cached}.
  usage   TEXT
);
CREATE INDEX idx_agent_run_chat ON agent_run(chat, id);

-- One use of a tool inside a run: a row, so the chat panel can run it on
-- the UI thread and the transcript can show what it came to.
CREATE TABLE agent_call(
  id           INTEGER PRIMARY KEY,
  run          INTEGER NOT NULL,
  -- The assistant turn that asked for it.
  turn         INTEGER NOT NULL,
  -- The wire's own id for the call, which the `tool` message answers by.
  tool_call_id TEXT NOT NULL,
  tool         TEXT NOT NULL,
  -- The arguments, as JSON.
  input        TEXT NOT NULL,
  -- pending · asked · done · failed · refused. A call of a tool that asks
  -- waits at `asked` until the person allows or refuses it.
  status       TEXT NOT NULL,
  -- What it came to, or why it did not, whichever the model reads back. A
  -- call the person refused has none: the sentence it answers with is the
  -- app's own word for a refusal.
  output       TEXT,
  -- The sentence the history shows for what a writing tool did — *rename
  -- “README.txt” to “readme-renamed.txt”* — read off the node the tool
  -- filed. The card says this where there is one; the model never sees it,
  -- because the model reads the tool's own JSON.
  label        TEXT,
  created      REAL NOT NULL,
  ended        REAL
);
CREATE INDEX idx_agent_call_run ON agent_call(run, id);
";

/// The runs, rebuilt on a key that never comes back.
///
/// A plain `INTEGER PRIMARY KEY` is `rowid`, and SQLite hands the highest
/// deleted one out again: undo a send while its run is streaming, redo it,
/// and the fresh run takes the id the old one had. The worker still inside
/// the gateway reads that row for its stop check, finds a run that is very
/// much alive, and appends its answer and its calls to the round that
/// replaced it. `AUTOINCREMENT` is the whole of the fix — an id this store
/// has once handed out is never handed out again.
///
/// The table is rebuilt rather than altered, because a key's kind is not
/// something `ALTER TABLE` changes. Every row is carried across under its
/// own id, and the insert leaves `sqlite_sequence` standing at the highest
/// of them, so the next run follows the last. `sqlite_sequence` itself is
/// SQLite's own: replication skips every `sqlite_*` name, so no device ever
/// receives another's counter.
const V2: &str = "
CREATE TABLE agent_run_next(
  id      INTEGER PRIMARY KEY AUTOINCREMENT,
  chat    INTEGER NOT NULL,
  -- pending · streaming · waiting · done · failed · stopped
  status  TEXT NOT NULL,
  -- The gateway's sentence, on a run that failed.
  error   TEXT,
  started REAL NOT NULL,
  ended   REAL,
  -- What the turn cost, as JSON: {in, out, cached}.
  usage   TEXT
);
INSERT INTO agent_run_next(id, chat, status, error, started, ended, usage)
  SELECT id, chat, status, error, started, ended, usage FROM agent_run;
DROP TABLE agent_run;
ALTER TABLE agent_run_next RENAME TO agent_run;
CREATE INDEX idx_agent_run_chat ON agent_run(chat, id);
";

/// What a crash left behind, put right.
///
/// A run that says `streaming` has a reply nobody is holding: the process
/// that was reading it is gone, and there is no way to ask the gateway for
/// the rest of an answer it already sent. It fails, with the word for what
/// happened, and the chat offers *retry* — nothing re-sends a paid request
/// by guesswork.
///
/// `pending` and `waiting` are left where they are. Both are resumable: a
/// pending run has not been asked for yet, and a waiting one is holding
/// calls the chat panel will run when it is next shown.
///
/// The stamp is the run's own start, because that is the only moment this
/// store can honestly name — a ladder step has no clock.
fn sweep(c: &rusqlite::Connection) -> rusqlite::Result<()> {
    c.execute(
        "UPDATE agent_run SET status = 'failed', error = 'interrupted', ended = started
         WHERE status = 'streaming'",
        [],
    )?;
    Ok(())
}
