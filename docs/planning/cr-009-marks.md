# CR-009 · Marks: batch selection and batch actions for the rich table

Status: **proposed** (Andrey, 2026-09-02: stelaxis's rich table had batch
selection and batch actions before UI v2 — the same functionality for our
rich tables, with a UI of our own). The surface is drafted in the panels
library — the scenes *inbox row* (its `marked` states), *marks bar* and
*inbox, marked* — for review before anything is wired.

## Why

A list that acts on one row at a time makes triage a walk: archive, arrow,
archive, arrow. Stelaxis's rich table (v1, before the cockpit) had the other
half — select rows, act on the set — and one idea in it is worth keeping
whole: the selection **survives the filter, the sort and the paging**, and
the selected rows a filter hides are shown *pinned* above the table, so a
batch verb never acts on a row the operator cannot see. It also left a list
of things undone — nothing cleared the selection after an action, no
confirmation, no result back, the rows handed over in arbitrary order,
select-all reaching the loaded rows only — and no product page ever used
it. The extraction, file and line, is in the workspace's
`.context/richtable-batch-selection-stelaxis-v1.md`.

This CR ports the idea, not the chrome, and decides what stelaxis left open.

## The word

The inbox already has a **selected** row — the cursor's wash, `sel` in the
panel, `selected` in the row's scene — so a second selection needs a second
word. A row is **marked**; the **marks** are the set; the **marks bar** is
what a list shows while any exist. (mutt tags, gmail ticks; the word is the
one the mark itself suggests.)

## The model

**Marks are a set of row keys beside the cursor.** The cursor's identity is
the thread anchor (CR-007); a mark's is the same key, so a row keeps its
mark through everything the store does underneath it — a reply arriving,
a sync landing above it, a page re-deriving. `richtable::Marks<K>` is a
`BTreeSet` with the four moves the surface needs — toggle one, extend over
a range, take a whole set, clear — and nothing about rows: std-only, tested
without a window, held by the panel next to `sel`.

**The datasource answers three more questions**, all under the current
filter, all one query on a `SqlSource`:

- the **keys** of every row that matches — `SELECT key … WHERE filter` —
  which is what `all` marks;
- **which of these keys** are under the filter — the same query with
  `key IN (…)` — which sorts the marks into *shown* and *hidden*;
- **the row for a key** — the inbox has `thread_head` already.

A `SqlSpec` names its key: the `group` where there is one (a thread is its
`message.thread`), else the unique column its order ends in. The engine
stays stateless about the data, as CR-006 made it.

**Marks are context, not intent.** The book already says so of row
selection: never a history node, restored only as part of a real action's
delta. So filtering, sorting, walking and syncing never touch the set;
`esc` and `clear` empty it; a batch verb consumes what it acted on; and
undoing that verb puts the rows back *marked*, the way undo carries the
cursor back today — a mis-fire is corrected by ⌘z and done again
differently, not re-marked by hand. Marks live in the panel's memory and go
with the process, like the typed filter.

**A mark the filter hides is still a mark.** It counts in the bar, it is
listed above the rows under its own caption, and it is in the set a verb
acts on. This is stelaxis's pinning, with the one thing it could not do
done: the hidden row is read fresh by its key, not shown from a snapshot
taken when it was marked.

**`all` means all matching.** Stelaxis could only reach the rows it had
loaded, and that set grew with every *load more*. Here the table knows its
count and the source can list its keys, so `all` is honest: every row under
the filter, the ones off screen included — `all 143 marked`.

## The surface

### The mark

A marked row wears an **ink bar down its left edge**, 3 pt, inside the
row's own 8 pt inset: no reflow, the text stays on the header's columns,
and a hundred marks read as a margin ticked in a book. With the cursor on
it, the bar rides the wash. It is shader-drawn like the dotted underline,
two more twins of the row's line rather than a flag (a quad's colour is not
settable at draw time — `OverlayRow`'s reason).

There is no checkbox and no gutter column: nothing appears on a row until
it is marked, and nothing shifts when the first one is.

### Keys and pointer

- **space** toggles the mark on the cursor's row. It arrives as text, the
  way `/` does — the one plain key the grammar keeps — so while the filter
  owns the keyboard it types a space, exactly as `/` types a slash there.
- **shift+↓ / shift+↑** mark the cursor's row and step: a range, by the
  arrows the walk already uses. Cmd+arrows stay the workspace's.
- **esc** clears the marks, when no field is listening (it is *leave the
  field* first, as everywhere).
- **click the row's gutter** — its left 12 pt, the mark's own place — to
  toggle it; the row's body still previews. **shift+click** marks the range
  from the cursor's row to the clicked one. The gutter is one more hit in
  the shell's table, labelled `mark <topic>` for the harness.
