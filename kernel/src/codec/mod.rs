//! The encodings a capture is sent in.
//!
//! The senses answer in what a device happens to hear and see — samples at
//! whatever rate the microphone runs at, frames in whatever layout the
//! camera hands over — and a wire wants something else. This module is the
//! translation, and it is here rather than in the shell because it is
//! arithmetic over bytes: a scripted run's fake capture writes the very
//! files a real one does, which is what makes a fixture line drawn from a
//! fake a line drawn from a real recording.
//!
//! Three of them, one per capture:
//!
//! - [`opus_ogg`] writes a voice note — Opus in Ogg, 48 kHz mono, the one
//!   encoding Telegram's clients record and play — and reads one back,
//!   because no player Apple ships will take Opus, so on the Mac a note
//!   that arrives is decoded here before the shell's mixer plays it.
//! - [`waveform`] is the hundred bars drawn under it, by the clients' own
//!   rule, so a note recorded here looks the same in every other client.
//! - [`jpeg`] writes a photograph, and the thumbnail of a video message.
//!
//! [`pcm`] is under the sound in both of the first two — a voice note and
//! the track of a video message — because no microphone runs at the rate a
//! codec was told to expect, and every recording is resampled on its way in.
//!
//! Nothing here reaches the outside: each takes bytes and answers bytes or
//! a file it was told to write.

pub mod jpeg;
pub mod opus_ogg;
pub mod pcm;
pub mod waveform;
