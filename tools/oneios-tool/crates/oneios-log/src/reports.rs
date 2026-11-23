//! `log gpu`, `log cpu`, `log display`, `log perf`: tables over time from the periodic report lines.
//!
//! Every field is read by its label (`parse::Scan`), so a cut line gives the fields it still has and `-` for the
//! rest, and a format that gained a field at the end still reads. A line of a known kind whose first field is
//! not there counts as unparsed; the counts are in the notes.

use crate::parse::{Log, Rec, Scan, Src};
use crate::table::{Cell, Table};
use std::collections::BTreeMap;

#[derive(Default)]
pub struct Notes {
    unparsed: BTreeMap<&'static str, (usize, usize)>,
    cut: BTreeMap<&'static str, (usize, usize)>,
}

impl Notes {
    fn unparsed(&mut self, kind: &'static str, line: usize) {
        let e = self.unparsed.entry(kind).or_insert((0, line));
        e.0 += 1;
    }

    fn seen(&mut self, kind: &'static str, r: &Rec) {
        if r.tail_cut {
            let e = self.cut.entry(kind).or_insert((0, r.line));
            e.0 += 1;
        }
    }

    pub fn lines(&self) -> Vec<String> {
        let mut out = Vec::new();
        for (kind, (n, line)) in &self.unparsed {
            out.push(format!("unparsed: {n} `{kind}` line(s), the first at line {line}"));
        }
        for (kind, (n, line)) in &self.cut {
            out.push(format!("cut: {n} `{kind}` line(s) lost their end (another record was written into them), the first at line {line}; their missing fields are `-`"));
        }
        out
    }
}

fn head(r: &Rec) -> Vec<Cell> {
    vec![Cell::float(r.time, 3), Cell::Int(r.line as i64)]
}

fn guest<'a>(r: &'a Rec, tag: &str) -> Option<&'a str> {
    (r.src == Src::Guest).then(|| r.text.strip_prefix(tag)).flatten()
}

fn host<'a>(r: &'a Rec, tag: &str) -> Option<&'a str> {
    matches!(r.src, Src::Host(_)).then(|| r.text.strip_prefix(tag)).flatten()
}

/// `N s: ` or `N.N s: ` at the start: the period.
fn period(s: &mut Scan) -> Option<f64> {
    let p = s.num("")?;
    s.skip(" s").then_some(p)
}

