//! superapp — a personal "user space OS".
//!
//! No apps, no windows: specialized panels (kind + params) on one horizontally
//! scrolling 12×6 workspace, niri-style. See `docs/book` for the whole model.
//!
//! | module | role | depends on makepad |
//! |---|---|---|
//! | [`core`] | pure panel/column/join state machine | no |
//! | [`store`] | the one SQLite file + the reactive query layer | no |
//! | [`effect`] | the boundary: what leaves the process, and the job queue | no |
//! | [`history`] | the in-memory tree of actions and their claims | no |
//! | [`html`] | narrowing HTML from outside — mail, feed articles — to what a panel draws | no |
//! | [`filter`] | the rich table's filter grammar and completion context | no |
//! | [`richtable`] | the rich table: datasources, the SQL builder, paging | no |
//! | [`mail`] | the mail domain: queries, titles, seed, mutations | no |
//! | [`files`] | the file browser's domain: listings, the card, the held item (CR-008 draft) | no |
//! | [`sync`] | the IMAP engine: workers, ingest, push, reconciliation | no |
//! | [`send`] | drafts → outbox → SMTP, with the undo window | no |
//! | [`repl`] | device sync: the changeset log, the lease, the passes | no |
//! | [`object`] | the sync transport: object store (memory, HTTP) + `state` | no |
//! | [`secret`] | passwords: keychain (macOS) / private file | no |
//! | [`oauth`] | Gmail sign-in: the browser flow, and XOAUTH2's tokens | no |
//! | [`launcher`] | the launcher's search over panels + mail world | no |
//! | [`problems`] | standing problems, derived from the rows that carry them | no |
//! | [`spring`] | niri's closed-form spring (via mosaic) | no |
//! | [`ui`] | the shared vocabulary: text styles, accelerators | no |
//! | [`theme`] | the look: sizes and colours | no |
//! | [`scene`] | a subject in its named states, and the library canvas's layout | no |
//! | [`app`] | the makepad shell: drawing, events | yes |
//! | [`catalog`] | the scenes the panels library shows (CR-006) | yes |
//! | [`library`] | the panels library: a canvas of live scenes (CR-006) | yes |
//!
//! Everything above `app` is pure and unit-tested without opening a window.

pub mod app;
pub mod catalog;
pub mod core;
pub mod e2e;
pub mod effect;
pub mod files;
pub mod filter;
pub mod history;
pub mod html;
pub mod launcher;
pub mod library;
#[cfg(target_os = "macos")]
pub mod mac;
pub mod mail;
pub mod oauth;
pub mod object;
pub mod panels;
pub mod problems;
pub mod repl;
pub mod richtable;
pub mod scene;
pub mod secret;
pub mod send;
pub mod spring;
pub mod store;
pub mod sync;
pub mod theme;
pub mod ui;
