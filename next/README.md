# next

`next/` is the prototype of CR-010, the redesign that splits the app into a
kernel, a shell, and apps. It is built on fakes only — an in-memory store, a
clock that moves when a script moves it, a demo file tree — and the shipping
tree in `src/` is left running beside it. Finding out inside the working tree
what Makepad does with instance-owned tables or a bottom bar would break the
product for weeks; finding it out in a sandbox costs nothing but the sandbox.
Its design document is `docs/planning/cr-010-apps.md`, which is also the
contract the code is written against: the doc comments on the kernel's traits
are the specification, and the book stays about the shipping app until the
port.

Run its tests with `mise exec -- cargo test -p superapp-kernel` from this
directory; the kernel names no Makepad, so that needs no window and takes a
second. `mise exec -- cargo clippy -p superapp-kernel --all-targets -- -D
warnings` is the other half, and both are expected to be green at every
commit. `mise exec -- cargo test -p superapp-next` is the other crate: the
apps' own tests, which drive a session with no widget in sight, and the
boundary test. It pulls in Makepad and takes minutes the first time;
`.cargo/config.toml` points at `../target`, so that build is shared with the
shipping tree rather than done twice.

The suites are the other gate:

```
MAKEPAD=headless mise exec -- cargo build -p superapp-next
./e2e/run-all.sh
```

`run-all.sh` runs every suite in parallel under `--no-draw`, which does the
whole widget pass — so labels resolve exactly as they do with pictures —
while rasterizing nothing. The shell's own suites sit in `e2e/*.txt` and an
app's in `e2e/<app>/*.txt`; a suite is named by the path it is at, so
`mail/basic` and a shell suite of the same name never collide. What a suite
needs beyond the defaults it says in its own first lines, `# args:` and
`# env:`. To look at the chrome instead of only asserting on it, run one
suite rendered:
`MAKEPAD_HEADLESS_OUT_DIR=/tmp/frames mise exec -- ../target/debug/superapp-next
--e2e e2e/shell-basic.txt --e2e-out /tmp/shots --draws 4000` — under `mise`,
because the headless backend shells out to `rustc` to compile its shaders,
and without it every frame comes out blank.

The panels library is the other way in. `--library` opens the window on a
zoomable canvas instead of a workspace: every scene of the catalogue, laid out
by name, each node a live mount — a bare widget populated from a fixture, or a
whole stage on a session of its own, replaying a short script to reach its
state and then freezing into a picture. Drag or scroll to pan, `cmd+scroll`
and `cmd+=` / `cmd+-` to zoom, `cmd+0` to fit; click a scene's title to fit
its block, click a node to enter it at 1:1 — from there the keyboard and the
pointer are that mount's alone, and `cmd+esc` leaves. `shift+cmd+l` puts the
canvas up over a running workspace and takes it down again; the stage
underneath is suspended, not torn down. `--library mailbox files` narrows the
canvas to the scenes whose names match. The shell's own scenes are in
`shell/catalog.rs`, which is also where `Setup` and its constructors live; an
app's are its own, returned from `AppUi::scenes` — mail's, files', and the
system app's, so nothing under `shell/` names an app to draw the canvas.

The layout is one Cargo workspace with two members. `kernel/` is everything
generic that does not draw: the panel model and navigation (`panel`, `nav`,
`session`, `layout`), the store and its cached queries (`store`), effects and
the queue (`effect`, `caps`), undo history (`history`), the filter and the
rich table's state (`filter`, `richtable`), search and the launcher list
(`search`, `launcher`), problems, springs, the e2e grammar, and the
interfaces an app implements (`app`).

`app/` is the binary and the Makepad half. `src/main.rs` lists the apps —
both halves of each, the kernel's `App` and the shell's `AppUi` — and
`src/root.rs` is the window, the one place that hangs every app's widget
templates on the stage. Under `src/shell/` is everything generic that draws
or takes input, split by concern: `boot` (argv, the world, the session),
`stage` (the widget that owns the session and its frame loop), `keys`,
`pointer`, `anim`, `draw` (the chrome, and the bar at a panel's foot),
`hosted` (one widget per slot), `overlays`, `hits`, `bar`, `e2e`, `dsl` (the
theme and the base widgets), `widgets` (the shared components: the rich
table over a panel's own list state, its completion box, and the file
card), `catalog` and `library` (the panels library: what a scene's node comes
up as, and the canvas that mounts them), and `system` — the shell's own app, which supplies help, about, the
effect log with its job panel, problems, and the card a panel gets when no
app in this build owns its tag.

Two boundary tests keep the split honest, both by reading the source:
`kernel/src/lib.rs` fails if anything in the kernel names Makepad or an app,
and `app/tests/boundaries.rs` fails if anything under `shell/` or `platform/`
names an app. `src/platform/` is what macOS gives the shell that Makepad does
not: the borderless window over the display's visible frame.
