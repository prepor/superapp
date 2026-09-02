#!/bin/bash
# A local two-device sync demo, entirely headless, producing screenshots.
# A bucketd daemon plus two app instances (A then B) sharing it. The headless
# makepad loop needs `--draws N` to pump N frames (without it, a single frame
# renders and the script never advances).
set -e
cd "$(dirname "$0")/.."
PORT=${PORT:-9100}
DIR=/tmp/superapp-sync-demo
OUT=e2e/out
BIN=./target/debug/superapp
# A cap, not a count — the walk ends at its `quit`. It was 260 when this
# script was written; the shell has grown panels and passes since, and a
# budget that runs out shows up as a suite that reported nothing at all.
DRAWS=${DRAWS:-800}
rm -rf "$DIR" /tmp/superapp-syncA.db /tmp/superapp-syncB.db /tmp/frA /tmp/frB
mkdir -p "$DIR" "$OUT" /tmp/frA /tmp/frB

./target/debug/bucketd --dir "$DIR" --port "$PORT" --bind 127.0.0.1 >/tmp/bucketd-demo.log 2>&1 &
BPID=$!
trap 'kill $BPID 2>/dev/null' EXIT
sleep 0.4

run() { # name script db frames
  MAKEPAD_HEADLESS_OUT_DIR="$4" MAKEPAD=headless "$BIN" \
    --e2e "$2" --e2e-out "$OUT" --db "$3" --bucket "http://127.0.0.1:$PORT" --draws "$DRAWS" \
    >/tmp/"$1".log 2>&1 &
  local pid=$!
  for _ in $(seq 1 120); do ps -p $pid >/dev/null 2>&1 || break; sleep 1; done
  ps -p $pid >/dev/null 2>&1 && kill $pid 2>/dev/null
  echo "$1: $(grep 'e2e: done' /tmp/"$1".log | tail -1)"
}

echo "=== device A (bootstrap → hold → seed → archive) ==="
run A e2e/sync-a.txt /tmp/superapp-syncA.db /tmp/frA
echo "=== device B (install → locked → take over → writable) ==="
run B e2e/sync-b.txt /tmp/superapp-syncB.db /tmp/frB
echo "=== screenshots in $OUT: ==="; ls -1 "$OUT" | grep sync- || true
