# Developer Experience

## Build and run

```sh
mise trust && mise install
mise exec -- cargo run -p superapp
```

mise supplies the stable Rust toolchain and nothing else. The Makepad revision
and its local patches are pinned in the root `Cargo.toml`.

The normal database is
`~/Library/Application Support/superapp/superapp.db`. `--db PATH` selects
another file, and `--bucket URL` points a run at a
[device-sync](./device-sync.md) bucket.

## Tests

```sh
mise exec -- cargo test --workspace
```

Both crates, no window, no network, no keychain. The kernel's tests are the
panel mechanics, springs, the store, effects, history, and device sync; the
app's are the mail engine, the files model, the bar, the catalogue, and the
platform's own disk and keychain code. Everything runs against fakes.

A test drives a `Session` with no widget at all. `Session::fake(apps)` opens an
in-memory store, seeds it, builds a `Fake` world, and mounts the workers
inline, so a scripted action is followed by its consequences in the same call.
`Session::fake_mode` is the same with the outside said, which is what a
panels-library mount takes. A test navigates with `Nav`, files an `Action`,
calls `settle`, and reads the slots back; it reaches a fake capability through
the world to plant a row or take a server offline.

### The boundary tests

Two tests keep the layering honest by reading the source, because the one
mistake that is invisible in review is an import that seems harmless until the
layer it crossed has to be moved.

- `kernel/src/lib.rs` asserts that nothing in the kernel names Makepad and
  nothing names an app. It also asserts that `Connection::open` appears only in
  `store.rs`, so no code can open a second writable handle and route around the
  one writer.
- `app/tests/boundaries.rs` asserts that nothing under `app/src/shell/` or
  `app/src/platform/` names an app. It skips `app/src/shell/system/`, which is the
  shell's own app on purpose, and `app/src/shell/mod.rs`, which is the list itself.

Both read only the code part of each line, so prose about "the mail an app
sends" is fine and `use crate::mail` is not.

## CI

One macOS job in `.github/workflows/ci.yml`, on every push to `main` and every
pull request:

```sh
cargo clippy --workspace --all-targets --locked -- -D warnings
cargo test --workspace --locked
MAKEPAD=headless cargo build --locked -p superapp
./e2e/run-all.sh
```

The end-to-end suites run last because `MAKEPAD=headless` is a build-time
switch and re-does the crates that read it. One job avoids compiling Makepad
twice, and `--locked` rejects a stale lockfile. There is no `cargo fmt --check`
gate: several tables and comment columns are aligned by hand.

## End-to-end suites

```sh
MAKEPAD=headless mise exec -- cargo build -p superapp
./e2e/run-all.sh
```

`run-all.sh` runs every suite in parallel under `--no-draw`, which does the
whole widget pass, so labels resolve exactly as they do with pictures, while
rasterizing nothing. A failure is a label that did not resolve: a real one. A
failing step prints what it asked for and every label that was on offer.

Each run gets a fresh seeded temporary database, in its own directory, so
parallel suites share nothing and what sits beside a store cannot leak between
them.

### Where a suite lives, and what it says about itself

The shell's own suites are `e2e/*.txt` and an app's are `e2e/<app>/*.txt`. A
suite is named by the path it is at, so `mail/basic` and a shell suite of the
same name never collide. `e2e/sync/` is the one directory left out: those walks
are two devices over one bucket, so each needs a second process and a
`bucketd`.

What a suite needs beyond the defaults it says in two header lines of its own
file:

```text
# args: --grid 4x3 --window 380x780
# env:  SUPERAPP_SEND_DELAY=1
```

The script picks up the first line of each and passes them through. There is no
table of special cases anywhere else.

Run one suite by hand:

```sh
mise exec -- cargo run -p superapp -- --e2e e2e/mail/basic.txt --no-draw --draws 4000
```

`--draws` belongs to Makepad's headless loop: it caps how many frames are
pumped, and without it a single frame renders and the script never advances.
Each walk normally ends earlier, at its own `quit`.

### The rendered run

To look at the chrome instead of only asserting on it, run one suite with
pictures:

```sh
MAKEPAD_HEADLESS_OUT_DIR=/tmp/frames mise exec -- ./target/debug/superapp \
  --e2e e2e/shell-basic.txt --e2e-out /tmp/shots --draws 4000
```

