# Device sync: the demo, and the real bucket (CR-005)

Two devices, one store, a leased single writer. Two ways to run it: locally
against `bucketd` with no cloud account (below), or against a real
**Cloudflare R2** bucket ([The real bucket](#the-real-bucket-r2)). Same
engine, same contract — only the transport differs, and the app picks it from
the URL scheme.

The local transport is `bucketd`, a tiny object-store daemon that stands in
for R2/S3 (same compare-and-swap contract; see [`src/object.rs`]). The macOS
side reaches it at `127.0.0.1`; the android emulator reaches the *same* host
daemon at `10.0.2.2` (the emulator's alias for the host).

## 1. Build

```sh
mise exec -- cargo build                 # the app and bucketd
```

## 2. Start the bucket

```sh
./target/debug/bucketd --dir /tmp/superapp-bucket --port 9000
# curl http://127.0.0.1:9000/state  → 404 until a device bootstraps it
```

Leave it running. To start over, `rm -rf /tmp/superapp-bucket`.

## 3. Point the apps at it

The bucket URL is resolved, in order, from:

1. `--bucket http://HOST:9000` on the command line (desktop),
2. the `SUPERAPP_BUCKET` environment variable,
3. the first line of a `bucket` file **beside the store** (how android is
   configured).

## 4. Device A — the first to run

```sh
mise exec -- cargo run -- --db /tmp/superapp-A.db --bucket http://127.0.0.1:9000
```

A finds no lineage, so it **bootstraps**: it becomes the holder, seeds the
demo world, uploads a snapshot, and publishes. The account status shows *you
hold the lease*; the inbox is writable.

## 5. Device B — the android emulator

Build and install the APK (needs `cargo-makepad` + the NDK, per
[Developer Experience](book/src/dev-x.md)):

```sh
cargo-makepad android --sdk-path=$HOME/.cache/makepad-android-sdk \
  --package-name=dev.prepor.superapp --app-label=superapp build -p superapp
cargo-makepad android --sdk-path=$HOME/.cache/makepad-android-sdk \
  --package-name=dev.prepor.superapp --app-label=superapp run -p superapp
```

Point it at the host daemon by dropping a `bucket` file next to its store
(the app's files dir):

```sh
adb shell run-as dev.prepor.superapp sh -c 'echo http://10.0.2.2:9000 > files/bucket'
# then relaunch the app
```

B finds that A holds the lease. It **installs A's snapshot** (gaining the
same mail), materializes A's writes, and shows the **locked screen**: *held
by <device> — read-only*, with a **take over** button.

Two android networking notes, if B cannot reach the bucket:

- The APK needs `android.permission.INTERNET`. The client is a **raw TCP
  socket**, not the framework HTTP stack, so android's cleartext-traffic
  policy does not apply to it — but the socket permission still must be
  granted in the manifest.
- `10.0.2.2` is the emulator's alias for the host loopback; a physical device
  needs the host's LAN address instead, and `bucketd --bind 0.0.0.0`.

Against R2 the same two notes hold, minus the address: the socket is still a
raw one, TLS is rustls against the Mozilla roots compiled in, so android's
system trust store and network-security config are not in the path either —
only `INTERNET` is. The crates are the ones imap and lettre already build for
that target, so an APK that can fetch mail can reach R2.

## 6. The handoff

- On A, quit the app (or background it): A **releases** the lease.
- On B, the locked screen now reads *the lease is free*. Tap **acquire** (or
  **take over** if A is still holding — an override). B becomes the holder
  and the store unlocks.
- Archive a mail on B. Pick A back up: A is now the follower, and B's archive
  has synced to it.

That is the full loop: synced state, a locked follower, an explicit lease
request, and a clean handoff.

## No second device? Two desktop instances

```sh
# terminal 1 — the holder
mise exec -- cargo run -- --db /tmp/superapp-A.db --bucket http://127.0.0.1:9000
# terminal 2 — the follower (locked screen, then acquire)
mise exec -- cargo run -- --db /tmp/superapp-B.db --bucket http://127.0.0.1:9000
```

## The real bucket: R2

Everything above is the same walk against a real endpoint; nothing in the
engine changes. `https://` in the bucket URL selects [`src/r2.rs`] — R2 over
its S3 API — and `http://` keeps the plain daemon client.

Two things a local demo could do without: **TLS**, and **AWS SigV4** request
signing. The compare-and-swap is *not* emulated on top of them: R2 implements
S3's conditional writes, so `If-None-Match: *` is the create-only put and
`If-Match: <etag>` is the compare-and-swap, each answered `412` when its
precondition loses. The single-writer property the lease rests on — the one
[`formal/Lease.tla`](../formal/README.md) checks — therefore rests on the
object store itself.

### 1. The bucket and a key

In the Cloudflare dashboard: **R2 → Create bucket**, then **Manage R2 API
Tokens → Create API token**, *Object Read & Write*, scoped to that bucket.
The token page shows three things worth keeping: the **access key id**, the
**secret access key** (once), and the S3 endpoint,
`https://<ACCOUNT_ID>.r2.cloudflarestorage.com`.

The bucket URL the app wants is that endpoint plus the bucket, and optionally
a prefix — a lineage can live in a subdirectory, so one bucket can hold
several (a demo run, the real one):

```
https://<ACCOUNT_ID>.r2.cloudflarestorage.com/<BUCKET>[/<PREFIX>]
```

### 2. Prove the credentials before trusting them

```sh
export SUPERAPP_R2_ACCESS_KEY_ID=…
export SUPERAPP_R2_SECRET_ACCESS_KEY=…
mise exec -- cargo run --bin sync-demo -- \
  --bucket https://<ACCOUNT_ID>.r2.cloudflarestorage.com/<BUCKET>
```

This runs the whole two-device story — bootstrap, snapshot install, sync both
ways, a follower's write refused, release + acquire, an override that strands
— against the real bucket, under a fresh `sync-demo/<stamp>/` prefix, and
**deletes every object it made** on the way out. It starts with the three
verbs alone (`404 → create → refuse → read → stale CAS refused → fresh CAS
wins`), so a wrong key fails on the first line with `403
SignatureDoesNotMatch` rather than as a puzzling bootstrap four steps later.

### 3. Point the app at it

```sh
mise exec -- cargo run -- --db /tmp/superapp-A.db \
  --bucket https://<ACCOUNT_ID>.r2.cloudflarestorage.com/<BUCKET>/home
```

The access key id and its secret are resolved, in order, from:

1. `SUPERAPP_R2_ACCESS_KEY_ID` / `SUPERAPP_R2_SECRET_ACCESS_KEY`,
2. lines 2 and 3 of the `bucket` file beside the store,
3. the platform's secret store, for the secret half — the macOS login
   keychain, written by:

```sh
mise exec -- cargo run -- --r2-login    # reads the secret from stdin, then exits
```

Stdin, not a flag: an argument is in `ps` and in the shell's history, and this
one key can write the whole lineage. `--r2-login` needs the key id (from the
environment or the file); it stores only the secret. Secrets never go in the
store — same rule as mail passwords ([`src/secret.rs`]).

On android, one file carries all three, pushed into the app's files dir:

```sh
adb shell run-as dev.prepor.superapp sh -c 'cat > files/bucket' <<EOF
https://<ACCOUNT_ID>.r2.cloudflarestorage.com/<BUCKET>/home
<ACCESS_KEY_ID>
<SECRET_ACCESS_KEY>
EOF
```

(Blank lines and `#` comments are skipped, so the file can carry a note.)

### What it costs, and what it says when it fails

An idle follower polls a real bucket every **5 seconds**, not the 1.5 the
local daemon gets — the transport sets its own cadence, because two million
class-B operations a month is a lot to pay for asking a question whose answer
almost never changes. A holder's write still publishes at once: the worker is
kicked, not waited for.

A bucket that refuses us is not the same thing as a dead network, and the app
says which: the S3 error code rides into the status (`bucket GET state: 403
SignatureDoesNotMatch`), onto the locked screen, into a toast, and onto
stderr. Credentials that cannot be found at all refuse to start sync rather
than run a device that only *looks* synced:

```
superapp: device sync is off — no secret for <key id> — run `superapp --r2-login`, …
```

## The headless scripts

The headless makepad loop needs **`--draws N`** to pump N frames — without it a
single frame renders and the script never advances (`--no-draw --draws N` runs
the same walk fast, asserting through label resolution instead of pixels).


`e2e/sync-demo.sh` orchestrates the two device roles headlessly against a
`bucketd` for a scripted walk (`e2e/sync-a.txt`, `e2e/sync-b.txt`). It needs
a headless makepad event loop that pumps frames; where that is available it
drives the passes on the virtual clock exactly like the mail engine.

`e2e/reseed.sh` proves a subtler thing: a *running* follower whose open
compose panel has a peer's draft edit materialized underneath it re-seeds the
retained widget (a compose seeds its fields from the `draft` row only when
its widget is built, so without this it would show a stale buffer no reopen
dislodges). The holder types "alpha", the follower installs it, a helper
(`reseed-edit`) publishes "alpha beta" as the holder while the follower is
live, and the follower's take-over screenshot must read "alpha beta". Ordering
is gated on each device's DB state, not wall-clock.

Build for these with `MAKEPAD=headless` **set at build time** as well as run
time: `build.rs` mirrors it into `cfg(headless)`, which makes the sync passes
run inline on the frame loop's virtual clock (deterministic) instead of on
the production worker thread.

## What is proven without a device

The lease protocol itself is model-checked: [`formal/Lease.tla`](../formal/README.md)
is a TLA+ model of `src/repl.rs` (every read/CAS interleaving, offline
passes, overrides) with the single-writer and history properties checked
exhaustively over a bounded state space — and the one hole it found, a
superseded holder's unpublished writes surfacing under a later lease, is
what `acquire`'s unconditional reset closes.

The mechanism is unit-tested end to end, including over the real HTTP
transport:

- `repl::tests::two_devices_sync_acquire_and_strand` — bootstrap, install,
  publish/materialize both ways, release+acquire handoff, follower read-only,
  and an override that strands the old holder, over an in-memory bucket.
- `repl::tests::two_devices_sync_over_real_http` — the same stack over a live
  socket: snapshot upload/install, batch upload/apply, and the lease CAS
  through `bucketd`'s handler and the `HttpBucket` client.

And the R2 client's own arithmetic, which no local run exercises:

- `r2::tests::the_signature_matches_the_aws_test_vector` — the AWS SigV4 test
  suite's `get-vanilla` case, byte for byte. It pins canonical request, scope,
  signing key and signature at once, so a drift in any of them fails here
  rather than at a real endpoint with a `SignatureDoesNotMatch` and no clue
  which step moved.
- the conditional headers are signed, the path is encoded the way S3 signs it
  (once), the clock formats as `x-amz-date`, and an endpoint URL splits into
  bucket and lineage prefix.

Between the two there is one probe that needs no account at all: point the
client at real AWS S3 with the documentation's example keys.

```sh
SUPERAPP_R2_REGION=us-east-1 \
SUPERAPP_R2_ACCESS_KEY_ID=AKIAIOSFODNN7EXAMPLE \
SUPERAPP_R2_SECRET_ACCESS_KEY=wJalrXUtnFEMI/K7MDENG+bPxRfiCYEXAMPLEKEY \
mise exec -- cargo run --bin sync-demo -- --bucket https://s3.amazonaws.com/any-name
# → bucket GET contract-check: 403 InvalidAccessKeyId
```

`InvalidAccessKeyId` is the *good* answer: it means a real S3 endpoint
completed the TLS handshake, accepted the request framing, and parsed the
`Authorization` header far enough to look the key up — a malformed envelope
would have come back `AuthorizationHeaderMalformed` instead. What is left
after that is whether R2 agrees about the keys and the conditional writes,
which is what `sync-demo --bucket https://…` is for.
