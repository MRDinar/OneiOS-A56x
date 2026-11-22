// SPDX-License-Identifier: MIT
//! OneiOS's host bridges (docs/hardware.md): the Linux/Android side of each device the guest's drivers use.

pub mod proto; // the protocols shared with the guest's drivers (drivers/bridge, drivers/net, drivers/haptics)
pub mod cpu; // cpu/: host topology, the vCPU plan, thread placement
pub mod hw; // hw/: the hardware backends and the host device
pub mod hal; // hal/: the treble HAL clients
pub mod haptics; // haptics/: the haptic actuator
pub mod net; // net/: the network bridge
pub mod bt; // bt/: Bluetooth
pub mod audio; // audio/: sound
pub mod brightness; // brightness/: the backlight and the light sensor
pub mod battery; // battery/: the battery
pub mod thermal; // thermal/: temperatures and thermal steps
pub mod wifi; // wifi/: Wi-Fi
pub mod devices; // devices/: the virtio device models on xnuhost's transport
