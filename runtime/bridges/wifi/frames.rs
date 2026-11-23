// SPDX-License-Identifier: MIT
//! The Wi-Fi bridge's frame rules (docs/hardware.md section 3): which frames cross between the guest's en0 and the
//! phone's wlan0, and how a frame that the receive path coalesced (GRO) is cut back into frames of the link's size.
//! DHCP messages read for the log, a generic netlink family's groups and nl80211's bands. Pure functions.
//! C: wifi/frames.c.

use super::nl;
use super::proto::*;
use super::{CONTROL_TYPE, FRAME};
use xh_core::bytes::be;

pub fn ethertype(frame: &[u8]) -> u16 {
    if frame.len() >= 14 {
        be::u16(frame, 12)
    } else {
        0
    }
}

/// A frame of the guest's that may go on the air: not EAPOL, not the bridge's own type, sent with the radio's
/// address.
pub fn to_radio(frame: &[u8], mac: &[u8; 6]) -> bool {
    if frame.len() < 14 || frame.len() > FRAME {
        return false;
    }
    let kind = ethertype(frame);
    if kind == 0x888e || kind == CONTROL_TYPE {
        return false; // EAPOL is the supplicant's; ours is the bridge's
    }
    frame[6..12] == *mac // a station sends with its own address only
}

/// A frame of the radio's that the guest gets: to its address, broadcast or multicast; not EAPOL.
pub fn to_guest(frame: &[u8], mac: &[u8; 6]) -> bool {
    if frame.len() < 14 {
        return false;
    }
    let kind = ethertype(frame);
    if kind == 0x888e || kind == CONTROL_TYPE {
        return false;
    }
    frame[0] & 1 != 0 || frame[..6] == *mac
}

fn fold(mut sum: u32) -> u16 {
    while sum >> 16 != 0 {
        sum = (sum & 0xffff) + (sum >> 16);
    }
    !(sum as u16)
}

fn add(mut sum: u32, data: &[u8]) -> u32 {
    let mut pairs = data.chunks_exact(2);
    for w in &mut pairs {
        sum += u32::from(w[0]) << 8 | u32::from(w[1]);
    }
    if let [last] = pairs.remainder() {
        sum += u32::from(*last) << 8;
    }
    sum
}

/// The checksums of one IPv4 or IPv6 TCP segment: the IPv4 header (when IPv4) and TCP over the pseudo-header.
fn tcp_checksums(l3: &mut [u8], v6: bool, l3_len: usize, tcp_len: usize) {
    let mut sum = if v6 {
        add(0, &l3[8..40]) // source and destination
    } else {
        be::put_u16(l3, 10, 0);
        let h = fold(add(0, &l3[..l3_len]));
        be::put_u16(l3, 10, h);
        add(0, &l3[12..20])
    };
    sum += 6 + tcp_len as u32; // protocol, TCP length
    let tcp = &mut l3[l3_len..l3_len + tcp_len];
    be::put_u16(tcp, 16, 0);
    let t = fold(add(sum, tcp));
    be::put_u16(tcp, 16, t);
}

/// A frame for the guest: as it is when it fits the link and the receive path did not coalesce it; a coalesced TCP
/// frame (IPv4 or IPv6) cut into segments of at most `mss` payload bytes with their checksums made again. `out` gets
/// each frame. The frames given (0: the frame was too long and not TCP: dropped).
pub fn segment(frame: &[u8], mss: u16, coalesced: bool, mut out: impl FnMut(&[u8])) -> usize {
    let length = frame.len();
    let fits = length <= FRAME;
    let as_it_is = |out: &mut dyn FnMut(&[u8])| {
        if !fits {
            return 0;
        }
        out(frame);
        1
    };
    if fits && !coalesced {
        return as_it_is(&mut out);
    }
    if length < 14 + 20 || mss == 0 {
        return as_it_is(&mut out);
    }
    let kind = ethertype(frame);
    let l3 = &frame[14..];
    let (v6, l3_len) = if kind == 0x0800 && l3[0] >> 4 == 4 && l3[9] == 6 {
        (false, (l3[0] & 15) as usize * 4)
    } else if kind == 0x86dd && length >= 14 + 40 && l3[0] >> 4 == 6 && l3[6] == 6 {
        (true, 40)
    } else {
        return as_it_is(&mut out); // only TCP is coalesced
    };
    if l3_len < 20 || length < 14 + l3_len + 20 {
        return as_it_is(&mut out);
    }
    let tcp = &l3[l3_len..];
    let tcp_hdr = (tcp[12] >> 4) as usize * 4;
    let headers = 14 + l3_len + tcp_hdr;
    if tcp_hdr < 20 || headers >= length || headers >= FRAME {
        return as_it_is(&mut out);
    }
    let mss = (mss as usize).min(FRAME - headers);
    let payload = &frame[headers..];
    let seq = be::u32(tcp, 4);
    let id = if v6 { 0 } else { be::u16(l3, 4) };
    let flags = tcp[13];
    let mut seg = [0u8; FRAME];
    let mut count = 0;
    for (n, part) in payload.chunks(mss).enumerate() {
        let last = (n + 1) * mss >= payload.len();
        seg[..headers].copy_from_slice(&frame[..headers]);
        seg[headers..headers + part.len()].copy_from_slice(part);
        let o3 = &mut seg[14..];
        if v6 {
            be::put_u16(o3, 4, (tcp_hdr + part.len()) as u16);
        } else {
            be::put_u16(o3, 2, (l3_len + tcp_hdr + part.len()) as u16);
            be::put_u16(o3, 4, id.wrapping_add(count as u16));
        }
        let ot = &mut o3[l3_len..];
        be::put_u32(ot, 4, seq.wrapping_add((n * mss) as u32));
        ot[13] = if last { flags } else { flags & !0x09 }; // FIN and PSH on the last segment only
        tcp_checksums(o3, v6, l3_len, tcp_hdr + part.len());
        out(&seg[..headers + part.len()]);
        count += 1;
    }
    count
}

