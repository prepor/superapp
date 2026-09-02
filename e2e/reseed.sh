#!/bin/bash
# CR-005 draft re-seed proof (headless). A holder types a compose "alpha";
# a *running* follower installs it; a peer edits it to "alpha beta"; the
# follower must re-seed its retained compose widget from the materialized
# row, so on take-over it shows "alpha beta" — not the stale "alpha" buffer.
#
# Ordering is gated on each device's DB state, not wall-clock, so the two
# independent virtual clocks cannot race.
#
# Build with `MAKEPAD=headless mise exec -- cargo build` first: build.rs turns
# that into cfg(headless), which runs the sync passes inline on the frame
# loop (deterministic) instead of on the production worker thread — without
# it the take-over is asynchronous and the final shot may precede it.
set -e
cd "$(dirname "$0")/.."
PORT=${PORT:-9300}
DIR=/tmp/superapp-reseed-bucket
OUT=e2e/out
BIN=./target/debug/superapp
DBA=/tmp/superapp-reseedA.db
DBB=/tmp/superapp-reseedB.db
rm -rf "$DIR" "$DBA"* "$DBB"* /tmp/frRA /tmp/frRB
mkdir -p "$DIR" "$OUT" /tmp/frRA /tmp/frRB

./target/debug/bucketd --dir "$DIR" --port "$PORT" --bind 127.0.0.1 >/tmp/bucketd-reseed.log 2>&1 &
BPID=$!
trap 'kill $BPID 2>/dev/null' EXIT
sleep 0.4

echo "=== A: hold, seed, compose \"alpha\", publish ==="
MAKEPAD_HEADLESS_OUT_DIR=/tmp/frRA MAKEPAD=headless "$BIN" \
  --e2e e2e/reseed-a.txt --e2e-out "$OUT" --db "$DBA" --bucket "http://127.0.0.1:$PORT" --draws 600 \
  >/tmp/reseedA.log 2>&1
echo "A: $(grep 'e2e: done' /tmp/reseedA.log | tail -1)"
echo "A draft: [$(sqlite3 "$DBA" 'SELECT body FROM draft' 2>/dev/null)]"

echo "=== B: follower installs, runs long ==="
MAKEPAD_HEADLESS_OUT_DIR=/tmp/frRB MAKEPAD=headless "$BIN" \
  --e2e e2e/reseed-b.txt --e2e-out "$OUT" --db "$DBB" --bucket "http://127.0.0.1:$PORT" --draws 4500 \
  >/tmp/reseedB.log 2>&1 &
BAPP=$!

# Gate on B's progress: wait until it installed the compose draft "alpha".
for _ in $(seq 1 300); do
  [ "$(sqlite3 "$DBB" 'SELECT body FROM draft' 2>/dev/null)" = "alpha" ] && break
  sleep 0.1
done
echo "B installed draft: [$(sqlite3 "$DBB" 'SELECT body FROM draft' 2>/dev/null)]"

echo "=== peer edits the draft to \"alpha beta\" and publishes ==="
./target/debug/reseed-edit "$DBA" "http://127.0.0.1:$PORT"

# Wait until the running follower materialized the edit (its row moved).
for _ in $(seq 1 300); do
  [ "$(sqlite3 "$DBB" 'SELECT body FROM draft' 2>/dev/null)" = "alpha beta" ] && break
  sleep 0.1
done
echo "B materialized draft: [$(sqlite3 "$DBB" 'SELECT body FROM draft' 2>/dev/null)]"

wait "$BAPP" || true
echo "B: $(grep 'e2e: done' /tmp/reseedB.log | tail -1)"
echo "B failed steps: $(grep -i 'e2e:.*fail\|missing label' /tmp/reseedB.log | tail -3)"
echo "=== shots ==="; ls -1 "$OUT" | grep '^rs-' || true
