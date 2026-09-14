//! The hundred bars under a voice note.
//!
//! Every Telegram client draws a received voice note from a field the
//! sender computed: a hundred five-bit numbers packed into sixty-three
//! bytes, sent beside the duration. It is the sender's job because only the
//! sender has the samples — the file is Opus, and a client that wanted to
//! draw the shape of a note it has not downloaded yet would have nothing to
//! draw from.
//!
//! So the rule has to be *theirs*, not ours: a note recorded here is looked
//! at in the official clients, and a note recorded there is looked at here.
//! The recording is cut into a hundred slices, each slice's peak is measured
//! against 1.8 times the mean of the peaks — never below a floor, so that a
//! whispered note is not drawn as a shout — and the result is clamped to the
//! five bits it has.

/// How many bars a note is drawn with. The wire's number, not a choice.
pub const SLICES: usize = 100;

/// What [`SLICES`] five-bit values weigh, rounded up: 500 bits.
pub const PACKED: usize = 63;

/// The largest value a bar can hold — five bits.
const FULL: i64 = 31;

/// The floor under the scale, in sample units of a 16-bit recording.
///
/// Without it a room's own hiss is the loudest thing in the slice and a
/// silent note is drawn as a full one. The clients' number.
const QUIET: i64 = 2500;

/// How far above the mean peak counts as full: the clients' 1.8, so that a
/// note with one shout in it still shows the talking around it.
const HEADROOM: f64 = 1.8;

/// The hundred bars of a recording, packed as the wire carries them.
///
/// Always [`PACKED`] bytes, silence included — a note with no samples at
/// all is a flat line rather than a missing field, because the row draws
/// what it is given.
#[must_use]
pub fn of_samples(samples: &[i16]) -> Vec<u8> {
    pack(&bars(samples))
}

/// The hundred bar heights, 0 to 31, before they are packed. Public because
/// the shape is what a test asserts on; the packing is only how it travels.
#[must_use]
pub fn bars(samples: &[i16]) -> [u8; SLICES] {
    let mut peaks = [0i64; SLICES];
    if !samples.is_empty() {
        for (n, s) in samples.iter().enumerate() {
            // By the sample's index rather than by a slice width, so that a
            // recording shorter than a hundred samples still fills the
            // hundred slices instead of leaving the tail empty.
            let slice = n * SLICES / samples.len();
            let loud = i64::from(s.unsigned_abs());
            if loud > peaks[slice] {
                peaks[slice] = loud;
            }
        }
    }
    let mean = peaks.iter().sum::<i64>() as f64 / SLICES as f64;
    let scale = ((HEADROOM * mean) as i64).max(QUIET);
    let mut out = [0u8; SLICES];
    for (bar, peak) in out.iter_mut().zip(peaks) {
        *bar = (peak * FULL / scale).min(FULL) as u8;
    }
    out
}

/// Five bits each, least significant first, spilling into the next byte —
/// the order every client reads them back in.
fn pack(bars: &[u8; SLICES]) -> Vec<u8> {
    let mut out = vec![0u8; PACKED];
    for (n, bar) in bars.iter().enumerate() {
        let value = u32::from(*bar) & 0x1f;
        let bit = n * 5;
        let (byte, shift) = (bit / 8, bit % 8);
        out[byte] |= (value << shift) as u8;
        if shift > 3 {
            // The last three bars spill past the end of the sixty-third
            // byte's neighbour only if the field were longer; it is not.
            if let Some(next) = out.get_mut(byte + 1) {
                *next |= (value >> (8 - shift)) as u8;
            }
        }
    }
    out
}

/// The hundred bars read back out of a packed field — what a client does
/// with a note somebody else recorded, and what proves the packing here.
#[must_use]
pub fn unpack(packed: &[u8]) -> [u8; SLICES] {
    let mut out = [0u8; SLICES];
    for (n, bar) in out.iter_mut().enumerate() {
        let bit = n * 5;
        let (byte, shift) = (bit / 8, bit % 8);
        let low = u32::from(packed.get(byte).copied().unwrap_or(0)) >> shift;
        let high = if shift > 3 {
            u32::from(packed.get(byte + 1).copied().unwrap_or(0)) << (8 - shift)
        } else {
            0
        };
        *bar = ((low | high) & 0x1f) as u8;
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A tone at the top of the range: every slice's peak is the same, so
    /// every bar is full — and 1.8 times the mean is what the scale is,
    /// which puts a steady tone at 31/1.8, not at 31.
    #[test]
    fn a_steady_tone_draws_a_steady_line() {
        let tone: Vec<i16> = (0..48_000)
            .map(|n| {
                let t = f64::from(n) / 48_000.0;
                (i16::MAX as f64 * (t * 440.0 * std::f64::consts::TAU).sin()) as i16
            })
            .collect();
        let bars = bars(&tone);
        let first = bars[0];
        assert!(
            (16..=18).contains(&first),
            "a full-scale tone against 1.8× its own mean sits near 17, not {first}"
        );
        assert!(
            bars.iter().all(|b| b.abs_diff(first) <= 1),
            "a steady tone is a flat line: {bars:?}"
        );
        let packed = of_samples(&tone);
        assert_eq!(packed.len(), PACKED);
        assert_eq!(unpack(&packed), bars, "what was packed reads back");
    }

    /// Silence is a flat zero, not a full line: the floor under the scale is
    /// what keeps a quiet room from being drawn as a shout.
    #[test]
    fn silence_is_a_flat_zero() {
        let quiet = vec![0i16; 24_000];
        assert_eq!(bars(&quiet), [0u8; SLICES]);
        assert_eq!(of_samples(&quiet), vec![0u8; PACKED]);
        // And so is nothing at all, rather than a field the row cannot draw.
        assert_eq!(of_samples(&[]).len(), PACKED);
    }

    /// One shout in a quiet note: the loud slice is full and the quiet ones
    /// stay low, which is the whole point of measuring against the mean.
    #[test]
    fn one_shout_does_not_flatten_the_rest() {
        let mut samples = vec![600i16; 48_000];
        for s in &mut samples[24_000..24_480] {
            *s = i16::MAX;
        }
        let bars = bars(&samples);
        let loud = bars[50];
        assert_eq!(loud, 31, "the shout is full");
        assert!(
            bars[0] > 0 && bars[0] < 8,
            "the talking around it still shows, low: {}",
            bars[0]
        );
    }

    /// The five-bit packing, by hand, so the bit order is not a matter of
    /// opinion: bar 0 is the low five bits of byte 0, bar 1 straddles byte
    /// 0 and byte 1.
    #[test]
    fn the_bits_go_in_least_significant_first() {
        let mut bars = [0u8; SLICES];
        bars[0] = 0b1_0001;
        bars[1] = 0b1_1110;
        let packed = pack(&bars);
        assert_eq!(packed[0], 0b1101_0001);
        assert_eq!(packed[1], 0b0000_0011);
        assert_eq!(unpack(&packed)[..2], [0b1_0001, 0b1_1110]);
    }
}
