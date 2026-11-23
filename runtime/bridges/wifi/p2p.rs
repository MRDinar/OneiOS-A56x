// SPDX-License-Identifier: MIT
//! What the phone's Wi-Fi radios offer for a peer-to-peer link, as nl80211 reports them (docs/hardware.md
//! "AirDrop"): the interface types and the combinations of them, the action frames a station may send and receive,
//! off-channel sends, Wi-Fi Aware (NAN), Wi-Fi Direct, the vendor's commands, every radio (wiphy) and every interface.
//! AirDrop runs on AWDL, Apple's own peer-to-peer link; on this phone it can come from Google's Wonder radio (a
//! soft-MAC over the Wi-Fi driver's PASSTHRU vdev, vendor commands under 00:1a:11: docs/hardware.md 4a), from a
//! monitor interface with injection beside the station (OWL's way), or from the vendor's own AWDL. `wifictl p2p`
//! prints this report on the phone; the parser and the report are pure. C: wifi/p2p.c.

use super::nl;
use std::fmt::Write;

// nl80211's numbers (linux/nl80211.h of NDK r28c), so that this file needs no Linux header.
const A_WIPHY: u16 = 1;
const A_WIPHY_NAME: u16 = 2;
const A_IFINDEX: u16 = 3;
const A_IFNAME: u16 = 4;
const A_IFTYPE: u16 = 5;
const A_MAC: u16 = 6;
const A_SUPPORTED_IFTYPES: u16 = 32;
const A_WIPHY_FREQ: u16 = 38;
const A_SUPPORTED_COMMANDS: u16 = 50;
const A_TX_FRAME_TYPES: u16 = 99;
const A_RX_FRAME_TYPES: u16 = 100;
const A_FRAME_TYPE: u16 = 101;
const A_OFFCHANNEL_TX_OK: u16 = 108;
const A_MAX_ROC_DURATION: u16 = 111;
const A_INTERFACE_COMBINATIONS: u16 = 120;
const A_SOFTWARE_IFTYPES: u16 = 121;
const A_FEATURE_FLAGS: u16 = 143;
const A_VENDOR_DATA: u16 = 197;
const A_VENDOR_EVENTS: u16 = 198;
const A_EXT_FEATURES: u16 = 217;
const COMB_LIMITS: u16 = 1;
const COMB_MAXNUM: u16 = 2;
const COMB_NUM_CHANNELS: u16 = 4;
const LIMIT_MAX: u16 = 1;
const LIMIT_TYPES: u16 = 2;
const CMD_REMAIN_ON_CHANNEL: u32 = 55;
const CMD_FRAME: u32 = 59;
const CMD_START_P2P_DEVICE: u32 = 89;
const CMD_VENDOR: u32 = 103;
const CMD_START_NAN: u32 = 115;
const FEATURE_ACTIVE_MONITOR: u32 = 1 << 17;
const EXT_SECURE_NAN: usize = 63;
const FC_ACTION: u16 = 0x00d0; // a management frame of subtype 13 (action), as nl80211 writes the frame control field

pub const IF_STATION: u32 = 2;
pub const IF_AP: u32 = 3;
pub const IF_MONITOR: u32 = 6;
pub const IF_P2P_CLIENT: u32 = 8;
pub const IF_P2P_GO: u32 = 9;
pub const IF_P2P_DEVICE: u32 = 10;
pub const IF_NAN: u32 = 12;

pub const IFTYPES: u32 = 13; // NUM_NL80211_IFTYPES
pub const COMBINATIONS: usize = 8;
pub const COMB_LIMITS_MAX: usize = 6;
pub const VENDOR_CMDS: usize = 256;
pub const IFACES: usize = 16;
pub const WIPHYS: usize = 4;
/// Wonder's vendor commands (Google's OUI_GOOGLE).
pub const OUI_GOOGLE: u32 = 0x001a11;
/// The monitor interface `wifictl wonder` makes on Wonder's radio.
pub const WONDER_IFNAME: &str = "wondermon0";

const IFTYPE_NAMES: [&str; IFTYPES as usize] = [
    "unspecified",
    "ad-hoc",
    "station",
    "access point",
    "ap-vlan",
    "wds",
    "monitor",
    "mesh",
    "p2p-client",
    "p2p-go",
    "p2p-device",
    "ocb",
    "nan",
];

pub fn iftype_name(kind: u32) -> &'static str {
    IFTYPE_NAMES.get(kind as usize).copied().unwrap_or("?")
}

