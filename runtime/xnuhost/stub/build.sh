#!/bin/sh
# runtime/xnuhost/stub/build.sh OUT: builds the task stub (src/main.rs, a no_std crate for aarch64-unknown-none) and
# writes the flat blob to OUT (a file, or OUT/stub.bin when OUT is a directory), as the C Makefile's stub.bin rule did:
# linked at 0 by stub.ld, refused when it would need relocating or data (.got, .data, .bss), when a code path could
# panic (a panic's location is a stored pointer), when its entry is not at offset 0, or when it is larger than
# XS_TEXT_MAX (64 KiB, what task.rs copies). xnuhost's build.rs calls it.
#   NDK=...          the NDK whose llvm-objcopy and llvm-readelf it uses (else $ANDROID_NDK_HOME, else the SDK's
#                    ndk/28.2.13676358), found as runtime/ndk-cc finds it
#   STUB_TARGET_DIR  cargo's target directory (default: this folder's target/; never the caller's, whose lock a build
#                    script's cargo holds)
set -eu
here=$(cd "$(dirname "$0")" && pwd)
[ $# -eq 1 ] || { echo "usage: build.sh OUT" >&2; exit 2; }
out=$1
[ -d "$out" ] && out=$out/stub.bin
case "$(uname -s)" in
  Darwin) host=darwin-x86_64; default=$HOME/Library/Android/sdk/ndk/28.2.13676358 ;;
  *)      host=linux-x86_64;  default=$HOME/Android/Sdk/ndk/28.2.13676358 ;;
esac
ndk=${NDK:-${ANDROID_NDK_HOME:-$default}}
tools=$ndk/toolchains/llvm/prebuilt/$host/bin
for tool in llvm-objcopy llvm-readelf llvm-nm; do
  [ -x "$tools/$tool" ] || { echo "stub: $tools/$tool not found (set NDK)" >&2; exit 1; }
done
target_dir=${STUB_TARGET_DIR:-$here/target}
# A cargo run by a build script inherits the outer build's flags and target: none of them is the stub's.
unset RUSTFLAGS CARGO_ENCODED_RUSTFLAGS CARGO_BUILD_RUSTFLAGS CARGO_BUILD_TARGET CARGO_TARGET_DIR CARGO_BUILD_TARGET_DIR
cargo build --quiet --release --locked --manifest-path "$here/Cargo.toml" --target aarch64-unknown-none \
  --target-dir "$target_dir"
elf=$target_dir/aarch64-unknown-none/release/stub
if "$tools/llvm-readelf" -S "$elf" | grep -q '\.got\|\.data\|\.bss'; then echo "stub: needs relocation or data" >&2; exit 1; fi
if "$tools/llvm-nm" "$elf" | grep -qi 'panic'; then echo "stub: has a panic path" >&2; exit 1; fi
entry=$("$tools/llvm-readelf" -h "$elf" | sed -n 's/^ *Entry point address: *//p')
[ "$entry" = 0x0 ] || { echo "stub: entry at $entry, not at offset 0" >&2; exit 1; }
mkdir -p "$(dirname "$out")"
"$tools/llvm-objcopy" -O binary "$elf" "$out.tmp"
size=$(wc -c < "$out.tmp")
[ "$size" -gt 0 ] && [ "$size" -le 65536 ] || { rm -f "$out.tmp"; echo "stub: $size bytes, not 1 to XS_TEXT_MAX (65536)" >&2; exit 1; }
mv "$out.tmp" "$out"
