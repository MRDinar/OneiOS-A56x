// SPDX-License-Identifier: MIT
//! `wifictl wonder` (docs/hardware.md 4a): a session on Google's Wonder radio, the raw 802.11 path One UI's Wi-Fi
//! driver offers beside wlan0, with AWDL's frames counted. The order is Samsung's Wonder 1.5 (S928BXXU5DZDP,
//! vendor/qcom/opensource/wlan/wonder, GPL-2.0): Wonder caches the frequency, the TX rate, the BSSID and the country
//! and needs all four before its interface comes up; the interface's up (mac80211's start) calls the provider's init
//! in qcacld (wlan_hdd_wondertap.c), which opens the PASSTHRU vdev wondertap0 beside wlan0 and sets the driver's
//! country to the one given; its down calls deinit, which closes the vdev.
//!   1. wlan0's link before; every radio; refused without Wonder, with Wonder in use (another interface on it, a
//!      wondertap vdev up), without wlan0, without a known country, on a frequency that is no AWDL channel
//!   2. a monitor interface on Wonder's wiphy (NL80211_CMD_NEW_INTERFACE)
//!   3. SET_REGULATORY (the station radio's own country), SET_FREQUENCY (20 MHz), SET_FIXED_TX_RATE (HT MCS 0),
//!      SET_FILTER (AWDL's BSSID)
//!   4. the interface up; GET_IF_MAC_ADDR (Wonder answers it only when up)
//!   5. AF_PACKET on it: frames counted for the seconds asked, AWDL's by kind and by peer
//!   6. in reverse, also after any failed step: frames closed, the interface down (deinit), the interface removed
//!   7. wlan0's link after, against the link before (-ENOLINK when it was associated and is not again within 10 s)
//!
//! The steps are pure over [`WonderOps`] (wlan has the phone's); the tests run them on a stand-in. C: wifi/wonder.c.

use super::nl;
use super::p2p::{iftype_name, wonder_subcmd_name, Radios, OUI_GOOGLE, WONDER_IFNAME};
use super::station::Link;
use crate::hal::binder::strerror;
use crate::log::Level;
use crate::{br_error, br_info, br_log};
use std::fmt::Write;
use std::sync::atomic::{AtomicBool, Ordering};
use xh_core::bytes::le;

const APPLE_OUI: [u8; 3] = [0x00, 0x17, 0xf2];
/// Every AWDL data frame's addr3 (OWL).
pub const AWDL_BSSID: [u8; 6] = [0x00, 0x25, 0x00, 0xff, 0x94, 0x73];
pub const AWDL_PEERS: usize = 16;

// ---- the frames ---------------------------------------------------------------------------------
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum AwdlKind {
    #[default]
    Not,
    /// An action frame of category 127 under Apple's OUI 00:17:f2 with AWDL's type 8.
    Action,
    /// A data frame to AWDL's BSSID 00:25:00:ff:94:73.
    Data,
    OtherAction,
}

/// One frame Wonder's monitor interface gave (radiotap, then 802.11).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct AwdlFrame {
    pub kind: AwdlKind,
    pub frame_type: u8,  // 802.11: 0 management, 1 control, 2 data, 3 extension
    pub subtype: u8,     // an AWDL action frame's subtype (0 PSF, 3 MIF)
    pub source: [u8; 6], // addr2
    pub tsft: bool,
    pub bad_fcs: bool,
    pub signal_dbm: Option<i8>,
}

