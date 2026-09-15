//! NTgCalls, the engine that actually carries a call — on the Mac and on
//! the phone, which are the two platforms the library is built for here.
//!
//! The library is asynchronous and keeps threads of its own, so the whole of
//! it lives in one task: the worker's pass hands it an instruction down a
//! channel and returns, and the library's callbacks — which arrive on
//! libwebrtc's threads — put a frame in the shared slot or a word in the
//! worker's channel. Neither side ever waits for the other, and neither
//! touches the store.
//!
//! The task is a *local* one ([`kernel::runtime::spawn_local`]) because the
//! library's handle may be sent to a thread but not shared between two, and
//! a task on the shared pool moves between threads at every await.
//!
//! This file is compiled only where the library is linked. What it does is
//! the five steps every tgcalls client takes: make the call, skip the key
//! exchange (TDLib did it), name the devices, connect to the servers the
//! wire gave, and relay the signalling both ways.
//!
//! The one difference between the two platforms is the camera. The Mac's
//! library opens an `AVCaptureSession` itself; the phone's, built without a
//! JavaVM, has no camera of its own at all, so the camera is *ours* there
//! ([`camera_is_ours`](super::camera_is_ours)): the description says
//! external, makepad holds the session, and every frame it makes is pushed
//! in through `send_external_frame`. The microphone and the speaker are the
//! library's own devices on both — WebRTC's audio module, which on the phone
//! is the native Oboe path the build links.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use kernel::caps::{CameraFrame, FrameTap};
use ntgcalls::{
    AudioDescription, ConnectionState, DeviceInfo, FrameData, MediaDescription, MediaDevices,
    MediaSource, NTgCalls, RTCServer, StreamDevice, StreamMode, VideoDescription, VideoRotation,
};
use tokio::sync::mpsc::{self, UnboundedSender};

use super::{CallEngine, Link, Protocol, Ready, Told};

/// What the camera is told it makes, where it is ours to hold. The real
/// size of each frame goes over with the frame itself, and is what the
/// library reads; this is what the description has to say to be a valid one.
const CAMERA: (i16, i16, u8) = (1280, 720, 30);

/// What the library says it speaks.
pub fn protocol() -> Result<Protocol, String> {
    let p = NTgCalls::get_protocol().map_err(|e| e.to_string())?;
    Ok(Protocol {
        min_layer: p.min_layer,
        max_layer: p.max_layer,
        udp_p2p: p.udp_p2p,
        udp_reflector: p.udp_reflector,
        library_versions: p.library_versions,
    })
}

/// One instruction for the task that owns the library.
enum Cmd {
    Start(Box<Ready>),
    Signalling(i64, Vec<u8>),
    Mute(i64, bool),
    Camera(i64, bool),
    /// A camera frame is waiting in [`Waiting`]. The frame does not travel
    /// down the channel itself: a camera makes thirty a second and the task
    /// is allowed to be behind, and a queue of whole pictures is how a phone
    /// runs out of memory.
    Frame,
    Stop(i64),
}

/// The newest camera frame and nothing older: one slot, overwritten.
///
/// The capture thread writes and the task takes; a take that finds nothing
/// is a frame the next one already took, which is exactly the frame that
/// should be dropped.
#[derive(Default)]
struct Waiting(Mutex<Option<(u16, u16, Vec<u8>)>>);

/// The handle the worker holds.
pub struct NtgEngine {
    cmd: UnboundedSender<Cmd>,
    waiting: Arc<Waiting>,
}

impl NtgEngine {
    pub fn new(out: UnboundedSender<Told>) -> NtgEngine {
        let (cmd, rx) = mpsc::unbounded_channel();
        let waiting = Arc::new(Waiting::default());
        let theirs = waiting.clone();
        drop(kernel::runtime::spawn_local(move || run(out, rx, theirs)));
        NtgEngine { cmd, waiting }
    }
}

