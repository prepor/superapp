//! The senses: where the device is, what its camera sees, what its
//! microphone hears.
//!
//! Two capabilities, because they are asked for in two different moods. A
//! [`Location`] is *turned on and read*: a panel wants the fix, a worker
//! moving a live share wants the newest one, and neither cares when it
//! arrived. A [`Capture`] is *started and stopped*: it opens a device, runs
//! for as long as a finger is down, and answers a file.
//!
//! Both are the kernel's rather than an app's for the reason the voice is:
//! the platform is what answers — CoreLocation and AVFoundation on a Mac,
//! `LocationManager` and `MediaCodec` on the phone — and the shell is the
//! one layer that may name a platform. What an app sees is a trait, and
//! what a scripted run sees is the fake beside it.
//!
//! The fakes are not stubs. [`FakeCapture`] writes *real files*: a real
//! JPEG, a real Ogg Opus note with the waveform computed by the very rule a
//! recording uses, a real mp4 copied out of the demo tree. A suite that
//! sends a voice note therefore sends what a person would, and a fixture
//! line drawn from a fake capture is a line drawn from a real one. The one
//! thing it does not answer is a [`CameraId`]: there is no device behind it
//! for a `Video` widget to open, and a made-up id would raise the platform's
//! own camera dialog on a windowed run that is nobody's. Its camera is
//! *open* and has no picture, which is what a suite sees anyway.
//!
//! Both are handed to every world of a run through [`SenseSource`] on the
//! [`Env`](crate::app::Env), the way the disk and the blob cache are: a
//! worker moving a live share and the window drawing the map have to read
//! one receiver, not two.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use crate::codec::{jpeg, opus_ogg};

/// Where the device says it is when nobody has told it otherwise: the
/// trailhead above Lucerne the demo world is written around.
pub const TRAILHEAD: (f64, f64) = (47.0472, 8.3164);

/// The longest a video message runs. The clients' minute: past it the
/// recording stops itself and the strip stays with its two verbs, rather
/// than filling a disk because a finger stayed down.
pub const CIRCLE_MAX: f64 = 60.0;

/// The side a video message is square at, in pixels. The phone's number.
pub const CIRCLE_SIDE: u32 = 384;

// -- what the senses answer with -----------------------------------------------

/// One reading of where the device is.
///
/// The heading is there only while the device is moving: a fix taken
/// standing still has no course over ground, and a live share that drew an
/// arrow from a stale one would point somewhere nobody is going.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Fix {
    pub lat: f64,
    pub lon: f64,
    /// How far off the reading may be, in metres.
    pub accuracy_m: f64,
    /// Degrees clockwise from north, when the device is moving.
    pub heading_deg: Option<f64>,
    /// When it was taken, in unix seconds.
    pub at: f64,
}

impl Fix {
    /// A fix at a place, as accurate as a phone with a clear sky.
    #[must_use]
    pub fn at(lat: f64, lon: f64, when: f64) -> Fix {
        Fix {
            lat,
            lon,
            accuracy_m: 12.0,
            heading_deg: None,
            at: when,
        }
    }

    /// How far another fix is, in metres — the equirectangular
    /// approximation, which is exact enough over the metres a live share
    /// asks about and needs no trigonometry beyond a cosine.
    #[must_use]
    pub fn metres_from(&self, other: &Fix) -> f64 {
        const EARTH_M: f64 = 6_371_000.0;
        let lat = (self.lat - other.lat).to_radians();
        let lon = (self.lon - other.lon).to_radians();
        let mid = ((self.lat + other.lat) / 2.0).to_radians();
        let x = lon * mid.cos();
        (x * x + lat * lat).sqrt() * EARTH_M
    }
}

/// One camera frame, as the three tightly packed I420 planes, lying the way
/// the sensor made it.
///
/// Borrowed rather than owned: it is handed over on the capture thread,
/// thirty times a second, and whoever wants to keep it copies what it needs.
/// It is not turned upright here — a call sends the turn beside the picture
/// and lets the far side do the turning — so `turns` says how it lies.
pub struct CameraFrame<'a> {
    pub width: usize,
    pub height: usize,
    /// Quarter turns clockwise this frame needs to stand upright on the
    /// screen — [`CameraId::turns`] as it was when the frame was made.
    pub turns: u8,
    pub y: &'a [u8],
    pub u: &'a [u8],
    pub v: &'a [u8],
}

