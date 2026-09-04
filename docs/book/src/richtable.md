# The Rich Table

The rich table is one component in two halves. The kernel half is state a panel
instance owns; the shell half is one widget that draws any such state.
Mailboxes, the effect log, and the file browser are three panels over one
component.

## The kernel half: state the panel owns

`ListState<D>` is the whole of what a panel that shows a list holds: the
`Table<D>` over a `Datasource`, the cursor as a row, its key and an index, the
`Marks`, and the marked rows the filter currently hides. The panel owns it;
nothing about a list reaches the shell.

A `Datasource` supplies rows, a count, stable row keys and their text spelling,
filter tags, and completion values. `Table<D>` owns the typed filter and the
loaded pages for one source. None of it depends on Makepad.

`SqlSource` builds queries from a static `SqlSpec`. A spec defines selected
columns, source tables, base conditions, searchable text columns, tags, order,
row decoding, stable keys, and dynamic suggestions. Queries go through the
store cache with dependency tracking, so pages update after a relevant commit.

A source may group several records into one row. A mailbox's rows group
messages by their thread anchor: the filter matches the group when any member
matches, while the displayed totals still cover the whole conversation.

`DirSource` implements the same interface over an in-memory directory listing.

## Filter syntax

| Pattern | Example | Meaning |
|---|---|---|
| `@tag` | `@unread` | Boolean tag |
| `@tag:value` | `@from:vera` | Equal; text values use contains |
| `@tag:"a b"` | `@subject:"panel model"` | Value containing spaces |
| `@tag>value`, `>=`, `<`, `<=` | `@date>30.08.2026` | Comparison |
| `@not:tag` | `@not:unread` | Opposite of a tag |
| `(@a @or @b)` | `(@unread @or @html)` | Either expression |
| `@a @b` | `@unread vera` | Both expressions |
| `text` | `budget draft` | Substring in searchable columns |

Invalid syntax and unknown tags appear as errors below the field. The valid
parts still filter: an invalid or unknown term is left out of the SQL rather
than hiding every row. An unfinished tag at the caret is not reported until the
caret moves away.

Dates use `dd.mm.yyyy`; an optional time narrows to a minute. Date equality
means the matching day or minute, while comparisons use that range's edges.

Negation includes rows where the inner value is absent. This matters for a tag
such as `@not:risky`: in SQL, plain `NOT NULL` is still unknown and would lose
those rows.

