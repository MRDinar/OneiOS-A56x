// SPDX-License-Identifier: MIT
//! fake-hal: a stand-in servicemanager with stand-in HALs on a binder device, for the treble backend's tests (the
//! HAL bridges' binder tests) and the stand-in phone (tools/oneios-tool/tests/phone/hw-backend.sh, the `fake-hal`
//! binary). Both ends use [`super::aidl`] and [`super`]: what it proves is the client's protocol (the lookup by name,
//! the descriptor and version checks, the codes, the parcels as hal writes them), not the vendor's HALs; those are
//! the phone's.
//!
//! [`serve`]`(DIR)`: the context manager of ONEIOS_BINDER (a binderfs device), until the process ends; "ready" said.
//! Each HAL is registered when its file is there, and the file is read again at every call:
//!   DIR/health    android.hardware.health.IHealth/default: key=value lines (level, status 1..5, present, temp in
//!                 tenths, voltage_mv, current_ua, cycles, full_uah, design_uah, ac, usb, wireless, dock)
//!   DIR/thermal   android.hardware.thermal.IThermal/default: lines "TYPE NAME CELSIUS" (TYPE: TemperatureType)
//!   DIR/vibrator  android.hardware.vibrator.IVibrator/default: its capabilities (a number; empty: 4, amplitude);
//!                 every call appended to DIR/vibrator.log ("on 30", "off", "amplitude 0.50")
//! C: tests/fake_hal.c.

use super::aidl::{self, Connection, Object, Parcel, Reader};
use super::*;
use std::io::Write;
use std::path::{Path, PathBuf};

const OBJ_MANAGER: u64 = 0;
const OBJ_HEALTH: u64 = 1;
const OBJ_THERMAL: u64 = 2;
const OBJ_VIBRATOR: u64 = 3;

/// C's strtol on the start of `s` (leading blanks, a sign, digits): None without a digit.
fn long_prefix(s: &str) -> Option<i64> {
    let s = s.trim_start();
    let (sign, digits) = match s.as_bytes().first() {
        Some(b'-') => (-1, &s[1..]),
        Some(b'+') => (1, &s[1..]),
        _ => (1, s),
    };
    let end = digits.bytes().take_while(u8::is_ascii_digit).count();
    digits[..end].parse::<i64>().ok().map(|v| sign * v)
}

/// C's strtof on a whole word, or its longest prefix that is a number.
fn float_prefix(s: &str) -> Option<f32> {
    (1..=s.len()).rev().filter(|&n| s.is_char_boundary(n)).find_map(|n| s[..n].parse::<f32>().ok())
}

/// C's "%.2f".
fn two_places(a: f32) -> String {
    if a.is_nan() {
        "nan".to_owned()
    } else {
        format!("{a:.2}")
    }
}

struct Fake {
    dir: PathBuf,
}

impl Fake {
    /// A file's text (at most 1023 bytes), None when it is not there.
    fn text(&self, name: &str) -> Option<String> {
        let bytes = std::fs::read(self.dir.join(name)).ok()?;
        Some(String::from_utf8_lossy(&bytes[..bytes.len().min(1023)]).into_owned())
    }
    fn has(&self, name: &str) -> bool {
        std::fs::File::open(self.dir.join(name)).is_ok()
    }

    fn note(&self, line: &str) {
        if let Ok(mut f) = std::fs::OpenOptions::new().append(true).create(true).open(self.dir.join("vibrator.log")) {
            let _ = f.write_all(format!("{line}\n").as_bytes());
        }
    }

    fn health(&self) -> HealthInfo {
        let mut h = HealthInfo { technology: "Li-ion".into(), ..Default::default() };
        let Some(text) = self.text("health") else { return h };
        for line in text.lines() {
            let Some((key, value)) = line.split_once('=') else { continue };
            let (Some(v), true) = (long_prefix(value), !key.is_empty() && key.len() <= 31) else { continue };
            let x = v as i32;
            match key {
                "level" => h.level = x,
                "status" => h.status = x,
                "present" => h.present = x != 0,
                "temp" => h.temp_deci_c = x,
                "voltage_mv" => h.voltage_mv = x,
                "current_ua" => h.current_ua = x,
                "cycles" => h.cycles = x,
                "full_uah" => h.full_uah = x,
                "design_uah" => h.design_uah = x,
                "ac" => h.ac = x != 0,
                "usb" => h.usb = x != 0,
                "wireless" => h.wireless = x != 0,
                "dock" => h.dock = x != 0,
                _ => {}
            }
        }
        h
    }

