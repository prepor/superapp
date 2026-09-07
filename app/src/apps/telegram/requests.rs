//! Pure TDLib request builders and response-correlation metadata.
//!
//! Panels depend on this module rather than the worker implementation.
//! Login secrets are serialized only for the in-memory command queue.

#![cfg_attr(not(feature = "tdlib"), allow(dead_code))]

use std::path::Path;
use serde_json::{json, Value};
use super::model::{self, MsgId, PeerId};

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

// -- the content verbs, live ---------------------------------------------------
//
// The phase-4 requests: the composer's send, reply and edit, and a line's
// delete and read. TDLib echoes each back as an update — a sent line as
// `updateNewMessage`, an edit as `updateMessageContent`, a delete as
// `updateDeleteMessages` — which the projection lays into `tg_*`, so the
// transcript reflects the wire through the one path it already draws, with no
// optimistic local write of our own.

/// The `inputMessageText` a send or an edit carries: the text as a
/// `formattedText` with no entities of ours — Telegram parses none unasked, so
/// what the composer holds travels as plain text. Shared so a send and an edit
/// spell the content the one way.
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
    content[names_it][names_it] = json!({
        "@type": "inputFileLocal",
        "path": kernel::caps::real_path(&file.path).to_string_lossy(),
    });
    let mut req = json!({
        "@type": "sendMessage",
        "chat_id": chat_id,
        "input_message_content": content,
    });
    with_reply(&mut req, reply_to);
    req.to_string()
}

/// Where the device says I am, to a chat. `live_period` nought is the one-off
/// share — a location for an hour is a live one, and that is a later phase —
/// and with it the two fields that only a live location moves, the heading and
/// the radius an alert would fire at, are nought too. The accuracy is nought
/// for *not measured*, this round's place being a constant rather than a
/// reading.
#[must_use]
pub fn send_location(chat_id: PeerId, reply_to: Option<MsgId>, lat: f64, lon: f64) -> String {
    let mut req = json!({
        "@type": "sendMessage",
        "chat_id": chat_id,
        "input_message_content": {
            "@type": "inputMessageLocation",
            "location": {
                "@type": "location",
                "latitude": lat,
                "longitude": lon,
                "horizontal_accuracy": 0,
            },
            "live_period": 0,
            "heading": 0,
            "proximity_alert_radius": 0,
        },
    });
    with_reply(&mut req, reply_to);
    req.to_string()
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

/// Route a composer command to a forum topic. Message ids still belong to
/// the parent chat; the topic is a separate destination in TDLib's API.
pub fn in_topic(request: String, topic: i64) -> String {
    if topic == 0 { return request; }
    let mut req: Value = serde_json::from_str(&request).expect("a request builder's JSON");
    req["topic_id"] = json!({"@type": "messageTopicForum", "forum_topic_id": topic});
    req.as_object_mut().unwrap().remove("message_thread_id");
    if req["@type"] == "viewMessages" {
        req.as_object_mut().unwrap().remove("topic_id");
        req["source"] = json!({"@type": "messageSourceForumTopicHistory"});
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

pub fn get_history_in(chat: PeerId, topic: i64, from: MsgId, walk: Walk) -> String {
    if topic == 0 { return get_chat_history(chat, from, walk); }
    json!({"@type": "getForumTopicHistory", "chat_id": chat, "forum_topic_id": topic,
        "from_message_id": from, "offset": 0, "limit": HISTORY_PAGE,
        "@extra": history_extra_in(chat, topic, from, walk)}).to_string()
}

pub(super) fn history_extra_in(chat: PeerId, topic: i64, from: MsgId, walk: Walk) -> String {
    if topic == 0 {
        format!("history:{chat}:{}:{from}", walk.word())
    } else {
        format!("topic_history:{chat}:{topic}:{}:{from}", walk.word())
    }
}

pub(super) fn parse_history_in(extra: &str) -> Option<(PeerId, i64, Walk, MsgId)> {
    if let Some((chat, walk, from)) = parse_history_extra(extra) {
        return Some((chat, 0, walk, from));
    }
    let mut parts = extra.split(':');
    if parts.next()? != "topic_history" { return None; }
    Some((parts.next()?.parse().ok()?, parts.next()?.parse().ok()?,
        Walk::parse(parts.next()?)?, parts.next()?.parse().ok()?))
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
        "@extra": history_extra_in(chat, 0, from, walk),
    })
    .to_string()
}

/// Ask TDLib for one line by its ids — the answer is the `message` itself,
/// re-projected whole. What a viewer sends for a video line that predates the
/// clip's remote id being kept on the row.
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
