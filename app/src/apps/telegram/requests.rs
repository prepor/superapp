//! Pure TDLib request builders and response-correlation metadata.
//!
//! Panels depend on this module rather than the worker implementation.
//! Login secrets are serialized only for the in-memory command queue.

#![cfg_attr(not(feature = "tdlib"), allow(dead_code))]

use std::path::Path;
use kernel::codec::jpeg;
use kernel::caps::{Fix, VideoNote, VoiceNote};
use serde_json::{json, Value};
use super::model::{self, MsgId, MsgKey, PeerId, Scope};

// -- the requests --------------------------------------------------------------
//
// Small builders, one per request, so the JSON in the type language lives in
// one greppable place rather than inline in the state machine.

/// The one-time handshake. The message, chat and file databases are **off**
/// on purpose: we project what we want into `tg_*` ourselves, so there is one
/// durable view of a chat, not two (see the CR). The binlog under
/// `database_directory` is the only thing TDLib keeps, and it is local.
pub(super) fn set_tdlib_parameters(api_id: i32, api_hash: &str, dir: &Path) -> String {
    json!({
        "@type": "setTdlibParameters",
        "@extra": "tdlib_parameters",
        "database_directory": dir.to_string_lossy().into_owned(),
        "use_message_database": false,
        "use_chat_info_database": false,
        "use_file_database": false,
        "use_secret_chats": false,
        "use_test_dc": false,
        "api_id": api_id,
        "api_hash": api_hash,
        "system_language_code": "en",
        "device_model": "superapp",
        "application_version": env!("CARGO_PKG_VERSION"),
        "database_encryption_key": "",
    })
    .to_string()
}

/// The phone number the account signs in with. `pub` so the sign-in panel
/// sends the number the account holder typed through the same builder the
/// state machine uses, rather than spelling the JSON a second time.
#[must_use]
pub fn set_authentication_phone(phone: &str) -> String {
    json!({ "@type": "setAuthenticationPhoneNumber", "phone_number": phone }).to_string()
}

/// The login code, from the user through the sign-in panel — never auto-sent
/// from an update. `pub` for the panel, for the reason above.
#[must_use]
pub fn check_authentication_code(code: &str) -> String {
    json!({ "@type": "checkAuthenticationCode", "code": code }).to_string()
}

/// The two-factor password, from the user through the sign-in panel. `pub`
/// for the panel, for the reason above.
#[must_use]
pub fn check_authentication_password(password: &str) -> String {
    json!({ "@type": "checkAuthenticationPassword", "password": password }).to_string()
}

/// The main chat list, a window at a time — the seam `on_ready` opens.
/// The two lists a chat can sit in, as `loadChats` names them.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChatList {
    Main,
    Archive,
}

impl ChatList {
    fn td_type(self) -> &'static str {
        match self {
            ChatList::Main => "chatListMain",
            ChatList::Archive => "chatListArchive",
        }
    }

    pub(super) fn word(self) -> &'static str {
        match self {
            ChatList::Main => "main",
            ChatList::Archive => "archive",
        }
    }
}

/// Ask TDLib for the next page of a list's chats: each arrives as its own
/// `updateNewChat` and `updateChatPosition`, and the request answers `ok`
/// while there may be more, an error 404 once the list is complete. The
/// `@extra` names the list so `Account::on_reply` knows which it is.
pub(super) fn load_chats(list: ChatList) -> String {
    json!({
        "@type": "loadChats",
        "chat_list": { "@type": list.td_type() },
        "limit": 200,
        "@extra": format!("load_chats:{}", list.word()),
    })
    .to_string()
}

// -- reactions ----------------------------------------------------------------

pub(super) fn get_message_added_reactions(chat: PeerId, msg: MsgId, offset: &str) -> String {
    json!({"@type": "getMessageAddedReactions", "chat_id": chat, "message_id": msg,
        "reaction_type": null, "offset": offset, "limit": 100}).to_string()
}

pub(super) fn search_mention_members(chat: PeerId, scope: Scope, query: &str) -> String {
    let topic = match scope {
        Scope::Whole => None,
        Scope::Topic(id) => Some(json!({"@type": "messageTopicForum", "forum_topic_id": id})),
        Scope::Thread(root) => Some(json!({"@type": "messageTopicThread", "message_thread_id": root})),
    };
    json!({"@type": "searchChatMembers", "chat_id": chat, "query": query, "limit": 50,
        "filter": {"@type": "chatMembersFilterMention", "topic_id": topic}}).to_string()
}

/// Available reactions for this particular message, in Telegram's preferred
/// order. The correlation id belongs to the panel's in-memory picker.
#[must_use]
pub fn get_message_available_reactions(chat: PeerId, msg: MsgId, request: u64) -> String {
    json!({
        "@type": "getMessageAvailableReactions",
        "chat_id": chat,
        "message_id": msg,
        "row_size": 6,
        "@extra": format!("reactions:{request}"),
    })
    .to_string()
}

pub(super) fn refresh_available_reactions(chat: PeerId, msg: MsgId, request: u64, attempt: u64) -> String {
    let mut v: serde_json::Value = serde_json::from_str(&get_message_available_reactions(chat, msg, request)).expect("reaction request");
    v["@extra"] = json!(format!("reaction_choices:{request}:{attempt}"));
    v.to_string()
}

pub(super) fn parse_reaction_choices_extra(extra: &str) -> Option<(u64, u64)> {
    if let Some(id) = extra.strip_prefix("reactions:") {
        return Some((id.parse().ok()?, 0));
    }
    let (id, attempt) = extra.strip_prefix("reaction_choices:")?.split_once(':')?;
    Some((id.parse().ok()?, attempt.parse().ok()?))
}

/// Add one ordinary emoji. The interaction-info update supplies Telegram's
/// resulting counts.
#[must_use]
pub fn add_message_reaction(chat: PeerId, msg: MsgId, emoji: &str, request: u64) -> String {
    json!({
        "@type": "addMessageReaction",
        "chat_id": chat,
        "message_id": msg,
        "reaction_type": {"@type": "reactionTypeEmoji", "emoji": emoji},
        "is_big": false,
        "update_recent_reactions": true,
        "@extra": format!("reaction:{request}:{chat}:{msg}"),
    })
    .to_string()
}

pub(super) fn parse_reaction_extra(extra: &str) -> Option<u64> {
    extra
        .strip_prefix("reactions:")
        .and_then(|id| id.parse().ok())
        .or_else(|| parse_added_reaction_extra(extra).map(|(id, _, _)| id))
}

