#!/bin/bash
# Host tests of the runtime's link rule for Android shared objects (every cdylib link of the Rust workspace has
# -z defs: runtime/display/build-android.sh's cargo rustc line) and of runtime/package/elf-check.sh (every NEEDED
# library an API 30 stub or shipped, every undefined symbol in a NEEDED library). Needs the NDK, rustup's
# aarch64-linux-android target, and what runtime/build.sh needs (a JDK and d8 for the display helper's dex).
#   runtime/package/elf-check-test.sh
# The fault they exist for (the phone, 2026-11-14): liboneios-display.so of 1f7dd1f called
# ANativeWindow_getBuffersDataSpace and _setBuffersDataSpace (libnativewindow's) and linked against libandroid only.
# The link passed; app_process could not load the library. The tests make such a library again (elf-fault.rs, with
# rustc and the NDK's clang as its linker, as cargo links the runtime), in a temporary folder: with -z defs its link
# must fail, without it elf-check.sh must fail and name libnativewindow.so, and with libnativewindow it must pass both.
# They also check that executables fail to link on an undefined symbol with no flag at all, the NEEDED rule, the
# display helper's library as runtime/display/build-android.sh builds it, and a whole stage of runtime/build.sh.
# NDK: $NDK, else $ANDROID_NDK_ROOT, else $ANDROID_NDK_HOME, else the SDK's ndk/28.2.13676358.
set -euo pipefail
ROOT=$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)
case "$(uname -s)" in
  Darwin) NDK_HOST=darwin-x86_64; NDK_DEFAULT=$HOME/Library/Android/sdk/ndk/28.2.13676358 ;;
  *)      NDK_HOST=linux-x86_64;  NDK_DEFAULT=$HOME/Android/Sdk/ndk/28.2.13676358 ;;
esac
export NDK=${NDK:-${ANDROID_NDK_ROOT:-${ANDROID_NDK_HOME:-$NDK_DEFAULT}}}
[ -x "$NDK/toolchains/llvm/prebuilt/$NDK_HOST/bin/aarch64-linux-android30-clang" ] || { echo "elf-check-test: no NDK at $NDK ($NDK_HOST)" >&2; exit 1; }
READELF=$NDK/toolchains/llvm/prebuilt/$NDK_HOST/bin/llvm-readelf
[ -d "$HOME/.cargo/bin" ] && PATH=$HOME/.cargo/bin:$PATH
CHECK=$ROOT/runtime/package/elf-check.sh
FAULT=$ROOT/runtime/package/elf-fault.rs
T=$(mktemp -d)
trap 'rm -rf "$T"' EXIT
fail() { echo "FAIL: $*" >&2; exit 1; }
pass=0
ok() { pass=$((pass + 1)); echo "ok $pass: $*"; }
# rustc for the phone, linked by the NDK's clang (runtime/ndk-cc), as cargo builds the runtime
rs() { rustc --edition 2021 --target aarch64-linux-android -C opt-level=2 -C linker="$ROOT/runtime/ndk-cc" -A linker_messages "$@"; }
DEFS=(-C link-arg=-Wl,-z,defs)

link_fails() { # NAME SYMBOL COMMAND...: the link must fail, and on SYMBOL
  local name=$1 symbol=$2; shift 2
  if "$@" > "$T/log" 2>&1; then fail "$name: the link passed"; fi
  grep -q "undefined symbol: $symbol\$" "$T/log" || { cat "$T/log"; fail "$name: the link failed, but not on $symbol"; }
}
links() { # NAME COMMAND...
  local name=$1; shift
  "$@" > "$T/log" 2>&1 || { cat "$T/log"; fail "$name: the link failed"; }
}
check_fails() { # NAME PATH LINE...: elf-check.sh PATH must end with exit 1 and print every LINE
  local name=$1 path=$2 rc=0 line; shift 2
  "$CHECK" "$path" > "$T/log" 2>&1 || rc=$?
  [ "$rc" = 1 ] || { cat "$T/log"; fail "$name: elf-check.sh ended with $rc, not 1"; }
  for line in "$@"; do
    grep -qxF -- "$line" "$T/log" || { cat "$T/log"; fail "$name: elf-check.sh did not print: $line"; }
  done
}
check_passes() { # NAME PATH...
  local name=$1; shift
  "$CHECK" "$@" > "$T/log" 2>&1 || { cat "$T/log"; fail "$name: elf-check.sh failed"; }
}
needed() { # FILE: its NEEDED libraries as elf-check.sh lists them ("a.so, b.so")
  "$READELF" -d "$1" | sed -n 's/.*(NEEDED).*\[\(.*\)\]/\1/p' | paste -sd, - | sed 's/,/, /g'
}
missing() { # LIB SYMBOL NEEDED WHERE: elf-check.sh's line for a symbol that no NEEDED library has
  echo "$1: symbol $2 is in none of its NEEDED libraries (NEEDED: $3); $4"
}
getter=ANativeWindow_getBuffersDataSpace
setter=ANativeWindow_setBuffersDataSpace

