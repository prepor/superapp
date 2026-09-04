//! Where a password lives on this machine: the kernel's
//! [`Secrets`] over the platform's own store.
//!
//! macOS uses the login keychain through `/usr/bin/security`; everywhere
//! else a mode-0600 file inside the app directory. Never the store: a secret
//! is the one thing that must not replicate.
//!
//! A key that begins `r2/` is a device-sync bucket's secret access key and
//! goes under its own keychain service, so a key id can never collide with a
//! mail account's address. The kernel spells that prefix once, in
//! [`kernel::repl::r2::secret_key`].
//!
//! Only [`Mode::Real`](kernel::app::Mode) gets this. A scripted run keeps
//! the kernel's in-memory one: a suite must no more write to a human's
//! keychain than delete their files.

use std::path::{Path, PathBuf};

use kernel::caps::Secrets;

/// The keychain service a mail password goes under.
#[cfg(target_os = "macos")]
const SERVICE: &str = "superapp-imap";

/// The one a device-sync bucket's secret access key goes under.
#[cfg(target_os = "macos")]
const BUCKET_SERVICE: &str = "superapp-r2";

/// The prefix the kernel files a bucket secret under.
const BUCKET_PREFIX: &str = "r2/";

/// The platform's secret store.
pub struct Keychain {
    /// Where the file fallback writes. The keychain needs none — and a
    /// caller with no store file still deserves the keychain.
    #[cfg_attr(target_os = "macos", allow(dead_code))]
    dir: Option<PathBuf>,
}

impl Keychain {
    /// One over the directory beside the store.
    #[must_use]
    pub fn new(dir: Option<PathBuf>) -> Keychain {
        Keychain { dir }
    }
}

impl Secrets for Keychain {
    fn get(&mut self, key: &str) -> Option<String> {
        let (service, account) = split(key);
        let _ = service;
        #[cfg(target_os = "macos")]
        {
            keychain_get(service, account)
        }
        #[cfg(not(target_os = "macos"))]
        {
            file_get(self.dir.as_deref()?, key)
        }
    }

    fn set(&mut self, key: &str, secret: &str) -> bool {
        let (service, account) = split(key);
        let _ = service;
        #[cfg(target_os = "macos")]
        {
            keychain_set(service, account, secret)
        }
        #[cfg(not(target_os = "macos"))]
        {
            match self.dir.as_deref() {
                Some(dir) => file_set(dir, key, secret),
                None => false,
            }
        }
    }
}

/// The service a key belongs to, and the account inside it. Two services,
/// because the two kinds of secret are two kinds of thing and a person
/// looking in Keychain Access should see which is which.
fn split(key: &str) -> (&'static str, &str) {
    #[cfg(target_os = "macos")]
    match key.strip_prefix(BUCKET_PREFIX) {
        Some(key_id) => (BUCKET_SERVICE, key_id),
        None => (SERVICE, key),
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = BUCKET_PREFIX;
        ("", key)
    }
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

/// The file fallback: `<dir>/secrets/<key>`, private mode, the slash of a
/// prefixed key flattened so one directory holds them all. Unreached on
/// macOS, where the keychain is the store; its own test exercises it.
#[cfg_attr(target_os = "macos", allow(dead_code))]
fn file_set(dir: &Path, key: &str, pass: &str) -> bool {
    let p = dir.join("secrets").join(key.replace('/', "_"));
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

#[cfg_attr(target_os = "macos", allow(dead_code))]
fn file_get(dir: &Path, key: &str) -> Option<String> {
    let p = dir.join("secrets").join(key.replace('/', "_"));
    let s = std::fs::read_to_string(p).ok()?;
    (!s.is_empty()).then_some(s)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The file fallback round-trips, and a bucket key lives under its own
    /// name. The keychain path is not exercised — tests must not write to a
    /// human's keychain.
    #[test]
    fn the_file_fallback_round_trips() {
        let dir = std::env::temp_dir().join(format!("superapp-secret-{}", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);
        assert!(file_set(&dir, "a@b.c", "s3cret"));
        assert_eq!(file_get(&dir, "a@b.c").as_deref(), Some("s3cret"));
        assert_eq!(file_get(&dir, "nobody@x"), None);

        let bucket = kernel::repl::r2::secret_key("a@b.c");
        assert!(file_set(&dir, &bucket, "r2key"));
        assert_eq!(file_get(&dir, &bucket).as_deref(), Some("r2key"));
        assert_eq!(
            file_get(&dir, "a@b.c").as_deref(),
            Some("s3cret"),
            "and cannot collide with a mail password"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The two kinds of secret go under two services, so a bucket key and a
    /// mail password of the same name are two entries.
    #[cfg(target_os = "macos")]
    #[test]
    fn a_bucket_key_and_a_password_are_two_entries() {
        assert_eq!(split("a@b.c"), (SERVICE, "a@b.c"));
        assert_eq!(
            split(&kernel::repl::r2::secret_key("AKIDEXAMPLE")),
            (BUCKET_SERVICE, "AKIDEXAMPLE")
        );
    }
}