pub(super) fn parse_added_reaction_extra(extra: &str) -> Option<(u64, PeerId, MsgId)> {
    let mut parts = extra.strip_prefix("reaction:")?.split(':');
    let result = (parts.next()?.parse().ok()?, parts.next()?.parse().ok()?, parts.next()?.parse().ok()?);
    parts.next().is_none().then_some(result)
}

// -- the content verbs, live ---------------------------------------------------
//
// The phase-4 requests: the composer's send, reply and edit, and a line's
// delete and read. TDLib echoes each back as an update — a sent line as
// `updateNewMessage`, an edit as `updateMessageContent`, a delete as
// `updateDeleteMessages` — which the projection lays into `tg_*`, so the
// transcript reflects the wire through the one path it already draws, with no
// optimistic local write of our own.

/// The `inputMessageText` a send or an edit carries: the text as a
/// `formattedText` with no entities of ours. TDLib detects username mentions
/// and URLs; the composer does not apply Markdown formatting. Shared so a
/// send and an edit spell the content the same way.
/// `clear_draft` is what a send says and an edit does not: the draft the
/// server holds for the chat is the line being sent, and sending it is what
/// ends it — on every device (review, 2026-09-07).
pub(super) fn input_text(text: &str, clear_draft: bool) -> Value {
    json!({
        "@type": "inputMessageText",
        "text": { "@type": "formattedText", "text": text },
        "clear_draft": clear_draft,
    })
}

/// A text line to a chat, optionally answering one of its messages. The reply
/// is the modern *input* form — `inputMessageReplyToMessage` by message id, the
/// `InputMessageReplyTo` that `sendMessage` takes — not the received-message
/// `messageReplyToMessage`, which is a different, output-only type carrying its
/// own `chat_id`. Left off entirely when nothing is replied to.
#[must_use]
pub fn send_message(chat_id: PeerId, text: &str, reply_to: Option<MsgId>) -> String {
    let mut req = json!({
        "@type": "sendMessage",
        "chat_id": chat_id,
        "input_message_content": input_text(text, true),
    });
    if let Some(id) = reply_to {
        req["reply_to"] = json!({
            "@type": "inputMessageReplyToMessage",
            "message_id": id,
        });
    }
    req.to_string()
}

/// An old basic-group message retains its source identity when answered from
/// the upgraded group's composer (TDLib's inputMessageReplyToExternalMessage).
pub(super) fn reply_in_chat(request: String, chat: PeerId, reply: Option<model::MsgKey>) -> String {
    let Some((source, id)) = reply.filter(|(source, _)| *source != chat) else { return request };
    let mut v: Value = serde_json::from_str(&request).expect("Telegram request");
    if v["reply_to"]["message_id"] == id {
        v["reply_to"]["@type"] = json!("inputMessageReplyToExternalMessage");
        v["reply_to"]["chat_id"] = json!(source);
    }
    v.to_string()
}

/// The line a send answers, put on a request the way [`send_message`] puts it:
/// the *input* form, by message id, absent entirely when nothing is answered.
/// Shared by the sends that carry something other than text, so all of them
/// spell a reply the one way.
pub(super) fn with_reply(req: &mut Value, reply_to: Option<MsgId>) {
    if let Some(id) = reply_to {
        req["reply_to"] = json!({
            "@type": "inputMessageReplyToMessage",
            "message_id": id,
        });
    }
}

/// One carried file to a chat, as a message of its own: what
/// [`Carried::kind`](model::Carried::kind) says it is — a picture as a photo,
/// a moving one as a video, a sound as audio, anything else as a document —
/// wrapped in an `inputFileLocal`, which is TDLib's *the bytes are at this
/// path on this machine*; the upload is the engine's.
///
/// The words in the composer ride as the `caption`, so a picture with
/// something written under it is one message and not two, the way the client
/// sends it. That makes the caption the *first* file's alone: the rest go
/// bare, an empty caption being an empty `formattedText` rather than no field
/// — TDLib takes the content object whole.
#[must_use]
pub fn send_file(
    chat_id: PeerId,
    reply_to: Option<MsgId>,
    file: &model::Carried,
    caption: &str,
) -> String {
    let mut req = json!({
        "@type": "sendMessage",
        "chat_id": chat_id,
        "input_message_content": file_content(file, caption),
    });
    with_reply(&mut req, reply_to);
    req.to_string()
}

/// What one carried file goes as, as an `InputMessageContent`: what
/// [`Carried::kind`](model::Carried::kind) says it is, wrapped the way
/// TDLib wants it wrapped. Shared by the message a single file is and the
/// album a strip of pictures is.
fn file_content(file: &model::Carried, caption: &str) -> Value {
    // The kind's `@type`, and the field it names its file by: each input
    // content spells its own, `photo` for a photo and so on down.
    let (kind, media, names_it) = match file.kind() {
        "photo" => ("inputMessagePhoto", "inputPhoto", "photo"),
        "animation" => ("inputMessageAnimation", "inputAnimation", "animation"),
        "video" => ("inputMessageVideo", "inputVideo", "video"),
        "audio" => ("inputMessageAudio", "inputAudio", "audio"),
        _ => ("inputMessageDocument", "inputDocument", "document"),
    };
    let mut content = json!({
        "@type": kind,
        "caption": { "@type": "formattedText", "text": caption },
    });
    // The files app spells a path the way it shows it — `~/Downloads/x` —
    // and the engine reads a path as the disk has it (review, 2026-09-07).
    // Current TDLib takes an inputPhoto/inputVideo/etc, containing the
    // InputFile. Passing InputFile directly is accepted by the JSON parser
    // but loses the file and fails with "InputFile is not specified".
    content[names_it] = json!({ "@type": media });
    content[names_it][names_it] = local_file(&file.path);
    content
}

/// The most pictures one album carries. The clients' ten.
pub const ALBUM_MAX: usize = 10;

