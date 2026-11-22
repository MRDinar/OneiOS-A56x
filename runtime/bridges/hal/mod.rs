// SPDX-License-Identifier: MIT
//! The treble backend's HAL client: a stable AIDL HAL asked for by name from servicemanager (handle 0 of /dev/binder,
//! IServiceManager.getService), its descriptor and interface version checked, then plain calls. No Android library:
//! the binder layer is [`binder`] (shared with the Bluetooth HAL's HIDL, [`hidl`]) and [`aidl`]. And the parcels of
//! the HALs the bridges use, both ways (the encoders serve the fake HAL of the tests and the stand-in phone,
//! [`fake`]).
//!
//! The method codes are the order of each interface's frozen AIDL file in AOSP (hardware/interfaces/.../aidl): a
//! stable AIDL interface only appends methods, so they hold for every version. [I] until a phone answers: the open
//! checks the descriptor (`_NTF`) and logs the version, so a wrong object is refused before any call.
//!
//! ```ignore
//! use onebridges::hal::{self, Hal};
//! let hal = Hal::open(None, hal::HEALTH_INSTANCE, hal::HEALTH)?;          // Err(why) for the log line
//! let info = hal.call(hal::HEALTH_GET_INFO, &hal::parcel(hal::HEALTH), hal::HealthInfo::decode)?;
//! // a failure is a negative errno: hal::error_text(rc) for the log line
//! ```
//! C: hal/hal.h, hal/hal.c.

pub mod aidl;
pub mod binder;
pub mod binderfs;
pub mod fake;
pub mod hidl;

pub use aidl::{CallError, Parcel, Reader};

pub const SERVICE_MANAGER: &str = "android.os.IServiceManager";
pub const SM_GET_SERVICE: u32 = 1; // IServiceManager.getService(String): IBinder

// android.hardware.health.IHealth (health/aidl/android/hardware/health/IHealth.aidl)
pub const HEALTH: &str = "android.hardware.health.IHealth";
pub const HEALTH_INSTANCE: &str = "android.hardware.health.IHealth/default";
pub const HEALTH_GET_INFO: u32 = 12; // getHealthInfo(): HealthInfo (after registerCallback .. getDiskStats)
                                     // android.hardware.thermal.IThermal (thermal/aidl/android/hardware/thermal/IThermal.aidl)
pub const THERMAL: &str = "android.hardware.thermal.IThermal";
pub const THERMAL_INSTANCE: &str = "android.hardware.thermal.IThermal/default";
pub const THERMAL_GET_TEMPERATURES: u32 = 3; // getTemperatures(): Temperature[]
                                             // android.hardware.vibrator.IVibrator (vibrator/aidl/android/hardware/vibrator/IVibrator.aidl)
pub const VIBRATOR: &str = "android.hardware.vibrator.IVibrator";
pub const VIBRATOR_INSTANCE: &str = "android.hardware.vibrator.IVibrator/default";
pub const VIB_GET_CAPABILITIES: u32 = 1; // getCapabilities(): int
pub const VIB_OFF: u32 = 2; // off()
pub const VIB_ON: u32 = 3; // on(int timeoutMs, IVibratorCallback callback): callback null here
pub const VIB_PERFORM: u32 = 4; // perform(Effect, EffectStrength, IVibratorCallback): duration in ms
pub const VIB_EFFECT_CLICK: i32 = 0; // android.hardware.vibrator.Effect.CLICK
pub const VIB_EFFECT_TICK: i32 = 2; // android.hardware.vibrator.Effect.TICK
pub const VIB_EFFECT_HEAVY_CLICK: i32 = 5; // android.hardware.vibrator.Effect.HEAVY_CLICK
pub const VIB_STRENGTH_LIGHT: i32 = 0; // android.hardware.vibrator.EffectStrength.LIGHT
pub const VIB_STRENGTH_MEDIUM: i32 = 1; // android.hardware.vibrator.EffectStrength.MEDIUM
pub const VIB_STRENGTH_STRONG: i32 = 2; // android.hardware.vibrator.EffectStrength.STRONG
pub const VIB_SUPPORTED_EFFECTS: u32 = 5; // getSupportedEffects(): Effect[] (read only)
pub const VIB_SET_AMPLITUDE: u32 = 6; // setAmplitude(float 0 < a <= 1)
pub const VIB_COMPOSE_DELAY_MAX: u32 = 8; // getCompositionDelayMax(): int (read only)
pub const VIB_COMPOSE_SIZE_MAX: u32 = 9; // getCompositionSizeMax(): int (read only)
pub const VIB_SUPPORTED_PRIMITIVES: u32 = 10; // getSupportedPrimitives(): CompositePrimitive[] (read only)
pub const VIB_PRIMITIVE_DURATION: u32 = 11; // getPrimitiveDuration(CompositePrimitive): int (read only)
pub const VIB_CAP_AMPLITUDE: i32 = 4; // IVibrator.CAP_AMPLITUDE_CONTROL
pub const VIB_CAP_COMPOSE: i32 = 32; // IVibrator.CAP_COMPOSE_EFFECTS

