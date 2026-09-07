//! Idempotent writes of normalized Telegram updates into the local projection.
//!
//! The worker supplies `Incoming*` values decoded by `updates`. All writes
//! run on the store's writer inside the caller's transaction; FTS triggers
//! keep the search index in step with message upserts and retention.
#![cfg_attr(not(feature = "tdlib"), allow(dead_code))]

use rusqlite::Connection;

use super::model::{Media, MsgHit, MsgId, PeerId};

// -- what comes down the wire -------------------------------------------------------

/// A peer as an update carries it: who or what, and what is known about
/// them. Kept close to TDLib's `user` / `basicGroup` / `supergroup` /
/// `chat` fields, flattened to the columns `tg_peer` already has.
#[derive(Debug, Clone, PartialEq)]
pub struct IncomingPeer {
    pub id: PeerId,
    /// `person`, `group` or `channel`.
    pub kind: String,
    pub name: String,
    pub username: Option<String>,
    pub about: Option<String>,
    pub phone: Option<String>,
    /// A person's presence word: `online`, `recently`, `week`, `month`,
    /// `long`.
    pub status: Option<String>,
    pub last_seen: Option<f64>,
    pub members: Option<i64>,
    pub online: Option<i64>,
    pub admin: bool,
    pub is_contact: bool,
    pub is_self: bool,
}

/// One row of the dialog list: the chat's flags, drawn whole because the
/// list is small and wanted always. `typing` is an ephemeral update rather
/// than a stored fact, but it has a column, so it rides along.
#[derive(Debug, Clone, PartialEq)]
pub struct IncomingChat {
    pub peer: PeerId,
    /// 0 unpinned, else the place among the pinned, 1 first.
    pub pinned: i64,
    pub muted: bool,
    pub archived: bool,
    /// Whether the chat sits in my main list — a conversation of mine — as
    /// against one the engine merely came to know: a channel a line was
    /// forwarded from, a group a reply was quoted out of, a peer who was
    /// mentioned. The engine announces every chat it learns of the same way,
    /// and only the positions tell them apart (Andrey, 2026-09-07: chats I
    /// never joined were in the list).
    pub in_main: bool,
    pub unread: i64,
    pub mention: bool,
    pub draft: Option<String>,
    pub typing: Option<String>,
    /// The last line I read, where the unread line is drawn.
    pub last_read: Option<MsgId>,
}

/// One membership: a peer in a group, and whether they run it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct IncomingMember {
    pub chat: PeerId,
    pub peer: PeerId,
    pub admin: bool,
}

/// One message as an update carries it. The fields are TDLib's `message`,
/// flattened: its id and chat, who sent it and when, the text, my-or-theirs
/// and its send state, what it answers and what it was forwarded from, its
/// media (kind and reference, the bytes fetched later into the blob cache),
/// a channel post's counts, and whether it is a service line nobody wrote.
#[derive(Debug, Clone, PartialEq)]
pub struct IncomingMessage {
    pub id: MsgId,
    pub chat: PeerId,
    /// `None` for a service line and a channel's own post.
    pub sender: Option<PeerId>,
    pub date: f64,
    pub text: String,
    pub out: bool,
    /// Of mine: `sending`, `sent`, `read`, `failed`.
    pub state: Option<String>,
    pub edited: bool,
    pub reply_to: Option<MsgId>,
    pub fwd_from: Option<String>,
    /// The reference in `media.reference` is what the blob cache is keyed by
    /// once the file is fetched; the store holds no bytes.
    pub media: Option<Media>,
    pub views: Option<i64>,
    pub comments: Option<i64>,
    pub reactions: Option<String>,
    pub service: bool,
}

// -- the projections ----------------------------------------------------------------

// Everything a source may not know is coalesced onto what the row already
// holds: no two updates about one peer carry the same fields, and the last to
// arrive must not erase what an earlier one knew. A chat brings a group's
// title and nothing else; `updateSupergroupFullInfo` brings its size and its
// description; a user brings a handle and a presence. Without the coalesce
// every group read 0 members the moment its chat was re-projected
// (2026-09-07). What a mapper *does* know it says outright, absent or not —
// which is why a presence is always a word (see
// [`updates::peer`](super::updates::peer)) and a nought member count is
// dropped there rather than here.
const UPSERT_PEER: &str = "
INSERT INTO tg_peer(
  id, kind, name, username, about, phone, status, last_seen,
  members, online, admin, is_contact, is_self)
