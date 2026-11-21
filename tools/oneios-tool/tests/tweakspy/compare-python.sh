#!/bin/bash
# userland/roottweak and userland/image-hook (the frozen Python) against their Rust port, `oneios-tool image tweaks`,
# side by side on the same inputs:
#   python3 -I userland/tweak apply|daemons|cdhash|boot-check ...  ->  oneios-tool image tweaks apply|daemons|cdhash|boot-check ...
#   userland/image-hook (ONEIOS_* of hook API 1)                    ->  oneios-tool image tweaks hook
#
#   tools/oneios-tool/tests/tweakspy/compare-python.sh      one line per case (same | DIFFERENT), then the counts
#
# The inputs are made from nothing by the Python tests' own fixtures (userland/tests/fakeroot.py: a stand-in OS
# root, signed stand-in Mach-Os), plus property lists of every type plistlib writes in the files the tweaks merge
# into (the "odd plists" cases), so no file of Apple's is read. Each side works on its own copy; what is compared:
# the exit status, standard output, the last line of standard error without its tool name, every file of OUT (and
# the hook's owners and info files) by SHA-256, and every path of the trees (type, mode, link target, SHA-256).
# The real 24A446 inputs are compared by scripts/image/build.sh with each hook (README "image tweaks").
# Env: ONEIOS_TOOL (the binary), KEEP=1 (keep the scratch folder), TMPDIR.
set -u
HERE=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
ROOT=$(cd "$HERE/../../../.." && pwd)
TOOL=${ONEIOS_TOOL:-$ROOT/tools/oneios-tool/target/release/oneios-tool}
[ -x "$TOOL" ] || { echo "compare-python: no $TOOL (cargo build --release --manifest-path tools/oneios-tool/Cargo.toml)" >&2; exit 2; }
for f in userland/tweak userland/image-hook userland/roottweak/tweaks.py userland/tests/fakeroot.py; do
  [ -f "$ROOT/$f" ] || { echo "compare-python: $f is not there (retired): cargo test -p oneios-tweaks has the port's tests" >&2; exit 2; }
done
W=$(cd "$(mktemp -d "${TMPDIR:-/tmp}/tweakspy.XXXXXX")" && pwd -P)
[ "${KEEP:-0}" = 1 ] || trap 'rm -rf "$W"' EXIT
sha() { if command -v sha256sum >/dev/null; then sha256sum "$@"; else shasum -a 256 "$@"; fi; }
mode() { stat -c %a "$1" 2>/dev/null || stat -f %Lp "$1"; }
same=0; differ=0
py() { python3 -I -c "import sys; sys.path.insert(0, sys.argv[1]); import fakeroot; $1" "$ROOT/userland/tests" "${@:2}"; }

# listing DIR: every path with its type, mode, link target or SHA-256
listing() {
  ( cd "$1" && find . -mindepth 1 | LC_ALL=C sort | while read -r f; do
      if [ -L "$f" ]; then echo "link $f -> $(readlink "$f")"
      elif [ -d "$f" ]; then echo "dir  $f $(mode "$f")"
      else echo "file $f $(mode "$f") $(sha < "$f" | cut -c1-64)"; fi
    done )
}

