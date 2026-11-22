#!/bin/sh
# runtime/xnuhost/check-arm64.sh [smoke|check|bench|parallel|rpc|all]...: the qemu boots of the test kernel that the C
# Makefile's check-arm64, bench-arm64, parallel-arm64 and rpc-arm64 targets made, for the Rust xnuhost. With no
# argument: check. Under qemu-aarch64 on a PC (the times are qemu's: compare runs on the same host only), natively on
# an arm64 Linux host.
#   smoke     the vCPU machinery alone (examples/machine_smoke.rs): no kernel image, no session; seconds
#   check     the three boots of testkernel/check-arm64.sh: the normal boot ('TESTKERNEL: all passed'), ram=auto with
#             a made-up meminfo, the restart loop (exit 3 after 9 'TESTKERNEL: restart')
#   bench     the trap path's costs (docs/booter.md 2.6a): the test kernel's benchmarks, then xnuhost's "perf:" report
#   parallel  N threads of one task, one per vCPU (docs/booter.md 2.6b): the speed-up for 1, 2, 4 and 8 threads
#   rpc       the Mach RPC round trip (docs/booter.md 2.6d), once per interrupt path (RPC_MODES)
# It builds, unless given: the static xnuhost (cargo, aarch64-linux-android, crt-static, target dir target/machine-static),
# the test kernel (testkernel/build.sh) and its image (the loader crate's mkkernel: kernel.kc, os.img).
#   XNUHOST=FILE   boot this binary instead (e.g. the C one: make -C runtime/xnuhost build/android/xnuhost)
#   IMAGE=DIR      this test image (kernel.kc, os.img) instead of building one
#   WORK=DIR       the data store, the logs and the built image (default runtime/target/machine-check)
#   QEMU=FILE      another qemu-aarch64
#   BENCH_SCALE=N, BENCH_SET="--set spin-host=5 ..."      the bench's scale and xnuhost settings
#   PAR_CPUS=8, PAR_SCALE=N, PAR_SET="--set cpu-pin=2"   the parallel test's vCPUs, scale, settings
#   RPC_MODES="0 1 2", RPC_SCALE=N, RPC_SET=...          the rpc test's modes (0 ArmCpu's trapped GIC, 1 host calls,
#                                                         2 one vCPU, no IPI: the floor), scale, settings
set -eu
here=$(cd "$(dirname "$0")" && pwd)
runtime=$(dirname "$here")
work=${WORK:-$runtime/target/machine-check}
mkdir -p "$work/testdata"
[ $# -gt 0 ] || set -- check
for what in "$@"; do
  case $what in smoke|check|bench|parallel|rpc) ;; all) set -- smoke check bench parallel rpc; break ;;
    *) echo "usage: check-arm64.sh [smoke|check|bench|parallel|rpc|all]..." >&2; exit 2 ;; esac
done

case "$(uname -s)/$(uname -m)" in
  Linux/aarch64|Linux/arm64) run= ;;
  *) run=${QEMU:-$(command -v qemu-aarch64 || true)}
     [ -n "$run" ] || { echo "check-arm64: qemu-aarch64 not installed: skipped"; exit 0; } ;;
esac

# ---- what is booted -----------------------------------------------------------------------------------------------
static_build() {
  (cd "$runtime" && CARGO_TARGET_DIR=target/machine-static cargo build --quiet --release --target aarch64-linux-android \
     -p xnuhost "$@" --config 'target.aarch64-linux-android.rustflags=["-C","target-feature=+crt-static"]')
}
for what in "$@"; do
  [ "$what" = smoke ] || continue
  static_build --example machine-smoke
  # shellcheck disable=SC2086 # $run is a word list
  timeout 60 $run "$runtime/target/machine-static/aarch64-linux-android/release/examples/machine-smoke"
done
for what in "$@"; do [ "$what" = smoke ] || need=1; done
[ -n "${need:-}" ] || exit 0
if [ -z "${XNUHOST:-}" ]; then
  static_build --bin xnuhost
  XNUHOST=$runtime/target/machine-static/aarch64-linux-android/release/xnuhost
fi
if [ -z "${IMAGE:-}" ]; then
  IMAGE=$work/testimage
  mkdir -p "$IMAGE"
  sh "$here/testkernel/build.sh" "$work/testkernel.elf"
  (cd "$runtime" && CARGO_TARGET_DIR=target/machine cargo run --quiet -p xnuloader --bin mkkernel -- \
     "$work/testkernel.elf" "$IMAGE/kernel.kc" "$IMAGE/os.img")
fi
common="--mode bench --image $IMAGE --data $work/testdata --display none --no-input --width 64 --height 64"
# key=value boot arguments: "testkernel-bench" or "testkernel-bench=N"
scaled() { if [ -n "$2" ]; then echo "$1=$2"; else echo "$1"; fi; }

for what in "$@"; do
  rm -f "$work/testdata/data.img"
  case $what in
  check)
    sh "$here/testkernel/check-arm64.sh" "$XNUHOST" "$IMAGE" "$work" ;;
  bench)
    # shellcheck disable=SC2086 # $run, $common and the *_SET settings are word lists
    timeout 120 $run "$XNUHOST" $common --cpus 3 --ram 256M --set ram-giveback=1 \
      --boot-args "$(scaled testkernel-bench "${BENCH_SCALE:-}")" ${BENCH_SET:-} > "$work/bench.log" 2>&1 || true
    grep -E 'BENCH|perf:|TESTKERNEL: (all|FAIL)|hand-off' "$work/bench.log" ;;
  parallel)
    # shellcheck disable=SC2086
    timeout 300 $run "$XNUHOST" $common --cpus "${PAR_CPUS:-8}" --ram 256M --set ram-giveback=1 \
      --boot-args "$(scaled testkernel-parallel "${PAR_SCALE:-}")" ${PAR_SET:-} > "$work/parallel.log" 2>&1 || true
    grep -E 'PARALLEL|perf:   queue|TESTKERNEL: (all|FAIL)' "$work/parallel.log" ;;
  rpc)
    # Each mode is its own run, so xnuhost's perf report and `emulated:` lines at the stop are that mode's.
    for m in ${RPC_MODES:-0 1 2}; do
      rm -f "$work/testdata/data.img"
      # shellcheck disable=SC2086
      timeout 120 $run "$XNUHOST" $common --cpus 2 --ram 256M --set ram-giveback=1 \
        --boot-args "$(scaled testkernel-rpc "${RPC_SCALE:-}") testkernel-rpc-mode=$m" ${RPC_SET:-} > "$work/rpc-$m.log" 2>&1 || true
      grep -E 'RPC|BENCH|perf: [0-9.]+ s since|perf:   (up|down|queue|release)|emulated: insn|TESTKERNEL: (all|FAIL)' "$work/rpc-$m.log"
    done ;;
  esac
done
