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
//!   to 384, stands it upright and hands it to the recorder.
//! - The **microphone callback** runs on the audio thread. It smooths a
//!   level for the meter and hands the samples to whichever recorder is
//!   running.
//! - Each **recorder** is a thread of its own with a channel in front of it,
//!   so neither callback ever waits on an encoder. A voice note goes through
//!   [`opus_ogg`](kernel::codec::opus_ogg); a video message through
//!   [`circle`], which is AVAssetWriter on a Mac and `MediaCodec` with
//!   `MediaMuxer` on the phone.
//!
//! **Which way up.** A phone's sensor is bolted to the body a quarter turn
//! from the screen, and the frames come out the way it sees rather than the
//! way the phone is held: nothing turns them but this module. The turn is
//! never assumed — it is [worked out](upright_turns) from the sensor's own
//! mounting, which way the lens faces and how the screen is turned at that
//! moment, by the rule CameraX uses. A file leaves here upright, because an
//! mp4 and a JPEG carry no turn anybody reads; a frame handed to a call
//! leaves as it lies, with the turn beside it, because the far side can turn
//! it for nothing; and a preview is turned by the shader that draws it.
//!
//! Only a real run that nobody scripts gets any of this (`shell::boot`),
//! beside the real voice and the real clipboard: a suite may not open the
//! camera of whoever is at the machine, and every scripted run, every test
//! and every library mount reads the kernel's fakes instead — which write
//! real files, so the panels above are the same panels.

use std::collections::VecDeque;
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

/// The frames a second a video message is written at, as a number to
/// compare a camera's own rate against. The file's own is
/// [`circle::FPS`](circle::FPS); this is the one a format is chosen by.
const WANTED_FPS: f64 = circle::FPS as f64;

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
    /// How many callers are holding the camera open. A count rather than a
    /// flag because two panels may want the picture at once — an attach
    /// panel photographing and a video call sending — and one of them
    /// closing it on the other is a panel left drawing an empty box.
    camera_held: usize,
    /// What the platform offers, once it has said. Empty until then.
    cameras: Vec<Choice>,
    /// The camera the session was started on, which is what a widget shows.
    camera: Option<CameraId>,
    /// And which of the platform's cameras that was. Kept because the turn
    /// its picture needs is not fixed: the sensor's mounting is, but the
    /// way the phone is being held is not, and the turn is worked out from
    /// both every time the screen moves.
    chosen: Option<Choice>,
    /// How the screen is turned now, in quarter turns anticlockwise from
    /// the device's natural orientation — `Display.getRotation()` on the
    /// phone, and nought on every other platform, whose windows do not
    /// turn. Read when a session opens and again on every geometry change,
    /// which is what a rotation arrives as.
    screen_turns: u8,
    /// Whether the frame callback has been registered — once per run.
    watching: bool,
    /// Whether the open session was started before the permission was
    /// answered, which on a phone is a session with no picture in it. The
    /// grant closes it and opens it again.
    camera_stale: bool,
    camera_trouble: Option<String>,
    /// Whether the platform's last word on the camera was a refusal, which
    /// a second wish for the picture looks at again: a person may have gone
    /// to the settings panel and undone it, and nothing reports that.
    camera_refused: bool,
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
    /// The same as [`camera_stale`](State::camera_stale): an input opened
    /// before the permission was answered hands over silence, and the grant
    /// is what reopens it.
    microphone_stale: bool,
    /// A call asking for the permission and nothing else — its own library
    /// is what opens the device. Taken by the next [`Senses::work`].
    ask_microphone: bool,
    /// The same wish with no dialog in it: a call that is already carrying
    /// and hearing nothing, looking again at a refusal.
    poll_microphone: bool,
    microphone_trouble: Option<String>,
    /// What the platform answered about the microphone, once it has
    /// answered anything: `None` while the dialog is open. A call reads it
    /// through [`Capture::microphone_allowed`] before it starts its engine,
    /// whose capture opens the device where this cannot see it.
    microphone_answer: Option<bool>,
    /// Whether that answer was android's *denied, ask me again* — the first
    /// refusal, which the platform will still raise a dialog for. The next
    /// call may have that second dialog; nothing after it will.
    microphone_retry: bool,
    level: f32,

    // -- permissions, asked once per kind per run
    asked_location: bool,
    asked_camera: bool,
    asked_microphone: bool,
    /// The one being put to the platform, until its result lands. One at
    /// a time: android cancels a request made while another is up, and the
    /// second one's answer never comes at all.
    asking: Option<Ask>,
    /// The ones waiting their turn behind it, first asked first.
    queued: VecDeque<Ask>,

    // -- what is being recorded
    voice: Option<Run<VoiceNote>>,
    circle: Option<Run<VideoNote>>,
}

