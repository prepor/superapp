# superapp

A personal "user space OS": no apps, no windows — specialized panels (kind +
params) on one horizontally scrolling 12×6 workspace, niri-style. Rust +
Makepad, macOS, with an android build off the same library.

Three layers in two crates: a **kernel** that does not draw, a **shell** that
does, and **apps** on top of both. Mail and files are apps; so is the system
app, which is the shell's own. Device sync is not — it replicates the store
itself, every app's tables included.

**The [book](docs/book/src/SUMMARY.md) is the single source of truth** —
model, grammar, architecture, open questions. `mise exec -- mdbook serve
docs/book` to read it rendered. Under the book, the doc comments on the
kernel's traits are the specification the three layers are written against.

## Run

```sh
mise trust && mise install
mise exec -- cargo run -p superapp
```

Borderless over the display's visible frame. `cmd` + arrows focus panels
(`+shift` moves one, `cmd+1`…`9` walk the workspaces, `cmd+w` closes,
`cmd+z` undoes and `shift` redoes, `cmd+u` opens the history, `cmd+i` writes
the focused panel's provenance to the clipboard); plain keys belong to the
focused panel, and the help panel documents the rest.

The store lives in `~/Library/Application Support/superapp`; `--db <path>`
puts it anywhere else, and `--bucket <url>` points a run at a device-sync
bucket. `--help` lists every flag.

There is one store schema and no migration to it, so a file another build
wrote is refused: the run says which file, which two schemas, and the two
ways past — `--db PATH` for another file, or that one moved aside by hand —
and exits 2 without opening a window.

## Develop

```sh
mise exec -- cargo test --workspace                                  # both crates, no window
mise exec -- cargo clippy --workspace --all-targets -- -D warnings   # the linter, as CI runs it
mise exec -- cargo run -p superapp -- --e2e e2e/shell-basic.txt      # one scripted run (--front to watch)
MAKEPAD=headless mise exec -- cargo build -p superapp && ./e2e/run-all.sh   # every suite, a couple of seconds
mise exec -- cargo run -p superapp -- --library                      # the panels library (⇧⌘L in the app)
```

`cargo test` is the pure suite: the kernel's panel mechanics, springs, store,
effects, history and device sync, and the app's own — the mail engine, the
files model — all against fakes, with no window, no network and no keychain.
Two boundary tests come with it and keep the split honest by reading the
source: the kernel names no Makepad and no app, and nothing under `shell/` or
`platform/` names an app.

CI (macOS) runs the linter, the tests and the whole e2e battery on every push
to `main` and every PR.

## The suites

```sh
MAKEPAD=headless mise exec -- cargo build -p superapp
./e2e/run-all.sh
```

`run-all.sh` runs every suite in parallel under `--no-draw`, which does the
whole widget pass — so labels resolve exactly as they do with pictures —
while rasterizing nothing. A failure is a label that did not resolve: a real
one. The shell's own suites sit in `e2e/*.txt` and an app's in
`e2e/<app>/*.txt`; a suite is named by the path it is at, so `mail/basic` and
a shell suite of the same name never collide. What a suite needs beyond the
defaults it says in its own first lines, `# args:` and `# env:`.

`MAKEPAD=headless` is a build-time switch — `build.rs` turns it into
`cfg(headless)` — and it is what gives a run its virtual clock and its inline
passes, so a scripted `wait` advances a handoff rather than a wall clock.

To look at the chrome instead of only asserting on it, run one suite
rendered:

```sh
MAKEPAD_HEADLESS_OUT_DIR=/tmp/frames mise exec -- ./target/debug/superapp \
  --e2e e2e/shell-basic.txt --e2e-out /tmp/shots --draws 4000
```

Under `mise`, because the headless backend shells out to `rustc` to compile
its shaders, and without it every frame comes out blank. A `shot` waits for
its own frame: it reads the rasterizer's frame counter, asks for a draw, and
copies nothing until a higher one exists — and past a blank frame too, since
a pass whose shader is not loaded yet paints nothing. While it waits the
world stands still, so the picture is the state at that step. `--no-draw`,
which is the gate, skips `shot` entirely.

## Device sync

`e2e/sync/` is the one directory `run-all.sh` leaves out: those walks are two
devices over one bucket, so each needs a second process and a `bucketd`
beside it. Their own scripts run them, and each is its own gate:

```sh
./e2e/sync/sync-demo.sh          # A bootstraps and archives; B locks, takes over, writes
./e2e/sync/bucket.sh             # a device gives itself a bucket from inside the app
./e2e/sync/reseed.sh             # a peer's edit reaches a running follower's live panel
cargo run -p superapp --bin sync-demo   # the same lease lifecycle, narrated, with no window
```

`app/src/bin/` holds the three programs they are driven with: `bucketd` (a
directory served with the compare-and-swap semantics the lease needs),
`sync-demo`, and `reseed-edit` (a peer that edits a row and publishes it).
The TLA+ model the lease is checked against is `formal/`; `formal/README.md`
says how to run it.

## The panels library

`--library` opens the window on a zoomable canvas instead of a workspace:
every scene of the catalogue, laid out by name, each node a live mount — a
bare widget populated from a fixture, or a whole stage on a session of its
own, replaying a short script to reach its state and then freezing into a
picture. Drag or scroll to pan, `cmd+scroll` and `cmd+=` / `cmd+-` to zoom,
`cmd+0` to fit; click a scene's title to fit its block, click a node to enter
it at 1:1 — from there the keyboard and the pointer are that mount's alone,
and `cmd+esc` leaves. `shift+cmd+l` puts the canvas up over a running
workspace and takes it down again; the stage underneath is suspended, not
torn down. `--library mailbox files` narrows it to the scenes whose names
match.

The shell's own scenes are in `app/src/shell/catalog.rs`; an app's are its
own, returned from `AppUi::scenes`, so nothing under `shell/` names an app to
draw the canvas.

## Layout

One Cargo workspace, two members.

- `kernel/` — everything generic that does not draw: the panel model and
  navigation (`panel`, `nav`, `session`, `layout`), the store and its cached
  queries (`store`), effects and the queue (`effect`, and `caps/` — the
  capability traits and the fixtures behind them), device sync (`repl`), undo
  history (`history`), the filter and the rich table's state (`filter`,
  `richtable`), search and the launcher (`search`, `launcher`), problems,
  springs, the e2e grammar, and the interfaces an app implements (`app`).
- `app/` — the Makepad half: a library with a one-line binary on top of it,
  which is the shape android needs. `app/src/lib.rs` lists the apps, both
  halves of each; `app/src/root.rs` is the window; `app/src/shell/` is
  everything generic that draws or takes input; `app/src/platform/` is what
  this machine gives the shell that Makepad does not — the disk, the
  keychain, the trash, a window-layer screenshot; `app/src/apps/` is mail
  and files.
- `app/resources/` — the fonts, the app icon in every size and platform
  (`make_icons.py` regenerates them all from one drawing), and android's
  launcher icons.
- `e2e/` — the suites, `sync/` for the two-device ones, `out/` generated.
- `docs/book/` — the book.
- `formal/` — the TLA+ model of the device-sync lease.
