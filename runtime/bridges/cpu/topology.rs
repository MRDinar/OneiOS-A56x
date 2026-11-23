// SPDX-License-Identifier: MIT
//! The CPU bridge (C: cpu/topology.c): the host's cores from /sys/devices/system/cpu, the vCPU plan XNU's topology is
//! made from (P = the big and prime cores, E = the little ones), and the placement of host threads.
//!
//! On the Galaxy S24 Ultra (SM8650): cpu0-1 Cortex-A520, cpu2-6 Cortex-A720, cpu7 Cortex-X4. On the Galaxy S25
//! (SM8750, Snapdragon 8 Elite for Galaxy): two prime and six performance Oryon cores, no little ones
//! (docs/hardware.md "Galaxy S25"); the exact capacities and MIDR come from the phone (tools/probe/s25-probe.sh).
//! Linux keeps clock control: the only frequency input from the guest is a utilisation clamp.

use std::io;
use std::sync::atomic::{AtomicU32, AtomicU64, Ordering};
use xh_core::fs::read_text; // a sysfs file's text, its trailing white space dropped

pub const MAX_CPUS: usize = 16;
pub const MAX_VCPUS: usize = 8;
pub const MAX_FREQS: usize = 40;

/// A core's kind: the ADT cluster-type of a vCPU ('P' or 'E').
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum CoreKind {
    #[default]
    P,
    E,
}

impl CoreKind {
    /// 'P' or 'E'.
    pub fn letter(self) -> char {
        match self {
            CoreKind::P => 'P',
            CoreKind::E => 'E',
        }
    }

    /// The cluster id of this kind: 0 = E cluster, 1 = P cluster (E first, as on Apple SoCs).
    pub fn cluster_id(self) -> u32 {
        u32::from(self == CoreKind::P)
    }
}

/// One host CPU.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct HostCpu {
    pub present: bool,
    /// cpu_capacity (0..1024); 0 = not reported
    pub capacity: u32,
    /// cpufreq policy id (first CPU of the frequency domain)
    pub policy: u32,
    /// cpuinfo_min_freq, cpuinfo_max_freq
    pub min_khz: u32,
    pub max_khz: u32,
    /// scaling_available_frequencies (and the boost ones), ascending, at most MAX_FREQS
    pub freq_khz: Vec<u32>,
    /// the highest of scaling_boost_frequencies (also in freq_khz), 0 = none listed
    pub boost_khz: u32,
    /// scaling_max_freq when read (a cap someone set: logged, never written)
    pub scaling_max_khz: u32,
    /// MIDR part number (Arm: 0xd80 A520, 0xd81 A720, 0xd82 X4), 0 = unknown
    pub part: u32,
    /// MIDR implementer: 0x41 Arm, 0x51 Qualcomm (Oryon: the SM8750's cores), 0 = unknown
    pub implementer: u32,
    pub l1d_bytes: u32,
    pub l1i_bytes: u32,
    pub l2_bytes: u32,
    pub l3_bytes: u32,
    /// set by [`Topology::read`]
    pub kind: CoreKind,
}

impl HostCpu {
    /// Without cpu_capacity (a kernel without the arch topology driver) the top frequency orders the cores.
    fn rank(&self) -> u32 {
        if self.capacity != 0 {
            self.capacity
        } else {
            self.max_khz / 1000
        }
    }
}

/// The host's CPUs: `cpu[i]` is CPU i, up to the highest present one (`ncpu()`), with holes not present.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Topology {
    pub cpu: Vec<HostCpu>,
    pub n_p: u32,
    pub n_e: u32,
}

/// The leading decimal number of `text` (strtoul base 10), 0 when there is none.
fn leading_decimal(text: &str) -> u64 {
    xh_core::parse::leading_u64(text, 10).map_or(0, |(v, _)| v)
}

/// A number as C's strtoull(text, &end, 0) read it (0x hex, 0 octal, decimal).
fn read_number(path: &str) -> Option<u64> {
    xh_core::parse::leading_u64(&read_text(path)?, 0).map(|(v, _)| v)
}

