// SPDX-License-Identifier: MIT
//! Port of sysreg.c. What the kernel does that is undefined at EL0, emulated from SIGILL: EL1 system registers, the
//! virtual timer's control registers, PSTATE.DAIF, PSCI calls, WFI and TLB maintenance. A hosted kernel replaces the
//! hot ones with host calls; unchanged code (Apple's kexts, ArmCpu, ArmDTPlatform) still works through this path,
//! slowly.

use crate::cpu::{self, Vcpu};
use crate::{gic, hostcalls, mem, session};
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering::Relaxed};
use xh_core::insn::{decode_sys, sysreg};
use xh_core::regs::Regs;
use xnuhost_abi::*;

const SHADOWS: usize = 64;

/// A vCPU's trapped registers: the timer's, DAIF, and a shadow of other EL1 registers the kernel writes and reads
/// back. Written by the vCPU's own thread only (atomics: plain loads and stores).
#[derive(Debug)]
pub struct SysregState {
    pub cntv_ctl: AtomicU64,
    pub cntv_cval: AtomicU64,
    pub daif: AtomicU64,
    shadow: [AtomicU64; SHADOWS],
    shadow_key: [AtomicU32; SHADOWS],
    nshadow: AtomicU32,
}

impl Default for SysregState {
    fn default() -> Self {
        Self::new()
    }
}

impl SysregState {
    pub const fn new() -> SysregState {
        SysregState {
            cntv_ctl: AtomicU64::new(0),
            cntv_cval: AtomicU64::new(0),
            daif: AtomicU64::new(0),
            shadow: [const { AtomicU64::new(0) }; SHADOWS],
            shadow_key: [const { AtomicU32::new(0) }; SHADOWS],
            nshadow: AtomicU32::new(0),
        }
    }

    /// The timer_set host call's deadline in the register view (CNTV_CVAL_EL0, ENABLE): one timer, two ways in.
    pub fn set_timer(&self, deadline: u64) {
        self.cntv_cval.store(deadline, Relaxed);
        if deadline != u64::MAX {
            self.cntv_ctl.store(1, Relaxed);
        }
    }

    /// The shadow of register `key`, made (reading 0) on first use; None when the 64 slots are taken.
    fn shadow(&self, key: u32) -> Option<&AtomicU64> {
        let n = self.nshadow.load(Relaxed) as usize;
        if let Some(i) = (0..n).find(|&i| self.shadow_key[i].load(Relaxed) == key) {
            return Some(&self.shadow[i]);
        }
        if n == SHADOWS {
            return None;
        }
        self.shadow_key[n].store(key, Relaxed);
        self.shadow[n].store(0, Relaxed);
        self.nshadow.store(n as u32 + 1, Relaxed);
        Some(&self.shadow[n])
    }
}

fn timer_update(v: &Vcpu) {
    // CNTV_CTL_EL0: bit 0 ENABLE, bit 1 IMASK, bit 2 ISTATUS. The line (INTID 27) is a level: high while the timer
    // is enabled, unmasked and due; the GIC reads it from the CPU layer.
    let ctl = v.sys.cntv_ctl.load(Relaxed);
    let on = ctl & 1 != 0 && ctl & 2 == 0;
    cpu::timer_set(v, if on { v.sys.cntv_cval.load(Relaxed) } else { u64::MAX });
}

fn set_daif(v: &Vcpu, daif: u64) {
    v.sys.daif.store(daif & 0x3c0, Relaxed);
    v.cpu.irq_masked.store(((daif >> 7) & 1) as u32, Relaxed);
}

fn daif(v: &Vcpu) -> u64 {
    v.sys.daif.load(Relaxed) | u64::from(v.cpu.irq_masked.load(Relaxed)) << 7
}

/// CPU_OFF, SYSTEM_OFF, SYSTEM_RESET: the vCPU idles for good.
fn idle_forever(v: &Vcpu) -> ! {
    #[cfg(test)]
    panic!("idle_forever in a test (vCPU {})", v.index());
    #[cfg(not(test))]
    loop {
        cpu::idle(v);
    }
}

