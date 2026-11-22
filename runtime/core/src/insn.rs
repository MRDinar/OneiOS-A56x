// SPDX-License-Identifier: MIT
//! AArch64 instruction decoding (`insn.c`) for the two things xnuhost emulates in kernel context: integer loads and
//! stores that hit a trapped device page, and system-register accesses that are undefined at EL0. Encodings: Arm ARM
//! (DDI 0487), C4.1 "A64 instruction set encoding". Also the TPIDRRO_EL0 text fixup of iOS's code pages.

use std::sync::atomic::{AtomicBool, Ordering};
/// An integer load or store (`xh_mem_insn`).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct MemInsn {
    /// A load (to `rt`) or a store (from `rt`).
    pub load: bool,
    /// Access bytes: 1, 2, 4, 8.
    pub size: u32,
    /// The data register; 31 is the zero register.
    pub rt: u32,
    /// A sign-extending load.
    pub sign_extend: bool,
    /// Sign-extend to 64 bits (else 32).
    pub to64: bool,
    /// Pre/post index: the base is updated by `offset`.
    pub writeback: bool,
    /// The base register (31 = sp).
    pub rn: u32,
    /// The writeback amount.
    pub offset: i64,
    /// ldp/stp: the second register `rt2` at address + size.
    pub pair: bool,
    /// The second register of a pair.
    pub rt2: u32,
}

impl MemInsn {
    /// A loaded value `v` of `size` bytes extended as the instruction says.
    pub fn extend(&self, mut v: u64) -> u64 {
        if self.sign_extend && self.size < 8 {
            let bits = self.size * 8;
            if v & (1u64 << (bits - 1)) != 0 {
                v |= !0u64 << bits;
            }
            if !self.to64 {
                v &= 0xffff_ffff;
            }
        }
        v
    }
}

/// Decodes an AArch64 integer load/store (`xh_decode_mem`); `None` for anything else (SIMD, exclusive, prfm, ...).
pub fn decode_mem(insn: u32) -> Option<MemInsn> {
    let mut m = MemInsn { rt: insn & 31, rn: (insn >> 5) & 31, ..MemInsn::default() };
    if insn & (1 << 26) != 0 {
        return None; // SIMD&FP register
    }
    let size = insn >> 30;
    let opc = (insn >> 22) & 3;

    let imm9 = insn & 0x3b20_0000 == 0x3800_0000; // register, imm9 (unscaled, pre, post)
    if insn & 0x3b00_0000 == 0x3900_0000      // register, unsigned immediate
        || imm9
        || insn & 0x3b20_0c00 == 0x3820_0800
    // register, register offset
    {
        let kind = (insn >> 10) & 3;
        if imm9 && kind == 2 {
            return None; // unprivileged (ldtr/sttr)
        }
        if size == 3 && opc >= 2 {
            return None; // prfm, and opc 3 with size 3
        }
        m.size = 1 << size;
        m.load = opc != 0;
        m.sign_extend = opc >= 2;
        m.to64 = opc == 2 || (opc == 1 && size == 3);
        if opc == 3 && size == 2 {
            return None;
        }
        if imm9 && (kind == 1 || kind == 3) {
            m.writeback = true;
            m.offset = i64::from((insn >> 12) & 0x1ff);
            if m.offset & 0x100 != 0 {
                m.offset -= 0x200;
            }
        }
        return Some(m);
    }
    if insn & 0x3a00_0000 == 0x2800_0000 {
        // pair: stp/ldp/ldpsw
        let popc = insn >> 30;
        let kind = (insn >> 23) & 3;
        let load = insn & (1 << 22) != 0;
        if popc == 3 || (popc == 1 && !load) {
            return None;
        }
        m.pair = true;
        m.rt2 = (insn >> 10) & 31;
        m.size = if popc == 2 { 8 } else { 4 };
        m.load = load;
        m.sign_extend = popc == 1;
        m.to64 = popc != 0;
        if kind == 1 || kind == 3 {
            m.writeback = true;
            let mut imm7 = i64::from((insn >> 15) & 0x7f);
            if imm7 & 0x40 != 0 {
                imm7 -= 0x80;
            }
            m.offset = imm7 * i64::from(m.size);
        }
        return Some(m);
    }
    None
}

