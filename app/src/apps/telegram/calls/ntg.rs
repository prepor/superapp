//! NTgCalls, the engine that actually carries a call on this Mac.
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
//! This file is compiled only where the archive is linked. What it does is
//! the five steps every tgcalls client takes: make the call, skip the key
//! exchange (TDLib did it), name the devices, connect to the servers the
//! wire gave, and relay the signalling both ways.

use std::collections::HashMap;

use ntgcalls::{
    AudioDescription, ConnectionState, DeviceInfo, MediaDescription, MediaDevices, MediaSource,
    NTgCalls, RTCServer, StreamDevice, StreamMode, VideoDescription,
};
use tokio::sync::mpsc::{self, UnboundedSender};

use super::{CallEngine, Link, Protocol, Ready, Told};

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
    Stop(i64),
}

/// The handle the worker holds.
pub struct NtgEngine {
    cmd: UnboundedSender<Cmd>,
}

impl NtgEngine {
    pub fn new(out: UnboundedSender<Told>) -> NtgEngine {
        let (cmd, rx) = mpsc::unbounded_channel();
        drop(kernel::runtime::spawn_local(move || run(out, rx)));
        NtgEngine { cmd }
    }
}

/// The task that owns the library: its callbacks set up once, then one
/// instruction at a time until the worker is gone.
async fn run(out: UnboundedSender<Told>, mut rx: mpsc::UnboundedReceiver<Cmd>) {
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
                }
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
        .set_stream_sources(user, StreamMode::Playback, &playback())
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
            media_source: MediaSource::Device,
            width: 1280,
            height: 720,
            fps: 30,
            input: device(|d| &d.camera),
            keep_open: false,
        }),
        screen: None,
    }
}

/// Where the other side comes out.
fn playback() -> MediaDescription {
    MediaDescription {
        microphone: None,
        speaker: Some(AudioDescription {
            media_source: MediaSource::Device,
            sample_rate: 48_000,
            channel_count: 2,
            input: device(|d| &d.speaker),
            keep_open: false,
        }),
        camera: None,
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
}
