#!/bin/bash
# The device-sync form, headless. A device with no flag and no pushed file is
# pointed at a bucket from inside the app, and the proof is outside it: a
# `bucket` file beside the store, and a lineage in the daemon's directory
# that only a holder writes.
#
# The walk itself (e2e/shell-bucket.txt) is in the ordinary battery, where it
# proves the form draws and its verbs resolve. This script is the other half:
# it starts a daemon on the port the walk types, so the connect reaches
# something and the device becomes canonical.
#
# Build headless first: `MAKEPAD=headless mise exec -- cargo build
# -p superapp`.
set -e
cd "$(dirname "$0")/../.."
# The port the walk types. Change both together.
PORT=9299
DIR=/tmp/superapp-bucket-panel
HOME_DIR=/tmp/superapp-bucketpanel
DB="$HOME_DIR/store.db"
OUT=e2e/out
BIN=${BIN:-./target/debug/superapp}
rm -rf "$DIR" "$HOME_DIR" /tmp/frBP
mkdir -p "$DIR" "$HOME_DIR" "$OUT" /tmp/frBP

"$(dirname "$BIN")/bucketd" --dir "$DIR" --port "$PORT" --bind 127.0.0.1 \
  >/tmp/superapp-bucketd-panel.log 2>&1 &
BPID=$!
trap 'kill $BPID 2>/dev/null' EXIT
sleep 0.4

MAKEPAD_HEADLESS_OUT_DIR=/tmp/frBP MAKEPAD=headless "$BIN" \
  --e2e e2e/shell-bucket.txt --e2e-out "$OUT" --db "$DB" \
  --draws "${DRAWS:-1500}" >/tmp/superapp-bucket-panel.log 2>&1 &
pid=$!
for _ in $(seq 1 180); do ps -p $pid >/dev/null 2>&1 || break; sleep 1; done
ps -p $pid >/dev/null 2>&1 && kill "$pid" 2>/dev/null

echo "walk: $(grep 'e2e: done' /tmp/superapp-bucket-panel.log | tail -1)"

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
grep -q ', 0 failure' /tmp/superapp-bucket-panel.log || { echo "FAIL: the walk had failures"; fail=1; }
echo "=== shots ==="; ls -1 "$OUT" | grep '^bp' || true
exit $fail
