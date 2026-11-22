#!/bin/bash
# Host tests of the runtime, the Rust workspace runtime/Cargo.toml (CI runs every executable */tests/host.sh; the
# whole suite with the port, link and package checks is runtime/host-tests.sh). No phone and no Apple files.
#   cargo test --workspace: every crate's tests (the binder ones on a binderfs in a user namespace, skipped with a
#     line where the kernel has none; drm2kgsl's when external/virglrenderer is there)
#   cargo fmt --check, cargo clippy --workspace --all-targets -D warnings
#   runtime/xnuhost/check-arm64.sh smoke check: the static Android xnuhost booting the Rust test kernel under qemu-aarch64
#     (as it is on an arm64 Linux host), when the NDK and qemu-aarch64 are there; skipped with a line otherwise
set -euo pipefail
cd "$(dirname "${BASH_SOURCE[0]}")/.."
# Linux tests: on a Mac, runtime/host-tests.sh runs the suite in an arm64 Linux container.
[ "$(uname -s)" != Darwin ] || { echo "runtime: host tests are Linux tests: run runtime/host-tests.sh" >&2; exit 1; }
[ -d "$HOME/.cargo/bin" ] && PATH=$HOME/.cargo/bin:$PATH
# The NDK for every part (runtime/ndk-cc, the build scripts, the tests that probe the NDK's headers): CI sets ANDROID_NDK_ROOT
export NDK=${NDK:-${ANDROID_NDK_ROOT:-${ANDROID_NDK_HOME:-$HOME/Android/Sdk/ndk/28.2.13676358}}}
cargo test --workspace --locked
cargo fmt --all --check
cargo clippy --workspace --all-targets --locked -- -D warnings
if [ ! -x "$NDK/toolchains/llvm/prebuilt/linux-x86_64/bin/aarch64-linux-android30-clang" ]; then
  echo "runtime: no NDK at $NDK: the test kernel's boots skipped"
elif [ "$(uname -m)" != aarch64 ] && ! command -v qemu-aarch64 > /dev/null; then
  echo "runtime: no qemu-aarch64: the test kernel's boots skipped"
else
  xnuhost/check-arm64.sh smoke check
fi
