# CR-006 · The panels library: a canvas of live stages

Status: **implemented** 2026-09-02 (Andrey: "a dev tool that should allow
to quickly iterate through UI changes by reviewing different UI states /
flows … an infinity canvas that we can zoom / pan and there are panels
live … annotations / arrows from one state to another"). The follow-up
CR-004 was built to enable and deliberately did not specify. CR-004 called
it the *components library*; this is the *panels library*, because the
thing worth looking at is a panel in a workspace, not a widget on a card.

## Why

Iterating on the UI means looking at many states, and every state costs a
hand walk — or a suite run and a screenshot, which is a dead picture of one
state under one grid. CR-004 made an app instance a value: an in-memory
store, an outside you choose, a clock that moves when you say. This mounts
a hundred of them in one window.

## The reading: stories

The scripts under `e2e/` already document the UI, one walk per file, with
prose. Read as a **story**, every `shot` is a named state — a node on the
canvas — the steps between two shots are the arrow between their nodes,
and the `#` comments are the annotations. Nothing is authored twice: a
story stays a suite the harness runs, and the canvas is a second reading
of the same file. A story may open with a `#!` header naming what its
mounts need, where the book's prose used to ("run with `--window 380x780
--grid 4x3`"):

```text
#! window 380x780 grid 4x3      # the mount's viewport and unit grid
#! send-delay 1                 # the send-undo window, seconds
#! outside real                 # deny (default) | fake | real
#! canvas                       # drives the canvas itself; never mounted
```

The harness ignores the header — it is a comment — so a story runs under
`--e2e` exactly as before. `src/story.rs` is the reading and the canvas
layout, std-only and unit-tested.

## The grain: a whole stage per node

A node is a whole [`Stage`] — the workspace, its columns, joins, previews,
overlays, springs — not one panel widget. The behaviour worth reviewing
(a solid link joins, the next one replaces, a preview keeps focus, a swipe
triages) lives between panels, and the scripts exercise exactly that.

## Mounts

A mount is a `Stage` booted by the canvas rather than by argv
(`Stage::boot`, on a [`Boot`]): an in-memory store with the demo seed, the
story's grid and send window, passwords in memory, **virtual time**
(`Pump::Manual`, the frame clock), and the outside the header asked for —
`Deny` with a clock by default, so a panel that reaches the network while
you look at it fails loudly and the clock still runs (CR-004's open
question 4, settled).

**Replay.** A mount replays `steps[..=shot]` and stops. Drawing is what a
replay costs — a hundred mounts, a full widget pass each — so a frame runs
steps until the next one resolves against the hit list (a click, a drag, a
swipe), each on its own jump of the virtual clock: a pending `wait` is
consumed whole, and the manual pump runs for every half second of virtual
time that passed. A node therefore needs one frame per click on its way,
not one per millisecond. Earlier nodes' shots on the way are no-ops; the
mount's own shot is arrival, and its clock stops there. An *entered* mount
ticks a frame at a time, so its toasts fade and its deadlines pass.

**Nothing outside its pass.** A mount never touches the menu bar; the IME
and key focus only while the canvas has entered it; its redraws mark its
own draw list and the canvas's, never everything; and the actions its
widgets raise are captured and handed straight back to it, so a hundred
stages never hear each other (a `PanelAction` carries a panel id, and every
mount numbers its panels from one).

## The canvas

`--library [PATH...]` (default `e2e/`) opens the window on the canvas
instead of the workspace (`src/library.rs`). One row per story, nodes left
to right, arrows between with their steps stacked above, the note under
the shot's name, the intro under the story's; deterministic from the
scripts, so nothing is persisted and nothing is dragged.

Each mount renders into **its own pass** at the canvas's zoom — the pass's
dpi factor is the window's times the zoom, snapped to a quarter octave —
and the canvas shows the pass's texture. Text is crisp at every level
because it is rasterised at that level, not scaled. The pass rect comes
from a transparent quad the mount's logical size with the origin at zero,
so a mount's own coordinates never move: hits recorded during its draw
stay valid however the camera moves. Off-screen mounts that have arrived
are not drawn at all; off-screen mounts still replaying draw at a tenth of
a dpi, because a step needs fresh hits, not pixels.

**Entering.** A click on a node (or its name) flies the camera to 1:1 on
it and routes the keyboard and the pointer to that stage, remapped into
its own coordinates. A flow can be continued by hand from any of its
states. A click outside, or ⌘esc, leaves. Every control is in the legend
along the bottom.

**The canvas's own suite.** `e2e/library.txt` (`#! canvas`) drives the
canvas: `wait`, `shot`, `click` on a node's name or a story's, and the
canvas chords. Run headless with a small story set and `MAKEPAD_HEADLESS_DPI=1`.

## Costs, named honestly

- **Replay is draws.** Every node replays from the seed; the total is the
  sum over nodes of the clicks on the way, each a full stage draw. Thirteen
  nodes replay in under a second of frames; the hundred nodes of the whole
  `e2e/` directory take a while on first open, and the canvas stays live
  while they do. Sharing a story's prefix between its nodes would cut it
  and is not done (see open 2).
- **Textures stay allocated** once a mount has rendered, at its last size.
  Bounded by the last zoom each was seen at; not freed.
- **`Deny` is not `Real`.** A settings story that adds an `.invalid` host
  shows "this world has no outside (connect)" rather than the DNS error the
  suite shows. `#! outside real` restores parity where it matters.
- **The pointer at any zoom, the keyboard at 1:1.** Interaction goes to the
  entered mount only; other mounts are pictures until entered.
- **Names clamp, notes do not.** Story and node names stay legible from any
  height (laid in screen space above their mounts); notes and arrow labels
  scale with the canvas and vanish far out.
- **Two copies of the panel-template DSL** — the window's stage and the
  library's `stage_tpl` list the same ten templates.
- **`--draws` is not optional.** The headless backend renders one frame and
  exits — with code 0 — without a frame budget. The book's commands now
  carry one.

## Decisions taken

- **Stories are the e2e scripts.** No second format; `#!` headers for
  what argv used to carry.
- **A node is a stage**, not a panel widget.
- **Boot is a value** (`Boot`) for the primary stage and every mount alike;
  the cfg(headless) forks in the shell became one `virtual_time` flag.
- **`Deny` by default, with a clock.**
- **Replays fast-forward and draw only before hit-resolving steps.**
- **Per-mount passes at zoom dpi, events remapped, actions captured.**

## Still open

1. **Mounting on demand.** Everything mounts at open; a canvas of a hundred
   nodes could mount a row when it first scrolls near, and the entered
   story first.
2. **Shared-prefix replay.** Node *k* could start from node *k−1*'s world
   (an SQLite backup of an in-memory store is milliseconds) if the retained
   widget state — scroll positions, carets — were rebuilt from the store.
3. **Live DSL reload.** A DSL change today is a rebuild and a reopen; the
   platform has a reload path the canvas does not yet use.
4. **Save as shot.** A flow continued by hand inside an entered mount could
   append its steps and a new `shot` to the story file.
5. **One story, three grids.** A header could ask for the same story at
   12×6, 8×4 and 4×3 side by side.
