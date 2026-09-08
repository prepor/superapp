//! One line of a chat, as a card: the whole of it, its media at the card's
//! width, and the verbs that act on one line — reply, forward, copy, react, pin,
//! and on a line of mine edit and delete, which are real on the store.
//!
//! Reached from the chat's bar by `line`, over the line under the cursor,
//! and joined to the chat, so a reply or an edit asked of the card lands on
//! the chat's composer: the card finds the chat it hangs under and tells
//! it.

use std::any::Any;
use std::rc::Rc;

use kernel::effect::World;
use kernel::layout::SlotId;
use kernel::nav::Nav;
use kernel::panel::{Opening, Panel, PanelId, PanelKind, Tag, Verb};
use kernel::session::Session;

use crate::shell::widgets::map;
use crate::shell::widgets::media::PlayerState;

use super::super::draft_toast;
use super::super::model::{self, Msg, MsgId, PeerId};
use super::super::{downloads, requests, runtime, verbs};
use super::chat::copy_line;
use super::reactions::{self, Reactions};
use super::playback::Playback;
use super::{Chat, Chats, Viewer};

/// A line's card.
pub struct Line {
    id: PanelId,
    chat: PeerId,
    msg: MsgId,
    world: Rc<World>,
    slot: SlotId,
    /// The card's own player, where the line has a recording.
    pub playback: Playback,
    reactions: Reactions,
}

impl Line {
    pub const TAG: Tag = Tag("line");

    /// The identity of one line's card.
    #[must_use]
    pub fn id(chat: PeerId, msg: MsgId) -> PanelId {
        PanelId::new(Self::TAG, [chat.to_string(), msg.to_string()])
    }

    /// The chat and the line a `line` panel names; `None` for any other
    /// tag.
    #[must_use]
    pub fn of(id: &PanelId) -> Option<(PeerId, MsgId)> {
        if id.tag != Self::TAG {
            return None;
        }
        Some((id.arg(0)?.parse().ok()?, id.arg(1)?.parse().ok()?))
    }

    /// The line, off the store. `None` for one the store does not have.
    #[must_use]
    pub fn msg(&self) -> Option<Msg> {
        model::line(self.world.store(), self.chat, self.msg)
    }

    fn blocked(&self) -> bool {
        model::peer(self.world.store(), self.chat).is_some_and(|c| c.blocked)
    }

    pub fn cancel_reactions(&mut self) -> bool {
        self.reactions.cancel()
    }

    pub fn poll_reactions(&mut self, s: &mut Session) -> bool {
        self.reactions.poll(s)
    }

    /// Where the player stands, for a line with a recording.
    #[must_use]
    pub fn player_state(&self, m: &Msg, now: f64) -> Option<PlayerState> {
        self.playback.player_state(m, now)
    }

    /// Play or pause in the card.
    pub fn toggle_play(&mut self, m: &Msg, now: f64) {
        self.playback.toggle_play(m, now);
    }

    /// The card's title: the writer and the time.
    fn head(&self) -> String {
        match self.msg() {
            Some(m) => format!("{} · {}", m.writer(), model::fmt_hour(m.date)),
            None => "line".to_string(),
        }
    }

    /// Runs `f` on the chat this card hangs under, if it is one, and
    /// answers that chat's slot with the result. The borrow lasts exactly
    /// as long as the call.
    fn tell_chat<R>(&self, s: &Session, f: impl FnOnce(&mut Chat) -> R) -> Option<(SlotId, R)> {
        let parent = s.join_parent_of(self.slot)?;
        let inst = s.panel(parent)?;
        let mut b = inst.borrow_mut();
        let c = b.as_any().downcast_mut::<Chat>()?;
        Some((parent, f(c)))
    }
}

impl Panel for Line {
    fn id(&self) -> &PanelId {
        &self.id
    }

    fn title(&self) -> String {
        self.head()
    }

    fn about(&self) -> String {
        format!(
            "One Telegram message as a card: its complete text or caption, author, \
             time, quoted reply, forwarding information, reactions and media metadata. \
             Its arguments are the chat id ({}, `tg_peer.id`) and message id \
             ({}, `tg_message.id`); together they identify one row of `tg_message`. \
             Message ids are only unique within a chat. The row's `topic` is the \
             forum topic id, or 0 outside a topic. The `text` and `reply_text` \
             sections contain the full cached message and quoted text with their \
             line breaks. A person reads, copies, replies, forwards or reacts here, \
             and can edit or delete their own message. Reply and edit use the \
             Telegram conversation's composer.",
            self.chat, self.msg
        )
    }