/// None: not a radiotap frame with a whole 802.11 header (counted as unparsed).
pub fn radiotap_frame(f: &[u8]) -> Option<AwdlFrame> {
    let n = f.len();
    if n < 8 || f[0] != 0 {
        return None; // radiotap version 0
    }
    let rt = (f[2] as usize) | (f[3] as usize) << 8;
    if !(8..=n).contains(&rt) {
        return None;
    }
    let present = le::u32(f, 4);
    let mut at = 8;
    let mut p = present;
    while p & 0x8000_0000 != 0 {
        // the fields come after every present word
        if at + 4 > rt {
            return None;
        }
        p = le::u32(f, at);
        at += 4;
    }
    // The first word's fields 0 to 5, in bit order, each aligned to its size from the header's start: TSFT (u64),
    // FLAGS, RATE, CHANNEL (u16 frequency, u16 flags), FHSS, DBM_ANTSIGNAL (s8).
    const ALIGN: [usize; 6] = [8, 1, 1, 2, 1, 1];
    const SIZE: [usize; 6] = [8, 1, 1, 4, 2, 1];
    let mut out = AwdlFrame::default();
    let mut flags = 0u8;
    for b in 0..6 {
        if present >> b & 1 == 0 {
            continue;
        }
        at = (at + ALIGN[b] - 1) & !(ALIGN[b] - 1);
        if at + SIZE[b] > rt {
            return None;
        }
        match b {
            0 => out.tsft = true,
            1 => flags = f[at],
            5 => out.signal_dbm = Some(f[at] as i8),
            _ => {}
        }
        at += SIZE[b];
    }
    out.bad_fcs = flags & 0x40 != 0;
    let h = &f[rt..];
    let mut len = n - rt;
    if flags & 0x10 != 0 && len >= 4 {
        len -= 4; // the FCS at the end
    }
    if len < 10 {
        return None; // frame control, duration, addr1: the shortest (an ACK)
    }
    out.frame_type = h[0] >> 2 & 3;
    if out.frame_type == 1 {
        return Some(out); // control: no source we count
    }
    if len < 24 {
        return None;
    }
    out.source.copy_from_slice(&h[10..16]);
    let subtype = h[0] >> 4;
    if out.frame_type == 0 && (subtype == 13 || subtype == 14) {
        // action, action no-ack: AWDL's action header (OWL's struct awdl_action): category 127, Apple's OUI, type 8,
        // version, subtype
        let body = &h[24..len];
        if body.len() >= 7 && body[0] == 127 && body[1..4] == APPLE_OUI && body[4] == 8 {
            out.kind = AwdlKind::Action;
            out.subtype = body[6];
        } else {
            out.kind = AwdlKind::OtherAction;
        }
    } else if out.frame_type == 2 && h[1] & 3 == 0 && h[16..22] == AWDL_BSSID {
        out.kind = AwdlKind::Data;
    }
    Some(out)
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct AwdlPeer {
    pub mac: [u8; 6],
    pub action: u32,
    pub data: u32,
    pub best_dbm: Option<i8>,
}

/// The frames counted: how many are AWDL's, and from whom.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct AwdlCount {
    pub frames: u64,
    pub unparsed: u64,
    pub bad_fcs: u64,
    pub with_tsft: u64,
    pub management: u64,
    pub control: u64,
    pub data: u64,
    pub action: u64,
    pub awdl_action: u64,
    pub awdl_psf: u64,
    pub awdl_mif: u64,
    pub awdl_other: u64,
    pub awdl_data: u64,
    pub peers: Vec<AwdlPeer>,
    pub peers_dropped: u32,
}

impl AwdlCount {
    fn peer(&mut self, f: &AwdlFrame) {
        let i = match self.peers.iter().position(|p| p.mac == f.source) {
            Some(i) => i,
            None if self.peers.len() == AWDL_PEERS => {
                self.peers_dropped += 1;
                return;
            }
            None => {
                self.peers.push(AwdlPeer { mac: f.source, ..Default::default() });
                self.peers.len() - 1
            }
        };
        let p = &mut self.peers[i];
        if f.kind == AwdlKind::Action {
            p.action += 1;
        } else {
            p.data += 1;
        }
        if let Some(s) = f.signal_dbm {
            if p.best_dbm.is_none_or(|b| s > b) {
                p.best_dbm = Some(s);
            }
        }
    }

    pub fn add(&mut self, frame: &[u8]) {
        self.frames += 1;
        let Some(f) = radiotap_frame(frame) else {
            self.unparsed += 1;
            return;
        };
        if f.bad_fcs {
            self.bad_fcs += 1;
            return;
        }
        self.with_tsft += u64::from(f.tsft);
        match f.frame_type {
            0 => self.management += 1,
            1 => self.control += 1,
            2 => self.data += 1,
            _ => {}
        }
        if matches!(f.kind, AwdlKind::Action | AwdlKind::OtherAction) {
            self.action += 1;
        }
        match f.kind {
            AwdlKind::Action => {
                self.awdl_action += 1;
                match f.subtype {
                    0 => self.awdl_psf += 1,
                    3 => self.awdl_mif += 1,
                    _ => self.awdl_other += 1,
                }
                self.peer(&f);
            }
            AwdlKind::Data => {
                self.awdl_data += 1;
                self.peer(&f);
            }
            _ => {}
        }
    }

