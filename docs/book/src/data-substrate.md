# The Data Substrate

One SQLite file holds **all** durable data — the mail *and* the UI: which
panels are open, where, joined to what, focused on which workspace. `sqlite3
"~/Library/Application Support/superapp/superapp.db"` shows your session as
rows; that inspectability is a feature, not a debugging aid. The design is
rel.systems' idioms brought in-process: single writer, WAL, hook-driven
change capture, and side effects as rows.

## Three write paths

1. **Actions** — user intent. The only path UI code mutates through.
2. **Ingest** — sync results: not undoable, same store, same invalidation.
3. **Effects** — what reaches the outside world, as rows in one queue
   (below).

All go through one `write(tx)` seam: one mutation, one transaction.

## The line: what is an effect

> **An effect is anything whose result the store cannot reproduce.**

The store is the app's memory — replaying its transactions replays the app —
so SQLite is emphatically *not* an effect. A socket, the keychain, the
clipboard, a file beside the store, the screen and the clock are. The
corollary is the one that shapes everything below: **archiving a mail is not
an effect; pushing the archive is.**

Effects are values that describe themselves in one line and know how to do
themselves, behind one swappable backend — the real one, an in-memory fake,
or one that refuses everything. That last is what lets a panel be drawn in
isolation without it quietly sending mail.

## The reactive layer

Panels read **only** through registered queries (`store.rows(Q, params)`).
Each result is cached per `(query, params)` and stamped with the
**generation** of every table it read; a commit bumps its touched tables'
generations (SQLite's `update_hook` reports them), and stale results re-run
lazily on next draw. Dependencies are not declared — they are **captured by
SQLite's authorizer** at prepare time, so the dependency set is complete by
construction.

That same trace **is the panel context**: every panel's draw runs inside a
trace, so its provenance — which queries, which parameters, how many rows —
exists as a side effect of how drawing works, never by declaration.
**`cmd+i`** serializes the focused panel's context (identity, params, the
traced queries with their SQL) to the clipboard and to `panel-context.md`
beside the store — ready to hand to an agent; the agent hookup itself is
future work. The trace is honest to a fault: it records what a draw
actually read, not what the panel is nominally about. This is what "data
centric" means mechanically.

