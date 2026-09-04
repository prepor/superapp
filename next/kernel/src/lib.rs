//! The kernel: everything generic that does not draw.
//!
//! One rule holds over every module here: **nothing in this crate names
//! Makepad, and nothing names an app.** What is left is the panel model and
//! navigation, the store and its cached queries, effects and the queue, undo
//! history, the filter and the rich table's state, search and the launcher
//! list, problems, springs, the e2e grammar, and the interfaces an app
//! implements. It is what `cargo test` runs without a window.
//!
//! The three layers around it: the **shell** draws and takes input and
//! depends on this crate; the **apps** implement [`app::App`] and supply
//! their widgets to the shell. Apps reach each other only through
//! [`app::Apps::get`] and [`app::Apps::get_as`], and work when the answer is
//! `None`.
//!
//! # Where to start
//!
//! - [`session::Session`] is the whole surface a verb, an instance, or a
//!   widget acts on.
//! - [`panel`] is what a panel *is*: [`panel::PanelId`], [`panel::PanelKind`],
//!   [`panel::Panel`], [`panel::Verb`].
//! - [`app`] is what an app registers, and [`app::Workers`] is what runs its
//!   background passes.
//! - [`nav::Nav`] is where a click goes.

pub mod app;
pub mod caps;
pub mod e2e;
pub mod effect;
pub mod filter;
pub mod history;
pub mod launcher;
pub mod layout;
pub mod nav;
pub mod panel;
pub mod problems;
pub mod richtable;
pub mod scene;
pub mod search;
pub mod session;
pub mod spring;
pub mod store;
pub mod theme;
pub mod time;

#[cfg(test)]
mod boundary {
    /// Rule 1 of the contract, enforced by reading the source: nothing in
    /// the kernel names Makepad, and nothing names an app.
    ///
    /// The shell will grow the same test for rule 2. Both are cheap and
    /// both catch the one mistake that is invisible in review: an import
    /// that seems harmless until the layer it crossed has to be moved.
    #[test]
    fn the_kernel_names_no_makepad_and_no_app() {
        // Word-ish matches, so prose about "the mail an app sends" is fine
        // and `use crate::mail` is not.
        let banned = [
            "makepad",
            "crate::mail",
            "crate::files",
            "crate::html",
            "crate::oauth",
            "crate::sync",
            "crate::send",
            "crate::repl",
            "crate::panels",
            "crate::catalog",
            "crate::library",
        ];
        let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
        let mut offenders = Vec::new();
        for entry in std::fs::read_dir(&dir).expect("read src") {
            let path = entry.expect("dir entry").path();
            if path.extension().and_then(|e| e.to_str()) != Some("rs") {
                continue;
            }
            if path.file_name().and_then(|n| n.to_str()) == Some("lib.rs") {
                continue; // the list itself
            }
            let src = std::fs::read_to_string(&path).expect("read source");
            for (n, line) in src.lines().enumerate() {
                let code = line.split("//").next().unwrap_or("");
                for word in banned {
                    if code.to_lowercase().contains(word) {
                        offenders.push(format!("{}:{}: {word}", path.display(), n + 1));
                    }
                }
            }
        }
        assert!(
            offenders.is_empty(),
            "the kernel names no Makepad and no app; found: {offenders:?}"
        );
    }

    /// The other half of the store's old guarantee: `Connection::open` lives
    /// in one module, so no code can quietly open a second writable handle
    /// and route around the one writer.
    #[test]
    fn connection_open_is_confined_to_the_store() {
        let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
        let mut offenders = Vec::new();
        for entry in std::fs::read_dir(&dir).expect("read src") {
            let path = entry.expect("dir entry").path();
            if path.extension().and_then(|e| e.to_str()) != Some("rs") {
                continue;
            }
            let name = path.file_name().and_then(|n| n.to_str());
            if name == Some("store.rs") || name == Some("lib.rs") {
                continue; // the one place the writer lives, and this test
            }
            let src = std::fs::read_to_string(&path).expect("read source");
            for (n, line) in src.lines().enumerate() {
                let code = line.split("//").next().unwrap_or("");
                if code.contains("Connection::open") {
                    offenders.push(format!("{}:{}", path.display(), n + 1));
                }
            }
        }
        assert!(
            offenders.is_empty(),
            "Connection::open must stay in store.rs; found: {offenders:?}"
        );
    }
}
