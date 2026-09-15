//! A microphone's samples at the rate the encoders want.
//!
//! No device runs at the rate a codec was told to expect: a Mac's built-in
//! microphone is 44.1 kHz or 48, a phone's whatever the route decides, and
//! both Opus and the AAC track of a video message are written at 48 kHz
//! mono. So every recording goes through here first.
//!
//! Linear interpolation, which for speech at these rates costs less than
//! the codec after it throws away anyway, and which is a few lines rather
//! than a filter bank. The position and the last sample are kept between
//! buffers, because a microphone hands over a hundred small ones a second
//! and a seam that started over each time would be a hundred clicks.

/// The rate everything downstream is written at.
pub const RATE: u32 = 48_000;

/// Sound with nothing in front of it: one float a sample, mono, at the rate
/// it was decoded to.
///
/// What comes *out* of a codec, where [`Resampler`] is what goes into one.
/// Mono because everything that plays a recording here plays it to both
/// ears: a voice note is recorded mono, and a stereo file is folded on the
/// way out rather than carried in two halves nothing would ever pan.
#[derive(Debug, Clone, PartialEq)]
pub struct Pcm {
    pub samples: Vec<f32>,
    pub rate: u32,
}

impl Pcm {
    /// How long it plays, in seconds.
    #[must_use]
    pub fn secs(&self) -> f64 {
        if self.rate == 0 {
            return 0.0;
        }
        self.samples.len() as f64 / f64::from(self.rate)
    }
}

/// One microphone's samples on their way to [`RATE`].
#[derive(Debug, Clone)]
pub struct Resampler {
    /// What the device hands over.
    from: f64,
    /// Where the next output sample falls among the input ones.
    at: f64,
    /// The previous buffer's last sample, which this one interpolates from.
    carry: f32,
}

impl Resampler {
    /// A resampler from a device's rate.
    ///
    /// # Errors
    ///
    /// If the rate is not one a device could have.
    pub fn from(rate: f64) -> Result<Resampler, String> {
        if !(8_000.0..=384_000.0).contains(&rate) {
            return Err(format!("the microphone reports {rate} Hz, which is not a rate"));
        }
        Ok(Resampler {
            from: rate,
            at: 0.0,
            carry: 0.0,
        })
    }

    /// Whether this is the rate the device is still handing over.
    #[must_use]
    pub fn is_from(&self, rate: f64) -> bool {
        (self.from - rate).abs() < 1.0
    }

    /// One buffer, appended to `out` as 16-bit samples at [`RATE`].
    pub fn push(&mut self, samples: &[f32], out: &mut Vec<i16>) {
        if samples.is_empty() {
            return;
        }
        let step = self.from / f64::from(RATE);
        // The buffer is read as `[carry] ++ samples`, so position zero is
        // where the previous one left off and the seam is interpolated
        // rather than jumped.
        while self.at < samples.len() as f64 {
            let i = self.at as usize;
            let frac = self.at - i as f64;
            let a = if i == 0 { self.carry } else { samples[i - 1] };
            let b = samples[i];
            let value = f64::from(a) + f64::from(b - a) * frac;
            out.push((value.clamp(-1.0, 1.0) * f64::from(i16::MAX)) as i16);
            self.at += step;
        }
        self.at -= samples.len() as f64;
        self.carry = samples[samples.len() - 1];
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The same rate is the same samples, and another rate is the same
    /// seconds.
    #[test]
    fn a_buffer_keeps_its_seconds() {
        let mut same = Resampler::from(48_000.0).expect("a rate");
        let mut out = Vec::new();
        same.push(&vec![0.5f32; 4_800], &mut out);
        assert_eq!(out.len(), 4_800, "a tenth of a second stays one");

        let mut up = Resampler::from(44_100.0).expect("a rate");
        let mut out = Vec::new();
        for chunk in vec![0.25f32; 44_100].chunks(441) {
            up.push(chunk, &mut out);
        }
        assert!(
            (out.len() as i64 - 48_000).abs() < 8,
            "a second at 44.1 kHz is a second at 48: {}",
            out.len()
        );
        assert!(
            out.iter().skip(10).all(|s| (*s - 8191).abs() < 8),
            "and the level is the level"
        );
    }

    /// Chunked or whole, the same samples come out — which is what keeping
    /// the position between buffers is for.
    #[test]
    fn the_seams_between_buffers_are_not_clicks() {
        let tone: Vec<f32> = (0..9_600)
            .map(|n| (f64::from(n) / 400.0).sin() as f32)
            .collect();
        let mut whole = Vec::new();
        Resampler::from(32_000.0)
            .expect("a rate")
            .push(&tone, &mut whole);
        let mut in_pieces = Vec::new();
        let mut r = Resampler::from(32_000.0).expect("a rate");
        for chunk in tone.chunks(137) {
            r.push(chunk, &mut in_pieces);
        }
        assert_eq!(whole.len(), in_pieces.len());
        assert!(
            whole.iter().zip(&in_pieces).all(|(a, b)| (a - b).abs() <= 1),
            "the pieces are the whole"
        );
        assert!(!r.is_from(44_100.0) && r.is_from(32_000.0));
    }

    /// A rate no device has is refused.
    #[test]
    fn an_impossible_rate_is_refused() {
        assert!(Resampler::from(0.0).is_err());
        assert!(Resampler::from(1_000_000.0).is_err());
    }
}
