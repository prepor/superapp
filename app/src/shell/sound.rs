//! The sound out: one mixer over the machine's default output, for the
//! recordings the platform's own player will not take.
//!
//! Everything else the app plays goes to the platform — a clip, a track, a
//! `<video>` in a reading — because the platform has a decoder for it and a
//! player around the decoder. A voice note does not: it is Ogg Opus, which
//! is what every Telegram client records and what nothing Apple ships will
//! open. So the app decodes it ([`kernel::codec::opus_ogg::decode`]) and
//! plays it itself, and this is the *itself*.
//!
//! One mixer, one clip in it. The machine has one pair of speakers and the
//! kit's rule is that one thing plays at a time, so a slot that holds at
//! most one recording is not a limitation to work around — it is the rule,
//! written where the samples are. A [`Voice`] is a claim on the slot: taking
//! it stops whatever had it, and dropping it — a panel closing — stops the
//! sound, which is the one thing a wish on a transport cannot do without a
//! draw.
//!
//! The callback runs on the platform's audio thread and may not wait: it
//! locks the slot, copies what it needs and returns. Nothing it does
//! allocates after the first few buffers.
//!
//! **With no device out** — a headless build, a machine with no speakers,
//! a run before the platform has named one — the position still moves, by
//! the clock the transport already ticks against. A scripted run therefore
//! behaves exactly as a real one does, minus the sound, which is the point:
//! a suite can play a voice note and read the time off the strip.

use std::sync::{Arc, Mutex, OnceLock};

use makepad_widgets::makepad_platform::audio::{AudioBuffer, AudioDeviceId, AudioInfo};
use makepad_widgets::*;

use kernel::codec::pcm::Pcm;

use super::widgets::media::PlayerState;

/// The rate everything the mixer is handed is at — [`kernel::codec::pcm`]'s,
/// because that is what Opus decodes to.
const RATE: f64 = kernel::codec::pcm::RATE as f64;

// -- the slot ------------------------------------------------------------------------

/// The one recording the machine is playing, where it stands, and whether
/// it runs. Behind a mutex the audio thread and the drawing thread share.
#[derive(Default)]
struct Slot {
    /// What is loaded. 48 kHz mono, as the decoder answers.
    pcm: Option<Arc<Pcm>>,
    /// Which voice holds the slot; zero is nobody.
    owner: u64,
    /// Where it stands, in samples — fractional because a device that runs
    /// at another rate consumes a fraction of a sample per frame.
    at: f64,
    running: bool,
    /// Whether the platform's output has ever asked this mixer for a
    /// buffer. Once it has, where the recording stands is the callback's to
    /// say; until then — a headless build, a machine with no speaker, the
    /// moment before the device is open — the clock says it.
    heard: bool,
    /// The clock the position was last moved against, while nothing is
    /// pulling.
    ticked: Option<f64>,
}

impl Slot {
    fn samples(&self) -> usize {
        self.pcm.as_ref().map_or(0, |pcm| pcm.samples.len())
    }

    fn length(&self) -> f64 {
        self.samples() as f64 / RATE
    }

    fn ended(&self) -> bool {
        self.samples() > 0 && self.at >= self.samples() as f64
    }

    /// Where it stands now, and whether it runs — after the clock has had
    /// its turn where no device is taking one.
    fn state(&mut self, now: f64) -> PlayerState {
        let since = self.ticked.replace(now);
        if !self.heard && self.running {
            let dt = since.map_or(0.0, |then| (now - then).max(0.0));
            self.at = (self.at + dt * RATE).min(self.samples() as f64);
            if self.ended() {
                self.running = false;
            }
        }
        PlayerState {
            playing: self.running,
            position: self.at / RATE,
            length: self.length(),
        }
    }

    /// Where the next output frame reads from, moving `at` as it goes.
    /// Linear between the two samples it falls between, which for speech at
    /// 48 kHz against a device at 44.1 is below what the speaker can say.
    fn next(&mut self, step: f64) -> f32 {
        let Some(pcm) = self.pcm.as_ref() else { return 0.0 };
        let i = self.at as usize;
        let Some(&a) = pcm.samples.get(i) else {
            self.at = pcm.samples.len() as f64;
            self.running = false;
            return 0.0;
        };
        let b = pcm.samples.get(i + 1).copied().unwrap_or(a);
        let frac = (self.at - i as f64) as f32;
        self.at += step;
        a + (b - a) * frac
    }
}