fn psci(v: &Vcpu, r: &Regs) -> i64 {
    let (fn_id, a1) = (r.x[0] as u32, r.x[1] as u32);
    match fn_id {
        XH_PSCI_VERSION => 0x10000, // 1.0
        0x8400_000a => {
            // PSCI_FEATURES
            if [XH_PSCI_CPU_ON, XH_PSCI_SYSTEM_OFF, XH_PSCI_SYSTEM_RESET, XH_PSCI_CPU_OFF].contains(&a1) {
                0
            } else {
                -1
            }
        }
        XH_PSCI_CPU_ON => {
            let index = (r.x[1] & 0xff) as usize;
            if r.x[1] & 0xff_00ff_ff00 != 0 || index >= cpu::ncpu() {
                return -2; // INVALID_PARAMETERS
            }
            let Some(entry) = mem::ram(r.x[2], 4) else { return -9 }; // a physical entry address; INVALID_ADDRESS
                                                                      // Entered like vCPU 0: x0 = the context id, x1 = the vector, x2 = its struct xh_cpu.
            let vector = hostcalls::vector().map_or(0, |v| v as *const XhVector as u64);
            match cpu::start(index, entry.as_ptr() as u64, r.x[3], vector, cpu::vcpu(index).cpu.as_ptr() as u64) {
                Ok(()) => 0,
                Err(cpu::StartError::Already) => -4, // ALREADY_ON
                Err(cpu::StartError::Invalid) => -6, // INTERNAL_FAILURE
            }
        }
        XH_PSCI_CPU_OFF => {
            v.cpu.irq_masked.store(1, Relaxed);
            idle_forever(v)
        }
        XH_PSCI_SYSTEM_OFF => {
            session::stop(XH_HALT_OFF, 0, "PSCI SYSTEM_OFF");
            idle_forever(v)
        }
        XH_PSCI_SYSTEM_RESET => {
            session::stop(XH_HALT_RESTART, 0, "PSCI SYSTEM_RESET");
            idle_forever(v)
        }
        _ => -1, // NOT_SUPPORTED
    }
}

/// config.setway_skip (the idle set/way flush, docs/booter.md 2.5): a trapped dc set/way ends the guest's set/way
/// loop of its level at once.
pub static SETWAY_SKIP: AtomicBool = AtomicBool::new(false);

// Which instructions the kernel traps on, and how often (each is a signal round trip on the vCPU's thread): the
// table stats_log prints at the stop and on SIGQUIT. An idle guest that keeps its vCPUs busy shows here.
const INSN_SLOTS: usize = 256;
struct Seen {
    insn: AtomicU32,
    count: AtomicU64,
    pc: AtomicU64,
}
static INSN_SEEN: [Seen; INSN_SLOTS] =
    [const { Seen { insn: AtomicU32::new(0), count: AtomicU64::new(0), pc: AtomicU64::new(0) } }; INSN_SLOTS];
static INSN_OTHER: AtomicU64 = AtomicU64::new(0);

fn insn_count(insn: u32, pc: u64) {
    let h = (insn.wrapping_mul(2_654_435_761) >> 24) as usize;
    for k in 0..8 {
        let slot = &INSN_SEEN[(h + k) % INSN_SLOTS];
        let mut key = slot.insn.load(Relaxed);
        if key == 0 {
            match slot.insn.compare_exchange(0, insn, Relaxed, Relaxed) {
                Ok(_) => {
                    slot.pc.store(pc, Relaxed);
                    key = insn;
                }
                Err(now) => key = now,
            }
        }
        if key == insn {
            slot.count.fetch_add(1, Relaxed);
            return;
        }
    }
    INSN_OTHER.fetch_add(1, Relaxed);
}

/// The most frequent trapped instructions (`xh_sysreg_stats_log`): at the stop and on SIGQUIT.
pub fn stats_log() {
    let mut order: Vec<&Seen> = INSN_SEEN.iter().filter(|s| s.count.load(Relaxed) != 0).collect();
    order.sort_by_key(|s| std::cmp::Reverse(s.count.load(Relaxed)));
    for s in order.iter().take(12) {
        xh_core::logi!(
            "emulated: insn 0x{:08x} {} times (first at pc 0x{:x})",
            s.insn.load(Relaxed),
            s.count.load(Relaxed),
            s.pc.load(Relaxed)
        );
    }
    let other = INSN_OTHER.load(Relaxed);
    if other != 0 {
        xh_core::logi!("emulated: {} of other instructions", other);
    }
}