/// A DHCP message as the log names it: whose it is (the client's hardware address), what it asks or gives. A
/// REQUEST never has a yiaddr: the address it asks for is option 50 (SELECTING, INIT-REBOOT) or its ciaddr
/// (RENEWING, REBINDING), and only a SELECTING REQUEST names the server (option 54).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct DhcpMsg {
    /// option 53: 1 DISCOVER, 2 OFFER, 3 REQUEST, 4 DECLINE, 5 ACK, 6 NAK, 7 RELEASE, 8 INFORM
    pub kind: u8,
    pub client: [u8; 6], // chaddr
    pub ciaddr: [u8; 4],
    pub yiaddr: [u8; 4],
    pub requested: Option<[u8; 4]>, // option 50
    pub server: Option<[u8; 4]>,    // option 54
    /// sent to 255.255.255.255 (a REBINDING REQUEST is; a RENEWING one goes to the server)
    pub ip_broadcast: bool,
}

/// Whose a DHCP message is, for the log.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DhcpWho {
    /// iOS's own exchange.
    Ios,
    /// A message from the radio whose client is not iOS (another station's broadcast; counted, the first logged).
    Other,
}

/// A DHCP message in an Ethernet frame (IPv4, UDP between ports 67 and 68, the BOOTP header and the magic cookie).
/// For the log only: what the bridge carries is not changed.
pub fn dhcp(frame: &[u8]) -> Option<DhcpMsg> {
    let length = frame.len();
    if length < 14 + 20 + 8 + 240 || ethertype(frame) != 0x0800 {
        return None;
    }
    let ip = &frame[14..];
    let ihl = (ip[0] & 15) as usize * 4;
    if ip[0] >> 4 != 4 || ihl < 20 || ip[9] != 17 || length < 14 + ihl + 8 + 240 {
        return None;
    }
    let udp = &ip[ihl..];
    let b = &udp[8..];
    let (sport, dport) = (be::u16(udp, 0), be::u16(udp, 2));
    if !((sport == 68 && dport == 67) || (sport == 67 && dport == 68)) || be::u32(b, 236) != 0x6382_5363 {
        return None;
    }
    let mut m = DhcpMsg {
        ciaddr: b[12..16].try_into().expect("4 bytes"),
        yiaddr: b[16..20].try_into().expect("4 bytes"),
        ip_broadcast: be::u32(ip, 16) == 0xffff_ffff,
        ..Default::default()
    };
    if b[1] == 1 && b[2] == 6 {
        m.client = b[28..34].try_into().expect("6 bytes"); // Ethernet's hardware type and length
    }
    let options = &b[240..];
    let mut o = 0;
    while o < options.len() && options[o] != 255 {
        if options[o] == 0 {
            o += 1;
            continue;
        }
        if o + 2 > options.len() || o + 2 + options[o + 1] as usize > options.len() {
            break;
        }
        let (code, len, value) = (options[o], options[o + 1], &options[o + 2..o + 2 + options[o + 1] as usize]);
        match (code, len) {
            (53, 1) => m.kind = value[0],
            (50, 4) => m.requested = Some(value.try_into().expect("4 bytes")),
            (54, 4) => m.server = Some(value.try_into().expect("4 bytes")),
            _ => {}
        }
        o += 2 + len as usize;
    }
    (m.kind != 0).then_some(m)
}

