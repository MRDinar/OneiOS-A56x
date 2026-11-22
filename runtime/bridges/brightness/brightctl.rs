// SPDX-License-Identifier: MIT
//! brightctl: the brightness bridge on the phone, without xnuhost or a guest. As root, One UI running.
//!   brightctl inspect                 passive, read-only sysfs inspection while One UI is running
//!   brightctl info                    opens bridge; MAY WRITE panel/touch/ALS
//!   brightctl levels [--ms N] [L...]  sets each level (percent of iOS's slider, default 5 25 50 75 100 10) for N ms
//!                                     (default 2000), with a 300 ms fade, then restores
//!   brightctl als [SECONDS]           live light sensor readings (default 15 s)
//! Environment: ONEIOS_BRIGHT_MAX, ONEIOS_BRIGHT_MIN, ONEIOS_ALS_SCALE (brightness/), and for a test off the phone
//! ONEIOS_BRIGHT_DIR, ONEIOS_INPUT_DIR, ONEIOS_INPUT_SYSFS, ONEIOS_TSP_DIR. The backend: ONEIOS_HW_BACKEND,
//! ONEIOS_HW_BACKEND_BACKLIGHT (samsung, the default, or treble), ONEIOS_HW_FALLBACK (hw/).
//! It ends with "brightctl: ok" or the reason it failed. SIGINT and SIGTERM restore the panel too.

use onebridges::brightness::{self, AlsSample, Bright, Info, Options, FULL};
use onebridges::hw::{self, HwSettings};
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::Arc;
use std::time::Duration;

/// Inspect only: never opens the bridge, which can set an initial brightness, touch the IC and turn on ALS when One
/// UI left sysfs at zero. The value of DIR/NAME/FIELD: a whole number (white space after it only), else None.
fn panel_read_u32(dir: &Path, name: &str, field: &str) -> Option<u32> {
    use std::io::Read;
    let mut text = [0u8; 63];
    let n = std::fs::File::open(dir.join(name).join(field)).ok()?.read(&mut text).ok()?;
    let text = std::str::from_utf8(&text[..n]).ok()?;
    let t = text.trim_start_matches([' ', '\t', '\n', '\r', '\x0b', '\x0c']);
    let t = t.strip_prefix('+').unwrap_or(t);
    let digits = t.bytes().take_while(u8::is_ascii_digit).count();
    if n == 0 || digits == 0 || !t[digits..].trim_start_matches([' ', '\t', '\r', '\n']).is_empty() {
        return None;
    }
    t[..digits].parse().ok()
}

fn inspect_readonly() -> ExitCode {
    const NAMES: [&str; 5] = ["panel", "panel0-backlight", "panel1-backlight", "backlight", "lcd-backlight"];
    let dir =
        std::env::var("ONEIOS_BRIGHT_DIR").ok().filter(|d| !d.is_empty()).unwrap_or("/sys/class/backlight".into());
    let dir = Path::new(&dir);
    println!("brightctl: passive inspect (read-only; no panel, touch IC, or ALS writes)");
    println!("directory: {}", dir.display());
    let (mut selected, mut scanned) = (false, 0);
    let shown = |v: Option<u32>| v.map_or("unavailable".to_owned(), |v| v.to_string());
    for name in NAMES {
        let Some(max) = panel_read_u32(dir, name, "max_brightness") else { continue };
        scanned += 1;
        let saved = panel_read_u32(dir, name, "brightness");
        let actual = panel_read_u32(dir, name, "actual_brightness");
        let power = panel_read_u32(dir, name, "bl_power");
        let chosen = max > 0 && !selected;
        selected |= chosen;
        let note = if max == 0 {
            "; skipped (max=0)"
        } else if chosen {
            "; selected"
        } else {
            ""
        };
        println!(
            "node {name}: max {max}; brightness {}; actual {}; bl_power {}{note}",
            shown(saved),
            shown(actual),
            shown(power)
        );
        if chosen && saved.unwrap_or(0) == 0 {
            println!(
                "WARNING: selected node brightness is {}; do not use brightctl info/levels on a running One UI host \
                 until the brightness path is validated. Sysfs zero does not prove the panel is off.",
                if saved.is_some() { "0" } else { "unavailable" }
            );
        }
    }
    if scanned == 0 {
        println!("brightctl: no readable named backlight nodes");
        return ExitCode::from(1);
    }
    if !selected {
        println!("brightctl: no usable positive-max backlight node");
        return ExitCode::from(1);
    }
    println!("brightctl: inspect ok (no state changes)");
    ExitCode::SUCCESS
}

