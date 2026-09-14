//! A voice note, written as the clients record one: Opus in Ogg.
//!
//! Telegram's wire calls a voice note a document with the *voice* attribute,
//! and every client writes the same file behind it — 48 kHz mono, 20 ms
//! frames, around 30 kbps, in libopus' VoIP mode, paged into an Ogg stream
//! with the `OpusHead` and `OpusTags` headers RFC 7845 asks for. Anything
//! else plays on the machine that made it and nowhere else.
//!
//! A microphone is not 48 kHz mono, so the samples come through
//! [`pcm::Resampler`](super::pcm::Resampler) on their way in.
//!
//! [`Voice`] is written to while the recording runs and answers the file's
//! length and its [waveform](super::waveform) at the end, because the
//! waveform is over the whole recording and a recording only has a whole
//! when it stops.

use std::fs::File;
use std::io::BufWriter;
use std::path::{Path, PathBuf};

use super::pcm::{self, Resampler};
use super::waveform;

/// The one rate Opus decodes to, and so the rate everything here is in.
pub const RATE: u32 = pcm::RATE;

/// 20 ms of it: the frame every client records in.
pub const FRAME: usize = (RATE as usize) / 50;

/// What a voice note is worth, in bits per second. The clients' rate.
pub const BITRATE: i32 = 30_000;

/// The shortest recording that counts as one. Below this a press was a
/// press, not a note, and the panel says so rather than sending a click.
pub const LEAST: f64 = 0.5;

/// The most one encoded frame can weigh, as libopus documents it.
const PACKET_MAX: usize = 4000;

/// The stream's serial number. A constant because a voice note is one
/// logical stream in a file of its own: there is nothing to tell it apart
/// from, and a fixed one keeps two recordings of the same samples byte for
/// byte the same file.
const SERIAL: u32 = 0x5375_7065;

/// What a finished recording is.
#[derive(Debug, Clone, PartialEq)]
pub struct Recorded {
    pub path: PathBuf,
    /// How long it plays, in seconds.
    pub secs: f64,
    /// The hundred bars, packed — [`waveform::PACKED`] bytes.
    pub waveform: Vec<u8>,
}

/// A voice note being written.
///
/// Samples go in at whatever rate the device hands over and frames come out
/// at 48 kHz; the resampler's position is kept between calls, so a recording
/// pushed in a hundred small buffers is the same file as one pushed whole.
pub struct Voice {
    path: PathBuf,
    pages: ogg::PacketWriter<'static, BufWriter<File>>,
    encoder: opus::Encoder,
    /// What the decoder throws away at the start: libopus' own lookahead,
    /// which the header carries so a player does not begin with silence.
    pre_skip: u16,
    /// The device's rate into the one Opus keeps.
    rate: Resampler,
    /// Resampled samples not yet a whole frame.
    pending: Vec<i16>,
    /// The whole recording at 48 kHz, for the waveform. A minute of talking
    /// is six megabytes, which is what the rule costs: it is the peaks of a
    /// hundred slices of the *whole* note, and there is no whole until the
    /// recording stops.
    heard: Vec<i16>,
    /// Frames written, which is the granule position in 20 ms steps.
    frames: u64,
}

impl Voice {
    /// Opens a note at `path` and writes its two header pages.
    ///
    /// # Errors
    ///
    /// If the file cannot be made, libopus refuses the settings, or the
    /// headers cannot be written.
    pub fn create(path: &Path, input_rate: f64) -> Result<Voice, String> {
        let rate = Resampler::from(input_rate)?;
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
        }
        let file = File::create(path).map_err(|e| format!("{}: {e}", path.display()))?;
        let mut encoder = opus::Encoder::new(RATE, opus::Channels::Mono, opus::Application::Voip)
            .map_err(|e| format!("opus: {e}"))?;
        encoder
            .set_bitrate(opus::Bitrate::Bits(BITRATE))
            .map_err(|e| format!("opus: {e}"))?;
        let pre_skip = u16::try_from(encoder.get_lookahead().unwrap_or(312).max(0)).unwrap_or(312);
        let mut pages = ogg::PacketWriter::new(BufWriter::new(file));
        write_page(&mut pages, head(pre_skip))?;
        write_page(&mut pages, tags())?;
        Ok(Voice {
            path: path.to_path_buf(),
            pages,
            encoder,
            pre_skip,
            rate,
            pending: Vec::with_capacity(FRAME * 2),
            heard: Vec::new(),
            frames: 0,
        })
    }

    /// One buffer of the microphone's samples, mono, as floats between -1
    /// and 1.
    ///
    /// # Errors
    ///
    /// If libopus refuses a frame or the file cannot be written.
    pub fn push(&mut self, samples: &[f32]) -> Result<(), String> {
        let was = self.pending.len();
        self.rate.push(samples, &mut self.pending);
        self.heard.extend_from_slice(&self.pending[was..]);
        while self.pending.len() >= FRAME {
            let frame: Vec<i16> = self.pending.drain(..FRAME).collect();
            let packet = self
                .encoder
                .encode_vec(&frame, PACKET_MAX)
                .map_err(|e| format!("opus: {e}"))?;
            self.frames += 1;
            let granule = self.frames * FRAME as u64 + u64::from(self.pre_skip);
            self.pages
                .write_packet(packet, SERIAL, ogg::PacketWriteEndInfo::NormalPacket, granule)
                .map_err(|e| format!("ogg: {e}"))?;
        }
        Ok(())
    }

    /// Ends the stream and answers the file, its length and its waveform.
    ///
    /// The tail shorter than a frame is padded with silence rather than
    /// dropped, so the length the header claims is the length there is.
    ///
    /// # Errors
    ///
    /// If the last frame or the file's end cannot be written.
    pub fn finish(mut self) -> Result<Recorded, String> {
        // Always one last frame, padded with silence, even when nothing is
        // pending: Ogg marks the end of a stream on a packet, and there has
        // to be a packet to mark. It costs twenty milliseconds of quiet.
        self.pending.resize(FRAME, 0);
        let frame = std::mem::take(&mut self.pending);
        let packet = self
            .encoder
            .encode_vec(&frame, PACKET_MAX)
            .map_err(|e| format!("opus: {e}"))?;
        self.frames += 1;
        let granule = self.frames * FRAME as u64 + u64::from(self.pre_skip);
        self.pages
            .write_packet(packet, SERIAL, ogg::PacketWriteEndInfo::EndStream, granule)
            .map_err(|e| format!("ogg: {e}"))?;
        drop(self.pages);
        Ok(Recorded {
            path: self.path,
            // What was heard, not what was written: the silence the last
            // frame was padded with is not part of the note.
            secs: self.heard.len() as f64 / f64::from(RATE),
            waveform: waveform::of_samples(&self.heard),
        })
    }
}

