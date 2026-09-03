# superapp

A personal "user space OS": no apps, no windows — specialized panels (kind +
params) on one horizontally scrolling 12×6 workspace, niri-style. Rust +
Makepad, macOS.

**The [book](docs/book/src/SUMMARY.md) is the single source of truth** —
model, grammar, architecture, open questions. `mise exec -- mdbook serve
docs/book` to read it rendered.

## Run

```sh
# once: materialise the vendored makepad (pin + patches owned by rel.systems)
(cd ../rel.systems/mosaic && ./scripts/vendor-makepad.sh)

mise trust && mise install
mise exec -- cargo run
```

Borderless over the display's visible frame. `cmd` + arrows/`hjkl` focus
panels (`+shift` moves, `cmd+w` closes); plain keys belong to the focused
panel; the help panel documents the rest.

## Develop

```sh
mise exec -- cargo test                        # pure core: panel mechanics, springs, e2e grammar
mise exec -- cargo clippy --all-targets -- -D warnings   # the linter, as CI runs it
mise exec -- cargo run -- --e2e e2e/basic.txt  # scripted run + screenshots to e2e/out (--front to watch)
MAKEPAD=headless mise exec -- cargo build && ./e2e/run-all.sh   # every e2e suite, ~2s
mise exec -- cargo run -- --library            # the panels library (also Dev → Panels Library, ⇧⌘L in the app)
```

CI (macOS) runs the tests, the linter and the whole e2e battery on every
push to `main` and every PR.

## Layout

- `src/` — `core` (pure state machine) · `app` (makepad shell) · `data` ·
  `spring` · `theme` · `e2e` · `mac`
- `resources/` — fonts, and the app icon (`make_icons.py` regenerates every
  size and platform from one drawing)
- `docs/book/` — the book
- `e2e/` — e2e scripts (`out/` is generated)