pub fn gpu(log: &Log) -> (Vec<Table>, Vec<String>) {
    let mut notes = Notes::default();
    let mut report = Table::new(
        "guest GPU report (oneios-gpu-report: per period; us)",
        &["time", "line", "s", "submits", "fenced", "frames", "mean_us", "max_ever_us", "<1ms", "<2ms", "<4ms", "<8ms", "<16ms", "<32ms", "longer", "fence_timeouts", "refusals"],
    );
    let mut memory = Table::new(
        "guest GPU memory (oneios-gpu-memory)",
        &["time", "line", "blobs+", "closed+", "deferred+", "waiting", "live", "own", "own_MiB", "wired", "wired_MiB", "contexts+", "clients"],
    );
    let mut clients = Table::new("guest GPU clients (oneios-gpu-clients: own+wired MiB)", &["time", "line", "client", "own_MiB", "wired_MiB"]);
    let mut hosts = Table::new(
        "host GPU (xnuhost gpu: N s host; us)",
        &["time", "line", "s", "bb_fences", "retired_mean", "retired_max", "submit_mean", "submit_max", "other_fences", "other_mean", "blobs", "blob_mean", "blob_max", "udmabuf", "clock_held+", "keepalives+"],
    );
    for r in &log.recs {
        if let Some(text) = guest(r, "oneios-gpu-report: ") {
            let mut s = Scan::new(text);
            let Some(p) = period(&mut s) else {
                notes.unparsed("oneios-gpu-report", r.line);
                continue;
            };
            notes.seen("oneios-gpu-report", r);
            let mut row = head(r);
            row.push(Cell::Int(p as i64));
            for label in ["submits ", "(fenced ", "frames ", "mean ", "max ever ", " ms ", "/", "/", "/", "/", "/", "longer ", "fence timeouts ", "refusals "] {
                row.push(Cell::int(s.num(label)));
            }
            report.push(row);
        } else if let Some(text) = guest(r, "oneios-gpu-memory: ") {
            let mut s = Scan::new(text);
            let Some(blobs) = s.num("blobs ") else {
                notes.unparsed("oneios-gpu-memory", r.line);
                continue;
            };
            notes.seen("oneios-gpu-memory", r);
            let mut row = head(r);
            row.push(Cell::Int(blobs as i64));
            for label in ["closed ", "(deferred ", "waiting ", "live ", "own ", "(", "wired ", "(", "contexts ", "clients "] {
                row.push(Cell::int(s.num(label)));
            }
            memory.push(row);
        } else if let Some(text) = guest(r, "oneios-gpu-clients: ") {
            notes.seen("oneios-gpu-clients", r);
            let mut any = false;
            for part in text.split(", ") {
                let mut words = part.split(' ');
                let (Some(name), Some(sizes)) = (words.next(), words.next()) else { continue };
                let Some((own, wired)) = sizes.split_once('+') else { continue };
                let (Ok(own), Ok(wired)) = (own.parse::<i64>(), wired.parse::<i64>()) else { continue };
                let mut row = head(r);
                row.extend([Cell::text(name), Cell::Int(own), Cell::Int(wired)]);
                clients.push(row);
                any = true;
            }
            if !any {
                notes.unparsed("oneios-gpu-clients", r.line);
            }
        } else if let Some(text) = host(r, "gpu: ") {
            let mut s = Scan::new(text);
            let Some(p) = period(&mut s) else { continue };
            if !s.skip(" host: ") {
                continue;
            }
            let mut row = head(r);
            row.push(Cell::Int(p as i64));
            for label in ["backboardd fences ", "submit->retired mean ", "max ", "submit calls mean ", "max ", "other fences ", "mean ", "blobs made ", "mean ", "max ", "(udmabufs ", "clock held ", "keep-alives "] {
                row.push(Cell::int(s.num(label)));
            }
            hosts.push(row);
        }
    }
    (vec![report, memory, clients, hosts], notes.lines())
}

/// `name[pid] N%` entries of an `oneios-cpu` line.
fn cpu_entries(text: &str) -> Vec<(String, i64, i64)> {
    let mut out = Vec::new();
    for part in text.split(", ") {
        let part = part.split(" (all").next().unwrap_or(part).trim();
        let Some(open) = part.rfind('[') else { continue };
        let Some(close) = part[open..].find(']') else { continue };
        let pid = part[open + 1..open + close].parse::<i64>();
        let pct = part[open + close + 1..].trim().trim_end_matches('%').parse::<i64>();
        if let (Ok(pid), Ok(pct)) = (pid, pct) {
            out.push((part[..open].to_string(), pid, pct));
        }
    }
    out
}

pub fn cpu(log: &Log) -> (Vec<Table>, Vec<String>) {
    let mut notes = Notes::default();
    let mut report = Table::new("guest CPU (oneios-cpu: percent of one CPU per period)", &["time", "line", "s", "all%", "top"]);
    let mut totals: BTreeMap<(String, i64), (i64, i64, i64)> = BTreeMap::new();
    let mut reports = 0i64;
    for r in &log.recs {
        let Some(text) = guest(r, "oneios-cpu: ") else { continue };
        let mut s = Scan::new(text);
        let Some(p) = period(&mut s) else {
            notes.unparsed("oneios-cpu", r.line);
            continue;
        };
        notes.seen("oneios-cpu", r);
        let entries = cpu_entries(s.rest().trim_start_matches(':').trim());
        let all = Scan::new(text).num("(all ");
        let top: Vec<String> = entries.iter().map(|(name, pid, pct)| format!("{name}[{pid}] {pct}%")).collect();
        let mut row = head(r);
        row.extend([Cell::Int(p as i64), Cell::int(all), Cell::text(top.join(", "))]);
        report.push(row);
        reports += 1;
        for (name, pid, pct) in entries {
            let e = totals.entry((name, pid)).or_insert((0, 0, 0));
            e.0 += 1;
            e.1 += pct;
            e.2 = e.2.max(pct);
        }
    }
    let mut summary = Table::new(
        "guest CPU by process (a process is listed in a report when it is among the top ones)",
        &["process", "pid", "reports", "sum%", "mean%_listed", "mean%_all_reports", "max%"],
    );
    let mut rows: Vec<_> = totals.into_iter().collect();
    rows.sort_by(|a, b| b.1 .1.cmp(&a.1 .1).then_with(|| a.0.cmp(&b.0)));
    for ((name, pid), (n, sum, max)) in rows {
        summary.push(vec![
            Cell::text(name),
            Cell::Int(pid),
            Cell::Int(n),
            Cell::Int(sum),
            Cell::Float(sum as f64 / n as f64, 1),
            Cell::Float(sum as f64 / reports.max(1) as f64, 1),
            Cell::Int(max),
        ]);
    }
    let (qos, clocks) = host_cpu(log, &mut notes);
    (vec![report, summary, qos, clocks], notes.lines())
}