VALUES(?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13)
ON CONFLICT(id) DO UPDATE SET
  kind = excluded.kind, name = excluded.name,
  username = COALESCE(excluded.username, tg_peer.username),
  about = COALESCE(excluded.about, tg_peer.about),
  phone = COALESCE(excluded.phone, tg_peer.phone),
  status = COALESCE(excluded.status, tg_peer.status),
  last_seen = COALESCE(excluded.last_seen, tg_peer.last_seen),
  members = COALESCE(excluded.members, tg_peer.members),
  online = COALESCE(excluded.online, tg_peer.online),
  admin = MAX(excluded.admin, tg_peer.admin),
  is_contact = excluded.is_contact,
  -- The account holder is named once, by `my_id`; a user update that
  -- knows nothing of it must not unname them.
  is_self = MAX(excluded.is_self, tg_peer.is_self)";

/// Upserts peers — the dialog list's people and groups, a chat's members,
/// a message's senders. Idempotent on the peer's id, and additive: a field
/// the update knew nothing of keeps the value the row had.
///
/// # Errors
///
/// If the store refuses the write.
pub fn project_peers(c: &Connection, peers: &[IncomingPeer]) -> rusqlite::Result<()> {
    let mut stmt = c.prepare_cached(UPSERT_PEER)?;
    for p in peers {
        stmt.execute(rusqlite::params![
            p.id,
            p.kind,
            p.name,
            p.username,
            p.about,
            p.phone,
            p.status,
            p.last_seen,
            p.members,
            p.online,
            p.admin,
            p.is_contact,
            p.is_self,
        ])?;
    }
    Ok(())
}

const UPSERT_CHAT: &str = "
INSERT INTO tg_chat(peer, pinned, muted, archived, unread, mention, draft, typing, last_read, in_main)
VALUES(?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)
ON CONFLICT(peer) DO UPDATE SET
  pinned = excluded.pinned, muted = excluded.muted, archived = excluded.archived,
  unread = excluded.unread, mention = excluded.mention, draft = excluded.draft,
  typing = excluded.typing, last_read = excluded.last_read, in_main = excluded.in_main";

/// Upserts the dialog list — one chat row per peer I have a conversation
/// with, its flags and its unread count as the server counts them.
/// Idempotent on the peer. The peer itself must be projected first, the
/// foreign key insists.
///
/// # Errors
///
/// If the store refuses the write.
pub fn project_chats(c: &Connection, chats: &[IncomingChat]) -> rusqlite::Result<()> {
    let mut stmt = c.prepare_cached(UPSERT_CHAT)?;
    for ch in chats {
        stmt.execute(rusqlite::params![
            ch.peer,
            ch.pinned,
            ch.muted,
            ch.archived,
            ch.unread,
            ch.mention,
            ch.draft,
            ch.typing,
            ch.last_read,
            ch.in_main,
        ])?;
    }
    Ok(())
}

const UPSERT_MEMBER: &str = "
INSERT INTO tg_member(chat, peer, admin) VALUES(?1, ?2, ?3)
ON CONFLICT(chat, peer) DO UPDATE SET admin = excluded.admin";

/// Upserts a group's membership. Both peers — the group and the member —
/// must be projected first.
///
/// # Errors
///
/// If the store refuses the write.
pub fn project_members(c: &Connection, members: &[IncomingMember]) -> rusqlite::Result<()> {
    let mut stmt = c.prepare_cached(UPSERT_MEMBER)?;
    for m in members {
        stmt.execute(rusqlite::params![m.chat, m.peer, m.admin])?;
    }
    Ok(())
}

// The conflict is on the *pair*: a message id is Telegram's, unique within
// its chat and nowhere else, and taking it for an identity moved a channel's
// post into another channel (V8). Nothing here says `chat = excluded.chat`
// any more, the chat being half of what is being matched on.
const UPSERT_MESSAGE: &str = "
INSERT INTO tg_message(
  id, chat, sender, date, text, out, state, edited, reply_to, fwd_from,
  media, media_label, media_ref, media_rid, media_w, media_h, media_secs,
  media_lat, media_lon, media_until, media_clip, media_clip_rid,
  views, comments, reactions, service)
VALUES(?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16,
       ?17, ?18, ?19, ?20, ?21, ?22, ?23, ?24, ?25, ?26)
ON CONFLICT(chat, id) DO UPDATE SET
  sender = excluded.sender, date = excluded.date,
  text = excluded.text, out = excluded.out, state = excluded.state,
  edited = excluded.edited, reply_to = excluded.reply_to,
  fwd_from = excluded.fwd_from, media = excluded.media,
  media_label = excluded.media_label, media_ref = excluded.media_ref,
  media_rid = excluded.media_rid,
  media_w = excluded.media_w, media_h = excluded.media_h,
  media_secs = excluded.media_secs, media_lat = excluded.media_lat,
  media_lon = excluded.media_lon, media_until = excluded.media_until,
  media_clip = excluded.media_clip, media_clip_rid = excluded.media_clip_rid,
  views = excluded.views, comments = excluded.comments,
  reactions = excluded.reactions, service = excluded.service";

