# Tech Stack

Superapp is a Rust application built with Makepad. macOS is the target.

One Cargo workspace, two members:

- **`kernel/`**: the crate named `kernel`. It has **no Makepad dependency at
  all**, which is the layering rule made structural rather than agreed to. It
  carries `rusqlite`, `serde`, `serde_json`, and the TLS and signing crates
  device sync needs.
- **`app/`**: the crate named `superapp`, a library with a one-line binary on
  top of it, which is the shape Android needs, since a desktop build starts at
  a `fn main` and an activity has no main at all. It depends on the kernel and
  on `makepad-widgets`, plus the mail protocols and HTML crates. `bucketd`,
  `sync-demo`, and `reseed-edit` are auto-discovered binaries under
  `app/src/bin/`.

The pieces:

- **Makepad** draws the interface and handles input. The packages are pinned in
  the root `Cargo.toml`, which also patches them to a small fork carrying five
  local fixes for text input, headless screenshots, canvas zoom, and text
  selection. The comment there names each patch and the exact revision.
- **SQLite**, through `rusqlite`, stores application data. It is bundled so
  every target uses the same version and features. Update hooks invalidate
  cached queries, SQLite's authorizer records query dependencies, and the
  session extension records changes for device sync.
- **Serde and serde_json** encode queued effects and device-sync metadata.
- **imap, lettre, and mail-parser** provide IMAP, SMTP, and MIME support.
- **html5ever, markup5ever_rcdom, and simplecss** narrow HTML mail.
- **rustls** provides TLS. `ring`, `base64`, `rustls-connector`, and
  `webpki-roots` support Gmail sign-in and signed R2 requests. The app does not
  use a general-purpose HTTP client.
- **Makepad's macOS APIs** provide the menu bar. Small gaps such as screen
  geometry, the trash, and window screenshots use `makepad-apple-sys` and
  `makepad-objc-sys`, in `app/src/platform/mac.rs`.
- **mise** selects the stable Rust toolchain. The application has no other
  runtime dependency.

There are no Cargo features. Every switch is argv, an environment variable, or
`cfg(headless)`.

## Headless

Setting `MAKEPAD=headless` at build time replaces macOS drawing with Makepad's
software renderer, and `app/build.rs` mirrors it into `cfg(headless)` for this
crate, because the shell has to know which backend it is linked against: a
window-layer screenshot is meaningless when there is no window.

`cfg(headless)` is what turns on virtual time. Workers run inline from the
frame loop instead of on threads, the device-sync driver does too, and a
screenshot is a copy of the rasterizer's newest frame rather than a photograph
of a window. See [Developer Experience](./dev-x.md).

The main window is borderless and covers the display's usable area, leaving the
menu bar and Dock visible. It does not use a macOS full-screen Space.

## Android

An Android build is not part of this tree today. The crate is already shaped
for one: a library with a JNI entry point beside the desktop `fn main`, its
own launcher icons under `app/resources/android/`, and a grid the layout can
switch at runtime. The touch input, the fold handling, and the on-screen
keyboard are not ported. See [Open Questions](./open-questions.md).
