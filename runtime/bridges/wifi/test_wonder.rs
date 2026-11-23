// SPDX-License-Identifier: MIT
//! AWDL's frames, Wonder's vendor data and `wifictl wonder` on a stand-in nl80211 (tests/test_wifi.c's
//! test_awdl_frames, test_wonder_vendor, test_wonder_session).

use super::super::p2p::tests::{iface_msg, phone_radios, T_MON};
use super::super::p2p::{Iface, Radios, OUI_GOOGLE, WONDER_IFNAME};
use super::super::station::Link;
use super::*;
use crate::log::{Capture, Level};

/// A frame as Wonder's monitor interface gives it: radiotap (TSFT, FLAGS, RATE, CHANNEL, DBM_ANTSIGNAL, padded to
/// 24), then the 802.11 header and body.
fn radiotap(tsft: bool, flags: u8, dbm: i8) -> Vec<u8> {
    let mut f = vec![0u8; 24];
    let present: u32 = u32::from(tsft) | 1 << 1 | 1 << 2 | 1 << 3 | 1 << 5;
    f[4..8].copy_from_slice(&present.to_le_bytes());
    let mut at = 8;
    if tsft {
        at += 8; // aligned at 8 already
    }
    f[at] = flags;
    f[at + 1] = 12; // 6 Mb/s
    at += 2;
    f[at..at + 2].copy_from_slice(&2437u16.to_le_bytes());
    f[at + 2..at + 4].copy_from_slice(&0x00a0u16.to_le_bytes());
    at += 4;
    f[at] = dbm as u8;
    at += 1;
    let len = (at + 3) & !3;
    f[2] = len as u8;
    f.truncate(len);
    f
}

fn awdl_action(src: &[u8; 6], subtype: u8, dbm: i8) -> Vec<u8> {
    let mut f = radiotap(true, 0, dbm);
    let mut h = vec![0u8; 24 + 16];
    h[0] = 0xd0;
    h[4..10].fill(0xff);
    h[10..16].copy_from_slice(src);
    h[16..22].copy_from_slice(&AWDL_BSSID);
    h[24..32].copy_from_slice(&[127, 0x00, 0x17, 0xf2, 8, 0x10, subtype, 0]);
    f.extend_from_slice(&h);
    f
}

const A: [u8; 6] = [0x02, 0xaa, 0, 0, 0, 1];
const B2: [u8; 6] = [0x02, 0xbb, 0, 0, 0, 2];

