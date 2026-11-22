// SPDX-License-Identifier: MIT
//! net/: the network bridge: the guest's en0 frames to a tap (routed by net/oneios-net.sh, which stays a shell
//! script), the built-in DHCP answer, and the backend trait the virtio-net device and the Wi-Fi bridge share.

#[allow(clippy::module_inception)] // one C file, one module: net/net.c
pub mod net;

pub use net::{dhcp_reply, Net, NetBackend, NetConfig, NetOpen, RxFn, FRAME_MAX};

#[cfg(test)]
mod test_net; // tests/test_bridges.c's test_net
