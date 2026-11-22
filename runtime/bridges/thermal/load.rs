// SPDX-License-Identifier: MIT
//! Where the heat comes from (2026-11-14): one sample of the host's load from named files only (never a folder list),
//! and the "thermal: N s: host CPU ..." line between two samples.

use super::{deci_text, strtoll, Raw};
use xh_core::time::monotonic_ms;

/// The "thermal: load" line's period.
pub const LOAD_MS: u32 = 30000;

/// One sample: root/proc/stat's "cpu" line and its cpuN count, root/proc/vmstat's pswpin and pswpout,
/// root/proc/self/stat's utime and stime (this process: xnuhost), and root/sys/class/power_supply/battery/{current_now,
/// status}. A field that cannot be read is 0 with its have_* false.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct HostLoad {
    pub have_cpu: bool,
    pub have_swap: bool,
    pub have_self: bool,
    pub have_current: bool,
    /// cpuN lines of /proc/stat
    pub ncpu: u32,
    /// The "cpu" line, ticks summed over every CPU.
    pub user: u64,
    pub nice: u64,
    pub system: u64,
    pub idle: u64,
    pub iowait: u64,
    pub irq: u64,
    pub softirq: u64,
    pub steal: u64,
    /// Pages since boot.
    pub pswpin: u64,
    pub pswpout: u64,
    /// utime + stime of root/proc/self/stat
    pub self_ticks: u64,
    /// current_now as the phone gives it (Samsung: mA; + charging, - discharging)
    pub current: i64,
    /// "Charging", "Discharging", "Full", "Not charging", or "" (at most 15 bytes)
    pub status: String,
    /// When (CLOCK_MONOTONIC; the caller may set it in tests).
    pub ms: u64,
}

/// Up to `max` bytes of root/rel; None when it cannot be read or is empty.
fn read_text(root: &str, rel: &str, max: usize) -> Option<String> {
    let buf = xh_core::fs::read_bytes(format!("{root}{rel}"), max).ok()?;
    (!buf.is_empty()).then(|| String::from_utf8_lossy(&buf).into_owned())
}

/// utime + stime of a /proc/PID/stat text: fields 14 and 15, counted after the comm's closing parenthesis (a comm may
/// hold spaces and parentheses).
pub fn stat_ticks(text: &str) -> Option<u64> {
    let after = &text[text.rfind(')')? + 1..];
    // state ppid pgrp session tty tpgid flags minflt cminflt majflt cmajflt, then utime stime
    let mut fields = after.split_whitespace().skip(11);
    let utime: u64 = fields.next()?.parse().ok()?;
    let stime: u64 = fields.next()?.parse().ok()?;
    Some(utime + stime)
}

impl HostLoad {
    pub fn read(root: &str) -> HostLoad {
        let mut out = HostLoad { ms: monotonic_ms(), ..HostLoad::default() };
        const MAX: usize = 16383;
        if let Some(text) = read_text(root, "/proc/stat", MAX) {
            if let Some(rest) = text.strip_prefix("cpu") {
                let v: Vec<u64> = rest.split_whitespace().take(8).map_while(|t| t.parse().ok()).collect();
                if v.len() >= 4 {
                    let at = |i: usize| v.get(i).copied().unwrap_or(0);
                    (out.user, out.nice, out.system, out.idle) = (at(0), at(1), at(2), at(3));
                    (out.iowait, out.irq, out.softirq, out.steal) = (at(4), at(5), at(6), at(7));
                    out.have_cpu = true;
                }
            }
            out.ncpu = text
                .match_indices("\ncpu")
                .filter(|(i, _)| text[i + 4..].starts_with(|c: char| c.is_ascii_digit()))
                .count() as u32;
        }
        if let Some(text) = read_text(root, "/proc/vmstat", MAX) {
            let (mut pswpin, mut pswpout) = (None, None);
            let value = |rest: &str| rest.split_whitespace().next()?.parse::<u64>().ok();
            for line in text.lines() {
                if let Some(v) = line.strip_prefix("pswpin").and_then(value) {
                    pswpin = Some(v);
                } else if let Some(v) = line.strip_prefix("pswpout").and_then(value) {
                    pswpout = Some(v);
                }
            }
            out.pswpin = pswpin.unwrap_or(0);
            out.pswpout = pswpout.unwrap_or(0);
            out.have_swap = pswpin.is_some() && pswpout.is_some();
        }
        if let Some(ticks) = read_text(root, "/proc/self/stat", MAX).as_deref().and_then(stat_ticks) {
            out.self_ticks = ticks;
            out.have_self = true;
        }
        if let Some(v) = read_text(root, "/sys/class/power_supply/battery/current_now", 63).as_deref().and_then(strtoll)
        {
            out.current = v;
            out.have_current = true;
        }
        if let Some(text) = read_text(root, "/sys/class/power_supply/battery/status", 63) {
            out.status = crate::log::truncate(text.split('\n').next().unwrap_or(""), 15).to_owned();
        }
        out
    }

