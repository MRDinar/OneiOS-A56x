// SPDX-License-Identifier: MIT
//! The Wi-Fi station's messages between the guest's Wi-Fi driver (kernel/patches/oneios-hosted/net/OneiOSWifi.cpp)
//! and the Wi-Fi bridge, mirrored from drivers/net/oneios_wifi_proto.h (which stays, for the kernel's C; a test
//! checks every constant, size and offset against it). They travel in-band on the virtio-net queues as Ethernet
//! frames of their own type: the guest's frames of this type go to the bridge and never to the radio; the bridge's
//! come into the driver and never reach iOS's network stack. One message per frame, at most [`OW_MAX_PAYLOAD`]
//! bytes after the header. docs/hardware.md section 3.
//!
//! Frame to the host: dst [`OW_ADDRESS`] (02:4f:57:49:46:49), src the guest's MAC. Frame to the guest: dst the
//! guest's MAC (the transport's receive filter passes it), src [`OW_ADDRESS`]. Type [`OW_ETHERTYPE`], then
//! [`OwHeader`] and the payload. Little-endian, as both ends are.

use crate::proto::wire::{Le, Wire};
use crate::wire_struct;

pub const OW_ETHERTYPE: u16 = 0x88b5; // IEEE 802 local experimental 1: never on the air from here
pub const OW_MAGIC: u32 = 0x4f57_4946; // "OWIF"
pub const OW_VERSION: u32 = 1;
pub const OW_FRAME_MAX: usize = 1514;
pub const OW_MAX_PAYLOAD: usize = OW_FRAME_MAX - 14 - 8;
pub const OW_MAX_IES: usize = 1200; // information elements of one BSS kept (a beacon's are rarely more)
pub const OW_ADDRESS: [u8; 6] = [0x02, 0x4f, 0x57, 0x49, 0x46, 0x49];

// guest -> host
pub const OW_HELLO: u16 = 1; // OwHello: the host answers OW_STATE
pub const OW_SCAN: u16 = 2; // no payload: the host scans, sends OW_BSS for each network, then OW_SCAN_DONE
pub const OW_JOIN: u16 = 3; // OwJoin: the host answers OW_JOIN_RESULT (once it knows)
pub const OW_LEAVE: u16 = 4; // no payload: the host disconnects; OW_STATE follows
pub const OW_POWER: u16 = 5; // OwPower
                             // host -> guest
pub const OW_STATE: u16 = 16; // OwState: at once for a change, every 2 s while joined
pub const OW_BSS: u16 = 17; // OwBss + ies: one network of a scan
pub const OW_SCAN_DONE: u16 = 18; // OwScanDone
pub const OW_JOIN_RESULT: u16 = 19; // OwJoinResult
pub const OW_CHANNELS: u16 = 20; // OwChannels, then count OwChannel: the radio's channels (after OW_HELLO)

// OwState.radio
pub const OW_RADIO_NONE: u32 = 0;
pub const OW_RADIO_OK: u32 = 1;
// OwState.link: the station's state (as DroidVMWifi.inc's station numbers it)
pub const OW_LINK_OFF: u32 = 0;
pub const OW_LINK_IDLE: u32 = 1;
pub const OW_LINK_SCANNING: u32 = 2;
pub const OW_LINK_AUTH: u32 = 3;
pub const OW_LINK_ASSOC: u32 = 4;
pub const OW_LINK_KEYS: u32 = 5;
pub const OW_LINK_UP: u32 = 6;

pub const OW_SECURITY_OPEN: u8 = 0;
pub const OW_SECURITY_PSK: u8 = 1;
pub const OW_SECURITY_SAE: u8 = 2;
pub const OW_SECURITY_AUTO: u8 = 3; // the host picks from the network's RSN element

// OwJoinResult.status: 0 joined; else why not
pub const OW_JOIN_OK: u32 = 0;
pub const OW_JOIN_FAILED: u32 = 1;
pub const OW_JOIN_NO_NETWORK: u32 = 2;
pub const OW_JOIN_AUTH: u32 = 3;
pub const OW_JOIN_TIMEOUT: u32 = 4;
pub const OW_JOIN_NO_RADIO: u32 = 5;
pub const OW_JOIN_REJECTED: u32 = 6;

// OwChannel.flags
pub const OW_CH_PASSIVE: u8 = 1;
pub const OW_CH_RADAR: u8 = 2;
pub const OW_CH_INDOOR: u8 = 4;
pub const OW_CH_NO_HT40: u8 = 8;
pub const OW_CH_NO_80MHZ: u8 = 16;
pub const OW_MAX_CHANNELS: usize = 128;