/// Upserts a batch of a chat's messages. Idempotent on the chat and the
/// message id together, so a re-sync of the same window writes the same rows;
/// the AFTER INSERT/UPDATE triggers keep the full-text index in step either
/// way. The chat and every sender must already be peers in the store.
///
/// # Errors
///
/// If the store refuses the write.
pub fn project_messages(c: &Connection, msgs: &[IncomingMessage]) -> rusqlite::Result<()> {
    let mut stmt = c.prepare_cached(UPSERT_MESSAGE)?;
    for m in msgs {
        let md = m.media.as_ref();
        stmt.execute(rusqlite::params![
            m.id,
            m.chat,
            m.sender,
            m.date,
            m.text,
            m.out,
            m.state,
            m.edited,
            m.reply_to,
            m.fwd_from,
            md.map(|x| x.kind.as_str()),
            md.and_then(|x| x.label.as_deref()),
            md.and_then(|x| x.reference.as_deref()),
            md.and_then(|x| x.rid.as_deref()),
            md.and_then(|x| x.w),
            md.and_then(|x| x.h),
            md.and_then(|x| x.secs),
            md.and_then(|x| x.lat),
            md.and_then(|x| x.lon),
            md.and_then(|x| x.until),
            md.and_then(|x| x.clip.as_deref()),
            md.and_then(|x| x.clip_rid.as_deref()),
            m.views,
            m.comments,
            m.reactions,
            m.service,
        ])?;
    }
    Ok(())
}

// -- the window ---------------------------------------------------------------------

/// The history the store keeps per chat: the newest this many lines, trimmed
/// as new ones arrive. Scrolling within the window is the store's; scrolling
/// past it needs a separate paging path; the current UI reads this window.
pub const HISTORY_KEEP: usize = 10_000;

/// Marks read every sent line up to the chat's outbox cursor — where the far
/// side has read to, as `tg_chat.read_outbox` holds it. Run after a batch
/// lands and when the cursor moves, so the state is right whichever came
/// first; a pending or a failed line is nobody's to have read.
///
/// # Errors
///
/// If the store refuses the write.
pub fn apply_read_outbox(c: &Connection, chat: PeerId) -> rusqlite::Result<()> {
    c.execute(
        "UPDATE tg_message SET state = 'read'
         WHERE chat = ?1 AND out = 1 AND state = 'sent'
           AND id <= COALESCE((SELECT read_outbox FROM tg_chat WHERE peer = ?1), 0)",
        [chat],
    )
    .map(|_| ())
}

/// What the window holds of a chat: its oldest line's id — `None` for a
/// chat with no line yet — and how many lines. What a history walk reads to
/// decide where to ask from and whether to ask at all.
///
/// # Errors
///
/// If the store refuses the read.
pub fn history_window(c: &Connection, chat: PeerId) -> rusqlite::Result<(Option<MsgId>, i64)> {
    c.query_row(
        "SELECT MIN(id), COUNT(*) FROM tg_message WHERE chat = ?1",
        [chat],
        |r| Ok((r.get(0)?, r.get(1)?)),
    )
}

/// Trims a chat to the newest [`HISTORY_KEEP`] lines, dropping the rest.
/// Called after a batch is projected. The dropped rows' index entries follow
/// through the AFTER DELETE trigger, so the window and its search stay the
/// same size. Answers how many were dropped.
///
/// # Errors
///
/// If the store refuses the write.
pub fn trim_chat(c: &Connection, chat: PeerId) -> rusqlite::Result<usize> {
    let gone = c.execute(
        "DELETE FROM tg_message
         WHERE chat = ?1 AND seq NOT IN (
           SELECT seq FROM tg_message WHERE chat = ?1
           ORDER BY date DESC, id DESC LIMIT ?2
         )",
        rusqlite::params![chat, HISTORY_KEEP as i64],
    )?;
    Ok(gone)
}

// -- local indexed search -----------------------------------------

/// How many local hits one question is worth returning before the list is
/// paged.
const LOCAL_LIMIT: i64 = 500;

// The index is keyed by the row, not by the message: an external-content
// FTS5 table's rowid is its content table's, which is `seq` from V8 on.
const SEARCH_LOCAL_SQL: &str = "
SELECT m.seq, m.id, m.chat, p.name, COALESCE(s.name, ''), m.date, m.text, m.out,
       m.media, m.media_label, m.media_ref, m.media_rid, m.media_w, m.media_h,
       m.media_secs, m.media_lat, m.media_lon, m.media_until,
       m.media_clip, m.media_clip_rid
FROM tg_message_fts
JOIN tg_message m ON m.seq = tg_message_fts.rowid
JOIN tg_peer p ON p.id = m.chat
LEFT JOIN tg_peer s ON s.id = m.sender
WHERE tg_message_fts MATCH ?1 AND m.service = 0 AND (?2 IS NULL OR m.chat = ?2)
ORDER BY m.date DESC, m.id DESC
LIMIT ?3";