/// "64K", "2048K", "12M" or bytes.
fn cache_bytes(text: &str) -> u32 {
    u32::try_from(xh_core::parse::cache_size(text)).unwrap_or(0)
}

/// The frequencies of a list "364800 460800 ..." (zeroes left out).
fn frequencies(text: &str) -> impl Iterator<Item = u32> + '_ {
    text.split([' ', '\t', '\n']).filter(|w| !w.is_empty()).map(|w| leading_decimal(w) as u32).filter(|&k| k != 0)
}

fn read_caches(dir: &str, c: &mut HostCpu) {
    for i in 0..6 {
        let at = |name: &str| read_text(format!("{dir}/cache/index{i}/{name}"));
        let (Some(level), Some(kind), Some(size)) = (at("level"), at("type"), at("size")) else { continue };
        let bytes = cache_bytes(&size);
        match (level.as_bytes()[0], kind.as_bytes()[0]) {
            (b'1', b'D') => c.l1d_bytes = bytes,
            (b'1', b'I') => c.l1i_bytes = bytes,
            (b'2', _) => c.l2_bytes = bytes,
            (b'3', _) => c.l3_bytes = bytes,
            _ => {}
        }
    }
}

fn read_cpu(dir: &str) -> Option<HostCpu> {
    let mut c = HostCpu::default();
    let number = |rel: &str| read_number(&format!("{dir}/{rel}"));
    let text = |rel: &str| read_text(format!("{dir}/{rel}"));
    let capacity = number("cpu_capacity");
    let max = number("cpufreq/cpuinfo_max_freq");
    if capacity.is_none() && max.is_none() && text("topology/core_id").is_none() {
        return None;
    }
    c.present = true;
    c.capacity = capacity.unwrap_or(0) as u32;
    c.max_khz = max.unwrap_or(0) as u32;
    c.min_khz = number("cpufreq/cpuinfo_min_freq").unwrap_or(0) as u32;
    if let Some(t) = text("cpufreq/scaling_available_frequencies") {
        c.freq_khz = frequencies(&t).take(MAX_FREQS).collect();
        c.freq_khz.sort_unstable();
    }
    if c.freq_khz.is_empty() && c.max_khz != 0 {
        // no table: the two ends
        if c.min_khz != 0 && c.min_khz < c.max_khz {
            c.freq_khz.push(c.min_khz);
        }
        c.freq_khz.push(c.max_khz);
    }
    // The boost frequencies are not in scaling_available_frequencies (Linux lists CPUFREQ_BOOST_FREQ entries apart;
    // Qualcomm's cpufreq-hw marks a LUT entry as one when fewer cores than the domain's may run at it: the X4's top). The
    // phone's guest saw the X4's vCPU top at 3052.8 MHz where the core reaches 3.39 GHz (host-overhead, 2026-11-15):
    // they belong in its table.
    if let Some(t) = text("cpufreq/scaling_boost_frequencies") {
        for khz in frequencies(&t) {
            if c.freq_khz.len() >= MAX_FREQS {
                break;
            }
            if !c.freq_khz.contains(&khz) {
                c.freq_khz.push(khz);
                c.boost_khz = c.boost_khz.max(khz);
            }
        }
        c.freq_khz.sort_unstable();
    }
    c.scaling_max_khz = number("cpufreq/scaling_max_freq").unwrap_or(0) as u32;
    if let Some(&top) = c.freq_khz.last() {
        c.max_khz = c.max_khz.max(top);
    }
    if let Some(midr) = number("regs/identification/midr_el1") {
        c.part = (midr >> 4) as u32 & 0xfff;
        c.implementer = (midr >> 24) as u32 & 0xff;
    }
    read_caches(dir, &mut c);
    Some(c)
}

impl Topology {
    /// Highest present index + 1.
    pub fn ncpu(&self) -> usize {
        self.cpu.len()
    }