// -- the mixer -----------------------------------------------------------------------

/// The machine's sound out. One per device, which means one per run — and
/// one per test that wants the arithmetic without a device.
#[derive(Clone, Default)]
pub struct Mixer(Arc<Mutex<Slot>>);

/// The process's own, which every driver plays through.
#[must_use]
pub fn mixer() -> Mixer {
    static MIXER: OnceLock<Mixer> = OnceLock::new();
    MIXER.get_or_init(Mixer::default).clone()
}

impl Mixer {
    /// A mixer of its own, for a caller that is not the machine — a test.
    #[must_use]
    pub fn new() -> Mixer {
        Mixer::default()
    }

    /// A claim on the slot, not yet taken. Taking it is [`Voice::load`].
    #[must_use]
    pub fn voice(&self) -> Voice {
        static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);
        Voice {
            slot: self.0.clone(),
            id: NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed),
        }
    }

    /// Whether anything is loaded — what says the machine's output is worth
    /// opening at all, so a run that never plays a note never asks the
    /// platform for a speaker.
    #[must_use]
    fn wanted(&self) -> bool {
        self.0.lock().is_ok_and(|slot| slot.pcm.is_some())
    }

    /// The callback the platform's audio thread calls for every buffer.
    fn sink(&self) -> impl FnMut(AudioInfo, &mut AudioBuffer) + Send + 'static {
        let slot = self.0.clone();
        let mut scratch: Vec<f32> = Vec::new();
        move |info, buffer| {
            let frames = buffer.frame_count();
            let channels = buffer.channel_count();
            buffer.zero();
            if frames == 0 || channels == 0 || buffer.data.len() < frames * channels {
                return;
            }
            let Ok(mut slot) = slot.lock() else { return };
            slot.heard = true;
            if !slot.running || slot.pcm.is_none() {
                return;
            }
            // The device's rate, not ours: one output frame is this many
            // of the recording's samples.
            let rate = if info.sample_rate > 0.0 { info.sample_rate } else { RATE };
            let step = RATE / rate;
            scratch.resize(frames, 0.0);
            for out in scratch.iter_mut() {
                *out = slot.next(step);
            }
            drop(slot);
            for channel in 0..channels {
                buffer.channel_mut(channel).copy_from_slice(&scratch);
            }
        }
    }
}

// -- a claim on it -------------------------------------------------------------------

/// One driver's claim on the machine's sound out.
///
/// Holding it is what plays; losing it — another voice loading — is what
/// stops. A dropped voice that still holds the slot empties it, so a panel
/// that closes mid-note takes its sound with it without anybody having to
/// remember to say so.
pub struct Voice {
    slot: Arc<Mutex<Slot>>,
    id: u64,
}

impl Voice {
    /// Takes the slot with a recording in it: whatever played before stops.
    pub fn load(&self, pcm: Arc<Pcm>) {
        let Ok(mut slot) = self.slot.lock() else { return };
        *slot = Slot { pcm: Some(pcm), owner: self.id, heard: slot.heard, ..Slot::default() };
    }

    /// Whether this voice is the one the slot holds.
    #[must_use]
    pub fn holds(&self) -> bool {
        self.slot.lock().is_ok_and(|slot| slot.owner == self.id)
    }

    /// Run. A recording played again at its end starts over, the way the
    /// kit's clock timeline does.
    pub fn play(&self) {
        self.with(|slot| {
            if slot.ended() {
                slot.at = 0.0;
            }
            // Only where it was not running already: a draw carries the
            // wish out every frame, and a clock that started over every
            // frame would never move.
            if !slot.running {
                slot.ticked = None;
            }
            slot.running = true;
        });
    }

    /// Hold, where it stands.
    pub fn pause(&self) {
        self.with(|slot| {
            slot.running = false;
            slot.ticked = None;
        });
    }

