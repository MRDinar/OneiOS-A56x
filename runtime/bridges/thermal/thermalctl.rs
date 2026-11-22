// SPDX-License-Identifier: MIT
//! thermalctl: the thermal bridge on the phone, without xnuhost (README.md, "On the phone"). Read only.
//!   thermalctl                 the sensors found, one reading, the level and xnuhost's step with its CPUs
//!   thermalctl watch [-i S] [-n N] [-q]
//!                              a line every S seconds (10), N lines (90, 15 minutes): temperatures, level, CPU and GPU
//!                              clocks, the battery's current, the phone's CPU use, xnuhost's and its session's
//!   thermalctl top [-i S]      over S seconds (10): xnuhost's busiest threads, and the busiest processes of its session
//!   ONEIOS_THERMAL_ROOT=DIR    a fixture instead of the phone's /sys (tests)
//!   ONEIOS_HW_BACKEND, ONEIOS_HW_BACKEND_THERMAL=samsung|treble, ONEIOS_HW_FALLBACK (hw/): the report's sensors
//!                              (treble: the thermal HAL, through the bridge; ONEIOS_BINDER off the phone)

use onebridges::cpu::{CoreKind, Topology};
use onebridges::hw::{self, Backend, Bridge, HwSettings};
use onebridges::thermal::{self, decide, level_name, Limits, Options, Raw, Sensors, Thermal, CAP_CRITICAL};
use onebridges::thermal::{stat_ticks, UNKNOWN};
use std::process::ExitCode;
use std::time::{Duration, Instant};

fn root() -> String {
    std::env::var("ONEIOS_THERMAL_ROOT").unwrap_or_default()
}

/// C's strtoll of the first 63 bytes of a file; `fallback` when it cannot be read or has no number.
fn read_ll(path: &str, fallback: i64) -> i64 {
    use std::io::Read;
    let mut text = [0u8; 63];
    let Ok(mut f) = std::fs::File::open(path) else { return fallback };
    match f.read(&mut text) {
        Ok(n) if n > 0 => thermal::strtoll(&String::from_utf8_lossy(&text[..n])).unwrap_or(fallback),
        _ => fallback,
    }
}

/// Tenths as "41.0", "-" when missing.
fn deci(v: i32) -> String {
    if v == UNKNOWN {
        "-".to_owned()
    } else {
        format!("{}.{}", v / 10, (v % 10).abs())
    }
}

fn mask_text(m: u64) -> String {
    (0..64).filter(|i| m >> i & 1 != 0).map(|i: u32| i.to_string()).collect::<Vec<_>>().join(",")
}

