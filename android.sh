#!/bin/bash
# Build superapp for the phone and put it there.
#
# One command for what is otherwise four, none of them guessable:
#
#   * `cargo-makepad`, built from the exact makepad revision the root
#     `Cargo.toml` pins. The one on `$PATH` belongs to whatever project
#     installed it last, and the activity's Java is compiled from the tool's
#     own copy — so the wrong tool quietly produces an app that behaves
#     differently on the glass. This reads the pin, so it cannot drift.
#   * Telegram's `libtdjson.so` for arm64, staged into the target directory,
#     because that is where makepad packages shared libraries from.
#     `--no-tdlib` builds without Telegram and needs no prefix at all.
#   * The build, which wants the SDK path, the package name and the label the
#     phone knows this app by.
#   * The install, the start, and the log filtered to what the app says.
#
# The SDK itself is a one-time download, and `./android.sh sdk` is it.
# See docs/book/src/dev-x.md.
set -euo pipefail

usage() {
  cat <<'EOF'
Usage: ./android.sh [options] [build|install|run|logcat|sdk]

  build     the APK, and stop
  install   the APK, installed and started on the phone
  run       install, then the app's log until ^C — which leaves the app
            running, since it only ends the log (the default)
  logcat    the log of whatever is already running, building nothing
  sdk       the Android SDK and the full NDK, downloaded once

Options:
  --release     optimized, and not debuggable: `run-as` backups of the store
                stop working while a release build is the one installed
  --no-tdlib    without Telegram, so no TDLib prefix is needed
  -d SERIAL     which phone, when more than one is plugged in
  -h, --help    this

Read from the environment: ANDROID_SDK, TDLIB_DIR, PACKAGE, LABEL.
EOF
}

cd "$(dirname "$0")"

verb=run
profile=debug
tdlib=yes
device=
build_args=(-p superapp)

while [ $# -gt 0 ]; do
  case "$1" in
  build | install | run | logcat | sdk) verb=$1 ;;
  --release)
    profile=release
    build_args+=(--release)
    ;;
  --no-tdlib)
    tdlib=no
    build_args+=(--no-default-features)
    ;;
  -d | --device)
    shift
    device=${1:-}
    [ -n "$device" ] || {
      echo "-d wants a device serial" >&2
      exit 2
    }
    ;;
  -h | --help)
    usage
    exit 0
    ;;
  *)
    echo "unknown argument: $1" >&2
    usage >&2
    exit 2
    ;;
  esac
  shift
done

SDK=${ANDROID_SDK:-$HOME/.cache/makepad-android-sdk}
PACKAGE=${PACKAGE:-dev.prepor.superapp}
LABEL=${LABEL:-superapp}

# Everything runs under mise where there is one: it supplies the pinned Rust
# toolchain (there is no cargo on `$PATH` without it) and puts `build-tools`
# there too. `env` is the do-nothing stand-in, so the prefix is never empty.
RUN=(env)
command -v mise >/dev/null 2>&1 && RUN=(mise exec --)

# One `adb`, so the device flag is written once. There is no adb on `$PATH`
# here; the SDK's is the one that matches the platform tools that built the
# APK.
adb() {
  if [ -n "$device" ]; then
    "$SDK/platform-tools/adb" -s "$device" "$@"
  else
    "$SDK/platform-tools/adb" "$@"
  fi
}

tool=target/makepad-tool/bin/cargo-makepad

