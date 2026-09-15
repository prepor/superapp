//! What a call's media runs on, and the seam the worker drives it through.
//!
//! TDLib does the signalling — it creates the call, rings the other side,
//! hands over the key, the servers and the four emoji — and something else
//! carries the voice and the picture. That something is
//! [NTgCalls](https://github.com/pytgcalls/ntgcalls), a C library over
//! libwebrtc that speaks tgcalls' protocol, behind the `calls` feature on
//! macOS and android (`cfg(calls_engine)`, which `build.rs` sets for the two
//! together); everywhere else, and in every test, scene and scripted run, it
//! is [`FakeEngine`], which connects two seconds after it is asked to and
//! remembers what it was told.
//!
//! Nothing here touches the store. An engine is given a sender and speaks
//! [`Told`] into it; the worker's pass drains that channel and is the only
//! thing that writes. The pictures go the other way round — an engine drops
//! the newest frame of each side into [`frames`], a slot the panel reads on
//! its draw — because a video call makes thirty of them a second and not one
//! is worth a pass of its own.
//!
//! Most of this is the real engine's, so a build without it — a build
//! without the feature, a platform with no library for it — holds the shapes
//! and uses none of them.
#![cfg_attr(not(calls_engine), allow(dead_code))]

use std::sync::{Mutex, OnceLock};

use kernel::caps::FrameTap;
use tokio::sync::mpsc::UnboundedSender;

#[cfg(calls_engine)]
mod ntg;

pub mod sounds;

/// What a client says it speaks, and what `createCall` and `acceptCall`
/// carry.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Protocol {
    pub min_layer: i32,
    pub max_layer: i32,
    pub udp_p2p: bool,
    pub udp_reflector: bool,
    pub library_versions: Vec<String>,
}

/// The lowest and highest tgcalls layer a client may offer — the reference
/// clients' own numbers, outside which the wire refuses the call.
pub const MIN_LAYER: i32 = 65;
pub const MAX_LAYER: i32 = 92;

/// What the fake says it speaks. The real engine answers for itself
/// (`ntg_get_protocol`), which is the library versions it was built with;
/// this is one plausible list, and no call is ever made on it.
#[must_use]
fn fake_protocol() -> Protocol {
    Protocol {
        min_layer: MIN_LAYER,
        max_layer: MAX_LAYER,
        udp_p2p: true,
        udp_reflector: true,
        library_versions: vec!["11.0.0".to_string(), "10.0.0".to_string(), "2.7.7".to_string()],
    }
}

/// What this build speaks. A free function rather than a method on the
/// engine, because `createCall` goes out from a panel's verb, which has no
/// engine to ask — the library answers this one without an instance.
#[must_use]
pub fn protocol() -> Protocol {
    #[cfg(calls_engine)]
    {
        ntg::protocol().unwrap_or_else(|_| fake_protocol())
    }
    #[cfg(not(calls_engine))]
    {
        fake_protocol()
    }
}

/// One of the wire's `callServer`s in the shape the engine wants it: a
/// reflector carries a peer tag, a WebRTC server a username and a password.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Server {
    pub id: u64,
    pub ipv4: String,
    pub ipv6: String,
    pub port: u16,
    pub username: String,
    pub password: String,
    pub turn: bool,
    pub stun: bool,
    pub tcp: bool,
    pub peer_tag: Vec<u8>,
}

/// Everything `callStateReady` hands over, which is everything the engine
/// needs to reach the other side.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Ready {
    /// The person. The engine names a conversation by whom it is with.
    pub user: i64,
    pub outgoing: bool,
    /// Whether my camera goes out with my voice.
    ///
    /// The wire says which kind of call was placed; what actually goes out
    /// is the *row's* camera, which the bar may have turned off while the
    /// *ready* waited on the microphone's permission. The worker writes the
    /// row's answer here as the call begins.
    pub video: bool,
    /// Whether the call starts muted — the row's again, and for the same
    /// reason: *mute* pressed while a call is parked would otherwise start
    /// a call that carries the voice the row says is off.
    pub muted: bool,
    /// Whether the capture carries a microphone at all.
    ///
    /// False where the permission was refused, or where the wait for the
    /// dialog ran out with nobody answering. The engine opens the device
    /// itself and NTgCalls throws where it cannot, so naming one that may
    /// not open is a call discarded rather than a call with no voice going
    /// out. A permission granted later turns it on
    /// ([`CallEngine::microphone`]).
    pub microphone: bool,
    /// The shared secret, already out of its base64.
    pub key: Vec<u8>,
    pub servers: Vec<Server>,
    pub versions: Vec<String>,
    pub allow_p2p: bool,
    pub custom_parameters: Option<String>,
}

/// Where the media's connection stands, in the engine's words.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Link {
    /// Reaching the other side. After it had once connected, this is what
    /// *reconnecting* is: no engine says that word, the row remembers.
    Connecting,
    Connected,
    Failed(String),
}

/// What an engine says back. The worker's pass drains these; nothing else
/// reads them, and no engine thread ever writes a row itself.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Told {
    /// Bytes for `sendCallSignalingData`.
    Signalling { user: i64, data: Vec<u8> },
    Link { user: i64, link: Link },
}

