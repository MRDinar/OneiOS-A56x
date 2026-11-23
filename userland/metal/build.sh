#!/bin/bash
# Build the Metal plug-in, NativeAdrenoMetal.bundle, for iOS 27 arm64e on macOS (Xcode) or Linux, for this guest's shared cache.
#
#   scripts/bg.sh metal userland/metal/build.sh            everything (about 25 minutes the first time)
#   userland/metal/build.sh stage|turnip|plugin|finish|addon   one step
#
# What is built (docs/gpu.md section 7, docs/userland.md 7.1):
#   libvulkan_freedreno.dylib   Mesa's Turnip (external/mesa-droidvm cecc0e9 + Droid-VM-UniXNU patches/mesa, the list of
#                               scripts/data/adreno-compiler-inputs.json) with the darwin virtio-gpu backend, a Mach-O dylib
#   libadreno_metal_bridge.a    metal2vulkan (external/metal2vulkan 624bab3 + NativeMetal bridge/patches + Droid-VM-UniXNU
#                               patches/metal2vulkan + this folder's m2v-spare-command-pools.patch) behind NativeMetal's C ABI; Rust, arm64e-apple-ios, std from source
#   NativeAdrenoMetal           external/NativeMetal b0d6a07: the plug-in and the Metal frontend, linked with the bridge
#                               (GpuScanoutPresenter.h includes the wire header of external/NativeMobileDisplay)
#   -> out/userland/NativeAdrenoMetal.bundle (Info.plist, NativeAdrenoMetal, libvulkan_freedreno.dylib, oneios-target)
#      out/userland/NativeAdrenoMetal.gpuaddon: the bundle as the kernel.kc.gpuaddon sidecar (build.sh addon makes it alone)
#      out/userland/NativeAdrenoMetal.build.json: the SDK and stubs used, the pins, sizes and CDHashes
#
# How: the kernel base's own build scripts (Droid-VM-UniXNU scripts/build-turnip-darwin.py, build-adreno-metal-bridge.py,
# build-adreno-metal-plugin.py, --target ios27) run from a staged copy of its tree (out/metal/root), with this folder's
# shim/ in front of PATH: xcrun's clang is scripts/ios/ios-clang (so every link is against the guest's stubs), codesign
# is `oneios-tool ios codesign` over `ios sign` (byte for byte Apple's codesign for a thin file; no bundle seal), strip and nm the linker's
# siblings. On macOS ios-clang is Xcode's clang and ld, and xcrun's other answers are Xcode's; on Linux the cross toolchain.
# The staged copies differ from upstream in the few lines `stage` prints. Before the compile, the plug-in's shared
# cache addresses are replaced with this guest's (hooks/<product>-<build>.json) and two OneiOS files are appended for
# backboardd (drivers/brightness/BacklightMode.inc; DisplayWide.inc: half floats with oneios-display-wide=1); `finish` then marks the bundle with
# `oneios-target`, which is what lets the root tweaks (`oneios-tool image tweaks`) place it.
#
# Needs: scripts/setup/ios-toolchain.sh says "ok"; scripts/setup/get-references.sh (NativeMetal, NativeAdreno,
# NativeMobileDisplay, AdrenoPM4, metal2vulkan, mesa-droidvm, Droid-VM-UniXNU); meson, ninja, glslang, glslc, spirv-val, bison, flex, llvm-as,
# llvm-objcopy, llvm-ar, python3 with mako and yaml (for the base's build scripts and Mesa's meson build: their Python, not
# this project's); rustup with 1.98.0 and its rust-src
# (rustup toolchain install 1.98.0 && rustup +1.98.0 component add rust-src).
# macOS: brew install ninja glslang shaderc spirv-tools bison llvm (LLVM's tools are found in `brew --prefix llvm`/bin,
# Homebrew's bison is put before /usr/bin's 2.3); uv pip install --python "$(command -v python3)" meson mako pyyaml.
set -euo pipefail
ROOT=$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)
HERE=$ROOT/userland/metal
. "$ROOT/scripts/ios/env.sh"
. "$ROOT/scripts/lib/oneios-tool.sh"
oneios_tool "$ROOT"     # (exported: the codesign shim runs it)
TARGET_ID=${ONEIOS_METAL_TARGET:-iPhone18,2 24A446}
HOOKS=$HERE/hooks/${TARGET_ID/ /-}.json
W=${METAL_WORK:-$ROOT/out/metal}
R=$W/root
EXT=$ROOT/external
BASE=$EXT/Droid-VM-UniXNU
OUT=${METAL_OUT:-$ROOT/out/userland}
JOBS=${JOBS:-$(getconf _NPROCESSORS_ONLN)}
export PATH="$HERE/shim:$PATH"
AR=llvm-ar
if [ "$IOS_HOST" = Darwin ]; then
  # Homebrew's LLVM is keg-only: its llvm-ar here, its llvm-as and llvm-objcopy found by the base's scripts themselves
  LLVM_BIN=${LLVM_BIN:-$(brew --prefix llvm 2>/dev/null || echo /opt/homebrew/opt/llvm)/bin}
  if [ -x "$LLVM_BIN/llvm-ar" ]; then AR=$LLVM_BIN/llvm-ar; else AR=$(/usr/bin/xcrun --sdk iphoneos --find ar); fi