# make CASE: the inputs of a case in $W/CASE/in (root, data, helpers, bundle, ...), made once
make_inputs() {
  local in=$W/inputs/$1
  [ -d "$in" ] && return
  mkdir -p "$in/data" "$in/out"
  py "fakeroot.make(sys.argv[2])" "$in/root" >/dev/null
  mkdir -p "$in/helpers"
  py "
import os
for n in ('oneios-init', 'oneios-bootstat', 'oneios-mg', 'oneios-thermal', 'dropbear', 'oneios-seed'):
    d, _ = fakeroot.signed_macho('com.oneios.' + n)
    open(os.path.join(sys.argv[2], n), 'wb').write(d); os.chmod(os.path.join(sys.argv[2], n), 0o755)
open(os.path.join(sys.argv[2], 'README.txt'), 'w').write('not a Mach-O\n')
" "$in/helpers"
  mkdir -p "$in/badhelpers"
  py "
import os
d, _ = fakeroot.signed_macho('com.oneios.oneios-init', arm64e=False, page_shift=14)
open(os.path.join(sys.argv[2], 'oneios-init'), 'wb').write(d)
" "$in/badhelpers"
  mkdir -p "$in/out-userland/NativeAdrenoMetal.bundle/_CodeSignature" "$in/out-userland/camera" "$in/appos/usr/libexec" "$in/appos/.fseventsd"
  py "
import os, plistlib, json
b = sys.argv[2]
plistlib.dump({'CFBundleExecutable': 'NativeAdrenoMetal', 'NSPrincipalClass': 'NativeAdrenoMetalDevice'}, open(b + '/Info.plist', 'wb'))
hashes = []
for n in ('NativeAdrenoMetal', 'libvulkan_freedreno.dylib'):
    d, h = fakeroot.signed_macho('org.example.' + n); hashes.append(h); open(os.path.join(b, n), 'wb').write(d)
open(b + '/_CodeSignature/CodeResources', 'w').write('resources')
open(b + '/oneios-target', 'w').write('iPhone18,2 ' + fakeroot.BUILD + '\n')
json.dump({'target': 'iPhone18,2 ' + fakeroot.BUILD, 'cdhashes': sorted(hashes), 'sdk': {'version': '26.5'}}, open(sys.argv[3], 'w'))
json.dump({'target': 'x', 'cdhashes': sorted(hashes[:1] + ['00' * 20])}, open(sys.argv[4], 'w'))
d, _ = fakeroot.signed_macho('com.oneios.H9ISP'); open(sys.argv[5], 'wb').write(d)
" "$in/out-userland/NativeAdrenoMetal.bundle" "$in/out-userland/NativeAdrenoMetal.build.json" "$in/bad.build.json" "$in/out-userland/camera/H9ISP.mediacapture"
  echo stand-in > "$in/appos/usr/libexec/exampled"; chmod 755 "$in/appos/usr/libexec/exampled"; ln -s libexec/exampled "$in/appos/usr/link"
  mkdir -p "$in/root/private/var/staged_system_apps/Example.app" "$in/root/Applications"
  py "import plistlib; plistlib.dump({'CFBundleIdentifier': 'com.example.app'}, open(sys.argv[2], 'wb'))" "$in/root/private/var/staged_system_apps/Example.app/Info.plist"
}

# odd DATA: property lists of every type plistlib writes, where the tweaks merge (binary and XML, dates to the
# microsecond, data, floats, big and negative integers, text outside ASCII, nested and empty containers)
odd() {
  mkdir -p "$1/mobile/Library/Preferences" "$1/preferences/FeatureFlags/Domain" "$1/db/com.apple.xpc.launchd" "$1/root/Library/Preferences"
  py "
import datetime, plistlib, sys
d = sys.argv[2]
odd = {'Date': datetime.datetime(2026, 11, 21, 10, 20, 30, 123457), 'Far': datetime.datetime(4001, 1, 1), 'Old': datetime.datetime(1601, 3, 4, 5, 6, 7, 999999),
       'Data': bytes(range(256)) * 3, 'Real': 0.1, 'Neg': -0.0, 'Big': 2**64 - 1, 'Small': -2**63, 'Text': 'café ☃ \U0001F600', 'Amp': 'a & <b>',
       'List': [1, 1, True, 1.0, 'x', 'x', b'', [], {}], 'Nested': {'z': {'y': [datetime.datetime(2001, 1, 1)]}}, 'FBLaunchWatchdogScale': 20}
plistlib.dump(odd, open(d + '/mobile/Library/Preferences/com.apple.springboard.plist', 'wb'), fmt=plistlib.FMT_BINARY)
plistlib.dump(odd, open(d + '/mobile/Library/Preferences/.GlobalPreferences.plist', 'wb'), fmt=plistlib.FMT_XML)
plistlib.dump(dict(odd, AppleLocale='en_US'), open(d + '/root/Library/Preferences/com.apple.backboardd.plist', 'wb'), fmt=plistlib.FMT_BINARY)
plistlib.dump({'Somebody': {'Enabled': True, 'When': datetime.datetime(2020, 2, 29, 23, 59, 59)}}, open(d + '/preferences/FeatureFlags/Domain/CoreLocation.plist', 'wb'), fmt=plistlib.FMT_BINARY)
plistlib.dump({'com.example.theirs': True, 'com.example.on': False, 'com.apple.nfcd': 0, 'x': odd}, open(d + '/db/com.apple.xpc.launchd/disabled.plist', 'wb'), fmt=plistlib.FMT_XML)
" "$1"
}

