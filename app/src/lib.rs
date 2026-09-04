//! The Makepad half: the app list, the shell that runs it, and
//! the window they draw into.
//!
//! It is a library with a binary on top of it, which is the shape android
//! needs: a desktop build starts at a `fn main`, and android has no main at
//! all. There the activity loads this crate as a shared object and calls the
//! JNI symbol `app_main!` generates in [`root`] — so everything above the
//! entry point has to live here, where both can reach it, and `main.rs` is
//! one line.
//!
//! Two lists, side by side: what each app adds to the store, the queue and
//! the launcher, and what it adds to the screen. The kernel builds its
//! registry from the first; the shell asks the second for templates. This
//! is the only place in the build that names an app.

// The apps are leaves: nothing outside this file names one, so nothing
// outside this crate needs to either.
mod apps;
pub mod platform;
pub mod root;
pub mod shell;

use kernel::app::App;

use crate::apps::{agent, files, mail};
use crate::shell::app_ui::AppUi;
use crate::shell::system;

/// Every app in this build. `system` is listed last, so the launcher's
/// roots keep their order: an app's own panels lead, help and about close.
/// Mail leads, so a store nobody has booted comes up on the inbox.
static APPS: &[&dyn App] = &[&mail::MAIL, &files::FILES, &agent::AGENT, &system::SYSTEM];

/// Their Makepad halves, in the same order.
static UIS: &[&dyn AppUi] = &[&mail::UI, &files::UI, &agent::UI, &system::UI];

/// Hands the shell the app list.
///
/// Called from [`root::App`]'s `script_mod`, which is the one place *both*
/// entry points pass through: a desktop `main` and android's
/// `activityOnCreate` alike reach it on the startup event, before a widget
/// exists to ask what a panel is. Idempotent, so calling it from [`run`]
/// too costs nothing.
pub fn install() {
    shell::install(APPS, UIS);
}

/// The desktop entry point: what the binary's `fn main` is a call to.
///
/// Android never gets here — its activity enters through the JNI symbol
/// instead — which is why the app list is installed from `script_mod` and
/// not from this function.
#[cfg(not(target_os = "android"))]
pub fn run() {
    // `--r2-login` files a device-sync secret and exits: it reads the key
    // from stdin, because an argument is in `ps` and in the shell's history
    // and this one key can write the whole lineage. Before the window,
    // because there is no window to be confused by it.
    if let Some(code) = kernel::repl::r2::login_from_argv(&mut platform::secret::Keychain::new(
        shell::boot::login_dir(),
    )) {
        std::process::exit(code);
    }
    install();
    // Read argv before the window exists, so a bad script fails loudly.
    let _ = shell::boot::config();
    root::run();
}