    /// Reads `<root>/devices/system/cpu` (root = "/sys" on the phone; tests pass a fixture). None when no CPU was found.
    /// CPUs of the lowest capacity are E when capacities differ, else all P, with one exception: a Qualcomm core
    /// (implementer 0x51, Oryon) is never E, because Oryon has no little cores. The Galaxy S25's SM8750 is 2 prime + 6
    /// performance Oryon cores, all P here; [`CpuPlan::make`] then lends the slowest ones to the guest as E vCPUs.
    pub fn read(root: &str) -> Option<Topology> {
        let mut cpu: Vec<HostCpu> =
            (0..MAX_CPUS).map(|i| read_cpu(&format!("{root}/devices/system/cpu/cpu{i}")).unwrap_or_default()).collect();
        let ncpu = cpu.iter().rposition(|c| c.present)? + 1;
        cpu.truncate(ncpu);
        // related_cpus: the policy is the first CPU of the domain (C read it within the loop; the same).
        for (i, c) in cpu.iter_mut().enumerate().filter(|(_, c)| c.present) {
            c.policy = read_text(format!("{root}/devices/system/cpu/cpu{i}/cpufreq/related_cpus"))
                .map_or(i as u32, |t| leading_decimal(&t) as u32);
        }
        let ranks = cpu.iter().filter(|c| c.present).map(HostCpu::rank);
        let (low, high) = ranks.fold((u32::MAX, 0), |(lo, hi), r| (lo.min(r), hi.max(r)));
        let mut t = Topology { cpu, n_p: 0, n_e: 0 };
        for c in t.cpu.iter_mut().filter(|c| c.present) {
            // Oryon (Qualcomm, implementer 0x51) has no little cores: the slower cluster is still a P cluster.
            c.kind = if low != high && c.rank() == low && c.implementer != 0x51 { CoreKind::E } else { CoreKind::P };
            match c.kind {
                CoreKind::E => t.n_e += 1,
                CoreKind::P => t.n_p += 1,
            }
        }
        Some(t)
    }

    /// The mask of the present CPUs of `kind`.
    pub fn mask_of(&self, kind: CoreKind) -> u64 {
        self.cpu.iter().enumerate().filter(|(_, c)| c.present && c.kind == kind).fold(0, |m, (i, _)| m | 1 << i)
    }

    /// The CPUs of the frequency domain with the highest (top = true) or lowest top clock among `mask`.
    fn domain_by_clock(&self, mask: u64, top: bool) -> u64 {
        let mut best: Option<(u32, u32)> = None; // (khz, policy)
        for (i, c) in self.cpu.iter().enumerate() {
            if mask >> i & 1 == 0 {
                continue;
            }
            if best.is_none_or(|(khz, _)| if top { c.max_khz > khz } else { c.max_khz < khz }) {
                best = Some((c.max_khz, c.policy));
            }
        }
        let Some((_, policy)) = best else { return 0 };
        self.cpu
            .iter()
            .enumerate()
            .filter(|(i, c)| mask >> i & 1 != 0 && c.policy == policy)
            .fold(0, |m, (i, _)| m | 1 << i)
    }

    /// Thermal steps (bridges/thermal): the host CPUs a vCPU of `kind` may use at step `cap`. 0: all of its kind;
    /// 1: P without the frequency domain of the highest top clock (the X4 here); 2: P limited to the domain of the lowest
    /// top clock, and the E cores; 3: the E cores only. A step that would leave no CPU keeps the one before it.
    pub fn thermal_mask(&self, kind: CoreKind, cap: u32) -> u64 {
        let e = self.mask_of(CoreKind::E);
        let p = self.mask_of(CoreKind::P);
        if kind == CoreKind::E && e != 0 {
            return e; // the E cores are the floor of every step
        }
        let mut m = p;
        let without_top = p & !self.domain_by_clock(p, true);
        if cap >= 1 && without_top != 0 {
            m = without_top;
        }
        if cap >= 2 {
            m = self.domain_by_clock(m, false) | e;
        }
        if cap >= 3 && e != 0 {
            m = e;
        }
        m
    }
}

