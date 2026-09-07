//! What goes with the next message, and the ways to make more of it.
//!
//! The chat's bar carries `attach`, one link, and this panel behind it
//! carries the sending: the files the composer will send with the text —
//! added from what the files app holds, removed, put in the order they will
//! go — a voice note or a video message recorded here, and the way to the
//! place to send. Gathered here so the chat's bar stays what a chat is for.
//!
//! The list is the chat's own: the composer sends it and shows it on its
//! `CARRIES` line. This panel edits it through the join, as the line's card
//! replies through it, and opened away from its chat it says so. A
//! recording is the panel's own: the strip stands here, `enter` sends it and
//! `esc` throws it away.

use std::any::Any;
use std::rc::Rc;

use kernel::layout::SlotId;
use kernel::nav::Nav;
use kernel::panel::{Opening, Panel, PanelId, PanelKind, Tag, Verb};
use kernel::session::Session;
use kernel::store::Store;

use crate::apps::files::Files;

use super::super::draft_toast;
use super::super::model::{self, Carried, PeerId, RecKind, Recording};
use super::{Chat, Place};

/// The files app's directory panel, where a file is held from. Named by
/// tag rather than by app: a build without it gets the shell's missing
/// card, which says whose panel it would have been.
const FILES_TAG: Tag = Tag("files");

/// The attach panel.
pub struct Attach {
    id: PanelId,
    chat: PeerId,
    store: Rc<Store>,
    slot: SlotId,
    /// The chat's list as it stood when the widget last looked: the bar is
    /// built without the session.
    items: Vec<Carried>,
    /// Whether a chat stands behind the join.
    joined: bool,
    /// The row the arrows walk, as an index into the list.
    cursor: Option<usize>,
    /// What the files app was holding when the widget last looked; *add*
    /// comes and goes with it.
    held: Vec<String>,
    /// A recording under way.
    recording: Option<Recording>,
}

impl Attach {
    pub const TAG: Tag = Tag("attach");

    /// The identity of the attach panel of one chat.
    #[must_use]
    pub fn id(chat: PeerId) -> PanelId {
        PanelId::new(Self::TAG, [chat.to_string()])
    }

    pub fn in_topic(chat: PeerId, topic: i64) -> PanelId {
        if topic == 0 { return Self::id(chat); }
        PanelId::new(Self::TAG, [chat.to_string(), topic.to_string()])
    }

    /// The chat an `attach` panel is for; `None` for any other tag.
    #[must_use]
    pub fn of(id: &PanelId) -> Option<PeerId> {
        (id.tag == Self::TAG)
            .then(|| id.arg(0)?.parse().ok())
            .flatten()
    }

    /// The chat's title.
    #[must_use]
    pub fn chat_title(&self) -> String {
        let topic = self.id.arg(1).and_then(|s| s.parse().ok()).unwrap_or(0);
        super::super::topics::card(&self.store, self.chat, topic).map_or_else(|| "chat".to_string(), |c| c.name)
    }

    /// What the composer will send with the text, in the order it will go.
    #[must_use]
    pub fn items(&self) -> &[Carried] {
        &self.items
    }

    /// Whether a chat stands behind the join, at the last look.
    #[must_use]
    pub fn joined(&self) -> bool {
        self.joined
    }

    #[must_use]
    pub fn cursor(&self) -> Option<usize> {
        self.cursor
    }

    pub fn set_cursor(&mut self, i: usize) {
        if i < self.items.len() {
            self.cursor = Some(i);
        }
    }

    /// Steps the cursor over the list; from nothing, either way lands on the
    /// first row. Answers where it landed.
    pub fn walk(&mut self, d: isize) -> Option<usize> {
        if self.items.is_empty() {
            self.cursor = None;
            return None;
        }
        let last = self.items.len() as isize - 1;
        let at = match self.cursor {
            Some(i) => (i as isize + d).clamp(0, last) as usize,
            None => 0,
        };
        self.cursor = Some(at);
        self.cursor
    }

