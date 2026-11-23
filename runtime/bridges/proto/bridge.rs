// SPDX-License-Identifier: MIT
//! The OneiOS bridge channel (C: drivers/bridge/oneios_bridge_proto.h, which stays for the guest's drivers): one
//! virtio-mmio device ("oneios,bridge" in /arm-io) that carries small messages between the guest drivers in drivers/
//! and the host bridges.
//!
//! Queue 0 (guest to host): one readable buffer per message. Queue 1 (host to guest): the guest posts writable
//! buffers of OB_MAX_MESSAGE bytes; the host fills one per message. A message is this header and `length` payload
//! bytes. All values little-endian.

use crate::wire_struct;

pub const OB_VIRTIO_DEVICE_ID: u32 = 0x4f42; // not a registered VirtIO id; the guest matches the node, not the id
pub const OB_PROTO_VERSION: u32 = 1;
pub const OB_MAX_MESSAGE: u32 = 4096;
pub const OB_MAX_PAYLOAD: u32 = OB_MAX_MESSAGE - 8;
pub const OB_QUEUE_TX: u32 = 0; // guest to host
pub const OB_QUEUE_RX: u32 = 1; // host to guest

wire_struct! {
    /// struct ob_header: the start of every message.
    pub struct ObHeader {
        /// OB_CH_*
        pub channel: u16,
        /// per channel
        pub kind: u16,
        /// payload bytes after the header
        pub length: u32,
    }
}

pub const OB_CH_CONTROL: u16 = 0;
pub const OB_CH_HAPTIC: u16 = 1;
pub const OB_CH_HCI: u16 = 2;
pub const OB_CH_CAMERA: u16 = 3;
pub const OB_CH_BRIGHTNESS: u16 = 4;
pub const OB_CH_BATTERY: u16 = 5;
pub const OB_CH_THERMAL: u16 = 6;
pub const OB_CH_COUNT: u16 = 7;

// ---- control ----------------------------------------------------------------------------------
// guest -> host OB_CTL_HELLO { u32 version }; host -> guest OB_CTL_HELLO { u32 version, u32 channels } where bit n of
// channels says the host serves channel n. host -> guest OB_CTL_SHUTDOWN (no payload): the return asked for a graceful
// shutdown. The guest should begin its normal power-off and then halt; xnuhost waits up to return-graceful-ms and kills
// regardless.
pub const OB_CTL_HELLO: u16 = 1;
pub const OB_CTL_SHUTDOWN: u16 = 2;
wire_struct! {
    pub struct ObCtlHello { pub version: u32, pub channels: u32 }
}
/// The optional v1 hello extension (12 bytes): an older guest ignores the extra word; a newer guest treats an 8-byte
/// reply as features=0.
pub const OB_FEAT_HAPTIC_IMPACT: u32 = 1 << 0;
wire_struct! {
    pub struct ObCtlHelloExt { pub version: u32, pub channels: u32, pub features: u32 }
}

// ---- haptics (guest -> host only) -------------------------------------------------------------
pub const OB_HAPTIC_VIB: u16 = 1; // struct ob_haptic_vib: vibrate from now; on_ms 0 stops
pub const OB_HAPTIC_STOP: u16 = 2; // no payload
pub const OB_HAPTIC_IMPACT: u16 = 3; // explicit one-shot impact; not a short VIB segment
pub const OB_IMPACT_LIGHT: u32 = 1;
pub const OB_IMPACT_MEDIUM: u32 = 2;
pub const OB_IMPACT_HEAVY: u32 = 3;
wire_struct! {
    pub struct ObHapticImpact { pub kind: u32 }
}
wire_struct! {
    pub struct ObHapticVib {
        /// 0 = stop
        pub on_ms: u32,
        /// 0..1000 (iOS's 0.0..1.0)
        pub intensity: u16,
        /// 0 = the actuator's own (resonance)
        pub frequency_hz: u16,
    }
}

// ---- Bluetooth HCI ----------------------------------------------------------------------------
// Types 1..5 are the H4 packet indicators and carry one whole HCI packet (without the indicator).
pub const OB_HCI_COMMAND: u16 = 1;
pub const OB_HCI_ACL: u16 = 2;
pub const OB_HCI_SCO: u16 = 3;
pub const OB_HCI_EVENT: u16 = 4;
pub const OB_HCI_ISO: u16 = 5;
pub const OB_HCI_OPEN: u16 = 16; // guest -> host: power the controller, no payload
pub const OB_HCI_CLOSE: u16 = 17; // guest -> host: release it
pub const OB_HCI_STATE: u16 = 18; // host -> guest: struct ob_hci_state
pub const OB_HCI_STATE_OFF: u32 = 0;
pub const OB_HCI_STATE_READY: u32 = 1;
pub const OB_HCI_STATE_FAILED: u32 = 2;
wire_struct! {
    pub struct ObHciState { pub state: u32, pub bd_addr: [u8; 6], pub reserved: [u8; 2] }
}

