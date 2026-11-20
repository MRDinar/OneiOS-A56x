# Devices and forks (2026-11-20)

One project, many phones. This file maps every known OneiOS device fork: what runs where,
what already flowed back into `main`, and what is still fork-only. Status as of 2026-11-20;
fork contents move fast and this file will go stale — the dates say when it was true.

Our own target stays the rooted Galaxy S24 Ultra (SM-S928U, SM8650); everything below is
judged by what it teaches `main` or what runs on more phones without breaking the S24 Ultra.

| Device | Fork(s) | Best result claimed | Ported to `main` | Still fork-only |
|---|---|---|---|---|
| Galaxy S24 Ultra (ours) | `akshaynexus/OneiOS` (this repo) | iOS 27 booted, SpringBoard, sessions with return | — (home) | — |
| Galaxy S25 Ultra (SM-S938, SM8750) | `denis0001-dev/OneiOS-S25-Ultra` — name only, zero unique commits vs our `main` (Nov 16); README still describes the S24 Ultra. Stale mirror. | — | nothing (mirror) | — |
| Galaxy S25 Ultra (continued) | `xaxaapple/s25ver` — not a GitHub fork, a Nov-16 snapshot copy (~386 files) preserving S25 work our tree never had | S25 layer (probe, Oryon topology, metal patches, port record) | Oryon CPU rule + E-lending (`topology.c`), SM8750 fixture, `tools/probe/s25-probe.sh`, `docs/s25.md` + hardware/building notes (merged 2026-11-20) | 1080x2340 fallbacks, SM-S931 phone gates (live in the OneOS fork, not here), Adreno 830 unknowns, bare-metal/Xiaomi parts (rejected in `s25.md` itself) |
| Galaxy S26 Ultra (SM-S948B, SM8850) | `YMGPwcca/OneiOS` — furthest hardware progress: iOS 27 guest boot, first frame, ~600 s run on real S26 hardware (their `docs/SM8850_LIVE_STATUS_2026-11-20.md`), via a root-only Framework-takeover experiment; OneOS app switch still blocked | 600 s on-device run | native haptics impact path, multi-phone hw-backend stand-in tests, Metal/Vulkan trace patches (in port, Nov 2026) | Framework-takeover experiment, S26-only GPU refactor (their tree deleted our 0076/0078/0080/0081 — do NOT take as-is) |
| Pixel 11 Pro (Tensor, claim) | `mangosaretasty/PixeliOS` — zero unique commits (Nov 14); README still S24 Ultra. Stale mirror. | — | nothing (mirror) | — |
| Galaxy A56 (SM-A566E, Exynos 1580) | `Musicalista/OneiOS-A56` — iOS 16.5 userland to lock screen on device (Nov 18) by mixing 24A446 kexts + hosted patches 0067–0073 | 16.5 lock screen on A56 | kernel 0068/0072–0074/0077 + 0004/0029 deltas, `ONEIOS_CRYPTO=original` (opt-in), version-gated `RAMDisk1`, roottweak 20F66 boot-args, amdgpu staged-GPU3D + golden tests, `emit.rs` no-seal, A56 profiles (reference), `tools/test_a56_gpu.c`, `docs/a56.md` + `docs/a56-oneos-fork.md` (ported 2026-11-21, host-tested, no phone/kernel proof yet) | switch script + Manager app (OneOS-fork proposal, `docs/a56-oneos-fork.md`), touch, S24-untested `original` crypto, clang-wrapper flags |
| Galaxy A53 | — | no A53 fork found (searched 2026-11-20) | — | — |

## What the forks already made better (in `main`)

- **Oryon CPU topology** (s25ver): Qualcomm implementer `0x51` is never E; all-performance hosts lend the slowest
  cores as E vCPUs. A latent wrong-topology bug on every big-only SoC, not just the S25; S24 behavior unchanged.
- **Multi-phone backend tests** (YMGPwcca via the hw-backend merge): samsung vs treble backends against stand-in
  sysfs/binderfs, the harness every new phone reuses.
- **Native haptics impact path** (YMGPwcca, in port): discrete taps instead of chopped VIB segments, negotiated
  (`features=0` keeps old hosts working).
- **Phone probe tooling** (`tools/probe/s25-probe.sh`): read-only fact-gatherer; the first step of any new port.
- **Lasting trace tooling** (metal/Vulkan traces, sandbox-exec triage): off-by-default debug aids, per the
  fix-plus-tool rule.
- **iOS 16 compat shims, version-gated** (A56, `docs/a56.md`): the 16.5 user ABI repairs (0072 swap/surface-info
  rewrite, 0073 mac-exec triage, 0077 legacy bootstrap replies, 0074 shared-cache check, 0068 registered ports),
  the loader's `RAMDisk1` for 20F66 images only, roottweak's 20F66 boot-args, and the amdgpu staged-GPU3D branch.
  All default-off or size-triggered; the 27 path is unchanged.

## How one tree supports many phones (the pattern)

- **Backends, not forks**: `runtime/bridges/hw` (`hw-backend=samsung|treble`, per-bridge overrides) plus the
  stand-in tests above. A new phone starts by proving its backend against fakes, not by forking the repo.
- **Per-device records**: `docs/s25.md` is the template (F/I/U-marked device table, verdict table, probe→decision
  matrix). A port lands as a record plus its profile/config, not as a parallel universe.
- **Per-device config**: `userland/config/device-iphone18,2.toml` is the guest-side pattern; the host side follows
  the same shape as backends grow.
- **What a new port still needs each time**: panel mode/refresh (display helper XHDP report), input/evdev
  capabilities, sensor/HAL paths (backend stand-ins first), GPU backend notes (AGX gate pattern in `docs/gpu.md`),
  kernel notes only where the SoC differs (the Oryon rule is the template: describe, test, keep old phones unchanged).

Guesses, marked: fork "best results" are the forks' own claims (log/session references inside their docs where
given); nothing here was re-run on our side except the ported items' host tests. The A53 row is empty — point us at
a fork if one exists.