/// Whoever wants the camera's frames as they are made.
///
/// A call cannot *draw* the picture; it has to send it, and the library it
/// sends through has no camera of its own on a phone. So the engine leaves a
/// tap here and every frame goes over as it arrives. It is called on the
/// capture thread, where it may not block and may not touch a store: what it
/// is for is one copy into a queue of its own.
pub type FrameTap = Arc<dyn Fn(CameraFrame<'_>) + Send + Sync>;

/// Which camera is open, as the two live ids makepad's `Video` widget wants
/// to be pointed at — and how its picture lies.
///
/// Two plain numbers rather than the platform's own types, because the
/// kernel names no Makepad: the shell puts them back into a `VideoInputId`
/// and a `VideoFormatId` when it points a widget at the open camera.
///
/// A phone's sensor is mounted a quarter turn from its screen, and the
/// screen itself may be turned; the frames come out the way the sensor
/// sees. `turns` is what makes them upright, worked out by the platform's
/// own rule from what the device reports — the sensor's mounting, which way
/// the lens faces and how the screen is turned right now — never assumed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct CameraId {
    pub input: u64,
    pub format: u64,
    /// Quarter turns clockwise a frame of it needs to stand upright on the
    /// screen as it is held now. Nought on a Mac, whose frames arrive
    /// upright.
    pub turns: u8,
    /// Whether it looks at the person: the one a self-view shows as a
    /// mirror, and whose shots are mirrored to match.
    pub front: bool,
}

/// A photograph the camera made.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Photo {
    pub path: PathBuf,
    pub width: u32,
    pub height: u32,
}

/// A voice note the microphone made: Ogg Opus, with the hundred bars the
/// wire carries beside it.
#[derive(Debug, Clone, PartialEq)]
pub struct VoiceNote {
    pub path: PathBuf,
    pub secs: f64,
    /// [`waveform::PACKED`] bytes, as the wire carries them.
    pub waveform: Vec<u8>,
}

/// A video message: a square mp4 and the poster a row draws before it is
/// downloaded.
#[derive(Debug, Clone, PartialEq)]
pub struct VideoNote {
    pub path: PathBuf,
    pub secs: f64,
    /// The side of the square, in pixels.
    pub side: u32,
    pub thumbnail: PathBuf,
}

// -- the two capabilities ------------------------------------------------------

/// Where the device is.
///
/// [`Location::want`] turns the receiver on and [`Location::release`] lets
/// it go — refcounted by the caller, not here: a panel and a worker both
/// want it, and the last one to let go is the one that stops the receiver.
/// [`Location::fix`] is the last reading, `None` until one arrives, which on
/// a cold receiver is a second or two. A denied permission is an error a
/// panel says out loud rather than a silence it waits through.
pub trait Location {
    /// Turns the receiver on. Answers at once; the fix arrives later.
    ///
    /// # Errors
    ///
    /// If the permission was refused, or this build has no receiver.
    fn want(&mut self) -> Result<(), String>;

    /// Lets it go. Nothing being wanted is not a failure.
    fn release(&mut self);

    /// The last reading, if one has arrived.
    fn fix(&self) -> Option<Fix>;

    /// What is wrong with the receiver now, where anything is.
    ///
    /// Not the same question as [`want`](Location::want)'s answer: the
    /// platform's dialog is answered *after* the wish was made, so a panel
    /// that asked and was told nothing can still be told *no* a second
    /// later. A panel reads this on every draw, which is why it is here and
    /// not kept by whoever asked.
    fn trouble(&self) -> Option<String> {
        None
    }
}

/// The camera and the microphone, as captures.
///
/// Each `start_*` opens a device and each `stop_*` closes it and answers the
/// file; [`Capture::discard`] throws away whatever is running without
/// answering one, which is what closing a panel does. [`Capture::level`] is
/// what the meter draws while a recording runs, between 0 and 1.
///
/// The camera is opened on its own, by [`Capture::open_camera`], because a
/// preview runs before and after a recording does: the attach panel shows
/// the picture, then records from the same frames, then shows it again.
///
/// Opening it is two calls rather than the one the change was sketched with
/// — a wish, and [`Capture::camera`] for which camera it turned out to be.
/// It cannot be one: a device list arrives as a platform event, so the
/// camera a `Video` widget is pointed at is not known in the frame the wish
/// was made in. A panel asks for the picture and shows it when it comes, the
/// way it asks for a fix and draws it when it comes.
///
/// A circle stops itself at [`CIRCLE_MAX`]: the recorder writes no more, and
/// [`Capture::stop_circle`] answers the minute it kept.
pub trait Capture {
    /// Asks for the camera — the front one where the device has one.
    /// Answers at once; the picture arrives a moment later.
    ///
    /// One *hold*, not a flag: two panels may want the picture at once —
    /// an attach panel photographing and a video call sending — and the
    /// camera stays open until every one of them has called
    /// [`close_camera`](Capture::close_camera). Whoever opens it closes it;
    /// nobody closes another's.
    ///
    /// # Errors
    ///
    /// If the permission was refused, or this build has no camera.
    fn open_camera(&mut self) -> Result<(), String>;

    /// Which camera is running, once one is: the two live ids a `Video`
    /// widget is pointed at. `None` while it is still opening — and always
    /// on a fake, which has no device to point a widget at, so the box a
    /// panel draws stays empty.
    fn camera(&self) -> Option<CameraId>;

    /// Whether the camera is open, which is not the same as having a
    /// [`camera`](Capture::camera) to draw: the platform names the one it
    /// opened a moment after it is asked, and a run with no device never
    /// names one at all. This is what a video message waits for and what
    /// the panel's line says.
    fn camera_open(&self) -> bool;

    /// Gives a hold back. The camera closes when the last one does;
    /// nothing open is not a failure.
    fn close_camera(&mut self);