/// How a carried list leaves: each group is one message — the photos and
/// the videos in albums of ten, everything else on its own — in the order
/// they were carried, each group standing where its first file stood.
///
/// The phone sends a strip of shots as an album, which is the point of the
/// camera putting them on the list rather than sending each at the shutter;
/// a document or a sound has no album to be in, and a lone picture is a
/// picture, not an album of one. More pictures than one album holds is more
/// albums, as the clients split them — never one album and a trail of
/// single pictures behind it.
#[must_use]
pub fn parcels(files: &[model::Carried]) -> Vec<Vec<model::Carried>> {
    let together = |c: &model::Carried| matches!(c.kind(), "photo" | "video");
    let pictures: Vec<usize> = files
        .iter()
        .enumerate()
        .filter(|(_, c)| together(c))
        .map(|(i, _)| i)
        .collect();
    // A chunk of one is no album: the eleventh picture goes as a picture.
    let albums: Vec<&[usize]> = pictures.chunks(ALBUM_MAX).filter(|c| c.len() > 1).collect();
    let mut out: Vec<Vec<model::Carried>> = Vec::new();
    for (i, file) in files.iter().enumerate() {
        match albums.iter().find(|album| album.contains(&i)) {
            Some(album) if album[0] == i => {
                out.push(album.iter().map(|&i| files[i].clone()).collect());
            }
            // The rest of an album has already gone with its first picture.
            Some(_) => {}
            None => out.push(vec![file.clone()]),
        }
    }
    out
}

/// A strip of pictures as one message — `sendMessageAlbum`, what the phone
/// sends a camera roll with. The caption rides on the first of them, as a
/// caption under an album does; the reply is the message's, not each
/// picture's.
#[must_use]
pub fn send_album(
    chat_id: PeerId,
    reply_to: Option<MsgId>,
    files: &[model::Carried],
    caption: &str,
) -> String {
    let contents: Vec<Value> = files
        .iter()
        .enumerate()
        .map(|(i, file)| file_content(file, if i == 0 { caption } else { "" }))
        .collect();
    let mut req = json!({
        "@type": "sendMessageAlbum",
        "chat_id": chat_id,
        "input_message_contents": contents,
    });
    with_reply(&mut req, reply_to);
    req.to_string()
}

/// A voice note the microphone made: the Ogg Opus by its path, how long it
/// runs, and the hundred bars the clients draw under it — `bytes` on the
/// wire, which TDLib's JSON spells in base64.
///
/// It goes on its own: no caption and nothing else with it, the way a held
/// button sends one.
#[must_use]
pub fn send_voice_note(chat_id: PeerId, reply_to: Option<MsgId>, note: &VoiceNote) -> String {
    use base64::Engine as _;
    let mut req = json!({
        "@type": "sendMessage",
        "chat_id": chat_id,
        "input_message_content": {
            "@type": "inputMessageVoiceNote",
            "voice_note": {
                "@type": "inputVoiceNote",
                "voice_note": local_file_at(&note.path),
                "duration": note.secs.round() as i64,
                "waveform": base64::engine::general_purpose::STANDARD.encode(&note.waveform),
            },
            "caption": { "@type": "formattedText", "text": "" },
            "self_destruct_type": null,
        },
    });
    with_reply(&mut req, reply_to);
    req.to_string()
}

/// A video message: the square mp4, its side as the wire's `length`, how
/// long it runs, and the first frame as the thumbnail a row draws before
/// the clip is downloaded.
#[must_use]
pub fn send_video_note(chat_id: PeerId, reply_to: Option<MsgId>, note: &VideoNote) -> String {
    let mut req = json!({
        "@type": "sendMessage",
        "chat_id": chat_id,
        "input_message_content": {
            "@type": "inputMessageVideoNote",
            "video_note": {
                "@type": "inputVideoNote",
                "video_note": local_file_at(&note.path),
                "thumbnail": {
                    "@type": "inputThumbnail",
                    "thumbnail": local_file_at(&note.thumbnail),
                    "width": jpeg::THUMBNAIL,
                    "height": jpeg::THUMBNAIL,
                },
                "duration": note.secs.round() as i64,
                "length": note.side,
            },
            "self_destruct_type": null,
        },
    });
    with_reply(&mut req, reply_to);
    req.to_string()
}

/// *The bytes are at this path on this machine*, as the files app spells a
/// path; the upload is the engine's.
fn local_file(path: &str) -> Value {
    json!({
        "@type": "inputFileLocal",
        "path": kernel::caps::real_path(path).to_string_lossy(),
    })
}

/// The same for a capture, whose path is the disk's already — it was
/// written there by the microphone or the camera a moment ago.
fn local_file_at(path: &Path) -> Value {
    json!({
        "@type": "inputFileLocal",
        "path": path.to_string_lossy(),
    })
}

/// Where the device says I am, to a chat, once: `inputMessageLocation`, whose
/// whole content is the point and how far off the reading may be. A place
/// that goes on moving is a different content — [`send_live_location`].
#[must_use]
pub fn send_location(chat_id: PeerId, reply_to: Option<MsgId>, fix: &Fix) -> String {
    let mut req = json!({
        "@type": "sendMessage",
        "chat_id": chat_id,
        "input_message_content": {
            "@type": "inputMessageLocation",
            "location": point(fix),
        },
    });
    with_reply(&mut req, reply_to);
    req.to_string()
}

/// The periods a live share runs for, as the phone's sheet offers them, and
/// the label each wears on the bar. The last is the wire's *until stopped*.
pub const LIVE_PERIODS: [(i64, &str); 4] = [
    (15 * 60, "15 min"),
    (60 * 60, "1 h"),
    (8 * 60 * 60, "8 h"),
    (LIVE_FOREVER, "until stopped"),
];

/// *Until stopped*, as the wire spells it: the largest int32 there is.
pub const LIVE_FOREVER: i64 = 0x7FFF_FFFF;

/// A place that keeps moving for `period` seconds:
/// `inputMessageLiveLocation`, whose content is a `liveLocation` — the
/// point, the period, and the heading while the device is going somewhere.
///
/// No reply: a live share is opened from the attach panel, which the
/// composer's reply line does not reach. No proximity alert either — the
/// wire carries one and this client asks for none.
#[must_use]
pub fn send_live_location(chat_id: PeerId, fix: &Fix, period: i64) -> String {
    json!({
        "@type": "sendMessage",
        "chat_id": chat_id,
        "input_message_content": {
            "@type": "inputMessageLiveLocation",
            "location": live(fix, period),
        },
    })
    .to_string()
}

/// A live share moved, or stopped. `fix` absent is the stop: the clients
/// end a share by editing the location away, never by deleting the line, so
/// the message stays in the chat as the place it last was.
#[must_use]
pub fn edit_live_location(chat_id: PeerId, message_id: MsgId, fix: Option<&Fix>) -> String {
    json!({
        "@type": "editMessageLiveLocation",
        "chat_id": chat_id,
        "message_id": message_id,
        "reply_markup": null,
        // The period is the message's own and is not moved by an edit; the
        // wire still wants the whole `liveLocation`, so it is sent back
        // unchanged at nought, which TDLib reads as *leave it alone*.
        "location": fix.map(|fix| live(fix, 0)),
    })
    .to_string()
}

