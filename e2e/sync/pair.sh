#!/bin/bash
# Two devices pairing on a ticket, as two processes on two stores.
#
# Not in e2e/run-all.sh: every suite there is one process, and this is two
# that have to be alive at the same time — there is no store-and-forward, so
# what one writes reaches the other only while both are up.
#
# Build headless first:
#   MAKEPAD=headless mise exec -- cargo build -p superapp --no-default-features
#
# A opens *device sync*, which makes a ticket; `SUPERAPP_E2E_TICKET_OUT`
# tells the scripted world where to drop it (honoured only under a script).
# This waits for that file, substitutes it into B's walk, and starts B while
# A is still up. `SUPERAPP_SYNC=loopback` binds one socket on 127.0.0.1 with
# no relay and no lookup, so nothing off this machine is dialed.
set -u
cd "$(dirname "$0")/../.." || exit 2

BIN=${BIN:-./target/debug/superapp}
DRAWS=${DRAWS:-4000}
DIR=${DIR:-/tmp/superapp-pair-$$}

if [ ! -x "$BIN" ]; then
  echo "no binary at $BIN — MAKEPAD=headless mise exec -- cargo build -p superapp --no-default-features" >&2
  exit 2
fi
command -v sqlite3 >/dev/null || { echo "sqlite3 is needed for the roster check" >&2; exit 2; }

rm -rf "$DIR"
mkdir -p "$DIR"
trap 'rm -rf "$DIR"' EXIT

# -- A, showing a ticket -------------------------------------------------------

SUPERAPP_SYNC=loopback SUPERAPP_E2E_TICKET_OUT="$DIR/ticket" MAKEPAD=headless \
  "$BIN" --e2e e2e/sync/pair-a.txt --e2e-out e2e/out --db "$DIR/a.db" \
  --no-draw --draws "$DRAWS" >"$DIR/a.log" 2>&1 &
A=$!

for _ in $(seq 1 200); do
  [ -s "$DIR/ticket" ] && break
  ps -p $A >/dev/null 2>&1 || break
  sleep 0.1
done
TICKET=$(cat "$DIR/ticket" 2>/dev/null)
if [ -z "$TICKET" ]; then
  echo "FAIL: device A never showed a ticket"
  sed -n '$p' "$DIR/a.log" | sed 's/^/       /'
  kill $A 2>/dev/null
  exit 1
fi

# -- B, pasting it -------------------------------------------------------------

sed "s|TICKET|$TICKET|" e2e/sync/pair-b.txt >"$DIR/pair-b.txt"
SUPERAPP_SYNC=loopback MAKEPAD=headless \
  "$BIN" --e2e "$DIR/pair-b.txt" --e2e-out e2e/out --db "$DIR/b.db" \
  --no-draw --draws "$DRAWS" >"$DIR/b.log" 2>&1 &
B=$!

wait $A; A_CODE=$?
wait $B; B_CODE=$?

# -- what the two of them made -------------------------------------------------

fails=0
for n in a b; do
  pid_code=$([ "$n" = a ] && echo "$A_CODE" || echo "$B_CODE")
  done_line=$(grep 'e2e: done' "$DIR/$n.log" | tail -1)
  if [ -z "$done_line" ]; then
    printf 'FAIL %-8s never reached quit (exit %s)\n' "$n" "$pid_code"
    tail -3 "$DIR/$n.log" | sed 's/^/       /'
    fails=$((fails + 1))
  elif case "$done_line" in *", 0 failure"*) true ;; *) false ;; esac; then
    printf 'ok   %-8s %s\n' "$n" "${done_line#e2e: done — }"
  else
    printf 'FAIL %-8s %s\n' "$n" "${done_line#e2e: done — }"
    grep 'e2e: FAIL' "$DIR/$n.log" | sed 's/^/       /'
    fails=$((fails + 1))
  fi
done

# The roster is a replicated table, so pairing is visible in both stores:
# each holds its own row and the other's, neither removed.
for n in a b; do
  peers=$(sqlite3 "$DIR/$n.db" 'SELECT count(*) FROM sync_peer WHERE removed = 0' 2>/dev/null)
  if [ "$peers" != "2" ]; then
    printf 'FAIL %-8s sync_peer holds %s device(s), not 2\n' "$n" "${peers:-none}"
    fails=$((fails + 1))
  fi
done
same=$(sqlite3 "$DIR/a.db" 'SELECT group_concat(device) FROM (SELECT device FROM sync_peer ORDER BY device)')
also=$(sqlite3 "$DIR/b.db" 'SELECT group_concat(device) FROM (SELECT device FROM sync_peer ORDER BY device)')
if [ "$same" != "$also" ] || [ -z "$same" ]; then
  echo "FAIL: the two rosters name different devices"
  fails=$((fails + 1))
fi

echo
if [ "$fails" -gt 0 ]; then
  echo "$fails check(s) failed — logs kept in $DIR"
  trap - EXIT
  exit 1
fi
echo "two devices paired, and a note crossed each way"