/// CTR_EL0 of the host (its DminLine: the line size), 4 << 16 (64-byte lines) where it cannot be read.
fn ctr_el0() -> u64 {
    #[cfg(target_arch = "aarch64")]
    {
        let ctr: u64;
        // SAFETY: CTR_EL0 is readable at EL0 on Linux.
        unsafe { std::arch::asm!("mrs {}, ctr_el0", out(reg) ctr, options(nomem, nostack, preserves_flags)) };
        ctr
    }
    #[cfg(not(target_arch = "aarch64"))]
    {
        4 << 16
    }
}

/// The cache geometry CCSIDR_EL1 answers and set/way skips use: the host's line size (CTR_EL0.DminLine: 64 bytes on
/// every core this runs on), the sizes of a current big core (64 KiB 4-way L1, 1 MiB 8-way beyond).
struct Cache {
    line: u64,
    ways: u64,
    bytes: u64,
}

fn cache(level: u64) -> Cache {
    let line = 4u64 << ((ctr_el0() >> 16) & 15);
    if level == 0 {
        Cache { line, ways: 4, bytes: 64 << 10 }
    } else {
        Cache { line, ways: 8, bytes: 1 << 20 }
    }
}

fn log2_ceil(n: u64) -> u32 {
    n.next_power_of_two().trailing_zeros()
}

const TPIDR_EL1: u32 = sysreg(3, 0, 13, 0, 4);
const DAIF: u32 = sysreg(3, 3, 4, 2, 1);
const MPIDR_EL1: u32 = sysreg(3, 0, 0, 0, 5);
const CURRENT_EL: u32 = sysreg(3, 0, 4, 2, 2);
const CNTV_CTL_EL0: u32 = sysreg(3, 3, 14, 3, 1);
const CNTV_CVAL_EL0: u32 = sysreg(3, 3, 14, 3, 2);
const CNTV_TVAL_EL0: u32 = sysreg(3, 3, 14, 3, 0);
const CLIDR_EL1: u32 = sysreg(3, 1, 0, 0, 1);
const CCSIDR_EL1: u32 = sysreg(3, 1, 0, 0, 0);
const CSSELR_EL1: u32 = sysreg(3, 2, 0, 0, 0);