/// One reading as the wire's `location`: the point, and how far off it may
/// be. Nought accuracy is the wire's *not measured*.
fn point(fix: &Fix) -> Value {
    json!({
        "@type": "location",
        "latitude": fix.lat,
        "longitude": fix.lon,
        "horizontal_accuracy": fix.accuracy_m,
    })
}

/// The same reading as a `liveLocation`: with the period it runs for and the
/// heading, where the device is moving.
fn live(fix: &Fix, period: i64) -> Value {
    json!({
        "@type": "liveLocation",
        "location": point(fix),
        "live_period": period,
        "heading": heading(fix),
        "proximity_alert_radius": 0,
    })
}

/// A course over ground as the wire wants it: whole degrees from 1 to 360,
/// nought for *not known*. Due north is 360 rather than 0, which is the one
/// direction the wire cannot spell the obvious way.
fn heading(fix: &Fix) -> i64 {
    match fix.heading_deg {
        Some(deg) if deg.is_finite() => match deg.rem_euclid(360.0).round() as i64 {
            0 => 360,
            d => d,
        },
        _ => 0,
    }
}

/// Lines out of one chat and into another — the pick the client's forward
/// sheet makes. `send_copy` false is the forward proper: each line arrives
/// wearing the *forwarded from* the transcript already draws, rather than as
/// a fresh line of mine, and so `remove_caption` has nothing to remove.
/// `options` null takes the account's own defaults for the send.
///
/// Nothing local is written for it: every copy comes back as its own
/// `updateNewMessage`, the way a send's echo does.
#[must_use]
pub fn forward_messages(chat_id: PeerId, from_chat_id: PeerId, message_ids: &[MsgId]) -> String {
    json!({
        "@type": "forwardMessages",
        "chat_id": chat_id,
        "message_thread_id": 0,
        "from_chat_id": from_chat_id,
        "message_ids": message_ids,
        "options": null,
        "send_copy": false,
        "remove_caption": false,
    })
    .to_string()
}

/// New text over one of my lines — the same `inputMessageText` a send carries.
/// TDLib answers with an `updateMessageContent` the projection rewrites in
/// place, so no local edit is wanted once this is out.
#[must_use]
pub fn edit_message_text(chat_id: PeerId, message_id: MsgId, text: &str) -> String {
    json!({
        "@type": "editMessageText",
        "chat_id": chat_id,
        "message_id": message_id,
        "input_message_content": input_text(text, false),
    })
    .to_string()
}

/// Edits the caption under a line's media — a photo's, a file's — which is
/// its own request: `editMessageText` is for a text line and is refused on
/// a media one (review, 2026-09-07).
#[must_use]
pub fn edit_message_caption(chat_id: PeerId, message_id: MsgId, text: &str) -> String {
    json!({
        "@type": "editMessageCaption",
        "chat_id": chat_id,
        "message_id": message_id,
        "caption": { "@type": "formattedText", "text": text },
    })
    .to_string()
}

/// Deletes lines. `revoke` true is the client's *delete for everyone* — the
/// message goes from every side, not merely my own copy. TDLib answers with an
/// `updateDeleteMessages` the projection strikes the rows on.
#[must_use]
pub fn delete_messages(chat_id: PeerId, message_ids: &[MsgId], revoke: bool) -> String {
    json!({
        "@type": "deleteMessages",
        "chat_id": chat_id,
        "message_ids": message_ids,
        "revoke": revoke,
    })
    .to_string()
}

/// Marks the named lines seen, including their reply or mention notifications.
/// `force_read` also advances the ordinary inbox's read position; callers
/// reading a chat without viewing mentions name its newest ordinary line.
#[must_use]
pub fn view_messages(chat_id: PeerId, message_ids: &[MsgId]) -> String {
    json!({
        "@type": "viewMessages",
        "chat_id": chat_id,
        "message_ids": message_ids,
        "force_read": true,
    })
    .to_string()
}

/// An open chat receives live interaction updates. Balanced by closeChat
/// when its last message widget goes away.
pub fn chat_open(chat_id: PeerId, open: bool) -> String {
    json!({"@type": if open { "openChat" } else { "closeChat" }, "chat_id": chat_id}).to_string()
}

/// Load visible rows into TDLib as well as our durable projection before
/// asking it to keep their reactions fresh.
pub fn get_visible_messages(chat_id: PeerId, message_ids: &[MsgId], request: u64) -> String {
    json!({
        "@type": "getMessages", "chat_id": chat_id, "message_ids": message_ids,
        "@extra": format!("visible:{chat_id}:{request}"),
    }).to_string()
}

pub(super) fn parse_visible_extra(extra: &str) -> Option<(PeerId, u64)> {
    let (chat, request) = extra.strip_prefix("visible:")?.split_once(':')?;
    Some((chat.parse().ok()?, request.parse().ok()?))
}

/// TDLib only schedules ongoing reaction polling while this client is online.
pub(super) fn set_online(online: bool) -> String {
    json!({"@type": "setOption", "name": "online",
        "value": {"@type": "optionValueBoolean", "value": online}}).to_string()
}

/// Subscribe to counts without changing the existing read-cursor behavior.
pub fn observe_messages(chat_id: PeerId, message_ids: &[MsgId]) -> String {
    json!({
        "@type": "viewMessages", "chat_id": chat_id, "message_ids": message_ids,
        "source": {"@type": "messageSourceOther"}, "force_read": false,
    }).to_string()
}

// -- the verbs about a chat ----------------------------------------------------
//
// Mute, pin, archive and leave: what the peer's card and the list's batch do.
// Each is answered by an update of its own — `updateChatNotificationSettings`,
// `updateChatPosition`, `updateChatLastMessage` for a group one has left — so
// the store converges on the engine's word; the panel's own flip is only what
// keeps the bar honest between the press and that answer.

/// How long a muted chat stays muted: the far end of an int32, which is what
/// every Telegram client means by *forever*.
const MUTE_FOREVER: i64 = 2_147_483_647;