    pub fn report(&self, seconds: u32) -> String {
        let mut o = String::new();
        let _ = writeln!(
            o,
            "frames: {} in {seconds} s: {} management ({} action), {} control, {} data; {} not parsed, {} with a bad FCS; \
             {} carry the radiotap TSFT",
            self.frames, self.management, self.action, self.control, self.data, self.unparsed, self.bad_fcs, self.with_tsft
        );
        let _ = writeln!(
            o,
            "AWDL: {} action frames ({} PSF, {} MIF, {} other subtypes), {} data frames to AWDL's BSSID, from {} peer(s){}",
            self.awdl_action,
            self.awdl_psf,
            self.awdl_mif,
            self.awdl_other,
            self.awdl_data,
            self.peers.len(),
            if self.peers_dropped > 0 { " (more were not kept)" } else { "" }
        );
        for p in &self.peers {
            let m = p.mac;
            let _ = write!(
                o,
                "AWDL peer: {:02x}:{:02x}:{:02x}:{:02x}:{:02x}:{:02x}: {} action, {} data",
                m[0], m[1], m[2], m[3], m[4], m[5], p.action, p.data
            );
            if let Some(b) = p.best_dbm {
                let _ = write!(o, ", best {b} dBm");
            }
            o.push('\n');
        }
        if self.frames == 0 {
            o.push_str("AWDL: no frame at all: Wonder's receive path gave nothing (the vdev's channel, the BSSID filter, the firmware)\n");
        } else if self.awdl_action == 0 {
            o.push_str(
                "AWDL: no AWDL frame: no Apple device sends AWDL on this channel now (an iPhone in AirDrop \"Everyone for 10 minutes\", its share sheet open)\n",
            );
        }
        let read = self.frames - self.unparsed - self.bad_fcs; // the frames whose radiotap was read and kept
        if self.with_tsft < read {
            let _ = writeln!(
                o,
                "radiotap: {} frames without the TSFT (an AWDL engine needs it for the sync)",
                read - self.with_tsft
            );
        }
        o
    }
}

// ---- the vendor data (nlattrs under NL80211_ATTR_VENDOR_DATA, Samsung's Wonder 1.5 and Google's wonder_ven_cmd.h) ---
/// SET_FREQUENCY: CHANNEL_FREQ (u32, MHz), CHANNEL_BANDWIDTH (u16: 20 MHz). None when it does not fit `size`.
pub fn set_frequency(size: usize, freq_mhz: u32) -> Option<Vec<u8>> {
    let mut w = nl::Writer::new(size);
    w.u32(1, freq_mhz).attr(2, &0u16.to_le_bytes());
    w.finish().map(<[u8]>::to_vec)
}

/// SET_REGULATORY: REG_COUNTRY_CODE, an NLA_NUL_STRING of at most 2.
pub fn set_regulatory(size: usize, country: &[u8; 2]) -> Option<Vec<u8>> {
    let mut w = nl::Writer::new(size);
    w.attr(1, &[country[0], country[1], 0]);
    w.finish().map(<[u8]>::to_vec)
}

/// SET_FIXED_TX_RATE: HT MCS 0, 1 stream, 20 MHz, 0.8 us.
pub fn set_fixed_tx_rate(size: usize) -> Option<Vec<u8>> {
    let mut w = nl::Writer::new(size);
    w.u32(1, 1) // PREAMBLE: HT
        .attr(2, &0u16.to_le_bytes()) // BW 20 MHz (policy u16; Wonder reads it as u32 over the zero pad)
        .u32(3, 2) // GI: 0.8 us
        .attr(4, &[1]) // NSS
        .attr(5, &[0]); // MCS
    w.finish().map(<[u8]>::to_vec)
}

/// SET_FILTER: TOP_FILTER_TYPE (u32: BSSID), TOP_FILTER_PARAMS nested { FILTER_BSSID_ENABLED u8 1, FILTER_BSSID_ADDR }.
pub fn set_bssid_filter(size: usize, bssid: &[u8; 6]) -> Option<Vec<u8>> {
    let mut inner = nl::Writer::new(24);
    inner.attr(1, &[1]).attr(2, bssid);
    let inner = inner.finish()?.to_vec();
    let mut w = nl::Writer::new(size);
    w.u32(1, 0).attr(2 | nl::NESTED, &inner);
    w.finish().map(<[u8]>::to_vec)
}

