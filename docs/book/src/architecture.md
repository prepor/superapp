# Architecture

Mosaic's division of labour, adopted wholesale: a pure core that owns *what*
the layout is, and a shell that owns *how it gets there*.

| module | role | depends on makepad |
|---|---|---|
| `src/core.rs` | panel/column/join state machine + scene targets | no |
| `src/store.rs` | the one SQLite file + the reactive query layer | no |
| `src/effect.rs` | the boundary: what leaves the process, and the job queue | no |
| `src/history.rs` | the in-memory tree of actions and their claims | no |
| `src/filter.rs` | the rich table's filter grammar and completion context | no |
| `src/richtable.rs` | the rich table: datasources, the SQL builder, paging | no |
| `src/mail.rs` | the mail domain: queries, titles, seed, effects, intents | no |
| `src/html.rs` | narrowing a mail's HTML to what a panel can draw | no |
| `src/sync.rs` | the IMAP engine: passes, ingest, push, the pump | no |
| `src/send.rs` | drafts → outbox → SMTP, with the undo window | no |
| `src/secret.rs` | passwords: keychain (macOS) / private file | no |
| `src/launcher.rs` | the launcher's search over panels + mail world | no |
| `src/spring.rs` | niri's closed-form spring (via mosaic) | no |
| `src/ui.rs` | the semantic content vocabulary: lines, fields, forms | no |
| `src/theme.rs` | the look: sizes and colours | no |
| `src/e2e.rs` | e2e script grammar + runner state | no |
| `src/mac.rs` | screen geometry, activation, window screenshots | no (apple-sys) |
| `src/panels.rs` | retained panel widgets | yes |
| `src/app.rs` | the makepad shell: drawing, events, animation | yes |

Everything above `panels` is std-only and unit-tested without opening a
window.

## The world

`World` is the one handle to everything outside the pure core: the store,
the outside (`Real` / `Fake` / `Deny`), the effect registry. It is a value
you construct, never a global — the UI thread owns one, and each
worker thread builds its own. That is what lets a whole app instance exist
in memory, which is what makes tests isolated and parallel. See [The Data
Substrate](./data-substrate.md).

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
