//! `oneios-tool phone procs [--seconds N] [--sort cpu|mem] [--top N] [--kernel] [--raw] [--from FILE]`: every process
//! on the phone with its CPU share and its RAM, read only (ios-first-session, 2026-11-23; docs/plan-ios-first-session.md
//! stage 1). With or without a session. It answers "what of One UI still runs beside iOS, and what does it cost":
//! - the host's CPU over the span (the same figures as xnuhost's `thermal: 30 s: host CPU` line) and its memory;
//! - one line per group: the session (xnuhost, the iOS task processes, its helpers, the display helper), the fork's
//!   watchers (the way back), adb (this command's own shell is in it), the kernel's threads, Android and the vendor;
//! - one line per Android or vendor process: CPU, RssAnon, VmSwap, VmRSS, threads, the init service that owns it
//!   (`init.svc_debug_pid.NAME`, or `NAME+` for a child of that service), its command line, and what the repository
//!   knows about it (`role`: which bridge needs it, the way back, the keys, or zygote's restart list).
//!
//! Named files only (the phone's rule): the pids come from `ps -A` (the program `phone cpu` already runs), and for
//! each pid the two named files /proc/PID/stat and /proc/PID/status are read by one `cat` and one `grep`; then
//! /proc/stat, /proc/meminfo, `getprop`, and the stage's two pid files. No glob, no directory walk, nothing written.
//! No smaps or smaps_rollup: that read walks a process's page tables under its mmap lock, which would stall the iOS
//! tasks' maps and unmaps in a session. The cost on the phone is three `ps`, one `cat`, one `grep` and one `getprop`.

use crate::adb::{Adb, STAGE};
use anyhow::{bail, Context, Result};
use std::collections::{BTreeMap, HashMap, HashSet};

#[derive(Clone, Copy, Debug, PartialEq, clap::ValueEnum)]
pub enum Sort {
    /// The CPU share, then RssAnon + VmSwap
    Cpu,
    /// RssAnon + VmSwap, then the CPU share
    Mem,
}

pub struct Options {
    pub seconds: u64,
    pub sort: Sort,
    pub top: usize,
    pub kernel: bool,
    pub raw: bool,
    pub from: Option<String>,
}

/// The read. Sections, each begun by a line `@NAME`: `@session PID`, `@display PIDS`, `@snap 1` and `@snap 2`
/// (/proc/stat's cpu lines, then every /proc/PID/stat), `@status` (`grep -H` of six keys of every /proc/PID/status),
/// `@args` (`ps`: pid and command line), `@svc` (init's services: state and pid), `@meminfo`, `@end`.
/// `seconds` 0: one snapshot, so no CPU figures.
pub fn procs_script(seconds: u64) -> String {
    let mut s = String::new();
    s += &format!("echo \"@session $(cat {STAGE}/session/xnuhost.pid 2>/dev/null)\"\n");
    s += &format!("echo \"@display $(cat {STAGE}/control/display.pid 2>/dev/null | tr '\\n' ' ')\"\n");
    s += "snap() {\n";
    s += "  echo \"@snap $1\"\n";
    s += "  grep '^cpu' /proc/stat\n";
    s += "  f=; for p in $(ps -A -o PID=); do f=\"$f /proc/$p/stat\"; done\n";
    s += "  cat $f 2>/dev/null\n";
    s += "}\n";
    s += "snap 1\n";
    if seconds > 0 {
        s += &format!("sleep {seconds}\n");
        s += "snap 2\n";
    }
    s += "echo @status\n";
    s += "f=; for p in $(ps -A -o PID=); do f=\"$f /proc/$p/status\"; done\n";
    s += "grep -H -E '^(Uid|VmRSS|RssAnon|RssFile|RssShmem|VmSwap):' $f 2>/dev/null\n";
    s += "echo @args\n";
    s += "ps -A -w -o PID=,ARGS= 2>/dev/null\n";
    s += "echo @svc\n";
    s += "getprop | grep -E '^\\[init\\.svc(_debug_pid)?\\.'\n";
    s += "echo @meminfo\n";
    s += "cat /proc/meminfo\n";
    s += "echo @end\n";
    s
}

/// The first eight fields of /proc/stat's `cpu` line, in ticks.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct CpuTimes {
    pub user: u64,
    pub nice: u64,
    pub system: u64,
    pub idle: u64,
    pub iowait: u64,
    pub irq: u64,
    pub softirq: u64,
    pub steal: u64,
}

impl CpuTimes {
    fn total(&self) -> u64 {
        self.user + self.nice + self.system + self.idle + self.iowait + self.irq + self.softirq + self.steal
    }
}

#[derive(Clone, Debug, Default)]
pub struct Proc {
    pub pid: u32,
    pub ppid: u32,
    pub comm: String,
    pub kernel: bool,
    pub threads: u64,
    /// utime + stime + cutime + cstime at each snapshot: the process and the children it waited for
    pub ticks: [Option<u64>; 2],
    pub uid: Option<u32>,
    pub rss_kb: Option<u64>,
    pub anon_kb: Option<u64>,
    pub file_kb: Option<u64>,
    pub shmem_kb: Option<u64>,
    pub swap_kb: Option<u64>,
    pub args: String,
}

#[derive(Debug, Default)]
pub struct Capture {
    pub session: Option<u32>,
    pub display: Vec<u32>,
    pub cpu: [Option<CpuTimes>; 2],
    pub ncpu: usize,
    pub snaps: usize,
    pub procs: BTreeMap<u32, Proc>,
    /// init's services: the pid init gives for each, and each one's state
    pub svc_pid: HashMap<u32, String>,
    pub svc_state: BTreeMap<String, String>,
    pub meminfo: HashMap<String, u64>,
    pub complete: bool,
}