    /// The newest camera frame, written into `dir` as a JPEG.
    ///
    /// # Errors
    ///
    /// If the camera is not open, no frame has arrived yet, or the file
    /// cannot be written.
    fn take_photo(&mut self, dir: &Path) -> Result<Photo, String>;

    /// Asks for the microphone's permission and opens nothing.
    ///
    /// Only a call asks. Every other recording asks by starting — the wish
    /// and the permission go together — but a call's microphone is the call
    /// library's own device, opened where this capability cannot see it, so
    /// the permission would never be asked for at all and the first call on
    /// a phone would capture silence. It answers nothing: the platform's
    /// dialog is what happens, and a refusal is the next recording's error.
    fn ask_microphone(&mut self) {}

    /// Asks the platform what the microphone's permission is *now*, raising
    /// no dialog whatever the answer.
    ///
    /// A refusal is remembered, and a person who goes to the platform's own
    /// settings and undoes it is a thing neither platform reports — so a
    /// call that began carrying no voice out would carry none until the app
    /// was started again. This is a call looking, while it has one.
    ///
    /// It answers nothing here either: the platform's answer lands where
    /// [`microphone_allowed`](Capture::microphone_allowed) reads it.
    fn recheck_microphone(&mut self) {}

    /// Whether the microphone's permission has been answered, and how.
    ///
    /// `None` while nobody has answered — the platform's dialog is still
    /// standing open — `Some(true)` once it is granted or on a platform
    /// that never asks, `Some(false)` where it was refused.
    ///
    /// Only a call asks this, and for the reason it asks
    /// [`ask_microphone`](Capture::ask_microphone): its engine opens the
    /// device itself, where this capability cannot see it, and a capture
    /// handed a device the platform has not allowed throws rather than
    /// falling quiet. So a call that is not told *yes* here is started with
    /// no microphone named at all, and looks again while it runs
    /// ([`recheck_microphone`](Capture::recheck_microphone)). It does not
    /// wait: the other end's connection is given about ten seconds, which
    /// is less than a dialog takes to read.
    fn microphone_allowed(&self) -> Option<bool> {
        Some(true)
    }

    /// Starts a voice note in `dir`.
    ///
    /// # Errors
    ///
    /// If the permission was refused, there is no microphone, or something
    /// is already being recorded.
    fn start_voice(&mut self, dir: &Path) -> Result<(), String>;

    /// Ends it and answers the file.
    ///
    /// # Errors
    ///
    /// If nothing was being recorded, the note does not clear the floor
    /// ([`opus_ogg::long_enough`]) — which only a device a person let go of
    /// too quickly does not — or the file could not be finished.
    fn stop_voice(&mut self) -> Result<VoiceNote, String>;

    /// Starts a video message in `dir`, from the camera and the microphone
    /// together. The camera must be open.
    ///
    /// # Errors
    ///
    /// As [`Capture::start_voice`], and if the camera is not open.
    fn start_circle(&mut self, dir: &Path) -> Result<(), String>;

    /// Ends it and answers the file and its poster.
    ///
    /// # Errors
    ///
    /// If nothing was being recorded, or the file could not be finished.
    fn stop_circle(&mut self) -> Result<VideoNote, String>;

    /// Throws away whatever is running, file and all.
    fn discard(&mut self);

    /// What the meter draws, 0 to 1. Zero when nothing is running.
    fn level(&self) -> f32;

    /// Hands every camera frame on as it arrives, as well as keeping it;
    /// `None` takes the tap off again.
    ///
    /// Only a video call asks: the picture it wants is one to send, not one
    /// to draw, and only while the call runs. A capture with no real camera
    /// behind it — which is every scripted run — keeps the tap and never
    /// calls it, having no frame to call it with.
    fn watch_frames(&mut self, _watch: Option<FrameTap>) {}
}

/// A recording's level as a fake microphone hears it: a wave over the
/// seconds, so a level that never moves is never mistaken for a live one.
///
/// Here rather than in the shell's media kit because both draw it: the kit
/// fills a meter with it, and [`FakeCapture`] answers it as its level, and
/// the two have to be the one wave.
#[must_use]
pub fn fake_level(elapsed: f64) -> f32 {
    let t = elapsed * 7.3;
    (0.45 + 0.35 * (t.sin() * (t * 0.37).cos())).clamp(0.05, 1.0) as f32
}

// -- the fakes -----------------------------------------------------------------

/// A receiver that answers the trailhead, and a test may move it.
///
/// Shared, like the clipboard and the voice, because each world is built
/// where it is used: a worker moving a live share reads the fix the window
/// is drawing.
#[derive(Clone, Default, Debug)]
pub struct FakeLocation(Arc<Mutex<Where>>);

/// What [`FakeLocation`] keeps.
#[derive(Debug)]
struct Where {
    fix: Option<Fix>,
    /// How many callers have asked for the receiver, so a test can prove
    /// that the last release is what turns it off.
    wanted: usize,
    /// A refusal to answer with, for a test that wants one.
    denied: Option<String>,
}

