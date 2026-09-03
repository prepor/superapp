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

## CI

`.github/workflows/ci.yml` runs the linter and that same suite on every push
to `main` and every pull request:

```sh
cargo clippy --all-targets --locked -- -D warnings
cargo test --locked
```

On a **macOS runner**, because macOS is the target: the apple sys crates and
the keychain in `src/secret.rs` are cfg'd to it, and a linux job would be
checking a graph that never ships. One job, not a lint job beside a test job
— building the makepad graph is the whole cost of a run, and two would pay
it twice. `--locked` makes a stale `Cargo.lock` fail loudly instead of being
re-resolved behind the commit.

Then the e2e battery — `e2e/run-all.sh`, every single-process suite in the
fast `--no-draw` mode, a couple of seconds for the lot. It runs last because
`MAKEPAD=headless` is a build-time switch, so it re-does the four crates that
read it and leaves the earlier steps' artifacts alone until then. The
device-sync suites stay out: they need a second device and a bucketd between
them, and `e2e/sync-demo.sh` / `e2e/reseed.sh` drive those by hand.

Clippy is a *gate*: `-D warnings`, and the tree is clean of them. rustfmt is
not — the layout here is hand-set (comment columns, tables that line up), so
there is no `cargo fmt --check` step and `cargo fmt` on the whole tree is
not a thing to run.

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
# the whole battery, in parallel, no rendering — what CI runs
MAKEPAD=headless mise exec -- cargo build && ./e2e/run-all.sh

