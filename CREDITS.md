# Credits: work ported from forks

The projects this one builds on are credited in [README.md](README.md) "Credits" and, with their licences, in
[docs/references.md](docs/references.md). This file names the people whose changes in a fork of this repository were
ported here: which commit, what it did, and where it is now. A fork of this repository is under this repository's
terms; each port's commit message names its source as well.

## orumanayurpanjayat-ux/OneiOS

Authors: **leusernamealadeen** and **orumanayurpanjayat-ux**. Ported 2026-11-25 (branch `port/oruma-2026-11-24`), not
booted on a phone at the time of the port.

| Fork commit | What it did | Here |
|---|---|---|
| `84cb2ef` "Force Refresh-Rate" (leusernamealadeen) | The display helper asks surfaceflinger for the native mode and opens its refresh ranges in every takeover, not only when the size differs (One UI can leave the panel at its native resolution with app refresh capped at 60 Hz, Motion smoothness: Standard), and compares the active mode id as well as its size | Re-applied on `OneiOSDisplay.pick` (the active mode id), `isNative`, `takeoverLine`; tests in `runtime/display/tests/TestModes.java`; `runtime/display/README.md` "The panel" |
| `d157b43` "Shader-Fixes" (leusernamealadeen) | Applied the Adreno shader fixes `userland/metal/patches/metal2vulkan` 0101 and 0102 in the Metal bridge in every build | Applied behind a switch, off by default: `0110-shader-fix-switch.patch`, `userland/metal/ShaderFix.inc`, boot argument `oneios-m2v-shader-fix=1` (docs/switches.md) |
| `79cf4fb`, `d5018d0` (leusernamealadeen) | `.github/workflows/release.yml` downloads the pinned kernel from akshaynexus/OneiOS, so a fork's CI finds it | As a fallback: this repository's release of the tag first, akshaynexus/OneiOS when a fork has none (docs/releases.md) |
| `076c1a3`, `56e1430` (leusernamealadeen, orumanayurpanjayat-ux) | A Metal build job in CI, then removed again; `METAL_LICENSES_DIR` in `runtime/package.sh` | Not ported: the job is gone in the fork too, and nothing here would set `METAL_LICENSES_DIR` |

## Droid-VM/UniXNU-droidvm (upstream, not a fork)

An upstream project (credited with its licence in docs/references.md); listed here because a change of it newer than
our pin was ported by itself. Authors: **lateautumn233** and **HuJK** (BSD-2-Clause-Patent). Ported 2026-11-25 (branch
`fix/codec-irq`), not booted on a phone at the time of the port.

| Upstream commit | What it did | Here |
|---|---|---|
| `96e8c1f` "virtio-media: completion interrupts instead of polling" (lateautumn233) | `DroidVMVirtioMedia.c`: the device's level SPI attached, the event worker and command waiters woken by it instead of a 1 ms poll; events copied under the transport's lock and delivered without it | Edit 7 of the codec archive (`tools/oneios-tool/crates/oneios-kernel/src/staged/codec_transport.rs`), with a guard timeout and the command queue kept `NO_INTERRUPT` (xnuhost answers commands inside the notify, and honours the flag on that device: `Device::quiet_queues`); boot argument `oneios-codec-irq=0` keeps the poll (docs/switches.md) |
| `2cd4c8e` "either ADT name for the window's interrupt, and polling where there is none" (HuJK) | The INTID from `droidvm,intid` or `gic-spi`; a window with neither, or an SPI that cannot be attached, polled as before | The same, with `interrupts` too (`OneiOSCodecWait.h` `oneios_media_polled_why`) |
