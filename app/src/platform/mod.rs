//! What the platform gives the shell that makepad does not.
//!
//! The kernel declares the capabilities and keeps the fakes; the real ones
//! live here and are installed by `shell::boot`: the disk this machine
//! actually has, the store the platform keeps a password in, the voice it
//! reads a word in, where it says it is and what its camera and microphone
//! make, and — on a windowed macOS build — the window itself.
//!
//! [`watch`], [`speech`] and [`senses`] are the three that are split by
//! platform rather than compiled for one: macOS watches the disk with
//! FSEvents and android with inotify, and anywhere else a files panel
//! refreshes on its own writes alone; macOS speaks with AVFoundation and
//! android with `TextToSpeech`, and anywhere else a card offers its words in
//! writing; and a video message is written by AVAssetWriter on a Mac and by
//! `MediaCodec` on the phone, which is the one encoder the fork does not
//! carry, so the app talks to it itself.
//!
//! [`senses`] is also the one that is *served* rather than called: makepad's
//! receiver, camera and microphone answer as events on the window's thread,
//! and a capability has neither a `Cx` nor an event loop, so what a
//! capability writes down is a wish and the stage serves it.
//!
//! [`audio_route`] is the phone's alone in what it does, though every
//! platform can call it: android decides where a call is heard from the mode
//! the app is in, and a Mac has one output and nothing to say.
//!
//! [`mac`] is macOS only, and most of it is windowed-only besides: a
//! headless build draws into a buffer, and shaping or photographing the
//! frame it rasterizes would make a run depend on the display it ran on.
//! The trash is the exception, because a real disk needs one either way.
//!
//! Like `shell/`, this names no app.

pub mod audio_route;
pub mod clipboard;
pub mod browser;
pub mod disk;
pub mod secret;
pub mod senses;
pub mod speech;
pub mod watch;

#[cfg(target_os = "macos")]
pub mod mac;

#[cfg(target_os = "android")]
pub mod android;
