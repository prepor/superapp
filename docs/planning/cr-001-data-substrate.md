# CR-001 · Make it real: the data substrate and a true mail client

Status: **accepted** 2026-09-01. Decisions: undo model as amended (compensation
semantics, DAG data + linear undo first, overlay later); first providers are
**fastmail-style IMAP + app passwords** (OAuth deferred); account setup gets a
**real settings panel** from the start; rel.systems relationship as proposed
(idioms not crates, substrate-shaped schema).

## Intent

Replace the demo mail with a full email client — IMAP, SMTP, multiple
accounts, archive, delete, drafts, delayed send — and, underneath it, the data
substrate the whole superapp will stand on:

- one SQLite database for **all** data — mail *and* UI state (open panels,
  focus, workspaces);
- UI derived from that database through a small **reactive query layer**;
- **every action undoable** where physics permits, recorded as a history
  DAG (undo-tree-shaped), with a uniform system-level API;
- every panel able to hand over its **data provenance** — the queries that
  rendered it — as one-click agent context later. Data-centric.

## Relationship to rel.systems

rel.systems is a working v0 of this exact philosophy: one WAL SQLite owned by
a single writer, preupdate-hook CDC, an incremental Z-set SPJ engine, Electric
sync, and the transactional-outbox provider pattern (its canonical example is
literally an email provider reacting to `outbox_emails`). Verdict for
superapp:

**Adopt the idioms, not the crates; keep the shapes daemon-compatible.**

- *Not the engine*: the incremental SPJ engine rejects `ORDER BY`, `LIMIT`,
  `GROUP BY`/aggregates at registration (`reactive/src/plan.rs:251,585`) — by
  design, because in the daemon model that compute belongs client-side. But
  superapp **is** the client; its bread-and-butter queries (inbox newest-first,
  sender counts, filters) are exactly the rejected class. In-process, at
  personal-mail scale, re-running a query on invalidation is microseconds;
  incremental maintenance buys nothing yet. The engine remains the documented
  upgrade path if some view ever outgrows re-run.
- *Not the daemon*: a separate process is wrong on android and adds a hop on
  macOS. Everything runs in-process.
- *Yes the idioms*: single writer + WAL + read connections, hook-driven
  invalidation, a durable txid watermark, side effects as **outbox/op rows**
  claimed with status guards, at-least-once + idempotent executors. The
  in-process workers (IMAP syncer, SMTP sender) are *providers in the small* —
  same contract, linked in. If the substrate vision matures, superapp's schema
  is already the shape a `rel` daemon serves; moving a worker out to a real
  provider process is mechanical.

## Architecture

### One store

- `rusqlite` with `bundled` SQLite (per rel.systems research/03: the only
  crate with the full hook surface; bundling guarantees preupdate/session
  support and one known version on both OSes). WAL, `synchronous=NORMAL`.
- **One writer** on a dedicated thread — an actor taking closures over a
  channel, replying synchronously. UI-thread writes are sub-ms and block;
  ingest batches don't jank the UI. The UI thread and each worker hold their
  own **read-only** connections (WAL: readers never block).
- After every commit the writer posts `{txid, dirty tables}` to the UI thread
  (makepad `SignalToUI`). Table-level dirtiness comes from `update_hook` — no
  row diffing needed for invalidation.
- The file lives at `~/Library/Application Support/superapp/superapp.db`
  (macOS) / the app files dir (android). `--db PATH` overrides; **e2e runs on
  a temp file seeded with the demo mail**, so suites stay deterministic.
  `src/data.rs` retires into a seed script. Everything becomes inspectable
  with plain `sqlite3` — that inspectability *is* part of the data-centric
  story.

### Two write paths

1. **Actions** — user intent. The only path UI code may mutate through.
   Logged, undoable (below).
2. **Ingest** — sync results: new mail fetched, flags changed remotely, op
   outcomes. Not undoable, not logged as actions; same store, same
   invalidation.

Both serialize through the one writer, so history stays coherent.

### The reactive layer

A registry of named queries:

```rust
Query { id, sql, describe }                 // static registration
store.query(Q_INBOX, params) -> &Rows       // cached, typed decode at call site
```

- Results cached per `(query, params)`, stamped with the generation of each
  table they read. **Dependencies are captured automatically** via SQLite's
  authorizer during prepare — no hand-maintained dep lists.
- A commit bumps its dirty tables' generations; stale entries re-run lazily on
  next access; a dirty signal requests a redraw. That is the whole framework —
  re-run-on-invalidate, a few hundred lines, no async.
- Panels read data **only** through this layer. The draw pass records which
  queries each panel touched — the trace that becomes panel context (below).

### What stays out of the database

- **Ephemeral physics**: springs, in-flight gestures, held drags, caret blink.
  The line: *if losing it in a crash would annoy you, it belongs in the DB.*
  Logical layout, focus, filters, cursors — DB. Where the camera is mid-spring
  — memory.
- **Secrets**: never in the SQLite file (it's meant to be handed to agents and
  synced someday). macOS keychain; android v1: an app-private file (Keystore
  later), noted honestly.
