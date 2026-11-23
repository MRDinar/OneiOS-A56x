#!/bin/bash
# CI: the pinned NDK (through the SDK's own manager), a current stable Rust with rustfmt and clippy, and the Rust
# targets (aarch64-linux-android: the runtime and oneios-tool for the phone; aarch64-unknown-none: the task stub and the
# test kernel), qemu-user (the test kernel's boots, runtime/tests/host.sh), for runtime/build.sh and runtime/package.sh, and oneios-tool (tools/oneios-tool, Rust 1.95 or newer): runtime/package.sh checks the package
# with it (`oneios-tool package`: no Apple file may go in) and kernel/tests/host.sh runs its checks.
# The revision is the one runtime/package.sh checks.
set -euo pipefail
NDK_REVISION=28.2.13676358
SDK=${ANDROID_SDK_ROOT:-${ANDROID_HOME:?no Android SDK on this runner}}
if [ ! -f "$SDK/ndk/$NDK_REVISION/source.properties" ]; then
  manager=$(ls "$SDK"/cmdline-tools/*/bin/sdkmanager | tail -1)
  # `yes` ends with SIGPIPE when the manager has read its answers: not a failure of the install.
  set +o pipefail
  yes 2>/dev/null | "$manager" --install "ndk;$NDK_REVISION" > /dev/null
  set -o pipefail
fi
grep -q "Pkg.Revision = $NDK_REVISION" "$SDK/ndk/$NDK_REVISION/source.properties"
echo "ANDROID_NDK_ROOT=$SDK/ndk/$NDK_REVISION" >> "$GITHUB_ENV"
rustup toolchain install stable --profile minimal --component rustfmt,clippy --no-self-update
rustup default stable
rustup target add aarch64-linux-android aarch64-unknown-none
command -v qemu-aarch64 > /dev/null || { sudo apt-get update -qq && sudo apt-get install -y -qq qemu-user > /dev/null; }
cargo build --release --manifest-path "$(dirname "${BASH_SOURCE[0]}")/../../tools/oneios-tool/Cargo.toml"
