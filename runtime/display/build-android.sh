#!/bin/sh
# The display helper's four phone files from the Rust crate (runtime/display, oneios-display) and the Java class:
#   runtime/display/build-android.sh OUT
#     OUT/bin/oneios-display        the launcher (src/bin/launcher.rs), static
#     OUT/bin/displayctl            the phone test (src/bin/displayctl.rs), static
#     OUT/bin/liboneios-display.so  the JNI library (the lib as a cdylib with the feature jni): NEEDED libandroid,
#                                   libnativewindow, libdl, libm, libc; linked with -z defs and soname liboneios-display.so
#     OUT/lib/oneios-display.dex    java/OneiOSDisplay.java by javac and d8 (min API 30)
# Not stripped (runtime/build.sh strips what it stages). Then runtime/package/elf-check.sh OUT checks the dynamic one.
# Needs the NDK (runtime/ndk-cc: $NDK, else $ANDROID_NDK_HOME, else the SDK's ndk/28.2.13676358), rustup's
# aarch64-linux-android target, a JDK (javac; $JDK/bin first when JDK is set) and d8 from the SDK's build-tools
# ($ANDROID_SDK_ROOT, else $ANDROID_HOME, else $NDK/../..; or d8 on PATH). Target dirs: $CARGO_TARGET_DIR (default
# runtime/target/display) for the library, the same with -static for the two programs.
set -eu
[ $# -eq 1 ] || { echo "usage: runtime/display/build-android.sh OUT" >&2; exit 2; }
mkdir -p "$1/bin" "$1/lib"
OUT=$(cd "$1" && pwd)
HERE=$(cd "$(dirname "$0")" && pwd)
cd "$HERE/.."
T=${CARGO_TARGET_DIR:-target/display}
case "$T" in /*) ;; *) T=$PWD/$T ;; esac
A=aarch64-linux-android

# The link rule of the runtime's Android shared objects: -z defs, so a symbol that no library on the link line has fails
# the link here, not on the phone (liboneios-display.so of 1f7dd1f called libnativewindow's ANativeWindow_getBuffersDataSpace,
# linked against libandroid only, and Android refused it: `dlopen failed: cannot locate symbol`, 2026-11-14).
# runtime/package/elf-check-test.sh checks that every cdylib link of the runtime has it.
CARGO_TARGET_DIR=$T cargo rustc --release --target $A -p oneios-display --lib --features jni --crate-type cdylib -- \
  -C link-arg=-Wl,-z,defs -C link-arg=-Wl,-soname,liboneios-display.so
CARGO_TARGET_DIR=$T-static cargo build --release --target $A -p oneios-display --bins \
  --config "target.$A.rustflags=[\"-C\",\"target-feature=+crt-static\"]"
cp "$T/$A/release/liboneios_display.so" "$OUT/bin/liboneios-display.so"
cp "$T-static/$A/release/oneios-display" "$T-static/$A/release/displayctl" "$OUT/bin/"

# No android.jar: the source reaches the framework by reflection only.
case "$(uname -s)" in Darwin) ndk_default=$HOME/Library/Android/sdk/ndk/28.2.13676358 ;; *) ndk_default=$HOME/Android/Sdk/ndk/28.2.13676358 ;; esac
NDK=${NDK:-${ANDROID_NDK_HOME:-$ndk_default}}
SDK=${ANDROID_SDK_ROOT:-${ANDROID_HOME:-$NDK/../..}}
D8=$(ls "$SDK"/build-tools/*/d8 2>/dev/null | sort | tail -n 1)
[ -n "$D8" ] || D8=$(command -v d8 || true)
[ -n "$D8" ] || { echo "display: no d8 (Android SDK build-tools) under $SDK" >&2; exit 1; }
if [ -n "${JDK:-}" ]; then JAVAC=$JDK/bin/javac; export JAVA_HOME="$JDK" PATH="$JDK/bin:$PATH"; else JAVAC=javac; fi
W=$T/dex-build
rm -rf "$W" && mkdir -p "$W/classes" "$W/dex"
"$JAVAC" --release 11 -Xlint:all -Werror -d "$W/classes" "$HERE/java/OneiOSDisplay.java"
"$D8" --release --min-api 30 --output "$W/dex" "$W/classes/OneiOSDisplay.class"
cp "$W/dex/classes.dex" "$OUT/lib/oneios-display.dex"
chmod 0755 "$OUT/bin/oneios-display" "$OUT/bin/displayctl" "$OUT/bin/liboneios-display.so"
chmod 0644 "$OUT/lib/oneios-display.dex"
echo "display: $OUT/bin/oneios-display $OUT/bin/displayctl $OUT/bin/liboneios-display.so $OUT/lib/oneios-display.dex"