# c NAME INPUTS KIND ARGS...: one case. KIND apply|rw|hook|cmd; `@` in ARGS is the side's own folder.
c() {
  local name=$1 inputs=$2 kind=$3 s a args rc; shift 3
  make_inputs "$inputs"
  for s in py rs; do
    rm -rf "$W/$s"; cp -a "$W/inputs/$inputs" "$W/$s"
    [ -z "${PREP:-}" ] || "$PREP" "$W/$s"
    args=(); for a in "$@"; do args+=("${a//@/$W/$s}"); done
    case "$kind" in
    apply|rw)
      local base=(apply --root "$W/$s/root" --out "$W/$s/out")
      [ "$kind" = rw ] && base+=(--layout rw) || base+=(--data "$W/$s/data")
      if [ $s = py ]; then python3 -I "$ROOT/userland/tweak" "${base[@]}" "${args[@]}"; else "$TOOL" image tweaks "${base[@]}" "${args[@]}"; fi \
        > "$W/$s.out" 2> "$W/$s.err"; rc=$?
      if [ -n "${AGAIN:-}" ] && [ $rc = 0 ]; then
        if [ $s = py ]; then python3 -I "$ROOT/userland/tweak" "${base[@]}" "${args[@]}"; else "$TOOL" image tweaks "${base[@]}" "${args[@]}"; fi \
          > "$W/$s.out" 2> "$W/$s.err"; rc=$?
      fi ;;
    hook)
      mkdir -p "$W/$s/overlay"; : > "$W/$s/owners.tsv"
      ( export ONEIOS_HOOK_API=1 ONEIOS_OS_ROOT="$W/$s/root" ONEIOS_DATA="$W/$s/data" ONEIOS_OVERLAY="$W/$s/overlay" \
          ONEIOS_OWNERS="$W/$s/owners.tsv" ONEIOS_INFO="$W/$s/info.json" ONEIOS_BUILD=24A999 \
          ONEIOS_HELPERS="$W/$s/none" ONEIOS_METAL_PLUGIN="$W/$s/none" ONEIOS_CAMERA_PLUGIN="$W/$s/none"
        for a in "${args[@]}"; do export "${a?}"; done
        if [ $s = py ]; then python3 -I "$ROOT/userland/image-hook"; else "$TOOL" image tweaks hook --config "$ROOT/userland/config"; fi ) \
        > "$W/$s.out" 2> "$W/$s.err"; rc=$?
      # the summary names its tool: the one field that differs by design
      [ ! -f "$W/$s/info.json" ] || sed -i 's#"tool": "[^"]*"#"tool": TOOL#' "$W/$s/info.json" ;;
    cmd)
      if [ $s = py ]; then python3 -I "$ROOT/userland/tweak" "${args[@]}"; else "$TOOL" image tweaks "${args[@]}"; fi > "$W/$s.out" 2> "$W/$s.err"; rc=$? ;;
    esac
    { echo "status $rc"
      tail -n 1 "$W/$s.err" | sed -e "s#$W/$s#DIR#g" -e 's/^roottweak: //' -e 's/^oneios-tool image tweaks: //' -e 's/^image-hook: //'
      listing "$W/$s" | grep -v '^\(dir\|file\|link\) \./\(inputs\)' ; } >> "$W/$s.out"
    sed "s#$W/$s#DIR#g" "$W/$s.out" > "$W/$s.said"
  done
  if cmp -s "$W/py.said" "$W/rs.said"; then same=$((same + 1)); echo "same       $name"
  else differ=$((differ + 1)); echo "DIFFERENT  $name"; diff "$W/py.said" "$W/rs.said" | head -${LINES_SHOWN:-12}; fi
}

