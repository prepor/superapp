# The Rich Table

The rich table is the shared list engine used by mailboxes, the Effects panel,
and the file browser. It provides filtering, completion, paging, a stable cursor,
and marks. Widgets decide how rows look and what actions mean.

## Data sources

A `DataSource` provides rows, counts, stable row keys, filter tags, and
completion values. `Table<D>` owns the typed filter and loaded pages for one
source. It does not depend on Makepad.

`SqlSource` builds queries from a static `SqlSpec`. A spec defines selected
columns, source tables, base conditions, searchable text columns, tags, order,
row decoding, stable keys, and dynamic suggestions. Queries use the store cache
and dependency tracking, so pages update after relevant commits.

A source may group several records into one row. Mailbox rows group messages by
`message.thread`. A filter matches the group when any member matches, but the
displayed totals still cover the full conversation. Inbox, archive, sent, and
spam each use a static spec generated from the same definition.

`DirSource` implements the same interface over an in-memory directory listing.
It reloads when the panel changes directory or one of Superapp's own actions
changes the disk. It does not watch changes made by other programs.

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
parts still filter. An invalid or unknown term is left out of the SQL rather
than hiding every row. An unfinished tag at the caret is not reported until the
caret moves away.

Dates use `dd.mm.yyyy`; an optional time narrows to a minute. Date equality
means the matching day or minute, while comparisons use that range's edges.

Negation includes rows where the inner value is absent. This matters for a tag
such as `@not:risky`: in SQL, plain `NOT NULL` is still unknown and would lose
those rows.

Mailbox tags are `@unread`, `@html`, `@from:`, `@subject:`, `@date`, and
`@account:`. `@from:` suggestions come only from the current mailbox.

## Autocomplete

Typing `@` opens up to eight suggestions below the field. Choosing a boolean
tag inserts `@name `; choosing a valued tag inserts `@name:` and opens its
values. Suggestions can be fixed in the tag definition or supplied by the data
source.

Arrows choose an item, `enter` or `tab` accepts it, and `esc` closes the box. A
second `esc` leaves the field. With no box open, down moves from the filter to
the first result row.

Completion is a shared text interface in `richtable::Completion`, not part of a
specific widget. `panels::Suggest` draws it. Mail filters use one implementation;
the compose recipient field uses `mail::Recipients`. Recipient completion
searches known senders by name or address, inserts the bare address, and omits
addresses already entered.

## Paging and the cursor

SQL sources count all matching rows and load pages of 50 as the visible area
needs them. A commit invalidates stale pages, but only visible pages are loaded
again. Sources that cannot count grow their loaded window by one page when the
end becomes visible.

The cursor stores a row key, not only an index. SQL sources calculate that row's
rank in the current order. If new rows arrive above it, the cursor finds the same
row at its new position. If the row disappears, the cursor stays near its old
position. A mailbox cursor identifies the conversation, not whichever message
the conversation currently opens.

## Marks

Marks select rows for a batch action. They are stored as stable keys, so they
survive filters, paging, and sync updates. `Marks` can toggle a key, add a range,
select all matching keys, keep failed items, and clear the set.

A source can list all matching keys, test which marked keys still match, and
read a row by key without applying the current filter. Hidden marks therefore
remain selected and still take part in actions. A key whose row no longer
exists is removed. **All** means all filtered rows, including unloaded pages.

Filtering and cursor movement do not change marks. `esc` and **clear** remove
them. A batch action removes successful keys and leaves refused keys marked.
Undo restores marks consumed by that action. Marks themselves are temporary UI
state and do not create history nodes.

Marked rows show a left bar. Hidden marked rows appear above the normal rows.
The marks bar at the bottom shows counts, row actions, **all**, and **clear**.
Mail offers archive and delete where valid. Files offer copy, move, and delete.

A batch checks each key separately. Unsupported mail folders or refused file
paths remain marked while valid items proceed. If every item is refused, no
history action is created.

`panels::PanelMarks<D>` contains the shared panel-side behavior: hidden rows,
counts, row state, keyboard controls, and marks bar. Each panel still supplies
its row key and batch actions.

## Drawing long lists

Pages keep data loading proportional to the visible area. Makepad's
`PortalList` reuses row widgets while scrolling. A row is populated again only
when its data, cursor state, or mark state changes.

## Adding a table

For a SQL table, define a `SqlSpec` and `TagDef` values near the domain queries.
Choose a stable key: the group key for grouped rows, or a unique final order
column for flat rows. Provide a row decoder, order key, and suggestion function,
then construct `SqlSource` and `Table` values.

The effect source in `effect::LOG` is a flat example. Its SQL source is the
database queue combined with the in-memory effect ring through `UNION ALL` and
`mem_effects()`. The spec declares the ring dependency because SQLite's
query tracker cannot discover changes outside database tables. See
[Recent in-memory effects](./data-substrate.md#recent-in-memory-effects).