/// A search term as an FTS5 query: each word a prefix token, so `therm`
/// reaches `thermos`, joined by an implicit AND. Split on everything that is
/// not a letter or a digit, so a `@handle` or an `emoji.` cannot smuggle the
/// grammar's own punctuation into the match. `None` when the term is only
/// punctuation.
fn match_query(term: &str) -> Option<String> {
    let mut parts: Vec<String> = Vec::new();
    for word in term.split(|c: char| !c.is_alphanumeric()) {
        if !word.is_empty() {
            parts.push(format!("{}*", word.to_lowercase()));
        }
    }
    (!parts.is_empty()).then(|| parts.join(" "))
}

/// The lines a term reaches in the local window, latest first, service lines
/// left out — scoped to one chat, or across every chat when `chat` is
/// `None`. Over the full-text index, so it is instant and offline. The
/// spellings are a [`MsgHit`]'s, the same a messages list draws.
#[must_use]
#[allow(dead_code)] // The current UI uses substring search; this is the indexed query API.
pub fn search_local(conn: &Connection, chat: Option<PeerId>, term: &str) -> Vec<MsgHit> {
    let Some(query) = match_query(term) else {
        return Vec::new();
    };
    let mut stmt = match conn.prepare_cached(SEARCH_LOCAL_SQL) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("telegram: preparing the local search failed: {e}");
            return Vec::new();
        }
    };
    let rows = stmt.query_map(
        rusqlite::params![query, chat, LOCAL_LIMIT],
        super::model::msg_hit_row,
    );
    match rows {
        Ok(rows) => rows.filter_map(Result::ok).collect(),
        Err(e) => {
            eprintln!("telegram: the local search refused {term:?}: {e}");
            Vec::new()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::super::schema::SCHEMA;
    use super::super::seed::{self, HIKE, VERA};
    use super::*;
    use kernel::store::Store;
    use kernel::time::ts;

    /// A store at V2, with the demo world seeded through the trigger-fed
    /// index — the fixture every projection test starts from.
    fn store() -> Store {
        let store = Store::open(None, &[&SCHEMA]).expect("an in-memory telegram store");
        seed::seed_if_empty(&store).expect("the demo world");
        store
    }

    /// A message on a chat that stands in the demo world, with the id and
    /// date given and nothing else, sent by nobody (a channel-post shape, so
    /// no sender peer is needed).
    fn msg(id: MsgId, chat: PeerId, at: f64, text: &str) -> IncomingMessage {
        IncomingMessage {
            id,
            chat,
            sender: None,
            date: at,
            text: text.to_string(),
            out: false,
            state: None,
            edited: false,
            reply_to: None,
            fwd_from: None,
            media: None,
            views: None,
            comments: None,
            reactions: None,
            service: false,
        }
    }

    /// The index finds a projected line the moment it lands: the trigger runs
    /// inside the same write, so no rebuild is needed.
    #[test]
    fn a_projected_line_is_found_through_the_index() {
        let s = store();
        // A word the demo world does not carry, so the hit can only be the
        // projected line.
        s.write(|c| project_messages(c, &[msg(9_001, VERA, ts(2026, 9, 2, 9, 0), "quokka rendezvous")]))
            .unwrap();
        let raw: i64 = s
            .conn()
            .query_row(
                "SELECT COUNT(*) FROM tg_message_fts WHERE tg_message_fts MATCH 'quokka'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(raw, 1, "the fts row is there");
        let hits = search_local(s.conn(), None, "quokka");
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].id, 9_001);
        assert_eq!(hits[0].text, "quokka rendezvous");
    }

    /// A batch upserts, and the same batch twice leaves exactly the same
    /// rows: a re-sync is not a duplication.
    #[test]
    fn a_batch_projects_and_is_idempotent() {
        let s = store();
        // Invented marker words, so a hit can only be a projected line and
        // never a demo one.
        let batch = vec![
            msg(9_101, VERA, ts(2026, 9, 2, 10, 0), "narwhal alpha"),
            msg(9_102, VERA, ts(2026, 9, 2, 10, 1), "narwhal bravo"),
        ];
        let count = |s: &Store| -> i64 {
            s.conn()
                .query_row("SELECT COUNT(*) FROM tg_message WHERE chat = ?1", [VERA], |r| r.get(0))
                .unwrap()
        };
        let before = count(&s);
        let b1 = batch.clone();
        s.write(move |c| project_messages(c, &b1)).unwrap();
        let after_one = count(&s);
        assert_eq!(after_one, before + 2);
        // The same batch again — with a field changed on one line — updates
        // in place rather than inserting anew.
        let mut b2 = batch.clone();
        b2[1].text = "narwhal charlie".to_string();
        s.write(move |c| project_messages(c, &b2)).unwrap();
        assert_eq!(count(&s), before + 2, "no duplicate rows");
        let edited = search_local(s.conn(), Some(VERA), "charlie");
        assert_eq!(edited.len(), 1, "the update reached the index");
        assert!(search_local(s.conn(), Some(VERA), "bravo").is_empty(), "the old term is gone");
    }

    /// A moving picture's two files both land and both come back: the
    /// poster the transcript draws, and the clip beside it — where its bytes
    /// will be, and what they are asked for by. The clip is a name in the
    /// row, not a fetch; nothing about it makes the line draw differently
    /// until the file is on the device.
    #[test]
    fn a_video_line_keeps_its_poster_and_its_clip() {
        let s = store();
        let mut line = msg(9_301, VERA, ts(2026, 9, 2, 11, 0), "wombat reef");
        line.media = Some(Media {
            reference: Some("tg:poster".to_string()),
            rid: Some("RID_POSTER".to_string()),
            w: Some(640),
            h: Some(480),
            secs: Some(12),
            clip: Some("tg:reef".to_string()),
            clip_rid: Some("RID_REEF".to_string()),
            ..Media::of("video")
        });
        let batch = vec![line.clone()];
        s.write(move |c| project_messages(c, &batch)).unwrap();
        let hit = search_local(s.conn(), Some(VERA), "wombat");
        assert_eq!(hit.len(), 1);
        let md = hit[0].media.as_ref().expect("the media came back");
        assert_eq!(md.reference.as_deref(), Some("tg:poster"), "the poster");
        assert_eq!(md.rid.as_deref(), Some("RID_POSTER"), "and what to ask for it by");
        assert_eq!(md.clip.as_deref(), Some("tg:reef"));
        assert_eq!(md.clip_rid.as_deref(), Some("RID_REEF"));

        // And the upsert carries them over an edit, as it does every other
        // media column.
        let mut edited = line;
        edited.text = "wombat lagoon".to_string();
        edited.media.as_mut().unwrap().clip = Some("tg:lagoon".to_string());
        let batch = vec![edited];
        s.write(move |c| project_messages(c, &batch)).unwrap();
        let hit = search_local(s.conn(), Some(VERA), "lagoon");
        assert_eq!(hit.len(), 1);
        assert_eq!(
            hit[0].media.as_ref().and_then(|md| md.clip.as_deref()),
            Some("tg:lagoon"),
            "the second pass wrote the new clip"
        );
    }

    /// A message id is not a message's identity. Telegram numbers a
    /// supergroup's and a channel's lines `server_id << 20`, so every
    /// channel's first post is 1 048 576 and two channels collide on every
    /// number they use: before V8 the second post overwrote the first and
    /// carried the row into the other conversation. Each keeps its own line
    /// now — two rows, one id — and every reader is scoped to the chat: the
    /// search, the transcript, and the quote a reply carries.
    #[test]
    fn one_id_in_two_channels_is_two_lines() {
        // 42 << 20, the shape of a real channel's message id.
        const ID: MsgId = 44_040_192;
        let s = store();
        let channel = |id: PeerId, name: &str| IncomingPeer {
            id,
            kind: "channel".to_string(),
            name: name.to_string(),
            username: None,
            about: None,
            phone: None,
            status: None,
            last_seen: None,
            members: Some(2),
            online: None,
            admin: false,
            is_contact: false,
            is_self: false,
        };
        let listed = |peer: PeerId| IncomingChat {
            peer,
            pinned: 0,
            muted: false,
            archived: false,
            in_main: true,
            unread: 0,
            mention: false,
            draft: None,
            typing: None,
            last_read: None,
        };
        let peers = vec![channel(7_101, "Quokka Weekly"), channel(7_102, "Reef Weekly")];
        let chats = vec![listed(7_101), listed(7_102)];
        let mut answer = msg(ID + 1, 7_102, ts(2026, 9, 2, 10, 2), "narwhal, surely");
        answer.reply_to = Some(ID);
        let batch = vec![
            msg(ID, 7_101, ts(2026, 9, 2, 10, 0), "aardvark weekly"),
            msg(ID, 7_102, ts(2026, 9, 2, 10, 1), "wombat weekly"),
            answer,
        ];
        s.write(move |c| {
            project_peers(c, &peers)?;
            project_chats(c, &chats)?;
            project_messages(c, &batch)
        })
        .unwrap();

        let both: Vec<(PeerId, String)> = s
            .conn()
            .prepare("SELECT chat, text FROM tg_message WHERE id = ?1 ORDER BY chat")
            .unwrap()
            .query_map([ID], |r| Ok((r.get(0)?, r.get(1)?)))
            .unwrap()
            .collect::<Result<_, _>>()
            .unwrap();
        assert_eq!(
            both,
            vec![
                (7_101, "aardvark weekly".to_string()),
                (7_102, "wombat weekly".to_string())
            ],
            "one id, two lines, each in its own channel"
        );

        // The search finds both, each carrying the chat it is in, and a
        // search scoped to one channel sees only that one's line.
        let sweep = search_local(s.conn(), None, "weekly");
        assert_eq!(sweep.len(), 2);
        assert_eq!(
            search_local(s.conn(), Some(7_101), "aardvark")
                .iter()
                .map(|h| (h.id, h.chat))
                .collect::<Vec<_>>(),
            vec![(ID, 7_101)]
        );
        assert!(
            search_local(s.conn(), Some(7_102), "aardvark").is_empty(),
            "the other channel's line is not this channel's"
        );

        // The transcript, and the quote the reply carries: the reef line,
        // not the one that happens to wear the same number elsewhere.
        let quokka = super::super::model::history(&s, 7_101);
        assert_eq!(
            quokka.iter().map(|m| m.text.as_str()).collect::<Vec<_>>(),
            vec!["aardvark weekly"]
        );
        let reef = super::super::model::history(&s, 7_102);
        assert_eq!(reef.len(), 2);
        assert_eq!(reef[1].reply_text, "wombat weekly", "the reply quotes its own chat");

        // And the upsert lands on the pair: the same id in one channel is
        // that channel's line, edited, and the other's is untouched.
        s.write(|c| project_messages(c, &[msg(ID, 7_101, ts(2026, 9, 2, 10, 0), "quokka weekly")]))
            .unwrap();
        let after: Vec<String> = s
            .conn()
            .prepare("SELECT text FROM tg_message WHERE id = ?1 ORDER BY chat")
            .unwrap()
            .query_map([ID], |r| r.get(0))
            .unwrap()
            .collect::<Result<_, _>>()
            .unwrap();
        assert_eq!(after, vec!["quokka weekly".to_string(), "wombat weekly".to_string()]);
    }

    /// Peers, a chat and its members project whole, and a second pass with a
    /// changed flag updates in place.
    #[test]
    fn peers_chats_and_members_project_and_upsert() {
        let s = store();
        let peer = |id: PeerId, name: &str, kind: &str| IncomingPeer {
            id,
            kind: kind.to_string(),
            name: name.to_string(),
            username: None,
            about: None,
            phone: None,
            status: Some("online".to_string()),
            last_seen: None,
            members: (kind != "person").then_some(2),
            online: None,
            admin: false,
            is_contact: false,
            is_self: false,
        };
        let peers = vec![
            peer(5_001, "Quokka Club", "group"),
            peer(5_002, "Nadia Quokka", "person"),
        ];
        let p = peers.clone();
        s.write(move |c| project_peers(c, &p)).unwrap();
        let chat = IncomingChat {
            peer: 5_001,
            pinned: 0,
            muted: false,
            archived: false,
            in_main: true,
            unread: 3,
            mention: false,
            draft: None,
            typing: None,
            last_read: None,
        };
        let ch = chat.clone();
        s.write(move |c| project_chats(c, &[ch])).unwrap();
        s.write(|c| {
            project_members(
                c,
                &[IncomingMember { chat: 5_001, peer: 5_002, admin: false }],
            )
        })
        .unwrap();
        let unread: i64 = s
            .conn()
            .query_row("SELECT unread FROM tg_chat WHERE peer = 5001", [], |r| r.get(0))
            .unwrap();
        assert_eq!(unread, 3);
        let is_admin: i64 = s
            .conn()
            .query_row("SELECT admin FROM tg_member WHERE chat = 5001 AND peer = 5002", [], |r| r.get(0))
            .unwrap();
        assert_eq!(is_admin, 0);
        // A second pass: the member becomes an admin, the chat is read.
        s.write(|c| {
            project_members(
                c,
                &[IncomingMember { chat: 5_001, peer: 5_002, admin: true }],
            )
        })
        .unwrap();
        let mut read = chat.clone();
        read.unread = 0;
        s.write(move |c| project_chats(c, &[read])).unwrap();
        let (unread, is_admin): (i64, i64) = s
            .conn()
            .query_row(
                "SELECT c.unread, m.admin FROM tg_chat c JOIN tg_member m ON m.chat = c.peer
                 WHERE c.peer = 5001",
                [],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap();
        assert_eq!((unread, is_admin), (0, 1), "both upserts landed");
        let n_peers: i64 = s
            .conn()
            .query_row("SELECT COUNT(*) FROM tg_peer WHERE id IN (5001, 5002)", [], |r| r.get(0))
            .unwrap();
        assert_eq!(n_peers, 2, "no duplicate peers");
    }

    /// A second projection that knows less keeps what the first knew: the
    /// group's size, its online count and its description survive the chat
    /// object's own peer, which carries a title and nothing else. What the
    /// later one *does* carry still lands.
    #[test]
    fn a_peer_keeps_what_a_later_update_does_not_know() {
        let s = store();
        let known = IncomingPeer {
            id: 5_010,
            kind: "channel".to_string(),
            name: "Quokka Weekly".to_string(),
            username: Some("quokkaweekly".to_string()),
            about: Some("a letter about quokkas".to_string()),
            phone: Some("+41 00 000".to_string()),
            status: Some("online".to_string()),
            last_seen: Some(1_725_000_000.0),
            members: Some(812),
            online: Some(12),
            admin: false,
            is_contact: false,
            is_self: false,
        };
        s.write(move |c| project_peers(c, &[known])).unwrap();

        // What `updates::chat_peer` derives from a chat object: the title,
        // the kind, and no other fact at all.
        let bare = IncomingPeer {
            id: 5_010,
            kind: "channel".to_string(),
            name: "Quokka Weekly · the letter".to_string(),
            username: None,
            about: None,
            phone: None,
            status: None,
            last_seen: None,
            members: None,
            online: None,
            admin: true,
            is_contact: false,
            is_self: false,
        };
        s.write(move |c| project_peers(c, &[bare])).unwrap();

        // One column at a time, so the assertion says which field it means.
        let word = |col: &str| -> Option<String> {
            s.conn()
                .query_row(&format!("SELECT {col} FROM tg_peer WHERE id = 5010"), [], |r| r.get(0))
                .unwrap()
        };
        let count = |col: &str| -> Option<i64> {
            s.conn()
                .query_row(&format!("SELECT {col} FROM tg_peer WHERE id = 5010"), [], |r| r.get(0))
                .unwrap()
        };
        assert_eq!(word("name").as_deref(), Some("Quokka Weekly · the letter"), "a new title lands");
        assert_eq!(word("username").as_deref(), Some("quokkaweekly"));
        assert_eq!(word("about").as_deref(), Some("a letter about quokkas"));
        assert_eq!(word("phone").as_deref(), Some("+41 00 000"));
        assert_eq!(word("status").as_deref(), Some("online"));
        let last_seen: Option<f64> = s
            .conn()
            .query_row("SELECT last_seen FROM tg_peer WHERE id = 5010", [], |r| r.get(0))
            .unwrap();
        assert_eq!(last_seen, Some(1_725_000_000.0));
        assert_eq!(count("members"), Some(812), "the size the counts update knew");
        assert_eq!(count("online"), Some(12));
        assert_eq!(count("admin"), Some(1), "a flag the update always carries still flips");

        // And a value that does arrive replaces the one held.
        let fuller = IncomingPeer {
            about: Some("now weekly-ish".to_string()),
            members: Some(815),
            ..IncomingPeer {
                id: 5_010,
                kind: "channel".to_string(),
                name: "Quokka Weekly".to_string(),
                username: None,
                about: None,
                phone: None,
                status: None,
                last_seen: None,
                members: None,
                online: None,
                admin: false,
                is_contact: false,
                is_self: false,
            }
        };
        s.write(move |c| project_peers(c, &[fuller])).unwrap();
        let (about, members): (String, i64) = s
            .conn()
            .query_row("SELECT about, members FROM tg_peer WHERE id = 5010", [], |r| {
                Ok((r.get(0)?, r.get(1)?))
            })
            .unwrap();
        assert_eq!(about, "now weekly-ish");
        assert_eq!(members, 815);
    }

    /// The 10 000-line trim keeps the newest window and no more, and the
    /// index holds exactly those.
    #[test]
    fn the_trim_keeps_the_newest_window() {
        let s = store();
        // A fresh chat of its own, so the demo world is untouched: its peer,
        // its chat row, then the lines.
        s.write(|c| {
            project_peers(
                c,
                &[IncomingPeer {
                    id: 6_000,
                    kind: "channel".to_string(),
                    name: "Deep Backlog".to_string(),
                    username: None,
                    about: None,
                    phone: None,
                    status: None,
                    last_seen: None,
                    members: Some(1),
                    online: None,
                    admin: true,
                    is_contact: false,
                    is_self: false,
                }],
            )?;
            project_chats(
                c,
                &[IncomingChat {
                    peer: 6_000,
                    pinned: 0,
                    muted: false,
                    archived: false,
                    in_main: true,
                    unread: 0,
                    mention: false,
                    draft: None,
                    typing: None,
                    last_read: None,
                }],
            )
        })
        .unwrap();
        // Oldest first, so ids and dates rise together. A handful of lines
        // wear an invented marker word — the oldest, the last that will be
        // dropped, the oldest that will be kept, the newest — so the index
        // can be probed at the window's edges without a number's prefix
        // matching its longer cousins.
        let base = ts(2026, 1, 1, 0, 0);
        let text_for = |i: i64| -> String {
            let mark = match i {
                0 => "alpha",      // the oldest of all, to be dropped
                49 => "bravo",     // the last line the trim drops
                50 => "charlie",   // the oldest line the trim keeps
                10_049 => "omega", // the newest of all
                _ => "zebra",
            };
            format!("line {i} {mark}")
        };
        let batch: Vec<IncomingMessage> = (0i64..10_050)
            .map(|i| msg(7_000 + i, 6_000, base + i as f64 * 60.0, &text_for(i)))
            .collect();
        s.write(move |c| project_messages(c, &batch)).unwrap();
        let dropped = s.write(|c| trim_chat(c, 6_000)).unwrap();
        assert_eq!(dropped, 50);
        let kept: i64 = s
            .conn()
            .query_row("SELECT COUNT(*) FROM tg_message WHERE chat = 6000", [], |r| r.get(0))
            .unwrap();
        assert_eq!(kept, 10_000);
        // The newest survived, the oldest went: id 7000..=7049 are the 50
        // dropped, 7050 the oldest kept.
        let (lo, hi): (i64, i64) = s
            .conn()
            .query_row("SELECT MIN(id), MAX(id) FROM tg_message WHERE chat = 6000", [], |r| {
                Ok((r.get(0)?, r.get(1)?))
            })
            .unwrap();
        assert_eq!((lo, hi), (7_050, 17_049));
        // The index followed the delete: the dropped lines are unfindable,
        // the kept ones are found, and the fts row-count matches the window.
        assert!(search_local(s.conn(), Some(6_000), "alpha").is_empty(), "the oldest, trimmed line");
        assert!(search_local(s.conn(), Some(6_000), "bravo").is_empty(), "the last dropped line");
        assert_eq!(search_local(s.conn(), Some(6_000), "charlie").len(), 1, "the oldest kept line");
        assert_eq!(search_local(s.conn(), Some(6_000), "omega").len(), 1, "the newest line");
        let fts_n: i64 = s
            .conn()
            .query_row(
                "SELECT COUNT(*) FROM tg_message_fts f JOIN tg_message m ON m.seq = f.rowid
                 WHERE m.chat = 6000",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(fts_n, 10_000, "the index holds only the window");
    }

    /// Indexed queries return only the requested scope's matches.
    #[test]
    fn local_search_is_scoped() {
        let s = store();
        // A word the demo world carries in two different chats.
        s.write(|c| {
            project_messages(
                c,
                &[
                    msg(9_201, VERA, ts(2026, 9, 2, 9, 0), "the pangolin plan"),
                    msg(9_202, HIKE, ts(2026, 9, 2, 9, 1), "another pangolin"),
                ],
            )
        })
        .unwrap();
        let global = search_local(s.conn(), None, "pangolin");
        assert_eq!(global.len(), 2, "both chats");
        let scoped = search_local(s.conn(), Some(VERA), "pangolin");
        assert_eq!(scoped.len(), 1, "one chat");
        assert_eq!(scoped[0].chat, VERA);
    }

    /// Projecting into a new chat leaves the demo seed's own rows exactly as
    /// they were: same count, same newest line.
    #[test]
    fn projecting_does_not_disturb_the_demo_seed() {
        let s = store();
        let snapshot = |s: &Store| -> (i64, String) {
            s.conn()
                .query_row(
                    "SELECT
                       (SELECT COUNT(*) FROM tg_message),
                       (SELECT text FROM tg_message WHERE chat = ?1 ORDER BY date DESC, id DESC LIMIT 1)",
                    [VERA],
                    |r| Ok((r.get(0)?, r.get(1)?)),
                )
                .unwrap()
        };
        let before = snapshot(&s);
        s.write(|c| {
            project_peers(
                c,
                &[IncomingPeer {
                    id: 8_000,
                    kind: "person".to_string(),
                    name: "Stranger".to_string(),
                    username: None,
                    about: None,
                    phone: None,
                    status: None,
                    last_seen: None,
                    members: None,
                    online: None,
                    admin: false,
                    is_contact: false,
                    is_self: false,
                }],
            )?;
            project_chats(
                c,
                &[IncomingChat {
                    peer: 8_000,
                    pinned: 0,
                    muted: false,
                    archived: false,
                    in_main: true,
                    unread: 0,
                    mention: false,
                    draft: None,
                    typing: None,
                    last_read: None,
                }],
            )?;
            project_messages(c, &[msg(8_500, 8_000, ts(2026, 9, 2, 12, 0), "a line elsewhere")])
        })
        .unwrap();
        let after = snapshot(&s);
        assert_eq!(after.0, before.0 + 1, "only our one line joined the store");
        assert_eq!(after.1, before.1, "the demo chat's newest line is untouched");
    }
}
