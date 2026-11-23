// SPDX-License-Identifier: MIT
//! Port of gic.c. The interrupt controller of xnuhost's board: a GICv3 with the register set ArmCpu
//! (https://github.com/UniXNU/ArmCpu, commit 1604083, ArmCpu.cpp) programs and reads: distributor and redistributor
//! frames by MMIO, the CPU interface by ICC_* system registers. A hosted kernel can use the irq_* host calls instead;
//! both drive this state. Priorities are not modelled beyond the priority mask being zero or not: the board's
//! interrupts are few and XNU runs them at one level.
//!
//! The state is one lock. A CPU's line is told to the CPU layer (cpu.rs) at each change, with the lock held; a CPU
//! that lowers its own line recomputes it from the state the caller holds (the C's recursive lock is not needed).

use crate::cpu::{self, MAX_CPUS};
use std::sync::{Arc, Mutex, MutexGuard};
use xh_core::insn::{sysreg, SysInsn};
use xh_core::mmio::MmioDevice;
use xnuhost_abi::{XH_INTID_NONE, XH_INTID_UNTAKEN, XH_INTID_VTIMER, XH_IPI_SPI_COUNT, XH_IPI_SPI_FIRST, XH_SGI_COUNT};

/// INTIDs 32..1019. The board's devices use the low ones (32 + n); ArmCpu's software IPIs (boot-arg
/// armcpu-spi-ipi=1) are SPIs 1008 + core, pended by a GICD_ISPENDR write.
pub const SPIS: usize = 992;
pub const IPI_SPI: u32 = 1008;
const NIDS: usize = 32 + SPIS;
const WORDS: usize = NIDS / 32;

#[derive(Clone, Copy)]
struct GicCpu {
    /// SGIs 0-15 and PPIs 16-31
    enabled: u32,
    pending: u32,
    level: u32,
    active: u32,
    /// ICC_IGRPEN1_EL1
    group_enable: u32,
    pmr: u32,
    waker: u32,
    priority: [u8; 32],
}

const CPU_RESET: GicCpu =
    GicCpu { enabled: 0, pending: 0, level: 0, active: 0, group_enable: 0, pmr: 0xff, waker: 6, priority: [0; 32] };

struct Gic {
    ncpu: usize,
    ctlr: u32,
    /// word 0 unused
    enabled: [u32; WORDS],
    pending: [u32; WORDS],
    level: [u32; WORDS],
    active: [u32; WORDS],
    priority: [u8; NIDS],
    /// CPU index, from GICD_IROUTER
    target: [u8; NIDS],
    icfgr: [u32; NIDS / 16],
    cpu: [GicCpu; MAX_CPUS],
    dist_base: u64,
    redist_base: u64,
}

static GIC: Mutex<Gic> = Mutex::new(Gic {
    ncpu: 0,
    ctlr: 0,
    enabled: [0; WORDS],
    pending: [0; WORDS],
    level: [0; WORDS],
    active: [0; WORDS],
    priority: [0; NIDS],
    target: [0; NIDS],
    icfgr: [0; NIDS / 16],
    cpu: [CPU_RESET; MAX_CPUS],
    dist_base: 0,
    redist_base: 0,
});

fn gic() -> MutexGuard<'static, Gic> {
    GIC.lock().unwrap_or_else(|e| e.into_inner())
}

fn bit(id: usize) -> u32 {
    1u32 << (id % 32)
}

/// Which bank a distributor offset in 0x100..0x400 names: set-enable/clear-enable, set-pending/clear-pending,
/// set-active/clear-active (bit 7 of the offset: clear).
#[derive(Clone, Copy, PartialEq, Eq)]
enum Bank {
    Enabled,
    Pending,
    Active,
}