# one suite, the same fast path
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
session. The one thing a store cannot seed is the **disk**: `--demo-disk`
gives the file browser the demo tree the [panels library](#panels-library)
shows instead of this machine’s own, so a files suite can address a row by
name. It takes the writes too, which is why forgetting the flag is safe:
a script replayed against a **real** disk has the browser's four writing
verbs sealed, and each one refuses on the status line. A suite must no
more delete your files than write to your keychain — the same reason a
run's passwords live in memory. Nothing else changes: the network and the
screenshots are the ones a run always had.

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
less cost** (a suite in ~1 s rather than ~30 s). A `shot` step is *skipped*
there rather than failed, which is what makes the mode a gate: the exit code
means every step that could run, ran. That is the mode to run constantly, and
it parallelises freely because it writes no frames — `e2e/run-all.sh` launches
every suite at once and reports a line each. Turn rendering on when you
want to *see* something, and run those one at a time.

Failed steps (no element matching a label, a failed capture) make the run
exit non-zero.

```text
wait 600            # ms
shot inbox          # e2e/out/inbox.png
click "reply"       # by label, case-insensitive and ranked (below)
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
overlay's search row), `e2e/cascade.txt` walks the join's cascade (a chain
built, its parent closed, the whole chain back on ⌘z — the contact link
resolving is the assertion, since it can only resolve while the message
panel stands), `e2e/undo.txt` walks the undo tree (archive → undo →
redo, close, workspace-move). The tree itself is in memory, so `sqlite3`
shows what the actions *wrote* — folders, flags, the effect queue — rather
than the history; the tree is the running process's, and dies with it. And
`e2e/settings.txt` walks the accounts flow (add against an `.invalid` host
— the worker fails fast and locally, and its error lands on the status
line through the real signal path — then remove), `e2e/oauth.txt` presses
*sign in with google* and reads back the line the shell answers with (a run
never leaves for a browser, so the flow refuses itself; what a script can
prove is the button, the action and the status line, and the rest lives in
`oauth`'s unit tests), `e2e/send.txt` walks the send window (run with
`--send-delay 1`: cancel inside the window, an honest no-smtp failure,
cmd+z reopening the draft), and `e2e/history.txt` walks the history
overlay (travel back, to the beginning, and forward).
`e2e/keys.txt` drives every panel accelerator, asserting through label
resolution — a walk that lands on the wrong mail fails the run, no picture
needed — `e2e/select.txt` covers selectable content, `e2e/height.txt`
reads a short letter, the demo world's long one and a short one again: the
same panel three rows tall, then six, then three, and `e2e/filter.txt`
walks the rich table (tags, the autocomplete and its dynamic values, the
grammar, the error line, and a keyboard walk onto the second page), and
`e2e/compose.txt` the compose panel's TO field completing addresses (a
pick by enter, a pick by click, esc putting the offer away, tab walking on),
`e2e/marks.txt` the marks (space, a shift+arrow range, `all`, a
mark the filter hides, cmd+a still select-all in a live field, and a batch
archive undone back to marked), `e2e/files-marks.txt` (with `--demo-disk`)
the same marks over a directory — the object verbs a joined files panel
wears, then the bar taking them while rows are marked, a dot-file marked
under `@hidden` and still drawn above the rows once the filter leaves it
out, `⌘p` holding the whole set and `copy here` performing it, and a
`move here` refusing it path by path. `e2e/files.txt` (with `--demo-disk`)
walks the verbs that write: `new dir` and the row it made, a `delete` found
again in the trash by `go to`, a copy beside itself under a free name, a
move proved by the card's own path line, and `⌘z` after each. It is the
suite that leans hardest on the clicks being the assertions — where two
panels could both be showing a row of that name, the card's path settles
which. `e2e/attach.txt` (also with `--demo-disk`) walks both directions of
what a letter carries: the seeded letter's part as a link, the card over it
and its preview, `open` handing it out; then a file held with `copy` in the
browser, `attach` on the compose that appears only because something is
held, the `CARRIES` link to the file's own card, and `cmd+z` taking the
attach back off. `e2e/problems.txt` (with `--send-delay 1
--draws 100000` — it waits out the executor's backoff in virtual time) walks
the problems surface: a send the demo account cannot make raises the mark, the
mark opens the panel, *retry* files it again, *reopen* brings the draft back
and `cmd+z` takes that back; an account against an `.invalid` host joins the
list, *sync* kicks it, and removing the account clears it.
`e2e/effects.txt` (run with `--send-delay 1`) walks the effect log — the
same failed send, seen from the other end: the empty queue, then the job it
files, addressed by *the sentence its effect describes itself with* (which
is how a log that stopped naming its jobs fails the run), and touching it
previews the job panel the run then reads and selects from.

`click` resolves the element's action directly — it proves the action, not
the click. `mouse` sends a real press-release pair into the stage, so the
hit lookup, the forwarding to hosted widgets and the key-focus rule all run
as they do for a physical click; `e2e/focus.txt` uses it to check that a
click inside a panel focuses the panel.

Two things a script cannot lean on. A `swipe` rides the list's real fling
physics and lands where the fling says, so it is no way to *address* a
row — walk there with `key down N`, which scroll-follows deterministically.
And under `--no-draw` no frame is written, so a `shot` step is skipped: that
mode proves the *other* things (a label that did not resolve), never a
picture.

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
rows (by subject) and panel titles. A label is matched **ranked** rather
than as a plain substring: a whole-label match first, then one that
*starts* with what was asked, then one that merely contains it — the
tightest label winning inside a rung, and the last registered on a full
tie, so a control drawn over another takes the click exactly as it would
from a pointer — `click "archive"` is the message panel's own button, not
the marks bar's `archive marked` beside it (CR-009).
Steps that mutate the workspace need a
`wait` after them — hits refresh on the next drawn frame. `e2e/basic.txt`
walks the whole join/replace grammar; the first frame also logs panel count
and measured cell metrics to stderr.

## Panels library

In the app, **Dev → Panels Library** (⇧⌘L) puts the library up over the
workspace and takes it down again — the chord works from the canvas as
well as from the workspace; the workspace underneath keeps its store, sync
and script running. A window opened on the library carries the Dev menu
from the start, and the workspace boots the first time the toggle asks for
it. From the command line:

```sh
# open on the library — every scene of the catalogue, live, on one canvas
mise exec -- cargo run -- --library

# the scenes whose names contain these
mise exec -- cargo run -- --library "inbox row" message

# the canvas's own suite — the fast path: mounts, steps and labels, no rendering
MAKEPAD=headless mise exec -- cargo run -- --library --e2e e2e/library.txt --no-draw --draws 3000

# …and rendered, for screenshots (the canvas is a big frame)
MAKEPAD=headless MAKEPAD_HEADLESS_DPI=1 MAKEPAD_HEADLESS_OUT_DIR=/tmp/frames \
  mise exec -- cargo run -- --library --e2e e2e/library.txt --e2e-out e2e/out --draws 3000
```

Under `--no-draw` a `shot` is logged and skipped rather than failed, for the
canvas and the workspace suites alike, so a green fast-path run means what
it says.

`--library` opens the window on an **infinite canvas** instead of the
workspace, showing the **catalogue** (`src/catalog.rs`): one **scene** per
subject, each the states worth a look while that subject is being worked
on. The e2e suites are not the source — they check behaviour, one walk per
file, and a design review wants the same thing in its variants, which fans
out. So a scene is a DAG: **nodes** are named states, **edges** say what
takes one to another, notes annotate both, and the layout is layered from
it — roots left, a fan-out stacked in a column, arrows with elbows.

A node is one of three things:

- a **component** — a bare widget (an inbox row, a thread message, an
  overlay row, the launcher sheet, an account row, a problem row, an effect
  row, a link) from the library's template, populated once with a fixture
  through its own API. No store, no clock; a texture the size of the piece.
- a **panel** — one panel widget on a world of its own (an in-memory store
  with the demo seed, a sealed `Deny` outside, virtual time), chrome
  included: the stage comes up *solo* on that panel and draws it at the
  whole viewport. Enter it and the keys work — the walk, ⌘a, ⌘z. A subject
  the demo seed does not cover plants its own rows on the way up: the
  effect log's scene files five real jobs, in states the executor will not
  revisit, so the queue it shows stands still while it is read — and its
  job nodes end by touching the sentence they drew, so a panel that stopped
  naming its effect fails to arrive rather than showing an empty page. The
  effect-row scene shows the ring's two shapes beside the queue's, built
  from real in-memory effects the same way.
- the **workspace** — the whole stage, kept for the shell's own subjects:
  joins, tabs, the phone grid.

A panel or workspace node may name a few steps in the harness's grammar,
inline, that lead to its state (`key down 3`; `click "filter"` then `type
"github"`). Those replay one node at a time (there is one keyboard), one
step per frame, fast-forwarded through waits: the whole catalogue fills in
within a few seconds, and stderr reports when the last node arrives.
Components are their state from the first draw.

Scenes are Rust, not a text file: fixtures are the real structs
(`ThreadHead`, `ThreadMail`, `OverlayRowData`, `mail::Move`…) and a state is
set through the widget's own methods, so a refactor that breaks a scene
fails to compile rather than quietly rearranging the canvas. The catalogue's
test checks that every scene is a DAG with a name per state; the shape and the
layout are pure (`src/scene.rs`), unit-tested without a window. Adding a
state is one line in its subject's function.

A node that has arrived is **frozen**: a picture that hears no events and
asks for no frames until you enter it. Rendering is budgeted per frame,
and a zoom change re-renders nothing on the spot — nodes show their last
texture scaled, and re-render crisp at the new level once the zoom has
stood still, nearest the pointer first — so panning and zooming stay
smooth however many nodes are on the canvas. Mounts render into their own
passes at the canvas's zoom, so text is crisp at every level rather than
scaled — except an entered stage at 1:1, which is drawn straight into the
window: a texture pass and its composite would double the GPU work of
every animated frame, and a stage worked by hand animates on every beat.

- **Pan**: drag the canvas, or scroll. **Zoom**: ⌘scroll around the
  pointer, ⌘= / ⌘- in steps, ⌘0 fits everything. Arrow keys pan.
- **Enter** a node with a click (on it, or its name): the camera flies to
  1:1 and the keyboard and pointer go to that mount, remapped into its own
  coordinates. An entered node runs on the wall clock, like the app;
  replays and headless runs keep the fixed frame step. Click outside it,
  or ⌘esc, to leave. A scene's name fits its block.
- The legend along the bottom spells all of it out.

`e2e/library.txt` drives the canvas itself: `wait`, `shot`, `click` on a
scene's name or a node's (`scene/node`), and the canvas chords. A step
that fails inside a node's replay is reported on stderr under the node's
name and counts against the run. `e2e/library-toggle.txt` is a workspace
suite: it presses ⇧⌘L over a running stage and back.

`SUPERAPP_FRAME_LOG=1` prints every frame's draw cost (the canvas's, and
what was spent inside mount renders, with the interval since the last
frame) and every event that took over a millisecond, for the library and
the app alike — the first thing to read when a window feels slow.