- Attachments/raw mail start life as blobs *in* the DB ("all data" taken
  seriously); a files/ escape hatch only if size ever hurts.

`Wm` remains the in-memory working copy (springs read it every frame) —
**write-through**: an action mutates `Wm` via the existing pure core, and the
same transaction rewrites the (tiny) UI tables. Boot restores `Wm` from them:
restart brings your whole session back. Core stays pure and unit-tested.

## Actions and the undo DAG

### The model

```sql
CREATE TABLE action(
  id      INTEGER PRIMARY KEY,
  parent  INTEGER REFERENCES action(id),  -- the DAG: HEAD's child at apply time
  ts      REAL NOT NULL,
  kind    TEXT NOT NULL,                  -- 'open', 'move', 'archive', 'send'…
  label   TEXT NOT NULL,                  -- 'archive "Q3 infra budget draft"'
  entity  TEXT,                           -- 'panel:7' | 'mail:123' — skip/coalesce scope
  changeset BLOB,                         -- session changeset of the action's tx
  state   TEXT NOT NULL DEFAULT 'applied' -- applied | undone
);
-- meta: head = current position in the DAG
```

Undo (`cmd+z`) inverts HEAD and moves to parent. Redo (`cmd+shift+z`)
re-applies the **latest child**. A new action while HEAD is mid-tree creates a
sibling — a branch; nothing is ever lost. All of this is in the same commit as
the state change, so history is crash-consistent by construction.

### Automatic inverses — the session extension

Every action runs inside a SQLite **session** scoped to its transaction; the
recorded changeset (before+after images, invertible with
`changeset_invert`) is stored on the action row. Undo = apply the inverted
changeset. This makes *navigational* actions — open, close, move, join,
workspace moves — undoable with **zero per-action code**, and covers local
data mutations (read flags, folder moves) identically. Conflicts (ingest
touched the same row since) resolve by skipping that row and surfacing it —
rare, honest. Fallback if the session extension fights the android build:
hand-rolled before-images of the touched tables, same shape.

### Side effects — compensation, not time travel

Server-visible effects ride the changeset too, because *the effect is a row*:
archive inserts an `op` row, send inserts an `outbox` row. Undo's inverted
changeset **deletes the pending row — cancelling the effect atomically**
(the executor claims rows with `WHERE status='pending'` guards, so the race
between undo and execution has exactly one winner; nobody double-sends).

Once an op has executed, inverting the local changeset is no longer enough;
the action's *kind* declares a *compensating op* (archive → move back;
delete → restore from trash). A sent mail past its window declares none —
that node becomes **irreversible**.

Delayed send is just the outbox pattern: `send_after = now + 10s`; the sender
claims only due rows; undo before the deadline deletes the row and reopens
the draft. Crash-safe: a pending outbox row survives restart and sends late
rather than never.

### What undo deliberately is not (the challenge, taken)

Emacs' undo-tree restores *exact prior states* of a closed world (one
buffer). Ours is an open world: IMAP ingest mutates state underneath, and
some effects (a delivered mail) are physically irreversible. So:

- **Undo is compensation, not restoration.** Moving through the DAG replays
  inverses/re-applies on top of *current* reality; it never pretends the
  world rolled back.
- **Irreversible nodes are transparent, not barriers.** `cmd+z` undoes the
  most recent *undoable* action, skipping an expired send when the earlier
  action touches an independent entity (the `entity` column arbitrates).
  Blocking all history behind one sent mail would be wrong; silently
  "undoing" it would be a lie.
- **Granularity is curated.** Focus and camera changes are recorded as
  context *on* nodes (undo restores where you were), never as nodes — else
  the tree is noise. Rapid same-kind, same-entity actions coalesce.
- **Branching is data first, UI later.** `parent` costs one column; the
  linear `cmd+z`/`cmd+shift+z` UX ships first; the history overlay (browse
  the tree, jump by replay) is its own phase. True travel-to-any-node only
  if real use earns it.

Panel-content text editing (compose body) is *not* system undo — that's the
future editor's local concern; `send` is where the system takes over.

## The mail engine

- **Accounts** in the DB (host, ports, auth kind, folder roles); secrets in
  the keychain. Multiple accounts from day one; the default Inbox is unified
  (a query), per-account inboxes are params.
- **Sync worker per account** (thread, sync `imap` crate + rustls): poll
  every N seconds v1 (IDLE later), UIDVALIDITY discipline, incremental UID
  fetch, flags reconciliation. Parsed with `mail-parser`; raw RFC822 kept.
- **Op executor**: claims `op` rows (`pending` guard), speaks IMAP
  (move/flag/delete), marks `done`/`failed`+error; retries with backoff;
  offline just queues. Local state is optimistic — the archive action already
  moved the local row; the op makes the server agree.
- **Sender**: claims due `outbox` rows, submits via `lettre` SMTP, appends to
  the Sent folder, records failures as status lines (the one red place).
- Threading (References/In-Reply-To) is stored from day one, *used* later.

## Panel context (agent-ready, not agent-wired)

`panel_context(pid)` returns, serializable:

```
{ kind, params, title, ws,
  queries: [ { id, sql, params, describe, rows, as_of_txid } ] }
```

