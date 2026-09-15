#!/bin/bash
# Build NTgCalls' C API for the phone, once, into a prefix.
#
# A call is tgcalls over WebRTC, and NTgCalls is the library that speaks it.
# On the Mac the `ntgcalls-sys` crate fetches a published archive; for the
# phone nobody publishes one. The AAR on Maven (`io.github.pytgcalls:ntgcalls`)
# carries an arm64 `libntgcalls.so` with the Java binding in it and not one
# `ntg_*` symbol, so the C API the crate binds has to be built from source:
# their own CMake, their prebuilt libwebrtc, the NDK this machine already has.
# What comes out is a prefix beside the TDLib one — `lib/libntgcalls.so` and
# `include/ntgcalls.h` — which `./android.sh` stages into the APK the way it
# stages `libtdjson.so`. It is one long build, and then it is done: a second
# run with the library already there says so and stops.
#
# Their build has only ever run on Linux, and the C binding only for the
# desktops, so five things are put right in the checkout first. All five are
# idempotent and each one is printed as it happens:
#
#   * `cmake/PlatformUtils.cmake` names the target after the *host*. When a
#     toolchain file runs CMake still reports Darwin, so a macOS host would
#     call an android build MACOS and go fetch the macOS libwebrtc. A wrapper
#     toolchain file says what the target is before including theirs, which
#     leaves their file alone.
#   * `targets/c/build.cmake` has no android among its platforms, so it links
#     neither oboe — which is what `MediaDevice::create_audio_device` is on
#     android — nor the platform libraries libwebrtc's android objects call.
#     It gets what `targets/android/build.cmake` gives the JNI binding.
#   * `NTgCalls::enable_glib_loop` is `#ifndef IS_ANDROID` in the C++ and the
#     JNI template skips it; the C template does not, and the binding it
#     generates then names a method that is not there. The same `@skip`.
#   * The compiler is chromium's clang, the one libwebrtc was built with,
#     which `cmake/FindClang.cmake` fetches — and it has to be chromium's:
#     the libc++ this pins says out loud that it wants clang 21 or later, and
#     the NDK's is 19. On a Mac it fetches the Mac build, and that one ships
#     no android compiler-rt, so the NDK's builtins are put where clang looks
#     for them, before anything is linked.
#   * The library reaches for a JavaVM it will never be given. Their android
#     is the AAR's: a `JNI_OnLoad` registers the VM, Java classes of theirs
#     carry the camera and the hardware codecs, and webrtc's own `JNI_OnLoad`
#     is what starts OpenSSL. The C binding has none of that — it defines no
#     `JNI_OnLoad`, and the app that loads it has none of their Java — so the
#     peer connection factory would abort inside `AttachCurrentThreadIfNeeded`
#     before the first call. Under `SUPERAPP_NO_JVM`, which this script puts
#     in the compiler's flags, there is simply no VM: `GetJNIEnv` answers
#     none, the video codec factories are libwebrtc's own software ones, the
#     camera and screen lists are empty, and OpenSSL is started here instead.
#
# The NDK has to be a complete one — `build/cmake/android.toolchain.cmake`
# and `meta/` — and the one `./android.sh sdk` installs is not: cargo-makepad
# keeps `toolchains/llvm` and throws the rest away. Android Studio's SDK has a
# complete one and this finds it; ANDROID_NDK names another.
#
# What comes out is not the whole of what the Mac's library does. The
# microphone and the speaker are native — oboe, through
# NTG_MEDIA_SOURCE_DEVICE, with the two devices `ntg_get_media_devices`
# answers — and the picture is pushed in as external frames, from makepad's
# camera. What is gone with the JavaVM is the camera and the screen as
# *devices* of the library's, and the phone's hardware H.264: the codecs are
# libwebrtc's software ones, which is VP8 and VP9 and whatever else that
# build carries.
#
# See docs/book/src/dev-x.md.
set -euo pipefail

usage() {
  cat <<'EOF'
Usage: build-tools/ntgcalls-android.sh [--force] [-h]

Builds NTgCalls' C API for arm64-v8a into $NTGCALLS_DIR:

  lib/libntgcalls.so   the library, stripped
  include/ntgcalls.h   the C header the `ntgcalls` crate binds
  src/, build/         the checkout and the build, kept for the next run

A run with the library already there does nothing.

Options:
  --force       build it again even so
  -h, --help    this

Read from the environment: NTGCALLS_DIR, ANDROID_SDK, ANDROID_NDK.
EOF
}

