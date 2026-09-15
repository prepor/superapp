//! The senses this machine has: the kernel's [`Location`] and [`Capture`]
//! over the platform's own receiver, camera and microphone.
//!
//! Makepad reaches all three through `Cx` and answers as events, and a
//! capability is called from a verb or from a worker's thread, where there
//! is no `Cx` and no event loop. So a capability here does not *do*
//! anything: it writes down a **wish**, and the stage serves the wishes on
//! its next event, in [`Senses::service`], before the hosted panels see it.
//! A fix, a device list, a permission's answer and a camera frame all land
//! back in the same state, which is what the capability reads.
//!
//! One state for the whole run, behind one lock, because the device has one
//! of each: the place panel and the worker that moves a live share ask for
//! the receiver separately and the last of them to let go is what turns it
//! off, and the attach panel's preview and its recording are one camera
//! session, not two.
//!
//! What happens off the UI thread:
//!
//! - The **camera callback** runs on the capture thread. It keeps the newest
//!   frame here as owned I420 — what a photograph is written from — and,
//!   while a video message records, crops it to its centre square, scales it
//!   to 384 and hands it to the recorder.
//! - The **microphone callback** runs on the audio thread. It smooths a
//!   level for the meter and hands the samples to whichever recorder is
//!   running.
//! - Each **recorder** is a thread of its own with a channel in front of it,
//!   so neither callback ever waits on an encoder. A voice note goes through
//!   [`opus_ogg`](kernel::codec::opus_ogg); a video message through
//!   [`circle`], which is AVAssetWriter on a Mac and `MediaCodec` with
//!   `MediaMuxer` on the phone.
//!
//! Only a real run that nobody scripts gets any of this (`shell::boot`),
//! beside the real voice and the real clipboard: a suite may not open the
//! camera of whoever is at the machine, and every scripted run, every test
//! and every library mount reads the kernel's fakes instead — which write
//! real files, so the panels above are the same panels.

use std::path::{Path, PathBuf};
use std::sync::mpsc::{sync_channel, Receiver, SyncSender};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;

use kernel::caps::senses::{
    CameraFrame, CameraId, Capture, Fix, FrameTap, Location, Photo, VideoNote, VoiceNote,
    CIRCLE_MAX, CIRCLE_SIDE,
};
use kernel::codec::{jpeg, opus_ogg, pcm};
// Everything else — `Cx`, the events, the audio and video types, the media
// api's own trait — arrives with the glob, as it does everywhere in the
// shell; the permissions are the one module it does not re-export.
use makepad_widgets::makepad_platform::permission::{Permission, PermissionStatus};
use makepad_widgets::*;

pub mod circle;

/// How many frames a second a video message is written at. The clients'.
const FPS: u64 = 30;

/// The most frames a circle keeps: the minute, at that rate.
const CIRCLE_FRAMES: u64 = CIRCLE_MAX as u64 * FPS;

/// How far behind a recorder may fall before a frame is dropped rather than
/// waited for. A capture thread that blocks is a camera that stutters.
const BACKLOG: usize = 8;

/// How fast the meter follows the microphone: the fraction of a new reading
/// that counts. Low enough that a syllable does not make it jump, high
/// enough that a pause shows.
const METER_FOLLOW: f32 = 0.25;

// -- the shared state ----------------------------------------------------------

/// The senses of one run: every wish, everything the platform has answered,
/// and whatever is being recorded.
///
/// Cloned into every world's capabilities and into the callbacks; the stage
/// holds one to serve it.
#[derive(Clone, Default)]
pub struct Senses(Arc<Mutex<State>>);

/// What [`Senses`] keeps.
#[derive(Default)]
struct State {
    // -- where the device is
    /// How many callers are holding the receiver on.
    receiver_held: usize,
    /// Whether `start_location_updates` has been called for them.
    receiving: bool,
    fix: Option<Fix>,
    location_trouble: Option<String>,

