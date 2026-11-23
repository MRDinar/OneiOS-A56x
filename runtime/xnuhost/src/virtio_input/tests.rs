// SPDX-License-Identifier: MIT
//! test_xnuhost.c's test_virtio_input: the configuration space, and events kept until the guest posts buffers, handed
//! over at a report boundary.
use std::sync::Arc;

use xh_core::mmio::MmioDevice;
use xh_core::virtio::testing::{self, Driver, GuestRam};

use super::*;

#[test]
fn config_and_events() {
    testing::install();
    let ram = GuestRam::new(1 << 20);
    let dev = create(0, 43).unwrap();
    assert_eq!(dev.mmio_read(8, 4), 18);
    set_abs(&dev, 0x35, 0, 1439, 10);
    set_abs(&dev, 0x36, 0, 3119, 10);
    dev.mmio_write(0x100, 1, 0x12); // select ABS_INFO
    dev.mmio_write(0x101, 1, 0x36);
    assert!(dev.mmio_read(0x102, 1) == 20 && dev.mmio_read(0x108 + 4, 4) == 3119);
    dev.mmio_write(0x100, 1, 0x11); // EV_BITS of EV_ABS
    dev.mmio_write(0x101, 1, 3);
    assert!(dev.mmio_read(0x102, 1) == 7 && dev.mmio_read(0x108 + 6, 1) & 0x60 == 0x60);
    dev.mmio_write(0x101, 1, 1); // EV_KEY: volume down is key 114
    assert!(dev.mmio_read(0x108 + 14, 1) & (1 << 2) != 0);
    dev.mmio_write(0x100, 1, 1);
    assert_eq!(dev.mmio_read(0x108, 4), 0x6965_6e4f); // "Onei"
    let mut d = Driver::up(dev.clone() as Arc<dyn MmioDevice>, 0, 2, 8, ram.pa(0x10000)).unwrap();
    // events before the guest posts buffers are kept, and handed over at the report boundary
    event(&dev, 3, 0x35, 700);
    event(&dev, 3, 0x36, 1500);
    event(&dev, 0, 0, 0);
    assert_eq!(d.used_idx(0), 0);
    for i in 0..4 {
        d.send(0, &[(ram.pa(0x50000 + i * 8), 8, true)], i == 3);
    }
    assert_eq!(d.used_idx(0), 3);
    let ev = |i: usize| {
        let b = ram.read(0x50000 + i * 8, 8);
        (
            u16::from_le_bytes([b[0], b[1]]),
            u16::from_le_bytes([b[2], b[3]]),
            i32::from_le_bytes([b[4], b[5], b[6], b[7]]),
        )
    };
    assert!(ev(0) == (3, 0x35, 700) && ev(1).2 == 1500 && ev(2).0 == 0);
    event(&dev, 1, 116, 1); // the power key, no report yet
    assert_eq!(d.used_idx(0), 3);
    event(&dev, 0, 0, 0);
    assert!(d.used_idx(0) == 4 && ev(3).0 == 1 && ev(3).1 == 116); // only one buffer was left: the report waits
    d.send(0, &[(ram.pa(0x50000), 8, true)], true);
    assert!(d.used_idx(0) == 5 && ev(0).0 == 0);
    assert_eq!(backlog(&dev), 0);
}
