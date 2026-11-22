// SPDX-License-Identifier: MIT
//! devices/: the bridges' virtio devices on xh-core's virtio-mmio transport (C: include/bridges/devices.h). Each
//! `create` takes the window's base and its SPI, as the block device does, and returns None when the device could not
//! be made. docs/hardware.md and the board ("bridges -> booter") say which device-tree node goes with each.

pub mod bridge_dev; // devices/bridge_dev.c: "oneios,bridge"
pub mod virtio_net; // devices/virtio_net.c: "droidvm,net"
pub mod virtio_snd; // devices/virtio_snd.c: "droidvm,virtio-snd"

pub use bridge_dev::{request_shutdown, BridgeConfig, BridgeDevice, HciFactory};
pub use virtio_net::NetDevice;
pub use virtio_snd::SndDevice;

#[cfg(test)]
mod test_devices; // tests/test_bridges.c's device tests
