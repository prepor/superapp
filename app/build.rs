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
//! `libtdjson`. The feature is off by default, so a plain build emits none of
//! this and needs no native library — see `Cargo.toml`.

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
        let dir =
            std::env::var("TDLIB_DIR").unwrap_or_else(|_| "/opt/homebrew/opt/tdlib".to_string());
        println!("cargo:rustc-link-search=native={dir}/lib");
        println!("cargo:rustc-link-lib=dylib=tdjson");
        println!("cargo:rustc-link-arg=-Wl,-rpath,{dir}/lib");
        println!("cargo:rerun-if-env-changed=TDLIB_DIR");
    }
}