impl Default for Where {
    fn default() -> Where {
        Where {
            fix: Some(Fix::at(TRAILHEAD.0, TRAILHEAD.1, crate::time::virtual_epoch())),
            wanted: 0,
            denied: None,
        }
    }
}

impl FakeLocation {
    /// A receiver at the trailhead, wanted by nobody yet.
    #[must_use]
    pub fn new() -> FakeLocation {
        FakeLocation::default()
    }

    /// Moves the device.
    pub fn set_fix(&self, fix: Fix) {
        if let Ok(mut w) = self.0.lock() {
            w.fix = Some(fix);
        }
    }

    /// Takes the fix away: what a receiver that has not warmed up answers.
    pub fn clear(&self) {
        if let Ok(mut w) = self.0.lock() {
            w.fix = None;
        }
    }

    /// Refuses, in these words, until it is told otherwise.
    pub fn deny(&self, why: &str) {
        if let Ok(mut w) = self.0.lock() {
            w.denied = Some(why.to_string());
        }
    }

    /// Allows again.
    pub fn allow(&self) {
        if let Ok(mut w) = self.0.lock() {
            w.denied = None;
        }
    }

    /// How many callers are holding the receiver on.
    #[must_use]
    pub fn wanted(&self) -> usize {
        self.0.lock().map_or(0, |w| w.wanted)
    }
}

impl Location for FakeLocation {
    fn want(&mut self) -> Result<(), String> {
        let mut w = self
            .0
            .lock()
            .map_err(|_| "the receiver is poisoned".to_string())?;
        if let Some(why) = &w.denied {
            return Err(why.clone());
        }
        w.wanted += 1;
        Ok(())
    }

    fn release(&mut self) {
        if let Ok(mut w) = self.0.lock() {
            w.wanted = w.wanted.saturating_sub(1);
        }
    }

    fn fix(&self) -> Option<Fix> {
        self.0.lock().ok().and_then(|w| w.fix)
    }

    /// Whatever it is refusing with — which a test may set after a panel
    /// has already been given the receiver, as a person tapping *no* does.
    fn trouble(&self) -> Option<String> {
        self.0.lock().ok().and_then(|w| w.denied.clone())
    }
}

/// A camera and a microphone that write real files.
///
/// What it captures is fixed — the same shot, the same tone, the same clip
/// — because a suite compares what it drew against what it drew last time.
/// What is *real* about it is the file: a JPEG a decoder opens, an Ogg
/// Opus a player plays, an mp4 with a picture in it. Shared, like the
/// others, so a test reads back what a panel captured.
#[derive(Clone, Default, Debug)]
pub struct FakeCapture(Arc<Mutex<Books>>);

/// What [`FakeCapture`] keeps.
#[derive(Debug, Default)]
struct Books {
    /// How many callers are holding the camera open. Which camera it is,
    /// the fake does not say: there is no device behind a scripted run, and
    /// an id a widget cannot find one behind is worse than none — on a
    /// windowed run it is the platform's camera dialog, raised for a camera
    /// that is not there.
    camera: usize,
    /// What is being recorded, and where it will be written.
    running: Option<(Kind, PathBuf)>,
    /// How many times the microphone's permission has been asked for, so a
    /// test can prove that a call asks before it is ready to record.
    asked: u32,
    /// And how many times it has been looked at again without a dialog, so
    /// a test can prove that a call carrying no voice out keeps looking —
    /// and how often.
    rechecked: u32,
    /// What it says the microphone's permission is: granted, until a test
    /// says the dialog is still open or that it was refused.
    microphone: Allowed,
    /// Every file it has written, oldest first.
    made: Vec<PathBuf>,
    /// How many times the level has been read, which is what moves it: the
    /// fake has no clock, and a meter is drawn once a frame.
    readings: u32,
}

/// What a fake says about the microphone's permission.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
enum Allowed {
    /// Granted, or a platform with no dialog to raise — which is what a
    /// suite is, and why this is the one a test does not have to set.
    #[default]
    Granted,
    /// The dialog is open and nobody has answered it yet.
    Unanswered,
    Refused,
}

/// What a fake recording is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Kind {
    Voice,
    Circle,
}

/// How long a fake voice note is. Fixed, because the fake has no clock and
/// a suite wants the same duration on every run.
const FAKE_VOICE_SECS: f64 = 2.0;

/// And how long a fake video message is — the demo clip's own length.
const FAKE_CIRCLE_SECS: f64 = 1.0;

impl FakeCapture {
    /// A camera and a microphone with nothing running and nothing written.
    #[must_use]
    pub fn new() -> FakeCapture {
        FakeCapture::default()
    }

    /// Every file it has written, oldest first.
    #[must_use]
    pub fn made(&self) -> Vec<PathBuf> {
        self.0.lock().map(|b| b.made.clone()).unwrap_or_default()
    }

    /// The last file it wrote.
    #[must_use]
    pub fn last(&self) -> Option<PathBuf> {
        self.0.lock().ok()?.made.last().cloned()
    }

    /// Whether something is being recorded.
    #[must_use]
    pub fn recording(&self) -> bool {
        self.0.lock().is_ok_and(|b| b.running.is_some())
    }