# The tag: 3.0.0-rc03 is the version the `ntgcalls-sys` crate on crates.io
# fetches for the desktops, so the phone runs the same library as the Mac.
TAG=v3.0.0-rc03
REPO=https://github.com/pytgcalls/ntgcalls.git
ABI=arm64-v8a
# cargo-makepad's `sdk_version` (`tools/cargo_makepad/src/android/sdk.rs`),
# which is the app's own `minSdkVersion`. Their `version.properties` says 21;
# building for 26 costs nothing and keeps one number in the APK.
API=26
# cargo-makepad's `ndk_version_full`, preferred when there is a choice.
NDK_VERSION=28.2.13676358

force=no
while [ $# -gt 0 ]; do
  case "$1" in
  --force) force=yes ;;
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

PREFIX=${NTGCALLS_DIR:-$HOME/.cache/superapp-ntgcalls-android}
SDK=${ANDROID_SDK:-$HOME/.cache/makepad-android-sdk}
src=$PREFIX/src
build=$PREFIX/build

if [ "$force" = no ] && [ -f "$PREFIX/lib/libntgcalls.so" ] &&
  [ -f "$PREFIX/include/ntgcalls.h" ]; then
  echo "$PREFIX/lib/libntgcalls.so is already built — --force builds it again"
  exit 0
fi

for tool in git cmake ninja python3; do
  command -v "$tool" >/dev/null 2>&1 || {
    echo "no $tool on PATH; this build wants git, cmake, ninja and python3" >&2
    exit 2
  }
done

# The NDK. Their CMake includes the toolchain file and reads `meta/abis.json`,
# neither of which is in the stripped NDK cargo-makepad installs, so what is
# wanted is a complete one — of the right version where there is a choice.
ndk=
for candidate in "${ANDROID_NDK:-}" \
  "$SDK/ndk/$NDK_VERSION" "$SDK"/ndk/* \
  "$HOME/Library/Android/sdk/ndk/$NDK_VERSION" "$HOME/Library/Android/sdk"/ndk/*; do
  if [ -n "$candidate" ] && [ -f "$candidate/build/cmake/android.toolchain.cmake" ]; then
    ndk=$candidate
    break
  fi
done
if [ -z "$ndk" ]; then
  cat >&2 <<EOF
no complete Android NDK

Looked for build/cmake/android.toolchain.cmake under \$ANDROID_NDK,
$SDK/ndk/* and ~/Library/Android/sdk/ndk/*. The NDK
'./android.sh sdk' installs is stripped to toolchains/llvm, which builds Rust
but not CMake projects. Android Studio's SDK Manager installs a complete one
(NDK $NDK_VERSION), or unpack the official archive somewhere and:

  ANDROID_NDK=/path/to/android-ndk-r28 build-tools/ntgcalls-android.sh
EOF
  exit 2
fi
llvm=
for candidate in "$ndk"/toolchains/llvm/prebuilt/*/bin; do
  [ -x "$candidate/llvm-nm" ] && llvm=$candidate && break
done
[ -n "$llvm" ] || {
  echo "no llvm toolchain under $ndk/toolchains/llvm/prebuilt" >&2
  exit 2
}
echo "NDK: $ndk"

# The checkout. `--recursive` because oboe, their android audio, is a
# submodule; `-f` because a second run finds the files below patched.
if [ -d "$src/.git" ]; then
  if [ "$(git -C "$src" describe --tags --exact-match 2>/dev/null || true)" != "$TAG" ]; then
    echo "moving the checkout to $TAG"
    git -C "$src" fetch --depth 1 origin "refs/tags/$TAG:refs/tags/$TAG"
    git -C "$src" checkout -f "$TAG"
    git -C "$src" submodule update --init --depth 1
  fi
else
  echo "cloning ntgcalls $TAG"
  mkdir -p "$PREFIX"
  git clone --recursive --depth 1 --branch "$TAG" "$REPO" "$src"
fi