/// One thing to put to the platform about a permission: which one, and
/// whether the person is to be asked or the answer merely looked up.
///
/// A *check* is makepad's `check_permission`: on a Mac it reads
/// `AVCaptureDevice`'s authorization status, on the phone it is
/// `checkSelfPermission`, and on both it answers with the same
/// `PermissionResult` and raises nothing at all. It is how a refusal undone
/// in the platform's own settings is noticed, since neither platform says a
/// word when that happens.
#[derive(Clone, Copy, PartialEq)]
struct Ask {
    permission: Permission,
    dialog: bool,
}

impl Ask {
    /// One the person answers.
    fn dialog(permission: Permission) -> Ask {
        Ask { permission, dialog: true }
    }

    /// One nobody sees.
    fn check(permission: Permission) -> Ask {
        Ask { permission, dialog: false }
    }
}

impl State {
    /// Puts one thing in the queue, where the same permission is not
    /// already on its way to the platform. A poll that ran while an answer
    /// was still coming would otherwise stack up behind it.
    fn wants(&mut self, ask: Ask) {
        if self.asking.is_some_and(|a| a.permission == ask.permission)
            || self.queued.iter().any(|q| q.permission == ask.permission)
        {
            return;
        }
        self.queued.push_back(ask);
    }

    /// The microphone's permission, wanted by a call.
    ///
    /// `dialog` is whether the person may be asked: a call *appearing* may
    /// ask, a call already carrying without a voice may only look. Which
    /// of the three this turns into:
    ///
    /// - never asked in this run — the one real request;
    /// - refused — a check, because the settings panel may have undone it
    ///   since and nothing else would ever notice;
    /// - refused once on android, and a call appearing — the second dialog
    ///   the platform is still willing to raise, and only that one;
    /// - answered any other way — nothing: a grant is a grant, and an open
    ///   dialog is already outstanding.
    fn want_microphone(&mut self, dialog: bool) {
        if !self.asked_microphone {
            self.asked_microphone = true;
            self.wants(Ask::dialog(Permission::AudioInput));
            return;
        }
        if self.microphone_answer != Some(false) {
            return;
        }
        let again = dialog && std::mem::take(&mut self.microphone_retry);
        self.wants(if again {
            Ask::dialog(Permission::AudioInput)
        } else {
            Ask::check(Permission::AudioInput)
        });
    }

    /// Works out again how the open camera's picture lies — from the camera
    /// the session was opened on and how the screen is turned now — and
    /// writes it where a panel and the capture thread read it.
    ///
    /// Answers whether the turn moved, which is a preview drawn the wrong
    /// way up until something asks for another frame. Nothing open is no
    /// move: there is no picture to turn.
    fn stand(&mut self) -> bool {
        let Some(chosen) = self.chosen else {
            return false;
        };
        let turns = upright_turns(chosen.orientation, chosen.front, self.screen_turns);
        match &mut self.camera {
            Some(camera) if camera.turns != turns => {
                camera.turns = turns;
                true
            }
            _ => false,
        }
    }
}

/// One camera the platform offers, and the format picked on it.
#[derive(Clone, Copy)]
struct Choice {
    input: VideoInputId,
    format: VideoFormatId,
    /// Whether it faces the person. The front one is what a video message
    /// is recorded with.
    front: bool,
    /// How the sensor is mounted: the clockwise degrees a frame of it needs
    /// to stand upright on the device's natural screen, which is android's
    /// `SENSOR_ORIENTATION`. Nought on a Mac, whose frames arrive upright.
    orientation: u32,
}

