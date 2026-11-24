# Credits: work ported from forks

The projects this one builds on are credited in [README.md](README.md) "Credits" and, with their licences, in
[docs/references.md](docs/references.md). This file names the people whose changes in a fork of this repository were
ported here: which commit, what it did, and where it is now. A fork of this repository is under this repository's
terms; each port's commit message names its source as well. The last section names the other iOS-on-Android ports whose code
was taken file by file (not forks of this repository).

## orumanayurpanjayat-ux/OneiOS

Authors: **leusernamealadeen** and **orumanayurpanjayat-ux**. Ported 2026-11-25 (branch `port/oruma-2026-11-24`), not
booted on a phone at the time of the port.

| Fork commit | What it did | Here |
|---|---|---|
| `84cb2ef` "Force Refresh-Rate" (leusernamealadeen) | The display helper asks surfaceflinger for the native mode and opens its refresh ranges in every takeover, not only when the size differs (One UI can leave the panel at its native resolution with app refresh capped at 60 Hz, Motion smoothness: Standard), and compares the active mode id as well as its size | Re-applied on `OneiOSDisplay.pick` (the active mode id), `isNative`, `takeoverLine`; tests in `runtime/display/tests/TestModes.java`; `runtime/display/README.md` "The panel" |
| `d157b43` "Shader-Fixes" (leusernamealadeen) | Applied the Adreno shader fixes `userland/metal/patches/metal2vulkan` 0101 and 0102 in the Metal bridge in every build | Applied behind a switch, off by default: `0110-shader-fix-switch.patch`, `userland/metal/ShaderFix.inc`, boot argument `oneios-m2v-shader-fix=1` (docs/switches.md) |
| `79cf4fb`, `d5018d0` (leusernamealadeen) | `.github/workflows/release.yml` downloads the pinned kernel from akshaynexus/OneiOS, so a fork's CI finds it | As a fallback: this repository's release of the tag first, akshaynexus/OneiOS when a fork has none (docs/releases.md) |
| `076c1a3`, `56e1430` (leusernamealadeen, orumanayurpanjayat-ux) | A Metal build job in CI, then removed again; `METAL_LICENSES_DIR` in `runtime/package.sh` | Not ported: the job is gone in the fork too, and nothing here would set `METAL_LICENSES_DIR` |

## Other ports' code taken file by file

### The sensors and auto-brightness (fix/sensors-als, 2026-11-25; host-tested and built, not booted)

| Source | Authors, licence | What was taken | Here |
|---|---|---|---|
| [DenialOfGravity/ios-redfin](https://github.com/DenialOfGravity/ios-redfin) 23019ab4 `phone/sensors/` (`sns.c`, `pb.c`, `sensor.c`, `test_sensd.c`), `phone/wifi/qmi.c` | dipa-studio (Copyright (c) 2026), MIT | the Qualcomm SEE client's message build and parse, the lookup/attribute/stream sequence and data types, the independently encoded test vectors | `runtime/bridges/sensors/` (Rust; the QRTR transport is Linux's own) |
| ios-redfin 23019ab4 `xnu/patches/1298-redfin-auto-brightness-true-tone.patch`, `0971-redfin-sensor-hid-services.patch` | dipa-studio, MIT | the last light reading re-posted within 15 ms of a wake (CoreBrightness's 250 ms first-sample timeout); the SPU usage pairs and the illuminance field | `drivers/brightness/OneiOSALSDevice.cpp`, `kernel/patches/oneios-hosted/hid/OneiOSALSEvents.cpp` |
| [Droid-VM/UniXNU-droidvm](https://github.com/Droid-VM/UniXNU-droidvm) f9121cd `package/DroidVMMotionHid.inc`, `DroidVMSensorProcessor.cpp`, `DroidVMMotion.c` | Droid-VM, BSD-2-Clause-Patent | the motion sensors as an iPad's SPU HID devices and their command channel, proven on iPadOS 27 24A446 | staged into the HID archive (`kernel/patches/oneios-hosted/hid`, `oneios-tool kernel gen hid`) |
| [UniXNU/ShengHidInput](https://github.com/UniXNU/ShengHidInput) 24f1f3f `NativeHidInput.cpp` | UniXNU, BSD-2-Clause-Patent | the event driver's vtable copy (dispatch wrapped, every slot's discriminator checked) | `kernel/patches/oneios-hosted/hid/OneiOSALSEvents.cpp` |
| [jesvi-eulah/Jesvi-iOS](https://github.com/jesvi-eulah/Jesvi-iOS) a13e406 `userland/src/inject_sensors.c` | Jesvi-iOS (HakiOS), MIT (`userland/`) | what CoreBrightness checks on an ALS service: Built-In, VendorID 0x5ac, `ALSCurveInfo`, the illuminance field 0xC000B | `drivers/brightness/OneiOSALSDevice.cpp` |