/// Mutes a chat, or lets it speak again. TDLib takes the settings object
/// whole rather than a patch, so every field is spelled: `mute_for` is ours —
/// with `use_default_mute_for` off, a default being exactly what would
/// override it — and the rest say *use the account's own*, which is what they
/// said before we touched them.
#[must_use]
pub fn set_chat_muted(chat_id: PeerId, muted: bool) -> String {
    json!({
        "@type": "setChatNotificationSettings",
        "chat_id": chat_id,
        "notification_settings": {
            "@type": "chatNotificationSettings",
            "use_default_mute_for": false,
            "mute_for": if muted { MUTE_FOREVER } else { 0 },
            "use_default_sound": true,
            "sound_id": 0,
            "use_default_show_preview": true,
            "show_preview": false,
            "use_default_mute_stories": true,
            "mute_stories": false,
            "use_default_story_sound": true,
            "story_sound_id": 0,
            "use_default_show_story_sender": true,
            "show_story_sender": false,
            "use_default_disable_pinned_message_notifications": true,
            "disable_pinned_message_notifications": false,
            "use_default_disable_mention_notifications": true,
            "disable_mention_notifications": false,
        },
    })
    .to_string()
}

/// Pins a chat to the top of the main list, or lets it down. The archive
/// keeps its own pins; this one names the list it means. TDLib answers with
/// the `updateChatPosition` that `Account::on_chat_position`
/// already lays on the row.
#[must_use]
pub fn toggle_chat_pinned(chat_id: PeerId, pinned: bool) -> String {
    json!({
        "@type": "toggleChatIsPinned",
        "chat_list": {"@type": "chatListMain"},
        "chat_id": chat_id,
        "is_pinned": pinned,
    })
    .to_string()
}

/// Archives a chat, or brings it back. One call does both ways: a chat
/// belongs to the list it is added to, and adding it to the main list is what
/// taking it out of the archive means.
#[must_use]
pub fn add_chat_to_list(chat_id: PeerId, archived: bool) -> String {
    json!({
        "@type": "addChatToList",
        "chat_id": chat_id,
        "chat_list": {
            "@type": if archived { "chatListArchive" } else { "chatListMain" },
        },
    })
    .to_string()
}

/// Leaves a group or a channel. Only the membership goes: the conversation
/// stays on Telegram until it is deleted, and the peer stays known, which is
/// why the local half keeps the `tg_peer` row and takes only the chat
/// ([`model::leave_chat_tx`]).
#[must_use]
pub fn leave_chat(chat_id: PeerId) -> String {
    json!({
        "@type": "leaveChat",
        "chat_id": chat_id,
    })
    .to_string()
}

/// Joins a group or a channel the account merely knows of — one a line was
/// forwarded from, one an invite named. Nothing is written locally: joining
/// is the engine's to confirm, and it does, with the `updateChatPosition`
/// and `updateChatAddedToList` that put the chat in my list
/// (`Account::on_chat_listing`) — where *leave* has to
/// flip at once because what follows it is an absence.
#[must_use]
pub fn join_chat(chat_id: PeerId) -> String {
    json!({
        "@type": "joinChat",
        "chat_id": chat_id,
    })
    .to_string()
}

/// Deletes my side of a conversation — the messages, and the chat from my
/// list — and nothing of the other person's. `deleteChatHistory` with
/// `revoke` false is the request that means that; TDLib's `deleteChat`
/// deletes for every member wherever it may, which no *delete chat* on a
/// card should quietly do (review, 2026-09-07).
#[must_use]
pub fn delete_chat(chat_id: PeerId) -> String {
    delete_chat_history(chat_id, true)
}

/// Profile actions whose local result must wait for Telegram's acknowledgement.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PeerAction {
    Block,
    Unblock,
    DeleteContact,
    DeleteChat,
}

impl PeerAction {
    pub fn word(self) -> &'static str {
        match self {
            Self::Block => "block user",
            Self::Unblock => "unblock user",
            Self::DeleteContact => "delete contact",
            Self::DeleteChat => "delete chat",
        }
    }

    pub fn request(self, peer: PeerId, id: u64) -> String {
        let mut request = match self {
            Self::Block | Self::Unblock => json!({
                "@type": "setMessageSenderBlockList",
                "sender_id": { "@type": "messageSenderUser", "user_id": peer },
                "block_list": if self == Self::Block { json!({ "@type": "blockListMain" }) } else { Value::Null },
            }),
            Self::DeleteContact => json!({ "@type": "removeContacts", "user_ids": [peer] }),
            Self::DeleteChat => serde_json::from_str(&delete_chat(peer)).expect("delete chat JSON"),
        };
        request["@extra"] = json!(format!("peer_action:{}:{peer}:{id}", self.word()));
        request.to_string()
    }

    pub fn from_reply(reply: &Value) -> Option<(Self, PeerId, u64)> {
        let (word, rest) = reply["@extra"].as_str()?.strip_prefix("peer_action:")?.split_once(':')?;
        let (peer, id) = rest.split_once(':')?;
        let action = match word {
            "block user" => Self::Block,
            "unblock user" => Self::Unblock,
            "delete contact" => Self::DeleteContact,
            "delete chat" => Self::DeleteChat,
            _ => return None,
        };
        Some((action, peer.parse().ok()?, id.parse().ok()?))
    }
}

/// A contact without a conversation still needs its current block state.
pub fn get_user_full_info(peer: PeerId) -> String {
    json!({
        "@type": "getUserFullInfo", "user_id": peer,
        "@extra": format!("user_full_info:{peer}"),
    }).to_string()
}

/// Clears my side of a conversation's history and keeps the chat in the
/// list, as the client's *clear history* does.
#[must_use]
pub fn clear_history(chat_id: PeerId) -> String {
    delete_chat_history(chat_id, false)
}

pub(super) fn delete_chat_history(chat_id: PeerId, remove_from_list: bool) -> String {
    json!({
        "@type": "deleteChatHistory",
        "chat_id": chat_id,
        "remove_from_chat_list": remove_from_list,
        "revoke": false,
    })
    .to_string()
}

/// The draft, sent to the server as a chat is left, so the half-written line
/// is on the phone too — which is the other half of
/// `Account::on_chat_draft`, one client's composer being
/// every client's.
///
/// An empty text sends `draft_message: null`, which is how the type language
/// spells *there is no draft*: clearing one and never having had one are the
/// same request. [`in_topic`] supplies a forum destination when this draft
/// belongs to a topic. The server dates the draft as it arrives.
#[must_use]
pub fn set_chat_draft(chat_id: PeerId, text: Option<&str>) -> String {
    let draft = match text.map(str::trim).filter(|t| !t.is_empty()) {
        Some(t) => json!({
            "@type": "draftMessage",
            "reply_to": null,
            "date": 0,
            "input_message_text": input_text(t, false),
        }),
        None => Value::Null,
    };
    json!({
        "@type": "setChatDraftMessage",
        "chat_id": chat_id,
        "message_thread_id": 0,
        "draft_message": draft,
    })
    .to_string()
}