    // -- the camera
    camera_wanted: bool,
    /// What the platform offers, once it has said. Empty until then.
    cameras: Vec<Choice>,
    /// The camera the session was started on, which is what a widget shows.
    camera: Option<CameraId>,
    /// Whether the frame callback has been registered — once per run.
    watching: bool,
    camera_trouble: Option<String>,
    /// The newest frame, as I420. What a photograph is written from.
    frame: Option<Frame>,
    /// Whoever else wants each frame as it arrives: a video call, which has
    /// to send the picture rather than draw it. `None` the rest of the time,
    /// which is nearly always.
    tap: Option<FrameTap>,

    // -- the microphone
    microphone_wanted: bool,
    /// The default input, once the platform has said.
    inputs: Vec<AudioDeviceId>,
    /// The default output, once it has said that too. Nothing here wishes
    /// for it — the shell's [mixer](crate::shell::sound) does, and the
    /// stage hands it over — but the platform names inputs and outputs in
    /// one event, so this is where it lands.
    outputs: Vec<AudioDeviceId>,
    /// Whether the session is running on them.
    hearing: bool,
    /// Whether the sample callback has been registered — once per run.
    listening: bool,
    microphone_trouble: Option<String>,
    level: f32,

    // -- permissions, asked once per kind per run
    asked_location: bool,
    asked_camera: bool,
    asked_microphone: bool,

    // -- what is being recorded
    voice: Option<Run<VoiceNote>>,
    circle: Option<Run<VideoNote>>,
}

/// One camera the platform offers, and the format picked on it.
#[derive(Clone, Copy)]
struct Choice {
    input: VideoInputId,
    format: VideoFormatId,
    /// Whether it faces the person. The front one is what a video message
    /// is recorded with.
    front: bool,
}

/// The newest camera frame, kept as I420 with tightly packed planes.
#[derive(Default)]
struct Frame {
    width: usize,
    height: usize,
    y: Vec<u8>,
    u: Vec<u8>,
    v: Vec<u8>,
}

/// A recording under way: the channel its samples and frames go down, the
/// thread that encodes them, and the file it is writing — kept here because
/// a discarded recording has to be taken off the disk whether or not the
/// encoder got as far as answering one.
struct Run<T> {
    pieces: SyncSender<Piece>,
    thread: JoinHandle<Result<T, String>>,
    path: PathBuf,
}

/// What a recorder's thread is handed.
enum Piece {
    /// The microphone, at the rate it reported.
    Sound(f64, Vec<f32>),
    /// One square NV12 frame of a video message.
    Square(Vec<u8>),
}

impl Senses {
    #[must_use]
    pub fn new() -> Senses {
        Senses::default()
    }

    /// The capabilities of one world: two handles on this one state, as the
    /// kernel's [`SenseSource`](kernel::caps::SenseSource) hands them out.
    #[must_use]
    pub fn capabilities(&self) -> (Box<dyn Location>, Box<dyn Capture>) {
        (
            Box::new(RealLocation(self.clone())),
            Box::new(RealCapture(self.clone())),
        )
    }

    /// The speakers the platform named, for whoever plays through them.
    /// Empty until it has said, which it does not until something has asked
    /// for audio at all.
    #[must_use]
    pub fn outputs(&self) -> Vec<AudioDeviceId> {
        self.0.lock().map(|s| s.outputs.clone()).unwrap_or_default()
    }

    /// One event, and whatever the wishes now ask of the platform.
    ///
    /// Answers whether anything a panel reads has changed — a fix, a device
    /// list, a refusal — so the stage can redraw. (The change was sketched
    /// without an answer; a panel that showed *finding you…* forever until
    /// something else happened to ask for a frame is why there is one.)
    pub fn service(&mut self, cx: &mut Cx, event: &Event) -> bool {
        let moved = self.land(event);
        let work = self.work();
        self.perform(cx, work);
        moved
    }

