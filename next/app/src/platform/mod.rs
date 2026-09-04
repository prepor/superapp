//! What the platform gives the shell that makepad does not.
//!
//! Only a windowed macOS build has a window to reach for. A headless one
//! draws into a buffer, and shaping the frame it rasterizes would make a
//! run depend on the display it ran on, so nothing here is compiled into
//! it. Like `shell/`, this names no app.

#[cfg(all(target_os = "macos", not(headless)))]
pub mod mac;