impl Le for i16 {
    const SIZE: usize = 2;
    fn put(self, out: &mut [u8]) {
        out[..2].copy_from_slice(&self.to_le_bytes());
    }
    fn get(bytes: &[u8]) -> Self {
        i16::from_le_bytes([bytes[0], bytes[1]])
    }
}

wire_struct! {
    pub struct OwHeader {
        pub magic: u32, // OW_MAGIC
        pub kind: u16, // OW_*
        pub length: u16, // payload bytes
    }
}

wire_struct! {
    pub struct OwHello { pub version: u32 }
}

wire_struct! {
    /// A network as a scan saw it. ies[ie_length] follow the struct: the information elements as the beacon or
    /// probe response had them (the SSID element first is not promised).
    pub struct OwBss {
        pub bssid: [u8; 6],
        pub capability: u16, // 802.11 capability information
        pub freq_mhz: u32,
        pub signal_dbm: i16,
        pub beacon_interval: u16, // TU
        pub age_ms: u32, // since the radio last saw it
        pub ie_length: u16,
        pub ssid_length: u8,
        pub ssid: [u8; 32],
        pub pad: u8,
    }
}

impl Le for OwBss {
    const SIZE: usize = <OwBss as Wire>::SIZE;
    fn put(self, out: &mut [u8]) {
        self.write_to(out)
    }
    fn get(bytes: &[u8]) -> Self {
        OwBss::read_from(bytes)
    }
}

impl OwBss {
    /// The network's name as bytes.
    pub fn ssid(&self) -> &[u8] {
        &self.ssid[..(self.ssid_length as usize).min(32)]
    }
}

wire_struct! {
    pub struct OwState {
        pub version: u32, // OW_VERSION
        pub radio: u32, // OW_RADIO_*
        pub link: u32, // OW_LINK_*
        pub rssi_dbm: i32, // joined: the signal; else 0
        pub noise_dbm: i32,
        pub tx_rate_mbps: u32, // joined: the last transmit rate
        pub reason: u32, // why the link went down (802.11 reason code), 0 if it did not
        pub mac: [u8; 6], // the radio's address (wlan0's): the guest's en0 must send with it
        pub country: [u8; 2], // the regulatory country, "00" when unknown
        pub bss: OwBss, // joined: the network (its ies follow the struct, ie_length of them)
    }
}

/// A join: the network and its key ([`OwJoin::wipe`] zeroes it once the radio has it).
#[derive(Clone, Copy, PartialEq, Eq)]
pub struct OwJoin {
    pub ssid_length: u8,
    pub ssid: [u8; 32],
    pub bssid: [u8; 6], // 00:00:00:00:00:00: any BSS of the network
    pub security: u8,   // OW_SECURITY_*
    pub key_length: u8, // 0, 8..63 (a passphrase), or 32 with key_raw (a PSK)
    pub key_raw: u8,    // 1: key is the 32-byte PSK itself
    pub pad: [u8; 2],
    pub key: [u8; 64],
}

impl Default for OwJoin {
    fn default() -> Self {
        OwJoin {
            ssid_length: 0,
            ssid: [0; 32],
            bssid: [0; 6],
            security: 0,
            key_length: 0,
            key_raw: 0,
            pad: [0; 2],
            key: [0; 64],
        }
    }
}

impl std::fmt::Debug for OwJoin {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("OwJoin").field("ssid_length", &self.ssid_length).field("security", &self.security).finish()
    }
}

impl OwJoin {
    /// A join of `ssid` with `key` (a passphrase, or the raw PSK with `raw`).
    pub fn new(ssid: &[u8], security: u8, key: &[u8], raw: bool) -> OwJoin {
        let mut j =
            OwJoin { ssid_length: ssid.len().min(32) as u8, security, key_raw: u8::from(raw), ..Default::default() };
        j.ssid[..j.ssid_length as usize].copy_from_slice(&ssid[..j.ssid_length as usize]);
        j.key_length = key.len().min(64) as u8;
        j.key[..j.key_length as usize].copy_from_slice(&key[..j.key_length as usize]);
        j
    }
    pub fn ssid(&self) -> &[u8] {
        &self.ssid[..(self.ssid_length as usize).min(32)]
    }
    pub fn key(&self) -> &[u8] {
        &self.key[..(self.key_length as usize).min(64)]
    }
    /// Zeroes it (the key).
    pub fn wipe(&mut self) {
        for b in self.key.iter_mut() {
            // SAFETY: a valid &mut u8; volatile so the zeroing stays.
            unsafe { std::ptr::write_volatile(b, 0) };
        }
        *self = OwJoin::default();
    }
}