fn bank(offset: u64) -> Option<(Bank, usize)> {
    let word = ((offset & 0x7f) / 4) as usize;
    if word == 0 || word >= WORDS {
        return None;
    }
    let b = match offset & !0x7f {
        0x100 | 0x180 => Bank::Enabled,
        0x200 | 0x280 => Bank::Pending,
        0x300 | 0x380 => Bank::Active,
        _ => return None,
    };
    Some((b, word))
}

impl Gic {
    /// Highest-priority-by-number pending and enabled interrupt of a CPU, or NONE.
    fn best(&self, c: usize) -> u32 {
        let p = &self.cpu[c];
        if p.group_enable == 0 || p.pmr == 0 {
            return XH_INTID_NONE;
        }
        let level = p.level | if cpu::timer_due(c) { 1 << XH_INTID_VTIMER } else { 0 }; // the timer line is a level
        let local = (p.pending | level) & p.enabled & !p.active;
        if local != 0 {
            return local.trailing_zeros();
        }
        if self.ctlr & 3 == 0 {
            return XH_INTID_NONE;
        }
        for w in 1..WORDS {
            let mut bits = (self.pending[w] | self.level[w]) & self.enabled[w] & !self.active[w];
            if w == WORDS - 1 {
                bits &= 0x0fff_ffff; // 1020-1023 are not interrupts
            }
            while bits != 0 {
                let id = w * 32 + bits.trailing_zeros() as usize;
                if self.target[id] as usize == c {
                    return id as u32;
                }
                bits &= bits - 1;
            }
        }
        XH_INTID_NONE
    }

    /// Tells one CPU the state of its line: a change that can move only that CPU's line (its SGI, PPI, the ack or EOI
    /// of one of its interrupts, an SPI routed to it). Every ack and EOI used to compute and tell all eight lines
    /// under the one lock (host-overhead, 2026-11-15): eight best() scans per interrupt step, and a signal to each CPU
    /// with a line up.
    fn update_cpu(&self, c: usize) {
        if c < self.ncpu {
            let up = self.best(c) != XH_INTID_NONE;
            cpu::line_with(c, up, || self.best(c) != XH_INTID_NONE);
        }
    }

    /// Tells every CPU the state of its line.
    fn update(&self) {
        for c in 0..self.ncpu {
            self.update_cpu(c);
        }
    }

    fn ack(&mut self, c: usize) -> u32 {
        let id = self.best(c);
        let i = id as usize;
        if id < 32 {
            self.cpu[c].pending &= !bit(i);
            self.cpu[c].active |= bit(i);
        } else if id != XH_INTID_NONE {
            self.pending[i / 32] &= !bit(i);
            self.active[i / 32] |= bit(i);
        }
        if id != XH_INTID_NONE {
            let v = cpu::vcpu(c);
            v.stat.acks.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            if id == XH_INTID_VTIMER {
                v.stat.timer.add(1);
            } else if !(XH_SGI_COUNT..IPI_SPI).contains(&id) {
                v.stat.sgi.add(1);
            }
        }
        self.update_cpu(c);
        id
    }

    fn eoi(&mut self, c: usize, intid: u32) {
        let i = intid as usize;
        if i < 32 {
            self.cpu[c].active &= !bit(i);
        } else if i < NIDS {
            self.active[i / 32] &= !bit(i);
        }
        self.update_cpu(c); // a level line still high is pending again
        if (32..NIDS).contains(&i) && self.target[i] as usize != c {
            self.update_cpu(self.target[i] as usize);
        }
    }

    fn send_sgi(&mut self, c: usize, sgi: u32) {
        if c < self.ncpu && sgi < 16 {
            self.cpu[c].pending |= 1 << sgi;
            self.update_cpu(c);
        }
    }
}

pub fn init(ncpu: usize, dist_base: u64, redist_base: u64) {
    let mut g = gic();
    g.enabled = [0; WORDS];
    g.pending = [0; WORDS];
    g.level = [0; WORDS];
    g.active = [0; WORDS];
    g.target = [0; NIDS];
    g.cpu = [CPU_RESET; MAX_CPUS];
    g.ncpu = ncpu;
    g.ctlr = 0;
    g.dist_base = dist_base;
    g.redist_base = redist_base;
}