    fn context_text_columns(&self) -> &'static [&'static str] {
        &["text", "reply_text"]
    }

    /// A card: four wide, four tall — room for a picture at its width.
    fn wish(&self, _cols: usize) -> (u32, u32) {
        (4, 4)
    }

    fn placed(&mut self, slot: SlotId) {
        self.slot = slot;
    }

    /// The verbs on one line. *edit* and *delete* while it is mine; *play*
    /// or *pause* while it carries a recording; the viewer's link while it
    /// carries a picture, a video or a sound; and a place's two ways out —
    /// Apple Maps, and the map in a browser.
    fn verbs(&self) -> Vec<Verb> {
        if let Some(verbs) = self.reactions.verbs() {
            return verbs;
        }
        let m = self.msg();
        let mine = m.as_ref().is_some_and(|m| m.out);
        let mut v = Vec::new();
        if !self.blocked() {
            v.push(Verb::run("telegram.reply", "reply", Some('r')));
            if mine {
                v.push(Verb::run("telegram.edit", "edit", Some('e')));
            }
        }
        v.push(Verb::run("telegram.forward", "forward", Some('f')));
        v.push(Verb::run("telegram.copy", "copy", Some('c')));
        v.extend(m.as_ref().and_then(downloads::verb));
        if m.as_ref().is_some_and(reactions::can_react) {
            v.push(Verb::run("telegram.react", "react(j)", Some('j')));
        }
        if mine {
            v.push(Verb::run("telegram.delete", "delete", Some('d')));
        }
        v.push(Verb::run("telegram.pin", "pin", Some('p')));
        if let Some(md) = m.as_ref().and_then(|m| m.media.as_ref()) {
            match md.kind.as_str() {
                "location" | "live" => {
                    v.push(Verb::run("telegram.maps", "maps", Some('m')));
                    v.push(Verb::run("telegram.browser", "browser", Some('b')));
                }
                _ => {
                    if m.as_ref().is_some_and(|m| self.player_state(m, self.world.now()).is_some()) {
                        let playing = m.as_ref().is_some_and(|m| {
                            self.player_state(m, self.world.now()).is_some_and(|s| s.playing)
                        });
                        v.push(Verb::run(
                            "telegram.play",
                            if playing { "pause" } else { "play" },
                            Some('y'),
                        ));
                    }
                    if md.kind != "sticker" {
                        v.push(Verb::go(
                            "telegram.open",
                            "open",
                            Some('o'),
                            Nav::Open {
                                from: self.slot,
                                id: Viewer::id(self.chat, self.msg),
                                fresh: false,
                            },
                        ));
                    }
                }
            }
        }
        v
    }

    fn run(&mut self, verb: &str, s: &mut Session) {
        if self.reactions.run(self.world.store(), verb, s) {
            return;
        }
        let now = s.now();
        match verb {
            "telegram.react" => {
                if let Some(m) = self.msg() {
                    self.reactions.open(s, &m);
                    s.redraw();
                }
            }
            // The chat this card hangs under takes the reply or the edit,
            // and the keyboard with it: the composer is there, and both are
            // written.
            "telegram.reply" | "telegram.edit" => {
                if self.blocked() {
                    s.notify("unblock this user before replying or editing", false);
                    return;
                }
                let msg = self.msg;
                let told = self.tell_chat(s, |c| {
                    if verb == "telegram.reply" {
                        c.reply((self.chat, msg))
                    } else {
                        c.edit((self.chat, msg))
                    }
                });
                match told {
                    Some((p, true)) => {
                        s.nav(Nav::Focus(p));
                        s.redraw();
                    }
                    Some((_, false)) => s.notify("not a line of mine to edit", true),
                    None => s.notify(
                        if verb == "telegram.reply" {
                            "open this line from its chat to reply to it"
                        } else {
                            "open this line from its chat to edit it"
                        },
                        true,
                    ),
                }
            }
            // The line goes; the chat it hangs under steps its cursor off it
            // first. Over the wire where the build is signed in — `deleteMessages`
            // for everyone, and TDLib's echo strikes the row — else the local,
            // undoable action, never both.
            "telegram.delete" => {
                let (chat, msg) = (self.chat, self.msg);
                if super::live(self.world.store()) {
                    match super::super::history::command(s, &requests::delete_messages(chat, &[msg], true)) {
                        Ok(_) => { self.tell_chat(s, |c| c.lines_gone(&[(self.chat, msg)])); }
                        Err(error) => error.notify(s, "delete"),
                    }
                } else {
                    self.tell_chat(s, |c| c.lines_gone(&[(self.chat, msg)]));
                    verbs::delete_lines(s, chat, vec![msg]);
                }
                s.redraw();
            }
            "telegram.play" => {
                if let Some(m) = self.msg() {
                    self.toggle_play(&m, now);
                    s.redraw();
                }
            }
            "telegram.maps" | "telegram.browser" => {
                let Some((lat, lon)) = self
                    .msg()
                    .and_then(|m| m.media)
                    .and_then(|md| Some((md.lat?, md.lon?)))
                else {
                    return;
                };
                let url = if verb == "telegram.maps" {
                    map::maps_url(lat, lon)
                } else {
                    map::osm_url(lat, lon)
                };
                s.notify(draft_toast(&format!("open {url}")), false);
            }
            "telegram.copy" => {
                if let Some(m) = self.msg() {
                    copy_line(s, &m);
                }
            }
            "telegram.download" => {
                if let Some(m) = self.msg() {
                    downloads::request(s, &m);
                }
            }
            // The line waits for the chat it goes to and the list opens to be
            // picked from — the same forward the transcript's bar starts, over
            // one line rather than the marks.
            "telegram.forward" => {
                runtime::of(self.world.store()).carry_forward(self.chat, vec![self.msg]);
                s.nav(Nav::Open {
                    from: self.slot,
                    id: Chats::id(),
                    fresh: false,
                });
                s.notify("pick a chat to forward to", false);
                s.redraw();
            }
            "telegram.pin" => s.notify(draft_toast("pin"), false),
            _ => {}
        }
    }

    fn as_any(&mut self) -> &mut dyn Any {
        self
    }
}

/// Its factory.
pub struct LineKind;

impl PanelKind for LineKind {
    fn tag(&self) -> Tag {
        Line::TAG
    }

    fn open(&self, id: &PanelId, cx: &mut Opening<'_>) -> Box<dyn Panel> {
        let (chat, msg) = Line::of(id).unwrap_or_default();
        Box::new(Line {
            id: id.clone(),
            chat,
            msg,
            world: cx.session().world().clone(),
            slot: 0,
            playback: Playback::new(cx.session().store().clone(), (chat, msg)),
            reactions: Reactions::default(),
        })
    }
}
