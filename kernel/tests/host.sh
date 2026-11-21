#!/bin/bash
# Host tests of kernel/ that need no toolchain and no network: script syntax, the pin tables, the patch series
# files, the profile and the build-record writer. Run by CI and by hand: kernel/tests/host.sh
set -euo pipefail
ROOT=$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)
cd "$ROOT"
pass=0
ok() { pass=$((pass + 1)); echo "ok $pass: $*"; }
fail() { echo "FAIL: $*" >&2; exit 1; }

for f in kernel/*.sh kernel/lib/*.sh kernel/linux/*.sh kernel/prelink/*.sh kernel/tests/*.sh scripts/image/kernel-inputs.sh; do bash -n "$f" || fail "syntax: $f"; done
for f in kernel/*.py kernel/linux/*.py; do [ -e "$f" ] || continue; python3 -I -m py_compile "$f" || fail "syntax: $f"; done
ok "script syntax"

# The portable helpers (kernel/lib/host.sh) with this host's tools: GNU on Linux, BSD on macOS.
. kernel/lib/host.sh
t=$(mktemp -d)
printf 'abc\n' > "$t/f"
[ "$(file_size "$t/f")" = 4 ] || fail "file_size"
[ "$(sha256 "$t/f")" = edeaaff3f1774ad2888673770c6d64097e391bc362d7d6fb34982ddf0efd18cb ] || fail "sha256"
sed_i 's/b/x/' "$t/f"; [ "$(cat "$t/f")" = axc ] || fail "sed_i"
[ "$(host_jobs)" -ge 1 ] || fail "host_jobs"
rm -rf "$t"
ok "portable helpers ($(uname -s))"

. kernel/pins.sh
for table in "$PINS_SOURCES" "$PINS_TOOLCHAIN"; do
  pin_rows "$table" | while read -r name url commit branch; do
    [[ $commit =~ ^[0-9a-f]{40}$ ]] || fail "pin $name: '$commit' is not a full commit"
    [[ $url == https://github.com/*.git ]] || fail "pin $name: url $url"
    [ -n "$branch" ] || fail "pin $name: no branch"
  done
done
[[ $XNU_COMMIT =~ ^[0-9a-f]{40}$ ]] || fail "XNU_COMMIT"
ok "pins are full commits"

# The series and profile checks are `oneios-tool kernel series-check` (kernel/tests/series-check.py until 2026-11-13),
# and the build record writer is `oneios-tool kernel build-record` (kernel/build-record.py until then). Both run
# here when the tool is built (CI builds it first); `cargo test` in tools/oneios-tool runs the record's checks and
# the frozen record of the Python writer, tools/oneios-tool/tests/kernelpy the frozen cases of series-check.
tool=${ONEIOS_TOOL:-tools/oneios-tool/target/release/oneios-tool}
if [ -x "$tool" ]; then
  "$tool" kernel series-check || fail "series"
  ok "patch series and profile"
  "$tool" kernel build-record --self-test || fail "build record"
  ok "build record"
else
  echo "skipped: patch series and profile, build record (no $tool: cargo build --release --manifest-path tools/oneios-tool/Cargo.toml)"
fi
# The arm64e kernel's compiler line (kernel/linux/clang-wrapper): the PAC ABI (signed calls, returns, the intrinsics)
# and function-pointer type discrimination reach cc1 whatever the host clang implies for the triple by itself.
# A clang that refuses the kernel's own driver flags (CI's Ubuntu clang: an older release) builds no kernel either, and
# CI never builds one (the release kernel comes from kernel/release-kernel.txt): skipped with its first error. Until
# 2026-11-21 the wrapper's exit status ended this script under set -e with no line at all (release run 44882714612).
if command -v "${CROSS_CLANG:-clang}" > /dev/null && "${CROSS_CLANG:-clang}" -target arm64e-apple-ios27.0 -### -c -x c /dev/null -o /dev/null > /dev/null 2>&1 &&
   cc1=$(kernel/linux/clang-wrapper -arch arm64e -mkernel -### -c -x c /dev/null -o /dev/null 2>&1); then
  for flag in -fptrauth-calls -fptrauth-returns -fptrauth-intrinsics -fptrauth-function-pointer-type-discrimination; do
    grep -q -- "\"$flag\"" <<< "$cc1" || fail "clang-wrapper: arm64e cc1 has no $flag"
  done
  ok "clang-wrapper: arm64e PAC flags reach cc1 ($("${CROSS_CLANG:-clang}" --version | head -1))"
elif [ -n "${cc1:-}" ]; then
  echo "skipped: clang-wrapper PAC flags ($("${CROSS_CLANG:-clang}" --version | head -1) refuses the kernel's flags: $(grep -m1 -i error <<< "$cc1" || head -1 <<< "$cc1"))"
else
  echo "skipped: clang-wrapper PAC flags (no clang with an arm64e-apple-ios target here)"
fi
echo "all $pass tests passed"
