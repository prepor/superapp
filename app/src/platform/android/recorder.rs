//! The phone's video encoder: `MediaCodec` and `MediaMuxer` over JNI.
//!
//! A Mac writes a video message through makepad's own file encoder, which is
//! AVAssetWriter behind a `Cx`-free seam. The fork carries no android
//! backend for it, so the app talks to the framework itself — and to the
//! framework *only*: every class named here (`MediaCodec`, `MediaFormat`,
//! `MediaMuxer`, `Image`) ships with the platform, so there is no Java of
//! ours to compile, package or keep in step.
//!
//! In the **synchronous** mode, which is the one that needs no callback
//! object: ask for an input buffer, fill it, queue it, then pull whatever
//! came out. Everything here runs on the recorder's own thread, the one
//! [`senses`](crate::platform::senses) spawned for this recording, so no
//! frame of the window waits on an encoder.
//!
//! Two things are worth knowing about the shape:
//!
//! - **The colour format is `COLOR_FormatYUV420Flexible`**, which means the
//!   codec decides the layout and hands over an `Image` with three planes
//!   and their strides rather than a flat buffer. The frame arrives here as
//!   NV12 and is written plane by plane against those strides, which is the
//!   one way that is right on both a planar and a semi-planar encoder.
//! - **A muxer cannot take a sample before both tracks are added**, and a
//!   track is added only when its encoder reports its format — which is
//!   after it has been fed. So the first encoded samples are *held* here
//!   rather than dropped: the first video sample is the keyframe, and a file
//!   that starts without one starts grey.
//!
//! None of this can be proved on a build machine: there is no camera behind
//! it and no phone under it. What is proved here is that it compiles for the
//! phone; the picture is Andrey's to see on the glass.

use std::path::Path;

use jni::objects::{GlobalRef, JByteBuffer, JObject, JObjectArray, JValue};
use jni::{JNIEnv, JavaVM};

use crate::platform::senses::circle::{AUDIO_BITRATE, FPS, VIDEO_BITRATE};

/// `MediaCodec.CONFIGURE_FLAG_ENCODE`.
const ENCODE: i32 = 1;
/// `MediaCodecInfo.CodecCapabilities.COLOR_FormatYUV420Flexible`.
const COLOR_YUV420_FLEXIBLE: i32 = 0x7F42_0888;
/// `MediaCodecInfo.CodecProfileLevel.AACObjectLC`.
const AAC_LC: i32 = 2;
/// `MediaMuxer.OutputFormat.MUXER_OUTPUT_MPEG_4`.
const MPEG_4: i32 = 0;
/// `MediaCodec.INFO_OUTPUT_FORMAT_CHANGED`.
const FORMAT_CHANGED: i32 = -2;
/// `MediaCodec.BUFFER_FLAG_CODEC_CONFIG`: the parameter sets, which the
/// muxer takes from the format rather than as a sample.
const CODEC_CONFIG: i32 = 2;
/// `MediaCodec.BUFFER_FLAG_END_OF_STREAM`.
const END_OF_STREAM: i32 = 4;

/// A keyframe a second, as the clients record one.
const KEYFRAME_SECONDS: i32 = 1;

/// How long an input buffer is waited for, in microseconds. Long enough
/// that a busy encoder is given its moment, short enough that a dropped
/// frame is a dropped frame rather than a stall.
const WAIT_US: i64 = 10_000;

/// How many local references one JNI round may make. Generous: a frame
/// touches an image, three planes and three buffers.
const LOCAL_REFS: i32 = 32;

/// How many rounds the end of a recording is pumped for before it is called
/// finished anyway — about a second at [`WAIT_US`].
const DRAIN_TRIES: usize = 200;

/// A JNI failure, in the words it can be said in.
struct Fault(String);

impl From<jni::errors::Error> for Fault {
    fn from(e: jni::errors::Error) -> Fault {
        Fault(e.to_string())
    }
}

type Faulted<T> = Result<T, Fault>;

/// One encoded sample on its way to the muxer, copied out of the codec's
/// own buffer so that it can wait here for the muxer to be ready.
struct Sample {
    video: bool,
    bytes: Vec<u8>,
    pts_us: i64,
    flags: i32,
}

