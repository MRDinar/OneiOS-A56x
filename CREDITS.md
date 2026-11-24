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

## YumikoHayamato/OneiOS

The Galaxy S25 Ultra fork (branch `s25-ultra-sm8750`). Author: **YumikoHayamato** (commits signed "Stiven"). Ported
2026-11-25 (branch `fix/s24-fps-heat-camera`), not booted on a phone at the time of the port.

| Fork commit | What it did | Here |
|---|---|---|
| `cdb297bbf` "native path, step 6" (the display part) | With `ONEIOS_DISPLAY_PRESENT=ahb` the display helper posts its frames in three write-only AHardwareBuffers (`CPU_WRITE_RARELY`) on a child of its layer with ASurfaceTransaction, each filled with the damage since it was last filled, at most two in flight, refilled after its release fence; the window path kept when anything is missing. Measured on the S25: 85% of the helper's time had been the CPU caches' maintenance of the window's whole buffer at each lock and post; 60 frames a second at 49.5% of a core became 114 at 17.1% | Rewritten in Rust on the run loop's `Layer` (`runtime/display/src/xhd/present.rs`, the NDK half in `src/jni.rs`), behind xnuhost's `display-present=ahb` (default) through the `options` message (docs/switches.md); the child made from the layer's window rather than by the Java half; host tests `test_present_*`. Their copy-loop floor (`ONEIOS_DISPLAY_UCLAMP_MIN`), the frame-time line, `displayctl pool` and the PL030 kernel patch of the same commit are not ported (a VM's path) |
