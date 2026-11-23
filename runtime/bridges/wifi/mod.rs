// SPDX-License-Identifier: MIT
//! wifi/: xnuhost's Wi-Fi bridge (docs/hardware.md section 3, README.md here): the guest's en0 on the phone's wlan0.
//!   frames      which frames cross, and GRO frames cut back to the link's size; DHCP for the log; nl80211's bands
//!   station     the station the guest's driver talks to ([`proto`]), over a radio back end
//!   p2p         what every radio offers for a peer-to-peer link (AirDrop's AWDL, Google's Wonder), `wifictl p2p`
//!   wonder      `wifictl wonder`: a session on Wonder's raw 802.11 path beside wlan0, its AWDL frames counted
//!   wlan        the back end on the phone: AF_PACKET on wlan0, nl80211 for scans and the link, wlan0's host state
//!               kept and given back
//!   supplicant  joins: the vendor's wpa_supplicant through its AIDL service (crate::hal::aidl)
//!   sim         a made-up radio for a boot with no phone (wifi=sim)
//! C: wifi/wifi.h and the files of the same names.

pub mod frames; // wifi/frames.c
pub mod nl; // the netlink attribute loops of frames.c, p2p.c, wonder.c, wlan.c
pub mod p2p; // wifi/p2p.c
pub mod proto; // drivers/net/oneios_wifi_proto.h
pub mod sim; // wifi/sim.c
pub mod station; // wifi/station.c
pub mod supplicant; // wifi/supplicant.c, supplicant.h
pub mod wlan; // wifi/wlan.c
pub mod wonder; // wifi/wonder.c

/// The longest frame of the link.
pub const FRAME: usize = 1514;
/// The station's own frames' type.
pub const CONTROL_TYPE: u16 = proto::OW_ETHERTYPE;
