// SPDX-License-Identifier: MIT
//! The protocols shared with the guest's drivers, mirrored from their C headers (which stay, for the kernel's C):
//! drivers/bridge/oneios_bridge_proto.h ([`bridge`]) and drivers/haptics/oneios_actuator.h ([`actuator`]). Each has a
//! test that reads the C header and checks every mirrored constant, size and offset. (drivers/net/oneios_wifi_proto.h
//! is crate::wifi::proto, bridges-net's; audio/audio_proto.h is crate::audio::proto.)

pub mod actuator; // drivers/haptics/oneios_actuator.h
pub mod bridge; // drivers/bridge/oneios_bridge_proto.h
pub mod wire; // little-endian fixed-layout structs

pub use bridge::*;
pub use wire::Wire;

#[cfg(test)]
pub(crate) mod cheader;
#[cfg(test)]
mod test_proto;