Under `mise`, because the headless backend shells out to `rustc` to compile its
shaders, and without it every frame comes out blank.

A `shot` waits for its own frame. The loop takes a next-frame event and then
draws, so at the instant a `shot` step runs, the newest frame on disk is the one
*before* it. The step reads the rasterizer's frame counter, asks for a draw, and
copies nothing until a higher number exists, and past a blank frame too, since
a pass whose shader is not loaded yet paints nothing. While it waits, no step is
taken and no virtual time passes, so the picture is the state at that step. Out
of patience it says so and takes what there is. The copy itself is an effect, so
a world that may not photograph refuses out loud.

`--no-draw`, which is what `run-all.sh` uses, skips `shot` entirely.

### The clock

`MAKEPAD=headless` is what gives a run its virtual clock and its inline passes.
The runner is handed a fixed `dt` per tick and counts milliseconds down itself,
so `wait 600` is exactly 36 frames whether the machine is idle or running twelve
other suites, and a scripted `wait` advances a send deadline or a device-sync
handoff rather than a wall clock.

### Script commands

```text
wait 600
shot inbox
click "reply"
cmdclick "Q3"
mouse "filter"
selectall "mail html"
drag "mail body" 420 0
key cmd+shift+left
key down 45
key cmd 2
type "hello"
quit
```

Commands use labels from links, buttons, fields, rows, and panel titles. Labels
match without case. Exact matches rank above prefixes, which rank above other
substring matches; a panel's own focus rectangle yields to anything named, the
tightest label wins, and otherwise the last drawn one wins, as it does for
pointer hit testing.

Add `wait` after a step that changes the workspace, because hit areas update on
the next drawn frame. Use keyboard movement to select a distant list row.

`click` synthesizes a real press and release at the label's centre, so the
widget under it handles it as it would a finger's. `mouse` sends the same pair
through the shell's own mouse path, which also tests focus and input routing. A
synthesized mouse click on a hosted text field is reliable only once per run,
because Makepad's private capture and delayed focus state cannot be reset by the
harness; reach later fields with `tab`. A scripted `drag` does not prove that
text became selected, because Makepad starts a selection only for a
platform-captured pointer; `selectall` is the assertion for that.

`swipe`, `pan2`, `holdmove`, and `drop` still parse, and the runner says out
loud that they are not in this build, so a suite that asks for touch fails
visibly rather than on a label that was never drawn.

### Device sync

`e2e/sync/` has its own scripts, each its own gate:

```sh
./e2e/sync/sync-demo.sh        # A bootstraps and archives; B locks, takes over, writes
./e2e/sync/bucket.sh           # a device gives itself a bucket from inside the app
./e2e/sync/reseed.sh           # a peer's edit reaches a running follower's live panel
cargo run -p superapp --bin sync-demo   # the same lease lifecycle, narrated, with no window
```

They gate on each device's store rather than on a wall clock, so two
independent virtual clocks cannot race. `docs/device-sync-demo.md` is the whole
walk, local and against a real bucket.

## Flags

| Flag | Meaning |
|---|---|
| `--db PATH` | the store; a scripted run without it gets a fresh temporary one |
| `--bucket URL` | where device sync's lease and log live |
| `--e2e PATH` | the script to replay |
| `--e2e-out DIR` | where `shot` writes; `e2e/out` by default |
| `--no-draw` | run the widget pass, rasterize nothing, skip `shot` |
| `--draws N` | Makepad's cap on frames pumped by the headless loop |
| `--demo-disk` | the disk capability reads the kernel's demo tree; the only way a scripted run may write to a disk at all |
| `--front` | let a scripted run take the screen; off by default |
| `--grid WxH` | force the unit grid |
| `--window WxH` | force the window size |
| `--library [NAME…]` | open on the panels-library canvas, filtered by name |
| `--r2-login` | read a device-sync secret from stdin, file it, and exit |

## Environment knobs

The shell's own:

| Variable | Meaning |
|---|---|
| `SUPERAPP_BUCKET` | the bucket URL, between `--bucket` and the `bucket` file |
| `SUPERAPP_FRAME_LOG=1` | print each frame's drawing cost and every event over a millisecond |
| `SUPERAPP_R2_ACCESS_KEY_ID`, `SUPERAPP_R2_SECRET_ACCESS_KEY`, `SUPERAPP_R2_REGION` | the device-sync bucket's credentials |
| `SUPERAPP_BUCKET_DIR` | `bucketd`'s directory |

