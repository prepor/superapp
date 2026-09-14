# Tech Stack

Superapp is a Rust application built with Makepad for macOS and Android.

One Cargo workspace, two members:

- **`kernel/`**: the package `superapp-kernel`, imported as `kernel`. It has
  **no Makepad dependency at all**, which is the layering rule made structural
  rather than agreed to. It
  carries `rusqlite`, `serde`, `serde_json`, `iroh`, and the TLS and signing
  crates R2 and the agent's gateway need.
- **`app/`**: the crate named `superapp`, a library with a one-line binary on
  top of it, which is the shape Android needs, since a desktop build starts at
  a `fn main` and an activity has no main at all. It depends on the kernel and
  on `makepad-widgets`, plus the app protocols, readers and desktop terminal
  libraries. Its minimum Rust version is 1.92.

The pieces:

- **Makepad** draws the interface and handles input. The packages are pinned in
  the root `Cargo.toml`, which also patches them to a fork with fixes for text
  input and selection, headless screenshots, canvas zoom, native playback and
  Android activity relaunch. The comment there names each patch and the revision.
- **SQLite**, through `rusqlite`, stores application data. It is bundled so
  every target uses the same version and features. Update hooks invalidate
  cached queries, SQLite's authorizer records query dependencies, and the
  session extension records the changes device sync turns into ops.
- **iroh** is [device sync](./device-sync.md)'s transport: an authenticated
  stream between two endpoint keys, over the local network or through n0's
  public relays. Its `tls-ring` feature matches the kernel's rustls pin, so the
  build has one crypto provider; it is also what sets the kernel's minimum Rust
  to 1.91.
- **Serde and serde_json** encode queued effects, device sync's frames, and the
  values its ops carry.
- **Tokio** schedules I/O services, timers and completion channels. Blocking native and CPU work uses its blocking pool.
- **async-imap, lettre, and mail-parser** provide asynchronous IMAP and SMTP, and MIME parsing.
- **html5ever, markup5ever_rcdom, and simplecss** narrow HTML mail.
- **TDLib**, through its native JSON interface, owns Telegram authorization
  and protocol traffic. The app projects updates into SQLite.
- **feed-rs** parses RSS, Atom and JSON Feed; **quick-xml** reads OPML imports.
- **chrono, chrono-tz, and rrule** handle Calendar dates, IANA time zones and
  recurrence. Google Calendar requests use the shared HTTP client.
- **Hayro** renders PDFs and supplies viewer text geometry; **pdf-extract**
  supplies PDF text for agent attachment reads.
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
frame loop instead of by background services, a scripted run binds no sync
endpoint at all, and a screenshot is a copy of the rasterizer's newest frame
rather than a photograph of a window. See [Developer Experience](./dev-x.md).

The main window is borderless and covers the display's usable area, leaving the
menu bar and Dock visible. It does not use a macOS full-screen Space.

## Android

`android.sh` builds, installs and starts the Android app using the pinned
Makepad tooling and the manifest and icons under `app/resources/android/`.
The SDK/NDK and an Android TDLib library are external prerequisites; the script
can install the SDK or build without Telegram. See
[Android build and run](./dev-x.md#android-build-and-run).

The library supplies the JNI entry point. Touch goes through the same input
paths a mouse does. The grid is
picked from the screen: 8×4 above about 600 dp and 4×3 below it, which is the
compact/medium breakpoint a fold or an unfold crosses, and `--grid` forces
either on the desktop for a preview. The workspace sits inside the safe-area
insets a window-geometry change reports, clear of the notification-shade strip
at the top; the soft keyboard's occlusion shortens it, since the manifest
adjusts nothing and the app makes its own room. Android's system Back cancels
a panel drag or closes an overlay first, then undoes the latest workspace
action. See [Interaction Grammar](./interaction-grammar.md).

Google sign-in and web links use the Android system browser; Mail and Calendar
share the phone's own grant. Device sync installs the Android context needed by
iroh's DNS resolver. Terminal and Workshop are excluded from this target.

Secrets currently use private mode-0600 files. Android Keystore integration,
Storage Access Framework access beyond the app directory, a `FileProvider`
for opening local files in another app, and system trash integration remain
open. See [Open Questions](./open-questions.md).