#[test]
fn awdl_frames() {
    let f = awdl_action(&A, 0, -40);
    let k = radiotap_frame(&f).unwrap();
    assert!(k.kind == AwdlKind::Action && k.subtype == 0 && k.frame_type == 0 && k.tsft && k.signal_dbm == Some(-40));
    assert!(k.source == A && !k.bad_fcs);
    let mut c = AwdlCount::default();
    c.add(&f); // PSF from a, -40 dBm
    c.add(&awdl_action(&A, 0, -35)); // PSF from a, -35 dBm (its best)
    c.add(&awdl_action(&B2, 3, -70)); // MIF from b
    c.add(&awdl_action(&B2, 6, -71)); // another subtype
    let mut f = awdl_action(&B2, 0, -60);
    f[24 + 24 + 4] = 9; // vendor action of Apple's with a type that is not AWDL
    assert_eq!(radiotap_frame(&f).unwrap().kind, AwdlKind::OtherAction);
    c.add(&f);
    let mut f = awdl_action(&B2, 0, -60);
    f[24 + 24] = 4; // a public action frame
    c.add(&f);
    // An AWDL data frame (addr3 AWDL's BSSID), an ordinary data frame, an ACK, a beacon.
    let mut f = radiotap(true, 0, -50);
    let at = f.len();
    f.resize(at + 40, 0);
    f[at] = 0x08;
    f[at + 4..at + 10].copy_from_slice(&B2);
    f[at + 10..at + 16].copy_from_slice(&A);
    f[at + 16..at + 22].copy_from_slice(&AWDL_BSSID);
    let k = radiotap_frame(&f).unwrap();
    assert!(k.kind == AwdlKind::Data && k.frame_type == 2);
    c.add(&f);
    f[at + 16] = 0x12;
    assert_eq!(radiotap_frame(&f).unwrap().kind, AwdlKind::Not);
    c.add(&f);
    f[at + 16] = 0x00;
    f[at + 1] = 0x02; // from DS: not AWDL's
    assert_eq!(radiotap_frame(&f).unwrap().kind, AwdlKind::Not);
    f[at] = 0xd4; // an ACK: 10 bytes, control
    f[at + 1] = 0;
    let k = radiotap_frame(&f[..at + 10]).unwrap();
    assert!(k.frame_type == 1 && k.kind == AwdlKind::Not);
    c.add(&f[..at + 10]);
    f[at] = 0x80; // a beacon (management) cut at 20 bytes: not parsed
    assert!(radiotap_frame(&f[..at + 20]).is_none());
    c.add(&f[..at + 20]);
    // A bad FCS (flags 0x40) is counted alone; an FCS at the end (0x10) is cut off before the body is read.
    let mut f = awdl_action(&A, 0, -40);
    f[16] = 0x40;
    assert!(radiotap_frame(&f).unwrap().bad_fcs);
    c.add(&f);
    let mut f = awdl_action(&A, 3, -40);
    f[16] = 0x10;
    f.extend_from_slice(&[0xee; 4]);
    let k = radiotap_frame(&f).unwrap();
    assert!(k.kind == AwdlKind::Action && k.subtype == 3);
    c.add(&f); // MIF from a
               // Without TSFT; with a second present word (bit 31): the fields start after it.
    let mut f = radiotap(false, 0, -45);
    assert_eq!(f.len(), 16);
    let n = f.len();
    f.resize(n + 40, 0);
    f[n] = 0xd0;
    f[n + 10..n + 16].copy_from_slice(&B2);
    f[n + 24..n + 31].copy_from_slice(b"\x7f\x00\x17\xf2\x08\x10\x00");
    let k = radiotap_frame(&f).unwrap();
    assert!(!k.tsft && k.kind == AwdlKind::Action && k.signal_dbm == Some(-45));
    c.add(&f); // PSF from b, no TSFT
    let mut g = vec![0u8; 256];
    let p0: u32 = 1 << 1 | 1 << 31;
    g[4..8].copy_from_slice(&p0.to_le_bytes());
    g[12] = 0x00; // FLAGS, after both words
    g[2] = 16;
    g[16] = 0xd0;
    g[16 + 10..16 + 16].copy_from_slice(&A);
    g[16 + 24..16 + 31].copy_from_slice(b"\x7f\x00\x17\xf2\x08\x10\x03");
    let k = radiotap_frame(&g[..56]).unwrap();
    assert!(k.kind == AwdlKind::Action && k.subtype == 3 && k.signal_dbm.is_none());
    // Broken radiotap: another version, a length past the frame, a present word past the header.
    g[0] = 1;
    assert!(radiotap_frame(&g[..56]).is_none());
    g[0] = 0;
    g[2] = 200;
    assert!(radiotap_frame(&g[..56]).is_none());
    g[2] = 8;
    assert!(radiotap_frame(&g[..56]).is_none());
    assert!(radiotap_frame(&g[..4]).is_none());
    c.add(&g[..4]);
    assert!(c.frames == 14 && c.unparsed == 2 && c.bad_fcs == 1 && c.control == 1 && c.data == 2 && c.management == 8);
    assert!(c.awdl_action == 6 && c.awdl_psf == 3 && c.awdl_mif == 2 && c.awdl_other == 1 && c.action == 8);
    assert!(c.awdl_data == 1 && c.with_tsft == 10);
    assert!(c.peers.len() == 2 && c.peers[0].action == 3 && c.peers[0].data == 1 && c.peers[0].best_dbm == Some(-35));
    assert!(c.peers[1].action == 3 && c.peers[1].data == 0 && c.peers[1].best_dbm == Some(-45));
    let text = c.report(30);
    assert!(text.contains(
        "frames: 14 in 30 s: 8 management (8 action), 1 control, 2 data; 2 not parsed, 1 with a bad FCS; 10 carry the radiotap TSFT\n"
    ));
    assert!(text.contains(
        "AWDL: 6 action frames (3 PSF, 2 MIF, 1 other subtypes), 1 data frames to AWDL's BSSID, from 2 peer(s)\n"
    ));
    assert!(text.contains("AWDL peer: 02:aa:00:00:00:01: 3 action, 1 data, best -35 dBm\n"));
    assert!(text.contains("AWDL peer: 02:bb:00:00:00:02: 3 action, 0 data, best -45 dBm\n"));
    assert!(text.contains("radiotap: 1 frames without the TSFT") && !text.contains("no AWDL frame"));
    // More peers than kept; nothing at all; frames but no AWDL.
    for i in 0..AWDL_PEERS as u8 + 3 {
        c.add(&awdl_action(&[2, 0, 0, 0, 1, i], 0, -50));
    }
    assert!(c.peers.len() == AWDL_PEERS && c.peers_dropped == 5);
    let mut c = AwdlCount::default();
    assert!(c.report(5).contains("AWDL: no frame at all: Wonder's receive path gave nothing"));
    let mut f = radiotap(true, 0, -50);
    let at = f.len();
    f.resize(at + 24, 0);
    f[at] = 0x80;
    c.add(&f);
    let text = c.report(5);
    assert!(
        text.contains("AWDL: no AWDL frame: no Apple device sends AWDL on this channel now")
            && !text.contains("without the TSFT")
    );
}

