#!/bin/bash
# Headless draft re-seed check. A holder types a compose "alpha"; a *running*
# follower installs it; a peer edits it to "alpha beta"; the follower must
# re-seed its retained compose widget from the materialized row, so on
# take-over it shows "alpha beta" — not the stale "alpha" buffer.
#
# Ordering is gated on each device's store, not on the wall clock, so the two
# independent virtual clocks cannot race.
#
# Build headless first — `MAKEPAD=headless mise exec -- cargo build
# -p superapp` — which runs the sync passes inline on the frame loop
# (deterministic) instead of on the driver thread; without it the take-over
# is asynchronous and the final shot may precede it.
#
# Not in e2e/run-all.sh: it needs three processes and a daemon.
set -e
cd "$(dirname "$0")/../.."
PORT=${PORT:-9300}
DIR=/tmp/superapp-reseed-bucket
OUT=e2e/out
BIN=${BIN:-./target/debug/superapp}
DIRA=/tmp/superapp-reseedA
DIRB=/tmp/superapp-reseedB
DBA="$DIRA/store.db"
DBB="$DIRB/store.db"
rm -rf "$DIR" "$DIRA" "$DIRB" /tmp/frRA /tmp/frRB
mkdir -p "$DIR" "$DIRA" "$DIRB" "$OUT" /tmp/frRA /tmp/frRB

"$(dirname "$BIN")/bucketd" --dir "$DIR" --port "$PORT" --bind 127.0.0.1 \
  >/tmp/superapp-bucketd-reseed.log 2>&1 &
BPID=$!
trap 'kill $BPID 2>/dev/null' EXIT
sleep 0.4

# The draft's body, and its first line — a reply carries the quoted original
# under what was typed, so the marker to watch for is at either end and the
# whole body is far too long to print.
draft() { sqlite3 "$1" 'SELECT body FROM draft ORDER BY panel LIMIT 1' 2>/dev/null; }
head_of() { draft "$1" | head -1; }
# Whether the body starts with `alpha` (A's own text) and, separately,
# whether the peer's ` beta` has reached the end of it.
typed() { case "$(draft "$1")" in alpha*) return 0 ;; *) return 1 ;; esac; }
edited() { case "$(draft "$1")" in *" beta") return 0 ;; *) return 1 ;; esac; }

echo "=== A: hold, seed, compose \"alpha\", publish ==="
MAKEPAD_HEADLESS_OUT_DIR=/tmp/frRA MAKEPAD=headless "$BIN" \
  --e2e e2e/sync/reseed-a.txt --e2e-out "$OUT" --db "$DBA" \
  --bucket "http://127.0.0.1:$PORT" --draws 900 >/tmp/superapp-reseedA.log 2>&1
echo "A: $(grep 'e2e: done' /tmp/superapp-reseedA.log | tail -1)"
echo "A draft: [$(head_of "$DBA")…]"

echo "=== B: follower installs, runs long ==="
MAKEPAD_HEADLESS_OUT_DIR=/tmp/frRB MAKEPAD=headless "$BIN" \
  --e2e e2e/sync/reseed-b.txt --e2e-out "$OUT" --db "$DBB" \
  --bucket "http://127.0.0.1:$PORT" --draws 6000 >/tmp/superapp-reseedB.log 2>&1 &
BAPP=$!

# Gate on B's progress: wait until it installed the compose draft "alpha".
for _ in $(seq 1 300); do
  typed "$DBB" && break
  sleep 0.1
done
echo "B installed draft: [$(head_of "$DBB")…]"

echo "=== peer edits the draft to \"alpha beta\" and publishes ==="
"$(dirname "$BIN")/reseed-edit" "$DBA" "http://127.0.0.1:$PORT"

# Wait until the running follower materialized the edit (its row moved).
for _ in $(seq 1 300); do
  edited "$DBB" && break
  sleep 0.1
done
edited "$DBB" && echo "B materialized the peer's edit" \
              || echo "FAIL: B never materialized the peer's edit"

wait "$BAPP" || true
echo "B: $(grep 'e2e: done' /tmp/superapp-reseedB.log | tail -1)"
echo "B failed steps: $(grep -i 'e2e: FAIL' /tmp/superapp-reseedB.log | tail -3)"
echo "=== shots ==="; ls -1 "$OUT" | grep '^rs-' || true