/// A name attribute as C's "%.*s" into a buffer of max + 1: up to its first NUL, at most `max` bytes.
fn c_text(data: &[u8], max: usize) -> String {
    xh_core::text::nul_text(&data[..data.len().min(max)])
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct VendorCmd {
    pub oui: u32,
    pub subcmd: u32,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Combination {
    pub max_interfaces: u32,
    pub channels: u32,
    /// Each limit: (interface types as a bit mask, how many of them).
    pub limits: Vec<(u32, u32)>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Iface {
    pub name: String,
    pub ifindex: u32,
    pub wiphy: u32,
    pub iftype: u32,
    pub freq_mhz: u32,
    pub mac: [u8; 6],
}

/// One radio's capabilities. Interface types are bit masks: bit n is nl80211_iftype n.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Caps {
    pub have_wiphy: bool,
    pub wiphy: u32,
    pub name: String,
    pub iftypes: u32,
    pub software_iftypes: u32,
    pub combinations: Vec<Combination>,
    pub action_tx: u32, // the interface types that may send / register for action frames
    pub action_rx: u32,
    pub offchannel_tx: bool,
    pub cmd_frame: bool,
    pub cmd_remain_on_channel: bool,
    pub cmd_start_nan: bool,
    pub cmd_start_p2p_device: bool,
    pub cmd_vendor: bool,
    pub max_roc_ms: u32,
    pub features: u32,
    pub ext_features: Vec<u8>, // at most 16 bytes kept
    pub commands: Vec<VendorCmd>,
    pub events: Vec<VendorCmd>,
    pub ifaces: Vec<Iface>,
}

/// A set of interface types: one flag attribute per type (the attribute's type is the interface type).
fn type_flags(data: &[u8]) -> u32 {
    nl::attrs(data).filter(|(t, _)| *t < 32).fold(0, |m, (t, _)| m | 1 << t)
}

/// TX_FRAME_TYPES and RX_FRAME_TYPES: one nested attribute per interface type, holding the frame control values.
fn action_types(data: &[u8]) -> u32 {
    let mut mask = 0;
    for (iftype, values) in nl::attrs(data) {
        let action = nl::attrs(values).any(|(t, v)| t == A_FRAME_TYPE && nl::u16_of(v) == Some(FC_ACTION));
        if action && iftype < 32 {
            mask |= 1 << iftype;
        }
    }
    mask
}

/// VENDOR_DATA and VENDOR_EVENTS: each entry a struct nl80211_vendor_cmd_info { u32 vendor_id; u32 subcmd; }.
fn vendor_entries(data: &[u8], list: &mut Vec<VendorCmd>) {
    for (_, e) in nl::attrs(data) {
        if e.len() < 8 {
            continue;
        }
        let v = VendorCmd { oui: nl::u32_of(&e[..4]).unwrap_or(0), subcmd: nl::u32_of(&e[4..8]).unwrap_or(0) };
        if !list.contains(&v) && list.len() < VENDOR_CMDS {
            list.push(v); // a split dump repeats nothing, but be safe
        }
    }
}

fn combination(data: &[u8]) -> Combination {
    let mut c = Combination::default();
    for (kind, v) in nl::attrs(data) {
        match kind {
            COMB_LIMITS => {
                for (_, limit) in nl::attrs(v) {
                    let (mut types, mut max) = (0, 0);
                    for (t, lv) in nl::attrs(limit) {
                        match t {
                            LIMIT_MAX => max = nl::u32_of(lv).unwrap_or(max),
                            LIMIT_TYPES => types |= type_flags(lv),
                            _ => {}
                        }
                    }
                    if c.limits.len() < COMB_LIMITS_MAX {
                        c.limits.push((types, max));
                    }
                }
            }
            COMB_MAXNUM => c.max_interfaces = nl::u32_of(v).unwrap_or(c.max_interfaces),
            COMB_NUM_CHANNELS => c.channels = nl::u32_of(v).unwrap_or(c.channels),
            _ => {}
        }
    }
    c
}

fn iface_of(attrs: &[u8]) -> Option<Iface> {
    let mut i = Iface::default();
    for (kind, data) in nl::attrs(attrs) {
        match kind {
            A_IFNAME => i.name = c_text(data, 15),
            A_IFINDEX => i.ifindex = nl::u32_of(data).unwrap_or(i.ifindex),
            A_WIPHY => i.wiphy = nl::u32_of(data).unwrap_or(i.wiphy),
            A_IFTYPE => i.iftype = nl::u32_of(data).unwrap_or(i.iftype),
            A_MAC if data.len() == 6 => i.mac.copy_from_slice(data),
            A_WIPHY_FREQ => i.freq_mhz = nl::u32_of(data).unwrap_or(i.freq_mhz),
            _ => {}
        }
    }
    (i.iftype != 0 || i.ifindex != 0 || !i.name.is_empty()).then_some(i)
}

impl Caps {
    /// One GET_WIPHY message's attributes (a split dump: call it for each).
    pub fn wiphy(&mut self, attrs: &[u8]) {
        for (kind, data) in nl::attrs(attrs) {
            match kind {
                A_WIPHY => {
                    if let Some(w) = nl::u32_of(data) {
                        self.wiphy = w;
                        self.have_wiphy = true;
                    }
                }
                A_WIPHY_NAME => self.name = c_text(data, 31),
                A_SUPPORTED_IFTYPES => self.iftypes |= type_flags(data),
                A_SOFTWARE_IFTYPES => self.software_iftypes |= type_flags(data),
                A_INTERFACE_COMBINATIONS => {
                    for (_, entry) in nl::attrs(data) {
                        if self.combinations.len() < COMBINATIONS {
                            self.combinations.push(combination(entry));
                        }
                    }
                }
                A_TX_FRAME_TYPES => self.action_tx |= action_types(data),
                A_RX_FRAME_TYPES => self.action_rx |= action_types(data),
                A_OFFCHANNEL_TX_OK => self.offchannel_tx = true,
                A_MAX_ROC_DURATION => self.max_roc_ms = nl::u32_of(data).unwrap_or(self.max_roc_ms),
                A_SUPPORTED_COMMANDS => {
                    for cmd in nl::attrs(data).filter_map(|(_, d)| nl::u32_of(d)) {
                        match cmd {
                            CMD_FRAME => self.cmd_frame = true,
                            CMD_REMAIN_ON_CHANNEL => self.cmd_remain_on_channel = true,
                            CMD_START_NAN => self.cmd_start_nan = true,
                            CMD_START_P2P_DEVICE => self.cmd_start_p2p_device = true,
                            CMD_VENDOR => self.cmd_vendor = true,
                            _ => {}
                        }
                    }
                }
                A_FEATURE_FLAGS => self.features = nl::u32_of(data).unwrap_or(self.features),
                A_EXT_FEATURES => self.ext_features = data[..data.len().min(16)].to_vec(),
                A_VENDOR_DATA => vendor_entries(data, &mut self.commands),
                A_VENDOR_EVENTS => vendor_entries(data, &mut self.events),
                _ => {}
            }
        }
    }

    /// One GET_INTERFACE message's attributes.
    pub fn iface(&mut self, attrs: &[u8]) {
        if let Some(i) = iface_of(attrs) {
            if self.ifaces.len() < IFACES {
                self.ifaces.push(i);
            }
        }
    }

    /// Interface types a and b can be up at the same time: a combination allows two interfaces, and a sits in one
    /// limit and b in another (each limit allowing one or more), or both in one limit that allows two or more.
    pub fn combines(&self, a: u32, b: u32) -> bool {
        self.combinations.iter().filter(|k| k.max_interfaces >= 2).any(|k| {
            k.limits.iter().enumerate().any(|(la, &(ta, ma))| {
                k.limits.iter().enumerate().any(|(lb, &(tb, mb))| {
                    ta >> a & 1 != 0 && tb >> b & 1 != 0 && if la != lb { ma >= 1 && mb >= 1 } else { ma >= 2 }
                })
            })
        })
    }

    pub fn has_vendor(&self, oui: u32, subcmd: u32) -> bool {
        self.commands.contains(&VendorCmd { oui, subcmd })
    }

    /// The report of one radio: the capabilities, every interface, then one "p2p:" line per way to an AWDL link.
    pub fn report(&self) -> String {
        let mut o = String::new();
        facts(&mut o, self);
        for f in &self.ifaces {
            iface_line(&mut o, f, None);
        }
        verdicts(&mut o, self, None);
        o
    }
}

fn types(o: &mut String, mask: u32) {
    let names: Vec<&str> = (0..IFTYPES).filter(|t| mask >> t & 1 != 0).map(iftype_name).collect();
    o.push_str(if names.is_empty() { "none" } else { "" });
    o.push_str(&names.join(", "));
}

fn oui_name(oui: u32) -> &'static str {
    match oui {
        0x001374 => "Qualcomm",
        0x001018 => "Broadcom",
        0x001a11 => "Google",
        0x0017f2 => "Apple",
        0x0000f0 | 0x001632 => "Samsung",
        0x506f9a => "Wi-Fi Alliance",
        _ => "",
    }
}

/// The vendor commands or events by OUI: "  001374 Qualcomm: 12 81\n".
fn vendor_lines(o: &mut String, list: &[VendorCmd], prefix: &str) {
    for (i, c) in list.iter().enumerate() {
        if i == 0 || c.oui != list[i - 1].oui {
            let _ = write!(o, "{}  {prefix}{:06x} {}:", if i > 0 { "\n" } else { "" }, c.oui, oui_name(c.oui));
        }
        let _ = write!(o, " {}", c.subcmd);
    }
    if !list.is_empty() {
        o.push('\n');
    }
}

/// The facts of one radio, from its name to its vendor events.
fn facts(o: &mut String, c: &Caps) {
    let _ = writeln!(o, "radio: {} (wiphy {})", if c.name.is_empty() { "?" } else { &c.name }, c.wiphy);
    o.push_str("interface types: ");
    types(o, c.iftypes);
    o.push('\n');
    if c.software_iftypes != 0 {
        o.push_str("software interface types: ");
        types(o, c.software_iftypes);
        o.push('\n');
    }
    for (i, k) in c.combinations.iter().enumerate() {
        let _ = write!(o, "combination {}: up to {} interfaces on {} channel(s):", i + 1, k.max_interfaces, k.channels);
        for (l, (t, max)) in k.limits.iter().enumerate() {
            let _ = write!(o, "{} {max} of {{", if l > 0 { ";" } else { "" });
            types(o, *t);
            o.push('}');
        }
        o.push('\n');
    }
    o.push_str("action frames: sent by ");
    types(o, c.action_tx);
    o.push_str("; received by ");
    types(o, c.action_rx);
    o.push('\n');
    let yes = |b: bool| if b { "yes" } else { "no" };
    let _ =
        write!(o, "off-channel sends: {}; remain on channel: {}", yes(c.offchannel_tx), yes(c.cmd_remain_on_channel));
    if c.max_roc_ms != 0 {
        let _ = write!(o, " (up to {} ms)", c.max_roc_ms);
    }
    o.push('\n');
    let _ = writeln!(o, "vendor commands: {}, vendor events: {}", c.commands.len(), c.events.len());
    vendor_lines(o, &c.commands, "");
    vendor_lines(o, &c.events, "events ");
}

fn iface_line(o: &mut String, f: &Iface, wonder: Option<&Caps>) {
    let m = f.mac;
    let _ = write!(
        o,
        "interface: {} (index {}) {}, wiphy {}, {:02x}:{:02x}:{:02x}:{:02x}:{:02x}:{:02x}",
        if f.name.is_empty() { "(no netdev)" } else { &f.name },
        f.ifindex,
        iftype_name(f.iftype),
        f.wiphy,
        m[0],
        m[1],
        m[2],
        m[3],
        m[4],
        m[5]
    );
    if f.freq_mhz != 0 {
        let _ = write!(o, ", {} MHz", f.freq_mhz);
    }
    if wonder.is_some_and(|w| f.wiphy == w.wiphy) {
        o.push_str(" (on Wonder's radio)");
    } else if f.name.starts_with("wondertap") {
        o.push_str(" (the Wi-Fi driver's PASSTHRU vdev for Wonder)");
    }
    o.push('\n');
}

/// The verdicts: each way an AWDL link could be made on the station's radio, and Wonder's path when there is one.
fn verdicts(o: &mut String, c: &Caps, wonder: Option<&Caps>) {
    let yes = |b: bool| if b { "yes" } else { "no" };
    let monitor = c.iftypes >> IF_MONITOR & 1 != 0;
    let monitor_beside = monitor && c.combines(IF_STATION, IF_MONITOR);
    let _ = write!(o, "p2p: monitor mode: {}", yes(monitor));
    if monitor {
        let _ = write!(
            o,
            " (beside the station: {}; active monitor, with ACKs: {}; frame injection is not reported by nl80211)",
            yes(monitor_beside),
            yes(c.features & FEATURE_ACTIVE_MONITOR != 0)
        );
    }
    o.push('\n');
    let (sta_tx, sta_rx) = (c.action_tx >> IF_STATION & 1 != 0, c.action_rx >> IF_STATION & 1 != 0);
    let _ = writeln!(
        o,
        "p2p: AWDL's action frames (vendor action, Apple's 00:17:f2) on the station: send {}, receive {}{}",
        yes(sta_tx && c.cmd_frame),
        yes(sta_rx),
        if sta_tx && c.offchannel_tx { ", off channel too" } else { "" }
    );
    let ext_secure_nan = c.ext_features.get(EXT_SECURE_NAN / 8).is_some_and(|b| b >> (EXT_SECURE_NAN % 8) & 1 != 0);
    let _ = writeln!(
        o,
        "p2p: Wi-Fi Aware (NAN) through nl80211: {}{} (the vendor's NAN goes through its own HAL and vendor commands, not this)",
        yes(c.iftypes >> IF_NAN & 1 != 0 && c.cmd_start_nan),
        if ext_secure_nan { ", secure NAN" } else { "" }
    );
    let _ = writeln!(
        o,
        "p2p: Wi-Fi Direct: {} (AirDrop does not use it)",
        yes(c.iftypes >> IF_P2P_GO & 1 != 0 && c.iftypes >> IF_P2P_CLIENT & 1 != 0)
    );
    if c.commands.iter().any(|v| v.oui == 0x0017f2) {
        o.push_str(
            "p2p: the driver has vendor commands under Apple's OUI 00:17:f2 [I: an AWDL interface of the vendor's]\n",
        );
    }
    match wonder {
        Some(w) => {
            let _ = writeln!(
                o,
                "p2p: for AirDrop iOS needs AWDL: Wonder's raw 802.11 path is offered (wiphy {}): an AWDL engine on the host can run on a \
                 monitor interface of it beside wlan0 (`wifictl wonder` opens one and counts the AWDL frames)",
                w.wiphy
            );
        }
        None => {
            let _ = write!(
                o,
                "p2p: for AirDrop iOS needs AWDL: {}",
                if monitor_beside {
                    "a monitor interface beside the station is offered (OWL's way, if the driver injects frames); no Wonder radio"
                } else {
                    "no Wonder radio and no monitor beside the station: only the vendor's own AWDL can carry it (compare this report with \
                     Quick Share's \"Share with Apple devices\" on and off: Wonder may come only with it)"
                }
            );
        }
    }
}

/// Every radio of the phone (docs/hardware.md 4a): a GET_WIPHY dump with no interface named gives each wiphy, split
/// over several messages that each name their wiphy (NL80211_ATTR_WIPHY first); a GET_INTERFACE dump gives every
/// interface.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Radios {
    pub wiphys: Vec<Caps>,
    pub wiphys_dropped: u32, // wiphys past WIPHYS are counted, not kept
    pub ifaces: Vec<Iface>,
}