/// The `OpusHead` packet: one channel at 48 kHz, no gain, no mapping.
fn head(pre_skip: u16) -> Vec<u8> {
    let mut h = Vec::with_capacity(19);
    h.extend_from_slice(b"OpusHead");
    h.push(1); // version
    h.push(1); // channels
    h.extend_from_slice(&pre_skip.to_le_bytes());
    h.extend_from_slice(&RATE.to_le_bytes());
    h.extend_from_slice(&0i16.to_le_bytes()); // output gain
    h.push(0); // mapping family: mono, nothing to map
    h
}

/// The `OpusTags` packet: who wrote it, and no comments.
fn tags() -> Vec<u8> {
    const VENDOR: &[u8] = b"superapp";
    let mut t = Vec::with_capacity(8 + 4 + VENDOR.len() + 4);
    t.extend_from_slice(b"OpusTags");
    t.extend_from_slice(&(VENDOR.len() as u32).to_le_bytes());
    t.extend_from_slice(VENDOR);
    t.extend_from_slice(&0u32.to_le_bytes());
    t
}

/// A header packet, alone on its own page, as RFC 7845 requires.
fn write_page(
    pages: &mut ogg::PacketWriter<'static, BufWriter<File>>,
    packet: Vec<u8>,
) -> Result<(), String> {
    pages
        .write_packet(packet, SERIAL, ogg::PacketWriteEndInfo::EndPage, 0)
        .map_err(|e| format!("ogg: {e}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tone(secs: f64, rate: f64) -> Vec<f32> {
        (0..(secs * rate) as usize)
            .map(|n| {
                let t = n as f64 / rate;
                (0.8 * (t * 440.0 * std::f64::consts::TAU).sin()) as f32
            })
            .collect()
    }

    /// A tone written and read back: the pages parse, the headers are the
    /// two RFC 7845 asks for, and the length is the length that went in.
    #[test]
    fn a_tone_is_a_readable_ogg_opus_stream() {
        let dir = tempfile::tempdir().expect("a temp dir");
        let path = dir.path().join("note.ogg");
        let mut voice = Voice::create(&path, 48_000.0).expect("an encoder");
        // Pushed in pieces, as a microphone hands them over.
        for chunk in tone(2.0, 48_000.0).chunks(512) {
            voice.push(chunk).expect("a frame");
        }
        let done = voice.finish().expect("the file");
        assert_eq!(done.path, path);
        assert!(
            (done.secs - 2.0).abs() < 0.05,
            "two seconds in, two seconds out: {}",
            done.secs
        );
        assert_eq!(done.waveform.len(), waveform::PACKED);
        assert!(
            done.waveform.iter().any(|b| *b != 0),
            "a tone is not a flat line"
        );

        let bytes = std::fs::read(&path).expect("the file");
        assert!(bytes.len() > 1000, "a couple of seconds weighs something");
        let mut reader = ogg::PacketReader::new(std::io::Cursor::new(bytes));
        let first = reader.read_packet_expected().expect("OpusHead");
        assert!(first.data.starts_with(b"OpusHead"));
        assert_eq!(first.data[9], 1, "one channel");
        let second = reader.read_packet_expected().expect("OpusTags");
        assert!(second.data.starts_with(b"OpusTags"));
        let mut frames = 0;
        let mut last = 0;
        while let Some(packet) = reader.read_packet().expect("a page") {
            frames += 1;
            last = packet.absgp_page();
        }
        assert!(frames >= 99, "a hundred frames of twenty milliseconds: {frames}");
        assert!(last > 96_000, "the granule counts the samples: {last}");
    }

    /// A microphone at another rate lands at 48 kHz all the same, and the
    /// length is the length of what was said, not of what was sampled.
    #[test]
    fn another_rate_is_resampled_to_the_one_opus_has() {
        let dir = tempfile::tempdir().expect("a temp dir");
        let path = dir.path().join("note.ogg");
        let mut voice = Voice::create(&path, 44_100.0).expect("an encoder");
        for chunk in tone(1.0, 44_100.0).chunks(441) {
            voice.push(chunk).expect("a frame");
        }
        let done = voice.finish().expect("the file");
        assert!(
            (done.secs - 1.0).abs() < 0.05,
            "a second at 44.1 kHz is a second at 48: {}",
            done.secs
        );
    }

    /// A rate no device has is refused before a file is made.
    #[test]
    fn an_impossible_rate_is_refused() {
        let dir = tempfile::tempdir().expect("a temp dir");
        let path = dir.path().join("note.ogg");
        assert!(Voice::create(&path, 0.0).is_err());
        assert!(!path.exists(), "and nothing was written");
    }
}

