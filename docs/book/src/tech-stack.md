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
- **imap, lettre, and mail-parser** provide IMAP, SMTP, and MIME support.
- **html5ever, markup5ever_rcdom, and simplecss** narrow HTML mail.
- **rustls** provides TLS. `ring`, `base64`, `rustls-connector`, and
  `webpki-roots` support Gmail sign-in, signed R2 requests, and the agent's
  gateway. The app does not use a general-purpose HTTP client.
- **Makepad's macOS APIs** provide the menu bar. Small gaps such as screen
  geometry, the trash, and window screenshots use `makepad-apple-sys` and
  `makepad-objc-sys`, in `app/src/platform/mac.rs`.
- **The system's own file watch** tells the file panels about writes that were
  not ours: FSEvents through CoreServices on macOS, inotify on android, both
  declared where they are used in `app/src/platform/watch/`. No crate — the
  whole of each is a handful of foreign functions.
- **mise** selects the stable Rust toolchain. The application has no other
  runtime dependency.

There are no Cargo features. Every switch is argv, an environment variable, or
`cfg(headless)`.

## Still no HTTP client

`kernel::http` is a hand-rolled HTTP/1.1 client and `kernel::sse` is the
server-sent-events framing over its body reader. Together they are what an
[agent](./agents.md#no-library-one-small-client)'s long streamed answer arrives
through.

They exist because the alternative is a dependency this tree cannot take:
`ureq`, `reqwest` and their kin bring an async runtime or a second TLS stack,
and Android must build the same crate. What is actually needed is one verb, one
host, no redirects, one long body — the size of the two clients this tree
already hand-rolls for Gmail sign-in and for R2. So the third one is small
enough to read in one sitting: a request with headers and a body; a response as
a status, its headers, and a body that undoes `Transfer-Encoding: chunked` as
it arrives rather than at the end; timeouts on connect, on the first byte and
between bytes. The connection is verified against the Mozilla roots and not the
machine's, because a phone has no machine roots to verify against.

The parsing is split from the socket on purpose — the head reader, the chunked
reader and the event framing are driven by tests over an in-memory cursor — so
the wire's edge cases are pinned without a network, including the one that
bites: a multibyte character divided between two frames, which is why a line is
never turned into text until its newline has arrived.

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
adjusts nothing and the app makes its own room. See [Interaction
Grammar](./interaction-grammar.md).

What is left is what needs a device or an SDK to write against: a secrets
backend that is not a private file, and everything the file browser wants
outside the app's own directory: the Storage Access Framework, a
`FileProvider` for the system opener, and `MediaStore` for the system trash.
See [Open Questions](./open-questions.md).
