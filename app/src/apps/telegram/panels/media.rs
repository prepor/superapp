//! The viewer: one line's media, as large as the grid allows.
//!
//! A panel that asks for the whole grid is what *full screen* is in this
//! grammar: it takes a column as wide as the screen, the camera goes to it,
//! and it is still a panel — joined to the card or the chat it came from,
//! closed with `cmd+w`, undone with `cmd+z`. `previous` and `next` walk the
//! chat's media in place, so the same panel shows the next picture.
//!
//! A picture is usually here the moment the line is: the worker fetches a
//! photo, and a moving picture's poster, on arrival — though only for the
//! newest forty lines of a chat as it opens, and the cache evicts what it
//! must, so one may well be missing. Then it is asked for by the durable
//! remote id the row keeps, the same way the clip is. The clip behind a
//! poster is never fetched on arrival — a chat full of video would
//! otherwise pull every one of them — so
//! opening the viewer on a video is what asks for it, and the panel says
//! *downloading…* until the file lands in the blob cache under the key the
//! row already names. Then the platform's own player draws and plays it, and
//! the one *play* button drives that instead of the timeline. A voice note
//! and a track still run the fake timeline against the clock; playing a
//! sound is not implemented.

use std::any::Any;
use std::path::PathBuf;
use std::rc::Rc;

use kernel::effect::World;
use kernel::layout::SlotId;
use kernel::nav::Nav;
use kernel::panel::{Opening, Panel, PanelId, PanelKind, Tag, Verb};
use kernel::session::Session;

use crate::shell::widgets::media::PlayerState;

use super::super::draft_toast;
use super::super::model::{self, Msg, MsgId, PeerId, Player};
use super::super::{requests, runtime};
use super::wire;

/// The viewer.
pub struct Viewer {
    id: PanelId,
    chat: PeerId,
    msg: MsgId,
    world: Rc<World>,
    slot: SlotId,
    /// The fake timeline, for what this build cannot really play: a voice
    /// note, a track, and every clip in a build with no engine behind it.
    player: Option<Player>,
    /// Whether the clip should be running. A verb has no `Cx` to speak to a
    /// player through, so the wish is kept here and the draw carries it out;
    /// the draw hands back what it stands at, which is how a clip that has
    /// run to its end puts the button back to *play*.
    running: bool,
    /// The line was sent for afresh, for the clip id it lacked — once.
    refreshing: bool,
    /// Whether the clip has been asked for. Once per open: the answer lands
    /// in the blob cache under the row's own key, and every draw looks there
    /// again until it does.
    asked: bool,
    /// Whether the picture itself has been asked for — the photo, or the
    /// poster a moving picture is opened on. Once per open, for the same
    /// reason.
    wanted_pic: bool,
}

impl Viewer {
    pub const TAG: Tag = Tag("media");

    /// The identity of the viewer over one line's media.
    #[must_use]
    pub fn id(chat: PeerId, msg: MsgId) -> PanelId {
        PanelId::new(Self::TAG, [chat.to_string(), msg.to_string()])
    }

    /// The chat and the line a `media` panel names; `None` for any other
    /// tag.
    #[must_use]
    pub fn of(id: &PanelId) -> Option<(PeerId, MsgId)> {
        if id.tag != Self::TAG {
            return None;
        }
        Some((id.arg(0)?.parse().ok()?, id.arg(1)?.parse().ok()?))
    }

    /// The line, off the store.
    #[must_use]
    pub fn msg(&self) -> Option<Msg> {
        model::line(self.world.store(), self.chat, self.msg)
    }

    /// The line's neighbours among the chat's media: the one before and
    /// the one after, where there are any.
    #[must_use]
    pub fn neighbours(&self) -> (Option<MsgId>, Option<MsgId>) {
        let ids = model::media_ids(self.world.store(), self.chat);
        let Some(i) = ids.iter().position(|id| *id == self.msg) else {
            return (None, None);
        };
        (
            i.checked_sub(1).and_then(|j| ids.get(j).copied()),
            ids.get(i + 1).copied(),
        )
    }

