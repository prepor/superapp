//! superapp — a personal "user space OS".
//!
//! No apps, no windows: specialized panels (kind + params) on one horizontally
//! scrolling 12×6 workspace, niri-style. See `docs/book` for the whole model.
//!
//! | module | role | depends on makepad |
//! |---|---|---|
//! | [`core`] | pure panel/column/join state machine | no |
//! | [`launcher`] | the launcher's search over panels + mail world | no |
//! | [`data`] | fake mail data behind the demo panels | no |
//! | [`spring`] | niri's closed-form spring (via mosaic) | no |
//! | [`theme`] | the look: sizes and colours | no |
//! | [`app`] | the makepad shell: drawing, events | yes |
//!
//! Everything above `app` is pure and unit-tested without opening a window.

pub mod app;
pub mod core;
pub mod data;
pub mod e2e;
pub mod launcher;
#[cfg(target_os = "macos")]
pub mod mac;
pub mod spring;
pub mod theme;