/// Route a composer command to a part of a chat — a forum topic, or the
/// comments under a post. Message ids still belong to the chat itself; the
/// part is a separate destination in TDLib's API, a `MessageTopic` of its
/// own kind, and a read receipt names it as a *source* rather than a
/// destination.
pub fn in_scope(request: String, scope: Scope) -> String {
    let topic = match scope {
        Scope::Whole => return request,
        Scope::Topic(id) => json!({"@type": "messageTopicForum", "forum_topic_id": id}),
        Scope::Thread(root) => json!({"@type": "messageTopicThread", "message_thread_id": root}),
    };
    let mut req: Value = serde_json::from_str(&request).expect("a request builder's JSON");
    req["topic_id"] = topic;
    req.as_object_mut().unwrap().remove("message_thread_id");
    if req["@type"] == "viewMessages" {
        req.as_object_mut().unwrap().remove("topic_id");
        req["source"] = json!({"@type": match scope {
            Scope::Thread(_) => "messageSourceMessageThreadHistory",
            _ => "messageSourceForumTopicHistory",
        }});
    }
    req.to_string()
}

pub fn get_forum_topics(chat: PeerId, date: i64, message: MsgId, topic: i64) -> String {
    json!({"@type": "getForumTopics", "chat_id": chat, "query": "",
        "offset_date": date, "offset_message_id": message, "offset_forum_topic_id": topic,
        "limit": 100, "@extra": format!("topics:{chat}:{date}:{message}:{topic}")}).to_string()
}

pub fn get_forum_topic(chat: PeerId, topic: i64) -> String {
    json!({"@type": "getForumTopic", "chat_id": chat, "forum_topic_id": topic,
        "@extra": format!("topic:{chat}:{topic}")}).to_string()
}

pub fn set_topic_muted(chat: PeerId, topic: i64, muted: bool) -> String {
    let mut request: Value = serde_json::from_str(&set_chat_muted(chat, muted)).unwrap();
    request["@type"] = json!("setForumTopicNotificationSettings");
    request["forum_topic_id"] = json!(topic);
    request.to_string()
}

/// One page of a part of a chat: the chat itself, one forum topic, or one
/// post's comments. A thread's page is asked for by a message *in* it — the
/// root will do — and comes back in the same `messages` answer as the rest.
pub fn get_history_in(chat: PeerId, scope: Scope, from: MsgId, walk: Walk) -> String {
    match scope {
        Scope::Whole => get_chat_history(chat, from, walk),
        Scope::Topic(topic) => json!({"@type": "getForumTopicHistory", "chat_id": chat,
            "forum_topic_id": topic, "from_message_id": from, "offset": 0, "limit": HISTORY_PAGE,
            "@extra": history_extra_in(chat, scope, from, walk)}).to_string(),
        Scope::Thread(root) => json!({"@type": "getMessageThreadHistory", "chat_id": chat,
            "message_id": root, "from_message_id": from, "offset": 0, "limit": HISTORY_PAGE,
            "@extra": history_extra_in(chat, scope, from, walk)}).to_string(),
    }
}

pub(super) fn history_extra_in(chat: PeerId, scope: Scope, from: MsgId, walk: Walk) -> String {
    match scope {
        Scope::Whole => format!("history:{chat}:{}:{from}", walk.word()),
        Scope::Topic(topic) => format!("topic_history:{chat}:{topic}:{}:{from}", walk.word()),
        Scope::Thread(root) => format!("thread_history:{chat}:{root}:{}:{from}", walk.word()),
    }
}

pub(super) fn parse_history_in(extra: &str) -> Option<(PeerId, Scope, Walk, MsgId)> {
    if let Some((chat, walk, from)) = parse_history_extra(extra) {
        return Some((chat, Scope::Whole, walk, from));
    }
    let mut parts = extra.split(':');
    let kind = parts.next()?;
    if kind != "topic_history" && kind != "thread_history" { return None; }
    let chat = parts.next()?.parse().ok()?;
    let part: i64 = parts.next()?.parse().ok()?;
    let scope = if kind == "thread_history" { Scope::of_thread(part) } else { Scope::of_topic(part) };
    Some((chat, scope, Walk::parse(parts.next()?)?, parts.next()?.parse().ok()?))
}

/// Where one post's comments are. The answer — a `messageThreadInfo` — names
/// the discussion group and the thread's root but not the post it was asked
/// about, so the post rides the `@extra` and comes back with it.
///
/// The moment of the ask rides with it too. The answer is a snapshot taken
/// then, and its draft may carry no date of its own — a thread with none is
/// simply a null — so the ask's own clock is what a draft cleared elsewhere
/// is weighed by against one typed here.
pub fn get_message_thread(chat: PeerId, post: MsgId, at: f64) -> String {
    json!({"@type": "getMessageThread", "chat_id": chat, "message_id": post,
        "@extra": format!("thread:{chat}:{post}:{at}")}).to_string()
}

pub(super) fn parse_thread_extra(extra: &str) -> Option<(MsgKey, f64)> {
    let mut parts = extra.strip_prefix("thread:")?.split(':');
    let chat = parts.next()?.parse().ok()?;
    let post = parts.next()?.parse().ok()?;
    let at = parts.next().and_then(|at| at.parse().ok()).unwrap_or(0.0);
    Some(((chat, post), at))
}

/// Ask TDLib to fetch a file by its session-local id. Not synchronous — the
/// worker learns it finished from an `updateFile` and ingests the bytes into
/// the blob cache; a low `priority` keeps a photo behind whatever the account
/// holder is looking at. The file lands under the same `tg:` key the row
/// already names, so no row changes and the next redraw resolves the picture.
#[must_use]
pub fn download_file(file_id: i32, priority: i32) -> String {
    json!({
        "@type": "downloadFile",
        "file_id": file_id,
        "priority": priority,
        "synchronous": false,
    })
    .to_string()
}

/// Tell TDLib to forget a file's local copy — the bytes having moved into the
/// blob cache, the one place media lives. Left believing it still has the
/// file, the engine announces it at a path that is gone for every line that
/// names it, and fetches it off the server again the next time it is asked.
/// Forgotten, it is remote-only on the engine's side, and a later
/// `downloadFile` — one the cache did not answer — is a fresh fetch.
#[must_use]
pub fn delete_file(file_id: i32) -> String {
    json!({
        "@type": "deleteFile",
        "file_id": file_id,
    })
    .to_string()
}

