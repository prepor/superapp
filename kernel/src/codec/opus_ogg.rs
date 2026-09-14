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
use std::io::{BufReader, BufWriter};
use std::path::{Path, PathBuf};

use super::pcm::{self, Pcm, Resampler};
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

/// The most one Opus packet decodes to, per channel: 120 ms at 48 kHz, the
/// longest frame the format allows.
const DECODED_MAX: usize = (RATE as usize) / 1000 * 120;

/// Reads a voice note back: an Ogg Opus file as samples the app can play.
///
/// The counterpart of [`Voice`], and here rather than in the shell for the
/// same reason the writer is — it is arithmetic over bytes. It exists
/// because AVFoundation will not play Opus at all, so on the Mac a received
/// voice note has no platform player to hand to and the app decodes it
/// itself ([CR-020](../../../../docs/planning/cr-020-telegram-senses.md)).
///
/// What comes back is mono at 48 kHz, which is what every voice note is;
/// a stereo file — a track somebody sent as Opus — is folded to one channel
/// rather than refused. The `pre_skip` the header names is thrown away, as
/// RFC 7845 says it must be, and the last page's granule position trims the
/// silence an encoder padded its final frame with.
///
/// # Errors
///
/// If the file cannot be read, is not an Ogg Opus stream, or libopus
/// refuses a packet.
pub fn decode(path: &Path) -> Result<Pcm, String> {
    let file = File::open(path).map_err(|e| format!("{}: {e}", path.display()))?;
    let mut pages = ogg::PacketReader::new(BufReader::new(file));
    let read = |pages: &mut ogg::PacketReader<BufReader<File>>| {
        pages.read_packet().map_err(|e| format!("ogg: {e}"))
    };
    // Every Ogg Opus stream opens with `OpusHead` and `OpusTags`, each alone
    // on its own page; everything after them is sound.
    let head = read(&mut pages)?.ok_or_else(|| "not an Ogg stream".to_string())?;
    let head = Head::parse(&head.data)?;
    read(&mut pages)?.ok_or_else(|| "the stream ends after its header".to_string())?;

    let channels = if head.channels == 1 { opus::Channels::Mono } else { opus::Channels::Stereo };
    let mut decoder = opus::Decoder::new(RATE, channels).map_err(|e| format!("opus: {e}"))?;
    let mut frame = vec![0f32; DECODED_MAX * head.channels as usize];
    let mut samples: Vec<f32> = Vec::new();
    // Where the last page says the stream ends. A page that finishes no
    // packet carries `-1`, which is not a position.
    let mut end = 0u64;
    while let Some(packet) = read(&mut pages)? {
        let n = decoder
            .decode_float(&packet.data, &mut frame, false)
            .map_err(|e| format!("opus: {e}"))?;
        if head.channels == 1 {
            samples.extend_from_slice(&frame[..n]);
        } else {
            // Both ears in one: the mean, which keeps a centred voice at
            // its level and does not clip a hard-panned one.
            let stride = head.channels as usize;
            samples.extend((0..n).map(|i| {
                frame[i * stride..i * stride + stride].iter().sum::<f32>() / stride as f32
            }));
        }
        let granule = packet.absgp_page();
        if granule != u64::MAX {
            end = end.max(granule);
        }
    }
    // The gain the header carries, in 256ths of a decibel.
    if head.gain != 0 {
        let scale = 10f32.powf(f32::from(head.gain) / (20.0 * 256.0));
        for s in &mut samples {
            *s *= scale;
        }
    }
    let skip = usize::from(head.pre_skip).min(samples.len());
    samples.drain(..skip);
    // The last page says how many samples the stream really is, so the
    // silence a writer padded its final frame with is not played.
    let played = usize::try_from(end.saturating_sub(u64::from(head.pre_skip))).unwrap_or(usize::MAX);
    if played > 0 && played < samples.len() {
        samples.truncate(played);
    }
    Ok(Pcm { samples, rate: RATE })
}

