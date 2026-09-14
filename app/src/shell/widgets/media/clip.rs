//! The driver: one native player, leased for the life of its host, pointed
//! at a source exactly once, and the poster kept up until the player really
//! has a picture.
//!
//! A host owns one `Clip` per player it embeds and drives it every draw
//! with where the source is and what the transport wishes. What comes back
//! is whether the box shows the moving picture, what the platform says of
//! it, and whether to draw again.
//!
//! It is also where the kit chooses between the two players it has. A file
//! the platform has no decoder for — Ogg Opus, which is what a voice note
//! is — goes to [`OpusClip`](super::OpusClip) instead, which decodes it and
//! plays it through the shell's own mixer. The host is not told: it drives
//! one driver, and the answer has the same shape either way.

use makepad_widgets::*;

use super::{OpusClip, PlayerState, Source, VideoPlayback};

/// What one draw over a clip found.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct ClipDrawn {
    /// The player has a picture in it and the box shows it; the poster is
    /// put away.
    pub shown: bool,
    /// The wish as it stands afterwards: `false` once the clip has run to
    /// its end, so the button goes back to reading *play*.
    pub playing: bool,
    /// Where the platform's player stands, once there is one to read.
    pub state: Option<PlayerState>,
    /// Whether the host should draw again without waiting for input: a
    /// player that runs, or a seek not yet applied.
    pub redraw: bool,
}

#[derive(Default)]
pub struct Clip {
    playback: VideoPlayback,
    /// The kit's own player, for what the platform's will not take.
    sound: OpusClip,
    /// Whether the last source was one the kit plays itself — which of the
    /// two a seek and a redraw are asked of.
    kits: bool,
    /// What the player is pointed at, by the host's own key for it.
    key: Option<String>,
    frame_ready: bool,
    last_word: String,
}

impl Clip {
    /// Lets the native player and the samples go; the next draw starts over.
    pub fn reset(&mut self, cx: &mut Cx) {
        self.playback.reset(cx);
        self.sound.reset();
        self.key = None;
        self.frame_ready = false;
    }

    /// Stops a recording the kit plays itself, now, without waiting for a
    /// draw — a panel going to the background or off the screen. The
    /// platform's player is stopped the same moment by
    /// [`pause_video`](super::pause_video) on the box.
    pub fn hush(&mut self) {
        self.sound.pause();
    }

    /// Points the driver at a thing to play, by the host's key for it. A
    /// change releases the player: two things never share a first frame.
    pub fn point_at(&mut self, cx: &mut Cx, key: &str) {
        if self.key.as_deref() != Some(key) {
            self.playback.reset(cx);
            self.sound.point_at(key);
            self.key = Some(key.to_string());
            self.frame_ready = false;
        }
    }

    /// Whether the driver has been pointed at anything.
    #[must_use]
    pub fn pointed_at(&self, key: &str) -> bool {
        self.key.as_deref() == Some(key)
    }

    /// Keeps the latest seek until the source and the native player are
    /// ready. Seeking leaves the play/pause wish alone.
    pub fn seek(&mut self, position: f64) {
        // Both, because which player the source belongs to is only known
        // once it is here, and a seek can be the first thing a row is asked
        // for. Whichever is not driving is never consulted.
        self.playback.seek(position);
        self.sound.seek(position);
    }

    #[must_use]
    pub fn awaiting_seek(&self) -> bool {
        if self.kits {
            self.sound.awaiting_seek()
        } else {
            self.playback.awaiting_seek()
        }
    }

    #[must_use]
    pub fn seek_needs_redraw(&self) -> bool {
        if self.kits {
            self.sound.awaiting_seek()
        } else {
            self.playback.seek_needs_redraw()
        }
    }

    /// Whether the player has handed over a first frame since it was
    /// pointed here.
    #[must_use]
    pub fn frame_ready(&self) -> bool {
        self.frame_ready
    }

    #[cfg(test)]
    pub fn frame_ready_for_test(&mut self) {
        self.frame_ready = true;
    }

    /// Prepared or playing only says the decoder has started. The poster
    /// stays until this player has delivered its first texture, even when
    /// that frame's timestamp is zero. True when it just did.
    pub fn handle_actions(&mut self, cx: &mut Cx, video: &WidgetRef, actions: &Actions) -> bool {
        let clip = video.widget(cx, ids!(clip));
        for action in actions.filter_widget_actions(clip.widget_uid()) {
            if let VideoAction::TextureUpdated = action.cast() {
                if clip.as_video().is_playing() || clip.as_video().is_paused() {
                    self.frame_ready = true;
                    clip.as_video().should_dispatch_texture_updates(false);
                    return true;
                }
            }
        }
        false
    }

    /// Drives the player at `video` towards the wish, over `source` where
    /// there is one; `length` stands in for the clip's until the platform
    /// reports it. The box is shown only with a decoded frame in it.
    pub fn drive(
        &mut self,
        cx: &mut Cx,
        video: &WidgetRef,
        source: Option<&Source>,
        wish: bool,
        length: f64,
        now: f64,
    ) -> ClipDrawn {
        self.kits = source.is_some_and(super::played_by_kit);
        if self.kits {
            // The platform's player has no part in this one: the box gives
            // it back, so nothing keeps a lease over a file it cannot open.
            self.playback.drive(cx, video, None, false);
            video.set_visible(cx, false);
            self.frame_ready = false;
            let drawn = self.sound.drive(source, wish, length, now);
            if crate::shell::boot::frame_log() {
                let word = self.sound.word();
                let at = drawn.state.map_or(0.0, |st| st.position);
                let beat = format!("{word} {:.0}s", at.floor());
                if self.last_word != beat {
                    eprintln!(
                        "sound {}: {word} (wanted={wish}, at {at:.1}s){}",
                        self.key.as_deref().unwrap_or(""),
                        self.sound.trouble().map(|t| format!(" — {t}")).unwrap_or_default(),
                    );
                    self.last_word = beat;
                }
            }
            return drawn;
        }
        let drawn = self.playback.drive(cx, video, source, wish);
        if !drawn.shown {
            self.frame_ready = false;
        }
        video.widget(cx, ids!(clip)).as_video().should_dispatch_texture_updates(!self.frame_ready);
        let shown = drawn.shown && self.frame_ready;
        video.set_visible(cx, shown);
        if crate::shell::boot::frame_log() {
            let word = format!(
                "{}{}",
                super::video_word(cx, video),
                if shown { ", picture up" } else { "" }
            );
            let at = video.widget(cx, ids!(clip)).as_video().current_position_ms() as f64 / 1000.0;
            // Every second of a running clip, too, so a stalled one can be
            // told from one that runs unseen.
            let beat = format!("{word} {:.0}s", at.floor());
            if self.last_word != beat {
                eprintln!(
                    "clip {}: {word} (wanted={wish}, source={}, at {at:.1}s)",
                    self.key.as_deref().unwrap_or(""),
                    source.is_some(),
                );
                self.last_word = beat;
            }
        }
        let state = (drawn.shown || self.playback.awaiting_seek()).then(|| {
            let mut st = self.playback.state(cx, video, length);
            st.playing = drawn.playing;
            st
        });
        ClipDrawn {
            shown,
            playing: drawn.playing,
            state,
            redraw: drawn.playing || self.playback.seek_needs_redraw(),
        }
    }
}