/// The host's side of `log cpu` (2026-11-23): xnuhost's `perf: qos:` line (the iOS threads' turns on each vCPU by the
/// kernel's class of the thread) and the thermal bridge's `thermal: N s clocks:` line (each cpufreq policy's busy share
/// and frequency residency), one row per vCPU or policy per period.
fn host_cpu(log: &Log, notes: &mut Notes) -> (Table, Table) {
    let mut qos = Table::new(
        "host vCPUs by QoS (xnuhost perf: qos; percent of the vCPU's wall time in an iOS thread's turn)",
        &["time", "line", "cpu", "kind", "realtime%", "ui%", "default%", "background%"],
    );
    let mut clocks = Table::new(
        "host clocks (thermal: N s clocks; per cpufreq policy)",
        &["time", "line", "policy", "cpus", "busy%", "mean_mhz", "top_mhz", "at_top%", "under_half%"],
    );
    for r in &log.recs {
        if let Some(text) = host(r, "perf: qos: ") {
            let list = text.split_once("): ").map_or("", |x| x.1);
            let mut any = false;
            for part in list.split(", ") {
                // `cpu3 P 1/20/5/0`
                let w: Vec<&str> = part.split_whitespace().collect();
                let [cpu, kind, shares] = w[..] else { continue };
                let Some(Ok(cpu)) = cpu.strip_prefix("cpu").map(str::parse::<i64>) else { continue };
                let v: Vec<i64> = shares.split('/').filter_map(|x| x.parse().ok()).collect();
                let [rt, ui, def, bg] = v[..] else { continue };
                let mut row = head(r);
                row.extend([Cell::Int(cpu), Cell::text(kind), Cell::Int(rt), Cell::Int(ui), Cell::Int(def), Cell::Int(bg)]);
                qos.push(row);
                any = true;
            }
            if any {
                notes.seen("xnuhost perf: qos", r);
            } else {
                notes.unparsed("xnuhost perf: qos", r.line);
            }
        } else if let Some(text) = host(r, "thermal: ") {
            let Some((_, list)) = text.split_once(" s clocks: ") else { continue };
            let mut any = false;
            for part in list.split("; ") {
                // `policy0 (cpu0-1) busy 40%, mean 1478 of 2265 MHz, at the top 33%, under half 33%`
                let mut s = Scan::new(part);
                let Some(policy) = s.num("policy") else { continue };
                let cpus = part.split_once('(').and_then(|x| x.1.split_once(')')).map_or("", |x| x.0);
                let mut row = head(r);
                row.extend([Cell::Int(policy as i64), Cell::text(cpus)]);
                for label in ["busy ", "mean ", "of ", "at the top ", "under half "] {
                    row.push(Cell::int(s.num(label)));
                }
                clocks.push(row);
                any = true;
            }
            if any {
                notes.seen("thermal clocks", r);
            } else {
                notes.unparsed("thermal clocks", r.line);
            }
        }
    }
    (qos, clocks)
}

