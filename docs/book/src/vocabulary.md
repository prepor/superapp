# Vocabulary

- **Panel** — the only building block. A panel is a *kind + parameters*
  (`email/mailbox {archive}`, `email/message {id}`), specialized on a single
  function. There are no apps and no windows.
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
- **Mailbox** — a mail list over one folder **role**: inbox, archive, sent,
  spam. One kind with the role as a parameter — four panels of the same
  object, differing only in the folder their rows come from.
- **Thread** — a conversation: every mail that answers, or is answered by,
  another, joined through their `References` headers. A mailbox's rows are
  threads — one row per conversation with a mail in that folder — and a
  message panel shows the whole thread its mail belongs to.
- **Mark** — a row picked out for a batch verb. The **marks** are the set,
  held as row keys beside the list's cursor, so they survive the filter, the
  paging and a sync landing underneath; they are context, never history.
- **Marks bar** — what a list shows while any row is marked: how many of how
  many, how many the filter hides, and the verbs that act on the set.
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
- **Problem** — a standing background condition (a failed sync, a failed
  send, an unreachable bucket), derived from the rows that carry it. The
  **mark** in the toast's corner counts them; the problems panel lists them.
- **Effect / job** — anything whose result the store cannot reproduce: a
  socket, the keychain, the clipboard, the clock. The ones worth retrying are
  rows in one queue, and the `effects` panel is that queue read back.
