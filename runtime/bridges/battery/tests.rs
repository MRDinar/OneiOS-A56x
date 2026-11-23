// SPDX-License-Identifier: MIT
//! Host tests of the battery bridge (test_battery.c): a fake power_supply tree and the mapping. The health HAL's
//! parcel round trips are hal/'s tests; here what the bridge makes of a HealthInfo.

use super::session::from_health;
use super::*;
use crate::hal::{self, HealthInfo};
use std::fs;
use std::path::Path;
use std::sync::{Arc, Mutex};
use std::thread::sleep;
use std::time::Duration;

fn write_node(root: &Path, supply: &str, name: &str, text: &str) {
    fs::create_dir_all(root.join(supply)).unwrap();
    fs::write(root.join(supply).join(name), text).unwrap();
}

fn raw_base() -> Raw {
    Raw {
        capacity: Some(50),
        present: Some(1),
        temp: Some(305),
        voltage: Some(4_000_000),
        current: None,
        charge_full: Some(4_900_000),
        charge_full_design: Some(5_000_000),
        cycles: None,
        status: "Discharging".into(),
        plugged: false,
    }
}

const SUPPLIES: [&str; 4] = ["usb", "ac", "wireless", "pogo"];

#[test]
fn map() {
    let mut u = Units::default();
    let mut r = raw_base();
    let s = r.map(&mut u, 0);
    assert!(s.percent == 50 && s.present && !s.plugged && !s.charging && !s.full);
    assert!(
        s.temp_deci_c == 305 && s.voltage_mv == 4000 && s.full_mah == 4900 && s.design_mah == 5000 && s.cycles == 0
    );
    assert!(s.current_ma == UNKNOWN && s.minutes == MINUTES_UNKNOWN);

    // Charging says plugged in even when no online node is 1; Full says plugged and full, not charging.
    r.status = "Charging".into();
    let s = r.map(&mut u, 0);
    assert!(s.plugged && s.charging && !s.full);
    r.capacity = Some(100);
    r.status = "FULL".into();
    let s = r.map(&mut u, 0);
    assert!(s.percent == 100 && s.plugged && !s.charging && s.full);

    // Not charging: plugged only if a supply says so; full only at 100.
    r.status = "Not charging".into();
    r.capacity = Some(80);
    r.plugged = true;
    let s = r.map(&mut u, 0);
    assert!(s.plugged && !s.charging && !s.full);
    r.capacity = Some(100);
    let s = r.map(&mut u, 0);
    assert!(s.plugged && !s.charging && s.full);
    r.plugged = false;
    let s = r.map(&mut u, 0);
    assert!(!s.plugged && !s.full);

    // A cable that does not keep up: discharging but plugged.
    let r = Raw { plugged: true, ..raw_base() };
    let s = r.map(&mut u, 0);
    assert!(s.plugged && !s.charging);

    // Out of range, unknown, missing.
    let mut r = Raw {
        capacity: Some(130),
        temp: Some(9999),
        voltage: None,
        present: None,
        charge_full: None,
        charge_full_design: None,
        status: String::new(),
        ..raw_base()
    };
    let s = r.map(&mut u, 0);
    assert!(s.percent == 100 && s.present && s.temp_deci_c == UNKNOWN && s.voltage_mv == 0);
    assert!(s.full_mah == DEFAULT_MAH && s.design_mah == DEFAULT_MAH);
    r.capacity = Some(-3);
    r.present = Some(0);
    let s = r.map(&mut u, 4000);
    assert!(s.percent == 0 && !s.present && s.full_mah == 4000);
    // millivolts on a driver that reports them
    let r = Raw { voltage: Some(3850), charge_full: Some(4800), charge_full_design: Some(5000), ..raw_base() };
    let s = r.map(&mut u, 0);
    assert!(s.voltage_mv == 3850 && s.full_mah == 4800 && s.design_mah == 5000);
    let r = Raw { charge_full_design: None, cycles: Some(123), ..raw_base() };
    let s = r.map(&mut u, 0);
    assert!(s.design_mah == 4900 && s.cycles == 123);
}