Which tags a table offers is the source's; [mail](./mail.md#four-mailboxes-one-list),
[files](./files.md#the-directory-list), and the
[effect log](./data-substrate.md#effects-and-job-panels) each list their own.

## Autocomplete

Typing `@` opens up to eight suggestions below the field. Choosing a boolean
tag inserts `@name `; choosing a valued tag inserts `@name:` and opens its
values. Suggestions can be fixed in the tag definition or supplied by the data
source.

Arrows choose an item, `enter` or `tab` accepts it, and `esc` closes the box. A
second `esc` leaves the field. With no box open, down moves from the filter to
the first result row.

Completion is a shared text interface in the kernel, not part of a specific
widget: the filter's tag grammar and a compose panel's recipient list are two
implementations of it, and one shell widget draws the box, handles its keys,
and splices the pick.

## Paging and the cursor

SQL sources count all matching rows and load pages of 50 as the visible area
needs them. A commit invalidates stale pages, but only visible pages are loaded
again. Sources that cannot count grow their loaded window by one page when the
end becomes visible.

The cursor is the row it stands on — the row, its key and an index — and
resolves in that order: the remembered index while it still holds the key, else
the key's rank in the current order (a row landed above it), else the row's own
rank: where the order would put a row like it now, which is past every row that
outlived it and no further. Without the index a row filed out from under the
cursor would snap the walk back to the top; without the row's rank a batch that
took rows from above the cursor as well as under it would carry on from a stale
index, one row further down for each row that left. A mailbox cursor identifies
the conversation, not whichever message that conversation currently opens.

## Marks

Marks select rows for a batch action. They are stored as stable keys, so they
survive filters, paging, and updates from a worker. `Marks` can toggle a key,
add a range, select every matching key, keep failed items, and clear the set.

A source can list all matching keys, test which marked keys still match, and
read a row by key without applying the current filter. Hidden marks therefore
remain selected and still take part in actions. A key whose row no longer
exists is dropped on the next draw. **mark all** means every filtered row,
including unloaded pages.

Filtering and cursor movement do not change marks; `esc` and **clear** remove
them. A batch checks each key separately, removes the keys that succeeded, and
leaves refused ones marked, so what stays marked is exactly what could not be
done. If every item is refused, no history node is created.

Marks are the instance's own context, not data, so they create no history node
of their own. A batch verb adds an intent that holds a handle to its own table,
which is how undo puts the marks back where they were.

## The shell half: one widget for any list

The shell's table widget borrows the `ListState` from the scope on every draw
and every event, through the instance's `as_any`, and everything it changes it
changes there. It holds no state that belongs to a panel.

It draws the filter with its error line and completion box, the rows through a
`PortalList` with the hidden-marks band above them, the cursor wash and the
mark bar, and it registers the hits that make a row addressable. It answers a
press itself, by the row rectangles of its last draw, because portal-list items
are rebuilt every draw and a synthesized press must land the way a finger does.
A press moves the cursor and previews; `cmd` opens a fresh un-joined panel
instead.

It also owns the keys the [grammar](./interaction-grammar.md#keyboard) gives a
list: the arrows and their preview, `enter`, `/`, `space`, `shift`+arrows,
`esc`, and `tab`. While the caret is in the filter it tells the shell that it
keeps every letter, so no bar draws a bold letter that would not fire.

A walk keeps its cursor in sight. The row the arrow lands on is brought whole
into the panel by the overlap alone — a step scrolls by a row, never by a page
— and it is the rectangles of the last draw that say so, because a portal list
draws the row that straddles either edge as readily as the ones in plain
sight. A cursor that is nowhere on screen, the walk taken up again after the
list was scrolled away from it, is a jump rather than a step: the viewport
animates to it and lands it at the top.

Where the viewport stands is about the rows it was left on. A new filter is a
new list and answers at the top; rows that go out from under an unchanged one
— a batch verb files forty at once — leave it pinned to the last row. Neither
may leave it standing past the end, where the panel would draw nothing at all.

What a panel supplies is four short functions: the row template, how to fill a
row, what a script calls it, and what it opens. Two more have defaults: what
the field is seeded with once, the line an empty list shows, and the two verbs
a sideways finger runs.

Pages keep data loading proportional to the visible area, and `PortalList`
reuses row widgets while scrolling, so a row is populated again only when its
data, cursor state, or mark state changes.

A finger is the shell's to arbitrate and the table's to answer. Rows are
registered as rows, so the shell knows one is under a finger without knowing
whose. The three questions a gesture over one raises go back to the widget:
which row is this, what would a sweep across it run, and run it. A long press
marks the row. A sweep runs one of two verbs the panel names by id, over that
row alone: it is marked, the panel's own batch verb fires, and whatever was
marked before goes back on. A table that names no verb that way draws no
curtain.

## Adding a table

For a SQL table, define a `SqlSpec` and its `TagDef` values near the domain
queries. Choose a stable key: the group key for grouped rows, or a unique final
order column for flat rows. Provide a row decoder, an order key, and a
suggestion function, then construct the `SqlSource` and put a `ListState` on
the panel instance. On the shell side, declare the row template in the app's
own script block and implement the four functions above.

The effect log is the flat example. Its SQL source is the database queue
combined with the in-memory effect ring through `UNION ALL`, and its spec
declares the ring as a dependency because SQLite's query tracker cannot
discover changes outside database tables. See
[Recent in-memory effects](./data-substrate.md#recent-in-memory-effects).