    /// How many callers are holding the camera open, so a test can prove
    /// that one panel's clean-up does not close another panel's picture.
    #[must_use]
    pub fn camera_holders(&self) -> usize {
        self.0.lock().map_or(0, |b| b.camera)
    }

    /// How many times the microphone's permission has been asked for.
    #[must_use]
    pub fn microphone_asked(&self) -> u32 {
        self.0.lock().map_or(0, |b| b.asked)
    }

    /// How many times it has been looked at again, dialog and all left
    /// alone.
    #[must_use]
    pub fn microphone_rechecked(&self) -> u32 {
        self.0.lock().map_or(0, |b| b.rechecked)
    }

    /// Leaves the microphone's permission unanswered, as a dialog standing
    /// open on the glass leaves it: what a call started in that moment has
    /// to wait for.
    pub fn microphone_unanswered(&self) {
        self.say_microphone(Allowed::Unanswered);
    }

    /// Answers it, either way.
    pub fn answer_microphone(&self, allowed: bool) {
        self.say_microphone(if allowed { Allowed::Granted } else { Allowed::Refused });
    }

    fn say_microphone(&self, allowed: Allowed) {
        if let Ok(mut b) = self.0.lock() {
            b.microphone = allowed;
        }
    }

    fn books(&self) -> Result<std::sync::MutexGuard<'_, Books>, String> {
        self.0
            .lock()
            .map_err(|_| "the camera is poisoned".to_string())
    }
}

impl Capture for FakeCapture {
    fn open_camera(&mut self) -> Result<(), String> {
        self.books()?.camera += 1;
        Ok(())
    }

    /// Never one: there is no device here for a `Video` widget to open, and
    /// pointing it at an id nothing answers would ask whoever is at the
    /// machine for the camera permission on a run that is not theirs.
    fn camera(&self) -> Option<CameraId> {
        None
    }

    fn camera_open(&self) -> bool {
        self.0.lock().is_ok_and(|b| b.camera > 0)
    }

    fn close_camera(&mut self) {
        if let Ok(mut b) = self.0.lock() {
            b.camera = b.camera.saturating_sub(1);
        }
    }

    fn take_photo(&mut self, dir: &Path) -> Result<Photo, String> {
        let mut books = self.books()?;
        if books.camera == 0 {
            return Err("the camera is not open".to_string());
        }
        let path = next_name(&books, dir, "jpg");
        let (w, h) = DEMO_SHOT;
        let bytes = jpeg::of_rgb(&demo_shot(), w, h, jpeg::PHOTO_MAX)?;
        write(&path, &bytes)?;
        books.made.push(path.clone());
        let (w, h) = jpeg::fitted(w, h, jpeg::PHOTO_MAX);
        Ok(Photo {
            path,
            width: w as u32,
            height: h as u32,
        })
    }

    /// Written down and nothing else: there is no dialog to raise here, and
    /// what a test wants to know is that the call asked.
    fn ask_microphone(&mut self) {
        if let Ok(mut b) = self.0.lock() {
            b.asked = b.asked.saturating_add(1);
        }
    }

    /// The same, counted apart: there is no platform here to look at, and
    /// what a test wants to know is how often the call looked.
    fn recheck_microphone(&mut self) {
        if let Ok(mut b) = self.0.lock() {
            b.rechecked = b.rechecked.saturating_add(1);
        }
    }

    /// Granted, until a test says otherwise: a suite is never asked.
    fn microphone_allowed(&self) -> Option<bool> {
        match self.0.lock().map(|b| b.microphone).unwrap_or_default() {
            Allowed::Granted => Some(true),
            Allowed::Unanswered => None,
            Allowed::Refused => Some(false),
        }
    }

    fn start_voice(&mut self, dir: &Path) -> Result<(), String> {
        let mut books = self.books()?;
        if books.running.is_some() {
            return Err("something is already being recorded".to_string());
        }
        let path = next_name(&books, dir, "ogg");
        books.running = Some((Kind::Voice, path));
        books.readings = 0;
        Ok(())
    }

    fn stop_voice(&mut self) -> Result<VoiceNote, String> {
        let mut books = self.books()?;
        let path = match books.running.take() {
            Some((Kind::Voice, path)) => path,
            Some((Kind::Circle, path)) => {
                books.running = Some((Kind::Circle, path));
                return Err("a video message is being recorded, not a voice note".to_string());
            }
            None => return Err("nothing is being recorded".to_string()),
        };
        let mut voice = opus_ogg::Voice::create(&path, f64::from(opus_ogg::RATE))?;
        voice.push(&tone(FAKE_VOICE_SECS))?;
        let done = voice.finish()?;
        // The same floor the platform's microphone answers to. The fake's
        // note is two seconds and clears it, and this is what says so: the
        // rule belongs to a voice note, not to a device.
        opus_ogg::long_enough(done.secs)?;
        books.made.push(done.path.clone());
        Ok(VoiceNote {
            path: done.path,
            secs: done.secs,
            waveform: done.waveform,
        })
    }