    /// Where the player stands, for a line with something to play.
    ///
    /// A clip's real position and length are the platform player's, and the
    /// draw reads them off it; this is what stands in until there is a
    /// player to read — the wish, over the length the row itself knows. A
    /// sound has no player at all yet, so its timeline is the fake one,
    /// ticked against the clock.
    #[must_use]
    pub fn player_state(&self, m: &Msg, now: f64) -> Option<PlayerState> {
        let md = m.media.as_ref()?;
        if self.plays_clip(m) {
            return Some(PlayerState {
                playing: self.running,
                position: 0.0,
                length: md.secs.unwrap_or(0) as f64,
            });
        }
        let secs = md.secs?;
        Some(match self.player {
            Some(p) => p.state(now),
            None => PlayerState {
                playing: false,
                position: 0.0,
                length: secs as f64,
            },
        })
    }

    /// The clip's file on this device: where the download landed in the blob
    /// cache, or `None` while it has not. Every draw asks again, which is how
    /// the poster gives way to the player the moment the bytes are here.
    #[must_use]
    pub fn clip_file(&self, m: &Msg) -> Option<PathBuf> {
        model::playable_path(self.world.store().dir(), m.media.as_ref()?.clip.as_deref()?)
    }

    /// The file on this device to hand the system — the clip where it has
    /// landed, else the picture — or `None` with nothing here yet.
    #[must_use]
    pub fn file_to_open(&self, m: &Msg) -> Option<PathBuf> {
        self.clip_file(m).or_else(|| {
            model::media_path(self.world.store().dir(), m.media.as_ref()?.reference.as_deref()?)
        })
    }

    /// Whether the clip is this panel's to play: the line is a moving
    /// picture of the wire's — never the demo's — and either its file is
    /// already here, or it has been asked for, or the line itself is being
    /// fetched afresh for the id to ask by. A demo line and a build with no
    /// engine keep the poster and the fake timeline the panels library
    /// draws; a real video never runs the fake timeline, which would only
    /// count seconds over a still (Andrey, 2026-09-07: "the seconds update
    /// but nothing plays").
    #[must_use]
    pub fn plays_clip(&self, m: &Msg) -> bool {
        moving_picture_of_the_wire(m)
            && (self.asked || self.refreshing || self.clip_file(m).is_some())
    }

