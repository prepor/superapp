//! TDLib can omit reactions while metadata is loading, as well as when a
//! message has none. Confirm absence with metadata and a server search, using
//! the same read path as sync/counts; never infer ownership from display counts.

use super::*;
use super::super::reaction_state;

#[derive(Default)]
pub(super) struct Snapshot {
    phase: Phase,
    read: Option<String>,
    revision: i64,
    invalidated: bool,
}

#[derive(Default, PartialEq)]
enum Phase { #[default] Initial, Metadata, Confirmed }

pub(super) enum Preparation {
    Read(String),
    Ready(Option<Vec<Value>>),
}

impl Snapshot {
    pub(super) fn invalidate(&mut self) {
        if self.phase != Phase::Initial { self.invalidated = true; }
    }

    pub(super) fn prepare(&mut self, store: &Store, request: &Value, reply: &Value) -> Preparation {
        let (Some(chat), Some(id)) = (request["chat_id"].as_i64(), request["message_id"].as_i64())
            else { return Preparation::Ready(None); };
        let revision = reaction_state::state(store.conn(), chat, id).ok().map(|s| s.revision);
        if self.phase != Phase::Initial && (self.invalidated || revision != Some(self.revision)) {
            return self.confirm(store, request, chat, id, revision);
        }
        if self.phase == Phase::Metadata {
            let ready = reply["@type"] == "availableReactions" && (reply["allow_custom_emoji"] == true
                || ["top_reactions", "recent_reactions", "popular_reactions"].iter()
                    .any(|key| reply[key].as_array().is_some_and(|a| !a.is_empty())));
            if !ready { return Preparation::Ready(None); }
            self.phase = Phase::Confirmed;
            return Preparation::Read(self.read.clone().unwrap());
        }
        let message = if self.phase == Phase::Confirmed {
            if !matches!(reply["@type"].as_str(), Some("foundChatMessages" | "messages")) {
                return Preparation::Ready(None);
            }
            reply["messages"].as_array().into_iter().flatten()
                .find(|m| m["chat_id"] == chat && m["id"] == id)
        } else { Some(reply) };
        let Some(message) = message.filter(|m| m["@type"] == "message" && m["chat_id"] == chat && m["id"] == id)
            else { return Preparation::Ready(None); };
        if self.phase == Phase::Initial && missing_reactions(message) {
            return self.confirm(store, request, chat, id, revision);
        }
        Preparation::Ready(inverse(request, message, self.phase == Phase::Confirmed))
    }

    fn confirm(&mut self, store: &Store, request: &Value, chat: i64, id: i64, revision: Option<i64>) -> Preparation {
        let Some(message) = model::line(store, chat, id) else { return Preparation::Ready(None); };
        let Some(revision) = revision else { return Preparation::Ready(None); };
        let context = format!("undo_snapshot:{}", request["@extra"]["operation"]);
        self.read = Some(requests::reaction_count_snapshot(&message, &context));
        self.revision = revision;
        self.invalidated = false;
        self.phase = Phase::Metadata;
        Preparation::Read(requests::reaction_count_metadata(chat, id, &context))
    }
}

fn missing_reactions(message: &Value) -> bool {
    let info = &message["interaction_info"];
    (info.is_null() || info.is_object()) && info["reactions"].is_null()
}

fn inverse(request: &Value, message: &Value, confirmed: bool) -> Option<Vec<Value>> {
    let info = &message["interaction_info"]["reactions"];
    let reactions = info.as_array().or_else(|| info["reactions"].as_array()).map(Vec::as_slice)
        .or_else(|| (confirmed && missing_reactions(message)).then_some(&[]))?;
    let mut chosen = Vec::new();
    for reaction in reactions {
        if reaction["is_chosen"].as_bool()? {
            let kind = reaction.get("type").cloned().or_else(|| reaction["reaction"].as_str()
                .map(|emoji| json!({"@type": "reactionTypeEmoji", "emoji": emoji})))?;
            if kind["@type"] != "reactionTypePaid" { chosen.push(kind); }
        }
    }
    // Adding an already chosen emoji is a no-op: undo must not remove it.
    if chosen.contains(&request["reaction_type"]) { return Some(Vec::new()); }
    let mut base = request.clone();
    base.as_object_mut()?.remove("@extra");
    let mut remove = base.clone();
    remove["@type"] = json!("removeMessageReaction");
    remove.as_object_mut()?.remove("is_big");
    remove.as_object_mut()?.remove("update_recent_reactions");
    let mut commands = vec![remove];
    // Adding can evict a previous selection at the account's limit.
    for kind in chosen {
        let mut add = base.clone();
        add["reaction_type"] = kind;
        add["update_recent_reactions"] = json!(false);
        commands.push(add);
    }
    Some(commands)
}