/// One side's newest picture, in what a texture takes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Picture {
    pub width: usize,
    pub height: usize,
    /// BGRA, one word a pixel, `width * height` long.
    pub pixels: Vec<u32>,
    /// Bumped for every frame, so a draw can tell a new picture from the one
    /// it already uploaded without comparing a megabyte.
    pub stamp: u64,
}

/// The newest frame of each side. One slot for the process rather than a
/// thing hung off a world: frames arrive on a library's own thread, which
/// holds no world and has no `Cx` to reach one through, and one machine
/// carries one call at a time.
#[derive(Debug, Default)]
pub struct Frames {
    pub remote: Option<Picture>,
    pub local: Option<Picture>,
}

/// The slot itself.
#[must_use]
pub fn frames() -> &'static Mutex<Frames> {
    static FRAMES: OnceLock<Mutex<Frames>> = OnceLock::new();
    FRAMES.get_or_init(|| Mutex::new(Frames::default()))
}

impl Frames {
    /// Takes a frame as the newest of its side.
    pub fn put(&mut self, remote: bool, width: usize, height: usize, pixels: Vec<u32>) {
        let slot = if remote { &mut self.remote } else { &mut self.local };
        let stamp = slot.as_ref().map_or(0, |p| p.stamp) + 1;
        *slot = Some(Picture { width, height, pixels, stamp });
    }

    /// Forgets both.
    pub fn forget(&mut self) {
        self.remote = None;
        self.local = None;
    }
}

/// Forgets both pictures — what the end of a call does, so the next one does
/// not open on the last one's face.
pub fn forget_frames() {
    frames().lock().expect("call frames").forget();
}

/// Hands a frame over as the newest of its side.
pub fn put_frame(remote: bool, width: usize, height: usize, pixels: Vec<u32>) {
    frames().lock().expect("call frames").put(remote, width, height, pixels);
}

/// What carries a call's voice and picture.
///
/// Every method answers at once and does its work elsewhere: the real engine
/// queues it onto the shared runtime, the fake keeps a note. Nothing here may
/// hold up the worker's pass, and nothing here may touch the store.
pub trait CallEngine: Send {
    /// Reach this person: the key, the servers, and whether the camera goes
    /// with the voice.
    fn start(&self, ready: Ready);
    /// A packet the wire relayed from the other side.
    fn signalling(&self, user: i64, data: Vec<u8>);
    /// The microphone, off and on.
    fn mute(&self, user: i64, on: bool);
    /// The camera, off and on, mid-call.
    fn camera(&self, user: i64, on: bool);
    /// The microphone's *device*, off and on, mid-call — which is not
    /// muting: a call started under a refused or unanswered permission
    /// carries no microphone at all, and this is what a permission granted
    /// while it runs turns on. A call already muted stays muted.
    fn microphone(&self, user: i64, on: bool);
    /// Let it go. Every way a call ends comes through here.
    fn stop(&self, user: i64);
    /// The world's clock, once a pass. The real engine has one of its own and
    /// does nothing here; the fake connects two seconds after it was started,
    /// and those are the world's seconds, so a suite under a virtual clock
    /// walks the states a real call walks.
    fn tick(&self, _now: f64) {}
    /// What the engine was told, for a test to read back. The real one keeps
    /// nothing and answers empty.
    #[cfg(test)]
    fn heard(&self) -> Vec<Doing> {
        Vec::new()
    }

    /// Where the camera's frames go while this engine is sending a picture.
    ///
    /// `None` — the fake, and the Mac's, which opens a capture session of its
    /// own — says it wants none. The phone's says [`Some`]: its library has
    /// no camera without a JavaVM, so makepad holds the camera and every
    /// frame is pushed in as an external one. The worker leaves whatever
    /// this answers on the [`Capture`](kernel::caps::Capture) capability
    /// while a video call runs, and takes it off when the call ends.
    fn frames_wanted(&self) -> Option<FrameTap> {
        None
    }
}

/// One thing an engine was told to do, as the fake remembers it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Doing {
    /// Everything the call was started with, the row's `muted`, `camera` and
    /// microphone included — which is what a test reads to see that a choice
    /// made while the call was parked was carried into it.
    Start(Box<Ready>),
    Signalling(i64, Vec<u8>),
    Mute(i64, bool),
    Camera(i64, bool),
    Microphone(i64, bool),
    Stop(i64),
}

/// How long the fake takes to connect, in the world's seconds.
pub const FAKE_CONNECTS_AFTER: f64 = 2.0;

/// The engine that carries nothing. It answers the way a real one answers —
/// *connecting* at once, *connected* two seconds later — and keeps every
/// instruction, so a test can assert on what a call told it and a scene can
/// draw a call that never leaves the machine.
pub struct FakeEngine {
    out: UnboundedSender<Told>,
    heard: Mutex<Vec<Doing>>,
    /// The calls started but not yet connected, and when each began — `None`
    /// until the first tick, which is the clock this side has.
    waiting: Mutex<Vec<(i64, Option<f64>)>>,
}

