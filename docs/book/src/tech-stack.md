# Tech Stack

**Rust + Makepad** (GPU-rendered immediate-mode UI), macOS first, **android
second** (Samsung Fold — two screens, so resizes are a first-class event).
The same makepad generation as mosaic in `rel.systems`, but sourced
directly:

- `makepad-widgets` (and the apple/objc sys crates) are **git dependencies
  on upstream `makepad/makepad`, pinned to the same commit mosaic's vendor
  pipeline uses** — no filesystem coupling between the repos, any checkout
  builds. This is *plain* upstream: mosaic's three local patches are not
  applied. The one superapp used was 0003 (present-while-occluded); without
  it a fully covered window skips presents, so **e2e screenshots are only
  meaningful in `--front` runs** — suite logic (hits, labels, steps) is
  unaffected. If that trade stops being worth it, the fallback is a pushed
  branch of the patched tree and the same `{ git, rev }` shape.
- `makepad-apple-sys` / `makepad-objc-sys` for the few macOS calls makepad
  does not expose (screen geometry, activation, window screenshots).
- **`rusqlite` with bundled SQLite** — the one store (see [The Data
  Substrate](./data-substrate.md)). Bundling pins one SQLite version with
  the full hook surface (`update_hook` drives query invalidation; the
  session extension records the undo changesets) on macOS and android
  alike — the same choice rel.systems' research validated.
- **`imap` (rustls) + `mail-parser`** — the sync engine's protocol and
  MIME layers; the TLS stack is rustls-on-ring, which cross-compiles for
  android without ceremony. All engine logic hides behind a `Transport`
  trait, so the whole sync/reconcile machinery is unit-tested against an
  in-memory fake server.
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
