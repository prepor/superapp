//! Mail's tables, from version one.
//!
//! `message` records what the person wants; `server_msg` records what the
//! server last said. The two disagreeing is what the push pass turns into
//! jobs. A draft belongs to its compose **slot** — slot ids are stable and
//! persisted, so half-written text survives a restart — and an outbox row
//! shares that id, which means one pending send per compose and an undo
//! entity (`outbox:N`) that exists before the row does.
//!
//! No HTML and no full-text index: the prototype draws plain text and
//! searches it with `LIKE`. A letter carries files by path alone — there is
//! no MIME in the prototype, so a send ignores them.

use kernel::app::{Schema, Step};

/// Mail's ladder. Step one is the store shape this build was written
/// against; step two is what a draft carries, which arrived with the
/// compose panel's *attach*.
pub static SCHEMA: Schema = Schema {
    app: "mail",
    steps: &[Step::Sql(V1), Step::Sql(V2)],
};

const V1: &str = "
CREATE TABLE account(
  id        INTEGER PRIMARY KEY,
  label     TEXT NOT NULL,
  email     TEXT NOT NULL,
  imap_host TEXT,
  smtp_host TEXT,
  status    TEXT,
  synced    REAL
);

CREATE TABLE folder(
  id          INTEGER PRIMARY KEY,
  account     INTEGER NOT NULL REFERENCES account(id),
  name        TEXT NOT NULL,
  role        TEXT,
  uidvalidity INTEGER,
  uidnext     INTEGER
);
CREATE UNIQUE INDEX idx_folder_name ON folder(account, name);

CREATE TABLE message(
  id         INTEGER PRIMARY KEY,
  account    INTEGER NOT NULL REFERENCES account(id),
  folder     INTEGER NOT NULL REFERENCES folder(id),
  from_name  TEXT NOT NULL DEFAULT '',
  from_email TEXT NOT NULL DEFAULT '',
  subject    TEXT NOT NULL DEFAULT '',
  date       REAL NOT NULL,
  unread     INTEGER NOT NULL DEFAULT 0,
  body       TEXT NOT NULL DEFAULT '',
  status     TEXT,
  status_err INTEGER NOT NULL DEFAULT 0,
  message_id TEXT,
  -- The conversation's anchor: the lowest member's id, decided at ingest.
  thread     INTEGER,
  -- The subject with its reply prefixes stripped, so a list never has to
  -- strip them per row.
  topic      TEXT
);
CREATE INDEX idx_message_folder_date ON message(folder, date DESC);
CREATE INDEX idx_message_thread      ON message(thread);
CREATE INDEX idx_message_mid         ON message(account, message_id);

-- What a mail claims to belong to: its References and In-Reply-To, one row
-- an id. Threading is three lookups over this table.
CREATE TABLE reference(
  message INTEGER NOT NULL,
  mid     TEXT NOT NULL
);
CREATE INDEX idx_reference_mid     ON reference(mid);
CREATE INDEX idx_reference_message ON reference(message);

-- The server's view of a mail: where it is and whether it has been read.
CREATE TABLE server_msg(
  message INTEGER PRIMARY KEY,
  folder  INTEGER NOT NULL,
  uid     INTEGER,
  seen    INTEGER NOT NULL DEFAULT 0
);
CREATE UNIQUE INDEX idx_server_msg_uid ON server_msg(folder, uid) WHERE uid IS NOT NULL;

CREATE TABLE draft(
  panel       INTEGER PRIMARY KEY,
  account     INTEGER,
  re_message  INTEGER,
  fwd_message INTEGER,
  to_addr     TEXT NOT NULL DEFAULT '',
  subject     TEXT NOT NULL DEFAULT '',
  body        TEXT NOT NULL DEFAULT '',
  updated     REAL NOT NULL DEFAULT 0
);

CREATE TABLE outbox(
  id         INTEGER PRIMARY KEY,
  account    INTEGER NOT NULL,
  send_after REAL NOT NULL,
  status     TEXT NOT NULL DEFAULT 'pending',
  error      TEXT
);
";

/// What a draft carries, keyed by the compose slot the draft is: one row a
/// path, the same file twice being one attachment. A path, not bytes — the
/// prototype has no MIME, so the send leaves these behind and the panel
/// only says what it would have carried.
const V2: &str = "
CREATE TABLE draft_file(
  panel INTEGER NOT NULL,
  path  TEXT NOT NULL,
  added REAL NOT NULL DEFAULT 0,
  PRIMARY KEY(panel, path)
);
";