/// Wonder's vendor subcommand by its number ("SET_FREQUENCY", ...), "?" for one we do not know.
pub fn wonder_subcmd_name(subcmd: u32) -> &'static str {
    const NAMES: [&str; 16] = [
        "UNSPEC",
        "SET_FREQUENCY",
        "SET_FILTER",
        "SET_FIXED_TX_RATE",
        "SET_REGULATORY",
        "GET_IF_MAC_ADDR",
        "SET_CHANNEL_SCHEDULE_REQ",
        "GET_MAC_TSF",
        "GET_CAP",
        "GET_CHANNEL_STATUS_REPORT",
        "SET_STATION_INFO",
        "SET_FEATURES",
        "?",
        "?",
        "?",
        "SET_TX_RATE_TEST",
    ];
    NAMES.get(subcmd as usize).copied().unwrap_or("?")
}

impl Radios {
    /// One GET_WIPHY message's attributes: to the wiphy it names (a message that names none: the last one's, never
    /// seen from cfg80211).
    pub fn wiphy(&mut self, attrs: &[u8]) {
        let id = nl::attrs(attrs).find(|(t, d)| *t == A_WIPHY && d.len() == 4).and_then(|(_, d)| nl::u32_of(d));
        let i = match id {
            None if self.wiphys.is_empty() => return,
            None => self.wiphys.len() - 1,
            Some(id) => match self.wiphys.iter().position(|w| w.wiphy == id) {
                Some(i) => i,
                None if self.wiphys.len() == WIPHYS => {
                    self.wiphys_dropped += 1;
                    return;
                }
                None => {
                    self.wiphys.push(Caps::default());
                    self.wiphys.len() - 1
                }
            },
        };
        self.wiphys[i].wiphy(attrs);
    }