An app's own knobs are environment variables it reads itself, because argv
belongs to the shell: `SUPERAPP_SEND_DELAY`, `SUPERAPP_MAIL_DOWN`,
`SUPERAPP_GOOGLE_CLIENT_ID`, and `SUPERAPP_GOOGLE_CLIENT_SECRET` are all
[mail's](./mail.md#environment-knobs).

Makepad's own: `MAKEPAD=headless` at build time, `MAKEPAD_HEADLESS_OUT_DIR` for
the rasterizer's frames, and `MAKEPAD_HEADLESS_DPI` to fix the geometry.

## The panels library

```sh
mise exec -- cargo run -p superapp -- --library
mise exec -- cargo run -p superapp -- --library mailbox files
```

`--library` opens the window on a zoomable canvas instead of a workspace: every
scene of the catalogue, laid out by name, each node a live mount: a bare widget
populated from a fixture, or a whole stage on a session of its own, replaying a
short script to reach its state and then freezing into a picture. Names filter
the scenes by substring.

Drag or scroll to pan, `cmd+scroll` and `cmd+=` / `cmd+-` to zoom, `cmd+0` to
fit, arrows to pan. Click a scene's title to fit its block; click a node to
enter it at 1:1, where the keyboard and the pointer are that mount's alone, and
`cmd+esc` leaves. `shift+cmd+l` puts the canvas up over a running workspace and
takes it down again; the stage underneath is suspended, not torn down.

The shell's own scenes are in `app/src/shell/catalog.rs`; an app's are its own,
returned from `AppUi::scenes`, so nothing under `shell/` names an app to draw
the canvas. The shell's own app supplies its scenes the same way, which is how
the seam is proved on itself.

Scenes are built from real Rust types and the widgets' own APIs, so a change
that would break a scene breaks the build. A scene's script is parsed at boot
and a typo in one stops the process. The graph rules (no duplicate node names,
every edge naming a real node, no cycles) are the kernel's and have unit tests.

Its own suites are `e2e/shell-library.txt` and `e2e/shell-library-toggle.txt`.

## Phone preview

```sh
mise exec -- cargo run -p superapp -- --window 380x780 --grid 4x3
mise exec -- cargo run -p superapp -- --grid 8x4
```

These force a smaller unit grid, which is how a phone layout is looked at on a
desktop. `e2e/shell-phone.txt` walks the 4×3 grid the same way.

## App icons

`app/resources/make_icons.py` creates every icon from one drawing:
`icon_{32..1024}.png` for normal runs, `icon.icns` and `icon.ico` for desktop
bundles, and `android/res/` for adaptive and legacy launcher icons. Small sizes
are laid out on the pixel grid rather than downsampled. `--preview DIR` writes
a contact sheet for review.

## The book

```sh
mise exec -- mdbook serve docs/book
```

The book describes the current product. Feature work starts with a change
request under `docs/planning/`. Move the lasting information into the book and
delete the change request when the implementation is complete.

## Adding an app

1. Add a directory under `app/src/apps/`, with the kernel half and the Makepad
   half beside each other.
2. Implement `kernel::app::App`: an `id`, the panel kinds it owns, and whatever
   else it needs: a `Schema`, a `seed`, deferred `effects`, `outside`
   capabilities, `search_providers`, `problems`, `workers`, and `roots`. Prefix
   every table name with the app's id.
3. Implement a `PanelKind` and a `Panel` per tag. Give each tag a typed view
   that spells its arguments once. Verb ids are stable and prefixed.
4. Implement `AppUi`: a `script_mod` block whose template ids carry the app id,
   a `template` for every tag, and `scenes` for the panels library.
5. Add both halves to `APPS` and `UIS` in `app/src/lib.rs`, and hang each
   template on the stage in `app/src/root.rs`. A tag with no template is a boot
   error.
6. Put the app's suites under `e2e/<app>/`, naming only labels its own panels
   draw plus shell chrome, and give each suite its `# args:` and `# env:` lines
   if it needs any.
7. Test the app's bars: every accel unique within a bar, none of them reserved.
   The debug assertion catches it at run time; a test catches it in CI.
8. Write its chapter under `docs/book/src/`, add it to `SUMMARY.md`, and link
   it from [Overview](./overview.md).
