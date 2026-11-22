// SPDX-License-Identifier: MIT
//! The battery bridge: the phone's real battery for iOS. Reads Android's power_supply class under
//! /sys/class/power_supply and hands a normalized sample to xnuhost's bridge device, which tells drivers/battery
//! (iOS's IOPMPowerSource). Read only: nothing is written to the phone, so a killed session leaves nothing behind. One
//! worker thread, idle until the guest listens, SCHED_OTHER. README.md here has the nodes and the mapping.
//! Two backends (hw/, hw-backend-battery): samsung, the power_supply nodes above (unchanged); treble, the health HAL
//! android.hardware.health.IHealth/default through servicemanager (getHealthInfo), the same sample.

mod session;
#[cfg(test)]
mod tests;

pub use session::{Battery, Listener, Options};

/// How often the nodes are read while the guest listens.
pub const POLL_MS: u32 = 3000;
/// Temperature, voltage, current, time: at most this often.
pub const TELEMETRY_MS: u32 = 30000;
/// Sample.temp_deci_c, .current_ma when not known (drivers/bridge/oneios_bridge_proto.h's OB_BATTERY_UNKNOWN).
pub const UNKNOWN: i32 = i32::MIN;
/// Sample.minutes when not known.
pub const MINUTES_UNKNOWN: i32 = -1;
/// The S24 Ultra's rated capacity, when no node gives one.
pub const DEFAULT_MAH: u32 = 5000;
pub const MAX_SUPPLIES: usize = 8;

// A phone never moves 20 A, so a larger number can only be microamps. Below it the number stays in the unit last
// learned: a phone that reports milliamps never crosses it, one that reports microamps does as soon as it draws 20 mA,
// and from then on the reading is sticky.
const CURRENT_UA_MIN: i64 = 20000;
const VOLTAGE_UV_MIN: i64 = 100000; // 100 V
const CHARGE_UAH_MIN: i64 = 100000; // 100 Ah
const TEMP_DECI_MIN: i64 = -400;
const TEMP_DECI_MAX: i64 = 1500;
const CHARGE_MIN_MA: i32 = 50; // below this a time to full means nothing
const DISCHARGE_MIN_MA: i32 = 25; // the same for a time to empty
const MINUTES_MAX: u64 = 7 * 24 * 60;

/// The nodes as read, each None when missing (or not a number).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Raw {
    /// "Charging", "Discharging", "Full", "Not charging"; "" when missing
    pub status: String,
    pub capacity: Option<i64>,
    pub present: Option<i64>,
    pub temp: Option<i64>,
    pub voltage: Option<i64>,
    pub current: Option<i64>,
    pub charge_full: Option<i64>,
    pub charge_full_design: Option<i64>,
    pub cycles: Option<i64>,
    /// Some supply of the list has online > 0.
    pub plugged: bool,
}

/// Sticky between samples: the unit of current_now is learned from its size.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Units {
    pub current_ua: bool,
    pub current_fixed: bool,
}

/// What iOS is told.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Sample {
    /// 0..100
    pub percent: u32,
    pub present: bool,
    pub plugged: bool,
    pub charging: bool,
    pub full: bool,
    /// Tenths of a degree; [`UNKNOWN`]
    pub temp_deci_c: i32,
    /// 0 = unknown
    pub voltage_mv: u32,
    /// Into the battery > 0, out of it < 0; [`UNKNOWN`]
    pub current_ma: i32,
    /// Never 0.
    pub full_mah: u32,
    pub design_mah: u32,
    /// 0 = unknown
    pub cycles: u32,
    /// To empty, or to full while charging; [`MINUTES_UNKNOWN`]
    pub minutes: i32,
}

fn mah_of(v: Option<i64>) -> u32 {
    match v {
        Some(v) if v > 0 => u32::try_from(if v >= CHARGE_UAH_MIN { v / 1000 } else { v }).unwrap_or(0),
        _ => 0,
    }
}

impl Raw {
    /// The sample iOS is told; `units` learns current_now's unit.
    pub fn map(&self, units: &mut Units, default_mah: u32) -> Sample {
        let present = match self.present {
            None => self.capacity.is_some(),
            Some(p) => p != 0,
        };
        let mut out = Sample { percent: self.capacity.unwrap_or(0).clamp(0, 100) as u32, present, ..Sample::default() };
        let is = |text: &str| self.status.eq_ignore_ascii_case(text);
        let (is_charging, is_full, not_charging, discharging) =
            (is("Charging"), is("Full"), is("Not charging"), is("Discharging"));
        out.plugged = self.plugged || is_charging || is_full;
        out.full = is_full || (out.plugged && not_charging && out.percent >= 100);
        out.charging = is_charging;

        out.temp_deci_c = match self.temp {
            Some(t) if (TEMP_DECI_MIN..=TEMP_DECI_MAX).contains(&t) => t as i32,
            _ => UNKNOWN,
        };

        let voltage = self.voltage.unwrap_or(0).max(0);
        let voltage = if voltage >= VOLTAGE_UV_MIN { voltage / 1000 } else { voltage };
        out.voltage_mv = u32::try_from(voltage).unwrap_or(0);

        out.current_ma = UNKNOWN;
        if let Some(current) = self.current {
            if !units.current_fixed && current.abs() >= CURRENT_UA_MIN {
                units.current_ua = true;
            }
            let ma = if units.current_ua { current / 1000 } else { current };
            // The kernel's sign convention differs between drivers: the status says which way it flows.
            let ma = if is_charging {
                ma.abs()
            } else if discharging {
                -ma.abs()
            } else {
                ma
            };
            if ma > i64::from(i32::MIN) && ma <= i64::from(i32::MAX) {
                out.current_ma = ma as i32;
            }
        }

        out.full_mah = mah_of(self.charge_full);
        out.design_mah = mah_of(self.charge_full_design);
        let fallback = if default_mah != 0 { default_mah } else { DEFAULT_MAH };
        if out.full_mah == 0 {
            out.full_mah = if out.design_mah != 0 { out.design_mah } else { fallback };
        }
        if out.design_mah == 0 {
            out.design_mah = out.full_mah;
        }
        out.cycles = self.cycles.and_then(|c| u32::try_from(c).ok()).unwrap_or(0);

        out.minutes = MINUTES_UNKNOWN;
        if out.present && out.current_ma != UNKNOWN {
            let left = u64::from(out.full_mah) * u64::from(out.percent) / 100;
            let todo = u64::from(out.full_mah) - left;
            let minutes = if is_charging && out.current_ma >= CHARGE_MIN_MA {
                todo * 60 / out.current_ma as u64
            } else if discharging && out.current_ma <= -DISCHARGE_MIN_MA {
                left * 60 / (-i64::from(out.current_ma)) as u64
            } else {
                MINUTES_MAX + 1
            };
            if minutes <= MINUTES_MAX {
                out.minutes = minutes as i32;
            }
        }
        out
    }
}