/// A video message being written on the phone.
pub struct Encoder {
    video: GlobalRef,
    audio: GlobalRef,
    muxer: GlobalRef,
    /// One `MediaCodec.BufferInfo`, reused: it is an out-parameter, and
    /// making one per frame would be a thousand objects a recording.
    info: GlobalRef,
    side: usize,
    /// The muxer's track numbers, below zero until each format arrives.
    video_track: i32,
    audio_track: i32,
    /// Whether the muxer has started, which it may only do once both tracks
    /// are added.
    running: bool,
    /// Samples encoded before that moment.
    held: Vec<Sample>,
    /// Samples of sound written, which is what its timestamps are counted in.
    sounded: u64,
    /// Whether each encoder has reported the end of its stream.
    video_ended: bool,
    audio_ended: bool,
}

impl Encoder {
    /// Opens the two encoders and the muxer.
    ///
    /// # Errors
    ///
    /// If the JVM is not up, the phone has no encoder for H.264 or AAC, or
    /// the file cannot be made.
    pub fn create(path: &Path, side: u32) -> Result<Encoder, String> {
        let name = path
            .to_str()
            .ok_or_else(|| format!("{} is not a name MediaMuxer takes", path.display()))?
            .to_string();
        let width = side as i32;
        let (video, audio, muxer, info) = with_jni(|env| open(env, &name, width))?;
        Ok(Encoder {
            video,
            audio,
            muxer,
            info,
            side: side as usize,
            video_track: -1,
            audio_track: -1,
            running: false,
            held: Vec::new(),
            sounded: 0,
            video_ended: false,
            audio_ended: false,
        })
    }

    /// One square NV12 frame, `at` seconds into the recording — the moment
    /// the camera handed it over, not a frame counter: a camera that answers
    /// a rate of its own, or a frame the channel dropped, would otherwise
    /// move the picture against its own sound, which the muxer times off the
    /// samples it was given.
    ///
    /// # Errors
    ///
    /// If the encoder or the muxer refused it.
    pub fn push_frame(&mut self, nv12: &[u8], at: f64) -> Result<(), String> {
        let pts = (at * 1_000_000.0) as i64;
        let side = self.side;
        // Whether an input buffer was free: a frame the encoder was too busy
        // for is a dropped frame, and the next one carries its own moment,
        // so there is nothing to make good afterwards.
        let _taken = with_jni(|env| feed_picture(env, &self.video, nv12, side, pts))?;
        self.drain(true)
    }

    /// One buffer of mono 16-bit PCM at 48 kHz.
    ///
    /// # Errors
    ///
    /// If the encoder or the muxer refused it.
    pub fn push_audio(&mut self, pcm: &[i16]) -> Result<(), String> {
        if pcm.is_empty() {
            return Ok(());
        }
        let rate = u64::from(kernel::codec::pcm::RATE);
        let mut written = 0usize;
        while written < pcm.len() {
            let pts = (self.sounded * 1_000_000) / rate;
            let took = with_jni(|env| feed_sound(env, &self.audio, &pcm[written..], pts as i64))?;
            if took == 0 {
                // No input buffer free: the encoder is behind, and a
                // recording's sound is not worth stalling its picture for.
                break;
            }
            written += took;
            self.sounded += took as u64;
            self.drain(false)?;
        }
        Ok(())
    }

    /// Ends both streams, writes what is left, and closes the file.
    ///
    /// # Errors
    ///
    /// If the muxer could not be finished — which means an unplayable file.
    pub fn finish(mut self) -> Result<(), String> {
        let pts = ((self.sounded * 1_000_000) / u64::from(kernel::codec::pcm::RATE)) as i64;
        with_jni(|env| end_of_stream(env, &self.video, 0))?;
        with_jni(|env| end_of_stream(env, &self.audio, pts))?;
        for _ in 0..DRAIN_TRIES {
            if self.video_ended && self.audio_ended {
                break;
            }
            if !self.video_ended {
                self.drain(true)?;
            }
            if !self.audio_ended {
                self.drain(false)?;
            }
        }
        // The encoders are released whatever the muxer says: a recorder
        // that held them open would keep the phone's own codec busy.
        let running = self.running;
        with_jni(|env| close(env, &self.video, &self.audio, &self.muxer, running))
    }