#[test]
fn current() {
    // Milliamps stay milliamps, the sign from the status, either way the node reports it.
    let mut u = Units::default();
    let mut r = Raw { current: Some(300), ..raw_base() };
    let s = r.map(&mut u, 0);
    assert!(s.current_ma == -300 && !u.current_ua);
    r.current = Some(-300);
    assert_eq!(r.map(&mut u, 0).current_ma, -300);
    r.status = "Charging".into();
    r.current = Some(-2000);
    r.capacity = Some(90);
    let s = r.map(&mut u, 0);
    assert!(s.current_ma == 2000 && !u.current_ua);
    // 10 percent of 4900 mAh is 490 mAh; at 2000 mA that is 14 minutes (490 * 60 / 2000).
    assert_eq!(s.minutes, 14);

    // Microamps are learned from the size, and then small readings stay microamps.
    let mut u = Units::default();
    let mut r = Raw { current: Some(-350_000), ..raw_base() };
    let s = r.map(&mut u, 0);
    assert!(u.current_ua && s.current_ma == -350);
    // 50 percent of 4900 is 2450 mAh; at 350 mA that is 420 minutes.
    assert_eq!(s.minutes, 420);
    r.current = Some(-15000);
    let s = r.map(&mut u, 0);
    assert!(u.current_ua && s.current_ma == -15 && s.minutes == MINUTES_UNKNOWN); // too little for an estimate
                                                                                  // A fixed unit is not learned over.
    let mut m = Units { current_fixed: true, current_ua: false };
    r.current = Some(25000);
    let s = r.map(&mut m, 0);
    assert!(!m.current_ua && s.current_ma == -25000);
    // Not charging keeps the node's own sign, and no estimate.
    let r = Raw { status: "Not charging".into(), plugged: true, current: Some(-40), ..raw_base() };
    let s = r.map(&mut Units::default(), 0);
    assert!(s.current_ma == -40 && s.minutes == MINUTES_UNKNOWN);
}

#[test]
fn changed() {
    let a = raw_base().map(&mut Units::default(), 0);
    let mut b = a;
    assert!(!a.changed(&b, 0) && !a.changed(&b, 999999));
    b.percent = 49;
    assert!(a.changed(&b, 0));
    for f in [|s: &mut Sample| s.plugged = true, |s: &mut Sample| s.charging = true, |s: &mut Sample| s.full = true] {
        let mut b = a;
        f(&mut b);
        assert!(a.changed(&b, 0));
    }
    let b = Sample { temp_deci_c: 310, ..a };
    assert!(!a.changed(&b, TELEMETRY_MS - 1) && a.changed(&b, TELEMETRY_MS));
    let b = Sample { voltage_mv: 3999, ..a };
    assert!(!a.changed(&b, 1000) && a.changed(&b, TELEMETRY_MS));
    let b = Sample { current_ma: -1, ..a };
    assert!(a.changed(&b, TELEMETRY_MS));
}

#[test]
fn c_numbers() {
    assert_eq!(parse_c_int("312"), Some(312));
    assert_eq!(parse_c_int("-420000"), Some(-420000));
    assert_eq!(parse_c_int("0x1f"), Some(31));
    assert_eq!(parse_c_int("017"), Some(15));
    assert_eq!(parse_c_int("0"), Some(0));
    assert_eq!(parse_c_int("cold"), None);
    assert_eq!(parse_c_int("08"), None);
    assert_eq!(parse_c_int("12 3"), None);
    assert_eq!(parse_c_int("++5"), None);
    assert_eq!(parse_c_int("99999999999999999999"), None);
    assert_eq!(parse_c_prefix("4500mAh"), 4500);
    assert_eq!(parse_c_prefix("x"), 0);
}

/// A folder of its own for each test, removed at the end.
struct Root(std::path::PathBuf);