    /// Whatever the platform is saying, written down.
    fn land(&self, event: &Event) -> bool {
        let Ok(mut s) = self.0.lock() else { return false };
        match event {
            Event::LocationUpdate(u) => {
                s.fix = Some(Fix {
                    lat: u.lat,
                    lon: u.lon,
                    accuracy_m: u.accuracy_m,
                    // Only while the device is moving: a course read
                    // standing still is the last direction walked, and a
                    // live share would draw an arrow at nothing.
                    heading_deg: u
                        .heading_deg
                        .filter(|_| u.speed_mps.is_some_and(|v| v > 0.5)),
                    at: u.time,
                });
                s.location_trouble = None;
                true
            }
            Event::LocationError(e) => {
                s.location_trouble = Some(match e {
                    LocationErrorEvent::PermissionDenied => refused("your location"),
                    LocationErrorEvent::Unavailable(why) => {
                        format!("this device cannot say where it is — {why}")
                    }
                });
                true
            }
            Event::PermissionResult(r) => {
                let refusal = match r.status {
                    PermissionStatus::Granted | PermissionStatus::NotDetermined => None,
                    // Both refusals read the same: the platform stops
                    // asking after the second one either way, and what is
                    // left is the settings panel.
                    PermissionStatus::DeniedCanRetry | PermissionStatus::DeniedPermanent => {
                        Some(())
                    }
                };
                match r.permission {
                    Permission::Location => {
                        s.location_trouble = refusal.map(|()| refused("your location"));
                    }
                    Permission::Camera => {
                        s.camera_trouble = refusal.map(|()| refused("the camera"));
                        if refusal.is_some() {
                            s.camera_wanted = false;
                        }
                    }
                    Permission::AudioInput => {
                        s.microphone_trouble = refusal.map(|()| refused("the microphone"));
                        if refusal.is_some() {
                            s.microphone_wanted = false;
                        }
                    }
                    _ => return false,
                }
                true
            }
            Event::VideoInputs(inputs) => {
                s.cameras = choices(inputs);
                if s.cameras.is_empty() {
                    s.camera_trouble = Some("this device has no camera".to_string());
                }
                true
            }
            Event::AudioDevices(devices) => {
                s.inputs = devices.default_input();
                s.outputs = devices.default_output();
                if s.inputs.is_empty() {
                    s.microphone_trouble = Some("this device has no microphone".to_string());
                }
                true
            }
            _ => false,
        }
    }

    /// What the wishes now ask of the platform, marked as asked for so that
    /// the next event does not ask again.
    fn work(&self) -> Work {
        let mut work = Work::default();
        let Ok(mut s) = self.0.lock() else {
            return work;
        };

        // The receiver. The permission is asked for once; the platform's
        // own dialog is what the person sees, and a refusal arrives as an
        // event rather than as this call's answer.
        if s.receiver_held > 0 && !s.receiving {
            if !s.asked_location {
                s.asked_location = true;
                work.ask.push(Permission::Location);
            }
            s.receiving = true;
            work.start_location = true;
        } else if s.receiver_held == 0 && s.receiving {
            s.receiving = false;
            work.stop_location = true;
        }

        // The camera. Watching wakes the platform's enumeration, which is
        // what fills `cameras`; the session starts once it has.
        if s.camera_wanted {
            if !s.watching {
                s.watching = true;
                work.watch = true;
            }
            if !s.asked_camera {
                s.asked_camera = true;
                work.ask.push(Permission::Camera);
            }
            if s.camera.is_none() {
                if let Some(choice) = pick(&s.cameras) {
                    s.camera = Some(CameraId {
                        input: choice.input.0 .0,
                        format: choice.format.0 .0,
                    });
                    work.open_camera = Some((choice.input, choice.format));
                }
            }
        } else if s.camera.is_some() {
            s.camera = None;
            s.frame = None;
            work.close_camera = true;
        }

        // The microphone, the same shape.
        if s.microphone_wanted {
            if !s.listening {
                s.listening = true;
                work.listen = true;
            }
            if !s.asked_microphone {
                s.asked_microphone = true;
                work.ask.push(Permission::AudioInput);
            }
            if !s.hearing && !s.inputs.is_empty() {
                s.hearing = true;
                work.open_microphone = s.inputs.clone();
            }
        } else if s.hearing {
            s.hearing = false;
            s.level = 0.0;
            work.close_microphone = true;
        }
        work
    }

