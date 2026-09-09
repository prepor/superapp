//! Copies retained before deletion. Undo sends new messages from these bytes;
//! Telegram's deleted message ids and remote file references are never reused.

use std::path::PathBuf;

use kernel::caps::Blobs;
use kernel::effect::World;
use ring::rand::{SecureRandom, SystemRandom};
use serde_json::{json, Value};

use super::super::{downloads, updates};

pub(super) struct Copy {
    pub original: (i64, i64),
    pub request: Value,
    file: Option<(Value, String, String)>,
}

#[derive(Default)]
pub(super) struct Saved {
    pub copies: Vec<Copy>,
    directory: Option<PathBuf>,
}

fn supported(kind: &str) -> bool {
    matches!(kind, "messageText" | "messagePhoto" | "messageDocument" | "messageVideo"
        | "messageAnimation" | "messageAudio" | "messageVoiceNote" | "messageVideoNote"
        | "messageSticker" | "messageContact" | "messageLocation" | "messageVenue")
}

/// Known incoming and unsupported messages keep the explicit irreversible
/// action. The full snapshot verifies eligibility again before deletion.
pub(super) fn eligible(conn: &rusqlite::Connection, request: &Value) -> rusqlite::Result<bool> {
    let Some(chat) = request["chat_id"].as_i64() else { return Ok(false); };
    let Some(ids) = request["message_ids"].as_array().filter(|ids| !ids.is_empty()) else { return Ok(false); };
    if ids.iter().any(|id| id.as_i64().is_none()) { return Ok(false); }
    // Only the eligibility columns are needed. Unknown rows still require a
    // complete TDLib snapshot; a SQL error must never authorize deletion.
    let mut query = conn.prepare(
        "SELECT out, service, sender, content_type FROM tg_message
         WHERE chat = ?1 AND id IN (SELECT value FROM json_each(?2))")?;
    let mut rows = query.query(rusqlite::params![chat, request["message_ids"].to_string()])?;
    while let Some(row) = rows.next()? {
        if !row.get::<_, bool>(0)? || row.get::<_, bool>(1)?
            || row.get::<_, Option<i64>>(2)?.is_some_and(|sender| sender <= 0)
            || row.get::<_, Option<String>>(3)?.as_deref().is_some_and(|kind| !supported(kind)) {
            return Ok(false);
        }
    }
    Ok(true)
}

impl Saved {
    pub fn capture(request: &Value, reply: &Value) -> Result<Self, String> {
        let messages = reply["messages"].as_array().filter(|_| reply["@type"] == "messages")
            .ok_or("Telegram did not return the original messages")?;
        let ids = request["message_ids"].as_array().ok_or("Missing message ids")?;
        if messages.len() != ids.len() { return Err("Telegram did not return every original message".into()); }
        let mut copies = Vec::new();
        for (id, message) in ids.iter().zip(messages) {
            if message["@type"] != "message" || message["chat_id"] != request["chat_id"] || message["id"] != *id {
                return Err("An original message is unavailable".into());
            }
            copies.push(copy(message)?);
        }
        copies.sort_by_key(|c| c.original.1);
        if copies.windows(2).any(|pair| pair[0].original == pair[1].original) {
            return Err("Duplicate message ids".into());
        }
        Ok(Self { copies, directory: None })
    }

    /// Runs on the account worker. Keep independent files outside the media
    /// cache, whose eviction policy must not discard a history node's undo.
    pub fn next_file(&mut self, w: &World) -> Result<Option<i32>, String> {
        for index in 0..self.copies.len() {
            let Some((file, pointer, name)) = self.copies[index].file.clone() else { continue; };
            let cached = updates::file_ref(&file).and_then(|key|
                w.with_cap::<dyn Blobs, _>(|blobs| blobs.get(&key)).ok().flatten());
            let source = cached.or_else(|| (file["local"]["is_downloading_completed"] == true)
                .then(|| file["local"]["path"].as_str().map(PathBuf::from)).flatten()
                .filter(|path| path.is_file()));
            let Some(source) = source else {
                return file["id"].as_i64().and_then(|id| i32::try_from(id).ok()).filter(|id| *id > 0)
                    .map(Some).ok_or_else(|| "An attachment cannot be downloaded".into());
            };
            if self.directory.is_none() { self.directory = Some(private_directory()?); }
            let dir = self.directory.as_ref().unwrap().join(index.to_string());
            std::fs::create_dir(&dir).map_err(|e| format!("Cannot save attachment for undo: {e}"))?;
            let path = dir.join(name);
            let metadata = std::fs::metadata(&source).map_err(|e| e.to_string())?;
            if !metadata.is_file() || metadata.len() == 0 { return Err("An attachment has no readable bytes".into()); }
            std::fs::copy(&source, &path).map_err(|e| format!("Cannot save attachment for undo: {e}"))?;
            *self.copies[index].request.pointer_mut(&pointer).ok_or("Invalid attachment snapshot")? =
                json!({"@type": "inputFileLocal", "path": path});
            self.copies[index].file = None;
        }
        Ok(None)
    }

    pub fn downloaded(&mut self, w: &World, file: &Value) -> Result<(), String> {
        let pending = self.copies.iter_mut().find_map(|copy| copy.file.as_mut())
            .ok_or("Unexpected attachment download")?;
        if file["@type"] != "file" || file["id"] != pending.0["id"]
            || file["local"]["is_downloading_completed"] != true {
            return Err("Telegram did not finish downloading the original attachment".into());
        }
        let cached = updates::file_ref(file).is_some_and(|key|
            w.with_cap::<dyn Blobs, _>(|blobs| blobs.contains(&key)).unwrap_or(false));
        if !cached && !file["local"]["path"].as_str().is_some_and(|path| std::path::Path::new(path).is_file()) {
            return Err("Downloaded attachment bytes are unavailable".into());
        }
        pending.0 = file.clone();
        Ok(())
    }
}

impl Drop for Saved {
    fn drop(&mut self) {
        if let Some(path) = self.directory.take() {
            kernel::runtime::spawn_blocking(move || { let _ = std::fs::remove_dir_all(path); });
        }
    }
}

fn private_directory() -> Result<PathBuf, String> {
    let mut random = [0; 16];
    SystemRandom::new().fill(&mut random).map_err(|_| "Cannot create undo storage")?;
    let name: String = random.iter().map(|byte| format!("{byte:02x}")).collect();
    let path = std::env::temp_dir().join(format!("superapp-telegram-undo-{name}"));
    let mut builder = std::fs::DirBuilder::new();
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;
        builder.mode(0o700);
    }
    builder.create(&path).map_err(|e| format!("Cannot create undo storage: {e}"))?;
    Ok(path)
}