# The tool is built from the same source as the library it will package: the
# revision superapp patches makepad to, read from the manifest so the two can
# never drift. `cargo install` records the source it installed from, revision
# included, so its receipt is the whole of the staleness check — change the
# pin and this builds again, leave it alone and it is a no-op.
ensure_tool() {
  local pin url rev
  pin=$(sed -n 's/^makepad-widgets = { git = "\([^"]*\)", rev = "\([^"]*\)".*/\1 \2/p' Cargo.toml)
  if [ -z "$pin" ]; then
    echo "no makepad pin in Cargo.toml — expected a [patch] line naming a git url and a rev" >&2
    exit 2
  fi
  url=${pin% *}
  rev=${pin#* }
  if ! grep -qs "rev=$rev" target/makepad-tool/.crates.toml; then
    echo "building cargo-makepad from makepad $rev"
    "${RUN[@]}" cargo install --git "$url" --rev "$rev" --locked cargo-makepad \
      --root target/makepad-tool
  fi
}

# The one verb that runs without an SDK, because it is the one that fetches
# it. `--full-ndk` goes after the subcommand, where the toolchain installer
# reads it from; before it, the tool takes the flag for the subcommand.
if [ "$verb" = sdk ]; then
  ensure_tool
  "${RUN[@]}" "$tool" android --sdk-path="$SDK" install-toolchain --full-ndk
  exit 0
fi

if [ ! -x "$SDK/platform-tools/adb" ]; then
  cat >&2 <<EOF
no Android SDK at $SDK

  ./android.sh sdk

It is one download, and the full NDK is what that takes: SQLite and the other
native dependencies are built from source for the phone. Set ANDROID_SDK to
keep it somewhere else.
EOF
  exit 2
fi

# Reading a log builds nothing: it wants the SDK's adb and a running app, and
# neither the tool nor a crate compiled for the phone.
if [ "$verb" != logcat ]; then
  ensure_tool

  if [ "$tdlib" = yes ]; then
    for candidate in "${TDLIB_DIR:-}" "$HOME/.cache/superapp-tdlib-android" \
      "$HOME/conductor/archived-contexts/superapp/deploy-android-sync/tdlib-android"; do
      if [ -n "$candidate" ] && [ -f "$candidate/lib/libtdjson.so" ]; then
        TDLIB_DIR=$candidate
        break
      fi
    done
    if [ -z "${TDLIB_DIR:-}" ] || [ ! -f "$TDLIB_DIR/lib/libtdjson.so" ]; then
      cat >&2 <<EOF
no arm64 TDLib at ${TDLIB_DIR:-any of the usual places}

Telegram needs an Android build of TDLib's JSON interface — the macOS Homebrew
library cannot be used for this target. Point TDLIB_DIR at a prefix holding
lib/libtdjson.so, or build without Telegram:

  TDLIB_DIR=/path/to/tdlib-android ./android.sh
  ./android.sh --no-tdlib
EOF
      exit 2
    fi
    export TDLIB_DIR
    # Makepad packages shared libraries out of the cargo output directory, so
    # the library has to be there before the build, not after it.
    stage=target/android/aarch64-linux-android/$profile
    mkdir -p "$stage"
    cp "$TDLIB_DIR/lib/libtdjson.so" "$stage/"
  fi

  # Its own target directory: the phone's artifacts and the Mac's never share
  # one, and the staging path above is a fact about this one.
  CARGO_TARGET_DIR=$PWD/target/android \
    "${RUN[@]}" "$tool" android \
    --sdk-path="$SDK" \
    --package-name="$PACKAGE" \
    --app-label="$LABEL" \
    build "${build_args[@]}"

  # The tool names the file after the label, snake-cased, and empties the
  # directory each build: whatever aligned APK is in there is this one.
  APK=
  for f in target/android/makepad-android-apk/superapp/apk/*.apk; do
    case "$f" in *.unaligned.apk) continue ;; esac
    [ -f "$f" ] && APK=$f
  done
  if [ -z "$APK" ]; then
    echo "the build reported success but left no APK" >&2
    exit 1
  fi

  if [ "$verb" = build ]; then
    echo "$APK"
    exit 0
  fi
fi

# Which phone. One plugged in needs no saying; several do, and none is worth
# a clearer sentence than adb's own silence.
if [ -z "$device" ]; then
  found=$(adb devices | awk 'NR > 1 && $2 == "device" { print $1 }')
  count=$(printf '%s\n' "$found" | grep -c . || true)
  if [ "$count" -eq 0 ]; then
    echo "no phone: plug one in, allow USB debugging on it, and try again" >&2
    exit 2
  elif [ "$count" -gt 1 ]; then
    echo "more than one phone; name the one you mean with -d:" >&2
    printf '  %s\n' $found >&2
    exit 2
  fi
  device=$found
fi

if [ "$verb" != logcat ]; then
  echo "installing on $device"
  adb install --no-incremental -r "$APK"
  adb shell am start -S -n "$PACKAGE/$PACKAGE.MakepadApp" >/dev/null
fi

if [ "$verb" = install ]; then
  echo "started. Its log: ./android.sh logcat"
  exit 0
fi

# `pidof` is empty for the moment between the start and the process, and the
# log is worth nothing without it.
pid=
for _ in $(seq 1 50); do
  # `pidof` exits 1 until the process is there, which is what is being waited
  # out: without swallowing that, `set -e` ends the wait on its first turn.
  pid=$(adb shell pidof "$PACKAGE" 2>/dev/null | tr -d '\r' || true)
  [ -n "$pid" ] && break
  sleep 0.2
done
if [ -z "$pid" ]; then
  echo "$PACKAGE is not running" >&2
  exit 1
fi
echo "log of $PACKAGE ($pid) — ^C ends the log, not the app"
adb logcat --pid "$pid" Makepad:D '*:S'
