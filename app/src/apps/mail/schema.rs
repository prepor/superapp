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
//! One rule about column order is load-bearing: `raw` is a blob of a hundred
//! kilobytes and SQLite decodes a record left to right, so everything a list
//! reads sits *before* it. A query that asked for `thread` past the letter
//! would walk the overflow chain of every mail it touched.

use kernel::app::{Schema, Step};

/// Mail's ladder. Step one is the store shape this build was written
/// against; step two is what a draft carries, which arrived with the compose
/// panel's *attach*; step four is what a *letter* carries, and the draft rows
/// as the send actually needs them.
///
/// The three derived steps are versioned by the walk that makes each rather
/// than by the ladder's counter: an index, a narrowing and a set of derived
/// rows are all reproducible from `message` at any moment, so the honest
/// question is not "how old is this database" but "is this the shape this
/// build wants".
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
/// [`part_bytes`](super::sync::part_bytes) reads the bytes back by. The bytes
/// themselves stay in `raw`; a second copy of every attachment in the mailbox
/// is exactly the cost this design refuses.
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