    /// The `cx` calls the wishes came to, made with no lock held: a
    /// platform that answers a registration by calling straight back into a
    /// callback would otherwise meet its own lock.
    fn perform(&self, cx: &mut Cx, work: Work) {
        for permission in work.ask {
            cx.request_permission(permission);
        }
        if work.watch {
            let senses = self.clone();
            let mut scratch = CameraFrameOwned::default();
            cx.camera_frame_input(0, move |frame| senses.saw(&mut scratch, frame));
        }
        if work.listen {
            let senses = self.clone();
            cx.audio_input(0, move |info, buffer| senses.heard(info, buffer));
        }
        if let Some((input, format)) = work.open_camera {
            cx.use_video_input(&[(input, format)]);
        }
        if work.close_camera {
            cx.use_video_input(&[]);
        }
        if !work.open_microphone.is_empty() {
            cx.use_audio_inputs(&work.open_microphone);
        }
        if work.close_microphone {
            cx.use_audio_inputs(&[]);
        }
        if work.start_location {
            cx.start_location_updates();
        }
        if work.stop_location {
            cx.stop_location_updates();
        }
    }

    /// One camera frame, on the capture thread.
    fn saw(&self, scratch: &mut CameraFrameOwned, frame: CameraFrameRef<'_>) {
        if !scratch.convert_to_i420(frame) {
            return;
        }
        let (recording, tap) = match self.0.lock() {
            Ok(s) => (s.circle.is_some(), s.tap.clone()),
            Err(_) => return,
        };
        // The crop and the scale are the one expensive thing on this
        // thread, and they happen with no lock held: a draw asking for the
        // level must not wait behind a frame.
        let square = recording.then(|| square_nv12(scratch, CIRCLE_SIDE as usize));
        {
            let Ok(mut s) = self.0.lock() else { return };
            if let (Some(square), Some(run)) = (square, s.circle.as_ref()) {
                offer(&run.pieces, Piece::Square(square));
            }
            let kept = s.frame.get_or_insert_with(Frame::default);
            kept.width = scratch.width;
            kept.height = scratch.height;
            for (into, plane) in [
                (&mut kept.y, &scratch.planes[0]),
                (&mut kept.u, &scratch.planes[1]),
                (&mut kept.v, &scratch.planes[2]),
            ] {
                into.clear();
                into.extend_from_slice(&plane.bytes);
            }
        }
        // And the call's, with the lock let go: whoever took the tap does
        // its own queueing, and a frame it is too slow for is its to drop.
        if let Some(tap) = tap {
            tap(CameraFrame {
                width: scratch.width,
                height: scratch.height,
                y: &scratch.planes[0].bytes,
                u: &scratch.planes[1].bytes,
                v: &scratch.planes[2].bytes,
            });
        }
    }

    /// One buffer of the microphone, on the audio thread.
    fn heard(&self, info: AudioInfo, buffer: &AudioBuffer) {
        if buffer.frame_count() == 0 || buffer.channel_count() == 0 {
            return;
        }
        // One channel: a voice note is mono, and the meter is one bar.
        let samples = buffer.channel(0);
        let peak = samples.iter().fold(0.0f32, |top, s| top.max(s.abs()));
        let Ok(mut s) = self.0.lock() else { return };
        s.level += (peak.clamp(0.0, 1.0) - s.level) * METER_FOLLOW;
        for run in [
            s.voice.as_ref().map(|r| &r.pieces),
            s.circle.as_ref().map(|r| &r.pieces),
        ]
        .into_iter()
        .flatten()
        {
            offer(run, Piece::Sound(info.sample_rate, samples.to_vec()));
        }
    }
}

/// What one round of [`Senses::service`] asks of `Cx`.
#[derive(Default)]
struct Work {
    ask: Vec<Permission>,
    start_location: bool,
    stop_location: bool,
    watch: bool,
    listen: bool,
    open_camera: Option<(VideoInputId, VideoFormatId)>,
    close_camera: bool,
    open_microphone: Vec<AudioDeviceId>,
    close_microphone: bool,
}

