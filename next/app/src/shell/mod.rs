//! The shell: everything generic that draws or takes input.
//!
//! It uses Makepad and depends on the kernel, and it names no app. The
//! stage, the chrome, the verb bar, animation, the overlays, the shared
//! widgets, and the hosting of panel widgets are all here; what a panel is
//! *about* is never.
//!
//! The one exception is [`system`], the shell's own app — help and about,
//! and the card a panel gets when no app in this build owns its tag. It is
//! listed by the binary like any other app, so the shell uses its own
//! extension points rather than a private door.
//!
//! # The seams
//!
//! - a widget reaches its instance and the session through the scope:
//!   `scope.data.get_mut::<Session>()` and
//!   `scope.props.get::<`[`hosted::PanelProps`]`>()`;
//! - a template is registered by the binary, as a named child of the stage,
//!   and claimed by [`app_ui::AppUi::template`];
//! - a component registers its own rectangles through
//!   [`hits::Hits`] on the props;
//! - a verb reaches the bar through [`kernel::panel::Panel::verbs`], and its
//!   letter through [`bar::chord`] in the order [`keys`] documents — which
//!   [`bar::bold`] draws, so a bold letter promises only what that order
//!   would reach;
//! - a widget with a live text field says what it keeps from those bars
//!   through [`hosted::Chord::field`].

pub mod anim;
pub mod app_ui;
pub mod bar;
pub mod boot;
pub mod catalog;
pub mod draw;
pub mod dsl;
pub mod e2e;
pub mod hits;
pub mod hosted;
pub mod keys;
pub mod library;
pub mod overlays;
pub mod pointer;
pub mod stage;
pub mod system;
pub mod widgets;

use std::sync::OnceLock;

use app_ui::AppUi;
use kernel::app::App;
use makepad_widgets::{ScriptValue, ScriptVm};

/// The shell's own DSL: the theme and the base widgets, then the shared
/// components built on them. The binary calls this before any app's.
pub fn script_mod(vm: &mut ScriptVm) -> ScriptValue {
    dsl::script_mod(vm);
    widgets::dsl::script_mod(vm);
    library::script_mod(vm)
}

static APPS: OnceLock<&'static [&'static dyn App]> = OnceLock::new();
static UIS: OnceLock<&'static [&'static dyn AppUi]> = OnceLock::new();

/// Every app in this build, as the binary listed them.
#[must_use]
pub fn apps() -> &'static [&'static dyn App] {
    APPS.get().copied().unwrap_or(&[])
}

/// Their Makepad halves, in the same order.
#[must_use]
pub fn uis() -> &'static [&'static dyn AppUi] {
    UIS.get().copied().unwrap_or(&[])
}

/// Boots the shell on an app list, and runs until the window closes.
///
/// The binary is the only place that knows which apps exist: it lists them,
/// hands both halves over here, and passes the makepad entry point its own
/// `app_main!` generated.
pub fn run(apps: &'static [&'static dyn App], uis: &'static [&'static dyn AppUi], start: fn()) {
    let _ = APPS.set(apps);
    let _ = UIS.set(uis);
    // Every tag an app registered must have a template, and every template
    // must belong to a tag: a panel nothing can draw is a boot error, not a
    // blank rectangle found later.
    let registry = kernel::app::Apps::new(apps);
    for tag in registry.tags() {
        assert!(
            uis.iter().any(|ui| ui.template(tag).is_some()),
            "no app supplies a widget template for the tag {tag}"
        );
    }
    // Read argv before the window exists, so a bad script fails loudly.
    let _ = boot::config();
    start();
}
