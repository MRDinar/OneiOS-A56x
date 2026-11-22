// SPDX-License-Identifier: MIT
//! bt/: Bluetooth (docs/hardware.md "Bluetooth"). One HCI controller for the guest's transport ([`hci::Hci`]) on a
//! back end: the phone's HAL over /dev/hwbinder ([`hci_hidl`]), or a stand-in ([`hci_sim`]).
//!
//! ```ignore
//! use onebridges::bt::{self, hci::{Hci, HciState, HciFactory}};
//! let factory: HciFactory = if bt::hci_hidl::probe() > 0 { bt::hci_hidl::open_hidl } else { bt::hci_sim::open_sim };
//! let hci = factory(Box::new(|kind, packet| ...), Box::new(|state: HciState| ...)).unwrap();
//! hci.open()?; hci.send(1, &command)?; hci.close();
//! let mac = bt::hci::bt_address("/mnt/vendor/efs/bluetooth/bt_addr");
//! ```

pub mod hci; // bt/hci.c
pub mod hci_hidl; // bt/hci_hidl.c
pub mod hci_sim; // bt/hci_sim.c
pub mod hidl_parcel; // bt/hidl_parcel.c (the generic HIDL wire format: hal::hidl)
pub mod le_scan; // bt/le_scan.c