# `cmake/FindNDK.cmake` includes the toolchain file from deps/ndk/src and
# nowhere else, whatever ANDROID_NDK says, and `cmake/FindWebRTC.cmake` reads
# libwebrtc with the llvm-readelf it finds there. So the NDK goes there.
mkdir -p "$src/deps/ndk"
if [ ! -e "$src/deps/ndk/src" ] || [ "$(readlink "$src/deps/ndk/src")" != "$ndk" ]; then
  rm -rf "$src/deps/ndk/src"
  ln -s "$ndk" "$src/deps/ndk/src"
  echo "linked deps/ndk/src -> $ndk"
fi

# The C target, taught android: oboe and the platform libraries, exactly what
# targets/android/build.cmake gives the JNI binding.
if ! grep -q "superapp: android" "$src/targets/c/build.cmake"; then
  cat >>"$src/targets/c/build.cmake" <<'EOF'

# superapp: android is not one of this target's platforms upstream, so nothing
# here links oboe -- which MediaDevice::create_audio_device is on android --
# or the platform libraries libwebrtc's android objects call. Same as
# targets/android/build.cmake.
if (ANDROID)
    add_subdirectory("${ROOT_DIR}/deps/oboe" "${CMAKE_BINARY_DIR}/deps/oboe")
    setup_platform_flags(oboe OFF)
    target_link_libraries(${NTG_LIB_NAME} PRIVATE oboe android log OpenSLES EGL)
endif ()
EOF
  echo "patched targets/c/build.cmake (oboe and the platform libraries)"
fi

# enable_glib_loop is #ifndef IS_ANDROID in the C++, and only the JNI template
# knows it. Skip it in the C one the same way, in the binding and the header.
skip_glib_loop() {
  local file=$1 anchor=$2
  grep -q enableGlibLoop "$file" && return 0
  awk -v anchor="$anchor" '
    $0 == anchor && !done { print "@skip m.name in enableGlibLoop"; done = 1 }
    { print }
  ' "$file" >"$file.tmp"
  mv "$file.tmp" "$file"
  echo "patched ${file#"$src"/} (skipped enableGlibLoop)"
}
skip_glib_loop "$src/targets/c/ntgcalls_c.cpp.tpl" \
  'extern "C" NTG_C_EXPORT ntg_result ntg_@{m.name|snake}('
skip_glib_loop "$src/targets/c/ntgcalls.h.tpl" \
  'NTG_C_EXPORT ntg_result ntg_@{m.name|snake}('

# The JavaVM that is not there. Three places reach for one, and each is given
# the answer instead of the question; the define is set in the toolchain file
# below, so nothing in the checkout knows about it until this build.
python3 - "$src" <<'PY'
import pathlib
import sys

root = pathlib.Path(sys.argv[1])

