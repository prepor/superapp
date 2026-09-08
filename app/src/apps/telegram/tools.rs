//! Agent composition through the same chat instance the person edits and sends.
//! Drafts open for review; sends name their exact contents so an approval
//! cannot send an edit or attachment added while the call was waiting.

use kernel::layout::SlotId;
use kernel::nav::Nav;
use kernel::session::Session;
use kernel::tool::Tool;
use serde_json::{json, Value};

use super::model::{self, MsgId, PeerCard, PeerId};
use super::operations::Status;
use super::panels::Chat;
use super::{runtime, topics};

pub const DESCRIBE: &str = "\
Telegram is a local cache of one account's TDLib updates. `tg_peer` names \
people, groups and channels: `id`, `name`, `username`, `kind`, `is_self`, \
`is_contact`, `is_forum`, `blocked`. `tg_chat` holds chat flags and the \
text draft, keyed by `peer` = `tg_peer.id`. `tg_topic` holds forum topics \
keyed by (`chat`, `id`), each with its own name and draft. `tg_message` is \
keyed by (`chat`, `id`): `topic` (0 outside a forum topic), `sender`, \
`date`, `text`, `out`, `reply_to`, and media metadata. Message ids are \
only unique within a chat. Use sql.query to find a recipient by name or \
username and read their cached messages; the cache may be incomplete.

Use telegram.draft to put text in the correct chat's composer for review, \
then telegram.send with the returned slot and exact chat, topic, text and \
reply_to. Sending asks for approval. Existing unsent text is preserved \
unless draft's replace is explicitly set. Drafts persist like typed text; \
reply selections belong to the open composer. Use telegram.status to check \
the returned operation: queued does not mean delivered. Never repeat a send \
just because its acknowledgement is pending or uncertain.

Do not INSERT messages or UPDATE drafts/flags with sql.write: these tables \
are a projection, not a command queue. Such writes do not send anything to \
Telegram and bypass the live composer's state. Telegram panel tag is \
`telegram-chat`, with args [chat_id] or [chat_id, \"topic\", topic_id]; \
`chat` is the AI conversation, not Telegram. Find Saved Messages via \
tg_peer.is_self rather than guessing its id.";

pub fn all() -> Vec<Tool> {
    let mut draft_input = message_input();
    draft_input["properties"]["replace"] = json!({
        "type": "boolean",
        "description": "Explicitly replace existing draft text; defaults to false. Never discards an edit or attachments."
    });
    let mut send_input = message_input();
    send_input["properties"]["slot"] = json!({
        "type": "integer", "description": "the Telegram composer slot returned by telegram.draft"
    });
    send_input["required"] = json!(["slot", "chat", "text"]);
    vec![
        Tool::new(
            "telegram.draft",
            "Write a text draft and open its Telegram chat for review. Find the chat id \
             with sql.query first. Supports forum topics and replies to cached messages. \
             Updates every open copy of this composer. Refuses existing unsent work \
             unless replace is true. Returns the exact arguments telegram.send takes.",
            draft_input,
            true,
            draft,
        ),
        Tool::new(
            "telegram.send",
            "Send the text draft already open in a Telegram composer, using the slot, \
             chat, topic, text and reply_to returned by telegram.draft. Asks for approval \
             and refuses if the draft changed or now carries an edit or files. A success \
             means queued, not delivered: check telegram.status with the operation id. \
             Offline failures keep the draft. Undo requests deletion for everyone; Telegram must confirm it.",
            send_input,
            true,
            send,
        )
        .asking(),
        Tool::new(
            "telegram.status",
            "Check a Telegram send operation returned by telegram.send: pending, done, \
             or failed. An uncertain failure may already have been delivered; do not \
             send it again automatically. Operation ids last for this app session.",
            json!({
                "type": "object",
                "properties": {"operation": {"type": "integer"}},
                "required": ["operation"], "additionalProperties": false
            }),
            false,
            status,
        ),
    ]
}

fn message_input() -> Value {
    json!({
        "type": "object",
        "properties": {
            "chat": {"type": "integer", "description": "the recipient's tg_peer.id, not an AI chat id"},
            "topic": {"type": "integer", "description": "tg_topic.id within this chat; omit or use 0 outside a forum topic"},
            "text": {"type": "string", "description": "the whole plain-text message"},
            "reply_to": {"type": ["integer", "null"], "description": "a cached tg_message.id in this chat and topic; omit for no reply"}
        },
        "required": ["chat", "text"], "additionalProperties": false
    })
}

struct Message<'a> {
    chat: PeerId,
    topic: i64,
    text: &'a str,
    reply_to: Option<MsgId>,
}

impl<'a> Message<'a> {
    fn read(input: &'a Value) -> Result<Self, String> {
        let chat = input["chat"].as_i64().ok_or("`chat` must be an integer")?;
        let topic = match input.get("topic") {
            None => 0,
            Some(v) => v
                .as_i64()
                .filter(|t| *t >= 0)
                .ok_or("`topic` must be a nonnegative integer")?,
        };
        let text = input["text"].as_str().ok_or("`text` must be a string")?;
        if text.trim().is_empty() {
            return Err("the message is empty".into());
        }
        let reply_to = match input.get("reply_to") {
            None | Some(Value::Null) => None,
            Some(v) => Some(
                v.as_i64()
                    .filter(|id| *id > 0)
                    .ok_or("`reply_to` must be a positive message id")?,
            ),
        };
        Ok(Self {
            chat,
            topic,
            text,
            reply_to,
        })
    }