    /// Pulls whatever one encoder has made, adds its track the first time it
    /// says what its format is, and writes what the muxer will take.
    fn drain(&mut self, video: bool) -> Result<(), String> {
        let codec = if video { &self.video } else { &self.audio };
        let (track, samples, ended) =
            with_jni(|env| pump(env, codec, &self.info, &self.muxer, video))?;
        if let Some(track) = track {
            if video {
                self.video_track = track;
            } else {
                self.audio_track = track;
            }
        }
        self.held.extend(samples);
        if ended {
            if video {
                self.video_ended = true;
            } else {
                self.audio_ended = true;
            }
        }
        self.write_held()
    }

    /// The muxer takes what it can: nothing until both tracks are known, and
    /// then everything that has been waiting, in the order it was encoded.
    fn write_held(&mut self) -> Result<(), String> {
        if !self.running {
            if self.video_track < 0 || self.audio_track < 0 {
                return Ok(());
            }
            with_jni(|env| {
                env.call_method(&self.muxer, "start", "()V", &[])?;
                Ok(())
            })?;
            self.running = true;
        }
        if self.held.is_empty() {
            return Ok(());
        }
        let held = std::mem::take(&mut self.held);
        let (video_track, audio_track) = (self.video_track, self.audio_track);
        with_jni(|env| {
            for sample in &held {
                let track = if sample.video { video_track } else { audio_track };
                write_sample(env, &self.muxer, track, sample)?;
            }
            Ok(())
        })
    }
}

/// One round of JNI on this thread, inside a frame of its own.
///
/// The recorder's thread never returns to Java, so every local reference it
/// makes is freed here or not at all — and a pending exception left on it
/// would fail the next call for a reason nobody could read.
fn with_jni<T>(f: impl FnOnce(&mut JNIEnv<'_>) -> Faulted<T>) -> Result<T, String> {
    let raw = makepad_android_state::get_java_vm();
    if raw.is_null() {
        return Err("the JVM is not up yet".to_string());
    }
    // SAFETY: the process's own JavaVM, as `platform::android` reads it.
    let vm = unsafe { JavaVM::from_raw(raw.cast()) }.map_err(|e| e.to_string())?;
    let mut env = vm
        .attach_current_thread_as_daemon()
        .map_err(|e| e.to_string())?;
    let out = env.with_local_frame(LOCAL_REFS, f);
    if out.is_err() {
        let _ = env.exception_clear();
    }
    out.map_err(|Fault(why)| why)
}

/// The two encoders and the muxer, configured and started.
fn open(
    env: &mut JNIEnv<'_>,
    path: &str,
    side: i32,
) -> Faulted<(GlobalRef, GlobalRef, GlobalRef, GlobalRef)> {
    let video_format = video_format(env, side)?;
    let video = codec(env, "video/avc", &video_format)?;
    let audio_format = audio_format(env)?;
    let audio = codec(env, "audio/mp4a-latm", &audio_format)?;

    let name = env.new_string(path)?;
    let muxer = env.new_object(
        "android/media/MediaMuxer",
        "(Ljava/lang/String;I)V",
        &[JValue::Object(&name), JValue::Int(MPEG_4)],
    )?;
    let info = env.new_object("android/media/MediaCodec$BufferInfo", "()V", &[])?;
    Ok((
        env.new_global_ref(video)?,
        env.new_global_ref(audio)?,
        env.new_global_ref(muxer)?,
        env.new_global_ref(info)?,
    ))
}

/// H.264, square, at the clients' bitrate with a keyframe a second.
fn video_format<'local>(env: &mut JNIEnv<'local>, side: i32) -> Faulted<JObject<'local>> {
    let mime = env.new_string("video/avc")?;
    let format = env
        .call_static_method(
            "android/media/MediaFormat",
            "createVideoFormat",
            "(Ljava/lang/String;II)Landroid/media/MediaFormat;",
            &[JValue::Object(&mime), JValue::Int(side), JValue::Int(side)],
        )?
        .l()?;
    set(env, &format, "color-format", COLOR_YUV420_FLEXIBLE)?;
    set(env, &format, "bitrate", VIDEO_BITRATE as i32)?;
    set(env, &format, "frame-rate", FPS as i32)?;
    set(env, &format, "i-frame-interval", KEYFRAME_SECONDS)?;
    Ok(format)
}

