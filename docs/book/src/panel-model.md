# Panel Model

## Grid and columns

The viewport is a **12×6 grid** with 8 pt gaps. Every kind requests grid units
(inbox 4×6, message 4×3, contact 3×2, compose 4×4, help 4×6, about 3×2) and is
honoured **literally**: a 3-row panel in an otherwise empty column leaves the
remaining rows empty. A column is as wide as its widest panel. Columns line up
left to right on an infinite strip; the workspace scrolls horizontally when
they overflow the viewport (niri's scrolling model).

The camera follows focus — the minimal scroll that keeps the focused panel
fully visible with one gap of margin — and pans 1:1 under a horizontal
trackpad scroll.

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