/// The task that owns the library: its callbacks set up once, then one
/// instruction at a time until the worker is gone.
async fn run(
    out: UnboundedSender<Told>,
    mut rx: mpsc::UnboundedReceiver<Cmd>,
    waiting: Arc<Waiting>,
) {
    let mut calls = NTgCalls::new();
    let signalling = out.clone();
    calls.on_signaling_data(move |user, data| {
        let _ = signalling.send(Told::Signalling { user, data });
    });
    let link = out.clone();
    calls.on_connection_change(move |user, info| {
        let said = match info.state {
            // Connecting after it had once connected is *reconnecting*; the
            // row remembers that, not the engine.
            ConnectionState::Connecting => Link::Connecting,
            ConnectionState::Connected => Link::Connected,
            ConnectionState::Failed => Link::Failed("the connection failed".into()),
            ConnectionState::Timeout => Link::Failed("the connection timed out".into()),
            // Closed is what a discard looks like from below, and the wire
            // said so itself a moment before.
            ConnectionState::Closed => return,
        };
        let _ = link.send(Told::Link { user, link: said });
    });
    calls.on_frames(|_user, mode, device, frames| {
        // Only the pictures. A microphone's frames are the library's own
        // business and never reach a panel.
        if !matches!(device, StreamDevice::Camera | StreamDevice::Screen) {
            return;
        }
        // The newest frame is the only one worth converting: a draw shows one
        // picture and the rest are already stale.
        let Some(frame) = frames.last() else { return };
        let (w, h) = (frame.frame_data.width as usize, frame.frame_data.height as usize);
        let Some(pixels) = super::i420_to_bgra(&frame.data, w, h) else { return };
        super::put_frame(mode == StreamMode::Playback, w, h, pixels);
    });

    // What each live call was started with, so the camera can be turned on
    // and off without asking the wire again.
    let mut live: HashMap<i64, Ready> = HashMap::new();
    while let Some(cmd) = rx.recv().await {
        match cmd {
            Cmd::Start(ready) => {
                let user = ready.user;
                if let Err(error) = start(&calls, &ready).await {
                    let _ = out.send(Told::Link { user, link: Link::Failed(error) });
                    continue;
                }
                live.insert(user, *ready);
            }
            Cmd::Signalling(user, data) => {
                let _ = calls.send_signaling_data(user, &data).await;
            }
            Cmd::Mute(user, on) => {
                let _ = if on { calls.mute(user).await } else { calls.unmute(user).await };
            }
            Cmd::Camera(user, on) => {
                if let Some(ready) = live.get_mut(&user) {
                    ready.video = on;
                    let _ = calls.set_stream_sources(user, StreamMode::Capture, &capture(on)).await;
                    // A call that began without a picture has no playback
                    // camera either, so the other side's frames would have
                    // nowhere to arrive: turning ours on opens theirs too.
                    // Turning it off does *not* take it away again — whether
                    // they are sending a picture is theirs to decide, and a
                    // client that went blind by closing its own camera would
                    // be a worse bug than the one this fixes.
                    if on {
                        let _ = calls
                            .set_stream_sources(user, StreamMode::Playback, &playback(true))
                            .await;
                    }
                }
            }
            // Whatever the camera made last, to whichever call wants a
            // picture. One machine carries one call, so there is never a
            // second to tell it from.
            Cmd::Frame => {
                let Some((width, height, data)) =
                    waiting.0.lock().expect("the waiting frame").take()
                else {
                    continue;
                };
                let Some(user) = live.iter().find(|(_, r)| r.video).map(|(user, _)| *user) else {
                    continue;
                };
                let frame = FrameData {
                    absolute_capture_timestamp_ms: now_ms(),
                    rotation: VideoRotation::VideoRotation0,
                    width,
                    height,
                };
                let _ = calls
                    .send_external_frame(user, StreamDevice::Camera, &data, &frame)
                    .await;
            }
            Cmd::Stop(user) => {
                live.remove(&user);
                let _ = calls.stop(user).await;
                super::forget_frames();
            }
        }
    }
}

/// The five steps, in the order every tgcalls client takes them.
async fn start(calls: &NTgCalls, ready: &Ready) -> Result<(), String> {
    let user = ready.user;
    calls.create_p2p_call(user).await.map_err(|e| e.to_string())?;
    // TDLib did the Diffie-Hellman and handed over the shared key, so the
    // engine is told the answer rather than asked to work it out.
    calls
        .skip_exchange(user, &ready.key, ready.outgoing)
        .await
        .map_err(|e| e.to_string())?;
    calls
        .set_stream_sources(user, StreamMode::Capture, &capture(ready.video))
        .await
        .map_err(|e| e.to_string())?;
    calls
        .set_stream_sources(user, StreamMode::Playback, &playback(ready.video))
        .await
        .map_err(|e| e.to_string())?;
    let servers: Vec<RTCServer> = ready
        .servers
        .iter()
        .map(|s| RTCServer {
            id: s.id,
            ipv4: s.ipv4.clone(),
            ipv6: s.ipv6.clone(),
            port: s.port,
            username: s.username.clone(),
            password: s.password.clone(),
            turn: s.turn,
            stun: s.stun,
            tcp: s.tcp,
            peer_tag: s.peer_tag.clone(),
        })
        .collect();
    calls
        .connect_p2p(
            user,
            &servers,
            &ready.versions,
            ready.allow_p2p,
            ready.custom_parameters.as_deref(),
        )
        .await
        .map_err(|e| e.to_string())
}

