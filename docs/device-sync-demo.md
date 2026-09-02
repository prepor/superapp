# Device sync: a local demo (CR-005)

Two devices, one store, a leased single writer. This runs the whole thing
locally — a macOS build and an android emulator (or a second desktop
instance) sharing one bucket, with no cloud account.

The transport is `bucketd`, a tiny local object-store daemon that stands in
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
3. a one-line `bucket` file **beside the store** (how android is configured).

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
