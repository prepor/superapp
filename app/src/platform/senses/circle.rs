//! The mp4 a video message is written into, on whichever platform.
//!
//! One shape, two machines behind it. A video message is the same file
//! everywhere — 384 pixels square, H.264 at a megabit, thirty frames a
//! second, a mono AAC track at 48 kHz — because it is a Telegram document
//! and every other client has to play it. What differs is who does the
//! encoding: a Mac has AVAssetWriter through makepad's own file encoder,
//! and a phone has `MediaCodec` and `MediaMuxer`, which the fork carries no
//! encoder for, so the app talks to them itself
//! ([`recorder`](crate::platform::android::recorder)).
//!
//! Frames arrive as NV12 already cropped and scaled: the capture thread
//! does that, because it is the one place that has the frame and nobody is
//! waiting on it.

/// What a video message is worth: the clients' megabit.
pub const VIDEO_BITRATE: u32 = 1_000_000;

/// And its sound: mono AAC at the rate everything is resampled to.
pub const AUDIO_BITRATE: u32 = 64_000;

/// The frames a second the file declares.
pub const FPS: u32 = 30;

#[cfg(target_os = "macos")]
pub use mac::Encoder;

#[cfg(target_os = "android")]
pub use crate::platform::android::recorder::Encoder;

#[cfg(not(any(target_os = "macos", target_os = "android")))]
pub use elsewhere::Encoder;

// -- macOS ---------------------------------------------------------------------

#[cfg(target_os = "macos")]
mod mac {
    //! AVAssetWriter, through makepad's file encoder: it takes NV12 frames
    //! and 16-bit PCM and hands the container to VideoToolbox and the
    //! system AAC encoder, which is exactly the shape of a circle.

    use std::path::Path;

    use super::{AUDIO_BITRATE, FPS, VIDEO_BITRATE};
    use makepad_video::{
        PcmAudioTrackOptions, VideoFileCodec, VideoFileEncoder, VideoFileEncoderOptions,
    };

    /// The open file, until [`Encoder::finish`] takes it. `None` after,
    /// so a second `finish` is not a second file.
    pub struct Encoder(Option<VideoFileEncoder>);

    /// Seconds as the encoder counts time: hundreds of nanoseconds, which is
    /// what its `pts` is in.
    fn hns(secs: f64) -> i64 {
        (secs * 10_000_000.0) as i64
    }

    impl Encoder {
        /// Opens the file. The side is even by construction, which the
        /// encoder requires of a 4:2:0 picture.
        ///
        /// # Errors
        ///
        /// If the path is not a name the platform can take, or
        /// AVFoundation refused the settings.
        pub fn create(path: &Path, side: u32) -> Result<Encoder, String> {
            let name = path
                .to_str()
                .ok_or_else(|| format!("{} is not a name AVFoundation takes", path.display()))?;
            let options = VideoFileEncoderOptions {
                codec: VideoFileCodec::H264,
                width: side,
                height: side,
                fps_num: FPS,
                fps_den: 1,
                video_bitrate_bps: VIDEO_BITRATE,
                audio: Some(PcmAudioTrackOptions {
                    sample_rate: kernel::codec::pcm::RATE,
                    channels: 1,
                    aac_bitrate_bps: AUDIO_BITRATE,
                }),
                keyframe_only: false,
            };
            VideoFileEncoder::new(name, options)
                .map(|encoder| Encoder(Some(encoder)))
                .map_err(|e| format!("the video encoder refused: {e}"))
        }

        /// One square NV12 frame, `at` seconds into the recording — the
        /// moment the camera handed it over, not a frame counter: a camera
        /// that answers a rate of its own, or a frame the channel dropped,
        /// would otherwise move the picture against its own sound.
        ///
        /// # Errors
        ///
        /// If the encoder refused the frame.
        pub fn push_frame(&mut self, nv12: &[u8], at: f64) -> Result<(), String> {
            self.writer()?
                .push_frame_nv12(nv12, Some(hns(at)))
                .map_err(|e| format!("the video encoder refused a frame: {e}"))
        }

        /// One buffer of mono 16-bit PCM at 48 kHz.
        ///
        /// # Errors
        ///
        /// If the encoder refused the samples.
        pub fn push_audio(&mut self, pcm: &[i16]) -> Result<(), String> {
            if pcm.is_empty() {
                return Ok(());
            }
            self.writer()?
                .push_audio_i16(pcm)
                .map_err(|e| format!("the audio encoder refused a buffer: {e}"))
        }

        /// Finalizes the container, without which the file is not playable.
        ///
        /// # Errors
        ///
        /// If the writer could not finish.
        pub fn finish(mut self) -> Result<(), String> {
            let Some(encoder) = self.0.take() else {
                return Err("the video encoder is already closed".to_string());
            };
            encoder
                .finish()
                .map_err(|e| format!("the video encoder could not finish: {e}"))
        }

        fn writer(&mut self) -> Result<&mut VideoFileEncoder, String> {
            self.0
                .as_mut()
                .ok_or_else(|| "the video encoder is already closed".to_string())
        }
    }
}

// -- everywhere else -----------------------------------------------------------

#[cfg(not(any(target_os = "macos", target_os = "android")))]
mod elsewhere {
    //! No encoder here. Said plainly, so the panel refuses the recording
    //! rather than writing a file nothing can play.

    use std::path::Path;

    /// One that never opens, so the rest of the app compiles and says why.
    pub struct Encoder;

    impl Encoder {
        /// # Errors
        ///
        /// Always: this platform has no video encoder.
        pub fn create(path: &Path, side: u32) -> Result<Encoder, String> {
            let _ = (path, side);
            Err("no video encoder on this platform".to_string())
        }

        /// # Errors
        ///
        /// Never reached: there is no encoder to push to.
        pub fn push_frame(&mut self, nv12: &[u8], at: f64) -> Result<(), String> {
            let _ = (nv12, at);
            Err("no video encoder on this platform".to_string())
        }

        /// # Errors
        ///
        /// Never reached: there is no encoder to push to.
        pub fn push_audio(&mut self, pcm: &[i16]) -> Result<(), String> {
            let _ = pcm;
            Err("no video encoder on this platform".to_string())
        }

        /// # Errors
        ///
        /// Never reached: there is no encoder to finish.
        pub fn finish(self) -> Result<(), String> {
            Err("no video encoder on this platform".to_string())
        }
    }
}