H=@/helpers M=@/out-userland/NativeAdrenoMetal.bundle CAM=@/out-userland/camera
c "sealed, no options" base apply
c "sealed, the helpers (a debug shell, a file that is no Mach-O)" base apply --helpers $H
c "sealed, a helper not arm64e with 16 KiB pages" base apply --helpers @/badhelpers
c "sealed, --scale 2 --lighter-visuals --no-awake --plain-keybag" base apply --scale 2 --lighter-visuals --no-awake --plain-keybag
c "sealed, the Metal plug-in, the camera, the App cryptex" base apply --metal-plugin $M --camera-plugin $CAM --appos @/appos
c "sealed, the Metal plug-in for every process" base apply --metal-plugin $M --metal-client '*' --metal-client SpringBoard
c "sealed, groups, keep, disable by hand" base apply --no-group base --group fewest --keep com.apple.momentsd --disable com.apple.CommCenter --disable com.apple.example.unknownd
c "sealed, every preference group off" base apply --no-setup --no-migration --no-watchdogs --no-awake --no-coreanimation
AGAIN=1 c "sealed, a second run over the same trees" base apply --helpers $H --metal-plugin $M
PREP=odd c "sealed, odd plists already in the Data tree" base apply --lighter-visuals
PREP=odd AGAIN=1 c "sealed, odd plists, a second run" base apply
c "rw, the helpers, the plug-in unchecked, a staged app" base rw --helpers $H --metal-plugin $M --metal-plugin-unchecked --camera-plugin $CAM --appos @/appos
AGAIN=1 c "rw, a second run" base rw --helpers $H
c "rw, RecoverOnCrash kept" base rw --keep-recover-on-crash
iv() { py "import plistlib; plistlib.dump({'ProductBuildVersion': '20F66', 'ProductVersion': '16.5', 'ProductName': 'iPhone OS'}, open(sys.argv[2], 'wb'))" "$1/root/System/Library/CoreServices/SystemVersion.plist"; }
PREP=iv c "20F66 with --any-version (xnuhost.conf's 16.5 lines)" base apply --any-version
PREP=iv c "refused: another version without --any-version" base apply
broken() { mkdir -p "$1/data/mobile/Library/Preferences"; echo 'not a plist' > "$1/data/mobile/Library/Preferences/com.apple.purplebuddy.plist"; }
PREP=broken c "refused: a file that is not a property list" base apply
c "refused: no such group" base apply --group nonesuch
noroot() { rm -rf "$1/root/sbin"; }
PREP=noroot c "refused: not an iOS root" base apply
c "refused: a bundle that is no bundle" base apply --metal-plugin @/appos
c "refused: no H9ISP.mediacapture" base apply --camera-plugin @/appos
c "refused: a helper that is not a Mach-O" base apply --helpers @/appos/usr/libexec

c "hook: no inputs, ROOTTWEAK_ARGS and scale" base hook "ONEIOS_ROOTTWEAK_ARGS=--keep com.apple.momentsd --disable 'com.apple.example.unknownd'" ONEIOS_SCALE=2
c "hook: helpers with oneios-seed, the plug-in and its record, the camera" base hook ONEIOS_HELPERS=$H ONEIOS_METAL_PLUGIN=$M ONEIOS_CAMERA_PLUGIN=$CAM
bad_record() { cp "$1/bad.build.json" "$1/out-userland/NativeAdrenoMetal.build.json"; }
PREP=bad_record c "hook: a bundle that is not the one its record describes" base hook ONEIOS_METAL_PLUGIN=$M
missing_bundle() { cp "$1/out-userland/NativeAdrenoMetal.build.json" "$1/out-userland/Missing.build.json"; }
PREP=missing_bundle c "hook: a record beside a bundle that is not there" base hook ONEIOS_METAL_PLUGIN=@/out-userland/Missing.bundle/
c "hook: API 2 is refused" base hook ONEIOS_HOOK_API=2

c "daemons" base cmd daemons
c "daemons --group fewest --no-group base --keep com.apple.weatherd" base cmd daemons --group fewest --no-group base --keep com.apple.weatherd
c "cdhash: signed, not signed" base cmd cdhash @/helpers/oneios-init @/badhelpers/oneios-init @/appos/usr/libexec/exampled
booted() {
  python3 -I "$ROOT/userland/tweak" apply --root "$1/root" --data "$1/data" --out "$1/bout" >/dev/null
  mkdir -p "$1/data/log/com.apple.xpc.launchd"
  printf '%s\n' '2026-11-10 14:11:18.969742 (user/501) <Notice>: pending spawn, domain in on-demand-only mode: com.apple.sleepd' \
    '2026-11-10 14:11:19.407385 (user/501/com.apple.sleepd) <Notice>: service state: spawning' \
    '2026-11-10 14:12:00.000000 (system/com.apple.nfcd) <Notice>: service state: spawning' \
    '2026-11-10 14:12:01.000000 (system/com.apple.OTATaskingAgent [70]) <Notice>: service state: spawning' \
    '2026-11-10 14:11:29.000000 (user/501) <Warning>: failed lookup: name = com.apple.seld, requestor = backboardd[55], error = 3: No such process' \
    > "$1/data/log/com.apple.xpc.launchd/launchd.log"
}
PREP=booted c "boot-check with the OS volume" base cmd boot-check --data @/data --root @/root --ignore-owner
PREP=booted c "boot-check without it, owners checked" base cmd boot-check --data @/data
c "boot-check: no override" base cmd boot-check --data @/data --ignore-owner

echo "$same same, $differ different"
[ "$differ" = 0 ]
