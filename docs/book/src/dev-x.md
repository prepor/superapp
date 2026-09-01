# Developer Experience

## Build & run

```sh
mise trust && mise install
mise exec -- cargo run   # makepad comes from upstream git, pinned in Cargo.toml
```

`cargo test` runs the pure-core suite — the panel mechanics (the web
prototype's whole smoke scenario is a test) and the spring maths — with no
window.

## Android build & run

```sh
# once: cargo-makepad + SDK/NDK (~4 GB, self-contained under ~/.cache)
cargo +stable install --path ../rel.systems/mosaic/third_party/makepad/tools/cargo_makepad --locked
cargo-makepad android --sdk-path=$HOME/.cache/makepad-android-sdk install-toolchain

# build the APK / install & launch on the connected device
cargo-makepad android --sdk-path=$HOME/.cache/makepad-android-sdk \
  --package-name=dev.prepor.superapp --app-label=superapp build -p superapp
cargo-makepad android --sdk-path=$HOME/.cache/makepad-android-sdk \
  --package-name=dev.prepor.superapp --app-label=superapp run -p superapp
```

Note: `cargo-makepad` shells out to **rustup's** `stable` (mise and rustup
share `~/.rustup` here; the rustup *default* is old — always `+stable`).

## Phone preview on desktop

The android grids and every touch gesture run on macOS too:

```sh
mise exec -- cargo run -- --window 380x780 --grid 4x3   # cover display
mise exec -- cargo run -- --grid 8x4                     # unfolded, full frame
```

## The book

```sh
mise exec -- mdbook serve docs/book
```

The book is the single source of truth (see [About](./about.md)). Feature work
starts as a Change Request under `docs/planning/`.

## E2E harness

```sh
mise exec -- cargo run -- --e2e e2e/basic.txt   # add --front to watch it
```

An e2e run replays a line-based script against the shell's real input paths —
hit resolution, key handling, text input — and captures window-layer
screenshots to `e2e/out/`. The window sits behind everything, click-through,
so a run never takes the screen — but on plain upstream makepad an occluded
window skips presents, so **screenshots from a background run are stale;
run with `--front` when the pictures matter**. Steps themselves (hits,
labels, keys) work either way. Failed steps (no element matching a label,
failed capture) make the run exit non-zero.

```text
wait 600            # ms
shot inbox          # e2e/out/inbox.png
click "reply"       # label substring, case-insensitive
cmdclick "Q3"       # cmd held: fresh un-joined panel
key cmd+shift+left  # chords; plain letters flow as text, like real typing
key j 45            # optional repeat count
type "hello"
key cmd+3           # digits work: workspace chords
swipe "inbox" 0 -160   # one-finger touch drag from the element's centre
pan2 -400              # two-finger workspace pan
pan2 0 260             # …vertical: swipe down (workspaces overlay); 0 -260 up
holdmove "help" 500 0  # long-press the panel's header, drag, drop
holdmove "help" 500 0 hold   # …or keep holding (screenshot the preview)
drop                   # release a held drag
quit
```

The touch steps drive the same gesture state machine android uses, so
`e2e/touch.txt` and `e2e/phone.txt` (run with `--window 380x780 --grid 4x3`)
verify the android interactions on the desktop, and `e2e/workspaces.txt`
walks the workspace grammar (switch, move-and-follow, overlay).

Labels address links, buttons, fields (`filter`, `to`, `subject`, `body`),
rows (by subject) and panel titles. Steps that mutate the workspace need a
`wait` after them — hits refresh on the next drawn frame. `e2e/basic.txt`
walks the whole join/replace grammar; the first frame also logs panel count
and measured cell metrics to stderr.
