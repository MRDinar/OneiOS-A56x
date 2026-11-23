// SPDX-License-Identifier: MIT
//! What the host's clocks did (2026-11-23): each cpufreq policy's busy share and frequency residency between two
//! samples, the "thermal: N s clocks: ..." line beside the load line. From named files only (never a folder list):
//! root/proc/stat's cpuN lines and, for policy0..policy15 that exist,
//! root/sys/devices/system/cpu/cpufreq/policyN/{stats/time_in_state, related_cpus, cpuinfo_max_freq}.
//!
//! Why: with One UI's power HAL gone (the switch stops system_server) the governor alone picks the clocks, from the load
//! and the uclamp xnuhost gives its threads; the thermal line said the SoC was at 92-98 C and nothing said which cluster
//! ran at which clock for how long (the session of 2026-11-23 20:23). time_in_state counts every moment of the policy at
//! a frequency, idle or not; the busy share is its CPUs' non-idle ticks.

use xh_core::time::monotonic_ms;

/// Policies looked for (policy0..policy15, by name).
const MAX_POLICIES: u32 = 16;

/// One policy at one sample.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct PolicyClocks {
    /// N of policyN
    pub policy: u32,
    /// related_cpus (the frequency domain)
    pub cpus: Vec<u32>,
    /// cpuinfo_max_freq, or the highest frequency time_in_state lists when that is higher (a boost frequency)
    pub top_khz: u64,
    /// time_in_state: (kHz, time in 10 ms units since boot)
    pub time: Vec<(u64, u64)>,
}

/// One sample of every policy and of each CPU's ticks.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Clocks {
    /// cpuN of /proc/stat: (busy, all) ticks, indexed by N (CPUs missing from the file: (0, 0))
    pub cpu: Vec<(u64, u64)>,
    pub policies: Vec<PolicyClocks>,
    /// When (CLOCK_MONOTONIC; the caller may set it in tests).
    pub ms: u64,
}

fn read_text(root: &str, rel: &str, max: usize) -> Option<String> {
    let buf = xh_core::fs::read_bytes(format!("{root}{rel}"), max).ok()?;
    (!buf.is_empty()).then(|| String::from_utf8_lossy(&buf).into_owned())
}

/// "0-1", "2 3 4", "5-6,7": the CPUs of a related_cpus or a cpu list file.
pub fn cpu_list(text: &str) -> Vec<u32> {
    let mut out = Vec::new();
    for part in text.split(|c: char| c == ',' || c.is_whitespace()).filter(|p| !p.is_empty()) {
        let mut ends = part.splitn(2, '-').map(|n| n.parse::<u32>().ok());
        match (ends.next().flatten(), ends.next()) {
            (Some(a), None) => out.push(a),
            (Some(a), Some(Some(b))) if b >= a && b - a < 64 => out.extend(a..=b),
            _ => {}
        }
    }
    out
}

/// The cpuN lines of a /proc/stat text: (busy, all) ticks by N. Busy is all but idle and iowait.
pub fn cpu_ticks(stat: &str) -> Vec<(u64, u64)> {
    let mut out: Vec<(u64, u64)> = Vec::new();
    for line in stat.lines() {
        let Some(rest) = line.strip_prefix("cpu") else { continue };
        let Some((n, fields)) = rest.split_once(' ') else { continue };
        let Ok(n) = n.parse::<usize>() else { continue };
        let v: Vec<u64> = fields.split_whitespace().take(8).map_while(|t| t.parse().ok()).collect();
        if v.len() < 4 || n >= 64 {
            continue;
        }
        let all: u64 = v.iter().sum();
        let idle = v[3] + v.get(4).copied().unwrap_or(0);
        if out.len() <= n {
            out.resize(n + 1, (0, 0));
        }
        out[n] = (all - idle, all);
    }
    out
}