/// One line of /proc/PID/stat: the pid, the name (it may hold spaces and parentheses) and the fields after it.
pub fn parse_stat(line: &str) -> Option<(u32, String, Vec<&str>)> {
    let open = line.find('(')?;
    let close = line.rfind(')')?;
    if close < open {
        return None;
    }
    let pid = line[..open].trim().parse().ok()?;
    let rest: Vec<&str> = line[close + 1..].split_whitespace().collect();
    // state ppid pgrp session tty tpgid flags minflt cminflt majflt cmajflt utime stime ... (21 are read)
    if rest.len() < 21 {
        return None;
    }
    Some((pid, line[open + 1..close].to_string(), rest))
}

const PF_KTHREAD: u64 = 0x0020_0000;

fn parse_cpu_times(line: &str) -> Option<CpuTimes> {
    let n: Vec<u64> = line.split_whitespace().skip(1).take(8).filter_map(|w| w.parse().ok()).collect();
    if n.len() < 4 {
        return None;
    }
    let g = |i: usize| n.get(i).copied().unwrap_or(0);
    Some(CpuTimes { user: g(0), nice: g(1), system: g(2), idle: g(3), iowait: g(4), irq: g(5), softirq: g(6), steal: g(7) })
}

/// What the phone's script printed, as a table. Lines that are not understood are skipped.
pub fn parse(out: &str) -> Capture {
    let mut c = Capture::default();
    let mut section = "";
    let mut snap = 0usize;
    for line in out.lines() {
        if let Some(name) = line.strip_prefix('@') {
            let (name, rest) = name.split_once(' ').unwrap_or((name, ""));
            match name {
                "session" => c.session = rest.trim().parse().ok(),
                "display" => c.display = rest.split_whitespace().filter_map(|w| w.parse().ok()).collect(),
                "snap" => {
                    snap = rest.trim().parse::<usize>().unwrap_or(1).clamp(1, 2) - 1;
                    c.snaps = c.snaps.max(snap + 1);
                    if snap == 1 {
                        c.ncpu = 0;
                    }
                }
                "end" => c.complete = true,
                _ => {}
            }
            section = match name {
                "snap" => "snap",
                "status" => "status",
                "args" => "args",
                "svc" => "svc",
                "meminfo" => "meminfo",
                _ => "",
            };
            continue;
        }
        match section {
            "snap" => {
                if line.starts_with("cpu") {
                    let word = line.split_whitespace().next().unwrap_or("");
                    if word == "cpu" {
                        c.cpu[snap] = parse_cpu_times(line);
                    } else if word[3..].bytes().all(|b| b.is_ascii_digit()) {
                        c.ncpu += 1;
                    }
                } else if let Some((pid, comm, rest)) = parse_stat(line) {
                    let p = c.procs.entry(pid).or_default();
                    p.pid = pid;
                    p.comm = comm;
                    p.ppid = rest[1].parse().unwrap_or(0);
                    let flags = rest[6].parse::<u64>().unwrap_or(0);
                    // a kernel thread: the flag, or (a kernel that hides it) no address space under kthreadd
                    p.kernel = flags & PF_KTHREAD != 0 || (rest[20] == "0" && (pid == 2 || p.ppid == 2));
                    // its own time and that of the children it waited for (cutime, cstime): a shell loop's short
                    // commands end between the readings, and their time is counted nowhere else
                    p.ticks[snap] = Some([11, 12, 13, 14].iter().map(|&i| rest[i].parse::<u64>().unwrap_or(0)).sum());
                    p.threads = rest[17].parse().unwrap_or(0);
                }
            }
            "status" => {
                // PATH/PID/status:Key:   VALUE kB   (the path is the stand-in's own on a stand-in phone)
                let Some((path, rest)) = line.split_once("/status:") else { continue };
                let Some(pid) = path.rsplit('/').next().and_then(|w| w.parse::<u32>().ok()) else { continue };
                let Some((key, value)) = rest.split_once(':') else { continue };
                let Some(n) = value.split_whitespace().next().and_then(|w| w.parse::<u64>().ok()) else { continue };
                let Some(p) = c.procs.get_mut(&pid) else { continue };
                match key {
                    "Uid" => p.uid = Some(n as u32),
                    "VmRSS" => p.rss_kb = Some(n),
                    "RssAnon" => p.anon_kb = Some(n),
                    "RssFile" => p.file_kb = Some(n),
                    "RssShmem" => p.shmem_kb = Some(n),
                    "VmSwap" => p.swap_kb = Some(n),
                    _ => {}
                }
            }
            "args" => {
                let t = line.trim_start();
                let (pid, args) = t.split_once(char::is_whitespace).unwrap_or((t, ""));
                if let Some(p) = pid.parse::<u32>().ok().and_then(|pid| c.procs.get_mut(&pid)) {
                    p.args = args.trim().to_string();
                }
            }
            "svc" => {
                // [init.svc.NAME]: [running]   [init.svc_debug_pid.NAME]: [1234]
                let Some((key, value)) = line.split_once("]: [") else { continue };
                let (key, value) = (key.trim_start_matches('['), value.trim_end_matches(']'));
                if let Some(name) = key.strip_prefix("init.svc_debug_pid.") {
                    if let Ok(pid) = value.parse::<u32>() {
                        c.svc_pid.insert(pid, name.to_string());
                    }
                } else if let Some(name) = key.strip_prefix("init.svc.") {
                    c.svc_state.insert(name.to_string(), value.to_string());
                }
            }
            "meminfo" => {
                if let Some((key, value)) = line.split_once(':') {
                    if let Some(n) = value.split_whitespace().next().and_then(|w| w.parse::<u64>().ok()) {
                        c.meminfo.insert(key.trim().to_string(), n);
                    }
                }
            }
            _ => {}
        }
    }
    c
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Group {
    Session,
    Fork,
    Adb,
    Kernel,
    Android,
}

impl Group {
    fn title(self) -> &'static str {
        match self {
            Group::Session => "the session (xnuhost, iOS's tasks, the helpers)",
            Group::Fork => "the fork's watchers (the way back)",
            Group::Adb => "adb (this command's shell is in it)",
            Group::Kernel => "the kernel's threads",
            Group::Android => "Android and the vendor",
        }
    }
}

