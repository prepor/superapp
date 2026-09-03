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

## One writer, and the replication log

There is exactly **one writable connection**, private to a dedicated writer
thread. Every mutation in the process — from the UI, from a sync worker, from
the sender — is a closure submitted to that thread and awaited; the closure
runs on the writer, so it owns what it touches rather than borrowing UI
state. Every *other* connection is a `query_only` reader, so a write that
tries to skip the gate fails with `SQLITE_READONLY` loudly in a test rather
than racing silently in production. That single door is what device sync
needs (CR-005): a write nobody captured would be divergence no changeset can
reconstruct.

Inside the gate, a SQLite **session** over the durable tables records what
each transaction wrote as a changeset and queues it in `repl_log`, in the
same transaction. That log is a queue that drains and prunes, not a table
anything migrates through.

Those frames reach the other device through an **object store** — three
verbs, get and create-only and compare-and-swap — with a lease so only one
device writes at a time. A single `state` object fuses the lease with the
head pointer, so the compare-and-swap that advances the log *is* the check
that this device still holds the lease. One device bootstraps the lineage
(uploads a snapshot, writes the first `state`); the other installs that
snapshot to gain a common ancestry, then applies each published batch
forward.

The transport is chosen by the URL: a small local `bucketd` daemon for a demo
with no cloud account, or **Cloudflare R2** over its S3 API for real (TLS and
signed requests, and nothing else different — see
`docs/device-sync-demo.md`). The compare-and-swap is not emulated on top of
either: R2 implements S3's conditional writes, so `If-None-Match: *` is the
create-only put and `If-Match: <etag>` is the compare-and-swap. The
single-writer property rests on the object store, which is why it can be
model-checked rather than hoped for.

A device is given its bucket the way it is given a mailbox: from a panel with
three fields, not from a command line. The secret goes to the platform's
secret store (never the SQLite file, never a config file); the endpoint and
key id go to a `bucket` file beside the store; the worker restarts onto them.
That road exists because the device most in need of it — a phone — has no
shell to run a flag from.

The role drives the UI and the write gate together. A **holder** writes;
everyone else is read-only and sees a **locked screen** — who holds the
lease, and a button to take it. The holder hands the lease back on sleep and
on close, so the other device can acquire it without an override; taking it
from a live holder is an override, worded as the risk it is (that device may
hold work it never published). A device overridden while it thought it held
the lease is **stranded**: read-only, recovery by hand, never a silent merge.

Effects are values that describe themselves in one line and know how to do
themselves, behind one swappable backend — the real one, an in-memory fake,
or one that refuses everything. That last is what lets a panel be drawn in
isolation without it quietly sending mail.

## The disk