/// Emulates the undefined instruction `insn` trapped in kernel context. Returns false if it is not one the host
/// emulates. Advances `r.pc` itself.
pub fn emulate(v: &Vcpu, r: &mut Regs, insn: u32) -> bool {
    v.stat.sysreg.add(1);
    insn_count(insn, r.pc);
    if xh_core::log::enabled(xh_core::log::Level::Debug) {
        xh_core::log::signal_line("emulated instruction, pc", u64::from(insn), r.pc);
    }
    if insn == 0xd400_0002 || insn == 0xd400_0003 {
        // hvc #0, smc #0
        r.x[0] = psci(v, r) as u64;
        r.pc += 4;
        return true;
    }
    if insn == 0xd503_207f || insn == 0xd503_205f {
        // wfi, wfe
        r.pc += 4;
        if insn == 0xd503_207f && v.cpu.irq_pending.load(Relaxed) == 0 {
            cpu::idle(v);
        }
        return true;
    }
    if insn & 0xfff8_0000 == 0xd508_0000 {
        // sys: tlbi, at, dc, ic at EL1: nothing to do.
        // dc isw/csw/cisw (set/way): with setway-skip, the operand becomes the last set and way of its level, so the
        // guest's set/way loop (XNU's DCACHE_SET_WAY: FlushPoU_Dcache at every idle entry) ends after this one trap
        // instead of one trap per line (1024 for L1). The geometry is the one CCSIDR_EL1 answers below (4 ways of
        // 64 KiB at L1, 8 of 1 MiB beyond, the host's line size). The operation does nothing here either way.
        let op = insn & 0xffff_ffe0;
        let rt = insn & 31;
        if SETWAY_SKIP.load(Relaxed) && [0xd508_7640, 0xd508_7a40, 0xd508_7e40].contains(&op) && rt != 31 {
            let x = r.get(rt);
            let c = cache((x >> 1) & 7);
            let sets = c.bytes / (c.line * c.ways);
            let x = (x & 0xe) | (sets - 1) << log2_ceil(c.line) | (c.ways - 1) << (32 - log2_ceil(c.ways));
            r.set(rt, x);
        }
        r.pc += 4;
        return true;
    }
    if insn & 0xfff8_f01f == 0xd500_401f {
        // msr <pstatefield>, #imm
        let (op1, op2, imm) = ((insn >> 16) & 7, (insn >> 5) & 7, u64::from((insn >> 8) & 15));
        let d = daif(v);
        if op1 == 3 && op2 == 6 {
            set_daif(v, d | imm << 6); // DAIFSet
        } else if op1 == 3 && op2 == 7 {
            set_daif(v, d & !(imm << 6)); // DAIFClr
        }
        r.pc += 4; // SPSel, PAN, UAO, ...: no effect here
        return true;
    }
    let Some(s) = decode_sys(insn) else { return false };
    let mut value = if s.read { 0 } else { r.get(s.rt) };
    let key = s.key();
    let sys = &v.sys;
    match key {
        TPIDR_EL1 => {
            // XNU's current thread
            if s.read {
                value = v.cpu.tpidr_el1.load(Relaxed);
            } else {
                v.cpu.tpidr_el1.store(value, Relaxed);
            }
        }
        DAIF => {
            if s.read {
                value = daif(v);
            } else {
                set_daif(v, value);
            }
        }
        // MPIDR_EL1: Aff0 = the vCPU (the tree's "reg"), where the host traps it (Linux answers it itself: 0x80000000)
        MPIDR_EL1 => value = 0x8000_0000 | v.index() as u64,
        CURRENT_EL => value = 4, // EL1
        CNTV_CTL_EL0 => {
            if s.read {
                let ctl = sys.cntv_ctl.load(Relaxed);
                let due = ctl & 1 != 0 && cpu::counter() >= sys.cntv_cval.load(Relaxed);
                value = (ctl & 3) | if due { 4 } else { 0 };
            } else {
                sys.cntv_ctl.store(value & 3, Relaxed);
                timer_update(v);
            }
        }
        CNTV_CVAL_EL0 => {
            if s.read {
                value = sys.cntv_cval.load(Relaxed);
            } else {
                sys.cntv_cval.store(value, Relaxed);
                timer_update(v);
            }
        }
        CNTV_TVAL_EL0 => {
            if s.read {
                value = u64::from(sys.cntv_cval.load(Relaxed).wrapping_sub(cpu::counter()) as u32);
            } else {
                sys.cntv_cval.store(cpu::counter().wrapping_add(value as u32 as i32 as i64 as u64), Relaxed);
                timer_update(v);
            }
        }
        // CLIDR_EL1: L1 separate instruction and data, L2 unified (LoC and LoUU 2)
        CLIDR_EL1 => value = 3 | 4 << 3 | 2 << 24 | 2 << 27,
        CCSIDR_EL1 => {
            // The geometry XNU reads at each CPU's start (cpuid.c do_cacheid) of the cache CSSELR_EL1 selects. A
            // shadow answered 0 here: 16-byte lines, and the kernel then aligned its per-CPU page queues to 16 bytes
            // ("pointer can't be packed" in vm_page_init_local_q).
            let level = sys.shadow(CSSELR_EL1).map_or(0, |s| (s.load(Relaxed) >> 1) & 7);
            let c = cache(level);
            value = u64::from(log2_ceil(c.line) - 4) | (c.ways - 1) << 3 | (c.bytes / (c.line * c.ways) - 1) << 13;
        }
        _ => {
            if !gic::sysreg_access(v.index(), &s, &mut value) {
                // Any other EL1 or EL2 register: a per-CPU shadow that reads back what was written. This lets
                // unchanged start-up code run; it does not give the write its hardware meaning (page tables, vectors
                // and PAC keys are the hosted kernel's own business). That includes EL0-class registers Linux refuses
                // at EL0 (XNU's start clears TPIDRRO_EL0).
                let Some(slot) = sys.shadow(key) else { return false };
                if s.read {
                    value = slot.load(Relaxed);
                } else {
                    slot.store(value, Relaxed);
                }
            }
        }
    }
    if s.read {
        r.set(s.rt, value);
    }
    r.pc += 4;
    true
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    /// test_sysreg (after test_gic: two vCPUs).
    pub(crate) fn run_sysreg() {
        const GUEST_PA: u64 = 0x8000_0000;
        let guest = vec![0u8; 1 << 20];
        let _window = mem::tests::window(guest.as_ptr() as *mut u8, GUEST_PA, guest.len() as u64);
        let v = cpu::vcpu(0);
        let mut r = Regs { pc: 0x100, ..Regs::default() };
        r.x[5] = 0xdead_beef_cafe;
        assert!(emulate(v, &mut r, 0xd518_d085) && v.cpu.tpidr_el1.load(Relaxed) == 0xdead_beef_cafe && r.pc == 0x104); // msr tpidr_el1, x5
        assert!(emulate(v, &mut r, 0xd538_d089) && r.x[9] == 0xdead_beef_cafe); // mrs x9, tpidr_el1
        v.cpu.irq_masked.store(0, Relaxed);
        assert!(emulate(v, &mut r, 0xd503_42df) && v.cpu.irq_masked.load(Relaxed) == 1); // msr daifset, #2
        assert!(emulate(v, &mut r, 0xd53b_4221) && r.x[1] & 0x80 != 0); // mrs x1, daif
        assert!(emulate(v, &mut r, 0xd503_42ff) && v.cpu.irq_masked.load(Relaxed) == 0); // msr daifclr, #2
        assert!(emulate(v, &mut r, 0xd538_4242) && r.x[2] == 4); // mrs x2, CurrentEL
        r.x[3] = 0x30d0_0800;
        assert!(emulate(v, &mut r, 0xd518_1003) && emulate(v, &mut r, 0xd538_1004) && r.x[4] == 0x30d0_0800); // sctlr_el1 shadow
        assert!(emulate(v, &mut r, 0xd508_871f)); // tlbi vmalle1
        assert!(!emulate(v, &mut r, 0xd69f_03e0)); // eret: not emulated
        assert!(!emulate(v, &mut r, 0x0000_0000)); // udf
                                                   // the timer registers arm the CPU's timer
        r.x[0] = 12345;
        r.x[1] = 1;
        assert!(emulate(v, &mut r, 0xd51b_e340) && emulate(v, &mut r, 0xd51b_e321)); // cntv_cval_el0, cntv_ctl_el0
        assert!(v.timer_armed.load(Relaxed) && v.timer_deadline.load(Relaxed) == 12345);
        r.x[1] = 3;
        assert!(emulate(v, &mut r, 0xd51b_e321) && !v.timer_armed.load(Relaxed)); // masked
                                                                                  // PSCI
        r.x[0] = u64::from(XH_PSCI_VERSION);
        assert!(emulate(v, &mut r, 0xd400_0002) && r.x[0] == 0x10000);
        (r.x[0], r.x[1], r.x[2], r.x[3]) = (u64::from(XH_PSCI_CPU_ON), 1, GUEST_PA + 0x1000, 7);
        assert!(emulate(v, &mut r, 0xd400_0003) && r.x[0] == 0 && cpu::vcpu(1).started.load(Relaxed));
        (r.x[0], r.x[1], r.x[2]) = (u64::from(XH_PSCI_CPU_ON), 1, GUEST_PA + 0x1000);
        assert!(emulate(v, &mut r, 0xd400_0002) && r.x[0] as i64 == -4); // ALREADY_ON
        (r.x[0], r.x[1], r.x[2]) = (u64::from(XH_PSCI_CPU_ON), 1, 0x10);
        assert!(emulate(v, &mut r, 0xd400_0002) && r.x[0] as i64 == -9); // INVALID_ADDRESS
        (r.x[0], r.x[1], r.x[2]) = (u64::from(XH_PSCI_CPU_ON), 5, GUEST_PA);
        assert!(emulate(v, &mut r, 0xd400_0002) && r.x[0] as i64 == -2);
        r.x[0] = 0x8400_0099;
        assert!(emulate(v, &mut r, 0xd400_0002) && r.x[0] as i64 == -1);
        // set/way skip and CCSIDR_EL1 (not in the C test): L1 of 64-byte lines, 4 ways, 256 sets
        SETWAY_SKIP.store(true, Relaxed);
        r.x[7] = 0; // level 1 (field 0), set 0, way 0
        assert!(emulate(v, &mut r, 0xd508_7647)); // dc isw, x7
        assert_eq!(r.x[7], 255 << 6 | 3 << 30);
        SETWAY_SKIP.store(false, Relaxed);
        r.x[8] = 0;
        assert!(emulate(v, &mut r, 0xd51a_0008)); // msr csselr_el1, x8
        assert!(emulate(v, &mut r, 0xd539_0009)); // mrs x9, ccsidr_el1
        assert_eq!(r.x[9], 2 | 3 << 3 | 255 << 13);
    }
}