# 1. The rule: every cdylib link of the runtime (a `cargo rustc` command with `--crate-type cdylib` in a runtime script,
#    the line and its continuation lines; the tests' own fixtures, linked by rustc alone, are not the runtime's) passes -Wl,-z,defs; no Cargo.toml declares a cdylib crate-type (a link of its own,
#    outside that rule).
cdylib=0
while IFS= read -r file; do
  # each command with its backslash continuations joined into one line
  while IFS= read -r command; do
    cdylib=$((cdylib + 1))
    case "$command" in *-Wl,-z,defs*) ;; *) fail "$file: a cdylib link without -Wl,-z,defs: $command" ;; esac
  done < <(sed -e ':a' -e '/\\$/N; s/\\\n//; ta' "$file" | grep -E '^[^#]*cargo rustc[^#]*--crate-type cdylib')
done < <(cd "$ROOT/runtime" && grep -rlE --include='*.sh' -e '--crate-type cdylib' . | sed -e 's|^\./||' -e "s|^|$ROOT/runtime/|")
[ "$cdylib" -ge 1 ] || fail "no cdylib link found in the runtime's scripts (liboneios-display.so has one)"
declared=$(cd "$ROOT/runtime" && grep -rlE --include=Cargo.toml '^crate-type *=.*cdylib' . || true)
[ -z "$declared" ] || fail "a Cargo.toml declares a cdylib (link it from a script with -Wl,-z,defs instead): $declared"
ok "the $cdylib cdylib link(s) of the runtime's scripts pass -Wl,-z,defs, and no Cargo.toml declares a cdylib of its own"

# 2. liboneios-display.so as runtime/display/build-android.sh builds it: -z defs, libnativewindow, elf-check passes.
CARGO_TARGET_DIR=$T/target "$ROOT/runtime/display/build-android.sh" "$T/display" > "$T/log" 2>&1 ||
  { cat "$T/log"; fail "runtime/display/build-android.sh"; }
lib=$T/display/bin/liboneios-display.so
check_passes "liboneios-display.so" "$lib"
grep -q 'NEEDED .*libnativewindow.so' "$T/log" || { cat "$T/log"; fail "liboneios-display.so does not need libnativewindow.so"; }
"$READELF" -d "$lib" | grep -q 'SONAME.*\[liboneios-display.so\]' || fail "liboneios-display.so: no soname liboneios-display.so"
ok "liboneios-display.so as build-android.sh links it: it links with -z defs, needs libnativewindow.so, and elf-check.sh passes"

# 3. The fault in a library of its own (elf-fault.rs), which does not depend on the display helper's source.
mkdir "$T/f"
link_fails "elf-fault.rs, libandroid, -z defs" "$getter" rs --crate-type cdylib "${DEFS[@]}" "$FAULT" -o "$T/f/defs.so"
ok "a shared object that calls $getter with libandroid only and -z defs: the link fails"
links "elf-fault.rs, libandroid" rs --crate-type cdylib "$FAULT" -o "$T/f/libfault.so"
fault_needed=$(needed "$T/f/libfault.so")
check_fails "elf-fault.rs, libandroid" "$T/f/libfault.so" \
  "$(missing "$T/f/libfault.so" "$getter" "$fault_needed" 'it is in libnativewindow.so')" \
  "$(missing "$T/f/libfault.so" "$setter" "$fault_needed" 'it is in libnativewindow.so')"
ok "the same without -z defs: it links, and elf-check.sh fails and names libnativewindow.so"
links "elf-fault.rs, libnativewindow" rs --crate-type cdylib "${DEFS[@]}" -C link-arg=-lnativewindow "$FAULT" -o "$T/f/good.so"
check_passes "elf-fault.rs, libnativewindow" "$T/f/good.so"
ok "the same with libnativewindow and -z defs: it links, and elf-check.sh passes"

# 4. Executables need no flag: an undefined symbol fails the link of a dynamic and of a static one.
cat > "$T/nosuch.rs" <<'EOF'
extern "C" {
    fn oneios_no_such_symbol() -> i32;
}
fn main() {
    // SAFETY: never run; the link must fail.
    std::process::exit(unsafe { oneios_no_such_symbol() });
}
EOF
link_fails "a dynamic executable, libandroid" "$getter" rs --crate-type bin --cfg elf_fault_main "$FAULT" -o "$T/f/exe"
link_fails "a dynamic executable" oneios_no_such_symbol rs "$T/nosuch.rs" -o "$T/f/exe"
link_fails "a static executable" oneios_no_such_symbol rs -C target-feature=+crt-static "$T/nosuch.rs" -o "$T/f/exe"
ok "executables with no flag: a dynamic one that calls $getter with libandroid only, a dynamic and a static one with a symbol nothing has: every link fails"
mkdir "$T/e"
links "a dynamic executable, libnativewindow" rs --crate-type bin --cfg elf_fault_main -C link-arg=-lnativewindow "$FAULT" -o "$T/e/exe"
check_passes "a dynamic executable, libnativewindow" "$T/e/exe"
grep -q '^elf-check: 1 dynamic ELF files' "$T/log" || { cat "$T/log"; fail "the dynamic executable was not checked"; }
ok "a dynamic executable with libnativewindow: it links, and elf-check.sh checks it and passes"

