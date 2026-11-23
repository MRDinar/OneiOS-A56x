// SPDX-License-Identifier: MIT
//! The board's device windows (`mmio.c`). The kernel reaches them either by loads and stores into the trapped I/O
//! aperture (emulated here from the faulting instruction, [`Bus::emulate`]) or by the mmio_read/mmio_write host calls.
//!
//! The machine has one bus, [`bus()`]; a test may make its own [`Bus`].
use crate::insn::decode_mem;
use crate::regs::Regs;
use std::fmt;
use std::sync::{Arc, OnceLock, RwLock};

/// A device window's registers. Offsets are from the window's base; values are masked to `size` bytes for both.
pub trait MmioDevice: Send + Sync {
    /// A read of `size` (1, 2, 4, 8) bytes at `offset`. The default reads as zero.
    fn mmio_read(&self, offset: u64, size: u32) -> u64 {
        let _ = (offset, size);
        0
    }
    /// A write of `size` bytes at `offset`. The default drops it.
    fn mmio_write(&self, offset: u64, size: u32, value: u64) {
        let _ = (offset, size, value);
    }
}

/// Why [`Bus::add`] refused a window.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AddError {
    /// The bus holds [`MAX_REGIONS`] windows already.
    Full,
    /// The window overlaps one already there.
    Overlap,
}

impl fmt::Display for AddError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            AddError::Full => "no room for another device window",
            AddError::Overlap => "the window overlaps another",
        })
    }
}

impl std::error::Error for AddError {}

/// The most windows a bus holds.
pub const MAX_REGIONS: usize = 32;

/// One device window.
#[derive(Clone)]
pub struct Region {
    /// Its first physical address.
    pub base: u64,
    /// Its size in bytes.
    pub size: u64,
    /// The device's name (the device tree's).
    pub name: String,
    /// The registers.
    pub device: Arc<dyn MmioDevice>,
}

impl fmt::Debug for Region {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Region({} {:#x}+{:#x})", self.name, self.base, self.size)
    }
}

/// A set of device windows. Windows are added at the start and never removed.
#[derive(Debug, Default)]
pub struct Bus {
    regions: RwLock<Vec<Region>>,
}

fn mask(size: u32, value: u64) -> u64 {
    if size >= 8 {
        value
    } else {
        value & ((1u64 << (size * 8)) - 1)
    }
}

impl Bus {
    /// An empty bus.
    pub fn new() -> Bus {
        Bus::default()
    }

    /// Adds the window `[base, base + size)` of `device`.
    pub fn add(&self, base: u64, size: u64, name: &str, device: Arc<dyn MmioDevice>) -> Result<(), AddError> {
        let mut regions = self.regions.write().unwrap_or_else(|e| e.into_inner());
        if regions.len() == MAX_REGIONS {
            return Err(AddError::Full);
        }
        if regions.iter().any(|r| base < r.base.wrapping_add(r.size) && r.base < base.wrapping_add(size)) {
            return Err(AddError::Overlap);
        }
        regions.push(Region { base, size, name: name.to_owned(), device });
        Ok(())
    }

    fn find<R>(&self, pa: u64, f: impl FnOnce(&Region) -> R) -> Option<R> {
        let regions = self.regions.read().unwrap_or_else(|e| e.into_inner());
        regions.iter().find(|r| pa >= r.base && pa - r.base < r.size).map(f)
    }

    /// Whether a window holds `pa`.
    pub fn covers(&self, pa: u64) -> bool {
        self.find(pa, |_| ()).is_some()
    }

    /// The name of the window that holds `pa`.
    pub fn name_at(&self, pa: u64) -> Option<String> {
        self.find(pa, |r| r.name.clone())
    }

    /// A read of `size` bytes at `pa`: zero where no window is (an unmapped bus address reads as zero).
    pub fn read(&self, pa: u64, size: u32) -> u64 {
        let hit = self.find(pa, |r| (Arc::clone(&r.device), pa - r.base));
        hit.map_or(0, |(dev, off)| mask(size, dev.mmio_read(off, size)))
    }

    /// A write of `size` bytes at `pa`, dropped where no window is.
    pub fn write(&self, pa: u64, size: u32, value: u64) {
        if let Some((dev, off)) = self.find(pa, |r| (Arc::clone(&r.device), pa - r.base)) {
            dev.mmio_write(off, size, mask(size, value));
        }
    }