static STOPPED: AtomicBool = AtomicBool::new(false);

extern "C" fn on_signal(_: libc::c_int) {
    STOPPED.store(true, Ordering::Relaxed);
}

fn catch_signals() {
    for signal in [libc::SIGINT, libc::SIGTERM, libc::SIGHUP] {
        // SAFETY: the handler only stores to an atomic, which is async-signal-safe.
        unsafe { libc::signal(signal, on_signal as extern "C" fn(libc::c_int) as libc::sighandler_t) };
    }
}

/// Waits `ms`, in steps of 20 ms, until a signal comes.
fn wait_ms(ms: u32) {
    let mut done = 0;
    while done < ms && !STOPPED.load(Ordering::Relaxed) {
        std::thread::sleep(Duration::from_millis(20));
        done += 20;
    }
}

/// C's (unsigned)atoi or strtoul of a variable: the leading number, 0 without one.
fn number(text: &str) -> u32 {
    xh_core::parse::strtoul(text, 10).map_or(0, |(v, _)| v as u32)
}

fn var(name: &str) -> Option<String> {
    std::env::var(name).ok()
}

fn open_bridge(no_als: bool) -> Option<Bright> {
    let o = Options {
        backlight_dir: var("ONEIOS_BRIGHT_DIR").map(PathBuf::from),
        input_dir: var("ONEIOS_INPUT_DIR").map(PathBuf::from),
        input_sysfs: var("ONEIOS_INPUT_SYSFS").map(PathBuf::from),
        ceiling: var("ONEIOS_BRIGHT_MAX").map_or(0, |t| number(&t)),
        floor: var("ONEIOS_BRIGHT_MIN").map_or(0, |t| number(&t)),
        als_millilux_per_count: var("ONEIOS_ALS_SCALE").map_or(0, |t| number(&t)),
        no_als,
        // the phone test leaves the touch IC alone
        touch_dir: Some(var("ONEIOS_TSP_DIR").unwrap_or("/nonexistent".into()).into()),
        ..Options::default()
    };
    Bright::open(&o)
}

fn print_info(i: &Info) {
    println!("backend: {}", i.backend.name());
    if i.panel {
        println!(
            "panel: {}  max_brightness {}, slider range {}..{}, One UI's value {}",
            i.node, i.max_brightness, i.floor, i.ceiling, i.saved
        );
    } else {
        println!("panel: no backlight device");
    }
    if i.als {
        println!("light sensor: {}", i.als_node);
    } else {
        println!("light sensor: none");
    }
}

