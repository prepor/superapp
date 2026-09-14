//! Mirrors makepad's own `MAKEPAD=headless` switch into a cfg we can read.
//!
//! makepad's `platform/build.rs` turns `MAKEPAD=headless` into
//! `--cfg headless`, which swaps its whole apple backend for a software
//! rasterizer. That cfg is set on *its* crate, not ours, so without this the
//! shell has no way to know which backend it is linked against — and it has
//! to know, because a window-layer screenshot is meaningless when there is
//! no window.
//!
//! Its second job is Telegram's engine: when the `tdlib` feature is on, link
//! `libtdjson`. Normal builds enable it; `--no-default-features` lets tests
//! and demos build without the native library. Its third is the other native
//! library a Telegram build wants, NTgCalls, which carries a call's media.

use std::path::PathBuf;
use std::process::Command;

/// The NTgCalls release the `ntgcalls` crate binds. Kept in step with the
/// version in `Cargo.toml`: the C API and the crate are one thing.
const NTGCALLS: &str = "3.0.0-rc03";

fn main() {
    println!("cargo:rustc-check-cfg=cfg(headless)");
    println!("cargo:rerun-if-env-changed=MAKEPAD");
    let headless = std::env::var("MAKEPAD")
        .map(|v| v.split(['+', ',']).any(|c| c.trim() == "headless"))
        .unwrap_or(false);
    if headless {
        println!("cargo:rustc-cfg=headless");
    }

    // The `tdlib` feature reaches cargo here as `CARGO_FEATURE_TDLIB`. Its
    // directory is `TDLIB_DIR` if set, else Homebrew's macOS prefix. The
    // -rpath is what lets the built binary — and the test binary, since
    // `rustc-link-arg` covers both — find `libtdjson.dylib` at runtime.
    if std::env::var("CARGO_FEATURE_TDLIB").is_ok() {
        let android = std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("android");
        let dir = std::env::var("TDLIB_DIR").unwrap_or_else(|_| {
            assert!(!android, "Android Telegram builds require TDLIB_DIR with lib/libtdjson.so for the target ABI");
            "/opt/homebrew/opt/tdlib".to_string()
        });
        println!("cargo:rustc-link-search=native={dir}/lib");
        println!("cargo:rustc-link-lib=dylib=tdjson");
        if !android {
            println!("cargo:rustc-link-arg=-Wl,-rpath,{dir}/lib");
        }
        println!("cargo:rerun-if-env-changed=TDLIB_DIR");
    }

    ntgcalls();
}

/// The call engine's shared library, on a Mac.
///
/// `ntgcalls-sys` would fetch and link the release's *static* archive, and
/// that archive does not link with the `ld` Xcode 26 ships — it asserts in
/// its own relocation parser on the ffmpeg objects inside it. The shared
/// library of the same release links and runs, so `.cargo/config.toml` points
/// the crate at a directory of ours and this puts the library in it: the
/// first build of a checkout fetches twelve megabytes, and every one after
/// finds it already there. The -rpath is what lets the binary — and the test
/// binary, since `rustc-link-arg` covers both — find it at runtime.
///
/// Android builds their own (`build-tools/ntgcalls-android.sh`) and export
/// `NTGCALLS_LIB_DIR` themselves, which wins over the config's; nothing here
/// runs for them.
fn ntgcalls() {
    println!("cargo:rerun-if-env-changed=NTGCALLS_LIB_DIR");
    if std::env::var("CARGO_FEATURE_CALLS").is_err()
        || std::env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("macos")
    {
        return;
    }
    let Ok(dir) = std::env::var("NTGCALLS_LIB_DIR") else { return };
    let dir = PathBuf::from(dir);
    println!("cargo:rustc-link-arg=-Wl,-rpath,{}", dir.display());
    let library = dir.join("libntgcalls.dylib");
    if library.exists() {
        return;
    }
    let into = dir.parent().unwrap_or(&dir).to_path_buf();
    std::fs::create_dir_all(&into).expect("a directory for the call engine");
    let zip = into.join("ntgcalls.zip");
    let url = format!(
        "https://github.com/pytgcalls/ntgcalls/releases/download/v{NTGCALLS}/ntgcalls.macos-arm64-shared_libs.zip"
    );
    let fetched = Command::new("curl")
        .args(["-fsSL", "-o"])
        .arg(&zip)
        .arg(&url)
        .status()
        .expect("curl, to fetch the call engine");
    assert!(fetched.success(), "could not fetch the call engine from {url}");
    let opened = Command::new("unzip")
        .arg("-oq")
        .arg(&zip)
        .arg("-d")
        .arg(&into)
        .status()
        .expect("unzip, to open the call engine");
    assert!(opened.success(), "could not open {}", zip.display());
    let _ = std::fs::remove_file(&zip);
    assert!(library.exists(), "{} is not where the release said", library.display());
}
