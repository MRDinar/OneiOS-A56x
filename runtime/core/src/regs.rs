// SPDX-License-Identifier: MIT
//! The registers of a trapped kernel context (`xh_regs`).

/// x0-x30, sp, pc and pstate of a trapped kernel context; the layout of C's `xh_regs`.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Regs {
    /// x0..x30.
    pub x: [u64; 31],
    /// The stack pointer.
    pub sp: u64,
    /// The program counter.
    pub pc: u64,
    /// PSTATE (NZCV, DAIF, ...).
    pub pstate: u64,
}

impl Regs {
    /// Register `n` as an instruction's operand: 31 is the zero register.
    #[inline]
    pub fn get(&self, n: u32) -> u64 {
        self.x.get(n as usize).copied().unwrap_or(0)
    }

    /// Sets register `n` as an instruction's destination: a write of 31 (the zero register) is dropped.
    #[inline]
    pub fn set(&mut self, n: u32, value: u64) {
        if let Some(r) = self.x.get_mut(n as usize) {
            *r = value;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn zero_register() {
        let mut r = Regs::default();
        r.set(3, 7);
        r.set(31, 9);
        assert_eq!((r.get(3), r.get(31), r.sp), (7, 0, 0));
        assert_eq!(std::mem::size_of::<Regs>(), 34 * 8);
    }
}
