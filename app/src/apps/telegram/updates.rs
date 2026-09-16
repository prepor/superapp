//! Pure decoders from TDLib JSON to the projection's `Incoming*` values.
//!
//! Missing optional fields become defaults or `None`; messages without an
//! identity are rejected. Media carries cache keys and remote ids, never
//! bytes. The worker handles downloading and persistence.
#![cfg_attr(not(feature = "tdlib"), allow(dead_code))]


use serde_json::Value;

use super::calls;
use super::model::{self, DownloadProgress, Media, MsgId, MsgKey, PeerId};
use super::project::{
    Forward, IncomingChat, IncomingMember, IncomingMessage, IncomingPeer, IncomingThread,
    IncomingTopic,
};
use super::runtime::Reason;

// -- a message ----------------------------------------------------------------------

/// One TDLib `message` object, flattened to an [`IncomingMessage`]. `None`
/// only when the object carries no id or chat at all — the two fields a row
/// cannot do without; everything else degrades to a default.
///
/// `now` is the projection's own clock, which a live location's expiry is
/// read against: the wire says how much is *left* of a share, and that is
/// the truer end than the message's date plus its period (see
/// [`live_location_media`]).
#[must_use]
pub fn message(m: &Value, now: f64) -> Option<IncomingMessage> {
    let id = m["id"].as_i64()?;
    let chat = m["chat_id"].as_i64()?;
    let date = m["date"].as_f64().unwrap_or(0.0);
    let out = m["is_outgoing"].as_bool().unwrap_or(false);
    let (text, mut media) = content(&m["content"], date, now);
    // A share that has moved has been edited, and the edit's date is when
    // the pin was last put down. One that has not moved yet was last put
    // down when it was sent.
    if let Some(live) = media.as_mut().filter(|m| m.kind == "live") {
        live.updated = Some(m["edit_date"].as_f64().filter(|&d| d > 0.0).unwrap_or(date));
    }
    if let Some(call) = media.as_mut() {
        call_way(call, out);
    }
    let info = &m["interaction_info"];
    Some(IncomingMessage {
        content_type: m["content"]["@type"].as_str().map(str::to_string),
        id,
        chat,
        topic: message_topic(m),
        thread: message_thread(m),
        sender: sender_id(&m["sender_id"], chat),
        date,
        text,
        entities: content_entities(&m["content"]),
        out,
        // A read receipt needs the chat's read cursor, not one message, so a
        // sent line is 'sent' until `updateChatReadOutbox` says
        // otherwise.
        state: out.then(|| send_state(&m["sending_state"]).to_string()),
        edited: m["edit_date"].as_i64().unwrap_or(0) > 0,
        // TDLib 1.8.0 carries `reply_to_message_id`; a later layer moved it
        // under a `reply_to` object. Read the flat field, then the nested.
        reply_to: m["reply_to_message_id"]
            .as_i64()
            .or_else(|| m["reply_to"]["message_id"].as_i64())
            .filter(|&r| r != 0),
        reply_chat: m["reply_to"]["chat_id"].as_i64().filter(|id| *id != 0 && *id != chat),
        unread_mention: !out && m["contains_unread_mention"].as_bool().unwrap_or(false),
        fwd: forward(m),
        media,
        views: info["view_count"].as_i64().filter(|&n| n > 0),
        comments: comment_count(info),
        last_comment: reply_cursor(info, "last_message_id"),
        read_comment: reply_cursor(info, "last_read_inbox_message_id"),
        source_post: source_post(m),
        reactions: reactions_line(info),
        // Upgrade boundaries are service lines, not messages to react to.
        service: matches!(m["content"]["@type"].as_str(), Some("messageChatUpgradeFrom" | "messageChatUpgradeTo")),
    })
}

/// Which way a call went, written in front of the words its content carries.
///
/// *Missed* one way is *cancelled* the other, and only the message knows
/// which way it went — the content does not. Anything that is not a call
/// passes through untouched, so both roads a call's media travels can go
/// through here: a whole [`message`], and an `updateMessageContent` that
/// carries a `messageCall` under a line the projection already knows the
/// direction of.
pub fn call_way(media: &mut Media, out: bool) {
    if media.kind != "call" {
        return;
    }
    let way = if out { "outgoing" } else { "incoming" };
    media.label = Some(format!("{way} {}", media.label.as_deref().unwrap_or("empty")));
}

/// Forum topic ids in the installed TDLib API are carried by MessageTopic.
/// Ordinary chats, channel comments and direct-message topics stay distinct.
pub fn message_topic(m: &Value) -> i64 {
    if m["topic_id"]["@type"].as_str() == Some("messageTopicForum") {
        m["topic_id"]["forum_topic_id"].as_i64().unwrap_or(0)
    } else {
        0
    }
}

/// The thread a line is a comment in, carried by the same `MessageTopic`:
/// the root it answers, which is a message id in the line's own chat — the
/// discussion group. 0 for everything that is not a comment.
#[must_use]
pub fn message_thread(m: &Value) -> MsgId {
    if m["topic_id"]["@type"].as_str() == Some("messageTopicThread") {
        m["topic_id"]["message_thread_id"].as_i64().unwrap_or(0).max(0)
    } else {
        0
    }
}

/// How many comments a line has, where it is a line that can have them.
///
/// `reply_info` rides a post in a channel with a discussion group, and its
/// copy in that group — and nothing else. So its *presence* is what says a
/// line can be commented on at all, and a nought is a real answer: *leave a
/// comment*. A line with no `reply_info` answers `None`: no discussion, no
/// way in, nothing drawn.
#[must_use]
fn comment_count(info: &Value) -> Option<i64> {
    let reply = &info["reply_info"];
    reply.is_object().then(|| reply["reply_count"].as_i64().unwrap_or(0).max(0))
}

/// `messageThreadInfo`: where one post's comments are — the discussion
/// group and the post's own copy in it, the root every comment answers —
/// with how many there are and how far Telegram has seen me read.
///
/// The answer does not name the post it was asked about, so the request's
/// `@extra` carries it (see `requests::get_message_thread`). `None` where
/// the wire named no chat or no thread.
#[must_use]
pub fn thread(chat: PeerId, post: MsgId, asked_at: f64, v: &Value) -> Option<IncomingThread> {
    let group = v["chat_id"].as_i64().filter(|&id| id != 0)?;
    let root = v["message_thread_id"].as_i64().filter(|&id| id > 0)?;
    Some(IncomingThread {
        chat,
        post,
        group: Some(group),
        root: Some(root),
        count: v["reply_info"]["reply_count"].as_i64().map(|n| n.max(0)),
        last: reply_cursor(v, "last_message_id"),
        last_read: reply_cursor(v, "last_read_inbox_message_id"),
        // The thread's own draft, as another device left it. An answer
        // always says whether there is one, so a null is a draft cleared —
        // and a clear has no date of its own, so it is dated by the ask it
        // answers: whatever was true then, anything typed here since is
        // newer and stands.
        has_draft: true,
        draft: nonempty(v["draft_message"]["input_message_text"]["text"]["text"].as_str()),
        draft_date: Some(
            v["draft_message"]["date"].as_f64().filter(|d| *d > 0.0).unwrap_or(asked_at),
        ),
    })
}

/// Which message a line was forwarded from *last* — the way back from a
/// post's copy in a discussion group to the post it hangs from.
///
/// `forward_info.source` and nothing else. The wire fills it in for exactly
/// three kinds of forward — to Saved Messages, to the Replies bot chat, and
/// **to a channel's discussion group** — and leaves it null for every other,
/// which is what makes it the one field that means *this is that post's own
/// copy*. The `origin` beside it is the line's first author, which is
/// neither necessary (a post whose content came from a person still has a
/// copy in the group) nor sufficient (a person forwarding a channel's post
/// by hand carries the same origin and no source at all).
#[must_use]
pub fn source_post(m: &Value) -> Option<MsgKey> {
    let source = &m["forward_info"]["source"];
    let chat = source["chat_id"].as_i64().filter(|&id| id != 0)?;
    let post = source["message_id"].as_i64().filter(|&id| id > 0)?;
    Some((chat, post))
}

/// A full forumTopic, an updateForumTopic, or a bare forumTopicInfo.
pub fn topic(chat: PeerId, v: &Value) -> Option<IncomingTopic> {
    let info = v.get("info").unwrap_or(v);
    let id = info["forum_topic_id"].as_i64()?;
    if id <= 0 { return None; }
    let settings = &v["notification_settings"];
    Some(IncomingTopic {
        chat,
        id,
        name: info["name"].as_str().map(str::to_string),
        closed: info["is_closed"].as_bool(),
        hidden: info["is_hidden"].as_bool(),
        unread: v["unread_count"].as_i64(),
        mention: v["unread_mention_count"].as_i64().map(|n| n.max(0)),
        muted: settings["mute_for"].as_i64().map(|n| n > 0),
        mute_default: settings["use_default_mute_for"].as_bool(),
        last_read: v["last_read_inbox_message_id"].as_i64(),
        read_outbox: v["last_read_outbox_message_id"].as_i64(),
        has_draft: v.get("draft_message").is_some(),
        draft: nonempty(v["draft_message"]["input_message_text"]["text"]["text"].as_str()),
    })
}

/// A `MessageSender`: a user posts as themselves, a chat posts as itself. A
/// channel's own post and an anonymous admin come as the chat that owns the
/// message — the model draws those senderless, so they map to `None`.
fn sender_id(sender: &Value, chat: PeerId) -> Option<PeerId> {
    match sender["@type"].as_str() {
        Some("messageSenderUser") => sender["user_id"].as_i64(),
        Some("messageSenderChat") => sender["chat_id"].as_i64().filter(|&c| c != chat),
        _ => None,
    }
}

/// A sending line's state word. A pending or failed send says so; anything
/// already on the server is 'sent'.
fn send_state(st: &Value) -> &'static str {
    match st["@type"].as_str() {
        Some("messageSendingStatePending") => "sending",
        Some("messageSendingStateFailed") => "failed",
        _ => "sent",
    }
}

/// Where a forwarded line came from.
///
/// Three of the four origins name a peer — a person, a group posting as
/// itself, a channel — and one, a sender who hid themselves, carries a bare
/// name. So the peer is what is kept, and the name is read off `tg_peer` at
/// draw time the way a sender's is: a forward from a channel that renames
/// itself reads by its new title, and no name is duplicated onto every line
/// forwarded from it.
///
/// A channel origin also names the post itself, which is what *came from*
/// opens; a chat or channel origin may carry the signature of whoever wrote
/// it, which the client prints after the title.
///
/// TDLib renamed these constructors from `messageForwardOrigin*` to
/// `messageOrigin*` and moved an imported message's sender to the message's
/// own `import_info` (the installed build has only the new spelling, which is
/// why every forward here read unattributed). Both spellings are accepted:
/// the field names inside them did not change.
fn forward(m: &Value) -> Forward {
    let origin = &m["forward_info"]["origin"];
    let kind = origin["@type"].as_str().unwrap_or("");
    let kind = kind.strip_prefix("messageForwardOrigin").or_else(|| kind.strip_prefix("messageOrigin"));
    let sign = || nonempty(origin["author_signature"].as_str());
    match kind {
        Some("User") => Forward { peer: origin["sender_user_id"].as_i64(), ..Forward::default() },
        Some("Chat") => Forward {
            peer: origin["sender_chat_id"].as_i64(),
            sign: sign(),
            ..Forward::default()
        },
        Some("Channel") => Forward {
            peer: origin["chat_id"].as_i64(),
            msg: origin["message_id"].as_i64().filter(|&id| id != 0),
            sign: sign(),
            ..Forward::default()
        },
        // `MessageImport` was the old spelling's fourth origin; the new one
        // carries an import beside `forward_info` instead.
        Some("HiddenUser" | "MessageImport") => Forward {
            name: nonempty(origin["sender_name"].as_str()),
            ..Forward::default()
        },
        _ => Forward {
            name: nonempty(m["import_info"]["sender_name"].as_str()),
            ..Forward::default()
        },
    }
}

