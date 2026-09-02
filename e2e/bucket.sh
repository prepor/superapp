#!/bin/bash
# CR-005: the device-sync form, headless. A device with no flag and no
# adb-pushed file is pointed at a bucket from inside the app, and the proof
# is outside it: a `bucket` file beside the store, and a lineage in the
# daemon's directory that only a holder writes.
#
# Build with `MAKEPAD=headless mise exec -- cargo build` first: build.rs
# turns that into cfg(headless), which runs the sync passes inline on the
# frame loop instead of the production worker thread.
set -e
cd "$(dirname "$0")/.."
PORT=${PORT:-9200}
DIR=/tmp/superapp-bucket-panel
HOME_DIR=/tmp/superapp-bucketpanel
DB="$HOME_DIR/store.db"
OUT=e2e/out
rm -rf "$DIR" "$HOME_DIR" /tmp/frBP
mkdir -p "$DIR" "$HOME_DIR" "$OUT" /tmp/frBP

./target/debug/bucketd --dir "$DIR" --port "$PORT" --bind 127.0.0.1 >/tmp/bucketd-panel.log 2>&1 &
BPID=$!
trap 'kill $BPID 2>/dev/null' EXIT
sleep 0.4

# The walk carries the port the daemon actually got.
sed "s/{{PORT}}/$PORT/" e2e/bucket.txt > /tmp/superapp-bucket-walk.txt

MAKEPAD_HEADLESS_OUT_DIR=/tmp/frBP MAKEPAD=headless ./target/debug/superapp \
  --e2e /tmp/superapp-bucket-walk.txt --e2e-out "$OUT" --db "$DB" \
  --draws "${DRAWS:-900}" >/tmp/bucket-panel.log 2>&1 &
pid=$!
for _ in $(seq 1 180); do ps -p $pid >/dev/null 2>&1 || break; sleep 1; done
ps -p $pid >/dev/null 2>&1 && kill "$pid" 2>/dev/null

echo "walk: $(grep 'e2e: done' /tmp/bucket-panel.log | tail -1)"

fail=0
if [ -f "$HOME_DIR/bucket" ]; then
  echo "bucket file: $(head -1 "$HOME_DIR/bucket")"
  grep -q "127.0.0.1:$PORT" "$HOME_DIR/bucket" || { echo "FAIL: wrong url"; fail=1; }
  [ "$(wc -l < "$HOME_DIR/bucket")" -eq 1 ] || { echo "FAIL: it carries more than the url"; fail=1; }
else
  echo "FAIL: no bucket file beside the store"; fail=1
fi
if [ -f "$DIR/state" ]; then
  echo "lineage: $(head -c 120 "$DIR/state")…"
else
  echo "FAIL: the daemon holds no lineage — the device never became a holder"; fail=1
fi
grep -q 'failure(s)' /tmp/bucket-panel.log && \
  grep -q '0 failure(s)' /tmp/bucket-panel.log || { echo "FAIL: the walk had failures"; fail=1; }
exit $fail
