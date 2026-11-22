// SPDX-License-Identifier: MIT
//! What the devices need from the machine, as hooks the machine installs: a device's interrupt line
//! (`xh_gic_set_spi`) and guest physical RAM (`xh_ram`). xnuhost installs its GIC and its RAM window at the start;
//! a tool or a test installs its own (see [`identity_ram`]). Until then a line goes nowhere and no RAM is there.
use std::ptr::NonNull;
use std::sync::atomic::{AtomicPtr, Ordering};

/// Raises (`true`) or lowers a device's interrupt line, INTID 32..1019 (`xh_gic_set_spi`).
pub type SpiFn = fn(intid: u32, level: bool);
/// The host address of guest physical `[pa, pa + len)`, `None` unless all of it is RAM (`xh_ram`).
pub type RamFn = fn(pa: u64, len: u64) -> Option<NonNull<u8>>;

fn no_spi(_: u32, _: bool) {}
fn no_ram(_: u64, _: u64) -> Option<NonNull<u8>> {
    None
}

static SPI: AtomicPtr<()> = AtomicPtr::new(no_spi as SpiFn as *mut ());
static RAM: AtomicPtr<()> = AtomicPtr::new(no_ram as RamFn as *mut ());

/// Installs the interrupt line hook.
pub fn install_spi(f: SpiFn) {
    SPI.store(f as *mut (), Ordering::Release);
}

/// Installs the guest RAM hook.
pub fn install_ram(f: RamFn) {
    RAM.store(f as *mut (), Ordering::Release);
}

/// Sets device line `intid` to `level`.
#[inline]
pub fn set_spi(intid: u32, level: bool) {
    let p = SPI.load(Ordering::Acquire);
    // SAFETY: SPI only ever holds a `SpiFn` (the default or one stored by install_spi).
    let f: SpiFn = unsafe { std::mem::transmute::<*mut (), SpiFn>(p) };
    f(intid, level)
}

/// The host address of guest physical `[pa, pa + len)`, `None` if it is not all RAM.
#[inline]
pub fn ram(pa: u64, len: u64) -> Option<NonNull<u8>> {
    let p = RAM.load(Ordering::Acquire);
    // SAFETY: RAM only ever holds a `RamFn` (the default or one stored by install_ram).
    let f: RamFn = unsafe { std::mem::transmute::<*mut (), RamFn>(p) };
    f(pa, len)
}

/// A RAM hook for tools and tests whose "guest" is plain host memory: a guest physical address is the host address
/// (as the bridges' tests and encctl). The first 64 KiB (and a range that wraps) are not RAM, so a test can post a bad
/// address.
pub fn identity_ram(pa: u64, len: u64) -> Option<NonNull<u8>> {
    if pa < 0x10000 || pa.checked_add(len).is_none() {
        return None;
    }
    NonNull::new(pa as usize as *mut u8)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn identity() {
        let mut b = [0u8; 4];
        let pa = b.as_mut_ptr() as u64;
        assert_eq!(identity_ram(pa, 4).map(|p| p.as_ptr()), Some(b.as_mut_ptr()));
        assert!(identity_ram(0x100, 4).is_none() && identity_ram(u64::MAX - 1, 4).is_none());
    }
}