/// A message's reactions as one line — `👍 3 · ❤️ 1` — the way a channel post
/// draws them. `None` when there are none.
fn reactions_line(info: &Value) -> Option<String> {
    // Current TDLib wraps the list in a `messageReactions` object; the
    // older layer carried the bare array. Read either (review, 2026-09-07).
    let arr = info["reactions"]
        .as_array()
        .or_else(|| info["reactions"]["reactions"].as_array())?;
    let parts: Vec<String> = arr
        .iter()
        .filter_map(|r| {
            // TDLib 1.8.0 carried the emoji as a bare `reaction` string; a
            // later layer wrapped it in a `type` object. Read either.
            let emoji = r["reaction"]
                .as_str()
                .or_else(|| r["type"]["emoji"].as_str())
                .or_else(|| match r["type"]["@type"].as_str() {
                    Some("reactionTypePaid") => Some("⭐"),
                    // Custom artwork has no Unicode representation. Keep
                    // its count visible until a sticker renderer is used.
                    Some("reactionTypeCustomEmoji") => Some("custom emoji"),
                    _ => None,
                })?;
            let n = r["total_count"].as_i64().unwrap_or(0);
            Some(format!("{emoji} {n}"))
        })
        .collect();
    (!parts.is_empty()).then(|| parts.join(" · "))
}

/// Missing interaction metadata is unknown. An actual list (even an empty
/// one) is evidence; unsupported or malformed entries must not become zero.
pub(super) fn reaction_counts(info: &Value) -> Option<Option<String>> {
    let list = info["reactions"].as_array().or_else(|| info["reactions"]["reactions"].as_array())?;
    if list.iter().any(|r| {
        let named = r["reaction"].as_str().is_some_and(|s| !s.is_empty()) || match r["type"]["@type"].as_str() {
            Some("reactionTypeEmoji") => r["type"]["emoji"].as_str().is_some_and(|s| !s.is_empty()),
            Some("reactionTypeCustomEmoji" | "reactionTypePaid") => true,
            _ => false,
        };
        !named || r["total_count"].as_i64().is_none_or(|n| n < 0)
    }) { return None; }
    Some(reactions_line(info))
}

/// What a line has gathered since it was posted: the views, the comments
/// under it, and its reactions as the one line a post draws.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Interaction {
    pub chat: PeerId,
    pub id: MsgId,
    pub views: Option<i64>,
    pub comments: Option<i64>,
    /// The newest comment, and the newest one Telegram has seen me read —
    /// what tells a post's foot there is something new under it.
    pub last_comment: Option<MsgId>,
    pub read_comment: Option<MsgId>,
    pub reactions: Option<String>,
}

/// Ordinary emoji available to this user, preserving Telegram's ordering.
/// Custom emoji need their own renderer; paid reactions use a separate API.
#[must_use]
pub fn available_reactions(v: &Value) -> Vec<String> {
    if !v["unavailability_reason"].is_null() {
        return Vec::new();
    }
    let mut emojis = Vec::new();
    for key in ["top_reactions", "recent_reactions", "popular_reactions"] {
        for reaction in v[key].as_array().into_iter().flatten() {
            if reaction["needs_premium"].as_bool() == Some(true)
                || reaction["type"]["@type"].as_str() != Some("reactionTypeEmoji")
            {
                continue;
            }
            if let Some(emoji) = reaction["type"]["emoji"].as_str().filter(|e| !e.is_empty()) {
                if !emojis.iter().any(|e| e == emoji) {
                    emojis.push(emoji.to_string());
                }
            }
        }
    }
    emojis
}

/// `updateMessageInteractionInfo`: a channel post counted again, a reaction
/// added or taken back. TDLib also emits null while reaction metadata is
/// invalidated; the worker reconciles that ambiguity before clearing counts.
/// `None` only when the update names no chat or no message.
#[must_use]
pub fn interaction(u: &Value) -> Option<Interaction> {
    let info = &u["interaction_info"];
    Some(Interaction {
        chat: u["chat_id"].as_i64()?,
        id: u["message_id"].as_i64()?,
        views: info["view_count"].as_i64().filter(|&n| n > 0),
        comments: comment_count(info),
        last_comment: reply_cursor(info, "last_message_id"),
        read_comment: reply_cursor(info, "last_read_inbox_message_id"),
        reactions: reactions_line(info),
    })
}

/// One of `messageReplyInfo`'s cursors, where the line carries a reply info
/// at all. A nought is *none yet*, which is nothing to write down.
#[must_use]
fn reply_cursor(info: &Value, field: &str) -> Option<MsgId> {
    info["reply_info"][field].as_i64().filter(|&id| id > 0)
}

// -- a message's content ------------------------------------------------------------
//
// The content @types this build knows, and the media kind each becomes — the
// nine kinds `model::Media` and the widgets already draw. An unknown kind is
// not dropped: it becomes a plain line naming itself, so a new content type
// TDLib grows degrades to a word rather than a hole.
//
//   messageText       → (no media, the text is the line)
//   messagePhoto      → photo     · sizes[largest].photo         (fetched on arrival)
//   messageVideo      → video     · video.thumbnail.file         (the poster, fetched on arrival;
//   messageAnimation  → video     · animation.thumbnail.file      the clip beside it, on opening)
//   messageVideoNote  → circle    · video_note.thumbnail.file
//   messageVoiceNote  → voice     · voice_note.voice             (fetched on open, later)
//   messageAudio      → audio     · audio.audio
//   messageSticker    → sticker   · sticker.sticker, labelled by its emoji
//   messageDocument   → file      · document.document, labelled by name
//   messageLocation   → location  · a place, sent once
//   messageLiveLocation → live    · a place that keeps moving, until its period ends
//   (anything else)   → (no media, the @type minus its 'message' prefix)

/// A content object as a line of text and, where it carries any, a [`Media`].
/// `date` and `now` seed a live location's expiry — the message's own date,
/// and the clock what is *left* of a share is counted from; both are ignored
/// by every other kind.
///
/// A file the media names is named twice: `reference`, the blob-cache key —
/// the remote *unique* id — and `rid`, the remote id it can be asked for by
/// ([`file_rid`]). The first says where the bytes go, the second how to get
/// them back once the cache has let them go, or when they were never fetched:
/// a photo past the first forty lines of a chat could otherwise never be
/// downloaded at all (review, 2026-09-07).
#[must_use]
pub fn content(content: &Value, date: f64, now: f64) -> (String, Option<Media>) {
    let caption = || formatted_text(&content["caption"]);
    match content["@type"].as_str() {
        Some("messageChatUpgradeFrom" | "messageChatUpgradeTo") => ("group upgraded".into(), None),
        Some("messageText") => (formatted_text(&content["text"]), None),
        Some("messagePhoto") => (caption(), photo_media(&content["photo"])),
        // A moving picture's reference is its thumbnail, the poster the
        // transcript draws: small, fetched on arrival. The clip is named
        // beside it and left alone — it is never fetched on arrival (Andrey,
        // 2026-09-06), only when the viewer is opened on the line and the
        // player asks; see [`with_clip`].
        Some("messageVideo") => {
            let v = &content["video"];
            let m = av_media("video", &v["thumbnail"]["file"], v);
            (caption(), Some(with_clip(m, &v["video"])))
        }
        Some("messageAnimation") => {
            let a = &content["animation"];
            let m = av_media("video", &a["thumbnail"]["file"], a);
            (caption(), Some(with_clip(m, &a["animation"])))
        }
        Some("messageVideoNote") => {
            let n = &content["video_note"];
            let m = av_media("circle", &n["thumbnail"]["file"], n);
            (String::new(), Some(with_clip(m, &n["video"])))
        }
        Some("messageVoiceNote") => {
            let n = &content["voice_note"];
            (caption(), Some(av_media("voice", &n["voice"], n)))
        }
        Some("messageAudio") => {
            let a = &content["audio"];
            let mut m = av_media("audio", &a["audio"], a);
            m.label = audio_label(a);
            (caption(), Some(m))
        }
        Some("messageSticker") => {
            let s = &content["sticker"];
            let mut m = Media::of("sticker");
            m.reference = file_ref(&s["sticker"]);
            m.rid = file_rid(&s["sticker"]);
            m.w = s["width"].as_i64();
            m.h = s["height"].as_i64();
            m.label = nonempty(s["emoji"].as_str());
            (String::new(), Some(m))
        }
        Some("messageDocument") => {
            let d = &content["document"];
            let mut m = Media::of("file");
            m.reference = file_ref(&d["document"]);
            m.rid = file_rid(&d["document"]);
            m.label = nonempty(d["file_name"].as_str());
            (caption(), Some(m))
        }
        Some("messageLocation") => (String::new(), Some(location_media(content, date))),
        Some("messageLiveLocation") => (String::new(), Some(live_location_media(content, date, now))),
        // A call that happened. Its label is the few words a call line is
        // made of — *video*, and how it ended. Which way it went is the
        // message's and not the content's, so [`call_way`] writes that one in
        // front; the row keeps them all in the one column it has for a
        // media's label ([`Media::call`](super::model::Media::call)).
        Some("messageCall") => {
            let mut m = Media::of("call");
            let video = content["is_video"].as_bool().unwrap_or(false);
            let reason = discard_reason(&content["discard_reason"]).map_or("empty", Reason::word);
            m.label = Some(if video { format!("video {reason}") } else { reason.to_string() });
            m.secs = content["duration"].as_i64().filter(|&d| d > 0);
            (String::new(), Some(m))
        }
        Some(other) => (
            other.strip_prefix("message").unwrap_or(other).to_string(),
            None,
        ),
        None => (String::new(), None),
    }
}

/// A `formattedText`'s plain string — the `text` inside it — or empty.
fn formatted_text(ft: &Value) -> String {
    ft["text"].as_str().unwrap_or_default().to_string()
}

/// Message text and media captions both carry the same formatted-text shape.
pub fn content_entities(content: &Value) -> Vec<super::text::Entity> {
    let field = if content["@type"].as_str() == Some("messageText") { "text" } else { "caption" };
    super::text::entities(&content[field]["entities"])
}

/// The largest of a photo's sizes: the biggest file the server offers, the
/// one the client both draws and downloads. Shared by [`photo_media`] and
/// [`download_id`] so the size the row points at is the size fetched.
fn photo_best(photo: &Value) -> Option<&Value> {
    photo["sizes"].as_array()?.iter().max_by_key(|s| {
        s["width"]
            .as_i64()
            .unwrap_or(0)
            .saturating_mul(s["height"].as_i64().unwrap_or(0))
    })
}

/// The largest of a photo's sizes as a [`Media`]: the biggest file the server
/// offers, with its pixels so the box has a height before the bytes land, and
/// the id it can be asked for by afterwards ([`file_rid`]).
fn photo_media(photo: &Value) -> Option<Media> {
    let best = photo_best(photo)?;
    let mut m = Media::of("photo");
    m.reference = file_ref(&best["photo"]);
    m.rid = file_rid(&best["photo"]);
    m.w = best["width"].as_i64();
    m.h = best["height"].as_i64();
    Some(m)
}