pub fn display(log: &Log) -> (Vec<Table>, Vec<String>) {
    let mut notes = Notes::default();
    let mut report = Table::new(
        "guest display report (oneios-display-report: counts since the boot)",
        &["time", "line", "prepare", "clients", "swaps", "swaps+", "accepted", "refused", "transfers", "retirements", "kicks", "in_flight", "not_ready", "no_head", "latch_busy", "fence", "copy", "blank", "flips", "latches", "retires"],
    );
    let mut flushes = Table::new("host display (xnuhost display: N s; copy us)", &["time", "line", "s", "flushes", "copy_mean", "copy_max", "MiB"]);
    let mut swaps = Table::new(
        "guest swaps (oneios-display-report: W records; t is the guest's clock; each record once)",
        &["swap", "line", "t", "since_last_ms", "offer_ms", "done_ms"],
    );
    // One row per report: the block's lines carry the same time.
    let mut open: Option<(usize, f64)> = None; // (row, time)
    let mut last_swaps: Option<f64> = None;
    let mut seen_swaps: BTreeMap<i64, ()> = BTreeMap::new();
    for r in &log.recs {
        if r.src == Src::Guest && r.text.starts_with("Darwin Kernel Version") {
            last_swaps = None;
            seen_swaps.clear();
            open = None;
        }
        if let Some(text) = guest(r, "oneios-display-report: ") {
            let time = r.time.unwrap_or(0.0);
            let block_line = text.starts_with("prepare ") || text.starts_with("clients ") || text.starts_with("swapstat ");
            if block_line {
                notes.seen("oneios-display-report", r);
                if !matches!(open, Some((_, t)) if (t - time).abs() < 0.5) {
                    let mut row = head(r);
                    row.extend(std::iter::repeat_n(Cell::None, report.columns.len() - 2));
                    report.push(row);
                    open = Some((report.rows.len() - 1, time));
                }
            }
            let row = open.map(|(i, _)| i);
            let mut s = Scan::new(text);
            if text.starts_with("prepare ") {
                if let (Some(row), Some(calls)) = (row, s.num("calls=")) {
                    report.rows[row][2] = Cell::Int(calls as i64);
                } else {
                    notes.unparsed("oneios-display-report: prepare", r.line);
                }
            } else if text.starts_with("clients ") {
                let (Some(row), Some(clients)) = (row, s.num("clients ")) else {
                    notes.unparsed("oneios-display-report: clients", r.line);
                    continue;
                };
                let validated = s.num("swaps validated ");
                let cells = &mut report.rows[row];
                cells[3] = Cell::Int(clients as i64);
                cells[4] = Cell::int(validated);
                cells[5] = match (validated, last_swaps) {
                    (Some(now), Some(before)) => Cell::Int((now - before) as i64),
                    (Some(now), None) => Cell::Int(now as i64),
                    _ => Cell::None,
                };
                if validated.is_some() {
                    last_swaps = validated;
                }
                cells[6] = Cell::int(s.num("(accepted "));
                cells[7] = Cell::int(s.num("refused "));
                cells[8] = Cell::int(s.num("transfers "));
                cells[9] = Cell::int(s.num("retirements "));
            } else if text.starts_with("swapstat ") {
                let Some(row) = row else { continue };
                for (i, label) in ["kicks ", "in-flight ", "not-ready ", "no-head ", "latch-busy ", "fence ", "copy ", "blank ", "flips ", "latches ", "retires "].iter().enumerate() {
                    report.rows[row][10 + i] = Cell::int(s.num(label));
                }
            } else if let Some(rest) = text.strip_prefix('W') {
                let mut s = Scan::new(rest);
                let Some(n) = s.num("") else {
                    notes.unparsed("oneios-display-report: W", r.line);
                    continue;
                };
                if seen_swaps.insert(n as i64, ()).is_some() {
                    continue;
                }
                swaps.push(vec![
                    Cell::Int(n as i64),
                    Cell::Int(r.line as i64),
                    Cell::float(s.num(" t="), 3),
                    Cell::int(s.num("s +")),
                    Cell::int(s.num("offer=")),
                    Cell::int(s.num("done=")),
                ]);
            }
        } else if let Some(text) = host(r, "display: ") {
            let mut s = Scan::new(text);
            let Some(p) = period(&mut s) else { continue };
            let Some(n) = s.num(": flushes ") else { continue };
            let mut row = head(r);
            row.extend([Cell::Int(p as i64), Cell::Int(n as i64), Cell::int(s.num("copy mean ")), Cell::int(s.num("max ")), Cell::int(s.num("("))]);
            flushes.push(row);
        }
    }
    (vec![report, flushes, swaps], notes.lines())
}