/// A refusal, in the words a panel says out loud. Where to go is named
/// because a platform that has stopped asking will not ask again.
fn refused(what: &str) -> String {
    let settings = if cfg!(target_os = "macos") {
        "System Settings › Privacy & Security"
    } else {
        "Settings › Apps › superapp › Permissions"
    };
    format!("{what} is not allowed — {settings}")
}

/// A piece handed to a recorder, or dropped where it is already behind: a
/// capture thread that waits is a camera that stutters, and one frame of a
/// video message is a thirtieth of a second. A recorder that has already
/// been joined is the same answer — the recording is over.
fn offer(pieces: &SyncSender<Piece>, piece: Piece) {
    let _ = pieces.try_send(piece);
}

// -- the two capabilities ------------------------------------------------------

/// Where the device is, over the platform's own receiver.
pub struct RealLocation(Senses);

impl Location for RealLocation {
    fn want(&mut self) -> Result<(), String> {
        let mut s = self
            .0
             .0
            .lock()
            .map_err(|_| "the receiver is poisoned".to_string())?;
        if let Some(trouble) = &s.location_trouble {
            return Err(trouble.clone());
        }
        s.receiver_held += 1;
        Ok(())
    }

    fn release(&mut self) {
        if let Ok(mut s) = self.0 .0.lock() {
            s.receiver_held = s.receiver_held.saturating_sub(1);
        }
    }

    fn fix(&self) -> Option<Fix> {
        self.0 .0.lock().ok()?.fix
    }
}

/// The camera and the microphone, over the platform's own.
pub struct RealCapture(Senses);

impl RealCapture {
    fn state(&self) -> Result<std::sync::MutexGuard<'_, State>, String> {
        self.0
             .0
            .lock()
            .map_err(|_| "the camera is poisoned".to_string())
    }
}

/// A recorder on a thread of its own, with a short channel in front of it:
/// neither callback ever waits on an encoder.
fn recorder<T: Send + 'static>(
    path: PathBuf,
    encode: impl FnOnce(&Path, &Receiver<Piece>) -> Result<T, String> + Send + 'static,
) -> Run<T> {
    let (pieces, taken) = sync_channel(BACKLOG);
    let writing = path.clone();
    Run {
        pieces,
        thread: std::thread::spawn(move || encode(&writing, &taken)),
        path,
    }
}

impl Capture for RealCapture {
    fn open_camera(&mut self) -> Result<(), String> {
        let mut s = self.state()?;
        if let Some(trouble) = &s.camera_trouble {
            return Err(trouble.clone());
        }
        s.camera_wanted = true;
        Ok(())
    }

    fn camera(&self) -> Option<CameraId> {
        self.0 .0.lock().ok()?.camera
    }

    fn close_camera(&mut self) {
        if let Ok(mut s) = self.0 .0.lock() {
            s.camera_wanted = false;
        }
    }

    fn take_photo(&mut self, dir: &Path) -> Result<Photo, String> {
        let s = self.state()?;
        let frame = s.frame.as_ref().ok_or_else(|| {
            if s.camera_wanted {
                "the camera has not sent a picture yet".to_string()
            } else {
                "the camera is not open".to_string()
            }
        })?;
        let (width, height) = (frame.width, frame.height);
        let rgb = rgb_of(frame);
        // Nothing below needs the lock, and a photograph is milliseconds of
        // scaling and encoding that a camera frame should not wait behind.
        drop(s);
        let bytes = jpeg::of_rgb(&rgb, width, height, jpeg::PHOTO_MAX)?;
        let path = named(dir, "photo", "jpg")?;
        std::fs::write(&path, &bytes).map_err(|e| format!("{}: {e}", path.display()))?;
        let (width, height) = jpeg::fitted(width, height, jpeg::PHOTO_MAX);
        Ok(Photo {
            path,
            width: width as u32,
            height: height as u32,
        })
    }

