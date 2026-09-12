# Device Sync

Every device owns its store and writes to it whenever it likes. What travels
between two devices is not the file but what a person decided on one of them —
a subscription, a read mark, a note, the name of a device — carried as
individual cells, last writer wins, over a direct connection.

Mail comes from IMAP, chats from TDLib, events from Google, articles from the
feeds. Every device can ask those providers itself, and they already carry the
read flags that matter, so none of that replicates. What is left is small
enough to merge instead of fence.

## One store per device

A device's SQLite file is its own. Every open is a writer open: there is no
lease, no follower, no admission gate, and no screen a device waits behind. A
phone writes while the laptop is open, and neither asks the other first.

Two devices exchange rows once they have been **paired** with each other, and
not before. Signing into the same Google or Telegram account on both pairs
nothing: those are provider sessions, and each device keeps its own.

Provider caches stay where they were fetched, and so does the layout.
Workspaces, columns, panels and `wm` are device-local, because a phone and a
desktop do not want the same arrangement of the same work. The `Missing` card
is therefore about one device's own history: a tag no app in this build owns
came from an older or differently built version of *this* device.

A file-backed database still takes a Unix process lock before migrations. Two
processes cannot share one device identity and independently run its native
sessions; the lock survives until the writer drains and closes SQLite, and
another reader inside that process must use the same `Db`. Two processes on one
file is not a sync question. Non-Unix file stores need a platform lock
implementation before they can open.

## What replicates