/// `root` and everything below it by parent pid.
fn tree(children: &HashMap<u32, Vec<u32>>, root: u32, into: &mut HashSet<u32>) {
    let mut todo = vec![root];
    while let Some(pid) = todo.pop() {
        if into.insert(pid) {
            if let Some(list) = children.get(&pid) {
                todo.extend(list.iter().copied());
            }
        }
    }
}

/// Which group each process is in. The session: the pid of session/xnuhost.pid when that process is an xnuhost, and
/// the helpers of control/display.pid, each with what it started. The fork's watchers: its three scripts by name.
pub fn groups(c: &Capture) -> HashMap<u32, Group> {
    let mut children: HashMap<u32, Vec<u32>> = HashMap::new();
    for p in c.procs.values() {
        children.entry(p.ppid).or_default().push(p.pid);
    }
    let named = |p: &Proc, word: &str| p.args.contains(word) || p.comm.contains(word);
    let mut session = HashSet::new();
    if let Some(p) = c.session.and_then(|pid| c.procs.get(&pid)) {
        if named(p, "xnuhost") {
            tree(&children, p.pid, &mut session);
        }
    }
    if !session.is_empty() {
        for pid in &c.display {
            if c.procs.get(pid).is_some_and(|p| named(p, "oneios-display") || named(p, "app_process")) {
                tree(&children, *pid, &mut session);
            }
        }
    }
    let mut fork = HashSet::new();
    let mut adb = HashSet::new();
    for p in c.procs.values() {
        if ["oneios_switch.sh", "oneios_return.sh", "oneios_root.sh"].iter().any(|w| p.args.contains(w)) {
            tree(&children, p.pid, &mut fork);
        }
        if p.comm == "adbd" {
            tree(&children, p.pid, &mut adb);
        }
    }
    c.procs
        .values()
        .map(|p| {
            let g = if p.kernel {
                Group::Kernel
            } else if session.contains(&p.pid) {
                Group::Session
            } else if fork.contains(&p.pid) {
                Group::Fork
            } else if adb.contains(&p.pid) {
                Group::Adb
            } else {
                Group::Android
            };
            (p.pid, g)
        })
        .collect()
}

/// The init service a process belongs to: its own (`NAME`), or the nearest parent's (`NAME+`), init itself left out.
pub fn service(c: &Capture, pid: u32) -> String {
    if let Some(name) = c.svc_pid.get(&pid) {
        return name.clone();
    }
    let mut at = pid;
    for _ in 0..6 {
        let Some(p) = c.procs.get(&at) else { break };
        if p.ppid <= 1 {
            break;
        }
        at = p.ppid;
        if let Some(name) = c.svc_pid.get(&at) {
            return format!("{name}+");
        }
    }
    "-".into()
}

/// What the repository's docs say about a process, by its service and its name. A starting list (docs/
/// plan-ios-first-session.md, "The inventory"), not a proof: `-` means "not classified", which is not "safe to stop".
pub fn role(service: &str, name: &str) -> &'static str {
    let s = format!("{} {}", service.to_ascii_lowercase(), name.to_ascii_lowercase());
    let has = |words: &[&str]| words.iter().any(|w| s.contains(w));
    if has(&["qseecom", "tzdaemon", "vaultkeeper", "keymint", "keymaster", "gatekeeper", "weaver", "tlc_", ".tlc", "secure_element", "strongbox", "authsecret", "hermes", "widevine", "hardware.drm", "ssgtzd", "spdaemon"]) {
        return "keep: keys and the TEE (never touched)";
    }
    if has(&["surfaceflinger"]) {
        return "bridge: display (the helper's layer; the return restarts it)";
    }
    if has(&["display.composer", "hwcomposer", "graphics.composer"]) {
        return "bridge: display (holds the panel; the return restarts it)";
    }
    if has(&["display.allocator", "graphics.allocator"]) {
        return "bridge: camera, encoder (gralloc buffers)";
    }
    if has(&["audioserver"]) {
        return "bridge: audio (AAudio); restart list";
    }
    if has(&["audio", "adsprpcd", "agm", "soundtrigger"]) {
        return "bridge: audio (the HAL, the DSP's routing)";
    }
    if has(&["camera.provider", "camera-provider", "cameraprovider"]) {
        return "bridge: camera (the provider HAL)";
    }
    if has(&["cameraserver"]) {
        return "restart list; no bridge uses it in a session";
    }
    if has(&["media.c2", "media.swcodec", "mediaswcodec", "media.codec", "media.omx"]) {
        return "bridge: encoder (MediaCodec)";
    }
    if has(&["mediaserver"]) || service == "media" || service == "media+" {
        return "restart list; encoder: not known if needed";
    }
    if has(&["mediaextractor", "mediametrics", "media.metrics", "media.extractor", "media.tuner", "perfmond"]) {
        return "restart list; no bridge uses it";
    }
    if has(&["wpa_supplicant"]) {
        return "bridge: Wi-Fi (joins)";
    }
    if has(&["wificond"]) {
        return "restart list; no bridge uses it (scans are nl80211)";
    }
    if has(&["hardware.wifi", "wifi-service", "wifi_hal", "cnss"]) {
        return "bridge: Wi-Fi (the chip's HAL and firmware)";
    }
    if has(&["bluetooth"]) {
        return "bridge: Bluetooth (the HAL)";
    }
    if has(&["netd"]) {
        return "restart list; no bridge asks it";
    }
    if has(&["thermal", "health", "charger"]) {
        return "keep: thermal and charge limits";
    }
    if has(&["servicemanager", "ueventd", "logd", "lmkd", "vold", "keystore", "apexd", "adbd", "ksud", "tombstoned", "watchdogd", "property"]) || name == "init" || name.starts_with("init ") {
        return "keep: the way back";
    }
    if has(&["ril", "radio", "ims", "netmgrd", "qmi", "modem", "atfwd", "port-bridge", "cnd"]) {
        return "the modem's (not classified for a stop)";
    }
    if has(&["vibrator", "sensors", "sscrpcd", "light", "input"]) {
        return "a HAL our bridges pass by (evdev, sysfs)";
    }
    "-"
}