    /// One GET_INTERFACE message's attributes.
    pub fn iface(&mut self, attrs: &[u8]) {
        if let Some(i) = iface_of(attrs) {
            if self.ifaces.len() < IFACES {
                self.ifaces.push(i);
            }
        }
    }

    /// Google's Wonder radio (a mac80211 soft-MAC over the Wi-Fi driver's PASSTHRU vdev): the wiphy named
    /// "wonder...", else one with vendor commands under Google's OUI that offers monitor mode but no access point (the
    /// Wi-Fi driver's own wiphy offers an access point; a Broadcom driver also has commands under Google's OUI).
    pub fn wonder(&self) -> Option<&Caps> {
        self.wiphys.iter().find(|w| w.name.starts_with("wonder")).or_else(|| {
            self.wiphys.iter().find(|c| {
                c.commands.iter().any(|v| v.oui == OUI_GOOGLE)
                    && c.iftypes >> IF_MONITOR & 1 != 0
                    && c.iftypes >> IF_AP & 1 == 0
            })
        })
    }

    /// What Wonder offers and who uses it now.
    fn wonder_block(&self, o: &mut String, w: Option<&Caps>) {
        let tap = self.ifaces.iter().rev().find(|f| f.name.starts_with("wondertap"));
        let Some(w) = w else {
            let _ = writeln!(
                o,
                "p2p: Wonder: no Wonder radio (no wiphy named wonder, none with vendor commands under Google's 00:1a:11 and monitor mode \
                 without an access point){}",
                if tap.is_some() { "; but a wondertap interface is up: its Wonder radio is not in the dump [U]" } else { "" }
            );
            return;
        };
        let _ = write!(
            o,
            "p2p: Wonder: wiphy {} ({}), interface types ",
            w.wiphy,
            if w.name.is_empty() { "?" } else { &w.name }
        );
        types(o, w.iftypes);
        o.push('\n');
        o.push_str("p2p: Wonder: vendor subcommands under 00:1a:11:");
        let google: Vec<u32> = w.commands.iter().filter(|v| v.oui == OUI_GOOGLE).map(|v| v.subcmd).collect();
        for s in &google {
            let _ = write!(o, " {s} {}", wonder_subcmd_name(*s));
        }
        let _ = writeln!(o, "{}", if google.is_empty() { " none" } else { "" });
        let top = google.iter().copied().max().unwrap_or(0);
        let basic = [1, 4, 5].iter().all(|s| w.has_vendor(OUI_GOOGLE, *s));
        let _ = writeln!(
            o,
            "p2p: Wonder: version {}",
            if !basic {
                "unknown: SET_FREQUENCY, SET_REGULATORY or GET_IF_MAC_ADDR is missing (`wifictl wonder` refuses it)"
            } else if top >= 6 {
                "1.6 or later (a channel schedule: the firmware can hop channels with the TSF)"
            } else {
                "1.4 or 1.5 (subcommands 1 to 5 only: one channel, no hopping; Samsung's S928BXXU5DZDP source)"
            }
        );
        let mut users = 0;
        for f in self.ifaces.iter().filter(|f| f.wiphy == w.wiphy) {
            let _ = writeln!(
                o,
                "p2p: Wonder: in use: {} ({}){}",
                if f.name.is_empty() { "(no netdev)" } else { &f.name },
                iftype_name(f.iftype),
                if f.name == WONDER_IFNAME {
                    ": wifictl wonder's own (a session that did not end: the next `wifictl wonder` removes it)"
                } else {
                    ""
                }
            );
            users += 1;
        }
        if let Some(tap) = tap {
            let _ = write!(
                o,
                "p2p: Wonder: the PASSTHRU vdev {} (index {}, wiphy {}) is up",
                tap.name, tap.ifindex, tap.wiphy
            );
            if tap.freq_mhz != 0 {
                let _ = write!(o, " on {} MHz", tap.freq_mhz);
            }
            o.push('\n');
        }
        if users == 0 && tap.is_none() {
            o.push_str("p2p: Wonder: idle (no interface on it, no wondertap vdev)\n");
        }
    }