/// Whether a CPU has an interrupt to take.
pub fn pending(c: usize) -> bool {
    c < MAX_CPUS && gic().best(c) != XH_INTID_NONE
}

/// A device's line (the devices' hook, xh_core::hooks::set_spi).
pub fn set_spi(intid: u32, level: bool) {
    let i = intid as usize;
    if !(32..NIDS).contains(&i) {
        return;
    }
    let mut g = gic();
    if level {
        g.level[i / 32] |= bit(i);
    } else {
        g.level[i / 32] &= !bit(i);
    }
    let target = g.target[i] as usize;
    g.update_cpu(target);
}

pub fn set_ppi(c: usize, intid: u32, level: bool) {
    let mut g = gic();
    if c >= g.ncpu || !(16..32).contains(&intid) {
        return;
    }
    if level {
        g.cpu[c].level |= 1 << intid;
    } else {
        g.cpu[c].level &= !(1 << intid);
    }
    g.update_cpu(c);
}

pub fn send_sgi(c: usize, sgi: u32) {
    gic().send_sgi(c, sgi)
}

/// The highest pending INTID of a CPU, now active; XH_INTID_NONE.
pub fn ack(c: usize) -> u32 {
    if c >= MAX_CPUS {
        return XH_INTID_NONE;
    }
    gic().ack(c)
}

/// irq_take (hostcall.h): [`ack`] for the timer, an SGI or a software-IPI SPI; any other INTID stays pending and is
/// returned with XH_INTID_UNTAKEN, for the kernel's controller driver to take by ICC_IAR1_EL1 (fix-mach-rpc).
pub fn take(c: usize) -> u32 {
    if c >= MAX_CPUS {
        return XH_INTID_NONE;
    }
    let mut g = gic();
    let id = g.best(c);
    let mine = id == XH_INTID_VTIMER
        || id < XH_SGI_COUNT
        || (XH_IPI_SPI_FIRST..XH_IPI_SPI_FIRST + XH_IPI_SPI_COUNT).contains(&id);
    if id != XH_INTID_NONE && !mine {
        return id | XH_INTID_UNTAKEN;
    }
    g.ack(c) // the same choice, now acknowledged
}

pub fn eoi(c: usize, intid: u32) {
    if c < MAX_CPUS {
        gic().eoi(c, intid)
    }
}

pub fn enable(c: usize, intid: u32, on: bool) {
    if c >= MAX_CPUS {
        return;
    }
    let mut g = gic();
    let i = intid as usize;
    if i < 32 {
        if on {
            g.cpu[c].enabled |= bit(i);
        } else {
            g.cpu[c].enabled &= !bit(i);
        }
    } else if i < NIDS {
        if on {
            g.enabled[i / 32] |= bit(i);
            g.target[i] = c as u8;
        } else {
            g.enabled[i / 32] &= !bit(i);
        }
    }
    if on {
        // the host-call path needs no register setup
        g.ctlr |= 3;
        g.cpu[c].group_enable = 1;
    }
    g.update();
}

