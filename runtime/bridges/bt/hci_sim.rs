// SPDX-License-Identifier: MIT
//! Bluetooth bridge: a stand-in controller (bluetooth=sim) for a boot with no phone, the PC's qemu-aarch64 boot
//! above all. No radio and no HAL: every HCI command gets the answer a Bluetooth 5.4 controller gives (a Command
//! Complete with plausible return parameters, or a Command Status for the commands that answer that way), vendor
//! commands (OGF 0x3f) get "Unknown HCI Command" as a Qualcomm controller gives Broadcom's, and an LE scan that is
//! turned on hears one made-up device (an advertising report named "OneiOS sim"), so that the whole path
//! (bluetoothd, the guest's Skywalk pipe and H4 core, the bridge channel, this file) can be seen working in
//! xnuhost's log. ACL, SCO and ISO data are dropped: nothing connects. C: bt/hci_sim.c.

use super::hci::{Hci, HciBackend, HciState, RxFn, StateFn, H4_COMMAND, H4_EVENT};
use std::sync::atomic::{AtomicU32, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, PoisonError, Weak};

/// Commands logged (the first SAY).
const SAY: u32 = 64;

/// The address the stand-in reports (display order): locally administered unless xnuhost sets the phone's.
static SIM_ADDRESS: Mutex<[u8; 6]> = Mutex::new([0x02, 0x0e, 0x10, 0x05, 0x51, 0x4d]);

/// The address it reports from the next open.
pub fn set_address(address: [u8; 6]) {
    *SIM_ADDRESS.lock().unwrap_or_else(PoisonError::into_inner) = address;
}

struct SimBt {
    hci: Weak<Hci>,
    address: Mutex<[u8; 6]>,
    commands: AtomicU64,
    said: AtomicU32,
}

const FEATURES: [u8; 8] = *b"\xff\xfe\x8f\xfe\xd8\x3f\x5b\x87";

impl SimBt {
    /// Return parameters after the status byte, by opcode: written into r, their length.
    fn returns(&self, op: u16, param: &[u8], r: &mut [u8]) -> usize {
        match op {
            0x1001 => {
                // Read Local Version Information: 5.4, Qualcomm
                r[..8].copy_from_slice(&[0x0d, 0x00, 0x00, 0x0d, 0x1d, 0x00, 0x00, 0x00]);
                8
            }
            0x1002 => {
                r[..64].fill(0xff); // Read Local Supported Commands: all of them
                64
            }
            0x1003 => {
                r[..8].copy_from_slice(&FEATURES); // Read Local Supported Features (BR/EDR and LE)
                8
            }
            0x1004 => {
                // Read Local Extended Features: page, max page, 8 bytes
                r[0] = param.first().copied().unwrap_or(0);
                r[1] = 2;
                if r[0] == 0 {
                    r[2..10].copy_from_slice(&FEATURES);
                } else if r[0] == 1 {
                    r[2] = 0x0f;
                }
                10
            }
            0x1005 => {
                r[..6].copy_from_slice(&[0xfd, 0x03, 64, 8, 0, 8]); // Read Buffer Size: ACL 1021 x 8, SCO 64 x 8
                7
            }
            0x1009 => {
                let a = *self.address.lock().unwrap_or_else(PoisonError::into_inner);
                for i in 0..6 {
                    r[i] = a[5 - i]; // Read BD_ADDR (little-endian)
                }
                6
            }
            0x100b => 2, // Read Local Supported Codecs: none
            0x0c14 => {
                r[..10].copy_from_slice(b"OneiOS sim"); // Read Local Name
                248
            }
            0x0c23 => 3, // Read Class of Device
            0x0c25 => {
                r[0] = 0x60; // Read Voice Setting
                2
            }
            0x0c38 => {
                r[0] = 1; // Read Number of Supported IAC
                1
            }
            0x0c39 => {
                r[..4].copy_from_slice(&[1, 0x33, 0x8b, 0x9e]); // Read Current IAC LAP: GIAC
                4
            }
            0x0c44 | 0x0c45 | 0x0c46 | 0x0c48 => 1, // inquiry scan type, inquiry mode, page scan type, AFH mode
            0x0c58 => 1,                            // Read Inquiry Response Transmit Power Level
            0x0c6d => {
                r[0] = 1; // Read LE Host Support
                2
            }
            0x0c79 => 1, // Read Secure Connections Host Support
            0x1006 | 0x1007 | 0x0c0d | 0x0c1b | 0x0c1d | 0x0c27 | 0x0c29 | 0x0c2d => 2,
            0x1408 => 2, // Read Encryption Key Size: handle
            0x2002 => {
                r[0] = 0xfb; // LE Read Buffer Size: 251 x 8
                r[2] = 8;
                3
            }
            0x2060 => {
                r[0] = 0xfb; // LE Read Buffer Size v2 (no ISO buffers)
                r[2] = 8;
                6
            }
            0x2003 => {
                r[..8].copy_from_slice(b"\xff\x79\x00\x00\x00\x00\x00\x00"); // LE features: through extended advertising
                8
            }
            0x2007 => 1, // LE Read Advertising Physical Channel Tx Power
            0x200f => {
                r[0] = 16; // LE Read Filter Accept List Size
                1
            }
            0x2017 => 16, // LE Encrypt
            0x2018 => {
                // LE Rand
                let c = self.commands.load(Ordering::Relaxed);
                for (i, b) in r[..8].iter_mut().enumerate() {
                    *b = 0x5a ^ c.wrapping_mul(31).wrapping_add(i as u64 * 17) as u8;
                }
                8
            }
            0x201c => {
                r[..5].fill(0xff); // LE Read Supported States
                r[5] = 0x03;
                8
            }
            0x2023 => {
                r[..4].copy_from_slice(&[0xfb, 0, 0x48, 0x08]); // LE Read Suggested Default Data Length: 251, 2120
                4
            }
            0x202a => {
                r[0] = 16; // LE Read Resolving List Size
                1
            }
            0x202b | 0x202c => 6, // LE Read Peer/Local Resolvable Address
            0x202f => {
                r[..8].copy_from_slice(&[0xfb, 0, 0x48, 0x08, 0xfb, 0, 0x48, 0x08]); // LE Read Maximum Data Length
                8
            }
            0x2030 => 4, // LE Read PHY: handle, 1M, 1M
            0x2036 => 1, // LE Set Extended Advertising Parameters: Tx power
            0x203a => {
                r[..2].copy_from_slice(&[0xfb, 0x00]); // LE Read Maximum Advertising Data Length: 251
                2
            }
            0x203b => {
                r[0] = 6; // LE Read Number of Supported Advertising Sets
                1
            }
            0x204b => {
                r[..2].copy_from_slice(&[0xf0, 0x14]); // LE Read Transmit Power: -16..20 dBm
                2
            }
            0x204c => 2, // LE Read RF Path Compensation
            _ => 0,
        }
    }