/// The first device of a kind the library knows of, named the way it names
/// them; empty where there is none, which is what the library reads as *the
/// default one*.
fn device(of: fn(&MediaDevices) -> &Vec<DeviceInfo>) -> String {
    NTgCalls::get_media_devices()
        .ok()
        .and_then(|d| of(&d).first().map(|i| i.metadata.clone()))
        .unwrap_or_default()
}

/// What goes out: the system's own microphone, and the camera when the call
/// has one.
///
/// The devices are the library's, not makepad's: WebRTC's audio module comes
/// with the echo cancellation a speakerphone needs, and its capture is the
/// one session on the camera — a second one of ours beside it is what
/// AVFoundation refuses.
fn capture(video: bool) -> MediaDescription {
    MediaDescription {
        microphone: Some(AudioDescription {
            media_source: MediaSource::Device,
            sample_rate: 48_000,
            channel_count: 1,
            input: device(|d| &d.microphone),
            keep_open: false,
        }),
        speaker: None,
        camera: video.then(|| VideoDescription {
            media_source: if super::camera_is_ours() {
                MediaSource::External
            } else {
                MediaSource::Device
            },
            width: CAMERA.0,
            height: CAMERA.1,
            fps: CAMERA.2,
            // An external source is named by nothing: the frames arrive
            // rather than being read from somewhere.
            input: if super::camera_is_ours() { String::new() } else { device(|d| &d.camera) },
            keep_open: false,
        }),
        screen: None,
    }
}

/// Where the other side comes out: their voice through the system's own
/// speaker, and — in a video call — their picture through us.
///
/// The camera here is *theirs*, not ours. The library hands a received video
/// track to `on_frames` only while the playback description has a camera in
/// it (`StreamManager` keeps the device in its external writers, and nothing
/// else is what the frame callback is gated on), and only an *external*
/// source is allowed on that side — an internal one is *invalid input mode*.
/// Without it the far side's picture never arrives and the panel draws an
/// empty box, which is what this used to do.
///
/// The speaker is the library's own device on both platforms, as the
/// microphone is: `set_stream_sources` configures `desc.speaker` as the
/// Speaker device in every mode, and a non-external audio output is exactly
/// what it builds a writer for.
fn playback(video: bool) -> MediaDescription {
    MediaDescription {
        microphone: None,
        speaker: Some(AudioDescription {
            media_source: MediaSource::Device,
            sample_rate: 48_000,
            channel_count: 2,
            input: device(|d| &d.speaker),
            keep_open: false,
        }),
        camera: video.then(|| VideoDescription {
            media_source: MediaSource::External,
            width: CAMERA.0,
            height: CAMERA.1,
            fps: CAMERA.2,
            // Their frames arrive; nothing is read from anywhere.
            input: String::new(),
            keep_open: false,
        }),
        screen: None,
    }
}

impl CallEngine for NtgEngine {
    fn start(&self, ready: Ready) {
        let _ = self.cmd.send(Cmd::Start(Box::new(ready)));
    }

    fn signalling(&self, user: i64, data: Vec<u8>) {
        let _ = self.cmd.send(Cmd::Signalling(user, data));
    }

    fn mute(&self, user: i64, on: bool) {
        let _ = self.cmd.send(Cmd::Mute(user, on));
    }

    fn camera(&self, user: i64, on: bool) {
        let _ = self.cmd.send(Cmd::Camera(user, on));
    }

    fn stop(&self, user: i64) {
        let _ = self.cmd.send(Cmd::Stop(user));
    }

    /// The tap, where the camera is ours to hold. The copy into one buffer
    /// happens here, on the capture thread, because by the time the task
    /// gets to it the planes it was made of are another frame's.
    fn frames_wanted(&self) -> Option<FrameTap> {
        if !super::camera_is_ours() {
            return None;
        }
        let cmd = self.cmd.clone();
        let waiting = self.waiting.clone();
        Some(Arc::new(move |frame: CameraFrame<'_>| {
            let (Ok(width), Ok(height)) = (u16::try_from(frame.width), u16::try_from(frame.height))
            else {
                return;
            };
            let mut data = Vec::with_capacity(frame.y.len() + frame.u.len() + frame.v.len());
            data.extend_from_slice(frame.y);
            data.extend_from_slice(frame.u);
            data.extend_from_slice(frame.v);
            if let Ok(mut slot) = waiting.0.lock() {
                *slot = Some((width, height, data));
            }
            let _ = cmd.send(Cmd::Frame);
        }))
    }
}

/// The wall clock in milliseconds, which is what a frame is stamped with.
fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| i64::try_from(d.as_millis()).unwrap_or(0))
}