pub fn perf(log: &Log) -> (Vec<Table>, Vec<String>) {
    let mut notes = Notes::default();
    let mut guest_t = Table::new(
        "guest perf (oneios-perf: the kernel's share of user traps per period; us)",
        &["time", "line", "s", "user_stays", "syscalls", "bsd", "bsd_us", "mach", "mach_us", "platform", "platform_us", "faults", "filled", "filled_us", "vm_fault", "vm_fault_us", "refilled", "interrupts", "twins+", "twins-"],
    );
    let mut pmap = Table::new(
        "guest pmap (oneios-perf: pmap; the TLB lock in us and percent of a vCPU)",
        &["time", "line", "user_maps", "user_unmaps", "kernel_maps", "kernel_unmaps", "tlb_taken", "tlb_waited", "wait_us", "wait%", "held_us", "held%", "copyio", "copyio_faults"],
    );
    let mut calls = Table::new("guest top calls (oneios-perf: bsd / mach traps: number, calls, mean us)", &["time", "line", "kind", "number", "calls", "mean_us"]);
    let mut host_t = Table::new(
        "host perf (xnuhost perf: per period)",
        &["time", "line", "s", "task_traps", "per_s", "syscall", "fault", "interrupt", "other", "sleeps_vcpu", "sleeps_ios", "handoffs", "wakes_a", "wakes_b", "map_by_release", "map_ordered", "timer_set", "to_host_timer"],
    );
    let mut lat = Table::new("host latencies (xnuhost perf rows; us)", &["time", "line", "what", "n", "mean", "p50", "p90", "p99", "max"]);
    let mut cpus = Table::new("host vCPUs (xnuhost perf: cpuN; percent)", &["time", "line", "cpu", "core%", "kernel%", "ios_thread%", "idle%", "traps_per_s", "host_wait%"]);
    for r in &log.recs {
        if let Some(text) = guest(r, "oneios-perf: ") {
            let body = text.trim_start();
            let mut s = Scan::new(body);
            if let Some(rest) = body.strip_prefix("pmap: ") {
                let mut s = Scan::new(rest);
                let Some(maps) = s.num("user maps ") else {
                    notes.unparsed("oneios-perf: pmap", r.line);
                    continue;
                };
                notes.seen("oneios-perf: pmap", r);
                let mut row = head(r);
                row.push(Cell::Int(maps as i64));
                for label in ["unmaps ", "kernel maps ", "unmaps ", "TLB lock taken ", ", "] {
                    row.push(Cell::int(s.num(label)));
                }
                row.push(Cell::int(s.num("waited (")));
                row.push(Cell::float(s.num("us waiting, "), 1));
                row.push(Cell::int(s.num("held ")));
                row.push(Cell::float(s.num("us ("), 1));
                row.push(Cell::int(s.num("copyio ")));
                row.push(Cell::int(s.num("(")));
                pmap.push(row);
            } else if body.starts_with("bsd (") || body.starts_with("mach traps:") {
                notes.seen("oneios-perf: calls", r);
                let kind = if body.starts_with("bsd") { "bsd" } else { "mach" };
                let list = body.split_once(": ").map_or("", |x| x.1);
                let words: Vec<&str> = list.split_whitespace().collect();
                let mut any = false;
                for triple in words.chunks(3) {
                    let [number, count, mean] = triple else { continue };
                    let (Ok(number), Ok(count), Some(Ok(mean))) =
                        (number.parse::<i64>(), count.parse::<i64>(), mean.strip_prefix('x').map(|m| m.parse::<i64>()))
                    else {
                        continue;
                    };
                    let mut row = head(r);
                    row.extend([Cell::text(kind), Cell::Int(number), Cell::Int(count), Cell::Int(mean)]);
                    calls.push(row);
                    any = true;
                }
                if !any {
                    notes.unparsed("oneios-perf: calls", r.line);
                }
            } else if let Some(p) = period(&mut s) {
                let Some(stays) = s.num(": ") else {
                    notes.unparsed("oneios-perf", r.line);
                    continue;
                };
                notes.seen("oneios-perf", r);
                let mut row = head(r);
                row.extend([Cell::Int(p as i64), Cell::Int(stays as i64)]);
                for label in ["user stays; ", "(bsd ", ", ", "mach ", ", ", "platform ", ", ", "us); ", "faults, ", "(", "us), ", "(", "us), ", "refilled; ", "twins +", " -"] {
                    row.push(Cell::int(s.num(label)));
                }
                guest_t.push(row);
            }
        } else if let Some(text) = host(r, "perf: ") {
            let body = text.trim_start();
            let mut s = Scan::new(body);
            if let Some(rest) = body.strip_prefix("cpu") {
                let mut s = Scan::new(rest);
                let Some(n) = s.num("") else { continue };
                let mut row = head(r);
                row.push(Cell::Int(n as i64));
                for label in [": ", "of its wall time ", "kernel, ", "turn, ", "idle; "] {
                    row.push(Cell::int(s.num(label)));
                }
                // since 2026-11-23: the vCPU thread's wait for a host CPU (schedstat); empty in older logs
                row.push(Cell::int(s.num("waited for a host CPU ")));
                cpus.push(row);
            } else if let Some(at) = body.find("  n ") {
                // `up (trap->vCPU)    n    46933  mean   12.52  p50 ...`
                let what = body[..at].trim();
                let mut s = Scan::new(&body[at..]);
                let mut row = head(r);
                row.push(Cell::text(what));
                let mut field = |label: &str, decimals: usize| {
                    s.skip(label);
                    let rest = s.rest().trim_start();
                    let mut inner = Scan::new(rest);
                    Cell::float(inner.num(""), decimals)
                };
                let n = field(" n ", 0);
                row.push(match n {
                    Cell::Float(v, _) => Cell::Int(v as i64),
                    other => other,
                });
                for (label, decimals) in [("mean", 2), ("p50", 2), ("p90", 2), ("p99", 2), ("max", 1)] {
                    row.push(field(label, decimals));
                }
                lat.push(row);
            } else if let Some(p) = period(&mut s) {
                let Some(traps) = s.num(": ") else {
                    notes.unparsed("xnuhost perf", r.line);
                    continue;
                };
                let mut row = head(r);
                row.extend([Cell::Float(p, 1), Cell::Int(traps as i64)]);
                for label in ["task traps (", "syscall ", "fault ", "interrupt ", "other ", "vCPU ", "iOS thread ", "(of ", "futex wakes ", "+ ", "task_map ", "release, ", "timer_set ", "("] {
                    row.push(Cell::int(s.num(label)));
                }
                host_t.push(row);
            }
        }
    }
    (vec![guest_t, pmap, calls, host_t, lat, cpus], notes.lines())
}