fn mib(kb: u64) -> String {
    format!("{}.{}", kb / 1024, kb % 1024 * 10 / 1024)
}

struct Row<'a> {
    p: &'a Proc,
    cpu: Option<f64>,
}

impl Row<'_> {
    fn private_kb(&self) -> u64 {
        self.p.anon_kb.unwrap_or(0) + self.p.swap_kb.unwrap_or(0)
    }
}

fn pct(v: Option<f64>) -> String {
    v.map(|x| format!("{x:.1}")).unwrap_or_else(|| "-".into())
}

/// The report.
pub fn render(c: &Capture, sort: Sort, top: usize, kernel: bool) -> String {
    let mut text = String::new();
    if c.procs.is_empty() {
        return "no process was read (the phone gave no /proc/PID/stat line)\n".into();
    }
    // the CPU span: ticks of all CPUs between the two snapshots; one CPU's share of them is 100 %
    let span = match (c.cpu[0], c.cpu[1]) {
        (Some(a), Some(b)) if b.total() > a.total() && c.ncpu > 0 => Some((a, b, (b.total() - a.total()) as f64 / c.ncpu as f64)),
        _ => None,
    };
    let share = |ticks: u64| span.map(|(_, _, one)| ticks as f64 * 100.0 / one);
    let group = groups(c);
    let rows: Vec<Row> = c
        .procs
        .values()
        .map(|p| {
            let cpu = match (p.ticks[0], p.ticks[1]) {
                (Some(a), Some(b)) => share(b.saturating_sub(a)),
                (None, Some(b)) if c.snaps == 2 => share(b), // started inside the span
                _ => None,
            };
            Row { p, cpu }
        })
        .collect();
    let session_pid = c.session.filter(|pid| group.get(pid) == Some(&Group::Session));
    match session_pid {
        Some(pid) => text += &format!("session: xnuhost pid {pid}\n"),
        None => text += "session: none\n",
    }
    match span {
        Some((a, b, one)) => {
            let d = |x: u64, y: u64| y.saturating_sub(x) as f64 * 100.0 / one;
            let busy = (b.total() - a.total()).saturating_sub(b.idle.saturating_sub(a.idle) + b.iowait.saturating_sub(a.iowait));
            text += &format!(
                "host: CPU {:.0}% of {} (user {:.0}%, sys {:.0}%, irq {:.0}%), {} CPUs online\n",
                busy as f64 * 100.0 / one,
                c.ncpu * 100,
                d(a.user + a.nice, b.user + b.nice),
                d(a.system, b.system),
                d(a.irq + a.softirq, b.irq + b.softirq),
                c.ncpu
            );
        }
        None => text += "host: CPU not measured (one snapshot, or the clock did not move between the two)\n",
    }
    let m = |k: &str| c.meminfo.get(k).copied();
    if let (Some(total), Some(avail)) = (m("MemTotal"), m("MemAvailable")) {
        text += &format!("memory: MemTotal {} MiB, MemAvailable {} MiB", total / 1024, avail / 1024);
        if let (Some(st), Some(sf)) = (m("SwapTotal"), m("SwapFree")) {
            text += &format!(", swap {} of {} MiB used", st.saturating_sub(sf) / 1024, st / 1024);
        }
        for (key, name) in [("Shmem", "Shmem"), ("Mlocked", "Mlocked"), ("PageTables", "page tables"), ("Slab", "slab"), ("KernelStack", "kernel stacks")] {
            if let Some(v) = m(key) {
                text += &format!(", {name} {} MiB", v / 1024);
            }
        }
        text += "\n";
    }
    // the groups
    text += "\n";
    text += &format!("{:<50} {:>5} {:>7} {:>7} {:>9} {:>9} {:>9}\n", "group", "procs", "threads", "cpu%", "anon MiB", "swap MiB", "rss MiB");
    for g in [Group::Session, Group::Fork, Group::Adb, Group::Kernel, Group::Android] {
        let of: Vec<&Row> = rows.iter().filter(|r| group.get(&r.p.pid) == Some(&g)).collect();
        if of.is_empty() {
            continue;
        }
        let sum = |f: &dyn Fn(&Row) -> u64| of.iter().map(|r| f(r)).sum::<u64>();
        let cpu = span.map(|_| of.iter().filter_map(|r| r.cpu).sum::<f64>());
        text += &format!(
            "{:<50} {:>5} {:>7} {:>7} {:>9} {:>9} {:>9}\n",
            g.title(),
            of.len(),
            sum(&|r| r.p.threads),
            pct(cpu),
            mib(sum(&|r| r.p.anon_kb.unwrap_or(0))),
            mib(sum(&|r| r.p.swap_kb.unwrap_or(0))),
            mib(sum(&|r| r.p.rss_kb.unwrap_or(0))),
        );
    }
    text += "  cpu%: of one CPU. anon: RssAnon, the process's own pages in RAM. swap: VmSwap, its pages in zram (compressed: a\n";
    text += "  stop frees about a third of that as RAM). rss: VmRSS, with file and shared pages, which several processes count.\n";
    // the services a session is about
    let watch = ["zygote", "zygote_secondary", "surfaceflinger", "vendor.qti.hardware.display.composer", "vendor.hwcomposer-3", "audioserver", "cameraserver", "media", "netd", "wificond", "wpa_supplicant", "bootanim", "adbd"];
    let states: Vec<String> = watch.iter().filter_map(|n| c.svc_state.get(*n).map(|s| format!("{n} {s}"))).collect();
    if !states.is_empty() {
        let running = c.svc_state.values().filter(|s| *s == "running").count();
        text += &format!("\ninit services: {} running of {}; {}\n", running, c.svc_state.len(), states.join(", "));
    }
    // Android and the vendor, one line each
    let mut android: Vec<&Row> = rows.iter().filter(|r| group.get(&r.p.pid) == Some(&Group::Android)).collect();
    let by_cpu = |a: &&Row, b: &&Row| b.cpu.unwrap_or(0.0).partial_cmp(&a.cpu.unwrap_or(0.0)).unwrap_or(std::cmp::Ordering::Equal);
    let by_mem = |a: &&Row, b: &&Row| b.private_kb().cmp(&a.private_kb());
    match sort {
        Sort::Cpu => android.sort_by(|a, b| by_cpu(a, b).then(by_mem(a, b)).then(a.p.pid.cmp(&b.p.pid))),
        Sort::Mem => android.sort_by(|a, b| by_mem(a, b).then(by_cpu(a, b)).then(a.p.pid.cmp(&b.p.pid))),
    }
    let shown = if top == 0 { android.len() } else { top.min(android.len()) };
    text += &format!(
        "\nAndroid and the vendor: {} processes{}, by {}:\n",
        android.len(),
        if shown < android.len() { format!(" (the first {shown})") } else { String::new() },
        if sort == Sort::Cpu { "CPU, then anon + swap" } else { "anon + swap, then CPU" }
    );
    text += &format!("{:>7} {:>6} {:>8} {:>8} {:>8} {:>4}  {:<40} {:<36} {}\n", "pid", "cpu%", "anon MiB", "swap MiB", "rss MiB", "thr", "service", "command", "role");
    for r in &android[..shown] {
        let svc = service(c, r.p.pid);
        let name = if r.p.args.is_empty() { format!("({})", r.p.comm) } else { r.p.args.clone() };
        let short: String = name.chars().take(36).collect();
        text += &format!(
            "{:>7} {:>6} {:>8} {:>8} {:>8} {:>4}  {:<40} {:<36} {}\n",
            r.p.pid,
            pct(r.cpu),
            mib(r.p.anon_kb.unwrap_or(0)),
            mib(r.p.swap_kb.unwrap_or(0)),
            mib(r.p.rss_kb.unwrap_or(0)),
            r.p.threads,
            svc,
            short,
            role(&svc, &name)
        );
    }
    // the session's own processes (xnuhost, the task factory, its helpers, the display helper), and iOS's task
    // processes, which the factory starts, as one line
    let factory: HashSet<u32> = c.procs.values().filter(|p| p.comm == "xnuhost-tasks").map(|p| p.pid).collect();
    let is_task = |p: &Proc| {
        let mut at = p.ppid;
        for _ in 0..8 {
            if factory.contains(&at) {
                return true;
            }
            match c.procs.get(&at) {
                Some(up) if up.ppid > 1 => at = up.ppid,
                _ => break,
            }
        }
        false
    };
    let (tasks, mut own): (Vec<&Row>, Vec<&Row>) = rows.iter().filter(|r| group.get(&r.p.pid) == Some(&Group::Session)).partition(|r| is_task(r.p));
    if !own.is_empty() {
        own.sort_by(|a, b| by_cpu(a, b).then(by_mem(a, b)).then(a.p.pid.cmp(&b.p.pid)));
        text += "\nthe session's own processes:\n";
        text += &format!("{:>7} {:>6} {:>8} {:>8} {:>8} {:>4}  {}\n", "pid", "cpu%", "anon MiB", "swap MiB", "rss MiB", "thr", "command");
        for r in &own {
            let name = if r.p.args.is_empty() { format!("({})", r.p.comm) } else { r.p.args.clone() };
            text += &format!(
                "{:>7} {:>6} {:>8} {:>8} {:>8} {:>4}  {}\n",
                r.p.pid,
                pct(r.cpu),
                mib(r.p.anon_kb.unwrap_or(0)),
                mib(r.p.swap_kb.unwrap_or(0)),
                mib(r.p.rss_kb.unwrap_or(0)),
                r.p.threads,
                name.chars().take(80).collect::<String>()
            );
        }
        if !tasks.is_empty() {
            let sum = |f: &dyn Fn(&Row) -> u64| tasks.iter().map(|r| f(r)).sum::<u64>();
            text += &format!(
                "{:>7} {:>6} {:>8} {:>8} {:>8} {:>4}  iOS's tasks: {} process(es) (their anon is the guest's RAM, held by xnuhost: rss counts it again)\n",
                "",
                pct(span.map(|_| tasks.iter().filter_map(|r| r.cpu).sum::<f64>())),
                mib(sum(&|r| r.p.anon_kb.unwrap_or(0))),
                mib(sum(&|r| r.p.swap_kb.unwrap_or(0))),
                mib(sum(&|r| r.p.rss_kb.unwrap_or(0))),
                sum(&|r| r.p.threads),
                tasks.len()
            );
        }
    }
    // the kernel's threads: the busiest, or all
    let mut kthreads: Vec<&Row> = rows.iter().filter(|r| group.get(&r.p.pid) == Some(&Group::Kernel) && (kernel || r.cpu.unwrap_or(0.0) >= 0.1)).collect();
    kthreads.sort_by(|a, b| by_cpu(a, b).then(a.p.pid.cmp(&b.p.pid)));
    if !kthreads.is_empty() {
        let n = if kernel { kthreads.len() } else { kthreads.len().min(8) };
        text += &format!("\nthe kernel's threads{}:\n", if kernel { "" } else { " at 0.1% of a CPU or more (the first 8; --kernel: all)" });
        for r in &kthreads[..n] {
            text += &format!("{:>7} {:>6}  {}\n", r.p.pid, pct(r.cpu), r.p.comm);
        }
    }
    if !c.complete {
        text += "\nnote: the phone's output ended before its last line (@end): the list may be short\n";
    }
    text
}