/// A video, video note, voice note or animation as a [`Media`] of the given
/// `kind`: the file to fetch — for a moving picture, its poster — the
/// dimensions where the object has them, the recording's length, and the
/// durable id that file is asked for by.
fn av_media(kind: &str, file: &Value, dims: &Value) -> Media {
    Media {
        kind: kind.to_string(),
        reference: file_ref(file),
        rid: file_rid(file),
        w: dims["width"].as_i64(),
        h: dims["height"].as_i64(),
        secs: dims["duration"].as_i64(),
        ..Media::default()
    }
}

/// The clip hung on a moving picture's [`Media`], beside the poster its
/// `reference` already names: where the bytes will be once they are here,
/// and what they are asked for by.
///
/// Two ids, because they answer two questions. The blob-cache key is the
/// remote *unique* id, the same shape every other file has, so a clip that
/// lands resolves through the one path media already takes. The remote id is
/// what a `getRemoteFile` takes — it outlives a session, where the `file.id`
/// a download runs on is only this run's and would be a stale number by
/// morning. Neither is fetched here: naming a file is not asking for it.
fn with_clip(mut m: Media, file: &Value) -> Media {
    m.clip = file_ref(file);
    m.clip_rid = file_rid(file);
    m
}

/// An audio track's label: its title, else its performer, else its file name.
fn audio_label(audio: &Value) -> Option<String> {
    nonempty(audio["title"].as_str())
        .or_else(|| nonempty(audio["performer"].as_str()))
        .or_else(|| nonempty(audio["file_name"].as_str()))
}

/// A location, or a live one while its `live_period` still runs — the moving
/// kind carries when the sharing ends.
///
/// The installed TDLib tells the two apart by the content's own type
/// (`messageLiveLocation`), but earlier layers — and a fixture written
/// against them — put the period on `messageLocation` itself; both are read,
/// so a line from either wire draws the same.
fn location_media(content: &Value, date: f64) -> Media {
    let loc = &content["location"];
    let mut m = Media::of("location");
    m.lat = loc["latitude"].as_f64();
    m.lon = loc["longitude"].as_f64();
    let live = content["live_period"].as_i64().unwrap_or(0);
    if live > 0 {
        m.kind = "live".to_string();
        m.until = Some(model::live_end(date + live as f64, live));
    }
    m
}

/// A place that keeps moving: `messageLiveLocation`, whose `location` is a
/// `liveLocation` — the point, the period, the heading — with what is left
/// of the period beside it.
///
/// The end is what the wire says is *left* of the share, counted off `now`,
/// and the message's own date plus the period where the wire says nothing:
/// the two are the same moment read from opposite ends, and the wire's own
/// countdown is the better of them, since a client whose clock is a minute
/// out from ours would have a date we cannot add to. Both readings go
/// through [`model::live_end`], which is where the phone's five-second grace
/// is.
fn live_location_media(content: &Value, date: f64, now: f64) -> Media {
    let live = &content["location"];
    let period = live["live_period"].as_i64().unwrap_or(0);
    let mut m = Media::of("live");
    m.lat = live["location"]["latitude"].as_f64();
    m.lon = live["location"]["longitude"].as_f64();
    m.until = Some(
        live_expiry(content, now).unwrap_or_else(|| model::live_end(date + period as f64, period)),
    );
    m
}

/// When a live location's sharing ends, read from what the wire says is
/// *left* of it rather than from a date — the only reading an edit gives,
/// since `updateMessageContent` carries the content and no date, and the
/// truer of the two where a fetched message gives both.
/// `None` where the content is not a live location, or where its period has
/// run out (`expires_in` nought, which is the wire's word for *ended*).
#[must_use]
pub fn live_expiry(content: &Value, now: f64) -> Option<f64> {
    (content["@type"].as_str() == Some("messageLiveLocation"))
        .then(|| content["expires_in"].as_i64().unwrap_or(0))
        .filter(|&left| left > 0)
        .map(|left| {
            let period = content["location"]["live_period"].as_i64().unwrap_or(0);
            model::live_end(now + left as f64, period)
        })
}

/// A live share of mine, as one of my own messages tells it: the chat, the
/// line, and when the sharing ends.
///
/// This is how the worker learns a share — from the echo of its own send,
/// and from the list TDLib pushes on sign-in. Someone else's live location
/// is not a share of mine to keep moving, so an incoming one answers `None`.
#[must_use]
pub fn live_share(message: &Value) -> Option<(PeerId, MsgId, f64)> {
    if !message["is_outgoing"].as_bool().unwrap_or(false) {
        return None;
    }
    let content = &message["content"];
    if content["@type"].as_str() != Some("messageLiveLocation") {
        return None;
    }
    let period = content["location"]["live_period"].as_i64().unwrap_or(0);
    if period <= 0 {
        return None;
    }
    let date = message["date"].as_f64().unwrap_or(0.0);
    Some((
        message["chat_id"].as_i64()?,
        message["id"].as_i64()?,
        model::live_end(date + period as f64, period),
    ))
}

/// Where a share's pin stands and when it was last put down, as the message
/// itself tells it: the point the wire echoed back, and the edit's date — or
/// the send's, where nothing has edited it yet.
///
/// This is the baseline the worker measures the phone's rule from, so the fix
/// a `sendMessage` has only just carried is not sent again as an
/// `editMessageLiveLocation` on the next pass. The accuracy and the heading
/// are not in it: what the rule asks of a baseline is where it was and when,
/// and the wire keeps neither of the other two on a message.
#[must_use]
pub fn live_pin(message: &Value) -> Option<(kernel::caps::Fix, f64)> {
    let point = &message["content"]["location"]["location"];
    let (lat, lon) = (point["latitude"].as_f64()?, point["longitude"].as_f64()?);
    let date = message["date"].as_f64().unwrap_or(0.0);
    let at = message["edit_date"].as_f64().filter(|&d| d > 0.0).unwrap_or(date);
    Some((kernel::caps::Fix::at(lat, lon, at), at))
}

/// The blob-cache key a file resolves through: `tg:<remote unique id>`. `None`
/// when the file has no remote yet — a purely local one not on the server.
pub fn file_ref(file: &Value) -> Option<String> {
    nonempty(file["remote"]["unique_id"].as_str()).map(|uid| format!("tg:{uid}"))
}

/// Downloaded bytes are the total received, not the contiguous prefix used
/// for streaming. Prefer the exact size; zero means TDLib does not know it.
pub fn download_progress(file: &Value) -> DownloadProgress {
    let size = file["size"].as_u64().filter(|&n| n > 0);
    let expected = file["expected_size"].as_u64().filter(|&n| n > 0);
    DownloadProgress {
        downloaded: file["local"]["downloaded_size"].as_u64().unwrap_or(0),
        total: size.or(expected),
        estimated: size.is_none() && expected.is_some(),
    }
}

/// The id a file is taken back by across sessions: `remoteFile.id`, which a
/// `getRemoteFile` turns into this run's file again. `None` when the file has
/// no remote yet.
fn file_rid(file: &Value) -> Option<String> {
    nonempty(file["remote"]["id"].as_str())
}

/// The session-local id TDLib downloads a file by — the counterpart to the
/// remote unique id [`file_ref`] keys the cache with. `None` where the object
/// carries none.
fn file_id(file: &Value) -> Option<i32> {
    file["id"].as_i64().and_then(|n| i32::try_from(n).ok())
}

/// The file a message's content carries to fetch, or `None` where it carries
/// nothing downloadable — a text or a location. Only what the transcript
/// draws on arrival is fetched on arrival: a photo's largest size, and a
/// moving picture's thumbnail for its poster. The video file, a voice note, a
/// track, a sticker, a document are the player's or the opener's to ask for,
/// so a chat full of clips does not pull every clip (Andrey, 2026-09-06: no
/// auto-downloading videos). The same file [`content`] maps to the row's
/// [`Media`]; [`download_id`] and [`download_key`] read it, so the id fetched
/// and the key looked up name one file.
fn download_target(content: &Value) -> Option<&Value> {
    Some(match content["@type"].as_str()? {
        "messagePhoto" => &photo_best(&content["photo"])?["photo"],
        "messageVideo" => &content["video"]["thumbnail"]["file"],
        "messageAnimation" => &content["animation"]["thumbnail"]["file"],
        "messageVideoNote" => &content["video_note"]["thumbnail"]["file"],
        _ => return None,
    })
}

/// The viewer's full clip or picture, including videos without thumbnails.
pub fn viewer_file(content: &Value, clip: bool) -> Option<&Value> {
    let file = if clip {
        match content["@type"].as_str()? {
            "messageVideo" => &content["video"]["video"],
            "messageAnimation" => &content["animation"]["animation"],
            "messageVideoNote" => &content["video_note"]["video"],
            _ => return None,
        }
    } else {
        download_target(content)?
    };
    file_id(file).filter(|id| *id > 0)?;
    Some(file)
}

/// The full attachment, including documents and sounds that the transcript
/// never downloads automatically. A video's thumbnail is not its attachment.
pub fn attachment_file(content: &Value) -> Option<&Value> {
    let file = match content["@type"].as_str()? {
        "messageDocument" => &content["document"]["document"],
        "messageAudio" => &content["audio"]["audio"],
        "messageVoiceNote" => &content["voice_note"]["voice"],
        "messagePhoto" => return viewer_file(content, false),
        _ => return viewer_file(content, true),
    };
    file_id(file).filter(|id| *id > 0)?;
    Some(file)
}

pub fn attachment_name(content: &Value) -> Option<&str> {
    let field = match content["@type"].as_str()? {
        "messageDocument" => "document",
        "messageAudio" => "audio",
        "messageVideo" => "video",
        "messageAnimation" => "animation",
        _ => return None,
    };
    content[field]["file_name"].as_str().filter(|name| !name.is_empty())
}

/// The session-local id the worker fires `downloadFile` on, for the file
/// [`download_target`] picks. The finished file lands in the blob cache under
/// the same `tg:` key the row already names, and the next redraw resolves it.
#[must_use]
pub fn download_id(content: &Value) -> Option<i32> {
    file_id(download_target(content)?)
}

/// The blob-cache key that same file resolves through — what the worker looks
/// up before it asks: a file the cache already holds is not fetched again,
/// the cache being the one store of media and the engine's copy long moved
/// into it.
#[must_use]
pub fn download_key(content: &Value) -> Option<String> {
    file_ref(download_target(content)?)
}

// -- a call -------------------------------------------------------------------------

/// One `updateCall`, flattened: who it is with, which way it went, and where
/// it stands.
#[derive(Debug, Clone, PartialEq)]
pub struct IncomingCall {
    pub id: i32,
    pub user: PeerId,
    pub outgoing: bool,
    pub video: bool,
    pub state: CallWire,
}

/// The wire's `CallState`, decoded. Every shape the installed TDLib names,
/// and nothing between them.
#[derive(Debug, Clone, PartialEq)]
pub enum CallWire {
    /// `is_created` says the server knows of it; `is_received` that the other
    /// side's client has it and is ringing.
    Pending { created: bool, received: bool },
    ExchangingKeys,
    /// The key, the servers and the four emoji: everything the engine needs
    /// and the one thing the panel shows.
    Ready { ready: Box<calls::Ready>, emoji: Vec<String> },
    HangingUp,
    Discarded { reason: Option<Reason>, need_rating: bool },
    Error(String),
}