// ---- CPU interface -------------------------------------------------------------------------------------------------
const ICC_IAR1_EL1: u32 = sysreg(3, 0, 12, 12, 0);
const ICC_EOIR1_EL1: u32 = sysreg(3, 0, 12, 12, 1);
const ICC_HPPIR1_EL1: u32 = sysreg(3, 0, 12, 12, 2);
const ICC_IAR0_EL1: u32 = sysreg(3, 0, 12, 8, 0);
const ICC_HPPIR0_EL1: u32 = sysreg(3, 0, 12, 8, 2);
const ICC_SRE_EL1: u32 = sysreg(3, 0, 12, 12, 5);
const ICC_IGRPEN1_EL1: u32 = sysreg(3, 0, 12, 12, 7);
const ICC_PMR_EL1: u32 = sysreg(3, 0, 4, 6, 0);
const ICC_SGI1R_EL1: u32 = sysreg(3, 0, 12, 11, 5);
const ICC_RPR_EL1: u32 = sysreg(3, 0, 12, 11, 3);
/// ICC_BPR1_EL1, ICC_CTLR_EL1, ICC_IGRPEN0_EL1, ICC_EOIR0_EL1, ICC_BPR0_EL1, ICC_DIR_EL1, ICC_RPR_EL1, ICC_SGI0R_EL1:
/// read as 0 (RPR: idle priority), writes dropped.
const ICC_PLAIN: [u32; 8] = [
    sysreg(3, 0, 12, 12, 3),
    sysreg(3, 0, 12, 12, 4),
    sysreg(3, 0, 12, 12, 6),
    sysreg(3, 0, 12, 8, 1),
    sysreg(3, 0, 12, 8, 3),
    sysreg(3, 0, 12, 11, 1),
    ICC_RPR_EL1,
    sysreg(3, 0, 12, 11, 7),
];

/// ICC_* system registers of CPU `c`. Returns false if the register is not the GIC's.
pub fn sysreg_access(c: usize, s: &SysInsn, value: &mut u64) -> bool {
    if c >= MAX_CPUS {
        return false;
    }
    let key = s.key();
    match key {
        ICC_IAR1_EL1 => {
            if s.read {
                *value = u64::from(ack(c));
            }
        }
        ICC_EOIR1_EL1 => {
            if !s.read {
                eoi(c, (*value & 0xff_ffff) as u32);
            }
        }
        ICC_HPPIR1_EL1 => {
            let g = gic();
            if s.read {
                *value = u64::from(g.best(c));
            }
        }
        ICC_IAR0_EL1 | ICC_HPPIR0_EL1 => {
            // no group 0 interrupts
            if s.read {
                *value = u64::from(XH_INTID_NONE);
            }
        }
        ICC_SRE_EL1 => {
            // system registers on, bypass off
            if s.read {
                *value = 7;
            }
        }
        ICC_IGRPEN1_EL1 => {
            let mut g = gic();
            if s.read {
                *value = u64::from(g.cpu[c].group_enable);
            } else {
                g.cpu[c].group_enable = (*value & 1) as u32;
                g.update();
            }
        }
        ICC_PMR_EL1 => {
            let mut g = gic();
            if s.read {
                *value = u64::from(g.cpu[c].pmr);
            } else {
                g.cpu[c].pmr = (*value & 0xff) as u32;
                g.update();
            }
        }
        ICC_SGI1R_EL1 => {
            if !s.read {
                let sgi = ((*value >> 24) & 15) as u32;
                let mut g = gic();
                let n = g.ncpu;
                if *value & (1 << 40) != 0 {
                    // IRM: all but self
                    for t in (0..n).filter(|&t| t != c) {
                        g.send_sgi(t, sgi);
                    }
                } else if (*value >> 16) & 0xff == 0 {
                    // Aff1 0: the board's CPUs are Aff0 0..7
                    for t in (0..n.min(16)).filter(|&t| *value & (1 << t) != 0) {
                        g.send_sgi(t, sgi);
                    }
                }
            }
        }
        k if ICC_PLAIN.contains(&k) => {
            if s.read {
                *value = if k == ICC_RPR_EL1 { 0xff } else { 0 };
            }
        }
        _ => return false,
    }
    true
}

// ---- distributor ---------------------------------------------------------------------------------------------------
struct Distributor;

