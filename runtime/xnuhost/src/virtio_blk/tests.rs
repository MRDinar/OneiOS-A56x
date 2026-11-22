// SPDX-License-Identifier: MIT
// tests/test_xnuhost.c's test_virtio_blk: the read-only image's device driven as a guest driver would, the writable
// Data store (created sparse, seeded from an image and not from a tar, kept across starts), the late flush.

use super::*;
use crate::config::DiskConfig;
use std::fs;
use std::os::unix::fs::MetadataExt;
use xh_core::mmio::MmioDevice;
use xh_core::virtio::testing::{self, Driver, GuestRam};

const REQ: usize = 0x20000;
const DATA: usize = 0x21000;
const STATUS: usize = 0x22000;
const RINGS: usize = 0x10000;

struct Guest {
    ram: GuestRam,
}

impl Guest {
    fn new() -> Guest {
        testing::install();
        Guest { ram: GuestRam::new(0x30000) }
    }
    fn req(&self, kind: u32, sector: u64) {
        self.ram.set_u32(REQ, kind);
        self.ram.set_u32(REQ + 4, 0);
        self.ram.set_u64(REQ + 8, sector);
    }
    fn pa(&self, at: usize) -> u64 {
        self.ram.pa(at)
    }
    fn status(&self) -> u8 {
        self.ram.read(STATUS, 1)[0]
    }
    /// header, a data part of `len` (device-writable or not), the status byte
    fn rw(&self, len: u32, writable: bool) -> [(u64, u32, bool); 3] {
        [(self.pa(REQ), 16, false), (self.pa(DATA), len, writable), (self.pa(STATUS), 1, true)]
    }
}

fn up(dev: &BlkDevice, features: u64, g: &Guest) -> Option<Driver> {
    Driver::up(dev.clone() as Arc<dyn MmioDevice>, features, 1, 16, g.pa(RINGS))
}