fn report() -> ExitCode {
    let mut settings = HwSettings::default();
    if let Err(bad) = settings.env() {
        println!("thermalctl: bad {bad} (samsung or treble; ONEIOS_HW_FALLBACK 0 or 1)");
        return ExitCode::from(2);
    }
    // The host phone (hw/host_device): its environment defaults (ONEIOS_BRIGHT_MAX on the S26 Ultra) and hw-backend
    // defaults under the environment's own, as xnuhost takes them; ONEIOS_HOST_DEVICE=none: today's defaults.
    match hw::host_tool(&mut settings) {
        Ok((_, line)) => onebridges::br_info!("{line}"),
        Err(line) => {
            println!("thermalctl: {line}");
            return ExitCode::from(2);
        }
    }
    hw::hw_configure(&settings);
    let root = root();
    let mut from_hal = false;
    let mut raw: Option<Raw> = None;
    let mut sensors: Option<Sensors> = None;
    if settings.backend(Bridge::Thermal, Backend::Unset) == Backend::Treble {
        // The treble backend: the bridge itself (its HAL client), one reading.
        let t = Thermal::open(&Options { root: Some(root.clone()), backend: Backend::Treble, ..Options::default() });
        match t {
            None if !settings.fallback_allowed() => {
                println!("thermalctl: no sensor can be read (treble)");
                return ExitCode::from(1);
            }
            None => {}
            Some(t) => {
                raw = Some(t.get().raw);
                let backend = t.backend();
                from_hal = backend == Backend::Treble;
                t.close();
                println!("backend: {}", backend.name());
            }
        }
    }
    let raw = match raw {
        Some(raw) => raw,
        None => {
            let Some(s) = Sensors::find(&root) else {
                println!("thermalctl: no sensor can be read");
                return ExitCode::from(1);
            };
            println!("backend: samsung\nzones: {} of interest", s.zones.len());
            for z in &s.zones {
                println!("  {:<14} {:<4} {}", z.kind, z.group.name(), read_ll(&z.path, -999999));
            }
            let raw = s.read();
            println!(
                "kernel trips (C): first passive {}, hot {} (the median of {} SoC zones)",
                deci(s.trip_first),
                deci(s.trip_hot),
                s.trip_zones
            );
            sensors = Some(s);
            raw
        }
    };
    let mut l = Limits::default();
    // As xnuhost takes them (Thermal::open): the kernel's trips with ONEIOS_THERMAL_SOC_TRIPS=1 (files backend only),
    // the window of ONEIOS_THERMAL_SOC_SAMPLES (one reading here: thermalctl reads once).
    let trips = std::env::var("ONEIOS_THERMAL_SOC_TRIPS").is_ok_and(|v| v == "1");
    // A treble bridge that fell back read the files through the bridge: its trips are unknown here, as in C.
    let (first, hot) = sensors.as_ref().map_or((UNKNOWN, UNKNOWN), |s| (s.trip_first, s.trip_hot));
    if !from_hal && trips && l.from_trips(first, hot) {
        println!("SoC rule: raised to the kernel's trips (ONEIOS_THERMAL_SOC_TRIPS=1)");
    }
    let st = decide(&l, &raw, None);
    let zone = if raw.soc_zone.is_empty() { "-" } else { &raw.soc_zone };
    println!(
        "reading (C): cpu {} gpu {} ddr {} soc {} ({zone}) xo {} ap {} battery {} lrp {}; case = {} ({})",
        deci(raw.cpu),
        deci(raw.gpu),
        deci(raw.ddr),
        deci(raw.soc),
        deci(raw.xo),
        deci(raw.ap),
        deci(raw.battery),
        deci(raw.lrp),
        deci(raw.skin),
        if raw.lrp != UNKNOWN {
            "lrp"
        } else if from_hal {
            "the HAL's SKIN"
        } else {
            "ap"
        }
    );
    println!(
        "level: {} ({}); case rule {}, SoC rule {}, battery rule {}; xnuhost step {}",
        level_name(st.level),
        st.level,
        st.skin_level,
        st.soc_level,
        st.battery_level,
        st.cap
    );
    if let Some(topo) = Topology::read(&format!("{root}/sys")) {
        for cap in [0, CAP_CRITICAL] {
            // the two xnuhost uses
            println!(
                "step {cap}{}: E vCPUs on cpu {}, P vCPUs on cpu {}",
                if cap != 0 { " (case or cell at 50 C)" } else { " (all cores)" },
                mask_text(topo.thermal_mask(CoreKind::E, cap)),
                mask_text(topo.thermal_mask(CoreKind::P, cap))
            );
        }
    }
    ExitCode::SUCCESS
}

// ---- watch --------------------------------------------------------------------------------------

fn read_head(path: &str, max: usize) -> Option<String> {
    use std::io::Read;
    let mut buf = vec![0; max];
    let n = std::fs::File::open(path).ok()?.read(&mut buf).ok()?;
    (n > 0).then(|| String::from_utf8_lossy(&buf[..n]).into_owned())
}

fn proc_ticks(pid: i32) -> Option<u64> {
    stat_ticks(&read_head(&format!("/proc/{pid}/stat"), 1023)?)
}

/// The pids under /proc.
fn pids(dir: &str) -> Vec<i32> {
    std::fs::read_dir(dir)
        .into_iter()
        .flatten()
        .flatten()
        .filter_map(|e| e.file_name().to_str().and_then(|n| n.parse().ok()))
        .filter(|&pid| pid > 0)
        .collect()
}

fn find_xnuhost() -> i32 {
    pids("/proc")
        .into_iter()
        .find(|pid| read_head(&format!("/proc/{pid}/comm"), 31).as_deref() == Some("xnuhost\n"))
        .unwrap_or(0)
}

fn mount_ns(path: &str) -> Option<std::path::PathBuf> {
    std::fs::read_link(path).ok()
}

/// CPU ticks of every process in xnuhost's mount namespace: the whole session (iOS's processes are Linux processes
/// there).
fn session_ticks(xnuhost: i32) -> u64 {
    let Some(ns) = mount_ns(&format!("/proc/{xnuhost}/ns/mnt")) else { return 0 };
    pids("/proc")
        .into_iter()
        .filter(|pid| mount_ns(&format!("/proc/{pid}/ns/mnt")).as_ref() == Some(&ns))
        .filter_map(proc_ticks)
        .sum()
}

/// The busy and all ticks of /proc/stat's "cpu" line (guest and guest_nice are in user already).
fn cpu_stat() -> Option<(u64, u64)> {
    let text = std::fs::read_to_string("/proc/stat").ok()?;
    let v: Vec<u64> = text.strip_prefix("cpu")?.split_whitespace().take(10).map_while(|t| t.parse().ok()).collect();
    if v.len() < 5 {
        return None;
    }
    let all: u64 = v.iter().take(8).sum();
    Some((all - v[3] - v[4], all))
}