/// `phone procs`.
pub fn procs(adb: Option<&Adb>, o: &Options) -> Result<()> {
    let out = match (&o.from, adb) {
        (Some(file), _) => std::fs::read_to_string(file).with_context(|| format!("cannot read {file}"))?,
        (None, Some(adb)) => {
            adb.need_root()?;
            let r = adb.root(&procs_script(o.seconds))?;
            if !r.ok() && r.out.is_empty() {
                bail!("the read failed on the phone (status {}): {}", r.status, r.err.trim());
            }
            println!("phone: {}", adb.serial);
            r.out
        }
        (None, None) => bail!("no phone"),
    };
    if o.raw {
        print!("{out}");
        return Ok(());
    }
    print!("{}", render(&parse(&out), o.sort, o.top, o.kernel));
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_script_reads_named_files_only() {
        let s = procs_script(5);
        assert!(!s.contains('*'), "no glob");
        assert!(!s.contains("ls ") && !s.contains("find ") && !s.contains("du "), "no directory walk");
        assert!(!s.contains("smaps"), "no page-table walk of a process");
        assert!(!s.contains(" > ") && !s.contains(">>") && !s.contains("rm ") && !s.contains("kill") && !s.contains("setprop"), "nothing written, nothing signalled");
        assert!(s.contains("/proc/$p/stat\"") && s.contains("/proc/$p/status\"") && s.contains("ps -A -o PID="));
        assert!(s.contains("sleep 5\nsnap 2\n"));
        let one = procs_script(0);
        assert!(!one.contains("sleep") && !one.contains("snap 2"));
    }

    #[test]
    fn a_stat_line_with_spaces_and_parentheses_in_the_name() {
        let line = "1234 (Binder:(1234)_2 x) S 1 1234 0 0 -1 4194560 10 0 0 0 150 50 0 0 20 0 7 0 100 123456 2000 18446744073709551615";
        let (pid, comm, rest) = parse_stat(line).unwrap();
        assert_eq!((pid, comm.as_str()), (1234, "Binder:(1234)_2 x"));
        assert_eq!((rest[1], rest[11], rest[12], rest[17]), ("1", "150", "50", "7"));
        assert!(parse_stat("no stat line").is_none());
        assert!(parse_stat("12 (short) S 1").is_none());
    }

    fn stat(pid: u32, comm: &str, ppid: u32, flags: u64, utime: u64, stime: u64, threads: u64) -> String {
        format!("{pid} ({comm}) S {ppid} {pid} 0 0 -1 {flags} 0 0 0 0 {utime} {stime} 0 0 20 0 {threads} 0 100 {} 2000 1\n", if flags & PF_KTHREAD != 0 { 0 } else { 123456 })
    }

    /// A session: 5 s on 8 CPUs at 100 ticks a second is 4000 ticks, 500 a CPU.
    fn capture() -> String {
        let mut s = String::from("@session 500\n@display 600\n@snap 1\ncpu  1000 0 1000 8000 0 0 0 0 0 0\n");
        for n in 0..8 {
            s += &format!("cpu{n} 1 1 1 1 1 1 1 1 0 0\n");
        }
        let first = [
            stat(1, "init", 0, 0, 10, 10, 1),
            stat(2, "kthreadd", 0, PF_KTHREAD, 0, 0, 1),
            stat(30, "kswapd0", 2, PF_KTHREAD, 0, 100, 1),
            stat(100, "surfaceflinger", 1, 0, 100, 100, 30),
            stat(110, "vendor.qti.hard", 1, 0, 50, 50, 12),
            stat(120, "audioserver", 1, 0, 5, 5, 20),
            stat(130, "cameraserver", 1, 0, 1, 1, 8),
            stat(140, "adbd", 1, 0, 1, 1, 4),
            stat(141, "sh", 140, 0, 0, 0, 1),
            stat(200, "sh", 1, 0, 0, 0, 1),
            stat(201, "getevent", 200, 0, 0, 0, 1),
            stat(500, "xnuhost", 1, 0, 1000, 2000, 40),
            stat(501, "xnuhost-tasks", 500, 0, 0, 0, 1),
            stat(502, "launchd", 501, 0, 100, 100, 3),
            stat(600, "app_process", 1, 0, 10, 10, 25),
        ];
        for l in &first {
            s += l;
        }
        s += "@snap 2\ncpu  2000 0 2500 9400 100 0 0 0 0 0\n";
        for n in 0..8 {
            s += &format!("cpu{n} 1 1 1 1 1 1 1 1 0 0\n");
        }
        let second = [
            stat(1, "init", 0, 0, 10, 10, 1),
            stat(2, "kthreadd", 0, PF_KTHREAD, 0, 0, 1),
            stat(30, "kswapd0", 2, PF_KTHREAD, 0, 125, 1),
            stat(100, "surfaceflinger", 1, 0, 130, 120, 30),
            stat(110, "vendor.qti.hard", 1, 0, 60, 65, 12),
            stat(120, "audioserver", 1, 0, 5, 5, 20),
            stat(130, "cameraserver", 1, 0, 3, 3, 8),
            stat(140, "adbd", 1, 0, 1, 1, 4),
            stat(141, "sh", 140, 0, 0, 0, 1),
            stat(200, "sh", 1, 0, 0, 0, 1),
            stat(201, "getevent", 200, 0, 0, 0, 1),
            stat(500, "xnuhost", 1, 0, 1400, 2600, 40),
            stat(501, "xnuhost-tasks", 500, 0, 0, 0, 1),
            stat(502, "launchd", 501, 0, 300, 400, 3),
            stat(600, "app_process", 1, 0, 30, 15, 25),
            stat(700, "newborn", 1, 0, 5, 0, 1),
        ];
        for l in &second {
            s += l;
        }
        s += "@status\n";
        for (pid, anon, swap, rss) in [(100, 51200, 10240, 153600), (110, 20480, 0, 61440), (120, 30720, 5120, 71680), (130, 40960, 2048, 92160), (500, 5767168, 0, 5800000), (600, 102400, 0, 204800), (700, 1024, 0, 2048)] {
            s += &format!("/proc/{pid}/status:Uid:\t1000\t1000\t1000\t1000\n/proc/{pid}/status:VmRSS:\t{rss} kB\n/proc/{pid}/status:RssAnon:\t{anon} kB\n/proc/{pid}/status:VmSwap:\t{swap} kB\n");
        }
        s += "@args\n    1 init second_stage\n  100 surfaceflinger\n  110 vendor.qti.hardware.display.composer-service\n  120 audioserver\n  130 cameraserver\n  140 adbd --root_seclabel=u:r:su:s0\n  141 sh -s\n  200 sh /data/local/tmp/oneios/oneios_switch.sh --keys\n  201 getevent -l /dev/input/event0\n  500 xnuhost --session-abi 1 --mode takeover\n  501 xnuhost-tasks\n  502 launchd\n  600 app_process /data/local/tmp/oneios/runtime/lib oneios.display.Main --session x\n  700 newborn\n";
        s += "@svc\n[init.svc.zygote]: [stopped]\n[init.svc.surfaceflinger]: [running]\n[init.svc.vendor.qti.hardware.display.composer]: [running]\n[init.svc.audioserver]: [running]\n[init.svc.cameraserver]: [running]\n[init.svc.adbd]: [running]\n[init.svc_debug_pid.surfaceflinger]: [100]\n[init.svc_debug_pid.vendor.qti.hardware.display.composer]: [110]\n[init.svc_debug_pid.audioserver]: [120]\n[init.svc_debug_pid.cameraserver]: [130]\n[init.svc_debug_pid.adbd]: [140]\n";
        s += "@meminfo\nMemTotal:       11350016 kB\nMemAvailable:    2043904 kB\nSwapTotal:      12581888 kB\nSwapFree:       11910144 kB\nPageTables:       369664 kB\n@end\n";
        s
    }

    #[test]
    fn a_session_is_parsed_and_grouped() {
        let c = parse(&capture());
        assert!(c.complete && c.snaps == 2 && c.ncpu == 8);
        assert_eq!(c.session, Some(500));
        assert_eq!(c.display, vec![600]);
        assert_eq!(c.cpu[1].unwrap().total() - c.cpu[0].unwrap().total(), 4000);
        let g = groups(&c);
        for (pid, want) in [(1, Group::Android), (2, Group::Kernel), (30, Group::Kernel), (100, Group::Android), (140, Group::Adb), (141, Group::Adb), (200, Group::Fork), (201, Group::Fork), (500, Group::Session), (501, Group::Session), (502, Group::Session), (600, Group::Session), (700, Group::Android)] {
            assert_eq!(g[&pid], want, "pid {pid}");
        }
        assert_eq!(c.procs[&100].anon_kb, Some(51200));
        assert_eq!(c.procs[&110].args, "vendor.qti.hardware.display.composer-service");
        assert_eq!(service(&c, 110), "vendor.qti.hardware.display.composer");
        assert_eq!(service(&c, 141), "adbd+");
        assert_eq!(service(&c, 700), "-");
    }

    #[test]
    fn the_report_of_a_session() {
        let text = render(&parse(&capture()), Sort::Cpu, 0, false);
        assert!(text.contains("session: xnuhost pid 500\n"), "{text}");
        // 4000 ticks on 8 CPUs: 500 a CPU. busy = 4000 - 1400 idle - 100 iowait = 2500 -> 500 %; user 1000 -> 200 %; sys 1500 -> 300 %
        assert!(text.contains("host: CPU 500% of 800 (user 200%, sys 300%, irq 0%), 8 CPUs online"), "{text}");
        assert!(text.contains("memory: MemTotal 11084 MiB, MemAvailable 1996 MiB, swap 656 of 12287 MiB used, page tables 361 MiB"), "{text}");
        let line = |start: &str| text.lines().find(|l| l.trim_start().starts_with(start)).unwrap_or_else(|| panic!("no line {start}: {text}")).split_whitespace().map(str::to_string).collect::<Vec<_>>();
        // the session: xnuhost 1000 ticks + launchd 500 + the display helper 25 = 1525 -> 305.0 %
        let s = line("the session");
        assert_eq!(s[s.len() - 6..], ["4", "69", "305.0", "5732.0", "0.0", "5864.0"].map(String::from), "{text}");
        // Android: surfaceflinger 50 ticks -> 10 %, the composer 25 -> 5 %, cameraserver 4 -> 0.8 %, the newborn 5 -> 1.0 %
        let sf = line("100 ");
        assert_eq!(sf[..7], ["100", "10.0", "50.0", "10.0", "150.0", "30", "surfaceflinger"].map(String::from), "{text}");
        assert!(text.lines().any(|l| l.trim_start().starts_with("110 ") && l.contains("5.0") && l.contains("vendor.qti.hardware.display.composer ") && l.contains("bridge: display (holds the panel")), "{text}");
        assert!(text.lines().any(|l| l.trim_start().starts_with("130 ") && l.contains("0.8") && l.contains("restart list; no bridge uses it in a session")), "{text}");
        assert!(text.lines().any(|l| l.trim_start().starts_with("700 ") && l.contains(" 1.0 ")), "a process born inside the span: {text}");
        // sorted by CPU: surfaceflinger, the composer, the newborn, cameraserver, then the idle ones by memory
        let order: Vec<&str> = text.lines().skip_while(|l| !l.starts_with("Android and the vendor:")).skip(2).map(|l| l.split_whitespace().next().unwrap_or("")).take(6).collect();
        assert_eq!(order, ["100", "110", "700", "130", "120", "1"], "{text}");
        assert!(text.contains("init services: 5 running of 6; zygote stopped, surfaceflinger running, vendor.qti.hardware.display.composer running, audioserver running, cameraserver running, adbd running"), "{text}");
        // the session's own processes, the busiest first: xnuhost 200 %, the display helper 5 %, the factory 0 %;
        // iOS's task (launchd, 500 ticks) is one line
        let own: Vec<&str> = text.lines().skip_while(|l| !l.starts_with("the session's own processes:")).skip(2).map(|l| l.split_whitespace().next().unwrap_or("")).take(3).collect();
        assert_eq!(own, ["500", "600", "501"], "{text}");
        assert!(text.lines().any(|l| l.trim_start().starts_with("600 ") && l.contains(" 5.0 ") && l.contains("100.0") && l.contains("app_process /data/local/tmp/oneios/runtime/lib oneios.display.Main")), "{text}");
        assert!(text.lines().any(|l| l.trim_start().starts_with("100.0 ") && l.contains("iOS's tasks: 1 process(es)")), "{text}");
        // kswapd: 25 ticks -> 5 %
        assert!(text.lines().any(|l| l.trim_start().starts_with("30 ") && l.contains("5.0") && l.contains("kswapd0")), "{text}");
        assert!(!text.contains("note: the phone's output ended"));
        // by memory: cameraserver (42 MiB) before audioserver (35), after surfaceflinger (60)
        let text = render(&parse(&capture()), Sort::Mem, 3, false);
        let order: Vec<&str> = text.lines().skip_while(|l| !l.starts_with("Android and the vendor:")).skip(2).map(|l| l.split_whitespace().next().unwrap_or("")).take(3).collect();
        assert_eq!(order, ["100", "130", "120"], "{text}");
        assert!(text.contains("Android and the vendor: 6 processes (the first 3), by anon + swap, then CPU:"), "{text}");
    }

    #[test]
    fn no_session_one_snapshot_and_a_cut_output() {
        // a pid file left by an ended session names a process that is not an xnuhost: no session
        let mut s = capture().replace("500 xnuhost --session-abi 1 --mode takeover", "500 sleep 100").replace("(xnuhost)", "(sleep)");
        let c = parse(&s);
        assert_eq!(groups(&c)[&500], Group::Android);
        assert_eq!(groups(&c)[&600], Group::Android, "the display pid file counts only in a session");
        assert!(render(&c, Sort::Cpu, 0, false).contains("session: none\n"));
        // one snapshot: no CPU figures
        let cut = s.find("@snap 2").unwrap();
        let end = s.find("@status").unwrap();
        s.replace_range(cut..end, "");
        let text = render(&parse(&s), Sort::Cpu, 0, false);
        assert!(text.contains("host: CPU not measured"), "{text}");
        assert!(text.lines().any(|l| l.trim_start().starts_with("100 ") && l.split_whitespace().nth(1) == Some("-")), "{text}");
        // an output that ended early says so
        let text = render(&parse(&capture().replace("@end\n", "")), Sort::Cpu, 0, false);
        assert!(text.contains("note: the phone's output ended before its last line"), "{text}");
        assert_eq!(render(&parse(""), Sort::Cpu, 0, false), "no process was read (the phone gave no /proc/PID/stat line)\n");
    }

    /// The fork's stop loop starts `getprop` three times every 0.2 s: the commands end between the readings, so
    /// their time shows only in the shell's cutime and cstime.
    #[test]
    fn a_shell_loop_is_charged_its_ended_commands() {
        let line = |cu: u64, cs: u64| format!("200 (sh) S 1 200 0 0 -1 0 0 0 0 0 1 1 {cu} {cs} 20 0 1 0 100 123456 2000 1\n");
        let s = format!(
            "@session \n@display \n@snap 1\ncpu  0 0 0 1000 0 0 0 0 0 0\ncpu0 0 0 0 1000 0 0 0 0\n{}@snap 2\ncpu  100 0 100 1300 0 0 0 0 0 0\ncpu0 100 0 100 1300 0 0 0 0\n{}@args\n200 sh /data/local/tmp/oneios/oneios_switch.sh --stop-loop\n@end\n",
            line(100, 200),
            line(110, 225)
        );
        let text = render(&parse(&s), Sort::Cpu, 0, false);
        // 500 ticks on one CPU; the loop's ended commands took 35 of them: 7.0 %
        assert!(text.lines().any(|l| l.starts_with("the fork's watchers") && l.contains(" 7.0 ")), "{text}");
    }

    #[test]
    fn roles() {
        assert_eq!(role("vendor.qti.hardware.display.composer", "x"), "bridge: display (holds the panel; the return restarts it)");
        assert_eq!(role("surfaceflinger", "surfaceflinger"), "bridge: display (the helper's layer; the return restarts it)");
        assert_eq!(role("audioserver", "audioserver"), "bridge: audio (AAudio); restart list");
        assert_eq!(role("vendor.audio-hal", "android.hardware.audio.service"), "bridge: audio (the HAL, the DSP's routing)");
        assert_eq!(role("wpa_supplicant", "wpa_supplicant"), "bridge: Wi-Fi (joins)");
        assert_eq!(role("wificond", "wificond"), "restart list; no bridge uses it (scans are nl80211)");
        assert_eq!(role("vendor.keymint-qti", "android.hardware.security.keymint-service-qti"), "keep: keys and the TEE (never touched)");
        assert_eq!(role("vold", "vold"), "keep: the way back");
        assert_eq!(role("-", "init"), "keep: the way back");
        assert_eq!(role("media", "mediaserver"), "restart list; encoder: not known if needed");
        assert_eq!(role("vendor.thermal-engine", "thermal-engine-v2"), "keep: thermal and charge limits");
        assert_eq!(role("-", "some.samsung.daemon"), "-");
    }
}