impl DhcpMsg {
    /// A REQUEST's state (RFC 2131 4.3.2): "SELECTING", "INIT-REBOOT", "RENEWING", "REBINDING"; "" for others.
    pub fn request_kind(&self) -> &'static str {
        if self.kind != 3 {
            ""
        } else if self.server.is_some() {
            "SELECTING"
        } else if self.ciaddr == [0; 4] {
            "INIT-REBOOT"
        } else if self.ip_broadcast {
            "REBINDING"
        } else {
            "RENEWING"
        }
    }

    /// The address it is about: a REQUEST's or DECLINE's (option 50, else ciaddr), a RELEASE's or INFORM's ciaddr,
    /// a server's answer's yiaddr.
    pub fn address(&self) -> [u8; 4] {
        match self.kind {
            3 | 4 => self.requested.unwrap_or(self.ciaddr),
            7 | 8 => self.ciaddr,
            _ => self.yiaddr,
        }
    }

    /// A message a client sends (DISCOVER, REQUEST, DECLINE, RELEASE, INFORM), not a server's answer.
    pub fn from_client(&self) -> bool {
        matches!(self.kind, 1 | 3 | 4 | 7 | 8)
    }

    /// The log line of a message that crossed the bridge (without the "wifi: DHCP: " prefix). from_guest: iOS sent
    /// it.
    pub fn describe(&self, from_guest: bool, mac: &[u8; 6]) -> (DhcpWho, String) {
        const NAMES: [&str; 9] = ["?", "DISCOVER", "OFFER", "REQUEST", "DECLINE", "ACK", "NAK", "RELEASE", "INFORM"];
        let name = NAMES.get(self.kind as usize).copied().unwrap_or("?");
        let kind = self.request_kind();
        let sp = if kind.is_empty() { "" } else { " " };
        let a = self.address();
        let addr = format!("{}.{}.{}.{}", a[0], a[1], a[2], a[3]);
        let client = self.from_client();
        let of = if client { "for" } else { "of" };
        if !from_guest && self.client != *mac {
            let c = self.client;
            return (
                DhcpWho::Other,
                format!(
                    "another station's {name}{sp}{kind} {of} {addr} (client {:02x}:{:02x}:{:02x}:..)",
                    c[0], c[1], c[2]
                ),
            );
        }
        let line = if from_guest {
            format!("iOS sends {name}{sp}{kind} {of} {addr}")
        } else if client {
            // iOS's own message, back from the radio (an access point's reflection)
            format!("iOS's own {name}{sp}{kind} for {addr} came back from the radio")
        } else {
            let s = self.server.unwrap_or_default();
            format!("{name} of {addr} from {}.{}.{}.{}", s[0], s[1], s[2], s[3])
        };
        (DhcpWho::Ios, line)
    }
}

/// One multicast group of a generic netlink family (the attributes nested in one entry of CTRL_ATTR_MCAST_GROUPS):
/// its id when its name is `name`. CTRL_ATTR_MCAST_GRP_NAME (1, a string) and CTRL_ATTR_MCAST_GRP_ID (2, 32 bits)
/// are both read before they are compared: the kernel writes the id first, and a reader that kept "the last name
/// seen" took the id of the group after the one it wanted (the bridge then listened to nl80211's "regulatory" group
/// for scan events and never got one).
pub fn nl_group(attrs: &[u8], name: &str) -> Option<u32> {
    let (mut found, mut id) = (None, None);
    for (kind, data) in nl::attrs(attrs) {
        match kind {
            1 => found = Some(data),
            2 if data.len() == 4 => id = nl::u32_of(data),
            _ => {}
        }
    }
    let (found, id) = (found?, id?);
    let want = name.as_bytes();
    let matches = found.len() >= want.len()
        && found[..want.len()] == *want
        && (found.len() == want.len() || found[want.len()] == 0);
    matches.then_some(id)
}

// nl80211's numbers (linux/nl80211.h): NL80211_BAND_ATTR_FREQS, and of a frequency NL80211_FREQUENCY_ATTR_FREQ,
// _DISABLED, _NO_IR (passive scan only), _RADAR (DFS), _NO_HT40_MINUS, _NO_HT40_PLUS, _NO_80MHZ, _INDOOR_ONLY.
const BAND_FREQS: u16 = 1;
const FREQ_FREQ: u16 = 1;
const FREQ_DISABLED: u16 = 2;
const FREQ_NO_IR: u16 = 3;
const FREQ_RADAR: u16 = 5;
const FREQ_NO_HT40_MINUS: u16 = 9;
const FREQ_NO_HT40_PLUS: u16 = 10;
const FREQ_NO_80MHZ: u16 = 11;
const FREQ_INDOOR_ONLY: u16 = 14;

/// Every attribute of `payload` to `f`; false when the walk was cut short or `f` said so.
fn each(payload: &[u8], mut f: impl FnMut(u16, &[u8]) -> bool) -> bool {
    let mut a = nl::attrs(payload);
    for (kind, data) in a.by_ref() {
        if !f(kind, data) {
            return false;
        }
    }
    !a.cut()
}

