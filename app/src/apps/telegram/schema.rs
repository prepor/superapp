//! Telegram's tables, from version one. Every one is prefixed `tg_`.
//!
//! A chat and its peer share one id, as they do on the wire: `tg_peer` is
//! who or what — a person, a group, a channel — and `tg_chat` is the
//! conversation I have with it, one row per peer I have one with. A message
//! is one line of a chat; a service line is a message nobody wrote.

use kernel::app::{Schema, Step};
use rusqlite::Connection;

/// Telegram's ladder: the demo shape this round was drawn against, then the
/// full-text index the projected store searches over, then the authorization
/// status row the real client writes, then the repair of `V1`'s one
/// in-place edit, the reply freed from the window, the clip a moving
/// picture plays, whether a chat is mine at all, and the message given a row
/// key of its own, message link metadata, the person's block state, and
/// topics. Published SQL keeps its version; column checks also repair
/// development builds that used the same version for a different feature.
pub static SCHEMA: Schema = Schema {
    app: "telegram",
    steps: &[
        Step::Sql(V1),
        Step::Sql(V2),
        Step::Sql(V3),
        Step::Run(v4_media_columns),
        Step::Sql(V5),
        Step::Sql(V6),
        Step::Run(v7_listing),
        Step::Sql(V8),
        Step::Always(v9_link_entities),
        Step::Always(v10_known_entities),
        Step::Always(v11_block_state),
        Step::Always(v12_mentions),
        Step::Always(v13_topic_schema),
    ],
};

// Existing cached text remains readable and can detect bare URLs locally.
// New updates retain their entities, including destinations behind labels.
const V9: &str = "ALTER TABLE tg_message ADD COLUMN entities TEXT NOT NULL DEFAULT '[]'";

// V9 conflated unavailable metadata with TDLib's empty list. Nonempty lists
// can be recognized on upgrade; empty ones remain unknown until refreshed.
const V10: &str = "
ALTER TABLE tg_message ADD COLUMN entities_known INTEGER NOT NULL DEFAULT 0;
UPDATE tg_message SET entities_known = 1 WHERE entities != '[]';
";

// Blocking belongs to the person, so deleting their chat keeps the block.
const V11: &str = "ALTER TABLE tg_peer ADD COLUMN blocked INTEGER NOT NULL DEFAULT 0";

// `tg_chat.mention` now keeps TDLib's count, rather than a boolean. Existing
// values remain a lower bound until the next chat update. Individual unread
// mentions (which include replies to me) survive the ordinary inbox read.
const V12: &str = "
ALTER TABLE tg_message ADD COLUMN unread_mention INTEGER NOT NULL DEFAULT 0;
ALTER TABLE tg_message ADD COLUMN mention_read INTEGER NOT NULL DEFAULT 0;
CREATE INDEX tg_message_unread_mention ON tg_message(chat, id) WHERE unread_mention = 1;
";

// Early topic builds used rungs 9 and 10 before the link migrations landed
// on main. Keep main's SQL and numbering, checking each column in place so
// V10 can never run before its entities column exists. Complete databases
// incur no schema writes, and existing metadata and block state stay intact.
fn v9_link_entities(c: &Connection) -> rusqlite::Result<()> {
    if !columns(c, "tg_message")?.contains("entities") {
        c.execute_batch(V9)?;
    }
    Ok(())
}

fn v10_known_entities(c: &Connection) -> rusqlite::Result<()> {
    if !columns(c, "tg_message")?.contains("entities_known") {
        c.execute_batch(V10)?;
    }
    Ok(())
}

fn v11_block_state(c: &Connection) -> rusqlite::Result<()> {
    if !columns(c, "tg_peer")?.contains("blocked") {
        c.execute_batch(V11)?;
    }
    Ok(())
}

