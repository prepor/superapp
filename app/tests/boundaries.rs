//! Rule 2 of the contract, enforced by reading the source: code under
//! `shell/` — and under `platform/`, which is lower still — names no app.
//!
//! The kernel grows the same test for rule 1. Both are cheap and both catch
//! the one mistake that is invisible in review: an import that seems
//! harmless until the layer it crossed has to be moved.

use std::path::{Path, PathBuf};

/// Every `.rs` file under a directory, recursively.
fn sources(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for e in entries.filter_map(Result::ok) {
        let p = e.path();
        if p.is_dir() {
            sources(&p, out);
        } else if p.extension().and_then(|x| x.to_str()) == Some("rs") {
            out.push(p);
        }
    }
}

/// Neither the shell nor the platform may reach for an app.
/// `shell/system` is the shell's own app — the CR lists it inside the
/// shell on purpose, so that the shell uses its own extension points —
/// and `shell/mod.rs` is the list itself.
#[test]
fn the_shell_and_the_platform_name_no_app() {
    let src = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    let mut files = Vec::new();
    sources(&src.join("shell"), &mut files);
    sources(&src.join("platform"), &mut files);
    assert!(
        files.len() > 5,
        "the shell should have more than a few files"
    );

    let banned = ["apps::", "crate::apps", "crate::mail", "crate::files"];
    let mut offenders = Vec::new();
    for path in files {
        let s = path.to_string_lossy().to_string();
        if s.contains("shell/system") || s.ends_with("shell/mod.rs") {
            continue;
        }
        let src = std::fs::read_to_string(&path).expect("read source");
        for (n, line) in src.lines().enumerate() {
            // Word-ish matches on code only, so prose about "the mail an
            // app sends" is fine and `use crate::mail` is not.
            let code = line.split("//").next().unwrap_or("");
            for word in banned {
                if code.contains(word) {
                    offenders.push(format!("{}:{}: {word}", path.display(), n + 1));
                }
            }
            // Nor may it name the shell's own app by module path.
            if code.contains("system::") || code.contains("shell::system") {
                offenders.push(format!("{}:{}: system::", path.display(), n + 1));
            }
        }
    }
    assert!(
        offenders.is_empty(),
        "code under shell/ and platform/ names no app; found: {offenders:?}"
    );
}