impl FakeEngine {
    #[must_use]
    pub fn new(out: UnboundedSender<Told>) -> FakeEngine {
        FakeEngine { out, heard: Mutex::new(Vec::new()), waiting: Mutex::new(Vec::new()) }
    }

    fn note(&self, doing: Doing) {
        self.heard.lock().expect("fake engine").push(doing);
    }
}

impl CallEngine for FakeEngine {
    fn start(&self, ready: Ready) {
        let user = ready.user;
        self.note(Doing::Start(Box::new(ready)));
        self.waiting.lock().expect("fake engine").push((user, None));
        let _ = self.out.send(Told::Link { user, link: Link::Connecting });
    }

    fn signalling(&self, user: i64, data: Vec<u8>) {
        self.note(Doing::Signalling(user, data));
    }

    fn mute(&self, user: i64, on: bool) {
        self.note(Doing::Mute(user, on));
    }

    fn camera(&self, user: i64, on: bool) {
        self.note(Doing::Camera(user, on));
    }

    fn microphone(&self, user: i64, on: bool) {
        self.note(Doing::Microphone(user, on));
    }

    fn stop(&self, user: i64) {
        self.note(Doing::Stop(user));
        self.waiting.lock().expect("fake engine").retain(|(u, _)| *u != user);
    }

    /// The first tick after a start is what the two seconds are counted from:
    /// this engine has no clock, and the pass that started the call is the
    /// first to tell it the time.
    fn tick(&self, now: f64) {
        let mut connected = Vec::new();
        {
            let mut waiting = self.waiting.lock().expect("fake engine");
            waiting.retain_mut(|(user, since)| {
                let since = *since.get_or_insert(now);
                if now - since < FAKE_CONNECTS_AFTER {
                    return true;
                }
                connected.push(*user);
                false
            });
        }
        for user in connected {
            let _ = self.out.send(Told::Link { user, link: Link::Connected });
        }
    }

    #[cfg(test)]
    fn heard(&self) -> Vec<Doing> {
        self.heard.lock().expect("fake engine").clone()
    }
}

/// The engine for an account worker.
///
/// It follows the transport, as everything else about a real account does:
/// the one live client of a build that linked NTgCalls gets the engine, and
/// every other account — a fake transport in a test, a build without the
/// feature — gets the one that carries nothing.
#[must_use]
pub fn engine(out: UnboundedSender<Told>, real: bool) -> Box<dyn CallEngine> {
    #[cfg(calls_engine)]
    if real {
        return Box::new(ntg::NtgEngine::new(out));
    }
    let _ = real;
    Box::new(FakeEngine::new(out))
}

/// Whether this build can carry a call at all. Without it the panel still
/// rings and can still decline; it simply cannot answer.
#[must_use]
pub fn available() -> bool {
    cfg!(calls_engine)
}

/// Whether the camera is *ours* to hold while a video call runs.
///
/// The Mac's library opens a capture session of its own and reports back the
/// frames it is sending, so one session serves both the call and the
/// preview. The phone's has no camera at all — its own is JNI to Java
/// classes this build does not carry — so makepad holds the camera, every
/// frame is pushed in as an external one, and the preview is that same
/// session drawn.
#[must_use]
pub fn camera_is_ours() -> bool {
    cfg!(all(calls_engine, target_os = "android"))
}

/// What a build with no engine says when asked to make or take one.
pub const NO_ENGINE: &str = "calls are not available on this device yet";

/// A frame's I420 planes as BGRA words, one to a pixel.
///
/// The conversion happens on whichever thread the frame arrived on — never on
/// a pass, never on a draw — because it is a megabyte of arithmetic thirty
/// times a second and neither of those can afford it. `data` is the three
/// planes end to end, as libwebrtc hands them over: `w × h` of luma, then two
/// half-sized chroma planes. `None` where the bytes are too few to be that.
#[must_use]
pub fn i420_to_bgra(data: &[u8], width: usize, height: usize) -> Option<Vec<u32>> {
    if width == 0 || height == 0 {
        return None;
    }
    let (cw, ch) = (width.div_ceil(2), height.div_ceil(2));
    let (y_len, c_len) = (width.checked_mul(height)?, cw.checked_mul(ch)?);
    if data.len() < y_len + 2 * c_len {
        return None;
    }
    let (y, rest) = data.split_at(y_len);
    let (u, v) = rest.split_at(c_len);
    let mut out = vec![0u32; y_len];
    for row in 0..height {
        for col in 0..width {
            let c = i32::from(y[row * width + col]) - 16;
            let i = (row / 2) * cw + col / 2;
            let d = i32::from(u[i]) - 128;
            let e = i32::from(v[i]) - 128;
            let r = (298 * c + 409 * e + 128) >> 8;
            let g = (298 * c - 100 * d - 208 * e + 128) >> 8;
            let b = (298 * c + 516 * d + 128) >> 8;
            let byte = |v: i32| u32::try_from(v.clamp(0, 255)).unwrap_or(0);
            out[row * width + col] = 0xff00_0000 | (byte(r) << 16) | (byte(g) << 8) | byte(b);
        }
    }
    Some(out)
}

#[cfg(test)]
mod tests;
