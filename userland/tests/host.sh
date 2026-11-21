#!/bin/sh
# Host tests of the guest helpers' logic (CI runs every executable */tests/host.sh): userland/helpers/*.c built for
# this machine with cc and -DONEIOS_HOST_TEST (they are plain POSIX C; the guest's Darwin parts, notify, libproc and
# the bootstrap port, are left out of a host build and are not run here). Linux or macOS.
#
#   userland/tests/host.sh            one line per check, then the counts; status 1 when one fails
#
# The cases are the eleven of userland/tests/test_helpers.py (Python, ported 2026-11-21 and deleted): oneios-init's
# services (run once, kept alive with a throttle, a second manager refused, SIGHUP, the bootstrap port, files it
# ignores, a program that is not there), oneios-bootstat's stages, oneios-thermal's level, and oneios-seed's unpack,
# extended attributes and refusals. The seed archives are written by `oneios-tool debug mktar` (the bytes Python's
# tarfile wrote for the old test, checked: tools/oneios-tool/README.md "debug"); the tool is found or built as the
# build scripts do (scripts/lib/oneios-tool.sh; ONEIOS_TOOL=PATH for another). Needs cc; CC=... for another.
# Extended attributes are read back with getfattr (Linux) or xattr (macOS); without either that check says so.
set -u
HERE=$(cd "$(dirname "$0")" && pwd)
ROOT=$(cd "$HERE/../.." && pwd)
HELPERS=$ROOT/userland/helpers
CC=${CC:-cc}
command -v "$CC" >/dev/null 2>&1 || { echo "skip: no C compiler ($CC)"; exit 0; }
# shellcheck source=../../scripts/lib/oneios-tool.sh
. "$ROOT/scripts/lib/oneios-tool.sh"
oneios_tool "$ROOT" >/dev/null || { echo "FAIL   no oneios-tool (debug mktar writes the seed archives)"; exit 1; }
TOOL=$ONEIOS_TOOL

W=$(mktemp -d "${TMPDIR:-/tmp}/oneios-helpers.XXXXXX")
PIDS=
cleanup() {
  for p in $PIDS; do kill -9 "$p" 2>/dev/null; done
  chmod -R u+w "$W" 2>/dev/null; rm -rf "$W"
}
trap cleanup EXIT
trap 'exit 1' INT TERM

pass=0 fail=0
ok() { pass=$((pass + 1)); echo "ok     $1"; }
bad() { fail=$((fail + 1)); echo "FAIL   $1${2:+: $2}"; }
# eq NAME GOT WANT
eq() { if [ "$2" = "$3" ]; then ok "$1"; else bad "$1" "got '$2', want '$3'"; fi; }
# has NAME TEXT WORDS: TEXT holds WORDS
has() { case "$2" in *"$3"*) ok "$1" ;; *) bad "$1" "'$3' not in: $(printf '%s' "$2" | head -c 300)" ;; esac; }

# ---- build
B=$W/build
mkdir -p "$B"
FLAGS="-std=c99 -D_DEFAULT_SOURCE -D_POSIX_C_SOURCE=200809L -DONEIOS_HOST_TEST -O1 -Wall -Wextra -Werror"
[ "$(uname -s)" != Darwin ] || FLAGS="$FLAGS -D_DARWIN_C_SOURCE"   # flock, setgroups, O_NOFOLLOW are BSD's
for name in oneios-init oneios-bootstat oneios-seed oneios-thermal; do
  # shellcheck disable=SC2086
  $CC $FLAGS "$HELPERS/$name.c" -o "$B/$name" || { echo "FAIL   cc $name.c"; exit 1; }
done

# wait_for SECONDS COMMAND...: until COMMAND succeeds (every 0.05 s)
wait_for() {
  wt=$(( $1 * 20 )); shift
  while [ "$wt" -gt 0 ]; do
    "$@" && return 0
    sleep 0.05; wt=$((wt - 1))
  done
  return 1
}
gone() { ! kill -0 "$1" 2>/dev/null; }
# stop PID [SECONDS]: SIGTERM, then its exit status (124 when it did not end in time)
stop() {
  kill -TERM "$1" 2>/dev/null
  if wait_for "${2:-10}" gone "$1"; then wait "$1"; return $?; fi
  kill -9 "$1" 2>/dev/null; wait "$1" 2>/dev/null; return 124
}
# finish PID SECONDS: wait for it to end by itself; its status (124 when it did not)
finish() {
  if wait_for "$2" gone "$1"; then wait "$1"; return $?; fi
  kill -9 "$1" 2>/dev/null; wait "$1" 2>/dev/null; return 124
}

