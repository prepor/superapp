# Tech Stack

**Rust + Makepad** (GPU-rendered immediate-mode UI), macOS first, **android
second** (Samsung Fold — two screens, so resizes are a first-class event).
The same makepad generation as mosaic in `rel.systems`, but sourced
directly:

- `makepad-widgets` (and the apple/objc sys crates) are **git dependencies
  pinned to `prepor/makepad@superapp-pin`** — upstream's pin plus two IME
  patches (see `Cargo.toml` for what each one fixes and why). No filesystem
  coupling between repos; any checkout builds.
- **`MAKEPAD=headless`** swaps makepad's whole apple backend for a software
  rasterizer — a virtual GPU and a shader JIT — that renders frames to PNG
  itself. That is what the e2e suite runs on: no window, no window server,
  no display, so a run works over ssh and under load. See [Developer
  Experience](./dev-x.md).
- `makepad-apple-sys` / `makepad-objc-sys` for the few macOS calls makepad
  does not expose (screen geometry, activation, window screenshots).
- **`rusqlite` with bundled SQLite** — the one store (see [The Data
  Substrate](./data-substrate.md)). Bundling pins one SQLite version with
  the full hook surface — `update_hook` drives query invalidation, the
  authorizer captures each query's dependencies — on macOS and android
  alike, the same choice rel.systems' research validated. The **session
  extension** records each write's changeset into a replication log, which
  device sync carries from one device to the other. It reinstates
  `buildtime_bindgen` on the android cross-build, a known-good configuration.
  SQLite's **JSON1** functions carry the effect queue's payloads (TEXT, not
  JSONB — a shell must be able to read them), and `json_each` is how the
  effect log joins the in-memory ring to the queue: a scalar function
  registered on every reader hands the ring over as one array, so a page of
  the log is still one query.
- **`serde` + `serde_json`** — effects are serializable values, and the
  deferred ones are JSON payloads in the `effect` table; the device-sync
  `state` object and batch headers are JSON too.
- **No new crate for device sync**, even for the real bucket. The transport
  (CR-005) is a plain-HTTP client and a tiny `bucketd` daemon over `std::net`
  for the local demo; the R2 backend is the same wire plus TLS and AWS SigV4,
  built from crates the graph already carried under imap/lettre's
  `rustls-tls` — `rustls` (ring, never aws-lc-rs: a new C toolchain on the
  android cross-build), `webpki-roots`, and `ring` for SHA-256 and
  HMAC-SHA256. The signing itself is ours, some eighty lines, pinned to the
  AWS test vector. Content hashing for snapshot integrity is a
  dependency-free FNV-1a.
- **`imap` (rustls) + `mail-parser`** — the sync engine's protocol and
  MIME layers; the TLS stack is rustls-on-ring, which cross-compiles for
  android without ceremony. All engine logic runs against an `Outside`
  backend, so the whole sync/reconcile machinery is unit-tested against an
  in-memory fake server, keychain and clock.
- **Gmail sign-in adds no HTTP client.** OAuth needs two POSTs to Google's
  token endpoint, and that is written on `rustls-connector` — the TLS layer
  `imap`'s rustls feature already pulls in. `ring` (rustls' own crypto)
  carries PKCE's SHA-256 and its randomness, and `base64` is the XOAUTH2
  envelope. All three were already in the graph; promoting them to direct
  dependencies costs nothing to compile and is honest about what is used.
- The **macOS menu bar** is makepad's own `MacosMenu` API
  (`cx.update_macos_menu`, clicks arrive as `Event::MacosMenuCommand`); the
  workspace menus rebuild only when the roster or the active space changes.
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

The soft keyboard uses makepad's full-state IME protocol: when it shows,
makepad slides the whole render pass up by the keyboard height (the
turtle's origin goes negative), and the shell cancels that shift and
shortens the viewport by the same `Event::VirtualKeyboard(DidShow {
height })` — so the workspace relayouts into exactly the visible region
above the keyboard.
Typed text arrives as `TextInput { full_state_sync }` — the IME-side
editable is authoritative and the app mirrors it wholesale — while the app
seeds and re-syncs that editable with `sync_ime_state` on focus and after
its own edits (never mid-composition). A user-dismissed keyboard latches
makepad's `text_ime_dismissed`; the shell's hide-on-blur resets it, which is
what lets the next field tap re-show the keyboard. Android also swallows
touches in the notification-shade zone at the very top of the window, so the
shell enforces a 28 dp top inset there — headers must stay tappable, they
hold the drag grip and the buttons.

## Why not web

The first prototype (`web/`, kept for reference) was web and validated the
interaction model cheaply. The real thing is native because the product is a
window-manager-shaped program: it wants the GPU, real keyboard ownership,
process-level integration (terminals, agents), and latency a browser cannot
promise. The web prototype is not maintained.