    /// What the files app holds, said directly: a test's way round the
    /// clipboard, which every test in the binary shares.
    #[cfg(test)]
    pub fn set_held(&mut self, paths: Vec<String>) {
        self.held = paths;
    }

    /// A recording under way, if one.
    #[must_use]
    pub fn recording(&self) -> Option<Recording> {
        self.recording
    }

    /// Starts recording, from the clock's now.
    pub fn start_recording(&mut self, kind: RecKind, now: f64) {
        self.recording = Some(Recording { kind, since: now });
    }

    /// Throws a recording away.
    pub fn cancel_recording(&mut self) {
        self.recording = None;
    }

    /// Enter on a recording: it goes. Nothing leaves this round; the toast
    /// says what would.
    pub fn send_recording(&mut self, s: &mut Session, now: f64) {
        let Some(r) = self.recording.take() else { return };
        s.notify(
            draft_toast(&format!(
                "{} {}",
                r.kind.word(),
                model::fmt_secs(r.elapsed(now).floor() as i64)
            )),
            false,
        );
        s.redraw();
    }

    /// Looks at what this panel cannot ask for while it builds its bar: the
    /// chat's list through the join, and what the files app holds. Called
    /// by the widget at the top of every draw and event, and after every
    /// verb that changed the list, so the bar never speaks of a row that is
    /// gone. A build without the files app holds nothing.
    pub fn observe(&mut self, s: &Session) {
        self.held = s
            .apps()
            .get_as::<Files>()
            .map(|f| f.clipboard().paths)
            .unwrap_or_default();
        let items = self.with_chat(s, |c| c.carrying().to_vec());
        self.joined = items.is_some();
        self.items = items.unwrap_or_default();
        self.cursor = match self.cursor {
            Some(_) if self.items.is_empty() => None,
            Some(i) => Some(i.min(self.items.len() - 1)),
            None => None,
        };
    }

    /// Runs `f` on the chat this panel hangs under, if it is one. The
    /// borrow lasts exactly as long as the call.
    fn with_chat<R>(&self, s: &Session, f: impl FnOnce(&mut Chat) -> R) -> Option<R> {
        let parent = s.join_parent_of(self.slot)?;
        let inst = s.panel(parent)?;
        let mut b = inst.borrow_mut();
        let c = b.as_any().downcast_mut::<Chat>()?;
        Some(f(c))
    }

    /// What the panel says when no chat stands behind the join.
    fn orphan(s: &mut Session) {
        s.notify("open this from its chat to attach to it", true);
    }
}

impl Panel for Attach {
    fn id(&self) -> &PanelId {
        &self.id
    }

    /// *attach · Vera Kovac*.
    fn title(&self) -> String {
        format!("attach · {}", self.chat_title())
    }

    /// A card with a list in it.
    fn wish(&self, _cols: usize) -> (u32, u32) {
        (4, 4)
    }

    fn placed(&mut self, slot: SlotId) {
        self.slot = slot;
    }