    fn start_circle(&mut self, dir: &Path) -> Result<(), String> {
        let mut books = self.books()?;
        if books.camera == 0 {
            return Err("the camera is not open".to_string());
        }
        if books.running.is_some() {
            return Err("something is already being recorded".to_string());
        }
        let path = next_name(&books, dir, "mp4");
        books.running = Some((Kind::Circle, path));
        books.readings = 0;
        Ok(())
    }

    fn stop_circle(&mut self) -> Result<VideoNote, String> {
        let mut books = self.books()?;
        let path = match books.running.take() {
            Some((Kind::Circle, path)) => path,
            Some((Kind::Voice, path)) => {
                books.running = Some((Kind::Voice, path));
                return Err("a voice note is being recorded, not a video message".to_string());
            }
            None => return Err("nothing is being recorded".to_string()),
        };
        write(&path, super::demo::CLIP_MP4)?;
        let thumbnail = path.with_extension("jpg");
        let side = jpeg::THUMBNAIL;
        write(
            &thumbnail,
            &jpeg::of_rgb(&square_poster(side), side, side, jpeg::THUMBNAIL)?,
        )?;
        books.made.push(path.clone());
        books.made.push(thumbnail.clone());
        Ok(VideoNote {
            path,
            secs: FAKE_CIRCLE_SECS,
            side: CIRCLE_SIDE,
            thumbnail,
        })
    }

    fn discard(&mut self) {
        if let Ok(mut b) = self.0.lock() {
            b.running = None;
        }
    }

    fn level(&self) -> f32 {
        let Ok(mut b) = self.0.lock() else { return 0.0 };
        if b.running.is_none() {
            return 0.0;
        }
        // The fake has no clock, so the reading itself is the clock: a meter
        // drawn once a frame walks the wave at sixty steps a second.
        b.readings = b.readings.wrapping_add(1);
        fake_level(f64::from(b.readings) / 60.0)
    }
}

/// The next name under `dir`, numbered rather than clocked: the fake has no
/// clock, and two captures in one run must not be one file.
fn next_name(books: &Books, dir: &Path, ext: &str) -> PathBuf {
    let n = books.made.len() + 1;
    dir.join(format!("capture-{n}.{ext}"))
}

/// The shot the fake camera takes, in pixels: small, because its whole job
/// is to be a real JPEG of a real size.
const DEMO_SHOT: (usize, usize) = (480, 360);

/// What it is a picture of: a soft gradient with a lighter square in it, so
/// a card that decoded it shows something rather than a flat field.
fn demo_shot() -> Vec<u8> {
    let (w, h) = DEMO_SHOT;
    let mut rgb = vec![0u8; w * h * 3];
    for y in 0..h {
        for x in 0..w {
            let i = (y * w + x) * 3;
            let inside = (160..320).contains(&x) && (100..260).contains(&y);
            let shade = if inside {
                220
            } else {
                40 + (x * 120 / w + y * 60 / h) as u8
            };
            rgb[i] = shade;
            rgb[i + 1] = shade;
            rgb[i + 2] = shade;
        }
    }
    rgb
}

/// The poster of the demo clip: the same square the clip draws.
fn square_poster(side: usize) -> Vec<u8> {
    let mut rgb = vec![42u8; side * side * 3];
    let (from, to) = (side * 4 / 10, side * 6 / 10);
    for y in from..to {
        for x in from..to {
            let i = (y * side + x) * 3;
            rgb[i] = 232;
            rgb[i + 1] = 232;
            rgb[i + 2] = 232;
        }
    }
    rgb
}

/// A 440 Hz tone, mono, at the one rate Opus keeps.
fn tone(secs: f64) -> Vec<f32> {
    let rate = f64::from(opus_ogg::RATE);
    (0..(secs * rate) as usize)
        .map(|n| {
            let t = n as f64 / rate;
            (0.7 * (t * 440.0 * std::f64::consts::TAU).sin()) as f32
        })
        .collect()
}

/// Writes a capture's file, making the directory it goes in. Both are one
/// step here because a test's directory may not exist until the first
/// capture asks for it.
fn write(path: &Path, bytes: &[u8]) -> Result<(), String> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    }
    std::fs::write(path, bytes).map_err(|e| format!("{}: {e}", path.display()))
}

// -- which senses a build runs on ----------------------------------------------

/// How one world gets the senses.
///
/// The same shape as [`DiskFactory`](super::DiskFactory), for the same
/// reason and one more. The reason: a worker builds its own world on its own
/// thread, so a capability cannot be handed over as a value. The one more:
/// there is only *one* camera and one receiver on a device, so every world
/// of a run must be given a handle on the same state — a worker that turned
/// a second receiver on would be asking a phone to warm up a chip nobody is
/// reading.
///
/// A build the shell gave nothing gets the fakes, which is what every test,
/// every scripted run and every library mount is.
#[derive(Clone)]
pub struct SenseSource {
    /// The fakes, when these are them — so a world can register them under
    /// their own types and a test can read back what was captured.
    fakes: Option<(FakeLocation, FakeCapture)>,
    make: Arc<MakeSenses>,
}

