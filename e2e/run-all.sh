#!/bin/bash
# Every single-process suite, in parallel, in the fast `--no-draw` mode —
# the whole battery in a couple of seconds, and the gate CI runs.
#
# The binary must be built headless (`MAKEPAD=headless cargo build`):
# build.rs turns that into cfg(headless), which is what gives a run its
# virtual clock and its inline pump. `--no-draw` then runs the full widget
# draw pass — so hit resolution and label matching work exactly as they do
# with pictures — while rasterizing nothing, and `shot` steps are skipped.
# A failure here is a label that did not resolve: a real one.
#
# The device-sync suites are not here. They need a second device and a
# bucketd between them; e2e/sync-demo.sh and e2e/reseed.sh drive those.
set -u
cd "$(dirname "$0")/.." || exit 2

BIN=${BIN:-./target/debug/superapp}
DRAWS=${DRAWS:-4000}

if [ ! -x "$BIN" ]; then
  echo "no binary at $BIN — MAKEPAD=headless cargo build" >&2
  exit 2
fi

LOGS=$(mktemp -d)
trap 'rm -rf "$LOGS"' EXIT

# What a suite needs beyond the defaults, from its own header comment.
extra_args() {
  case "$1" in
    phone)    echo "--window 380x780 --grid 4x3" ;;  # the cover display
    send)     echo "--send-delay 1" ;;               # a one-second undo window
    problems) echo "--send-delay 1" ;;               # the same, for its failing send
    library)  echo "--library" ;;                    # the canvas, not a workspace
    *)        echo "" ;;
  esac
}

# A suite that waits out a retry backoff needs a draw budget to match: the
# virtual clock only advances on a draw, so too small a budget leaves it
# short of its own `quit`. Never below what $DRAWS asks for.
suite_draws() {
  case "$1" in
    problems) [ "$DRAWS" -gt 100000 ] && echo "$DRAWS" || echo 100000 ;;
    *)        echo "$DRAWS" ;;
  esac
}

names=()
for f in e2e/*.txt; do
  n=$(basename "$f" .txt)
  case "$n" in sync-a | sync-b | reseed-a | reseed-b) continue ;; esac
  names+=("$n")
  # shellcheck disable=SC2046 # word-splitting is how the extra args arrive
  "$BIN" --e2e "$f" --no-draw --draws "$(suite_draws "$n")" $(extra_args "$n") \
    >"$LOGS/$n.log" 2>&1 &
done

wait

fails=0
for n in "${names[@]}"; do
  done_line=$(grep 'e2e: done' "$LOGS/$n.log" | tail -1)
  if [ -z "$done_line" ]; then
    printf 'FAIL %-14s never reached quit\n' "$n"
    sed -n '$p' "$LOGS/$n.log" | sed 's/^/       /'
    fails=$((fails + 1))
  # The comma and the space matter: "10 failure(s)" ends in "0 failure(s)".
  elif case "$done_line" in *", 0 failure"*) true ;; *) false ;; esac; then
    printf 'ok   %-14s %s\n' "$n" "${done_line#e2e: done — }"
  else
    printf 'FAIL %-14s %s\n' "$n" "${done_line#e2e: done — }"
    grep 'e2e: FAIL' "$LOGS/$n.log" | sed 's/^/       /'
    fails=$((fails + 1))
  fi
done

echo
if [ "$fails" -gt 0 ]; then
  echo "$fails of ${#names[@]} suite(s) failed"
  exit 1
fi
echo "${#names[@]} suites, no failures"
