//! macOS platform bits makepad does not expose (ported from mosaic's `mac.rs`):
//! screen geometry for the borderless window, activation, and window-layer
//! screenshots for the verification loop.

#![allow(unsafe_code)]
#![allow(unexpected_cfgs)]

use std::path::Path;
use std::process::Command;

use makepad_apple_sys::*;
use makepad_widgets::*;

/// Visible frame of the main display in Cocoa screen points (bottom-left
/// origin): the full display minus menu bar and Dock. Returns `(pos, size)`.
#[must_use]
pub fn visible_frame() -> (Vec2d, Vec2d) {
    unsafe {
        let screen: ObjcId = msg_send![class!(NSScreen), mainScreen];
        if screen == nil {
            return (dvec2(0., 0.), dvec2(1440., 900.));
        }
        let frame: NSRect = msg_send![screen, visibleFrame];
        (
            dvec2(frame.origin.x, frame.origin.y),
            dvec2(frame.size.width, frame.size.height),
        )
    }
}

/// Brings the app to the front. makepad skips presenting occluded windows, so
/// a window that launches behind the terminal would render nothing.
pub fn activate() {
    unsafe {
        let app: ObjcId = msg_send![class!(NSApplication), sharedApplication];
        let () = msg_send![app, activateIgnoringOtherApps: YES];
    }
}

/// Our biggest visible window's `windowNumber`, or 0.
#[must_use]
pub fn window_number() -> i64 {
    unsafe {
        let app: ObjcId = msg_send![class!(NSApplication), sharedApplication];
        let wins: ObjcId = msg_send![app, windows];
        let count: usize = msg_send![wins, count];
        let mut best = 0i64;
        let mut best_area = 0.0f64;
        for i in 0..count {
            let w: ObjcId = msg_send![wins, objectAtIndex: i];
            let visible: BOOL = msg_send![w, isVisible];
            if visible != YES {
                continue;
            }
            let frame: NSRect = msg_send![w, frame];
            let area = frame.size.width * frame.size.height;
            if area > best_area {
                best_area = area;
                let n: i64 = msg_send![w, windowNumber];
                best = n;
            }
        }
        best
    }
}

/// Puts every window of ours behind all normal windows, click-through, and
/// never key — an e2e run must not take the screen from whoever is using the
/// machine (mosaic REVIEW S47).
pub fn configure_background_window() {
    unsafe {
        let ns_app: ObjcId = msg_send![class!(NSApplication), sharedApplication];
        if ns_app == nil {
            return;
        }
        let windows: ObjcId = msg_send![ns_app, windows];
        let count: usize = msg_send![windows, count];
        for i in 0..count {
            let w: ObjcId = msg_send![windows, objectAtIndex: i];
            if w == nil {
                continue;
            }
            let _: () = msg_send![w, setLevel: -1i64];
            let _: () = msg_send![w, setIgnoresMouseEvents: true];
            let _: () = msg_send![w, resignKeyWindow];
        }
    }
}

/// Captures our window to `path` as a PNG via `screencapture -l`: the window's
/// own layer — no cursor, no desktop, no other windows, and no dependence on
/// whether anything covers it.
///
/// # Errors
///
/// If there is no visible window, or `screencapture` fails.
pub fn screenshot(path: &Path) -> Result<(), String> {
    let n = window_number();
    if n == 0 {
        return Err("no visible window to capture".to_string());
    }
    let out = Command::new("/usr/sbin/screencapture")
        .arg(format!("-l{n}"))
        .arg("-o")
        .arg("-x")
        .arg(path)
        .output()
        .map_err(|e| format!("screencapture: {e}"))?;
    if !out.status.success() {
        return Err(format!(
            "screencapture exited {}: {}",
            out.status,
            String::from_utf8_lossy(&out.stderr).trim()
        ));
    }
    Ok(())
}
