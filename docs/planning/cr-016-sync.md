# CR-016 · Device sync, again: one store per device, and a log of what you meant

Status: **accepted** (Andrey, 2026-09-12: "we are not replicate sqlite db
anymore … each app instance has it's own sqlite db … we do replicate _some_
actions using CRDT … can we use iroh for P2P replication? … keep r2
credentials / libs, we are going to use it for backups"; then: workspace
layout device-local, no R2 store-and-forward, "don't think a lot about
migration of current stores, only credentials and RSS read statuses are
something that is useful").

## Why

Device sync today is one SQLite lineage shared by every device: SQLite's
session extension captures every transaction as a changeset, a lease in an
R2 bucket says which device may write, and every other device is a read-only
follower behind a full-window lock. Everything hard about it — the lease, the
handoff, `Stranded`, recovery backups, the schema fingerprint, the snapshot
install, two TLA+ models, six thousand kernel lines — follows from one
decision: that the *whole* store is the unit of replication. Changesets do not
merge divergent branches, so there can be only one writer, so the phone is
locked while the laptop is open.

Most of what flows through that log is not ours to replicate. Mail comes from
IMAP, chats from TDLib, events from Google, articles from the feeds; every
device can ask the provider itself, and the providers already carry the read
flags that matter. What a person actually decides on a device is small: a
subscription, a note, a read mark on an article, an account's settings, the
name of a device. That is the set worth carrying, and it is small enough to
merge instead of fence.

## The model

> **Each device owns its store. What replicates is what a person decided, not
> what a provider said — as cells, last writer wins, over iroh.**

Four rules, and the fourth is the whole of the new machinery.

### One store per device, always writable

A device's SQLite file is its own. There is no lease, no follower, no locked
screen, no admission gate, no generation, no `Stranded`, no recovery. Every
open is a writer open; `Step::Writer` collapses into `Step::Always`. The
process lock beside the file stays: two processes on one file was never a
sync question.

Provider caches stay where they are fetched. Mail, Telegram, calendar and
feed *content* are re-derived on each device from the source. Workspaces,
columns, panels and `wm` are device-local: a phone and a desktop do not want
the same layout, and the `Missing` panel keeps its narrower job of carrying a
tag this build does not own.

### What replicates is declared

An app names the tables that replicate, the stable key that identifies a row
on every device, and the columns that carry a decision. Everything else on
the row — the local rowid, fetch bookkeeping, caches — stays local.

```rust
pub struct Replicated {
    /// The table.
    pub table: &'static str,
    /// The columns that identify a row on every device; a UNIQUE index
    /// over exactly these must exist. Never a rowid.
    pub key: &'static [&'static str],
    /// The columns whose values travel. At least one.
    pub columns: &'static [&'static str],
}

// on App:
fn replicated(&self) -> &'static [Replicated] { &[] }
```

At open, the kernel checks every declaration against the schema: the table
exists, the key has its unique index, every named column exists, and every
column but the key has a default or accepts NULL, whether it travels or
not — because a row created by another device arrives one cell at a time,
and SQLite tests NOT NULL before the conflict that would have made that
insert an update. A declaration that fails is refused in one line, like a
kernel version mismatch.

The first set:

| Table | Key | Replicated columns | Stays local |
|---|---|---|---|
| `sync_peer` (kernel) | `device` | `name`, `added`, `removed` | — |
| `rss_feed` | `url` | `subscribed` | `title`, `checked`, `error`, `etag`, `modified`, `requested`, `completed` |
| `rss_seen` (new) | `feed_url`, `guid` | `seen` | — |
| `notes_note` | `uid` (new) | `title`, `body`, `created`, `modified`, `deleted` | `id` |

Not in this change: accounts (address, servers and enabled services would
be worth carrying, but `account` identifies itself by `email` with a partial
unique index on `google_sub`, and a row created on two devices before they
paired would collide on that index — it needs its own key story first),
agent chats (append-only, easy later), mail drafts, calendar drafts,
Telegram anything, workspace layout (decided local).

RSS read state moves out of the article row as the fact. `rss_seen` is about
`(feed url, guid)` and can exist before the article does, because this
device has not fetched it yet. `rss_article.seen` stays as a projection kept
by triggers: an insert or an update of `rss_seen` writes the matching
article's `seen`, and an article arriving later picks up its mark on insert. The
`Flags` write for `Flag::Seen` goes to `rss_seen`; every read of `seen` stays
as it is.

Notes gain `uid TEXT NOT NULL UNIQUE`, defaulting to sixteen random bytes in
hex so no insert site changes. Existing rows derive theirs from `(id,
created)` — two notes made in one virtual instant share a `created` — so two
devices that once shared a lineage produce the same uid for the same note
and merge instead of duplicating; that needs a table rebuild, because
`ALTER TABLE` cannot add a column with an expression default. The local `id`
keeps its place in panel args. `rss_feed` is rebuilt too: `title` had no
default, and a feed another device subscribed to arrives with only its url
and `subscribed`.

### Every change is a cell op

The writer already opens a session over the replicated tables inside every
`Store::write`. That capture stays; what it feeds changes. After the closure
runs and before commit, the changeset is walked and turned into ops:

- an **INSERT** yields one op per replicated column, with the row's key;
- an **UPDATE** yields one op per replicated column that changed. The key is
  read back by rowid inside the same transaction, because a changeset's
  `UPDATE` record carries only primary-key and changed columns. Changing a
  key column is refused: the write fails, nothing commits;
- a **DELETE** yields one tombstone op — the empty column name — from the old
  row's key.

A write that touches no replicated column emits nothing. Applying a peer's
ops runs without a capture, so nothing echoes.

```sql
CREATE TABLE sync_op(
  origin TEXT NOT NULL,      -- the device that made the change
  seq    INTEGER NOT NULL,   -- that device's own count, from 1, no gaps
  hlc    INTEGER NOT NULL,   -- hybrid logical clock, see below
  tbl    TEXT NOT NULL,
  key    TEXT NOT NULL,      -- JSON array of the key values, in declared order
  col    TEXT NOT NULL,      -- '' is the row's tombstone
  val    TEXT,               -- JSON; SQL NULL is NULL, a blob is {"b64":…}
  PRIMARY KEY(origin, seq)
);
CREATE TABLE sync_cell(      -- who last won each cell, so a merge is one read
  tbl TEXT NOT NULL, key TEXT NOT NULL, col TEXT NOT NULL,
  hlc INTEGER NOT NULL, origin TEXT NOT NULL,
  PRIMARY KEY(tbl, key, col)
);
CREATE TABLE sync_have(      -- the vector clock: ops held, contiguous, per origin
  origin TEXT PRIMARY KEY, seq INTEGER NOT NULL
);
CREATE TABLE sync_self(      -- this device
  id       INTEGER PRIMARY KEY CHECK(id = 1),
  device   TEXT NOT NULL,    -- its iroh endpoint id
  next_seq INTEGER NOT NULL DEFAULT 1,
  hlc      INTEGER NOT NULL DEFAULT 0
);
CREATE TABLE sync_peer(      -- replicated: the roster
  device  TEXT PRIMARY KEY,
  name    TEXT NOT NULL DEFAULT '',
  added   REAL NOT NULL DEFAULT 0,
  removed INTEGER NOT NULL DEFAULT 0
);
```

The log is kept whole. At this volume — a read mark is one row — compaction
is a later change, not this one.

**The clock.** An op's `hlc` is `(unix milliseconds << 16) | counter`. Issuing
one takes `max(now << 16, last + 1)`; seeing one takes `last = max(last,
seen)`. Two ops compare by `(hlc, origin)`, so a tie has one answer on every
device. `now` is the world's clock, not the wall's: a scripted run stamps its
ops from virtual time and stays deterministic, and `store.rs`'s one direct
read of `SystemTime` goes with `repl_log.ts`.

**Apply.** For each incoming op, in `(origin, seq)` order: it is recorded in
`sync_op`; if the row's tombstone is newer, it is done; if `sync_cell` holds a
newer winner for the cell, it is done; otherwise the cell is written —
`INSERT … ON CONFLICT(key) DO UPDATE` over the declared key, so a row that
does not exist yet is created with its defaults — and the winner is recorded.
A tombstone deletes the row and becomes the winner of `''`, then re-applies
every cell whose winner is newer than it, so the row after "deleted here,
edited there" is the same in either arrival order: gone if the delete was
last, back with the newer cells if an edit was. Ops for a table or
column this build does not declare are kept in the log and not applied: a
newer build will. An op whose cell write fails a constraint — a unique index
that is not the key, a check — is kept, skipped, and reported once as a
problem; it never stalls the run. `sync_have` advances only over a
contiguous run.

Rows that predate the log — what a migration left behind — are filed at
every open as ops of this device's own with `hlc = 0`, one per cell
`sync_cell` has no winner for: below any real op, so an edit made anywhere
since wins, and two devices backfilling one lineage tie by origin over
equal values. One transaction, and the second open files nothing.

All of this is one transaction on the one writer, so the update hook
invalidates the cached queries that drew those rows, exactly as a local edit
would.

### The exchange is a conversation between two peers

Over one bidirectional byte stream, length-prefixed JSON frames:

```
Hello { v, device, name, pairing? }   -- both sides, first
Have  { [(origin, seq)] }             -- what I hold, contiguous
Ops   { [Op] }                        -- what you lack, in (origin, seq) order, chunked
```

Each side answers the other's `Have` with every op past it, from every origin
it holds — so a device carries a third device's ops, and the roster need not
be fully connected at once. A backlog goes out while the other side's is
coming in: the reader and the writer are each a task, and the loop between
them waits only for room in the writer's queue, taking an inbound frame in
preference to that — two devices that both wake with a backlog would
otherwise each fill the other's pipe and wait to be read by a peer that was
waiting itself. The connection then stays open; a local commit
that produced ops sends them on every live connection, and a reconnect starts
again from `Have`, which is what makes a lost frame harmless.

The protocol is written over `AsyncRead + AsyncWrite`. The kernel's tests
drive two stores over `tokio::io::duplex` under virtual time and prove
convergence: concurrent edits of one note, a tombstone against an edit, a
transitive third device, a reconnect after a cut. Iroh is one implementation
of the stream.

### Peers, pairing, and the ticket

A device's identity is its iroh endpoint id — the public half of an Ed25519
key the device makes on first open and keeps in the platform secret store
under `sync/key`, next to the R2 token and the IMAP passwords. A scripted run
makes a fresh one in its in-memory secrets. Its own row in `sync_peer` is
written at open.

Connections are mutually authenticated by iroh; the question is only whether
the other end is *ours*. The listener accepts a dialer that is in
`sync_peer` and not removed. Anyone else must show a pairing secret:

- the *device sync* panel shows this device's **ticket** while it is open —
  an `EndpointTicket` for this endpoint plus sixteen fresh random bytes,
  rotated every time the panel opens and worthless once it closes;
- the other device pastes it into **pair with** and presses **pair**: it
  dials the ticket's endpoint on ALPN `superapp/sync/1` and says `Hello` with
  the secret. On a match the exchange begins, and once `Have` has crossed
  each side adds the other to `sync_peer` — an ordinary local write, so the
  roster replicates; a wrong secret leaves nothing on either side. The
  dialer trusts the listener because it dialed the key in the ticket, and
  iroh proved the answerer holds it.

A wrong or stale secret closes the connection before any `Have`. **Forget**
sets `removed` on a peer's row; a removed device is refused on both ends from
the moment the op lands. A ticket is a long string; Telegram's saved messages
is the practical road between a laptop and a phone until a QR code exists.

### The service

A kernel task, like the lease driver was, and for the same reason: it wants
an endpoint, an accept loop and a dial loop, not a worker's pass.

- The endpoint binds with `iroh::endpoint::presets::N0` — public relays and
  DNS lookup, rate-limited and end-to-end encrypted — plus local-network
  lookup (`iroh-mdns-address-lookup`) so two devices on one Wi-Fi never
  leave it. A scripted run does not bind at all; the e2e walk that needs two
  real processes binds `presets::Minimal` on an explicit `127.0.0.1:0`, with
  no relay and no lookup, because iroh omits loopback from an endpoint's
  addresses whenever another interface exists.
- The dial loop tries every live peer that has no connection, with backoff
  from five seconds to five minutes, and is kicked by a local commit that
  produced ops, by foreground and resume, and by pairing.
- The accept loop routes the ALPN to the handshake above.
- Status is an in-memory snapshot per peer — connected, last seen, last
  error, ops behind — read by the panel. An unreachable peer is the panel's
  line, not a problem: being away is a peer's ordinary state. A problem
  source says only what is wrong — the ops a constraint refused — once, not
  per pass.

Relays forward live traffic and store nothing. Two devices that are never
awake together do not converge until they are; ops wait in their origin's
log, and nothing is lost. That is the trade taken here: no store-and-forward
through R2.

### The panel

*device sync* is the shell's, drawn by the `system` app, launcher root
`device sync`:

- this device: its name (editable, replicated) and short id;
- **ticket** with a **copy** verb;
- **pair with**: one field and the **pair** verb;
- peers: name, short id, state; **forget** on each.

The R2 form keeps its three fields and its write-only secret discipline and
becomes the **backup** root: **connect** files the credentials and nothing
else. The agents' gateway keeps reading the same token entry.

## What goes

- `kernel/src/repl/` except `r2.rs` and the part of `object.rs` the R2 client
  needs, which move to `kernel/src/r2/`; `MemBucket`, `HttpBucket`, the
  bucketd server, the lease `State`, batches, snapshots.
- `kernel/src/store/authority.rs`, `store/repl.rs`, `store/repl/replay.rs`;
  `repl`, `repl_log`, `repl_event`; `Db::authority`, generations,
  `Activity`, `admit`, `quiesce`, `requires_writer`, `SUSPENDED`,
  `set_writable` / `is_writable`, `grant_async`, `request_release`,
  `Job::Apply`, `Job::Raw`, `Wrote.cs`, `write_recorded`, `take_changeset`.
- `kernel/src/session/repl_mount.rs`; the lease gate in `restore`; the
  follower refusal in `edits.rs`; the two-phase release in `shutdown.rs`
  becomes flush-then-close.
- `app/src/shell/lock.rs`, `Act::{Acquire, ForceAcquire, RecoverSync}`,
  `tick_repl`, `initialize_authority`, every "another device holds the
  lease" branch in the apps.
- `app/src/bin/bucketd.rs`, `sync-demo.rs`, `reseed-edit.rs`; `e2e/sync/`;
  `formal/`; `docs/device-sync-demo.md` until phase 3 rewrites it.
- The agent's inner `sql.write` capture stays: undo is not sync.

## Migration

Kernel schema goes to **2**: a version-1 store drops the three `repl` tables
and gains the `sync_*` tables; a version-0 store is created at 2. Nothing
else is asked of the old design. Notes derive `uid` from `created`;
`rss_seen` is filled from `rss_article.seen` joined to its feed's url.
Credentials are in the secret store and untouched. Two devices that shared a
lineage come up as two unpaired devices with equal data; pairing them merges
the equal rows by key.

## Phases

1. **Remove the lease.** Everything under *What goes*; the store always
   writable; kernel schema 2; the bucket form filing credentials only; the
   book's chapter reduced to a stub that points here and the other chapters'
   mentions corrected. Green: `cargo test --workspace --no-default-features`,
   clippy, headless build, `e2e/run-all.sh`.
2. **The log.** `kernel/src/sync/`: schema, clock, `Replicated` and its open
   check, capture-to-ops in `do_write`, apply, `Have`/`Ops` over a stream,
   the pairing handshake, status; `sync_peer`; notes and rss declared and
   migrated; duplex tests under virtual time.
3. **Iroh.** The endpoint, the service, the panel, the e2e walk with two
   processes on loopback, the chapter, the demo doc.
4. **Later.** R2 backups (`VACUUM INTO`, per device, restore takes a new
   identity), accounts with a key of their own, agent chats, a QR ticket,
   log compaction, the Fold build.

## Decisions taken

- Layout is device-local. (Andrey, 2026-09-12)
- No R2 store-and-forward; peers converge when both are awake. (Andrey,
  2026-09-12)
- Cells, not documents: notes merge by last writer per column, and a
  concurrent edit of one body on two devices keeps one. A text CRDT is a
  later change if that ever hurts.
- The kernel's MSRV rises to iroh's 1.91; the app already asks for 1.92.
- iroh's `tls-ring` matches the kernel's rustls pin; no second provider.
  `portmapper` stays off (nine crates including a second HTTP client and
  an XML stack, for UPnP the relay already covers). iroh brings
  `reqwest 0.13` beside the kernel's `0.12`; moving the kernel to 0.13 is a
  follow-up that dedupes an HTTP client.

## Limits

No proof of the merge beyond its tests; the property is the usual one for
last-writer-wins registers under a hybrid clock and needs no model of its
own. The log is never compacted. A device restored from a backup must take a
new identity, which is phase 4's problem. Public relays are rate-limited;
a self-hosted relay is the fallback if that is ever felt.