— the panel row plus the query trace from its last draw. Because panels can
*only* read through the query layer, provenance is complete by construction,
not by discipline. A shortcut copies it; wiring it into an agent is future
work this CR only keeps honest.

## Schema sketch (v1)

```sql
-- substrate
CREATE TABLE meta(key TEXT PRIMARY KEY, value);
CREATE TABLE action(…);                        -- as above

-- ui (mutated only via actions)
CREATE TABLE workspace(k INTEGER PRIMARY KEY, focus INTEGER);
CREATE TABLE panel(id INTEGER PRIMARY KEY, ws INT, col INT, row INT,
                   kind TEXT, params TEXT DEFAULT '{}', joined_to INT,
                   tabbed INT DEFAULT 0);

-- mail
CREATE TABLE account(id INTEGER PRIMARY KEY, label TEXT, email TEXT,
                     imap_host TEXT, smtp_host TEXT, auth TEXT);
CREATE TABLE folder(id INTEGER PRIMARY KEY, account INT, name TEXT,
                    role TEXT, uidvalidity INT);
CREATE TABLE message(id INTEGER PRIMARY KEY, account INT, folder INT,
                     uid INT, message_id TEXT, in_reply_to TEXT, refs TEXT,
                     from_name TEXT, from_email TEXT, to_json TEXT,
                     date REAL, subject TEXT, unread INT, raw BLOB,
                     UNIQUE(account, folder, uid));
CREATE TABLE draft(id INTEGER PRIMARY KEY, account INT, re_message INT,
                   to_addr TEXT, subject TEXT, body TEXT, updated REAL);
CREATE TABLE outbox(id INTEGER PRIMARY KEY, draft INT, send_after REAL,
                    status TEXT DEFAULT 'pending', error TEXT);
CREATE TABLE op(id INTEGER PRIMARY KEY, account INT, kind TEXT,
                payload TEXT, status TEXT DEFAULT 'pending',
                created REAL, error TEXT);
```

Note: `MailId` stops being `&'static str` — messages get DB identities; this
ripples through `Kind` in phase 1.

## Phases

Each lands green (unit + all e2e suites) with its book pages updated.

1. **The store under everything.** — **landed 2026-09-01**: rusqlite
   (bundled, hooks), schema v1, seeded demo world, query layer with
   authorizer-captured deps, wm write-through + boot restore, `--db` +
   fresh temp store per e2e run, android verified (emulator; store opens,
   inbox renders from SQLite). Deviation from the sketch: panel params are
   typed columns (`p_int`/`p_txt`), not JSON — queryable, join-able. rusqlite in; schema v1; demo mail seeded;
   panels + launcher read through the query layer (`src/data.rs` retires);
   `Wm` write-through + boot restore; `--db`, temp DB for e2e. *Visible win:
   restart restores your session; the DB opens in `sqlite3`.*
2. **Actions and undo.** Session-changeset action wrapper; navigation +
   archive/read as actions; `cmd+z` / `cmd+shift+z`; coalescing; toasts name
   what they did. Linear UX, DAG data. — **landed 2026-09-01**: session
   feature verified on the android cross-build (bindgen on the host); every
   mutation site routes through `State::act`; focus/switch/camera stay
   non-actions; undo teleports back via the action's own workspace rows;
   conflicts (rows changed since) OMIT-skipped; `wm.active` moved out of
   `meta` into a recorded `wm` table (schema v2); menu bar Undo/Redo.
3. **IMAP read.** Accounts + keychain; sync workers; real unified inbox;
   bodies via mail-parser. — **landed 2026-09-01**: settings panel (add
   form with masked password, per-account status lines, undoable
   add/remove); one worker thread per account, own connection, 60 s poll +
   kickable Refresh; folders by special-use; 200-message window per
   folder; `dirty` rows immune to reconciliation until phase 4 pushes
   them; foreign commits reach the UI via SignalToUI + `data_version`.
   Engine unit-tested against a fake transport; the `.invalid`-host e2e
   exercises the real signal path.
4. **Ops for real.** Executor to the server (archive/delete/flags),
   offline-safe, undo enqueues compensations.
5. **Send.** Drafts persist; outbox with a 10 s window; SMTP worker; undo-send;
   Sent append.
6. **The history overlay.** The DAG made visible and walkable; jumping =
   replaying compensations along the path. Entry point decided then.
7. **Panel context.** The trace serialized behind a shortcut.

## Decisions needed

1. **Undo scope**: bless "DAG data + linear undo first, overlay later,
   compensation semantics, irreversibles transparent" — or argue for true
   travel now.
2. **Real accounts**: which providers first? Plain IMAP/app-password
   (fastmail-style) is straightforward; **Gmail OAuth is its own work
   package** (XOAUTH2 + token refresh + a browser hop). Need the actual
   account list to size phase 3.
3. **Account setup UX v1**: rows inserted via `sqlite3`/a tiny CLI (fastest),
   or a real settings panel from the start?
4. **rel.systems relationship**: bless "idioms not crates, substrate-shaped
   schema, workers as in-process providers".