fn clk_tck() -> f64 {
    // SAFETY: sysconf has no memory effects.
    let hz = unsafe { libc::sysconf(libc::_SC_CLK_TCK) };
    if hz > 0 {
        hz as f64
    } else {
        100.0
    }
}

/// C's atoi of an option's value.
fn atoi(text: &str) -> u32 {
    let t = text.trim_start();
    let digits = t.bytes().take_while(u8::is_ascii_digit).count();
    t[..digits].parse().unwrap_or(0)
}

fn local_time() -> (i32, i32, i32) {
    // SAFETY: time with a null pointer only returns the time.
    let now = unsafe { libc::time(std::ptr::null_mut()) };
    // SAFETY: a zeroed tm is valid for localtime_r to fill.
    let mut tm: libc::tm = unsafe { std::mem::zeroed() };
    // SAFETY: both pointers live across the call.
    unsafe { libc::localtime_r(&now, &mut tm) };
    (tm.tm_hour, tm.tm_min, tm.tm_sec)
}

fn watch(args: &[String]) -> ExitCode {
    let (mut interval, mut count, mut quiet) = (10u32, 90u32, false);
    let mut it = args[2..].iter();
    while let Some(a) = it.next() {
        match a.as_str() {
            "-i" if it.len() > 0 => interval = atoi(it.next().unwrap()),
            "-n" if it.len() > 0 => count = atoi(it.next().unwrap()),
            "-q" => quiet = true,
            _ => {
                eprintln!("usage: thermalctl watch [-i SECONDS] [-n LINES] [-q]");
                return ExitCode::from(2);
            }
        }
    }
    let interval = interval.max(1);
    let root = root();
    let Some(s) = Sensors::find(&root) else {
        println!("thermalctl: no sensor can be read");
        return ExitCode::from(1);
    };
    let l = Limits::default();
    let mut prev = None;
    let hz = clk_tck();
    if !quiet {
        println!(
            "# time cpu gpu ddr xo ap battery lrp (C) level step | MHz cpu0 cpu2 cpu5 cpu7 (cpu7 max) gpu gpu% | \
             battery mA mV | CPU% phone xnuhost session"
        );
    }
    let (mut busy0, mut all0) = cpu_stat().unwrap_or((0, 0));
    let mut xpid = find_xnuhost();
    let (mut x0, mut s0) = if xpid != 0 { (proc_ticks(xpid).unwrap_or(0), session_ticks(xpid)) } else { (0, 0) };
    let mut t0 = Instant::now();
    let freq = |policy: &str, what: &str| {
        read_ll(&format!("{root}/sys/devices/system/cpu/cpufreq/policy{policy}/{what}"), 0) / 1000
    };
    for _ in 0..count {
        std::thread::sleep(Duration::from_secs(interval.into()));
        let t1 = Instant::now();
        let mut span = (t1 - t0).as_secs_f64(); // the reads take time too
        t0 = t1;
        if span <= 0.0 {
            span = f64::from(interval);
        }
        let raw = s.read();
        let st = decide(&l, &raw, prev.as_ref());
        let mut phone = -1.0;
        if let Some((busy, all)) = cpu_stat() {
            if all > all0 {
                phone = 100.0 * 8.0 * busy.wrapping_sub(busy0) as f64 / (all - all0) as f64;
            }
            (busy0, all0) = (busy, all);
        }
        let (mut xh, mut ses) = (-1.0, -1.0);
        let (mut x, mut se) = (0, 0);
        let now_pid = find_xnuhost();
        let same = if now_pid != 0 && now_pid == xpid { proc_ticks(xpid) } else { None };
        if let Some(ticks) = same {
            x = ticks;
            se = session_ticks(xpid);
            xh = 100.0 * x.wrapping_sub(x0) as f64 / hz / span;
            ses = if se >= s0 { 100.0 * (se - s0) as f64 / hz / span } else { -1.0 };
        } else if now_pid != 0 {
            x = proc_ticks(now_pid).unwrap_or(0);
            se = session_ticks(now_pid);
        }
        (xpid, x0, s0) = (now_pid, x, se);
        let (h, m, sec) = local_time();
        let kgsl = |what: &str, fallback| read_ll(&format!("{root}/sys/class/kgsl/kgsl-3d0/{what}"), fallback);
        let battery = |what: &str| read_ll(&format!("{root}/sys/class/power_supply/battery/{what}"), 0);
        let mut line = format!(
            "{h:02}:{m:02}:{sec:02} {} {} {} {} {} {} {} {} {} | {} {} {} {} ({}) {} {} | {} {} | ",
            deci(raw.cpu),
            deci(raw.gpu),
            deci(raw.ddr),
            deci(raw.xo),
            deci(raw.ap),
            deci(raw.battery),
            deci(raw.lrp),
            level_name(st.level),
            st.cap,
            freq("0", "scaling_cur_freq"),
            freq("2", "scaling_cur_freq"),
            freq("5", "scaling_cur_freq"),
            freq("7", "scaling_cur_freq"),
            freq("7", "scaling_max_freq"),
            kgsl("gpuclk", 0) / 1000000,
            kgsl("gpu_busy_percentage", -1),
            battery("current_now"),
            battery("voltage_now") / 1000
        );
        line += &if phone >= 0.0 { format!("{phone:.0} ") } else { "- ".to_owned() };
        line += &if xh >= 0.0 { format!("{xh:.0} {ses:.0}") } else { "- -".to_owned() };
        println!("{line}");
        prev = Some(st);
    }
    ExitCode::SUCCESS
}