- **touch**: a long press on a row marks it (a header's long press drags a
  panel; a row's is free); while any mark exists a tap toggles rather than
  opens, and the last mark cleared gives the tap back. The bar's controls
  are buttons, so nothing is keyboard-only.

### The marks bar

Between the filter and the header row, while the set is not empty:

```
3 of 143 marked    archive  delete  all  clear
```

The count at body size; then four **bordered side-effect buttons** — the
grammar's one button, never navigation — each wearing its letter:
**a**rchive, **d**elete, a**l**l; `clear` wears none, `esc` is its key.
`all` stands down once everything under the filter is marked (`all 143
marked`). With a mark outside the filter the count says so, muted —
`3 marked · 1 hidden by the filter` — and the verbs drop under the count
when the line cannot hold them, as they do at the phone's width. (A `Fit`
child never wraps in this makepad; the bar measures its texts and decides
at draw, where the width is known.) Nothing is drawn for an empty set: the
bar comes with the first mark and goes with the last, so a list that is not
being triaged looks exactly as it does today.

**The letters.** `archive` and `delete` are the verbs a single row already
has, on the message panel's chrome, borrowed by the inbox while it previews
(⌘a, ⌘d). A batch verb is the same verb on a wider set, so it takes the
same letter — nothing to memorise — and two visible controls may not answer
to one chord, so **while the bar is up the borrowed keys stand down**, the
mechanism the filter already uses when it holds the keyboard. `accels` grows
a second argument for it, and the table's tests hold both states to the
rules. `l` for `all` is the first free letter in the word; it is a choice to
review.

### The hidden marks

Under the header rule, before the rows, while a mark is outside the filter:

```
MARKED · HIDDEN BY THE FILTER
▌ me, Elena, Vera · 4                              30.08
  Q3 infra
━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━
```

The caption in the section register, the rows the same `InboxRow` with
their bars, a strong rule closing the group. They can be unmarked from
their gutter and opened by a click like any row; the arrows do not visit
them — they stand outside the table's order, and the walk is the table's.

## The batch verbs

**archive** and **delete**, on the marked set, the way `triage` does one
row (CR-007): every inbox mail of every marked thread, one `Filed` intent
each, **one node** — `archive 12 conversations` — so one ⌘z takes the whole
batch back. The toast says what happened and how to undo it:

```
archived 12 conversations (31 mails) — ⌘z undoes
```

**The pre-flight runs over the set.** A thread whose account has no such
folder is skipped rather than failing the batch: the toast says
`archived 10 of 12 — 2 have no archive folder`, and the two **stay marked**,
so the marks after a verb are exactly what it could not do. With nothing
skippable the bar goes.

**No confirmation.** Every action here is undoable and every toast names
its ⌘z; a dialog before a batch archive would be the one place the app asks
twice. Delete moves to trash, recoverable by undo and by the server's own
trash after that — the same as a row's delete, only more of it.

**The cursor carries on.** If its row went with the batch it moves to the
nearest row that stayed, previewing as the walk does, in the same node —
the rule `triage` already follows for one row.

**Readers close.** A message panel open on a marked thread closes with the
filing, on whichever workspace, as for a single archive.

## What the draft shows

In the panels library — `--library "inbox row" "marks bar" "inbox, marked"`,
or headless with the review script and its stills kept in the workspace's
`.context/marks-draft/` (`review.txt` enters every new node at 1:1 and
shoots it):

- **inbox row** — `marked`, and `marked, cursor` beside the existing
  `selected`: the bar alone, and the bar over the wash with bold still
  meaning unread.
- **marks bar** — `three` (of 143), `hidden` (a mark the filter hides,
  counted and said), `all` (the button stands down), `narrow` (the phone's
  width, the verbs wrapped).
- **inbox, marked** — the inbox as stills: `none` (no mark, no bar — the
  list as it is today), `three`, `filtered` (the hidden section over two
  matches), `all`, `phone`.

The stills are a composite widget with fixed slots (`InboxDraft`), no store
behind it. It is scaffolding for this review and goes when the inbox panel
draws the real thing.

## To decide in review

- The mark as an ink bar in the inset, against a glyph in a gutter column.
- `space` as the mark key, against `x`.
- `l` for `all`, against no letter.
- The hidden section, against a count in the bar only (`1 hidden`) with
  `all` clearing the filter.
- The touch mapping — a long press to mark, then taps toggling.

## Not done, on purpose

- **Sorting** stays out (CR-006 left it out); marks are keyed, so they will
  survive it when it comes.
- **Marks across restarts.** The typed filter is ephemeral; so are the marks.
- **Verbs beyond archive and delete.** A batch *read* / *unread* is a
  natural third and waits for the row's own.
- **Other tables.** The inbox is the only list panel today. `Marks<K>` and
  the datasource's three questions are generic; the bar and the gutter are
  the inbox's until a second list wants them, when they lift like the
  filter field did.
