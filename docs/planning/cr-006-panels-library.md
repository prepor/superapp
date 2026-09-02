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

**Replay.** A mount replays `steps[..=shot]` and stops, one step per
frame, the way the harness runs one per tick — a pending `wait` is
consumed whole together with the step after it, and the manual pump runs
for every half second of virtual time that passed, so a node needs one
frame per step on its way rather than one per millisecond. One per frame
is not a limitation but the contract: anything a step did — the hits it
changed, the hosted widgets it created, and above all the actions its
widgets raised, which the canvas hands back only after the event returns —
has to land before the next step. A first cut ran several steps per frame
and lost every launcher query, preview and walk to exactly that. Earlier
nodes' shots on the way are no-ops; the mount's own shot is arrival, and
its clock stops there. An *entered* mount ticks a frame at a time, so its
toasts fade and its deadlines pass.

**Nothing outside its pass.** A mount never touches the menu bar; the IME
and key focus only while the canvas has entered it; its redraws mark its
own draw list and the canvas's, never everything; and the actions its
widgets raise are captured and handed straight back to it, so a hundred
stages never hear each other (a `PanelAction` carries a panel id, and every
mount numbers its panels from one).

**Frozen means frozen.** A mount that has arrived and is not entered is a
picture: it gets no events, asks for no frames, and never re-runs its
widget pass. Without this a node whose shot landed inside a toast's three
seconds kept asking for frames forever — its clock stopped, so the toast
never expired — and re-drew its whole stage sixty times a second. Only the
entered mount is live; entering wakes it, leaving freezes it again.

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
are not drawn at all. A replaying mount draws small — a quarter of the
window's dpi, whatever the canvas shows — because a step needs fresh
hits, not pixels, and hits are logical: layout does not depend on the dpi.
It matters under the headless backend, whose render-to-texture costs
seconds per full-size stage; a first cut drew replays at full dpi and the
canvas suite went from a minute to ten.

**Replays run one mount at a time.** makepad has one key focus and one
IME, and a story that types — into the launcher's query, the settings
form, the inbox filter — cannot share the keyboard with another replaying
beside it: run in parallel, a hundred mounts fought over focus and a
quarter of them lost their typed text and every label after it. So the
canvas hands frames to one replaying mount at a time, in canvas order,
and the rest wait their turn; a replay costs one frame per step on its
way, and the total is the sum of those.

**Rendering is budgeted; zoom never waits for it.** Every frame plans
which mounts render: the entered one whenever it drew, unbudgeted; the
replaying one when it stepped, within a budget (8 ms windowed, a count
under headless, always at least one); a frozen mount once more when its
arrival is pending. A zoom change re-renders nothing on the spot — frozen
mounts show their last texture scaled (soft for a moment, like any canvas
tool mid-zoom) and re-render at the new level only once the zoom has stood
still for six frames, nearest the pointer first, within the same budget.
Whatever the budget leaves over sets `more_work`, and the next frame comes.
A replay cannot take its next step until its last one has been drawn, so a
mount whose render was deferred simply waits (`stale_hits`); makepad's own
redraw marks are folded into a per-mount `pending` flag, since a draw
event consumes them whether or not the budget let the mount render.

**Entering.** A click on a node (or its name) flies the camera to 1:1 on
it and routes the keyboard and the pointer to that stage, remapped into
its own coordinates. A flow can be continued by hand from any of its
states. A click outside, or ⌘esc, leaves. Every control is in the legend
along the bottom.

**The canvas's own suite.** `e2e/library.txt` (`#! canvas`) drives the
canvas: `wait`, `shot`, `click` on a node's name or a story's, and the
canvas chords. Its fast path is `--no-draw` — replays and labels, seconds;
under it a `shot` is logged and skipped rather than failed, for the
workspace suites too. Rendered runs, for screenshots, take a small story
set and `MAKEPAD_HEADLESS_DPI=1`, and a minute or two.

## Costs, named honestly

- **Replay is draws, one mount at a time.** Every node replays from the
  seed; the total is the sum over nodes of the steps on its way, each a
  full stage draw, one per frame. Thirteen nodes fill in within three
  seconds, the hundred nodes of the whole `e2e/` directory in about a
  minute, with the canvas at frame rate throughout (the canvas logs the
  count and the frames when the last one arrives). Sharing a story's
  prefix between its nodes would cut the total and is not done (see open
  2).
- **Zoomed-in nodes are soft until the zoom settles**, and a wheel gesture
  that never rests keeps them so. The price of never re-rendering a hundred
  mounts inside one frame.
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
- **Replays fast-forward through waits, one step per frame.**
- **Per-mount passes at zoom dpi, events remapped, actions captured.**
- **Frozen mounts are pictures**; only the entered one is live.
- **Replays are sequential**, because the keyboard is one.
- **Renders are planned per frame within a budget**; zoom re-renders are
  deferred until the zoom settles, nearest the pointer first.

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
