# The Data Substrate

One SQLite file holds **all** durable data — the mail *and* the UI: which
panels are open, where, joined to what, focused on which workspace. `sqlite3
"~/Library/Application Support/superapp/superapp.db"` shows your session as
rows; that inspectability is a feature, not a debugging aid. The design is
CR-001 (`docs/planning/`), rel.systems' idioms brought in-process: single
writer, WAL, hook-driven change capture, and side effects as rows (phases 2+).

## Two write paths

1. **Actions** — user intent. The only path UI code mutates through
   (phase 2 wraps them in the undo log).
2. **Ingest** — sync results, once IMAP lands (phase 3): not undoable, same
   store, same invalidation.

Both go through one `write(tx)` seam: one mutation, one transaction.

## The reactive layer

Panels read **only** through registered queries (`store.rows(Q, params)`).
Each result is cached per `(query, params)` and stamped with the
**generation** of every table it read; a commit bumps its touched tables'
generations (SQLite's `update_hook` reports them), and stale results re-run
lazily on next draw. Dependencies are not declared — they are **captured by
SQLite's authorizer** at prepare time, so the dependency set is complete by
construction.

That same trace is the future **panel context**: a panel's provenance — which
queries, which parameters, over which tables, as of which commit — exists as
a side effect of how drawing works, ready to be serialized for an agent
(phase 7). This is what "data centric" means mechanically.

At personal-mail scale, re-run-on-invalidate is microseconds; rel.systems'
incremental Z-set engine remains the documented upgrade path if a view ever
outgrows it (its SPJ class deliberately excludes the `ORDER BY`/aggregate
queries panels actually use — in the daemon model that compute is the
client's job, and superapp *is* the client).

## The mail engine

Real accounts are **fastmail-style**: IMAP over rustls (port 993) with an
app password; the *settings* panel (a launcher root) lists accounts with
their live sync status and holds the add-account form. One **worker thread
per account** — its own connection to the same file — polls every minute
(and on *refresh*): mirror the special-use folders, fetch what is new
(each folder retains the newest **200** messages; below that window the
panels honestly know nothing), reconcile flags and deletions.

Server effects run on a **desired/actual split**: a `message` row is the
user's *intent* (which folder, read or not); `server_msg` is what the
server actually holds, written only by the workers. A row whose two sides
disagree **is** the push queue — each pass starts by making the server
agree (`UID MOVE`, `\Seen`), then fetches and reconciles facts.
Reconciliation never fights the user: divergent intent is pushed over the
server, never clobbered by it (deletion is the one place the server wins).
And because `server_msg` lives outside the undo world, undoing an
already-pushed archive needs no compensation machinery at all — intent
flips back, the next pass moves the mail back. A moved mail whose new uid
the server never reported (no COPYUID) is re-identified by Message-ID
instead of duplicated; per-message push failures land on that message's
status line.

A worker's commit reaches the UI as a signal; the store notices foreign
commits by `data_version` and re-runs stale queries on the next draw.
Account add/remove are undoable actions like everything else.

## What stays out of the file

- **Ephemeral physics**: spring positions, in-flight gestures, the caret
  blink, where the camera is mid-slide. The line: *if losing it in a crash
  would annoy you, it belongs in the store.* Layout, focus, filters — in.
  Motion — out, re-derived at boot.
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
