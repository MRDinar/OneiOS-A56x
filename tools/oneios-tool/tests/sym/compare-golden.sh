#!/bin/bash
# tools/oneios-tool/tests/sym/compare-golden.sh   oneios-tool sym against the frozen output of scripts/debug/dsc.py
# (the Python symbolizer it replaced, deleted on 2026-11-12 after 18 of 18 address sets gave the same bytes on the
# Mac's real logs and on Linux: tools/oneios-tool/README.md "sym").
#
# The address sets come from two stand-in logs that `oneios-tool debug sym-make-log` writes from the dyld shared
# cache of 24A446 (real return addresses, frames drawn at random with a fixed seed: the addresses
# tests/sym/make-log.py drew until 2026-11-21), and `debug sym-cases` makes the sets from them:
#   a   whole backtraces of the logs (oneios-ubt thread lines, sheng-ubt frames), no --slide
#   b   the same with --slide
#   c   addresses outside the cache, with and without --slide
#   d   two addresses: several slides tie (the "NOT reliable" text)
#   e   a large set (4000 addresses of one log), with --slide and without
# golden/24A446.txt holds per set: its name, the SHA-256 of its arguments, the SHA-256 of dsc.py's stdout and its
# exit status (no Apple bytes). A set whose arguments are not the golden's (another cache) fails as "other
# arguments". UPDATE=1 writes the file from this run (a record of what the tool gives, not a comparison).
#
# Needs: ONEIOS_DSC_DIR (the folder with dyld_shared_cache_arm64e, its subcaches and .symbols; from the image:
# oneios-tool image apfs-copy out/image/24A446/systemos.img System/Library/Caches/com.apple.dyld DIR), the tool
# built (or ONEIOS_TOOL=path). Work files: out/rustport-sym/.
set -u
here=$(cd "$(dirname "$0")" && pwd)
root=$(cd "$here/../../../.." && pwd)
tool=${ONEIOS_TOOL:-$root/tools/oneios-tool/target/release/oneios-tool}
work=$root/out/rustport-sym
golden=$here/golden/24A446.txt
: "${ONEIOS_DSC_DIR:?set ONEIOS_DSC_DIR to the folder that holds dyld_shared_cache_arm64e}"
[ -x "$tool" ] || { echo "no tool at $tool: build it or set ONEIOS_TOOL" >&2; exit 2; }
rm -rf "$work"; mkdir -p "$work/logs"
logs=$work/logs
"$tool" debug sym-make-log "$ONEIOS_DSC_DIR" "$logs/standin-a.log" || exit 2
"$tool" debug sym-make-log "$ONEIOS_DSC_DIR" "$logs/standin-b.log" --slide 0x124d8000 --seed 7 --stacks 350 || exit 2

# The address sets: one file per case, the arguments one per line (a: whole backtraces, c: outside the cache,
# d: two that tie, e-large), and the names of the a sets in $work/cases.
"$tool" debug sym-cases "$work" "$logs"/*.log || exit 2

TIMEFORMAT=%R
: > "$work/found.txt"
run() {  # run NAME ARGS...
  local name=$1; shift
  local took
  took=$( { time "$tool" sym "$@" > "$work/$name.out" 2> "$work/$name.err"; echo $? > "$work/$name.status"; } 2>&1 )
  printf '%s %s %s %s\n' "$name" "$(printf '%s\n' "$@" | sha256sum | cut -c1-64)" "$(sha256sum < "$work/$name.out" | cut -c1-64)" "$(cat "$work/$name.status")" >> "$work/found.txt"
  printf 'ran      %-10s %5d arguments  %7d bytes  status %s  %6s s\n' "$name" "$#" "$(wc -c < "$work/$name.out")" "$(cat "$work/$name.status")" "$took"
}
args_of() { tr '\n' ' ' < "$work/$1.args"; }

"$tool" sym index || exit 2
for name in $(cat "$work/cases"); do
  run "$name" $(args_of "$name")
  slide=$(sed -n '1s/^slide \(0x[0-9a-f]*\) .*/\1/p' "$work/$name.out")
  [ -n "$slide" ] && run "b-$name" --slide "$slide" $(args_of "$name")
done
run c1 $(args_of c1)
run c2 $(args_of c2)
run d1 $(args_of d1)
run d2 $(args_of d2)
if [ -f "$work/e-large.args" ]; then
  slide=$(sed -n '1s/^slide \(0x[0-9a-f]*\) .*/\1/p' "$work/a1.out")
  run e1-slide --slide "${slide:-0x0}" $(args_of e-large)
  run e2 $(args_of e-large)
fi
if [ -n "${UPDATE:-}" ]; then mkdir -p "$here/golden"; cp "$work/found.txt" "$golden"; echo "wrote $golden (this run's output, not a comparison)"; exit 0; fi
status=0
while read -r name args out code; do
  want=$(grep "^$name " "$golden") || { echo "NO GOLDEN $name"; status=1; continue; }
  set -- $want
  if [ "$2" != "$args" ]; then echo "OTHER ARGUMENTS $name (not the golden's address set: another cache or log?)"; status=1
  elif [ "$3" != "$out" ] || [ "$4" != "$code" ]; then echo "DIFFERS  $name (stdout or status is not dsc.py's): $work/$name.out"; status=1; fi
done < "$work/found.txt"
[ "$(wc -l < "$work/found.txt")" = "$(wc -l < "$golden")" ] || { echo "the run has $(wc -l < "$work/found.txt") sets, the golden $(wc -l < "$golden")"; status=1; }
[ $status = 0 ] && echo "sym compare: $(wc -l < "$golden") sets the same as dsc.py's frozen output" || echo "sym compare: FAILED"
exit $status