    fn destination(&self, s: &Session) -> Result<PeerCard, String> {
        s.store().poll_external();
        let card = topics::card(s.store(), self.chat, self.topic)
            .ok_or("no cached Telegram chat or topic at that id")?;
        if !card.can_post() {
            return Err(if card.blocked {
                "unblock this user before sending a message"
            } else {
                "you can't post here"
            }
            .into());
        }
        if self.topic != 0
            && topics::get(s.store(), self.chat, self.topic)
                .is_some_and(|t| t.closed && !card.admin)
        {
            return Err("that forum topic is closed".into());
        }
        if let Some(id) = self.reply_to {
            if !model::history_in(s.store(), self.chat, self.topic)
                .iter()
                .any(|m| m.id == id && !m.service)
            {
                return Err(
                    "the reply target is not a cached message in this chat and topic".into(),
                );
            }
        }
        Ok(card)
    }

    fn matches(&self, c: &Chat) -> bool {
        c.peer() == self.chat && c.topic_id() == self.topic
    }

    fn result(&self, slot: SlotId) -> Value {
        json!({"slot": slot, "chat": self.chat, "topic": self.topic,
            "text": self.text, "reply_to": self.reply_to})
    }
}

fn ready(s: &Session) -> Result<(), String> {
    if s.writable() {
        Ok(())
    } else {
        Err("another device holds the lease — nothing was written".into())
    }
}

fn text_composer(c: &Chat) -> Result<(), String> {
    if c.editing().is_some() || !c.carrying().is_empty() {
        Err("this composer has an edit or attachments; finish those in Telegram first".into())
    } else {
        Ok(())
    }
}

fn draft(s: &mut Session, input: &Value) -> Result<Value, String> {
    ready(s)?;
    let msg = Message::read(input)?;
    let card = msg.destination(s)?;
    let replace = input["replace"].as_bool().unwrap_or(false);
    if !replace
        && card
            .draft
            .as_deref()
            .is_some_and(|d| !d.is_empty() && d != msg.text)
    {
        return Err(
            "this chat already has a draft; use replace only to intentionally replace it".into(),
        );
    }
    let mut slots = Vec::new();
    for (slot, panel) in s.panels() {
        let mut p = panel.borrow_mut();
        let Some(c) = p.as_any().downcast_mut::<Chat>().filter(|c| msg.matches(c)) else {
            continue;
        };
        let _ = c.card();
        text_composer(c)?;
        if !replace
            && ((!c.field_text().is_empty() && c.field_text() != msg.text)
                || (c.reply_to().is_some() && c.reply_to() != msg.reply_to))
        {
            return Err(
                "an open composer has unsent work; use replace only to intentionally replace it"
                    .into(),
            );
        }
        slots.push(slot);
    }
    let slot = if let Some(slot) = slots.first().copied() {
        s.nav(Nav::Focus(slot));
        slot
    } else {
        let from = s
            .focus()
            .ok_or("no panel has focus to open the Telegram draft beside")?;
        let id = if msg.topic == 0 {
            Chat::id(msg.chat)
        } else {
            Chat::topic(msg.chat, msg.topic)
        };
        s.nav(Nav::Open {
            from,
            id: id.clone(),
            fresh: false,
        });
        // Navigation records the new slot first; the composer instance is
        // mounted by settle. Agent calls hold no panel borrow across this.
        s.settle();
        let slot = s
            .showing(&id)
            .into_iter()
            .next()
            .ok_or("the Telegram composer did not open")?;
        slots.push(slot);
        slot
    };
    for open in slots {
        let panel = s.panel(open).ok_or("the Telegram composer closed")?;
        let mut p = panel.borrow_mut();
        let c = p
            .as_any()
            .downcast_mut::<Chat>()
            .ok_or("that slot is not a Telegram composer")?;
        c.stage_draft(msg.text, msg.reply_to, open == slot)?;
    }
    s.redraw();
    Ok(msg.result(slot))
}

fn send(s: &mut Session, input: &Value) -> Result<Value, String> {
    ready(s)?;
    let msg = Message::read(input)?;
    msg.destination(s)?;
    let slot = input["slot"]
        .as_u64()
        .ok_or("`slot` must be a nonnegative integer")?;
    let panel = s
        .panel(slot)
        .ok_or("no Telegram composer at that slot; use telegram.draft first")?;
    let mut p = panel.borrow_mut();
    let c = p
        .as_any()
        .downcast_mut::<Chat>()
        .ok_or("that slot is not a Telegram composer")?;
    let _ = c.card();
    text_composer(c)?;
    if !msg.matches(c) || c.field_text() != msg.text || c.reply_to() != msg.reply_to {
        return Err("the Telegram draft changed; review it and request a new send with its current contents".into());
    }
    if !super::panels::live(s.store()) {
        return Err("Telegram is not connected; the draft is kept and nothing was sent".into());
    }
    let operation = c
        .send_text_draft(s)
        .ok_or("Telegram could not queue the message; the draft is kept")?;
    Ok(json!({"status": "queued", "operation": operation,
        "chat": msg.chat, "topic": msg.topic}))
}

fn status(s: &mut Session, input: &Value) -> Result<Value, String> {
    let id = input["operation"]
        .as_u64()
        .ok_or("`operation` must be a nonnegative integer")?;
    let rt = runtime::of(s.store());
    rt.operations.expire(s.store(), std::time::Instant::now());
    let op = rt
        .operations
        .outcome(id)
        .ok_or("no Telegram operation at that id in this app session")?;
    let (status, error, uncertain) = match &op.status {
        Status::Pending => ("pending", None, false),
        Status::Done => ("done", None, false),
        Status::Failed { error, uncertain } => ("failed", Some(error), *uncertain),
    };
    Ok(json!({"operation": id, "chat": op.chat, "status": status,
        "error": error, "uncertain": uncertain, "retryable": op.retryable}))
}