patches = [
    # `AttachCurrentThreadIfNeeded` does not answer `no VM` -- it aborts on
    # one. This is the one place that asks, and every other reaches through it.
    (
        "wrtc/src/utils/java_context.cpp",
        """    void* GetJNIEnv() {
#ifdef IS_ANDROID
""",
        """    void* GetJNIEnv() {
// superapp: nothing registers a JavaVM with webrtc in this build -- the C
// binding defines no JNI_OnLoad and the app that loads it carries none of
// this library's Java -- and AttachCurrentThreadIfNeeded aborts rather than
// answer without one. So: no VM, and every caller reads that as none.
#if defined(IS_ANDROID) && !defined(SUPERAPP_NO_JVM)
""",
    ),
    (
        "wrtc/src/interfaces/peer_connection/peer_connection_factory.cpp",
        """#include <wrtc/video_factory/hardware/android/video_factory.hpp>
""",
        """#include <wrtc/video_factory/hardware/android/video_factory.hpp>
#ifdef SUPERAPP_NO_JVM
#include <api/video_codecs/builtin_video_decoder_factory.h>
#include <api/video_codecs/builtin_video_encoder_factory.h>
#endif
""",
    ),
    # The factory is built for every call, audio or video, so this is the
    # reach that would end an audio call before it began.
    (
        "wrtc/src/interfaces/peer_connection/peer_connection_factory.cpp",
        """#ifdef IS_ANDROID
        dependencies.video_encoder_factory = android::create_video_encoder_factory(static_cast<JNIEnv*>(jni_env_));
        dependencies.video_decoder_factory = android::create_video_decoder_factory(static_cast<JNIEnv*>(jni_env_));
#else
""",
        """#if defined(IS_ANDROID) && !defined(SUPERAPP_NO_JVM)
        dependencies.video_encoder_factory = android::create_video_encoder_factory(static_cast<JNIEnv*>(jni_env_));
        dependencies.video_decoder_factory = android::create_video_decoder_factory(static_cast<JNIEnv*>(jni_env_));
#elif defined(IS_ANDROID)
        // superapp: the android factories are org.webrtc's Java ones, wrapped
        // through classes this build has none of. libwebrtc's own software
        // codecs ask for no VM, and an audio call never reaches them at all.
        dependencies.video_encoder_factory = webrtc::CreateBuiltinVideoEncoderFactory();
        dependencies.video_decoder_factory = webrtc::CreateBuiltinVideoDecoderFactory();
#else
""",
    ),
    # webrtc's own JNI_OnLoad is what starts OpenSSL on android, which is why
    # this is skipped there. With no JNI_OnLoad it has to happen here.
    (
        "wrtc/src/interfaces/peer_connection/peer_connection_factory.cpp",
        """#ifndef IS_ANDROID
            webrtc::InitializeSSL();
#endif
""",
        """#if !defined(IS_ANDROID) || defined(SUPERAPP_NO_JVM)
            // superapp: webrtc's JNI_OnLoad does this on android, and there
            // is no JNI_OnLoad in this build.
            webrtc::InitializeSSL();
#endif
""",
    ),
    # MediaDevice asks this before it lists a camera or a screen and before it
    # opens one, so a `no' here is the whole of both.
    (
        "ntgcalls/src/media/devices/java_video_capturer_module.cpp",
        """    bool JavaVideoCapturerModule::is_supported(const bool is_screencast) {
        if (is_screencast) {
            return android_get_device_api_level() >= __ANDROID_API_L__;
        }
        return android_get_device_api_level() >= __ANDROID_API_J_MR2__;
    }
""",
        """    bool JavaVideoCapturerModule::is_supported(const bool is_screencast) {
#ifdef SUPERAPP_NO_JVM
        // superapp: with no VM there is no camera and no screen to list, and
        // none to open either. A call's picture is pushed in as external
        // frames, from the camera makepad already holds open.
        (void) is_screencast;
        return false;
#else
        if (is_screencast) {
            return android_get_device_api_level() >= __ANDROID_API_L__;
        }
        return android_get_device_api_level() >= __ANDROID_API_J_MR2__;
#endif
    }
""",
    ),
]

for name, old, new in patches:
    path = root / name
    text = path.read_text()
    if new in text:
        continue
    if old not in text:
        sys.exit(f"{name}: the text this patches is not there any more")
    path.write_text(text.replace(old, new, 1))
    print(f"patched {name} (no JavaVM)")
PY

# The wrapper toolchain: their PlatformUtils.cmake asks the host what the
# target is, and at this point CMake still answers Darwin. Answer for it.
toolchain=$PREFIX/android-toolchain.cmake
cat >"$toolchain" <<EOF
# Written by build-tools/ntgcalls-android.sh — do not edit.
#
# cmake/PlatformUtils.cmake reads WIN32/UNIX/APPLE to name the target, and a
# toolchain file runs before CMake has looked at any platform module, so on a
# macOS host those are the host's answers and an android build would be called
# MACOS. These two are what the target is.
set(APPLE 0)
set(UNIX 1)
include("$src/cmake/Toolchain.cmake")

# What the patches to the checkout are guarded by. It goes in after their
# toolchain has run because the NDK's own file sets this same plain variable
# and would otherwise write over it; the build's compile lines read it.
string(APPEND CMAKE_CXX_FLAGS " -DSUPERAPP_NO_JVM")
EOF

