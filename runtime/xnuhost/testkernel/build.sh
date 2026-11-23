#!/bin/sh
# runtime/xnuhost/testkernel/build.sh OUT: builds the test kernel (src/, a no_std crate for aarch64-unknown-none, the
# C's tests/testkernel.c) and writes its ELF to OUT (a file, or OUT/testkernel.elf when OUT is a directory). xnuloader's
# mkkernel wraps it as a kernel.kc and writes the test disk:
#   cargo run -p xnuloader --bin mkkernel -- OUT/testkernel.elf IMAGE/kernel.kc IMAGE/os.img
# Refused when its code uses x18 (the C's -ffixed-x18; .cargo/config.toml reserves it) anywhere but the frame_resume
# test's save of it, or when its entry is not _start (mkkernel takes the base one page below it).
#   NDK=...               the NDK whose llvm-objdump and llvm-readelf it uses (as runtime/ndk-cc finds it)
#   TESTKERNEL_TARGET_DIR cargo's target directory (default: this folder's target/; never the caller's)
set -eu
here=$(cd "$(dirname "$0")" && pwd)
[ $# -eq 1 ] || { echo "usage: build.sh OUT" >&2; exit 2; }
out=$1
[ -d "$out" ] && out=$out/testkernel.elf
case "$(uname -s)" in
  Darwin) host=darwin-x86_64; default=$HOME/Library/Android/sdk/ndk/28.2.13676358 ;;
  *)      host=linux-x86_64;  default=$HOME/Android/Sdk/ndk/28.2.13676358 ;;
esac
ndk=${NDK:-${ANDROID_NDK_HOME:-$default}}
tools=$ndk/toolchains/llvm/prebuilt/$host/bin
for tool in llvm-objdump llvm-readelf llvm-nm; do
  [ -x "$tools/$tool" ] || { echo "testkernel: $tools/$tool not found (set NDK)" >&2; exit 1; }
done
target_dir=${TESTKERNEL_TARGET_DIR:-$here/target}
# A cargo run by a build script inherits the outer build's flags and target: none of them is the test kernel's (its
# own are in .cargo/config.toml, read from this folder).
unset RUSTFLAGS CARGO_ENCODED_RUSTFLAGS CARGO_BUILD_RUSTFLAGS CARGO_BUILD_TARGET CARGO_TARGET_DIR CARGO_BUILD_TARGET_DIR
cd "$here"
cargo build --quiet --release --locked --target aarch64-unknown-none --target-dir "$target_dir" 2>&1 |
  grep -v 'reserve-x18\|phased out\|issue #116344\|generated 1 warning\|^ *|$\|^ *= note\|^$' >&2 || true
elf=$target_dir/aarch64-unknown-none/release/testkernel
[ -f "$elf" ] || { echo "testkernel: the build failed" >&2; exit 1; }
x18=$("$tools/llvm-objdump" -d "$elf" | grep -E '\b[xw]18\b' | grep -cv 'str[[:space:]]*x18, \[x10, #0x90\]' || true)
[ "$x18" = 0 ] || { echo "testkernel: the code uses x18 ($x18 instructions)" >&2; exit 1; }
entry=$("$tools/llvm-readelf" -h "$elf" | sed -n 's/^ *Entry point address: *0x//p' | tr A-F a-f)
start=$("$tools/llvm-nm" "$elf" | sed -n 's/^0*\([0-9a-f]*\) [Tt] _start$/\1/p')
[ -n "$start" ] && [ "$entry" = "$start" ] || { echo "testkernel: entry 0x$entry is not _start (0x$start)" >&2; exit 1; }
mkdir -p "$(dirname "$out")"
cp "$elf" "$out.tmp"
mv "$out.tmp" "$out"
