# Device Sync

Two devices, one store, a leased single writer. Device sync is not an app: it
replicates the store itself, every app's tables included, and the shell depends
on it. The write gate in `Session::act`, the locked screen, and the lease
driver are all the kernel's, in `kernel/src/repl/`.

## The log

SQLite's session extension records each transaction over the durable tables as
a changeset in `repl_log`. The changeset and its log row are written in the
same transaction. Applying a changeset from another device records nothing, so
an applied frame never echoes back into the log it came from.

A batch is a length-prefixed list of frames, one per transaction, so a failed
apply can name the transaction that failed rather than the whole batch. Every
device-sync object carries a wire-format version, and an unknown value is
refused rather than guessed at.

## The bucket

Devices exchange snapshots and batches through a small object-store interface:
read, create-only write, and compare-and-swap. A single `state` object contains
both the current lease and the log head. Updating the head therefore also
proves that the device still owns the lease.

The first device finds no lineage and **bootstraps**: it becomes the holder,
seeds the demo world, uploads a snapshot, and publishes. Another device
installs that snapshot and then applies later batches.

The transport is chosen by the URL. `http://` is `bucketd`, a small daemon
serving a directory with the compare-and-swap semantics the lease needs, which
is what local demos use. `https://` is Cloudflare R2 through its S3 API, where
`If-None-Match: *` is the create-only put and `If-Match: <etag>` is the
compare-and-swap, each answered `412` when its precondition loses. The
single-writer property therefore rests on the object store itself.

A local bucket is polled every 1.5 seconds and a real one every 5, because two
million class-B operations a month is a lot to pay for asking a question whose
answer almost never changes. A holder's write publishes at once either way: the
driver is kicked, not waited for.

## The lease

Only the lease holder may write. `Role` is where a device stands:

| Role | What it means |
|---|---|
| `Detached` | no bucket configured, or no lineage yet: local and writable |
| `Holder` | this device holds the lease and the store is writable |
| `Free` | the last holder released it; anyone may acquire |
| `Follower` | another device holds it; read-only |
| `Stranded` | the lineage moved to an epoch past ours; read-only, recovery is manual |
| `Offline` | the bucket could not be reached this pass |

A pass never fails. One that cannot reach the bucket answers `Offline` rather
than failing, because offline has to keep working: a prior holder keeps writing
and surfaces the risk as a count of unpublished frames, while a follower stays
locked. A device that has never joined a lineage stays writable and local; it
is not locked out because the bucket happened to be down before its first join.

The holder releases the lease on sleep and on close. Taking it from a live
device may discard changes it has not published, so the screen says so. The
lease driver keeps its own thread and command channel inside the kernel: it
needs acquire, release, and override, not only a kick, which is why it is not
an ordinary [worker](./apps.md#workers).

An unreachable bucket while a holder is accruing unpublished frames is the
kernel's own [problem](./apps.md#problems) source, listed before any app's. A
follower behind the locked screen is not listed, because the screen says it
already.

## The locked screen

When a bucket is configured and this device may not write, a full-window modal
owns every hit and offers to take the lease. It is not an overlay: an overlay
is something a person raised and can dismiss, and this is a fact about the
device. It goes when the lease turns over and not before. It is drawn under the
toast, so an *acquiring…* line still shows.

The card's title and its button follow the role: *the lease is free* with
**acquire**, *another device is writing* with **take over**, *this device has
diverged* with **recover**, and *offline, the bucket is unreachable* with no
button at all. The reason the last pass gave, when it had one, is one more line
under it, so `bucket GET state: 403 SignatureDoesNotMatch` reaches the screen
rather than only stderr.

## The bucket form

Device sync is not an app, so its form is the shell's, drawn by the `system`
app like every other panel there. It has three fields, the bucket URL, the
access key id and the secret, and a **connect** verb. It is the launcher root
*device sync*.

This is the road a device with no shell and no cable has: a phone is still a
device that has to be given a credential, and typing one in is the only way it
can be. Connect does three things:

- the secret — the token's value — goes to the platform's secret store through
  the effect boundary, so a scripted run writes to memory and never to a
  human's keychain;
- the URL and the key id, and only those two, are written to a `bucket` file
  beside the store, so a file that carried a secret on its third line is
  rewritten without it;
- the lease driver is restarted onto the new bucket, the old lease handed back
  first, so connecting takes effect without a relaunch.

The secret field is write-only. It seeds blank even on a configured device,
because a key that can be read back off a screen is a key that leaves by a
route nobody chose. Leaving it empty on a device that already has one keeps it.

The bucket URL is resolved, in order, from `--bucket`, the `SUPERAPP_BUCKET`
environment variable, and the first line of the `bucket` file beside the store.
The access key id and its secret come from `SUPERAPP_R2_ACCESS_KEY_ID` and
`SUPERAPP_R2_SECRET_ACCESS_KEY`, from lines 2 and 3 of that file, or from the
platform's secret store. `superapp --r2-login` reads a secret from stdin and
files it, because an argument is in `ps` and in the shell's history and this one
key can write the whole lineage.

## One token, two doors

What is filed as the secret is the **Cloudflare API token's value**, not the S3
secret access key the dashboard shows beside it. By Cloudflare's own definition
the second is the SHA-256 of the first, so R2 hashes on the way to a signature
and sees exactly the credentials it saw before, computed one line earlier —
and the same entry can be borne whole by whatever else the account owns. The
[agent](./agents.md#one-cloudflare-token-shared-with-r2)'s gateway is what
asks for that: it reads this entry and no other, and takes its account from the
first label of the bucket's host, so a device that syncs has a gateway and a
device that does not has neither.

The token wants three permissions in the dashboard: *Workers R2 Storage Edit*
for the bucket, and *AI Gateway Run* and *Workers AI Read* for the gateway.

A device configured before this change filed the hash, and from a hash no token
can be recovered. It is recognised by its shape — 64 hex digits, which a
40-character Cloudflare token can never be — so that device keeps syncing on
what it holds; only the gateway asks for anything, and what it says is to run
`superapp --r2-login` again, with the token's value.

## What is proven

The lease protocol is model-checked. `formal/Lease.tla` is a TLA+ model of
`kernel/src/repl/` covering every read and compare-and-swap interleaving,
offline passes, and overrides, with the single-writer and history properties
checked exhaustively over a bounded state space. The one hole it found, a
superseded holder's unpublished writes surfacing under a later lease, is what
`acquire`'s unconditional reset closes. `formal/README.md` says how to run it.

Beside the model, the kernel's own tests drive two devices over an in-memory
bucket and over a live socket through `bucketd`, and the R2 client's signature
is pinned against the AWS SigV4 test vector.

The walks that need two processes and a daemon are in `e2e/sync/` and are the
one directory `run-all.sh` leaves out. `docs/device-sync-demo.md` is the whole
demo, local and against a real R2 bucket.