/// One vCPU of the plan.
#[derive(Debug, Default)]
pub struct Vcpu {
    /// the ADT cluster-type of this vCPU
    pub kind: CoreKind,
    /// 0 = E cluster, 1 = P cluster (E first, as on Apple SoCs)
    pub cluster_id: u32,
    /// host CPUs its thread (and the iOS thread it runs) may use: a thermal step changes it while placement reads it
    pub affinity: AtomicU64,
    /// the core it is described as: its own with pin, else the one cpu-pin gives it
    pub host_cpu: u32,
    /// of host_cpu
    pub capacity: u32,
    pub min_khz: u32,
    pub max_khz: u32,
    pub freq_khz: Vec<u32>,
    pub l1d_bytes: u32,
    pub l1i_bytes: u32,
    pub l2_bytes: u32,
    pub l3_bytes: u32,
}

impl Clone for Vcpu {
    fn clone(&self) -> Vcpu {
        Vcpu {
            kind: self.kind,
            cluster_id: self.cluster_id,
            affinity: AtomicU64::new(self.affinity()),
            host_cpu: self.host_cpu,
            capacity: self.capacity,
            min_khz: self.min_khz,
            max_khz: self.max_khz,
            freq_khz: self.freq_khz.clone(),
            l1d_bytes: self.l1d_bytes,
            l1i_bytes: self.l1i_bytes,
            l2_bytes: self.l2_bytes,
            l3_bytes: self.l3_bytes,
        }
    }
}

impl Vcpu {
    /// Its affinity now.
    pub fn affinity(&self) -> u64 {
        self.affinity.load(Ordering::Acquire)
    }

    /// A vCPU of `kind` on the host CPUs in `mask`: the fastest one gives the frequencies and caches.
    fn from_cpus(t: &Topology, mask: u64, kind: CoreKind) -> Vcpu {
        let mut best: Option<&HostCpu> = None;
        for (i, c) in t.cpu.iter().enumerate() {
            if mask >> i & 1 == 0 {
                continue;
            }
            if best.is_none_or(|b| c.capacity > b.capacity || (c.capacity == b.capacity && c.max_khz > b.max_khz)) {
                best = Some(c);
            }
        }
        let mut v = Vcpu { kind, cluster_id: kind.cluster_id(), affinity: AtomicU64::new(mask), ..Vcpu::default() };
        if let Some(b) = best {
            v.capacity = if b.capacity != 0 { b.capacity } else { 1024 };
            v.min_khz = b.min_khz;
            v.max_khz = b.max_khz;
            v.freq_khz = b.freq_khz.clone();
            (v.l1d_bytes, v.l1i_bytes, v.l2_bytes, v.l3_bytes) = (b.l1d_bytes, b.l1i_bytes, b.l2_bytes, b.l3_bytes);
        }
        v
    }
}

/// The vCPUs for the guest: E first, then P.
#[derive(Clone, Debug, Default)]
pub struct CpuPlan {
    pub v: Vec<Vcpu>,
    pub n_p: u32,
    pub n_e: u32,
}

/// struct sched_attr of sched_setattr(2), as far as the utilisation clamps (Linux 5.3).
#[repr(C)]
struct SchedAttr {
    size: u32,
    sched_policy: u32,
    sched_flags: u64,
    sched_nice: i32,
    sched_priority: u32,
    sched_runtime: u64,
    sched_deadline: u64,
    sched_period: u64,
    sched_util_min: u32,
    sched_util_max: u32,
}
const SCHED_FLAG_KEEP_ALL: u64 = 0x18; // KEEP_POLICY | KEEP_PARAMS
const SCHED_FLAG_UTIL_CLAMP_MIN: u64 = 0x20;

fn einval() -> io::Error {
    io::Error::from_raw_os_error(libc::EINVAL)
}

impl CpuPlan {
    pub fn nvcpu(&self) -> usize {
        self.v.len()
    }

