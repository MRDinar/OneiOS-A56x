# Sourced by tests/phone/run.sh (with lib.sh's helpers, HERE, W and TOOL set): `oneios-tool phone procs`, the process
# list with CPU and RAM (docs/plan-ios-first-session.md stage 1), against a stand-in phone of its own ($W/procsphone)
# that has what the command reads:
#   - the stand-in's own /proc (FAKE_PROC=stand-in on every host: these checks do not read this machine's processes):
#     .fake/proc/stat, .fake/proc/meminfo and, for each made-up process, .fake/proc/PID/stat and status (a file
#     `made-up` beside them keeps fake-adb.sh from dropping the entry as a process that is gone);
#   - a stand-in `ps` (.fake/bin/ps: the pids and command lines of .fake/ps.txt; one of them has no /proc entry, a
#     process that ended between `ps` and the read), a stand-in `sleep` (.fake/bin/sleep: time passes, each
#     process's stat becomes its stat.next and the CPUs' too; the seconds asked for go to .fake/slept), and getprop
#     with no argument (lib.sh: the whole of .fake/props, init's services among them).
# The scene: a session (xnuhost 500 with its task factory and one iOS task, the display helper 600), the fork's key
# reader, adbd with the tool's own shell, two kernel threads, and One UI's surfaceflinger, composer, audioserver and
# cameraserver. 8 CPUs, 4000 ticks between the two readings: 500 ticks are 100 % of one CPU.
# Checked: root asked for first; the report (the session, the host's CPU and memory lines, each group's sums, each
# Android process with its CPU share, RAM, init service and role, the order by CPU and by memory, --top, the
# services' states, the busy kernel thread); --seconds (the span the phone waited; 0: one reading and no CPU
# figures); --raw and --from (the same report from the kept output, no phone asked); a pid file that names a
# process that is not an xnuhost (no session); no session at all; a value out of range; and what was sent to the
# phone: named files only, nothing written.
procs_checks() {
  local P=$W/procsphone before=${FAKE_PHONE:-}
  local S=$P/data/local/tmp/oneios PR=$P/.fake/proc
  local out raw n0 sent i
  export FAKE_PHONE=$P
  make_phone "$P"
  mkdir -p "$PR" "$S/control"
  pp() { FAKE_PROC=stand-in "$TOOL" phone procs "$@"; }
  # mkproc PID COMM PPID FLAGS "U S" "U S" THREADS [ANON SWAP RSS]: a process's stat at the first reading (stat.first)
  # and at the second (stat.next), and its status (a kernel thread has no Vm lines)
  mkproc() {
    local pid=$1 comm=$2 ppid=$3 flags=$4 t1=$5 t2=$6 thr=$7 vsz=123456
    [ "$flags" = 2097152 ] && vsz=0
    mkdir -p "$PR/$pid"; : > "$PR/$pid/made-up"
    echo "$pid ($comm) S $ppid $pid 0 0 -1 $flags 0 0 0 0 $t1 0 0 20 0 $thr 0 100 $vsz 2000 1" > "$PR/$pid/stat.first"
    echo "$pid ($comm) S $ppid $pid 0 0 -1 $flags 0 0 0 0 $t2 0 0 20 0 $thr 0 100 $vsz 2000 1" > "$PR/$pid/stat.next"
    printf 'Name:\t%s\nUid:\t1000\t1000\t1000\t1000\n' "$comm" > "$PR/$pid/status"
    [ $# -lt 10 ] || printf 'VmRSS:\t%8s kB\nRssAnon:\t%8s kB\nRssFile:\t%8s kB\nRssShmem:\t%8s kB\nVmSwap:\t%8s kB\nThreads:\t%s\n' "${10}" "$8" 0 0 "$9" "$thr" >> "$PR/$pid/status"
  }
  # the first reading again (the stand-in sleep moved every stat to its stat.next)
  first() {
    local d
    for d in "$PR"/*/; do cp "$d/stat.first" "$d/stat"; done
    cp "$PR/stat.first" "$PR/stat"
  }
  {
    echo "cpu  1000 0 1000 8000 0 0 0 0 0 0"
    for i in 0 1 2 3 4 5 6 7; do echo "cpu$i 125 0 125 1000 0 0 0 0 0 0"; done
    echo "intr 1 2 3"
  } > "$PR/stat.first"
  {
    echo "cpu  2000 0 2500 9400 100 0 0 0 0 0"
    for i in 0 1 2 3 4 5 6 7; do echo "cpu$i 250 0 312 1175 12 0 0 0 0 0"; done
    echo "intr 4 5 6"
  } > "$PR/stat.next"
  printf 'MemTotal:       11350016 kB\nMemFree:          204800 kB\nMemAvailable:    2043904 kB\nSwapTotal:      12581888 kB\nSwapFree:       11910144 kB\nShmem:           5869568 kB\nMlocked:         5771264 kB\nPageTables:       369664 kB\n' > "$PR/meminfo"
  mkproc 1 init 0 0 "10 10" "10 10" 1 2048 0 4096
  mkproc 2 kthreadd 0 2097152 "0 0" "0 0" 1
  mkproc 30 kswapd0 2 2097152 "0 100" "0 125" 1
  mkproc 100 surfaceflinger 1 0 "100 100" "130 120" 30 51200 10240 153600
  mkproc 110 vendor.qti.hard 1 0 "50 50" "60 65" 12 20480 0 61440
  mkproc 120 audioserver 1 0 "5 5" "5 5" 20 30720 5120 71680
  mkproc 130 cameraserver 1 0 "1 1" "3 3" 8 40960 2048 92160
  mkproc 140 adbd 1 0 "1 1" "1 1" 4 1024 0 4096
  mkproc 141 sh 140 0 "0 0" "0 0" 1 512 0 2048
  mkproc 200 sh 1 0 "0 0" "0 0" 1 512 0 2048
  mkproc 201 getevent 200 0 "0 0" "0 0" 1 256 0 1024
  mkproc 500 xnuhost 1 0 "1000 2000" "1400 2600" 40 5767168 0 5800000
  mkproc 501 xnuhost-tasks 500 0 "0 0" "0 0" 1 1024 0 2048
  mkproc 502 "launchd (x)" 501 0 "100 100" "300 400" 3 20480 0 40960
  mkproc 600 app_process 1 0 "10 10" "30 15" 25 102400 0 204800
  cat > "$P/.fake/ps.txt" <<'PS'
1 init second_stage
2 [kthreadd]
30 [kswapd0]
100 surfaceflinger
110 vendor.qti.hardware.display.composer-service
120 audioserver
130 cameraserver
140 adbd --root_seclabel=u:r:su:s0
141 sh -s
200 sh /data/local/tmp/oneios/oneios_switch.sh --keys
201 getevent -l /dev/input/event0
500 xnuhost --session-abi 1 --mode takeover
501 xnuhost-tasks
502 launchd
600 app_process /data/local/tmp/oneios/runtime/lib oneios.display.Main --session /data/local/tmp/oneios/session
999 gone-before-the-read
PS
  cat > "$P/.fake/bin/ps" <<'SH'
#!/bin/sh
# the stand-in phone's processes (tests/phone/procs.sh): pids, or with ARGS asked the pids and command lines
case "$*" in *ARGS*) cat "$FAKE_PHONE/.fake/ps.txt" ;; *) cut -d' ' -f1 "$FAKE_PHONE/.fake/ps.txt" ;; esac
SH
  cat > "$P/.fake/bin/sleep" <<'SH'
#!/bin/sh
# time passes on the stand-in phone (tests/phone/procs.sh): the second reading's figures
for f in "$FAKE_PHONE"/.fake/proc/*/stat.next "$FAKE_PHONE"/.fake/proc/stat.next; do [ ! -e "$f" ] || cp "$f" "${f%.next}"; done
echo "$1" >> "$FAKE_PHONE/.fake/slept"
SH
  chmod +x "$P/.fake/bin/ps" "$P/.fake/bin/sleep"
  cat > "$P/.fake/props" <<'PROPS'
[ro.product.model]: [SM-S928B]
[init.svc.zygote]: [stopped]
[init.svc.surfaceflinger]: [running]
[init.svc.vendor.qti.hardware.display.composer]: [running]
[init.svc.audioserver]: [running]
[init.svc.cameraserver]: [running]
[init.svc.adbd]: [running]
[init.svc_debug_pid.surfaceflinger]: [100]
[init.svc_debug_pid.vendor.qti.hardware.display.composer]: [110]
[init.svc_debug_pid.audioserver]: [120]
[init.svc_debug_pid.cameraserver]: [130]
[init.svc_debug_pid.adbd]: [140]
[sys.usb.state]: [mtp,adb]
PROPS
  echo 500 > "$S/session/xnuhost.pid"; echo 600 > "$S/control/display.pid"

  check "procs: no root is said first" 1 "no root on the phone FAKE0001.*Nothing was changed" env FAKE_NO_ROOT=1 FAKE_PROC=stand-in "$TOOL" phone procs
  check "procs: --seconds out of range is refused" 2 "601" pp --seconds 601

  # ---- the report of a session
  first; rm -f "$P/.fake/slept"; n0=$(wc -c < "$FAKE_ROOT_LOG")
  out=$(pp 2>&1); echo "$out" > "$W/procs.out"
  sent=$(tail -c +$((n0 + 1)) "$FAKE_ROOT_LOG")
  has() { printf '%s\n' "$out" | grep -Eq -- "$2" && ok "procs: $1" || bad "procs: $1" "no /$2/ in $W/procs.out"; }
  has "the phone and the session" "^session: xnuhost pid 500$"
  printf '%s\n' "$out" | head -n 1 | grep -q '^phone: FAKE0001$' && ok "procs: the phone's serial first" || bad "procs: the phone's serial first"
  has "the host's CPU over the span" "^host: CPU 500% of 800 \(user 200%, sys 300%, irq 0%\), 8 CPUs online$"
  has "the host's memory" "^memory: MemTotal 11084 MiB, MemAvailable 1996 MiB, swap 656 of 12287 MiB used, Shmem 5732 MiB, Mlocked 5636 MiB, page tables 361 MiB$"
  has "the session's sum (xnuhost, the factory, the task, the display helper)" "^the session .* +4 +69 +305\.0 +5753\.0 +0\.0 +5906\.0$"
  has "the fork's watchers" "^the fork's watchers .* +2 +2 +0\.0 +0\.7 +0\.0 +3\.0$"
  has "adb, with the tool's own shell" "^adb .* +2 +5 +0\.0 +1\.5 +0\.0 +6\.0$"
  has "the kernel's threads" "^the kernel's threads +2 +2 +5\.0 +0\.0 +0\.0 +0\.0$"
  has "Android and the vendor's sum" "^Android and the vendor +5 +71 +15\.8 +142\.0 +17\.0 +374\.0$"
  has "surfaceflinger's line" "^ +100 +10\.0 +50\.0 +10\.0 +150\.0 +30  surfaceflinger +surfaceflinger +bridge: display \(the helper's layer; the return restarts it\)$"
  has "the composer by its init service" "^ +110 +5\.0 +20\.0 +0\.0 +60\.0 +12  vendor\.qti\.hardware\.display\.composer +vendor\.qti\.hardware\.display\.composer +bridge: display \(holds the panel"
  has "cameraserver: on the restart list, no bridge" "^ +130 +0\.8 +40\.0 +2\.0 +90\.0 +8  cameraserver +cameraserver +restart list; no bridge uses it in a session$"
  has "audioserver" "^ +120 +0\.0 +30\.0 +5\.0 +70\.0 +20  audioserver +audioserver +bridge: audio \(AAudio\); restart list$"
  has "init" "^ +1 +0\.0 +2\.0 +0\.0 +4\.0 +1  - +init second_stage +keep: the way back$"
  [ "$(printf '%s\n' "$out" | sed -n '/^Android and the vendor: 5 processes, by CPU, then anon + swap:$/,/^$/p' | awk 'NR > 2 && NF { printf "%s ", $1 }')" = "100 110 130 120 1 " ] \
    && ok "procs: by CPU, then by anon + swap" || bad "procs: by CPU, then by anon + swap" "see $W/procs.out"
  has "the services a session is about" "^init services: 5 running of 6; zygote stopped, surfaceflinger running, vendor.qti.hardware.display.composer running, audioserver running, cameraserver running, adbd running$"
  has "the session's own: xnuhost" "^ +500 +200\.0 +5632\.0 +0\.0 +5664\.0 +40  xnuhost --session-abi 1 --mode takeover$"
  has "the session's own: the display helper" "^ +600 +5\.0 +100\.0 +0\.0 +200\.0 +25  app_process /data/local/tmp/oneios/runtime/lib oneios.display.Main "
  has "iOS's tasks as one line (a name with a parenthesis in /proc/PID/stat)" "^ +100\.0 +20\.0 +0\.0 +40\.0 +3  iOS's tasks: 1 process\(es\)"
  has "the busy kernel thread" "^ +30 +5\.0  kswapd0$"
  printf '%s\n' "$out" | grep -q 'gone-before-the-read\|999' && bad "procs: a process that ended before the read is not listed" || ok "procs: a process that ended before the read is not listed"
  printf '%s\n' "$out" | grep -q "the phone's output ended" && bad "procs: the output is whole" || ok "procs: the output is whole"
  [ "$(cat "$P/.fake/slept" 2>/dev/null)" = 5 ] && ok "procs: the phone waited the 5 s between the readings, once" || bad "procs: the phone waited 5 s once" "$(cat "$P/.fake/slept" 2>/dev/null | tr '\n' ' ')"

  # ---- what was sent: named files only, nothing written, no page-table walk
  if printf '%s' "$sent" | grep -q 'ps -A -o PID=' && printf '%s' "$sent" | grep -q '/proc/\$p/stat' && printf '%s' "$sent" | grep -q '/proc/meminfo'; then ok "procs: the script reads /proc/PID/stat, status and meminfo by name"
  else bad "procs: the script reads /proc/PID/stat, status and meminfo by name" "$(printf '%s' "$sent" | head -n 5 | tr '\n' '|')"; fi
  if printf '%s' "$sent" | grep -Eq '\*|find |ls |du |smaps|setprop|kill|rm |>>| > |/sys/'; then bad "procs: no glob, no walk, nothing written or signalled" "$(printf '%s' "$sent" | grep -E '\*|find |ls |du |smaps|setprop|kill|rm |>>| > |/sys/' | head -n 3 | tr '\n' '|')"
  else ok "procs: no glob, no walk, nothing written or signalled"; fi

  # ---- the order by memory, --top
  first
  out=$(pp --sort mem --top 3 --seconds 2 2>&1)
  [ "$(printf '%s\n' "$out" | sed -n '/^Android and the vendor: 5 processes (the first 3), by anon + swap, then CPU:$/,/^$/p' | awk 'NR > 2 && NF { printf "%s ", $1 }')" = "100 130 120 " ] \
    && ok "procs: --sort mem --top 3" || bad "procs: --sort mem --top 3" "$(printf '%s' "$out" | tail -n 8 | tr '\n' '|')"
  [ "$(tail -n 1 "$P/.fake/slept")" = 2 ] && ok "procs: --seconds 2 is the span" || bad "procs: --seconds 2 is the span"

  # ---- one reading: no CPU figures, no wait
  first; rm -f "$P/.fake/slept"
  out=$(pp --seconds 0 2>&1)
  has "--seconds 0: CPU not measured" "^host: CPU not measured"
  has "--seconds 0: a process's CPU is -" "^ +100 +- +50\.0 "
  [ ! -e "$P/.fake/slept" ] && ok "procs: --seconds 0 does not wait" || bad "procs: --seconds 0 does not wait"

  # ---- --raw and --from: the same report from the kept output, with no phone
  first
  pp --raw > "$W/procs.raw" 2>&1
  grep -q '^@snap 2$' "$W/procs.raw" && grep -q '^@end$' "$W/procs.raw" && ok "procs: --raw is the phone's own lines" || bad "procs: --raw is the phone's own lines" "$(head -n 3 "$W/procs.raw" | tr '\n' '|')"
  out=$(env FAKE_DEVICES= "$TOOL" phone procs --from "$W/procs.raw" 2>&1)
  if [ "$out" = "$(sed 1d "$W/procs.out")" ]; then ok "procs: --from gives the live report, with no phone on adb"
  else bad "procs: --from gives the live report, with no phone on adb" "$(diff <(printf '%s\n' "$out") <(sed 1d "$W/procs.out") | head -n 6 | tr '\n' '|')"; fi
  check "procs: --from a file that is not there" 1 "cannot read" "$TOOL" phone procs --from "$W/no-such-file"

  # ---- a pid file that names another process (the pid was used again), then no session at all
  first
  sed -i 's/^500 xnuhost .*/500 sleep 100/' "$P/.fake/ps.txt"
  for i in stat.first stat.next; do sed -i 's/(xnuhost)/(sleep)/' "$PR/500/$i"; done
  first
  out=$(pp --seconds 0 2>&1)
  has "a pid file that names no xnuhost: no session" "^session: none$"
  has "then nothing is the session's (the display pid file too)" "^ +600 .*app_process"
  rm -f "$S/session/xnuhost.pid" "$S/control/display.pid"
  out=$(pp --seconds 0 2>&1)
  has "no pid file: no session" "^session: none$"

  rm -rf "$PR"
  export FAKE_PHONE=$before
}