/// What a source hands one world: the pair, because the two are served from
/// one state and are made together.
type MakeSenses = dyn Fn() -> (Box<dyn Location>, Box<dyn Capture>) + Send + Sync;

impl SenseSource {
    /// The fakes, shared by every world built from this source.
    #[must_use]
    pub fn fake() -> SenseSource {
        let (location, capture) = (FakeLocation::new(), FakeCapture::new());
        let (l, c) = (location.clone(), capture.clone());
        SenseSource {
            fakes: Some((location, capture)),
            make: Arc::new(move || (Box::new(l.clone()), Box::new(c.clone()))),
        }
    }

    /// The platform's own, as the shell installed them. The closure hands
    /// out handles on one state, never a second device.
    #[must_use]
    pub fn platform(
        make: impl Fn() -> (Box<dyn Location>, Box<dyn Capture>) + Send + Sync + 'static,
    ) -> SenseSource {
        SenseSource {
            fakes: None,
            make: Arc::new(make),
        }
    }

    /// The fakes behind this source, where they are the fakes.
    #[must_use]
    pub fn fakes(&self) -> Option<(FakeLocation, FakeCapture)> {
        self.fakes.clone()
    }

    /// One pair of capabilities, for one world.
    #[must_use]
    pub fn capabilities(&self) -> (Box<dyn Location>, Box<dyn Capture>) {
        (self.make)()
    }
}

impl Default for SenseSource {
    fn default() -> SenseSource {
        SenseSource::fake()
    }
}