    /// What the viewer says while there is nothing to play yet. `None` once
    /// the file is here — the player says the rest from there — and for every
    /// line that is not a clip's.
    #[must_use]
    pub fn clip_note(&self, m: &Msg) -> Option<&'static str> {
        (self.plays_clip(m) && self.clip_file(m).is_none()).then_some("downloading…")
    }

    /// Asks the engine for the clip, once.
    ///
    /// A clip is never fetched on arrival — the transcript draws its poster
    /// and no more (Andrey, 2026-09-06) — so the file behind a video line is
    /// asked for here, when somebody opens the viewer on it or presses play.
    /// The request is a `getRemoteFile` on the row's durable remote id; the
    /// worker turns the answer into a download at the front of the queue, and
    /// the finished file lands in the blob cache under the row's own `clip`
    /// key, where [`clip_file`](Self::clip_file) is already looking. Nothing
    /// happens where the file is here, where the row names no clip, or where
    /// this build has no engine to ask.
    ///
    /// A line from before the remote id was kept on the row has none to ask
    /// by; that line is fetched afresh from the engine ([`runtime::Runtime::want_line`])
    /// and re-projected, and the next draw finds the id and asks.
    pub fn ask_for_clip(&mut self, m: &Msg) {
        if self.asked || self.clip_file(m).is_some() {
            return;
        }
        let Some(rid) = m.media.as_ref().and_then(|md| md.clip_rid.as_deref()) else {
            if moving_picture_of_the_wire(m) && !self.refreshing {
                self.refreshing = true;
                runtime::of(self.world.store()).want_line(m.chat, m.id);
            }
            return;
        };
        self.asked = wire(self.world.store(), &requests::request_file(rid));
    }

    /// Asks the engine for the picture, once.
    ///
    /// A photo is fetched as its line arrives — but only for the newest forty
    /// lines of a chat as it opens, and the blob cache evicts what it must,
    /// so a picture opened on may well have no bytes on this device at all.
    /// The row keeps the file's durable remote id for exactly this
    /// ([`Media::rid`](model::Media::rid)): the worker turns it into a
    /// download on its next pass ([`runtime::Runtime::want_file`]), which lands under the
    /// row's own key, where the draw is already looking. Nothing happens
    /// where the bytes are here, or where the row names no id — a demo line,
    /// or one projected before the column existed.
    pub fn ask_for_picture(&mut self, m: &Msg) {
        if self.wanted_pic {
            return;
        }
        let Some(md) = m.media.as_ref() else { return };
        // Only a kind that draws as a picture is waited for: a file's or a
        // sound's bytes would never decode into the box, and a viewer
        // waiting on them would draw forever (review, 2026-09-07).
        if !md.has_picture() || md.picture_bytes(self.world.store().dir()).is_some() {
            return;
        }
        let Some(rid) = md.rid.as_deref() else {
            // A row from before the id was kept: fetched afresh, once, and
            // asked for on the draw that finds the id.
            let real = md.reference.as_deref().is_some_and(|r| r.starts_with("tg:"));
            if real && !self.refreshing {
                self.refreshing = true;
                runtime::of(self.world.store()).want_line(m.chat, m.id);
            }
            return;
        };
        self.wanted_pic = true;
        runtime::of(self.world.store()).want_file(rid);
    }

    /// Whether a picture was asked for and has not landed — what keeps the
    /// viewer drawing until it does, a file appearing under the cache's name
    /// announcing itself to nobody.
    #[must_use]
    pub fn awaiting_picture(&self, m: &Msg) -> bool {
        self.wanted_pic
            && m.media
                .as_ref()
                .is_some_and(|md| md.picture_bytes(self.world.store().dir()).is_none())
    }

    /// Play or pause. Pressing play on a clip is also the asking, since a
    /// clip nobody has opened was never downloaded; the wish is all a verb
    /// can set — a player is only reachable where there is a `Cx`, which is
    /// the draw. Everything else toggles the fake timeline against the clock.
    pub fn toggle_play(&mut self, m: &Msg, now: f64) {
        self.ask_for_clip(m);
        if self.plays_clip(m) {
            self.running = !self.running;
            return;
        }
        let Some(secs) = m.media.as_ref().and_then(|md| md.secs) else {
            return;
        };
        let mut p = self.player.unwrap_or_else(|| Player::over(m.id, secs as f64));
        p.toggle(now);
        self.player = Some(p);
    }

    /// Seek the demo or audio timeline; a real clip is sought by the widget's
    /// native player, which needs a `Cx`.
    pub fn seek(&mut self, m: &Msg, position: f64, now: f64) {
        if self.plays_clip(m) {
            return;
        }
        let Some(secs) = m.media.as_ref().and_then(|md| md.secs) else { return };
        let mut p = self.player.unwrap_or_else(|| Player::over(m.id, secs as f64));
        p.seek(position, now);
        self.player = Some(p);
    }

    /// The wish the draw carries out over the clip's player.
    #[must_use]
    pub fn running(&self) -> bool {
        self.running
    }

    /// What the draw found the player at afterwards — `false` where the clip
    /// has run to its end, which is what puts the button back to *play*.
    pub fn set_running(&mut self, running: bool) {
        self.running = running;
    }

    /// Whether anything runs — what asks for the next frame.
    #[must_use]
    pub fn playing(&self, now: f64) -> bool {
        self.running || self.player.is_some_and(|p| p.state(now).playing)
    }
}

impl Panel for Viewer {
    fn id(&self) -> &PanelId {
        &self.id
    }

    /// *photo · Vera Kovac · 16:40*.
    fn title(&self) -> String {
        match self.msg() {
            Some(m) => format!(
                "{} · {} · {}",
                m.media.as_ref().map_or("media", |md| md.word()),
                m.writer(),
                model::fmt_hour(m.date)
            ),
            None => "media".to_string(),
        }
    }