At personal-mail scale, re-run-on-invalidate is microseconds; rel.systems'
incremental Z-set engine remains the documented upgrade path if a view ever
outgrows it (its SPJ class deliberately excludes the `ORDER BY`/aggregate
queries panels actually use — in the daemon model that compute is the
client's job, and superapp *is* the client).

## The mail engine

Real accounts are **fastmail-style**: IMAP over rustls (port 993) with an
app password; the *settings* panel (a launcher root) lists accounts with
their live sync status and links to the *add account* panel, which holds the
form. One **worker thread
per account** — its own connection to the same file — polls every minute
(and on *refresh*): mirror the special-use folders, fetch what is new
(each folder retains the newest **200** messages; below that window the
panels honestly know nothing), reconcile flags and deletions.

Server effects run on a **desired/actual split**: a `message` row is the
user's *intent* (which folder, read or not); `server_msg` is what the
server actually holds, written only by the workers. A row whose two sides
disagree **is** the push queue — each pass turns every disagreement into a
job, then fetches and reconciles facts.
Reconciliation never fights the user: divergent intent is pushed over the
server, never clobbered by it (deletion is the one place the server wins).

**Threads** are decided at ingest, in the transaction that stores the mail.
`message.thread` is the id of the conversation's lowest member — an anchor,
not a root; no row is the parent of another, and what a thread *has* is a
`GROUP BY` at read time. A `reference` table keeps one row per id in a
mail's `References` and `In-Reply-To`, and threading is the union of three
lookups over the account: mails my references name, mails whose
references name me (the parent arrived late — Sent syncs after Inbox, or it
is below the window), and mails whose references share an id with mine
(two GitHub comments under an issue mail that never arrived). Whatever they
find merges into one anchor; nothing found, and the mail anchors itself. No
subject heuristic. A reply of yours carries the parent's whole `References`
chain, so it threads for the other side too. A mail present twice in an
account — your reply, in Sent and back through a list — is one message to
the panel.
And because `server_msg` lives outside the undo world, undoing an
already-pushed archive needs no compensation machinery at all — intent
flips back, the next pass moves the mail back. A moved mail whose new uid
the server never reported (no COPYUID) is re-identified by Message-ID
instead of duplicated; per-message push failures land on that message's
status line.

The push pass itself **never talks to the server**. It materializes each
disagreement as a row in the `effect` table, and one executor performs it:
claim (`WHERE status='pending'`, so undo and the executor have exactly one
winner), revalidate, perform outside any transaction, then record the
outcome *in the same transaction as the fact it establishes*. Every job
re-checks the diff before its round trip, so intent reverted while the job
waited goes `obsolete` rather than pushing stale work — and intent reverted
*before* the pass ran was never queued at all. Undoing an archive costs the
server nothing and works offline.

A job that fails backs off and retries; after six attempts it stops and
waits for a human. A job carries whether repeating it is **idempotent**,
because that is the one judgement a crash cannot guess: on the next launch
idempotent work returns to the queue, and everything else fails with
*"interrupted; outcome unknown"* rather than risking a second send. Payloads
and replies are JSON text and reference rows rather than embedding their
contents, so `sqlite3` shows every attempt the app has made on the world,
with its status, its answer and its failures. A panel can ask for its own
(`WHERE entity = 'panel:7'`); a reply is read back through the same reactive
query layer as anything else, so watching a job is invalidation, not polling.

A worker's commit reaches the UI as a signal; the store notices foreign
commits by `data_version` and re-runs stale queries on the next draw.
Account add/remove are undoable actions like everything else.

What "perform outside any transaction" is worth, measured: a UI action costs
0.10 ms uncontended and **468 ms** behind a 400 ms fetch that holds
`BEGIN IMMEDIATE` across the wire. SQLite has one writer and the UI shares
the file, so a pass that keeps a transaction open over a round-trip stalls
everything behind it for as long as the server takes — and it reads as the
app hanging, not as sync being slow. The rule above is what keeps a reading
walk down the inbox, which writes on every keystroke, from queueing behind
the network.

**Sending** is the outbox pattern with the undo window built in. A compose
panel's draft persists in the store *as you type* (plain upkeep — typing is
the future editor's local undo, not the system's), keyed by the panel id,
so half-written mail survives restarts. *Send* is an action: it files an
outbox row with `send_after = now + 10 s` and closes the panel; `cmd+z`
inside the window cancels it — the row's deletion *is* the undo, and the
claim (`WHERE status='pending'`) means the race between undo and the
sender has exactly one winner. The sender wakes at the deadline and queues
a `submit` job; the executor submits over SMTP (rustls, port 465; reply
headers thread via `In-Reply-To`), appends the sent bytes to the account's
Sent folder over IMAP, and records the outcome. Because both the outbox row
and the job are durable, a mail that hit *send* and never left goes out late
rather than never. A delivered send is physics: its claim refuses, the
history node goes **expired**, and the walk skips it transparently.
A *failed* send stays cancellable — the error toasts and `cmd+z` reopens
the draft. The launcher's *new mail* root opens a blank compose.

## What stays out of the file

- **Ephemeral physics**: spring positions, in-flight gestures, the caret
  blink, where the camera is mid-slide. The line: *if losing it in a crash
  would annoy you, it belongs in the store.* Layout, focus, filters — in.
  Motion — out, re-derived at boot.
- **History.** The undo tree is in memory, so it dies with the process. A
  node is a layout snapshot plus its claims on the world; the claims are in
  memory too, and never serialized. What survives is the row each one wrote
  — which is all the background passes ever read, and why a restart loses
  undo but never loses work. See [Interaction
  Grammar](./interaction-grammar.md).
- **Secrets**: the macOS keychain (android: an app-private file until a
  Keystore binding exists), never this file — it is meant to be handed to
  agents someday.

## Session persistence

Every mutation funnels through the shell's `sync()`, which snapshots the
logical workspace state and writes it — wholesale, the UI tables are tiny —
only when the snapshot actually changed. Boot restores it: quit with a mail
open on workspace 4 and relaunch, and you are on workspace 4 with that mail
open (and it stays read — flags live in the same file). A store that never
booted seeds the demo mail and the default layout; `--db` points anywhere
else, and e2e runs get a fresh seeded temp file so suites stay deterministic.

Panel params are typed columns (`p_int`, `p_txt`), not serialized blobs: a
`message` panel row *joins* against the `message` table. The schema is the
API.