    /// vCPUs for the guest: E vCPUs first (the boot CPU is an E core on iPhones), then P. A host whose cores are all P
    /// but of different speeds (Oryon) lends its slowest cores to the E vCPUs (one in four, at least one, and one P
    /// core is always kept). With pin and nvcpu == host CPUs each vCPU gets one host CPU; otherwise a vCPU may run on
    /// any core of its kind and Linux places it. None on bad arguments.
    pub fn make(t: &Topology, nvcpu: usize, pin: bool) -> Option<CpuPlan> {
        if nvcpu == 0 || nvcpu > MAX_VCPUS || t.ncpu() == 0 || t.n_p == 0 {
            return None;
        }
        let present = || t.cpu.iter().enumerate().filter(|(_, c)| c.present);
        let mut e_list: Vec<usize> = present().filter(|(_, c)| c.kind == CoreKind::E).map(|(i, _)| i).collect();
        let mut p_list: Vec<usize> = present().filter(|(_, c)| c.kind == CoreKind::P).map(|(i, _)| i).collect();
        // The P list fastest first, so that with fewer vCPUs than cores the prime core is used.
        p_list.sort_by(|&a, &b| t.cpu[b].capacity.cmp(&t.cpu[a].capacity).then(a.cmp(&b)));
        // A host with no little cores whose cores differ in speed (the SM8750's Oryon: 2 prime + 6 performance): XNU's
        // AMP scheduler and the iPhone's boot CPU want E cores too, so the slowest cores are lent as E: one in four
        // vCPUs, at least one, and one P core is always kept. A host whose cores are all alike stays all P.
        let mut lent = [false; MAX_CPUS];
        if e_list.is_empty() && nvcpu > 1 && p_list.len() > 1 {
            let first = t.cpu[p_list[0]].capacity;
            if p_list.iter().any(|&c| t.cpu[c].capacity != first) {
                let lend = (nvcpu / 4).max(1).min(p_list.len() - 1);
                for cpu in p_list.split_off(p_list.len() - lend) {
                    e_list.push(cpu);
                    lent[cpu] = true;
                }
            }
        }
        let mask = |list: &[usize]| list.iter().fold(0u64, |m, &c| m | 1 << c);
        let (e_mask, p_mask) = (mask(&e_list), mask(&p_list));
        let (ne, np) = (e_list.len(), p_list.len());
        // How many E vCPUs: all the host's when every core gets a vCPU; otherwise one in four, at least one (the boot
        // CPU of an iPhone is an E core and XNU's AMP scheduler wants both kinds), none when the host has no little
        // cores or there is one vCPU.
        let n_e = if ne == 0 || nvcpu == 1 {
            0
        } else if nvcpu >= ne + np {
            ne
        } else {
            (nvcpu / 4).max(1).min(ne)
        };
        let n_p = nvcpu - n_e;
        let one_to_one = pin && n_e <= ne && n_p <= np;
        let mut v: Vec<Vcpu> = Vec::with_capacity(nvcpu);
        for &cpu in &e_list[..n_e] {
            v.push(Vcpu::from_cpus(t, if one_to_one { 1 << cpu } else { e_mask }, CoreKind::E));
        }
        for &cpu in &p_list[..n_p.min(np)] {
            v.push(Vcpu::from_cpus(t, if one_to_one { 1 << cpu } else { p_mask }, CoreKind::P));
        }
        for _ in np..n_p {
            v.push(Vcpu::from_cpus(t, p_mask, CoreKind::P)); // more P vCPUs than P cores: they share the mask
        }
        if one_to_one {
            for (vcpu, &cpu) in v.iter_mut().zip(e_list.iter().take(n_e).chain(p_list.iter().take(n_p))) {
                vcpu.host_cpu = cpu as u32;
            }
        } else {
            // With a mask per kind, cpu-pin ([`CpuPlan::pin_mask`]) puts the rank-th vCPU of a kind on the rank-th CPU of
            // the mask, in CPU order: on the SM8650 vCPU i on CPU i. Each vCPU is described as that core (its clock
            // table, capacity, caches), not as the fastest core of its kind: before (host-overhead, 2026-11-15) the
            // guest saw six X4s, and a frequency request of a vCPU on an A720 was sized against the X4's table.
            let (mut re, mut rp) = (0, 0);
            for (c, cpu) in present() {
                // A lent core (above) is an E vCPU's core although the topology calls it P.
                let at = if lent[c] || cpu.kind == CoreKind::E {
                    (re < n_e).then(|| {
                        re += 1;
                        re - 1
                    })
                } else {
                    (rp < n_p).then(|| {
                        rp += 1;
                        n_e + rp - 1
                    })
                };
                let Some(at) = at else { continue };
                let mut described = Vcpu::from_cpus(t, 1 << c, v[at].kind);
                described.affinity = AtomicU64::new(v[at].affinity());
                described.host_cpu = c as u32;
                v[at] = described;
            }
        }
        Some(CpuPlan { v, n_p: n_p as u32, n_e: n_e as u32 })
    }