impl Root {
    fn new(name: &str) -> Root {
        let dir = std::env::temp_dir().join(format!("oneios-battery-test.{}.{name}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        Root(dir)
    }
    fn s(&self) -> String {
        self.0.display().to_string()
    }
}

impl Drop for Root {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn fake_phone(root: &Path) {
    for (supply, name, text) in [
        ("battery", "capacity", "73\n"),
        ("battery", "status", "Discharging\n"),
        ("battery", "present", "1\n"),
        ("battery", "temp", "312\n"),
        ("battery", "voltage_now", "4012000\n"),
        ("battery", "current_now", "-420000\n"),
        ("battery", "charge_full", "4800000\n"),
        ("battery", "charge_full_design", "5000000\n"),
        ("battery", "battery_cycle", "212\n"),
        ("usb", "online", "0\n"),
        ("ac", "online", "0\n"),
        ("wireless", "online", "0\n"),
        ("otg", "online", "1\n"), // a device on the port: the phone gives power, it does not get it
    ] {
        write_node(root, supply, name, text);
    }
}

#[test]
fn sysfs() {
    let root = Root::new("sysfs");
    let (r0, dir) = (&root.0, root.s());
    fake_phone(r0);
    let r = read_raw(&dir, &SUPPLIES).unwrap();
    assert!(r.capacity == Some(73) && r.status == "Discharging" && r.present == Some(1) && r.temp == Some(312));
    assert!(r.voltage == Some(4012000) && r.current == Some(-420000) && r.charge_full == Some(4800000));
    assert!(r.charge_full_design == Some(5000000) && r.cycles == Some(212) && !r.plugged);
    write_node(r0, "usb", "online", "1\n");
    assert!(read_raw(&dir, &SUPPLIES).unwrap().plugged);
    write_node(r0, "usb", "online", "0\n");
    write_node(r0, "ac", "online", "2\n");
    assert!(read_raw(&dir, &SUPPLIES).unwrap().plugged);
    write_node(r0, "ac", "online", "0\n");

    // A number that is not one counts as missing; no capacity is no battery.
    write_node(r0, "battery", "temp", "cold\n");
    assert_eq!(read_raw(&dir, &SUPPLIES).unwrap().temp, None);
    write_node(r0, "battery", "temp", "312\n");
    write_node(r0, "battery", "cycle_count", "7\n");
    assert_eq!(read_raw(&dir, &SUPPLIES).unwrap().cycles, Some(7)); // cycle_count first
    assert!(read_raw("/nonexistent/power_supply", &SUPPLIES).is_none());
}

/// The owner's S24 Ultra on USB, as read on 2026-11-11: Full at 100 %, an empty cycle_count, the cycles in
/// battery_cycle, and battery/online holding a cable type, which must not be read as a supply.
#[test]
fn probe() {
    let root = Root::new("probe");
    let (r0, dir) = (&root.0, root.s());
    for (supply, name, text) in [
        ("battery", "capacity", "100\n"),
        ("battery", "status", "Full\n"),
        ("battery", "present", "1\n"),
        ("battery", "temp", "389\n"),
        ("battery", "voltage_now", "4294000\n"),
        ("battery", "current_now", "74\n"),
        ("battery", "charge_full", "5000000\n"),
        ("battery", "charge_full_design", "5000000\n"),
        ("battery", "cycle_count", ""),
        ("battery", "battery_cycle", "1103\n"),
        ("battery", "online", "19\n"),
        ("usb", "online", "1\n"),
        ("ac", "online", "0\n"),
        ("wireless", "online", "0\n"),
        ("pogo", "online", "0\n"),
    ] {
        write_node(r0, supply, name, text);
    }
    let mut u = Units::default();
    let r = read_raw(&dir, &SUPPLIES).unwrap();
    assert!(r.cycles == Some(1103) && r.plugged);
    let s = r.map(&mut u, 0);
    assert!(s.percent == 100 && s.present && s.plugged && !s.charging && s.full);
    assert!(
        s.temp_deci_c == 389 && s.voltage_mv == 4294 && s.full_mah == 5000 && s.design_mah == 5000 && s.cycles == 1103
    );
    assert!(s.current_ma == 74 && s.minutes == MINUTES_UNKNOWN);
    // Unplugged, usb/online 0 and Discharging: nothing says plugged, though battery/online still holds a number.
    write_node(r0, "usb", "online", "0\n");
    write_node(r0, "battery", "status", "Discharging\n");
    let r = read_raw(&dir, &SUPPLIES).unwrap();
    assert!(!r.plugged);
    let s = r.map(&mut u, 0);
    assert!(!s.plugged && !s.charging && !s.full);
}

/// The base S25 (SM-S931B), as the PA1Q dtbo names the drivers: the max77775 fuel gauge is `battery`, the cps4038
/// wireless charger is `wireless`, and `dc` is sec-direct-charger's bypass supply (not in the default list). The gauge
/// reports microamps and leaves cycle_count empty, so the cycles come from battery_cycle, as on the S24 Ultra.
#[test]
fn s25() {
    let root = Root::new("s25");
    let (r0, dir) = (&root.0, root.s());
    for (supply, name, text) in [
        ("battery", "capacity", "63\n"),
        ("battery", "status", "Discharging\n"), // on the pad, the phone still runs on the battery
        ("battery", "present", "1\n"),
        ("battery", "temp", "302\n"),
        ("battery", "voltage_now", "3821000\n"),
        ("battery", "current_now", "-612000\n"), // microamps
        ("battery", "charge_full", "4850000\n"),
        ("battery", "charge_full_design", "5000000\n"),
        ("battery", "cycle_count", ""),
        ("battery", "battery_cycle", "87\n"),
        ("usb", "online", "0\n"),
        ("ac", "online", "0\n"),
        ("wireless", "online", "1\n"), // cps4038
        ("pogo", "online", "0\n"),
        ("dc", "online", "1\n"), // sec-direct-charger's bypass, not a cable
    ] {
        write_node(r0, supply, name, text);
    }
    let mut u = Units::default();
    let r = read_raw(&dir, &SUPPLIES).unwrap();
    assert!(r.capacity == Some(63) && r.present == Some(1) && r.temp == Some(302) && r.cycles == Some(87) && r.plugged);
    let s = r.map(&mut u, 0);
    assert!(s.percent == 63 && s.present && s.plugged && !s.charging && !s.full);
    assert!(s.temp_deci_c == 302 && s.voltage_mv == 3821 && s.current_ma == -612); // the unit was learned
    assert!(s.full_mah == 4850 && s.design_mah == 5000 && s.minutes == 299);

    // The pad now keeps up: Charging, and the gauge's current turns positive.
    write_node(r0, "battery", "status", "Charging\n");
    write_node(r0, "battery", "capacity", "64\n");
    write_node(r0, "battery", "current_now", "1850000\n");
    let r = read_raw(&dir, &SUPPLIES).unwrap();
    assert!(r.plugged);
    let s = r.map(&mut u, 0);
    assert!(s.percent == 64 && s.charging && s.plugged && s.current_ma == 1850 && s.minutes == 56);

    // Unplugged: wireless offline and dc still online. dc is not one of the supplies the bridge reads.
    write_node(r0, "battery", "status", "Discharging\n");
    write_node(r0, "wireless", "online", "0\n");
    assert!(!read_raw(&dir, &SUPPLIES).unwrap().plugged);

    // Failure paths: a gauge with no capacity (no battery), a value that is not a number, an absent supply node, and
    // sensors that are off or out of range.
    write_node(r0, "battery", "temp", "9999\n");
    let r = read_raw(&dir, &SUPPLIES).unwrap();
    assert_eq!(r.temp, Some(9999)); // mapped to unknown below
    assert_eq!(r.map(&mut u, 0).temp_deci_c, UNKNOWN);
    fs::remove_file(r0.join("battery/capacity")).unwrap();
    assert!(read_raw(&dir, &SUPPLIES).is_none()); // no battery
    write_node(r0, "battery", "capacity", "not-a-number\n");
    assert!(read_raw(&dir, &SUPPLIES).is_none());
    write_node(r0, "battery", "capacity", "64\n");
    write_node(r0, "battery", "temp", "302\n");
    fs::remove_file(r0.join("wireless/online")).unwrap();
    assert!(!read_raw(&dir, &SUPPLIES).unwrap().plugged); // a missing supply is offline
}

/// What the listener heard.
#[derive(Clone, Default)]
struct Heard(Arc<Mutex<(usize, Sample)>>);

impl Heard {
    fn listener(&self) -> session::Listener {
        let into = Arc::clone(&self.0);
        Arc::new(move |s: &Sample| {
            let mut h = into.lock().unwrap();
            h.0 += 1;
            h.1 = *s;
        })
    }
    fn count(&self) -> usize {
        self.0.lock().unwrap().0
    }
    fn last(&self) -> Sample {
        self.0.lock().unwrap().1
    }
    /// Whether `count` reports came within 2 s.
    fn wait(&self, count: usize) -> bool {
        for _ in 0..400 {
            if self.count() >= count {
                return true;
            }
            sleep(Duration::from_millis(5));
        }
        false
    }
}

fn ms(n: u64) {
    sleep(Duration::from_millis(n));
}

#[test]
fn none() {
    let _hw = crate::hw::settings_lock();
    assert!(Battery::open(&Options { dir: Some("/nonexistent/power_supply".into()), ..Options::default() }).is_none());
}

#[test]
fn session() {
    let _hw = crate::hw::settings_lock();
    let root = Root::new("session");
    let r0 = &root.0;
    fake_phone(r0);
    let b = Battery::open(&Options { dir: Some(root.s()), poll_ms: 10, ..Options::default() }).unwrap();
    let s = b.query().unwrap();
    assert!(s.percent == 73 && !s.plugged && s.current_ma == -420);

    // The worker is idle until a listener comes, then reports only what changed.
    let heard = Heard::default();
    b.listen(Some(heard.listener()));
    ms(60);
    assert_eq!(heard.count(), 0);
    write_node(r0, "battery", "status", "Charging\n");
    write_node(r0, "usb", "online", "1\n");
    assert!(heard.wait(1));
    let last = heard.last();
    assert!(last.plugged && last.charging && last.current_ma == 420);
    let after_plug = heard.count();
    write_node(r0, "battery", "capacity", "74\n");
    assert!(heard.wait(after_plug + 1));
    assert_eq!(heard.last().percent, 74);

    // Telemetry alone waits for its interval.
    let n = heard.count();
    write_node(r0, "battery", "temp", "333\n");
    ms(60);
    assert_eq!(heard.count(), n);

    // A battery node that vanishes costs a log line; the last sample stays.
    fs::remove_file(r0.join("battery/capacity")).unwrap();
    assert_eq!(b.query().unwrap_err().percent, 74);
    write_node(r0, "battery", "capacity", "60\n");

    // Listening stops and close joins promptly.
    b.listen(None);
    let n = heard.count();
    ms(40);
    assert_eq!(heard.count(), n);
    b.close();
}

/// The boot of 2026-11-12 23:45: the phone on its charger, Full at 100 %. The guest's query gets the real state
/// (plugged and full) before its service registers; nothing follows while it stays so; an unplug and a plug are each
/// reported.
#[test]
fn session_on_charger() {
    let _hw = crate::hw::settings_lock();
    let root = Root::new("charger");
    let r0 = &root.0;
    fake_phone(r0);
    write_node(r0, "battery", "capacity", "100\n");
    write_node(r0, "battery", "status", "Full\n");
    write_node(r0, "battery", "current_now", "74\n");
    write_node(r0, "usb", "online", "1\n");
    let b = Battery::open(&Options { dir: Some(root.s()), poll_ms: 10, ..Options::default() }).unwrap();
    let s = b.query().unwrap();
    assert!(s.percent == 100 && s.present && s.plugged && s.full && !s.charging);
    let heard = Heard::default();
    b.listen(Some(heard.listener()));
    ms(80); // eight polls of the same state
    assert_eq!(heard.count(), 0);

    write_node(r0, "usb", "online", "0\n");
    write_node(r0, "battery", "status", "Discharging\n");
    assert!(heard.wait(1));
    let last = heard.last();
    assert!(!last.plugged && !last.full && !last.charging);

    let n = heard.count();
    write_node(r0, "usb", "online", "1\n");
    write_node(r0, "battery", "status", "Charging\n");
    assert!(heard.wait(n + 1));
    let last = heard.last();
    assert!(last.plugged && last.charging);
    let n = heard.count();
    ms(60);
    assert_eq!(heard.count(), n); // one report per change
    b.listen(None);
    b.close();
}

/// The treble backend's sample: HealthInfo as the health HAL sends it, made the nodes' reading; an older HAL's
/// shorter parcelable (no design capacity); each BatteryStatus as the nodes' text.
#[test]
fn health() {
    let mut info = HealthInfo {
        usb: true,
        status: hal::BATTERY_FULL,
        present: true,
        level: 100,
        voltage_mv: 4380,
        temp_deci_c: 287,
        current_ua: 12000,
        cycles: 3,
        full_uah: 4_900_000,
        design_uah: 5_000_000,
        time_to_full_s: -1,
        technology: "Li-ion".into(),
        ..HealthInfo::default()
    };
    let raw = from_health(&info);
    assert!(raw.status == "Full" && raw.capacity == Some(100) && raw.present == Some(1) && raw.plugged);
    assert!(raw.temp == Some(287) && raw.voltage == Some(4380) && raw.current == Some(12000));
    assert!(raw.charge_full == Some(4_900_000) && raw.charge_full_design == Some(5_000_000) && raw.cycles == Some(3));
    let s = raw.map(&mut Units { current_ua: true, current_fixed: true }, 0);
    assert!(s.full && s.plugged && s.current_ma == 12 && s.full_mah == 4900 && s.design_mah == 5000);
    // An older HAL: no batteryFullChargeDesignCapacityUah.
    info.design_uah = 0;
    assert_eq!(from_health(&info).charge_full_design, None);
    let names = ["Unknown", "Unknown", "Charging", "Discharging", "Not charging", "Full", "Unknown"];
    for (status, name) in names.iter().enumerate() {
        info.status = status as i32;
        assert_eq!(from_health(&info).status, *name);
    }
}