/// What `OpusHead` says: how many channels, what to throw away at the
/// start, and what to multiply the result by.
struct Head {
    channels: u8,
    pre_skip: u16,
    gain: i16,
}

impl Head {
    fn parse(packet: &[u8]) -> Result<Head, String> {
        if !packet.starts_with(b"OpusHead") || packet.len() < 19 {
            return Err("not an Opus stream".to_string());
        }
        let channels = packet[9];
        if channels == 0 || channels > 2 || packet[18] != 0 {
            return Err(format!("{channels} channels is not a recording this plays"));
        }
        Ok(Head {
            channels,
            pre_skip: u16::from_le_bytes([packet[10], packet[11]]),
            gain: i16::from_le_bytes([packet[16], packet[17]]),
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

    /// How strongly one frequency stands in a run of samples — Goertzel's
    /// single bin, which is the whole of a Fourier transform one is
    /// interested in.
    fn strength(samples: &[f32], freq: f64) -> f64 {
        let n = samples.len() as f64;
        let k = (n * freq / f64::from(RATE)).round();
        let w = std::f64::consts::TAU * k / n;
        let coeff = 2.0 * w.cos();
        let (mut s1, mut s2) = (0.0f64, 0.0f64);
        for &x in samples {
            let s0 = f64::from(x) + coeff * s1 - s2;
            s2 = s1;
            s1 = s0;
        }
        ((s1 * s1 + s2 * s2 - coeff * s1 * s2).max(0.0)).sqrt() / n
    }

    /// The tone that went in is the tone that comes out: written by the
    /// encoder a recording uses, read back by the decoder a player uses.
    /// Opus is lossy, so this asks what a listener would — that the note is
    /// the same note, at about the same loudness, for about as long.
    #[test]
    fn a_tone_written_is_the_tone_read_back() {
        let dir = tempfile::tempdir().expect("a temp dir");
        let path = dir.path().join("note.ogg");
        let mut voice = Voice::create(&path, 48_000.0).expect("an encoder");
        for chunk in tone(2.0, 48_000.0).chunks(512) {
            voice.push(chunk).expect("a frame");
        }
        let written = voice.finish().expect("the file");

        let heard = decode(&path).expect("the note read back");
        assert_eq!(heard.rate, RATE);
        assert!(
            (heard.secs() - written.secs).abs() < 0.05,
            "two seconds in, two seconds out: {}",
            heard.secs()
        );
        // Opus needs a moment to find a steady tone; the judgement is over
        // the middle of the note, not its first breath.
        let middle = &heard.samples[RATE as usize / 2..heard.samples.len() - RATE as usize / 4];
        let at_440 = strength(middle, 440.0);
        assert!(
            at_440 > 0.2,
            "the 440 Hz the recording was is still the loudest thing in it: {at_440}"
        );
        for other in [220.0, 660.0, 1_000.0, 3_000.0] {
            let elsewhere = strength(middle, other);
            assert!(
                elsewhere * 10.0 < at_440,
                "{other} Hz is not in a 440 Hz tone: {elsewhere} against {at_440}"
            );
        }
        let rms = (middle.iter().map(|s| f64::from(*s) * f64::from(*s)).sum::<f64>()
            / middle.len() as f64)
            .sqrt();
        assert!(
            (0.3..0.8).contains(&rms),
            "and it comes back at the level it went in at: {rms}"
        );
    }

    /// Nothing here trusts its input: a file that is not a stream, and a
    /// stream that is not Opus, are refused rather than played as noise.
    #[test]
    fn what_is_not_a_voice_note_is_refused() {
        let dir = tempfile::tempdir().expect("a temp dir");
        let path = dir.path().join("not-a-note");
        std::fs::write(&path, b"this is not an ogg file at all").expect("the file");
        assert!(decode(&path).is_err());
        assert!(decode(&dir.path().join("nothing-here.ogg")).is_err());
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