/// Reads an `updateCall`'s `call` object. `None` for anything else, or for a
/// state this build does not know.
#[must_use]
pub fn call(v: &Value) -> Option<IncomingCall> {
    let c = if v["@type"] == "updateCall" { &v["call"] } else { v };
    let id = i32::try_from(c["id"].as_i64()?).ok()?;
    let user = c["user_id"].as_i64()?;
    let outgoing = c["is_outgoing"].as_bool().unwrap_or(false);
    let video = c["is_video"].as_bool().unwrap_or(false);
    let st = &c["state"];
    let state = match st["@type"].as_str()? {
        "callStatePending" => CallWire::Pending {
            created: st["is_created"].as_bool().unwrap_or(false),
            received: st["is_received"].as_bool().unwrap_or(false),
        },
        "callStateExchangingKeys" => CallWire::ExchangingKeys,
        "callStateReady" => CallWire::Ready {
            ready: Box::new(calls::Ready {
                user,
                outgoing,
                video,
                // The wire knows of neither. What a call is actually
                // started with is the row's mute and camera and what the
                // platform said about the microphone, and the worker
                // writes all three over these as it begins the call.
                muted: false,
                microphone: true,
                key: bytes(&st["encryption_key"]),
                servers: st["servers"].as_array().map(|s| s.iter().map(server).collect()).unwrap_or_default(),
                versions: strings(&st["protocol"]["library_versions"]),
                allow_p2p: st["allow_p2p"].as_bool().unwrap_or(false),
                custom_parameters: nonempty(st["custom_parameters"].as_str()),
            }),
            emoji: strings(&st["emojis"]),
        },
        "callStateHangingUp" => CallWire::HangingUp,
        "callStateDiscarded" => CallWire::Discarded {
            reason: discard_reason(&st["reason"]),
            need_rating: st["need_rating"].as_bool().unwrap_or(false),
        },
        "callStateError" => CallWire::Error(
            nonempty(st["error"]["message"].as_str()).unwrap_or_else(|| "the call failed".to_string()),
        ),
        _ => return None,
    };
    Some(IncomingCall { id, user, outgoing, video, state })
}

/// One `callServer`: a Telegram reflector carries a peer tag and says whether
/// it wants TCP; a WebRTC one carries the credentials and says which of STUN
/// and TURN it serves.
fn server(v: &Value) -> calls::Server {
    let kind = &v["type"];
    let webrtc = kind["@type"] == "callServerTypeWebrtc";
    calls::Server {
        id: v["id"].as_u64().unwrap_or(0),
        ipv4: v["ip_address"].as_str().unwrap_or_default().to_string(),
        ipv6: v["ipv6_address"].as_str().unwrap_or_default().to_string(),
        port: u16::try_from(v["port"].as_i64().unwrap_or(0)).unwrap_or(0),
        username: kind["username"].as_str().unwrap_or_default().to_string(),
        password: kind["password"].as_str().unwrap_or_default().to_string(),
        turn: webrtc && kind["supports_turn"].as_bool().unwrap_or(false),
        stun: webrtc && kind["supports_stun"].as_bool().unwrap_or(false),
        tcp: !webrtc && kind["is_tcp"].as_bool().unwrap_or(false),
        peer_tag: if webrtc { Vec::new() } else { bytes(&kind["peer_tag"]) },
    }
}

/// The `call_id` of an `updateNewCallSignalingData`, and the packet itself.
#[must_use]
pub fn call_signalling(v: &Value) -> Option<(i32, Vec<u8>)> {
    Some((i32::try_from(v["call_id"].as_i64()?).ok()?, bytes(&v["data"])))
}

/// A `callDiscardReason`, in a word. `None` where the wire named none.
fn discard_reason(v: &Value) -> Option<Reason> {
    Some(match v["@type"].as_str()? {
        "callDiscardReasonEmpty" => Reason::Empty,
        "callDiscardReasonMissed" => Reason::Missed,
        "callDiscardReasonDeclined" => Reason::Declined,
        "callDiscardReasonDisconnected" => Reason::Disconnected,
        "callDiscardReasonHungUp" => Reason::HungUp,
        "callDiscardReasonUpgradeToGroupCall" => Reason::UpgradeToGroupCall,
        _ => return None,
    })
}

/// The wire's `bytes`, which its JSON interface spells as base64.
fn bytes(v: &Value) -> Vec<u8> {
    use base64::Engine as _;
    v.as_str()
        .and_then(|s| base64::engine::general_purpose::STANDARD.decode(s).ok())
        .unwrap_or_default()
}

/// An array of strings, dropping anything that is not one.
fn strings(v: &Value) -> Vec<String> {
    v.as_array()
        .map(|a| a.iter().filter_map(|s| s.as_str().map(str::to_string)).collect())
        .unwrap_or_default()
}

// -- a peer -------------------------------------------------------------------------

/// One TDLib `user` object as an [`IncomingPeer`]. `None` only when it carries
/// no id. A user is always a person; a group or a channel becomes a peer
/// through [`chat_peer`] instead, since only the chat carries its title.
#[must_use]
pub fn peer(user: &Value) -> Option<IncomingPeer> {
    let id = user["id"].as_i64()?;
    let (status, last_seen) = user_status(&user["status"]);
    Some(IncomingPeer {
        id,
        kind: "person".to_string(),
        name: full_name(user),
        username: username(user),
        about: None,
        phone: nonempty(user["phone_number"].as_str()),
        status,
        last_seen,
        members: None,
        online: None,
        admin: false,
        is_contact: user["is_contact"].as_bool().unwrap_or(false),
        // Self is the account holder, known from `my_id`, not from a user
        // object; the worker settles the saved-messages peer from my_id.
        is_self: false,
    })
}

/// Both chat objects and userFullInfo carry this nullable block list.
/// A stories-only block does not prevent messages.
pub fn blocked(value: &Value) -> bool {
    value["block_list"]["@type"].as_str() == Some("blockListMain")
}

/// A user's display name: first and last joined, falling back to the username
/// where a bot or a deleted account has no name at all.
fn full_name(user: &Value) -> String {
    let first = user["first_name"].as_str().unwrap_or_default().trim();
    let last = user["last_name"].as_str().unwrap_or_default().trim();
    let joined = format!("{first} {last}").trim().to_string();
    if joined.is_empty() {
        username(user).unwrap_or_default()
    } else {
        joined
    }
}

/// A user's @-handle without the @. TDLib 1.8.0 carried a single `username`;
/// a later layer moved to a `usernames` object with an active list. Read the
/// old field, then the first active of the new.
fn username(user: &Value) -> Option<String> {
    nonempty(user["username"].as_str()).or_else(|| {
        user["usernames"]["active_usernames"]
            .as_array()
            .and_then(|a| a.first())
            .and_then(|u| nonempty(u.as_str()))
    })
}

/// A user's presence, as the model's coarse word and, where the server shows
/// it, the moment last seen. TDLib's exact-offline case gives a timestamp but
/// no bucket [`presence`](super::model::presence) has a phrase for, so it
/// says `offline`, which that catch-all draws as *last seen hidden* — a word
/// rather than an absence, because the peer upsert keeps what it is not told
/// (2026-09-07) and a person who went away would otherwise read *online*
/// forever, and stay in the people list's `@online`.
fn user_status(st: &Value) -> (Option<String>, Option<f64>) {
    match st["@type"].as_str() {
        Some("userStatusOnline") => (Some("online".to_string()), st["expires"].as_f64()),
        Some("userStatusRecently") => (Some("recently".to_string()), None),
        Some("userStatusLastWeek") => (Some("week".to_string()), None),
        Some("userStatusLastMonth") => (Some("month".to_string()), None),
        Some("userStatusOffline") => (Some("offline".to_string()), st["was_online"].as_f64()),
        _ => (None, None),
    }
}

/// `updateUserStatus`: a person came online, or went away. The same status a
/// `user` object carries, arriving on its own — which is the only way a
/// header's *online* and the people list's `@online` move between the rare
/// refreshes of the user itself.
///
/// `None` where the update names no user, or wears a status this build has
/// no word for: a value once known stays known (2026-09-07), and an unknown
/// bucket must not null the last one the store was told.
#[must_use]
pub fn user_presence(u: &Value) -> Option<(PeerId, String, Option<f64>)> {
    let id = u["user_id"].as_i64()?;
    let (status, last_seen) = user_status(&u["status"]);
    Some((id, status?, last_seen))
}

// -- a chat -------------------------------------------------------------------------

/// One TDLib `chat` object as an [`IncomingChat`]: its place in the list, its
/// unread count, its mute and its draft. `None` only when it carries no id.
/// Typing is not a chat fact but an `updateChatAction`, so it stays blank
/// here.
#[must_use]
pub fn chat(chat: &Value) -> Option<IncomingChat> {
    let peer = chat["id"].as_i64()?;
    let (pinned, archived, in_main) = positions(chat);
    Some(IncomingChat {
        peer,
        pinned,
        in_main,
        muted: chat["notification_settings"]["mute_for"]
            .as_i64()
            .unwrap_or(0)
            > 0,
        archived,
        unread: chat["unread_count"].as_i64().unwrap_or(0),
        unread_mentions: chat["unread_mention_count"].as_i64().unwrap_or(0).max(0),
        draft: nonempty(chat["draft_message"]["input_message_text"]["text"]["text"].as_str()),
        typing: None,
        last_read: chat["last_read_inbox_message_id"]
            .as_i64()
            .filter(|&m| m != 0),
    })
}

/// The peer a chat implies where the chat itself is the only source of it — a
/// group or a channel, named by its title. A private chat's peer is the user,
/// projected from `updateUser`; deriving one here would clobber the user's own
/// fields, so it answers `None` and the caller leans on the user update.
#[must_use]
pub fn chat_peer(chat: &Value) -> Option<IncomingPeer> {
    let id = chat["id"].as_i64()?;
    let ty = &chat["type"];
    let kind = match ty["@type"].as_str() {
        Some("chatTypeBasicGroup") => "group",
        Some("chatTypeSupergroup") if ty["is_channel"].as_bool() == Some(true) => "channel",
        Some("chatTypeSupergroup") => "group",
        // Private and secret chats: the peer is the user.
        _ => return None,
    };
    Some(IncomingPeer {
        id,
        kind: kind.to_string(),
        name: chat["title"].as_str().unwrap_or_default().to_string(),
        username: None,
        about: None,
        phone: None,
        status: None,
        last_seen: None,
        // A group's counts ride `updateSupergroupFullInfo`, not the chat; they
        // fill in a later phase.
        members: None,
        online: None,
        admin: false,
        is_contact: false,
        is_self: false,
    })
}

/// A chat's pinned rank, whether it is archived, and whether it sits in the
/// main list at all, read off its `positions`. A chat with no position in a
/// list is one the engine merely knows of, not one of mine. A single update
/// cannot know the exact rank among the pinned — that needs every chat at
/// once — so pinned is a flag, 1 or 0, until the list orders itself.
fn positions(chat: &Value) -> (i64, bool, bool) {
    let mut pinned = 0;
    let mut archived = false;
    let mut in_main = false;
    if let Some(arr) = chat["positions"].as_array() {
        for p in arr {
            match p["list"]["@type"].as_str() {
                Some("chatListMain") if order_present(&p["order"]) => {
                    in_main = true;
                    if p["is_pinned"].as_bool() == Some(true) {
                        pinned = 1;
                    }
                }
                Some("chatListArchive") if order_present(&p["order"]) => archived = true,
                _ => {}
            }
        }
    }
    (pinned, archived, in_main)
}

/// Whether a `chatPosition.order` places the chat in its list. `order` is an
/// int64, which TDLib's JSON carries as a string, and `"0"` means absent; read
/// either the string or a number shape defensively.
fn order_present(order: &Value) -> bool {
    order
        .as_str()
        .map(|s| !s.is_empty() && s != "0")
        .or_else(|| order.as_i64().map(|n| n != 0))
        .unwrap_or(false)
}

// -- what a chat says between its objects -------------------------------------------
//
// A chat arrives whole once and then changes a field at a time: its title,
// its mention count, the draft another device typed, and who is at the
// keyboard. Each rides its own update, and each of these mappers answers the
// one field it carries — the projection writes that column and no other, so
// nothing a chat object knew is lost to an update that knew one thing.