# ---- oneios-init -------------------------------------------------------------------------------------------
fresh() {
  rm -rf "$W/t"; mkdir -p "$W/t"
  T=$W/t; R=$T/oneios
  mkdir -p "$R/services.d" "$R/enabled"
}
# service NAME [-off] [-mode MODE] LINE...
service() {
  n=$1; shift; en=1; mode=644
  while :; do
    case "${1:-}" in -off) en=0; shift ;; -mode) mode=$2; shift 2 ;; *) break ;; esac
  done
  : > "$R/services.d/$n.conf"
  for l in "$@"; do printf '%s\n' "$l" >> "$R/services.d/$n.conf"; done
  chmod "$mode" "$R/services.d/$n.conf"
  [ "$en" = 0 ] || : > "$R/enabled/$n"
}
# init [VAR=VALUE...]: start the manager on R; its pid in IP
init() {
  env "$@" "$B/oneios-init" "$R" 2>/dev/null &
  IP=$!; PIDS="$PIDS $IP"
}
# st NAME FIELD: a field of the service's status line (1 pid, 2 state, 3 starts, 4 last exit)
st() { awk -v n="$1" -v f="$2" '$1 == n { print $(f + 1) }' "$R/state/status" 2>/dev/null; }
logof() { cat "$R/log/$1.log" 2>/dev/null; }
ticks() { logof again | grep -c tick; }

test_run_once_or_kept_alive() {
  fresh
  service once '# a comment' arg=/bin/sh arg=-c 'arg=echo ran $GREETING' env=GREETING=hello bootstrap=0
  service again arg=/bin/sh arg=-c 'arg=echo tick; exit 3' keepalive=1 throttle=1 bootstrap=0
  service off -off arg=/bin/sh arg=-c 'arg=echo never'
  init
  two_ticks() { [ "$(ticks)" -ge 2 ]; }
  wait_for 15 two_ticks && ok "init: a kept-alive service is started again" || bad "init: a kept-alive service is started again" "$(logof oneios-init)"
  eq "init: a service that runs once, with its env" "$(logof once)" "ran hello"
  eq "init: a disabled service does not run" "$(logof off)" ""
  eq "init: status of the service that ran once" "$(st once 2) $(st once 3) $(st once 4)" "done 1 0"
  eq "init: status of the disabled service" "$(st off 2)" disabled
  eq "init: the kept-alive service's last exit" "$(st again 4)" 3
  [ "$(st again 3)" -ge 2 ] 2>/dev/null && ok "init: the kept-alive service's starts ($(st again 3))" || bad "init: the kept-alive service's starts" "$(st again 3)"
  # at least the throttle apart: two starts in under a second would be a spin
  [ "$(ticks)" -le 4 ] && ok "init: the throttle holds ($(ticks) starts)" || bad "init: the throttle" "$(ticks) starts"
  # a second manager on the same root exits
  second=$("$B/oneios-init" "$R" 2>&1 >/dev/null); code=$?
  eq "init: a second manager on the same root exits 1" "$code" 1
  has "init: a second manager says why" "$second" "another oneios-init is running"
  stop "$IP"; eq "init: SIGTERM ends the manager with 0" "$?" 0
  has "init: the manager logs its stop" "$(logof oneios-init)" stopping
}