/// The newest camera frame, kept as I420 with tightly packed planes, lying
/// the way the sensor made it.
///
/// The turn and the mirror come with it because a photograph is written
/// from here long after the frame arrived, and by then the screen may have
/// moved: what a shot must be is what the person was looking at when they
/// pressed.
#[derive(Default)]
struct Frame {
    width: usize,
    height: usize,
    /// Quarter turns clockwise it needs to stand upright —
    /// [`CameraId::turns`] as it was when the frame came in.
    turns: u8,
    /// Whether it came out of the front camera, whose shot is mirrored to
    /// match the preview.
    mirror: bool,
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
    /// One square NV12 frame of a video message, and the moment the camera
    /// handed it over — which is what times it. A counter would time it at
    /// whatever rate the file declares, and a camera that answers
    /// twenty-four or sixty frames a second would then run against its own
    /// sound; the channel in front of the encoder can drop a frame besides.
    Square(std::time::Instant, Vec<u8>),
}

impl Senses {
    /// The senses of a run, with nothing wanted, nothing open and nothing
    /// recording. The stage holds one and serves it; the capabilities are
    /// handles on it.
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
        // A phone turned in the hand arrives as a geometry change and
        // nothing else — no event of makepad's carries the screen's own
        // rotation — and the camera's picture has to be stood up by it
        // again: the sensor is mounted where it is mounted, but which
        // quarter turn makes it upright depends on how the phone is held.
        let turned = matches!(event, Event::WindowGeomChange(_)) && self.turned();
        let work = self.work();
        self.perform(cx, work);
        moved || turned
    }

    /// Reads how the screen is turned now and stands the open camera's
    /// picture by it. Answers whether the turn moved, which is a preview
    /// that has to be drawn again.
    ///
    /// The platform is asked with no lock held — on the phone it is JNI,
    /// and a frame arriving in the middle of it must not queue behind a
    /// call into Java — and what comes back is written down under the lock.
    fn turned(&self) -> bool {
        let turns = screen_turns();
        let Ok(mut s) = self.0.lock() else { return false };
        s.screen_turns = turns;
        s.stand()
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
                // Whatever it was for, the dialog it belonged to is down,
                // and the next [`Senses::work`] may raise the next one.
                // Android cancels a request made while another is up and
                // answers the cancelled one with nothing at all, so this —
                // and not the pass that made the wish — is what lets the
                // queue move.
                s.asking = None;
                let refusal = match r.status {
                    PermissionStatus::Granted => None,
                    // Both refusals read the same to a person: the platform
                    // stops asking after the second one either way, and
                    // what is left is the settings panel. Which of the two
                    // it is decides one thing only, and that is whether
                    // there is a second dialog left to raise.
                    PermissionStatus::DeniedCanRetry | PermissionStatus::DeniedPermanent => {
                        Some(())
                    }
                    // Not an answer at all, and nothing here is moved by
                    // it. No dialog sends one — both platforms ask instead
                    // — but a *check* does: it is what android says of a
                    // permission never asked about, and of one denied so
                    // firmly that it will not ask again. A refusal already
                    // written down must outlive it.
                    PermissionStatus::NotDetermined => return false,
                };
                // A device opened while the dialog was still up is a device
                // that hands over nothing: android answers a session it has
                // not allowed with black frames and silence, and it does not
                // start answering when the person says yes. So the grant
                // makes what is open *stale*, and the next pass closes it
                // and opens it again — which is the whole of why a first
                // recording used to be empty.
                let granted = matches!(r.status, PermissionStatus::Granted);
                match r.permission {
                    Permission::Location => {
                        s.location_trouble = refusal.map(|()| refused("your location"));
                    }
                    Permission::Camera => {
                        s.camera_trouble = refusal.map(|()| refused("the camera"));
                        if refusal.is_some() {
                            s.camera_held = 0;
                        }
                        s.camera_refused = refusal.is_some();
                        s.camera_stale |= granted && s.camera.is_some();
                    }
                    Permission::AudioInput => {
                        s.microphone_trouble = refusal.map(|()| refused("the microphone"));
                        if refusal.is_some() {
                            s.microphone_wanted = false;
                        }
                        s.microphone_stale |= granted && s.hearing;
                        // What a call reads before it starts, and what it
                        // keeps looking at while it carries no voice out.
                        s.microphone_answer = Some(granted);
                        // Android's first refusal is one it will still
                        // raise a dialog for. A permanent one is not, and
                        // neither is a grant.
                        s.microphone_retry =
                            matches!(r.status, PermissionStatus::DeniedCanRetry);
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
                s.wants(Ask::dialog(Permission::Location));
            }
            s.receiving = true;
            work.start_location = true;
        } else if s.receiver_held == 0 && s.receiving {
            s.receiving = false;
            work.stop_location = true;
        }

        // A call's microphone permission: asked for, and nothing opened.
        // The library's own device is what captures, so nothing else here
        // would ever raise the dialog. A call appearing may ask; a call
        // already carrying without a voice only looks.
        let asked = std::mem::take(&mut s.ask_microphone);
        let polled = std::mem::take(&mut s.poll_microphone);
        if asked || polled {
            s.want_microphone(asked);
        }

        // The camera. Watching wakes the platform's enumeration, which is
        // what fills `cameras`; the session starts once it has.
        if s.camera_held > 0 {
            if !s.watching {
                s.watching = true;
                work.watch = true;
            }
            if !s.asked_camera {
                s.asked_camera = true;
                s.wants(Ask::dialog(Permission::Camera));
            }
            // A session the permission arrived after is closed here and
            // opened again below, in that order: `perform` closes first.
            if std::mem::take(&mut s.camera_stale) && s.camera.is_some() {
                s.camera = None;
                s.chosen = None;
                s.frame = None;
                work.close_camera = true;
            }
            if s.camera.is_none() {
                if let Some(choice) = pick(&s.cameras) {
                    s.camera = Some(CameraId {
                        input: choice.input.0 .0,
                        format: choice.format.0 .0,
                        turns: upright_turns(choice.orientation, choice.front, s.screen_turns),
                        front: choice.front,
                    });
                    s.chosen = Some(choice);
                    work.open_camera = Some((choice.input, choice.format));
                }
            }
        } else if s.camera.is_some() {
            s.camera = None;
            s.chosen = None;
            s.frame = None;
            s.camera_stale = false;
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
                s.wants(Ask::dialog(Permission::AudioInput));
            }
            if std::mem::take(&mut s.microphone_stale) && s.hearing {
                s.hearing = false;
                work.close_microphone = true;
            }
            if !s.hearing && !s.inputs.is_empty() {
                s.hearing = true;
                work.open_microphone = s.inputs.clone();
            }
        } else if s.hearing {
            s.hearing = false;
            s.level = 0.0;
            s.microphone_stale = false;
            work.close_microphone = true;
        }

        // And one dialog, never two. A first video call wants the camera
        // and the microphone in the one pass, and android cancels the
        // second request as the first is still up — its result never
        // arrives, and whatever was waiting for it waits for ever. So the
        // rest stay in the queue and a landed result is what lets the next
        // one out. A check goes through the same queue, being answered by
        // the same event.
        if s.asking.is_none() {
            s.asking = s.queued.pop_front();
            if let Some(ask) = s.asking {
                if ask.dialog {
                    work.ask = Some(ask.permission);
                } else {
                    work.check = Some(ask.permission);
                }
            }
        }
        work
    }

    /// The `cx` calls the wishes came to, made with no lock held: a
    /// platform that answers a registration by calling straight back into a
    /// callback would otherwise meet its own lock.
    fn perform(&self, cx: &mut Cx, work: Work) {
        if let Some(permission) = work.ask {
            cx.request_permission(permission);
        }
        // The same answer, asked for without a dialog: the platform reads
        // what it has already been told and sends the result back.
        if let Some(permission) = work.check {
            cx.check_permission(permission);
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
        // Closed before opened, because one pass may be both: a session
        // started before its permission was answered is torn down and built
        // again here, and `Cx` acts on each call as it comes.
        if work.close_camera {
            cx.use_video_input(&[]);
        }
        if let Some((input, format)) = work.open_camera {
            // How the screen is turned, asked of the platform before the
            // first frame is asked for, so the picture stands up from the
            // very first one: nothing else reads it until the window's
            // geometry changes, and a phone opened on its side would
            // otherwise show a quarter turn of nonsense until it moved.
            // The call is made here because `perform` holds no lock.
            let turns = screen_turns();
            if let Ok(mut s) = self.0.lock() {
                s.screen_turns = turns;
                s.stand();
            }
            cx.use_video_input(&[(input, format)]);
        }
        if work.close_microphone {
            cx.use_audio_inputs(&[]);
        }
        if !work.open_microphone.is_empty() {
            cx.use_audio_inputs(&work.open_microphone);
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
        let (recording, tap, turns, mirror) = match self.0.lock() {
            Ok(s) => (
                s.circle.is_some(),
                s.tap.clone(),
                s.camera.map_or(0, |c| c.turns),
                s.camera.is_some_and(|c| c.front),
            ),
            Err(_) => return,
        };
        // The crop, the scale and the turn are the one expensive thing on
        // this thread, and they happen with no lock held: a draw asking for
        // the level must not wait behind a frame.
        let square = recording.then(|| {
            let square = square_nv12(scratch, CIRCLE_SIDE as usize);
            // Stood up before the encoder ever sees it: a video message is
            // a file other clients play, and a file carries no turn.
            upright_square(square, CIRCLE_SIDE as usize, turns, mirror)
        });
        {
            let Ok(mut s) = self.0.lock() else { return };
            if let (Some(square), Some(run)) = (square, s.circle.as_ref()) {
                offer(&run.pieces, Piece::Square(std::time::Instant::now(), square));
            }
            let kept = s.frame.get_or_insert_with(Frame::default);
            kept.width = scratch.width;
            kept.height = scratch.height;
            kept.turns = turns;
            kept.mirror = mirror;
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
                // Lying as the sensor made it, with the turn beside it: a
                // call sends the picture and the turn together and lets
                // the far side do the turning, which is cheaper than
                // turning thirty frames a second on this thread.
                turns,
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
    /// The one permission to raise a dialog for, where one is due — never
    /// two: the second dialog of a pair is cancelled on android and
    /// answered by nothing.
    ask: Option<Permission>,
    /// Or the one to look up, which raises nothing and answers with the
    /// same event: how a refusal undone in the platform's own settings is
    /// noticed. Never both — the queue lets one out at a time.
    check: Option<Permission>,
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

    /// What the receiver is refusing with, where it is refusing at all.
    ///
    /// Not [`want`](Location::want)'s answer: the platform's dialog is
    /// answered *after* the wish was made, so a panel that asked and was
    /// told nothing is told *no* a second later and has nowhere else to
    /// read it. The place panel asks on every draw.
    fn trouble(&self) -> Option<String> {
        self.0 .0.lock().ok()?.location_trouble.clone()
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
            let said = trouble.clone();
            // A refusal is remembered, and the settings panel may have
            // undone it since with nothing to say so. The wish that met it
            // still fails — the answer is this moment's — and the check it
            // sets going is what makes the next one succeed.
            if s.camera_refused {
                s.wants(Ask::check(Permission::Camera));
            }
            return Err(said);
        }
        s.camera_held += 1;
        Ok(())
    }

    fn camera(&self) -> Option<CameraId> {
        self.0 .0.lock().ok()?.camera
    }

    fn camera_open(&self) -> bool {
        self.0 .0.lock().is_ok_and(|s| s.camera.is_some())
    }

    /// One hold back. The session closes when the last one is given back:
    /// a panel that never opened the camera cannot close another's.
    fn close_camera(&mut self) {
        if let Ok(mut s) = self.0 .0.lock() {
            s.camera_held = s.camera_held.saturating_sub(1);
        }
    }

    fn take_photo(&mut self, dir: &Path) -> Result<Photo, String> {
        let s = self.state()?;
        let frame = s.frame.as_ref().ok_or_else(|| {
            if s.camera_held > 0 {
                "the camera has not sent a picture yet".to_string()
            } else {
                "the camera is not open".to_string()
            }
        })?;
        // Stood upright, and mirrored where the lens faces the person: a
        // front camera's shot is the preview they were looking at when they
        // pressed, which every phone client mirrors and nobody expects to
        // come back the other way round.
        let frame = upright(frame);
        let (width, height) = (frame.width, frame.height);
        // Nothing below needs the lock, and a photograph is milliseconds of
        // colour, scaling and encoding that a camera frame should not wait
        // behind.
        drop(s);
        let rgb = rgb_of(&frame);
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

    /// A wish for the dialog alone. The stage raises it on its next event
    /// and opens nothing: what wants the permission is a call, whose own
    /// library holds the device.
    fn ask_microphone(&mut self) {
        if let Ok(mut s) = self.0 .0.lock() {
            s.ask_microphone = true;
        }
    }

    /// The same wish with no dialog in it. What a call already carrying
    /// without a voice asks for, every few seconds: the person may have
    /// gone to the settings panel and allowed it, and neither platform
    /// says so.
    fn recheck_microphone(&mut self) {
        if let Ok(mut s) = self.0 .0.lock() {
            s.poll_microphone = true;
        }
    }

    /// What the platform said, once it has said anything — and *allowed*
    /// where nothing has been asked at all, which is a platform whose
    /// dialog this build never raises. Between the wish and the answer it
    /// is `None`, and a call started in that moment is a call started
    /// without a microphone: the wire's *ready* waits for nothing.
    fn microphone_allowed(&self) -> Option<bool> {
        let Ok(s) = self.0 .0.lock() else {
            // A poisoned lock is a broken run, not a refusal; a call that
            // waited on one would wait through its own ringing.
            return Some(true);
        };
        if let Some(answer) = s.microphone_answer {
            return Some(answer);
        }
        (!s.ask_microphone && !s.asked_microphone).then_some(true)
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
        if let Err(said) = opus_ogg::long_enough(note.secs) {
            let _ = std::fs::remove_file(&note.path);
            return Err(said);
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
///
/// Everything is timed off the frames' own moments rather than off a count:
/// where the first frame arrived is nought, and every frame after it is
/// stamped with how long after that it came. So the minute is a minute of
/// the clock, the length is how long the recording ran, and the picture
/// keeps step with the sound whatever rate the camera answers at.
fn write_circle(path: &Path, pieces: &Receiver<Piece>) -> Result<VideoNote, String> {
    let side = CIRCLE_SIDE;
    let mut encoder = circle::Encoder::create(path, side)?;
    let mut first: Option<Vec<u8>> = None;
    let mut started: Option<std::time::Instant> = None;
    let mut span = 0.0;
    let mut frames: u64 = 0;
    let mut full = false;
    let mut rate: Option<pcm::Resampler> = None;
    let mut pcm16: Vec<i16> = Vec::new();
    while let Ok(piece) = pieces.recv() {
        // Past the minute the recording is over: nothing more is written,
        // and the channel stays open so the panel's own stop is what ends it.
        if full {
            continue;
        }
        match piece {
            Piece::Square(at, nv12) => {
                let secs = at.saturating_duration_since(*started.get_or_insert(at)).as_secs_f64();
                if secs >= CIRCLE_MAX {
                    full = true;
                    continue;
                }
                if first.is_none() {
                    first = Some(nv12.clone());
                }
                encoder.push_frame(&nv12, secs)?;
                span = secs;
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
        secs: circle_secs(span, frames),
        side: CIRCLE_SIDE,
        thumbnail,
    })
}

/// How long a video message runs: from its first frame to its last, and one
/// frame's own moment on the screen on top of that — a frame is shown until
/// the next one, and the last one has no next.
///
/// A frame's worth is the average interval the frames actually arrived at,
/// so a camera answering twenty-four a second gives a length as true as one
/// answering thirty; where there is only one frame there is no interval to
/// average and the file's own rate stands in.
fn circle_secs(span: f64, frames: u64) -> f64 {
    let each = if frames > 1 {
        span / (frames - 1) as f64
    } else {
        1.0 / WANTED_FPS
    };
    span + each
}

// -- which way up --------------------------------------------------------------

/// How the screen is turned right now, in quarter turns anticlockwise from
/// the device's natural orientation.
///
/// The phone's own answer, and nought everywhere else: a Mac's window does
/// not turn, and neither does a headless run's.
#[cfg(target_os = "android")]
fn screen_turns() -> u8 {
    crate::platform::android::screen_turns()
}

#[cfg(not(target_os = "android"))]
fn screen_turns() -> u8 {
    0
}

/// The quarter turns clockwise a frame of a camera needs to stand upright
/// on the screen as it is held now — never assumed, always worked out from
/// what the device reports.
///
/// Three things go into it:
///
/// - `sensor_degrees` is how the sensor is *mounted*: the clockwise degrees
///   a frame needs to stand upright on the device's **natural** screen,
///   which is android's `SENSOR_ORIENTATION` and nought on a Mac. In
///   practice it is only ever a multiple of ninety, which is why the
///   quarter turns are simply `/ 90`.
/// - `front` is which way the lens faces.
/// - `screen_turns` is how the screen is turned *now*, in quarter turns
///   anticlockwise from natural — `Display.getRotation()`.
///
/// The rule is CameraX's `getRelativeImageRotation`, in quarter turns
/// rather than degrees: turning the screen turns the picture against the
/// sensor on a back camera and with it on a front one, because a front
/// camera's picture is already reversed left for right. So the screen's
/// turn is taken away from the sensor's for the back lens and added for the
/// front.
fn upright_turns(sensor_degrees: u32, front: bool, screen_turns: u8) -> u8 {
    let sensor = (sensor_degrees / 90) % 4;
    let screen = u32::from(screen_turns) % 4;
    let turns = if front {
        sensor + screen
    } else {
        sensor + 4 - screen
    };
    (turns % 4) as u8
}

// -- pixels --------------------------------------------------------------------

/// Which cameras the platform offers, with a format picked on each: the
/// best YUV one nearest thirty frames a second and under 720p, because a
/// video message is 384 pixels square at thirty frames and a photograph is
/// capped at 1280 — asking a sensor for its largest mode would cost frames
/// nothing draws.
///
/// The rate is ranked above the size because it is the one thing a file
/// cannot be scaled to afterwards: a sixty-frame mode is twice the work for
/// a picture nobody sees twice, and a fifteen-frame one is a video message
/// that stutters. A format that does not say its rate is taken at its word
/// as the one wanted, there being nothing better to go on.
fn choices(inputs: &VideoInputsEvent) -> Vec<Choice> {
    let mut out = Vec::new();
    for desc in &inputs.descs {
        let mut best: Option<(i32, std::cmp::Reverse<i64>, usize)> = None;
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
            let score = (rank, std::cmp::Reverse(off_wanted(f.frame_rate)), f.width * f.height);
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
                orientation: desc.sensor_orientation,
            });
        }
    }
    out
}

/// How far a format's rate is from the thirty a video message is written at,
/// in hundredths of a frame — whole numbers, so formats can be ordered by
/// it, and nought for a format that does not say.
fn off_wanted(rate: Option<f64>) -> i64 {
    ((rate.unwrap_or(WANTED_FPS) - WANTED_FPS).abs() * 100.0).round() as i64
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

/// One plane of `px`-byte samples, turned `turns` quarters clockwise.
///
/// A luma plane is one byte a sample and an I420 chroma plane is too; an
/// NV12 chroma plane is two, the U and the V of one sample side by side,
/// and turning it means moving the pair rather than the bytes. An odd turn
/// stands the plane on its side, so what comes back is `h` wide and `w`
/// tall; an even one is the same shape it went in.
///
/// Plainly written on purpose: it runs on the capture thread beside the
/// crop, and a clever index is a picture nobody can read when it is wrong.
fn turn_plane(src: &[u8], w: usize, h: usize, px: usize, turns: u8) -> Vec<u8> {
    let mut out = vec![0u8; w * h * px];
    let turns = turns % 4;
    if turns == 0 {
        let same = src.len().min(out.len());
        out[..same].copy_from_slice(&src[..same]);
        return out;
    }
    // The destination is as wide as the turned plane: the other side's
    // height on an odd turn, its own width on a half one.
    let across = if turns == 2 { w } else { h };
    for y in 0..h {
        for x in 0..w {
            // One quarter clockwise puts the source's first row down the
            // destination's last column; a half turns it end for end; three
            // quarters puts the first row up the first column.
            let (dx, dy) = match turns {
                1 => (h - 1 - y, x),
                2 => (w - 1 - x, h - 1 - y),
                _ => (y, w - 1 - x),
            };
            let from = (y * w + x) * px;
            let into = (dy * across + dx) * px;
            let Some(sample) = src.get(from..from + px) else {
                continue;
            };
            out[into..into + px].copy_from_slice(sample);
        }
    }
    out
}

/// One plane of `px`-byte samples, left for right: what makes a front
/// camera's picture the one the person was looking at.
fn mirror_plane(plane: &mut [u8], w: usize, h: usize, px: usize) {
    for y in 0..h {
        for x in 0..w / 2 {
            let left = (y * w + x) * px;
            let right = (y * w + (w - 1 - x)) * px;
            if right + px > plane.len() {
                continue;
            }
            for b in 0..px {
                plane.swap(left + b, right + b);
            }
        }
    }
}

/// The kept frame stood upright: the three I420 planes turned by its own
/// [`turns`](Frame::turns) and mirrored where it came from the front lens.
///
/// An odd turn swaps the frame's sides, and the chroma planes swap with it
/// — a plane half as wide and half as tall turned a quarter is half as tall
/// and half as wide, which is exactly the chroma the turned luma wants.
fn upright(frame: &Frame) -> Frame {
    let (w, h) = (frame.width, frame.height);
    let (cw, ch) = (w.div_ceil(2), h.div_ceil(2));
    let odd = frame.turns % 2 == 1;
    let mut made = Frame {
        width: if odd { h } else { w },
        height: if odd { w } else { h },
        turns: 0,
        mirror: false,
        y: turn_plane(&frame.y, w, h, 1, frame.turns),
        u: turn_plane(&frame.u, cw, ch, 1, frame.turns),
        v: turn_plane(&frame.v, cw, ch, 1, frame.turns),
    };
    if frame.mirror {
        let (mw, mh) = (made.width, made.height);
        let (mcw, mch) = (if odd { ch } else { cw }, if odd { cw } else { ch });
        mirror_plane(&mut made.y, mw, mh, 1);
        mirror_plane(&mut made.u, mcw, mch, 1);
        mirror_plane(&mut made.v, mcw, mch, 1);
    }
    made
}

/// A square NV12 frame stood upright the same way — what a video message is
/// written from, which has to leave here the right way up because an mp4
/// carries no turn a chat row could read.
///
/// The square stays a square whatever the turn, so only the pixels move:
/// the luma at one byte a sample, and the interleaved chroma at two, half
/// the side each way.
fn upright_square(square: Vec<u8>, side: usize, turns: u8, mirror: bool) -> Vec<u8> {
    if turns.is_multiple_of(4) && !mirror {
        return square;
    }
    let half = side / 2;
    if square.len() < side * side + half * half * 2 {
        return square;
    }
    let (luma, chroma) = square.split_at(side * side);
    let mut y = turn_plane(luma, side, side, 1, turns);
    let mut uv = turn_plane(chroma, half, half, 2, turns);
    if mirror {
        mirror_plane(&mut y, side, side, 1);
        mirror_plane(&mut uv, half, half, 2);
    }
    y.extend_from_slice(&uv);
    y
}

#[cfg(test)]
mod tests;