    fn start_voice(&mut self, dir: &Path) -> Result<(), String> {
        let mut s = self.state()?;
        if let Some(trouble) = &s.microphone_trouble {
            return Err(trouble.clone());
        }
        if s.voice.is_some() || s.circle.is_some() {
            return Err("something is already being recorded".to_string());
        }
        s.voice = Some(recorder(named(dir, "voice", "ogg")?, write_voice));
        s.microphone_wanted = true;
        Ok(())
    }

    fn stop_voice(&mut self) -> Result<VoiceNote, String> {
        let mut s = self.state()?;
        let run = s.voice.take().ok_or("nothing is being recorded")?;
        s.microphone_wanted = s.circle.is_some();
        drop(s);
        let note = join(run)?;
        if note.secs < opus_ogg::LEAST {
            let _ = std::fs::remove_file(&note.path);
            return Err("that was too short to send — hold it a little longer".to_string());
        }
        Ok(note)
    }

    fn start_circle(&mut self, dir: &Path) -> Result<(), String> {
        let mut s = self.state()?;
        if s.camera.is_none() {
            return Err(s
                .camera_trouble
                .clone()
                .unwrap_or_else(|| "the camera is not open yet".to_string()));
        }
        if let Some(trouble) = &s.microphone_trouble {
            return Err(trouble.clone());
        }
        if s.voice.is_some() || s.circle.is_some() {
            return Err("something is already being recorded".to_string());
        }
        s.circle = Some(recorder(named(dir, "circle", "mp4")?, write_circle));
        s.microphone_wanted = true;
        Ok(())
    }

    fn stop_circle(&mut self) -> Result<VideoNote, String> {
        let mut s = self.state()?;
        let run = s.circle.take().ok_or("nothing is being recorded")?;
        s.microphone_wanted = s.voice.is_some();
        drop(s);
        join(run)
    }

    /// Throws away whatever is running, and the file with it: a recording
    /// the panel closed on is a recording nobody asked to keep.
    fn discard(&mut self) {
        let (voice, circle) = match self.0 .0.lock() {
            Ok(mut s) => {
                s.microphone_wanted = false;
                s.level = 0.0;
                (s.voice.take(), s.circle.take())
            }
            Err(_) => return,
        };
        for path in [voice.map(forget), circle.map(forget)].into_iter().flatten() {
            let _ = std::fs::remove_file(&path);
            let _ = std::fs::remove_file(path.with_extension("jpg"));
        }
    }

    fn level(&self) -> f32 {
        self.0 .0.lock().map_or(0.0, |s| s.level)
    }

    /// The tap a video call leaves on the frames. One at a time, because one
    /// machine carries one call; a second overwrites the first, and `None`
    /// is what the end of the call puts back.
    fn watch_frames(&mut self, watch: Option<FrameTap>) {
        if let Ok(mut s) = self.0 .0.lock() {
            s.tap = watch;
        }
    }
}

/// Ends a recording: the channel closes, which is what tells the thread the
/// recording is over, and then the file is whatever it made of it.
fn join<T>(run: Run<T>) -> Result<T, String> {
    drop(run.pieces);
    match run.thread.join() {
        Ok(made) => made,
        Err(_) => Err("the recorder stopped".to_string()),
    }
}

/// Ends a recording nobody wants and answers the file it was writing, so it
/// can be taken away — whatever the encoder made of it, or failed to.
fn forget<T>(run: Run<T>) -> PathBuf {
    let path = run.path.clone();
    let _ = join(run);
    path
}

/// A capture's file, named by the clock under `dir`, which is the app's
/// `captures/`.
fn named(dir: &Path, what: &str, ext: &str) -> Result<PathBuf, String> {
    std::fs::create_dir_all(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    let at = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis())
        .unwrap_or(0);
    Ok(dir.join(format!("{what}-{at}.{ext}")))
}

// -- the recorders -------------------------------------------------------------

