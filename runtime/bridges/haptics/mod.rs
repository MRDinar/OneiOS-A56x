// SPDX-License-Identifier: MIT
//! haptics/: the haptic actuator (docs/hardware.md 5): the evdev force-feedback device (samsung), the vibrator HAL
//! (treble) or the stand-in (haptics=sim), behind one worker.

pub mod haptic; // haptics/haptic.c: the worker, the mapping, the counters, the evdev and stand-in backends
mod vibrator; // haptics/haptic.c: the vibrator HAL backend

pub use haptic::{find, map, set_native_impacts, Errno, Haptic, HapticBackend, HapticStats, Impact};

#[cfg(test)]
pub(crate) mod test_haptic; // tests/test_bridges.c's test_haptic
