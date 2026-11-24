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