    fn total(&self) -> u64 {
        self.user + self.nice + self.system + self.idle + self.iowait + self.irq + self.softirq + self.steal
    }

    /// The one line between two samples, after "thermal: ":
    ///   "30 s: host CPU 800% of 800 (sys 226%, user 167%, irq 12%), swap in 9520 out 8040 KiB/s, xnuhost 152%;
    ///    battery 40.3 C, Charging 1600 mA; case 41.0 C, SoC 78.0 C (cpu-1-2-1)"
    /// Percentages are of one CPU, as top shows them (all busy = ncpu x 100). Missing parts say "-". `page` is the
    /// host's page size in bytes (for swap KiB/s).
    pub fn line(was: &HostLoad, now: &HostLoad, raw: Option<&Raw>, page: u64) -> String {
        let ms = now.ms.saturating_sub(was.ms);
        let mut out = format!("{} s: host CPU ", (ms + 500) / 1000);
        // Per cent of one CPU, as top shows it: the share of every CPU's ticks, times the CPUs.
        let ncpu = u64::from(if now.ncpu != 0 { now.ncpu } else { 1 });
        let dt = now.total().saturating_sub(was.total());
        let share = |d: u64| d.wrapping_mul(100 * ncpu).wrapping_add(dt / 2).checked_div(dt).unwrap_or(0);
        let diff = |f: fn(&HostLoad) -> u64| f(now).saturating_sub(f(was));
        if now.have_cpu && was.have_cpu && dt != 0 {
            let busy = dt.wrapping_sub(diff(|l| l.idle)).wrapping_sub(diff(|l| l.iowait));
            out += &format!(
                "{}% of {} (sys {}%, user {}%, irq {}%)",
                share(busy),
                ncpu * 100,
                share(diff(|l| l.system)),
                share(diff(|l| l.user) + diff(|l| l.nice)),
                share(diff(|l| l.irq) + diff(|l| l.softirq))
            );
        } else {
            out += "-";
        }
        if now.have_swap && was.have_swap && ms != 0 {
            let rate = |pages: u64| pages * page / 1024 * 1000 / ms;
            out += &format!(", swap in {} out {} KiB/s", rate(diff(|l| l.pswpin)), rate(diff(|l| l.pswpout)));
        } else {
            out += ", swap -";
        }
        // xnuhost's own ticks against the ticks one CPU had in the interval
        if now.have_self && was.have_self && dt != 0 {
            out += &format!(", xnuhost {}%", share(diff(|l| l.self_ticks)));
        } else {
            out += ", xnuhost -";
        }
        let reading = |f: fn(&Raw) -> i32| deci_text(raw.map_or(super::UNKNOWN, f));
        out +=
            &format!("; battery {}, {}", reading(|r| r.battery), if now.status.is_empty() { "-" } else { &now.status });
        if now.have_current {
            out += &format!(" {} mA", now.current);
        }
        let zone = raw.map(|r| r.soc_zone.as_str()).filter(|z| !z.is_empty()).unwrap_or("-");
        out += &format!("; case {}, SoC {} ({zone})", reading(|r| r.skin), reading(|r| r.soc));
        out
    }
}