    /// Move along it, running or not.
    pub fn seek(&self, position: f64) {
        if !position.is_finite() {
            return;
        }
        self.with(|slot| {
            slot.at = (position.max(0.0) * RATE).min(slot.samples() as f64);
            slot.ticked = None;
        });
    }

    /// Where it stands at `now` — and the tick itself, since a run with no
    /// device out moves by this clock and nothing else would move it.
    /// `None` where another voice has the slot.
    #[must_use]
    pub fn state(&self, now: f64) -> Option<PlayerState> {
        let mut slot = self.slot.lock().ok()?;
        (slot.owner == self.id).then(|| slot.state(now))
    }

    /// Whether it runs, reading nothing and moving nothing — for a word in
    /// a trace, which may not be the thing that ticks the clock.
    #[must_use]
    pub fn running(&self) -> bool {
        self.slot.lock().is_ok_and(|slot| slot.owner == self.id && slot.running)
    }

    /// Whether it has run out.
    #[must_use]
    pub fn ended(&self) -> bool {
        self.slot.lock().is_ok_and(|slot| slot.owner == self.id && slot.ended())
    }

    fn with(&self, act: impl FnOnce(&mut Slot)) {
        if let Ok(mut slot) = self.slot.lock() {
            if slot.owner == self.id {
                act(&mut slot);
            }
        }
    }
}

impl Drop for Voice {
    fn drop(&mut self) {
        self.with(|slot| *slot = Slot { heard: slot.heard, ..Slot::default() });
    }
}

// -- the device ----------------------------------------------------------------------

/// What the machine's output is opened on, and whether the callback is in.
#[derive(Default)]
struct Out {
    installed: bool,
    open: Vec<AudioDeviceId>,
}

static OUT: Mutex<Option<Out>> = Mutex::new(None);

/// Services the machine's sound out, the way the stage services its senses:
/// on every event, with whatever device list the platform last named.
///
/// Registering the callback is also what wakes makepad's enumeration — no
/// `AudioDevices` event arrives until something has asked for audio — so
/// the order is: a note wants to be heard, the callback goes in, the
/// platform says which devices there are, and the default one is opened. A
/// run that never plays a note never opens a speaker.
pub fn service(cx: &mut Cx, outputs: &[AudioDeviceId]) {
    if !mixer().wanted() {
        return;
    }
    let Ok(mut held) = OUT.lock() else { return };
    let out = held.get_or_insert_with(Out::default);
    if !out.installed {
        out.installed = true;
        cx.audio_output(0, mixer().sink());
    }
    if !outputs.is_empty() && out.open != outputs {
        out.open = outputs.to_vec();
        cx.use_audio_outputs(outputs);
    }
}

