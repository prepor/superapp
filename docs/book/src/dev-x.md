# Developer Experience

## Build & run

```sh
# once: materialise the vendored makepad (owned by rel.systems)
(cd ../rel.systems/mosaic && ./scripts/vendor-makepad.sh)

mise trust && mise install
mise exec -- cargo run
```

`cargo test` runs the pure-core suite — the panel mechanics (the web
prototype's whole smoke scenario is a test) and the spring maths — with no
window.

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
screenshots to `e2e/out/`. The window sits behind everything, click-through
(patch 0003 keeps it presenting while occluded), so a run never takes the
screen. Failed steps (no element matching a label, failed capture) make the
run exit non-zero.

```text
wait 600            # ms
shot inbox          # e2e/out/inbox.png
click "reply"       # label substring, case-insensitive
cmdclick "Q3"       # cmd held: fresh un-joined panel
key cmd+shift+left  # chords; plain letters flow as text, like real typing
key j 45            # optional repeat count
type "hello"
quit
```

Labels address links, buttons, fields (`filter`, `to`, `subject`, `body`),
rows (by subject) and panel titles. Steps that mutate the workspace need a
`wait` after them — hits refresh on the next drawn frame. `e2e/basic.txt`
walks the whole join/replace grammar; the first frame also logs panel count
and measured cell metrics to stderr.