// ---- top ----------------------------------------------------------------------------------------

struct Usage {
    id: i32,
    name: String,
    ticks: u64,
}

/// Every DIR/<id>/stat's ticks (in `ns_of`'s mount namespace only, when given), at most `max`.
fn snapshot(dir: &str, ns_of: Option<i32>, max: usize) -> Vec<Usage> {
    let ns = match ns_of {
        Some(pid) => match mount_ns(&format!("/proc/{pid}/ns/mnt")) {
            Some(ns) => Some(ns),
            None => return Vec::new(),
        },
        None => None,
    };
    let mut out = Vec::new();
    for id in pids(dir) {
        if out.len() >= max {
            break;
        }
        if let Some(ns) = &ns {
            if mount_ns(&format!("{dir}/{id}/ns/mnt")).as_ref() != Some(ns) {
                continue;
            }
        }
        let Some(text) = read_head(&format!("{dir}/{id}/stat"), 1023) else { continue };
        let (Some(open), Some(close)) = (text.find('('), text.rfind(')')) else { continue };
        let Some(ticks) = stat_ticks(&text) else { continue };
        let name = onebridges::log::truncate(text.get(open + 1..close).unwrap_or(""), 23).to_owned();
        out.push(Usage { id, name, ticks });
    }
    out
}

/// The busiest of `now` against `then`, by id, as a percentage of one CPU over `span` seconds.
fn busiest(title: &str, then: &[Usage], mut now: Vec<Usage>, span: f64, hz: f64, shown: usize) {
    for u in &mut now {
        // new in the interval: all of it
        let before = then.iter().find(|t| t.id == u.id).map_or(u.ticks, |t| t.ticks);
        u.ticks = u.ticks.saturating_sub(before);
    }
    now.sort_by_key(|u| std::cmp::Reverse(u.ticks));
    let all: u64 = now.iter().map(|u| u.ticks).sum();
    println!("{title}: {:.0}% in all", 100.0 * all as f64 / hz / span);
    for u in now.iter().take(shown).take_while(|u| u.ticks != 0) {
        println!("  {:6} {:<24} {:5.1}%", u.id, u.name, 100.0 * u.ticks as f64 / hz / span);
    }
}

fn top(args: &[String]) -> ExitCode {
    let mut interval = 10;
    let mut it = args[2..].iter();
    while let Some(a) = it.next() {
        if a == "-i" && it.len() > 0 {
            interval = atoi(it.next().unwrap());
        } else {
            eprintln!("usage: thermalctl top [-i SECONDS]");
            return ExitCode::from(2);
        }
    }
    let x = find_xnuhost();
    if x == 0 {
        println!("thermalctl: no xnuhost running");
        return ExitCode::from(1);
    }
    let tasks = format!("/proc/{x}/task");
    let (t0, p0) = (snapshot(&tasks, None, 512), snapshot("/proc", Some(x), 2048));
    let a = Instant::now();
    std::thread::sleep(Duration::from_secs(if interval != 0 { interval.into() } else { 1 }));
    let (t1, p1) = (snapshot(&tasks, None, 512), snapshot("/proc", Some(x), 2048));
    let span = a.elapsed().as_secs_f64();
    let hz = clk_tck();
    busiest(&format!("xnuhost {x}, its threads ({span:.1} s)"), &t0, t1, span, hz, 16);
    busiest("the session's processes (xnuhost's mount namespace)", &p0, p1, span, hz, 12);
    ExitCode::SUCCESS
}

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().collect();
    match args.get(1).map(String::as_str) {
        None => report(),
        Some("watch") => watch(&args),
        Some("top") => top(&args),
        Some(_) => {
            eprintln!("usage: thermalctl [watch [-i SECONDS] [-n LINES] [-q] | top [-i SECONDS]]");
            ExitCode::from(2)
        }
    }
}
