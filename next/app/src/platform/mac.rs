//! macOS screen geometry and activation, through AppKit directly.

use makepad_apple_sys::*;
use makepad_widgets::*;

/// Visible frame of the main display in Cocoa screen points (bottom-left
/// origin): the whole display minus the menu bar and the Dock. Returns
/// `(pos, size)`.
#[must_use]
pub fn visible_frame() -> (DVec2, DVec2) {
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

/// Brings the app to the front. makepad skips presenting an occluded
/// window, so one that launched behind the terminal would draw nothing.
pub fn activate() {
    unsafe {
        let app: ObjcId = msg_send![class!(NSApplication), sharedApplication];
        let () = msg_send![app, activateIgnoringOtherApps: YES];
    }
}

/// Puts every window of ours behind the normal ones and lets clicks and
/// keys through: a scripted run must not take the screen from whoever is
/// using the Mac.
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