/// The S Pen's stages, host to guest, in the order `docs/hardware.md` 7f "On the phone" lists them: the needle that
/// starts a stage (host or guest text), and what a missing one most likely means.
const PEN_STAGES: [(&str, bool, &str, &str); 12] = [
    ("device found", true, "pen: /dev/input/", "pen=off (no `pen:` line at all), or `pen: none found` (the next row)"),
    ("guest device", true, "pen: the guest's pen device is /arm-io/input-1", "no pen device for the guest"),
    ("pen IC woken", true, "brightness: pen: the S Pen's IC", "the brightness bridge did not wake the pen's IC (before 2026-11-22 it never did; oneios-tool phone pen status|wake)"),
    ("transport", false, "droidvm input /arm-io/input-1:", "the guest's transport did not take /arm-io/input-1 as a pen"),
    ("HID device", false, "oneios-hid: pen: HID device started", "oneios-pen=0, or the transport found no pen"),
    ("event driver", false, "oneios-hid: pen: event driver ", "no IOHIDEventDriver on the pen's HID device"),
    ("published", false, "oneios-hid: pen: Digitizer/Stylus", "the event service's pencil properties were not set"),
    ("backboardd", false, "user client(s) on the event service", "backboardd did not open the pen's event service"),
    ("first event", true, "pen: the first event from ", "the pen's device sent nothing: its IC asleep (oneios-tool phone pen status, then wake), or the pen not used"),
    ("in range", true, "pen: in range ", "the pen never came near and left again"),
    ("guest frames", false, "pen frames, ", "no pen frame reached the HID device"),
    ("guest queue", false, "HID events queued for the service's client(s)", "no HID event reached backboardd's queue"),
];