/// `updateChatTitle`: a group renamed, a person's name changed. The title
/// lives on the peer, a chat and its peer sharing one id, so this is a name
/// for [`project_peers`](super::project::project_peers)' row. `None` for an
/// update with no chat, or one whose title is blank — a name is not a thing
/// to lose to an empty string.
#[must_use]
pub fn chat_title(u: &Value) -> Option<(PeerId, String)> {
    Some((u["chat_id"].as_i64()?, nonempty(u["title"].as_str())?))
}

/// TDLib counts direct mentions and replies to my messages together.
#[must_use]
pub fn chat_mentions(u: &Value) -> Option<(PeerId, i64)> {
    let chat = u["chat_id"].as_i64()?;
    Some((chat, u["unread_mention_count"].as_i64()?.max(0)))
}

/// `updateChatDraftMessage`: what is typed and unsent, from whichever device
/// typed it. The inner `Option` is the draft itself — `None` where TDLib
/// sends a null draft, which is a draft cleared, and what the store must
/// write rather than keep. Only a text draft is read: a half-composed photo
/// is not a thing this composer can hold.
#[must_use]
pub fn chat_draft(u: &Value) -> Option<(PeerId, Option<String>)> {
    let chat = u["chat_id"].as_i64()?;
    let text = nonempty(u["draft_message"]["input_message_text"]["text"]["text"].as_str());
    Some((chat, text))
}

/// What an `updateChatAction` says: the chat, whoever is at the keyboard in
/// it, and whether they are still at it. Every action but `chatActionCancel`
/// is *somebody is doing something* — typing, recording a voice note,
/// sending a photo — which the header draws the one way, so the kind is not
/// kept; the cancel is the one that means *stopped*.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChatAction {
    pub chat: PeerId,
    /// The user doing it, where the sender is one — a channel's own
    /// anonymous action names no person, and reads as *someone*.
    pub who: Option<PeerId>,
    pub on: bool,
}

/// `updateChatAction`. `None` for an update with no chat or no action at all.
#[must_use]
pub fn chat_action(u: &Value) -> Option<ChatAction> {
    let chat = u["chat_id"].as_i64()?;
    let kind = u["action"]["@type"].as_str()?;
    Some(ChatAction {
        chat,
        who: sender_id(&u["sender_id"], chat),
        on: kind != "chatActionCancel",
    })
}

// -- a group's counts ---------------------------------------------------------------
//
// A chat says who it is; how many are in it comes separately, on the group
// behind it. TDLib numbers groups in their own spaces — a basic group and a
// supergroup may both be 42 — so each update carries a group id, and the
// chat that holds it is that id turned into the chat space the same way
// every Telegram client turns it. Nothing here writes: the mappers answer
// what the update said, and the projection keeps what it was not told.

/// The marker a supergroup's or a channel's chat id counts down from.
const SUPERGROUP_BASE: PeerId = -1_000_000_000_000;

/// What a group update says about the peer behind it — whichever of the
/// counts and the description it carried, and no more. Every field is
/// optional on purpose: a `supergroup` knows its size and not its
/// description, an online count knows neither, and a value the update did
/// not carry must not null the one the store has
/// ([`project_peers`](super::project::project_peers) coalesces).
#[derive(Debug, Clone, PartialEq, Default)]
pub struct PeerCounts {
    pub id: PeerId,
    pub members: Option<i64>,
    pub online: Option<i64>,
    pub about: Option<String>,
    /// Whether I may post in it — a channel's owner or an administrator
    /// allowed to post, as the supergroup's `status` says; `None` where
    /// the update carries no status.
    pub admin: Option<bool>,
    /// The chat linked to this one: a channel's discussion group, or a
    /// discussion group's channel. `Some(0)` says there is none, which is
    /// how a link that has been taken away is cleared.
    pub linked: Option<PeerId>,
    /// Whether it takes a message only from a member. The wire allows this
    /// to be false only for a discussion group, which is exactly the case
    /// where a stranger may leave a comment without joining.
    pub join_to_send: Option<bool>,
}

/// Whether a `ChatMemberStatus` lets me post: the creator always, an
/// administrator with the right, nobody else. A channel hides its composer
/// from everyone but these (review, 2026-09-07: an owner could not post).
fn may_post(status: &Value) -> Option<bool> {
    Some(match status["@type"].as_str()? {
        "chatMemberStatusCreator" => true,
        "chatMemberStatusAdministrator" => status["rights"]["can_post_messages"]
            .as_bool()
            .unwrap_or(true),
        _ => false,
    })
}

/// `updateSupergroup`: a supergroup's or a channel's size, under the chat id
/// its own id stands for. A nought is *not yet known* rather than an empty
/// group — one I am in has me in it — so it is dropped and the full info,
/// which knows, fills it in.
#[must_use]
pub fn supergroup(u: &Value) -> Option<PeerCounts> {
    let g = &u["supergroup"];
    Some(PeerCounts {
        id: SUPERGROUP_BASE - g["id"].as_i64()?,
        members: g["member_count"].as_i64().filter(|&n| n > 0),
        admin: may_post(&g["status"]),
        join_to_send: g["join_to_send_messages"].as_bool(),
        ..PeerCounts::default()
    })
}

/// `updateBasicGroup`: a small group's size. A basic group's chat is simply
/// its id negated.
#[must_use]
pub fn basic_group(u: &Value) -> Option<PeerCounts> {
    let g = &u["basic_group"];
    Some(PeerCounts {
        id: -g["id"].as_i64()?,
        members: g["member_count"].as_i64().filter(|&n| n > 0),
        ..PeerCounts::default()
    })
}

/// `updateSupergroupFullInfo`: the count again, now the server's own answer,
/// and the description the card shows under the name. The group id is beside
/// the object rather than in it.
#[must_use]
pub fn supergroup_full(u: &Value) -> Option<PeerCounts> {
    let full = &u["supergroup_full_info"];
    Some(PeerCounts {
        id: SUPERGROUP_BASE - u["supergroup_id"].as_i64()?,
        members: full["member_count"].as_i64().filter(|&n| n > 0),
        online: full["online_member_count"].as_i64(),
        about: nonempty(full["description"].as_str()),
        admin: None,
        // The one update that names a channel's discussion group. It always
        // knows, so a nought is an answer — the link is gone — rather than
        // a field this update did not carry.
        linked: Some(full["linked_chat_id"].as_i64().unwrap_or(0)),
        join_to_send: None,
    })
}

/// `updateBasicGroupFullInfo`: the description, and the size — which a small
/// group spells out where it has a count and otherwise leaves to the member
/// list it carries, that list being the whole of a basic group.
#[must_use]
pub fn basic_group_full(u: &Value) -> Option<PeerCounts> {
    let full = &u["basic_group_full_info"];
    let listed = full["members"].as_array().map(|a| a.len() as i64);
    Some(PeerCounts {
        id: -u["basic_group_id"].as_i64()?,
        members: full["member_count"]
            .as_i64()
            .filter(|&n| n > 0)
            .or(listed),
        online: None,
        about: nonempty(full["description"].as_str()),
        admin: None,
        // A small group has neither a discussion group nor a joining rule.
        linked: None,
        join_to_send: None,
    })
}

/// The membership a `updateBasicGroupFullInfo` carries: who is in the group
/// and who runs it — the creator and the administrators, which is what the
/// members list marks and what lets me post in a channel.
///
/// A **supergroup's** members are in no update at all: they are asked for a
/// page at a time with `getSupergroupMembers`, which is a later phase. Until
/// then a supergroup's `tg_member` rows are only the senders a transcript
/// brought.
#[must_use]
pub fn basic_group_members(u: &Value) -> Vec<IncomingMember> {
    let Some(chat) = u["basic_group_id"].as_i64().map(|id| -id) else {
        return Vec::new();
    };
    let Some(rows) = u["basic_group_full_info"]["members"].as_array() else {
        return Vec::new();
    };
    rows.iter()
        .filter_map(|m| {
            // `member_id` is TDLib's `MessageSender`, the same shape a line's
            // writer wears, so it is read the same way.
            Some(IncomingMember {
                chat,
                peer: sender_id(&m["member_id"], chat)?,
                admin: matches!(
                    m["status"]["@type"].as_str(),
                    Some("chatMemberStatusCreator" | "chatMemberStatusAdministrator")
                ),
            })
        })
        .collect()
}

/// `updateChatOnlineMemberCount`: how many of a group are here now. Nought is
/// a value like any other — the group is quiet — so it is kept, not dropped.
/// This one names the chat outright; there is no group id to turn.
#[must_use]
pub fn chat_online(u: &Value) -> Option<PeerCounts> {
    Some(PeerCounts {
        id: u["chat_id"].as_i64()?,
        online: Some(u["online_member_count"].as_i64().unwrap_or(0)),
        ..PeerCounts::default()
    })
}

// -- shared -------------------------------------------------------------------------