/// AAC, mono, at the rate everything is resampled to.
fn audio_format<'local>(env: &mut JNIEnv<'local>) -> Faulted<JObject<'local>> {
    let mime = env.new_string("audio/mp4a-latm")?;
    let format = env
        .call_static_method(
            "android/media/MediaFormat",
            "createAudioFormat",
            "(Ljava/lang/String;II)Landroid/media/MediaFormat;",
            &[
                JValue::Object(&mime),
                JValue::Int(kernel::codec::pcm::RATE as i32),
                JValue::Int(1),
            ],
        )?
        .l()?;
    set(env, &format, "aac-profile", AAC_LC)?;
    set(env, &format, "bitrate", AUDIO_BITRATE as i32)?;
    Ok(format)
}

/// One encoder for a type, configured and started.
fn codec<'local>(
    env: &mut JNIEnv<'local>,
    mime: &str,
    format: &JObject<'local>,
) -> Faulted<JObject<'local>> {
    let name = env.new_string(mime)?;
    let codec = env
        .call_static_method(
            "android/media/MediaCodec",
            "createEncoderByType",
            "(Ljava/lang/String;)Landroid/media/MediaCodec;",
            &[JValue::Object(&name)],
        )?
        .l()?;
    let nothing = JObject::null();
    env.call_method(
        &codec,
        "configure",
        "(Landroid/media/MediaFormat;Landroid/view/Surface;Landroid/media/MediaCrypto;I)V",
        &[
            JValue::Object(format),
            JValue::Object(&nothing),
            JValue::Object(&nothing),
            JValue::Int(ENCODE),
        ],
    )?;
    env.call_method(&codec, "start", "()V", &[])?;
    Ok(codec)
}

/// `MediaFormat.setInteger`.
fn set(env: &mut JNIEnv<'_>, format: &JObject<'_>, key: &str, value: i32) -> Faulted<()> {
    let key = env.new_string(key)?;
    env.call_method(
        format,
        "setInteger",
        "(Ljava/lang/String;I)V",
        &[JValue::Object(&key), JValue::Int(value)],
    )?;
    Ok(())
}

/// One NV12 frame into the video encoder, written plane by plane against
/// the strides the codec asked for. Answers whether a buffer was free.
fn feed_picture(
    env: &mut JNIEnv<'_>,
    codec: &GlobalRef,
    nv12: &[u8],
    side: usize,
    pts_us: i64,
) -> Faulted<bool> {
    let index = env
        .call_method(codec, "dequeueInputBuffer", "(J)I", &[JValue::Long(WAIT_US)])?
        .i()?;
    if index < 0 {
        return Ok(false);
    }
    let image = env
        .call_method(
            codec,
            "getInputImage",
            "(I)Landroid/media/Image;",
            &[JValue::Int(index)],
        )?
        .l()?;
    if image.is_null() {
        // Nothing to fill, and a queued empty buffer is better than a
        // buffer never given back.
        env.call_method(
            codec,
            "queueInputBuffer",
            "(IIIJI)V",
            &[
                JValue::Int(index),
                JValue::Int(0),
                JValue::Int(0),
                JValue::Long(pts_us),
                JValue::Int(0),
            ],
        )?;
        return Ok(false);
    }
    let planes: JObjectArray<'_> = env
        .call_method(&image, "getPlanes", "()[Landroid/media/Image$Plane;", &[])?
        .l()?
        .into();
    let half = side / 2;
    let chroma = side * side;
    for p in 0..3i32 {
        let plane = env.get_object_array_element(&planes, p)?;
        let row = env.call_method(&plane, "getRowStride", "()I", &[])?.i()? as usize;
        let pixel = env.call_method(&plane, "getPixelStride", "()I", &[])?.i()? as usize;
        let buffer: JByteBuffer<'_> = env
            .call_method(&plane, "getBuffer", "()Ljava/nio/ByteBuffer;", &[])?
            .l()?
            .into();
        let address = env.get_direct_buffer_address(&buffer)?;
        let capacity = env.get_direct_buffer_capacity(&buffer)?;
        if address.is_null() || capacity == 0 {
            continue;
        }
        // SAFETY: a direct buffer the codec owns, as long as this frame,
        // and written only inside the size it reports.
        let into = unsafe { std::slice::from_raw_parts_mut(address, capacity) };
        if p == 0 {
            for y in 0..side {
                for x in 0..side {
                    let at = y * row + x * pixel.max(1);
                    if let (Some(dst), Some(src)) = (into.get_mut(at), nv12.get(y * side + x)) {
                        *dst = *src;
                    }
                }
            }
        } else {
            // The interleaved half of NV12, split into whichever plane the
            // codec calls U and V — with a pixel stride of two these two
            // writes land in the one buffer, interleaved, which is what a
            // semi-planar encoder wants.
            let offset = usize::from(p == 2);
            for y in 0..half {
                for x in 0..half {
                    let at = y * row + x * pixel.max(1);
                    let from = chroma + (y * half + x) * 2 + offset;
                    if let (Some(dst), Some(src)) = (into.get_mut(at), nv12.get(from)) {
                        *dst = *src;
                    }
                }
            }
        }
    }
    env.call_method(
        codec,
        "queueInputBuffer",
        "(IIIJI)V",
        &[
            JValue::Int(index),
            JValue::Int(0),
            JValue::Int((side * side * 3 / 2) as i32),
            JValue::Long(pts_us),
            JValue::Int(0),
        ],
    )?;
    Ok(true)
}