impl Sample {
    /// Whether `now` is worth a message after `self` was sent: at once for the level and the charging state, the rest
    /// when ms_since_sent has reached [`TELEMETRY_MS`].
    pub fn changed(&self, now: &Sample, ms_since_sent: u32) -> bool {
        if (self.percent, self.present, self.plugged, self.charging, self.full)
            != (now.percent, now.present, now.plugged, now.charging, now.full)
        {
            return true;
        }
        ms_since_sent >= TELEMETRY_MS && self != now
    }
}

// ---- sysfs --------------------------------------------------------------------------------------

/// DIR/SUPPLY/NAME without its trailing white space (at most `max` bytes); None when it cannot be read.
fn read_text(dir: &str, supply: &str, name: &str, max: usize) -> Option<String> {
    use std::io::Read;
    let mut buf = vec![0; max];
    let n = std::fs::File::open(format!("{dir}/{supply}/{name}")).ok()?.read(&mut buf).ok()?;
    let text = String::from_utf8_lossy(&buf[..n]);
    Some(text.trim_end_matches(['\n', ' ', '\r', '\t']).to_owned())
}

/// C's strtoll/strtoul with base 0 on a prefix: leading white space, an optional sign, then decimal, 0x hex or 0
/// octal digits. The signed value (None without digits or past 64 bits) and the length taken.
fn c_number(text: &str) -> Option<(i128, usize)> {
    let t = text.trim_start_matches([' ', '\t', '\n', '\r', '\x0b', '\x0c']);
    let mut at = text.len() - t.len();
    let negative = t.starts_with('-');
    if t.starts_with(['-', '+']) {
        at += 1;
    }
    let t = &text[at..];
    let hex = (t.starts_with("0x") || t.starts_with("0X")) && t[2..].starts_with(|c: char| c.is_ascii_hexdigit());
    let (radix, skip) = if hex {
        (16, 2)
    } else if t.starts_with('0') {
        (8, 0)
    } else {
        (10, 0)
    };
    let digits = t[skip..].bytes().take_while(|c| (*c as char).is_digit(radix)).count();
    if digits == 0 {
        return None;
    }
    let magnitude = i128::from(u64::from_str_radix(&t[skip..skip + digits], radix).ok()?);
    Some((if negative { -magnitude } else { magnitude }, at + skip + digits))
}

/// C's strtoll(text, &end, 0) of the whole text (the node's text, its trailing white space already cut).
fn parse_c_int(text: &str) -> Option<i64> {
    match c_number(text) {
        Some((v, len)) if len == text.len() => i64::try_from(v).ok(),
        _ => None,
    }
}

/// C's (uint32_t)strtoul(text, NULL, 0): the leading number, 0 without one.
fn parse_c_prefix(text: &str) -> u64 {
    c_number(text).map_or(0, |(v, _)| v as u64)
}

fn read_number(dir: &str, supply: &str, name: &str) -> Option<i64> {
    read_text(dir, supply, name, 39).filter(|t| !t.is_empty()).and_then(|t| parse_c_int(&t))
}

/// dir/battery/{capacity,status,...} and dir/<supply>/online for each of `supplies`. None when there is no readable
/// capacity.
pub fn read_raw(dir: &str, supplies: &[impl AsRef<str>]) -> Option<Raw> {
    let num = |name: &str| read_number(dir, "battery", name);
    let capacity = Some(num("capacity")?);
    Some(Raw {
        status: read_text(dir, "battery", "status", 23).unwrap_or_default(),
        capacity,
        present: num("present"),
        temp: num("temp"),
        voltage: num("voltage_now"),
        current: num("current_now"),
        charge_full: num("charge_full"),
        charge_full_design: num("charge_full_design"),
        cycles: num("cycle_count").or_else(|| num("battery_cycle")), // Samsung's name
        plugged: supplies.iter().any(|s| read_number(dir, s.as_ref(), "online").is_some_and(|online| online > 0)),
    })
}
