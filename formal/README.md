# A formal model of device sync (CR-005)

`Lease.tla` is a TLA+ model of the lease protocol in `src/repl.rs`: one
`state` object in the bucket advanced only by compare-and-swap on its
version, and devices that poll, publish, acquire, release, and go offline.
Every network round trip is modelled as an atomic **read** (snapshotting the
state and its version) followed by an atomic **decide/CAS** that succeeds only
if the version is unchanged — so every interleaving between one device's read
and another's write is explored.

Each captured local write is a *frame* tagged with the epoch the device
believed it held. That tag lets the model ask what the code cannot: does a
write made under a superseded lease ever enter the canonical history?

## Running it

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

## What holds

Checked exhaustively within the bounds, for the protocol as implemented:

- **`OneCurrentWriter`** — at most one device is writable *at the bucket's
  current epoch*. The CAS on `state` is what makes this true: two devices can
  never both win an epoch.
- **`LogEpochsMonotone`** — the history is published under non-decreasing
  epochs. A superseded holder's publish CAS loses, so it cannot append.
- **`StrandedIsReadOnly`** — a device the lineage moved past is read-only
  once it has polled. Its local `holding` flag stays set on purpose (that is
  how stranding is detected on the next pass); it does not mean "writable".
- **`MatBounded`** — no device materializes past the head.

## What fails, by design

- **`OneWriter`** — "at most one joined device writable at all" fails: after
  an override, the old holder keeps writing until its next pass (offline
  writing is a feature). Those writes cannot be published — the CAS fences
  them — but they sit in the old holder's `repl_log`, which is where the next
  finding starts.

## The finding: stale writes can be published

**`NoStaleWrite`** fails. Shortest counterexample:

1. `a` bootstraps (epoch 1) and captures a write under epoch 1.
2. `b` takes over (epoch 2).
3. `a` acquires again (epoch 3). Its `materialize` happens not to conflict,
   so its pending epoch-1 frame **survives**.
4. `a` polls and publishes that frame under epoch 3 — after `b`'s tenure.

CR-005's design calls for stranded recovery to be export-and-reset,
*discarding* local divergent writes. The code only resets when replaying the
peer's frames hits a SQLite row conflict; divergent writes that touch other
rows survive and are published late. Row-level non-conflict is not semantic
non-conflict, so this can land a write made against a state the history has
since moved past. The same survival happens on the poll path when the
overrider has *released*: the superseded device becomes a plain follower and
keeps its stale frames for a later acquire.

**The fix** (modelled by the `Discard` constant): whenever a device adopts a
newer epoch than the one its pending frames were captured under — in
`acquire`, and in the follower branch of `poll` — reset to the baseline and
replay, unconditionally. Another device held the lease in between; the frames
are divergent by definition. With `Discard = TRUE`, `NoStaleWrite` holds and
every property above still holds.
