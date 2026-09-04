#!/bin/bash
# Every suite, in parallel, in the fast `--no-draw` mode.
#
# The binary must be built headless (`MAKEPAD=headless cargo build`):
# build.rs turns that into cfg(headless), which is what gives a run its
# virtual clock and its inline passes. `--no-draw` then runs the full widget
# draw pass — so hit resolution and label matching work exactly as they do
# with pictures — while rasterizing nothing, and `shot` steps are skipped.
# A failure here is a label that did not resolve: a real one.
#
# What a suite needs beyond the defaults it says itself, in two header
# lines of its own file:
#
#   # args: --grid 4x3 --window 380x780
#   # env:  SUPERAPP_SOMETHING=1
set -u
cd "$(dirname "$0")/.." || exit 2

BIN=${BIN:-../target/debug/superapp-next}
DRAWS=${DRAWS:-4000}

if [ ! -x "$BIN" ]; then
  echo "no binary at $BIN — MAKEPAD=headless mise exec -- cargo build -p superapp-next" >&2
  exit 2
fi

LOGS=$(mktemp -d)
trap 'rm -rf "$LOGS"' EXIT

names=()
# The shell's own suites sit in `e2e/`; an app's live in `e2e/<app>/`, and a
# suite is named by the path it is at, so `mail/basic` and a shell suite of
# the same name never collide.
for f in e2e/*.txt e2e/*/*.txt; do
  [ -f "$f" ] || continue
  n=${f#e2e/}
  n=${n%.txt}
  names+=("$n")
  mkdir -p "$LOGS/$(dirname "$n")"
  args=$(sed -n 's/^# args:[[:space:]]*//p' "$f" | head -1)
  envs=$(sed -n 's/^# env:[[:space:]]*//p' "$f" | head -1)
  # shellcheck disable=SC2086 # word-splitting is how the header's args arrive
  env $envs "$BIN" --e2e "$f" --e2e-out e2e/out --no-draw --draws "$DRAWS" $args \
    >"$LOGS/$n.log" 2>&1 &
done

wait

fails=0
for n in "${names[@]}"; do
  done_line=$(grep 'e2e: done' "$LOGS/$n.log" | tail -1)
  if [ -z "$done_line" ]; then
    printf 'FAIL %-16s never reached quit\n' "$n"
    sed -n '$p' "$LOGS/$n.log" | sed 's/^/       /'
    fails=$((fails + 1))
  # The comma and the space matter: "10 failure(s)" ends in "0 failure(s)".
  elif case "$done_line" in *", 0 failure"*) true ;; *) false ;; esac; then
    printf 'ok   %-16s %s\n' "$n" "${done_line#e2e: done — }"
  else
    printf 'FAIL %-16s %s\n' "$n" "${done_line#e2e: done — }"
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
