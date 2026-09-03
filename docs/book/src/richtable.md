# The Rich Table

Every list panel is the same engine over a different **datasource**: the
inbox and [the effect log](./data-substrate.md#the-log-panel) today, feeds
and calendar events when they arrive. The engine
(`src/richtable.rs`, std-only) owns a **filter** and a **paging window**
and holds no rows; the panel widget draws what the engine hands it. The
design is a port of stelaxis's rich table, with the paging rebuilt for an
in-process store.

## Datasources

A datasource answers, under the current filter: *how many rows*, *rows
`offset..offset+n`*, and *where does this row sit*; and it declares the
**tags** its filter accepts, each typed (boolean, text, number, date) with
a one-line description and, optionally, its values — a closed set, or
*dynamic*, asked of the source as the operator types.

A source need not be SQL, and one is not: a files panel's `DirSource` holds
one directory's listing — read through the [outside](./data-substrate.md),
not the store — and answers the same three questions off it, evaluating the
filter's AST in Rust over the entries. Everything above the datasource is
unchanged by that: the same grammar, the same autocomplete, the same error
line, the same paging. What it costs is what the store was giving for free —
the listing is not reactive, so a panel re-reads when it lands on another
directory rather than when the disk moves under it.

A SQL-backed source is declared as a `static` beside the domain's other
queries (`mail::THREADS`): the fixed parts of its query — columns, `FROM`,
a base `WHERE`, the text-search columns, tag bindings, the order — plus a
row decoder, the row's order key, and a function for its dynamic values.

A source may declare a **group key**, and then a row is a group: the
columns are aggregates over its members, the page reads off the grouped
subquery, and the filter becomes a membership test — a group matches when
**any** member matches, and its aggregates always cover the whole group.
A mailbox is such a source: its rows are threads, grouped by
`message.thread` over the messages of one folder role, so `@from:vera` finds
the conversations Vera wrote in and still shows everyone in them, and
`@unread` finds the ones with unread mail. There are four of them — inbox,
archive, sent, spam — written out at compile time by a macro over the role's
name, because a `SqlSpec` is `static` text: that is what lets one builder,
one rank and one page cache serve four lists without a string being
formatted per keystroke.
The **SQL builder** completes that with the filter's `WHERE`, the page and
the rank. A built query goes through the store's cache, dependency capture
and trace exactly like a `static` one (`Store::rows_sql`), so a page is
reactive, and `cmd+i` shows the SQL that ran.

## The filter grammar

| pattern | example | meaning |
|---|---|---|
| `@tag` | `@unread` | a boolean tag |
| `@tag:value` | `@from:vera` | equals — for text, *contains* |
| `@tag:"a b"` | `@subject:"panel model"` | a value with spaces |
| `@tag>value` `>=` `<` `<=` | `@date>30.08.2026` | a comparison |
| `@not:tag` `@not:tag:value` | `@not:unread` | negation |
| `(@a @or @b)` | `(@unread @or @html)` | a group; members are OR'ed |
| `@a @b` | `@unread vera` | implicit AND |
| `text` | `budget draft` | free text: one substring over the text columns |

The parser (`src/filter.rs`) never refuses a line: what it cannot read
becomes an error shown under the field, in the one colour errors get, and
the rest still filters. An unknown tag is an error too and is *dropped*
from the query — the table shows everything rather than nothing; so is a
value a typed tag cannot read (`@date>yesterday`). A tag being typed at
the end of the line is not wrong yet, so its error waits until the caret
leaves. A date is written `dd.mm.yyyy` and is a **span**: `@date>D` means
after that day, `@date:D` on it, `@date:"30.08.2026 09:14"` that minute.

A mailbox's tags: `@unread`, `@html`, `@from:` (senders, dynamic),
`@subject:`, `@date`, `@account:` (dynamic) — one table for all four lists,
because the grammar of a mail list does not change with the folder it is
over. Each reads against that folder's messages; a thread matches when any
of them does. `@from:` completes against the senders of the list it is on:
the spam one offers spammers, and it is the only place they are offered.

## Autocomplete

Typing `@` opens a box under the field, over the rows, with the tags that
match what follows it; picking one lands `@name ` for a boolean or
`@name:` for a tag that takes a value — and the value list opens at once.
A closed set completes on label or value (the label shows, the value
lands); a dynamic tag's values come from the datasource with the typed
prefix, on the spot — in-process there is nothing to wait for. A quoted
value keeps its spaces while it is typed. Arrows walk the offer, `enter`
and `tab` take it, `esc` puts it away (a second `esc` leaves the field);
a pick keeps the field's focus. The box is capped at eight rows. With no
box up, `↓` belongs to the field's own panel: it hands the keyboard to the
rows and lands on the first — where `enter` lands, but as a walk rather
than an opening.

The box is not the filter's. What a field completes is a **completion**
(`richtable::Completion`): how the caret's context is read off the line,
what is offered for it, and how a pick splices back in — pure text, tested
without a widget. The table is one completion (the grammar above); the
compose panel's TO field is another (`mail::Recipients`): the
comma-separated token under the caret, matched as a substring against
every sender the store has heard from, by name or address — the `@from:`
offer, landing in a different field. A pick lands the bare address; an
address already in the list, or one typed out in full, is not offered.
The box, its keys and the pick are one component (`panels::Suggest`) that
takes any completion; a panel holds one per field that completes and
draws it last, so it covers what follows the field instead of pushing it.

## Paging and the cursor

The table's length is one `COUNT(*)` under the filter. Rows come in pages
of fifty by offset, fetched when a draw needs an index under the viewport
— so a hundred-thousand-row list costs a frame its visible rows, the
scrollbar is honest, and jumping to the middle is one page fetch. A commit
invalidates the pages like any query; only the ones on screen re-run,
lazily. There is no *load more*: a source that cannot count falls back to
a window that grows by a page each time the end of the list comes on
screen.

The list's cursor is kept by **rank**: a row's position is a `COUNT(*)`
of the rows the order puts before it, built from the source's order key.
A thread that moved under the cursor — a sync landed above it — is found
again without walking anything; a thread that left leaves the cursor on the
row it stood on. The cursor's identity is the thread, not the mail it
opens, because that mail changes as replies arrive.

## Marks

A table carries **marks** beside its cursor: the rows picked out for a batch
verb, held as a set of keys (`Marks`) rather than rows, so a mark survives
the filter, the paging and a sync landing underneath. The set is std-only
and knows nothing about rows or widgets — toggle one, add a range, keep
what a verb could not do, clear.

The datasource answers three more questions for them, all under the current
filter, all one query on a `SqlSource`: **every matching key**, which is
what `all` marks; **which of these keys** still match, which sorts a set
into shown and hidden; and **the row for a key**, under the source's base
`WHERE` and nothing else. A `SqlSpec` names its key: the group where there
is one — a thread is its `message.thread` — else the unique column its
order ends in. A source that is not SQL answers the same three off
whatever it holds: `DirSource` reads them from its listing, keyed by entry
name, and its base `WHERE` is the directory itself — so a dot-file the
filter hides is hidden, not gone.

`Table::split` sorts a set into what the filter shows and what it hides.
**A hidden mark is still a mark**: it counts in the bar, it is drawn above
the rows, and a batch verb acts on it — read fresh by its key on every
draw, never from a snapshot taken when it was marked. A mark whose row is
gone altogether — the thread left the inbox, the entry left the listing —
is dropped at that same point: the bar counts rows that exist. And **`all`
means all matching** — the table knows its count and the source can list
its keys, so the rows off
screen are marked too (`all 143 marked`).

**Marks are context, not intent.** Filtering, walking and syncing never
touch the set; `esc` and `clear` empty it; a batch verb consumes what it
acted on and leaves what it could not do marked. They are never a history
node of their own: the node a batch verb writes carries the keys it
consumed, so an [undo](./interaction-grammar.md#undo) puts the rows back
marked. They live in the panel's memory and go with the process, like the
typed filter.

A list draws three things for them. A marked row wears an ink bar down
its left edge, inside the row's own inset, so nothing reflows when the
first mark lands. The **marks bar** stands at the panel's **foot** while
the set is not empty — `3 of 143 marked`, `all 143 marked`, or
`3 marked · 1 hidden by the filter` — carrying the list's own row verbs on
the set, then `all` and `clear`, beside the count or under it where the
width cannot hold them. Which verbs those are is the list's to say
(`ui::mark_verbs`): the inbox `archive` and `delete`, a files panel
`copy`, `move` and `delete`.
Under the list rather than over it, so it takes its height off the rows'
own scroll instead of pushing every row down as the first mark lands; it
wears the header's rule on its other side, and the rows end at it. And the
marks the filter hides ride above the rows *in the same `PortalList`*, as a
prefix: a caption, the rows, a strong rule closing the group. The arrows
walk the table, so they never visit them.

A batch verb runs its pre-flight over the set, thread by thread: one whose
account has no such folder is skipped rather than failing the batch, and
**stays marked**, so what is left marked afterwards is exactly what the
verb could not do — `archived 10 of 12 conversations — 2 have no archive
folder — ⌘z undoes`. When nothing in the set can be filed there is no
action at all: the toast is the single row's error and the set stands as it
was. A files panel's batch refuses the same way, path by path: `copy` and
`move` hold every marked path the way they hold one, and a `copy here`
performs the set into the directory a panel shows — reaching the disk, and
refusing per path exactly as it does for one. `delete marked` takes the set
to the trash, and one `⌘z` brings the files back with their marks.

None of that is the inbox's any more. `Marks` and the datasource's three
questions were always the engine's; the panel side — the set beside the
table, the hidden rows, the prefix arithmetic, the per-row stamps, the
space / shift+arrow / esc keys and the bar itself — is one piece
(`panels::PanelMarks<D>`) that a list holds beside its `Table`, generic
over the same datasource. What stays a panel's own is only what a mark
*means*: which row the cursor is on, which verbs the bar wears, and what a
batch of them does. [A files panel](./interaction-grammar.md#files-a-directory-is-a-column-a-file-is-a-card)
was the second list to want them, and cost the widget nothing but its own
row twins.

## Drawing a long list

Three measures keep a frame proportional to what is on screen: the data
is virtual (pages, above); the `PortalList` reuses row widgets instead of
minting them as rows scroll in; and a row is repopulated only when the
mail, its selection or its mark changed, judged by a stamp the panel keeps
per live row.

## Adding a table

Declare the `SqlSpec` and its `TagDef`s beside the domain's queries, naming
the column that *is* a row — the group where there is one, else the unique
column the order ends in — so its marks have an identity; wrap them in a
`SqlSource` with the row decoder, the order key and the suggestion
function, and give the panel widget a `Table` over it. The autocomplete is
a `Suggest` over that table, as the inbox's is; the filter field, the error
line and the paging loop are the inbox panel's, and the next list kind
lifts them as they are.

The effect log (`effect::LOG`) is what that costs, measured: a flat spec,
eleven tags, a row decoder shared with the queue's own helpers, and a panel
that lifted the inbox's filter, error line and paging loop unchanged. It
carries no aggregate and no group key — so it is also the plain case the
inbox's grouping is the exception to.

Its `from` is the one that is not a table: the queue `UNION ALL` the
in-memory effect ring, read through a scalar function
([the ring](./data-substrate.md#the-ring-the-effects-that-were-never-rows)).
Everything above the `FROM` is unchanged by that — which is the point of
putting the join in SQL rather than in the panel. The one thing a spec has
to say for itself there is `deps`: rows that were never in the database are
invisible to the authorizer, so a query that reads them names that
dependency instead of having it captured.