// Earlier topic builds also occupied V12. Keep main's mention migration at
// V12 and repair its shape even when that shared counter has already advanced.
fn v12_mentions(c: &Connection) -> rusqlite::Result<()> {
    let message = columns(c, "tg_message")?;
    if !message.contains("unread_mention") && !message.contains("mention_read") {
        return c.execute_batch(V12);
    }
    for column in ["unread_mention", "mention_read"] {
        if !message.contains(column) {
            c.execute_batch(&format!("ALTER TABLE tg_message ADD COLUMN {column} INTEGER NOT NULL DEFAULT 0"))?;
        }
    }
    let indexed: bool = c.query_row("SELECT EXISTS(SELECT 1 FROM sqlite_master
        WHERE type = 'index' AND name = 'tg_message_unread_mention')", [], |r| r.get(0))?;
    if !indexed {
        c.execute_batch("CREATE INDEX tg_message_unread_mention
            ON tg_message(chat, id) WHERE unread_mention = 1")?;
    }
    Ok(())
}

const TOPIC_FLAGS: &[(&str, &str)] = &[
    ("selected", "INTEGER NOT NULL DEFAULT 0"),
    ("pinned", "INTEGER NOT NULL DEFAULT 0"),
    ("archived", "INTEGER NOT NULL DEFAULT 0"),
    ("closed", "INTEGER NOT NULL DEFAULT 0"),
    ("hidden", "INTEGER NOT NULL DEFAULT 0"),
    ("unread", "INTEGER NOT NULL DEFAULT 0"),
    ("mention", "INTEGER NOT NULL DEFAULT 0"),
    ("muted", "INTEGER NOT NULL DEFAULT 0"),
    ("mute_default", "INTEGER NOT NULL DEFAULT 1"),
    ("last_read", "INTEGER"),
    ("read_outbox", "INTEGER"),
    ("draft", "TEXT"),
];

fn columns(c: &Connection, table: &str) -> rusqlite::Result<std::collections::HashSet<String>> {
    c.prepare(&format!("PRAGMA table_info({table})"))?
        .query_map([], |r| r.get(1))?
        .collect()
}

/// Early topic and message-link builds both used rung 9. Opening a store
/// from the other build skipped topic creation and hid every listed chat.
/// Check the shape at every open, since another build can advance the shared
/// counter again. A complete topic schema needs no writes; repairs add only
/// missing columns and objects, preserving messages and topic preferences.
fn v13_topic_schema(c: &Connection) -> rusqlite::Result<()> {
    let peer = columns(c, "tg_peer")?;
    let message = columns(c, "tg_message")?;
    let topic = columns(c, "tg_topic")?;
    let objects: i64 = c.query_row(
        "SELECT COUNT(*) FROM sqlite_master WHERE
         (type = 'view' AND name = 'tg_dialog') OR
         (type = 'index' AND name = 'tg_message_topic')",
        [], |r| r.get(0),
    )?;
    if peer.contains("is_forum") && message.contains("topic") && objects == 2
        && TOPIC_FLAGS.iter().all(|(name, _)| topic.contains(*name))
    {
        return Ok(());
    }

    let tx = c.unchecked_transaction()?;
    tx.execute_batch("DROP VIEW IF EXISTS tg_dialog")?;
    if !peer.contains("is_forum") {
        tx.execute_batch("ALTER TABLE tg_peer ADD COLUMN is_forum INTEGER NOT NULL DEFAULT 0")?;
    }
    if !message.contains("topic") {
        tx.execute_batch("ALTER TABLE tg_message ADD COLUMN topic INTEGER NOT NULL DEFAULT 0")?;
    }
    tx.execute_batch("CREATE TABLE IF NOT EXISTS tg_topic(
        chat INTEGER NOT NULL REFERENCES tg_peer(id),
        id INTEGER NOT NULL, name TEXT NOT NULL, PRIMARY KEY(chat, id)
    )")?;
    for (name, ty) in TOPIC_FLAGS {
        if !topic.contains(*name) {
            tx.execute_batch(&format!("ALTER TABLE tg_topic ADD COLUMN {name} {ty}"))?;
        }
    }
    tx.execute_batch("
        CREATE INDEX IF NOT EXISTS tg_message_topic ON tg_message(chat, topic, date DESC, id DESC);
        CREATE VIEW tg_dialog AS
          SELECT c.peer, 0 AS topic, CAST(c.peer AS TEXT) || ':0' AS row_key,
                 p.name AS title, p.is_forum, c.pinned, c.muted, c.archived, c.in_main,
                 c.unread, c.mention, c.draft, c.typing
          FROM tg_chat c JOIN tg_peer p ON p.id = c.peer
          UNION ALL
          SELECT t.chat, t.id, CAST(t.chat AS TEXT) || ':' || t.id,
                 t.name || ' · ' || p.name, 0, t.pinned,
                 CASE WHEN t.mute_default = 1 THEN c.muted ELSE t.muted END, t.archived,
                 (c.in_main = 1 OR c.archived = 1), t.unread, t.mention, t.draft, NULL
          FROM tg_topic t JOIN tg_peer p ON p.id = t.chat JOIN tg_chat c ON c.peer = t.chat
          WHERE t.selected = 1 AND p.is_forum = 1;
    ")?;
    tx.commit()
}

const V1: &str = "
CREATE TABLE tg_peer(
  id         INTEGER PRIMARY KEY,
  -- 'person', 'group' or 'channel'.
  kind       TEXT NOT NULL,
  name       TEXT NOT NULL,
  -- Without the @.
  username   TEXT,
  -- A person's bio, a group's or a channel's description.
  about      TEXT,
  phone      TEXT,
  -- A person's presence: 'online', 'recently', 'week', 'month', 'long'.
  status     TEXT,
  last_seen  REAL,
  -- A group's or a channel's counts.
  members    INTEGER,
  online     INTEGER,
  -- Whether I administer it, which is what lets me post in a channel.
  admin      INTEGER NOT NULL DEFAULT 0,
  is_contact INTEGER NOT NULL DEFAULT 0,
  -- The one peer that is me: saved messages.
  is_self    INTEGER NOT NULL DEFAULT 0
);

CREATE TABLE tg_chat(
  peer      INTEGER PRIMARY KEY REFERENCES tg_peer(id),
  -- 0 for an unpinned chat, else its place among the pinned ones, 1 first.
  pinned    INTEGER NOT NULL DEFAULT 0,
  muted     INTEGER NOT NULL DEFAULT 0,
  archived  INTEGER NOT NULL DEFAULT 0,
  -- How many messages I have not read.
  unread    INTEGER NOT NULL DEFAULT 0,
  -- One of them mentions me.
  mention   INTEGER NOT NULL DEFAULT 0,
  draft     TEXT,
  -- Who is typing right now, in a word. Demo only until the update stream.
  typing    TEXT,
  -- The last message I read, which is where the unread line is drawn.
  last_read INTEGER
);

CREATE TABLE tg_message(
  id          INTEGER PRIMARY KEY,
  chat        INTEGER NOT NULL REFERENCES tg_chat(peer),
  -- NULL for a service line and for a channel's own post.
  sender      INTEGER REFERENCES tg_peer(id),
  date        REAL NOT NULL,
  text        TEXT NOT NULL,
  -- Mine.
  out         INTEGER NOT NULL DEFAULT 0,
  -- Of mine: 'sending', 'sent', 'read', 'failed'.
  state       TEXT,
  edited      INTEGER NOT NULL DEFAULT 0,
  reply_to    INTEGER REFERENCES tg_message(id),
  -- The name it was forwarded from.
  fwd_from    TEXT,
  -- 'photo', 'video', 'circle' (a round video message), 'sticker',
  -- 'voice', 'audio', 'file', 'location', 'live' (a location that moves).
  media       TEXT,
  -- What the media is called, where it has a name: a file's name and
  -- size, an audio track's title, a sticker's emoji.
  media_label TEXT,
  -- Where its bytes are: `demo:` a bundled picture this round, a path
  -- under the app's cache once an account downloads them.
  media_ref   TEXT,
  -- A picture's or a poster's size in pixels, so the box has its height
  -- before the bytes land.
  media_w     INTEGER,
  media_h     INTEGER,
  -- A recording's length.
  media_secs  INTEGER,
  -- A location.
  media_lat   REAL,
  media_lon   REAL,
  -- Until when a live location is shared.
  media_until REAL,
  -- A channel post's.
  views       INTEGER,
  comments    INTEGER,
  -- As one line: '👍 3 · ❤️ 1'.
  reactions   TEXT,
  -- A line about the chat rather than a message in it.
  service     INTEGER NOT NULL DEFAULT 0
);
CREATE INDEX tg_message_chat ON tg_message(chat, date, id);

CREATE TABLE tg_member(
  chat  INTEGER NOT NULL REFERENCES tg_peer(id),
  peer  INTEGER NOT NULL REFERENCES tg_peer(id),
  admin INTEGER NOT NULL DEFAULT 0,
  PRIMARY KEY(chat, peer)
);

CREATE TABLE tg_folder(
  id   INTEGER PRIMARY KEY,
  name TEXT NOT NULL
);

CREATE TABLE tg_folder_chat(
  folder INTEGER NOT NULL REFERENCES tg_folder(id),
  chat   INTEGER NOT NULL REFERENCES tg_peer(id),
  PRIMARY KEY(folder, chat)
);
";

// Search is 'a word in a chat', and the store answers it: an FTS5 index over
// message text, searched by the projected store and — from the third phase —
// falling through to the wire only for what the window does not hold.
//
// The index is external-content: it stores no second copy of the text, it
// reads `tg_message.text` by the message's own id, and the trigger trio below
// is the only thing that keeps it in step. No new columns — the wire's every
// field already has a home in `tg_message` (a downloaded file's bytes live in
// the blob cache, which `media_ref` keys), so V2 adds an index and nothing
// else.
const V2: &str = "
CREATE VIRTUAL TABLE tg_message_fts USING fts5(
  text,
  content='tg_message',
  content_rowid='id'
);

-- The external-content trigger trio (the FTS5 manual's own): a new line is
-- indexed, a gone one is told to the index by its old text so the right terms
-- are struck, an edited one is struck and re-indexed. A trim's DELETE and a
-- projection's edit both reach the index this way, so it never drifts.
CREATE TRIGGER tg_message_fts_ai AFTER INSERT ON tg_message BEGIN
  INSERT INTO tg_message_fts(rowid, text) VALUES(new.id, new.text);
END;
CREATE TRIGGER tg_message_fts_ad AFTER DELETE ON tg_message BEGIN
  INSERT INTO tg_message_fts(tg_message_fts, rowid, text)
    VALUES('delete', old.id, old.text);
END;
CREATE TRIGGER tg_message_fts_au AFTER UPDATE ON tg_message BEGIN
  INSERT INTO tg_message_fts(tg_message_fts, rowid, text)
    VALUES('delete', old.id, old.text);
  INSERT INTO tg_message_fts(rowid, text) VALUES(new.id, new.text);
END;

-- A store already carrying V1's messages has its index filled in one pass
-- here; a fresh store's tg_message is still empty at this rung, and the demo
-- seed's inserts — run after the ladder — fill it through the AFTER INSERT
-- trigger instead.
INSERT INTO tg_message_fts(tg_message_fts) VALUES('rebuild');
";

// A sign-in writes one row the sign-in UI reads back: where the authorization
// flow stands, and the hint a code or a password step carries. Device-local —
// a session is this machine's, not the account's — so it is never seeded and
// the demo world leaves it at its 'closed' default.
//
// It carries a primary key so device sync *records* it rather than dropping
// its rows silently; but a session should not travel between devices, and the
// kernel has no seam yet to hold one app table back from the changeset (only
// `repl`/`repl_log` are excluded, in the store itself). Left keyed with this
// note: excluding it is a phase-3d/e follow-up, once that seam exists.
const V3: &str = "
CREATE TABLE tg_session(
  id      INTEGER PRIMARY KEY CHECK (id = 1),
  phone   TEXT,
  -- 'closed', 'connecting', 'wait_phone', 'wait_code', 'wait_password',
  -- 'ready', 'logging_out'.
  state   TEXT NOT NULL DEFAULT 'closed',
  -- A code step's kind and length, a password hint, or an error — what the
  -- sign-in panel shows beside the field.
  detail  TEXT,
  updated REAL
);
";

// The fifth rung frees a reply from the window, and gives a chat its outbox
// cursor.
//
// `V1` declared `reply_to` a foreign key onto `tg_message` — wrong for a
// windowed table: a reply answers a line the store may not hold, older than
// the ten thousand kept or not yet backfilled, and a trim past a replied-to
// line was refused the same way. Live, that was every reply in a group
// failing with `FOREIGN KEY constraint failed` (2026-09-07). SQLite cannot
// drop a constraint in place, so the table is rebuilt without it: the same
// columns, copied across, the index and the FTS trigger trio made again on
// the new table and the index rebuilt from it. The engine's own copies of
// the reply are unaffected — the panel resolves a `reply_to` the store
// lacks to nothing, as it always did.
//
// `read_outbox` is where the far side has read up to in a chat — TDLib's
// `last_read_outbox_message_id` — so a sent line can be marked read on
// arrival, whichever comes first, the line or the cursor.
const V5: &str = "
BEGIN;
CREATE TABLE tg_message_v5(
  id          INTEGER PRIMARY KEY,
  chat        INTEGER NOT NULL REFERENCES tg_chat(peer),
  sender      INTEGER REFERENCES tg_peer(id),
  date        REAL NOT NULL,
  text        TEXT NOT NULL,
  out         INTEGER NOT NULL DEFAULT 0,
  state       TEXT,
  edited      INTEGER NOT NULL DEFAULT 0,
  -- The line it answers, by id — one the window may not hold.
  reply_to    INTEGER,
  fwd_from    TEXT,
  media       TEXT,
  media_label TEXT,
  media_ref   TEXT,
  media_w     INTEGER,
  media_h     INTEGER,
  media_secs  INTEGER,
  media_lat   REAL,
  media_lon   REAL,
  media_until REAL,
  views       INTEGER,
  comments    INTEGER,
  reactions   TEXT,
  service     INTEGER NOT NULL DEFAULT 0
);
INSERT INTO tg_message_v5
  SELECT id, chat, sender, date, text, out, state, edited, reply_to, fwd_from,
         media, media_label, media_ref, media_w, media_h, media_secs,
         media_lat, media_lon, media_until, views, comments, reactions, service
  FROM tg_message;
DROP TABLE tg_message;
ALTER TABLE tg_message_v5 RENAME TO tg_message;
CREATE INDEX tg_message_chat ON tg_message(chat, date, id);
CREATE TRIGGER tg_message_fts_ai AFTER INSERT ON tg_message BEGIN
  INSERT INTO tg_message_fts(rowid, text) VALUES(new.id, new.text);
END;
CREATE TRIGGER tg_message_fts_ad AFTER DELETE ON tg_message BEGIN
  INSERT INTO tg_message_fts(tg_message_fts, rowid, text)
    VALUES('delete', old.id, old.text);
END;
CREATE TRIGGER tg_message_fts_au AFTER UPDATE ON tg_message BEGIN
  INSERT INTO tg_message_fts(tg_message_fts, rowid, text)
    VALUES('delete', old.id, old.text);
  INSERT INTO tg_message_fts(rowid, text) VALUES(new.id, new.text);
END;
INSERT INTO tg_message_fts(tg_message_fts) VALUES('rebuild');
ALTER TABLE tg_chat ADD COLUMN read_outbox INTEGER;
COMMIT;
";

// The sixth rung gives a moving picture the clip it plays.
//
// Until now a video line held one file: `media_ref`, its poster — the
// thumbnail, small, fetched on arrival, the clip itself never (Andrey,
// 2026-09-06). The poster is still what the transcript draws, so
// `media_ref` is untouched; what the player needs is the *other* file, and
// two things to find it by.
//
// `media_clip` is where the clip's bytes will be once they are here: the
// same `tg:<remote unique id>` key the blob cache uses for every other
// file, so a download that lands under it resolves through the one path
// media already takes. `media_clip_rid` is what the clip is asked for by —
// TDLib's `remoteFile.id`, the id that outlives a session, unlike the
// `file.id` a `downloadFile` runs on, which is only this run's and would be
// a stale number the next morning. A row keeps the durable one and the
// worker turns it back into a live id when the player asks.

const V6: &str = "
ALTER TABLE tg_message ADD COLUMN media_clip TEXT;
ALTER TABLE tg_message ADD COLUMN media_clip_rid TEXT;
";

// The eighth rung gives a message a key of its own, and a picture the name it
// can be asked for by.
//
// `V1` made `id` the primary key of `tg_message`, as though a message id were
// a message's identity. It is not: TDLib's ids are unique *within a chat* and
// nowhere else — a supergroup's and a channel's are the server id shifted
// twenty bits, so every channel's first post is 1048576 — and a second
// channel's post with the same number overwrote the first channel's row and
// carried it into the other conversation (review, 2026-09-07). So the row
// gets a key that is a row's: `seq`, a fresh integer per line, with `id` and
// `chat` plain columns under a `UNIQUE(chat, id)` that says what identity
// really is. Every lookup names the chat as well as the id from here on.
//
// The table is rebuilt for it, the way `V5` rebuilt it: the same columns
// copied across, each row keeping its old rowid as its `seq` so nothing that
// was written down moves, the index made again, and the full-text index
// moved to the new key — an external-content index is keyed by its content
// table's rowid, so `tg_message_fts` is dropped and made again with
// `content_rowid='seq'`, its trigger trio speaking of `new.seq` and
// `old.seq`, and rebuilt from the rows. A pair that had already collided
// before this rung is one row and stays one row; that is a line lost the day
// it landed, not one lost here.
//
// `media_rid` is the other half of the round: the remote id — TDLib's
// `remoteFile.id` — of the file `media_ref` names, a photo's largest size or
// a moving picture's poster. Until now a picture could only be fetched at
// the moment its line arrived, from the file id that update carried, which is
// this run's number and gone by morning; a photo past the forty lines a chat
// fetches as it opens, or one the cache evicted, could never be asked for
// again. With the durable id on the row, the viewer and the transcript can
// ask for the bytes whenever they find them missing.
const V8: &str = "
BEGIN;
CREATE TABLE tg_message_v8(
  -- The row's own key: a fresh integer per line, which is what `id` was
  -- being asked to be and is not.
  seq         INTEGER PRIMARY KEY,
  -- Telegram's message id — unique in its chat, and only there.
  id          INTEGER NOT NULL,
  chat        INTEGER NOT NULL REFERENCES tg_chat(peer),
  sender      INTEGER REFERENCES tg_peer(id),
  date        REAL NOT NULL,
  text        TEXT NOT NULL,
  out         INTEGER NOT NULL DEFAULT 0,
  state       TEXT,
  edited      INTEGER NOT NULL DEFAULT 0,
  -- The line it answers, by id in this chat — one the window may not hold.
  reply_to    INTEGER,
  fwd_from    TEXT,
  media       TEXT,
  media_label TEXT,
  media_ref   TEXT,
  -- What the file `media_ref` names is asked for by, when its bytes are not
  -- here: TDLib's `remoteFile.id`, which outlives a session.
  media_rid   TEXT,
  media_w     INTEGER,
  media_h     INTEGER,
  media_secs  INTEGER,
  media_lat   REAL,
  media_lon   REAL,
  media_until REAL,
  media_clip  TEXT,
  media_clip_rid TEXT,
  views       INTEGER,
  comments    INTEGER,
  reactions   TEXT,
  service     INTEGER NOT NULL DEFAULT 0,
  UNIQUE(chat, id)
);
INSERT INTO tg_message_v8(
    seq, id, chat, sender, date, text, out, state, edited, reply_to, fwd_from,
    media, media_label, media_ref, media_w, media_h, media_secs,
    media_lat, media_lon, media_until, media_clip, media_clip_rid,
    views, comments, reactions, service)
  SELECT rowid, id, chat, sender, date, text, out, state, edited, reply_to, fwd_from,
         media, media_label, media_ref, media_w, media_h, media_secs,
         media_lat, media_lon, media_until, media_clip, media_clip_rid,
         views, comments, reactions, service
  FROM tg_message ORDER BY rowid;
DROP TABLE tg_message_fts;
DROP TABLE tg_message;
ALTER TABLE tg_message_v8 RENAME TO tg_message;
CREATE INDEX tg_message_chat ON tg_message(chat, date, id);
CREATE VIRTUAL TABLE tg_message_fts USING fts5(
  text,
  content='tg_message',
  content_rowid='seq'
);
CREATE TRIGGER tg_message_fts_ai AFTER INSERT ON tg_message BEGIN
  INSERT INTO tg_message_fts(rowid, text) VALUES(new.seq, new.text);
END;
CREATE TRIGGER tg_message_fts_ad AFTER DELETE ON tg_message BEGIN
  INSERT INTO tg_message_fts(tg_message_fts, rowid, text)
    VALUES('delete', old.seq, old.text);
END;
CREATE TRIGGER tg_message_fts_au AFTER UPDATE ON tg_message BEGIN
  INSERT INTO tg_message_fts(tg_message_fts, rowid, text)
    VALUES('delete', old.seq, old.text);
  INSERT INTO tg_message_fts(rowid, text) VALUES(new.seq, new.text);
END;
INSERT INTO tg_message_fts(tg_message_fts) VALUES('rebuild');
COMMIT;
";

/// The seventh rung: whether a chat is in my list at all.
///
/// The engine announces every chat it learns of the same way — one I am in,
/// one a line was forwarded from, one a reply was quoted out of — and the
/// first shape of `tg_chat` took each for a conversation of mine, so the
/// list held channels never joined (Andrey, 2026-09-07: "дядя сэм"). The
/// column defaults to *in*, which keeps the demo world's rows listed. A
/// store an account has signed in on — one with a phone on its session
/// row — is reset to *out* instead: the engine re-announces every listed
/// chat with its position as the list loads, and only those come back.
fn v7_listing(c: &Connection) -> rusqlite::Result<()> {
    let have: bool = c
        .prepare("PRAGMA table_info(tg_chat)")?
        .query_map([], |r| r.get::<_, String>(1))?
        .collect::<rusqlite::Result<Vec<_>>>()?
        .iter()
        .any(|n| n == "in_main");
    if !have {
        c.execute_batch("ALTER TABLE tg_chat ADD COLUMN in_main INTEGER NOT NULL DEFAULT 1")?;
    }
    c.execute(
        "UPDATE tg_chat SET in_main = 0
         WHERE EXISTS (SELECT 1 FROM tg_session WHERE id = 1 AND phone IS NOT NULL)",
        [],
    )?;
    Ok(())
}

/// The columns `V1` grew for the media round — a reference into the blob
/// cache, a picture's size, a recording's length, a location — with their
/// types, as `V1` declares them now.
const MEDIA_COLUMNS: [(&str, &str); 7] = [
    ("media_ref", "TEXT"),
    ("media_w", "INTEGER"),
    ("media_h", "INTEGER"),
    ("media_secs", "INTEGER"),
    ("media_lat", "REAL"),
    ("media_lon", "REAL"),
    ("media_until", "REAL"),
];

/// The fourth rung: `V1`'s media columns, for a store whose `tg_message`
/// predates them.
///
/// `V1` was widened in place for the media round (2026-09-05) after the
/// machine's store had already run its first shape, and the ladder — which
/// counts rungs, not columns — never came back to it. Every message the
/// live client then wrote failed on `media_ref`, silently, while the chat
/// list filled and the sign-in panel said the chats were syncing
/// (2026-09-07). This rung adds whatever of the seven a store lacks and
/// touches nothing where `V1` already carried them, so a first-shape store
/// and a fresh one end up the same table. The lesson stands above it: a
/// rung is frozen the day a store runs it; what a later round needs is a
/// later rung.
fn v4_media_columns(c: &Connection) -> rusqlite::Result<()> {
    let have: std::collections::HashSet<String> = c
        .prepare("PRAGMA table_info(tg_message)")?
        .query_map([], |r| r.get::<_, String>(1))?
        .collect::<rusqlite::Result<_>>()?;
    for (name, ty) in MEDIA_COLUMNS {
        if !have.contains(name) {
            c.execute_batch(&format!("ALTER TABLE tg_message ADD COLUMN {name} {ty}"))?;
        }
    }
    Ok(())
}

/// The single `tg_session` row, as the sign-in UI reads it.
#[derive(Debug, Clone, PartialEq)]
pub struct Session {
    pub phone: Option<String>,
    pub state: String,
    pub detail: Option<String>,
    pub updated: Option<f64>,
}

/// Reads the one session row, or the 'closed' default a store with no sign-in
/// yet answers with — so a caller never has to special-case the empty table.
#[must_use]
pub fn session(conn: &Connection) -> Session {
    conn.query_row(
        "SELECT phone, state, detail, updated FROM tg_session WHERE id = 1",
        [],
        |r| {
            Ok(Session {
                phone: r.get(0)?,
                state: r.get(1)?,
                detail: r.get(2)?,
                updated: r.get(3)?,
            })
        },
    )
    .unwrap_or_else(|_| Session {
        phone: None,
        state: "closed".to_string(),
        detail: None,
        updated: None,
    })
}

/// Writes the one session row: the auth `state` and its `detail`, plus the
/// `phone` when one is supplied — `None` keeps whatever is filed, so advancing
/// the state never clears a phone an earlier step recorded.
///
/// # Errors
///
/// If the store refuses the write.
#[cfg_attr(not(feature = "tdlib"), allow(dead_code))]
pub fn set_session(
    conn: &Connection,
    phone: Option<&str>,
    state: &str,
    detail: Option<&str>,
    now: f64,
) -> rusqlite::Result<()> {
    conn.execute(
        "INSERT INTO tg_session(id, phone, state, detail, updated)
         VALUES(1, ?1, ?2, ?3, ?4)
         ON CONFLICT(id) DO UPDATE SET
           phone = COALESCE(excluded.phone, tg_session.phone),
           state = excluded.state, detail = excluded.detail, updated = excluded.updated",
        rusqlite::params![phone, state, detail, now],
    )
    .map(|_| ())
}

#[cfg(test)]
mod tests {
    use rusqlite::Connection;

    mod topics;

    #[test]
    fn v9_keeps_cached_messages_and_their_search_index() {
        let old = kernel::app::Schema { app: "telegram", steps: &super::SCHEMA.steps[..8] };
        let c = Connection::open_in_memory().unwrap();
        c.pragma_update(None, "foreign_keys", "ON").unwrap();
        c.execute_batch("CREATE TABLE meta(key TEXT PRIMARY KEY, value ANY)").unwrap();
        old.apply(&c).unwrap();
        c.execute_batch(
            "INSERT INTO tg_peer(id, kind, name) VALUES(10, 'group', 'Links');
             INSERT INTO tg_chat(peer) VALUES(10);
             INSERT INTO tg_message(id, chat, date, text) VALUES(1, 10, 1.0, 'cached https://example.org');"
        ).unwrap();
        super::SCHEMA.apply(&c).unwrap();
        super::SCHEMA.apply(&c).unwrap();
        let (text, entities, known): (String, String, bool) = c.query_row(
            "SELECT text, entities, entities_known FROM tg_message WHERE chat = 10 AND id = 1", [],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?))
        ).unwrap();
        assert_eq!(text, "cached https://example.org");
        assert_eq!(entities, "[]");
        assert!(!known);
        assert!(super::super::text::html(&text, None).contains("<a href="));
        let matches: i64 = c.query_row(
            "SELECT COUNT(*) FROM tg_message_fts WHERE tg_message_fts MATCH 'cached'", [], |r| r.get(0)
        ).unwrap();
        assert_eq!(matches, 1);
    }

    #[test]
    fn v10_distinguishes_known_entities_from_ambiguous_empty_v9_rows() {
        let old = kernel::app::Schema { app: "telegram", steps: &super::SCHEMA.steps[..9] };
        let c = Connection::open_in_memory().unwrap();
        c.execute_batch("CREATE TABLE meta(key TEXT PRIMARY KEY, value ANY)").unwrap();
        old.apply(&c).unwrap();
        c.execute_batch(r#"
            INSERT INTO tg_peer(id, kind, name) VALUES(10, 'group', 'Links');
            INSERT INTO tg_chat(peer) VALUES(10);
            INSERT INTO tg_message(id, chat, date, text) VALUES(1, 10, 1.0, 'main.rs');
            INSERT INTO tg_message(id, chat, date, text, entities) VALUES(2, 10, 2.0, 'read',
                '[{"offset":0,"length":4,"type":{"@type":"textEntityTypeTextUrl","url":"https://example.org"}}]');
        "#).unwrap();
        super::SCHEMA.apply(&c).unwrap();
        super::SCHEMA.apply(&c).unwrap();
        let known: Vec<bool> = c.prepare("SELECT entities_known FROM tg_message ORDER BY id").unwrap()
            .query_map([], |r| r.get(0)).unwrap().collect::<Result<_, _>>().unwrap();
        assert_eq!(known, vec![false, true]);
        let stored: String = c.query_row("SELECT entities FROM tg_message WHERE id = 2", [], |r| r.get(0)).unwrap();
        assert!(stored.contains("https://example.org"));
    }

    #[test]
    fn v11_preserves_existing_contacts_messages_and_link_metadata() {
        let old = kernel::app::Schema { app: "telegram", steps: &super::SCHEMA.steps[..10] };
        let c = Connection::open_in_memory().unwrap();
        c.pragma_update(None, "foreign_keys", "ON").unwrap();
        c.execute_batch("CREATE TABLE meta(key TEXT PRIMARY KEY, value ANY)").unwrap();
        old.apply(&c).unwrap();
        c.execute_batch(
            "INSERT INTO tg_peer(id, kind, name, is_contact) VALUES(10, 'person', 'Vera', 1);
             INSERT INTO tg_chat(peer) VALUES(10);",
        ).unwrap();
        let entities = r#"[{"offset":0,"length":7,"type":{"@type":"textEntityTypeTextUrl","url":"https://example.org"}}]"#;
        c.execute(
            "INSERT INTO tg_message(id, chat, date, text, entities, entities_known) VALUES(1, 10, 1.0, 'keep me', ?1, 1)",
            [entities],
        ).unwrap();
        super::SCHEMA.apply(&c).unwrap();
        super::SCHEMA.apply(&c).unwrap();
        assert_eq!(c.query_row("SELECT is_contact, blocked FROM tg_peer WHERE id = 10", [],
            |r| Ok((r.get::<_, i64>(0)?, r.get::<_, i64>(1)?))).unwrap(), (1, 0));
        let message: (String, String, bool) = c.query_row(
            "SELECT text, entities, entities_known FROM tg_message WHERE chat = 10 AND id = 1", [],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        ).unwrap();
        assert_eq!(message, ("keep me".to_string(), entities.to_string(), true));
    }

    #[test]
    fn v12_preserves_existing_messages_and_adds_unread_mention_state() {
        let c = Connection::open_in_memory().unwrap();
        c.pragma_update(None, "foreign_keys", true).unwrap();
        for step in &super::SCHEMA.steps[..11] {
            match step {
                kernel::app::Step::Sql(sql) => c.execute_batch(sql).unwrap(),
                kernel::app::Step::Run(run) | kernel::app::Step::Always(run) => run(&c).unwrap(),
                _ => unreachable!("the first eleven Telegram migrations are SQL or Run"),
            }
        }
        c.execute_batch("INSERT INTO tg_peer(id, kind, name) VALUES(10, 'group', 'a group');
            INSERT INTO tg_chat(peer, mention) VALUES(10, 1);
            INSERT INTO tg_message(id, chat, date, text, reply_to) VALUES(20, 10, 1, 'old reply', 19);").unwrap();
        c.execute_batch(super::V12).unwrap();
        let row: (String, i64, bool, bool) = c.query_row(
            "SELECT text, reply_to, unread_mention, mention_read FROM tg_message WHERE chat = 10 AND id = 20",
            [], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
        ).unwrap();
        assert_eq!(row, ("old reply".to_string(), 19, false, false));
        let indexed: i64 = c.query_row("SELECT COUNT(*) FROM tg_message_fts WHERE tg_message_fts MATCH 'reply'", [], |r| r.get(0)).unwrap();
        assert_eq!(indexed, 1);
    }

    /// The upgrade path a fresh store never walks: a store already carrying
    /// V1's messages runs only V2, and its `'rebuild'` must catch the lines
    /// that were there before the index existed — the seed's own inserts,
    /// which the triggers cannot have seen. Then the trigger trio keeps step
    /// with what lands after.
    #[test]
    fn v2_indexes_the_lines_v1_already_held_and_keeps_step() {
        let c = Connection::open_in_memory().expect("an in-memory db");
        c.execute_batch(super::V1).expect("V1");
        // A peer and its chat first, since the foreign keys are enforced;
        // then two lines, the way a V1 store already holds them.
        c.execute_batch(
            "INSERT INTO tg_peer(id, kind, name) VALUES(10, 'group', 'Zoo');
             INSERT INTO tg_chat(peer) VALUES(10);
             INSERT INTO tg_message(id, chat, date, text) VALUES(1, 10, 1.0, 'aardvark before');
             INSERT INTO tg_message(id, chat, date, text) VALUES(2, 10, 2.0, 'unrelated line');",
        )
        .expect("V1 rows");

        // V2 on a store that already has rows: the rebuild fills the index
        // from them.
        c.execute_batch(super::V2).expect("V2");
        let found: i64 = c
            .query_row(
                "SELECT COUNT(*) FROM tg_message_fts WHERE tg_message_fts MATCH 'aardvark'",
                [],
                |r| r.get(0),
            )
            .expect("a match count");
        assert_eq!(found, 1, "the pre-existing line was rebuilt into the index");

        // A line that lands after V2 is indexed by the AFTER INSERT trigger.
        c.execute(
            "INSERT INTO tg_message(id, chat, date, text) VALUES(3, 10, 3.0, 'aardvark after')",
            [],
        )
        .expect("a later row");
        let found: i64 = c
            .query_row(
                "SELECT COUNT(*) FROM tg_message_fts WHERE tg_message_fts MATCH 'aardvark'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(found, 2, "the trigger indexed the new line too");

        // An edit re-indexes, a delete un-indexes: the term follows the row.
        c.execute("UPDATE tg_message SET text = 'wombat instead' WHERE id = 1", [])
            .unwrap();
        c.execute("DELETE FROM tg_message WHERE id = 3", []).unwrap();
        let aardvark: i64 = c
            .query_row(
                "SELECT COUNT(*) FROM tg_message_fts WHERE tg_message_fts MATCH 'aardvark'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        let wombat: i64 = c
            .query_row(
                "SELECT COUNT(*) FROM tg_message_fts WHERE tg_message_fts MATCH 'wombat'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!((aardvark, wombat), (0, 1), "the edit and the delete both reached the index");
    }

    /// The fifth rung, with foreign keys on as the store keeps them: a reply
    /// chain survives the rebuild, a reply to a line the window lacks lands,
    /// a trim past a replied-to line goes through, the index still answers,
    /// and a chat has its outbox cursor.
    #[test]
    fn v5_frees_a_reply_from_the_window() {
        let c = Connection::open_in_memory().expect("an in-memory db");
        c.pragma_update(None, "foreign_keys", "ON").unwrap();
        c.execute_batch(super::V1).unwrap();
        c.execute_batch(super::V2).unwrap();
        c.execute_batch(super::V3).unwrap();
        super::v4_media_columns(&c).unwrap();
        c.execute_batch(
            "INSERT INTO tg_peer(id, kind, name) VALUES(10, 'group', 'Zoo');
             INSERT INTO tg_chat(peer) VALUES(10);
             INSERT INTO tg_message(id, chat, date, text) VALUES(1, 10, 1.0, 'aardvark');
             INSERT INTO tg_message(id, chat, date, text, reply_to) VALUES(2, 10, 2.0, 'and back', 1);",
        )
        .unwrap();
        // What V1 refused: a reply to a line the window lacks, and a trim
        // past a replied-to line.
        assert!(c
            .execute("INSERT INTO tg_message(id, chat, date, text, reply_to) VALUES(3, 10, 3.0, 'x', 999)", [])
            .is_err());
        assert!(c.execute("DELETE FROM tg_message WHERE id = 1", []).is_err());

        c.execute_batch(super::V5).expect("V5");

        let kept: Vec<(i64, Option<i64>)> = c
            .prepare("SELECT id, reply_to FROM tg_message ORDER BY id")
            .unwrap()
            .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))
            .unwrap()
            .collect::<Result<_, _>>()
            .unwrap();
        assert_eq!(kept, vec![(1, None), (2, Some(1))], "the rows and the reply crossed");
        c.execute("INSERT INTO tg_message(id, chat, date, text, reply_to) VALUES(3, 10, 3.0, 'wombat', 999)", [])
            .expect("a reply to an absent line lands");
        c.execute("DELETE FROM tg_message WHERE id = 1", []).expect("a replied-to line trims");
        let found: i64 = c
            .query_row("SELECT COUNT(*) FROM tg_message_fts WHERE tg_message_fts MATCH 'wombat'", [], |r| r.get(0))
            .unwrap();
        assert_eq!(found, 1, "the trigger trio was made again on the new table");
        let gone: i64 = c
            .query_row("SELECT COUNT(*) FROM tg_message_fts WHERE tg_message_fts MATCH 'aardvark'", [], |r| r.get(0))
            .unwrap();
        assert_eq!(gone, 0, "and the index followed the delete");
        // The other foreign keys still hold.
        assert!(c
            .execute("INSERT INTO tg_message(id, chat, date, text) VALUES(4, 77, 4.0, 'no such chat')", [])
            .is_err());
        c.execute("UPDATE tg_chat SET read_outbox = 2 WHERE peer = 10", [])
            .expect("the outbox cursor column is there");
    }

    /// The sixth rung, on a store that has climbed the whole ladder: a video
    /// line gains the clip's cache key and the id it is asked for by, the
    /// poster is left where it was, and a line with no clip stays empty in
    /// both.
    #[test]
    fn v6_gives_a_video_line_its_clip() {
        let c = Connection::open_in_memory().expect("an in-memory db");
        c.execute_batch(super::V1).unwrap();
        c.execute_batch(super::V2).unwrap();
        c.execute_batch(super::V3).unwrap();
        super::v4_media_columns(&c).unwrap();
        c.execute_batch(super::V5).unwrap();
        c.execute_batch(
            "INSERT INTO tg_peer(id, kind, name) VALUES(10, 'group', 'Zoo');
             INSERT INTO tg_chat(peer) VALUES(10);
             INSERT INTO tg_message(id, chat, date, text, media, media_ref)
               VALUES(1, 10, 1.0, 'the reef', 'video', 'tg:poster');",
        )
        .unwrap();
        assert!(
            c.execute("UPDATE tg_message SET media_clip = 'tg:clip' WHERE id = 1", [])
                .is_err(),
            "before the rung there is nowhere to put a clip"
        );

        c.execute_batch(super::V6).expect("V6");
        c.execute(
            "UPDATE tg_message SET media_clip = 'tg:clip', media_clip_rid = 'RID' WHERE id = 1",
            [],
        )
        .expect("the clip lands beside the poster");
        let (poster, clip, rid): (String, Option<String>, Option<String>) = c
            .query_row(
                "SELECT media_ref, media_clip, media_clip_rid FROM tg_message WHERE id = 1",
                [],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )
            .unwrap();
        assert_eq!(poster, "tg:poster", "the poster is untouched");
        assert_eq!(clip.as_deref(), Some("tg:clip"));
        assert_eq!(rid.as_deref(), Some("RID"));

        // A line that carries no clip — a photo, a text — has both empty.
        c.execute(
            "INSERT INTO tg_message(id, chat, date, text, media, media_ref)
             VALUES(2, 10, 2.0, 'the garden', 'photo', 'tg:pic')",
            [],
        )
        .unwrap();
        let empty: (Option<String>, Option<String>) = c
            .query_row(
                "SELECT media_clip, media_clip_rid FROM tg_message WHERE id = 2",
                [],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap();
        assert_eq!(empty, (None, None));
    }

    /// The seventh rung: the demo world's chats stay listed, a signed-in
    /// store's are reset for the engine to re-list, and the rung runs twice
    /// without complaint.
    #[test]
    fn v7_resets_a_signed_in_stores_listing_and_keeps_the_demos() {
        let ladder = |c: &Connection| {
            c.execute_batch(super::V1).unwrap();
            c.execute_batch(super::V2).unwrap();
            c.execute_batch(super::V3).unwrap();
            super::v4_media_columns(c).unwrap();
            c.execute_batch(super::V5).unwrap();
            c.execute_batch(super::V6).unwrap();
            c.execute_batch(
                "INSERT INTO tg_peer(id, kind, name) VALUES(10, 'group', 'Zoo');
                 INSERT INTO tg_chat(peer) VALUES(10);",
            )
            .unwrap();
        };
        let listed = |c: &Connection| -> i64 {
            c.query_row("SELECT in_main FROM tg_chat WHERE peer = 10", [], |r| r.get(0))
                .unwrap()
        };

        let demo = Connection::open_in_memory().unwrap();
        ladder(&demo);
        super::v7_listing(&demo).expect("V7 on a demo store");
        assert_eq!(listed(&demo), 1, "no account: the demo's chats stay in the list");
        super::v7_listing(&demo).expect("V7 again is a no-op");

        let signed = Connection::open_in_memory().unwrap();
        ladder(&signed);
        super::set_session(&signed, Some("+4915150525565"), "ready", None, 1.0).unwrap();
        super::v7_listing(&signed).expect("V7 on a signed-in store");
        assert_eq!(listed(&signed), 0, "signed in: the engine re-lists what is mine");
    }

    /// The eighth rung, on a store that has climbed the whole ladder with
    /// real lines in it — this morning's store, in miniature. Every row
    /// crosses with its reply link, the index answers over the new key, and
    /// what `V1`'s primary key made impossible is possible: two channels
    /// each holding their own line with the one number for an id.
    #[test]
    fn v8_gives_a_message_a_row_key_of_its_own() {
        let c = Connection::open_in_memory().expect("an in-memory db");
        c.pragma_update(None, "foreign_keys", "ON").unwrap();
        c.execute_batch(super::V1).unwrap();
        c.execute_batch(super::V2).unwrap();
        c.execute_batch(super::V3).unwrap();
        super::v4_media_columns(&c).unwrap();
        c.execute_batch(super::V5).unwrap();
        c.execute_batch(super::V6).unwrap();
        super::v7_listing(&c).unwrap();
        c.execute_batch(
            "INSERT INTO tg_peer(id, kind, name) VALUES(10, 'channel', 'Zoo News');
             INSERT INTO tg_peer(id, kind, name) VALUES(20, 'channel', 'Reef Weekly');
             INSERT INTO tg_chat(peer) VALUES(10);
             INSERT INTO tg_chat(peer) VALUES(20);
             INSERT INTO tg_message(id, chat, date, text, media, media_ref)
               VALUES(1048576, 10, 1.0, 'aardvark arrives', 'photo', 'tg:pic');
             INSERT INTO tg_message(id, chat, date, text, reply_to)
               VALUES(1048577, 10, 2.0, 'and back', 1048576);
             INSERT INTO tg_message(id, chat, date, text) VALUES(9, 20, 3.0, 'wombat');
             INSERT INTO tg_message(id, chat, date, text)
               VALUES(44040192, 10, 4.0, 'zebra crossing');",
        )
        .unwrap();
        // What `V1` made of a second channel's post wearing the same id: the
        // first channel's line, rewritten and carried into the other
        // conversation — the projection's own upsert, on the old key.
        c.execute(
            "INSERT INTO tg_message(id, chat, date, text) VALUES(44040192, 20, 5.0, 'a reef post')
             ON CONFLICT(id) DO UPDATE SET chat = excluded.chat, text = excluded.text",
            [],
        )
        .unwrap();
        let moved: i64 = c
            .query_row("SELECT chat FROM tg_message WHERE id = 44040192", [], |r| r.get(0))
            .unwrap();
        assert_eq!(moved, 20, "the bug this rung is for: the line changed chats");

        c.execute_batch(super::V8).expect("V8");

        // Every row crossed, with its chat, its reply link and its media,
        // each keeping its old key as its `seq`.
        let kept: Vec<(i64, i64, i64, Option<i64>)> = c
            .prepare("SELECT seq, id, chat, reply_to FROM tg_message ORDER BY seq")
            .unwrap()
            .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)))
            .unwrap()
            .collect::<Result<_, _>>()
            .unwrap();
        assert_eq!(
            kept,
            vec![
                (9, 9, 20, None),
                (1_048_576, 1_048_576, 10, None),
                (1_048_577, 1_048_577, 10, Some(1_048_576)),
                (44_040_192, 44_040_192, 20, None),
            ],
            "the rows crossed as they stood, the reply link with them"
        );
        let pic: String = c
            .query_row("SELECT media_ref FROM tg_message WHERE id = 1048576", [], |r| r.get(0))
            .unwrap();
        assert_eq!(pic, "tg:pic");

        // The index was moved to the new key and rebuilt from the rows.
        let found = |term: &str| -> i64 {
            c.query_row(
                "SELECT COUNT(*) FROM tg_message_fts WHERE tg_message_fts MATCH ?1",
                [term],
                |r| r.get(0),
            )
            .unwrap()
        };
        assert_eq!(found("aardvark"), 1, "the lines from before the rung are indexed");
        assert_eq!(found("wombat"), 1);

        // And now the thing V1 could not hold: one id, two chats.
        c.execute(
            "INSERT INTO tg_message(id, chat, date, text) VALUES(44040192, 10, 6.0, 'quokka news')",
            [],
        )
        .expect("the same id in another chat");
        assert_eq!(found("quokka"), 1, "the AFTER INSERT trigger followed on seq");
        let both: i64 = c
            .query_row("SELECT COUNT(*) FROM tg_message WHERE id = 44040192", [], |r| r.get(0))
            .unwrap();
        assert_eq!(both, 2, "each chat keeps its own line");
        assert!(
            c.execute(
                "INSERT INTO tg_message(id, chat, date, text) VALUES(44040192, 10, 7.0, 'twice')",
                [],
            )
            .is_err(),
            "and one chat holds one line per id"
        );

        // The trigger trio, on the new key: an edit re-indexes, a delete
        // strikes, and only the chat's own row goes.
        c.execute(
            "UPDATE tg_message SET text = 'narwhal news' WHERE chat = 10 AND id = 44040192",
            [],
        )
        .unwrap();
        assert_eq!((found("quokka"), found("narwhal")), (0, 1));
        c.execute("DELETE FROM tg_message WHERE chat = 10 AND id = 44040192", [])
            .unwrap();
        assert_eq!(found("narwhal"), 0);
        let left: i64 = c
            .query_row("SELECT COUNT(*) FROM tg_message WHERE id = 44040192", [], |r| r.get(0))
            .unwrap();
        assert_eq!(left, 1, "the other chat's line stands");

        // The picture's own durable name has a column.
        c.execute(
            "UPDATE tg_message SET media_rid = 'RID_PIC' WHERE chat = 10 AND id = 1048576",
            [],
        )
        .expect("the remote id lands beside the reference");
    }

    /// The repair rung, on the store shape that needed it: a `tg_message`
    /// from `V1`'s first day — no media columns — climbs to the same table a
    /// fresh store has, a message with a blob reference lands, and running
    /// the rung again changes nothing.
    #[test]
    fn v4_gives_a_first_shape_store_the_media_columns() {
        let c = Connection::open_in_memory().expect("an in-memory db");
        c.execute_batch(super::V1).expect("V1");
        // The first shape, reconstructed: V1 without the seven the media
        // round added in place.
        for (name, _) in super::MEDIA_COLUMNS {
            c.execute_batch(&format!("ALTER TABLE tg_message DROP COLUMN {name}"))
                .expect("drop a media column");
        }
        c.execute_batch(super::V2).expect("V2");
        c.execute_batch(super::V3).expect("V3");
        c.execute_batch(
            "INSERT INTO tg_peer(id, kind, name) VALUES(10, 'group', 'Zoo');
             INSERT INTO tg_chat(peer) VALUES(10);",
        )
        .unwrap();
        assert!(
            c.execute(
                "INSERT INTO tg_message(id, chat, date, text, media_ref) VALUES(1, 10, 1.0, 'x', 'tg:a')",
                [],
            )
            .is_err(),
            "the first shape refuses a media reference — the live failure"
        );

        super::v4_media_columns(&c).expect("V4");
        let cols: Vec<String> = c
            .prepare("PRAGMA table_info(tg_message)")
            .unwrap()
            .query_map([], |r| r.get::<_, String>(1))
            .unwrap()
            .collect::<Result<_, _>>()
            .unwrap();
        for (name, _) in super::MEDIA_COLUMNS {
            assert!(cols.contains(&name.to_string()), "{name} added");
        }
        c.execute(
            "INSERT INTO tg_message(id, chat, date, text, media, media_ref, media_w)
             VALUES(1, 10, 1.0, 'x', 'photo', 'tg:a', 640)",
            [],
        )
        .expect("a photo line lands");
        // And the index the V2 trigger keeps followed the insert.
        let found: i64 = c
            .query_row("SELECT COUNT(*) FROM tg_message_fts WHERE tg_message_fts MATCH 'x'", [], |r| r.get(0))
            .unwrap();
        assert_eq!(found, 1);

        // Idempotent: a store that already has them — a fresh V1 — is left as
        // it is.
        super::v4_media_columns(&c).expect("V4 again is a no-op");
        let fresh = Connection::open_in_memory().unwrap();
        fresh.execute_batch(super::V1).unwrap();
        super::v4_media_columns(&fresh).expect("nothing to add on a fresh V1");
    }

    /// The third rung is additive: a store already at V2 gains `tg_session`,
    /// the row a fresh read conjures defaults to 'closed', and a write lands
    /// on the single row and reads back whole — a later state change keeping
    /// the phone an earlier one filed.
    #[test]
    fn v3_adds_the_session_row_defaulting_to_closed() {
        let c = Connection::open_in_memory().expect("an in-memory db");
        c.execute_batch(super::V1).expect("V1");
        c.execute_batch(super::V2).expect("V2");
        // No table yet: the reader still answers, with the closed default.
        let before = super::session(&c);
        assert_eq!(before.state, "closed");
        assert_eq!(before.phone, None);

        // The V3 rung, as the ladder runs it on an existing V2 store.
        c.execute_batch(super::V3).expect("V3");
        assert_eq!(super::session(&c).state, "closed", "the table's own default");

        super::set_session(&c, Some("+4915150525562"), "wait_code", Some("sms · 5"), 42.0)
            .expect("write the session");
        let after = super::session(&c);
        assert_eq!(after.state, "wait_code");
        assert_eq!(after.phone.as_deref(), Some("+4915150525562"));
        assert_eq!(after.detail.as_deref(), Some("sms · 5"));
        assert_eq!(after.updated, Some(42.0));

        // Advancing the state keeps the phone (COALESCE(NULL, existing)) and
        // clears the step's detail.
        super::set_session(&c, None, "ready", None, 43.0).expect("advance");
        let ready = super::session(&c);
        assert_eq!(ready.state, "ready");
        assert_eq!(ready.phone.as_deref(), Some("+4915150525562"), "phone kept");
        assert_eq!(ready.detail, None);

        // The single-row guard holds: a second id cannot be inserted.
        assert!(
            c.execute("INSERT INTO tg_session(id, state) VALUES(2, 'ready')", [])
                .is_err(),
            "the CHECK(id = 1) keeps it a single row"
        );
    }
}