/// A voice note, on its own thread: samples in, Ogg Opus out.
fn write_voice(path: &Path, pieces: &Receiver<Piece>) -> Result<VoiceNote, String> {
    let mut voice: Option<opus_ogg::Voice> = None;
    while let Ok(piece) = pieces.recv() {
        let Piece::Sound(rate, samples) = piece else {
            continue;
        };
        let writer = match &mut voice {
            Some(writer) => writer,
            None => voice.insert(opus_ogg::Voice::create(path, rate)?),
        };
        writer.push(&samples)?;
    }
    let done = voice
        .ok_or("the microphone said nothing")?
        .finish()?;
    Ok(VoiceNote {
        path: done.path,
        secs: done.secs,
        waveform: done.waveform,
    })
}

/// A video message, on its own thread: square frames and samples in, a
/// square mp4 and its poster out.
fn write_circle(path: &Path, pieces: &Receiver<Piece>) -> Result<VideoNote, String> {
    let side = CIRCLE_SIDE;
    let mut encoder = circle::Encoder::create(path, side)?;
    let mut first: Option<Vec<u8>> = None;
    let mut frames: u64 = 0;
    let mut rate: Option<pcm::Resampler> = None;
    let mut pcm16: Vec<i16> = Vec::new();
    while let Ok(piece) = pieces.recv() {
        // Past the minute the recording is over: nothing more is written,
        // and the channel stays open so the panel's own stop is what ends it.
        if frames >= CIRCLE_FRAMES {
            continue;
        }
        match piece {
            Piece::Square(nv12) => {
                if first.is_none() {
                    first = Some(nv12.clone());
                }
                encoder.push_frame(&nv12)?;
                frames += 1;
            }
            Piece::Sound(from, samples) => {
                let resampler = match &mut rate {
                    Some(r) if r.is_from(from) => r,
                    _ => rate.insert(pcm::Resampler::from(from)?),
                };
                pcm16.clear();
                resampler.push(&samples, &mut pcm16);
                encoder.push_audio(&pcm16)?;
            }
        }
    }
    encoder.finish()?;
    let thumbnail = path.with_extension("jpg");
    let poster = first.ok_or("the camera sent no picture")?;
    let side = side as usize;
    let bytes = jpeg::of_rgb(&rgb_of_nv12(&poster, side), side, side, jpeg::THUMBNAIL)?;
    std::fs::write(&thumbnail, &bytes).map_err(|e| format!("{}: {e}", thumbnail.display()))?;
    Ok(VideoNote {
        path: path.to_path_buf(),
        secs: frames as f64 / FPS as f64,
        side: CIRCLE_SIDE,
        thumbnail,
    })
}

// -- pixels --------------------------------------------------------------------

/// Which cameras the platform offers, with a format picked on each: the
/// best YUV one under 720p, because a video message is 384 pixels square
/// and a photograph is capped at 1280 — asking a sensor for its largest
/// mode would cost frames nothing draws.
fn choices(inputs: &VideoInputsEvent) -> Vec<Choice> {
    let mut out = Vec::new();
    for desc in &inputs.descs {
        let mut best: Option<(usize, usize)> = None;
        let mut format = None;
        for f in &desc.formats {
            let rank = match f.pixel_format {
                VideoPixelFormat::NV12 => 3,
                VideoPixelFormat::YUV420 => 2,
                VideoPixelFormat::YUY2 => 1,
                _ => continue,
            };
            if f.width > 1920 || f.height > 1080 {
                continue;
            }
            let score = (rank, f.width * f.height);
            if best.is_none_or(|was| score > was) {
                best = Some(score);
                format = Some(f.format_id);
            }
        }
        if let Some(format) = format {
            let name = desc.name.to_lowercase();
            out.push(Choice {
                input: desc.input_id,
                format,
                front: name.contains("front") || name.contains("facetime"),
            });
        }
    }
    out
}

/// The camera to open: the front one where there is one, because a video
/// message is of the person holding the phone.
fn pick(cameras: &[Choice]) -> Option<Choice> {
    cameras
        .iter()
        .find(|c| c.front)
        .or_else(|| cameras.first())
        .copied()
}

