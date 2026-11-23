// SPDX-License-Identifier: MIT
//! A radio that is not there, for a boot with no phone (xnuhost wifi=sim, a PC or the Mac's container): the guest's
//! Wi-Fi driver and wifid get a station that finds two made-up networks and joins them, so the guest's side (the
//! Apple80211 requests, the scan records, a join's result and events) can be run and read in the log without wlan0,
//! a supplicant or a network. No frame goes anywhere: a joined network has no DHCP server behind it.
//!   "OneiOS open"   no security: joins
//!   "OneiOS WPA2"   WPA2-PSK, CCMP: joins with the passphrase "password" (or any 32-byte key); another one is a
//!                   refused key (802.11 status 15)
//! C: wifi/sim.c.

use super::frames::ethertype;
use super::proto::*;
use super::station::{Bss, Link, Radio, Station};
use super::CONTROL_TYPE;
use crate::br_info;
use crate::net::{NetBackend, NetConfig, NetOpen, RxFn};
use std::sync::{Arc, Mutex, OnceLock, PoisonError};

const OPEN_IES: [u8; 23] = [
    0, 11, b'O', b'n', b'e', b'i', b'O', b'S', b' ', b'o', b'p', b'e', b'n', 1, 8, 0x82, 0x84, 0x8b, 0x96, 0x24, 0x30,
    0x48, 0x6c,
];
const WPA2_IES: [u8; 45] = [
    0, 11, b'O', b'n', b'e', b'i', b'O', b'S', b' ', b'W', b'P', b'A', b'2', 1, 8, 0x8c, 0x12, 0x98, 0x24, 0xb0, 0x48,
    0x60, 0x6c, 48, 20, 1, 0, 0x00, 0x0f, 0xac, 4, 1, 0, 0x00, 0x0f, 0xac, 4, 1, 0, 0x00, 0x0f, 0xac, 2, 0x0c, 0,
];

fn network(bssid_last: u8, capability: u16, freq_mhz: u32, signal_dbm: i16, name: &[u8; 11]) -> OwBss {
    let mut ssid = [0u8; 32];
    ssid[..11].copy_from_slice(name);
    OwBss {
        bssid: [0x02, 0x4f, 0x53, 0x00, 0x00, bssid_last],
        capability,
        freq_mhz,
        signal_dbm,
        beacon_interval: 100,
        ssid_length: 11,
        ssid,
        ..Default::default()
    }
}

#[derive(Default)]
struct SimRadio {
    state: Mutex<(bool, Bss)>, // joined, the joined network
}

impl Radio for SimRadio {
    fn scan(&self, st: &Station) -> Result<(), i32> {
        st.scan_add(&network(1, 0x0401, 2437, -48, b"OneiOS open"), &OPEN_IES);
        st.scan_add(&network(2, 0x0411, 5180, -55, b"OneiOS WPA2"), &WPA2_IES);
        st.scan_done(0);
        Ok(())
    }

