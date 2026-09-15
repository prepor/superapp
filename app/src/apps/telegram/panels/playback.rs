//! Playback shared by a transcript, a message card and the media viewer:
//! the shell's [`Transport`] under what is Telegram's — asking for a clip
//! through TDLib, the download note, and the rule that a demo line runs the
//! clock timeline instead. Downloads start on play (or when the viewer
//! opens), and positions come back from the player rather than advancing a
//! fake clock.
//!
//! Two things a line can really play: a moving picture, which the platform
//! plays, and a voice note, which on the Mac the kit plays itself — Ogg
//! Opus has no decoder there ([`OpusClip`]). Both are fetched the moment
//! *play* is pressed and neither before, and to this panel they differ only
//! in which reference names the bytes: a clip's is `clip`, a note's is the
//! `reference` the line arrived with. A demo line keeps its fake timeline.
//!
//! [`OpusClip`]: crate::shell::widgets::media::OpusClip

use std::path::PathBuf;
use std::rc::Rc;

use kernel::store::Store;

use crate::shell::widgets::media::{PlayerState, Transport};
use super::super::model::{Msg, MsgKey};
use super::super::{media_cache, requests, runtime};
use super::wire;

pub struct Playback {
    store: Rc<Store>,
    pub msg: MsgKey,
    transport: Transport,
    asked: bool,
    wants_clip: bool,
    wanted_pic: bool,
    /// Whether the note's own bytes have been asked for. A voice note is
    /// never fetched on arrival, so pressing *play* is the asking.
    wanted_sound: bool,
}

impl Playback {
    pub fn new(store: Rc<Store>, msg: MsgKey) -> Self {
        let transport = Transport::new(&store);
        Self {
            store,
            msg,
            transport,
            asked: false,
            wants_clip: false,
            wanted_pic: false,
            wanted_sound: false,
        }
    }

    /// Where the player over a line stands, for a line with something to play.
    ///
    /// A clip's or a note's real position and length are its player's, and
    /// the draw reads them off it; this is what stands in until there is a
    /// player to read — the wish, over the length the row itself knows.
    /// Everything else — a demo line — has no player at all, so its
    /// timeline is the clock's.
    #[must_use]
    pub fn player_state(&self, m: &Msg, now: f64) -> Option<PlayerState> {
        let md = m.media.as_ref()?;
        if self.plays_clip(m) || plays_sound(m) {
            return Some(self.transport.state(now, md.secs.unwrap_or(0) as f64));
        }
        let secs = md.secs.or_else(|| moving_picture_of_the_wire(m).then_some(0))?;
        Some(if self.transport.on_timeline() {
            self.transport.state(now, secs as f64)
        } else {
            PlayerState {
                playing: false,
                position: 0.0,
                length: secs as f64,
            }
        })
    }

    /// The clip's file on this device: where the download landed in the blob
    /// cache, or `None` while preparation is pending. Draws share the prepared
    /// path; download completion wakes the cache without opening files here.
    #[must_use]
    pub fn clip_file(&self, m: &Msg) -> Option<PathBuf> {
        media_cache::read(&self.store, m.media.as_ref()?.clip.as_deref()?, true).paths()?.playable
    }

    /// The voice note's file on this device, under the link that carries
    /// the extension its bytes say it is — which is what the kit reads to
    /// know the platform will not play it.
    #[must_use]
    pub fn sound_file(&self, m: &Msg) -> Option<PathBuf> {
        if !plays_sound(m) {
            return None;
        }
        media_cache::read(&self.store, m.media.as_ref()?.reference.as_deref()?, true).paths()?.playable
    }

    /// The file the driver is pointed at: a clip's, or a voice note's.
    #[must_use]
    pub fn playable(&self, m: &Msg) -> Option<PathBuf> {
        if self.plays_clip(m) {
            self.clip_file(m)
        } else {
            self.sound_file(m)
        }
    }