/// The channels of an NL80211_ATTR_WIPHY_BANDS attribute (its payload: one nested attribute per band, each with
/// NL80211_BAND_ATTR_FREQS), appended to `out` up to `max`: the frequency and the OW_CH_* flags of each channel that
/// is not disabled. A split wiphy dump spreads the bands over several messages: call it for each. What was read
/// before a cut stays.
pub fn nl_bands(bands: &[u8], out: &mut Vec<OwChannel>, max: usize) {
    let freq_entry = |out: &mut Vec<OwChannel>, data: &[u8]| {
        let mut ch = OwChannel::default();
        let (mut disabled, mut have) = (false, false);
        let whole = each(data, |kind, value| {
            match kind {
                FREQ_FREQ if value.len() == 4 => {
                    ch.freq_mhz = nl::u32_of(value).unwrap_or(0) as u16;
                    have = true;
                }
                FREQ_DISABLED => disabled = true,
                FREQ_NO_IR => ch.flags |= OW_CH_PASSIVE,
                FREQ_RADAR => ch.flags |= OW_CH_RADAR,
                FREQ_INDOOR_ONLY => ch.flags |= OW_CH_INDOOR,
                FREQ_NO_HT40_MINUS | FREQ_NO_HT40_PLUS => ch.flags |= OW_CH_NO_HT40, // [I] either side kept as "no 40 MHz"
                FREQ_NO_80MHZ => ch.flags |= OW_CH_NO_80MHZ,
                _ => {}
            }
            true
        });
        if !whole {
            return false;
        }
        if have && !disabled && out.len() < max {
            out.push(ch);
        }
        true
    };
    each(bands, |_, band| {
        each(band, |kind, freqs| kind != BAND_FREQS || each(freqs, |_, entry| freq_entry(out, entry)))
    });
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::proto::wire::Wire;

    pub const MAC: [u8; 6] = [0x02, 0x11, 0x22, 0x33, 0x44, 0x55];

    /// Netlink attributes for the tests, nested ones opened and closed by hand (test_wifi.c's nla, nla_end).
    #[derive(Default)]
    pub struct Nla {
        pub b: Vec<u8>,
    }
    impl Nla {
        /// An attribute; where it starts (for a nested one: close it with end).
        pub fn put(&mut self, kind: u16, data: &[u8]) -> usize {
            let at = self.b.len();
            self.b.extend_from_slice(&((4 + data.len()) as u16).to_le_bytes());
            self.b.extend_from_slice(&kind.to_le_bytes());
            self.b.extend_from_slice(data);
            self.b.resize((self.b.len() + 3) & !3, 0);
            at
        }
        pub fn u32(&mut self, kind: u16, v: u32) -> usize {
            self.put(kind, &v.to_le_bytes())
        }
        pub fn open(&mut self, kind: u16) -> usize {
            self.put(kind, &[])
        }
        pub fn end(&mut self, at: usize) {
            let n = (self.b.len() - at) as u16;
            self.b[at..at + 2].copy_from_slice(&n.to_le_bytes());
        }
    }

    fn sum16(mut sum: u32, p: &[u8]) -> u32 {
        sum = add(sum, p);
        sum
    }
    fn sum_ok(mut sum: u32) -> bool {
        while sum >> 16 != 0 {
            sum = (sum & 0xffff) + (sum >> 16);
        }
        sum == 0xffff
    }
    // The IPv4 header and TCP checksums of a frame are right.
    fn tcp4_ok(f: &[u8]) -> bool {
        let n = f.len();
        let ip = &f[14..];
        let tcp_len = n - 34;
        if be::u16(ip, 2) as usize != n - 14 {
            return false;
        }
        sum_ok(sum16(0, &ip[..20])) && sum_ok(sum16(sum16(0, &ip[12..20]) + 6 + tcp_len as u32, &ip[20..20 + tcp_len]))
    }
    fn tcp6_ok(f: &[u8]) -> bool {
        let n = f.len();
        let ip = &f[14..];
        let tcp_len = n - 54;
        if be::u16(ip, 4) as usize != tcp_len {
            return false;
        }
        sum_ok(sum16(sum16(0, &ip[8..40]) + 6 + tcp_len as u32, &ip[40..40 + tcp_len]))
    }
    pub fn make_tcp(v6: bool, payload: usize, flags: u8) -> Vec<u8> {
        let l3 = if v6 { 40 } else { 20 };
        let mut f = vec![0u8; 14 + l3 + 20 + payload];
        f[..6].copy_from_slice(&MAC);
        f[6..12].copy_from_slice(b"\x02\xaa\xbb\xcc\xdd\xee");
        f[12..14].copy_from_slice(if v6 { &[0x86, 0xdd] } else { &[0x08, 0x00] });
        {
            let ip = &mut f[14..];
            if v6 {
                ip[0] = 0x60;
                ip[6] = 6;
                ip[7] = 64;
                ip[8] = 0xfe;
                ip[9] = 0x80;
                ip[23] = 1;
                ip[24] = 0xfe;
                ip[25] = 0x80;
                ip[39] = 2;
            } else {
                ip[0] = 0x45;
                ip[4] = 0x12;
                ip[5] = 0x34;
                ip[8] = 64;
                ip[9] = 6;
                ip[12..20].copy_from_slice(b"\xc0\xa8\x01\x01\xc0\xa8\x01\x02");
            }
        }
        let tcp = &mut f[14 + l3..];
        tcp[..8].copy_from_slice(&[0x01, 0xbb, 0xc0, 0x00, 0x00, 0x00, 0xff, 0x00]); // seq 0x0000ff00
        tcp[12] = 0x50;
        tcp[13] = flags;
        tcp[14] = 0xff;
        tcp[15] = 0xff;
        for i in 0..payload {
            tcp[20 + i] = (i * 7 + 1) as u8;
        }
        f
    }

    // test_wifi.c test_frames
    #[test]
    fn frame_rules_and_dhcp_parsing() {
        let mut f = [0u8; 64];
        f[..6].copy_from_slice(&[0xff; 6]);
        f[6..12].copy_from_slice(&MAC);
        f[12] = 0x08;
        f[13] = 0x06;
        assert!(ethertype(&f[..60]) == 0x0806 && ethertype(&f[..13]) == 0);
        assert!(to_radio(&f[..60], &MAC)); // the guest's ARP request
        f[6] ^= 4;
        assert!(!to_radio(&f[..60], &MAC)); // another source address: a private address of iOS's
        f[6] ^= 4;
        f[12] = 0x88;
        f[13] = 0x8e;
        assert!(!to_radio(&f[..60], &MAC) && !to_guest(&f[..60], &MAC)); // EAPOL is the supplicant's, both ways
        f[13] = 0xb5;
        assert!(!to_radio(&f[..60], &MAC) && !to_guest(&f[..60], &MAC)); // the station's own messages never cross
        f[12] = 0x08;
        f[13] = 0x00;
        assert!(to_guest(&f[..60], &MAC)); // broadcast
        f[..6].copy_from_slice(b"\x33\x33\x00\x00\x00\x01");
        assert!(to_guest(&f[..60], &MAC)); // multicast (IPv6 all nodes)
        f[..6].copy_from_slice(&MAC);
        assert!(to_guest(&f[..60], &MAC));
        f[5] ^= 1;
        let mut long = vec![0u8; 1515];
        long[..64].copy_from_slice(&f);
        assert!(!to_guest(&f[..60], &MAC) && !to_guest(&f[..10], &MAC) && !to_radio(&long, &MAC));

        // A DHCP OFFER (server to client) and a DISCOVER; a frame that is UDP but not DHCP; one cut inside its options.
        let mut d = vec![0u8; 14 + 20 + 8 + 240 + 16];
        d[..6].copy_from_slice(&MAC);
        d[12] = 0x08;
        d[14] = 0x45;
        d[23] = 17;
        d[34..38].copy_from_slice(&[0, 67, 0, 68]); // UDP 67 -> 68
        d[42] = 2;
        d[42 + 16..42 + 20].copy_from_slice(b"\x0a\x00\x01\x37"); // yiaddr 10.0.1.55
        d[42 + 236..42 + 240].copy_from_slice(b"\x63\x82\x53\x63");
        d[42 + 240..42 + 250].copy_from_slice(b"\x35\x01\x02\x36\x04\x0a\x00\x01\x01\xff"); // OFFER, server 10.0.1.1
        let m = dhcp(&d).unwrap();
        assert!(m.kind == 2 && m.yiaddr[3] == 0x37 && m.server.map(|s| s[3]) == Some(1));
        d[35] = 68;
        d[37] = 67;
        d[42 + 242] = 1;
        assert_eq!(dhcp(&d).map(|m| m.kind), Some(1));
        d[37] = 53;
        assert!(dhcp(&d).is_none());
        d[37] = 67;
        assert!(dhcp(&d[..14 + 20 + 8 + 240 + 2]).is_none() && dhcp(&d[..100]).is_none());
    }

    struct DhcpSpec {
        eth_dst: [u8; 6],
        eth_src: [u8; 6],
        client: [u8; 6],
        ip_src: &'static str,
        ip_dst: &'static str,
        ciaddr: Option<&'static str>,
        yiaddr: Option<&'static str>,
        requested: Option<&'static str>,
        server: Option<&'static str>,
        op: u8,
        kind: u8,
        broadcast_flag: bool,
    }
    fn ip4(s: Option<&str>) -> [u8; 4] {
        let mut out = [0u8; 4];
        if let Some(s) = s {
            for (o, part) in out.iter_mut().zip(s.split('.')) {
                *o = part.parse().unwrap();
            }
        }
        out
    }
    // An Ethernet frame with a DHCP message as iOS's and Android's clients put it (test_wifi.c make_dhcp).
    fn make_dhcp(s: &DhcpSpec) -> Vec<u8> {
        let mut b = vec![0u8; 240];
        b[0] = s.op;
        b[1] = 1;
        b[2] = 6;
        b[4..8].copy_from_slice(&[0x5a, 0x1e, 0x77, 0x03]); // xid
        if s.broadcast_flag {
            b[10] = 0x80;
        }
        b[12..16].copy_from_slice(&ip4(s.ciaddr));
        b[16..20].copy_from_slice(&ip4(s.yiaddr));
        b[28..34].copy_from_slice(&s.client);
        b[236..240].copy_from_slice(b"\x63\x82\x53\x63");
        b.extend_from_slice(&[53, 1, s.kind, 61, 7, 1]);
        b.extend_from_slice(&s.client);
        if s.op == 1 {
            b.extend_from_slice(&[55, 6, 1, 121, 3, 6, 15, 119, 57, 2, 5, 0xdc]);
        }
        if let Some(r) = s.requested {
            b.extend_from_slice(&[50, 4]);
            b.extend_from_slice(&ip4(Some(r)));
        }
        if let Some(r) = s.server {
            b.extend_from_slice(&[54, 4]);
            b.extend_from_slice(&ip4(Some(r)));
        }
        if s.op == 1 {
            b.extend_from_slice(&[12, 6]);
            b.extend_from_slice(b"iPhone");
        } else {
            b.extend_from_slice(&[51, 4, 0, 0, 0x0e, 0x10, 1, 4]);
            b.extend_from_slice(&ip4(Some("255.255.0.0")));
        }
        b.push(255);
        b.resize(b.len().max(300), 0); // BOOTP's minimum of 300 bytes, as clients pad
        let (udp_len, ip_len) = (8 + b.len(), 28 + b.len());
        let mut f = Vec::new();
        f.extend_from_slice(&s.eth_dst);
        f.extend_from_slice(&s.eth_src);
        f.extend_from_slice(&[0x08, 0x00]);
        let mut ip = vec![0x45, 0, (ip_len >> 8) as u8, ip_len as u8, 0, 0, 0, 0, 64, 17, 0, 0];
        ip.extend_from_slice(&ip4(Some(s.ip_src)));
        ip.extend_from_slice(&ip4(Some(s.ip_dst)));
        let sum = fold(add(0, &ip));
        ip[10..12].copy_from_slice(&sum.to_be_bytes());
        f.extend_from_slice(&ip);
        let (sp, dp) = if s.op == 1 { (68, 67) } else { (67, 68) };
        f.extend_from_slice(&[0, sp, 0, dp, (udp_len >> 8) as u8, udp_len as u8, 0, 0]);
        f.extend_from_slice(&b);
        f
    }

    // test_wifi.c test_dhcp: the REQUEST forms of RFC 2131 4.3.2 as clients send them.
    #[test]
    fn dhcp_lines() {
        const BCAST: [u8; 6] = [0xff; 6];
        const OTHER: [u8; 6] = [0x3c, 0x22, 0xfb, 0x10, 0x20, 0x30]; // another station on the network
        const ROUTER: [u8; 6] = [0x00, 0x1a, 0x2b, 0x3c, 0x4d, 0x5e]; // the access point's gateway
        let spec =
            |eth_dst, eth_src, client, ip_src, ip_dst, ciaddr, yiaddr, requested, server, op, kind, broadcast_flag| {
                DhcpSpec {
                    eth_dst,
                    eth_src,
                    client,
                    ip_src,
                    ip_dst,
                    ciaddr,
                    yiaddr,
                    requested,
                    server,
                    op,
                    kind,
                    broadcast_flag,
                }
            };
        let init_reboot = spec(
            BCAST,
            OTHER,
            OTHER,
            "0.0.0.0",
            "255.255.255.255",
            None,
            None,
            Some("172.16.235.99"),
            None,
            1,
            3,
            false,
        );
        let rebinding = spec(
            BCAST,
            OTHER,
            OTHER,
            "172.16.235.80",
            "255.255.255.255",
            Some("172.16.235.80"),
            None,
            None,
            None,
            1,
            3,
            false,
        );
        let renewing =
            spec(ROUTER, MAC, MAC, "172.16.235.71", "172.16.0.1", Some("172.16.235.71"), None, None, None, 1, 3, false);
        let selecting = spec(
            BCAST,
            MAC,
            MAC,
            "0.0.0.0",
            "255.255.255.255",
            None,
            None,
            Some("172.16.235.71"),
            Some("172.16.0.1"),
            1,
            3,
            false,
        );
        let discover = spec(BCAST, OTHER, OTHER, "0.0.0.0", "255.255.255.255", None, None, None, None, 1, 1, true);
        let ack = spec(
            MAC,
            ROUTER,
            MAC,
            "172.16.0.1",
            "172.16.235.71",
            None,
            Some("172.16.235.71"),
            None,
            Some("172.16.0.1"),
            2,
            5,
            false,
        );
        // What the log said before 2026-11-13 for a REQUEST: its yiaddr and option 54. No REQUEST has a yiaddr.
        for s in [&init_reboot, &rebinding, &renewing] {
            let m = dhcp(&make_dhcp(s)).unwrap();
            assert!(m.kind == 3 && m.yiaddr == [0; 4] && m.server.is_none());
        }
        // INIT-REBOOT of another station (a broadcast every station gets): its own line, counted apart from iOS's.
        let m = dhcp(&make_dhcp(&init_reboot)).unwrap();
        assert!(m.ip_broadcast && m.requested.is_some() && m.client == OTHER);
        assert_eq!(m.request_kind(), "INIT-REBOOT");
        let a = m.address();
        assert!(a[0] == 172 && a[3] == 99);
        assert_eq!(
            m.describe(false, &MAC),
            (DhcpWho::Other, "another station's REQUEST INIT-REBOOT for 172.16.235.99 (client 3c:22:fb:..)".into())
        );
        // REBINDING of another station: ciaddr, to the broadcast address.
        let m = dhcp(&make_dhcp(&rebinding)).unwrap();
        assert_eq!(m.request_kind(), "REBINDING");
        assert_eq!(
            m.describe(false, &MAC),
            (DhcpWho::Other, "another station's REQUEST REBINDING for 172.16.235.80 (client 3c:22:fb:..)".into())
        );
        // iOS's own RENEWING REQUEST (unicast to the server) and SELECTING one, as the guest sends them.
        let m = dhcp(&make_dhcp(&renewing)).unwrap();
        assert!(!m.ip_broadcast && m.request_kind() == "RENEWING");
        assert_eq!(m.describe(true, &MAC), (DhcpWho::Ios, "iOS sends REQUEST RENEWING for 172.16.235.71".into()));
        let m = dhcp(&make_dhcp(&selecting)).unwrap();
        assert_eq!(m.request_kind(), "SELECTING");
        assert_eq!(m.describe(true, &MAC), (DhcpWho::Ios, "iOS sends REQUEST SELECTING for 172.16.235.71".into()));
        // The same REQUEST back from the radio (an access point that reflects a station's broadcast to it).
        assert_eq!(
            m.describe(false, &MAC),
            (DhcpWho::Ios, "iOS's own REQUEST SELECTING for 172.16.235.71 came back from the radio".into())
        );
        // iOS's INIT-REBOOT (a known network's lease asked again after a join) sent by the guest.
        let own_reboot = DhcpSpec { eth_src: MAC, client: MAC, requested: Some("172.16.235.71"), ..init_reboot };
        let m = dhcp(&make_dhcp(&own_reboot)).unwrap();
        assert_eq!(m.describe(true, &MAC), (DhcpWho::Ios, "iOS sends REQUEST INIT-REBOOT for 172.16.235.71".into()));
        // Another station's DISCOVER; the server's ACK to iOS.
        let m = dhcp(&make_dhcp(&discover)).unwrap();
        assert_eq!(m.kind, 1);
        assert_eq!(
            m.describe(false, &MAC),
            (DhcpWho::Other, "another station's DISCOVER for 0.0.0.0 (client 3c:22:fb:..)".into())
        );
        let m = dhcp(&make_dhcp(&ack)).unwrap();
        assert!(m.kind == 5 && m.request_kind().is_empty());
        assert_eq!(m.describe(false, &MAC), (DhcpWho::Ios, "ACK of 172.16.235.71 from 172.16.0.1".into()));
        // An answer to another station (a server that broadcasts its OFFER or ACK): another station's.
        let other_ack = DhcpSpec { eth_dst: BCAST, client: OTHER, yiaddr: Some("172.16.235.99"), ..ack };
        let m = dhcp(&make_dhcp(&other_ack)).unwrap();
        assert_eq!(
            m.describe(false, &MAC),
            (DhcpWho::Other, "another station's ACK of 172.16.235.99 (client 3c:22:fb:..)".into())
        );
        // Cut inside the options: the type still read when option 53 came whole; cut in the header: not DHCP.
        let f = make_dhcp(&init_reboot);
        let m = dhcp(&f[..14 + 20 + 8 + 240 + 3]).unwrap();
        assert!(m.kind == 3 && m.requested.is_none());
        assert!(dhcp(&f[..14 + 20 + 8 + 239]).is_none());
    }

    // test_wifi.c test_bands
    #[test]
    fn bands() {
        // NL80211_ATTR_WIPHY_BANDS as the kernel writes it: band 0 (2.4 GHz) with three frequencies, one disabled and
        // one passive; band 1 (5 GHz) with its bitrates first, then a DFS channel (passive, radar, indoor only, no
        // 80 MHz) and one with no 40 MHz above it.
        let mut b = Nla::default();
        let band = b.open(0);
        let freqs = b.open(1);
        for (i, (f, flags)) in [(2412u32, &[][..]), (2467, &[2u16][..]), (2472, &[3][..])].iter().enumerate() {
            let e = b.open(i as u16);
            b.u32(1, *f);
            for fl in *flags {
                b.open(*fl);
            }
            b.end(e);
        }
        b.end(freqs);
        b.end(band);
        let band = b.open(1);
        b.put(2, &[0; 8]);
        let freqs = b.open(1);
        let e = b.open(0);
        b.u32(1, 5260);
        for fl in [3, 5, 14, 11] {
            b.open(fl);
        }
        b.end(e);
        let e = b.open(1);
        b.u32(1, 5745);
        b.open(10);
        b.end(e);
        b.end(freqs);
        b.end(band);
        let len = b.b.len();
        let mut ch = Vec::new();
        nl_bands(&b.b, &mut ch, 8);
        assert_eq!(ch.len(), 4);
        assert!(
            ch[0] == OwChannel { freq_mhz: 2412, flags: 0, pad: 0 }
                && ch[1].freq_mhz == 2472
                && ch[1].flags == OW_CH_PASSIVE
        );
        assert!(ch[2].freq_mhz == 5260 && ch[2].flags == OW_CH_PASSIVE | OW_CH_RADAR | OW_CH_INDOOR | OW_CH_NO_80MHZ);
        assert!(ch[3].freq_mhz == 5745 && ch[3].flags == OW_CH_NO_HT40);
        // A second message of a split dump appends; a full list stops; a cut band is left out, the bands before it stay.
        nl_bands(&b.b, &mut ch, 6);
        assert!(ch.len() == 6 && ch[5].freq_mhz == 2472);
        let mut two = Vec::new();
        nl_bands(&b.b, &mut two, 2);
        assert_eq!(two.len(), 2);
        let mut cut = Vec::new();
        nl_bands(&b.b[..len - 8], &mut cut, 8);
        assert_eq!(cut.len(), 2); // band 1 cut: band 0 stays
        let mut none = Vec::new();
        nl_bands(&b.b[..3], &mut none, 8);
        assert!(none.is_empty());
        assert_eq!(OwChannel::SIZE, 4);
    }

    // test_wifi.c test_groups_segments: the netlink groups, the cut of coalesced frames.
    #[test]
    fn groups_and_segments() {
        // A netlink multicast group as the kernel writes it (the id, then the name) and the other way round; another
        // name; a name that only begins with the wanted one; cut short.
        let scan_group = [8u8, 0, 2, 0, 3, 0, 0, 0, 9, 0, 1, 0, b's', b'c', b'a', b'n', 0, 0, 0, 0];
        let name_first = [9u8, 0, 1, 0, b's', b'c', b'a', b'n', 0, 0, 0, 0, 8, 0, 2, 0, 7, 0, 0, 0];
        let regulatory =
            [8u8, 0, 2, 0, 4, 0, 0, 0, 15, 0, 1, 0, b'r', b'e', b'g', b'u', b'l', b'a', b't', b'o', b'r', b'y', 0, 0];
        let scanner = [8u8, 0, 2, 0, 5, 0, 0, 0, 12, 0, 1, 0, b's', b'c', b'a', b'n', b'n', b'e', b'r', 0];
        assert_eq!(nl_group(&scan_group, "scan"), Some(3));
        assert_eq!(nl_group(&name_first, "scan"), Some(7));
        assert!(nl_group(&regulatory, "scan").is_none() && nl_group(&scanner, "scan").is_none());
        assert_eq!(nl_group(&regulatory, "regulatory"), Some(4));
        for cut in 0..scan_group.len() - 3 {
            assert!(nl_group(&scan_group[..cut], "scan").is_none());
        }

        let mut s: Vec<Vec<u8>> = Vec::new();
        // A frame of the link's size goes as it is.
        let f = make_tcp(false, 1000, 0x18);
        assert_eq!(segment(&f, 1448, false, |x| s.push(x.to_vec())), 1);
        assert!(s.len() == 1 && s[0] == f);
        // A coalesced IPv4 frame of 4000 bytes: three segments, lengths, sequence numbers, identifiers, flags, checksums.
        let big = make_tcp(false, 4000, 0x19); // FIN PSH ACK
        s.clear();
        assert_eq!(segment(&big, 1448, true, |x| s.push(x.to_vec())), 3);
        assert!(s[0].len() == 54 + 1448 && s[1].len() == 54 + 1448 && s[2].len() == 54 + 1104);
        assert!(tcp4_ok(&s[0]) && tcp4_ok(&s[1]) && tcp4_ok(&s[2]));
        assert_eq!(s[1][38..42], [0x00, 0x01, 0x04, 0xa8]); // 0xff00 + 1448
        assert!(s[0][47] == 0x10 && s[1][47] == 0x10 && s[2][47] == 0x19); // FIN and PSH on the last
        assert!(s[0][19] == 0x34 && s[1][19] == 0x35 && s[2][19] == 0x36);
        assert_eq!(s[2][54..], big[54 + 2896..54 + 2896 + 1104]);
        // A larger MSS than the link holds is cut to the link.
        s.clear();
        assert!(segment(&big, 9000, true, |x| s.push(x.to_vec())) == 3 && s[0].len() == 1514);
        // IPv6.
        let v6 = make_tcp(true, 3000, 0x10);
        s.clear();
        assert_eq!(segment(&v6, 1428, true, |x| s.push(x.to_vec())), 3);
        assert!(s[2].len() == 74 + 144 && tcp6_ok(&s[0]) && tcp6_ok(&s[2]));
        // Coalesced but of the link's size: one frame, its checksum made (the receive path left none).
        let f = make_tcp(false, 900, 0x18);
        s.clear();
        assert!(segment(&f, 1448, true, |x| s.push(x.to_vec())) == 1 && s[0].len() == f.len() && tcp4_ok(&s[0]));
        // Too long and not TCP: dropped. Of the link's size and not TCP: as it is.
        let mut udp = make_tcp(false, 4000, 0x19);
        udp.resize(14 + 40 + 20 + 4000, 0);
        udp[14 + 9] = 17;
        s.clear();
        assert!(segment(&udp[..3000], 1448, true, |x| s.push(x.to_vec())) == 0 && s.is_empty());
        assert!(segment(&udp[..600], 1448, true, |x| s.push(x.to_vec())) == 1 && s.len() == 1 && s[0] == udp[..600]);
    }
}
