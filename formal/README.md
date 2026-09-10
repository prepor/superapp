# Models of device sync

## Current authority and handoff boundaries

`AuthorityHandoff.tla` is a focused abstraction of admission, local service
lifetime, captured database work, publication, and ownership transfer. Reads
and compare-and-swap decisions are separate actions. Publication, acquisition,
and release acknowledgements can be lost. Network failure closes admission,
release intent persists, and a new local generation waits for old work.
Forced takeover and explicit backup-before-recovery are included.

The configured bounds are two devices, three remote epochs, three local
generations, one captured frame per device, and five remote state versions.
TLC 2.19 completed the check on September 10, 2026: **7,159,792 distinct states**,
49,909,840 generated states, complete graph depth 32, with all eight invariants
holding. The run took 7 minutes 17 seconds with two workers.

The invariants check that revoked generations cannot resume, uncertainty and
release intent keep admission closed, release occurs only after queued work,
pending publication and tracked services are drained, lost acknowledgements
do not duplicate publication, and recovery preserves discarded frames in a
backup. Remote ownership identifies at most one canonical publisher.

`AuthorityHandoffUnsafe.cfg` disables the release drain guard as a negative
control. TLC finds a five-state counterexample to `ReleaseWasDrained`: a
service starts, release closes admission, then a release CAS succeeds while
that service still exists. The negative control is expected to fail.

```sh
cd formal
java -XX:+UseParallelGC -Xmx2g -cp ~/.cache/tla/tla2tools.jar tlc2.TLC \
  -workers 2 -metadir ../.context/authority-tlc \
  -config AuthorityHandoff.cfg AuthorityHandoff.tla
java -XX:+UseParallelGC -Xmx512m -cp ~/.cache/tla/tla2tools.jar tlc2.TLC \
  -workers 1 -metadir ../.context/authority-tlc-unsafe \
  -config AuthorityHandoffUnsafe.cfg AuthorityHandoff.tla
```

These are bounded safety checks, not a proof of the Rust implementation or
unbounded executions. One modeled service represents the entire activity
tree, including native cleanup and UI completion. The model does not validate
the real task registry, SQL values and constraints, object encoding, provider
idempotency, operating-system scheduling, or liveness. Those boundaries need
implementation tests and device verification. The `remote` record and exact
CAS guard are assumptions provided by the object-store interface.

## Historical lease model

`Lease.tla` preserves an earlier TLA+ model of the lease protocol: one
`state` object in the bucket advanced only by compare-and-swap on its
version, and devices that poll, publish, acquire, release, and go offline.
Every network round trip is modelled as an atomic **read** (snapshotting the
state and its version) followed by an atomic **decide/CAS** that succeeds only
if the version is unchanged — so every interleaving between one device's read
and another's write is explored.

Each captured local write is a *frame* tagged with the epoch the device
believed it held. That tag lets the model ask what the code cannot: does a
write made under a superseded lease ever enter the canonical history?

### Running the historical model

```sh
# once: a JDK (mise has one) and the TLA+ tools
mise use -g java@21
curl -sSL -o ~/.cache/tla/tla2tools.jar \
  https://github.com/tlaplus/tlaplus/releases/latest/download/tla2tools.jar

cd formal
tlc() { mise exec java@21 -- java -XX:+UseParallelGC -Xmx3g \
          -cp ~/.cache/tla/tla2tools.jar tlc2.TLC -workers auto -deadlock "$@"; }
tlc -config Lease.cfg          Lease.tla   # the properties that must hold (~4 min)
tlc -config LeaseOneWriter.cfg Lease.tla   # expected counterexample (by design)
tlc -config LeaseStaleWrite.cfg Lease.tla  # expected counterexample (the finding)
tlc -config LeaseFixed.cfg     Lease.tla   # the finding, with the proposed fix
```

Bounds (`Lease.cfg`): two devices, four epochs, two writes per device, and a
state constraint on the version counter and history length. That is enough
to reach every role transition in the protocol; the run explores ~50M
distinct states.

### What held in that model

Checked exhaustively within the historical model's bounds:

- **`OneCurrentWriter`** — at most one device is writable *at the bucket's
  current epoch*. The CAS on `state` is what makes this true: two devices can
  never both win an epoch.
- **`LogEpochsMonotone`** — the history is published under non-decreasing
  epochs. A superseded holder's publish CAS loses, so it cannot append.
- **`StrandedIsReadOnly`** — a device the lineage moved past is read-only
  once it has polled. Its local `holding` flag stays set on purpose (that is
  how stranding is detected on the next pass); it does not mean "writable".
- **`MatBounded`** — no device materializes past the head.

### What failed under the previous offline-write policy

- **`OneWriter`** — "at most one joined device writable at all" fails: after
  an override, the old holder keeps writing until its next pass (offline
  writing is a feature). Those writes cannot be published — the CAS fences
  them — but they sit in the old holder's `repl_log`, which is where the next
  finding starts.

### Historical finding: stale writes could be published

**`NoStaleWrite`** fails. Shortest counterexample:

1. `a` bootstraps (epoch 1) and captures a write under epoch 1.
2. `b` takes over (epoch 2).
3. `a` acquires again (epoch 3). Its `materialize` happens not to conflict,
   so its pending epoch-1 frame **survives**.
4. `a` polls and publishes that frame under epoch 3 — after `b`'s tenure.

The implementation examined by this historical model called for stranded recovery to be export-and-reset,
*discarding* local divergent writes. The code only resets when replaying the
peer's frames hits a SQLite row conflict; divergent writes that touch other
rows survive and are published late. Row-level non-conflict is not semantic
non-conflict, so this can land a write made against a state the history has
since moved past. The same survival happens on the poll path when the
overrider has *released*: the superseded device becomes a plain follower and
keeps its stale frames for a later acquire.

**The historical proposal** (modelled by the `Discard` constant): whenever a device adopts a
newer epoch than the one its pending frames were captured under — in
`acquire`, and in the follower branch of `poll` — reset to the baseline and
replay, unconditionally. Another device held the lease in between; the frames
are divergent by definition. With `Discard = TRUE`, `NoStaleWrite` holds and
every property above still holds.

The current implementation does not apply that automatic-discard proposal.
It preserves divergent work and requires explicit recovery after backing up
the database. `AuthorityHandoff.tla` models that preservation boundary.
