# Devices and forks (2026-11-20)

One project, many phones. This file maps every known OneiOS device fork: what runs where,
what already flowed back into `main`, and what is still fork-only. Status as of 2026-11-21;
fork contents move fast and this file will go stale — the dates say when it was true.

Our own target stays the rooted Galaxy S24 Ultra (SM-S928U, SM8650); everything below is
judged by what it teaches `main` or what runs on more phones without breaking the S24 Ultra.

| Device | Fork(s) | Best result claimed | Ported to `main` | Still fork-only |
|---|---|---|---|---|
| Galaxy S24 Ultra (ours) | `akshaynexus/OneiOS` (this repo) | iOS 27 booted, SpringBoard, sessions with return | — (home) | — |
| Galaxy S25 Ultra (SM-S938, SM8750 — denis branch; base S25 SM-S931B pa1q — s25ver snapshot) | `denis0001-dev/OneiOS-S25-Ultra` branch `device/s25-ultra` — 2 commits (Adreno 830 fences, SM8750 host profile; fork claims SpringBoard on SM-S938B, no return/frame-rate/app-switch evidence; fork `main` is a pure upstream mirror plus ~40 upstream-mirror `fix-*` branches, not S25 work). [s25ver snapshot](https://github.com/xaxaapple/s25ver) — single-commit base-S25 layer plus `xaxaapple-patch-1` trivia | fork-claimed SpringBoard sessions with a fork-built `xnuhost` (not re-run here) | Adreno 830 wait-timestamp default (`0007-a830`), `virgl_fence_table_init`, host profile, verify/ssh scripts, dropbear/zsh builders, S25 record (ported 2026-11-21) | its `topology.c`/`test_bridges.c` (predate our Oryon rule), `ONEIOS_XNUHOST` dead line, Makefile deletions, README framing; OneOS phone gates (other repo); on-device proof |
| Galaxy S25 (base, SM-S931B pa1q — s25ver snapshot) | `xaxaapple/s25ver` — not a GitHub fork, a Nov-16 snapshot copy (~386 files) preserving S25 work our tree never had | S25 layer (probe, Oryon topology, metal patches, port record) | Oryon CPU rule + E-lending (`topology.c`), SM8750 fixture, `tools/probe/s25-probe.sh`, `docs/s25.md` + hardware/building notes (merged 2026-11-20) | 1080x2340 fallbacks, SM-S931 phone gates (live in the OneOS fork, not here), Adreno 830 unknowns, bare-metal/Xiaomi parts (rejected in `s25.md` itself) |
| Galaxy S26 Ultra (SM-S948B, SM8850) | `YMGPwcca/OneiOS` — iOS 27 guest boot, first frame, ~600 s run on real S26 hardware (their `docs/SM8850_LIVE_STATUS_2026-11-20.md`), via a root-only Framework-takeover experiment; OneOS app switch still blocked; return/watchdog handback explicitly NOT verified. Open PRs: #1 (SM8850 input/brightness/display diagnostics, on-phone PASSes) and #8 (thermal P0.1 preflight evidence, draft, not accepted for another run) | 600 s on-device run | native haptics impact path, multi-phone hw-backend stand-in tests, Metal/Vulkan trace patches (all in `port-metal-haptics`, unmerged; haptics delivery itself NOT physically verified — needs a safety-qualified session) | Framework-takeover experiment, S26-only GPU refactor (their tree deleted our 0076/0078/0080/0081 — do NOT take as-is) |
| Pixel 11 Pro (Tensor, claim) | `mangosaretasty/PixeliOS` — zero unique commits (Nov 14); README still S24 Ultra. Stale mirror. | — | nothing (mirror) | — |
| Galaxy A56 (SM-A566E, Exynos 1580) | `Musicalista/OneiOS-A56` — iOS 16.5 userland "starts to its UI" on device (Nov 18) by mixing 24A446 kexts + hosted patches 0067–0073 (fork's words: UI start on the panel; touch NOT working; their README screenshots still say boot stops at the logo — inconsistent). Also ships an `ios-27.0.1-a56.toml` profile and a Manager APK with GitHub download | UI start on A56 (our gloss: lock-screen-class; imprecise — fork doesn't claim more) | virtio_gpu adaptation template + golden tests, self-extracting APK bootstrap, AGX gate + mac-exec triage as doc/off-by-default tooling; no-source corecrypto mode held for a licence call | 16.5/A56 specifics, Adreno-only GPU code |
| Galaxy A53 | — | no A53 fork found (searched 2026-11-20; re-checked 2026-11-21) | — | — |
| (mirror, no device) | `gageconnor/OneiOS` — pure upstream mirror, 0 unique commits | — | nothing | — |

## What the forks already made better (in `main`)

- **Oryon CPU topology** (s25ver): Qualcomm implementer `0x51` is never E; all-performance hosts lend the slowest
  cores as E vCPUs. A latent wrong-topology bug on every big-only SoC, not just the S25; S24 behavior unchanged.
- **Multi-phone backend tests** (YMGPwcca via the hw-backend merge): samsung vs treble backends against stand-in
  sysfs/binderfs, the harness every new phone reuses.
- **Native haptics impact path** (YMGPwcca, in `port-metal-haptics`, unmerged — guest→HAL delivery NOT physically
  verified upstream): discrete taps instead of chopped VIB segments, negotiated (`features=0` keeps old hosts working).
- **Phone probe tooling** (`tools/probe/s25-probe.sh`): read-only fact-gatherer; the first step of any new port.
- **Adreno 830 fence default** (denis S25 Ultra): `WAITTIMESTAMP_CTXTID` everywhere until the FENCE path is
  proven on SM8750 (`NCTX_NO_WAITTS` for A/B); plus the missing `virgl_fence_table_init` and a `--forward`
  patch-loop re-run. S24 behavior unchanged (FENCE path was already unused there by default... on non-A830
  the default was chip-gated; now uniform — same effective path for the S24's Adreno 750).
- **S25 Ultra host record + session scripts** (denis S25 Ultra): `profiles/host-galaxy-s25-ultra-sm8750.toml`,
  `scripts/port/verify-host.sh` (read-only preflight), `scripts/phone/ssh-forward.sh` (guest shell relay),
  in-tree arm64e `build-dropbear.sh`/`build-zsh.sh`.
- **Lasting trace tooling** (in `port-metal-haptics`, unmerged: metal/Vulkan traces, sandbox-exec triage 0073):
  off-by-default debug aids, per the fix-plus-tool rule.

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
