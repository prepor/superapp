#!/bin/bash
# A local two-device sync demo, entirely headless, producing screenshots.
# A bucketd daemon plus two app instances (A then B) sharing it. The headless
# makepad loop needs `--draws N` to pump N frames (without it, a single frame
# renders and the script never advances).
#
# Build headless first — `MAKEPAD=headless mise exec -- cargo build
# -p superapp` — because that is what gives a run its virtual clock and
# its inline sync passes, so a scripted `wait` advances a handoff.
#
# Not in e2e/run-all.sh: it needs two processes and a daemon.
set -e
cd "$(dirname "$0")/../.."
PORT=${PORT:-9100}
DIR=/tmp/superapp-sync-demo
OUT=e2e/out
BIN=${BIN:-./target/debug/superapp}
# A cap, not a count — each walk ends at its own `quit`.
DRAWS=${DRAWS:-1500}
rm -rf "$DIR" /tmp/superapp-syncA /tmp/superapp-syncB /tmp/frA /tmp/frB
mkdir -p "$DIR" "$OUT" /tmp/frA /tmp/frB

"$(dirname "$BIN")/bucketd" --dir "$DIR" --port "$PORT" --bind 127.0.0.1 \
  >/tmp/superapp-bucketd-demo.log 2>&1 &
BPID=$!
trap 'kill $BPID 2>/dev/null' EXIT
sleep 0.4

run() { # name script db frames
  MAKEPAD_HEADLESS_OUT_DIR="$4" MAKEPAD=headless "$BIN" \
    --e2e "$2" --e2e-out "$OUT" --db "$3" --bucket "http://127.0.0.1:$PORT" --draws "$DRAWS" \
    >/tmp/superapp-"$1".log 2>&1 &
  local pid=$!
  for _ in $(seq 1 120); do ps -p $pid >/dev/null 2>&1 || break; sleep 1; done
  ps -p $pid >/dev/null 2>&1 && kill $pid 2>/dev/null
  echo "$1: $(grep 'e2e: done' /tmp/superapp-"$1".log | tail -1)"
}

echo "=== device A (bootstrap → hold → seed → archive) ==="
run A e2e/sync/sync-a.txt /tmp/superapp-syncA/store.db /tmp/frA
echo "=== device B (install → locked → take over → writable) ==="
run B e2e/sync/sync-b.txt /tmp/superapp-syncB/store.db /tmp/frB
echo "=== the lineage the two of them made ==="
ls -1 "$DIR" 2>/dev/null | sed 's/^/  /'
echo "=== screenshots in $OUT: ==="; ls -1 "$OUT" | grep sync- || true

fail=0
for n in A B; do
  grep -q ', 0 failure' /tmp/superapp-$n.log || { echo "FAIL: device $n had failed steps"; fail=1; }
done
[ -f "$DIR/state" ] || { echo "FAIL: no lineage in the daemon's directory"; fail=1; }
exit $fail