/// A call's parcel: the interface token.
pub fn parcel(descriptor: &str) -> Parcel {
    Parcel::with_token(descriptor)
}

/// A few words for a log line about a failed call (a negative errno).
pub fn error_text(rc: i32) -> String {
    match -rc {
        libc::ENOENT => "not registered with servicemanager, or SELinux denies the lookup".to_owned(),
        libc::ETIMEDOUT => "no answer in 5 s".to_owned(),
        libc::EPIPE => "the HAL's process died".to_owned(),
        libc::EREMOTEIO => "the HAL answered with an exception".to_owned(),
        libc::EPROTO => "a reply this client cannot read".to_owned(),
        _ => binder::strerror(rc),
    }
}

/// A connection to one HAL.
pub struct Hal {
    conn: aidl::Connection,
    handle: u32,
    version: i32,
}

impl Hal {
    /// Opens the binder device (None: ONEIOS_BINDER, else /dev/binder), asks servicemanager for `instance` (no wait:
    /// a HAL that is not registered is missing), checks that it answers `descriptor`. Err: the reason, for the log.
    pub fn open(device: Option<&str>, instance: &str, descriptor: &str) -> Result<Hal, String> {
        let conn = aidl::Connection::open(device, None, None).map_err(|e| {
            let path = device.map_or_else(aidl::default_device, str::to_owned);
            format!("{instance}: no binder device ({path}: {})", binder::strerror(e.errno()))
        })?;
        let mut p = parcel(SERVICE_MANAGER);
        p.put_str(instance);
        let handle = conn
            .call(0, SM_GET_SERVICE, &p, |r| aidl::take_handle(&conn, r))
            .map_err(|e| format!("{instance} (no service: {})", error_text(e.rc)))?;
        let mut hal = Hal { conn, handle, version: 0 };
        let named = hal.conn.call(handle, aidl::INTERFACE, &Parcel::new(), |r| r.string(95).ok_or(-libc::EPROTO));
        match named {
            Err(e) => return Err(format!("{instance} ({})", error_text(e.rc))),
            Ok(name) if name != descriptor => {
                return Err(format!("{instance} (it is not a service of that interface)"))
            }
            Ok(_) => {}
        }
        hal.version = hal.conn.call(handle, aidl::GET_VERSION, &parcel(descriptor), aidl::take_i32).unwrap_or(0);
        Ok(hal)
    }

    /// The interface version it reported (0: none).
    pub fn version(&self) -> i32 {
        self.version
    }

