//! The other driver: a recording the platform will not play, decoded here
//! and played through the shell's own [mixer](crate::shell::sound).
//!
//! [`Clip`](super::Clip) leases the platform's player and hands it a file.
//! That works for everything the operating system has a decoder for, which
//! on the Mac does not include Opus — and Opus in Ogg is what every
//! Telegram client records a voice note as. So a note arrives and there is
//! nothing to play it with: until now it ran the kit's clock timeline, the
//! seconds counting over silence.
//!
//! This is the same shape a host drives — pointed at a thing by its key,
//! given a source and a wish, answering where it stands and whether to draw
//! again — over a decoder of ours instead of the platform's. Which of the
//! two a file gets is [`super::played_by_kit`]'s to say, and the host never
//! asks: the transport, the strip, the scrub and the rule that one thing
//! plays at a time are the same either way.
//!
//! The decode runs on the blocking pool, because a minute of speech is a
//! few million samples and a draw may not wait for them. Until it lands the
//! strip reads *pause* at `0:00`, exactly as it does while the platform
//! prepares a clip.

use std::path::{Path, PathBuf};
use std::sync::mpsc::{Receiver, TryRecvError};
use std::sync::Arc;

use kernel::codec::pcm::Pcm;
use makepad_widgets::SignalToUI;

use crate::shell::sound::{self, Voice};

use super::{ClipDrawn, Source};

/// A recording the kit plays itself.
pub struct OpusClip {
    voice: Voice,
    /// What the host pointed this at. A change starts over: two rows never
    /// share a decode.
    key: Option<String>,
    /// The file the samples are of — the decode is redone when it changes,
    /// since a blob can be replaced under its name.
    from: Option<PathBuf>,
    /// The decode in flight.
    reading: Option<Receiver<Result<Pcm, String>>>,
    /// The samples, once they are here. Kept even while another recording
    /// has the mixer, so taking the slot back costs nothing.
    pcm: Option<Arc<Pcm>>,
    /// What went wrong, for the trace and for a host that shows a note.
    trouble: Option<String>,
    /// Where it stands, kept across a loss of the slot so playing again
    /// takes it on from there rather than from the top.
    at: f64,
    /// A seek not yet made so.
    seek: Option<f64>,
    /// The wish at the last draw. A rising one is a fresh press of *play*,
    /// which is what starts a recording that has run out over again — the
    /// same frame's report that it ended must not.
    wished: bool,
}

impl Default for OpusClip {
    fn default() -> OpusClip {
        OpusClip {
            voice: sound::mixer().voice(),
            key: None,
            from: None,
            reading: None,
            pcm: None,
            trouble: None,
            at: 0.0,
            seek: None,
            wished: false,
        }
    }
}

impl OpusClip {
    /// Lets the samples and the mixer go; the next draw starts over.
    pub fn reset(&mut self) {
        *self = OpusClip::default();
    }

    /// Points the driver at a thing to play, by the host's key for it.
    pub fn point_at(&mut self, key: &str) {
        if self.key.as_deref() != Some(key) {
            let key = key.to_string();
            self.reset();
            self.key = Some(key);
        }
    }

    /// Stops the sound now, without waiting for a draw — what a panel going
    /// to the background or off the screen needs, since a wish on a
    /// transport is only carried out where there is a draw to carry it.
    pub fn pause(&mut self) {
        self.voice.pause();
        self.wished = false;
    }

    /// Keeps the latest seek until there is something to seek in.
    pub fn seek(&mut self, position: f64) {
        if position.is_finite() {
            self.seek = Some(position.max(0.0));
        }
    }

    /// Whether a seek is still waiting on the samples.
    #[must_use]
    pub fn awaiting_seek(&self) -> bool {
        self.seek.is_some()
    }

