//! A message's reaction picker, shared by the transcript and the line card.
//! It temporarily uses the panel's bar, with six emoji per page.

use kernel::panel::Verb;
use kernel::session::Session;
use kernel::store::Store;

use super::super::model::{self, Msg, MsgId, PeerId};
use super::super::runtime::{self, ReactionReply, ReactionResult};
use super::super::{draft_toast, requests};
use super::wire;

const CHOICES: [&str; 6] = [
    "telegram.reaction_0",
    "telegram.reaction_1",
    "telegram.reaction_2",
    "telegram.reaction_3",
    "telegram.reaction_4",
    "telegram.reaction_5",
];

#[derive(Default)]
pub(super) struct Reactions(Option<Picker>);

struct Picker {
    chat: PeerId,
    msg: MsgId,
    page: usize,
    reply: ReactionReply,
    adding: bool,
    live: bool,
    seen_reply: bool,
}

impl Picker {
    fn result(&self) -> Option<ReactionResult> {
        self.reply.lock().expect("reaction reply").clone()
    }

    fn active(&self) -> bool {
        !matches!(
            *self.reply.lock().expect("reaction reply"),
            Some(ReactionResult::Added)
        )
    }
}

pub(super) fn can_react(m: &Msg) -> bool {
    !m.service && !matches!(m.state.as_deref(), Some("sending" | "failed"))
}

impl Reactions {
    pub fn open(&mut self, store: &Store, m: &Msg) {
        if !can_react(m) {
            return;
        }
        let runtime = runtime::of(store);
        let (id, reply) = runtime.await_reaction();
        let was_live = self.0.as_ref().is_some_and(|p| p.live);
        let live = wire(
            store,
            &requests::get_message_available_reactions(m.chat, m.id, id),
        );
        if !live {
            let result = if was_live {
                ReactionResult::Error("Telegram is disconnected".into())
            } else {
                ReactionResult::Choices(
                    ["👍", "❤️", "🔥", "😂", "😮", "🙏", "🎉", "👏"]
                        .into_iter()
                        .map(str::to_string)
                        .collect(),
                )
            };
            runtime.finish_reaction(id, result);
        }
        self.0 = Some(Picker {
            chat: m.chat,
            msg: m.id,
            page: 0,
            reply,
            adding: false,
            live: live || was_live,
            seen_reply: false,
        });
    }

    /// Worker replies do not write SQLite. The widgets poll on the worker's
    /// signal and request one redraw when the in-memory answer arrives.
    pub fn poll(&mut self) -> bool {
        let Some(p) = self.0.as_mut() else {
            return false;
        };
        if !p.seen_reply && p.reply.lock().expect("reaction reply").is_some() {
            p.seen_reply = true;
            return true;
        }
        false
    }

    pub fn cancel(&mut self) -> bool {
        self.0.take().is_some_and(|p| p.active())
    }

    pub fn verbs(&self) -> Option<Vec<Verb>> {
        let p = self.0.as_ref().filter(|p| p.active())?;
        let mut verbs = Vec::new();
        match p.result() {
            None => verbs.push(Verb::run(
                "telegram.reaction_status",
                if p.adding {
                    "adding reaction…"
                } else {
                    "loading reactions…"
                },
                None,
            )),
            Some(ReactionResult::Choices(emojis)) if !emojis.is_empty() => {
                for (id, emoji) in CHOICES
                    .iter()
                    .zip(emojis.iter().skip(p.page * CHOICES.len()))
                {
                    verbs.push(Verb::run(id, emoji, None));
                }
                if p.page > 0 {
                    verbs.push(Verb::run("telegram.reactions_back", "back", Some('b')));
                }
                if (p.page + 1) * CHOICES.len() < emojis.len() {
                    verbs.push(Verb::run("telegram.reactions_more", "more", Some('m')));
                }
            }
            Some(ReactionResult::Choices(_)) => verbs.push(Verb::run(
                "telegram.reaction_status",
                "no emoji reactions available",
                None,
            )),
            Some(ReactionResult::Error(_)) => {
                verbs.push(Verb::run(
                    "telegram.reaction_status",
                    "reaction failed",
                    None,
                ));
                verbs.push(Verb::run("telegram.reactions_retry", "retry", Some('r')));
            }
            Some(ReactionResult::Added) => return None,
        }
        verbs.push(Verb::run("telegram.reactions_cancel", "cancel", Some('c')));
        Some(verbs)
    }

    /// Consume only picker verbs, so a stale choice never acts on the
    /// transcript's new cursor. A deleted or unsent message cannot be used.
    pub fn run(&mut self, store: &Store, verb: &str, s: &mut Session) -> bool {
        if !verb.starts_with("telegram.reaction") {
            return false;
        }
        let Some(p) = self.0.as_mut().filter(|p| p.active()) else {
            return true;
        };
        match verb {
            "telegram.reaction_status" => {
                if let Some(ReactionResult::Error(error)) = p.result() {
                    s.notify(format!("could not add reaction: {error}"), true);
                }
            }
            "telegram.reactions_cancel" => {
                self.cancel();
            }
            "telegram.reactions_back" => p.page = p.page.saturating_sub(1),
            "telegram.reactions_more" => {
                if let Some(ReactionResult::Choices(emojis)) = p.result() {
                    if (p.page + 1) * CHOICES.len() < emojis.len() {
                        p.page += 1;
                    }
                }
            }
            "telegram.reactions_retry" => {
                if let Some(m) = model::line(store, p.chat, p.msg) {
                    self.open(store, &m);
                } else {
                    self.cancel();
                    s.notify("that message is no longer available", true);
                }
            }
            _ => {
                if let Some(index) = CHOICES.iter().position(|id| *id == verb) {
                    let Some(ReactionResult::Choices(emojis)) = p.result() else {
                        return true;
                    };
                    let Some(emoji) = emojis.get(p.page * CHOICES.len() + index) else {
                        return true;
                    };
                    if !model::line(store, p.chat, p.msg)
                        .as_ref()
                        .is_some_and(can_react)
                    {
                        self.cancel();
                        s.notify("that message is no longer available", true);
                    } else if !p.live {
                        s.notify(draft_toast(&format!("react {emoji}")), false);
                        self.cancel();
                    } else {
                        let runtime = runtime::of(store);
                        let (id, reply) = runtime.await_reaction();
                        if !wire(
                            store,
                            &requests::add_message_reaction(p.chat, p.msg, emoji, id),
                        ) {
                            runtime.finish_reaction(
                                id,
                                ReactionResult::Error("Telegram is disconnected".into()),
                            );
                        }
                        p.reply = reply;
                        p.adding = true;
                        p.seen_reply = false;
                    }
                }
            }
        }
        s.redraw();
        true
    }
}