    /// Emulates the load/store `insn` at `regs.pc` against physical address `pa`, and advances pc; false when `insn`
    /// is not an integer load or store.
    pub fn emulate(&self, regs: &mut Regs, insn: u32, pa: u64) -> bool {
        let Some(m) = decode_mem(insn) else { return false };
        let second = pa.wrapping_add(u64::from(m.size));
        if m.load {
            regs.set(m.rt, m.extend(self.read(pa, m.size)));
            if m.pair {
                regs.set(m.rt2, m.extend(self.read(second, m.size)));
            }
        } else {
            self.write(pa, m.size, regs.get(m.rt));
            if m.pair {
                self.write(second, m.size, regs.get(m.rt2));
            }
        }
        if m.writeback {
            let base = if m.rn == 31 { &mut regs.sp } else { &mut regs.x[m.rn as usize] };
            *base = base.wrapping_add(m.offset as u64);
        }
        regs.pc = regs.pc.wrapping_add(4);
        true
    }
}

/// The machine's bus.
pub fn bus() -> &'static Bus {
    static BUS: OnceLock<Bus> = OnceLock::new();
    BUS.get_or_init(Bus::new)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    // test_xnuhost.c test_mmio: four 8-byte registers that remember the last write's size.
    #[derive(Default)]
    struct Fake {
        store: Mutex<([u64; 4], u32)>,
    }
    impl MmioDevice for Fake {
        fn mmio_read(&self, off: u64, _size: u32) -> u64 {
            self.store.lock().unwrap().0[((off / 8) & 3) as usize] >> (off % 8 * 8)
        }
        fn mmio_write(&self, off: u64, size: u32, v: u64) {
            let mut s = self.store.lock().unwrap();
            s.0[((off / 8) & 3) as usize] = v;
            s.1 = size;
        }
    }
    struct Nothing;
    impl MmioDevice for Nothing {}

    #[test]
    fn emulate() {
        let bus = Bus::new();
        let fake = Arc::new(Fake::default());
        assert_eq!(bus.add(0x900_0000, 0x1000, "fake", fake.clone()), Ok(()));
        assert_eq!(bus.add(0x900_0800, 0x1000, "overlap", Arc::new(Nothing)), Err(AddError::Overlap));
        assert!(bus.covers(0x900_0fff) && !bus.covers(0x900_1000));
        assert_eq!(bus.name_at(0x900_0010).as_deref(), Some("fake"));
        let mut r = Regs { pc: 0x1000, ..Regs::default() };
        fake.store.lock().unwrap().0[0] = 0xffff_ffff_f00d_feed;
        assert!(bus.emulate(&mut r, 0xb940_0001, 0x900_0000) && r.x[1] == 0xf00d_feed && r.pc == 0x1004); // ldr w1: zero-extended
        r.x[8] = 0x5000;
        assert!(bus.emulate(&mut r, 0xb880_4507, 0x900_0000)); // ldrsw, post-index
        assert!(r.x[7] == 0xffff_ffff_f00d_feed && r.x[8] == 0x5004);
        r.x[2] = 0x1122_3344_5566_7788;
        assert!(bus.emulate(&mut r, 0xf900_0462, 0x900_0008));
        assert_eq!(*fake.store.lock().unwrap(), ([0xffff_ffff_f00d_feed, 0x1122_3344_5566_7788, 0, 0], 8));
        r.x[0] = 0xabcd;
        assert!(bus.emulate(&mut r, 0x3900_0020, 0x900_0010)); // strb w0, [x1]
        let s = *fake.store.lock().unwrap();
        assert_eq!((s.0[2], s.1), (0xcd, 1));
        assert!(bus.emulate(&mut r, 0xb900_003f, 0x900_0010) && fake.store.lock().unwrap().0[2] == 0); // str wzr
        assert!(!bus.emulate(&mut r, 0x3dc0_0000, 0x900_0000));
        assert_eq!(bus.read(0x1234_5678, 4), 0); // nothing there
    }

    #[test]
    fn pair_and_sp_writeback() {
        let bus = Bus::new();
        let fake = Arc::new(Fake::default());
        bus.add(0x100_0000, 0x100, "fake", fake.clone()).unwrap();
        let mut r = Regs { sp: 0x8000, ..Regs::default() };
        r.x[1] = 1;
        r.x[2] = 2;
        assert!(bus.emulate(&mut r, 0xa900_0801, 0x100_0000)); // stp x1, x2, [x0]
        assert_eq!(fake.store.lock().unwrap().0[..2], [1, 2]);
        assert!(bus.emulate(&mut r, 0xf81f_8fe3, 0x100_0010)); // str x3, [sp, #-8]!
        assert_eq!(r.sp, 0x7ff8);
    }

    #[test]
    fn full() {
        let bus = Bus::new();
        for i in 0..MAX_REGIONS as u64 {
            bus.add(i * 0x1000, 0x1000, "n", Arc::new(Nothing)).unwrap();
        }
        assert_eq!(bus.add(0x10_0000, 0x1000, "n", Arc::new(Nothing)), Err(AddError::Full));
    }
}
