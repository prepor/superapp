# CR-006 · The rich table: one list engine for every list panel

Status: **implemented** (Andrey, 2026-09-02: port stelaxis's richtable —
the datasource seam, the SQL builder, the filter grammar with `@or`/`@not`
and the rest of the tags, autocomplete for tags and for a tag's values
including the lazy kind; lazy-load on scroll instead of a *load more*
button, and something smarter than a growing limit; and think about what
rendering a really long list costs).

Since CR-007 the inbox's rows are threads: the datasource declares a group
key, and the filter is a membership test over the members — see the rich
table chapter of the book.

## Why

The overview promised that "the same richtable that lists mail will list
feed items and calendar events", and the inbox had none of it: one text
field whose whole grammar was a lowercase substring, and a draw pass that
cloned every row of the inbox on every frame to find the ones that matched.
Stelaxis already has the thing the promise describes — a rich table with a
datasource behaviour, a filter parser, an Ecto query builder, an autocomplete
hook, and a load-more button. This CR ports what carries over and rethinks
the one part that was built for a browser and a server.

## The model

**A list panel is a rich table over a datasource.** The table
([`src/richtable.rs`]) owns the filter and a paging window; it holds no
rows. The datasource answers three questions under a filter — *how many*,
*rows `offset..offset+n`*, and *where does this row sit* — and one more
about itself: the tags it understands. The inbox's datasource is a
`static` in [`src/mail.rs`], declared beside its other queries: the fixed
parts of the query, the tag bindings, the order, the row decoder, the rank
key, and a function for the values of its dynamic tags.

**The filter is a grammar, not a substring** ([`src/filter.rs`], stelaxis's
`FilterParser` ported clause for clause): `@tag`, `@tag:value`,
`@tag>value`, `@not:…`, `(@a @or @b)`, implicit AND, bare text. The parser
never refuses: what it cannot read becomes an error and the rest still
filters. Bare text is still one case-insensitive substring over the same
columns as before, so nothing anyone typed yesterday means something else
today. Tags are typed — boolean, text, number, date — and the type decides
how a comparison binds: a text `:` is *contains*, a number compares
numerically, a date is written `dd.mm.yyyy` and is a *span*
(`@date>30.08.2026` means after that day, `@date:30.08.2026` means on
it). A value a typed tag cannot read is an error under the field, not a
silently dropped comparison.

**The SQL builder** turns the AST into the datasource's `WHERE`. Unlike the
Ecto version it builds one expression recursively — `NOT (…)`, `AND`, `OR`
compose over anything — rather than special-casing negation per leaf. What
does not bind (an unknown tag, a boolean given a value) is *dropped*, not
turned into "nothing matches": the table shows everything and the error
line under the field says why. The store gained the one seam this needs:
`rows_sql`, a built query going through the same cache, the same
dependency capture and the same trace as a `static` one, so a table's page
is reactive like everything else and `cmd+i` shows the SQL that actually
ran, filter and all.

### Paging: count, offset, rank — not "load more", not a growing limit

Stelaxis loads `pages × limit` rows and shows a button. The brief asked for
the load to happen on scroll and for something smarter than growing the
limit. The answer here is to stop holding rows at all:

- **Count.** One `SELECT COUNT(*)` under the filter is the table's length.
  The `PortalList` gets the real range, so the scrollbar is honest and
  jumping to the middle of a hundred-thousand-row list is a page fetch,
  not a walk.
- **Pages by offset.** A draw asks for `row(i)`; the table fetches page
  `i / 50` (`LIMIT 50 OFFSET …`) through the store's cache. A frame touches
  the one or two pages under the viewport and nothing else. A commit
  invalidates them like any query, and only the pages on screen re-run —
  lazily, on the next draw. Offset paging is usually rejected because rows
  shift under it; here every visible page is re-derived from the fresh
  state on every commit, so the window is always consistent with itself.
- **Rank instead of a keyset cursor.** The classic "smarter cursor" is
  keyset pagination (`WHERE (date, id) < (?, ?)`): robust, but it only
  loads forward, needs the previous page to ask for the next, and cannot
  jump. What the inbox actually needs a cursor *for* is its selection —
  "which row is this mail on now" after a sync landed above it. That is
  one `COUNT(*)` of the rows the order puts before it, built from the same
  order key. So the cursor is a rank query, and the walk survives anything
  the store does underneath it.
- **A source that cannot count** (a remote one, later) says `None`, and the
  engine falls back to the stelaxis shape done properly: a window that
  grows by one page each time the `PortalList` reports the end of its
  range on screen. No button. The inbox never takes this path; the unit
  tests do.

The whole inbox today is 69 rows and two pages; the design is for the day
it is not.

### The autocomplete

The context under the caret is classified the way stelaxis's JS hook does
it — the space/paren-bounded token, its first `@`, an optional `@not:`,
then `name` + operator + partial — with one improvement: a quoted value
keeps its spaces while it is being typed, so `@subject:"panel mo` still
completes. From the context the table offers:

