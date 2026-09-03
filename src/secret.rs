//! Stores mail passwords and device-sync secret keys outside SQLite.
//!
//! macOS uses the login keychain through `/usr/bin/security`. Other targets
//! currently use a mode-0600 file inside the app directory. Mail and bucket
//! secrets use separate service names.

use std::path::Path;

#[cfg(target_os = "macos")]
const SERVICE: &str = "superapp-imap";

/// The device-sync bucket's secret access key, by key id.
#[cfg(target_os = "macos")]
const BUCKET_SERVICE: &str = "superapp-r2";

pub fn set(dir: &Path, email: &str, pass: &str) -> bool {
    #[cfg(target_os = "macos")]
    {
        let _ = dir;
        keychain_set(SERVICE, email, pass)
    }
    #[cfg(not(target_os = "macos"))]
    file_set(dir, email, pass)
}

pub fn get(dir: &Path, email: &str) -> Option<String> {
    #[cfg(target_os = "macos")]
    {
        let _ = dir;
        keychain_get(SERVICE, email)
    }
    #[cfg(not(target_os = "macos"))]
    file_get(dir, email)
}

/// Stores a device-sync secret under its key ID.
pub fn set_bucket_secret(dir: &Path, key_id: &str, secret: &str) -> bool {
    #[cfg(target_os = "macos")]
    {
        let _ = dir;
        keychain_set(BUCKET_SERVICE, key_id, secret)
    }
    #[cfg(not(target_os = "macos"))]
    file_set(dir, &bucket_account(key_id), secret)
}

/// Recalls the bucket's secret access key for a key id. `dir` is optional
/// because the keychain does not need one — only the file fallback does, and
/// a caller with no store (a demo, a CLI) still deserves the keychain.
#[must_use]
pub fn bucket_secret(dir: Option<&Path>, key_id: &str) -> Option<String> {
    #[cfg(target_os = "macos")]
    {
        let _ = dir;
        keychain_get(BUCKET_SERVICE, key_id)
    }
    #[cfg(not(target_os = "macos"))]
    file_get(dir?, &bucket_account(key_id))
}

/// The file fallback's name for a bucket key — an `r2/` prefix (flattened to
/// `r2_…` on disk) so a key id can never collide with an email address.
#[allow(dead_code)]
fn bucket_account(key_id: &str) -> String {
    format!("r2/{key_id}")
}

#[cfg(target_os = "macos")]
fn keychain_set(service: &str, account: &str, pass: &str) -> bool {
    std::process::Command::new("/usr/bin/security")
        .args([
            "add-generic-password",
            "-U",
            "-a",
            account,
            "-s",
            service,
            "-w",
            pass,
        ])
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

#[cfg(target_os = "macos")]
fn keychain_get(service: &str, account: &str) -> Option<String> {
    let out = std::process::Command::new("/usr/bin/security")
        .args(["find-generic-password", "-a", account, "-s", service, "-w"])
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let s = String::from_utf8_lossy(&out.stdout).trim().to_string();
    (!s.is_empty()).then_some(s)
}

/// The file fallback (android): `<dir>/secrets/<account>`, private mode.
#[allow(dead_code)]
fn file_set(dir: &Path, account: &str, pass: &str) -> bool {
    let p = dir.join("secrets").join(account.replace('/', "_"));
    let Some(parent) = p.parent() else {
        return false;
    };
    if std::fs::create_dir_all(parent).is_err() {
        return false;
    }
    let ok = std::fs::write(&p, pass).is_ok();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o600));
    }
    ok
}

#[allow(dead_code)]
fn file_get(dir: &Path, account: &str) -> Option<String> {
    let p = dir.join("secrets").join(account.replace('/', "_"));
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
        // A bucket key lives under its own name and cannot collide with one.
        assert!(file_set(&dir, &bucket_account("a@b.c"), "r2key"));
        assert_eq!(file_get(&dir, &bucket_account("a@b.c")).as_deref(), Some("r2key"));
        assert_eq!(file_get(&dir, "a@b.c").as_deref(), Some("s3cret"));
        let _ = std::fs::remove_dir_all(&dir);
    }
}
