# Panel Model

## Workspaces

Above the strip sit **nine numbered workspaces** (niri/hyprland's model):
each is a full workspace — its own columns, focus and camera, restored
exactly on return — and an empty one is just an empty slot, no creation or
teardown. They stack **vertically**: a switch slides the view down or up a
viewport (the same springs as everything else), so workspace 3 is a *place
below workspace 2*, not a scene cut. Moving a panel to another workspace
re-homes it as its own trailing column there and **follows it** (niri's
default); its joins stay behind and die with the lost adjacency, and focus
in the old workspace falls to a neighbour exactly as on close.

## Grid and columns

The viewport is a **unit grid** with 8 pt gaps — **12×6 on desktop, 8×4 on the
unfolded phone screen, 4×3 on a cover display** (the shell switches at the
~600 dp breakpoint when a fold/unfold resize crosses it, and the springs
animate the relayout). Every kind requests grid units (inbox 4×6, message 4×3,
contact 3×2, compose 4×4, settings 4×3, problems 4×3, help 4×6, about 3×2); a
request larger than the active grid **clamps to it** — which is why a phone
shows the inbox full-screen with no phone-specific layout code. Requests are honoured
**literally** while a column's requests fit: a 3-row panel in an otherwise
empty column leaves the remaining rows empty. A column asked to hold *more*
than the grid (consume/expel deliberately over-fill) ignores the requests and
**distributes its height evenly** instead. A column is as wide as its widest
panel. Columns line up left to right on an infinite strip; the workspace
scrolls horizontally when they overflow the viewport (niri's scrolling model).

A kind's request is a **floor, not the whole story**. Where the length of what
a panel shows is knowable, the shell measures it and asks for more — today the
message panel does: a letter that does not fit its three rows asks for as many
as it needs, up to the whole column, so a long mail *opens tall* rather than
opening scrolled, while a one-liner stays short. A thread measures as its
open messages plus a line for each closed one, so opening an old message in
place grows the panel, and closing it shrinks it back. The measurement is re-taken
every time the shell recomputes targets, so a body that arrives after its
panel opened grows the panel when it lands, and nothing about it is persisted
— like the camera and the grid, it is re-derived. Downstream a measured wish
is treated exactly like a constant one: the grid clamps it, placement consults
it (a tall letter no longer fits under a neighbour, so it earns a column of
its own), and an over-filled column still splits evenly.

## Column operations and tabs (niri's)

- `cmd+[` / `cmd+]` — **consume-or-expel**: a lone panel is consumed into the
  neighbouring column on that side; a stacked panel is expelled into a fresh
  column there.
- `cmd+,` — consume the first panel of the column to the right into the
  bottom of the focused column; `cmd+.` — expel the focused column's bottom
  panel out to the right.
- `cmd+t` — **tabbed display**: the column shows only its active panel at
  full height, under a strip of title segments (active inverted). Click a
  segment, or move focus up/down, to switch tabs; the panel crossfades.
  A tabbed column remembers its active tab while unfocused, and left/right
  focus enters it on that tab.

The camera follows focus — the minimal scroll that keeps the focused panel
fully visible with one gap of margin — and pans 1:1 under a horizontal
trackpad scroll. A touch pan is free while the fingers are down and
magnetises on release to the nearest column alignment. A panel opened
*without* taking focus (a
[preview](./interaction-grammar.md#preview-the-one-open-that-does-not-go))
asks to be revealed as well, once, and loses to focus when both cannot fit.

## Placement

A new panel opens *to the right* of the panel that spawned it: into the
neighbouring column if its rows fit there, otherwise into a fresh column
inserted immediately right. A joined child always lands immediately right of
its parent (a join only lives there); an un-joined open respects an existing
joined pair and inserts after it rather than splitting it.

## Joins

- A solid link opens its target **joined** to the panel the link lives in.
- The next solid link in that parent **replaces** the joined child in place.
- **Replacing a panel closes its joined chain** — the chain to its right is
  context derived from content that just changed (open a contact for mail A,
  click mail B: the stale contact goes with it).
- A join is alive **only while the child sits in the column immediately right
  of its parent**. Any move or insert that breaks that adjacency breaks the
  join, visibly: the ═ bridge between the pair is the only indicator, always
  drawn for a live join.
- Alt (click or enter) always opens a fresh, un-joined panel.
- A **preview** is the same joined open with focus left behind, so a list's
  cursor can drive the panel beside it. It is raised to its column's shown
  tab explicitly — the usual rule promotes whatever holds focus, and a
  preview holds none. Where the pair cannot share the screen it keeps focus
  after all, and is an ordinary open.

## Focus, movement, closing

Focus is a single panel; its header inverts. Focus moves with the workspace
modifier (up/down walk the column; left/right pick the nearest panel by
vertical centre in the neighbouring column, judged on target geometry).
Moving a panel swaps within its column, merges into a neighbour column, swaps
whole columns when it travels alone, and expels into a fresh column at the
edges. Closing a panel drops focus to its nearest surviving neighbour and
removes empty columns.

Implementation: all of the above is `src/core.rs` — std-only, no rendering,
unit-tested (the web prototype's whole smoke scenario is transcribed as a
test).
