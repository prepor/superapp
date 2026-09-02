//! Secrets live **outside** the store — the SQLite file is meant to be
//! handed to agents someday, and passwords must never ride along.
//!
//! macOS: the login keychain via `/usr/bin/security` (the item is created
//! and read by the same tool, so no ACL prompts; the password does pass
//! through argv for one process — a local, single-user trade, noted).
//! Elsewhere (android): an app-private file next to the store, mode 0600 —
//! the app sandbox is the perimeter until a Keystore binding exists.

use std::path::Path;

#[cfg(target_os = "macos")]
const SERVICE: &str = "superapp-imap";

/// Stores an account's password.
pub fn set(dir: &Path, email: &str, pass: &str) -> bool {
    #[cfg(target_os = "macos")]
    {
        let _ = dir;
        std::process::Command::new("/usr/bin/security")
            .args([
                "add-generic-password",
                "-U",
                "-a",
                email,
                "-s",
                SERVICE,
                "-w",
                pass,
            ])
            .output()
            .map(|o| o.status.success())
            .unwrap_or(false)
    }
    #[cfg(not(target_os = "macos"))]
    file_set(dir, email, pass)
}

/// Recalls an account's password.
pub fn get(dir: &Path, email: &str) -> Option<String> {
    #[cfg(target_os = "macos")]
    {
        let _ = dir;
        let out = std::process::Command::new("/usr/bin/security")
            .args(["find-generic-password", "-a", email, "-s", SERVICE, "-w"])
            .output()
            .ok()?;
        if !out.status.success() {
            return None;
        }
        let s = String::from_utf8_lossy(&out.stdout).trim().to_string();
        (!s.is_empty()).then_some(s)
    }
    #[cfg(not(target_os = "macos"))]
    file_get(dir, email)
}

/// The file fallback (android): `<dir>/secrets/<email>`, private mode.
#[allow(dead_code)]
fn file_set(dir: &Path, email: &str, pass: &str) -> bool {
    let d = dir.join("secrets");
    if std::fs::create_dir_all(&d).is_err() {
        return false;
    }
    let p = d.join(email.replace('/', "_"));
    let ok = std::fs::write(&p, pass).is_ok();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o600));
    }
    ok
}

#[allow(dead_code)]
fn file_get(dir: &Path, email: &str) -> Option<String> {
    let p = dir.join("secrets").join(email.replace('/', "_"));
    let s = std::fs::read_to_string(p).ok()?;
    (!s.is_empty()).then_some(s)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The file fallback round-trips (the keychain path is not exercised —
    /// tests must not write to a human's keychain).
    #[test]
    fn file_fallback_round_trips() {
        let dir = std::env::temp_dir().join(format!("superapp-secret-{}", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);
        assert!(file_set(&dir, "a@b.c", "s3cret"));
        assert_eq!(file_get(&dir, "a@b.c").as_deref(), Some("s3cret"));
        assert_eq!(file_get(&dir, "nobody@x"), None);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
