# CR-007 · Mail threads: the conversation as the row, and as the page

Status: **implemented** (Andrey, 2026-09-02: full support of mail threads;
the message panel's `newer`/`older` links go — the inbox cursor already
walks; expansion by pointer only for now, "see whether it hurts"; a closed
row shows an error status in red, "it should be rare anyway").

## Why

A reply arrives and the inbox shows it as a stranger: a new row, bold, with
a `Re:` in front of a subject you have already filed twice. Reading a
conversation means opening its pieces one by one, in whatever order the list
has them; filing it means filing each piece. The store has kept `Message-ID`
since CR-001 phase 4 and the raw RFC 822 bytes since phase 3, so everything
a thread is made of is already on disk. Nothing reads it.

## The model

Two rules, one per surface:

> **The inbox row is a thread. The message panel is the thread its mail
> belongs to.**

### Identity: `message.thread`

A thread is a set of messages in one account, and its identity is a message
id: `message.thread` is the id of one of its members — the lowest, an
*anchor* rather than a root; no row is the parent of another. There is no
thread table. What a thread has — participants, last date, unread count — is
a `GROUP BY` at read time, which at personal-mail scale is microseconds and
invalidates honestly like every other query.

Membership comes from the standard headers, in one transaction at ingest
(`mail::thread_tx`), as the union of three lookups over the account:

1. **my references name them** — messages whose `Message-ID` is in my
   `References` ∪ `In-Reply-To`;
2. **they name me** — messages whose references contain my `Message-ID`.
   The parent arrived late: Sent syncs after Inbox, the 200-per-folder
   window, a moved mail;
3. **we name the same missing mail** — messages whose references share an
   id with mine. This is JWZ's *empty container* without the container: two
   GitHub comments that both point at an issue mail we never received, or
   one below the window, are still one thread.

Every thread the lookups find merges into the lowest anchor (`UPDATE message
SET thread = ?a WHERE account = ? AND thread IN (…)`); none found, and the
mail anchors itself. A merge is a fact about mail, never a UI event: a panel
showing a message re-derives its thread on every draw, and the inbox's
rank-kept cursor treats a thread whose anchor changed exactly as it treats a
row that left.

**No subject heuristic in v1.** Lookup 3 covers the case the heuristic
usually exists for; what is left is clients that send no references at all,
and folding by `Re:`-subject would also fold every Hetzner *invoice* into
one thread. If real mail hurts, the rule to add is narrow — reply-prefixed
subject, same normalized topic, same account — and it is a fourth lookup,
not a different model.

**Schema v9**: `message.thread` (NOT NULL once back-filled), `message.topic`
(the subject with its `Re:`/`Fwd:` prefixes stripped — mail-parser's
`thread_name`; rows and titles show this), and a `reference(message, mid)`
table indexed by `mid`, one row per id in `References` ∪ `In-Reply-To`. The
migration back-fills from `raw`, oldest id first, exactly as v6 back-filled
`html`; seeded mail declares its ids and references in the seed. Own replies
already carry `In-Reply-To` (CR-001 phase 5); they should also carry the
parent's `References` plus its `Message-ID`, as RFC 5322 asks, so a reply to
a reply threads for the other side too — one line in `load_outgoing`. The
Sent folder has role `sent` and is mirrored already, so what I wrote joins
the thread when the next pass lands it. A mail present twice in one account
(my reply: in Sent, and back through a list into Inbox) is one message to
the panel — dedupe by `Message-ID`, the non-Sent copy wins.

### The inbox: one row per thread

A row is a thread with at least one message in the inbox, ordered by its
latest inbox message, bold while any of them is unread. The same three
columns as today:

```
Max, me · 3        superapp panel model                     30.08
GitHub · 6         [stelaxis] CI failed on main             31.08
Vera Kovac         Q3 infra budget draft                    31.08
```

- **who** — the distinct senders, newest speaker first, `me` for the
  account's own address, first names once there are two of them; then
  `· n` for the whole conversation's count past one (trash left out, so the
  number matches the rows the panel shows). A one-message thread is a row
  exactly as now.
- **what** — the topic, off the oldest message, so it never starts with
  `Re:`.
- **when** — the latest inbox message, which is also the order.

The rich table gains one thing: a datasource may declare a **group key**.
With one, the builder wraps the spec — `SELECT <aggregates> … GROUP BY
<key>` becomes a subquery the page, the count and the rank all query over —
and the filter moves *inside* a membership test:

```sql
m.thread IN (SELECT m.thread FROM … WHERE <base> AND <filter>)
```

That fixes the semantics in one place: **a thread matches when any of its
inbox messages matches, and its aggregates are always over the whole
thread.** `@from:vera` finds the threads Vera wrote in and still shows all
their participants; `@unread` finds threads with unread mail; `@date>` reads
against the messages, not the row. Tags, autocomplete, paging and the cursor
are untouched; the row type becomes a `ThreadHead` and the rank key
`(last, thread)`.

The row's **target** — the id `Message { id }` opens with — is decided in
the query too: the oldest unread inbox message, else the newest. A walk onto
a thread with three new replies lands at the first new one.

**Triage files the thread.** Archive, delete and the row swipe act on every
message of the thread still in the inbox — one action, one `Filed` intent
per message, one `cmd+z`. A reply that arrives after the filing puts the
thread back in the inbox by itself, because membership is a query; filing
it again touches only the new message. `Wm::showing` closes readers of any
of the thread's messages.

### The panel: `Message { id }` is the whole conversation

No new kind. A panel's parameter stays a **message** id — stable across
merges, which a thread anchor is not — and the panel draws the thread that
message belongs to, scrolled to it. Title: the topic. Oldest first, so a
conversation reads down the page like the letters it is:

```
┌ superapp panel model ─────────────────── DELETE ARCHIVE × ┐
│ TO     me@prepor.dev                                       │
│ ────────────────────────────────────────────────────────── │
│ Andrey Rudenko  Wrote up the panel model: jo…  aug 29 14:02 │  closed
│ ────────────────────────────────────────────────────────── │
│ Max Ivanov <max@ivanov.dev>                    aug 30 22:47 │  open
│ Read your note on panels. The joined/replace rule feels    │
│ like the right default — …                                 │
│ ────────────────────────────────────────────────────────── │
│ Max Ivanov <max@ivanov.dev>                    aug 31 07:30 │  open
│ One more thought after sleeping on it: …                   │
│ › quoted                                                   │  folded
│ ────────────────────────────────────────────────────────── │
│                                                     reply  │
└────────────────────────────────────────────────────────────┘
```

- **TO once**, at the top: a thread is delivered to one account, and per
  message it was noise.
- **One row, two states.** A message is always one header row. Closed, it
  shows the sender's name, the first line the author wrote in muted grey,
  ellipsized, and the date at the right edge — or the status line instead
  of the preview, in red when it is an error, since that is the one thing
  a closed row should not hide. Open, the same row shows the sender as the
  solid contact link and the date in the same place, and the letter
  unfolds under it: the status line, the HTML or the text reading. Same
  height, same columns, so a touch anywhere on the row toggles it, except
  on the link, which stays a link. Below an open letter sits a hairline,
  like an inbox row's. This replaced the single message's FROM/TO/DATE
  block too, and the date stopped being selectable text — copying a date
  was the only thing that bought, and it would have muddled the row's
  target.
- The trailing quote (`On … wrote:` and the `>` block; `<blockquote>` in
  HTML) folds behind a one-line `› quoted` — in a thread, the quoted text
  is the message above it — and unfolds in place when touched. A letter
  that is nothing but quote stays whole.
- A closed row is a *row* in the inbox-row sense: touching it opens the
  message in place. That is panel context like the inbox cursor: not a
  link (nothing opens, nothing is replaced, no chain cascades), not an
  action, not history. The sender's name in a closed row is deliberately
  not a link, so the whole row is one target; the contact link appears
  once the message is open.
- **What starts open**: the target message, and every message that was
  unread when the panel opened — so what is open is what is new.
  **Opening a thread marks it read** — every unread message of it, inside
  the action that opened it, one `MarkRead` intent each — so the row stops
  being bold as a whole and one `cmd+z` puts every flag back, and none
  other. The shell seeds the set, because by the time the widget draws the
  marks have landed; redo re-seeds the same way. After a restart a panel
  comes back with only its own mail open: "what was new" died with the
  process, as undo does.
- **`reply`** stays one link at the bottom, `cmd+r`, and replies to the
  newest message — the conventional reply-to-conversation. Per-message
  reply links are a mouse affordance for later; rule 4 gives a chord only to
  a control a panel has one of.
- **Height**: the wish is the open readings plus a row's worth per closed
  one, so opening an old message grows the panel with the spring, the way
  a long letter opens tall, and closing it shrinks it back. The shell
  clamps as before.
