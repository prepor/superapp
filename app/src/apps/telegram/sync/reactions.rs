//! Reaction choices depend on asynchronously loaded chat/emoji metadata.
//! Keep the panel's weak reply subscribed, and correlate every refresh so
//! an old response cannot overwrite a newer attempt.

use std::collections::HashMap;

use super::*;
use runtime::ReactionResult;

const PATIENCE: f64 = 30.0;
const EMPTY_PATIENCE: f64 = 10.0;

#[derive(Default)]
pub(super) struct Reactions {
    loads: HashMap<u64, Load>,
    adds: HashMap<u64, (String, f64)>,
}

struct Load {
    chat: PeerId,
    msg: MsgId,
    attempt: u64,
    pending: bool,
    dirty: bool,
    due: f64,
    started: f64,
    has_choices: bool,
}

impl Load {
    fn context(&self, id: u64) -> String {
        if self.attempt == 0 { format!("reactions:{id}") }
        else { format!("reaction_choices:{id}:{}", self.attempt) }
    }
}

impl<T: Td> Account<T> {
    pub(super) fn track_reactions(&self, w: &World, request: &Value) {
        let Some(extra) = request["@extra"]["context"].as_str().or_else(|| request["@extra"].as_str()) else { return; };
        if let Some((id, 0)) = parse_reaction_choices_extra(extra) {
            let (Some(chat), Some(msg)) = (request["chat_id"].as_i64(), request["message_id"].as_i64()) else { return; };
            self.reactions.borrow_mut().loads.insert(id, Load {
                chat, msg, attempt: 0, pending: true, dirty: false,
                due: w.now() + PATIENCE, started: w.now(),
                has_choices: false,
            });
        } else if let Some((id, _, _)) = parse_added_reaction_extra(extra) {
            self.reactions.borrow_mut().adds.insert(id, (extra.to_string(), w.now() + PATIENCE));
        }
    }

    pub(super) fn sync_reactions(&self, w: &World) {
        let rt = runtime::of(w.store());
        let now = w.now();
        let mut state = self.reactions.borrow_mut();
        state.adds.retain(|id, (context, due)| {
            if now >= *due {
                let error = "Telegram did not confirm the reaction. Check the message before trying again.";
                rt.operations.fail_context(w.store(), context, error);
                rt.finish_reaction(*id, ReactionResult::Error(error.into()));
            }
            now < *due && rt.reaction_alive(*id)
        });
        let mut requests = Vec::new();
        state.loads.retain(|id, load| {
            if !rt.reaction_alive(*id) {
                rt.operations.retire_context(&load.context(*id));
                return false;
            }
            if now < load.due { return true; }
            if load.pending {
                let error = "Telegram did not return the available reactions. Try again.";
                rt.operations.fail_context(w.store(), &load.context(*id), error);
                rt.finish_reaction(*id, ReactionResult::Error(error.into()));
                return false;
            }
            load.attempt += 1;
            load.pending = true;
            load.dirty = false;
            load.due = now + PATIENCE;
            requests.push(refresh_available_reactions(load.chat, load.msg, *id, load.attempt));
            true
        });
        drop(state);
        for request in requests { self.send(w, &request); }
    }

    /// TDLib documents these three invalidations for available reactions.
    /// Coalesce updates while a query is pending; never overlap its replies.
    pub(super) fn reactions_changed(&self, w: &World, chat: Option<PeerId>, msg: Option<MsgId>) {
        for load in self.reactions.borrow_mut().loads.values_mut() {
            if chat.is_none_or(|c| c == load.chat) && msg.is_none_or(|m| m == load.msg) {
                if load.due.is_infinite() { load.started = w.now(); }
                if load.pending {
                    load.dirty = true;
                } else {
                    load.due = w.now();
                }
            }
        }
    }

    pub(super) fn on_reaction_choices(&self, w: &World, v: &Value, failed: bool) -> bool {
        let Some((id, attempt)) = v["@extra"].as_str().and_then(parse_reaction_choices_extra) else { return false; };
        let rt = runtime::of(w.store());
        if !rt.reaction_alive(id) { return true; }
        let mut state = self.reactions.borrow_mut();
        let Some(load) = state.loads.get_mut(&id) else { return true; };
        if load.attempt != attempt || !load.pending { return true; }
        if failed {
            let error = rt.connection_error().unwrap_or_else(|| v["message"].as_str().unwrap_or("reaction request failed").to_string());
            rt.finish_reaction(id, ReactionResult::Error(error));
            state.loads.remove(&id);
            return true;
        }
        load.pending = false;
        if load.dirty {
            load.due = w.now();
            return true;
        }
        let emojis = updates::available_reactions(v);
        let unavailable = match v["unavailability_reason"]["@type"].as_str() {
            Some("reactionUnavailabilityReasonAnonymousAdministrator") => Some("switch from anonymous admin to react"),
            Some("reactionUnavailabilityReasonGuest") => Some("join this chat to react"),
            Some("reactionUnavailabilityReasonRestricted") => Some("reactions are restricted in this chat"),
            Some(_) => Some("reactions are unavailable for this message"),
            None => None,
        };
        self.log(&format!("<< available reactions request={id} attempt={attempt} choices={} restricted={}", emojis.len(), unavailable.is_some()));
        if let Some(reason) = unavailable {
            load.due = f64::INFINITY;
            load.has_choices = false;
            rt.finish_reaction(id, ReactionResult::Unavailable(reason.into()));
        } else if emojis.is_empty() && ["top_reactions", "recent_reactions", "popular_reactions"].iter()
            .all(|key| v[key].as_array().is_none_or(|a| a.is_empty())) {
            // getMessageAvailableReactions reads a cache while TDLib loads
            // the active emoji and chat permissions. An initial empty list
            // is provisional; retry even if that metadata emits no update.
            load.due = w.now() + 2_f64.powi(load.attempt.min(5) as i32).min(30.0);
            if !load.has_choices && w.now() - load.started >= EMPTY_PATIENCE {
                rt.finish_reaction(id, ReactionResult::Waiting);
            }
        } else {
            load.due = f64::INFINITY;
            load.has_choices = !emojis.is_empty();
            rt.finish_reaction(id, ReactionResult::Choices(emojis));
        }
        true
    }
}
