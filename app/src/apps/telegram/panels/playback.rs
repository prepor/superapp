//! Playback shared by a transcript, a message card and the media viewer.
//! Downloads start on play (or when the viewer opens), and native video
//! positions come back from the widget rather than advancing a fake clock.

use std::path::PathBuf;
use std::rc::Rc;
use std::sync::{Arc, Mutex, Weak};

use kernel::store::Store;

use crate::shell::widgets::media::PlayerState;
use super::super::model::{self, Msg, MsgKey, Player};
use super::super::{requests, runtime};
use super::wire;

/// Panels over the same store share playback ownership, including separate
/// cards of the same message. A weak reference never keeps a closed panel alive.
#[derive(Default)]
struct ActivePlayback(Mutex<Weak<Mutex<Transport>>>);

#[derive(Default)]
struct Transport {
    player: Option<Player>,
    running: bool,
}

impl Transport {
    fn pause(&mut self, now: f64) {
        self.running = false;
        if let Some(p) = self.player.as_mut().filter(|p| p.state(now).playing) {
            p.toggle(now);
        }
    }
}

pub struct Playback {
    store: Rc<Store>,
    pub msg: MsgKey,
    transport: Arc<Mutex<Transport>>,
    active: Arc<ActivePlayback>,
    native: PlayerState,
    asked: bool,
    wanted_pic: bool,
}