/// Lines that end the pen's path early, with what they say.
const PEN_STOPS: [(bool, &str); 5] = [
    (true, "pen: none found: "),
    (true, "pen: pen=off"),
    (true, "pen: events dropped by the kernel"),
    (false, "oneios-hid: pen: oneios-pen=1, but xnuhost gave no pen device"),
    (false, "oneios-hid: pen: xnuhost has a pen device; not published"),
];

/// `log pen`: the S Pen's path, stage by stage (the first line of each and how many there are), each time in range
/// (xnuhost's `pen: in range` lines) and the guest's counts (`oneios-hid: pen: N pen frames`).
pub fn pen(log: &Log) -> (Vec<Table>, Vec<String>) {
    let mut notes = Notes::default();
    let mut stages = Table::new("pen stages (docs/hardware.md 7f; the first line of each)", &["stage", "side", "count", "time", "line", "text"]);
    let mut stops = Table::new("pen stops (lines that end the path early)", &["time", "line", "side", "text"]);
    let mut ranges = Table::new(
        "host pen in range (xnuhost pen: in range: one row per time the pen came near and left)",
        &["time", "line", "ms", "frames", "hovering", "touching", "strokes", "pressure_max", "height_min", "height_max", "tilt_x_min", "tilt_x_max", "tilt_y_min", "tilt_y_max", "presses", "unchanged", "coalesced", "hover_still", "sent"],
    );
    let mut counts = Table::new(
        "guest pen counts (oneios-hid: pen: since the boot)",
        &["time", "line", "pen_frames", "hid_reports", "sent", "as_pencil", "layout_unknown", "queued", "dropped"],
    );
    let mut seen: Vec<(usize, Option<&Rec>)> = vec![(0, None); PEN_STAGES.len()];
    let pending_queue = |counts: &mut Table, r: &Rec, text: &str| {
        // `N HID events queued ..., M dropped`: on the row of the frames line before it, else a row of its own.
        let mut s = Scan::new(text);
        let queued = s.num("oneios-hid: pen: ");
        let dropped = s.num("client(s), ");
        match counts.rows.last_mut() {
            Some(row) if matches!(row[7], Cell::None) => {
                row[7] = Cell::int(queued);
                row[8] = Cell::int(dropped);
            }
            _ => {
                let mut row = head(r);
                row.extend([Cell::None, Cell::None, Cell::None, Cell::None, Cell::None, Cell::int(queued), Cell::int(dropped)]);
                counts.push(row);
            }
        }
    };
    for r in &log.recs {
        let is_host = matches!(r.src, Src::Host(_));
        if !is_host && r.src != Src::Guest {
            continue;
        }
        let text = r.text.as_str();
        for (i, (name, host_side, needle, _)) in PEN_STAGES.iter().enumerate() {
            let hit = if *host_side { is_host && text.starts_with(needle) } else { !is_host && (text.starts_with(needle) || (text.starts_with("oneios-hid: pen: ") && text.contains(needle))) };
            // The transport's line names the pen's kind: `..., pen`.
            if hit && !(*name == "transport" && !text.ends_with(", pen")) {
                seen[i].0 += 1;
                seen[i].1.get_or_insert(r);
            }
        }
        for (host_side, needle) in PEN_STOPS {
            if host_side == is_host && text.starts_with(needle) {
                let mut row = head(r);
                row.extend([Cell::text(if is_host { "host" } else { "guest" }), Cell::text(text)]);
                stops.push(row);
            }
        }
        if let Some(rest) = host(r, "pen: in range ") {
            let mut s = Scan::new(rest);
            let Some(ms) = s.num("") else {
                notes.unparsed("pen: in range", r.line);
                continue;
            };
            notes.seen("pen: in range", r);
            let mut row = head(r);
            row.push(Cell::Int(ms as i64));
            for label in [" ms: ", "(", "hovering, ", "touching), "] {
                row.push(Cell::int(s.num(label)));
            }
            row.push(Cell::int(s.num("pressure up to ")));
            // The hover height (since 2026-11-22) only for a time with hovering frames, the tilt only for a device that has it.
            let mut h = Scan::new(s.rest());
            if h.skip(", height ") {
                let low = h.num("");
                row.extend([Cell::int(low), Cell::int(h.num(".."))]);
            } else {
                row.extend([Cell::None, Cell::None]);
            }
            let mut t = Scan::new(s.rest());
            let tilt: Vec<Option<f64>> = if t.skip("tilt x ") {
                let x0 = t.num("");
                let x1 = t.num("..");
                let y0 = t.num(" y ");
                let y1 = t.num("..");
                vec![x0, x1, y0, y1]
            } else {
                vec![None; 4]
            };
            row.extend(tilt.into_iter().map(Cell::int));
            row.push(Cell::int(s.num("pressed ")));
            row.push(Cell::int(s.num("; ")));
            // Since spen-lag (2026-11-22): the frames coalesced to the panel's rate, the hovering ones within a pixel, those sent.
            row.push(Cell::int(s.num(" not sent, ")));
            row.push(Cell::int(s.num(" edges), ")));
            row.push(Cell::int(s.num("pixel; ")));
            ranges.push(row);
        } else if let Some(rest) = guest(r, "oneios-hid: pen: ") {
            if rest.contains(" pen frames, ") {
                let mut s = Scan::new(rest);
                let mut row = head(r);
                row.push(Cell::int(s.num("")));
                row.push(Cell::int(s.num("pen frames, ")));
                s.skip("(last result ");
                row.push(Cell::int(s.num("), ")));
                row.push(Cell::int(s.num("sent (")));
                row.push(Cell::int(s.num("pressure and height, ")));
                row.extend([Cell::None, Cell::None]);
                counts.push(row);
            } else if rest.contains("HID events queued") {
                pending_queue(&mut counts, r, &r.text);
            }
        }
    }
    for (i, (name, host_side, _, missing)) in PEN_STAGES.iter().enumerate() {
        let side = Cell::text(if *host_side { "host" } else { "guest" });
        match seen[i] {
            (n, Some(r)) => stages.push(vec![Cell::text(*name), side, Cell::Int(n as i64), Cell::float(r.time, 3), Cell::Int(r.line as i64), Cell::text(r.text.clone())]),
            _ => stages.push(vec![Cell::text(*name), side, Cell::Int(0), Cell::None, Cell::None, Cell::text(format!("missing: {missing}"))]),
        }
    }
    let mut lines = notes.lines();
    // The stylus wrapper (staged/hid_pen.rs orient()): it goes on at the pen device's first report, so the publish
    // line's `edge wrapper N` (a global counter, printed before any report) says nothing about it. Its proof is the
    // frames line: stylus events given the pencil's angles. Sent but none given: the wrapper is not on the pen's driver.
    if let Some(row) = counts.rows.iter().rev().find(|r| matches!(r[4], Cell::Int(n) if n > 0)) {
        match (&row[5], &row[6]) {
            (Cell::Int(0), Cell::Int(unknown)) => lines.push(format!(
                "wrapper: the pen's reports were sent but no stylus event was given the pencil's angles ({unknown} of a layout not known): the wrapper is not on the pen's event driver, or the event data is not the layout orient() knows (line {})",
                match row[1] { Cell::Int(l) => l, _ => 0 }
            )),
            (Cell::Int(given), _) => lines.push(format!("wrapper: on the pen's event driver ({given} stylus events given the pencil's angles, pressure and height)")),
            _ => {}
        }
    }
    lines.push("iOS's side is in its own log, not here: oneios-tool ioslog show DATA.img --proc backboardd --grep 'pencil digitizer' (`*** acquired pencil digitizer`)".to_string());
    (vec![stages, stops, ranges, counts], lines)
}
