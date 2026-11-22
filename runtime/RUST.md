# The runtime in Rust (branch `rust-runtime`)

The host side of OneiOS (xnuhost, its loader, the bridges, the display helper, the package's small programs) is being
rewritten from C to Rust, one C file to one Rust module, in the Cargo workspace `runtime/Cargo.toml`. This file is the
rule book for everyone working on it, and the map of who owns what until the C is gone.

What stays C: the hosted kernel's patches (`kernel/patches/`), the kernel's drivers (`drivers/`, built with XNU's own
compile commands), the guest helpers (`userland/`, built for iOS), the guest-facing headers that kernel C includes
(`runtime/xnuhost/include/xnuhost/*.h`: hostcall.h, pixels.h, free_report*.h; `drivers/*/*_proto.h`) and other
projects' pinned code (virglrenderer's drm2kgsl, compiled by `xnuhost/build.rs`). The Rust side mirrors each shared
header and has a test that reads the C header and checks every constant, size and offset it mirrors.

## Crates

| Crate (folder) | Ports | Owner |
|---|---|---|
| `xh-core` (`core/`) | xnuhost's `log.c`, `virtio.c`, `mmio.c`, `insn.c`, the registers, `pixels.h`; the interrupt and guest-RAM hooks the devices call; shared helpers (sysfs/proc reads, number parsing, fds, threads) | core |
| `xnuhost-abi` (`abi/`, no_std) | `hostcall.h`, `stub_abi.h`, `free_report*.h` | boot |
| `xnuloader` (`loader/`) | `runtime/loader` (+ `xnuloader-inspect`, `mkkernel`) | boot |
| stub (`xnuhost/stub/`, own workspace, aarch64-unknown-none) | `stub.c`, `stub.ld` | boot |
| test kernel (`xnuhost/testkernel/`, own workspace) | `tests/testkernel.c`, `testkernel.ld` | boot |
| `oneios-display` (`display/`) | `runtime/display/src` (xhd, launcher, jni, frame_perf, displayctl) and its tests | display |
| `onebridges` (`bridges/`) `wifi`, `bt`, `hal` | `bridges/wifi`, `bridges/bt`, `bridges/hal`, wifictl, their tests, fake-hal | bridges-net |
| `onebridges` (`bridges/`) the rest | `cpu`, `hw`, `haptics`, `net`, `audio` (+ oneios-audio), `brightness`, `battery`, `thermal`, `devices`, `proto`, bridgectl/brightctl/batteryctl/thermalctl, `test_bridges` | bridges-hw |
| `onecamera`, `oneencoder` | `bridges/camera`, `bridges/encoder` | media |
| `xnuhost` (`xnuhost/`) machine | `arch_aarch64.S`, `cpu`, `task`, `sysreg`, `gic`, `hostcalls`, `kctx`, `mem`, `discard`, `ramfix`, `uart`, `perf`; `xnuhost/build.rs`, `xnuhost/Cargo.toml`; the qemu boots of the test kernel (`check-arm64`, bench, parallel, rpc) | machine |
| `xnuhost` session | `main.c` (`session.rs`, `main.rs`), `config`, `imageset`, `blk`, `virtio_blk`, `usb`, `ready`, `ret` (return.c); `tests/test_imageset.c` | session |
| `xnuhost` devices; `drm2kgsl-sys` (`xnuhost/drm2kgsl/`) | `display`, `virtio_gpu`, `gpu3d`, `gpu3d_kgsl`, `input`, `input_script`, `pen`, `virtio_input`, `tools/gpuctl.c`, the drm2kgsl build; `tests/test_display_mode.c`, `test_drm2kgsl.c`, `test_gpu_leaks.c`, `test_gpu_policy.c` | devices |
| `oneios-package-tools` (`package/`) | `runtime/package/*.c` | lead |

A folder's owner is the only one who edits it. Anyone may add a new helper module to `xh-core` (a new file and its one
`pub mod` line in `core/src/lib.rs`) when two crates need the same code; tell the core owner in the commit message.

## How to port

- **1:1 in behaviour.** Every setting and its default, every file path, every protocol layout, every timing constant,
  every exit code, and **every log line's text, byte for byte** (`oneios-tool log`, the docs and the phone tests grep
  them). A C bug found on the way: fix it in Rust and say so in the commit message.
- **Every C test becomes a Rust test** with the same cases and expectations (`#[cfg(test)] mod tests` beside the code,
  or `tests/*.rs` declared in Cargo.toml: `autotests = false`). A test that needs a phone, binderfs or root skips with a
  printed reason, as the C ones did.
- **Clean, DRY Rust, not C in Rust's clothes.** Enums for states and codes, `Result` for failures, `OwnedFd`/`Drop` for
  resources, slices for buffers, `std::sync` and atomics for shared state (a global only where the C design really
  needs one: `OnceLock`, `Mutex`). Code two modules need goes once into `xh-core` (or the crate's own `util`).
  `unsafe` only at the system boundary (syscalls, mmap'd guest RAM, the vCPU's context switch), each block with a
  `// SAFETY:` comment, wrapped in a safe function that the rest calls.
- Comments say what the C comments said (the why, the phone's numbers, the dates); keep them, shortened where Rust
  says it already.
- Dependencies: `libc` and the workspace crates. Another crate only when it replaces a lot of hand-written unsafe
  code; say why in Cargo.toml.
- `cargo fmt` (runtime/rustfmt.toml: width 120) and `cargo clippy --all-targets -- -D warnings` clean for your crate.

## Working on one branch, several at once

- Everyone works in this one checkout on branch `rust-runtime`: no worktrees, no other branches, no `git stash`,
  `git checkout -- <path>`, `git reset`, `git rebase`, or `git add -A`. Commit only your own paths:
  `git commit -m "..." -- <your paths>`. If `index.lock` is busy, run the commit again. The lead pushes.
- Build and test with your own target dir, so that cargo's lock never makes two people wait:
  `CARGO_TARGET_DIR=runtime/target/<owner>` (e.g. `cd runtime && CARGO_TARGET_DIR=target/core cargo test -p xh-core`).
- The phone's binaries: `cargo build --release --target aarch64-linux-android -p <crate>` (the linker is the NDK's
  clang, `runtime/ndk-cc`). A static one (xnuhost, the phone checks): add
  `--config 'target.aarch64-linux-android.rustflags=["-C","target-feature=+crt-static"]'` and a target dir of its own
  (`target/<owner>-static`). Static arm64 binaries run here under `qemu-aarch64`.
- Leave the C files and Makefiles in place: the lead switches `runtime/build.sh`, the package and CI to cargo and
  removes the C once every part has its Rust and its tests pass.
- The owner's rules (CLAUDE.md) hold: no Python; no `sleep` and no `until`/`for` wait loops (a long job runs as a
  background task and you wait for its notice); no Apple files in git; the way back to One UI must always work (never
  grab the side keys, run realtime threads, leave the namespace or cgroup, lower the OOM score, write under
  `/sys/power` or reboot). Every change is a fix or a lasting tool, never a one-boot probe.