    /// vCPU `index`'s affinity now (0 for a vCPU the plan does not have).
    pub fn affinity(&self, index: usize) -> u64 {
        self.v.get(index).map_or(0, Vcpu::affinity)
    }

    /// This plan's affinities from base's ([`CpuPlan::make`]), each limited to its step's CPUs (base's mask when they
    /// do not meet), with the step's CPUs of the other kind added (step 2: a P vCPU also on the E cores). Each word is
    /// stored atomically: placement may read the plan meanwhile.
    pub fn cap(&self, base: &CpuPlan, t: &Topology, cap: u32) {
        let e = t.mask_of(CoreKind::E);
        for (mine, b) in self.v.iter().zip(&base.v) {
            let want = b.affinity();
            let allowed = t.thermal_mask(b.kind, cap);
            let mut mask = if want & allowed != 0 {
                want & allowed
            } else if allowed != 0 {
                allowed
            } else {
                want
            };
            // The cores a step adds of the other kind (step 2's E cores for a P vCPU) are the vCPU's too: intersected
            // with its own kind's mask they were lost, and the phone's six P vCPUs ran on two cores at "trapping" (0x60,
            // 2026-11-13) where the step means four (0x63).
            let kind = if b.kind == CoreKind::E { e } else { !e };
            if want & allowed != 0 {
                mask |= allowed & !kind;
            }
            mine.affinity.store(mask, Ordering::Release);
        }
    }

    /// cpu-pin: vCPU `vcpu`'s one host CPU, the rank-th CPU of this plan's mask among base's vCPUs of its kind, while
    /// every vCPU of that kind can have a CPU of the mask to itself; otherwise (more vCPUs than CPUs, as at a thermal
    /// step) the whole mask, so that Linux spreads them. 0 for a vCPU the plan does not have.
    pub fn pin_mask(&self, base: &CpuPlan, vcpu: usize) -> u64 {
        let mask = self.affinity(vcpu); // the thermal step may change it
        let Some(me) = base.v.get(vcpu) else { return mask };
        if mask == 0 {
            return mask;
        }
        let same: Vec<usize> = (0..base.nvcpu()).filter(|&j| base.v[j].kind == me.kind).collect();
        let rank = same.iter().filter(|&&j| j < vcpu).count();
        // One core each only while there are enough: with more vCPUs of a kind than cores in the mask (a thermal step),
        // a pinned vCPU waits behind the others on its core while another core of the mask idles, and a vCPU preempted
        // while it holds a kernel lock holds up every vCPU spinning on that lock (phone, 2026-11-13: three P vCPUs on
        // each of two cores at "trapping"; interrupts up to 953 ms, system calls up to 639 ms). Then the whole mask:
        // Linux spreads them.
        if same.len() > mask.count_ones() as usize {
            return mask;
        }
        (0..64).filter(|i| mask >> i & 1 != 0).nth(rank).map_or(mask, |i| 1 << i)
    }

    /// Sets the affinity of Linux thread `tid` (0 = caller) to vCPU `index`'s mask.
    pub fn place(&self, tid: i32, index: usize) -> io::Result<()> {
        let mask = self.v.get(index).ok_or_else(einval)?.affinity(); // cap may change it
        set_affinity(tid, mask)
    }

