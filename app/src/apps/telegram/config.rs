//! Device-local Telegram settings: api_id and phone from the `telegram` file
//! beside the store, overridden by `SUPERAPP_TG_API_ID` and `SUPERAPP_TG_PHONE`.
//! Blank lines and comments are skipped. The api_hash is read separately from
//! the `tg/api_hash` keychain entry by the worker.
#![cfg_attr(not(feature = "tdlib"), allow(dead_code))]

use std::path::Path;

/// The environment override for the api_id — wins over the file, so a run can
/// point at a second account without editing what is on disk.
const ENV_API_ID: &str = "SUPERAPP_TG_API_ID";

/// The environment override for the account phone.
const ENV_PHONE: &str = "SUPERAPP_TG_PHONE";

/// The Telegram api_id: the environment first, then line 1 of the `telegram`
/// file. A small positive integer Telegram assigns an application, and not a
/// secret, so a plain file is enough for it.
#[must_use]
pub fn api_id(dir: Option<&Path>) -> Option<i32> {
    api_id_from(env(ENV_API_ID), dir)
}

/// The api_id given an already-resolved override, so the precedence is
/// testable without setting a process-wide environment variable that every
/// other test in the binary would also see.
fn api_id_from(over: Option<String>, dir: Option<&Path>) -> Option<i32> {
    over.or_else(|| from_file(dir).into_iter().next())
        .and_then(|s| s.parse::<i32>().ok())
}

/// The account phone: the environment first, then line 2 of the `telegram`
/// file. Beside the api_id because a sign-in needs both, and neither is the
/// secret the keychain keeps.
#[must_use]
pub fn phone(dir: Option<&Path>) -> Option<String> {
    phone_from(env(ENV_PHONE), dir)
}

/// The phone given an already-resolved override — the same testability the
/// api_id has, for the same reason.
fn phone_from(over: Option<String>, dir: Option<&Path>) -> Option<String> {
    over.or_else(|| from_file(dir).get(1).cloned())
}

/// The `telegram` file beside the store, as its meaningful lines in order.
/// Blank lines and `#` comments are skipped so the file can carry a note; the
/// rest are positional. Copied in shape from [`r2`'s `bucket`
/// reader](kernel::r2), so the two files are read the same way.
fn from_file(dir: Option<&Path>) -> Vec<String> {
    let Some(dir) = dir else {
        return Vec::new();
    };
    let Ok(text) = std::fs::read_to_string(dir.join("telegram")) else {
        return Vec::new();
    };
    text.lines()
        .map(str::trim)
        .filter(|l| !l.is_empty() && !l.starts_with('#'))
        .map(str::to_string)
        .collect()
}

/// A non-empty environment variable, trimmed.
fn env(name: &str) -> Option<String> {
    let v = std::env::var(name).ok()?.trim().to_string();
    (!v.is_empty()).then_some(v)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A scratch directory with a `telegram` file in it, named per test so a
    /// parallel run cannot collide.
    fn scratch(tag: &str, body: &str) -> std::path::PathBuf {
        let dir =
            std::env::temp_dir().join(format!("superapp-tg-config-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("telegram"), body).unwrap();
        dir
    }

    /// Line 1 is the api_id, line 2 the phone — the file the account holder
    /// filed, read positionally through the public entry points.
    #[test]
    fn the_file_gives_the_api_id_and_the_phone() {
        let dir = scratch("plain", "17844\n+4915150525562\n");
        assert_eq!(api_id(Some(&dir)), Some(17844));
        assert_eq!(phone(Some(&dir)).as_deref(), Some("+4915150525562"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A `#` header and blank lines are a note, not a position: the api_id and
    /// phone are still lines 1 and 2 of what is left.
    #[test]
    fn a_comment_and_blank_lines_are_skipped() {
        let dir = scratch("noted", "# my telegram\n\n17844\n\n+4915150525562\n");
        assert_eq!(api_id(Some(&dir)), Some(17844));
        assert_eq!(phone(Some(&dir)).as_deref(), Some("+4915150525562"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The override wins over the file — proven by handing the resolved value
    /// straight to the inner reader, so no process-wide environment variable
    /// is set that a concurrent test could then read.
    #[test]
    fn the_override_wins_over_the_file() {
        let dir = scratch("override", "17844\n+4915150525562\n");
        // No override: the file's own values.
        assert_eq!(api_id_from(None, Some(&dir)), Some(17844));
        assert_eq!(
            phone_from(None, Some(&dir)).as_deref(),
            Some("+4915150525562")
        );
        // An override present: it wins, the file untouched.
        assert_eq!(api_id_from(Some("99".into()), Some(&dir)), Some(99));
        assert_eq!(
            phone_from(Some("+10000000000".into()), Some(&dir)).as_deref(),
            Some("+10000000000")
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// No file and no override is nothing — not a default, not a panic.
    #[test]
    fn a_missing_file_yields_none() {
        let dir =
            std::env::temp_dir().join(format!("superapp-tg-config-absent-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        assert_eq!(api_id(Some(&dir)), None);
        assert_eq!(phone(Some(&dir)), None);
        // And with no directory to look in at all.
        assert_eq!(api_id(None), None);
        assert_eq!(phone(None), None);
    }
}