/// A trimmed, non-empty string, or `None` — the shape every optional text
/// field wants, so a `""` from the wire is an absence, not a blank value.
fn nonempty(s: Option<&str>) -> Option<String> {
    s.map(str::trim)
        .filter(|x| !x.is_empty())
        .map(str::to_string)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    /// The nine media kinds, one content object each: the @type maps to the
    /// kind the widgets draw, and the file's remote unique id becomes the
    /// blob-cache key.
    #[test]
    fn every_content_kind_maps_to_its_media() {
        let file = |uid: &str| json!({"remote": {"unique_id": uid}});
        // A file the player can ask back for carries the durable remote id
        // beside the unique one.
        let remote = |uid: &str, rid: &str| json!({"remote": {"unique_id": uid, "id": rid}});

        let (text, media) = content(
            &json!({"@type": "messageText", "text": {"text": "hi"}}),
            0.0,
                   0.0,
        );
        assert_eq!(text, "hi");
        assert!(media.is_none(), "text carries no media");

        let (cap, m) = content(
            &json!({
                "@type": "messagePhoto",
                "caption": {"text": "the garden"},
                "photo": {"sizes": [
                    {"width": 90, "height": 60, "photo": file("small")},
                    {"width": 1280, "height": 850, "photo": file("big")},
                ]},
            }),
            0.0,
                   0.0,
        );
        let m = m.expect("a photo");
        assert_eq!(cap, "the garden");
        assert_eq!(m.kind, "photo");
        assert_eq!(
            m.reference.as_deref(),
            Some("tg:big"),
            "the largest size wins"
        );
        assert_eq!((m.w, m.h), (Some(1280), Some(850)));

        // A moving picture's reference is its thumbnail — the poster — not
        // the clip: the clip is never fetched on arrival.
        let (_, m) = content(
            &json!({"@type": "messageVideo", "video": {"width": 640, "height": 480, "duration": 12,
                     "video": remote("v", "RID_V"), "thumbnail": {"file": file("vposter")}}}),
            0.0,
                   0.0,
        );
        let m = m.expect("a video");
        assert_eq!(m.kind, "video");
        assert_eq!(m.secs, Some(12));
        assert_eq!(m.reference.as_deref(), Some("tg:vposter"), "the poster, not the clip");
        assert_eq!(m.clip.as_deref(), Some("tg:v"), "the clip is named beside it");
        assert_eq!(m.clip_rid.as_deref(), Some("RID_V"), "and asked for by this");

        let (_, m) = content(
            &json!({"@type": "messageAnimation", "animation": {"width": 200, "height": 200, "duration": 3,
                     "animation": remote("gif", "RID_G"), "thumbnail": {"file": file("gifposter")}}}),
            0.0,
                   0.0,
        );
        let m = m.expect("a gif");
        assert_eq!(m.kind, "video", "an animation draws as video");
        assert_eq!(m.reference.as_deref(), Some("tg:gifposter"));
        assert_eq!((m.clip.as_deref(), m.clip_rid.as_deref()), (Some("tg:gif"), Some("RID_G")));

        let (_, m) = content(
            &json!({"@type": "messageVideoNote", "video_note": {"duration": 8,
                     "video": remote("vn", "RID_N"), "thumbnail": {"file": file("vnposter")}}}),
            0.0,
                   0.0,
        );
        let m = m.expect("a circle");
        assert_eq!(m.kind, "circle");
        assert_eq!(m.reference.as_deref(), Some("tg:vnposter"));
        assert_eq!((m.clip.as_deref(), m.clip_rid.as_deref()), (Some("tg:vn"), Some("RID_N")));

        // Without a thumbnail there is no poster to draw, and no clip
        // fetched; the clip is still named, so opening the line can ask.
        let (_, m) = content(
            &json!({"@type": "messageVideo", "video": {"duration": 5, "video": remote("bare", "RID_B")}}),
            0.0,
                   0.0,
        );
        let m = m.expect("a video");
        assert_eq!(m.reference, None, "no thumbnail, no poster");
        assert_eq!(m.clip.as_deref(), Some("tg:bare"), "the clip is there either way");

        let (_, m) = content(
            &json!({"@type": "messageVoiceNote", "voice_note": {"duration": 42, "voice": file("voi")}}),
            0.0,
                   0.0,
        );
        let m = m.expect("a voice note");
        assert_eq!(m.kind, "voice");
        assert_eq!(m.secs, Some(42));

        let (_, m) = content(
            &json!({"@type": "messageAudio", "audio": {"duration": 221, "title": "Scratchcard Lanyard", "performer": "Dry Cleaning", "audio": file("au")}}),
            0.0,
                   0.0,
        );
        let m = m.expect("an audio track");
        assert_eq!(m.kind, "audio");
        assert_eq!(
            m.label.as_deref(),
            Some("Scratchcard Lanyard"),
            "title over performer"
        );

        let (_, m) = content(
            &json!({"@type": "messageSticker", "sticker": {"emoji": "🙈", "width": 512, "height": 512, "sticker": file("st")}}),
            0.0,
                   0.0,
        );
        let m = m.expect("a sticker");
        assert_eq!(m.kind, "sticker");
        assert_eq!(m.label.as_deref(), Some("🙈"));

        let (_, m) = content(
            &json!({"@type": "messageDocument", "document": {"file_name": "report-q3.pdf", "document": file("doc")}}),
            0.0,
                   0.0,
        );
        let m = m.expect("a document");
        assert_eq!(m.kind, "file");
        assert_eq!(m.label.as_deref(), Some("report-q3.pdf"));

        let (_, m) = content(
            &json!({"@type": "messageLocation", "location": {"latitude": 47.0472, "longitude": 8.3164}}),
            0.0,
                   0.0,
        );
        assert_eq!(m.expect("a location").kind, "location");

        let (_, m) = content(
            &json!({"@type": "messageLocation", "live_period": 3600, "location": {"latitude": 55.75, "longitude": 37.61}}),
            1000.0,
            1000.0,
        );
        let m = m.expect("a live location");
        assert_eq!(m.kind, "live");
        assert_eq!(m.until, Some(4600.0), "date plus the live period");

        // An unknown content kind degrades to a word, never a dropped line.
        let (text, media) = content(&json!({"@type": "messagePoll", "poll": {}}), 0.0, 0.0);
        assert_eq!(text, "Poll");
        assert!(media.is_none());
    }

    /// A fetched live location ends where the wire says it has left to run,
    /// not where a date plus a period would put it — and either way it ends
    /// five seconds early for a period that is not whole minutes, which is
    /// how the phone's client copes with Apple's 3599 for an hour.
    #[test]
    fn a_live_location_ends_by_what_the_wire_says_is_left_of_it() {
        let live = |period: i64, left: i64| {
            json!({"@type": "messageLiveLocation", "expires_in": left, "location": {
                "@type": "liveLocation", "live_period": period, "heading": 0,
                "location": {"latitude": 47.0472, "longitude": 8.3164}}})
        };
        // A page fetched half an hour in: what is left is what is left,
        // whatever the message's date says.
        let (_, m) = content(&live(3600, 1800), 1000.0, 90_000.0);
        assert_eq!(m.expect("a live location").until, Some(91_800.0));

        // Apple's hour: five seconds early, from either end.
        let (_, m) = content(&live(3599, 3599), 1000.0, 90_000.0);
        assert_eq!(m.expect("a live location").until, Some(93_594.0));
        let (_, m) = content(&live(3599, 0), 1000.0, 90_000.0);
        assert_eq!(
            m.expect("a live location").until,
            Some(4594.0),
            "an ended share has no `expires_in`, and the date is what is left"
        );

        // *Until stopped* is no countdown, and keeps every second it has.
        let forever = super::super::requests::LIVE_FOREVER;
        let (_, m) = content(&live(forever, forever), 1000.0, 90_000.0);
        assert_eq!(
            m.expect("a live location").until,
            Some(90_000.0 + forever as f64)
        );

        // And a whole message reads the same, through the clock the
        // projection hands it.
        let m = message(
            &json!({"@type": "message", "id": 9, "chat_id": -7, "date": 1000.0,
                    "content": live(3600, 1800)}),
            90_000.0,
        )
        .expect("a line");
        assert_eq!(m.media.expect("a live location").until, Some(91_800.0));
    }

    /// The picture's own durable name. A photo is fetched as its line
    /// arrives — but only for the newest forty lines of a chat as it opens,
    /// and the blob cache evicts what it must, so the row keeps the id the
    /// file can be asked for by afterwards: the largest size's for a photo,
    /// the poster's for a moving picture, and never the clip's, which has a
    /// field of its own (review, 2026-09-07).
    #[test]
    fn a_picture_carries_the_id_it_can_be_asked_for_by() {
        let remote = |uid: &str, rid: &str| json!({"remote": {"unique_id": uid, "id": rid}});

        let (_, m) = content(
            &json!({"@type": "messagePhoto", "caption": {"text": ""},
                    "photo": {"sizes": [
                        {"width": 90, "height": 60, "photo": remote("small", "RID_SMALL")},
                        {"width": 1280, "height": 850, "photo": remote("big", "RID_BIG")},
                    ]}}),
            0.0,
                   0.0,
        );
        let m = m.expect("a photo");
        assert_eq!(m.reference.as_deref(), Some("tg:big"));
        assert_eq!(
            m.rid.as_deref(),
            Some("RID_BIG"),
            "the size the row points at is the size asked for"
        );

        let (_, m) = content(
            &json!({"@type": "messageVideo",
                    "video": {"width": 640, "height": 480, "duration": 12,
                              "video": remote("v", "RID_V"),
                              "thumbnail": {"file": remote("vposter", "RID_P")}}}),
            0.0,
                   0.0,
        );
        let m = m.expect("a video");
        assert_eq!(
            (m.reference.as_deref(), m.rid.as_deref()),
            (Some("tg:vposter"), Some("RID_P")),
            "the poster, and the poster's own id"
        );
        assert_eq!((m.clip.as_deref(), m.clip_rid.as_deref()), (Some("tg:v"), Some("RID_V")));

        // A file the server has no remote id for names none, and the column
        // stays empty rather than holding a number that means nothing.
        let (_, m) = content(
            &json!({"@type": "messagePhoto", "caption": {"text": ""},
                    "photo": {"sizes": [{"width": 9, "height": 9,
                                         "photo": {"remote": {"unique_id": "u"}}}]}}),
            0.0,
                   0.0,
        );
        assert_eq!(m.expect("a photo").rid, None);
    }

    /// The clip is the moving picture's alone, and it is a name, not a
    /// fetch: only a video, an animation and a video note carry one, a file
    /// the server has no copy of carries none, and what the worker asks for
    /// on arrival is still the poster.
    #[test]
    fn only_a_moving_picture_carries_a_clip() {
        let file = |uid: &str| json!({"remote": {"unique_id": uid}});
        let remote = |uid: &str, rid: &str| json!({"remote": {"unique_id": uid, "id": rid}});

        // Everything else leaves both empty — a sound is not this round's, a
        // sticker and a document are the opener's.
        for c in [
            json!({"@type": "messagePhoto", "photo": {"sizes": [{"width": 9, "height": 9, "photo": remote("p", "RID_P")}]}}),
            json!({"@type": "messageVoiceNote", "voice_note": {"duration": 42, "voice": remote("voi", "RID_W")}}),
            json!({"@type": "messageAudio", "audio": {"duration": 221, "audio": remote("au", "RID_A")}}),
            json!({"@type": "messageSticker", "sticker": {"emoji": "🙈", "sticker": remote("st", "RID_S")}}),
            json!({"@type": "messageDocument", "document": {"file_name": "q3.pdf", "document": remote("doc", "RID_D")}}),
        ] {
            let m = content(&c, 0.0, 0.0).1.expect("some media");
            assert_eq!((m.clip.clone(), m.clip_rid.clone()), (None, None), "{}", c["@type"]);
        }

        // A clip the server has no copy of — a purely local file, mid-upload
        // — has nothing to be asked back for, so neither is filed.
        let (_, m) = content(
            &json!({"@type": "messageVideo", "video": {"duration": 5, "video": {"id": 12},
                     "thumbnail": {"file": file("poster")}}}),
            0.0,
                   0.0,
        );
        let m = m.expect("a video");
        assert_eq!(m.reference.as_deref(), Some("tg:poster"));
        assert_eq!((m.clip, m.clip_rid), (None, None), "no remote, nothing to ask for");

        // And naming the clip did not make it something the worker fetches on
        // arrival: that is still the poster's file id and the poster's key.
        let v = json!({"@type": "messageVideo", "video": {"duration": 5,
                 "video": json!({"id": 40, "remote": {"unique_id": "clip", "id": "RID_C"}}),
                 "thumbnail": {"file": json!({"id": 41, "remote": {"unique_id": "poster"}})}}});
        assert_eq!(download_id(&v), Some(41));
        assert_eq!(download_key(&v).as_deref(), Some("tg:poster"));
    }

    /// A full message maps its frame — id, chat, sender, out, reply, edited —
    /// alongside its content.
    #[test]
    fn a_message_maps_its_frame() {
        let m = message(&json!({
            "@type": "message",
            "id": 4210,
            "chat_id": -100200,
            "date": 1_725_000_000,
            "is_outgoing": true,
            "sender_id": {"@type": "messageSenderUser", "user_id": 77},
            "edit_date": 1_725_000_050,
            "reply_to_message_id": 4200,
            "sending_state": {"@type": "messageSendingStatePending"},
            "content": {"@type": "messageText", "text": {"text": "on my way"}},
        }), 0.0)
        .expect("a message");
        assert_eq!(m.id, 4210);
        assert_eq!(m.chat, -100200);
        assert_eq!(m.sender, Some(77));
        assert!(m.out);
        assert_eq!(m.state.as_deref(), Some("sending"));
        assert!(m.edited);
        assert_eq!(m.reply_to, Some(4200));
        assert_eq!(m.text, "on my way");

        // A channel's own post — the chat posting as itself — is senderless.
        let post = message(&json!({
            "@type": "message",
            "id": 5,
            "chat_id": -100,
            "sender_id": {"@type": "messageSenderChat", "chat_id": -100},
            "content": {"@type": "messageText", "text": {"text": "notice"}},
        }), 0.0)
        .expect("a post");
        assert_eq!(post.sender, None);
        assert!(!post.out);
        assert_eq!(post.state, None, "not mine, so no send state");

        // No id, no row.
        assert!(message(&json!({"@type": "message", "chat_id": 1}), 0.0).is_none());
        assert!(message(&json!({}), 0.0).is_none());
    }

    /// Every origin a forward can have, in the spelling the installed TDLib
    /// writes: three name a peer, one carries a bare name, and a channel
    /// also names the post it was taken from.
    ///
    /// This is the shape that was missed. The whole decoder looked for
    /// `messageForwardOrigin*`, which TDLib renamed to `messageOrigin*`, so
    /// no forward on a real account ever read as one — and a person's, the
    /// commonest of the four, was never read under either spelling.
    #[test]
    fn a_forward_keeps_the_peer_it_came_from() {
        let forwarded = |origin: serde_json::Value| {
            message(&json!({
                "@type": "message", "id": 7, "chat_id": 9,
                "content": {"@type": "messageText", "text": {"text": "look at this"}},
                "forward_info": {"@type": "messageForwardInfo", "origin": origin, "date": 1_725_000_000},
            }), 0.0)
            .expect("a message")
            .fwd
        };

        let person = forwarded(json!({"@type": "messageOriginUser", "sender_user_id": 77}));
        assert_eq!(person.peer, Some(77), "the person, to be named off their peer row");
        assert_eq!(person.name, None, "no name is copied onto the line");

        let channel = forwarded(json!({"@type": "messageOriginChannel",
            "chat_id": -1001, "message_id": 4200, "author_signature": "Elena"}));
        assert_eq!(channel.peer, Some(-1001));
        assert_eq!(channel.msg, Some(4200), "the post itself, which `came from` opens");
        assert_eq!(channel.sign.as_deref(), Some("Elena"));

        let group = forwarded(json!({"@type": "messageOriginChat",
            "sender_chat_id": -55, "author_signature": ""}));
        assert_eq!(group.peer, Some(-55));
        assert_eq!(group.msg, None, "a group origin names no post");
        assert_eq!(group.sign, None, "an empty signature is an absent one");

        let hidden = forwarded(json!({"@type": "messageOriginHiddenUser", "sender_name": "Anon"}));
        assert_eq!(hidden.peer, None);
        assert_eq!(hidden.name.as_deref(), Some("Anon"), "a hidden sender has only a name");

        // The spelling before the rename still reads, the fields inside it
        // being unchanged.
        let old = forwarded(json!({"@type": "messageForwardOriginChannel",
            "chat_id": -1002, "message_id": 8, "author_signature": "Max"}));
        assert_eq!((old.peer, old.msg, old.sign.as_deref()), (Some(-1002), Some(8), Some("Max")));

        // A chat imported from another app carries its sender beside
        // `forward_info` rather than inside it.
        let imported = message(&json!({
            "@type": "message", "id": 8, "chat_id": 9,
            "content": {"@type": "messageText", "text": {"text": "from whatsapp"}},
            "import_info": {"@type": "messageImportInfo", "sender_name": "Nina", "date": 1},
        }), 0.0)
        .expect("a message").fwd;
        assert_eq!(imported.name.as_deref(), Some("Nina"));
        assert_eq!(imported.peer, None);

        // And an ordinary line is forwarded from nowhere.
        let plain = message(&json!({
            "@type": "message", "id": 9, "chat_id": 9,
            "content": {"@type": "messageText", "text": {"text": "mine"}},
        }), 0.0)
        .expect("a message").fwd;
        assert_eq!(plain, Forward::default());
    }

    /// The file a message points TDLib at to download: the largest photo
    /// size's own id, a document's, and nothing for a line with no file.
    #[test]
    fn download_id_picks_the_file_to_fetch() {
        // A photo offers several sizes; the largest is the one drawn, so its
        // file id — not a thumbnail's — is the one fetched.
        let photo = json!({
            "@type": "messagePhoto",
            "photo": {"sizes": [
                {"width": 90, "height": 60, "photo": {"id": 11, "remote": {"unique_id": "small"}}},
                {"width": 1280, "height": 850, "photo": {"id": 22, "remote": {"unique_id": "big"}}},
            ]},
        });
        assert_eq!(download_id(&photo), Some(22), "the largest size's id");

        // A video fetches its thumbnail — the poster — and never the clip.
        let video = json!({
            "@type": "messageVideo",
            "video": {"video": {"id": 40, "remote": {"unique_id": "clip"}},
                      "thumbnail": {"file": {"id": 41, "remote": {"unique_id": "poster"}}}},
        });
        assert_eq!(download_id(&video), Some(41), "the poster's id, not the clip's");
        // One without a thumbnail fetches nothing on arrival.
        assert_eq!(
            download_id(&json!({"@type": "messageVideo", "video": {"video": {"id": 40, "remote": {"unique_id": "clip"}}}})),
            None
        );

        // A document, a voice note, a track, a sticker are fetched on open,
        // never on arrival.
        let doc = json!({
            "@type": "messageDocument",
            "document": {"file_name": "q3.pdf", "document": {"id": 7, "remote": {"unique_id": "d"}}},
        });
        assert_eq!(download_id(&doc), None, "a document waits to be opened");
        assert_eq!(
            download_id(&json!({"@type": "messageVoiceNote", "voice_note": {"voice": {"id": 8}}})),
            None
        );
        assert_eq!(
            download_id(&json!({"@type": "messageSticker", "sticker": {"sticker": {"id": 9}}})),
            None
        );

        // A text and a location carry no file to fetch.
        assert_eq!(download_id(&json!({"@type": "messageText", "text": {"text": "hi"}})), None);
        assert_eq!(
            download_id(&json!({"@type": "messageLocation", "location": {"latitude": 1.0, "longitude": 2.0}})),
            None
        );
    }

    /// The key looked up before a fetch names the very file the id fetches —
    /// the largest size, the poster — so a cache hit on the key is a hit on
    /// the file; and nothing where nothing is fetched on arrival.
    #[test]
    fn download_key_names_the_file_the_id_fetches() {
        let photo = json!({
            "@type": "messagePhoto",
            "photo": {"sizes": [
                {"width": 90, "height": 60, "photo": {"id": 11, "remote": {"unique_id": "small"}}},
                {"width": 1280, "height": 850, "photo": {"id": 22, "remote": {"unique_id": "big"}}},
            ]},
        });
        assert_eq!(download_key(&photo).as_deref(), Some("tg:big"), "the largest size's key");

        let video = json!({
            "@type": "messageVideo",
            "video": {"video": {"id": 40, "remote": {"unique_id": "clip"}},
                      "thumbnail": {"file": {"id": 41, "remote": {"unique_id": "poster"}}}},
        });
        assert_eq!(download_key(&video).as_deref(), Some("tg:poster"), "the poster's key");

        assert_eq!(download_key(&json!({"@type": "messageText", "text": {"text": "hi"}})), None);
        assert_eq!(
            download_key(&json!({"@type": "messageDocument", "document": {"document": {"id": 7, "remote": {"unique_id": "d"}}}})),
            None,
            "a document is not fetched on arrival, so there is no key to look up"
        );
    }

    /// A user maps to a person peer with its name, handle and presence.
    #[test]
    fn a_user_maps_to_a_person() {
        let p = peer(&json!({
            "@type": "user",
            "id": 2,
            "first_name": "Vera",
            "last_name": "Kovac",
            "username": "vera",
            "phone_number": "",
            "status": {"@type": "userStatusOnline", "expires": 1_725_000_000},
            "is_contact": true,
        }))
        .expect("a peer");
        assert_eq!(p.id, 2);
        assert_eq!(p.kind, "person");
        assert_eq!(p.name, "Vera Kovac");
        assert_eq!(p.username.as_deref(), Some("vera"));
        assert_eq!(p.phone, None, "an empty phone is an absence");
        assert_eq!(p.status.as_deref(), Some("online"));
        assert!(p.is_contact);

        // Gone offline: a word, not an absence — the upsert keeps what it is
        // not told, and *online* must not outlive the person's being here.
        let gone = peer(&json!({
            "@type": "user", "id": 2, "first_name": "Vera",
            "status": {"@type": "userStatusOffline", "was_online": 1_725_000_000},
        }))
        .expect("a peer");
        assert_eq!(gone.status.as_deref(), Some("offline"));
        assert_eq!(gone.last_seen, Some(1_725_000_000.0));
    }

    /// A supergroup chat implies a channel peer named by its title, and its
    /// Reactions come wrapped in a `messageReactions` object on current
    /// TDLib and as a bare array on the old layer; a channel's status says
    /// whether I may post there.
    #[test]
    fn reactions_unwrap_and_a_status_says_who_may_post() {
        let wrapped = json!({"reactions": {"@type": "messageReactions", "reactions": [
            {"type": {"@type": "reactionTypeEmoji", "emoji": "👍"}, "total_count": 3}]}});
        assert_eq!(reactions_line(&wrapped).as_deref(), Some("👍 3"));
        let bare = json!({"reactions": [{"reaction": "❤", "total_count": 1}]});
        assert_eq!(reactions_line(&bare).as_deref(), Some("❤ 1"));

        let owner = json!({"@type": "updateSupergroup", "supergroup": {"id": 5, "member_count": 9,
            "status": {"@type": "chatMemberStatusCreator"}}});
        assert_eq!(supergroup(&owner).unwrap().admin, Some(true));
        let admin = json!({"@type": "updateSupergroup", "supergroup": {"id": 5, "member_count": 9,
            "status": {"@type": "chatMemberStatusAdministrator", "rights": {"can_post_messages": false}}}});
        assert_eq!(supergroup(&admin).unwrap().admin, Some(false));
        let member = json!({"@type": "updateSupergroup", "supergroup": {"id": 5, "member_count": 9,
            "status": {"@type": "chatMemberStatusMember"}}});
        assert_eq!(supergroup(&member).unwrap().admin, Some(false));
        let unsaid = json!({"@type": "updateSupergroup", "supergroup": {"id": 5, "member_count": 9}});
        assert_eq!(supergroup(&unsaid).unwrap().admin, None);
    }

    /// A chat with no position in any list is known, not mine: the mapper
    /// says so, and an unpinned main position is in the list without a rank.
    #[test]
    fn a_chat_without_a_position_is_not_in_the_list() {
        let seen = json!({"@type": "chat", "id": -1005, "title": "дядя сэм",
            "type": {"@type": "chatTypeSupergroup", "supergroup_id": 5, "is_channel": true},
            "positions": []});
        let c = chat(&seen).unwrap();
        assert!(!c.in_main && !c.archived && c.pinned == 0);
        let mine = json!({"@type": "chat", "id": -1006, "title": "stelaxis",
            "type": {"@type": "chatTypeSupergroup", "supergroup_id": 6},
            "positions": [{"list": {"@type": "chatListMain"}, "order": "42", "is_pinned": false}]});
        let c = chat(&mine).unwrap();
        assert!(c.in_main && !c.archived && c.pinned == 0);
        let shelved = json!({"@type": "chat", "id": -1007, "title": "old",
            "type": {"@type": "chatTypeSupergroup", "supergroup_id": 7},
            "positions": [{"list": {"@type": "chatListArchive"}, "order": "42", "is_pinned": false}]});
        let c = chat(&shelved).unwrap();
        assert!(!c.in_main && c.archived);
    }

    /// list flags read off the positions; a private chat implies no peer.
    #[test]
    fn a_chat_maps_its_flags_and_its_group_peer() {
        let raw = json!({
            "@type": "chat",
            "id": -1001,
            "title": "Stelaxis",
            "type": {"@type": "chatTypeSupergroup", "is_channel": false},
            "unread_count": 3,
            "unread_mention_count": 1,
            "last_read_inbox_message_id": 900,
            "notification_settings": {"mute_for": 0},
            "positions": [{"list": {"@type": "chatListMain"}, "order": "77", "is_pinned": true}],
            "draft_message": {"input_message_text": {"text": {"text": "half a thought"}}},
        });
        let ch = chat(&raw).expect("a chat");
        assert_eq!(ch.peer, -1001);
        assert_eq!(ch.unread, 3);
        assert_eq!(ch.unread_mentions, 1);
        assert_eq!(ch.pinned, 1);
        assert!(!ch.muted);
        assert_eq!(ch.last_read, Some(900));
        assert_eq!(ch.draft.as_deref(), Some("half a thought"));

        let cp = chat_peer(&raw).expect("a group peer");
        assert_eq!(cp.id, -1001);
        assert_eq!(cp.kind, "group");
        assert_eq!(cp.name, "Stelaxis");

        // A private chat carries no derived peer — the user update owns it.
        let private = json!({"@type": "chat", "id": 2, "title": "Vera Kovac",
                             "type": {"@type": "chatTypePrivate", "user_id": 2}});
        assert!(chat_peer(&private).is_none());
        assert!(chat(&private).is_some(), "the chat row still maps");

        // A muted chat says so through its notification settings.
        let quiet = json!({"@type": "chat", "id": -1002,
                           "notification_settings": {"mute_for": 2_147_483_647}});
        assert!(chat(&quiet).expect("a chat").muted);
    }

    /// The four group updates land on the chat id each group id stands for,
    /// and carry only what they knew: a size, an online count, a description.
    #[test]
    fn a_group_update_counts_its_members_under_the_chats_id() {
        let sg = supergroup(&json!({
            "@type": "updateSupergroup",
            "supergroup": {"id": 1234, "member_count": 812, "is_channel": true},
        }))
        .expect("a supergroup");
        assert_eq!(sg.id, -1_000_000_001_234, "the chat id counts down from the marker");
        assert_eq!(sg.members, Some(812));
        assert_eq!(sg.about, None, "the plain update carries no description");

        // A nought is not yet known, not an empty group.
        let unknown = supergroup(&json!({
            "@type": "updateSupergroup", "supergroup": {"id": 1234, "member_count": 0},
        }))
        .expect("a supergroup");
        assert_eq!(unknown.members, None);

        let bg = basic_group(&json!({
            "@type": "updateBasicGroup", "basic_group": {"id": 77, "member_count": 7},
        }))
        .expect("a basic group");
        assert_eq!(bg.id, -77);
        assert_eq!(bg.members, Some(7));

        let full = supergroup_full(&json!({
            "@type": "updateSupergroupFullInfo",
            "supergroup_id": 1234,
            "supergroup_full_info": {
                "member_count": 815, "online_member_count": 12,
                "description": "  the design channel  ",
            },
        }))
        .expect("the full info");
        assert_eq!(full.id, -1_000_000_001_234);
        assert_eq!(full.members, Some(815));
        assert_eq!(full.online, Some(12));
        assert_eq!(full.about.as_deref(), Some("the design channel"));

        let online = chat_online(&json!({
            "@type": "updateChatOnlineMemberCount",
            "chat_id": -1_000_000_001_234_i64, "online_member_count": 0,
        }))
        .expect("an online count");
        assert_eq!(online.id, -1_000_000_001_234);
        assert_eq!(online.online, Some(0), "quiet is a count, not an absence");
        assert_eq!(online.members, None);

        // Nothing without an id at all.
        assert!(supergroup(&json!({"@type": "updateSupergroup"})).is_none());
        assert!(basic_group(&json!({"@type": "updateBasicGroup"})).is_none());
        assert!(chat_online(&json!({"@type": "updateChatOnlineMemberCount"})).is_none());
    }

    /// A basic group's full info brings its members with it: the creator and
    /// the administrators run it, everyone else is in it, and where no count
    /// is spelled the list is the count.
    #[test]
    fn a_basic_groups_full_info_carries_its_members() {
        let u = json!({
            "@type": "updateBasicGroupFullInfo",
            "basic_group_id": 77,
            "basic_group_full_info": {
                "description": "the hike",
                "members": [
                    {"member_id": {"@type": "messageSenderUser", "user_id": 2},
                     "status": {"@type": "chatMemberStatusCreator"}},
                    {"member_id": {"@type": "messageSenderUser", "user_id": 3},
                     "status": {"@type": "chatMemberStatusAdministrator"}},
                    {"member_id": {"@type": "messageSenderUser", "user_id": 4},
                     "status": {"@type": "chatMemberStatusMember"}},
                ],
            },
        });
        let counts = basic_group_full(&u).expect("the full info");
        assert_eq!(counts.id, -77);
        assert_eq!(counts.members, Some(3), "the list is the count");
        assert_eq!(counts.about.as_deref(), Some("the hike"));

        let members = basic_group_members(&u);
        assert_eq!(
            members,
            vec![
                IncomingMember { chat: -77, peer: 2, admin: true },
                IncomingMember { chat: -77, peer: 3, admin: true },
                IncomingMember { chat: -77, peer: 4, admin: false },
            ]
        );
        assert!(basic_group_members(&json!({"@type": "updateBasicGroupFullInfo"})).is_empty());
    }

    /// A status arriving on its own carries the same words a user object's
    /// does, and a bucket with no word for it is dropped rather than
    /// nulling what the store holds.
    #[test]
    fn a_status_on_its_own_says_where_a_person_is() {
        let (id, status, seen) = user_presence(&json!({
            "@type": "updateUserStatus", "user_id": 2,
            "status": {"@type": "userStatusOnline", "expires": 1_725_000_600},
        }))
        .expect("a presence");
        assert_eq!((id, status.as_str()), (2, "online"));
        assert_eq!(seen, Some(1_725_000_600.0));

        let (_, status, seen) = user_presence(&json!({
            "@type": "updateUserStatus", "user_id": 2,
            "status": {"@type": "userStatusOffline", "was_online": 1_725_000_000},
        }))
        .expect("a presence");
        assert_eq!((status.as_str(), seen), ("offline", Some(1_725_000_000.0)));

        assert!(user_presence(&json!({"@type": "updateUserStatus", "user_id": 2})).is_none());
        assert!(user_presence(&json!({"@type": "updateUserStatus"})).is_none());
    }

    /// Every action but the cancel is somebody at the keyboard; the cancel
    /// is the one that says they stopped. An anonymous one names no person.
    #[test]
    fn an_action_says_who_is_at_the_keyboard_and_whether_they_still_are() {
        let typing = |kind: &str| {
            chat_action(&json!({
                "@type": "updateChatAction", "chat_id": -1005,
                "sender_id": {"@type": "messageSenderUser", "user_id": 2},
                "action": {"@type": kind},
            }))
            .expect("an action")
        };
        assert_eq!(typing("chatActionTyping"), ChatAction { chat: -1005, who: Some(2), on: true });
        assert!(typing("chatActionRecordingVoiceNote").on);
        assert!(typing("chatActionUploadingPhoto").on);
        assert!(!typing("chatActionCancel").on, "the cancel is the one that stops");

        // The chat acting as itself — an anonymous admin, a channel — is
        // nobody in particular.
        let anon = chat_action(&json!({
            "@type": "updateChatAction", "chat_id": -1005,
            "sender_id": {"@type": "messageSenderChat", "chat_id": -1005},
            "action": {"@type": "chatActionTyping"},
        }))
        .expect("an action");
        assert_eq!(anon.who, None);
        assert!(chat_action(&json!({"@type": "updateChatAction", "chat_id": -1005})).is_none());
        assert!(chat_action(&json!({"@type": "updateChatAction", "action": {"@type": "chatActionTyping"}})).is_none());
    }

    /// The one-field updates a chat sends after its object: a new title, the
    /// mention badge, and a draft another device typed — the null draft
    /// being a draft cleared, not a draft unknown.
    #[test]
    fn a_chats_own_updates_carry_one_field_each() {
        assert_eq!(
            chat_title(&json!({"@type": "updateChatTitle", "chat_id": -1005, "title": "Quokka Weekly"})),
            Some((-1005, "Quokka Weekly".to_string()))
        );
        assert!(chat_title(&json!({"@type": "updateChatTitle", "chat_id": -1005, "title": ""})).is_none());
        assert!(chat_title(&json!({"@type": "updateChatTitle", "title": "x"})).is_none());

        assert_eq!(
            chat_mentions(&json!({"@type": "updateChatUnreadMentionCount", "chat_id": 2, "unread_mention_count": 3})),
            Some((2, 3))
        );
        assert_eq!(
            chat_mentions(&json!({"@type": "updateChatUnreadMentionCount", "chat_id": 2, "unread_mention_count": 0})),
            Some((2, 0))
        );
        assert!(chat_mentions(&json!({"@type": "updateChatUnreadMentionCount"})).is_none());

        let typed = json!({"@type": "updateChatDraftMessage", "chat_id": 2, "draft_message": {
            "@type": "draftMessage",
            "input_message_text": {"@type": "inputMessageText", "text": {"text": "on my way"}},
        }});
        assert_eq!(chat_draft(&typed), Some((2, Some("on my way".to_string()))));
        let cleared = json!({"@type": "updateChatDraftMessage", "chat_id": 2, "draft_message": null});
        assert_eq!(chat_draft(&cleared), Some((2, None)), "a null draft clears it");
        assert!(chat_draft(&json!({"@type": "updateChatDraftMessage"})).is_none());
    }

    /// Decode counts without confusing nullable metadata with an empty list.
    #[test]
    fn interaction_counts_distinguish_unknown_metadata_from_empty_lists() {
        assert_eq!(reaction_counts(&json!(null)), None);
        assert_eq!(reaction_counts(&json!({"reactions": null})), None);
        assert_eq!(reaction_counts(&json!({"reactions": {"reactions": []}})), Some(None));
        assert_eq!(reaction_counts(&json!({"reactions": {"reactions": [
            {"type": {"@type": "reactionTypeFuture"}, "total_count": 4},
        ]}})), None);
        assert_eq!(reaction_counts(&json!({"reactions": {"reactions": [
            {"type": {"@type": "reactionTypeEmoji"}, "total_count": 4},
        ]}})), None);
        let i = interaction(&json!({
            "@type": "updateMessageInteractionInfo", "chat_id": -1005, "message_id": 4200,
            "interaction_info": {
                "view_count": 1_204,
                "reply_info": {"reply_count": 7},
                "reactions": [
                    {"reaction": "👍", "total_count": 3},
                    {"type": {"@type": "reactionTypeEmoji", "emoji": "❤️"}, "total_count": 1},
                ],
            },
        }))
        .expect("the info");
        assert_eq!((i.chat, i.id), (-1005, 4200));
        assert_eq!((i.views, i.comments), (Some(1_204), Some(7)));
        assert_eq!(i.reactions.as_deref(), Some("👍 3 · ❤️ 1"));

        let none = interaction(&json!({
            "@type": "updateMessageInteractionInfo", "chat_id": -1005, "message_id": 4200,
            "interaction_info": null,
        }))
        .expect("the info");
        assert_eq!((none.views, none.comments, none.reactions), (None, None, None));
        assert!(interaction(&json!({"@type": "updateMessageInteractionInfo", "chat_id": -1005})).is_none());
    }

    #[test]
    fn reaction_counts_include_paid_and_custom_emoji_instead_of_dropping_them() {
        let info = json!({"reactions": {"reactions": [
            {"type": {"@type": "reactionTypeEmoji", "emoji": "👍"}, "total_count": 12},
            {"type": {"@type": "reactionTypePaid"}, "total_count": 30},
            {"type": {"@type": "reactionTypeCustomEmoji", "custom_emoji_id": "123"}, "total_count": 4},
        ]}});
        assert_eq!(reactions_line(&info).as_deref(), Some("👍 12 · ⭐ 30 · custom emoji 4"));
    }
}
