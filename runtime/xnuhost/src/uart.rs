// SPDX-License-Identifier: MIT
//! Port of uart.c. PL011 at the board's "uart0": XNU's serial console (its VMAPPLE_UART path is a PL011 driver).
//! Transmit only: bytes go to the log as "guest" lines.

use std::sync::Arc;
use xh_core::mmio::{Bus, MmioDevice};

struct Pl011;

impl MmioDevice for Pl011 {
    fn mmio_read(&self, offset: u64, _size: u32) -> u64 {
        match offset {
            0x18 => 0x90,  // UARTFR: transmit FIFO empty, receive FIFO empty
            0xfe0 => 0x11, // peripheral ID: PL011
            0xfe4 => 0x10,
            0xfe8 => 0x14,
            0xfec => 0x00,
            0xff0 => 0x0d, // PrimeCell ID
            0xff4 => 0xf0,
            0xff8 => 0x05,
            0xffc => 0xb1,
            _ => 0,
        }
    }

    fn mmio_write(&self, offset: u64, _size: u32, value: u64) {
        if offset == 0 {
            xh_core::log::console(&[value as u8]);
        }
    }
}

/// The console at `base` on `bus`.
pub fn init_on(bus: &Bus, base: u64) -> Result<(), xh_core::mmio::AddError> {
    bus.add(base, 0x1000, "uart0", Arc::new(Pl011))
}

/// The console at `base` on the machine's bus.
pub fn init(base: u64) {
    if let Err(e) = init_on(xh_core::mmio::bus(), base) {
        xh_core::loge!("uart0: {e}");
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn pl011() {
        let bus = xh_core::mmio::Bus::new();
        super::init_on(&bus, 0x900_0000).unwrap();
        assert_eq!(bus.read(0x900_0018, 4), 0x90);
        assert_eq!((0..4).map(|i| bus.read(0x900_0fe0 + 4 * i, 4)).collect::<Vec<_>>(), [0x11, 0x10, 0x14, 0]);
        assert_eq!(bus.read(0x900_0ffc, 4), 0xb1);
        // the data register: a byte to the guest console (its own line buffer in xh-core); the others are dropped
        b"uart0: ok\n".iter().for_each(|&c| bus.write(0x900_0000, 4, c.into()));
        bus.write(0x900_0004, 4, 0x41);
    }
}