impl MmioDevice for Distributor {
    fn mmio_read(&self, off: u64, size: u32) -> u64 {
        let g = gic();
        let n = NIDS as u64;
        match off {
            0x0 => u64::from(g.ctlr),
            0x4 => (WORDS - 1) as u64,  // GICD_TYPER: ITLinesNumber
            0x8 => 0x43b,               // GICD_IIDR: implementer Arm
            0xffe8 => 0x3b,             // GICD_PIDR2: architecture revision 3
            0x80..0x100 => 0xffff_ffff, // GICD_IGROUPR: all group 1
            0x100..0x400 => match bank(off) {
                Some((Bank::Enabled, w)) => u64::from(g.enabled[w]),
                Some((Bank::Pending, w)) => u64::from(g.pending[w] | g.level[w]),
                Some((Bank::Active, w)) => u64::from(g.active[w]),
                None => 0,
            },
            o if (0x400..0x400 + n).contains(&o) => {
                let at = (o - 0x400) as usize;
                (0..size as usize)
                    .take_while(|i| at + i < NIDS)
                    .fold(0u64, |v, i| v | u64::from(g.priority[at + i]) << (i * 8))
            }
            o if (0xc00..0xc00 + n / 4).contains(&o) => u64::from(g.icfgr[((o - 0xc00) / 4) as usize]),
            o if (0x6000..0x6000 + 8 * n).contains(&o) => {
                if o & 4 != 0 {
                    0
                } else {
                    u64::from(g.target[((o - 0x6000) / 8) as usize])
                }
            }
            _ => 0,
        }
    }

    fn mmio_write(&self, off: u64, size: u32, value: u64) {
        let mut g = gic();
        let n = NIDS as u64;
        match off {
            0x0 => g.ctlr = value as u32 & 0x37,
            0x100..0x400 => {
                if let Some((b, w)) = bank(off) {
                    let words = match b {
                        Bank::Enabled => &mut g.enabled,
                        Bank::Pending => &mut g.pending,
                        Bank::Active => &mut g.active,
                    };
                    if off & 0x80 != 0 {
                        words[w] &= !(value as u32);
                    } else {
                        words[w] |= value as u32;
                    }
                }
            }
            o if (0x400..0x400 + n).contains(&o) => {
                let at = (o - 0x400) as usize;
                for i in (0..size as usize).take_while(|i| at + i < NIDS) {
                    g.priority[at + i] = (value >> (i * 8)) as u8;
                }
            }
            o if (0xc00..0xc00 + n / 4).contains(&o) => g.icfgr[((o - 0xc00) / 4) as usize] = value as u32,
            o if (0x6000..0x6000 + 8 * n).contains(&o) && o & 4 == 0 => {
                let c = (value & 0xff) as usize; // Aff0
                let ncpu = g.ncpu;
                g.target[((o - 0x6000) / 8) as usize] = if c < ncpu { c as u8 } else { 0 };
            }
            _ => {}
        }
        g.update();
    }
}

// ---- redistributors: RD_base then SGI_base, 64 KiB each, per CPU ---------------------------------------------------
struct Redistributor;

impl MmioDevice for Redistributor {
    fn mmio_read(&self, off: u64, size: u32) -> u64 {
        let c = (off / 0x20000) as usize;
        let r = off % 0x20000;
        let g = gic();
        if c >= g.ncpu {
            return 0;
        }
        let p = &g.cpu[c];
        let mut v = match r {
            0x8 => (c as u64) << 8 | if c == g.ncpu - 1 { 0x10 } else { 0 }, // GICR_TYPER low: number, Last
            0xc => c as u64,                                                 // GICR_TYPER high: affinity
            0x14 => u64::from(p.waker),
            0xffe8 => 0x3b,
            0x10100 | 0x10180 => u64::from(p.enabled),
            0x10200 | 0x10280 => u64::from(p.pending | p.level),
            0x10300 | 0x10380 => u64::from(p.active),
            0x10080 => 0xffff_ffff, // GICR_IGROUPR0: all group 1
            0x10400..0x10420 => {
                let at = (r - 0x10400) as usize;
                let mut bytes = [0u8; 8];
                let n = (size as usize).min(8).min(32 - at);
                bytes[..n].copy_from_slice(&p.priority[at..at + n]);
                u64::from_le_bytes(bytes)
            }
            _ => 0,
        };
        if r == 0x8 && size == 8 {
            v |= (c as u64) << 32;
        }
        v
    }

