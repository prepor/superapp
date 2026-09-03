# Tech Stack

Superapp is a Rust application built with Makepad. macOS is the main target;
Android is also supported, with special handling for foldable screens.

- **Makepad** draws the interface and handles input. The Makepad packages are
  pinned in `Cargo.toml`. A small fork carries five local fixes for text input,
  headless tests, canvas zoom, and text selection. `Cargo.toml` explains each
  patch and names the exact revision.
- **SQLite**, through `rusqlite`, stores application data. SQLite is bundled so
  macOS and Android use the same version and features. Update hooks invalidate
  cached queries, SQLite records query dependencies, and the session
  extension records changes for device sync.
- **Serde and serde_json** encode queued effects and device-sync metadata.
- **imap, lettre, and mail-parser** provide IMAP, SMTP, and MIME support.
- **rustls** provides TLS. `ring`, `base64`, `rustls-connector`, and
  `webpki-roots` support Gmail sign-in and signed R2 requests. The app does not
  use a general-purpose HTTP client.
- **Makepad's macOS APIs** provide the menu bar. Small gaps such as screen
  geometry and window screenshots use `makepad-apple-sys` and
  `makepad-objc-sys`.
- **mise** selects the stable Rust toolchain. The application has no other
  runtime dependency.

Setting `MAKEPAD=headless` replaces macOS drawing with Makepad's software
renderer. End-to-end tests can then run without a window or display and can
write frames directly to PNG files. See [Developer Experience](./dev-x.md).

The main window is borderless and covers the display's usable area, leaving the
menu bar and Dock visible. It does not use a macOS full-screen Space.

## Android

The library contains Makepad's Android entry point. The desktop binary and
macOS-only code are excluded from Android builds. `cargo-makepad` builds the
APK without Gradle.

Panel widgets use the bundled Geist Mono font. Shell-drawn text falls back to
bundled Liberation Mono because Menlo is not available on Android.

Fold and unfold events arrive as window-size changes. At about 600 dp wide, the
layout switches between the 8×4 unfolded grid and the 4×3 cover-screen grid.
Safe-area insets are removed from the available space before panels are laid
out.

Touch uses Makepad's raw touch events. Android's gesture detector supplies the
long-press event. The shell also keeps a 28 dp top inset so the system gesture
area does not cover panel headers.

The on-screen keyboard reports its height and full text state. The shell uses
the remaining visible area for the workspace and mirrors the full text state
into the focused field. It updates Android's editable state after app-side
changes, but not while text composition is active.