    /// Where the driver stands, in a word — for a trace, the way
    /// [`video_word`](super::video_word) says it of the platform's player.
    #[must_use]
    pub fn word(&self) -> &'static str {
        if self.trouble.is_some() {
            "refused"
        } else if self.reading.is_some() {
            "reading"
        } else if self.pcm.is_none() {
            "silent"
        } else if !self.voice.holds() {
            "ready"
        } else if self.voice.ended() {
            "ended"
        } else if self.voice.running() {
            "playing"
        } else {
            "paused"
        }
    }

    /// What went wrong reading the file, if anything did.
    #[must_use]
    pub fn trouble(&self) -> Option<&str> {
        self.trouble.as_deref()
    }

    /// Drives the recording at `source` towards the wish. `length` stands in
    /// for its own until the samples say how long it really is, and `now` is
    /// the clock the transport ticks against — which is also what moves the
    /// position on a run with no sound out at all.
    pub fn drive(
        &mut self,
        source: Option<&Source>,
        wish: bool,
        length: f64,
        now: f64,
    ) -> ClipDrawn {
        let began = wish && !self.wished;
        self.wished = wish;
        let Some(Source::File(path)) = source else {
            // Nothing here yet — a note still downloading. The wish stands:
            // it is what starts the recording the draw that finds its file.
            if self.from.is_some() || self.reading.is_some() {
                self.let_go();
            }
            return ClipDrawn { shown: false, playing: wish, state: None, redraw: false };
        };
        self.read(path.as_path());
        self.land();
        if wish && !self.voice.holds() {
            if let Some(pcm) = self.pcm.clone() {
                self.voice.load(pcm);
                self.voice.seek(self.at);
            }
        }
        if let Some(at) = self.seek.take() {
            self.at = at;
            self.voice.seek(at);
        }
        if wish {
            if began || !self.voice.ended() {
                self.voice.play();
            }
        } else {
            self.voice.pause();
        }
        let state = self.voice.state(now).map(|mut st| {
            if st.length <= 0.0 {
                st.length = length;
            }
            st
        });
        if let Some(st) = &state {
            self.at = st.position;
        }
        // The wish stands while there is nothing to play it against — a
        // read still out — but a recording that could not be read is not
        // playing, and saying so is what puts the button back to *play*
        // rather than leaving the host drawing at it forever.
        let playing = if self.trouble.is_some() {
            false
        } else {
            state.as_ref().map_or(wish, |st| st.playing)
        };
        ClipDrawn {
            // A sound has no picture: the surface keeps whatever the host
            // put on it.
            shown: false,
            playing,
            state,
            redraw: state.as_ref().is_some_and(|st| st.playing) || self.reading.is_some(),
        }
    }

    /// Starts a decode for a file that has none, on the blocking pool.
    fn read(&mut self, path: &Path) {
        if self.from.as_deref() == Some(path) {
            return;
        }
        self.let_go();
        self.from = Some(path.to_path_buf());
        self.at = 0.0;
        let (send, receive) = std::sync::mpsc::channel();
        let path = path.to_path_buf();
        kernel::runtime::spawn_blocking(move || {
            let read = kernel::codec::opus_ogg::decode(&path);
            if send.send(read).is_ok() {
                // The host draws again while a read is out, but a signal
                // costs nothing and a host that stopped is woken by it.
                SignalToUI::set_ui_signal();
            }
        });
        self.reading = Some(receive);
    }

    /// Takes the samples when the decode answers.
    fn land(&mut self) {
        let Some(reading) = self.reading.as_ref() else { return };
        match reading.try_recv() {
            Ok(Ok(pcm)) => {
                self.reading = None;
                self.pcm = Some(Arc::new(pcm));
            }
            Ok(Err(trouble)) => {
                self.reading = None;
                self.trouble = Some(trouble);
            }
            Err(TryRecvError::Disconnected) => {
                self.reading = None;
                self.trouble = Some("the recording could not be read".to_string());
            }
            Err(TryRecvError::Empty) => {}
        }
    }

    /// Lets the samples, the mixer and the read go, keeping the key and
    /// the place: a file that comes back is taken on from where it stood.
    fn let_go(&mut self) {
        let key = self.key.clone();
        *self = OpusClip { key, wished: self.wished, at: self.at, ..OpusClip::default() };
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A directory of this test's own, since the driver reads real files.
    fn scratch(name: &str) -> PathBuf {
        let at = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let dir = std::env::temp_dir().join(format!("superapp-{name}-{}-{at}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("a temp dir");
        dir
    }

    /// A real note, written by the encoder a recording uses, played by the
    /// driver a received one gets: the words it goes through, the position
    /// moving by the clock, and the end that puts the button back to *play*.
    #[test]
    fn a_note_is_read_then_played_and_says_where_it_stands() {
        let _alone = sound::alone();
        let dir = scratch("opus-driver");
        let path = dir.join("note.ogg");
        let mut writing = kernel::codec::opus_ogg::Voice::create(&path, 48_000.0).expect("an encoder");
        let tone: Vec<f32> = (0..48_000)
            .map(|n| (0.6 * (f64::from(n) / 48_000.0 * 440.0 * std::f64::consts::TAU).sin()) as f32)
            .collect();
        writing.push(&tone).expect("a frame");
        writing.finish().expect("the file");

        let mut clip = OpusClip::default();
        assert_eq!(clip.word(), "silent");
        clip.point_at("chat/7");
        let source = Source::File(path);

        // Nothing to play yet: the wish stands, because pressing play on a
        // note still downloading must start it when it lands.
        let drawn = clip.drive(None, true, 1.0, 0.0);
        assert!(drawn.playing && drawn.state.is_none() && !drawn.redraw);

        // The read, then the play.
        let drawn = clip.drive(Some(&source), true, 1.0, 0.0);
        assert!(drawn.redraw, "a read asks for the next frame");
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        let mut drawn = drawn;
        while clip.word() == "reading" {
            assert!(std::time::Instant::now() < deadline, "the note decoded");
            drawn = clip.drive(Some(&source), true, 1.0, 0.0);
        }
        assert_eq!(clip.word(), "playing", "{:?}", clip.trouble());
        let st = drawn.state.expect("a state once there are samples");
        assert!(st.playing && st.position == 0.0);
        assert!((st.length - 1.0).abs() < 0.1, "the samples say how long it is: {st:?}");
        assert!(!drawn.shown, "a sound has no picture to show");

        let drawn = clip.drive(Some(&source), true, 1.0, 0.4);
        let st = drawn.state.unwrap();
        assert!((st.position - 0.4).abs() < 0.01, "it moves by the clock: {st:?}");

        // Held where it is, and taken on from there.
        let drawn = clip.drive(Some(&source), false, 1.0, 0.5);
        assert_eq!(clip.word(), "paused");
        assert!(!drawn.playing && !drawn.redraw);
        assert!((drawn.state.unwrap().position - 0.4).abs() < 0.01);
        let drawn = clip.drive(Some(&source), true, 1.0, 10.0);
        assert!((drawn.state.unwrap().position - 0.4).abs() < 0.01, "not from the top");

        // Sought, then run out: the wish falls and the word says so.
        clip.seek(0.9);
        let drawn = clip.drive(Some(&source), true, 1.0, 10.0);
        assert!((drawn.state.unwrap().position - 0.9).abs() < 0.01);
        let drawn = clip.drive(Some(&source), true, 1.0, 11.0);
        assert!(!drawn.playing, "it ran out");
        assert_eq!(clip.word(), "ended");
        // The same wish, still standing, must not start it over.
        let drawn = clip.drive(Some(&source), true, 1.0, 12.0);
        assert!(!drawn.playing);
        // A fresh press does.
        let _ = clip.drive(Some(&source), false, 1.0, 12.0);
        let drawn = clip.drive(Some(&source), true, 1.0, 12.0);
        assert!(drawn.playing && drawn.state.unwrap().position == 0.0);

        // Another row on the same driver forgets all of it.
        clip.point_at("chat/8");
        assert_eq!(clip.word(), "silent");
        std::fs::remove_dir_all(dir).expect("the temp dir goes");
    }

    /// A file that is not a recording is refused rather than played as
    /// noise, and says so once.
    #[test]
    fn a_file_that_is_not_a_recording_is_refused() {
        let _alone = sound::alone();
        let dir = scratch("opus-refused");
        let path = dir.join("not-a-note.ogg");
        std::fs::write(&path, b"not an ogg stream").expect("the file");
        let mut clip = OpusClip::default();
        clip.point_at("chat/9");
        let source = Source::File(path);
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        let mut drawn = clip.drive(Some(&source), true, 3.0, 0.0);
        while clip.word() == "reading" {
            assert!(std::time::Instant::now() < deadline, "the read answered");
            drawn = clip.drive(Some(&source), true, 3.0, 0.0);
        }
        assert_eq!(clip.word(), "refused");
        assert!(clip.trouble().is_some());
        assert!(drawn.state.is_none() && !drawn.redraw);
        std::fs::remove_dir_all(dir).expect("the temp dir goes");
    }
}