/// The key of a system register (`XH_SYSREG`): op0 << 14 | op1 << 11 | CRn << 7 | CRm << 3 | op2.
pub const fn sysreg(op0: u32, op1: u32, crn: u32, crm: u32, op2: u32) -> u32 {
    op0 << 14 | op1 << 11 | crn << 7 | crm << 3 | op2
}

/// An mrs/msr with a register operand (`xh_sys_insn`).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct SysInsn {
    /// mrs (to `rt`) or msr (from `rt`).
    pub read: bool,
    /// op0 (2 or 3).
    pub op0: u32,
    /// op1.
    pub op1: u32,
    /// CRn.
    pub crn: u32,
    /// CRm.
    pub crm: u32,
    /// op2.
    pub op2: u32,
    /// The general register.
    pub rt: u32,
}

impl SysInsn {
    /// The register's [`sysreg`] key (`xh_sys_key`).
    pub const fn key(&self) -> u32 {
        sysreg(self.op0, self.op1, self.crn, self.crm, self.op2)
    }
}

/// Decodes an mrs/msr with op0 = 2 or 3 (`xh_decode_sys`); `None` otherwise.
pub fn decode_sys(insn: u32) -> Option<SysInsn> {
    if insn & 0xffd0_0000 != 0xd510_0000 {
        return None;
    }
    Some(SysInsn {
        read: insn & (1 << 21) != 0,
        op0: 2 + ((insn >> 19) & 1),
        op1: (insn >> 16) & 7,
        crn: (insn >> 12) & 15,
        crm: (insn >> 8) & 15,
        op2: (insn >> 5) & 7,
        rt: insn & 31,
    })
}

/// `udf #0x5c80`: what [`text_fixup`] leaves in place of `svc #0x80` without seccomp (`XH_SVC_MARK`).
pub const SVC_MARK: u32 = 0x0000_5c80;
/// Set (the task factory, on a host without seccomp): [`text_fixup`] also turns `svc #0x80` into [`SVC_MARK`]
/// (`xh_text_fixup_svc`).
pub static TEXT_FIXUP_SVC: AtomicBool = AtomicBool::new(false);
/// `svc #0x80`, iOS's system call.
const SVC_80: u32 = 0xd400_1001;

/// Rewrites one page of iOS code (`xh_text_fixup`); returns the count of words changed.
///
/// iOS user code reads its thread-specific data base with `mrs xN, TPIDRRO_EL0` (0xd53bd060 | N). Linux leaves that
/// register 0 and user space cannot write it, so the host keeps the base in TPIDR_EL0 (`mrs xN, TPIDR_EL0` is
/// 0xd53bd040 | N) and rewrites the reads when the kernel maps a page executable. The risk is a data word in text that
/// happens to equal the encoding.
///
/// iOS itself uses TPIDR_EL0 for something else: XNU keeps the CPU number in its low 12 bits and _os_cpu_number()
/// reads it to pick a per-CPU slot (libmalloc's magazines, libdispatch). With the TSD base there the slot index would
/// be garbage, so those reads become `mov xN, #0`: every thread says CPU 0, which is always a valid slot (the number is
/// only a hint). A write of TPIDR_EL0 would destroy the TSD base and becomes a nop.
///
/// Each word is classified by its original value, so one pass is right; a second pass over the same page is not (it
/// would zero the TSD reads): the kernel fixes a page once, when it is first entered executable, and only pages of
/// code. A host with no seccomp (qemu-user) cannot trap a task's `svc`; there ([`TEXT_FIXUP_SVC`] set) `svc #0x80` also becomes
/// [`SVC_MARK`], which the task runner reports as the system call it stands for.
pub fn text_fixup(text: &mut [u32]) -> u32 {
    let svc = TEXT_FIXUP_SVC.load(Ordering::Relaxed);
    let mut count = 0;
    for word in text.iter_mut() {
        let (insn, rt) = (*word & 0xffff_ffe0, *word & 31);
        *word = match insn {
            _ if svc && *word == SVC_80 => SVC_MARK,
            0xd53b_d060 => 0xd53b_d040 | rt, // mrs xN, TPIDRRO_EL0 -> mrs xN, TPIDR_EL0
            0xd53b_d040 => 0xd280_0000 | rt, // mrs xN, TPIDR_EL0   -> mov xN, #0
            0xd51b_d040 => 0xd503_201f,      // msr TPIDR_EL0, xN   -> nop
            _ => continue,
        };
        count += 1;
    }
    count
}

