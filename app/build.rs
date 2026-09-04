//! Mirrors makepad's own `MAKEPAD=headless` switch into a cfg we can read.
//!
//! makepad's `platform/build.rs` turns `MAKEPAD=headless` into
//! `--cfg headless`, which swaps its whole apple backend for a software
//! rasterizer. That cfg is set on *its* crate, not ours, so without this the
//! shell has no way to know which backend it is linked against — and it has
//! to know, because a window-layer screenshot is meaningless when there is
//! no window.

fn main() {
    println!("cargo:rustc-check-cfg=cfg(headless)");
    println!("cargo:rerun-if-env-changed=MAKEPAD");
    let headless = std::env::var("MAKEPAD")
        .map(|v| v.split(['+', ',']).any(|c| c.trim() == "headless"))
        .unwrap_or(false);
    if headless {
        println!("cargo:rustc-cfg=headless");
    }
}
