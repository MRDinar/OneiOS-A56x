// SPDX-License-Identifier: MIT
//! LE scan for a controller that refuses the extended scan commands, as iOSPlus10's laurel 9012 does in its kernel
//! (patches/laurel/xnu-12377/post/9012-laurel-bt-legacy-le-scan.patch): iOS 26/27's bluetoothd starts its LE scan
//! with LE Set Extended Scan Parameters (0x2041) and Enable (0x2042) whatever the controller's supported commands
//! say; on the Mi A3's WCN3950 the answer was status 1, OI status 701, "Could not start LE scan" and a new
//! bluetoothd every 10 s.
//!
//! Unlike laurel this is adaptive: nothing changes until the controller refuses 0x2041 (Unknown HCI Command 0x01 or
//! Unsupported Feature 0x11). Then that answer is kept from the guest, the same parameters go out as the legacy LE
//! Set Scan Parameters (0x200b) and its answer goes back under 0x2041; from then on 0x2042 goes out as LE Set Scan
//! Enable (0x200c), LE Advertising Report (bit 1 of the LE event mask) is turned on, and each legacy report goes up
//! as one LE Extended Advertising Report (subevent 0x0d). The S24 Ultra's WCN785x (Bluetooth 5.3) is expected to
//! accept the extended commands, so this should stay idle there [I]. Lost in legacy mode: coded PHY, scan duration.
//!
//! Packets are HCI packets without the H4 indicator; the caller holds a lock around each call. C: bt/le_scan.{c,h}.

/// Events to the guest per call at most.
pub const UP_MAX: usize = 8;
/// An event's largest size (code, length, 255 bytes).
pub const EVENT_MAX: usize = 258;

#[derive(Clone, Debug, Default)]
pub struct LeScan {
    /// The controller refused 0x2041: legacy commands from now on (kept over a close).
    pub legacy: bool,
    ext_used: bool,
    mask: Option<[u8; 8]>, // the guest's LE event mask
    last_params: Vec<u8>,  // the last 0x2041, resent as 0x200b when it is refused (at most 64 bytes)
    params_out: u32,       // legacy commands sent whose answers go up renamed (or not at all)
    enable_out: u32,
    mask_out: u32,
    /// How many times the scan went legacy.
    pub switched: u32,
}

/// What an event from the controller becomes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Verdict {
    Pass,
    Drop,
    Replace,
}

/// The result of one event: commands to send to the controller in every case, then the verdict, and with
/// [`Verdict::Replace`] the events the guest gets instead.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EventStep {
    pub to_controller: Vec<Vec<u8>>,
    pub verdict: Verdict,
    pub to_guest: Vec<Vec<u8>>,
}

impl EventStep {
    fn pass() -> EventStep {
        EventStep { to_controller: Vec::new(), verdict: Verdict::Pass, to_guest: Vec::new() }
    }
}

fn le16(p: &[u8]) -> u16 {
    u16::from(p[0]) | u16::from(p[1]) << 8
}

/// 0x2041 -> 0x200b: own, filter, phys, then per PHY type, interval, window; the first PHY's values go on.
fn legacy_params(c: &[u8]) -> Option<Vec<u8>> {
    if c.len() < 3 + 8 || c[2] < 8 || c[5] == 0 {
        return None;
    }
    // scan type, interval, window, own address type, filter policy
    Some(vec![0x0b, 0x20, 7, c[6], c[7], c[8], c[9], c[10], c[3], c[4]])
}

/// 0x2042 -> 0x200c: enable, filter duplicates (2, "reset each period", has no legacy value: 1).
fn legacy_enable(c: &[u8]) -> Option<Vec<u8>> {
    if c.len() < 3 + 6 || c[2] < 6 {
        return None;
    }
    Some(vec![0x0c, 0x20, 2, c[3], u8::from(c[4] != 0)])
}

/// Command Complete (0x0e: ncmd, opcode, status) or Command Status (0x0f: status, ncmd, opcode) for `op`: its
/// status.
fn answer_status(e: &[u8], op: u16) -> Option<u8> {
    let n = e.len();
    if n >= 6 && e[0] == 0x0e && e[1] as usize + 2 == n && le16(&e[3..]) == op {
        return Some(e[5]);
    }
    if n >= 6 && e[0] == 0x0f && e[1] as usize + 2 == n && le16(&e[4..]) == op {
        return Some(e[2]);
    }
    None
}

fn renamed(e: &[u8], op: u16) -> Vec<u8> {
    let mut up = e.to_vec();
    let at = if up[0] == 0x0e { 3 } else { 4 };
    up[at..at + 2].copy_from_slice(&op.to_le_bytes());
    up
}

impl LeScan {
    /// Forgets the scan in flight; what the controller refused stays known.
    pub fn reset(&mut self) {
        *self = LeScan { legacy: self.legacy, ..LeScan::default() };
    }