// ---- brightness (drivers/brightness, bridges/brightness) --------------------------------------
pub const OB_BRIGHT_SET: u16 = 1; // guest -> host: struct ob_bright_set
pub const OB_BRIGHT_QUERY: u16 = 2; // guest -> host: no payload; the host answers OB_BRIGHT_STATE
pub const OB_BRIGHT_STATE: u16 = 3; // host -> guest: struct ob_bright_state
pub const OB_BRIGHT_ALS: u16 = 4; // host -> guest: struct ob_bright_als, when the light changes and every 2 s
pub const OB_BRIGHT_POWER: u16 = 5; // guest -> host: struct ob_bright_power, iOS's display on or off
pub const OB_BRIGHT_LINEAR: u32 = 1; // ob_bright_set.flags: the level is a fraction of the light, not of the slider
pub const OB_BRIGHT_HAS_PANEL: u32 = 1; // ob_bright_state.flags
pub const OB_BRIGHT_HAS_ALS: u32 = 2;
wire_struct! {
    pub struct ObBrightSet {
        /// 0..65535 of the panel's range for iOS; 0 is the lowest level, not off
        pub level: u32,
        /// the host walks to the level in this time; 0 = at once
        pub fade_ms: u32,
        /// OB_BRIGHT_LINEAR
        pub flags: u32,
    }
}
wire_struct! {
    pub struct ObBrightState {
        /// OB_BRIGHT_HAS_*
        pub flags: u32,
        /// the light at level 65535
        pub max_nits: u32,
        /// the light at level 0
        pub min_millinits: u32,
        /// where the panel is now on the slider's scale, 0..65535
        pub level: u32,
    }
}
/// ob_bright_power.source: IOMobileFramebufferRequestPowerChange (selector 12). The touch IC sleeps while the display
/// is off and wakes when it is on; a brightness of 0 alone is a dim panel, not off.
pub const OB_POWER_FRAMEBUFFER: u32 = 1;
wire_struct! {
    pub struct ObBrightPower {
        /// 0 off, 1 on
        pub on: u32,
        /// OB_POWER_*
        pub source: u32,
    }
}
wire_struct! {
    pub struct ObBrightAls { pub millilux: u32, pub clear: u32, pub ir: u32, pub uv: u32 }
}

// ---- battery (drivers/battery, bridges/battery) -----------------------------------------------
pub const OB_BATTERY_QUERY: u16 = 1; // guest -> host: no payload; the host answers OB_BATTERY_STATE, then reports changes
pub const OB_BATTERY_STATE: u16 = 2; // host -> guest: struct ob_battery_state
pub const OB_BATTERY_PRESENT: u32 = 1; // ob_battery_state.flags
pub const OB_BATTERY_PLUGGED: u32 = 2;
pub const OB_BATTERY_CHARGING: u32 = 4;
pub const OB_BATTERY_FULL: u32 = 8;
pub const OB_BATTERY_UNKNOWN: i32 = i32::MIN; // temp_deci_c, current_ma
wire_struct! {
    pub struct ObBatteryState {
        /// OB_BATTERY_*
        pub flags: u32,
        /// 0..100
        pub percent: u32,
        /// tenths of a degree Celsius, or OB_BATTERY_UNKNOWN
        pub temp_deci_c: i32,
        /// 0 = unknown
        pub voltage_mv: u32,
        /// into the battery > 0, out of it < 0, or OB_BATTERY_UNKNOWN
        pub current_ma: i32,
        /// never 0
        pub full_mah: u32,
        pub design_mah: u32,
        /// 0 = unknown
        pub cycles: u32,
        /// to empty, or to full while charging; -1 = unknown
        pub minutes: i32,
    }
}

// ---- thermal (drivers/thermal, bridges/thermal) -----------------------------------------------
pub const OB_THERMAL_QUERY: u16 = 1; // guest -> host: no payload; the host answers OB_THERMAL_STATE, then reports changes
pub const OB_THERMAL_STATE: u16 = 2; // host -> guest: struct ob_thermal_state, at once for a new level, else every 30 s
pub const OB_THERMAL_UNKNOWN: i32 = i32::MIN; // a reading the phone does not have
pub const OB_THERMAL_BY_CASE: u32 = 1; // ob_thermal_state.reason
pub const OB_THERMAL_BY_SOC: u32 = 2;
pub const OB_THERMAL_BY_BATTERY: u32 = 4;
wire_struct! {
    pub struct ObThermalState {
        /// iOS's thermal pressure level (OSThermalNotification.h): 0 nominal, 10 light, 20 moderate, 30 heavy,
        /// 40 trapping, 50 sleeping
        pub level: u32,
        /// OB_THERMAL_BY_* of the readings that set it
        pub reason: u32,
        /// what xnuhost does about it: 0 nothing, 1 vCPUs off the fastest cores, 2 on the slowest P cores and the E
        /// cores, 3 on the E cores only
        pub host_step: u32,
        /// tenths of a degree C, or OB_THERMAL_UNKNOWN
        pub case_temp: i32,
        pub soc: i32,
        pub cpu: i32,
        pub gpu: i32,
        pub ddr: i32,
        pub xo: i32,
        pub board: i32,
        pub battery: i32,
    }
}

/// A whole message: the header and its payload.
pub fn message(channel: u16, kind: u16, payload: &[u8]) -> Vec<u8> {
    use super::wire::Wire;
    let mut out = ObHeader { channel, kind, length: payload.len() as u32 }.to_bytes();
    out.extend_from_slice(payload);
    out
}