#[test]
fn virtio_blk() {
    let dir = std::env::temp_dir().join(format!("xnuhost-vblk-{}", std::process::id()));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).unwrap();
    let path = dir.join("ro.img");
    let image: Vec<u8> = (0..64u8).flat_map(|s| [s + 1; 512]).collect();
    fs::write(&path, &image).unwrap();
    let ro = DiskConfig { path: path.to_str().unwrap().into(), ..Default::default() };
    let disk = Blk::open(&ro).unwrap();
    assert!(disk.bytes() == 64 * 512 && !disk.writable());
    let g = Guest::new();
    let dev = create(0, 40, disk, true).unwrap();
    let r = |off: u64| dev.mmio_read(off, 4) as u32;
    let w = |off: u64, v: u32| dev.mmio_write(off, 4, v as u64);
    assert_eq!(r(8), 2);
    w(0x14, 0);
    assert!(r(0x10) & F_RO as u32 != 0); // F_RO, which NativeVirtioBlock requires
    w(0x14, 1);
    assert!(r(0x10) & 1 != 0); // VERSION_1
    let mut d = up(&dev, F_RO, &g).unwrap();
    assert!(r(0x100) == 64 && r(0x104) == 0);
    g.req(T_IN, 5);
    g.ram.fill(STATUS, 0xff, 1);
    assert_eq!(d.send(0, &g.rw(1024, true), true), Some(1025));
    let data = g.ram.read(DATA, 1024);
    assert!(g.status() == 0 && data[0] == 6 && data[511] == 6 && data[512] == 7);
    assert_eq!(r(0x60), 1);
    w(0x64, 1);
    assert_eq!(r(0x60), 0);
    g.req(T_OUT, 5); // a write to a read-only disk
    assert!(d.send(0, &g.rw(512, false), true) == Some(1) && g.status() == 1);
    g.req(T_IN, 64); // past the end
    assert!(d.send(0, &g.rw(512, true), true) == Some(1) && g.status() == 1);
    g.req(T_IN, 0); // a part outside guest memory: the chain is returned unused
    let outside = [(g.pa(REQ), 16, false), (0x10, 512, true), (g.pa(STATUS), 1, true)];
    assert_eq!(d.send(0, &outside, true), Some(0));
    g.req(T_GET_ID, 0);
    assert_eq!(d.send(0, &g.rw(20, true), true), Some(21));
    assert_eq!(&g.ram.read(DATA, 20)[..15], b"oneios-xnuhost\0");
    g.req(99, 0); // an unknown request
    assert!(d.send(0, &g.rw(512, true), true) == Some(1) && g.status() == S_UNSUPP);
    // a driver that does not accept VERSION_1 is refused
    w(0x70, 0);
    w(0x70, 3);
    w(0x24, 1);
    w(0x20, 0);
    w(0x70, 11);
    assert_eq!(r(0x70) & 8, 0);

    // the writable Data store: created sparse on the first open, seeded from an image, not from a tar
    let rw = DiskConfig {
        path: dir.join("data.img").to_str().unwrap().into(),
        writable: true,
        data_store: true,
        seed: path.to_str().unwrap().into(),
    };
    let store = Blk::open(&rw).unwrap();
    assert!(store.writable() && store.bytes() >= 64 * 512 && store.label() == "data");
    let mut sector = [0u8; 512];
    assert!(store.read(3 * 512, &mut sector).is_ok() && sector[0] == 4); // the seed's content
    sector.fill(0xee);
    assert!(store.write(1 << 20, &sector).is_ok() && store.flush().is_ok());
    assert!(fs::metadata(&rw.path).unwrap().blocks() * 512 < 16 << 20); // sparse
    drop(store);
    let store = Blk::open(&rw).unwrap(); // a second start keeps the data
    assert!(store.read(1 << 20, &mut sector).is_ok() && sector[0] == 0xee);
    let wdev = create(0, 41, store.clone(), true).unwrap();
    let wr = |off: u64| wdev.mmio_read(off, 4) as u32;
    wdev.mmio_write(0x14, 4, 0);
    assert_eq!(wr(0x10) & F_RO as u32, 0);
    let mut wd = up(&wdev, 0, &g).unwrap();
    g.req(T_OUT, 9);
    g.ram.fill(DATA, 0x77, 512);
    assert!(wd.send(0, &g.rw(512, false), true) == Some(1) && g.status() == 0);
    g.req(T_FLUSH, 0); // flush
    let flush = [(g.pa(REQ), 16, false), (g.pa(STATUS), 1, true)];
    assert!(wd.send(0, &flush, true) == Some(1) && g.status() == 0);
    assert!(store.read(9 * 512, &mut sector).is_ok() && sector[5] == 0x77);
    assert_eq!(store.flushes(), 1); // the guest's flush, counted and timed
                                    // A driver that takes F_LATE_FLUSH (bit 23): a flush is completed by the disk's flush thread after the notify
                                    // returned; reads and writes are still done inside the notify.
    wdev.mmio_write(0x14, 4, 0);
    assert!(wr(0x10) & F_LATE_FLUSH as u32 != 0); // offered on a writable disk
    let mut wd = up(&wdev, F_LATE_FLUSH | F_FLUSH, &g).unwrap();
    g.req(T_OUT, 10);
    g.ram.fill(DATA, 0x55, 512);
    assert!(wd.send(0, &g.rw(512, false), true) == Some(1) && g.status() == 0);
    g.req(T_FLUSH, 0);
    g.ram.fill(STATUS, 0xff, 1);
    let raised = testing::spi_raised(41);
    let _ = wd.send(0, &flush, true);
    wdev.device().wait_flushed();
    assert_eq!(wd.reap(0).map(|(_, len)| len), Some(1)); // completed later, from the thread
    assert_eq!(g.status(), 0);
    assert!(wr(0x60) & 1 != 0 && testing::spi_raised(41) > raised); // and the interrupt raised, as for any completion
    wdev.mmio_write(0x64, 4, 1);
    g.req(T_IN, 10); // the next request is served in the notify
    g.ram.fill(DATA, 0, 512);
    assert!(wd.send(0, &g.rw(512, true), true) == Some(513) && g.ram.read(DATA, 8)[7] == 0x55);
    assert_eq!(store.flushes(), 2);
    // With blk-late-flush=0 the bit is not offered.
    let w2 = create(0, 43, store.clone(), false).unwrap();
    w2.mmio_write(0x14, 4, 0);
    assert_eq!(w2.mmio_read(0x10, 4) as u32 & F_LATE_FLUSH as u32, 0);
    drop((wd, d));
    let tar_path = dir.join("seed.tar");
    let mut tar = vec![0u8; 8192];
    tar[257..262].copy_from_slice(b"ustar");
    tar[4096..4608].fill(0x42);
    fs::write(&tar_path, &tar).unwrap();
    let fresh = DiskConfig {
        path: dir.join("fresh.img").to_str().unwrap().into(),
        writable: true,
        seed: tar_path.to_str().unwrap().into(),
        ..Default::default()
    };
    let store = Blk::open(&fresh).unwrap();
    assert!(store.read(4096, &mut sector).is_ok() && sector[0] == 0); // a tar seed is not a disk image
    let _ = fs::remove_dir_all(&dir);
}