impl Wire for OwJoin {
    const SIZE: usize = 108;
    fn layout() -> Vec<(&'static str, usize, usize)> {
        vec![
            ("ssid_length", 0, 1),
            ("ssid", 1, 32),
            ("bssid", 33, 6),
            ("security", 39, 1),
            ("key_length", 40, 1),
            ("key_raw", 41, 1),
            ("pad", 42, 2),
            ("key", 44, 64),
        ]
    }
    fn write_to(&self, out: &mut [u8]) {
        out[0] = self.ssid_length;
        out[1..33].copy_from_slice(&self.ssid);
        out[33..39].copy_from_slice(&self.bssid);
        out[39] = self.security;
        out[40] = self.key_length;
        out[41] = self.key_raw;
        out[42..44].copy_from_slice(&self.pad);
        out[44..108].copy_from_slice(&self.key);
    }
    fn read_from(b: &[u8]) -> Self {
        OwJoin {
            ssid_length: b[0],
            ssid: b[1..33].try_into().expect("32 bytes"),
            bssid: b[33..39].try_into().expect("6 bytes"),
            security: b[39],
            key_length: b[40],
            key_raw: b[41],
            pad: [b[42], b[43]],
            key: b[44..108].try_into().expect("64 bytes"),
        }
    }
}

wire_struct! {
    pub struct OwJoinResult {
        pub status: u32,
        pub code: u32, // the 802.11 status or reason when there is one
    }
}

wire_struct! {
    /// A channel the radio may use, as the phone's regulatory domain allows it (nl80211's frequency attributes;
    /// disabled channels are left out). The guest answers APPLE80211_IOC_CHANNELS_INFO and the supported channel
    /// lists from these.
    pub struct OwChannel { pub freq_mhz: u16, pub flags: u8, pub pad: u8 }
}

wire_struct! {
    pub struct OwChannels { pub count: u16, pub pad: u16 }
}

wire_struct! {
    pub struct OwPower { pub on: u32 }
}

wire_struct! {
    pub struct OwScanDone { pub count: u32, pub error: i32 }
}

/// A station message as a frame: `dst`, `src`, the type, the header, then the payload's parts.
pub fn frame(dst: &[u8; 6], src: &[u8; 6], kind: u16, parts: &[&[u8]]) -> Option<Vec<u8>> {
    let length: usize = parts.iter().map(|p| p.len()).sum();
    if 14 + OwHeader::SIZE + length > OW_FRAME_MAX {
        return None;
    }
    let mut f = Vec::with_capacity(14 + OwHeader::SIZE + length);
    f.extend_from_slice(dst);
    f.extend_from_slice(src);
    f.extend_from_slice(&OW_ETHERTYPE.to_be_bytes());
    f.extend_from_slice(&OwHeader { magic: OW_MAGIC, kind, length: length as u16 }.to_bytes());
    for p in parts {
        f.extend_from_slice(p);
    }
    Some(f)
}

