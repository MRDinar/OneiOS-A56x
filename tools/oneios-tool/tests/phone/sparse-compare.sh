#!/bin/bash
# oneios-tool phone sparse-write on made-up streams: the same bytes and the same length as the stream, and not more
# blocks than a sparse copy of it. Until 2026-11-21 this compared the tool with the script it replaced (the Mac's
# sparsewrite.py of docs/newtodo.md "How to resume" step 2; 35 of 35 checks the same, and on every stream the tool's
# copy held fewer blocks: tools/oneios-tool/README.md "phone"); the script is retired, so each copy is checked
# against the stream itself (which the script's copy had to equal) and against GNU cp --sparse=always's copy.
#
#   tools/oneios-tool/tests/phone/sparse-compare.sh            one line per check (ok | FAILED), then `ALL SAME`
#                                                              (exit 0) or `DIFFERENT: N` (exit 1); 10 s
#   MAP=1 ...                                                  also every file's data regions (SEEK_DATA, SEEK_HOLE)
#   SPEED=4096:280 ...                                         also the time on a stream of 4096 MiB with about
#                                                              280 MiB of data (debug mkstream phone), three ways
#
# The streams (`oneios-tool debug mkstream`): `runs` (zero runs and data runs shorter than 4 KiB, between 4 KiB and 4 MiB, and
# longer, on the edges of blocks and off them), `tail` (ends inside a block), `zero` (only zeros), `empty`,
# `lastbyte` (a data byte at the very end), `dense` (no zero run), `phone` (64 MiB shaped like the Data store).
# Each goes through `phone sparse-write`, and through gzip -1 and `phone sparse-write --gunzip --expect-size`.
# Checked: both copies are the stream (cmp: bytes and length); the tool says the stream's length; the copies do
# not hold more blocks than `cp --sparse=always`'s copy of the stream (st_blocks after an fsync, 1 % and 64 KiB
# allowed for a file system that counts its extent blocks). The line after each stream's checks says what the
# file system holds for each copy (`oneios-tool debug holes`).
#
# SPEED: the stream is written once as a sparse file (the reference of every timed copy's cmp), then timed (wall,
#   MB/s of the stream, CPU of each process): from the generator through gzip -1 (the pipe of the phone:
#   debug mkstream | gzip -1 -c | ...; and that source alone, which bounds it), from the packed stream (the reader
#   alone), and from the sparse file with no gzip. The times mean something only on a machine that does nothing
#   else: the load is printed. (The gzip pipe is not fed from the sparse file: reading 32 GiB of holes fills the
#   page cache with zeros, and the next run pays for it.)
#
# Env: TOOL (the binary), WORK (the scratch folder, default out/sparse-compare of this checkout; it decides the
# file system that is tested), KEEP=1 (keep the scratch folder). Needs GNU coreutils (cp --sparse, stat -f).
set -u
HERE=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
ROOT=$(cd "$HERE/../../../.." && pwd)
TOOL=${TOOL:-$ROOT/tools/oneios-tool/target/release/oneios-tool}
W=${WORK:-$ROOT/out/sparse-compare}
[ -x "$TOOL" ] || { echo "sparse-compare.sh: no $TOOL (cargo build --release --manifest-path tools/oneios-tool/Cargo.toml)" >&2; exit 2; }
FAILED=0
ok() { echo "ok      $1"; }
bad() { echo "FAILED  $1${2:+: $2}"; FAILED=$((FAILED + 1)); }
rm -rf "$W"; mkdir -p "$W"

# held FILE [--map]: "LENGTH ALLOCATED REGIONS DATA" (bytes), then with --map the regions, one per line
held() { "$TOOL" debug holes "$@"; }
show() { # LABEL FILE
  local h; h=$(held "$2" ${MAP:+--map}) || { echo "        $1: ?"; return; }
  set -- "$1" $(printf '%s\n' "$h" | head -n 1) "$(printf '%s\n' "$h" | tail -n +2)"
  echo "        $1: $3 bytes held, $4 data regions of $5 bytes"
  [ -z "$6" ] || printf '%s\n' "$6"
}

# one NAME mkstream-args...
one() {
  local name=$1 s=$W/$1; shift
  "$TOOL" debug mkstream "$@" > "$s.bin" || { bad "$name: debug mkstream"; return; }
  local size; size=$(wc -c < "$s.bin" | tr -d ' ')
  cp --reflink=never --sparse=always "$s.bin" "$s.cp.img" || { bad "$name: cp --sparse=always"; return; }
  local out
  out=$("$TOOL" phone sparse-write "$s.rs.img" < "$s.bin" 2>&1) && cmp "$s.rs.img" "$s.bin" > "$s.cmp" 2>&1 \
    && ok "$name ($size bytes): sparse-write: the stream's bytes and length" || bad "$name: sparse-write" "$out $(cat "$s.cmp" 2>/dev/null)"
  printf '%s\n' "$out" | grep -q "^$s.rs.img: $size bytes, " && ok "$name: sparse-write says $size bytes" || bad "$name: sparse-write's line" "$out"
  out=$(gzip -1 -c "$s.bin" | "$TOOL" phone sparse-write "$s.gz.img" --gunzip --expect-size "$size" 2>&1) && cmp "$s.gz.img" "$s.bin" > "$s.cmp" 2>&1 \
    && ok "$name: gzip -1 | sparse-write --gunzip --expect-size $size: the stream's bytes and length" || bad "$name: sparse-write --gunzip" "$out $(cat "$s.cmp" 2>/dev/null)"
  local cpb rs gz
  cpb=$(held "$s.cp.img" | awk '{print $2}'); rs=$(held "$s.rs.img" | awk '{print $2}'); gz=$(held "$s.gz.img" | awk '{print $2}')
  if [ -n "$cpb" ] && [ -n "$rs" ] && [ "$rs" = "$gz" ] && [ "$rs" -le $((cpb + cpb / 100 + 65536)) ]; then ok "$name: not more blocks than cp --sparse=always's copy (cp $cpb bytes, sparse-write $rs)"
  else bad "$name: blocks" "cp --sparse=always $cpb bytes, sparse-write $rs, through gzip $gz"; fi
  show "cp --sparse  " "$s.cp.img"; show "sparse-write " "$s.rs.img"
  rm -f "$s.bin" "$s.cp.img" "$s.rs.img" "$s.gz.img" "$s.cmp"
}

