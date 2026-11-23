// SPDX-License-Identifier: MIT
//! The vibrator HAL backend (treble) of the haptics bridge (C: haptics/haptic.c, its second half).
//!
//! The effect is a length and a level: on(length) then, with amplitude control, setAmplitude(level / 255 x gain), as
//! AOSP's vibrator service orders them. The period has no place in IVibrator's on(): the HAL plays at its own resonance.

use super::haptic::{Errno, Haptic, HapticBackend, Impact, LEVEL_FLOOR, LEVEL_MAX};
use crate::hal::{self, aidl, Hal};

struct Vibrator {
    hal: Hal,
    caps: i32,
    length_ms: u32,
    magnitude: u32,
    gain: u32,
    /// validated at open, only with explicit opt-in
    preset_mask: u32,
    /// opt-in inspection must never emit an unused off()
    native_requested: bool,
    native_enabled: bool,
    continuous: bool,
}

/// A HAL call's negative errno as a positive one.
fn errno(rc: i32) -> Errno {
    -rc
}

impl Vibrator {
    fn simple(&self, code: u32) -> Result<(), Errno> {
        self.hal.call_void(code, &hal::parcel(hal::VIBRATOR)).map_err(errno)
    }
}

/// getSupportedEffects' reply: the effects below 32 as a mask.
fn take_effects(r: &mut hal::Reader<'_>) -> Result<u32, i32> {
    let n = r.i32();
    if r.is_bad() || !(0..=32).contains(&n) {
        return Err(-libc::EPROTO);
    }
    let mut mask = 0u32;
    for _ in 0..n {
        let e = r.i32();
        if (0..32).contains(&e) {
            mask |= 1 << e;
        }
    }
    if r.is_bad() {
        Err(-libc::EPROTO)
    } else {
        Ok(mask)
    }
}

impl HapticBackend for Vibrator {
    fn upload(&mut self, _id: i32, length_ms: u32, _period_ms: u32, magnitude: u32) -> Result<i32, Errno> {
        self.length_ms = length_ms;
        self.magnitude = magnitude;
        Ok(0)
    }

    fn play(&mut self, _id: i32, on: bool) -> Result<(), Errno> {
        if !on {
            self.simple(hal::VIB_OFF)?;
            self.continuous = false;
            return Ok(());
        }
        let mut p = hal::parcel(hal::VIBRATOR);
        p.put_i32(self.length_ms as i32);
        p.put_null_binder(); // no callback: the worker does not wait for the end
        self.hal.call_void(hal::VIB_ON, &p).map_err(errno)?;
        self.continuous = true; // even if a later setAmplitude() fails
        if self.caps & hal::VIB_CAP_AMPLITUDE == 0 {
            return Ok(());
        }
        // The HAL refuses 0; the level floor keeps it above 0.01 anyway.
        let amplitude = (self.magnitude as f32 / LEVEL_MAX as f32 * self.gain as f32 / 10000.0).clamp(0.01, 1.0);
        let mut p = hal::parcel(hal::VIBRATOR);
        p.put_f32(amplitude);
        self.hal.call_void(hal::VIB_SET_AMPLITUDE, &p).map_err(errno)
    }

    fn gain(&mut self, gain: u32) -> Result<(), Errno> {
        self.gain = gain.min(10000);
        Ok(())
    }

    fn has_impact(&self) -> bool {
        true
    }

