#!/bin/sh
# runtime/xnuhost/testkernel/check-arm64.sh XNUHOST IMAGE WORK: the three boots of the Makefile's `make check-arm64`,
# for any xnuhost binary (the C one, the Rust one) and any test image (IMAGE/kernel.kc and IMAGE/os.img, as mkkernel
# writes them from the test kernel's ELF). WORK holds the data store and the logs (check.log, ramauto.log,
# restart.log). Under qemu-aarch64 on a PC, natively on an arm64 Linux host; QEMU=... names another qemu.
#   1. the normal boot: 3 vCPUs, 256 MiB, ram-giveback=1: 'TESTKERNEL: all passed'
#   2. ram=auto on a host of 4 GiB, 2.5 GiB available (ram-meminfo): 1024 or 1280 MiB, held and prefaulted
#   3. a guest that keeps restarting: xnuhost starts again in place 8 times, then exit 3 (9 'TESTKERNEL: restart')
set -u
[ $# -eq 3 ] || { echo "usage: check-arm64.sh XNUHOST IMAGE WORK" >&2; exit 2; }
xnuhost=$1 image=$2 work=$3
case "$(uname -s)/$(uname -m)" in
  Linux/aarch64|Linux/arm64) run= ;;
  *) run=${QEMU:-$(command -v qemu-aarch64 || true)}
     [ -n "$run" ] || { echo "check-arm64: qemu-aarch64 not installed: skipped"; exit 0; } ;;
esac
mkdir -p "$work/testdata"
common="--mode bench --image $image --data $work/testdata --display none --no-input --width 64 --height 64"
rm -f "$work/testdata/data.img"
echo "check-arm64: booting the test kernel under ${run:-this arm64 Linux kernel}"
# shellcheck disable=SC2086 # $run and $common are word lists
timeout 30 $run "$xnuhost" $common --cpus 3 --ram 256M --set ram-giveback=1 2>&1 | tee "$work/check.log"
grep -q 'TESTKERNEL: all passed' "$work/check.log" || { echo "check-arm64: the test kernel did not pass"; exit 1; }

echo "check-arm64: ram=auto on a host of 4 GiB, 2.5 GiB available (ram-meminfo): the guest gets what fits, 1024 MiB (1280 where the lock can work: a smaller headroom), held fixed and locked by default (ram-giveback=0; where mlock fails, a clean fallback)"
printf 'MemTotal:        4194304 kB\nMemAvailable:    2621440 kB\n' > "$work/meminfo-3g"
rm -f "$work/testdata/data.img"
# shellcheck disable=SC2086
timeout 30 $run "$xnuhost" $common --cpus 3 --set "ram-meminfo=$work/meminfo-3g" > "$work/ramauto.log" 2>&1
if ! { grep -Eq 'ram=auto: host MemTotal 4096 MiB, MemAvailable 2560 MiB; the guest gets (1024|1280) MiB' "$work/ramauto.log" &&
       grep -Eq 'memory: the guest.s RAM is held for the session: (1024|1280) MiB, prefaulted' "$work/ramauto.log" &&
       grep -Eq 'memory: the guest.s RAM is held: [0-9]+ MiB prefaulted in [0-9]+ ms, (locked \(mlock|not locked \(mlock: .*so not prefaulted)' "$work/ramauto.log" &&
       grep -q 'TESTKERNEL: all passed' "$work/ramauto.log"; }; then
  grep -E 'memory|FAIL|TESTKERNEL' "$work/ramauto.log" | tail -8
  echo "check-arm64: ram=auto failed"
  exit 1
fi

echo "check-arm64: a guest that keeps restarting: xnuhost starts again in place 8 times, then exit 3"
# shellcheck disable=SC2086
XNUHOST_RUNNER="$run" timeout 60 $run "$xnuhost" $common --cpus 1 --ram 256M --boot-args testkernel-restart > "$work/restart.log" 2>&1
code=$?
starts=$(grep -c 'TESTKERNEL: restart' "$work/restart.log")
if [ "$code" != 3 ] || [ "$starts" != 9 ]; then
  tail -5 "$work/restart.log"
  echo "check-arm64: restart: exit $code"
  exit 1
fi
echo "check-arm64: all three boots passed"