The filesystem is outside by that same line — nothing the store can
reproduce — so the [file browser](./interaction-grammar.md#files-a-directory-is-a-column-a-file-is-a-card)
reaches it through the same backend the network does: `list_dir`, `stat`,
`read_file` and `open_path` sit beside `now` and `clip`, and so do the four
that write — `make_dir`, `copy_path`, `move_path` and `trash`. A files
panel is the one kind that reads outside the store *while drawing*, so the
world rides its props; everything else in the app draws from rows alone.

The write verbs are performed inline, where the click is, the way `clip`
and `open` are — not filed as jobs. They are effects because the store
cannot reproduce them, not because anyone would retry them: a copy that
refuses has to say so in the same breath as the click, and an action whose
undo is recorded the moment it happens can never race a queue. The costs
are both open questions: the wait for a large tree, and a delete that
leaves no row in [the log panel](#the-log-panel). There is no
`remove` among them on purpose: `trash` answers *where it put it*, which is
what makes a delete an ordinary move to undo, and it is the only way this
app takes a path away — the reversal of a copy uses it too.

What that buys is what every backend buys: the real one reads and writes
the disk, hands a path to the OS (`/usr/bin/open` on macOS) and trashes
through `NSFileManager`'s own door — the right trash for the volume, the
Put Back the Finder offers — the fake serves a demo tree, which is why the
panels library draws the same directory on every machine, and a sealed
world says *this world has no outside* on the status line instead of
pretending. The demo tree takes the writes too, so an e2e run under
`--demo-disk` proves the verbs against a fixture rather than against
somebody's home. What it costs is the reactive layer: a listing is not a
query, so nothing invalidates it when the disk moves — a verb that wrote
tells the panels itself.

Paths cross that boundary in two spellings, and the mapping is one function
each way: the panels show and persist `~/Downloads/2026`, the outside reads
`$HOME/Downloads/2026`. A panel's params are the display form, so a session
restored on another machine points at that machine's home.

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
form. A **Gmail** account is the same engine with a different proof of
identity — see below. One **worker thread
per account** — its own connection to the same file — polls every minute
(and on *refresh*): mirror the special-use folders, fetch what is new
(each folder retains the newest **200** messages; below that window the
panels honestly know nothing), reconcile flags and deletions.

A folder's **role** comes from its RFC 6154 special-use attribute:
`inbox`, `archive` (`\Archive`, or `\All` — see Gmail below), `sent`,
`spam` (`\Junk`, spelled the way every client and every server that is not
the RFC spells it) and `trash`. A folder with no such attribute is not
mirrored at all. Four of those five roles have a panel — the mailboxes; the
fifth is where `delete` puts things.

Server effects run on a **desired/actual split**: a `message` row is the
user's *intent* (which folder, read or not, passed on or not); `server_msg` is what the
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
**What a letter carries** is derived, not stored twice. An `attachment` row
per part holds only the description a link and
[a card](./interaction-grammar.md#attachments-a-part-of-a-letter-is-a-card)
need — name, media type, size, the Content-ID an inline part wears — plus
the part's index into the parsed message; the bytes stay in the `raw` every
synced mail already keeps, and a card reads them back through that index on
a thread of its own. So the rows are versioned by the *walk* that made them
rather than by the schema counter (the argument the search index makes, and
for the same reason), and an `attachment_scan` row per mail says which walk
that was. A table rather than one `meta` key because the question is per
mail: a letter that arrives through **replication** ran no ingest code, so
its `raw` is one nobody has walked, and this is what notices. Neither table
replicates — every device derives its own from the `raw` it already has,
which is also what stops two devices fighting over one id sequence.

Which is exactly why a card panel is named `(mail, part)` and never by the
row's id: `panel` *does* replicate, and an id minted over here means
another letter over there. `(mail, part)` comes out of the same walk on both
devices, off a `raw` they both have. The rows also go when their letter
does — removing an account takes them, and the pass sweeps any whose message
is gone before it walks, because a `message` rowid is reused and a scan row
left behind would tell the walk that the next letter to take that id had
already been done.

The other direction is not derived at all: `draft_attachment` is a compose
panel's own list, keyed by panel like the draft beside it, holding each
file's **path**. The bytes are read at submit time, through the outside —
what leaves is the file as it stands, and a file that has moved, or grown
past the cap since, fails the send by name rather than going out truncated.
It replicates with the draft, so the other device shows the same letter; but
a path is only a file on the machine it was picked on, so the row records
**which install picked it** and a send from anywhere else refuses. And it is
held to the draft's seed like the text is: a compose retargeted in place
keeps its id, and the files a reply left are not the forward's.

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

One queue, but not one claimant. An account's IMAP session lives in that
account's sync worker and nowhere else, so a pass claims only what it can
perform: the worker takes the jobs filed against its own `account:N`, the
sender takes what needs no session. A job says what it needs by what it is
filed against, so it routes itself — and a claimant that could only have
failed it never takes it. Before that, a delete read *pending → "not
connected" → done*: the sender, waking on its own timer, took the move,
failed it on a session it never had, and left it sitting out a backoff the
worker beside it never asked for.

A job that fails backs off and retries; after six attempts it stops and
waits for a human. A job carries whether repeating it is **idempotent**,
because that is the one judgement a crash cannot guess: on the next launch
idempotent work returns to the queue, and everything else fails with
*"interrupted; outcome unknown"* rather than risking a second send. Payloads
and replies are JSON text and reference rows rather than embedding their
contents, so `sqlite3` shows every *queued* attempt, with its status, its
answer and its failures. A panel can ask for its own
(`WHERE entity = 'panel:7'`); a reply is read back through the same reactive
query layer as anything else, so watching a job is invalidation, not polling.

### The log panel

What `sqlite3` shows, the app shows too: the **effects** panel is the queue
read back, as [a rich table](./richtable.md) over the `effect` table —
because a queue that is only legible from a shell is legible to the wrong
person. Rows are newest first; each shows the verb, whose work it was, its
status (and the attempt count once a job has fought), and under that **the
sentence the effect describes itself with**. That line is not a string in
the panel: the registry decodes the payload back into the effect and calls
the same `describe` that names it everywhere else, so a new effect kind
arrives in the log the day it is registered and no central table of kinds
exists to forget it. A row a build cannot decode falls back to its payload
rather than disappearing.

It is the **queue** read back, though, not a record of everything that left
the process: an in-memory effect writes no row, so `clip`, `open` and the
file browser's four writing verbs never appear here. Nothing is retrying
them and nothing is waiting on them — but a delete is a large thing to
leave no trace, and closing that gap is an open question, not a decision
(see [Open Questions](./open-questions.md)).

The log is the **inbox's shape over another table**: it previews into a
**job** panel exactly as the inbox previews a message — the cursor walk
re-aims the pair and keeps the keyboard, `enter` goes. A job panel is the
whole row as a page: the sentence, the error if there is one, then the job's
own facts (filed, last touched, attempts, whether a crash may repeat it),
the payload it was filed as, and the answer the world gave back — all of it
selectable, because a payload is something one copies into a report. It
re-reads its row every draw, so a job that finishes while it is open
finishes on screen. A preview costs the world nothing here: looking at a
record establishes no fact, which is the one way this pair differs from the
inbox's.

The filter is the table's own grammar over the queue's columns: `@failed`,
`@live`, `@retried`, `@risky` (the work a crash cannot retry for you),
`@kind:`, `@entity:`, `@attempts>3`, `@date:`, and bare words over the
payload — which is where a uid or an address actually lives. Nothing in the
panel writes: the queue is the executor's to move, and a page of it is a
cached, reactive query like any other, so a job running redraws the rows on
screen and nothing else.

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
a `submit` job; the executor submits over SMTP (rustls, port 465; a reply
names its parent in `In-Reply-To` and both a reply and a forward carry the
source's chain in `References`, so the Sent copy folds into the
conversation when it syncs back), appends the sent bytes to the account's
Sent folder over IMAP, and records the outcome — for a forward, that the
mail it passed on is now *forwarded*, an intent the next push pass sets on
the server as the `$Forwarded` keyword, the one other clients draw their
arrow from — where the folder's `PERMANENTFLAGS` say it keeps keywords;
elsewhere the mark stays local, neither pushed nor read back. Because both the outbox row
and the job are durable, a mail that hit *send* and never left goes out late
rather than never. A delivered send is physics: its claim refuses, the
history node goes **expired**, and the walk skips it transparently.
A *failed* send stays cancellable — the error toasts, `cmd+z` reopens the
draft, and it stands in the problems panel with *retry* and *reopen* until
it goes out or is taken back. The launcher's *new mail* root opens a blank compose.

## Signing in to Gmail

Google stopped accepting passwords on IMAP, so a Gmail account proves
itself with a **bearer token** instead: SASL `XOAUTH2`, the same envelope
on both IMAP and SMTP. Everything above this line is unchanged — the same
worker, the same passes, the same desired/actual split. The account row
carries one extra word (`account.auth`), and the two sites that open a
session ask one function which mechanism that word means.

Getting a token is the **installed application** flow (RFC 8252), and it
never goes through this app's own UI: pressing *sign in with google* on the
add-account panel binds a loopback listener on `127.0.0.1`, opens the
system browser on Google's consent page with a PKCE challenge, and waits
for the redirect to come back to that port. No embedded webview, and no
Google password ever typed into superapp.

Three secrets, three lifetimes, and that is the whole design:

| | lives | kept where |
|---|---|---|
| authorization code | seconds | never leaves `src/oauth.rs` |
| refresh token | until revoked | the keychain, under `oauth:<address>` |
| access token | an hour | process memory, refreshed on demand |

The refresh token is what the account *is*; the access token is minted from
it by the backend that owns the process and is deliberately never written
down. A grant the human revokes at Google fails at the next refresh, and it
fails **honestly** — the sync stops and says `invalid_grant` rather than
falling back to a password that was never there.

Two Gmail behaviours the engine has to know about, both of them the
provider's rather than the protocol's.

Gmail advertises no `\Archive` mailbox: archiving there *is* dropping the
inbox label, leaving the message in All Mail. So the special-use `\All`
takes the archive role and a MOVE into it is the archive — but that folder
is a **move target only, never an ingest source**. All Mail holds every
message the account has, inbox included, under uids of its own, and this
store gives a message one folder; reading from it would file a second row
for every mail already mirrored from INBOX. The cost is stated rather than
hidden: mail archived on *another* device does not appear locally. What
this device archives stays, because the push records the move rather than
re-reading it. Gmail's label model is the real answer here, and it is not
this schema's — `X-GM-MSGID` as a cross-folder identity is where that would
start.

And Gmail's SMTP files its own copy into Sent Mail, unlike a plain relay, so
the APPEND every other account gets is skipped: one letter in Sent, not two.

A grant is checked before it becomes an account: **asking for a scope is not
getting it.** A consent screen that does not carry
`https://mail.google.com/` yields a grant with `openid email` and nothing
else — no error, no warning — and the account would then fail at its first
IMAP login with a bare "AUTHENTICATION FAILED", an hour of confusion from
its cause. The token response says what was granted, so the sign-in refuses
there instead, while the human is still standing at the door they must go
back through. A refusal that does arrive over IMAP is read too: Google's
XOAUTH2 no is a JSON challenge whose status separates a missing scope from a
mailbox with IMAP switched off, and those want opposite fixes.

One thing this cannot ship: the OAuth **client registration**. Google issues
those per developer, so superapp reads yours — `SUPERAPP_GOOGLE_CLIENT_ID`
and `SUPERAPP_GOOGLE_CLIENT_SECRET`, or the console's downloaded JSON
dropped verbatim at `google-oauth.json` beside the store. It must be a
**Desktop app** client: the redirect is a loopback port the OS picks per
sign-in, and a Web client only accepts redirect URIs registered in advance,
port and all — so one is refused by name here rather than as a
`redirect_uri_mismatch` in the browser three steps later. Without any
registration, the panel says so instead of pretending.

The consent round trip is the one thing e2e cannot script — it wants Google
and a human — so a run refuses it and puts that refusal on the panel's own
status line, which is what `e2e/oauth.txt` asserts. Everything up to that
door is unit-tested: PKCE against RFC 7636's worked vector, the consent
URL's parameters, the `id_token` read that names the address, the XOAUTH2
envelope byte for byte, and a bearer-token sync and send against the fake
world.

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
  agents someday. An app password and an OAuth refresh token live side by
  side there under different keys.

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