/// The most lines one `getChatHistory` asks for — TDLib's own ceiling.
pub const HISTORY_PAGE: i64 = 100;

/// History and unread-search walks, named in the request's `@extra` so the
/// page that answers knows which it belongs to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Walk {
    /// Down from the newest line, while pages bring lines the store lacks.
    Fill,
    /// Down from the oldest line held, until the window is full.
    Tail,
    /// Unread replies and mentions, with a generation to reject stale pages.
    Mentions(u64),
}

impl Walk {
    pub(super) fn word(self) -> String {
        match self {
            Walk::Fill => "fill".to_string(),
            Walk::Tail => "tail".to_string(),
            Walk::Mentions(generation) => format!("mentions-{generation}"),
        }
    }

    fn parse(word: &str) -> Option<Walk> {
        match word {
            "fill" => Some(Walk::Fill),
            "tail" => Some(Walk::Tail),
            _ => word.strip_prefix("mentions-")?.parse().ok().map(Walk::Mentions),
        }
    }
}

/// Ask TDLib for one page of `chat`'s lines older than `from` — `0` for the
/// newest — as the account's `messages` answer. Its `@extra` carries the
/// chat, the walk and `from`, which is how `Account::on_history` knows
/// whose page it reads and whether the page moved at all.
#[must_use]
pub fn get_chat_history(chat: PeerId, from: MsgId, walk: Walk) -> String {
    if matches!(walk, Walk::Mentions(_)) {
        return json!({
            "@type": "searchChatMessages",
            "chat_id": chat,
            "query": "",
            "sender_id": null,
            "from_message_id": from,
            "offset": 0,
            "limit": HISTORY_PAGE,
            "filter": {"@type": "searchMessagesFilterUnreadMention"},
            "@extra": format!("history:{chat}:{}:{from}", walk.word()),
        }).to_string();
    }
    json!({
        "@type": "getChatHistory",
        "chat_id": chat,
        "from_message_id": from,
        "offset": 0,
        "limit": HISTORY_PAGE,
        "only_local": false,
        "@extra": history_extra_in(chat, Scope::Whole, from, walk),
    })
    .to_string()
}

/// Ask TDLib for one line by its ids — the answer is the `message` itself,
/// re-projected whole. The viewer adds its media correlation with
/// [`request_media`] so the refreshed file can be downloaded next.
#[must_use]
pub fn get_message(chat: PeerId, id: MsgId) -> String {
    json!({
        "@type": "getMessage",
        "chat_id": chat,
        "message_id": id,
        "@extra": format!("line:{chat}:{id}"),
    })
    .to_string()
}

/// Makes TDLib's `chat` object for a conversation with this person.
///
/// A private chat is not a thing that exists until somebody asks for it. A
/// person the engine merely *knows* — a contact, a group's member, whoever a
/// line was forwarded from — has a `user` and no dialog, and every method
/// that names a chat answers *Chat not found* on their id until this has
/// been sent. `force` false because the id came off a peer the engine gave
/// us, not out of a username somebody typed.
///
/// Idempotent, and it writes no dialog: the answer is a `chat` with no
/// position, which the projection files as a chat that is not in my main
/// list (see [`IncomingChat::in_main`](super::project::IncomingChat)). So a
/// conversation opened and never written in does not appear in the list.
#[must_use]
pub fn create_private_chat(user: PeerId) -> String {
    json!({
        "@type": "createPrivateChat",
        "user_id": user,
        "force": false,
        "@extra": format!("private_chat:{user}"),
    })
    .to_string()
}

/// A server search starting at this message, with a criterion that includes
/// it. An empty query without any criterion can silently return no results.
/// Our TDLib message database is disabled in set_tdlib_parameters, so even
/// media searches bypass its database cache. This sends no read receipt.
pub(super) fn reaction_count_snapshot(m: &model::Msg, context: &str) -> String {
    let filter = m.content_type.as_deref().filter(|_| m.sender.is_none() && m.topic == 0).and_then(|kind| match kind {
        "messagePhoto" => Some("searchMessagesFilterPhoto"),
        "messageAnimation" => Some("searchMessagesFilterAnimation"),
        "messageVideo" => Some("searchMessagesFilterVideo"),
        "messageVoiceNote" => Some("searchMessagesFilterVoiceNote"),
        "messageVideoNote" => Some("searchMessagesFilterVideoNote"),
        "messageAudio" => Some("searchMessagesFilterAudio"),
        "messageDocument" => Some("searchMessagesFilterDocument"),
        "messagePoll" => Some("searchMessagesFilterPoll"),
        _ => None,
    }).map(|kind| json!({"@type": kind}));
    let sender = m.sender.unwrap_or(m.chat);
    let sender = if sender > 0 { json!({"@type": "messageSenderUser", "user_id": sender}) }
        else { json!({"@type": "messageSenderChat", "chat_id": sender}) };
    let topic = (m.topic != 0).then(|| json!({"@type": "messageTopicForum", "forum_topic_id": m.topic}));
    // A senderless channel post needs text or a content filter: TDLib
    // removes a redundant sender filter for the channel's own identity.
    let query = if filter.is_none() && topic.is_none() && m.sender.is_none() {
        m.text.split(|c: char| !c.is_alphanumeric()).filter(|word| !word.is_empty())
            .max_by_key(|word| word.chars().count()).or_else(|| m.text.split_whitespace().next())
            .unwrap_or("").chars().take(64).collect::<String>()
    } else { String::new() };
    json!({"@type": "searchChatMessages", "chat_id": m.chat, "from_message_id": m.id,
        "query": query, "sender_id": sender, "topic_id": topic,
        "offset": 0, "limit": 1, "filter": filter, "@extra": context}).to_string()
}

pub(super) fn reaction_count_metadata(chat: PeerId, id: MsgId, context: &str) -> String {
    json!({"@type": "getMessageAvailableReactions", "chat_id": chat, "message_id": id,
        "row_size": 8, "@extra": context}).to_string()
}

/// Restore the message's file source in this TDLib session before downloading.
/// A persistent remote file id alone cannot repair an expired file reference.
pub fn request_media(chat: PeerId, id: MsgId, clip: bool) -> String {
    let mut request: Value = serde_json::from_str(&get_message(chat, id)).unwrap();
    request["@extra"] = json!(media_context(chat, id, clip));
    request.to_string()
}

pub fn media_context(chat: PeerId, id: MsgId, clip: bool) -> String {
    format!("media:{chat}:{id}:{clip}")
}

