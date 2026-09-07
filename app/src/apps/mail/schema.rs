//! Mail's tables, from version one.
//!
//! `message` records what the person wants; `server_msg` records what the
//! server last said. The two disagreeing is what the push pass turns into
//! jobs — the folder a mail sits in, whether it has been read, and whether it
//! has been passed on. A draft belongs to its compose **slot** — slot ids are
//! stable and persisted, so half-written text survives a restart — and an
//! outbox row shares that id, which means one pending send per compose and an
//! undo entity (`outbox:N`) that exists before the row does.
//!
//! SQLite decodes records left to right, so everything a list reads sits
//! before `raw`. It now holds a content snapshot without file bodies; older
//! stores held full RFC822 messages, converted by the attachment derivation.

use kernel::app::{Schema, Step};

/// Mail's ladder. Step one is the store shape this build was written
/// against; step two is what a draft carries, which arrived with the compose
/// panel's *attach*; step four is what a *letter* carries, and the draft rows
/// as the send actually needs them; step seven is `to_addr` for a store built
/// before [`V1`] had it; the last is where a deleted letter came from, which
/// is what the trash gives back.
///
/// The four derived steps are versioned by the walk that makes each rather
/// than by the ladder's counter: an index, a narrowing, a set of derived rows
/// and a header read back out of the letters are all reproducible from
/// `message` at any moment, so the honest question is not "how old is this
/// database" but "is this the shape this build wants".
pub static SCHEMA: Schema = Schema {
    app: "mail",
    steps: &[
        Step::Sql(V1),
        Step::Sql(V2),
        Step::Derived {
            key: "mail:fts",
            version: FTS_VERSION,
            rebuild: rebuild_fts,
        },
        Step::Sql(V3),
        Step::Derived {
            key: "mail:html",
            version: HTML_VERSION,
            rebuild: rebuild_html,
        },
        Step::Derived {
            key: "mail:attachments",
            version: super::parts::ATTACH_VERSION,
            rebuild: super::parts::scan,
        },
        Step::Run(add_to_addr),
        Step::Derived {
            key: "mail:recipients",
            version: TO_VERSION,
            rebuild: rebuild_recipients,
        },
        Step::Sql(V5),
    ],
};

const V1: &str = "
CREATE TABLE account(
  id        INTEGER PRIMARY KEY,
  label     TEXT NOT NULL,
  email     TEXT NOT NULL,
  imap_host TEXT,
  smtp_host TEXT,
  status    TEXT,
  synced    REAL,
  -- How it authenticates: NULL and 'password' both mean an app password in
  -- the keychain, 'google' an OAuth grant whose refresh token lives under
  -- its own key and whose access token is never written down at all. A
  -- column rather than a table because it is one word per account — and
  -- because the secret, which is what a second row would be about, is
  -- exactly what must not be in the store.
  auth      TEXT
);

CREATE TABLE folder(
  id          INTEGER PRIMARY KEY,
  account     INTEGER NOT NULL REFERENCES account(id),
  name        TEXT NOT NULL,
  role        TEXT,
  uidvalidity INTEGER,
  uidnext     INTEGER,
  -- The provider's *all mail* view (Gmail's `\\All`), not a folder of its
  -- own: a move target, never an ingest source.
  all_mail    INTEGER NOT NULL DEFAULT 0,
  -- Whether this folder's server keeps keywords such as `$Forwarded` (its
  -- PERMANENTFLAGS), recorded at each SELECT; assumed until the first one
  -- says otherwise.
  keywords    INTEGER NOT NULL DEFAULT 1
);
CREATE UNIQUE INDEX idx_folder_name ON folder(account, name);

CREATE TABLE message(
  id         INTEGER PRIMARY KEY,
  account    INTEGER NOT NULL REFERENCES account(id),
  folder     INTEGER NOT NULL REFERENCES folder(id),
  from_name  TEXT NOT NULL DEFAULT '',
  from_email TEXT NOT NULL DEFAULT '',
  -- The other end of the letter: its `To` line, as addresses, comma-joined
  -- in header order. Beside the sender because it answers the same question
  -- from the other side, and well before `html` — a letter in Sent is the
  -- only one whose recipient the account cannot answer for, and the reader
  -- asks for it by name. Empty where the letter named nobody.
  to_addr    TEXT NOT NULL DEFAULT '',
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
  topic      TEXT,
  -- Passed on — the `$Forwarded` keyword, as this app or another client set
  -- it. Intent; `server_msg.forwarded` is what the server holds.
  forwarded  INTEGER NOT NULL DEFAULT 0,
  -- The HTML reading, narrowed at ingest, and the letter as it arrived.
  -- Last, and in this order: nothing a list reads is behind them.
  html       TEXT,
  raw        BLOB
);
CREATE INDEX idx_message_folder_date ON message(folder, date DESC);
CREATE INDEX idx_message_thread      ON message(thread);
CREATE INDEX idx_message_mid         ON message(account, message_id);

