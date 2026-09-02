# Developer Experience

## Build & run

```sh
mise trust && mise install
mise exec -- cargo run   # makepad comes from upstream git, pinned in Cargo.toml
```

`cargo test` runs the pure suite with no window: the panel mechanics (the
web prototype's whole smoke scenario is a test), the spring maths, the store
(schema, seed, query invalidation), the effect queue, the history tree, and
the whole mail engine — against a `World::fake()`.

A fake world is an **in-memory store, an in-memory outside, and a clock that
only moves when a test moves it**. It touches no file, no keychain, no
network and no thread, so the suite is isolated per test and parallel by
construction — nothing is shared to contend over. The mail engine's passes
run inline (`Pump::Manual`), so ingest, push and send happen in a knowable
order rather than whenever a worker thread wakes.

The store lives at `~/Library/Application Support/superapp/superapp.db`
(android: the app files dir); `--db PATH` points a run anywhere else —
`--db /tmp/scratch.db` for a throwaway session. See [The Data
Substrate](./data-substrate.md).

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

## App icons

`resources/make_icons.py` draws the icon — three panels on a workspace, the
focused header inverted, the left column two cells joined — and writes every
target from that one drawing (Pillow, plus `iconutil` for the icns):

- `resources/icon_{32..1024}.png` — the dock icon. makepad's
  `platform/build.rs` bakes them into the binary, so a plain `cargo run`
  shows them; `.cargo/config.toml` names the files (`MAKEPAD_APP_ICON_*`)
  so a custom target dir cannot lose them.
- `resources/icon.icns` / `icon.ico` — what `cargo-makepad desktop build`
  puts in a bundle, and the windows executable icon.
- `resources/android/res/` — the launcher icon, which `cargo-makepad
  android` copies into the APK: an adaptive icon (one vector drawable on a
  white background, doubling as the monochrome layer for themed icons) and
  legacy `mipmap-*/ic_launcher.png`.

Sizes at and below 64 px are laid out on the pixel grid rather than
downsampled, so the 16 px icon is three crisp boxes. `--preview DIR` writes
a contact sheet of every size on a light and a dark dock, with the android
vector rasterized the way a launcher masks it.

## The book

```sh
mise exec -- mdbook serve docs/book
```

The book is the single source of truth (see [About](./about.md)). Feature work
starts as a Change Request under `docs/planning/` and ends with that CR
deleted, its content folded into the chapters — the directory holds only what
is in flight.

## E2E harness

```sh
# the fast path: every suite, in parallel, no rendering
mise exec -- cargo run -- --e2e e2e/basic.txt --no-draw --draws 4000

# validation: render and write screenshots (one run at a time)
MAKEPAD_HEADLESS_OUT_DIR=/tmp/frames \
  mise exec -- cargo run -- --e2e e2e/basic.txt --e2e-out e2e/out --draws 4000
```

`--draws N` is makepad's, not ours: the headless backend runs a *bounded*
loop, and without it the process draws a single frame and exits before the
script's first step. N is an upper bound on draw cycles (≈ frames), so pick
one comfortably past the script's own waits — the run ends at `quit`, not at N.

An e2e run replays a line-based script against the shell's real input paths
— hit resolution, key handling, text input. Every run opens a **fresh
seeded temp store** (unless `--db` overrides it), so suites never touch your
session.

Runs go through makepad's **headless backend** (`MAKEPAD=headless`), a
software rasterizer with a virtual GPU and a shader JIT. There is no window,
no window server and no display: makepad renders the frames itself and
writes them to `MAKEPAD_HEADLESS_OUT_DIR`, and a `shot` step names the
newest one. A whole class of environmental failure cannot arise: a slept
display, a lock screen, a window occluded by another — none of them touch a
run that has no window at all, and `caffeinate -du` has nothing to keep
awake.

**Time is virtual.** Under a headless build one draw cycle is one frame of
exactly `FRAME_MS`, and that single clock drives the springs, the script's
`wait` steps *and* the app's own deadlines — the send window included, via
`Clock::Virtual` shared with the pump. Nothing reads the wall clock, so a
run is reproducible: the same script produces byte-identical screenshots
whether the machine is idle or running a dozen other suites. The pump is
`Manual` too, so ingest, push and send land on frame boundaries instead of
whenever a worker thread wakes.

**Two modes of the same suites.** Screenshots are for a human to look at,
not assertions — nothing diffs them against goldens. So `--no-draw` skips
rasterization while still running the full widget draw pass, which means
hit resolution and label matching work exactly as before at roughly **80×
less cost** (a suite in ~1 s rather than ~30 s). That is the mode to run
constantly, and it parallelises freely because it writes no frames. Turn
rendering on when you want to *see* something, and run those one at a time.

Failed steps (no element matching a label, a failed capture) make the run
exit non-zero.

```text
wait 600            # ms
shot inbox          # e2e/out/inbox.png
click "reply"       # label substring, case-insensitive
cmdclick "Q3"       # cmd held: fresh un-joined panel
mouse "filter"      # the same through the shell's real mouse-down path
key cmd+shift+left  # chords; plain letters flow as text, like real typing
key down 45         # optional repeat count
key cmd+a           # a panel accelerator (every letter parses)
type "hello"
drag "mail body" 420 0   # press-drag-release: text selection (see below)
key cmd+3           # digits work: workspace chords
key cmd 2           # a bare modifier taps (down+up); ×2 = double-cmd, the launcher
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
verify the android interactions on the desktop, `e2e/workspaces.txt` walks
the workspace grammar (switch, move-and-follow, overlay),
`e2e/launcher.txt` walks the launcher (double-cmd, open vs go-to, the
overlay's search row), `e2e/undo.txt` walks the undo tree (archive → undo →
redo, close, workspace-move). The tree itself is in memory, so `sqlite3`
shows what the actions *wrote* — folders, flags, the effect queue — rather
than the history; the tree is the running process's, and dies with it. And
`e2e/settings.txt` walks the accounts flow (add against an `.invalid` host
— the worker fails fast and locally, and its error lands on the status
line through the real signal path — then remove), `e2e/send.txt` walks the
send window (run with `--send-delay 1`: cancel inside the window, an
honest no-smtp failure, cmd+z reopening the draft), and `e2e/history.txt`
walks the history overlay (travel back, to the beginning, and forward).
`e2e/keys.txt` drives every panel accelerator, asserting through label
resolution — a walk that lands on the wrong mail fails the run, no picture
needed — `e2e/select.txt` covers selectable content, `e2e/height.txt`
reads a short letter, the demo world's long one and a short one again: the
same panel three rows tall, then six, then three, and `e2e/filter.txt`
walks the rich table (tags, the autocomplete and its dynamic values, the
grammar, the error line, and a keyboard walk onto the second page), and
`e2e/compose.txt` the compose panel's TO field completing addresses (a
pick by enter, a pick by click, esc putting the offer away, tab walking on).

`click` resolves the element's action directly — it proves the action, not
the click. `mouse` sends a real press-release pair into the stage, so the
hit lookup, the forwarding to hosted widgets and the key-focus rule all run
as they do for a physical click; `e2e/focus.txt` uses it to check that a
click inside a panel focuses the panel.

Two things a script cannot lean on. A `swipe` rides the list's real fling
physics and lands where the fling says, so it is no way to *address* a
row — walk there with `key down N`, which scroll-follows deterministically.
And under `--no-draw` no frame is written, so every `shot` step fails and
the run exits non-zero on those alone; read that mode for the *other*
failures (a label that did not resolve).

A `mouse` or `click` on a hosted **field** is trustworthy once per run.
makepad applies a key-focus change after the event that asked for it, and
releases a press's capture from the platform side — both `pub(crate)`. A
synthesized down/up pair runs inside one tick, so the field still holding
focus sees the up as a click outside itself and clears the focus the
pressed field just asked for; and the pressed field stays captured, so
every later press in the run is handed to it wherever it lands. A real
click has neither problem. Reach a second field with `key tab` (the
compose scripts do), and press at most one field per run.

`drag` is weaker than it looks: drag-selection runs on `Hit::FingerMove`,
which fires only for the area that **captured** the finger, and that
capture is platform state the harness never establishes. So a scripted drag
proves the run is addressable and that dragging breaks nothing — never that
a selection appeared. Keyboard selection (`cmd+a`) needs no capture and is
the assertion to reach for; drag-select itself belongs to a real mouse or
`e2e/cgpost.c`.

Labels address links, buttons, fields (`filter`, `to`, `subject`, `body`),
rows (by subject) and panel titles. Steps that mutate the workspace need a
`wait` after them — hits refresh on the next drawn frame. `e2e/basic.txt`
walks the whole join/replace grammar; the first frame also logs panel count
and measured cell metrics to stderr.