test_long_running_stopped_and_hup() {
  fresh
  service sleeper arg=/bin/sh arg=-c 'arg=echo up; exec sleep 600' keepalive=1 bootstrap=0
  init
  running() { [ "$(st sleeper 2)" = running ]; }
  wait_for 10 running && ok "init: a long-running service runs" || bad "init: a long-running service runs" "$(cat "$R/state/status" 2>/dev/null)"
  pid=$(st sleeper 1)
  service late arg=/bin/sh arg=-c 'arg=echo late' bootstrap=0
  rm -f "$R/enabled/sleeper"
  kill -HUP "$IP"
  late() { [ "$(logof late)" = late ]; }
  wait_for 10 late && ok "init: SIGHUP starts a new service" || bad "init: SIGHUP starts a new service" "$(logof oneios-init)"
  wait_for 10 gone "$pid" && ok "init: SIGHUP stops a service that was disabled" || { bad "init: the disabled service was not stopped"; kill -9 "$pid" 2>/dev/null; }
  disabled() { [ "$(st sleeper 2)" = disabled ]; }
  wait_for 10 disabled && ok "init: its status says disabled" || bad "init: its status says disabled" "$(st sleeper 2)"
  stop "$IP"; eq "init: SIGTERM ends the manager with 0 (after a reload)" "$?" 0
}

test_bootstrap_port() {
  fresh
  port=$T/port
  service needs arg=/bin/sh arg=-c 'arg=echo with launchd'
  service early arg=/bin/sh arg=-c 'arg=echo before launchd' bootstrap=0
  init "ONEIOS_FAKE_BOOTSTRAP=$port"
  early() { [ "$(logof early)" = "before launchd" ]; }
  wait_for 10 early && ok "init: bootstrap=0 starts before launchd's port" || bad "init: bootstrap=0 starts before launchd's port" "$(logof oneios-init)"
  sleep 1.2
  eq "init: a service waits for the bootstrap port" "$(logof needs)" ""
  eq "init: its status says waiting" "$(st needs 2)" waiting
  : > "$port"
  needs() { [ "$(logof needs)" = "with launchd" ]; }
  wait_for 5 needs && ok "init: it starts once the port is there" || bad "init: it starts once the port is there" "$(logof oneios-init)"
  stop "$IP" >/dev/null
}

test_ignored_service_files() {
  fresh
  service loose -mode 666 arg=/bin/sh arg=-c 'arg=echo loose' bootstrap=0
  service relative arg=sh arg=-c 'arg=echo relative' bootstrap=0
  service junk 'this is not a service'
  service unknown arg=/bin/true colour=blue
  service good arg=/bin/sh arg=-c 'arg=echo good' bootstrap=0
  : > "$R/services.d/not-a-service.txt"
  init
  good() { [ "$(logof good)" = good ]; }
  wait_for 10 good && ok "init: a good service runs beside bad files" || bad "init: a good service runs beside bad files" "$(logof oneios-init)"
  sleep 0.3
  for n in loose relative junk unknown; do
    eq "init: $n.conf is ignored (no log, no status)" "$(logof $n)|$(st $n 2)" "|"
  done
  has "init: a file others can write is named" "$(logof oneios-init)" "loose: services.d/loose.conf must belong to uid"
  stop "$IP" >/dev/null
}

test_missing_program() {
  fresh
  service missing arg=/nonexistent/program bootstrap=0
  init
  e127() { [ "$(st missing 4)" = 127 ]; }
  wait_for 10 e127 && ok "init: a program that is not there ends with 127" || bad "init: a program that is not there ends with 127" "$(cat "$R/state/status" 2>/dev/null)"
  has "init: its log says exec failed" "$(logof missing)" "exec /nonexistent/program"
  stop "$IP" >/dev/null
}

