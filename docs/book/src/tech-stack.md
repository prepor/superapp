# Tech Stack

Superapp is a Rust application built with Makepad. macOS is the target.

One Cargo workspace, two members:

- **`kernel/`**: the crate named `kernel`. It has **no Makepad dependency at
  all**, which is the layering rule made structural rather than agreed to. It
  carries `rusqlite`, `serde`, `serde_json`, and the TLS and signing crates
  device sync and the agent's gateway need.
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
- **Tokio** schedules I/O services, timers and completion channels. Blocking native and CPU work uses its blocking pool.
- **async-imap, lettre, and mail-parser** provide asynchronous IMAP and SMTP, and MIME parsing.
- **html5ever, markup5ever_rcdom, and simplecss** narrow HTML mail.
- **reqwest** provides pooled streaming HTTP over **rustls**. `ring`,
  `base64`, and `webpki-roots` support TLS, OAuth and signed R2 requests.
- **Makepad's macOS APIs** provide the menu bar. Small gaps such as screen
  geometry, the trash, and window screenshots use `makepad-apple-sys` and
  `makepad-objc-sys`, in `app/src/platform/mac.rs`.
- **The system's own file watch** tells the file panels about writes that were
  not ours: FSEvents through CoreServices on macOS, inotify on android, both
  declared where they are used in `app/src/platform/watch/`. No crate — the
  whole of each is a handful of foreign functions.
- **libghostty-vt** maintains terminal state, with **portable-pty** for the
  local shell and Makepad for drawing. Both are excluded on Android.
- **mise** selects the stable Rust toolchain and Zig 0.15.2 for the terminal's
  static library. Zig is a build dependency only; see [Terminal](./terminal.md).

The default `tdlib` Cargo feature links TDLib. `--no-default-features`
keeps deterministic Telegram fixtures available without the native library.
Runtime switches remain argv and environment variables.

## Asynchronous services

`kernel::runtime` owns a Tokio pool and a local service executor. A service
owns its world and read connection; neither crosses an await into another
thread. Network waits suspend tasks. Finite filesystem, document parsing,
image rendering and native keychain operations run on the blocking pool.
SQLite's single writer and TDLib's process-wide receive loop retain dedicated
threads because their native APIs block. Platform event loops remain native.

`kernel::http` wraps reqwest with explicit connection, first-byte and idle
budgets, streaming body limits and per-use redirect policy. Authenticated
requests do not follow redirects or automatically retry. RSS has its own
bounded redirect policy. `kernel::sse` frames the asynchronous body and
retains partial events across cancellation. Local HTTP servers and in-memory
streams exercise framing, truncation, timeouts and cancellation without
external accounts.

The [async I/O experiment](./async-io.md) records the migration boundaries and
measured results. Async scheduling moves waits away from input handling;
it does not make SQLite queries or document rendering intrinsically faster.

## Headless

Setting `MAKEPAD=headless` at build time replaces macOS drawing with Makepad's
software renderer, and `app/build.rs` mirrors it into `cfg(headless)` for this
crate, because the shell has to know which backend it is linked against: a
window-layer screenshot is meaningless when there is no window.

`cfg(headless)` is what turns on virtual time. Worker futures are driven from the
frame loop instead of by background services, the device-sync driver does too, and a
screenshot is a copy of the rasterizer's newest frame rather than a photograph
of a window. See [Developer Experience](./dev-x.md).

The main window is borderless and covers the display's usable area, leaving the
menu bar and Dock visible. It does not use a macOS full-screen Space.

## Android

An Android build is not part of this tree today, and there is no SDK here to
make one with. The crate is shaped for it: a library with a JNI entry point
beside the desktop `fn main`, its own launcher icons under
`app/resources/android/`, and a grid the layout switches at runtime.

The platform work the shell owns is written and compiles here behind its
`cfg`s. Touch goes through the same input paths a mouse does. The grid is
picked from the screen: 8×4 above about 600 dp and 4×3 below it, which is the
compact/medium breakpoint a fold or an unfold crosses, and `--grid` forces
either on the desktop for a preview. The workspace sits inside the safe-area
insets a window-geometry change reports, clear of the notification-shade strip
at the top; the soft keyboard's occlusion shortens it, since the manifest
adjusts nothing and the app makes its own room. Android's system Back cancels
a panel drag or closes an overlay first, then undoes the latest workspace
action. See [Interaction Grammar](./interaction-grammar.md).

What is left is what needs a device or an SDK to write against: a secrets
backend that is not a private file, and everything the file browser wants
outside the app's own directory: the Storage Access Framework, a
`FileProvider` for the system opener, and `MediaStore` for the system trash.
See [Open Questions](./open-questions.md).