    /// A command from the guest. Some: send these instead of it; None: send it as it is.
    pub fn command(&mut self, c: &[u8]) -> Option<Vec<Vec<u8>>> {
        if c.len() < 3 {
            return None;
        }
        match le16(c) {
            0x2001 if c.len() == 11 => {
                // LE Set Event Mask: remembered; in legacy mode with reports on
                self.mask = Some(c[3..11].try_into().expect("8 bytes"));
                if self.legacy && c[3] & 0x02 == 0 {
                    let mut fixed = c.to_vec();
                    fixed[3] |= 0x02;
                    return Some(vec![fixed]);
                }
                None
            }
            0x2041 => {
                self.ext_used = true;
                if c.len() <= 64 {
                    self.last_params = c.to_vec();
                }
                if !self.legacy {
                    return None;
                }
                let legacy = legacy_params(c)?;
                self.params_out += 1;
                Some(vec![legacy])
            }
            0x2042 if self.legacy => {
                let legacy = legacy_enable(c)?;
                self.enable_out += 1;
                Some(vec![legacy])
            }
            _ => None,
        }
    }

    /// An event from the controller.
    pub fn event(&mut self, e: &[u8]) -> EventStep {
        let n = e.len();
        if !(2..=EVENT_MAX).contains(&n) {
            return EventStep::pass();
        }
        if let Some(status) = answer_status(e, 0x2041) {
            if !self.legacy && (status == 0x01 || status == 0x11) && !self.last_params.is_empty() {
                let Some(legacy) = legacy_params(&self.last_params) else { return EventStep::pass() };
                self.legacy = true;
                self.switched += 1;
                let mut to_controller = Vec::new();
                if let Some(mask) = self.mask.filter(|m| m[0] & 0x02 == 0) {
                    // LE Advertising Report on; its answer is not the guest's
                    let mut command = vec![0x01, 0x20, 8];
                    command.extend_from_slice(&mask);
                    command[3] |= 0x02;
                    to_controller.push(command);
                    self.mask_out += 1;
                }
                to_controller.push(legacy);
                self.params_out += 1;
                return EventStep { to_controller, verdict: Verdict::Drop, to_guest: Vec::new() };
            }
        }
        let replace =
            |up: Vec<u8>| EventStep { to_controller: Vec::new(), verdict: Verdict::Replace, to_guest: vec![up] };
        if self.mask_out > 0 && answer_status(e, 0x2001).is_some() {
            self.mask_out -= 1;
            return EventStep { verdict: Verdict::Drop, ..EventStep::pass() };
        }
        if self.params_out > 0 && answer_status(e, 0x200b).is_some() {
            self.params_out -= 1;
            return replace(renamed(e, 0x2041));
        }
        if self.enable_out > 0 && answer_status(e, 0x200c).is_some() {
            self.enable_out -= 1;
            return replace(renamed(e, 0x2042));
        }
        // LE Advertising Report (0x3e, subevent 0x02): each report as one LE Extended Advertising Report.
        if self.legacy && self.ext_used && n >= 3 && e[0] == 0x3e && e[1] as usize + 2 == n && e[2] == 0x02 {
            const PROPS: [u8; 5] = [0x13, 0x15, 0x12, 0x10, 0x1b]; // Core 7.7.65.13, the legacy PDU types
            let mut up = Vec::new();
            let mut p = 4;
            let count = if n >= 4 { e[3] } else { 0 };
            for _ in 0..count {
                if up.len() == UP_MAX || p + 9 > n {
                    break;
                }
                let len = e[p + 8] as usize;
                if p + 9 + len + 1 > n || e[p] > 4 || len > 31 {
                    break;
                }
                let mut o = Vec::with_capacity(2 + 24 + len);
                o.extend_from_slice(&[0x3e, (2 + 24 + len) as u8, 0x0d, 1]); // LE Extended Advertising Report, 1
                o.extend_from_slice(&[PROPS[e[p] as usize], 0]); // event type
                o.push(e[p + 1]); // address type
                o.extend_from_slice(&e[p + 2..p + 8]);
                o.extend_from_slice(&[1, 0, 0xff, 0x7f]); // primary PHY LE 1M, no secondary PHY, no SID, TX power not known
                o.push(e[p + 9 + len]); // RSSI
                o.extend_from_slice(&[0, 0]); // no periodic advertising
                o.extend_from_slice(&[0; 7]); // no direct address
                o.push(len as u8);
                o.extend_from_slice(&e[p + 9..p + 9 + len]);
                up.push(o);
                p += 9 + len + 1;
            }
            if up.is_empty() {
                return EventStep::pass();
            }
            return EventStep { to_controller: Vec::new(), verdict: Verdict::Replace, to_guest: up };
        }
        EventStep::pass()
    }
}
