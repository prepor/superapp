# Vocabulary

- **Panel** — the only building block. A panel is a *kind + parameters*
  (`email/inbox`, `email/message {id}`), specialized on a single function.
  There are no apps and no windows.
- **Kind** — what a panel shows, parameters included. Replacing a panel swaps
  its kind in place, keeping its identity and slot.
- **Workspace / strip** — the single horizontally scrolling surface all
  columns live on, niri-style.
- **Column** — a vertical stack of panels. All columns are equal; there is no
  "main" column.
- **Grid** — the 12×6 unit grid a viewport is divided into. Every kind
  requests a width×height in grid units and gets exactly that; unused rows at
  the bottom of a column stay empty.
- **Wish** — a request measured from the content rather than fixed by the
  kind: a long letter asks for more rows than a short one. The kind's
  request is its floor, the grid its ceiling.
- **Join** — the preview-pane relation, generalized: a solid link opens its
  panel *joined* to the link's panel; the next solid link from the same parent
  replaces the joined child instead of opening another panel.
- **Bridge** — the ═ mark spanning the gap between a joined pair. The only
  join indicator.
- **Chain** — a join of a join (inbox → message → contact). Replacing a panel
  closes its chain.
- **Camera** — the strip x-offset of the viewport. It follows focus and pans
  1:1 under the trackpad.
- **Scene** — the pure layout output: discrete target rects for every panel
  plus the camera target. The shell springs towards scenes; it never animates
  inside the model.
- **Ghost** — the fading afterimage of a closed panel, drawn by the shell only.
- **Toast** — the transient bottom-right note a side-effect button leaves.