# The compiler has to arrive before anything is linked, and their FindClang is
# part of the toolchain file — so a project with no languages at all, which
# CMake still hands the toolchain file, fetches it and nothing else. (Doing it
# inside the real configure would mean a second build directory later, and
# their DownloadProject re-fetches all 1.5G the moment it sees a fresh cache.)
if [ ! -d "$src/deps/clang/bin" ]; then
  echo "fetching chromium's clang, the one libwebrtc was built with"
  fetch=$PREFIX/fetch-clang
  rm -rf "$fetch"
  mkdir -p "$fetch"
  printf 'cmake_minimum_required(VERSION 3.27)\nproject(fetch_clang NONE)\n' \
    >"$fetch/CMakeLists.txt"
  cmake -S "$fetch" -B "$fetch/build" \
    -DANDROID_ABI="$ABI" \
    -DANDROID_NDK="$ndk" \
    -DANDROID_NATIVE_API_LEVEL="$API" \
    -DPython_EXECUTABLE="$(command -v python3)" \
    -DCMAKE_TOOLCHAIN_FILE="$toolchain"
  rm -rf "$fetch"
fi

# The Mac build of chromium's clang carries darwin runtimes only. The NDK's
# android builtins go where clang looks, under both the API level asked for
# and the 21 its own try-compiles fall back to.
builtins=
for candidate in "$llvm"/../lib/clang/*/lib/linux/libclang_rt.builtins-aarch64-android.a; do
  [ -f "$candidate" ] && builtins=$candidate && break
done
[ -n "$builtins" ] || {
  echo "no aarch64 compiler-rt builtins under $ndk" >&2
  exit 2
}
for clang_lib in "$src"/deps/clang/lib/clang/*; do
  for level in 21 "$API"; do
    dst=$clang_lib/lib/aarch64-none-linux-android$level
    if [ ! -f "$dst/libclang_rt.builtins.a" ]; then
      mkdir -p "$dst"
      cp "$builtins" "$dst/libclang_rt.builtins.a"
      echo "staged the NDK's compiler-rt for android$level into $(basename "$clang_lib")"
    fi
  done
done

echo "configuring for $ABI, android $API"
cmake -S "$src" -B "$build" -G Ninja \
  -DCMAKE_BUILD_TYPE=Release \
  -DSTATIC_BUILD=OFF \
  -DBINDING=c \
  -DANDROID_ABI="$ABI" \
  -DANDROID_NDK="$ndk" \
  -DANDROID_NATIVE_API_LEVEL="$API" \
  -DPython_EXECUTABLE="$(command -v python3)" \
  -DCMAKE_TOOLCHAIN_FILE="$toolchain"

echo "building"
cmake --build "$build" --config Release \
  -j"$(getconf _NPROCESSORS_ONLN 2>/dev/null || echo 4)"

# Their build.cmake puts the pair under the checkout; the prefix is what the
# APK build reads. Unstripped it is 118M of debug info — the same library.
mkdir -p "$PREFIX/lib" "$PREFIX/include"
cp "$src/shared-output/lib/libntgcalls.so" "$PREFIX/lib/libntgcalls.so"
cp "$src/shared-output/include/ntgcalls.h" "$PREFIX/include/ntgcalls.h"
"$llvm/llvm-strip" --strip-unneeded "$PREFIX/lib/libntgcalls.so"

# The whole point of the exercise, said out loud: the AAR's library gets this
# far too, and has none of these.
exports=$("$llvm/llvm-nm" -D --defined-only "$PREFIX/lib/libntgcalls.so" |
  grep -c ' T ntg_' || true)
if [ "$exports" -lt 1 ]; then
  echo "built, but no ntg_ symbol is exported — that is not the C binding" >&2
  exit 1
fi

# And the fifth fix, said out loud: nothing in here waits for a Java that is
# never coming. (The `Java_J_N_*` boringssl defines are chromium's own JNI
# stubs, which no one calls and which ask for nothing.)
wants_java=$("$llvm/llvm-nm" -D --undefined-only "$PREFIX/lib/libntgcalls.so" |
  grep -ciE 'jni|jvm|_Java' || true)
if [ "$wants_java" -gt 0 ]; then
  echo "built, but it still wants a JavaVM:" >&2
  "$llvm/llvm-nm" -D --undefined-only "$PREFIX/lib/libntgcalls.so" |
    grep -iE 'jni|jvm|_Java' >&2
  exit 1
fi

echo
ls -l "$PREFIX/lib/libntgcalls.so" "$PREFIX/include/ntgcalls.h"
echo "$exports exported ntg_ functions, needing:"
"$llvm/llvm-readelf" -d "$PREFIX/lib/libntgcalls.so" |
  sed -n 's/.*Shared library: \[\(.*\)\]/  \1/p'