# ---- oneios-bootstat ---------------------------------------------------------------------------------------
# bootstat NAME=VALUE...: run 3 polls with these notification states set (and the earlier calls'); prints "STAGES|LAST" (the stages as the
# Python test read them: the stage word and the words after the time)
bootstat() {
  # the states add up from one call to the next, as in the Python test
  fake=$T/notify; mkdir -p "$fake" "$R/state"
  for s in "$@"; do printf '%s' "${s#*=}" > "$fake/${s%%=*}"; done
  console=$T/console; rm -f "$console"
  out=$(env -i "ONEIOS_FAKE_NOTIFY=$fake" "ONEIOS_ROOT=$R" "ONEIOS_CONSOLE=$console" "$B/oneios-bootstat" -n 3 -i 10) || { echo "status $?"; return; }
  [ "$(cat "$console")" = "$out" ] || { echo "the console is not stdout"; return; }
  stages=$(printf '%s\n' "$out" | awk '{ split($2, a, "="); s = a[2]; for (i = 4; i <= NF; i++) s = s " " $i; printf "%s%s", (NR > 1 ? "," : ""), s }')
  echo "$stages|$(awk '{ print $1; exit }' "$R/state/boot")"
}
test_bootstat() {
  fresh
  eq "bootstat: nothing up" "$(bootstat)" "bootstat-up|bootstat-up"
  eq "bootstat: backboardd" "$(bootstat proc.backboardd=1)" "bootstat-up,backboardd-up|backboardd-up"
  # finishedstartup left over from an earlier SpringBoard counts only once the process is there
  r=$(bootstat com.apple.springboard.finishedstartup=1)
  eq "bootstat: an old finishedstartup without SpringBoard" "${r#*|}" backboardd-up
  eq "bootstat: SpringBoard up and locked" "$(bootstat proc.SpringBoard=1 com.apple.springboard.lockstate=1)" \
    "bootstat-up,backboardd-up,springboard-process,springboard-up,screen locked=1 dark=0|springboard-up"
  "$B/oneios-bootstat" --bogus >/dev/null 2>&1
  eq "bootstat: a wrong option is status 2" "$?" 2
}

# ---- oneios-thermal ----------------------------------------------------------------------------------------
test_thermal() {
  fresh
  fake=$T/thermal; mkdir -p "$fake"
  thermal() { env -i "ONEIOS_FAKE_THERMAL=$fake" "$B/oneios-thermal" -n "$1" -i 0; }
  state() { cat "$fake/state" 2>/dev/null | tr -d '\n'; }
  has "thermal: an older kernel: no-service" "$(thermal 2)" no-service   # said once, nothing set
  eq "thermal: no-service is said once" "$(thermal 2 | grep -c no-service)" 1
  [ ! -e "$fake/state" ] && ok "thermal: nothing set without the service" || bad "thermal: nothing set without the service"
  printf '30 1 4420 7100\n' > "$fake/service"
  has "thermal: the host's level" "$(thermal 1)" "level=30 name=heavy step=1 case=44.2 soc=71.0"
  eq "thermal: the level is set" "$(state)" 30
  # someone else (thermalmonitord) sets nominal while the host says heavy: set back, said once
  rm -f "$fake/state"
  thermal 40 > "$T/th1.out" & p=$!; PIDS="$PIDS $p"
  s30() { [ "$(state)" = 30 ]; }
  wait_for 10 s30 && ok "thermal: set at the start" || bad "thermal: set at the start" "$(state)"
  printf '0\n' > "$fake/state"
  finish "$p" 20; eq "thermal: the run ends" "$?" 0
  eq "thermal: an overwritten state is said once" "$(grep -c 'overwritten state=0' "$T/th1.out")" 1
  eq "thermal: and set back" "$(state)" 30
  # a new level from the host is set and says the old one
  printf '0 0 3500 4000\n' > "$fake/service"
  rm -f "$fake/state"
  thermal 200 > "$T/th2.out" & p=$!; PIDS="$PIDS $p"
  s0() { [ "$(state)" = 0 ]; }
  wait_for 10 s0 && ok "thermal: nominal set" || bad "thermal: nominal set" "$(state)"
  printf '40 2 4650 9000\n' > "$fake/service"
  finish "$p" 20; eq "thermal: the second run ends" "$?" 0
  has "thermal: a new level and the old one" "$(cat "$T/th2.out")" "level=40 name=trapping step=2 case=46.5 soc=90.0 was=0"
  eq "thermal: the new level is set" "$(state)" 40
}

# ---- oneios-seed -------------------------------------------------------------------------------------------
# tar FORMAT < SPEC: T/data.seed (debug mktar: pax or gnu, numeric owners, xattrs as LIBARCHIVE.xattr records)
mktar() { "$TOOL" debug mktar --format "$1" "$T/data.seed"; }
# unpack ARGS...: oneios-seed ARGS T/data.seed T/volume; stdout in UO, stderr in UE, status in UC
unpack() {
  mkdir -p "$T/volume"
  "$B/oneios-seed" "$@" "$T/data.seed" "$T/volume" > "$T/u.out" 2> "$T/u.err"; UC=$?
  UO=$(cat "$T/u.out"); UE=$(cat "$T/u.err")
}
mode() { if [ "$(uname -s)" = Darwin ]; then stat -f %Lp "$1"; else stat -c %a "$1"; fi; }