    fn temperatures(&self) -> Vec<Temperature> {
        let Some(text) = self.text("thermal") else { return Vec::new() };
        text.lines()
            .filter_map(|line| {
                let mut w = line.split_whitespace();
                let kind = long_prefix(w.next()?)? as i32;
                let name: String = w.next()?.chars().take(TEMP_NAME_MAX).collect();
                let celsius = float_prefix(w.next()?)?;
                Some(Temperature { kind, name, celsius, throttling: 0 })
            })
            .take(TEMPS_MAX)
            .collect()
    }

    fn incoming(&self, object: u64, code: u32, r: &mut Reader<'_>) -> Result<Parcel, i32> {
        let mut reply = Parcel::new();
        if object == OBJ_MANAGER {
            let name = if code == SM_GET_SERVICE && r.token(SERVICE_MANAGER) { r.string(127) } else { None };
            let name = name.ok_or(aidl::UNKNOWN_TRANSACTION)?;
            reply.put_ok();
            match name.as_str() {
                HEALTH_INSTANCE if self.has("health") => reply.put_local(OBJ_HEALTH),
                THERMAL_INSTANCE if self.has("thermal") => reply.put_local(OBJ_THERMAL),
                VIBRATOR_INSTANCE if self.has("vibrator") => reply.put_local(OBJ_VIBRATOR),
                _ => reply.put_null_binder(),
            }
            return Ok(reply);
        }
        let descriptor = match object {
            OBJ_HEALTH => HEALTH,
            OBJ_THERMAL => THERMAL,
            _ => VIBRATOR,
        };
        if let Some(standard) = aidl::standard(code, descriptor, 2, "fake") {
            return Ok(standard);
        }
        if !r.token(descriptor) {
            return Err(aidl::BAD_TYPE);
        }
        if object == OBJ_HEALTH && code == HEALTH_GET_INFO {
            reply.put_ok();
            self.health().encode(&mut reply);
            return Ok(reply);
        }
        if object == OBJ_THERMAL && code == THERMAL_GET_TEMPERATURES {
            reply.put_ok();
            Temperature::encode_all(&mut reply, &self.temperatures());
            return Ok(reply);
        }
        if object != OBJ_VIBRATOR {
            return Err(aidl::UNKNOWN_TRANSACTION);
        }
        let ints = |reply: &mut Parcel, values: &[i32]| {
            reply.put_ok();
            for v in values {
                reply.put_i32(*v);
            }
        };
        match code {
            VIB_GET_CAPABILITIES => {
                let caps = match self.text("vibrator") {
                    Some(t) if !t.is_empty() => long_prefix(&t).unwrap_or(0) as i32,
                    _ => VIB_CAP_AMPLITUDE,
                };
                ints(&mut reply, &[caps]);
            }
            // One-shot active preset test. Unlike getters, perform() is logged so tests can prove it was called
            // once, with the right parcel and no off().
            VIB_PERFORM => {
                let (effect, strength) = (r.i32(), r.i32());
                let callback = matches!(r.binder(), Object::Handle(_));
                let click = effect == VIB_EFFECT_CLICK && strength == VIB_STRENGTH_MEDIUM;
                let tick = effect == VIB_EFFECT_TICK && strength == VIB_STRENGTH_LIGHT;
                let heavy = effect == VIB_EFFECT_HEAVY_CLICK && strength == VIB_STRENGTH_STRONG;
                if r.is_bad()
                    || callback
                    || (!click && !heavy && !tick)
                    || (click && self.has("vibrator_effects_no_click"))
                    || (heavy && self.has("vibrator_effects_no_heavy_click"))
                {
                    reply.put_service_error(1);
                    return Ok(reply);
                }
                self.note(&format!("perform {effect} {strength}"));
                ints(&mut reply, &[if heavy { 60 } else { 20 }]);
            }
            // Read-only AIDL capability/primitive queries. Do NOT note these methods: the phone test asserts they
            // never request actuation.
            VIB_SUPPORTED_EFFECTS => {
                if self.has("vibrator_effects_no_click") {
                    ints(&mut reply, &[2, VIB_EFFECT_TICK, VIB_EFFECT_HEAVY_CLICK]);
                } else if self.has("vibrator_effects_no_heavy_click") {
                    ints(&mut reply, &[2, VIB_EFFECT_CLICK, VIB_EFFECT_TICK]);
                } else {
                    ints(&mut reply, &[3, VIB_EFFECT_CLICK, VIB_EFFECT_TICK, VIB_EFFECT_HEAVY_CLICK]);
                }
            }
            VIB_COMPOSE_DELAY_MAX => ints(&mut reply, &[500]),
            VIB_COMPOSE_SIZE_MAX => ints(&mut reply, &[16]),
            VIB_SUPPORTED_PRIMITIVES => ints(&mut reply, &[3, 0, 1, 7]), // NOOP, CLICK, LIGHT_TICK
            VIB_PRIMITIVE_DURATION => {
                let primitive = r.i32();
                if r.is_bad() || ![0, 1, 7].contains(&primitive) {
                    reply.put_service_error(1);
                } else {
                    ints(
                        &mut reply,
                        &[match primitive {
                            0 => 0,
                            1 => 20,
                            _ => 9,
                        }],
                    );
                }
            }
            VIB_OFF => {
                self.note("off");
                reply.put_ok();
            }
            VIB_ON => {
                let ms = r.i32();
                let callback = matches!(r.binder(), Object::Handle(_));
                self.note(&format!("on {ms}{}", if callback { " with a callback" } else { "" }));
                reply.put_ok();
            }
            VIB_SET_AMPLITUDE => {
                let a = r.f32();
                if !(a > 0.0 && a <= 1.0) {
                    self.note(&format!("amplitude {} refused", two_places(a)));
                    reply.put_service_error(1); // EX_ILLEGAL_ARGUMENT's role
                } else {
                    self.note(&format!("amplitude {}", two_places(a)));
                    reply.put_ok();
                }
            }
            _ => return Err(aidl::UNKNOWN_TRANSACTION),
        }
        Ok(reply)
    }
}