#[test]
fn wonder_vendor_data() {
    // SET_FREQUENCY: CHANNEL_FREQ u32 5745, CHANNEL_BANDWIDTH u16 0 (20 MHz) with its pad
    assert_eq!(set_frequency(64, 5745).unwrap(), b"\x08\x00\x01\x00\x71\x16\x00\x00\x06\x00\x02\x00\x00\x00\x00\x00");
    // SET_REGULATORY: REG_COUNTRY_CODE "GB" with its NUL
    assert_eq!(set_regulatory(64, b"GB").unwrap(), b"\x07\x00\x01\x00GB\x00\x00");
    // SET_FIXED_TX_RATE: PREAMBLE 1 (HT), BW 0, GI 2 (0.8 us), NSS 1, MCS 0
    assert_eq!(
        set_fixed_tx_rate(64).unwrap(),
        b"\x08\x00\x01\x00\x01\x00\x00\x00\x06\x00\x02\x00\x00\x00\x00\x00\x08\x00\x03\x00\x02\x00\x00\x00\x05\x00\x04\x00\x01\x00\x00\x00\x05\x00\x05\x00\x00\x00\x00\x00"
    );
    // SET_FILTER: TOP_FILTER_TYPE u32 0 (BSSID), TOP_FILTER_PARAMS nested { ENABLED u8 1, ADDR AWDL's BSSID }
    assert_eq!(
        set_bssid_filter(64, &AWDL_BSSID).unwrap(),
        b"\x08\x00\x01\x00\x00\x00\x00\x00\x18\x00\x02\x80\x05\x00\x01\x00\x01\x00\x00\x00\x0a\x00\x02\x00\x00\x25\x00\xff\x94\x73\x00\x00"
    );
    assert!(
        set_frequency(12, 2437).is_none()
            && set_bssid_filter(31, &AWDL_BSSID).is_none()
            && set_fixed_tx_rate(39).is_none()
    );
    // GET_IF_MAC_ADDR's reply; after another attribute; cut; absent.
    assert_eq!(mac_reply(b"\x0a\x00\x01\x00\x02\x11\x22\x33\x44\x55\x00\x00"), Some(*b"\x02\x11\x22\x33\x44\x55"));
    assert_eq!(
        mac_reply(b"\x08\x00\x07\x00\x01\x02\x03\x04\x0a\x00\x01\x00\x02\x11\x22\x33\x44\x66\x00\x00").map(|m| m[5]),
        Some(0x66)
    );
    assert!(
        mac_reply(b"\x0a\x00\x01\x00\x02\x11\x22").is_none()
            && mac_reply(b"\x08\x00\x07\x00\x01\x02\x03\x04").is_none()
    );
    assert!(freq_ok(2437) && freq_ok(5745) && freq_ok(5220) && !freq_ok(5180) && !freq_ok(0));
}