/// The source request is retained for retry, so expired file references are
/// refreshed before another attempt to save an attachment to Downloads.
pub fn save_file(chat: PeerId, id: MsgId) -> String {
    let mut request: Value = serde_json::from_str(&get_message(chat, id)).unwrap();
    request["@extra"] = json!(save_context(chat, id));
    request.to_string()
}

pub fn save_context(chat: PeerId, id: MsgId) -> String {
    format!("save:{chat}:{id}")
}

/// An agent read refreshes the same source, but only fills the media cache.
pub fn cache_file(chat: PeerId, id: MsgId) -> String {
    let mut request: Value = serde_json::from_str(&get_message(chat, id)).unwrap();
    request["@extra"] = json!(format!("cache:{chat}:{id}"));
    request.to_string()
}

pub(super) fn parse_cache_extra(extra: &str) -> Option<(PeerId, MsgId)> {
    let (chat, id) = extra.strip_prefix("cache:")?.split_once(':')?;
    Some((chat.parse().ok()?, id.parse().ok()?))
}

pub(super) fn parse_save_extra(extra: &str) -> Option<(PeerId, MsgId)> {
    let (chat, id) = extra.strip_prefix("save:")?.split_once(':')?;
    Some((chat.parse().ok()?, id.parse().ok()?))
}

/// The person a `createPrivateChat` was for, off the extra it carries.
pub(super) fn parse_private_chat_extra(extra: &str) -> Option<PeerId> {
    extra.strip_prefix("private_chat:")?.parse().ok()
}

pub(super) fn parse_media_extra(extra: &str) -> Option<(PeerId, MsgId, bool)> {
    let mut parts = extra.strip_prefix("media:")?.split(':');
    let result = (parts.next()?.parse().ok()?, parts.next()?.parse().ok()?, parts.next()?.parse().ok()?);
    parts.next().is_none().then_some(result)
}

/// Still sent through the asynchronous JSON transport. Waiting for completion
/// makes TDLib return download failures as well as updateFile byte counts.
pub fn download_media(file_id: i32, context: &str) -> String {
    let mut request: Value = serde_json::from_str(&download_file(file_id, 32)).unwrap();
    request["synchronous"] = json!(true);
    request["@extra"] = json!(context);
    request.to_string()
}

/// The seconds a *Too Many Requests: retry after N* asks for, or `None`
/// for any other message.
pub(super) fn retry_after(message: &str) -> Option<f64> {
    message
        .rsplit_once("retry after ")
        .and_then(|(_, n)| n.trim().parse::<f64>().ok())
}

/// Ask TDLib for a file by its remote id — the persistent name a file
/// carries, kept on the row as `media_rid` for the picture and `clip_rid`
/// for the clip behind it — so it can be downloaded on demand: the answer is
/// a `file` with a session-local id, on which the worker fires `downloadFile`
/// (`Account::on_file_answer`), and the bytes land in the blob cache under
/// the key the row already names. The viewer queues this in the worker's
/// inbox; missing pictures use [`super::runtime::Runtime::want_file`] for
/// deduplication before the worker builds the request.
#[must_use]
pub fn request_file(remote_id: &str) -> String {
    json!({
        "@type": "getRemoteFile",
        "remote_file_id": remote_id,
        // Unknown on purpose: the remote id itself says what the file is —
        // a photo, a video, an animation, a video note — and a wrong type
        // here is a refusal.
        "file_type": null,
        "@extra": format!("file:{remote_id}"),
    })
    .to_string()
}

// -- calls ---------------------------------------------------------------------
//
// Five requests and no more: the wire rings, hands over the key and the
// servers, relays the packets and files the rating. Everything between those
// is the engine's (`super::calls`).

/// What the client says it speaks, in the wire's own shape. The numbers come
/// from the linked engine, so a build that cannot carry a call still offers
/// the layers it would have.
fn protocol(p: &super::calls::Protocol) -> Value {
    json!({
        "@type": "callProtocol",
        "udp_p2p": p.udp_p2p,
        "udp_reflector": p.udp_reflector,
        "min_layer": p.min_layer,
        "max_layer": p.max_layer,
        "library_versions": p.library_versions,
    })
}

/// Ring somebody. The answer is a `callId`, and the call itself arrives as an
/// `updateCall` a moment later — which is what the panel reads, so nothing
/// waits on this reply.
#[must_use]
pub fn create_call(user: PeerId, p: &super::calls::Protocol, video: bool) -> String {
    json!({
        "@type": "createCall",
        "user_id": user,
        "protocol": protocol(p),
        "is_video": video,
    })
    .to_string()
}

/// Answer one.
#[must_use]
pub fn accept_call(call: i32, p: &super::calls::Protocol) -> String {
    json!({ "@type": "acceptCall", "call_id": call, "protocol": protocol(p) }).to_string()
}

/// End one — the same request for hanging up, declining and cancelling; which
/// of those it was is the wire's to decide from the state it was in.
#[must_use]
pub fn discard_call(call: i32, disconnected: bool, secs: i64, video: bool) -> String {
    json!({
        "@type": "discardCall",
        "call_id": call,
        "is_disconnected": disconnected,
        "invite_link": "",
        "duration": secs,
        "is_video": video,
        "connection_id": 0,
    })
    .to_string()
}

/// One packet of the engine's, for the other side. Base64 because the wire's
/// `bytes` is a base64 string in the JSON interface.
#[must_use]
pub fn send_call_signaling_data(call: i32, data: &[u8]) -> String {
    use base64::Engine as _;
    json!({
        "@type": "sendCallSignalingData",
        "call_id": call,
        "data": base64::engine::general_purpose::STANDARD.encode(data),
    })
    .to_string()
}

/// How it went. One number and no words this round: the panel's *rate* says
/// it was fine, which is what a rating is for nine calls in ten.
#[must_use]
pub fn send_call_rating(call: i32, rating: i32) -> String {
    json!({
        "@type": "sendCallRating",
        "call_id": { "@type": "inputCallDiscarded", "call_id": call },
        "rating": rating,
        "comment": "",
        "problems": [],
    })
    .to_string()
}

/// The chat, walk and origin a history page's `@extra` names, or `None` for
/// any other answer's.
pub(super) fn parse_history_extra(extra: &str) -> Option<(PeerId, Walk, MsgId)> {
    let mut parts = extra.split(':');
    if parts.next()? != "history" {
        return None;
    }
    let chat = parts.next()?.parse().ok()?;
    let walk = Walk::parse(parts.next()?)?;
    let from = parts.next()?.parse().ok()?;
    Some((chat, walk, from))
}