#[cfg(test)]
mod tests {
    use super::*;

    // test_xnuhost.c test_insn
    #[test]
    fn mem() {
        let m = decode_mem(0xb940_0001).unwrap(); // ldr w1, [x0]
        assert!(m.load && m.size == 4 && m.rt == 1 && m.rn == 0 && !m.sign_extend && !m.writeback);
        let m = decode_mem(0xf900_0462).unwrap(); // str x2, [x3, #8]
        assert!(!m.load && m.size == 8 && m.rt == 2 && m.rn == 3);
        let m = decode_mem(0x3866_68a4).unwrap(); // ldrb w4, [x5, x6]
        assert!(m.load && m.size == 1 && m.rt == 4);
        let m = decode_mem(0xb880_4507).unwrap(); // ldrsw x7, [x8], #4
        assert!(m.load && m.size == 4 && m.sign_extend && m.to64 && m.writeback && m.offset == 4 && m.rn == 8);
        let m = decode_mem(0x381f_fc20).unwrap(); // strb w0, [x1, #-1]!
        assert!(!m.load && m.size == 1 && m.writeback && m.offset == -1 && m.rn == 1);
        let m = decode_mem(0xa900_0801).unwrap(); // stp x1, x2, [x0]
        assert!(!m.load && m.pair && m.size == 8 && m.rt == 1 && m.rt2 == 2);
        let m = decode_mem(0x29c1_1003).unwrap(); // ldp w3, w4, [x0, #8]!
        assert!(m.load && m.pair && m.size == 4 && m.rt2 == 4 && m.writeback && m.offset == 8);
        let m = decode_mem(0x7940_0020).unwrap(); // ldrh w0, [x1]
        assert!(m.load && m.size == 2);
        assert!(decode_mem(0x3dc0_0000).is_none()); // ldr q0, [x0]
        assert!(decode_mem(0xf980_0000).is_none()); // prfm
        assert!(decode_mem(0x8b01_0000).is_none()); // add
    }

    #[test]
    fn sys() {
        let s = decode_sys(0xd538_d080).unwrap(); // mrs x0, tpidr_el1
        assert!(s.read && s.key() == sysreg(3, 0, 13, 0, 4) && s.rt == 0);
        let s = decode_sys(0xd51b_4223).unwrap(); // msr daif, x3
        assert!(!s.read && s.key() == sysreg(3, 3, 4, 2, 1) && s.rt == 3);
        assert!(decode_sys(0xd503_201f).is_none()); // nop
    }

    #[test]
    fn fixup() {
        // two TPIDRRO reads, a TPIDR read (iOS's CPU number), a TPIDRRO write (left alone), a TPIDR write
        let mut text = [0xd53b_d060, 0xd53b_d07e, 0xd53b_d043, 0xd51b_d060, 0xd51b_d049];
        assert_eq!(text_fixup(&mut text), 4);
        assert_eq!(text, [0xd53b_d040, 0xd53b_d05e, 0xd280_0003, 0xd51b_d060, 0xd503_201f]);
        // (the only test that sets the switch: the one above runs without svc words)
        let mut svc = [SVC_80, 0xd400_0001];
        TEXT_FIXUP_SVC.store(true, Ordering::Relaxed);
        let n = text_fixup(&mut svc);
        TEXT_FIXUP_SVC.store(false, Ordering::Relaxed);
        assert_eq!(n, 1);
        assert_eq!(svc, [SVC_MARK, 0xd400_0001]);
    }

    #[test]
    fn extend() {
        let ldrsw = decode_mem(0xb880_4507).unwrap();
        assert_eq!(ldrsw.extend(0xf00d_feed), 0xffff_ffff_f00d_feed);
        let ldr = decode_mem(0xb940_0001).unwrap();
        assert_eq!(ldr.extend(0xf00d_feed), 0xf00d_feed);
    }
}