/// The stand-in nl80211 for run: what it was asked, in order, and what it answers.
#[derive(Default)]
struct FakeNl {
    radios: Radios,
    country: String,
    country_rc: Option<i32>,
    new_rc: Option<i32>,
    up_rc: Option<i32>,
    del_rc: Option<i32>,
    open_rc: Option<i32>,
    recv_rc: Option<i32>,
    vendor_fail: Option<(u32, i32)>, // the subcommand that answers an error
    up_now: bool,                    // the monitor is up (wondertap0 shows)
    monitor: bool,                   // the monitor exists
    before: Link,
    after: Link,
    linked_after: bool, // wlan0 is up again after the session
    links: u32,
    lose_for: u32, // after the session, wlan0 is down for this many more link calls
    pauses: u32,
    frames: Vec<Vec<u8>>,
    frame_next: usize,
    stop_after: usize,
    stop: Option<&'static AtomicBool>,
    now: u64,
    freq_data: Vec<u8>,
    calls: Vec<String>,
}

impl FakeNl {
    fn call(&mut self, what: impl Into<String>) {
        self.calls.push(what.into());
    }
    fn calls(&self) -> String {
        self.calls.join(" ")
    }
}

impl WonderOps for FakeNl {
    fn radios(&mut self) -> Result<Radios, i32> {
        self.call("radios");
        let mut out = self.radios.clone();
        if self.monitor {
            let iface = |name: &str, ifindex, wiphy, freq_mhz| Iface {
                name: name.into(),
                ifindex,
                wiphy,
                iftype: T_MON,
                freq_mhz,
                mac: [0; 6],
            };
            out.ifaces.push(iface("wondermon0", 52, 1, 0));
            if self.up_now {
                out.ifaces.push(iface("wondertap0", 51, 0, 5745));
            }
        }
        Ok(out)
    }
    fn country(&mut self, wiphy: u32) -> Result<String, i32> {
        self.call(format!("country({wiphy})"));
        match self.country_rc {
            Some(rc) => Err(rc),
            None => Ok(self.country.clone()),
        }
    }
    fn new_monitor(&mut self, wiphy: u32, name: &str) -> Result<u32, i32> {
        self.call(format!("new({wiphy},{name})"));
        if let Some(rc) = self.new_rc {
            return Err(rc);
        }
        self.monitor = true;
        Ok(52)
    }
    fn del_iface(&mut self, ifindex: u32) -> Result<(), i32> {
        self.call(format!("del({ifindex})"));
        if let Some(rc) = self.del_rc {
            return Err(rc);
        }
        self.monitor = false;
        Ok(())
    }
    fn set_up(&mut self, _name: &str, up: bool) -> Result<(), i32> {
        self.call(if up { "up" } else { "down" });
        if let (true, Some(rc)) = (up, self.up_rc) {
            return Err(rc);
        }
        self.up_now = up;
        Ok(())
    }
    fn vendor(&mut self, ifindex: u32, subcmd: u32, data: &[u8]) -> Result<Vec<u8>, i32> {
        self.call(format!("vendor({subcmd})"));
        if ifindex != 52 {
            return Err(-libc::ENODEV);
        }
        if let Some((s, rc)) = self.vendor_fail {
            if s == subcmd {
                return Err(rc);
            }
        }
        if subcmd == 1 {
            self.freq_data = data.to_vec();
        }
        if subcmd == 5 {
            // Wonder answers it only when up
            if !self.up_now {
                return Err(-libc::ENODEV);
            }
            return Ok(b"\x0a\x00\x01\x00\x02\x99\x88\x77\x66\x55\x00\x00".to_vec());
        }
        Ok(Vec::new())
    }
    fn open_frames(&mut self, ifindex: u32) -> Result<i32, i32> {
        self.call("open");
        match self.open_rc {
            Some(rc) => Err(rc),
            None if ifindex == 52 => Ok(7),
            None => Err(-libc::ENODEV),
        }
    }
    fn receive(&mut self, handle: i32, buffer: &mut [u8], wait_ms: u32) -> Result<usize, i32> {
        if handle != 7 {
            return Err(-libc::EBADF);
        }
        if let (Some(rc), true) = (self.recv_rc, self.frame_next == self.frames.len()) {
            return Err(rc);
        }
        if let (Some(stop), true) = (self.stop, self.stop_after != 0 && self.frame_next == self.stop_after) {
            stop.store(true, Ordering::Relaxed);
            return Err(-libc::EINTR);
        }
        if let Some(f) = self.frames.get(self.frame_next).filter(|f| f.len() <= buffer.len()) {
            buffer[..f.len()].copy_from_slice(f);
            self.now += 1;
            self.frame_next += 1;
            return Ok(f.len());
        }
        self.now += u64::from(wait_ms); // nothing came in the wait
        Ok(0)
    }
    fn close_frames(&mut self, _handle: i32) {
        self.call("close");
    }
    fn link(&mut self, ifname: &str) -> Result<Link, i32> {
        self.call("link");
        if ifname != "wlan0" {
            return Err(-libc::ENODEV);
        }
        self.links += 1;
        if self.links == 1 {
            return Ok(self.before);
        }
        if self.lose_for > 0 {
            self.lose_for -= 1;
            return Ok(Link::default());
        }
        Ok(if self.linked_after { self.after } else { Link::default() })
    }
    fn now_ms(&mut self) -> u64 {
        self.now
    }
    fn pause_ms(&mut self, ms: u32) {
        self.pauses += 1;
        self.now += u64::from(ms);
        self.call("pause");
    }
}