    /// The file on this device to hand the system — the clip where it has
    /// landed, else the picture — or `None` with nothing here yet.
    #[must_use]
    pub fn file_to_open(&self, m: &Msg) -> Option<PathBuf> {
        self.clip_file(m).or_else(|| {
            media_cache::read(&self.store, m.media.as_ref()?.reference.as_deref()?, false).paths()?.file
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
        if !moving_picture_of_the_wire(m) { return false; }
        let Some(reference) = m.media.as_ref().and_then(|media| media.clip.as_deref()) else { return false; };
        match media_cache::read(&self.store, reference, true) {
            media_cache::Reading::Pending(previous) => self.wants_clip || self.asked
                || previous.is_some_and(|paths| paths.playable.is_some()),
            media_cache::Reading::Ready(paths) => self.asked || paths.playable.is_some(),
        }
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
        } else if plays_sound(m) {
            // A note nobody has played was never fetched; once it is here
            // the player says the rest.
            if !self.wanted_sound || self.sound_file(m).is_some() {
                return None;
            }
            md.reference.as_deref()
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
        if !moving_picture_of_the_wire(m) { return; }
        self.wants_clip = true;
        self.poll(m);
    }

    /// A play/seek intent can precede its local file check. Keep the intent
    /// through preparation, and request a download only after a cache miss.
    pub fn poll(&mut self, m: &Msg) {
        if !self.wants_clip || self.asked { return; }
        let Some(reference) = m.media.as_ref().and_then(|media| media.clip.as_deref()) else { return; };
        if let media_cache::Reading::Ready(paths) = media_cache::read(&self.store, reference, true) {
            if paths.playable.is_none() {
                self.asked = wire(&self.store, &requests::request_media(m.chat, m.id, true));
            }
        }
    }

    /// Asks for a voice note's bytes, by the remote id the row keeps. The
    /// worker turns it into a `getRemoteFile` and a download, and the bytes
    /// land in the blob cache under the key the row already names — the
    /// same road a picture the cache has let go travels. Nothing fetches a
    /// note on arrival, so this is the whole of how one gets here.
    pub fn ask_for_sound(&mut self, m: &Msg) {
        if self.wanted_sound || !plays_sound(m) {
            return;
        }
        let Some(rid) = m.media.as_ref().and_then(|md| md.rid.as_deref()) else { return };
        self.wanted_sound = true;
        if self.sound_file(m).is_none() {
            runtime::of(&self.store).want_file(rid);
        }
    }

    /// Restore a missing photo through its source message too. A clip request
    /// already fetches its poster, so it needs no second message request.
    pub fn ask_for_picture(&mut self, m: &Msg) {
        if self.wanted_pic || self.asked {
            return;
        }
        let Some(md) = m.media.as_ref() else { return };
        if !md.has_picture() { return; }
        let Some(reference) = md.reference.as_deref().filter(|r| r.starts_with("tg:")) else { return; };
        if let media_cache::Reading::Ready(paths) = media_cache::read(&self.store, reference, false) {
            if paths.file.is_none() {
                self.wanted_pic = wire(&self.store, &requests::request_media(m.chat, m.id, false));
            }
        }
    }

    /// Whether a picture was asked for and has not landed — what keeps the
    /// viewer drawing until its prepared local path is available.
    #[must_use]
    pub fn awaiting_picture(&self, m: &Msg) -> bool {
        self.wanted_pic
            && m.media
                .as_ref()
                .and_then(|md| md.reference.as_deref())
                .is_some_and(|reference| media_cache::read(&self.store, reference, false)
                    .paths().is_none_or(|paths| paths.file.is_none()))
    }

    /// Play or pause. Pressing play on a clip is also the asking, since a
    /// clip nobody has opened was never downloaded; the wish is all a verb
    /// can set — a player is only reachable where there is a `Cx`, which is
    /// the draw. Everything else toggles the clock timeline.
    pub fn toggle_play(&mut self, m: &Msg, now: f64) {
        self.ask_for_clip(m);
        self.ask_for_sound(m);
        if self.playing(now) {
            self.pause(now);
            return;
        }
        let played = self.plays_clip(m) || plays_sound(m);
        let secs = m.media.as_ref().and_then(|md| md.secs);
        if !played && secs.is_none() { return; }
        if !played {
            self.transport.run_timeline(secs.unwrap() as f64);
        }
        self.transport.play(now);
    }

    /// Seek the demo or audio timeline; a real clip is sought by the widget's
    /// native player, which needs a `Cx`.
    pub fn seek(&mut self, m: &Msg, position: f64, now: f64) {
        if self.plays_clip(m) || plays_sound(m) {
            return;
        }
        let Some(secs) = m.media.as_ref().and_then(|md| md.secs) else { return };
        self.transport.run_timeline(secs as f64);
        self.transport.seek_timeline(position, now);
    }

    /// The wish the draw carries out over the clip's player.
    #[must_use]
    pub fn running(&self) -> bool {
        self.transport.running()
    }

    /// What the draw found the player at afterwards — `false` where the clip
    /// has run to its end, which is what puts the button back to *play*.
    pub fn set_running(&mut self, running: bool) {
        self.transport.set_running(running);
    }

    /// Keep the native position for controls and verbs between draws.
    pub fn set_native_state(&mut self, state: PlayerState) {
        self.transport.set_native(state);
    }

    pub fn pause(&mut self, now: f64) {
        self.transport.pause(now);
    }

    /// Whether anything runs — what asks for the next frame.
    #[must_use]
    pub fn playing(&self, now: f64) -> bool {
        self.transport.playing(now)
    }
}

/// Whether a line is a voice note that came over the wire — the one thing
/// a chat carries that no platform player on the Mac will open, and so the
/// one the kit plays itself. The demo world's voice lines name no `tg:`
/// file and keep the fake timeline they have always had.
#[must_use]
pub fn plays_sound(m: &Msg) -> bool {
    m.media.as_ref().is_some_and(|md| {
        md.kind == "voice"
            && md.reference.as_deref().is_some_and(|r| r.starts_with("tg:"))
            && md.rid.is_some()
    })
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

#[cfg(test)]
mod tests {
    use super::*;
    use kernel::session::Session;

    use crate::apps::telegram::{model, seed::STELAXIS, TELEGRAM};
    use crate::shell::widgets::media::{played_by_kit, Source};

    /// A voice note is never fetched on arrival: pressing *play* is the
    /// asking, by the remote id the row keeps, and once the bytes are here
    /// the file is the kind the kit plays itself.
    #[test]
    fn a_voice_note_is_asked_for_on_play_and_then_played_by_the_kit() {
        static APPS: &[&dyn kernel::app::App] = &[&TELEGRAM];
        let session = Session::fake(APPS);
        let mut m = model::history(session.store(), STELAXIS)
            .iter()
            .find(|m| m.media.as_ref().is_some_and(|md| md.kind == "voice"))
            .expect("a voice line in the demo world")
            .clone();
        // The demo line's own reference names no file at all, so it keeps
        // the fake timeline it has always had.
        assert!(!plays_sound(&m), "a demo note is not one of the wire's");

        let dir = std::env::temp_dir()
            .join(format!("superapp-tg-voice-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("blobs")).expect("a blob cache");
        let store = Rc::new(
            Store::open(Some(&dir.join("store.sqlite")), &[], kernel::sync::Device::fake())
                .expect("a store"),
        );
        let md = m.media.as_mut().unwrap();
        md.reference = Some("tg:the-note".into());
        md.rid = Some("RID-NOTE".into());

        let mut p = Playback::new(store.clone(), m.key());
        assert!(plays_sound(&m));
        assert!(p.sound_file(&m).is_none(), "nothing was fetched on arrival");
        assert!(p.download_note(&m).is_none(), "and nothing is being waited for");
        assert!(runtime::of(&store).take_wanted().files.is_empty());

        p.toggle_play(&m, 0.0);
        assert_eq!(
            runtime::of(&store).take_wanted().files,
            vec!["RID-NOTE".to_string()],
            "play is the asking, by the durable remote id"
        );
        assert!(p.download_note(&m).is_some(), "and the row says it is coming");
        assert!(p.player_state(&m, 0.0).expect("a state").playing, "the wish stands meanwhile");

        // The bytes land in the cache under the key the row already names.
        std::fs::write(
            dir.join("blobs").join(kernel::caps::file_name("tg:the-note")),
            b"OggS\0\0\0\0 and the rest of a voice note",
        )
        .expect("the blob");
        let file = p.sound_file(&m).expect("the link the player opens it through");
        assert_eq!(file.extension().and_then(|e| e.to_str()), Some("ogg"));
        assert_eq!(p.playable(&m), Some(file.clone()));
        assert!(played_by_kit(&Source::File(file)), "which the kit plays itself");
        assert!(p.download_note(&m).is_none(), "and the note goes");

        // Asked once: a second press does not queue the file again.
        p.toggle_play(&m, 1.0);
        p.toggle_play(&m, 2.0);
        assert!(runtime::of(&store).take_wanted().files.is_empty());

        drop(store);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
