//! Superapp puts focused panels on scrolling tiled workspaces.
//!
//! Most modules contain product logic and do not depend on Makepad. [`app`],
//! [`panels`], [`catalog`], and [`library`] contain the native interface. See
//! `docs/book` for the product and architecture guide.

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
pub mod r2;
pub mod repl;
pub mod richtable;
pub mod scene;
pub mod search;
pub mod secret;
pub mod send;
pub mod spring;
pub mod store;
pub mod sync;
pub mod theme;
pub mod ui;
