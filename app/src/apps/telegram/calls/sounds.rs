//! The four sounds a call makes, written rather than bundled.
//!
//! A ring is a couple of sine waves and a cadence — the telephone network's
//! own tones, which is why every client's sound like each other. Generating
//! them costs a few hundred kilobytes of arithmetic once per store and keeps
//! four binary files out of the repository, so that is what this does: at
//! first use the tone is written as a plain 16-bit WAV under the store's own
//! `sounds/` directory, and from then on it is a file the platform's player
//! opens like any other.
//!
//! A world with no directory — a fixture, a scene, a test — gets no file and
//! therefore no sound, which is the right answer: a scripted run is silent.

use std::path::{Path, PathBuf};

/// The sample rate, and what the header says. 48 kHz because everything else
/// in this build already speaks it.
const RATE: u32 = 48_000;

/// How loud a tone is, of full scale. Two tones sum, so each stays well
/// under half.
const LEVEL: f64 = 0.3;

/// Which sound, and therefore which tones and which cadence.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Ring {
    /// Somebody is calling: the bell's two tones, two seconds on and four
    /// off, over and over until the call is answered or refused.
    Incoming,
    /// I am calling: the single ringing tone the network plays back to a
    /// caller, one second on and four off.
    Ringback,
    /// The other side refused: the busy cadence, three times and done.
    Busy,
    /// It is over: one short note.
    Ended,
}

impl Ring {
    /// The file it is written to.
    #[must_use]
    pub fn name(self) -> &'static str {
        match self {
            Ring::Incoming => "incoming.wav",
            Ring::Ringback => "ringback.wav",
            Ring::Busy => "busy.wav",
            Ring::Ended => "ended.wav",
        }
    }

    /// Whether it goes round until something stops it. The two that ring do;
    /// the two that report do not.
    #[must_use]
    pub fn repeats(self) -> bool {
        matches!(self, Ring::Incoming | Ring::Ringback)
    }

    /// The tones it is made of, and the cadence they keep: pairs of hertz,
    /// then the seconds on and the seconds of silence after.
    fn recipe(self) -> (&'static [f64], f64, f64, usize) {
        match self {
            // The bell: 440 and 480 together, the North American ring.
            Ring::Incoming => (&[440.0, 480.0], 2.0, 4.0, 1),
            // What a caller hears while it rings there: one tone, the
            // European ringing cadence, so the two are never confused.
            Ring::Ringback => (&[425.0], 1.0, 4.0, 1),
            // Busy: 480 and 620, half a second each way, three times.
            Ring::Busy => (&[480.0, 620.0], 0.5, 0.5, 3),
            // One short note to say it has ended.
            Ring::Ended => (&[660.0], 0.18, 0.0, 1),
        }
    }

    /// The samples themselves, mono.
    #[must_use]
    fn samples(self) -> Vec<i16> {
        let (tones, on, off, times) = self.recipe();
        let cycle = (RATE as f64 * (on + off)) as usize;
        let sounding = (RATE as f64 * on) as usize;
        let mut out = Vec::with_capacity(cycle * times);
        for _ in 0..times {
            for n in 0..cycle {
                if n >= sounding {
                    out.push(0);
                    continue;
                }
                let t = n as f64 / f64::from(RATE);
                // A few milliseconds of fade at each end of a burst: a tone
                // that starts and stops on a step clicks.
                let edge = 0.008 * f64::from(RATE);
                let fade = ((n as f64) / edge).min((sounding - n) as f64 / edge).clamp(0.0, 1.0);
                let v: f64 = tones.iter().map(|hz| (t * hz * std::f64::consts::TAU).sin()).sum();
                let v = v * LEVEL * fade;
                out.push((v.clamp(-1.0, 1.0) * f64::from(i16::MAX)) as i16);
            }
        }
        out
    }
}

/// A RIFF/WAVE file of 16-bit mono samples. Forty-four bytes of header and
/// the samples little-endian after it — the whole of the format this needs.
#[must_use]
fn wav(samples: &[i16]) -> Vec<u8> {
    let data = samples.len() * 2;
    let mut out = Vec::with_capacity(44 + data);
    let n = u32::try_from(data).unwrap_or(u32::MAX);
    out.extend_from_slice(b"RIFF");
    out.extend_from_slice(&(36 + n).to_le_bytes());
    out.extend_from_slice(b"WAVEfmt ");
    out.extend_from_slice(&16u32.to_le_bytes()); // the chunk's own length
    out.extend_from_slice(&1u16.to_le_bytes()); // uncompressed PCM
    out.extend_from_slice(&1u16.to_le_bytes()); // one channel
    out.extend_from_slice(&RATE.to_le_bytes());
    out.extend_from_slice(&(RATE * 2).to_le_bytes()); // bytes a second
    out.extend_from_slice(&2u16.to_le_bytes()); // bytes a frame
    out.extend_from_slice(&16u16.to_le_bytes()); // bits a sample
    out.extend_from_slice(b"data");
    out.extend_from_slice(&n.to_le_bytes());
    for s in samples {
        out.extend_from_slice(&s.to_le_bytes());
    }
    out
}

/// The file for a sound, written under `dir` if it is not there yet. `None`
/// where there is no directory to write into, or the write refused — a call
/// that cannot ring is still a call, and a silent one is better than none.
#[must_use]
pub fn file(dir: Option<&Path>, ring: Ring) -> Option<PathBuf> {
    let path = dir?.join("sounds").join(ring.name());
    if path.exists() {
        return Some(path);
    }
    std::fs::create_dir_all(path.parent()?).ok()?;
    std::fs::write(&path, wav(&ring.samples())).ok()?;
    Some(path)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_written_ring_is_a_wav_of_the_cadence_it_names() {
        let dir = std::env::temp_dir().join(format!("tg-ring-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("a directory to ring in");
        let path = file(Some(&dir), Ring::Incoming).expect("the ring");
        let bytes = std::fs::read(&path).expect("the file");
        assert_eq!(&bytes[0..4], b"RIFF");
        assert_eq!(&bytes[8..12], b"WAVE");
        // Six seconds of one channel at 48 kHz, two bytes a sample.
        assert_eq!(bytes.len(), 44 + 6 * 48_000 * 2);
        // Two seconds sounding, four silent.
        let sample = |n: usize| i16::from_le_bytes([bytes[44 + n * 2], bytes[45 + n * 2]]);
        assert!((0..48_000).map(sample).any(|s| s.abs() > 1_000), "the bell rings");
        assert!((3 * 48_000..6 * 48_000).all(|n| sample(n) == 0), "and then is silent");
        // Written once: a second ask does not rewrite it.
        let first = std::fs::metadata(&path).expect("the file").modified().expect("a time");
        assert_eq!(file(Some(&dir), Ring::Incoming).as_deref(), Some(path.as_path()));
        assert_eq!(std::fs::metadata(&path).expect("the file").modified().expect("a time"), first);
        // A world with no directory is a world with no sound.
        assert_eq!(file(None, Ring::Incoming), None);
        std::fs::remove_dir_all(&dir).expect("the directory goes");
    }

    #[test]
    fn the_short_sounds_do_not_repeat_and_the_ringing_ones_do() {
        assert!(Ring::Incoming.repeats());
        assert!(Ring::Ringback.repeats());
        assert!(!Ring::Busy.repeats());
        assert!(!Ring::Ended.repeats());
    }
}