/// As much of a PCM buffer as one input buffer takes. Answers how many
/// samples went in; zero means no buffer was free.
fn feed_sound(
    env: &mut JNIEnv<'_>,
    codec: &GlobalRef,
    pcm: &[i16],
    pts_us: i64,
) -> Faulted<usize> {
    let index = env
        .call_method(codec, "dequeueInputBuffer", "(J)I", &[JValue::Long(WAIT_US)])?
        .i()?;
    if index < 0 {
        return Ok(0);
    }
    let buffer: JByteBuffer<'_> = env
        .call_method(
            codec,
            "getInputBuffer",
            "(I)Ljava/nio/ByteBuffer;",
            &[JValue::Int(index)],
        )?
        .l()?
        .into();
    let address = env.get_direct_buffer_address(&buffer)?;
    let capacity = env.get_direct_buffer_capacity(&buffer)?;
    let taken = if address.is_null() {
        0
    } else {
        let taken = pcm.len().min(capacity / 2);
        // SAFETY: a direct buffer the codec owns, written inside the size
        // it reports.
        let into = unsafe { std::slice::from_raw_parts_mut(address, capacity) };
        for (n, sample) in pcm[..taken].iter().enumerate() {
            into[n * 2..n * 2 + 2].copy_from_slice(&sample.to_le_bytes());
        }
        taken
    };
    env.call_method(
        codec,
        "queueInputBuffer",
        "(IIIJI)V",
        &[
            JValue::Int(index),
            JValue::Int(0),
            JValue::Int((taken * 2) as i32),
            JValue::Long(pts_us),
            JValue::Int(0),
        ],
    )?;
    Ok(taken)
}

/// An empty buffer with the end-of-stream flag: what tells an encoder that
/// there is nothing more coming.
fn end_of_stream(env: &mut JNIEnv<'_>, codec: &GlobalRef, pts_us: i64) -> Faulted<()> {
    let index = env
        .call_method(codec, "dequeueInputBuffer", "(J)I", &[JValue::Long(WAIT_US)])?
        .i()?;
    if index < 0 {
        return Ok(());
    }
    env.call_method(
        codec,
        "queueInputBuffer",
        "(IIIJI)V",
        &[
            JValue::Int(index),
            JValue::Int(0),
            JValue::Int(0),
            JValue::Long(pts_us),
            JValue::Int(END_OF_STREAM),
        ],
    )?;
    Ok(())
}