    fn event(&self, e: &[u8]) {
        if let Some(h) = self.hci.upgrade() {
            h.deliver(H4_EVENT, e);
        }
    }

    /// An LE scan turned on: one report of a made-up device (legacy or extended, as the scan asked).
    fn heard(&self, extended: bool) {
        let mut data = vec![2, 0x01, 0x06, 11, 0x09]; // flags: LE general discoverable; complete local name
        data.extend_from_slice(b"OneiOS sim");
        let mut e = vec![0x3e, 0];
        if !extended {
            e.extend_from_slice(&[0x02, 1, 0x00, 0x01]); // advertising report, 1, ADV_IND, random
            e.extend_from_slice(b"\x11\x22\x33\x44\x55\xc6");
            e.push(data.len() as u8);
            e.extend_from_slice(&data);
            e.push(0xc4); // RSSI -60
        } else {
            e.extend_from_slice(&[0x0d, 1, 0x13, 0x00, 0x01]); // extended report, 1, legacy ADV_IND, random
            e.extend_from_slice(b"\x11\x22\x33\x44\x55\xc6");
            e.extend_from_slice(&[1, 0, 0xff, 0x7f, 0xc4]); // 1M, no secondary, no SID, no Tx power, RSSI
            e.extend_from_slice(&[0; 9]); // interval, direct address type and address
            e.push(data.len() as u8);
            e.extend_from_slice(&data);
        }
        e[1] = (e.len() - 2) as u8;
        self.event(&e);
    }
}

/// The commands a controller answers with a Command Status (their result comes later as its own event).
fn status_only(op: u16) -> bool {
    matches!(
        op,
        0x0401
            | 0x0405
            | 0x0406
            | 0x0409
            | 0x040b
            | 0x0419
            | 0x041b
            | 0x041c
            | 0x041d
            | 0x200d
            | 0x2013
            | 0x2016
            | 0x2019
            | 0x2025
            | 0x2026
            | 0x2032
            | 0x2043
            | 0x205e
    )
}

impl HciBackend for SimBt {
    fn open(&self) -> Result<(), i32> {
        let a = *SIM_ADDRESS.lock().unwrap_or_else(PoisonError::into_inner);
        *self.address.lock().unwrap_or_else(PoisonError::into_inner) = a;
        self.said.store(0, Ordering::Relaxed);
        crate::br_info!(
            "bluetooth: sim: the stand-in controller is on, address {:02x}:{:02x}:{:02x}:{:02x}:{:02x}:{:02x}",
            a[0],
            a[1],
            a[2],
            a[3],
            a[4],
            a[5]
        );
        if let Some(h) = self.hci.upgrade() {
            h.set_state(HciState::On);
        }
        Ok(())
    }

