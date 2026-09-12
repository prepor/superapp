# superapp

A personal "user space OS": no apps, no windows — specialized panels (kind +
params) on one horizontally scrolling 12×6 workspace, niri-style. Rust +
Makepad, macOS, with an android build off the same library.

Three layers in two crates: a **kernel** that does not draw, a **shell** that
does, and **apps** on top of both. Mail and files are apps; so is the system
app, which is the shell's own. Device sync is not — it is the kernel's own, and
it carries the rows an app declares from one device to another.

**The [book](docs/book/src/SUMMARY.md) is the single source of truth** —
model, grammar, architecture, open questions. `mise exec -- mdbook serve
docs/book` to read it rendered. Under the book, the doc comments on the
kernel's traits are the specification the three layers are written against.

## Run

```sh
mise trust && mise install
mise exec -- cargo run -p superapp
```

Normal builds enable TDLib and require `libtdjson`; the [Telegram build
instructions](docs/book/src/telegram.md#builds) explain the library path.
Use `--no-default-features` for a build with Telegram's offline demo only.

Borderless over the display's visible frame. `cmd` + arrows focus panels
(`+shift` moves one, `cmd+1`…`9` walk the workspaces, `cmd+w` closes,
`cmd+z` undoes and `shift` redoes text edits while an input has the caret,
or workspace actions otherwise, `cmd+u` opens the history, `cmd+i` writes
the focused panel's provenance to the clipboard); plain keys belong to the
focused panel, and the help panel documents the rest.

The store lives in `~/Library/Application Support/superapp`; `--db <path>`
puts it anywhere else, and `--bucket <url>` names the R2 bucket the backup
credentials are for. `--help` lists every flag.

A store whose kernel schema this build does not know is refused, so a file
another build wrote does not open: the run says which file, which two schemas,
and the two ways past — `--db PATH` for another file, or that one moved aside
by hand — and exits 2 without opening a window.

## Develop

```sh
mise exec -- cargo test --workspace --no-default-features
mise exec -- cargo clippy --workspace --all-targets --no-default-features -- -D warnings
mise exec -- cargo run -p superapp --no-default-features -- --e2e e2e/shell-basic.txt
MAKEPAD=headless mise exec -- cargo build -p superapp --no-default-features && ./e2e/run-all.sh
mise exec -- cargo run -p superapp --no-default-features -- --library
```

These commands omit the native TDLib dependency. Run `cargo test --workspace`
without that flag to include the TDLib FFI smoke tests on a machine with the
library installed; accounts still use fakes.

The pure suite covers the kernel's panel mechanics, springs, store, effects,
history, device sync's log and merge and its service over two loopback
endpoints, and the app's own — the mail engine, the files model — all against
fakes, with no window, no network and no keychain.
Two boundary tests come with it and keep the split honest by reading the
source: the kernel names no Makepad and no app, and nothing under `shell/` or
`platform/` names an app.

CI (macOS) runs the linter, the tests, the whole e2e battery and the
two-process device-sync walk on every push to `main` and every PR.

## The suites

```sh
MAKEPAD=headless mise exec -- cargo build -p superapp --no-default-features
./e2e/run-all.sh
```

`run-all.sh` runs every suite in parallel under `--no-draw`, which does the
whole widget pass — so labels resolve exactly as they do with pictures —
while rasterizing nothing. A failure is a label that did not resolve: a real
one. The shell's own suites sit in `e2e/*.txt` and an app's in
`e2e/<app>/*.txt`; a suite is named by the path it is at, so `mail/basic` and
a shell suite of the same name never collide. What a suite needs beyond the
defaults it says in its own first lines, `# args:` and `# env:`.

`e2e/sync/` stays out of it: pairing two devices is two processes that have
to be up at the same time, so it has a script of its own, `./e2e/sync/pair.sh`
— two stores, two endpoints on loopback, a ticket carried from one to the
other, and a note crossing each way.

`MAKEPAD=headless` is a build-time switch — `build.rs` turns it into
`cfg(headless)` — and it is what gives a run its virtual clock and its inline
passes, so a scripted `wait` advances a send deadline rather than a wall clock.

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
  capability traits and the fixtures behind them), device sync (`sync`), undo
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
- `e2e/` — the suites, `out/` generated; `sync/` is the two-process pairing
  walk and its own script.
- `docs/book/` — the book.