- **tag names** matching the partial, each with its one-line description;
- **a static set** for a tag that declares one, `(label, value)` pairs,
  matched on either — the label shows, the value lands;
- **dynamic values** for a tag that declares `Values::Dynamic`: asked of
  the datasource with the typed prefix, on the spot. This is the "lazy"
  suggestion, and in-process it is not asynchronous at all — the senders
  query is cached and re-filtered per keystroke, microseconds, no debounce,
  no request id to race. The seam is the same as stelaxis's
  `suggest_values/3`; only the round trip is gone.

The box is drawn **over** the rows, not pushed into the flow (makepad's own
`CommandTextInput` pushes; a filter that shoves the table down on every
`@` reads as jumpy). It is a `View` field the panel draws last, at an
absolute rect hung off the field. The draw-call ordering trap the
selection wash documents applies in full: a stock-shader quad drawn last
still merges into an earlier call and paints under the rows. The box's
background carries its own pixel function — the hairline ink border the
design wants anyway — which is a shader no earlier call shares, so it
lands in a call of its own at the end, and its text, which cannot cross
that barrier, lands after it.

Keys while the box is open: arrows walk the offer, `enter` and `tab` take
it, `esc` puts it away (the dismissal holds until the caret moves on; a
second `esc` leaves the field as before). A pick splices into the line and
keeps the field's focus, so `@from` lands as `@from:` with the senders
already offered. Clicks go through the shell's hit table like every other
in-list control, registered after the rows they cover so they win the
overlap; the e2e harness addresses them by their label.

Errors show under the field in the one colour errors get, minus the tag
still being typed at the end of the line — `@fr` is not wrong yet.

**The box left the table** (follow-up, 2026-09-02). What a field
completes is a `Completion` — context off the line, offer for it, splice
of a pick — and the box, the keys and the pick are one `Suggest` over any
of them. The table implements the trait for the grammar above; the
compose panel's TO field is the second completion, over the senders the
store knows, so a name fragment lands an address the way `@from:` does.
The `SuggestBox` DSL is now shared too, one `suggest:` property per panel
that completes.

### What rendering a long list costs

Three measures, each answering one cost the old inbox paid per frame:

1. **The data is virtual.** `inbox_filtered` cloned every row per draw;
   the table fetches `row(i)` for the visible indices only.
2. **Widgets are reused.** The `PortalList` is `reuse_items: true`: a row
   that scrolls out is handed to the next row that scrolls in, reset to
   its template, rather than minted from the script VM.
3. **Rows repopulate only on change.** The panel keeps a stamp per live
   row — the `MailHead` it was populated with and whether it was selected
   — and skips the five `set_text`s when the stamp matches. A commit that
   flips one flag repopulates one row.

A frame over a long list therefore costs its visible rows: a count (cached),
one or two page lookups (cached), and a stamp comparison each. The
`PortalList` keeps a Fenwick tree of item heights for the scrollbar; at a
hundred thousand rows that is about a megabyte, allocated once per range
change.

## Findings

- **A row click did not focus its list.** CR-005 made a click preview and
  keep "whoever holds focus" — right for the walk, wrong when the click
  came from another panel: the mail opened beside an inbox whose header
  never inverted. The list takes focus first now; the preview keeps it.
  `e2e/focus.txt` holds the case through a new `mouse` step that sends a
  real press-release pair into the stage, because `click` resolves the
  action directly and could not have shown this.
- **The touch swipe in the harness is not a way to address a row.** A
  `swipe` rides the list's real fling physics and lands where the fling
  says — a second swipe from the row the first one left at the bottom
  moved a fraction of its travel, and a short one moved backwards. And a
  swipe from the panel *title* never reaches the list at all: the finger
  leaves the header upwards, and a scroll event outside the list's rect is
  nobody's. The keyboard walk (`key down 68`) scroll-follows
  deterministically and is what `e2e/filter.txt` uses to reach the second
  page.
- **`--no-draw` fails every `shot`.** With rasterization off no frame is
  written, so a `shot` has nothing to pick and every suite exits non-zero
  on those steps alone. The signal in that mode is the non-shot failures
  (a label that did not resolve), which is what this CR's runs were read
  for. Pre-existing; noted so the next reader does not chase it.

## Not done, on purpose

- **Sorting.** The datasource declares one order (it is also the rank key);
  stelaxis's sort modes and column sorts were not asked for and have no
  chrome here yet.
- **The in-memory datasource.** Stelaxis has an `InMemory` twin of the Ecto
  helper. Nothing here needs it — every list reads the store — so the
  trait has the seam and no second implementation beyond the tests'.
- **A default filter as a removable chip** (stelaxis's `default_filter`).
  The inbox datasource is scoped to the inbox folder in its base `WHERE`;
  a `@folder:` tag over all mail with `@folder:inbox` seeded into the
  field is the same feature and a visible product change, left for when a
  second folder view wants it.
- **The typed filter is still ephemeral** (only a baked `Kind::Inbox`
  param persists), as before.
- **No eviction** of visited pages from the store's cache; the cache never
  had one, and a visited page is no larger than the rows the old inbox
  held permanently.