/// GET_IF_MAC_ADDR's reply (the attributes of the reply's VENDOR_DATA): IF_ADDR_MAC_ADDR.
pub fn mac_reply(data: &[u8]) -> Option<[u8; 6]> {
    nl::attrs(data).find(|(t, d)| *t == 1 && d.len() == 6).map(|(_, d)| d.try_into().expect("6 bytes"))
}

pub fn freq_ok(f: u32) -> bool {
    f == 2437 || f == 5220 || f == 5745
}

#[cfg(test)]
#[path = "test_wonder.rs"]
mod tests;

// ---- the session --------------------------------------------------------------------------------
/// What the session does on the phone; wlan has the phone's (nl80211, an ioctl, AF_PACKET), the tests a stand-in.
/// Errors are negative errnos.
pub trait WonderOps {
    /// Every wiphy and interface.
    fn radios(&mut self) -> Result<Radios, i32>;
    /// The radio's regulatory country (its two letters as the driver gives them).
    fn country(&mut self, wiphy: u32) -> Result<String, i32>;
    fn new_monitor(&mut self, wiphy: u32, name: &str) -> Result<u32, i32>;
    fn del_iface(&mut self, ifindex: u32) -> Result<(), i32>;
    fn set_up(&mut self, name: &str, up: bool) -> Result<(), i32>;
    /// A vendor command under Google's OUI: the reply's VENDOR_DATA (at most 64 bytes).
    fn vendor(&mut self, ifindex: u32, subcmd: u32, data: &[u8]) -> Result<Vec<u8>, i32>;
    /// A frame socket on the interface: its handle.
    fn open_frames(&mut self, ifindex: u32) -> Result<i32, i32>;
    /// A frame into `buffer`: its length, 0 when none came in `wait_ms`.
    fn receive(&mut self, handle: i32, buffer: &mut [u8], wait_ms: u32) -> Result<usize, i32>;
    fn close_frames(&mut self, handle: i32);
    /// up: associated, and its AP.
    fn link(&mut self, ifname: &str) -> Result<Link, i32>;
    fn now_ms(&mut self) -> u64;
    fn pause_ms(&mut self, ms: u32);
}

pub struct WonderParams<'a> {
    pub station: &'a str,             // wlan0
    pub name: &'a str,                // the monitor interface made on Wonder's radio (WONDER_IFNAME)
    pub freq_mhz: u32,                // an AWDL channel: 2437 (6), 5220 (44) or 5745 (149)
    pub seconds: u32,                 // how long frames are counted
    pub country: String,              // "": the station's radio's own (Wonder's init sets the driver's country to it)
    pub stop: Option<&'a AtomicBool>, // set by a signal: the count ends and the teardown runs
}

