//! The apps in this build. Each is a directory that implements the kernel's
//! `App` and the shell's `AppUi`; nothing outside `apps/` names one except
//! `main.rs`.

pub mod accounts;
pub mod calendar;
pub mod agent;
pub mod files;
pub mod mail;
pub mod notes;
pub mod rss;
pub mod telegram;
#[cfg(not(target_os = "android"))]
pub mod terminal;

#[cfg(not(target_os = "android"))]
pub mod workshop;