# 5. The NEEDED rule: a library that is no API 30 stub must be shipped; and a symbol that nothing has.
mkdir "$T/n" "$T/alone"
printf '#[no_mangle]\npub extern "C" fn oneios_fixture() -> i32 {\n    7\n}\n' > "$T/fixture.rs"
printf 'extern "C" {\n    fn oneios_fixture() -> i32;\n}\n#[no_mangle]\npub extern "C" fn oneios_user() -> i32 {\n    unsafe { oneios_fixture() }\n}\n' > "$T/user.rs"
printf 'extern "C" {\n    fn oneios_no_such_symbol() -> i32;\n}\n#[no_mangle]\npub extern "C" fn oneios_user() -> i32 {\n    unsafe { oneios_no_such_symbol() }\n}\n' > "$T/nosuchlib.rs"
links "libonefixture.so" rs --crate-type cdylib "${DEFS[@]}" -C link-arg=-Wl,-soname,libonefixture.so "$T/fixture.rs" -o "$T/n/libonefixture.so"
links "libuser.so" rs --crate-type cdylib "${DEFS[@]}" -L "$T/n" -C link-arg=-lonefixture "$T/user.rs" -o "$T/n/libuser.so"
cp "$T/n/libuser.so" "$T/alone/libuser.so"
check_fails "a NEEDED library that is not shipped" "$T/alone" \
  'libuser.so: NEEDED library libonefixture.so is not an API 30 stub of the NDK and not a library the runtime ships' \
  "$(missing libuser.so oneios_fixture "$(needed "$T/alone/libuser.so")" 'no API 30 stub and no library the runtime ships has it')"
check_passes "a NEEDED library that is shipped" "$T/n"
ok "a NEEDED library that is no API 30 stub: refused alone, accepted when the stage ships it"
links "a shared object with a symbol nothing has" rs --crate-type cdylib "$T/nosuchlib.rs" -o "$T/alone/libuser.so"
check_fails "a symbol nothing has" "$T/alone" \
  "$(missing libuser.so oneios_no_such_symbol "$(needed "$T/alone/libuser.so")" 'no API 30 stub and no library the runtime ships has it')"
ok "a symbol that no stub has: elf-check.sh fails and says so"

# 6a. A stage without the GPU's renderer is refused before anything is built (YumikoHayamato/OneiOS b7efdea).
rc=0
VIRGL=$T/no-virglrenderer XNUHOST_2D_ONLY=0 "$ROOT/runtime/build.sh" "$T/stage2d" > "$T/build2d.log" 2>&1 || rc=$?
[ "$rc" = 1 ] && grep -q "^build: no GPU renderer: $T/no-virglrenderer/src/drm/drm2kgsl is missing" "$T/build2d.log" && [ ! -e "$T/stage2d" ] ||
  { cat "$T/build2d.log"; fail "runtime/build.sh without external/virglrenderer: not refused (rc $rc)"; }
ok "runtime/build.sh STAGE without external/virglrenderer: refused (exit 1, nothing staged)"
# A tree without the checkout (a fresh worktree) still checks the links below: the 2D-only xnuhost, on purpose.
[ -f "$ROOT/external/virglrenderer/src/drm/drm2kgsl/drm2kgsl_renderer.c" ] || export XNUHOST_2D_ONLY=1
# 6. A stage of runtime/build.sh: it runs elf-check.sh itself; then the same stage with the faulty library in it.
"$ROOT/runtime/build.sh" "$T/stage" > "$T/build.log" 2>&1 || { cat "$T/build.log"; fail "runtime/build.sh"; }
for elf in liboneios-display.so oneios-audio oneios-camera camctl oneios-encoder encctl; do
  grep -q "^elf-check: bin/$elf: NEEDED " "$T/build.log" || { cat "$T/build.log"; fail "runtime/build.sh did not check bin/$elf"; }
done
grep -q '^elf-check: bin/liboneios-display.so: NEEDED .*libnativewindow.so' "$T/build.log" || fail "bin/liboneios-display.so does not need libnativewindow.so"
grep -q '^elf-check: .* 0 failures$' "$T/build.log" || { cat "$T/build.log"; fail "runtime/build.sh: no summary of elf-check.sh"; }
ok "runtime/build.sh STAGE: $(grep '^elf-check: .* 0 failures$' "$T/build.log" | sed 's/^elf-check: //')"
cp "$T/f/libfault.so" "$T/stage/bin/liboneios-display.so"
check_fails "a stage with the faulty library" "$T/stage" \
  "$(missing bin/liboneios-display.so "$getter" "$fault_needed" 'it is in libnativewindow.so')" \
  "$(missing bin/liboneios-display.so "$setter" "$fault_needed" 'it is in libnativewindow.so')"
ok "the same stage with the faulty library as bin/liboneios-display.so: elf-check.sh fails: $(grep -m1 -o "symbol $getter.*" "$T/log")"
echo "all $pass tests passed"