/// A station message's header and payload in a frame of the station's type (None: not one, or cut short).
pub fn message(frame: &[u8]) -> Option<(OwHeader, &[u8])> {
    let h = OwHeader::parse(frame.get(14..)?)?;
    let start = 14 + OwHeader::SIZE;
    (h.magic == OW_MAGIC && start + h.length as usize <= frame.len())
        .then(|| (h, &frame[start..start + h.length as usize]))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::proto::cheader::{check_struct, check_values, repo_file, Header};

    #[test]
    fn wifi_proto_matches_the_c_header() {
        let h = Header::read(&repo_file("drivers/net/oneios_wifi_proto.h"));
        #[rustfmt::skip]
        let values: &[(&str, i64)] = &[
            ("OW_ETHERTYPE", OW_ETHERTYPE.into()), ("OW_MAGIC", OW_MAGIC.into()), ("OW_VERSION", OW_VERSION.into()),
            ("OW_FRAME_MAX", OW_FRAME_MAX as i64), ("OW_MAX_PAYLOAD", OW_MAX_PAYLOAD as i64), ("OW_MAX_IES", OW_MAX_IES as i64),
            ("OW_HELLO", OW_HELLO.into()), ("OW_SCAN", OW_SCAN.into()), ("OW_JOIN", OW_JOIN.into()), ("OW_LEAVE", OW_LEAVE.into()),
            ("OW_POWER", OW_POWER.into()), ("OW_STATE", OW_STATE.into()), ("OW_BSS", OW_BSS.into()),
            ("OW_SCAN_DONE", OW_SCAN_DONE.into()), ("OW_JOIN_RESULT", OW_JOIN_RESULT.into()), ("OW_CHANNELS", OW_CHANNELS.into()),
            ("OW_RADIO_NONE", OW_RADIO_NONE.into()), ("OW_RADIO_OK", OW_RADIO_OK.into()),
            ("OW_LINK_OFF", OW_LINK_OFF.into()), ("OW_LINK_IDLE", OW_LINK_IDLE.into()), ("OW_LINK_SCANNING", OW_LINK_SCANNING.into()),
            ("OW_LINK_AUTH", OW_LINK_AUTH.into()), ("OW_LINK_ASSOC", OW_LINK_ASSOC.into()), ("OW_LINK_KEYS", OW_LINK_KEYS.into()),
            ("OW_LINK_UP", OW_LINK_UP.into()),
            ("OW_SECURITY_OPEN", OW_SECURITY_OPEN.into()), ("OW_SECURITY_PSK", OW_SECURITY_PSK.into()),
            ("OW_SECURITY_SAE", OW_SECURITY_SAE.into()), ("OW_SECURITY_AUTO", OW_SECURITY_AUTO.into()),
            ("OW_JOIN_OK", OW_JOIN_OK.into()), ("OW_JOIN_FAILED", OW_JOIN_FAILED.into()), ("OW_JOIN_NO_NETWORK", OW_JOIN_NO_NETWORK.into()),
            ("OW_JOIN_AUTH", OW_JOIN_AUTH.into()), ("OW_JOIN_TIMEOUT", OW_JOIN_TIMEOUT.into()), ("OW_JOIN_NO_RADIO", OW_JOIN_NO_RADIO.into()),
            ("OW_JOIN_REJECTED", OW_JOIN_REJECTED.into()),
            ("OW_CH_PASSIVE", OW_CH_PASSIVE.into()), ("OW_CH_RADAR", OW_CH_RADAR.into()), ("OW_CH_INDOOR", OW_CH_INDOOR.into()),
            ("OW_CH_NO_HT40", OW_CH_NO_HT40.into()), ("OW_CH_NO_80MHZ", OW_CH_NO_80MHZ.into()),
            ("OW_MAX_CHANNELS", OW_MAX_CHANNELS as i64),
        ];
        check_values(&h, &["OW_"], values);
        check_struct::<OwHeader>(&h, "ow_header", &["magic", "type", "length"]);
        check_struct::<OwHello>(&h, "ow_hello", &["version"]);
        #[rustfmt::skip]
        check_struct::<OwBss>(&h, "ow_bss", &["bssid", "capability", "freq_mhz", "signal_dbm", "beacon_interval", "age_ms",
                                              "ie_length", "ssid_length", "ssid", "pad"]);
        #[rustfmt::skip]
        check_struct::<OwState>(&h, "ow_state", &["version", "radio", "link", "rssi_dbm", "noise_dbm", "tx_rate_mbps", "reason",
                                                  "mac", "country", "bss"]);
        #[rustfmt::skip]
        check_struct::<OwJoin>(&h, "ow_join", &["ssid_length", "ssid", "bssid", "security", "key_length", "key_raw", "pad", "key"]);
        check_struct::<OwJoinResult>(&h, "ow_join_result", &["status", "code"]);
        check_struct::<OwChannel>(&h, "ow_channel", &["freq_mhz", "flags", "pad"]);
        check_struct::<OwChannels>(&h, "ow_channels", &["count", "pad"]);
        check_struct::<OwPower>(&h, "ow_power", &["on"]);
        check_struct::<OwScanDone>(&h, "ow_scan_done", &["count", "error"]);
        let text = std::fs::read_to_string(repo_file("drivers/net/oneios_wifi_proto.h")).unwrap();
        assert!(text.contains("ow_address[6] = { 0x02, 0x4f, 0x57, 0x49, 0x46, 0x49 }"));
    }

    #[test]
    fn frames_and_messages() {
        let mac = [2, 0x11, 0x22, 0x33, 0x44, 0x55];
        let f = frame(&mac, &OW_ADDRESS, OW_JOIN_RESULT, &[&OwJoinResult { status: 3, code: 15 }.to_bytes()]).unwrap();
        assert_eq!(f.len(), 14 + 8 + 8);
        assert_eq!(&f[12..14], &[0x88, 0xb5]);
        let (h, p) = message(&f).unwrap();
        assert!(h.kind == OW_JOIN_RESULT && OwJoinResult::parse(p).unwrap().code == 15);
        assert!(message(&f[..20]).is_none());
        assert!(frame(&mac, &OW_ADDRESS, OW_BSS, &[&[0; 1500]]).is_none());
    }
}
