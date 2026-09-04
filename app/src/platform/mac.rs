//! macOS screen geometry, activation, the trash, and window screenshots —
//! through AppKit directly, because Makepad exposes none of them.
//!
//! Only [`trash`] is compiled into a headless build: a run with no window
//! has nothing to shape, activate or photograph, and the frames it
//! rasterizes are picked out of a directory instead (`shell::boot`).

use std::path::Path;
#[cfg(not(headless))]
use std::process::Command;

use makepad_apple_sys::*;
// The geometry types the windowed half answers in; a headless build has no
// window to describe, and only the trash below is compiled into it.
#[cfg(not(headless))]
use makepad_widgets::*;

/// Visible frame of the main display in Cocoa screen points (bottom-left
/// origin): the whole display minus the menu bar and the Dock. Returns
/// `(pos, size)`.
#[cfg(not(headless))]
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
#[cfg(not(headless))]
pub fn activate() {
    unsafe {
        let app: ObjcId = msg_send![class!(NSApplication), sharedApplication];
        let () = msg_send![app, activateIgnoringOtherApps: YES];
    }
}

/// Puts every window of ours behind the normal ones and lets clicks and
/// keys through: a scripted run must not take the screen from whoever is
/// using the Mac.
#[cfg(not(headless))]
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

/// Our biggest visible window's `windowNumber`, or 0.
#[cfg(not(headless))]
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

/// Captures our window to `path` as a PNG via `screencapture -l`: the
/// window's own layer — no cursor, no desktop, no other windows, and no
/// dependence on whether anything covers it.
///
/// A headless build has no window to point this at; there the shell copies
/// the newest frame the rasterizer wrote instead.
///
/// # Errors
///
/// If there is no visible window, or `screencapture` fails.
#[cfg(not(headless))]
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

/// Moves a path to the trash through `NSFileManager` — the door the Finder
/// uses — and answers where it landed. That is the right trash for the
/// volume the path is on, a name that does not clash with what is already
/// in there, and the Put Back the Finder offers; the landing path is what
/// undo moves back.
///
/// # Errors
///
/// Whatever `trashItemAtURL:` refused, in its own words.
pub fn trash(path: &Path) -> Result<std::path::PathBuf, String> {
    unsafe {
        let fm: ObjcId = msg_send![class!(NSFileManager), defaultManager];
        let url: ObjcId = msg_send![
            class!(NSURL),
            fileURLWithPath: str_to_nsstring(&path.to_string_lossy())
        ];
        let mut landed: ObjcId = nil;
        let mut err: ObjcId = nil;
        let ok: BOOL = msg_send![
            fm,
            trashItemAtURL: url
            resultingItemURL: &mut landed
            error: &mut err
        ];
        if ok != YES {
            let why = if err == nil {
                format!("the trash refused {}", path.display())
            } else {
                nsstring_to_string(msg_send![err, localizedDescription])
            };
            return Err(why);
        }
        // A trash that answers no URL is one we cannot undo from; say so
        // rather than remembering a path that is not where the file is.
        if landed == nil {
            return Err(format!("{}: the trash did not say where", path.display()));
        }
        let p: ObjcId = msg_send![landed, path];
        Ok(std::path::PathBuf::from(nsstring_to_string(p)))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The trash is a real system call, so this is the one test here that
    /// touches the machine — and it takes back everything it does: a file
    /// made in a scratch directory, trashed, and moved straight out again,
    /// which is exactly what undo does with it.
    #[test]
    fn a_file_goes_to_the_trash_and_comes_back_out() {
        let stamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| d.as_nanos());
        let dir = std::env::temp_dir().join(format!("superapp-trash-{stamp}"));
        std::fs::create_dir_all(&dir).expect("a scratch directory");
        let file = dir.join("scratch.txt");
        std::fs::write(&file, b"scratch").expect("a scratch file");

        let landed = trash(&file).expect("the trash took it");
        assert!(!file.exists(), "gone from where it was");
        assert!(landed.exists(), "and there, where the trash said it put it");

        // Out again — the move an undo makes — and then the scratch tree
        // goes, which is the one `remove` in this app and is our own.
        std::fs::rename(&landed, &file).expect("back out");
        assert!(file.exists());
        std::fs::remove_dir_all(&dir).expect("the scratch tree goes");
    }
}