    /// While a recording runs, its two ways out and nothing else. At rest:
    /// *browse*, a link to the files panel, always; *add* beside it while
    /// the files app holds something — the link keeps its place and the
    /// button comes and goes; the verbs on the cursor's row — *remove*, and
    /// *earlier* and *later* where there is a row to trade with, spelled by
    /// the order they will go rather than by the screen; the two
    /// recordings; and the place, a link.
    ///
    /// *add* wears `d` because *later* is `a`; *voice* wears `o` because
    /// `v` is the video message's.
    fn verbs(&self) -> Vec<Verb> {
        if self.recording.is_some() {
            return vec![
                Verb::run("telegram.send_rec", "send", Some('s')),
                Verb::run("telegram.discard", "discard", Some('d')),
            ];
        }
        let mut v = vec![Verb::go(
            "telegram.browse",
            "browse",
            Some('b'),
            Nav::Open {
                from: self.slot,
                id: PanelId::new(FILES_TAG, ["~"]),
                fresh: false,
            },
        )];
        if !self.held.is_empty() {
            let k = self.held.len();
            v.push(Verb::run(
                "telegram.add",
                if k == 1 { "add".to_string() } else { format!("add {k}") },
                Some('d'),
            ));
        }
        if let Some(i) = self.cursor.filter(|i| *i < self.items.len()) {
            v.push(Verb::run("telegram.remove", "remove", Some('r')));
            if i > 0 {
                v.push(Verb::run("telegram.earlier", "earlier", Some('e')));
            }
            if i + 1 < self.items.len() {
                v.push(Verb::run("telegram.later", "later", Some('a')));
            }
        }
        v.push(Verb::run("telegram.voice", "voice", Some('o')));
        v.push(Verb::run("telegram.video", "video", Some('v')));
        v.push(Verb::go(
            "telegram.place",
            "place",
            Some('p'),
            Nav::Open {
                from: self.slot,
                id: Place::in_topic(self.chat, self.id.arg(1).and_then(|s| s.parse().ok()).unwrap_or(0)),
                fresh: false,
            },
        ));
        v
    }

    fn run(&mut self, verb: &str, s: &mut Session) {
        let now = s.now();
        if super::live(&self.store) && matches!(verb, "telegram.voice" | "telegram.video") {
            let error =
                "Recording is not available yet. Attach a recorded audio or video file instead.";
            super::super::runtime::of(&self.store).operations.report(
                &self.store,
                "recording",
                error,
            );
            s.notify(error, true);
            return;
        }
        match verb {
            // What the files app holds goes to the chat's list, by path:
            // the send reads it as the message leaves, as a letter does.
            // The clipboard is not consumed — a copy is a copy.
            "telegram.add" => {
                let held = self.held.clone();
                let Some((added, len)) = self.with_chat(s, |c| {
                    let added = c.carry(&held);
                    (added, c.carrying().len())
                }) else {
                    Self::orphan(s);
                    return;
                };
                s.notify(
                    if added == 0 {
                        "already carrying it".to_string()
                    } else {
                        format!("carrying {added} file{}", if added == 1 { "" } else { "s" })
                    },
                    false,
                );
                if added > 0 {
                    self.cursor = Some(len - 1);
                }
                self.observe(s);
                s.redraw();
            }
            "telegram.remove" => {
                let Some(i) = self.cursor else { return };
                if self.with_chat(s, |c| c.uncarry(i)).is_none() {
                    Self::orphan(s);
                    return;
                }
                self.observe(s);
                s.redraw();
            }
            "telegram.earlier" | "telegram.later" => {
                let Some(i) = self.cursor else { return };
                let d: isize = if verb == "telegram.earlier" { -1 } else { 1 };
                match self.with_chat(s, |c| c.move_carried(i, d)) {
                    Some(Some(j)) => self.cursor = Some(j),
                    Some(None) => {}
                    None => {
                        Self::orphan(s);
                        return;
                    }
                }
                self.observe(s);
                s.redraw();
            }
            "telegram.voice" => {
                self.start_recording(RecKind::Voice, now);
                s.redraw();
            }
            "telegram.video" => {
                self.start_recording(RecKind::Video, now);
                s.redraw();
            }
            "telegram.send_rec" => self.send_recording(s, now),
            "telegram.discard" => {
                self.cancel_recording();
                s.redraw();
            }
            _ => {}
        }
    }

    fn as_any(&mut self) -> &mut dyn Any {
        self
    }
}

/// Its factory.
pub struct AttachKind;

impl PanelKind for AttachKind {
    fn tag(&self) -> Tag {
        Attach::TAG
    }

    fn open(&self, id: &PanelId, cx: &mut Opening<'_>) -> Box<dyn Panel> {
        Box::new(Attach {
            chat: Attach::of(id).unwrap_or_default(),
            id: id.clone(),
            store: cx.session().store().clone(),
            slot: 0,
            items: Vec::new(),
            joined: false,
            cursor: None,
            held: Vec::new(),
            recording: None,
        })
    }
}