impl Clocks {
    pub fn read(root: &str) -> Clocks {
        let mut out = Clocks { ms: monotonic_ms(), ..Clocks::default() };
        if let Some(text) = read_text(root, "/proc/stat", 16383) {
            out.cpu = cpu_ticks(&text);
        }
        for n in 0..MAX_POLICIES {
            let dir = format!("/sys/devices/system/cpu/cpufreq/policy{n}");
            let Some(text) = read_text(root, &format!("{dir}/stats/time_in_state"), 4095) else { continue };
            let time: Vec<(u64, u64)> = text
                .lines()
                .filter_map(|l| {
                    let mut w = l.split_whitespace();
                    Some((w.next()?.parse().ok()?, w.next()?.parse().ok()?))
                })
                .collect();
            if time.is_empty() {
                continue;
            }
            let listed = time.iter().map(|&(k, _)| k).max().unwrap_or(0);
            let max = read_text(root, &format!("{dir}/cpuinfo_max_freq"), 63)
                .and_then(|t| t.trim().parse::<u64>().ok())
                .unwrap_or(0);
            let cpus = read_text(root, &format!("{dir}/related_cpus"), 255).map(|t| cpu_list(&t)).unwrap_or_default();
            out.policies.push(PolicyClocks { policy: n, cpus, top_khz: max.max(listed), time });
        }
        out
    }

    /// The line between two samples, after "thermal: ", or None without a policy in both:
    ///   "30 s clocks: policy0 (cpu0-1) busy 92%, mean 1766 of 2266 MHz, at the top 41%, under half 20%; ..."
    /// busy: the policy's CPUs' non-idle ticks over all their ticks; mean: time_in_state's mean frequency over the
    /// interval (idle time included, at whatever clock the policy idled); at the top: at 95% of the top or more; under
    /// half: at half the top or less.
    pub fn line(was: &Clocks, now: &Clocks) -> Option<String> {
        let ms = now.ms.saturating_sub(was.ms);
        let mut parts = Vec::new();
        for p in &now.policies {
            let Some(q) = was.policies.iter().find(|q| q.policy == p.policy) else { continue };
            let (mut t, mut khz_t, mut top, mut low) = (0u64, 0u128, 0u64, 0u64);
            for &(khz, at) in &p.time {
                let before = q.time.iter().find(|&&(k, _)| k == khz).map_or(0, |&(_, b)| b);
                let d = at.saturating_sub(before);
                t += d;
                khz_t += u128::from(khz) * u128::from(d);
                if khz * 100 >= p.top_khz * 95 {
                    top += d;
                }
                if khz * 2 <= p.top_khz {
                    low += d;
                }
            }
            let (mut busy, mut all) = (0u64, 0u64);
            for &c in &p.cpus {
                let (b1, a1) = now.cpu.get(c as usize).copied().unwrap_or((0, 0));
                let (b0, a0) = was.cpu.get(c as usize).copied().unwrap_or((0, 0));
                busy += b1.saturating_sub(b0);
                all += a1.saturating_sub(a0);
            }
            let pct = |a: u64, of: u64| (a * 100 + of / 2).checked_div(of).unwrap_or(0);
            let cpus = match (p.cpus.first(), p.cpus.last()) {
                (Some(a), Some(b)) if a != b => format!("cpu{a}-{b}"),
                (Some(a), _) => format!("cpu{a}"),
                _ => "cpus ?".into(),
            };
            let busy = if all != 0 { format!("{}%", pct(busy, all)) } else { "-".into() };
            if t == 0 {
                parts.push(format!("policy{} ({cpus}) busy {busy}, no time counted", p.policy));
                continue;
            }
            parts.push(format!(
                "policy{} ({cpus}) busy {busy}, mean {} of {} MHz, at the top {}%, under half {}%",
                p.policy,
                (khz_t / u128::from(t) / 1000) as u64,
                p.top_khz / 1000,
                pct(top, t),
                pct(low, t)
            ));
        }
        (!parts.is_empty()).then(|| format!("{} s clocks: {}", (ms + 500) / 1000, parts.join("; ")))
    }
}