fn on_als(s: &AlsSample) {
    // SAFETY: a zeroed timespec is valid, and clock_gettime fills it.
    let mut t: libc::timespec = unsafe { std::mem::zeroed() };
    // SAFETY: t lives across the call.
    unsafe { libc::clock_gettime(libc::CLOCK_MONOTONIC, &mut t) };
    println!(
        "{:6}.{:03}  clear {:<8} ir {:<8} uv {:<8} -> {}.{:03} lux",
        t.tv_sec % 100000,
        t.tv_nsec / 1000000,
        s.clear,
        s.ir,
        s.uv,
        s.millilux / 1000,
        s.millilux % 1000
    );
}

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().collect();
    let command = args.get(1).map_or("", String::as_str);
    // Before any bridge initialization: unlike "info", this has no write path.
    if command == "inspect" {
        return if args.len() == 2 { inspect_readonly() } else { ExitCode::from(2) };
    }
    catch_signals();
    let mut settings = HwSettings::default();
    if let Err(bad) = settings.env() {
        println!("brightctl: failed: bad {bad} (samsung or treble; ONEIOS_HW_FALLBACK 0 or 1)");
        return ExitCode::from(2);
    }
    // The host phone (hw/host_device): its environment defaults (ONEIOS_BRIGHT_MAX on the S26 Ultra) and hw-backend
    // defaults under the environment's own, as xnuhost takes them; ONEIOS_HOST_DEVICE=none: today's defaults.
    match hw::host_tool(&mut settings) {
        Ok((_, line)) => onebridges::br_info!("{line}"),
        Err(line) => {
            println!("brightctl: failed: {line}");
            return ExitCode::from(2);
        }
    }
    hw::hw_configure(&settings);

    match command {
        "info" => {
            let Some(b) = open_bridge(false) else {
                println!("brightctl: failed: no backlight device and no light sensor");
                return ExitCode::from(1);
            };
            print_info(&b.info());
            b.close();
            println!("brightctl: ok");
            ExitCode::SUCCESS
        }
        "levels" => {
            let (mut ms, mut percents) = (2000u32, Vec::new());
            let mut rest = args[2..].iter();
            while let Some(a) = rest.next() {
                if a == "--ms" && rest.len() > 0 {
                    ms = number(rest.next().unwrap());
                } else if percents.len() < 16 {
                    percents.push(number(a));
                }
            }
            if percents.is_empty() {
                percents = vec![5, 25, 50, 75, 100, 10];
            }
            let Some(b) = open_bridge(true).filter(|b| b.info().panel) else {
                println!("brightctl: failed: no writable backlight device");
                return ExitCode::from(1);
            };
            let i = b.info();
            print_info(&i);
            for percent in percents {
                if STOPPED.load(Ordering::Relaxed) {
                    break;
                }
                let percent = percent.min(100);
                let level = FULL * percent / 100;
                b.set(level, false, 300);
                wait_ms(500);
                println!(
                    "slider {percent:3}% -> wanted {}, the node was written {}",
                    brightness::map(level, false, i.floor, i.ceiling),
                    b.current()
                );
                wait_ms(ms.saturating_sub(500));
            }
            b.close();
            println!(
                "restored {}\nbrightctl: {}",
                i.saved,
                if STOPPED.load(Ordering::Relaxed) { "stopped" } else { "ok" }
            );
            ExitCode::SUCCESS
        }
        "als" => {
            let seconds = args.get(2).map_or(15, |s| number(s));
            let Some(b) = open_bridge(false).filter(|b| b.info().als) else {
                println!("brightctl: failed: no light sensor (no input device named als_rear)");
                return ExitCode::from(1);
            };
            print_info(&b.info());
            let count = Arc::new(AtomicU32::new(0));
            let counter = Arc::clone(&count);
            b.listen(Some(Arc::new(move |s: &AlsSample| {
                on_als(s);
                counter.fetch_add(1, Ordering::Relaxed);
            })));
            wait_ms(seconds.saturating_mul(1000));
            b.listen(None);
            b.close();
            let count = count.load(Ordering::Relaxed);
            println!(
                "{count} readings in {seconds} s\nbrightctl: {}",
                if count != 0 { "ok" } else { "failed: the sensor sent nothing (is its enable file writable?)" }
            );
            ExitCode::from(if count != 0 { 0 } else { 1 })
        }
        _ => {
            println!(
                "usage: brightctl inspect (passive) | info (active!) | levels [--ms N] [PERCENT...] | als [SECONDS]"
            );
            ExitCode::from(2)
        }
    }
}