An app names the tables that replicate, the key that identifies a row on every
device, and the columns that carry a decision. Everything else on the row — the
local rowid, fetch bookkeeping, caches — stays local.

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
```

`App::replicated` answers with a slice of these, and with none by default.

At open the kernel checks every declaration against the schema: the table
exists, the key has its unique index, every named column exists, and **every
column but the key** has a default or accepts NULL — whether it travels or
not. A row created on another device arrives one cell at a time, so the
insert that makes it names the key, that one column, and nothing else; and
SQLite tests NOT NULL before it looks for the conflict that would have turned
that insert into an update, so one required column with no default stops
every cell of the table, new row or old. A declaration that fails is refused
in one line, the way a kernel version mismatch is. `rss_feed.title` gained a
default for exactly that reason — a subscription made on the phone reaches
the laptop before the laptop has ever fetched the feed's name — and so did
`sync_peer.added`, which is why a rename or a **forget** now crosses.

| Table | Key | What travels | What stays local |
|---|---|---|---|
| `sync_peer` (the kernel's) | `device` | `name`, `added`, `removed` | — |
| `rss_feed` | `url` | `subscribed` | `title`, `checked`, `error`, `etag`, `modified`, `requested`, `completed` |
| `rss_seen` | `feed_url`, `guid` | `seen` | — |
| `notes_note` | `uid` | `title`, `body`, `created`, `modified`, `deleted` | `id` |

RSS read state is a fact about `(feed url, guid)` rather than about an article
row, because it can exist before the article does: the other device read
something this one has not fetched yet. `rss_article.seen` stays as a
projection kept by triggers — writing `rss_seen` marks the matching article,
and an article arriving later picks up its mark on insert — so every read of
`seen` is unchanged.

A note is identified by `uid`, sixteen random bytes in hex, defaulted by the
column so that no insert site changes; the local `id` keeps its place in panel
arguments.

## The log

The writer opens a session over the replicated tables inside every
`Store::write`. After the closure has run and before the commit, the captured
changeset is walked and turned into **ops**, one per cell:

- an **INSERT** yields one op per replicated column, under the row's key;
- an **UPDATE** yields one op per replicated column that changed. The key is
  read back by rowid inside the same transaction, because a changeset's update
  record carries only the primary key and the columns that moved. Changing a
  key column is refused: the write fails and nothing commits;
- a **DELETE** yields one **tombstone** — the op whose column name is empty —
  under the old row's key.

A write that touched no replicated column emits nothing, and applying another
device's ops runs with no capture at all, so nothing echoes back.

```sql
CREATE TABLE sync_op(
  origin TEXT NOT NULL,      -- the device that made the change
  seq    INTEGER NOT NULL,   -- that device's own count, from 1, no gaps
  hlc    INTEGER NOT NULL,   -- hybrid logical clock
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

The `sync_*` tables are made by presence rather than by the kernel's schema
number, and corrected the same way: a store whose `sync_peer` still has
`added` without its default is rebuilt on its next open. The devices that ran
the build which wrote it that way are already stamped with this kernel's
number, so a ladder would never reach them.

**The clock.** An op's `hlc` is `(unix milliseconds << 16) | counter`. Issuing
one takes `max(now << 16, last + 1)`, and seeing one takes
`last = max(last, seen)`. Two ops compare by `(hlc, origin)`, so a tie has the
same answer on every device. `now` is the world's clock rather than the wall's:
a scripted run stamps its ops from virtual time and stays deterministic.

**Applying.** Incoming ops are taken in `(origin, seq)` order, and each is
recorded in `sync_op`. It is then done with if the row's tombstone is newer, or
if `sync_cell` already holds a newer winner for that cell. Otherwise the cell
is written — `INSERT … ON CONFLICT(key) DO UPDATE` over the declared key, so a
row that does not exist yet is created with its defaults — and the winner is
recorded.

A tombstone becomes the winner of `''` and sweeps the row away. What survives
it are the cells that are newer than it: an op newer than the tombstone
recreates the row from those and the defaults, which is what *edited after it
was deleted elsewhere* should mean. A tombstone that arrives after such an edit
rebuilds the row the same way rather than deleting it, so the two devices agree
whichever order the two ops reached them in.

Two rules keep one odd op from stopping the rest. An op for a table or column
this build does not declare is kept in the log and not applied, because a newer
build will know what to do with it. An op whose write fails a constraint — a
unique index that is not the key, a check — is kept, skipped, and reported once
as a problem; it never stalls the run. `sync_have` advances only over a
contiguous run.

All of it is one transaction on the one writer, so the update hook invalidates
the cached queries that drew those rows exactly as a local edit would: a peer's
note reaches an open list without anybody asking for it. The store counts the
cells a peer's ops wrote, and `Session::poll_sync` — which the shell asks on
every look outside — kicks and rediscovers the workers when that count has
moved, the way a local edit does through `Session::act_async`. Nobody here
pressed anything, so nothing here would otherwise have started the fetch of a
feed the phone subscribed to.

**What was here before the log.** A store that has been through a migration
holds rows no op ever described: notes typed before this build, feeds
subscribed to, articles marked read. Every open walks the declared tables and
files one op per cell `sync_cell` has no winner for, from this device, at
**`hlc = 0`** — under any real op there could be, so an edit made anywhere
since still wins, and two devices backfilling one lineage tie by origin over
values that are equal anyway. It is one transaction on the writer, and the
second open files nothing, because the first left a winner behind for every
cell it touched. A column a later build adds to a declaration is backfilled on
the open that declares it.

The log is kept whole. A read mark is one row, and at that volume there is
nothing to compact.

## The exchange

Two peers talk over one bidirectional byte stream, in length-prefixed JSON
frames:

```text
Hello { v, device, name, pairing? }   -- both sides, first
Have  { [(origin, seq)] }             -- what I hold, contiguous
Ops   { [Op] }                        -- what you lack, in (origin, seq) order, chunked
```

Each side answers the other's `Have` with every op past it, from **every**
origin it holds and not only its own. A laptop therefore carries a phone's ops
to a tablet, and three devices converge without the three of them ever being
awake together.

A backlog goes out while the other side's is coming in. The reader and the
writer are each a task of their own — a half-read frame must not be cancelled
by a local commit, and a half-written one cannot be taken back — and the loop
between them only ever waits for *room* in the writer's queue, taking an
inbound frame in preference to that. Two devices that both wake with a
thousand ops would otherwise each send until the other's pipe was full and
then wait to be read by a peer that was waiting to be read itself.

The connection then stays open. A local commit that produced ops sends them on
every live connection, and a reconnect starts again from `Have` — which is what
makes a lost frame harmless, since nothing is acknowledged and the next `Have`
says what is actually held. A `Have` puts the backlog cursor back, and what a
peer sent is never sent to it again.

The protocol is written over `AsyncRead + AsyncWrite`. iroh is one
implementation of that stream.

## Peers, pairing, and the ticket

A device's identity is its iroh endpoint id: the public half of an Ed25519 key
it makes on its first open and keeps in the platform secret store under
`sync/key`, beside the R2 token and the IMAP passwords. A scripted run makes a
fresh one in its in-memory secrets. Its own row in `sync_peer` is written at
open.

iroh authenticates both ends of a connection, so the only question left is
whether the other end is *ours*. The listener accepts a dialer that is in
`sync_peer` and not removed. Anyone else has to show a pairing secret:

- the *device sync* panel shows this device's **ticket** while it is open: an
  endpoint ticket for this endpoint plus sixteen fresh random bytes. It is
  rotated every time the panel opens and is worthless once it closes;
- the other device pastes it into **pair with** and presses **pair**. It dials
  the ticket's endpoint on ALPN `superapp/sync/1` and says `Hello` with the
  secret. On a match the listener adds the dialer to `sync_peer` — an ordinary
  local write, so the roster replicates like anything else — and the exchange
  begins. The dialer adds the listener the same way: it dialed the key in the
  ticket, and iroh proved the answerer holds it. Each side writes that row only
  once the other has answered its `Have`, so a wrong secret leaves nothing
  behind on either device.

A wrong or stale secret closes the connection before any `Have`. **forget**
sets `removed` on a peer's row, and a removed device is refused at both ends
from the moment that op lands.

A ticket is a long string. Until there is a QR code, Telegram's saved messages
is the practical road between a laptop and a phone.

## The service

The endpoint, the accept loop and the dial loop are a kernel task of their own
rather than an ordinary [worker](./apps.md#workers): a worker answers with one
pass and a wake, and this wants a listener and a set of connections that
outlive any pass.

- The endpoint binds the n0 preset — public relays and DNS lookup,
  rate-limited and end-to-end encrypted — plus local-network lookup, so two
  devices on one Wi-Fi never leave it. A scripted run does not bind at all
  unless it says `SUPERAPP_SYNC=loopback`, which is [the pairing
  walk](#validation-and-limits) asking for one socket on `127.0.0.1` that
  nothing off the machine can reach. On Android the app first hands its
  application context to `ndk_context`, because iroh's resolver reads the
  phone's DNS servers over JNI and Makepad does not fill that in; a device
  whose activity is not up yet runs without sync rather than panicking.
- The dial loop tries every live peer that has no connection, backing off from
  five seconds to five minutes. It is kicked by a local commit that produced
  ops, by foreground and resume, and by pairing — each of which matters only
  for a peer that has no connection, since a live one is pushed to by the
  protocol itself. A peer is dialed at the address it was last reached at —
  `sync_link(device, addr)`, device-local, filled from the ticket at pairing
  and from every session — or, failing that, at its bare id, which the
  lookups resolve.
- The accept loop routes the ALPN to the handshake above. A dialer this device
  is already talking to is dropped rather than doubled, and when two devices
  dial each other at once both keep the connection whose dialer has the
  smaller id, so one of the two survives on both sides.
- Status is an in-memory snapshot per peer — connected, when it was last heard
  from, and why the last attempt failed — and it is what the panel draws. How
  far behind a peer is stays zero: a connection that is open has had
  everything pushed to it already, and a peer that is away last said what it
  held in a `Have` on a connection there no longer is. The kernel's own
  problem source says how many devices are unreachable, once rather than once
  per pass, and counts only the peers that have failed since their last
  success; it says nothing on a device with no peers. The ops a constraint
  here refused stand as a second problem beside it.

Relays forward live traffic and store nothing. Two devices that are never awake
at the same time do not converge until they are: their ops wait in their
origin's log, and nothing is lost.

Quitting stops the service before the store closes: every session is closed
and the endpoint with it, so a peer hears why rather than waiting for a
timeout.

## The panel

Device sync is not an app, so its panel is the shell's, drawn by the `system`
app like every other panel there. *device sync* is a launcher root, and it
shows:

- this device — its name, which is editable and replicates, and its short id.
  The name is filed when it is finished with: on enter, or the moment the
  field is left;
- its **ticket**, as a selectable run, with a **copy** verb. It reads
  *connecting…* until the endpoint is reachable, which on a real device means
  until it holds a relay, and *not running* on a scripted run that bound no
  endpoint at all;
- **pair with**: one field and the **pair** verb. A string that is not a
  ticket, or this device's own, is refused where it was pasted;
- the peers: name (or short id), short id, one state line — *connected*, *seen
  3 min ago*, or the last error — and **forget** on each.

Opening the panel is what opens the pairing window, and closing it is what
closes it: the guard the open takes out goes with the instance. Nothing else
on the panel is stored — the two fields are the instance's, and everything
else is the service's snapshot, read on every draw.

## The bucket form

The R2 form is the launcher root *backup*. It has three fields — the bucket
URL, the access key id, and the secret — and a **connect** verb that files them
and does nothing else.

This is the road a device with no shell and no cable has: a phone is still a
device that has to be given a credential, and typing one in is the only way it
can be. Connect puts the secret — the token's value — into the platform's
secret store through the effect boundary, so a scripted run writes to memory
and never to a human's keychain. The URL and the key id, and only those two,
are written to a `bucket` file beside the store.

The secret field is write-only. It seeds blank even on a configured device,
because a key that can be read back off a screen is a key that leaves by a
route nobody chose. Leaving it empty on a device that already has one keeps it.

The bucket URL is resolved, in order, from `--bucket`, the `SUPERAPP_BUCKET`
environment variable, and the first line of the `bucket` file beside the store.
The access key id and its secret come from `SUPERAPP_R2_ACCESS_KEY_ID` and
`SUPERAPP_R2_SECRET_ACCESS_KEY`, from lines 2 and 3 of that file, or from the
platform's secret store. `superapp --r2-login` reads a secret from stdin and
files it, because an argument is in `ps` and in the shell's history. It takes
the key id from `SUPERAPP_R2_ACCESS_KEY_ID` or the file the first time and
remembers it in the secret store beside the token, so a device that never had a
bucket still knows which token it holds.

## One token, two doors

What is filed as the secret is the **Cloudflare API token's value**, not the S3
secret access key the dashboard shows beside it. By Cloudflare's own definition
the second is the SHA-256 of the first, so R2 hashes on the way to a signature
and sees exactly the credentials it saw before, computed one line earlier — and
the same entry can be borne whole by whatever else the account owns. The
[agent](./agents.md#one-cloudflare-token-shared-with-r2)'s gateway is what asks
for that: it reads this entry and no other, and takes its account from the
first label of the bucket's host.

The token wants three permissions in the dashboard: *Workers R2 Storage Edit*
for the bucket, and *AI Gateway Run* and *Workers AI Read* for the gateway.

A device configured before this change filed the hash, and from a hash no token
can be recovered. It is recognised by its shape — 64 hex digits, which a
40-character Cloudflare token can never be — and what the gateway says to do
about it is to run `superapp --r2-login` again, with the token's value.

## Validation and limits

The kernel's tests drive two stores over an in-memory duplex stream under
virtual time and check that they converge: concurrent edits of one note, a
tombstone against an edit, a third device carried by the second, a reconnect
after the stream is cut, and twelve thousand ops each way at once, which is
what proves neither side stops reading while its own backlog goes out. Others
take the roster across as an app's table would, rebuild an old one, and put
rows that predate the log in front of a device paired afterwards. Two more drive the service itself over two
real loopback endpoints: one device shows a ticket and the other pastes it,
after which what either writes reaches the other and **forget** ends it; and a
ticket whose window has closed leaves nothing behind on either side.

`e2e/sync/pair.sh` is the same thing as two processes, which is the only way
to prove the panel does it. Both bind the minimal preset on loopback, with no
relay and no lookup. A opens *device sync*; the scripted world writes the
ticket to the file `SUPERAPP_E2E_TICKET_OUT` names — honoured under a script
and nowhere else — and the script waits for it, substitutes it into B's walk,
and starts B while A is still up. A writes a note before B exists, so the
exchange that follows the pairing has to carry it; B writes one back over the
connection that stays open, and each asserts the other's note is in its own
list. Afterwards both stores are asked, in SQL, whether `sync_peer` holds the
same two devices. It is out of `e2e/run-all.sh`, where every suite is one
process, and runs as a step of its own.

There is no model of the merge beyond those tests. The property is the ordinary
one for last-writer-wins registers under a hybrid logical clock, and it needs
no proof of its own.

What last writer wins costs is plainest on a note's body: two devices that edit
the same note while apart keep one of the two edits and not a merge of them. A
text CRDT is what would change that.

The log is never compacted. A device restored from a backup takes a new
identity, because the key that names a device is in the secret store and not in
the file. Public relays are rate-limited, and a self-hosted relay is the
fallback.