- **The walk**: the arrows scroll the panel, as today. `newer`/`older` are
  gone — the inbox cursor is the walk, and it walks threads. Per-message
  expansion has no chord, by rule 4: chords go only to controls a panel
  has one of. Pointer-only for now; an `expand all` link with a chord is
  the one control that would fit the rule if the keyboard reach hurts.

The launcher's mail hits stay per message: a hit opens its thread with that
message expanded (one hit per thread is a follow-up). The contact card's
*messages from X* is an inbox filtered to X, which now reads: the threads X
wrote in.

### What was built

`message.thread`, `message.topic` and the `reference` table arrive with
schema v9, back-filled from `raw` in the migration; `mail::thread_tx` runs
the three lookups and the merge inside the ingest transaction and the
seed alike. The rich table's `SqlSpec` gained `group`: the page, the count
and the rank read off a grouped subquery, and the filter becomes the
membership test above (`richtable.rs`, one test per shape). The inbox's
datasource is `mail::THREADS` over a `ThreadHead` row — participants and
count by subquery over the whole conversation, target and last date over
the inbox messages — and the inbox cursor's identity is the thread anchor,
because which mail a row opens changes as replies arrive.

The message panel is a `PortalList` of `ThreadMsg` rows under a TO line,
with `reply` at the foot. Which rows are open lives on the shell as panel
context (`State::expand`, one `Expansion` per panel, handed in with the
props); touching a row registers through the shell's hit table like an
inbox row, and toggling re-measures the wish. Opening, previewing and the
launcher all mark the thread's unread mails; triage files every inbox mail
of the thread, one `Filed` intent each, and closes every reader of any of
them. Outgoing replies carry the parent's `References` chain. The demo
seed grew my note to Max in Sent, his second reply with a quoted tail, and
five archived CI runs under a parent that never arrived — the third
lookup's case — one of them red.

## Considered and not chosen

- **A `Thread { anchor }` kind beside `Message`.** Two panels for one
  reading surface; every link (launcher, contact, reply) would have to
  choose; and an anchor can change under a persisted param when threads
  merge. A message id never does.
- **Message rows with a thread page.** The row is where unread lives: three
  replies would be three bold rows for one conversation, and filing would
  still be per row.
- **A tree.** The References graph *groups*; a flat chronological page is
  what a conversation looks like on paper, and the monochrome grammar has
  nowhere to put indentation that would not read as a quote.
- **Subject folding in v1** — above.

## Costs, named honestly

- **Previewing a thread marks all of it read**, on the server too, from a
  navigation key. CR-005 accepted this per mail; a thread multiplies it. One
  `cmd+z` per walk still takes it all back.
- **A swipe files N messages.** The curtain says `archive`; it does not say
  how many. The toast should (*archived 3*).
- **The window.** Each folder keeps its newest 200, so an old thread's early
  messages may not be local. Lookup 3 keeps the thread whole; the page
  shows what is here. A collapsed line reading *2 earlier messages not
  fetched* is possible — an unresolved reference is exactly that fact — and
  is a refinement.
- **Grouped queries cost more than flat ones.** A page is a `GROUP BY` over
  the inbox folders plus a membership subquery under a filter. At the
  store's scale, indexed by `(folder, date)` and `thread`, it is well inside
  a frame; the builder's tests should carry a grouped case beside the flat
  ones, and the measured cost belongs here when it lands.
- **The seed changed.** Threads need members, so the demo world grew one
  mail in the inbox, one in Sent and five in the archive. Ids 1..=9 stayed
  what they were, and the inbox's rows kept their order; the counts in the
  mail tests moved.
- **Rendering a thread costs more frames than a letter did.** Under the
  headless rasterizer a frame with an open HTML message and a handful of
  rows takes noticeably longer than the plain panel's; on the GPU it is
  invisible. Worth measuring if a real inbox's threads run long.

## Follow-ups

- **An `expand all` control**, with a chord, if pointer-only expansion
  hurts.
- **One launcher hit per thread**; today every mail of a conversation is
  its own hit, each opening the same panel.
- **A closed line for what is not here**: an unresolved reference is a
  message below the window or never received, and the panel could say
  *2 earlier messages not fetched*.
- **The toast should count** what a swipe filed; the header buttons' toast
  does (`archived “…” (2 mails)`), the curtain still only says the verb.