fi
# cc-rs (the Rust crates' C and assembly) and rustc's own probes: the same compiler and SDK. On macOS SDKROOT stays
# unset: the crates' build scripts are linked for the Mac with its SDK, and rustc asks xcrun (the shim) for iOS's.
[ "$IOS_HOST" = Darwin ] || export SDKROOT=$IOS_SDK
export CC_arm64e_apple_ios=$IOS_DIR/ios-clang CXX_arm64e_apple_ios=$IOS_DIR/ios-clang++
export AR_arm64e_apple_ios=$AR CARGO_TARGET_ARM64E_APPLE_IOS_LINKER=$IOS_DIR/ios-clang

say() { echo "metal: $*"; }

stage() {
  [ -f "$HOOKS" ] || { echo "metal: no $HOOKS: the plug-in's shared cache addresses are not known for $TARGET_ID" >&2; exit 1; }
  for d in Droid-VM-UniXNU NativeMetal NativeAdreno NativeMobileDisplay AdrenoPM4 metal2vulkan mesa-droidvm; do
    [ -d "$EXT/$d" ] || { echo "metal: no external/$d (scripts/setup/get-references.sh $d)" >&2; exit 1; }
  done
  mkdir -p "$R/drivers" "$R/kits" "$R/.cache/gpu-reference" "$R/.cache/mesa-venv/bin" "$R/.cache/mesa-tools/bin" "$R/build"
  rsync -a --delete "$BASE/scripts/" "$R/scripts/"
  rsync -a --delete "$BASE/patches/" "$R/patches/"
  # This project's extra metal2vulkan patches (userland/metal/patches/metal2vulkan: the Adreno-generic shader
  # translation fixes of hakios-arm64e-dada, rebased on the pin): added after the base's own, numbered 9xxx so that
  # they sort last. The base's build script is assumed to apply patches/metal2vulkan/*.patch in name order;
  # `.github/workflows/gpu-patch-check.yml` checks that they apply to the pin, the build log shows if they did.
  if ls "$HERE"/patches/metal2vulkan/*.patch >/dev/null 2>&1; then
    mkdir -p "$R/patches/metal2vulkan"
    for p in "$HERE"/patches/metal2vulkan/*.patch; do cp "$p" "$R/patches/metal2vulkan/9$(basename "$p")"; done
    say "added $(ls "$HERE"/patches/metal2vulkan/*.patch | wc -l) metal2vulkan patches of OneiOS (9xxx)"
  fi
  # Lasting debug tooling, off by default (each patch only logs when its environment variable is set:
  # ONEIOS_VK_INIT, ONEIOS_SURFACE_TRACE): the trace patches staged beside the base's own, applied by the
  # `ios metal-stage-edit` below. Re-running stage is idempotent: the mesa-work copy is applied only once, after its
  # clone below (a fresh METAL_WORK has no mesa-work yet: applied before the clone, stage stopped with git's status 128).
  cp "$HERE/trace-turnip-init.patch" "$R/patches/mesa/"
  cp "$HERE/trace-metal-vulkan-init.patch" "$R/patches/metal2vulkan/"
  cp "$HERE/trace-metal-surfaces.patch" "$R/patches/"
  # Not a trace: spare command pools follow their recent use (docs/gpu.md, "Who keeps a blob"), applied after the trace
  # patch by `ios metal-stage-edit` (a name of the bridge script's list; the 9xxx copies above are in no list).
  cp "$HERE/m2v-spare-command-pools.patch" "$R/patches/metal2vulkan/"
  rsync -a --delete --exclude .git "$EXT/NativeMetal/" "$R/drivers/AdrenoMetal/"
  rsync -a --delete --exclude .git "$EXT/NativeAdreno/" "$R/drivers/Adreno740/"
  rsync -a --delete --exclude .git "$EXT/NativeMobileDisplay/" "$R/drivers/GopFramebuffer/"
  rsync -a --delete --exclude .git "$EXT/AdrenoPM4/" "$R/kits/AdrenoPM4/"
  ln -sf "$(command -v meson)" "$R/.cache/mesa-venv/bin/meson"
  # macOS's own bison is 2.3: Homebrew's goes first (the base's scripts put .cache/mesa-tools/bin first in PATH)
  if [ "$IOS_HOST" = Darwin ] && [ -x "$(brew --prefix bison 2>/dev/null)/bin/bison" ]; then
    ln -sf "$(brew --prefix bison)/bin/bison" "$R/.cache/mesa-tools/bin/bison"
  fi
  if [ ! -d "$R/.cache/mesa-work/.git" ]; then
    git clone -q "$EXT/mesa-droidvm" "$R/.cache/mesa-work"
    pins=$R/scripts/data/adreno-compiler-inputs.json
    n=$("$ONEIOS_TOOL" json len "$pins" patches)
    i=0
    while [ "$i" -lt "$n" ]; do
      git -C "$R/.cache/mesa-work" apply "$R/$("$ONEIOS_TOOL" json get "$pins" "patches.$i")"
      i=$((i + 1))
    done
    say "Mesa $(git -C "$R/.cache/mesa-work" rev-parse --short HEAD) with the recorded patches in $R/.cache/mesa-work"
  fi
  if ! git -C "$R/.cache/mesa-work" apply --reverse --check "$HERE/trace-turnip-init.patch" 2>/dev/null; then
    git -C "$R/.cache/mesa-work" apply "$HERE/trace-turnip-init.patch"
  fi
  [ -d "$R/.cache/gpu-reference/metal2vulkan/.git" ] || git clone -q "$EXT/metal2vulkan" "$R/.cache/gpu-reference/metal2vulkan"
  # The staged scripts and sources: what differs from upstream, each said.
  # clang 23's -Wunused-but-set-global: Xcode 27.2's clang 21 does not know it (and -Werror makes an unknown
  # warning option an error), so the plug-in script is told about it only when the compiler knows it
  local knows=0
  echo 'int x;' | "$IOS_DIR/ios-clang" -Werror -Wno-error=unused-but-set-global -x c -c - -o /dev/null 2>/dev/null && knows=1
  "$ONEIOS_TOOL" ios metal-stage-edit "$R" "$HOOKS" "$JOBS" "$knows"
  # drivers/brightness/BacklightMode.inc (owner: brightness): backboardd's CoreBrightness gets BLControl, not the
  # VMBLControl that vmm-present selects, and no frame-synchronous brightness (no handler for it here), so the slider
  # reaches the backlight service as brightness-nits. The plug-in is the OneiOS code
  # that runs in backboardd; the file is appended whole, so its constructor is in the plug-in's image.
  cat "$ROOT/drivers/brightness/BacklightMode.inc" >> "$R/drivers/AdrenoMetal/NativeAdrenoMetalPlugin.mm"
  say "staged NativeAdrenoMetalPlugin.mm: drivers/brightness/BacklightMode.inc appended (backboardd: initBLControl: mode 3 -> 1; useSyncBrightnessTransactionForDisplay: NO)"
  # userland/metal/DisplayWide.inc (owner: display): with the kernel's oneios-display-wide=1 (xnuhost's
  # display-format=rgba16f) backboardd's framebuffer is half floats ('RGhA'), which the display path then keeps whole.
  cat "$HERE/DisplayWide.inc" >> "$R/drivers/AdrenoMetal/NativeAdrenoMetalPlugin.mm"
  say "staged NativeAdrenoMetalPlugin.mm: userland/metal/DisplayWide.inc appended (backboardd: CA_FORCE_FP16_FRAMEBUFFER with oneios-display-wide=1)"
  # userland/metal/CommandPoolRow.inc: each client's command pools on its kernel text line every 60 s while they move
  # (oneios-gpu-text: ... CP pools/command buffers cut ... idle ... tuc ...); metal-stage-edit added its calls.
  cat "$HERE/CommandPoolRow.inc" >> "$R/drivers/AdrenoMetal/NativeAdrenoMetalPlugin.mm"
  say "staged NativeAdrenoMetalPlugin.mm: userland/metal/CommandPoolRow.inc appended (the command pool row)"
}

turnip() {
  (cd "$R" && python3 scripts/build-turnip-darwin.py --target ios27)
}

plugin() {
  (cd "$R" && python3 scripts/build-adreno-metal-plugin.py --target ios27)
}

finish() {
  local built=$R/build/adreno-metal-plugin27/NativeAdrenoMetal.bundle bundle=$OUT/NativeAdrenoMetal.bundle
  [ -f "$built/NativeAdrenoMetal" ] && [ -f "$built/libvulkan_freedreno.dylib" ] || { echo "metal: no built bundle in $built" >&2; exit 1; }
  mkdir -p "$OUT"
  rm -rf "$bundle"
  cp -r "$built" "$bundle"
  # The file the root tweaks look for: this bundle's addresses are this guest's.
  echo "$TARGET_ID" > "$bundle/oneios-target"
  "$ONEIOS_TOOL" ios macho-check --local "$bundle/libvulkan_freedreno.dylib" "$bundle/NativeAdrenoMetal" "$bundle/libvulkan_freedreno.dylib" | tee "$OUT/NativeAdrenoMetal.check.txt"
  "$ONEIOS_TOOL" ios metal-record "$ROOT" "$bundle" "$OUT/NativeAdrenoMetal.build.json" "$HOOKS" "$TARGET_ID"
  say "bundle $bundle; record $OUT/NativeAdrenoMetal.build.json"
}

# The bundle as a GPU addon: the sidecar kernel.kc.gpuaddon (format NGPUADD2, the format of Droid-VM-UniXNU's
# make-gpu-addon.py). xnuhost hands it to the kernel, which writes it into /private/var/ngpu before its addon mount
# (0031-gpu-addon-mount.patch), so the Data store survives a rebuilt plug-in. The bundle's CDHashes must be in the
# image's overlay/userland/owner-trust.json all the same; this is for changing the plug-in without a new Data seed.
addon() {
  local bundle=$OUT/NativeAdrenoMetal.bundle
  [ -f "$bundle/NativeAdrenoMetal" ] || { echo "metal: no bundle in $OUT: run the build first" >&2; exit 1; }
  "$ONEIOS_TOOL" ios gpuaddon "$bundle" "$OUT/NativeAdrenoMetal.gpuaddon"
}

say "SDK $IOS_SDK_VERSION ($IOS_SDK); stubs $([ -f "$IOS_TBD/usr/lib/libSystem.B.tbd" ] && echo "$IOS_TBD" || echo "none: the SDK's own"); target $IOS_TARGET"
case "${1:-all}" in
  stage) stage ;;
  turnip) turnip ;;
  plugin) plugin ;;
  finish) finish; addon ;;
  addon) addon ;;
  all) stage; turnip; plugin; finish; addon ;;
  *) sed -n '2,6p' "$0"; exit 2 ;;
esac