    fn join(&self, st: &Station, j: &OwJoin, bss: Option<&Bss>) -> Result<(), i32> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner).0 = false;
        let Some(bss) = bss else {
            st.join_done(OW_JOIN_NO_NETWORK, 0);
            return Ok(());
        };
        let secured = bss.bss.capability & 0x10 != 0;
        let key = (j.key_raw != 0 && j.key_length == 32) || j.key() == b"password";
        br_info!(
            "wifi: sim: join of a network name of {} bytes, {}, with {}",
            j.ssid_length,
            if secured { "WPA2" } else { "open" },
            if j.key_raw != 0 {
                "a 32-byte key"
            } else if j.key_length != 0 {
                "a passphrase"
            } else {
                "no key"
            }
        );
        if secured && !key {
            st.join_done(OW_JOIN_AUTH, 15);
            return Ok(());
        }
        *self.state.lock().unwrap_or_else(PoisonError::into_inner) = (true, bss.clone());
        st.join_done(OW_JOIN_OK, 0);
        Ok(())
    }

    fn leave(&self, _st: &Station) -> Result<(), i32> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner).0 = false;
        Ok(())
    }

    fn link(&self) -> Result<Link, i32> {
        let s = self.state.lock().unwrap_or_else(PoisonError::into_inner);
        let mut l = Link { up: s.0, reason: 3, ..Default::default() };
        if s.0 {
            l.rssi_dbm = s.1.bss.signal_dbm.into();
            l.noise_dbm = -95;
            l.tx_rate_mbps = 433;
            l.freq_mhz = s.1.bss.freq_mhz;
            l.bssid = s.1.bss.bssid;
        }
        Ok(l)
    }

    /// The made-up radio's channels: 2.4 GHz 1 to 13, 5 GHz 36 to 165 with the DFS ones (52 to 144) passive and
    /// radar.
    fn channels(&self, max: usize) -> Option<Result<Vec<OwChannel>, i32>> {
        const FIVE: [u16; 25] = [
            36, 40, 44, 48, 52, 56, 60, 64, 100, 104, 108, 112, 116, 120, 124, 128, 132, 136, 140, 144, 149, 153, 157,
            161, 165,
        ];
        let two = (1..=13u16).map(|c| OwChannel { freq_mhz: 2407 + 5 * c, ..Default::default() });
        let five = FIVE.iter().map(|&c| OwChannel {
            freq_mhz: 5000 + 5 * c,
            flags: if (52..=144).contains(&c) { OW_CH_PASSIVE | OW_CH_RADAR } else { 0 },
            pad: 0,
        });
        Some(Ok(two.chain(five).take(max).collect()))
    }
}

/// The guest's frames: the station's messages; anything else goes nowhere.
struct SimNet {
    station: Arc<Station>,
}

impl NetBackend for SimNet {
    fn send(&self, frame: &[u8]) -> Result<(), i32> {
        if ethertype(frame) == CONTROL_TYPE {
            self.station.message(frame);
        }
        Ok(())
    }
}

/// The made-up radio as the virtio-net device's back end. `mac`: the guest's en0 address.
pub fn prepare(mac: [u8; 6]) -> Option<NetOpen> {
    let rx: Arc<OnceLock<RxFn>> = Arc::default();
    let to = rx.clone();
    let station = Arc::new(Station::new(
        Box::new(SimRadio::default()),
        mac,
        Some(*b"XX"),
        Box::new(move |f: &[u8]| {
            if let Some(rx) = to.get() {
                let _ = rx(f);
            }
        }),
    ));
    br_info!("wifi: sim: a made-up radio with two networks (wifi=sim): no frame goes anywhere");
    Some(Box::new(move |_c: &NetConfig, guest: RxFn| {
        let _ = rx.set(guest);
        Some(Box::new(SimNet { station }) as Box<dyn NetBackend>)
    }))
}

#[cfg(test)]
mod tests {
    use super::super::frames::tests::MAC;
    use super::super::station::guest_frame;
    use super::super::supplicant::security_parse;
    use super::*;
    use crate::proto::wire::Wire;

    fn host_message(s: &Mutex<Vec<Vec<u8>>>, i: usize) -> (u16, Vec<u8>) {
        let s = s.lock().unwrap();
        let Some(f) = s.get(i) else { return (0, Vec::new()) };
        match message(f) {
            Some((h, p)) if f[..6] == MAC && f[6..12] == OW_ADDRESS => (h.kind, p.to_vec()),
            _ => (0, Vec::new()),
        }
    }

