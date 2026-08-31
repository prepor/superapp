# Tech Stack

**Rust + Makepad** (GPU-rendered immediate-mode UI), macOS first, **android
second** (Samsung Fold — two screens, so resizes are a first-class event).
The same stack, and the same *vendored copy*, as mosaic in `rel.systems`:

- `makepad-widgets` is a **path dependency** on
  `../rel.systems/mosaic/third_party/makepad` — makepad pinned at mosaic's
  commit with mosaic's patches applied. Run
  `rel.systems/mosaic/scripts/vendor-makepad.sh` once to materialise it.
  Superapp deliberately does not carry its own makepad: one pin, one patch
  set, owned where the expertise is.
- `makepad-apple-sys` / `makepad-objc-sys` for the few macOS calls makepad
  does not expose (screen geometry, activation, window screenshots).
- The window is **borderless over the display's visible frame** (menu bar and
  Dock stay) — mosaic's shape, deliberately not a macOS fullscreen Space.
- Toolchain via `mise` (`rust` stable); no other runtime dependencies.

## Android

One crate, two targets: the lib already carries `app_main!`, which expands to
the JNI `activityOnCreate` entry on android (the desktop binary and all
mac-only code sit behind `cfg`). The APK is assembled by makepad's own
`cargo-makepad` — no gradle; fonts ship as APK assets, where the Menlo
`file_resource` quietly fails to load and **LiberationMono fronts the mono
family instead**. Fold/unfold arrives as a `WindowGeomChange` (with safe-area
insets, which the shell subtracts from the viewport); crossing the ~600 dp
width breakpoint swaps the grid 8×4 ⇄ 4×3 and the springs carry the panels to
their new places. Touch is handled from raw `TouchUpdate` events; the
long-press comes from android's own `GestureDetector` via `Event::LongPress`.

## Why not web

The first prototype (`web/`, kept for reference) was web and validated the
interaction model cheaply. The real thing is native because the product is a
window-manager-shaped program: it wants the GPU, real keyboard ownership,
process-level integration (terminals, agents), and latency a browser cannot
promise. The web prototype is not maintained.