    /// The report `wifictl p2p` prints: each radio's capabilities, every interface, a "p2p: Wonder:" block, then the
    /// "p2p:" verdicts for the station's radio (the wiphy of the interface named `station`, else the first radio).
    pub fn report(&self, station: Option<&str>) -> String {
        let mut o = String::new();
        if self.wiphys.is_empty() {
            o.push_str("radio: none (nl80211 gave no wiphy)\n");
            return o;
        }
        let wonder = self.wonder();
        let mut home = &self.wiphys[0];
        for f in self.ifaces.iter().filter(|f| station == Some(f.name.as_str())) {
            if let Some(w) = self.wiphys.iter().rev().find(|w| w.wiphy == f.wiphy) {
                home = w;
            }
        }
        let _ = writeln!(
            o,
            "radios: {}{}",
            self.wiphys.len(),
            if self.wiphys_dropped > 0 { " (more were not kept)" } else { "" }
        );
        for w in &self.wiphys {
            facts(&mut o, w);
            if wonder.is_some_and(|x| std::ptr::eq(x, w)) {
                o.push_str("  (Google's Wonder radio)\n");
            }
            if let (true, Some(s)) = (std::ptr::eq(home, w), station) {
                let _ = writeln!(o, "  ({s}'s radio)");
            }
        }
        for f in &self.ifaces {
            iface_line(&mut o, f, wonder);
        }
        self.wonder_block(&mut o, wonder);
        verdicts(&mut o, home, wonder);
        o
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::super::frames::tests::{Nla, MAC};
    use super::*;

    /// A flag attribute per interface type (SUPPORTED_IFTYPES, a limit's TYPES).
    pub fn iftype_flags(b: &mut Nla, kind: u16, mask: u32) {
        let at = b.open(kind);
        for t in 0..13u16 {
            if mask >> t & 1 != 0 {
                b.open(t);
            }
        }
        b.end(at);
    }

    const STA: u32 = 2;
    const AP: u32 = 3;
    const MON: u32 = 6;
    const P2PC: u32 = 8;
    const P2PGO: u32 = 9;
    const P2PDEV: u32 = 10;
    const NANIF: u32 = 12;

    fn limit(b: &mut Nla, kind: u16, max: u32, types: u32) {
        let one = b.open(kind);
        b.u32(1, max);
        iftype_flags(b, 2, types);
        b.end(one);
    }

    // test_wifi.c test_p2p: wifictl p2p: the radio's peer-to-peer abilities from nl80211
    #[test]
    fn one_radio() {
        let mut c = Caps::default();
        // The first message of a split wiphy dump: the radio, its types, two combinations, the action frames, the commands.
        let mut b = Nla::default();
        b.u32(1, 0); // WIPHY 0
        b.put(2, b"phy0\0"); // WIPHY_NAME
        iftype_flags(&mut b, 32, 1 << STA | 1 << AP | 1 << MON | 1 << P2PC | 1 << P2PGO | 1 << P2PDEV);
        let at = b.open(120); // INTERFACE_COMBINATIONS
        let e = b.open(1); //   1: 2 interfaces on 1 channel: 1 station, 1 of {ap, p2p}
        let l = b.open(1); //     LIMITS
        limit(&mut b, 1, 1, 1 << STA);
        limit(&mut b, 2, 1, 1 << AP | 1 << P2PC | 1 << P2PGO);
        b.end(l);
        b.u32(2, 2);
        b.u32(4, 1);
        b.end(e);
        let e = b.open(2); //   2: the monitor alone (1 interface)
        let l = b.open(1);
        limit(&mut b, 1, 1, 1 << MON | 1 << STA);
        b.end(l);
        b.u32(2, 1);
        b.u32(4, 1);
        b.end(e);
        b.end(at);
        let at = b.open(99); // TX_FRAME_TYPES: the station sends action and probe requests
        let e = b.open(STA as u16);
        b.put(101, &0x00d0u16.to_le_bytes());
        b.put(101, &0x0040u16.to_le_bytes());
        b.end(e);
        let e = b.open(AP as u16);
        b.put(101, &0x0050u16.to_le_bytes()); // the AP: probe responses only
        b.end(e);
        b.end(at);
        let at = b.open(100); // RX_FRAME_TYPES: the station registers for action frames
        let e = b.open(STA as u16);
        b.put(101, &0x00d0u16.to_le_bytes());
        b.end(e);
        b.end(at);
        b.open(108); // OFFCHANNEL_TX_OK
        b.u32(111, 5000); // MAX_REMAIN_ON_CHANNEL_DURATION
        let at = b.open(50); // SUPPORTED_COMMANDS: FRAME, REMAIN_ON_CHANNEL, VENDOR
        b.u32(1, 59);
        b.u32(2, 55);
        b.u32(3, 103);
        b.end(at);
        let vendor = [(0x001374u32, 12u32), (0x001374, 81), (0x0000f0, 7)];
        let pair = |(o, s): (u32, u32)| [o.to_le_bytes(), s.to_le_bytes()].concat();
        let at = b.open(197); // VENDOR_DATA
        for (i, v) in vendor.iter().enumerate() {
            b.put(i as u16 + 1, &pair(*v));
        }
        b.end(at);
        c.wiphy(&b.b);
        assert!(c.have_wiphy && c.wiphy == 0 && c.name == "phy0");
        assert_eq!(c.iftypes, 1 << STA | 1 << AP | 1 << MON | 1 << P2PC | 1 << P2PGO | 1 << P2PDEV);
        assert!(c.combinations.len() == 2 && c.combinations[0].max_interfaces == 2 && c.combinations[0].channels == 1);
        assert_eq!(c.combinations[0].limits.len(), 2);
        assert!(
            c.combinations[0].limits[1].0 == 1 << AP | 1 << P2PC | 1 << P2PGO && c.combinations[1].limits[0].1 == 1
        );
        assert!(c.action_tx == 1 << STA && c.action_rx == 1 << STA);
        assert!(
            c.offchannel_tx
                && c.max_roc_ms == 5000
                && c.cmd_frame
                && c.cmd_remain_on_channel
                && c.cmd_vendor
                && !c.cmd_start_nan
        );
        assert!(c.commands.len() == 3 && c.commands[1] == VendorCmd { oui: 0x001374, subcmd: 81 });
        // Station and AP together (two limits); station and monitor never (only in the 1-interface combination); two
        // stations never (one limit of 1).
        assert!(c.combines(STA, AP) && c.combines(AP, STA));
        assert!(!c.combines(STA, MON) && !c.combines(STA, STA));
        // A later message of the split dump: vendor events, and the same command again (kept once).
        let mut b = Nla::default();
        let at = b.open(198);
        b.put(1, &pair((0x001374, 33)));
        b.end(at);
        let at = b.open(197);
        b.put(1, &pair(vendor[0]));
        b.end(at);
        c.wiphy(&b.b);
        assert!(c.events.len() == 1 && c.events[0].subcmd == 33 && c.commands.len() == 3);
        // Two GET_INTERFACE messages: wlan0 (a station on 5180 MHz) and a P2P device with no network device.
        let mut b = Nla::default();
        b.u32(3, 44);
        b.put(4, b"wlan0\0");
        b.u32(1, 0);
        b.u32(5, STA);
        b.put(6, &MAC);
        b.u32(38, 5180);
        c.iface(&b.b);
        let mut b = Nla::default();
        b.u32(1, 0);
        b.u32(5, P2PDEV);
        c.iface(&b.b);
        assert!(
            c.ifaces.len() == 2
                && c.ifaces[0].ifindex == 44
                && c.ifaces[0].name == "wlan0"
                && c.ifaces[0].freq_mhz == 5180
        );
        assert!(c.ifaces[1].iftype == P2PDEV && c.ifaces[1].name.is_empty() && iftype_name(P2PDEV) == "p2p-device");
        // The report: the facts, then the verdicts.
        let text = c.report();
        assert!(text.contains("radio: phy0 (wiphy 0)\n"));
        assert!(text.contains(
            "combination 1: up to 2 interfaces on 1 channel(s): 1 of {station}; 1 of {access point, p2p-client, p2p-go}\n"
        ));
        assert!(text.contains("action frames: sent by station; received by station\n"));
        assert!(text.contains("remain on channel: yes (up to 5000 ms)"));
        assert!(
            text.contains("  001374 Qualcomm: 12 81\n  0000f0 Samsung: 7\n")
                && text.contains("  events 001374 Qualcomm: 33\n")
        );
        assert!(text.contains("interface: wlan0 (index 44) station, wiphy 0, 02:11:22:33:44:55, 5180 MHz\n"));
        assert!(text.contains("interface: (no netdev) (index 0) p2p-device"));
        assert!(text.contains("p2p: monitor mode: yes (beside the station: no; active monitor, with ACKs: no;"));
        assert!(text.contains("on the station: send yes, receive yes, off channel too\n"));
        assert!(text.contains("p2p: Wi-Fi Aware (NAN) through nl80211: no") && text.contains("p2p: Wi-Fi Direct: yes"));
        assert!(text.contains("only the vendor's own AWDL can carry it") && !text.contains("Apple's OUI 00:17:f2 [I"));
        // (C's short-buffer case: the report is a String here, never cut.)
        // Cut and broken input reads what came before the cut and nothing after it.
        let mut cut = Caps::default();
        let whole = Nla { b: b.b.clone() };
        cut.wiphy(&whole.b[..3]);
        let bad = [2u8, 0, 1, 0, 0, 0, 0, 0]; // an attribute shorter than its header
        cut.wiphy(&bad);
        cut.iface(&bad);
        assert!(!cut.have_wiphy && cut.ifaces.is_empty());
        // A monitor beside the station, NAN, and a command under Apple's OUI change the verdicts.
        c.combinations[1].max_interfaces = 2;
        c.combinations[1].limits[0].1 = 2;
        c.iftypes |= 1 << NANIF;
        c.cmd_start_nan = true;
        c.commands.push(VendorCmd { oui: 0x0017f2, subcmd: 1 });
        let text = c.report();
        assert!(text.contains("beside the station: yes") && text.contains("(NAN) through nl80211: yes"));
        assert!(
            text.contains("Apple's OUI 00:17:f2 [I")
                && text.contains("a monitor interface beside the station is offered")
        );
    }

    // One message of a split GET_WIPHY dump: its wiphy and name first (as cfg80211 writes every message).
    pub fn wiphy_msg(wiphy: u32, name: &str) -> Nla {
        let mut b = Nla::default();
        b.u32(1, wiphy);
        b.put(2, format!("{name}\0").as_bytes());
        b
    }
    pub fn vendor_cmds(b: &mut Nla, oui: u32, subcmds: &[u32]) {
        let at = b.open(197);
        for (i, s) in subcmds.iter().enumerate() {
            b.put(i as u16 + 1, &[oui.to_le_bytes(), s.to_le_bytes()].concat());
        }
        b.end(at);
    }
    pub fn iface_msg(r: &mut Radios, ifindex: u32, name: &str, wiphy: u32, kind: u32, freq: u32) {
        let mut b = Nla::default();
        b.u32(3, ifindex);
        b.put(4, format!("{name}\0").as_bytes());
        b.u32(1, wiphy);
        b.u32(5, kind);
        if freq != 0 {
            b.u32(38, freq);
        }
        r.iface(&b.b);
    }
    pub const T_ADHOC: u32 = 1;
    pub const T_STA: u32 = 2;
    pub const T_AP: u32 = 3;
    pub const T_MON: u32 = 6;
    pub const T_P2PC: u32 = 8;
    pub const T_P2PGO: u32 = 9;
    pub const T_NAN: u32 = 12;
    pub const WONDER15: [u32; 5] = [1, 2, 3, 4, 5];

    /// The phone as Samsung's source says it is: phy0 (qcacld: station, AP, monitor, P2P; Qualcomm's commands) over
    /// two messages, and Wonder 1.5 (wiphy 1, "wonder": ad-hoc, station, monitor, NAN; Google's 1 to 5) between them.
    pub fn phone_radios() -> Radios {
        let mut r = Radios::default();
        let mut b = wiphy_msg(0, "phy0");
        iftype_flags(&mut b, 32, 1 << T_STA | 1 << T_AP | 1 << T_MON | 1 << T_P2PC | 1 << T_P2PGO);
        r.wiphy(&b.b);
        let mut b = wiphy_msg(1, "wonder");
        iftype_flags(&mut b, 32, 1 << T_ADHOC | 1 << T_STA | 1 << T_MON | 1 << T_NAN);
        vendor_cmds(&mut b, OUI_GOOGLE, &WONDER15);
        r.wiphy(&b.b);
        let mut b = wiphy_msg(0, "phy0"); // phy0's second message, after Wonder's
        vendor_cmds(&mut b, 0x001374, &[12, 81]);
        r.wiphy(&b.b);
        iface_msg(&mut r, 44, "wlan0", 0, T_STA, 5180);
        r
    }

    // test_wifi.c test_radios
    #[test]
    fn every_radio_and_wonder() {
        let mut r = phone_radios();
        if std::env::var_os("ONEIOS_TEST_SHOW").is_some() {
            print!("---- wifictl p2p on the stand-in phone (Wonder 1.5 idle)\n{}", r.report(Some("wlan0")));
        }
        assert!(
            r.wiphys.len() == 2 && r.wiphys[0].wiphy == 0 && r.wiphys[1].wiphy == 1 && r.wiphys[1].name == "wonder"
        );
        assert!(
            r.wiphys[0].commands.len() == 2 && r.wiphys[0].commands[1].subcmd == 81 && r.wiphys[1].commands.len() == 5
        ); // each message to its wiphy
        assert!(r.wiphys[0].iftypes >> T_AP & 1 != 0 && r.wiphys[1].iftypes >> T_AP & 1 == 0);
        let w = r.wonder().unwrap();
        assert!(std::ptr::eq(w, &r.wiphys[1]) && w.has_vendor(OUI_GOOGLE, 5) && !w.has_vendor(OUI_GOOGLE, 6));
        assert!(wonder_subcmd_name(4) == "SET_REGULATORY" && wonder_subcmd_name(15) == "SET_TX_RATE_TEST");
        assert!(wonder_subcmd_name(12) == "?" && wonder_subcmd_name(99) == "?");
        let text = r.report(Some("wlan0"));
        assert!(text.contains("radios: 2\nradio: phy0 (wiphy 0)\n") && text.contains("radio: wonder (wiphy 1)\n"));
        assert!(text.contains("  001374 Qualcomm: 12 81\n  (wlan0's radio)\n"));
        assert!(text.contains("  001a11 Google: 1 2 3 4 5\n  (Google's Wonder radio)\n"));
        assert!(text.contains("interface: wlan0 (index 44) station, wiphy 0, 00:00:00:00:00:00, 5180 MHz\n"));
        assert!(text.contains("p2p: Wonder: wiphy 1 (wonder), interface types ad-hoc, station, monitor, nan\n"));
        assert!(text.contains(
            "p2p: Wonder: vendor subcommands under 00:1a:11: 1 SET_FREQUENCY 2 SET_FILTER 3 SET_FIXED_TX_RATE 4 SET_REGULATORY 5 GET_IF_MAC_ADDR\n"
        ));
        assert!(text.contains("p2p: Wonder: version 1.4 or 1.5 (subcommands 1 to 5 only"));
        assert!(text.contains("p2p: Wonder: idle (no interface on it, no wondertap vdev)\n"));
        assert!(text.contains("p2p: for AirDrop iOS needs AWDL: Wonder's raw 802.11 path is offered (wiphy 1)"));
        assert!(text.contains("p2p: monitor mode: yes (beside the station: no;")); // the verdicts are phy0's (wlan0's), not Wonder's
                                                                                   // Wonder in use: a monitor of ours left over, and the PASSTHRU vdev on the driver's own wiphy.
        iface_msg(&mut r, 52, "wondermon0", 1, T_MON, 0);
        iface_msg(&mut r, 51, "wondertap0", 0, T_MON, 2437);
        let text = r.report(Some("wlan0"));
        assert!(
            text.contains("interface: wondermon0 (index 52) monitor, wiphy 1, 00:00:00:00:00:00 (on Wonder's radio)\n")
        );
        assert!(text.contains(
            "interface: wondertap0 (index 51) monitor, wiphy 0, 00:00:00:00:00:00, 2437 MHz (the Wi-Fi driver's PASSTHRU vdev for Wonder)\n"
        ));
        assert!(text.contains("p2p: Wonder: in use: wondermon0 (monitor): wifictl wonder's own"));
        assert!(
            text.contains("p2p: Wonder: the PASSTHRU vdev wondertap0 (index 51, wiphy 0) is up on 2437 MHz\n")
                && !text.contains("idle")
        );
        // Wonder 1.6 or later: a channel schedule.
        r.wiphys[1].commands.push(VendorCmd { oui: OUI_GOOGLE, subcmd: 6 });
        let text = r.report(Some("wlan0"));
        assert!(text.contains("6 SET_CHANNEL_SCHEDULE_REQ\n") && text.contains("p2p: Wonder: version 1.6 or later"));
        // No GET_IF_MAC_ADDR: a Wonder the session does not know.
        r.wiphys[1].commands.truncate(3);
        assert!(r.report(Some("wlan0")).contains("p2p: Wonder: version unknown"));

        // By its commands, not its name: a wiphy "phy1" with Google's commands and monitor mode and no AP is Wonder;
        // one with an AP (a Broadcom driver's own wiphy has Google's commands too) is not.
        let mut r = Radios::default();
        let mut b = wiphy_msg(0, "phy0");
        iftype_flags(&mut b, 32, 1 << T_STA | 1 << T_AP | 1 << T_MON);
        vendor_cmds(&mut b, OUI_GOOGLE, &WONDER15[..2]);
        r.wiphy(&b.b);
        assert!(r.wonder().is_none());
        let mut b = wiphy_msg(3, "phy1");
        iftype_flags(&mut b, 32, 1 << T_MON | 1 << T_STA);
        vendor_cmds(&mut b, OUI_GOOGLE, &WONDER15);
        r.wiphy(&b.b);
        assert!(r.wonder().is_some_and(|w| std::ptr::eq(w, &r.wiphys[1])) && r.wiphys[1].wiphy == 3);
        // No Wonder at all, and the station's radio without a monitor beside it: the old verdict, Wonder named.
        let mut r = Radios::default();
        let mut b = wiphy_msg(0, "phy0");
        iftype_flags(&mut b, 32, 1 << T_STA | 1 << T_AP);
        r.wiphy(&b.b);
        let text = r.report(Some("wlan0"));
        assert!(text.contains("p2p: Wonder: no Wonder radio ("));
        assert!(
            text.contains("no Wonder radio and no monitor beside the station: only the vendor's own AWDL can carry it")
        );
        iface_msg(&mut r, 51, "wondertap0", 0, T_MON, 0);
        assert!(r
            .report(Some("wlan0"))
            .contains("but a wondertap interface is up: its Wonder radio is not in the dump"));
        // A message that names no wiphy before any did is dropped; more wiphys than kept are counted; no radio at all.
        let mut r = Radios::default();
        let mut b = Nla::default();
        iftype_flags(&mut b, 32, 1 << T_STA);
        r.wiphy(&b.b);
        assert!(r.wiphys.is_empty());
        for i in 0..WIPHYS as u32 + 2 {
            r.wiphy(&wiphy_msg(i, "phy").b);
        }
        assert!(r.wiphys.len() == WIPHYS && r.wiphys_dropped == 2);
        assert!(r.report(None).contains("radios: 4 (more were not kept)\n"));
        assert_eq!(Radios::default().report(Some("wlan0")), "radio: none (nl80211 gave no wiphy)\n");
    }
}