impl Default for WonderParams<'_> {
    fn default() -> Self {
        WonderParams {
            station: "wlan0",
            name: WONDER_IFNAME,
            freq_mhz: 2437,
            seconds: 30,
            country: String::new(),
            stop: None,
        }
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct WonderResult {
    pub count: AwdlCount,
    pub mac: Option<[u8; 6]>, // the PASSTHRU vdev's address (GET_IF_MAC_ADDR)
    pub opened: bool,
    pub link_before: bool,
    pub link_after: bool,
    pub before: Link,
    pub after: Link,
}

fn err(rc: Result<(), i32>) -> String {
    match rc {
        Ok(()) => "ok".to_owned(),
        Err(rc) => strerror(rc),
    }
}
fn level(rc: Result<(), i32>) -> Level {
    if rc.is_ok() {
        Level::Info
    } else {
        Level::Error
    }
}

fn say_link(when: &str, station: &str, l: &Result<Link, i32>) {
    match l {
        Err(rc) => br_error!("wonder: {station} {when}: its link not read: {}", strerror(*rc)),
        Ok(l) if !l.up => br_info!("wonder: {station} {when}: not associated"),
        Ok(l) => {
            let b = l.bssid;
            br_info!(
                "wonder: {station} {when}: associated to {:02x}:{:02x}:{:02x}:{:02x}:{:02x}:{:02x} on {} MHz, {} dBm, {} Mb/s",
                b[0],
                b[1],
                b[2],
                b[3],
                b[4],
                b[5],
                l.freq_mhz,
                l.rssi_dbm,
                l.tx_rate_mbps
            )
        }
    }
}

fn vendor(ops: &mut dyn WonderOps, ifindex: u32, subcmd: u32, data: &[u8], what: &str) -> Result<(), i32> {
    let rc = ops.vendor(ifindex, subcmd, data).map(|_| ());
    br_log!(
        level(rc),
        "wonder: {what} (vendor 00:1a:11 subcommand {subcmd} {}): {}",
        wonder_subcmd_name(subcmd),
        err(rc)
    );
    rc
}

/// The interface down (when it is up) and removed; a failed removal is the session's error.
fn remove_iface(ops: &mut dyn WonderOps, name: &str, ifindex: u32, down: bool, rc: &mut Result<(), i32>) {
    if down {
        let d = ops.set_up(name, false);
        br_log!(
            level(d),
            "wonder: {name} down (Wonder's stop: the provider's deinit closes the PASSTHRU vdev): {}",
            err(d)
        );
    }
    let d = ops.del_iface(ifindex);
    br_log!(
        level(d),
        "wonder: {name} removed (NL80211_CMD_DEL_INTERFACE): {}{}",
        err(d),
        if d.is_err() { " (the next `wifictl wonder` removes it)" } else { "" }
    );
    if d.is_err() && rc.is_ok() {
        *rc = d;
    }
}

/// The whole session: refused when Wonder is missing or in use, the frequency is no AWDL channel, or the country is
/// not known; else the interface made and set, up, counted, then down and removed in reverse (also after a failed
/// step), and wlan0's link checked against its link before. Each step is a "wonder:" log line. Ok: done and wlan0
/// as it was; Err(-errno) of the step that failed; -ENOLINK: wlan0 lost its link.
pub fn run(ops: &mut dyn WonderOps, p: &WonderParams<'_>, r: &mut WonderResult) -> Result<(), i32> {
    *r = WonderResult::default();
    let station = if p.station.is_empty() { "wlan0" } else { p.station };
    let name = if p.name.is_empty() { WONDER_IFNAME } else { p.name };
    if !freq_ok(p.freq_mhz) {
        br_error!("wonder: refused: {} MHz is no AWDL channel (2437: channel 6, 5220: 44, 5745: 149)", p.freq_mhz);
        return Err(-libc::EINVAL);
    }
    let before = ops.link(station);
    say_link("before", station, &before);
    r.link_before = before.as_ref().is_ok_and(|l| l.up);
    r.before = before.unwrap_or_default();

    // ---- every radio, and the refusals
    let mut radios = ops.radios().inspect_err(|rc| {
        br_error!("wonder: refused: the radios not read (nl80211 GET_WIPHY, GET_INTERFACE): {}", strerror(*rc))
    })?;
    let Some(w) = radios.wonder() else {
        br_error!("wonder: refused: no Wonder radio (no wiphy named wonder, none with vendor commands under 00:1a:11): `wifictl p2p` lists the radios");
        return Err(-libc::ENODEV);
    };
    let wiphy = w.wiphy;
    let (has_filter, has_rate) = (w.has_vendor(OUI_GOOGLE, 2), w.has_vendor(OUI_GOOGLE, 3));
    br_info!(
        "wonder: radio: wiphy {wiphy} ({}){}{}",
        if w.name.is_empty() { "?" } else { &w.name },
        if has_filter { "" } else { ", no SET_FILTER" },
        if has_rate { "" } else { ", no SET_FIXED_TX_RATE" }
    );
    if ![1, 4, 5].iter().all(|s| w.has_vendor(OUI_GOOGLE, *s)) {
        br_error!("wonder: refused: wiphy {wiphy} lacks SET_FREQUENCY, SET_REGULATORY or GET_IF_MAC_ADDR: not a Wonder this tool knows");
        return Err(-libc::EOPNOTSUPP);
    }
    if let Some(old) = radios.ifaces.iter().find(|f| f.name == name).filter(|f| f.wiphy == wiphy).cloned() {
        // a session of ours that did not end (a kill): ours to remove
        br_info!("wonder: {name} (index {}) is left from a session that did not end: removed first", old.ifindex);
        let mut left = Ok(());
        remove_iface(ops, name, old.ifindex, true, &mut left);
        left?;
        radios = ops
            .radios()
            .inspect_err(|rc| br_error!("wonder: refused: the radios not read again: {}", strerror(*rc)))?;
        if radios.wonder().is_none() {
            br_error!("wonder: refused: the Wonder radio is gone");
            return Err(-libc::ENODEV);
        }
    }
    if let Some(f) = radios.ifaces.iter().find(|f| f.wiphy == wiphy || f.name.starts_with("wondertap")) {
        br_error!(
            "wonder: refused: Wonder is in use: {} ({}, wiphy {}) is up (Quick Share's \"Share with Apple devices\"? try again after it)",
            if f.name.is_empty() { "(no netdev)" } else { &f.name },
            iftype_name(f.iftype),
            f.wiphy
        );
        return Err(-libc::EBUSY);
    }
    let Some(sta) = radios.ifaces.iter().find(|f| f.name == station) else {
        br_error!("wonder: refused: no {station} (One UI's Wi-Fi is off?): Wonder's provider runs in its driver");
        return Err(-libc::ENODEV);
    };
    let country = ops.country(sta.wiphy);
    let known = country.as_ref().ok().filter(|c| {
        let b = c.as_bytes();
        b.len() >= 2 && b[0].is_ascii_uppercase() && b[1].is_ascii_uppercase()
    });
    let cc: String = if !p.country.is_empty() {
        if let Some(k) = known.filter(|k| k.as_bytes()[..2] != p.country.as_bytes()[..2]) {
            br_error!(
                "wonder: refused: the driver's country is {k}, --country {} would change it (Wonder's init sets the driver's country)",
                p.country
            );
            return Err(-libc::EINVAL);
        }
        p.country.chars().take(2).collect()
    } else if let Some(k) = known {
        k.chars().take(2).collect()
    } else {
        let why = match &country {
            Err(rc) => strerror(*rc),
            Ok(c) => format!("\"{c}"),
        };
        br_error!(
            "wonder: refused: the driver's country is not known ({why}): give --country XX, the phone's own (Wonder's init sets the driver's to it)"
        );
        return Err(-libc::EINVAL);
    };
    br_info!("wonder: country {cc} (the driver's own){}", if p.country.is_empty() { "" } else { " (as --country)" });
    if r.link_before && r.before.freq_mhz != 0 && r.before.freq_mhz != p.freq_mhz {
        br_info!(
            "wonder: {station} is on {} MHz, Wonder on {} MHz: the radio shares its time between them (MCC){}",
            r.before.freq_mhz,
            p.freq_mhz,
            if (r.before.freq_mhz >= 5000) == (p.freq_mhz >= 5000) { ", on one band" } else { "" }
        );
    }

    // ---- the session
    let made = ops.new_monitor(wiphy, name);
    br_log!(
        level(made.map(|_| ())),
        "wonder: {name}: a monitor interface on wiphy {wiphy} (NL80211_CMD_NEW_INTERFACE): {}",
        err(made.map(|_| ()))
    );
    let mut rc = made.map(|_| ());
    if let Ok(ifindex) = made {
        br_info!("wonder: {name} is index {ifindex}");
        let mut handle = None;
        let mut up = false;
        rc = (|| {
            let cc2: [u8; 2] = cc.as_bytes()[..2].try_into().unwrap_or([b'0'; 2]);
            vendor(ops, ifindex, 4, &set_regulatory(64, &cc2).unwrap_or_default(), "country")?;
            let what = format!("{} MHz, 20 MHz wide", p.freq_mhz);
            vendor(ops, ifindex, 1, &set_frequency(64, p.freq_mhz).unwrap_or_default(), &what)?;
            if has_rate {
                vendor(
                    ops,
                    ifindex,
                    3,
                    &set_fixed_tx_rate(64).unwrap_or_default(),
                    "TX rate HT MCS 0, 1 stream, 20 MHz, 0.8 us",
                )?;
            }
            if has_filter {
                vendor(
                    ops,
                    ifindex,
                    2,
                    &set_bssid_filter(64, &AWDL_BSSID).unwrap_or_default(),
                    "BSSID filter 00:25:00:ff:94:73 (AWDL's)",
                )?;
            }
            let u = ops.set_up(name, true);
            br_log!(
                level(u),
                "wonder: {name} up (Wonder's start: the provider's init opens the PASSTHRU vdev): {}{}",
                err(u),
                if u == Err(-libc::EBUSY) {
                    " (a connection or a roam of wlan0 in progress, or Wonder already up)"
                } else {
                    ""
                }
            );
            u?;
            up = true;
            let m = ops.vendor(ifindex, 5, &[]);
            r.mac = m.as_ref().ok().and_then(|reply| mac_reply(reply));
            match (r.mac, &m) {
                (Some(a), _) => br_info!(
                    "wonder: the PASSTHRU vdev's address {:02x}:{:02x}:{:02x}:{:02x}:{:02x}:{:02x} (GET_IF_MAC_ADDR)",
                    a[0],
                    a[1],
                    a[2],
                    a[3],
                    a[4],
                    a[5]
                ),
                (None, Err(e)) => br_error!("wonder: GET_IF_MAC_ADDR: {}", strerror(*e)),
                (None, Ok(_)) => br_error!("wonder: GET_IF_MAC_ADDR: no address in the reply"),
            }
            if let Ok(now) = ops.radios() {
                for f in now.ifaces.iter().filter(|f| f.name.starts_with("wondertap") || f.wiphy == wiphy) {
                    br_info!(
                        "wonder: up: {} (index {}) {}, wiphy {}, {} MHz",
                        if f.name.is_empty() { "(no netdev)" } else { &f.name },
                        f.ifindex,
                        iftype_name(f.iftype),
                        f.wiphy,
                        f.freq_mhz
                    );
                }
            }
            let h =
                ops.open_frames(ifindex).inspect_err(|e| br_error!("wonder: AF_PACKET on {name}: {}", strerror(*e)))?;
            handle = Some(h);
            r.opened = true;
            br_info!("wonder: counting {name}'s frames for {} s", p.seconds);
            let stopped = || p.stop.is_some_and(|s| s.load(Ordering::Relaxed));
            let mut frame = vec![0u8; 8192];
            let start = ops.now_ms();
            let end = start + u64::from(p.seconds) * 1000;
            let mut result = Ok(());
            let mut now = start;
            while now < end && !stopped() {
                let left = end - now;
                match ops.receive(h, &mut frame, left.min(500) as u32) {
                    Err(e) if e == -libc::EINTR || e == -libc::EAGAIN => {}
                    Err(e) => {
                        br_error!("wonder: receive on {name}: {}", strerror(e));
                        result = Err(e);
                        break;
                    }
                    Ok(0) => {}
                    Ok(n) => r.count.add(&frame[..n]),
                }
                now = ops.now_ms();
            }
            if stopped() {
                br_info!("wonder: stopped by a signal after {} ms", ops.now_ms() - start);
            }
            let seconds = ((ops.now_ms() - start + 500) / 1000) as u32;
            for line in r.count.report(seconds).lines() {
                br_info!("wonder: {line}");
            }
            result
        })();
        if let Some(h) = handle {
            ops.close_frames(h);
            br_info!("wonder: AF_PACKET on {name} closed");
        }
        remove_iface(ops, name, ifindex, up, &mut rc);
    }

    // ---- wlan0 as it was
    let mut after = ops.link(station);
    let mut tries = 0;
    while r.link_before && !after.as_ref().is_ok_and(|l| l.up) && tries < 10 {
        ops.pause_ms(1000);
        after = ops.link(station);
        tries += 1;
    }
    say_link("after", station, &after);
    r.link_after = after.as_ref().is_ok_and(|l| l.up);
    r.after = after.unwrap_or_default();
    if r.link_before && !r.link_after {
        br_error!(
            "wonder: {station} lost its link (associated before, not 10 s after the end): One UI's Wi-Fi joins again by itself; say this in the report"
        );
        return rc.and(Err(-libc::ENOLINK));
    }
    if r.link_before && r.before.bssid != r.after.bssid {
        br_info!("wonder: {station} is associated to another access point than before (a roam)");
    } else if r.link_before {
        br_info!("wonder: {station}: the same link as before");
    }
    rc
}