    // test_wifi.c test_sim: the made-up radio (wifi=sim) through its back-end calls
    #[test]
    fn made_up_radio() {
        let s: Arc<Mutex<Vec<Vec<u8>>>> = Arc::default();
        let t = s.clone();
        let open = prepare(MAC).unwrap();
        let net = open(
            &NetConfig::default(),
            Arc::new(move |f: &[u8]| {
                t.lock().unwrap().push(f.to_vec());
                true
            }),
        )
        .unwrap();
        let send = |kind, payload: &[u8]| net.send(&guest_frame(&MAC, kind, payload));
        assert!(send(OW_HELLO, &OwHello { version: OW_VERSION }.to_bytes()).is_ok());
        assert!(s.lock().unwrap().len() == 2 && host_message(&s, 0).0 == OW_STATE);
        // Then the radio's channels: 13 of 2.4 GHz and 25 of 5 GHz, the DFS ones passive and radar.
        let (kind, p) = host_message(&s, 1);
        assert!(kind == OW_CHANNELS && p.len() == OwChannels::SIZE + 38 * OwChannel::SIZE);
        let ch = |i: usize| OwChannel::parse(&p[OwChannels::SIZE + i * 4..]).unwrap();
        assert_eq!(OwChannels::parse(&p).unwrap().count, 38);
        assert!(ch(0).freq_mhz == 2412 && ch(0).flags == 0);
        assert!(ch(17).freq_mhz == 5260 && ch(17).flags == OW_CH_PASSIVE | OW_CH_RADAR); // the 18th: 5 GHz channel 52
        assert!(ch(37).freq_mhz == 5825 && ch(37).flags == 0);
        s.lock().unwrap().clear();
        send(OW_SCAN, &[]).unwrap();
        assert!(
            s.lock().unwrap().len() == 3 && host_message(&s, 0).0 == OW_BSS && host_message(&s, 2).0 == OW_SCAN_DONE
        );
        let (kind, p) = host_message(&s, 1);
        assert_eq!(kind, OW_BSS);
        let b = OwBss::parse(&p).unwrap();
        let sec = security_parse(&p[OwBss::SIZE..OwBss::SIZE + b.ie_length as usize]);
        assert!(b.ssid() == b"OneiOS WPA2" && sec.rsn && sec.psk && sec.ccmp && b.capability & 0x10 != 0);
        // The secured network: a wrong passphrase is a refused key, the right one joins; a data frame goes nowhere.
        let mut j = OwJoin::new(b"OneiOS WPA2", OW_SECURITY_AUTO, b"12345678", false);
        s.lock().unwrap().clear();
        send(OW_JOIN, &j.to_bytes()).unwrap();
        let (kind, p) = host_message(&s, 0);
        assert!(s.lock().unwrap().len() == 1 && kind == OW_JOIN_RESULT);
        assert_eq!(OwJoinResult::parse(&p).unwrap(), OwJoinResult { status: OW_JOIN_AUTH, code: 15 });
        j.key[..8].copy_from_slice(b"password");
        s.lock().unwrap().clear();
        send(OW_JOIN, &j.to_bytes()).unwrap();
        let (kind, p) = host_message(&s, 0);
        assert!(s.lock().unwrap().len() == 2 && kind == OW_STATE);
        let st = OwState::parse(&p).unwrap();
        assert!(st.link == OW_LINK_UP && st.bss.freq_mhz == 5180 && st.rssi_dbm == -55);
        let (kind, p) = host_message(&s, 1);
        assert!(kind == OW_JOIN_RESULT && OwJoinResult::parse(&p).unwrap().status == OW_JOIN_OK);
        s.lock().unwrap().clear();
        assert!(net.send(&[0xff; 60]).is_ok() && s.lock().unwrap().is_empty());
        send(OW_JOIN, &OwJoin::new(b"OneiOS open", OW_SECURITY_OPEN, b"", false).to_bytes()).unwrap();
        let (kind, p) = host_message(&s, 1);
        assert!(
            s.lock().unwrap().len() == 2
                && kind == OW_JOIN_RESULT
                && OwJoinResult::parse(&p).unwrap().status == OW_JOIN_OK
        );
        s.lock().unwrap().clear();
        send(OW_LEAVE, &[]).unwrap();
        let (kind, p) = host_message(&s, 0);
        assert!(s.lock().unwrap().len() == 1 && kind == OW_STATE && OwState::parse(&p).unwrap().link == OW_LINK_IDLE);
    }
}
