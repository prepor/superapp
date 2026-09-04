//! The prototype's binary: the app list, and the shell that runs it.
//!
//! Two lists, side by side: what each app adds to the store, the queue and
//! the launcher, and what it adds to the screen. The kernel builds its
//! registry from the first; the shell asks the second for templates. This
//! is the only place in the build that names an app.

mod apps;
mod platform;
mod root;
mod shell;

use kernel::app::App;

use crate::apps::{files, mail};
use crate::shell::app_ui::AppUi;
use crate::shell::system;

/// Every app in this build. `system` is listed last, so the launcher's
/// roots keep their order: an app's own panels lead, help and about close.
/// Mail leads, so a store nobody has booted comes up on the inbox.
static APPS: &[&dyn App] = &[&mail::MAIL, &files::FILES, &system::SYSTEM];

/// Their Makepad halves, in the same order.
static UIS: &[&dyn AppUi] = &[&mail::UI, &files::UI, &system::UI];

fn main() {
    shell::run(APPS, UIS, root::run);
}
