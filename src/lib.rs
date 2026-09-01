//! superapp — a personal "user space OS".
//!
//! No apps, no windows: specialized panels (kind + params) on one horizontally
//! scrolling 12×6 workspace, niri-style. See `docs/book` for the whole model.
//!
//! | module | role | depends on makepad |
//! |---|---|---|
//! | [`core`] | pure panel/column/join state machine | no |
//! | [`store`] | the one SQLite file + the reactive query layer | no |
//! | [`mail`] | the mail domain: queries, titles, seed, mutations | no |
//! | [`sync`] | the IMAP engine: workers, ingest, push, reconciliation | no |
//! | [`send`] | drafts → outbox → SMTP, with the undo window | no |
//! | [`secret`] | passwords: keychain (macOS) / private file | no |
//! | [`launcher`] | the launcher's search over panels + mail world | no |
//! | [`spring`] | niri's closed-form spring (via mosaic) | no |
//! | [`ui`] | the shared vocabulary: text styles, accelerators | no |
//! | [`theme`] | the look: sizes and colours | no |
//! | [`app`] | the makepad shell: drawing, events | yes |
//!
//! Everything above `app` is pure and unit-tested without opening a window.

pub mod app;
pub mod core;
pub mod e2e;
pub mod html;
pub mod launcher;
#[cfg(target_os = "macos")]
pub mod mac;
pub mod mail;
pub mod panels;
pub mod secret;
pub mod send;
pub mod spring;
pub mod store;
pub mod sync;
pub mod theme;
pub mod ui;
