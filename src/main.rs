//! superapp — panels on one scrolling workspace.
//!
//! All logic lives in the library so the core stays testable without a window.

fn main() {
    // `--r2-login` stores the device-sync bucket's secret access key and
    // exits — it never opens a window (CR-005).
    if let Some(code) = superapp::r2::login_from_argv() {
        std::process::exit(code);
    }
    superapp::app::run();
}
