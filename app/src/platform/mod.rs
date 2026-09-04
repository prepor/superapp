//! What the platform gives the shell that makepad does not.
//!
//! The kernel declares the capabilities and keeps the fakes; the real ones
//! live here and are installed by `shell::boot`: the disk this machine
//! actually has, the store the platform keeps a password in, and — on a
//! windowed macOS build — the window itself.
//!
//! [`watch`] is the one that is split by platform rather than compiled for
//! one: macOS watches the disk with FSEvents and android with inotify, and
//! anywhere else a files panel refreshes on its own writes alone.
//!
//! [`mac`] is macOS only, and most of it is windowed-only besides: a
//! headless build draws into a buffer, and shaping or photographing the
//! frame it rasterizes would make a run depend on the display it ran on.
//! The trash is the exception, because a real disk needs one either way.
//!
//! Like `shell/`, this names no app.

pub mod disk;
pub mod secret;
pub mod watch;

#[cfg(target_os = "macos")]
pub mod mac;
