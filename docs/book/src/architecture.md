# Architecture

Mosaic's division of labour, adopted wholesale: a pure core that owns *what*
the layout is, and a shell that owns *how it gets there*.

| module | role | depends on makepad |
|---|---|---|
| `src/core.rs` | panel/column/join state machine + scene targets | no |
| `src/data.rs` | the fake mail behind the demo panels | no |
| `src/spring.rs` | niri's closed-form spring (via mosaic) | no |
| `src/theme.rs` | the look: sizes and colours | no |
| `src/e2e.rs` | e2e script grammar + runner state | no |
| `src/mac.rs` | screen geometry, activation, window screenshots | no (apple-sys) |
| `src/app.rs` | the makepad shell: drawing, events, animation | yes |

Everything above `app` is std-only and unit-tested without opening a window.

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