    /// The whole grid: full screen, in a workspace of columns.
    fn wish(&self, _cols: usize) -> (u32, u32) {
        (12, 6)
    }

    fn placed(&mut self, slot: SlotId) {
        self.slot = slot;
    }

    /// The walk through the chat's media in place, the player's one button,
    /// and the system's own opener.
    fn verbs(&self) -> Vec<Verb> {
        let (prev, next) = self.neighbours();
        let m = self.msg();
        let mut v = Vec::new();
        if let Some(p) = prev {
            v.push(Verb::go(
                "telegram.previous",
                "previous",
                Some('p'),
                Nav::Replace {
                    slot: self.slot,
                    id: Viewer::id(self.chat, p),
                },
            ));
        }
        if let Some(n) = next {
            v.push(Verb::go(
                "telegram.next",
                "next",
                Some('n'),
                Nav::Replace {
                    slot: self.slot,
                    id: Viewer::id(self.chat, n),
                },
            ));
        }
        // The one button is wherever there is something to play: a
        // recording, or a clip this build can really play.
        if let Some(st) = m.as_ref().and_then(|m| self.player_state(m, self.world.now())) {
            v.push(Verb::run(
                "telegram.play",
                if st.playing { "pause" } else { "play" },
                Some('y'),
            ));
        }
        v.push(Verb::run("telegram.open", "open", Some('o')));
        v
    }

    fn run(&mut self, verb: &str, s: &mut Session) {
        let now = s.now();
        match verb {
            "telegram.play" => {
                if let Some(m) = self.msg() {
                    self.toggle_play(&m, now);
                    s.redraw();
                }
            }
            // The system's own player, which is the sure way to see a clip:
            // what the in-app player cannot show is still a file on disk.
            "telegram.open" => {
                let file = self.msg().and_then(|m| self.file_to_open(&m));
                match file {
                    Some(path) if cfg!(target_os = "macos") => {
                        let _ = super::super::operations::run_local(
                            self.world.store(),
                            Some(self.chat),
                            "opening media",
                            move || {
                                let output = std::process::Command::new("open")
                                    .arg(&path)
                                    .output()
                                    .map_err(|e| e.to_string())?;
                                if output.status.success() {
                                    Ok(())
                                } else {
                                    Err(format!(
                                        "{}: {}",
                                        output.status,
                                        String::from_utf8_lossy(&output.stderr).trim()
                                    ))
                                }
                            },
                        );
                        s.redraw();
                    }
                    Some(_) => s.notify(draft_toast("open with the system"), false),
                    None => s.notify("nothing here to open yet".to_string(), false),
                }
            }
            _ => {}
        }
    }

    fn as_any(&mut self) -> &mut dyn Any {
        self
    }
}

/// Whether a line is a moving picture that came over the wire — a video, a
/// circle or an animation whose poster is a `tg:` reference — as against
/// the demo world's, which are bundled stills with a fake timeline.
fn moving_picture_of_the_wire(m: &Msg) -> bool {
    m.media.as_ref().is_some_and(|md| {
        matches!(md.kind.as_str(), "video" | "circle" | "animation")
            && md.reference.as_deref().is_some_and(|r| r.starts_with("tg:"))
    })
}

/// Its factory.
pub struct ViewerKind;

impl PanelKind for ViewerKind {
    fn tag(&self) -> Tag {
        Viewer::TAG
    }

    fn open(&self, id: &PanelId, cx: &mut Opening<'_>) -> Box<dyn Panel> {
        let (chat, msg) = Viewer::of(id).unwrap_or_default();
        Box::new(Viewer {
            id: id.clone(),
            chat,
            msg,
            world: cx.session().world().clone(),
            slot: 0,
            player: None,
            // Opened from a line's own play button, it plays as it opens.
            running: runtime::of(cx.session().store()).take_play_on_open(chat, msg),
            refreshing: false,
            asked: false,
            wanted_pic: false,
        })
    }
}
