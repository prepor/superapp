# Developer Experience

## Build and run

```sh
mise trust && mise install
mise exec -- cargo run
```

The Makepad revision and local patches are pinned in `Cargo.toml`.

Run unit tests without opening a window:

```sh
mise exec -- cargo test
```

Tests use `World::fake()`: an in-memory database, fake outside services, and a
clock controlled by the test. They do not use files, the keychain, network, or
worker threads. Mail passes run in the calling thread so their order is
predictable.

The normal database is
`~/Library/Application Support/superapp/superapp.db`. Android uses the app's
files directory. `--db PATH` selects another file.

## CI

The macOS job in `.github/workflows/ci.yml` runs:

```sh
cargo clippy --all-targets --locked -- -D warnings
cargo test --locked
MAKEPAD=headless cargo build --locked
./e2e/run-all.sh
```

The end-to-end suites run last because `MAKEPAD=headless` changes part of the
build. Device-sync demos are excluded because they need two app processes and a
separate object-store server.

CI uses one job to avoid compiling Makepad twice. `--locked` rejects a stale
lockfile. The project does not run `cargo fmt --check`; several tables and
comments are intentionally aligned by hand.

## Android build and run

Install the tool once:

```sh
cargo +stable install \
  --path ../rel.systems/mosaic/third_party/makepad/tools/cargo_makepad \
  --locked
cargo-makepad android \
  --sdk-path=$HOME/.cache/makepad-android-sdk install-toolchain
```

Build or run on a connected device:

```sh
cargo-makepad android --sdk-path=$HOME/.cache/makepad-android-sdk \
  --package-name=dev.prepor.superapp --app-label=superapp build -p superapp
cargo-makepad android --sdk-path=$HOME/.cache/makepad-android-sdk \
  --package-name=dev.prepor.superapp --app-label=superapp run -p superapp
```

Use `+stable` when installing because `cargo-makepad` calls Rustup directly.

## Phone preview on desktop

```sh
mise exec -- cargo run -- --window 380x780 --grid 4x3
mise exec -- cargo run -- --grid 8x4
```

These modes use the Android grids and touch behavior on macOS.

## App icons

`resources/make_icons.py` creates all icons from one drawing:

- `resources/icon_{32..1024}.png` for normal application runs;
- `resources/icon.icns` and `resources/icon.ico` for desktop bundles;
- `resources/android/res/` for adaptive and legacy Android launcher icons.

Small icons are drawn on the pixel grid instead of scaled down. Use
`--preview DIR` to create a contact sheet for review.

## The book

```sh
mise exec -- mdbook serve docs/book
```

The book describes the current product. Feature work starts with a change
request under `docs/planning/`. Move lasting information into the book and
delete the change request when the implementation is complete.

## End-to-end tests

Run all suites without rendering pixels:

```sh
MAKEPAD=headless mise exec -- cargo build && ./e2e/run-all.sh
```

Run one suite:

```sh
mise exec -- cargo run -- --e2e e2e/basic.txt --no-draw --draws 4000
```

Render screenshots for review:

```sh
MAKEPAD=headless MAKEPAD_HEADLESS_OUT_DIR=/tmp/frames \
  mise exec -- cargo run -- --e2e e2e/basic.txt \
  --e2e-out e2e/out --draws 4000
```

`--draws` belongs to Makepad's headless mode. It sets a maximum number of
draw cycles; the script normally ends earlier at `quit`.

Each run uses a new seeded temporary database unless `--db` is present. File
tests use `--demo-disk`, a writable in-memory tree. Without that flag, scripted
file writes are refused, which protects the real disk.

Headless mode uses Makepad's software renderer and needs no window or display.
Time is fixed per frame, including animations, waits, send deadlines, and the
manual mail pump. This makes runs repeatable and screenshots stable.

`--no-draw` still runs widget layout and hit registration but skips pixel
rendering. Screenshot steps are skipped rather than failed. Use this mode as the
fast behavior check, and render one suite at a time when reviewing visuals.

Failed steps, including missing labels or failed captures, make the process exit
with a non-zero status.

### Script commands

```text
wait 600
shot inbox
click "reply"
cmdclick "Q3"
mouse "filter"
key cmd+shift+left
key down 45
type "hello"
drag "mail body" 420 0
swipe "inbox" 0 -160
pan2 -400
pan2 0 260
holdmove "help" 500 0
holdmove "help" 500 0 hold
drop
quit
```

Commands use labels from links, buttons, fields, rows, and panel titles. Labels
match without case. Exact matches rank above prefixes, which rank above other
substring matches. If two matches are otherwise equal, the last drawn one wins,
as it does for pointer hit testing.

Add `wait` after workspace changes because hit areas update on the next frame.
Use keyboard movement to select a distant list row; touch swipes include fling
physics and are not stable row selectors.

`click` invokes the resolved action. `mouse` sends a press and release through
the stage, which also tests focus and input routing. A synthesized mouse click
on hosted text fields is reliable only once per run because Makepad's private
capture and delayed focus state cannot be reset by the harness. Reach later
fields with `tab`.

Scripted `drag` does not prove that text became selected because Makepad starts
selection only for a platform-captured pointer. Use `cmd+a` for a selection
assertion or test drag selection with a real pointer.

The suites under `e2e/` cover panel placement and joins, focus, workspaces,
launcher, undo history, settings and OAuth entry, mail send and sync, filters,
marks, files, attachments, problems, the Effects view, touch, phone layouts,
selectable text, and the Panels Library. Device-sync workflows use
`e2e/sync-demo.sh` and `e2e/reseed.sh` separately.

## Panels Library

Use **Dev → Panels Library** (`shift+cmd+l`) in the app, or start there from the
command line:

```sh
mise exec -- cargo run -- --library
mise exec -- cargo run -- --library "inbox row" message
```

Run its own end-to-end suite without or with rendering:

```sh
MAKEPAD=headless mise exec -- cargo run -- \
  --library --e2e e2e/library.txt --no-draw --draws 3000

MAKEPAD=headless MAKEPAD_HEADLESS_DPI=1 \
  MAKEPAD_HEADLESS_OUT_DIR=/tmp/frames \
  mise exec -- cargo run -- --library --e2e e2e/library.txt \
  --e2e-out e2e/out --draws 3000
```

The library shows live scenes from `src/catalog.rs` on an infinite canvas. A
scene is a graph of named states and the steps between them. Its nodes can be:

- a component widget with fixture data;
- one panel in its own fake world;
- a full workspace for layout behavior.

Panel and workspace nodes may replay short steps from the end-to-end grammar.
Components start in their final state. Scenes use real Rust types and widget
APIs, so incompatible refactors fail to compile. Pure graph and layout rules
live in `src/scene.rs` and have unit tests.

Completed nodes stop receiving events and frames until entered. The canvas
delays sharp rerendering during zoom so panning remains smooth. Click a node to
enter it at 1:1 and send it keyboard and pointer input. Click outside or press
`cmd+esc` to leave.

- Drag or scroll to pan.
- Use `cmd+scroll`, `cmd+=`, or `cmd+-` to zoom.
- Use `cmd+0` to fit all scenes.
- Use arrow keys to pan.

`e2e/library.txt` can click a scene or `scene/node` label and use the same canvas
shortcuts. `e2e/library-toggle.txt` tests switching between the library and a
running workspace.

Set `SUPERAPP_FRAME_LOG=1` to print drawing cost and events that take more than
one millisecond.