fn copy(message: &Value) -> Result<Copy, String> {
    if message["is_outgoing"] != true || message["sender_id"]["@type"] == "messageSenderChat"
        || message["is_channel_post"] == true || message["can_be_saved"] == false
        || !message["self_destruct_type"].is_null() || !message["sending_state"].is_null()
        || !message["scheduling_state"].is_null() {
        return Err("This message cannot be saved and resent as your own message".into());
    }
    let chat = message["chat_id"].as_i64().ok_or("Missing original chat")?;
    let id = message["id"].as_i64().filter(|id| *id > 0).ok_or("Missing original message id")?;
    let content = &message["content"];
    let kind = content["@type"].as_str().filter(|kind| supported(kind)).ok_or("This message type cannot be resent")?;
    let mut input = json!({"@type": kind.replacen("message", "inputMessage", 1)});
    let mut file = None;
    match kind {
        "messageText" => {
            content["text"]["text"].as_str().ok_or("Missing original text")?;
            input["text"] = content["text"].clone();
            input["clear_draft"] = json!(false);
            input["link_preview_options"] = content["link_preview_options"].clone();
        }
        "messageContact" | "messageLocation" | "messageVenue" => {
            let field = match kind { "messageContact" => "contact", "messageVenue" => "venue", _ => "location" };
            input[field] = content[field].clone();
            input[field].as_object().ok_or("Missing original message content")?;
            if kind == "messageLocation" && content["live_period"].as_i64().is_some_and(|n| n > 0) {
                return Err("Live locations cannot be restored by resending".into());
            }
        }
        _ => {
            let (field, input_type, source_field) = match kind {
                "messagePhoto" => ("photo", "inputPhoto", "photo"),
                "messageDocument" => ("document", "inputDocument", "document"),
                "messageVideo" => ("video", "inputVideo", "video"),
                "messageAnimation" => ("animation", "inputAnimation", "animation"),
                "messageAudio" => ("audio", "inputAudio", "audio"),
                "messageVoiceNote" => ("voice_note", "inputVoiceNote", "voice"),
                "messageVideoNote" => ("video_note", "inputVideoNote", "video"),
                _ => ("sticker", "inputSticker", "sticker"),
            };
            let media = &content[field];
            let source = if kind == "messagePhoto" {
                media["sizes"].as_array().and_then(|sizes| sizes.iter().max_by_key(|size|
                    size["width"].as_i64().unwrap_or(0).saturating_mul(size["height"].as_i64().unwrap_or(0))))
                    .ok_or("Missing original photo")?
            } else { media };
            let original_file = source[source_field].as_object().ok_or("Missing original attachment")?;
            input[field] = json!({"@type": input_type, field: null});
            for key in ["width", "height", "duration", "length", "waveform", "title", "performer", "supports_streaming"] {
                if let Some(value) = source.get(key) { input[field][key] = value.clone(); }
            }
            if kind == "messageDocument" { input[field]["disable_content_type_detection"] = json!(true); }
            for key in ["caption", "show_caption_above_media", "has_spoiler"] {
                if let Some(value) = content.get(key) { input[key] = value.clone(); }
            }
            if kind == "messageSticker" { input["emoji"] = media["emoji"].clone(); }
            let name = media["file_name"].as_str().filter(|name| !name.is_empty())
                .map(downloads::safe_name).unwrap_or_else(|| format!("telegram-{chat}-{id}.{}", match kind {
                    "messagePhoto" => "jpg", "messageVideo" | "messageAnimation" | "messageVideoNote" => "mp4",
                    "messageVoiceNote" => "ogg", "messageSticker" => match media["format"]["@type"].as_str() {
                        Some("stickerFormatTgs") => "tgs", Some("stickerFormatWebm") => "webm", _ => "webp",
                    }, _ => "bin",
                }));
            file = Some((Value::Object(original_file.clone()), format!("/input_message_content/{field}/{field}"), name));
        }
    }
    let mut request = json!({"@type": "sendMessage", "chat_id": chat, "input_message_content": input,
        "topic_id": message["topic_id"]});
    if request["topic_id"].is_null() {
        if let Some(thread) = message["message_thread_id"].as_i64().filter(|id| *id != 0) {
            request["message_thread_id"] = json!(thread);
        }
    }
    if let Some(reply) = reply_to(&message["reply_to"], chat) { request["reply_to"] = reply; }
    Ok(Copy { original: (chat, id), request, file })
}

fn reply_to(original: &Value, chat: i64) -> Option<Value> {
    let mut reply = original.clone();
    let object = reply.as_object_mut()?;
    match original["@type"].as_str()? {
        "messageReplyToMessage" => {
            original["message_id"].as_i64().filter(|id| *id > 0)?;
            let external = original["chat_id"].as_i64().is_some_and(|id| id != 0 && id != chat);
            object.retain(|key, _| matches!(key.as_str(), "message_id" | "quote" | "checklist_task_id" | "poll_option_id")
                || (external && key == "chat_id"));
            object.insert("@type".into(), json!(if external { "inputMessageReplyToExternalMessage" } else { "inputMessageReplyToMessage" }));
            if let Some(quote) = reply["quote"].as_object_mut() {
                quote.insert("@type".into(), json!("inputTextQuote"));
                quote.remove("is_manual");
            }
        }
        "messageReplyToStory" => { object.insert("@type".into(), json!("inputMessageReplyToStory")); }
        _ => return None,
    }
    Some(reply)
}