impl Playback {
    pub fn new(store: Rc<Store>, msg: MsgKey) -> Self {
        let active = store.local();
        Self { store, msg, transport: Arc::default(), active, native: PlayerState::default(),
            asked: false, wanted_pic: false }
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
        let transport = self.transport.lock().expect("playback transport");
        if self.plays_clip(m) {
            return Some(PlayerState {
                playing: transport.running,
                position: self.native.position,
                length: if self.native.length > 0.0 { self.native.length } else { md.secs.unwrap_or(0) as f64 },
            });
        }
        let secs = md.secs.or_else(|| moving_picture_of_the_wire(m).then_some(0))?;
        Some(match transport.player {
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
        model::playable_path(self.store.dir(), m.media.as_ref()?.clip.as_deref()?)
    }

    /// The file on this device to hand the system — the clip where it has
    /// landed, else the picture — or `None` with nothing here yet.
    #[must_use]
    pub fn file_to_open(&self, m: &Msg) -> Option<PathBuf> {
        self.clip_file(m).or_else(|| {
            model::media_path(self.store.dir(), m.media.as_ref()?.reference.as_deref()?)
        })
    }

    /// Whether the clip is this panel's to play: the line is a moving
    /// picture of the wire's — never the demo's — and either its file is
    /// already here, or its source message and clip have been asked for.
    /// A demo line and a build with no
    /// engine keep the poster and the fake timeline the panels library
    /// draws; a real video never runs the fake timeline, which would only
    /// count seconds over a still (Andrey, 2026-09-07: "the seconds update
    /// but nothing plays").
    #[must_use]
    pub fn plays_clip(&self, m: &Msg) -> bool {
        moving_picture_of_the_wire(m)
            && (self.asked || self.clip_file(m).is_some())
    }

    /// Downloaded and total bytes for the clip or picture this viewer is
    /// waiting on. A clip takes precedence over its poster; once the clip
    /// is here, the player says the rest.
    #[must_use]
    pub fn download_note(&self, m: &Msg) -> Option<String> {
        let md = m.media.as_ref()?;
        let reference = if self.plays_clip(m) {
            if self.clip_file(m).is_some() {
                return None;
            }
            md.clip.as_deref()
        } else if self.awaiting_picture(m) {
            md.reference.as_deref()
        } else {
            return None;
        };
        let state = runtime::of(&self.store);
        if let Some(note) = state.connection_note() {
            return Some(note);
        }
        let context = requests::media_context(m.chat, m.id, self.plays_clip(m));
        if let Some(note) = state.operations.media_note(&context) {
            return Some(note);
        }
        Some(
            reference
                .and_then(|key| state.download(key))
                .map_or_else(|| "downloading…".to_string(), |progress| progress.note()),
        )
    }

    /// Fetch the source message in this session before asking for its clip.
    /// Remote file ids survive restarts, but their expiring file references
    /// need a source TDLib can refresh. The cache still answers immediately.
    pub fn ask_for_clip(&mut self, m: &Msg) {
        if self.asked || !moving_picture_of_the_wire(m) || self.clip_file(m).is_some() {
            return;
        }
        self.asked = wire(&self.store, &requests::request_media(m.chat, m.id, true));
    }

    /// Restore a missing photo through its source message too. A clip request
    /// already fetches its poster, so it needs no second message request.
    pub fn ask_for_picture(&mut self, m: &Msg) {
        if self.wanted_pic || self.asked {
            return;
        }
        let Some(md) = m.media.as_ref() else { return };
        if !md.has_picture() || md.picture_bytes(self.store.dir()).is_some()
            || !md.reference.as_deref().is_some_and(|r| r.starts_with("tg:"))
        {
            return;
        }
        self.wanted_pic = wire(&self.store, &requests::request_media(m.chat, m.id, false));
    }

    /// Whether a picture was asked for and has not landed — what keeps the
    /// viewer drawing until it does, a file appearing under the cache's name
    /// announcing itself to nobody.
    #[must_use]
    pub fn awaiting_picture(&self, m: &Msg) -> bool {
        self.wanted_pic
            && m.media
                .as_ref()
                .is_some_and(|md| md.picture_bytes(self.store.dir()).is_none())
    }

    /// Play or pause. Pressing play on a clip is also the asking, since a
    /// clip nobody has opened was never downloaded; the wish is all a verb
    /// can set — a player is only reachable where there is a `Cx`, which is
    /// the draw. Everything else toggles the fake timeline against the clock.
    pub fn toggle_play(&mut self, m: &Msg, now: f64) {
        self.ask_for_clip(m);
        if self.playing(now) {
            self.pause(now);
            return;
        }
        let native = self.plays_clip(m);
        let secs = m.media.as_ref().and_then(|md| md.secs);
        if !native && secs.is_none() { return; }
        let mut active = self.active.0.lock().expect("active playback");
        if let Some(previous) = active.upgrade().filter(|p| !Arc::ptr_eq(p, &self.transport)) {
            previous.lock().expect("playback transport").pause(now);
        }
        let mut transport = self.transport.lock().expect("playback transport");
        if native {
            transport.running = true;
        } else {
            let p = transport.player.get_or_insert_with(|| Player::over(m.id, secs.unwrap() as f64));
            p.toggle(now);
        }
        *active = Arc::downgrade(&self.transport);
    }

    /// Seek the demo or audio timeline; a real clip is sought by the widget's
    /// native player, which needs a `Cx`.
    pub fn seek(&mut self, m: &Msg, position: f64, now: f64) {
        if self.plays_clip(m) {
            return;
        }
        let Some(secs) = m.media.as_ref().and_then(|md| md.secs) else { return };
        let mut transport = self.transport.lock().expect("playback transport");
        let p = transport.player.get_or_insert_with(|| Player::over(m.id, secs as f64));
        p.seek(position, now);
    }

    /// The wish the draw carries out over the clip's player.
    #[must_use]
    pub fn running(&self) -> bool {
        self.transport.lock().expect("playback transport").running
    }

    /// What the draw found the player at afterwards — `false` where the clip
    /// has run to its end, which is what puts the button back to *play*.
    pub fn set_running(&mut self, running: bool) {
        let owns_playback = self.active.0.lock().expect("active playback")
            .upgrade().is_some_and(|active| Arc::ptr_eq(&active, &self.transport));
        self.transport.lock().expect("playback transport").running = running && owns_playback;
    }

    /// Keep the native position for controls and verbs between draws.
    pub fn set_native_state(&mut self, state: PlayerState) {
        self.native = state;
        self.set_running(state.playing);
    }

    pub fn pause(&mut self, now: f64) {
        self.transport.lock().expect("playback transport").pause(now);
    }

    /// Whether anything runs — what asks for the next frame.
    #[must_use]
    pub fn playing(&self, now: f64) -> bool {
        let transport = self.transport.lock().expect("playback transport");
        transport.running || transport.player.is_some_and(|p| p.state(now).playing)
    }
}

/// Whether a line is a moving picture that came over the wire — a video, a
/// circle or an animation whose clip or poster is a `tg:` reference — as against
/// the demo world's, which are bundled stills with a fake timeline.
pub fn moving_picture_of_the_wire(m: &Msg) -> bool {
    m.media.as_ref().is_some_and(|md| {
        matches!(md.kind.as_str(), "video" | "circle" | "animation")
            && [md.clip.as_deref(), md.reference.as_deref()]
                .into_iter()
                .flatten()
                .any(|r| r.starts_with("tg:"))
    })
}