/// Everything one encoder has ready: the track its format earned, the
/// samples it made, and whether it has reached the end of its stream.
fn pump(
    env: &mut JNIEnv<'_>,
    codec: &GlobalRef,
    info: &GlobalRef,
    muxer: &GlobalRef,
    video: bool,
) -> Faulted<(Option<i32>, Vec<Sample>, bool)> {
    let mut track = None;
    let mut samples = Vec::new();
    let mut ended = false;
    loop {
        let index = env
            .call_method(
                codec,
                "dequeueOutputBuffer",
                "(Landroid/media/MediaCodec$BufferInfo;J)I",
                &[JValue::Object(info.as_obj()), JValue::Long(0)],
            )?
            .i()?;
        if index == FORMAT_CHANGED {
            let format = env
                .call_method(codec, "getOutputFormat", "()Landroid/media/MediaFormat;", &[])?
                .l()?;
            track = Some(
                env.call_method(
                    muxer,
                    "addTrack",
                    "(Landroid/media/MediaFormat;)I",
                    &[JValue::Object(&format)],
                )?
                .i()?,
            );
            continue;
        }
        if index < 0 {
            break;
        }
        let flags = env.get_field(info, "flags", "I")?.i()?;
        let size = env.get_field(info, "size", "I")?.i()?;
        let offset = env.get_field(info, "offset", "I")?.i()?;
        let pts_us = env.get_field(info, "presentationTimeUs", "J")?.j()?;
        if flags & CODEC_CONFIG == 0 && size > 0 {
            let buffer: JByteBuffer<'_> = env
                .call_method(
                    codec,
                    "getOutputBuffer",
                    "(I)Ljava/nio/ByteBuffer;",
                    &[JValue::Int(index)],
                )?
                .l()?
                .into();
            let address = env.get_direct_buffer_address(&buffer)?;
            let capacity = env.get_direct_buffer_capacity(&buffer)?;
            let at = offset.max(0) as usize;
            let len = (size.max(0) as usize).min(capacity.saturating_sub(at));
            if !address.is_null() && len > 0 {
                // SAFETY: a direct buffer the codec owns until it is
                // released below, read inside the size it reports.
                let from = unsafe { std::slice::from_raw_parts(address, capacity) };
                samples.push(Sample {
                    video,
                    bytes: from[at..at + len].to_vec(),
                    pts_us,
                    flags,
                });
            }
        }
        if flags & END_OF_STREAM != 0 {
            ended = true;
        }
        env.call_method(
            codec,
            "releaseOutputBuffer",
            "(IZ)V",
            &[JValue::Int(index), JValue::Bool(0)],
        )?;
        if ended {
            break;
        }
    }
    Ok((track, samples, ended))
}

/// One encoded sample into the muxer, over a buffer of our own: the codec's
/// was released the moment it was read, and a sample may have been waiting
/// here for the other track's format.
fn write_sample(
    env: &mut JNIEnv<'_>,
    muxer: &GlobalRef,
    track: i32,
    sample: &Sample,
) -> Faulted<()> {
    if track < 0 || sample.bytes.is_empty() {
        return Ok(());
    }
    // SAFETY: the buffer is read by `writeSampleData` inside this call and
    // the bytes outlive it — they are owned by the sample, which is alive
    // for the length of this function.
    let buffer = unsafe {
        env.new_direct_byte_buffer(sample.bytes.as_ptr().cast_mut(), sample.bytes.len())?
    };
    let info = env.new_object("android/media/MediaCodec$BufferInfo", "()V", &[])?;
    env.call_method(
        &info,
        "set",
        "(IIJI)V",
        &[
            JValue::Int(0),
            JValue::Int(sample.bytes.len() as i32),
            JValue::Long(sample.pts_us),
            JValue::Int(sample.flags & !CODEC_CONFIG & !END_OF_STREAM),
        ],
    )?;
    env.call_method(
        muxer,
        "writeSampleData",
        "(ILjava/nio/ByteBuffer;Landroid/media/MediaCodec$BufferInfo;)V",
        &[
            JValue::Int(track),
            JValue::Object(&buffer),
            JValue::Object(&info),
        ],
    )?;
    Ok(())
}

/// Stops and releases everything, in the order the framework wants: the
/// encoders first, then the muxer, which is what finalizes the container.
fn close(
    env: &mut JNIEnv<'_>,
    video: &GlobalRef,
    audio: &GlobalRef,
    muxer: &GlobalRef,
    running: bool,
) -> Faulted<()> {
    for codec in [video, audio] {
        let _ = env.call_method(codec, "stop", "()V", &[]);
        let _ = env.exception_clear();
        let _ = env.call_method(codec, "release", "()V", &[]);
        let _ = env.exception_clear();
    }
    if running {
        env.call_method(muxer, "stop", "()V", &[])?;
    }
    env.call_method(muxer, "release", "()V", &[])?;
    if !running {
        return Err(Fault(
            "the recording ended before either track had a format".to_string(),
        ));
    }
    Ok(())
}
