//! The Makepad half of an app.
//!
//! The kernel's [`App`](kernel::app::App) says what an app adds to the
//! store, the queue and the launcher; this says what it adds to the screen.
//! The binary lists the two halves side by side and hands both to
//! [`install`](super::install).

use kernel::panel::Tag;
use kernel::scene::Scene;
use makepad_widgets::{LiveId, ScriptValue, ScriptVm};

/// A panels-library entry, as an app supplies it: what a node of a scene
/// comes up as. Defined by the shell's [`catalog`](super::catalog), which is
/// also where the constructors an app builds one with live.
pub use super::catalog::Setup;

/// The Makepad half of an app: its widget templates and its scenes.
pub trait AppUi: Sync + Send + 'static {
    /// The app's own `script_mod!` block. The binary's
    /// `AppMain::script_mod` calls the shell's, then each app's. Template
    /// ids carry the app id (`sys_help_tpl`), which keeps two apps apart in
    /// one script virtual machine.
    fn script_mod(&self, vm: &mut ScriptVm) -> ScriptValue;

    /// The template the shell instantiates for a panel of this tag: one
    /// widget per slot, kept across draws. The id names a child of the
    /// stage, which the binary's DSL declares. A tag the app registered
    /// without a template is a boot error.
    fn template(&self, tag: Tag) -> Option<LiveId>;

    /// The app's entries for the panels library, in canvas order after the
    /// shell's own. An app with nothing to show has none.
    fn scenes(&self) -> Vec<Scene<Setup>> {
        Vec::new()
    }
}
