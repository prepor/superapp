# Architecture

Mosaic's division of labour, adopted wholesale: a pure core that owns *what*
the layout is, and a shell that owns *how it gets there*.

| module | role | depends on makepad |
|---|---|---|
| `src/core.rs` | panel/column/join state machine + scene targets | no |
| `src/store.rs` | the one SQLite file + the reactive query layer | no |
| `src/effect.rs` | the boundary: what leaves the process, the job queue, and the log's datasource | no |
| `src/history.rs` | the in-memory tree of actions and their claims | no |
| `src/filter.rs` | the rich table's filter grammar and completion context | no |
| `src/richtable.rs` | the rich table: datasources, the SQL builder, paging | no |
| `src/mail.rs` | the mail domain: queries, titles, seed, effects, intents | no |
| `src/html.rs` | narrowing HTML from outside — mail, feed articles — to what a panel draws | no |
| `src/sync.rs` | the IMAP engine: passes, ingest, push, the pump | no |
| `src/send.rs` | drafts → outbox → SMTP, with the undo window | no |
| `src/repl.rs` | device sync: the log, the lease, and the sync passes | no |
| `src/object.rs` | the sync transport: object store (memory, HTTP) + `state` | no |
| `src/secret.rs` | passwords: keychain (macOS) / private file | no |
| `src/launcher.rs` | the launcher's search over panels + mail world | no |
| `src/problems.rs` | standing problems, derived from the rows that carry them | no |
| `src/spring.rs` | niri's closed-form spring (via mosaic) | no |
| `src/ui.rs` | the semantic content vocabulary: lines, fields, forms | no |
| `src/theme.rs` | the look: sizes and colours | no |
| `src/e2e.rs` | e2e script grammar + runner state | no |
| `src/scene.rs` | a subject in its named states, and the library canvas's layout | no |
| `src/mac.rs` | screen geometry, activation, window screenshots | no (apple-sys) |
| `src/panels.rs` | retained panel widgets | yes |
| `src/app.rs` | the makepad shell: drawing, events, animation | yes |
| `src/catalog.rs` | the scenes the panels library shows | yes |
| `src/library.rs` | the panels library: a canvas of live scenes | yes |

Everything above `panels` is std-only and unit-tested without opening a
window.

## The world

`World` is the one handle to everything outside the pure core: the store,
the outside (`Real` / `Fake` / `Deny`), the effect registry. It is a value
you construct, never a global — the UI thread owns one, and each
worker thread builds its own. That is what lets a whole app instance exist
in memory, which is what makes tests isolated and parallel. See [The Data
Substrate](./data-substrate.md).

## Stages and mounts

The shell's widget is the `Stage`: the workspace, its springs, its hits,
its script runner. It comes up on a `Boot` — a store path or memory, a
grid, the send window, whether time is virtual, which outside — and the
window's own stage builds its boot from argv. The panels library builds
one per panel or workspace node of a scene instead and **mounts** the
stage on the canvas — solo on the one panel the node names, or the whole
workspace: each mount renders into its own pass at the canvas's zoom,
replays the node's steps on virtual time, and owns nothing outside that
pass — no menu bar, the IME only while entered, redraws scoped to its own
draw list, and the actions its widgets raise captured and handed back to
it alone. A component node mounts a bare widget the same way, with no
stage at all. See [Developer Experience](./dev-x.md#panels-library).

## Frame loop

```text
Event::{Key,Mouse}* ──▶ mutate Ws ──▶ ensure_focus_visible ──▶ ws.scene()
                                              │
                                              ▼
                            Anim::apply (retarget / spawn / ghost)
Event::NextFrame ──▶ Anim::advance(dt) ──▶ redraw while anything moves
```

The scene is pulled only after a mutation, never per frame; the shell idles at
zero frames. Trackpad pans bypass the springs (1:1, camera jump).

## Shell internals

- **Char grid.** The mono face is measured once per display scale
  (`prepare_single_line_run`); panel content is a list of styled lines whose
  segments (text, links, buttons, key-caps, fields) advance on that grid.
  Bodies draw inside clipped, absolutely-positioned turtles, so scrolling is
  pixel-smooth and clipping exact.
- **Hit testing.** Drawing records `(rect, action, cursor)` for every
  interactive segment; a click resolves the topmost record. Immediate-mode UI:
  the hit list is one frame old at worst, only during animation.
- **Text input.** makepad emits `TextInput` only while the system IME is shown
  (`show_text_ime`); the shell mirrors field focus into IME visibility.
  Letters reach panels as text; control keys as `KeyDown`.
- **Ownership.** `State` (workspace, mail flags, per-panel UI, springs, toast)
  is a `#[rust]` box on the Stage widget, taken out during draw so drawing
  methods can borrow both the draw resources and the state.
- **Nothing heavy in a draw.** A `draw_walk` runs on the UI thread, inside
  the frame, and — this being immediate mode — runs again on every redraw.
  Anything that is not laying out the pixels in front of you does not belong
  there: no I/O, no parsing, no decoding, nothing that scales with the size
  of the data rather than the size of the view. A blob read out of SQLite, a
  MIME walk, a base64 decode, a PNG decode: each is milliseconds, each lands
  in one frame, and a few of them together are a visible stutter — a scroll
  that catches, a panel that opens late. Reading rows through
  [`Store::rows`](./data-substrate.md) is the exception the design pays for:
  results are cached per `(query, params)` and only re-run when a generation
  says they are stale, so the draw is a lookup.

  The shape of the fix is always the same. Ask for the work off the frame,
  once, keyed by what it is *for*; let the answer land through an action that
  redraws; and hold the right-sized box in the meantime, so nothing reflows
  when it arrives. The pictures in a letter (`Pictures` and `HtmlImage` in
  `panels.rs`) are the worked example: the panel asks a reader thread for a
  mail's `cid:` parts instead of reading and parsing its raw itself, the item
  hands the bytes to makepad's decode pool instead of decoding them, and the
  size comes off the image header — cheap, and enough to reserve the space
  the picture will fill while it is still decoding.