    /// The uclamp.min value [`CpuPlan::request`] uses (0..1024).
    pub fn util_for(&self, index: usize, khz: u32) -> u32 {
        let Some(v) = self.v.get(index) else { return 0 };
        if khz == 0 {
            return 0;
        }
        if v.max_khz == 0 || khz >= v.max_khz {
            return v.capacity.min(1024);
        }
        (u64::from(v.capacity) * u64::from(khz) / u64::from(v.max_khz)) as u32
    }

    /// Utilisation clamp of `tid`: asks Linux's governor for at least `khz` on the cores of vCPU `index` (0 drops the
    /// request). Linux keeps clock control: this is a hint.
    pub fn request(&self, tid: i32, index: usize, khz: u32) -> io::Result<()> {
        if index >= self.nvcpu() {
            return Err(einval());
        }
        set_uclamp_min(tid, self.util_for(index, khz))
    }
}

/// The utilisation clamp uclamp.min (0..1024) of Linux thread `tid` (0 = caller), its policy and nice kept
/// (sched_setattr with KEEP_POLICY | KEEP_PARAMS, Linux 5.3): the governor's clock and the placement, never a policy
/// change.
pub fn set_uclamp_min(tid: i32, util: u32) -> io::Result<()> {
    let attr = SchedAttr {
        size: std::mem::size_of::<SchedAttr>() as u32,
        sched_policy: 0,
        sched_flags: SCHED_FLAG_KEEP_ALL | SCHED_FLAG_UTIL_CLAMP_MIN,
        sched_nice: 0,
        sched_priority: 0,
        sched_runtime: 0,
        sched_deadline: 0,
        sched_period: 0,
        sched_util_min: util,
        sched_util_max: 0,
    };
    // SAFETY: sched_setattr reads `attr.size` bytes of a valid, initialised struct sched_attr.
    let rc = unsafe { libc::syscall(libc::SYS_sched_setattr, tid, &attr as *const SchedAttr, 0) };
    if rc != 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

/// Sets Linux thread `tid`'s (0 = caller) affinity to the CPUs of `mask`.
pub fn set_affinity(tid: i32, mask: u64) -> io::Result<()> {
    // SAFETY: an all-zero cpu_set_t is the empty set.
    let mut set: libc::cpu_set_t = unsafe { std::mem::zeroed() };
    for i in (0..64).filter(|i| mask >> i & 1 != 0) {
        // SAFETY: i < 64 < CPU_SETSIZE, inside the set.
        unsafe { libc::CPU_SET(i, &mut set) };
    }
    // SAFETY: a valid cpu_set_t of its size.
    if unsafe { libc::sched_setaffinity(tid, std::mem::size_of::<libc::cpu_set_t>(), &set) } != 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

// ---- the virtual cpufreq device -----------------------------------------------------------------
/// The window of one vCPU: xnuhost registers [base, base + nvcpu * WINDOW) and forwards accesses here.
pub const CPUFREQ_WINDOW: u64 = 0x1000;

/// The Linux thread to clamp for vCPU n (None: the calling thread).
pub type TidOf = Box<dyn Fn(usize) -> i32 + Send + Sync>;

/// A VMM that keeps each vCPU thread's clamp itself (xnuhost, 2026-11-23): called with the vCPU and the uclamp.min of
/// its new request ([`CpuPlan::util_for`], 0 = dropped) instead of a clamp set here. The guest writes its own window, so
/// the call comes on that vCPU's thread.
pub type ApplyFn = Box<dyn Fn(usize, u32) + Send + Sync>;

/// The virtual cpufreq device DroidVMCpufreq.c drives: the register layout of Linux's virtual-cpufreq driver, which
/// UniXNU-droidvm's DroidVMCpufreq.c uses (https://github.com/Droid-VM/UniXNU-droidvm f9121cd,
/// package/DroidVMCpufreq.c): per vCPU window 0x00 current kHz, 0x04 set kHz (write), 0x08 table length, 0x0c table
/// select, 0x10 table read, 0x14 performance domain.
pub struct Cpufreq {
    plan: CpuPlan,
    tid_of: Option<TidOf>,
    apply: Option<ApplyFn>,
    select: [AtomicU32; MAX_VCPUS],
    requested: [AtomicU32; MAX_VCPUS],
}

impl std::fmt::Debug for Cpufreq {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Cpufreq").field("plan", &self.plan).finish_non_exhaustive()
    }
}

impl Cpufreq {
    /// `tid_of(vcpu)` gives the Linux thread to clamp when the guest writes (None: the calling thread).
    pub fn new(plan: &CpuPlan, tid_of: Option<TidOf>) -> Cpufreq {
        Cpufreq { plan: plan.clone(), tid_of, apply: None, select: Default::default(), requested: Default::default() }
    }

    /// A device that records each request and hands it to `apply` (the VMM keeps the clamp: [`ApplyFn`]).
    pub fn with_apply(plan: &CpuPlan, apply: ApplyFn) -> Cpufreq {
        Cpufreq { apply: Some(apply), ..Cpufreq::new(plan, None) }
    }

    fn at(&self, offset: u64) -> Option<(usize, u64)> {
        let cpu = (offset / CPUFREQ_WINDOW) as usize;
        (cpu < self.plan.nvcpu()).then_some((cpu, offset % CPUFREQ_WINDOW))
    }

    pub fn read(&self, offset: u64, _size: u32) -> u64 {
        let Some((cpu, reg)) = self.at(offset) else { return 0 };
        let v = &self.plan.v[cpu];
        let select = self.select[cpu].load(Ordering::Relaxed) as usize;
        u64::from(match reg {
            0x00 => match self.requested[cpu].load(Ordering::Acquire) {
                0 => v.max_khz,
                khz => khz,
            },
            0x08 => v.freq_khz.len() as u32,
            0x0c => select as u32,
            0x10 => v.freq_khz.get(select).copied().unwrap_or(0),
            0x14 => v.cluster_id,
            _ => 0,
        })
    }

    pub fn write(&self, offset: u64, _size: u32, value: u64) {
        let Some((cpu, reg)) = self.at(offset) else { return };
        match reg {
            0x0c => self.select[cpu].store(value as u32, Ordering::Relaxed),
            0x04 => {
                let v = &self.plan.v[cpu];
                let mut khz = value as u32;
                // The lowest table entry is "no request": the governor is left alone.
                if khz <= v.min_khz || v.freq_khz.first().is_some_and(|&low| khz <= low) {
                    khz = 0;
                }
                if self.requested[cpu].swap(khz, Ordering::AcqRel) == khz {
                    return;
                }
                if let Some(apply) = self.apply.as_ref() {
                    apply(cpu, self.plan.util_for(cpu, khz));
                    return;
                }
                let tid = self.tid_of.as_ref().map_or(0, |f| f(cpu));
                if let Err(e) = self.plan.request(tid, cpu, khz) {
                    if e.raw_os_error() != Some(libc::ENOSYS) {
                        crate::br_debug!("cpufreq: vCPU {cpu}: uclamp for {khz} kHz: {}", xh_core::sys::error_text(&e));
                    }
                }
            }
            _ => {}
        }
    }

    /// The last kHz asked of vCPU `vcpu`, 0 = none.
    pub fn requested(&self, vcpu: usize) -> u32 {
        self.requested.get(vcpu).filter(|_| vcpu < self.plan.nvcpu()).map_or(0, |r| r.load(Ordering::Acquire))
    }

    /// The plan the device describes (a copy of the one it was made with).
    pub fn plan(&self) -> &CpuPlan {
        &self.plan
    }
}

/// The device on xnuhost's MMIO bus: `mmio::bus().add(base, nvcpu * CPUFREQ_WINDOW, "cpufreq", Arc::new(cpufreq))`.
impl xh_core::mmio::MmioDevice for Cpufreq {
    fn mmio_read(&self, offset: u64, size: u32) -> u64 {
        self.read(offset, size)
    }
    fn mmio_write(&self, offset: u64, size: u32, value: u64) {
        self.write(offset, size, value)
    }
}