    fn mmio_write(&self, off: u64, size: u32, value: u64) {
        let c = (off / 0x20000) as usize;
        let r = off % 0x20000;
        let mut g = gic();
        if c >= g.ncpu {
            return;
        }
        let p = &mut g.cpu[c];
        let v = value as u32;
        match r {
            0x14 => p.waker = if value & 2 != 0 { 6 } else { 0 }, // ChildrenAsleep follows ProcessorSleep
            0x10100 => p.enabled |= v,
            0x10180 => p.enabled &= !v,
            0x10200 => p.pending |= v,
            0x10280 => p.pending &= !v,
            0x10300 => p.active |= v,
            0x10380 => p.active &= !v,
            0x10400..0x10420 => {
                let at = (r - 0x10400) as usize;
                let n = (size as usize).min(8).min(32 - at);
                p.priority[at..at + n].copy_from_slice(&value.to_le_bytes()[..n]);
            }
            _ => {}
        }
        g.update();
    }
}

/// Registers the MMIO frames on `bus` (the machine's: xh_core::mmio::bus()).
pub fn map_on(bus: &xh_core::mmio::Bus) -> Result<(), xh_core::mmio::AddError> {
    let (dist, redist, n) = {
        let g = gic();
        (g.dist_base, g.redist_base, g.ncpu as u64)
    };
    bus.add(dist, 0x10000, "gicd", Arc::new(Distributor))?;
    bus.add(redist, 0x20000 * n, "gicr", Arc::new(Redistributor))
}