LONG=containers/a-folder-with-a-long-name/a-folder-with-a-long-name/a-folder-with-a-long-name/a-folder-with-a-long-name/a-folder-with-a-long-name/a-folder-with-a-long-name/file-with-a-name-longer-than-a-ustar-header-holds.plist
seed_spec() {
  echo "dir mobile mode=755 uid=501 gid=501"
  echo "dir mobile/Library mode=700"
  echo "file mobile/Library/a.plist data=$T/plist mode=600 uid=501 gid=501"
  echo "file mobile/empty mode=644"
  echo "file mobile/big data=$T/big"
  echo "file mobile/tool data=$T/tool mode=4755"
  echo "dir tmp mode=1777"
  echo "symlink link target=mobile/Library"
  p=; rest=$LONG
  while [ "${rest#*/}" != "$rest" ]; do
    p=${p:+$p/}${rest%%/*}; rest=${rest#*/}
    echo "dir $p mode=755"
  done
  echo "file $LONG data=$T/deep"
}

test_seed_unpacks() {
  fresh
  printf 'bplist00 stand-in' > "$T/plist"; printf '#!' > "$T/tool"; printf deep > "$T/deep"
  head -c 200000 /dev/urandom > "$T/big"
  seed_spec | mktar pax
  digest=abababababababababababababababababababababababababababababababab
  unpack --hash "$digest"
  eq "seed: unpacks (status)" "$UC" 0
  has "seed: the counts" "$UO" "5 files (200023 bytes), 10 folders, 1 links"
  cmp -s "$T/big" "$T/volume/mobile/big" && ok "seed: a big file's bytes" || bad "seed: a big file's bytes"
  eq "seed: a long name's file" "$(cat "$T/volume/$LONG" 2>/dev/null)" deep
  [ -f "$T/volume/mobile/empty" ] && [ ! -s "$T/volume/mobile/empty" ] && ok "seed: an empty file" || bad "seed: an empty file"
  eq "seed: a symbolic link" "$(readlink "$T/volume/link")" mobile/Library
  eq "seed: the modes (700 600 1777 4755)" "$(mode "$T/volume/mobile/Library") $(mode "$T/volume/mobile/Library/a.plist") $(mode "$T/volume/tmp") $(mode "$T/volume/mobile/tool")" "700 600 1777 4755"
  eq "seed: the mark of the seed" "$(cat "$T/volume/.oneios-seed")" "$digest"
  # the same seed again: left alone; another seed: refused, nothing touched
  rm -f "$T/volume/mobile/empty"
  unpack --hash "$digest"
  eq "seed: the same seed again is left alone" "$UC $([ -e "$T/volume/mobile/empty" ] && echo there || echo absent)" "0 absent"
  unpack --hash cdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcd
  eq "seed: another seed on the volume is refused (3)" "$UC" 3
  unpack --hash XYZ
  eq "seed: a bad --hash is status 2" "$UC" 2
  # GNU long names read as well
  rm -rf "$T/volume"
  seed_spec | mktar gnu
  unpack
  eq "seed: a GNU archive unpacks" "$UC" 0
  [ -f "$T/volume/$LONG" ] && ok "seed: a GNU long name" || bad "seed: a GNU long name" "$UE"
  [ ! -e "$T/volume/.oneios-seed" ] && ok "seed: no mark without --hash" || bad "seed: no mark without --hash"
}

