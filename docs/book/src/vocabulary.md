# Vocabulary

- **Panel** — one focused view, identified by a kind and its parameters. For
  example, a mailbox panel includes its folder role and a message panel
  includes a message ID.
- **Kind** — the content a panel shows. Replacing a panel changes its kind but
  keeps the panel in the same place.
- **Workspace** — a horizontally scrolling row of columns.
- **Column** — a vertical stack of panels. All columns are equal; there is no
  "main" column.
- **Grid** — the units used to size panels. Desktop uses 12×6; smaller screens
  use smaller grids. A panel requests a width and height within the grid.
- **Wish** — a panel size calculated from its content. It cannot be smaller
  than the kind's default or larger than the grid.
- **Mailbox** — a mail list for one folder role: inbox, archive, sent, or
  spam. All four use the same panel kind.
- **Thread** — a conversation: every mail that answers, or is answered by,
  another, joined through their `References` headers. A mailbox's rows are
  threads — one row per conversation with a mail in that folder — and a
  message panel shows the whole thread its mail belongs to.
- **Mark** — a row selected for a batch action. Marks use stable row keys, so
  they survive filtering, paging, and sync updates.
- **Marks bar** — the actions and counts shown while at least one row is
  marked.
- **Join** — a relationship between a panel and a child opened from it. A new
  joined child from the same parent replaces the old child.
- **Bridge** — the ═ symbol between two joined panels.
- **Chain** — a join of a join (inbox → message → contact). Replacing a panel
  closes its chain.
- **Camera** — the visible horizontal position of a workspace. It follows
  focus and moves directly with trackpad input.
- **Scene** — the target position and size of each panel, plus the target
  camera position. The shell animates toward these targets.
- **Ghost** — the fading image of a panel after it closes.
- **Toast** — a short status message in the bottom-right corner.
- **Problem** — a standing background condition (a failed sync, a failed
  send, an unreachable bucket), derived from the rows that carry it. The
  **mark** in the toast's corner counts them; the problems panel lists them.
- **Effect** — work outside the database, such as using the network, keychain,
  clipboard, clock, or disk.
- **Job** — an effect saved in the database so it can be retried. The Effects
  panel shows jobs together with recent effects that were not saved.