/// Serves `dir`'s HALs as the context manager of ONEIOS_BINDER (or /dev/binder) and writes "ready\n" to `ready`;
/// returns only when that fails: the exit code (91: no binder device, 92: not the context manager).
pub fn serve(dir: &Path, ready: &mut dyn Write) -> i32 {
    let fake = Fake { dir: dir.to_owned() };
    let incoming = move |object: u64, code: u32, _one_way: bool, r: &mut Reader<'_>| fake.incoming(object, code, r);
    let conn = match Connection::open(None, Some(Box::new(incoming)), None) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("fake-hal: no binder device (ONEIOS_BINDER): {}", binder::strerror(e.errno()));
            return 91;
        }
    };
    // it waits out a killed predecessor (EBUSY, 2 s)
    if let Err(rc) = conn.context_manager() {
        eprintln!("fake-hal: not the context manager: {}", binder::strerror(rc));
        return 92;
    }
    let _ = ready.write_all(b"ready\n");
    let _ = ready.flush();
    loop {
        std::thread::park();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn c_number_parsing() {
        assert_eq!(long_prefix(" 42x"), Some(42));
        assert_eq!(long_prefix("-7"), Some(-7));
        assert_eq!(long_prefix("x"), None);
        assert_eq!(float_prefix("41.5C"), Some(41.5));
        assert_eq!(two_places(0.5), "0.50");
        assert_eq!(two_places(f32::NAN), "nan");
    }

    #[test]
    fn fake_answers_without_a_binder() {
        let dir = std::env::temp_dir().join(format!("onebridges-fake-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("health"), "level=55\nstatus=2\nusb=1\n").unwrap();
        std::fs::write(dir.join("thermal"), "0 cpu-0-0 41.5\n3 skin 33\n").unwrap();
        let fake = Fake { dir: dir.clone() };
        let mut p = parcel(SERVICE_MANAGER);
        p.put_str(HEALTH_INSTANCE);
        let reply = fake.incoming(OBJ_MANAGER, SM_GET_SERVICE, &mut p.reader()).unwrap();
        let mut r = reply.reader();
        assert!(r.status().is_ok());
        let p = parcel(HEALTH);
        let reply = fake.incoming(OBJ_HEALTH, HEALTH_GET_INFO, &mut p.reader()).unwrap();
        let mut r = reply.reader();
        assert!(r.status().is_ok());
        let h = HealthInfo::decode(&mut r).unwrap();
        assert!(h.level == 55 && h.status == 2 && h.usb && h.technology == "Li-ion");
        let p = parcel(THERMAL);
        let reply = fake.incoming(OBJ_THERMAL, THERMAL_GET_TEMPERATURES, &mut p.reader()).unwrap();
        let mut r = reply.reader();
        assert!(r.status().is_ok());
        let t = Temperature::decode_all(&mut r, 64).unwrap();
        assert_eq!(t.len(), 2);
        assert_eq!(t[1].name, "skin");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