/// One test at a time over the process's [`mixer`].
///
/// It is the machine's one sound out and holds one recording, which is the
/// whole point of it; two tests playing at once would take the slot from
/// each other and read each other's position. A test that loads anything
/// into the process's mixer — rather than one of its own — holds this first.
#[cfg(test)]
pub fn alone() -> std::sync::MutexGuard<'static, ()> {
    static ONE: Mutex<()> = Mutex::new(());
    ONE.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tone(secs: f64) -> Arc<Pcm> {
        Arc::new(Pcm {
            samples: vec![0.5; (secs * RATE) as usize],
            rate: kernel::codec::pcm::RATE,
        })
    }

    /// The whole of the arithmetic, with no device anywhere near it: a
    /// recording moves by the clock, stops where it is told, moves where it
    /// is sent, and runs out once.
    #[test]
    fn a_note_runs_by_the_clock_and_stops_at_its_end() {
        let mixer = Mixer::new();
        let voice = mixer.voice();
        assert!(!voice.holds(), "a claim is not the slot");
        assert!(voice.state(0.0).is_none());

        voice.load(tone(4.0));
        assert!(voice.holds());
        let at_rest = voice.state(100.0).unwrap();
        assert_eq!(at_rest, PlayerState { playing: false, position: 0.0, length: 4.0 });

        voice.play();
        // The first tick after a press is the press: nothing has elapsed.
        assert_eq!(voice.state(100.0).unwrap().position, 0.0);
        let running = voice.state(101.5).unwrap();
        assert!(running.playing);
        assert!((running.position - 1.5).abs() < 0.001, "{running:?}");

        voice.pause();
        assert_eq!(voice.state(180.0).unwrap().position, 1.5);
        assert!(!voice.state(180.0).unwrap().playing);

        voice.seek(3.0);
        assert_eq!(voice.state(180.0).unwrap().position, 3.0, "a seek holds while paused");
        voice.play();
        let _ = voice.state(180.0);
        let over = voice.state(190.0).unwrap();
        assert_eq!(over.position, 4.0, "it stops at its end, not past it");
        assert!(!over.playing && voice.ended());

        // Played again at its end, a note starts over.
        voice.play();
        let _ = voice.state(200.0);
        assert!((voice.state(200.5).unwrap().position - 0.5).abs() < 0.001);

        // A seek outside the recording lands inside it.
        voice.seek(-3.0);
        assert_eq!(voice.state(201.0).unwrap().position, 0.0);
        voice.seek(99.0);
        assert_eq!(voice.state(201.0).unwrap().position, 4.0);
        voice.seek(f64::NAN);
        assert_eq!(voice.state(201.0).unwrap().position, 4.0, "and a seek to nowhere is not one");
    }

    /// One recording at a time: a second voice loading takes the slot, and
    /// the first one's word for it is that it no longer has it. A voice
    /// that goes takes the sound with it.
    #[test]
    fn the_slot_holds_one_note_and_empties_when_its_voice_goes() {
        let mixer = Mixer::new();
        let first = mixer.voice();
        first.load(tone(2.0));
        first.play();
        assert!(first.state(0.0).unwrap().playing);

        let second = mixer.voice();
        second.load(tone(1.0));
        assert!(second.holds() && !first.holds());
        assert!(first.state(0.0).is_none(), "the one that lost the slot reads nothing");
        first.play();
        assert!(!second.state(0.0).unwrap().playing, "and cannot start it again either");

        second.play();
        assert!(second.state(0.0).unwrap().playing);
        drop(second);
        let third = mixer.voice();
        assert!(!third.holds());
        third.load(tone(1.0));
        assert_eq!(third.state(0.0).unwrap().position, 0.0, "the slot was empty to take");
    }

    /// The device, when there is one: the callback fills every channel from
    /// the one recording, moves the position by the samples it consumed at
    /// the device's own rate, and the clock keeps its hands off.
    #[test]
    fn the_callback_fills_the_buffer_and_owns_the_position() {
        let mixer = Mixer::new();
        let voice = mixer.voice();
        voice.load(Arc::new(Pcm {
            samples: (0..48_000).map(|n| (n % 100) as f32 / 100.0).collect(),
            rate: kernel::codec::pcm::RATE,
        }));
        voice.play();
        let mut sink = mixer.sink();
        let info = AudioInfo {
            device_id: AudioDeviceId(makepad_widgets::LiveId(1)),
            time: None,
            sample_rate: 24_000.0,
        };
        let mut buffer = AudioBuffer::new_with_size(512, 2);
        sink(info, &mut buffer);
        assert_eq!(buffer.channel(0), buffer.channel(1), "both ears hear the one recording");
        assert!(buffer.channel(0).iter().any(|s| *s != 0.0), "and hear something");
        // Half the rate, so twice the samples: 512 frames took 1024.
        let played = voice.state(1_000.0).unwrap();
        assert!((played.position - 1024.0 / RATE).abs() < 0.001, "{played:?}");
        // A clock far in the future cannot move a position the device owns,
        // however long it is between draws.
        assert_eq!(voice.state(9_999.0).unwrap().position, played.position);

        // Run it out: the callback is what says it ended.
        for _ in 0..200 {
            sink(info, &mut buffer);
        }
        assert!(voice.ended() && !voice.state(9_999.0).unwrap().playing);
        let mut after = AudioBuffer::new_with_size(64, 1);
        sink(info, &mut after);
        assert!(after.channel(0).iter().all(|s| *s == 0.0), "a note that ran out is silence");
    }
}
