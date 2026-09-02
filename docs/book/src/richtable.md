# The Rich Table

Every list panel is the same engine over a different **datasource**: the
inbox today, feeds and calendar events when they arrive. The engine
(`src/richtable.rs`, std-only) owns a **filter** and a **paging window**
and holds no rows; the panel widget draws what the engine hands it. The
design is CR-006 (`docs/planning/`), a port of stelaxis's rich table with
the paging rebuilt for an in-process store.

## Datasources

A datasource answers, under the current filter: *how many rows*, *rows
`offset..offset+n`*, and *where does this row sit*; and it declares the
**tags** its filter accepts, each typed (boolean, text, number, date) with
a one-line description and, optionally, its values — a closed set, or
*dynamic*, asked of the source as the operator types.

A SQL-backed source is declared as a `static` beside the domain's other
queries (`mail::INBOX`): the fixed parts of its query — columns, `FROM`,
a base `WHERE`, the text-search columns, tag bindings, the order — plus a
row decoder, the row's order key, and a function for its dynamic values.
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

The inbox's tags: `@unread`, `@html`, `@from:` (senders, dynamic),
`@subject:`, `@date`, `@account:` (dynamic).

## Autocomplete

Typing `@` opens a box under the field, over the rows, with the tags that
match what follows it; picking one lands `@name ` for a boolean or
`@name:` for a tag that takes a value — and the value list opens at once.
A closed set completes on label or value (the label shows, the value
lands); a dynamic tag's values come from the datasource with the typed
prefix, on the spot — in-process there is nothing to wait for. A quoted
value keeps its spaces while it is typed. Arrows walk the offer, `enter`
and `tab` take it, `esc` puts it away (a second `esc` leaves the field);
a pick keeps the field's focus. The box is capped at eight rows.

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
A mail that moved under the cursor — a sync landed above it — is found
again without walking anything; a mail that left leaves the cursor on the
row it stood on.

## Drawing a long list

Three measures keep a frame proportional to what is on screen: the data
is virtual (pages, above); the `PortalList` reuses row widgets instead of
minting them as rows scroll in; and a row is repopulated only when the
mail or its selection changed, judged by a stamp the panel keeps per live
row.

## Adding a table

Declare the `SqlSpec` and its `TagDef`s beside the domain's queries, wrap
them in a `SqlSource` with the row decoder, the order key and the
suggestion function, and give the panel widget a `Table` over it. The
autocomplete is a `Suggest` over that table, as the inbox's is; the
filter field, the error line and the paging loop are the inbox panel's,
and the next list kind lifts them as they are.