-- What a mail claims to belong to: its References and In-Reply-To, one row
-- an id. Threading is three lookups over this table.
--
-- The primary key is not decoration: device sync records a table by its
-- primary key, and a table without one replicates nothing — a follower would
-- get `message.thread` and none of the rows it was derived from. The pair is
-- also the natural key (a mail names an id once), so it doubles as the index
-- the `message` lookups walk.
CREATE TABLE reference(
  message INTEGER NOT NULL,
  mid     TEXT NOT NULL,
  PRIMARY KEY(message, mid)
);
CREATE INDEX idx_reference_mid ON reference(mid);

-- The server's view of a mail: where it is, whether it has been read, and
-- whether it wears the keyword.
CREATE TABLE server_msg(
  message   INTEGER PRIMARY KEY,
  folder    INTEGER NOT NULL,
  uid       INTEGER,
  seen      INTEGER NOT NULL DEFAULT 0,
  forwarded INTEGER NOT NULL DEFAULT 0
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
/// path, the same file twice being one attachment. Superseded by
/// `draft_attachment` in [`V3`], which records what the send needs beside the
/// path.
const V2: &str = "
CREATE TABLE draft_file(
  panel INTEGER NOT NULL,
  path  TEXT NOT NULL,
  added REAL NOT NULL DEFAULT 0,
  PRIMARY KEY(panel, path)
);
";

/// What a letter carries, and what a draft will.
///
/// `attachment` is **derived**, like the HTML reading: one row per part of a
/// mail's `raw`, holding the description a list and a card need — name, media
/// type, size, the Content-ID an inline part wears — and `part`, the index
/// the stored content snapshot maps to an IMAP section. Attachment bodies are
/// downloaded on demand into the local file cache, never into SQLite.
///
/// `attachment_scan` is where the walk's version is written down, one row per
/// mail. A **table** rather than one `meta` key, because the question is per
/// mail and not per store: a letter that arrives through replication has a
/// `raw` nobody has walked yet, and this is what notices.
///
/// `draft_attachment` is the other direction and is not derived at all: a
/// compose panel's own list of files to carry out, keyed by its slot like the
/// draft it belongs to, holding the *path* rather than the bytes. It replaces
/// `draft_file`, which held a path and nothing else — the send needs the name
/// and the size it was attached at, and the device that picked it.
const V3: &str = "
CREATE TABLE attachment(
  id      INTEGER PRIMARY KEY,
  message INTEGER NOT NULL,
  part    INTEGER NOT NULL,
  name    TEXT NOT NULL,
  mime    TEXT NOT NULL DEFAULT 'application/octet-stream',
  size    INTEGER NOT NULL DEFAULT 0,
  cid     TEXT NOT NULL DEFAULT ''
);
CREATE INDEX idx_attachment_message ON attachment(message, id);
CREATE UNIQUE INDEX idx_attachment_part ON attachment(message, part);

CREATE TABLE attachment_scan(
  message INTEGER PRIMARY KEY,
  version INTEGER NOT NULL
);

CREATE TABLE draft_attachment(
  id     INTEGER PRIMARY KEY,
  panel  INTEGER NOT NULL,
  path   TEXT NOT NULL,
  name   TEXT NOT NULL,
  size   INTEGER NOT NULL DEFAULT 0,
  added  REAL NOT NULL DEFAULT 0,
  device TEXT NOT NULL DEFAULT ''
);
CREATE INDEX idx_draft_attachment_panel ON draft_attachment(panel, id);

DROP TABLE draft_file;
";

/// `to_addr`, for a store built before [`V1`] had it: the whole table
/// rewritten in place — SQLite's twelve-step ALTER, less the steps a table
/// nothing points at does not need.
///
/// `ADD COLUMN` would have been one line, and the column would have landed
/// *past* `raw`: every read of a letter's TO line would then walk the
/// overflow chain of a hundred-kilobyte blob to reach four bytes behind it,
/// which is the one rule the top of this file calls load-bearing. A copy
/// through a table of the right shape costs one rewrite, once, and leaves
/// every store — new or old — the same `message`.
///
/// Nothing declared a foreign key *to* `message` when this step was written
/// — [`V5`] is the first that does, and it climbs after this one, so the drop
/// needs no deferral; what it does take with it are the table's own indexes and the
/// FTS triggers, both put back after — the ids are copied as they stand, so
/// the index over them is still the index of these letters.
const V4: &str = "
CREATE TABLE message_new(
  id         INTEGER PRIMARY KEY,
  account    INTEGER NOT NULL REFERENCES account(id),
  folder     INTEGER NOT NULL REFERENCES folder(id),
  from_name  TEXT NOT NULL DEFAULT '',
  from_email TEXT NOT NULL DEFAULT '',
  to_addr    TEXT NOT NULL DEFAULT '',
  subject    TEXT NOT NULL DEFAULT '',
  date       REAL NOT NULL,
  unread     INTEGER NOT NULL DEFAULT 0,
  body       TEXT NOT NULL DEFAULT '',
  status     TEXT,
  status_err INTEGER NOT NULL DEFAULT 0,
  message_id TEXT,
  thread     INTEGER,
  topic      TEXT,
  forwarded  INTEGER NOT NULL DEFAULT 0,
  html       TEXT,
  raw        BLOB
);
INSERT INTO message_new(id, account, folder, from_name, from_email, subject,
                        date, unread, body, status, status_err, message_id,
                        thread, topic, forwarded, html, raw)
     SELECT id, account, folder, from_name, from_email, subject,
            date, unread, body, status, status_err, message_id,
            thread, topic, forwarded, html, raw FROM message;
DROP TABLE message;
ALTER TABLE message_new RENAME TO message;
CREATE INDEX idx_message_folder_date ON message(folder, date DESC);
CREATE INDEX idx_message_thread      ON message(thread);
CREATE INDEX idx_message_mid         ON message(account, message_id);
";

/// Where the ladder stands with everything before [`V4`] climbed: the six
/// steps a store made by the build before this one had run. What a test
/// winds a store back to, to send it up the rewrite again.
#[cfg(test)]
pub const BEFORE_TO_ADDR: i64 = 6;

/// Runs [`V4`] where it is owed. A fresh store climbs every step, this one
/// included, and it already has the column from [`V1`] — so what is asked is
/// the table rather than the counter, and a store of the right shape is left
/// alone.
fn add_to_addr(c: &rusqlite::Connection) -> rusqlite::Result<()> {
    let has: bool = c
        .prepare("SELECT 1 FROM pragma_table_info('message') WHERE name = 'to_addr'")?
        .exists([])?;
    if has {
        return Ok(());
    }
    c.execute_batch(V4)?;
    // The triggers went with the table they were on. The index itself is
    // still good — the letters kept their ids — but it is rebuilt with them
    // rather than trusted, which costs one walk on one store, once.
    rebuild_fts(c)
}

/// Where a deleted letter was, so it can be put back there.
///
/// One row per letter currently in the trash, written when a letter is filed
/// there and deleted when it leaves by any road — another filing, a tool, an
/// undo. The undo tree knows this too, and better, but only until the
/// process ends: history is in memory, keeps its last two hundred nodes, and
/// never had a node at all for a letter deleted on another device and
/// mirrored here. Two columns that survive a restart are what *put back*
/// reads.
///
/// A table rather than a `message` column, because of the rule [`V1`] is
/// built on: `raw` sits last so that everything a list reads is decoded
/// before the letter's own bytes, and a column added to `message` now would
/// land after it.
///
/// Both references cascade, because this table is a memory and a memory may
/// not hold anything open. A letter the server drops is deleted locally in
/// the middle of a sync pass's one commit, and a row that refused to go with
/// it would fail the whole pass; a folder that a resync drops takes the
/// memory of it with it, and the put back falls back to the inbox — which is
/// what a letter with nowhere remembered gets anyway.
/// `IF NOT EXISTS` because this step climbs after [`V4`], and a store wound
/// back to before that rewrite — which is how the rewrite is tested — walks
/// every step above it a second time.
const V5: &str = "
CREATE TABLE IF NOT EXISTS trashed(
  message INTEGER PRIMARY KEY REFERENCES message(id) ON DELETE CASCADE,
  folder  INTEGER NOT NULL REFERENCES folder(id) ON DELETE CASCADE
);
";

/// Which walk over `message` the index came out of. Bump it and every store
/// re-indexes on its next open.
const FTS_VERSION: i64 = 1;

/// The mail search index (FTS5): subject, both halves of the sender, and the
/// letter's text, over `message` by rowid.
///
/// It is **derived**, so it is versioned like a narrowing rather than by the
/// ladder's counter: a rebuild reproduces it from `message` at any moment, so
/// the honest question is not "how old is this database" but "is this index
/// the shape this build wants".
///
/// The triggers live in the database, not in a binary, so a build that has
/// never heard of this index still maintains it on every write.
/// `content='message'` means the index stores terms and no text of its own —
/// the letters are already in `message` and are not worth a second copy.
/// `unicode61` is what makes a Cyrillic subject tokenize like a Latin one.
const FTS: &str = "
DROP TRIGGER IF EXISTS message_fts_ai;
DROP TRIGGER IF EXISTS message_fts_ad;
DROP TRIGGER IF EXISTS message_fts_au;
DROP TABLE IF EXISTS message_fts;

CREATE VIRTUAL TABLE message_fts USING fts5(
  subject, from_name, from_email, body,
  content='message', content_rowid='id',
  tokenize='unicode61 remove_diacritics 2'
);

CREATE TRIGGER message_fts_ai AFTER INSERT ON message BEGIN
  INSERT INTO message_fts(rowid, subject, from_name, from_email, body)
  VALUES(new.id, new.subject, new.from_name, new.from_email, new.body);
END;

CREATE TRIGGER message_fts_ad AFTER DELETE ON message BEGIN
  INSERT INTO message_fts(message_fts, rowid, subject, from_name, from_email, body)
  VALUES('delete', old.id, old.subject, old.from_name, old.from_email, old.body);
END;

-- `UPDATE OF` on purpose: marking a mail read, moving it, threading it —
-- none of those touch a word of it, and none of them should cost a
-- re-index.
CREATE TRIGGER message_fts_au
AFTER UPDATE OF subject, from_name, from_email, body ON message BEGIN
  INSERT INTO message_fts(message_fts, rowid, subject, from_name, from_email, body)
  VALUES('delete', old.id, old.subject, old.from_name, old.from_email, old.body);
  INSERT INTO message_fts(rowid, subject, from_name, from_email, body)
  VALUES(new.id, new.subject, new.from_name, new.from_email, new.body);
END;
";

/// Drops the index and its triggers, builds them again, and re-indexes what
/// `message` already holds.
fn rebuild_fts(c: &rusqlite::Connection) -> rusqlite::Result<()> {
    c.execute_batch(FTS)?;
    c.execute("INSERT INTO message_fts(message_fts) VALUES('rebuild')", [])?;
    Ok(())
}

/// Which narrowing the stored readings came out of.
const HTML_VERSION: i64 = super::html::VERSION as i64;

/// Rewrites `message.html` from the `raw` blob each synced mail keeps.
///
/// The narrowing ([`html::sanitize`](super::html::sanitize)) runs at ingest,
/// so a stored reading is only as good as the build that stored it, and a
/// better narrowing has to be run over the rows already there. Messages
/// without `raw` — a seeded letter written by hand — are left alone, and a
/// mail whose sender wrote text only stays NULL.
fn rebuild_html(c: &rusqlite::Connection) -> rusqlite::Result<()> {
    let rows: Vec<(i64, Vec<u8>)> = c
        .prepare("SELECT id, raw FROM message WHERE raw IS NOT NULL")?
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?
        .collect::<rusqlite::Result<_>>()?;
    for (id, raw) in rows {
        c.execute(
            "UPDATE message SET html = ?2 WHERE id = ?1",
            rusqlite::params![id, super::sync::parse_mail(&raw).html],
        )?;
    }
    Ok(())
}

/// Which walk over the stored letters the TO lines came out of.
const TO_VERSION: i64 = 1;

/// Reads every stored letter's `To` line back out of the `raw` it keeps.
///
/// Derived, like the HTML reading: the header is in the letter, and the
/// column is a cache of it — which is what makes a mailbox synced before
/// this existed answer for its Sent folder on the next open rather than on
/// the next sync. Only the headers are parsed; the body is a hundred
/// kilobytes nobody here is asking about. A mail without `raw` — a seeded
/// letter written by hand — keeps the line the seed wrote.
fn rebuild_recipients(c: &rusqlite::Connection) -> rusqlite::Result<()> {
    let rows: Vec<(i64, Vec<u8>)> = c
        .prepare("SELECT id, raw FROM message WHERE raw IS NOT NULL")?
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?
        .collect::<rusqlite::Result<_>>()?;
    for (id, raw) in rows {
        c.execute(
            "UPDATE message SET to_addr = ?2 WHERE id = ?1",
            rusqlite::params![id, super::sync::to_of(&raw)],
        )?;
    }
    Ok(())
}