/// Registers the MMIO frames on the machine's bus, and the devices' interrupt hook.
pub fn map() {
    if let Err(e) = map_on(xh_core::mmio::bus()) {
        xh_core::loge!("gic: the controller's frames: {e}");
    }
    xh_core::hooks::install_spi(set_spi);
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::cpu;
    use std::sync::atomic::Ordering::Relaxed;
    use xh_core::mmio::Bus;

    pub(crate) fn sys(read: bool, op0: u32, op1: u32, crn: u32, crm: u32, op2: u32) -> SysInsn {
        SysInsn { read, op0, op1, crn, crm, op2, rt: 0 }
    }

    fn line(c: usize) -> bool {
        cpu::vcpu(c).cpu.irq_pending.load(Relaxed) != 0
    }

    /// test_gic: the register path as ArmCpu programs it, SGIs, ArmCpu's software IPI SPIs, irq_take, the timer's
    /// level line, the priority mask.
    pub(crate) fn run_gic() {
        cpu::init(2).unwrap();
        init(2, 0x800_0000, 0x80a_0000);
        let bus = Bus::new();
        map_on(&bus).unwrap();
        assert_eq!(bus.read(0x800_0000 + 0xffe8, 4) >> 4 & 15, 3); // GICD_PIDR2, as ArmCpu checks
        assert_eq!(bus.read(0x80a_0000 + 0x20000 + 0x8, 8) >> 32, 1); // CPU 1's GICR_TYPER affinity
        assert!(bus.read(0x80a_0000 + 0x20000 + 0x8, 4) & 0x10 != 0); // and it is the last frame
                                                                      // the register path, as ArmCpu programs it: distributor on, SPI 40 enabled and routed to CPU 1
        bus.write(0x800_0000, 4, 0x12);
        bus.write(0x800_0000 + 0x100 + 4, 4, 1 << 8);
        bus.write(0x800_0000 + 0x6000 + 8 * 40, 8, 1);
        let igrpen = sys(false, 3, 0, 12, 12, 7);
        let iar = sys(true, 3, 0, 12, 12, 0);
        let eoir = sys(false, 3, 0, 12, 12, 1);
        let sgi1r = sys(false, 3, 0, 12, 11, 5);
        let mut v = 1u64;
        assert!(sysreg_access(1, &igrpen, &mut v));
        set_spi(40, true);
        assert!(line(1) && !line(0) && pending(1));
        assert!(sysreg_access(1, &iar, &mut v) && v == 40);
        assert!(!line(1)); // active: not offered again
        assert_eq!(ack(1), XH_INTID_NONE);
        v = 40;
        assert!(sysreg_access(1, &eoir, &mut v) && line(1)); // level still high: pending again
        assert_eq!(ack(1), 40);
        set_spi(40, false);
        eoi(1, 40);
        assert!(!line(1) && ack(1) == XH_INTID_NONE);
        // SGIs: by register from CPU 1 to CPU 0, and by the host-call path
        enable(0, 3, true);
        v = 3 << 24 | 1;
        assert!(sysreg_access(1, &sgi1r, &mut v) && line(0) && ack(0) == 3);
        eoi(0, 3);
        assert!(!line(0));
        // ArmCpu's software IPI of core 1 (armcpu-spi-ipi=1): SPI 1009, routed, enabled, pended by a GICD_ISPENDR write
        let (word, b) = (1009u64 / 32 * 4, 1u64 << (1009 % 32));
        bus.write(0x800_0000 + 0x6000 + 8 * 1009, 8, 1);
        bus.write(0x800_0000 + 0x100 + word, 4, b);
        assert!(bus.read(0x800_0000 + 0x80 + word, 4) & b != 0); // group 1 reads back
        bus.write(0x800_0000 + 0x200 + word, 4, b);
        assert!(pending(1) && !pending(0) && ack(1) == 1009);
        eoi(1, 1009);
        assert_eq!(ack(1), XH_INTID_NONE);
        // irq_take (fix-mach-rpc): the timer, SGIs and IPI SPIs are taken; a device's SPI is left pending for the
        // driver's IAR
        assert_eq!(take(1), XH_INTID_NONE);
        bus.write(0x800_0000 + 0x200 + word, 4, b);
        assert!(take(1) == 1009 && take(1) == XH_INTID_NONE); // active: not offered again
        eoi(1, 1009);
        set_spi(40, true);
        assert!(take(1) == (40 | XH_INTID_UNTAKEN) && line(1) && pending(1));
        assert_eq!(take(1), 40 | XH_INTID_UNTAKEN); // still there, not active
        enable(1, 5, true);
        send_sgi(1, 5);
        assert_eq!(take(1), 5); // the SGI comes first (lower INTID)
        eoi(1, 5);
        assert!(sysreg_access(1, &iar, &mut v) && v == 40); // the device's, by the driver's path
        set_spi(40, false);
        eoi(1, 40);
        assert!(take(1) == XH_INTID_NONE && !line(1));
        // the timer's line is a level read from the CPU layer
        enable(0, XH_INTID_VTIMER, true);
        cpu::timer_set(cpu::vcpu(0), 1); // long past
        assert!(pending(0) && ack(0) == XH_INTID_VTIMER);
        cpu::timer_set(cpu::vcpu(0), u64::MAX);
        eoi(0, XH_INTID_VTIMER);
        assert!(!pending(0));
        // priority mask 0 holds everything back
        let pmr = sys(false, 3, 0, 4, 6, 0);
        v = 0;
        sysreg_access(0, &pmr, &mut v);
        send_sgi(0, 3);
        assert!(!pending(0));
        v = 0xff;
        sysreg_access(0, &pmr, &mut v);
        assert_eq!(ack(0), 3);
        eoi(0, 3);
    }

    #[test]
    fn gic_and_sysreg() {
        // The C ran test_gic then test_sysreg on the same machine (sysreg's PSCI test starts vCPU 1 of these two).
        let _m = cpu::tests::machine();
        run_gic();
        crate::sysreg::tests::run_sysreg();
    }
}