    /// A call: the reply's value, or a negative errno (-EPIPE: the HAL's process died; -EREMOTEIO: it answered
    /// with an exception).
    pub fn call<T>(
        &self,
        code: u32,
        p: &Parcel,
        on_reply: impl FnOnce(&mut Reader<'_>) -> Result<T, i32>,
    ) -> Result<T, i32> {
        self.conn.call(self.handle, code, p, on_reply).map_err(|e| e.rc)
    }

    /// A call whose reply carries nothing read.
    pub fn call_void(&self, code: u32, p: &Parcel) -> Result<(), i32> {
        self.call(code, p, |_| Ok(()))
    }
}

impl Drop for Hal {
    fn drop(&mut self) {
        if self.handle != 0 {
            self.conn.release(self.handle);
        }
    }
}

// ---- health: HealthInfo (HealthInfo.aidl, the fields this bridge reads, in their order) ---------
// BatteryStatus
pub const BATTERY_UNKNOWN: i32 = 1;
pub const BATTERY_CHARGING: i32 = 2;
pub const BATTERY_DISCHARGING: i32 = 3;
pub const BATTERY_NOT_CHARGING: i32 = 4;
pub const BATTERY_FULL: i32 = 5;

/// The longest technology name kept (C: char technology[24]).
pub const TECHNOLOGY_MAX: usize = 23;

#[derive(Clone, Debug, Default, PartialEq)]
pub struct HealthInfo {
    pub ac: bool, // charger*Online
    pub usb: bool,
    pub wireless: bool,
    pub dock: bool,
    pub max_charging_ua: i32,
    pub max_charging_uv: i32,
    pub status: i32, // BatteryStatus
    pub health: i32, // BatteryHealth
    pub present: bool,
    pub level: i32, // percent
    pub voltage_mv: i32,
    pub temp_deci_c: i32,
    pub current_ua: i32,
    pub cycles: i32,
    pub full_uah: i32,
    pub counter_uah: i32,
    pub technology: String,
    pub current_average_ua: i32,
    pub capacity_level: i32, // BatteryCapacityLevel (v1)
    pub time_to_full_s: i64,
    pub design_uah: i32, // batteryFullChargeDesignCapacityUah (0: not sent)
}

/// A parcelable array's elements: each a non-null marker and a sized body; skipped whole.
fn skip_parcelables(r: &mut Reader<'_>) -> bool {
    let n = r.i32();
    if r.is_bad() || !(-1..=4096).contains(&n) {
        r.mark_bad();
        return false;
    }
    for _ in 0..n {
        let Some(end) = r.parcelable() else { return false };
        r.set_position(end);
    }
    true
}

impl HealthInfo {
    /// The reply of getHealthInfo after its status header (a non-null parcelable). -EPROTO when malformed. Fields
    /// past the end of an older HAL's parcelable stay 0; fields a newer one adds are skipped.
    pub fn decode(r: &mut Reader<'_>) -> Result<HealthInfo, i32> {
        let end = r.parcelable().ok_or(-libc::EPROTO)?;
        let mut o = HealthInfo {
            ac: r.i32() != 0,
            usb: r.i32() != 0,
            wireless: r.i32() != 0,
            dock: r.i32() != 0,
            max_charging_ua: r.i32(),
            max_charging_uv: r.i32(),
            status: r.i32(),
            health: r.i32(),
            present: r.i32() != 0,
            level: r.i32(),
            voltage_mv: r.i32(),
            temp_deci_c: r.i32(),
            current_ua: r.i32(),
            cycles: r.i32(),
            full_uah: r.i32(),
            counter_uah: r.i32(),
            technology: r.string(TECHNOLOGY_MAX).ok_or(-libc::EPROTO)?,
            ..HealthInfo::default()
        };
        let more = |r: &Reader<'_>| r.position() + 4 <= end && !r.is_bad();
        if more(r) {
            o.current_average_ua = r.i32();
        }
        if more(r) && !skip_parcelables(r) {
            return Err(-libc::EPROTO); // diskStats
        }
        if more(r) && !skip_parcelables(r) {
            return Err(-libc::EPROTO); // storageInfos
        }
        if more(r) {
            o.capacity_level = r.i32();
        }
        if r.position() + 8 <= end {
            o.time_to_full_s = r.i64();
        }
        if more(r) {
            o.design_uah = r.i32();
        }
        if r.is_bad() || r.position() > end {
            return Err(-libc::EPROTO);
        }
        r.set_position(end); // what a newer HAL added (chargingState, ...)
        Ok(o)
    }