impl std::fmt::Debug for SenseSource {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(if self.fakes.is_some() {
            "SenseSource::fake"
        } else {
            "SenseSource::platform"
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::codec::waveform;

    /// The trailhead until a test moves the device, and one refusal in
    /// words when it is refused.
    #[test]
    fn the_fake_receiver_answers_the_trailhead_and_can_be_moved() {
        let shared = FakeLocation::new();
        let mut a = shared.clone();
        let b = shared.clone();
        let fix = b.fix().expect("a fix");
        assert_eq!((fix.lat, fix.lon), TRAILHEAD);

        a.want().expect("the receiver");
        assert_eq!(shared.wanted(), 1);
        a.release();
        assert_eq!(shared.wanted(), 0);
        a.release();
        assert_eq!(shared.wanted(), 0, "a release too many is not a panic");

        // Every world reads the one receiver.
        shared.set_fix(Fix::at(55.7558, 37.6173, 100.0));
        let moved = b.fix().expect("a fix");
        assert!((moved.lat - 55.7558).abs() < 1e-9);
        assert!(moved.metres_from(&fix) > 1_000_000.0, "Moscow is not Lucerne");

        shared.clear();
        assert_eq!(b.fix(), None);

        shared.deny("the location is not allowed");
        assert_eq!(a.want(), Err("the location is not allowed".to_string()));
        // And a refusal that arrives *after* the receiver was given out is
        // what a panel already holding it reads: the platform's dialog is
        // answered long after the wish that raised it.
        assert_eq!(b.trouble(), Some("the location is not allowed".to_string()));
        shared.allow();
        assert!(a.want().is_ok());
        assert_eq!(b.trouble(), None);
    }

    /// A metre is a metre: the rule a live share edits by.
    #[test]
    fn a_fix_measures_the_distance_to_another() {
        let here = Fix::at(TRAILHEAD.0, TRAILHEAD.1, 0.0);
        let same = Fix::at(TRAILHEAD.0, TRAILHEAD.1, 10.0);
        assert!(here.metres_from(&same) < 0.001);
        // A ten-thousandth of a degree of latitude is about eleven metres.
        let north = Fix::at(TRAILHEAD.0 + 0.0001, TRAILHEAD.1, 10.0);
        let moved = here.metres_from(&north);
        assert!((10.0..13.0).contains(&moved), "{moved} m");
    }

    /// The fake camera writes a real JPEG, and refuses a shot when it is
    /// not open.
    #[test]
    fn the_fake_camera_writes_a_real_photograph() {
        let dir = tempfile::tempdir().expect("a temp dir");
        let shared = FakeCapture::new();
        let mut capture = shared.clone();
        assert!(capture.take_photo(dir.path()).is_err(), "not open yet");

        capture.open_camera().expect("a camera");
        assert!(shared.camera_open());
        assert_eq!(
            capture.camera(),
            None,
            "open, and no camera for a widget to point at: a scripted run \
             must not raise the platform's own camera dialog"
        );
        let photo = capture.take_photo(dir.path()).expect("a shot");
        let bytes = std::fs::read(&photo.path).expect("the file");
        assert_eq!(&bytes[..2], b"\xff\xd8", "a JPEG");
        assert_eq!((photo.width, photo.height), (480, 360));
        assert_eq!(shared.made(), vec![photo.path.clone()]);

        // A second shot is a second file, as a strip of them would be.
        let again = capture.take_photo(dir.path()).expect("another");
        assert_ne!(again.path, photo.path);
        capture.close_camera();
        assert!(!shared.camera_open());
        assert_eq!(capture.camera(), None);
        capture.close_camera();
        assert_eq!(shared.camera_holders(), 0, "a close too many is not a panic");
    }

    /// The camera is held rather than flagged: two panels may want the
    /// picture at once, and the one that lets go is not the one that closes
    /// it — the last one is.
    #[test]
    fn the_fake_camera_stays_open_while_anybody_is_holding_it() {
        let shared = FakeCapture::new();
        let (mut panel, mut call) = (shared.clone(), shared.clone());
        panel.open_camera().expect("a camera");
        call.open_camera().expect("the same camera");
        assert_eq!(shared.camera_holders(), 2);

        call.close_camera();
        assert!(shared.camera_open(), "the panel is still looking through it");
        panel.close_camera();
        assert!(!shared.camera_open(), "and the last one out closes it");
    }

    /// A call asks for the microphone without opening one: its own library
    /// is what captures, and nothing else here would ever ask.
    #[test]
    fn the_fake_microphone_remembers_being_asked_for() {
        let shared = FakeCapture::new();
        let mut capture = shared.clone();
        assert_eq!(shared.microphone_asked(), 0);
        capture.ask_microphone();
        assert_eq!(shared.microphone_asked(), 1);
        assert!(!shared.recording(), "asking records nothing");

        // And what the answer is, for a call that must not start its
        // engine while the dialog is still standing open.
        assert_eq!(capture.microphone_allowed(), Some(true), "a suite is never asked");
        shared.microphone_unanswered();
        assert_eq!(capture.microphone_allowed(), None);
        shared.answer_microphone(false);
        assert_eq!(capture.microphone_allowed(), Some(false));
        shared.answer_microphone(true);
        assert_eq!(capture.microphone_allowed(), Some(true));
    }

    /// The fake microphone writes a real Ogg Opus note, with a waveform
    /// computed the way a real one is.
    #[test]
    fn the_fake_microphone_writes_a_real_voice_note() {
        let dir = tempfile::tempdir().expect("a temp dir");
        let shared = FakeCapture::new();
        let mut capture = shared.clone();
        assert!(capture.stop_voice().is_err(), "nothing is being recorded");
        assert_eq!(capture.level(), 0.0);

        capture.start_voice(dir.path()).expect("a recording");
        assert!(shared.recording());
        assert!(
            capture.start_voice(dir.path()).is_err(),
            "one recording at a time"
        );
        let level = capture.level();
        assert!(level > 0.0 && level <= 1.0, "the meter moves: {level}");

        let note = capture.stop_voice().expect("a note");
        assert!(!shared.recording());
        assert!((note.secs - 2.0).abs() < 0.05, "{}", note.secs);
        assert_eq!(note.waveform.len(), waveform::PACKED);
        assert!(note.waveform.iter().any(|b| *b != 0), "a tone, not silence");
        let bytes = std::fs::read(&note.path).expect("the file");
        assert_eq!(&bytes[..4], b"OggS", "an Ogg stream");
        assert!(bytes.len() > 1000);
    }

    /// And a real mp4 with a real poster beside it.
    #[test]
    fn the_fake_camera_writes_a_real_video_message() {
        let dir = tempfile::tempdir().expect("a temp dir");
        let shared = FakeCapture::new();
        let mut capture = shared.clone();
        assert!(
            capture.start_circle(dir.path()).is_err(),
            "the camera is not open"
        );
        capture.open_camera().expect("a camera");
        capture.start_circle(dir.path()).expect("a recording");
        assert!(
            capture.stop_voice().is_err(),
            "a video message is not a voice note"
        );
        let note = capture.stop_circle().expect("a video message");
        assert_eq!(note.side, CIRCLE_SIDE);
        let bytes = std::fs::read(&note.path).expect("the file");
        assert_eq!(&bytes[4..8], b"ftyp", "an mp4");
        let poster = std::fs::read(&note.thumbnail).expect("the poster");
        assert_eq!(&poster[..2], b"\xff\xd8", "a JPEG");
        assert_eq!(shared.made().len(), 2, "the clip and its poster");

        // What is thrown away leaves no file behind.
        capture.start_circle(dir.path()).expect("another");
        capture.discard();
        assert!(!shared.recording());
        assert_eq!(shared.made().len(), 2);
    }

    /// The source hands every world a handle on the one state.
    #[test]
    fn one_source_is_one_camera_for_every_world() {
        let dir = tempfile::tempdir().expect("a temp dir");
        let source = SenseSource::fake();
        let (mut window_l, mut window_c) = source.capabilities();
        let (worker_l, _) = source.capabilities();

        window_l.want().expect("the receiver");
        assert_eq!(
            worker_l.fix().map(|f| (f.lat, f.lon)),
            Some(TRAILHEAD),
            "the worker reads the window's receiver"
        );
        window_c.open_camera().expect("a camera");
        let (fake_location, fake_capture) = source.fakes().expect("the fakes");
        assert_eq!(fake_location.wanted(), 1);
        assert!(fake_capture.camera_open(), "and the one camera");
        window_c.take_photo(dir.path()).expect("a shot");
        assert_eq!(fake_capture.made().len(), 1);
        assert_eq!(format!("{source:?}"), "SenseSource::fake");
    }
}