test_seed_xattrs() {
  fresh
  printf x > "$T/x"
  # base64 of the bytes 0 to 255
  b64=AAECAwQFBgcICQoLDA0ODxAREhMUFRYXGBkaGxwdHh8gISIjJCUmJygpKissLS4vMDEyMzQ1Njc4OTo7PD0+P0BBQkNERUZHSElKS0xNTk9QUVJTVFVWV1hZWltcXV5fYGFiY2RlZmdoaWprbG1ub3BxcnN0dXZ3eHl6e3x9fn+AgYKDhIWGh4iJiouMjY6PkJGSk5SVlpeYmZqbnJ2en6ChoqOkpaanqKmqq6ytrq+wsbKztLW2t7i5uru8vb6/wMHCw8TFxsfIycrLzM3Oz9DR0tPU1dbX2Nna29zd3t/g4eLj5OXm5+jp6uvs7e7v8PHy8/T19vf4+fr7/P3+/w==
  echo "file f data=$T/x pax=LIBARCHIVE.xattr.user.oneios.test=$b64" | mktar pax
  unpack
  eq "seed: with an extended attribute (status)" "$UC" 0
  want=$(awk 'BEGIN { for (i = 0; i < 256; i++) printf "%02x", i }')
  have=
  if command -v getfattr >/dev/null 2>&1; then
    have=$(getfattr --only-values -n user.oneios.test "$T/volume/f" 2>/dev/null | od -An -v -tx1 | tr -d ' \n')
  elif command -v xattr >/dev/null 2>&1; then
    have=$(xattr -px user.oneios.test "$T/volume/f" 2>/dev/null | tr -d ' \n' | tr 'A-F' 'a-f')
  else
    has "seed: the attribute is counted (no getfattr or xattr here to read it back)" "$UO" "1 extended attributes"
    return
  fi
  if [ -z "$have" ]; then
    # a file system without user xattrs: said, not hidden
    has "seed: an attribute this file system cannot hold is said" "$UO" "1 extended attributes NOT set"
  else
    eq "seed: the attribute's 256 bytes" "$have" "$want"
    has "seed: the attribute is counted" "$UO" "1 extended attributes"
  fi
}

test_seed_refuses() {
  fresh
  printf x > "$T/x"
  outside=$T/outside; mkdir -p "$outside"
  n=0
  while IFS='|' read -r spec words; do
    n=$((n + 1))
    rm -rf "$T/volume"
    printf '%s\n' "$spec" | tr ';' '\n' | sed "s|@OUT|$outside|; s|@X|$T/x|g" | mktar pax
    unpack --hash abababababababababababababababababababababababababababababababab
    eq "seed: refused ($words): status 1" "$UC" 1
    has "seed: refused ($words): the message" "$UE" "$words"
    [ -z "$(ls -A "$outside")" ] && [ ! -e "$T/escape" ] && ok "seed: refused ($words): nothing outside" || bad "seed: refused ($words): something was written outside"
    [ ! -e "$T/volume/.oneios-seed" ] && ok "seed: refused ($words): not marked done" || bad "seed: refused ($words): a failed unpack is marked done"
  done <<'EOF'
file ../escape data=@X|..
file a/../../escape data=@X|..
file /abs data=@X|absolute
symlink out target=@OUT;file out/escape data=@X|through a symbolic link
file a data=@X;hard b target=a|not a file, folder or symbolic link
EOF
}

test_seed_damaged() {
  fresh
  head -c 5000 /dev/zero | tr '\0' x > "$T/x5000"; printf y > "$T/y"
  printf 'file a data=%s\nfile b data=%s\n' "$T/x5000" "$T/y" | mktar pax
  head -c 3000 "$T/data.seed" > "$T/cut"; mv "$T/cut" "$T/data.seed"
  unpack
  eq "seed: a cut archive is refused" "$UC" 1
  has "seed: a cut archive: the message" "$UE" "ends inside an entry"
  head -c 2048 /dev/zero | tr '\0' '\001' > "$T/data.seed"
  unpack
  eq "seed: a damaged header is refused" "$UC" 1
  case "$UE" in "oneios-seed: a header"*) ok "seed: a damaged header: the message" ;; *) bad "seed: a damaged header: the message" "$UE" ;; esac
  head -c 4096 /dev/zero > "$T/data.seed"
  unpack
  eq "seed: an archive of zeros is refused" "$UC" 1
}

for t in test_run_once_or_kept_alive test_long_running_stopped_and_hup test_bootstrap_port test_ignored_service_files \
         test_missing_program test_bootstat test_thermal test_seed_unpacks test_seed_xattrs test_seed_refuses test_seed_damaged; do
  echo "== $t"
  $t
done
echo "helpers host tests: $pass passed, $fail failed"
[ "$fail" = 0 ]