/// An I420 frame as tightly packed RGB, for the JPEG encoder.
fn rgb_of(frame: &Frame) -> Vec<u8> {
    let (w, h) = (frame.width, frame.height);
    let (cw, ch) = (w.div_ceil(2), h.div_ceil(2));
    let mut rgb = vec![0u8; w * h * 3];
    for y in 0..h {
        for x in 0..w {
            let luma = frame.y.get(y * w + x).copied().unwrap_or(16);
            let c = (y / 2).min(ch.saturating_sub(1)) * cw + (x / 2).min(cw.saturating_sub(1));
            let (r, g, b) = yuv_to_rgb(
                luma,
                frame.u.get(c).copied().unwrap_or(128),
                frame.v.get(c).copied().unwrap_or(128),
            );
            let i = (y * w + x) * 3;
            rgb[i] = r;
            rgb[i + 1] = g;
            rgb[i + 2] = b;
        }
    }
    rgb
}

/// A square NV12 frame as RGB — what a video message's poster is drawn from.
fn rgb_of_nv12(nv12: &[u8], side: usize) -> Vec<u8> {
    let (cw, ch) = (side / 2, side / 2);
    let mut rgb = vec![0u8; side * side * 3];
    for y in 0..side {
        for x in 0..side {
            let luma = nv12.get(y * side + x).copied().unwrap_or(16);
            let c = side * side + ((y / 2).min(ch - 1) * cw + (x / 2).min(cw - 1)) * 2;
            let (r, g, b) = yuv_to_rgb(
                luma,
                nv12.get(c).copied().unwrap_or(128),
                nv12.get(c + 1).copied().unwrap_or(128),
            );
            let i = (y * side + x) * 3;
            rgb[i] = r;
            rgb[i + 1] = g;
            rgb[i + 2] = b;
        }
    }
    rgb
}

/// One pixel, BT.601 in the integer form every client uses.
fn yuv_to_rgb(y: u8, u: u8, v: u8) -> (u8, u8, u8) {
    let c = i32::from(y) - 16;
    let d = i32::from(u) - 128;
    let e = i32::from(v) - 128;
    let clip = |n: i32| n.clamp(0, 255) as u8;
    (
        clip((298 * c + 409 * e + 128) >> 8),
        clip((298 * c - 100 * d - 208 * e + 128) >> 8),
        clip((298 * c + 516 * d + 128) >> 8),
    )
}

/// An I420 frame cropped to its centre square and scaled to `side`, as
/// NV12 — what both encoders are pushed.
///
/// Nearest neighbour, on the capture thread, thirty times a second: a
/// sensor's square is being made a third of its size, and an average over
/// nine pixels would cost more than the difference is worth at 384.
fn square_nv12(frame: &CameraFrameOwned, side: usize) -> Vec<u8> {
    let (w, h) = (frame.width, frame.height);
    let mut out = vec![0u8; side * side * 3 / 2];
    if w == 0 || h == 0 || frame.layout != CameraFrameLayout::I420 {
        // Mid grey rather than a black frame: a picture nobody can decode
        // is worse than an even one.
        out[..side * side].fill(128);
        out[side * side..].fill(128);
        return out;
    }
    let crop = w.min(h);
    let (left, top) = ((w - crop) / 2, (h - crop) / 2);
    let (cw, ch) = (w.div_ceil(2), h.div_ceil(2));
    for y in 0..side {
        let sy = top + y * crop / side;
        for x in 0..side {
            let sx = left + x * crop / side;
            out[y * side + x] = frame.planes[0]
                .bytes
                .get(sy * w + sx)
                .copied()
                .unwrap_or(16);
        }
    }
    let half = side / 2;
    for y in 0..half {
        let sy = ((top + y * 2 * crop / side) / 2).min(ch.saturating_sub(1));
        for x in 0..half {
            let sx = ((left + x * 2 * crop / side) / 2).min(cw.saturating_sub(1));
            let i = side * side + (y * half + x) * 2;
            out[i] = frame.planes[1].bytes.get(sy * cw + sx).copied().unwrap_or(128);
            out[i + 1] = frame.planes[2].bytes.get(sy * cw + sx).copied().unwrap_or(128);
        }
    }
    out
}

#[cfg(test)]
mod tests;