    fn send(&self, kind: u8, p: &[u8]) -> Result<(), i32> {
        if kind != H4_COMMAND {
            return Ok(()); // data: nothing is connected
        }
        let op = u16::from(p[0]) | u16::from(p[1]) << 8;
        let param = &p[3..];
        self.commands.fetch_add(1, Ordering::Relaxed);
        let status = if op >> 10 == 0x3f { 0x01 } else { 0x00 }; // vendor commands: Unknown HCI Command
        if self.said.load(Ordering::Relaxed) < SAY {
            self.said.fetch_add(1, Ordering::Relaxed);
            crate::br_debug!("bluetooth: sim: command 0x{op:04x}, {} parameter bytes", param.len());
        }
        if status == 0 && status_only(op) {
            self.event(&[0x0f, 4, 0, 1, p[0], p[1]]);
            if op == 0x0401 {
                self.event(&[0x01, 1, 0]); // Inquiry Complete: nobody there
            }
            return Ok(());
        }
        let mut e = [0u8; 3 + 3 + 1 + 255];
        let rn = if status != 0 { 0 } else { self.returns(op, param, &mut e[6..]) };
        e[..6].copy_from_slice(&[0x0e, (4 + rn) as u8, 1, p[0], p[1], status]);
        self.event(&e[..6 + rn]);
        if (op == 0x200c || op == 0x2042) && param.first() == Some(&1) {
            self.heard(op == 0x2042);
        }
        Ok(())
    }

    fn close(&self) {
        crate::br_info!(
            "bluetooth: sim: the stand-in controller is off after {} commands",
            self.commands.load(Ordering::Relaxed)
        );
    }
}

/// The stand-in controller (bluetooth=sim). C kept one per process; each call here makes a new one.
pub fn open_sim(rx: RxFn, state: StateFn) -> Option<Arc<Hci>> {
    Some(Hci::new(
        |hci| SimBt { hci, address: Mutex::new([0; 6]), commands: AtomicU64::new(0), said: AtomicU32::new(0) },
        Some(rx),
        Some(state),
    ))
}

#[cfg(test)]
mod tests {
    use super::super::hci::tests::recorder;
    use super::*;

    // tests/test_bridges.c test_bt_sim
    #[test]
    fn stand_in_controller() {
        let (r, rx, state) = recorder();
        set_address(*b"\x02\x0e\x10\xaa\xbb\xcc");
        let h = open_sim(rx, state).unwrap();
        let reset = [0x03u8, 0x0c, 0x00];
        let version = [0x01u8, 0x10, 0x00];
        let addr = [0x09u8, 0x10, 0x00];
        let vendor = [0x48u8, 0xfc, 0x01, 0x0d];
        let commands = [0x02u8, 0x10, 0x00];
        let inquiry = [0x01u8, 0x04, 0x05, 0x33, 0x8b, 0x9e, 0x08, 0x00];
        let scan = [0x0cu8, 0x20, 0x02, 0x01, 0x00];
        let extscan = [0x42u8, 0x20, 0x06, 0x01, 0x00, 0, 0, 0, 0];
        let ext_feat = [0x04u8, 0x10, 0x01, 0x01];
        let last = || r.lock().unwrap().last.clone();
        assert_eq!(h.send(1, &reset), Err(-libc::ENOTCONN)); // not open yet
        assert!(h.open().is_ok());
        assert!(r.lock().unwrap().states == 1 && r.lock().unwrap().state == Some(HciState::On)); // ready at once
        assert!(h.send(1, &reset).is_ok() && last() == b"\x0e\x04\x01\x03\x0c\x00");
        assert!(h.send(1, &version).is_ok());
        let l = last();
        assert!(l.len() == 14 && l[1] == 12 && l[5] == 0 && l[6] == 0x0d && l[10] == 0x1d);
        assert!(h.send(1, &addr).is_ok());
        let l = last();
        assert!(l.len() == 12 && l[6..12] == *b"\xcc\xbb\xaa\x10\x0e\x02"); // little-endian
        assert!(h.send(1, &commands).is_ok());
        let l = last();
        assert!(l.len() == 70 && l[6] == 0xff && l[69] == 0xff);
        assert!(h.send(1, &ext_feat).is_ok());
        let l = last();
        assert!(l.len() == 16 && l[6] == 1 && l[7] == 2);
        assert!(h.send(1, &vendor).is_ok());
        let l = last();
        assert!(l.len() == 6 && l[5] == 0x01); // Unknown HCI Command, as a Qualcomm chip
        let before = r.lock().unwrap().packets;
        assert!(h.send(1, &inquiry).is_ok());
        assert!(r.lock().unwrap().packets == before + 2 && last()[0] == 0x01); // Command Status, Inquiry Complete
        assert!(h.send(1, &scan).is_ok() && r.lock().unwrap().reports == 1 && last()[0] == 0x3e);
        assert!(h.send(1, &extscan).is_ok() && r.lock().unwrap().reports == 2); // and an extended one
        h.close();
        assert_eq!(r.lock().unwrap().state, Some(HciState::Off));
    }
}