fn fake_phone() -> FakeNl {
    let l = Link {
        up: true,
        rssi_dbm: -55,
        tx_rate_mbps: 433,
        freq_mhz: 5180,
        bssid: [0x0c, 1, 2, 3, 4, 5],
        ..Default::default()
    };
    let mut beacon = radiotap(true, 0, -70);
    let n = beacon.len();
    beacon.resize(n + 24, 0);
    beacon[n] = 0x80; // a beacon
    FakeNl {
        radios: phone_radios(),
        country: "GB".into(),
        before: l,
        after: l,
        linked_after: true,
        frames: vec![awdl_action(&A, 0, -40), awdl_action(&A, 0, -42), awdl_action(&B2, 3, -61), beacon],
        now: 1000,
        ..Default::default()
    }
}

/// The session, and its log as test_wifi.c's log_line kept it ("E " before an error line).
fn session(f: &mut FakeNl, p: &WonderParams<'_>, r: &mut WonderResult) -> (Result<(), i32>, String) {
    let log = Capture::start();
    let rc = run(f, p, r);
    let text =
        log.lines().iter().map(|(l, t)| format!("{}{t}\n", if *l == Level::Error { "E " } else { "" })).collect();
    (rc, text)
}

#[test]
fn wonder_session() {
    let mut r = WonderResult::default();
    let mut p =
        WonderParams { station: "wlan0", name: WONDER_IFNAME, freq_mhz: 5745, seconds: 30, ..Default::default() };
    // ---- a session that works: every step in Samsung's order, the frames counted, everything back
    let mut f = fake_phone();
    let (rc, logs) = session(&mut f, &p, &mut r);
    let logged = |t: &str| logs.contains(t);
    assert_eq!(rc, Ok(()));
    assert_eq!(
        f.calls(),
        "link radios country(0) new(1,wondermon0) vendor(4) vendor(1) vendor(3) vendor(2) up vendor(5) radios open close down del(52) link"
    );
    assert_eq!(f.freq_data, set_frequency(64, 5745).unwrap());
    assert!(
        r.mac == Some(*b"\x02\x99\x88\x77\x66\x55")
            && r.opened
            && r.link_before
            && r.link_after
            && !f.monitor
            && !f.up_now
    );
    assert!(
        r.count.frames == 4
            && r.count.awdl_action == 3
            && r.count.awdl_psf == 2
            && r.count.awdl_mif == 1
            && r.count.peers.len() == 2
    );
    assert!(f.now >= 1000 + 30000 && f.now < 1000 + 30000 + 600); // 30 s of the stand-in's clock
    assert!(logged("wonder: wlan0 before: associated to 0c:01:02:03:04:05 on 5180 MHz, -55 dBm, 433 Mb/s\n"));
    assert!(logged("wonder: radio: wiphy 1 (wonder)\n") && logged("wonder: country GB (the driver's own)\n"));
    assert!(logged(
        "wonder: wlan0 is on 5180 MHz, Wonder on 5745 MHz: the radio shares its time between them (MCC), on one band\n"
    ));
    assert!(
        logged("wonder: wondermon0: a monitor interface on wiphy 1 (NL80211_CMD_NEW_INTERFACE): ok\n")
            && logged("wonder: wondermon0 is index 52\n")
    );
    assert!(logged("wonder: country (vendor 00:1a:11 subcommand 4 SET_REGULATORY): ok\n"));
    assert!(logged("wonder: 5745 MHz, 20 MHz wide (vendor 00:1a:11 subcommand 1 SET_FREQUENCY): ok\n"));
    assert!(logged("subcommand 3 SET_FIXED_TX_RATE): ok\n"));
    assert!(logged("wonder: BSSID filter 00:25:00:ff:94:73 (AWDL's) (vendor 00:1a:11 subcommand 2 SET_FILTER): ok\n"));
    assert!(logged("wonder: wondermon0 up (Wonder's start: the provider's init opens the PASSTHRU vdev): ok\n"));
    assert!(logged("wonder: the PASSTHRU vdev's address 02:99:88:77:66:55 (GET_IF_MAC_ADDR)\n"));
    assert!(
        logged("wonder: up: wondermon0 (index 52) monitor, wiphy 1, 0 MHz\n")
            && logged("wonder: up: wondertap0 (index 51) monitor, wiphy 0, 5745 MHz\n")
    );
    assert!(
        logged("wonder: counting wondermon0's frames for 30 s\n")
            && logged("wonder: frames: 4 in 30 s: 4 management (3 action)")
    );
    assert!(logged("wonder: AWDL: 3 action frames (2 PSF, 1 MIF, 0 other subtypes), 0 data frames to AWDL's BSSID, from 2 peer(s)\n"));
    assert!(logged("wonder: AWDL peer: 02:aa:00:00:00:01: 2 action, 0 data, best -40 dBm\n"));
    assert!(logged("wonder: AF_PACKET on wondermon0 closed\n"));
    assert!(logged("wonder: wondermon0 down (Wonder's stop: the provider's deinit closes the PASSTHRU vdev): ok\n"));
    assert!(
        logged("wonder: wondermon0 removed (NL80211_CMD_DEL_INTERFACE): ok\n")
            && logged("wonder: wlan0: the same link as before\n")
    );
    // no error line of its own (the capture holds the lines of every thread meanwhile: another test's error too)
    assert!(!logs.starts_with("E wonder:") && !logged("\nE wonder:"));
    if std::env::var_os("ONEIOS_TEST_SHOW").is_some() {
        print!("---- wifictl wonder --seconds 30 --freq 5745 on the stand-in nl80211\n{logs}");
    }
    // On 2437 MHz beside a 5 GHz station: MCC across the bands; a roam during it is said.
    let mut f = fake_phone();
    p.freq_mhz = 2437;
    f.after.bssid[5] = 9;
    let (rc, logs) = session(&mut f, &p, &mut r);
    assert!(rc.is_ok() && logs.contains("on 2437 MHz: the radio shares its time between them (MCC)\n"));
    assert!(logs.contains("associated to another access point than before (a roam)"));
    p.freq_mhz = 5745;

    // ---- the refusals: nothing made, nothing changed
    let mut f = fake_phone();
    p.freq_mhz = 5180;
    let (rc, logs) = session(&mut f, &p, &mut r);
    assert!(
        rc == Err(-libc::EINVAL)
            && f.calls.is_empty()
            && logs.contains("E wonder: refused: 5180 MHz is no AWDL channel")
    );
    p.freq_mhz = 5745;
    let mut f = fake_phone();
    f.radios.wiphys.truncate(1); // no Wonder radio
    let (rc, logs) = session(&mut f, &p, &mut r);
    assert!(
        rc == Err(-libc::ENODEV) && f.calls() == "link radios" && logs.contains("E wonder: refused: no Wonder radio")
    );
    let mut f = fake_phone();
    f.radios.wiphys[1].commands.truncate(4); // no GET_IF_MAC_ADDR
    let (rc, _) = session(&mut f, &p, &mut r);
    assert!(rc == Err(-libc::EOPNOTSUPP) && f.calls() == "link radios");
    let mut f = fake_phone();
    iface_msg(&mut f.radios, 51, "wondertap0", 0, T_MON, 2437); // Quick Share holds Wonder
    let (rc, logs) = session(&mut f, &p, &mut r);
    assert!(rc == Err(-libc::EBUSY) && f.calls() == "link radios");
    assert!(logs.contains("E wonder: refused: Wonder is in use: wondertap0 (monitor, wiphy 0) is up"));
    let mut f = fake_phone();
    iface_msg(&mut f.radios, 60, "mosey0", 1, T_MON, 0); // another's interface on Wonder's radio
    let (rc, logs) = session(&mut f, &p, &mut r);
    assert!(rc == Err(-libc::EBUSY) && logs.contains("Wonder is in use: mosey0 (monitor, wiphy 1)"));
    let mut f = fake_phone();
    f.radios.ifaces.clear(); // no wlan0
    let (rc, logs) = session(&mut f, &p, &mut r);
    assert!(rc == Err(-libc::ENODEV) && logs.contains("E wonder: refused: no wlan0"));
    let mut f = fake_phone();
    f.country = "00".into(); // the world domain: not the phone's
    let (rc, logs) = session(&mut f, &p, &mut r);
    assert!(rc == Err(-libc::EINVAL) && f.calls() == "link radios country(0)");
    assert!(logs.contains("E wonder: refused: the driver's country is not known (\"00)"));
    let mut f = fake_phone();
    f.country_rc = Some(-libc::EIO);
    let (rc, logs) = session(&mut f, &p, &mut r);
    assert!(rc == Err(-libc::EINVAL) && logs.contains("not known (Input/output error)"));
    let mut f = fake_phone();
    p.country = "DE".into(); // --country against a known, other one
    let (rc, logs) = session(&mut f, &p, &mut r);
    assert!(
        rc == Err(-libc::EINVAL)
            && logs.contains("E wonder: refused: the driver's country is GB, --country DE would change it")
    );
    let mut f = fake_phone();
    f.country = "00".into(); // --country where the driver's is not known: used
    let (rc, logs) = session(&mut f, &p, &mut r);
    assert!(rc.is_ok() && logs.contains("wonder: country DE (the driver's own) (as --country)\n"));
    p.country.clear();

    // ---- a leftover of a killed session is removed first, then the session runs
    let mut f = fake_phone();
    f.monitor = true;
    let (rc, logs) = session(&mut f, &p, &mut r);
    assert!(rc.is_ok() && f.calls().starts_with("link radios down del(52) radios country(0) new(1,wondermon0) "));
    assert!(logs.contains("wonder: wondermon0 (index 52) is left from a session that did not end: removed first\n"));
    let mut f = fake_phone();
    f.monitor = true;
    f.del_rc = Some(-libc::EPERM);
    let (rc, _) = session(&mut f, &p, &mut r);
    assert!(rc == Err(-libc::EPERM) && f.calls() == "link radios down del(52)");

    // ---- failed steps: everything made is closed in reverse, and wlan0 checked
    let mut f = fake_phone();
    f.new_rc = Some(-libc::EOPNOTSUPP);
    let (rc, _) = session(&mut f, &p, &mut r);
    assert!(
        rc == Err(-libc::EOPNOTSUPP) && f.calls() == "link radios country(0) new(1,wondermon0) link" && r.link_after
    );
    let mut f = fake_phone();
    f.vendor_fail = Some((1, -libc::EINVAL)); // SET_FREQUENCY refused: removed, never up
    let (rc, logs) = session(&mut f, &p, &mut r);
    assert!(
        rc == Err(-libc::EINVAL)
            && f.calls() == "link radios country(0) new(1,wondermon0) vendor(4) vendor(1) del(52) link"
            && !f.monitor
    );
    assert!(logs
        .contains("E wonder: 5745 MHz, 20 MHz wide (vendor 00:1a:11 subcommand 1 SET_FREQUENCY): Invalid argument\n"));
    let mut f = fake_phone();
    f.up_rc = Some(-libc::EBUSY); // the provider's init: a connection in progress
    let (rc, logs) = session(&mut f, &p, &mut r);
    assert!(rc == Err(-libc::EBUSY));
    assert_eq!(
        f.calls(),
        "link radios country(0) new(1,wondermon0) vendor(4) vendor(1) vendor(3) vendor(2) up del(52) link"
    );
    assert!(logs.contains(
        "E wonder: wondermon0 up (Wonder's start: the provider's init opens the PASSTHRU vdev): Device or resource busy (a connection or a roam of wlan0 in progress, or Wonder already up)\n"
    ));
    let mut f = fake_phone();
    f.open_rc = Some(-libc::EPERM);
    let (rc, _) = session(&mut f, &p, &mut r);
    assert!(rc == Err(-libc::EPERM) && !r.opened);
    assert_eq!(
        f.calls(),
        "link radios country(0) new(1,wondermon0) vendor(4) vendor(1) vendor(3) vendor(2) up vendor(5) radios open down del(52) link"
    );
    let mut f = fake_phone();
    f.recv_rc = Some(-libc::ENETDOWN); // the interface went away under the count
    let (rc, logs) = session(&mut f, &p, &mut r);
    assert!(rc == Err(-libc::ENETDOWN) && r.count.frames == 4 && f.calls().contains("open close down del(52) link"));
    assert!(logs.contains("E wonder: receive on wondermon0: Network is down\n"));
    let mut f = fake_phone();
    f.del_rc = Some(-libc::EBUSY); // removal refused: the session's error, said
    let (rc, logs) = session(&mut f, &p, &mut r);
    assert!(rc == Err(-libc::EBUSY));
    assert!(logs.contains(
        "E wonder: wondermon0 removed (NL80211_CMD_DEL_INTERFACE): Device or resource busy (the next `wifictl wonder` removes it)\n"
    ));
    // A Wonder without SET_FILTER and SET_FIXED_TX_RATE: those two are left out.
    let mut f = fake_phone();
    f.radios.wiphys[1].commands[1].subcmd = 7;
    f.radios.wiphys[1].commands[2].subcmd = 8;
    let (rc, logs) = session(&mut f, &p, &mut r);
    assert!(rc.is_ok() && f.calls().contains("vendor(4) vendor(1) up vendor(5)"));
    assert!(logs.contains("wonder: radio: wiphy 1 (wonder), no SET_FILTER, no SET_FIXED_TX_RATE\n"));
    let _ = OUI_GOOGLE;

    // ---- a signal ends the count; the teardown still runs
    static STOP: AtomicBool = AtomicBool::new(false);
    let mut f = fake_phone();
    p.stop = Some(&STOP);
    f.stop = Some(&STOP);
    f.stop_after = 2;
    let (rc, logs) = session(&mut f, &p, &mut r);
    assert!(
        rc.is_ok()
            && r.count.frames == 2
            && STOP.load(Ordering::Relaxed)
            && f.calls().contains("open close down del(52) link")
    );
    assert!(logs.contains("wonder: stopped by a signal after 2 ms\n"));
    p.stop = None;

    // ---- wlan0 afterwards: back within the 10 s, or lost
    let mut f = fake_phone();
    f.lose_for = 3;
    let (rc, _) = session(&mut f, &p, &mut r);
    assert!(rc.is_ok() && f.pauses == 3 && r.link_after);
    let mut f = fake_phone();
    f.linked_after = false;
    let (rc, logs) = session(&mut f, &p, &mut r);
    assert!(rc == Err(-libc::ENOLINK) && f.pauses == 10 && !r.link_after);
    assert!(logs.contains("E wonder: wlan0 lost its link (associated before, not 10 s after the end)"));
    let mut f = fake_phone();
    f.before.up = false; // not associated before: nothing to wait for
    f.linked_after = false;
    let (rc, logs) = session(&mut f, &p, &mut r);
    assert!(
        rc.is_ok() && f.pauses == 0 && logs.contains("wonder: wlan0 before: not associated\n") && !logs.contains("MCC")
    );
}