    /// The fake HAL's: version 2's parcelable.
    pub fn encode(&self, p: &mut Parcel) {
        p.put_parcelable(|p| {
            for v in [self.ac, self.usb, self.wireless, self.dock] {
                p.put_i32(v as i32);
            }
            p.put_i32(self.max_charging_ua);
            p.put_i32(self.max_charging_uv);
            p.put_i32(self.status);
            p.put_i32(self.health);
            p.put_i32(self.present as i32);
            p.put_i32(self.level);
            p.put_i32(self.voltage_mv);
            p.put_i32(self.temp_deci_c);
            p.put_i32(self.current_ua);
            p.put_i32(self.cycles);
            p.put_i32(self.full_uah);
            p.put_i32(self.counter_uah);
            p.put_str(&self.technology);
            p.put_i32(self.current_average_ua);
            p.put_i32(0); // no diskStats
            p.put_i32(0); // no storageInfos
            p.put_i32(self.capacity_level);
            p.put_i64(self.time_to_full_s);
            p.put_i32(self.design_uah);
            p.put_i32(1); // chargingState NORMAL
            p.put_i32(1); // chargingPolicy DEFAULT (v2)
        });
    }
}

// ---- thermal: Temperature[] (Temperature.aidl: type, name, value in degrees C, throttlingStatus) -
// TemperatureType
pub const TEMP_CPU: i32 = 0;
pub const TEMP_GPU: i32 = 1;
pub const TEMP_BATTERY: i32 = 2;
pub const TEMP_SKIN: i32 = 3;
pub const TEMP_SOC: i32 = 13;
pub const TEMPS_MAX: usize = 64;
/// The longest sensor name kept (C: char name[24]).
pub const TEMP_NAME_MAX: usize = 23;

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Temperature {
    pub kind: i32,
    pub name: String,
    pub celsius: f32,
    pub throttling: i32,
}

impl Temperature {
    /// The reply of getTemperatures after its status header: at most `max` of them, or -EPROTO.
    pub fn decode_all(r: &mut Reader<'_>, max: usize) -> Result<Vec<Temperature>, i32> {
        let n = r.i32();
        if r.is_bad() || !(-1..=4096).contains(&n) {
            return Err(-libc::EPROTO);
        }
        let mut kept = Vec::new();
        for _ in 0..n {
            let end = r.parcelable().ok_or(-libc::EPROTO)?;
            let kind = r.i32();
            let name = r.string(TEMP_NAME_MAX).ok_or(-libc::EPROTO)?;
            let celsius = r.f32();
            let throttling = if r.position() + 4 <= end { r.i32() } else { 0 };
            if r.is_bad() || r.position() > end {
                return Err(-libc::EPROTO);
            }
            r.set_position(end);
            if kept.len() < max {
                kept.push(Temperature { kind, name, celsius, throttling });
            }
        }
        Ok(kept)
    }

    pub fn encode_all(p: &mut Parcel, all: &[Temperature]) {
        p.put_i32(all.len() as i32);
        for t in all {
            p.put_parcelable(|p| {
                p.put_i32(t.kind);
                p.put_str(&t.name);
                p.put_f32(t.celsius);
                p.put_i32(t.throttling);
            });
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // battery/test_battery.c's health parcel cases (the decoder is this file's).
    #[test]
    fn health_round_trip_and_old_hals() {
        let input = HealthInfo {
            usb: true,
            status: BATTERY_FULL,
            present: true,
            level: 100,
            voltage_mv: 4380,
            temp_deci_c: 287,
            technology: "Li-ion".into(),
            time_to_full_s: -1,
            design_uah: 5_000_000,
            ..Default::default()
        };
        let mut p = Parcel::new();
        input.encode(&mut p);
        p.put_i32(0x5a5a_5a5a); // whatever follows the parcelable in the reply
        let mut r = p.reader();
        let out = HealthInfo::decode(&mut r).unwrap();
        assert_eq!(out, input);
        assert_eq!(r.i32(), 0x5a5a_5a5a);
        // An older HAL's parcelable: it ends after the technology and currentAverage.
        let mut old = Parcel::new();
        old.put_parcelable(|p| {
            for _ in 0..9 {
                p.put_i32(0);
            }
            p.put_i32(100);
            for _ in 0..6 {
                p.put_i32(0);
            }
            p.put_str("Li-ion");
            p.put_i32(0);
        });
        let mut r = old.reader();
        let out = HealthInfo::decode(&mut r).unwrap();
        assert!(out.level == 100 && out.design_uah == 0 && out.time_to_full_s == 0 && r.position() == old.size());
        // A null parcelable, a cut one.
        let null = 0i32.to_ne_bytes();
        assert!(HealthInfo::decode(&mut Reader::new(&null, &[])).is_err());
        let cut = &p.bytes()[..40];
        assert!(HealthInfo::decode(&mut Reader::new(cut, &[])).is_err());
    }

    #[test]
    fn temperatures_round_trip() {
        let all = vec![
            Temperature { kind: TEMP_CPU, name: "cpu-0-0".into(), celsius: 41.5, throttling: 0 },
            Temperature { kind: TEMP_SKIN, name: "skin".into(), celsius: 33.0, throttling: 1 },
        ];
        let mut p = Parcel::new();
        Temperature::encode_all(&mut p, &all);
        assert_eq!(Temperature::decode_all(&mut p.reader(), 64).unwrap(), all);
        assert_eq!(Temperature::decode_all(&mut p.reader(), 1).unwrap().len(), 1);
        let mut bad = Parcel::new();
        bad.put_i32(5000);
        assert!(Temperature::decode_all(&mut bad.reader(), 64).is_err());
    }

    #[test]
    fn error_texts() {
        assert_eq!(error_text(-libc::ETIMEDOUT), "no answer in 5 s");
        assert_eq!(error_text(-libc::EACCES), "Permission denied");
    }
}