echo "sparse-compare: $(stat -f -c 'f_frsize %S f_bsize %s' "$W") in $W ($(uname -sm))"
one runs runs
one tail tail
one zero zero
one empty empty
one lastbyte lastbyte
one dense dense
one phone phone 64 6

# ---- the time
# timed LABEL STDIN(- for none) -- argv... [-- argv...]: the pipe's wall time and each process's CPU (bash's `time`
# around each stage)
timed() {
  local label=$1 stdin=$2 i=0 n start end cpu= failed=
  shift 3
  local stages=() cur=()
  for a in "$@"; do
    if [ "$a" = -- ]; then stages+=("$(printf '%q ' "${cur[@]}")"); cur=(); else cur+=("$a"); fi
  done
  stages+=("$(printf '%q ' "${cur[@]}")")
  n=${#stages[@]}
  local pipe=
  for i in "${!stages[@]}"; do
    local st="{ TIMEFORMAT='%U %S'; time ${stages[$i]} 2>/dev/null; } 2> $W/time.$i; echo \$? > $W/status.$i"
    [ "$i" = $((n - 1)) ] && st="{ TIMEFORMAT='%U %S'; time ${stages[$i]} > /dev/null 2>/dev/null; } 2> $W/time.$i; echo \$? > $W/status.$i"
    pipe=${pipe:+$pipe | }"{ $st; }"
  done
  [ "$stdin" = - ] && stdin=/dev/null
  start=$(date +%s.%N)
  bash -c "$pipe" < "$stdin"
  end=$(date +%s.%N)
  for i in "${!stages[@]}"; do
    local name; name=$(eval "set -- ${stages[$i]}"; basename "$1")
    cpu="$cpu${cpu:+, }$name $(awk '{ printf "%.1f", $1 + $2 }' "$W/time.$i") s"
    [ "$(cat "$W/status.$i")" = 0 ] || failed="$failed $name"
  done
  awk -v l="$label" -v s="$SIZE" -v a="$start" -v b="$end" -v c="$cpu" -v f="$failed" \
    'BEGIN { w = b - a; printf "%s: %6.1f s, %5.0f MB/s; CPU: %s%s\n", l, w, s / 1e6 / w, c, (f != "" ? "; FAILED:" f : "") }'
  [ -z "$failed" ]
}
if [ -n "${SPEED:-}" ]; then
  mib=${SPEED%%:*}; data=${SPEED#*:}; [ "$data" != "$SPEED" ] || data=$((mib * 7 / 100))
  echo "--- time: a stream of $mib MiB with about $data MiB of data; load:$(uptime | sed 's/.*load average[s]*://')"
  "$TOOL" debug mkstream phone "$mib" "$data" | "$TOOL" phone sparse-write "$W/speed.img" > /dev/null 2>&1 || bad "speed: the stream"
  SIZE=$(wc -c < "$W/speed.img" | tr -d ' ')
  gen=("$TOOL" debug mkstream phone "$mib" "$data")
  "${gen[@]}" | gzip -1 -c > "$W/speed.gz"
  show "the stream   " "$W/speed.img"
  echo "        packed with gzip -1: $(wc -c < "$W/speed.gz" | tr -d ' ') bytes"
  same() { cmp -s "$2" "$W/speed.img" && ok "speed: $1: the stream's bytes" || bad "speed: $1: not the stream"; show "$1" "$2"; rm -f "$2"; }
  timed "mkstream | gzip -1 -c > /dev/null (the source)     " - -- "${gen[@]}" -- gzip -1 -c || bad "speed: the source"
  timed "mkstream | gzip -1 -c | sparse-write --gunzip        " - -- "${gen[@]}" -- gzip -1 -c -- "$TOOL" phone sparse-write "$W/b.img" --gunzip --expect-size "$SIZE" || bad "speed: sparse-write through gzip"
  same "sparse-write " "$W/b.img"
  timed "sparse-write --gunzip < PACKED                       " "$W/speed.gz" -- "$TOOL" phone sparse-write "$W/d.img" --gunzip --expect-size "$SIZE" || bad "speed: sparse-write from the packed stream"
  same "sparse-write " "$W/d.img"
  timed "sparse-write < FILE                                  " "$W/speed.img" -- "$TOOL" phone sparse-write "$W/f.img" --expect-size "$SIZE" || bad "speed: sparse-write from the file"
  same "sparse-write " "$W/f.img"
fi

[ -n "${KEEP:-}" ] || rm -rf "$W"
[ $FAILED = 0 ] && echo "ALL SAME" || { echo "DIFFERENT: $FAILED"; exit 1; }