    /// Explicit one-shot impact: the calibrated vendor preset is separate from the legacy duration/frequency path. No
    /// EVIOCSFF or Samsung trigger indices.
    fn impact(&mut self, kind: Impact) -> Result<u32, Errno> {
        if !self.native_enabled {
            return Err(libc::EOPNOTSUPP);
        }
        let (effect, strength) = match kind {
            Impact::Light => (hal::VIB_EFFECT_TICK, hal::VIB_STRENGTH_LIGHT),
            Impact::Medium => (hal::VIB_EFFECT_CLICK, hal::VIB_STRENGTH_MEDIUM),
            Impact::Heavy => (hal::VIB_EFFECT_HEAVY_CLICK, hal::VIB_STRENGTH_STRONG),
        };
        if self.preset_mask & (1 << effect) == 0 {
            return Err(libc::EOPNOTSUPP);
        }
        let mut p = hal::parcel(hal::VIBRATOR);
        p.put_i32(effect);
        p.put_i32(strength);
        p.put_null_binder();
        let duration = self.hal.call(hal::VIB_PERFORM, &p, aidl::take_i32).map_err(errno)?;
        u32::try_from(duration).ok().filter(|d| (1..=1000).contains(d)).ok_or(libc::EPROTO)
    }
}

impl Drop for Vibrator {
    fn drop(&mut self) {
        // Keep legacy shutdown behavior when native mode was not requested. During explicit native opt-in, closing an
        // unused or completed bridge must NOT send off(): One UI could currently own the same actuator. A continuous
        // on(ms) started by this bridge still needs to be stopped.
        if !self.native_requested || self.continuous {
            let _ = self.simple(hal::VIB_OFF);
        }
    }
}

/// The bridge on the vibrator HAL (`binder`: None, ONEIOS_BINDER or /dev/binder). Err(why) when the HAL is not there.
pub(crate) fn open_hal(binder: Option<&str>) -> Result<Haptic, String> {
    let hal = Hal::open(binder, hal::VIBRATOR_INSTANCE, hal::VIBRATOR)?;
    let caps = hal
        .call(hal::VIB_GET_CAPABILITIES, &hal::parcel(hal::VIBRATOR), aidl::take_i32)
        .map_err(|rc| format!("{}: getCapabilities: {}", hal::VIBRATOR_INSTANCE, hal::error_text(rc)))?;
    let version = hal.version();
    let mut v = Vibrator {
        hal,
        caps,
        length_ms: 0,
        magnitude: 0,
        gain: 10000,
        preset_mask: 0,
        native_requested: super::haptic::native_impacts_wanted(),
        native_enabled: false,
        continuous: false,
    };
    if v.native_requested {
        let required = 1 << hal::VIB_EFFECT_TICK | 1 << hal::VIB_EFFECT_CLICK | 1 << hal::VIB_EFFECT_HEAVY_CLICK;
        match v.hal.call(hal::VIB_SUPPORTED_EFFECTS, &hal::parcel(hal::VIBRATOR), take_effects) {
            Ok(mask) if mask & required == required => {
                v.preset_mask = mask;
                v.native_enabled = true;
            }
            got => crate::br_error!(
                "haptics: native impacts unavailable: {} (supported preset mask {}), continuous path unchanged",
                match got {
                    Err(rc) => hal::error_text(rc),
                    Ok(_) => "not all TICK/CLICK/HEAVY_CLICK presets advertised".to_owned(),
                },
                c_hash_x(got.unwrap_or(0))
            ),
        }
    }
    let enabled = v.native_enabled;
    let h =
        Haptic::open_named(Box::new(v), "the vibrator HAL", enabled).ok_or_else(|| "no worker thread".to_owned())?;
    if enabled {
        crate::br_info!("haptics: native impacts enabled (explicit opt-in; TICK/CLICK/HEAVY_CLICK preflight PASS)");
    }
    crate::br_info!(
        "haptics: on: the vibrator HAL {} (version {version}, capabilities 0x{:x}: {}; level {LEVEL_FLOOR}..{LEVEL_MAX} as \
         the amplitude)",
        hal::VIBRATOR_INSTANCE,
        caps as u32,
        if caps & hal::VIB_CAP_AMPLITUDE != 0 {
            "amplitude control"
        } else {
            "no amplitude control, every effect at the HAL's own strength"
        }
    );
    Ok(h)
}

/// C's "%#x": "0" for zero, else "0x..." (Rust's {:#x} writes "0x0").
fn c_hash_x(v: u32) -> String {
    if v == 0 {
        "0".to_owned()
    } else {
        format!("{v:#x}")
    }
}
