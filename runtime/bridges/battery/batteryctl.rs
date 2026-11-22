// SPDX-License-Identifier: MIT
//! batteryctl: what the battery bridge reads on this phone. Run as root with One UI up; reads only.
//!   batteryctl            the sample once, and what iOS would be told
//!   batteryctl watch [S]  a line whenever the sample changes, for S seconds (default 60): plug and unplug
//! The backend: ONEIOS_HW_BACKEND, ONEIOS_HW_BACKEND_BATTERY (samsung, the default: the power_supply nodes; treble:
//! the health HAL), ONEIOS_HW_FALLBACK (hw/). Off the phone: ONEIOS_BATTERY_DIR (the power_supply class folder),
//! ONEIOS_BINDER (the binder device, for the treble backend).

use onebridges::battery::{Battery, Options, Sample};
use onebridges::hw::{self, Backend, Bridge, HwSettings};
use std::process::ExitCode;
use std::sync::Arc;
use std::time::Duration;

fn print(what: &str, s: &Sample) {
    println!(
        "{what}: {}% present={} plugged={} charging={} full={} temp={} (tenths C) voltage={} mV current={} mA full={} \
         mAh design={} mAh cycles={} minutes={}",
        s.percent,
        s.present as i32,
        s.plugged as i32,
        s.charging as i32,
        s.full as i32,
        s.temp_deci_c,
        s.voltage_mv,
        s.current_ma,
        s.full_mah,
        s.design_mah,
        s.cycles,
        s.minutes
    );
}

/// C's atoi: the leading number, 0 without one.
fn atoi(text: &str) -> i64 {
    let t = text.trim_start();
    let (sign, t) = match t.strip_prefix('-') {
        Some(rest) => (-1, rest),
        None => (1, t.strip_prefix('+').unwrap_or(t)),
    };
    let digits = t.bytes().take_while(u8::is_ascii_digit).count();
    sign * t[..digits].parse::<i64>().unwrap_or(0)
}

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().collect();
    let watch = args.get(1).is_some_and(|a| a == "watch");
    let seconds = if watch { args.get(2).map_or(60, |s| atoi(s)) } else { 60 };
    let mut settings = HwSettings::default();
    if let Err(bad) = settings.env() {
        println!("batteryctl: failed: bad {bad} (samsung or treble; ONEIOS_HW_FALLBACK 0 or 1)");
        return ExitCode::from(2);
    }
    // The host phone (hw/host_device): its environment defaults (ONEIOS_BRIGHT_MAX on the S26 Ultra) and hw-backend
    // defaults under the environment's own, as xnuhost takes them; ONEIOS_HOST_DEVICE=none: today's defaults.
    match hw::host_tool(&mut settings) {
        Ok((_, line)) => onebridges::br_info!("{line}"),
        Err(line) => {
            println!("batteryctl: failed: {line}");
            return ExitCode::from(2);
        }
    }
    hw::hw_configure(&settings);
    let backend = settings.backend(Bridge::Battery, Backend::Unset);
    // ONEIOS_BATTERY_DIR: the options (the bridge then reads no other variable); otherwise the phone's paths and
    // variables.
    let o = match std::env::var("ONEIOS_BATTERY_DIR") {
        Ok(dir) => {
            Options { dir: Some(dir), supplies: std::env::var("ONEIOS_BATTERY_SUPPLIES").ok(), ..Options::default() }
        }
        Err(_) => Options::from_env(),
    };
    let Some(b) = Battery::open(&o) else {
        println!("batteryctl: failed: no battery ({} backend)", backend.name());
        return ExitCode::from(1);
    };
    println!("backend: {}", b.backend().name());
    let s = b.query().unwrap_or_else(|last| last);
    print("sample", &s);
    if watch {
        b.listen(Some(Arc::new(|s: &Sample| print("change", s))));
        std::thread::sleep(Duration::from_secs(if seconds > 0 { seconds as u64 } else { 60 }));
        b.listen(None);
    }
    b.close();
    println!("batteryctl: ok");
    ExitCode::SUCCESS
}
