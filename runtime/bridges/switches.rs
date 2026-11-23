// SPDX-License-Identifier: MIT
//! The bridges' switches (docs/switches.md): one value each, set once by xnuhost from its config before the bridges
//! start (`wifi-ring-wait=`, `wifi-send-wait=`, `wifi-buffers=`, `wifi-joined-scan=` of the image's xnuhost.conf), read
//! with one relaxed load where the behaviour is chosen. The defaults are the behaviour of main; "off" (or `plain`) is
//! the behaviour before the change, so that one boot can test every change and a fault can be pinned by turning one
//! off. The host tests and the tools that never set them get the defaults.
use std::sync::atomic::{AtomicBool, AtomicU8, Ordering};

/// What a scan while joined does (`wifi-joined-scan=`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum JoinedScan {
    /// cache (b704922): within 120 s of the last good scan, its networks answer; else a low-priority scan.
    Cache = 0,
    /// low (cbf97ea): every scan goes to the radio, low priority (and low span) where the driver takes it.
    Low = 1,
    /// plain: every scan goes to the radio with no flags (before cbf97ea).
    Plain = 2,
}

impl JoinedScan {
    pub fn parse(v: &str) -> Option<JoinedScan> {
        match v {
            "cache" => Some(JoinedScan::Cache),
            "low" => Some(JoinedScan::Low),
            "plain" => Some(JoinedScan::Plain),
            _ => None,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            JoinedScan::Cache => "cache",
            JoinedScan::Low => "low",
            JoinedScan::Plain => "plain",
        }
    }

    fn from_u8(v: u8) -> JoinedScan {
        match v {
            1 => JoinedScan::Low,
            2 => JoinedScan::Plain,
            _ => JoinedScan::Cache,
        }
    }
}

/// The Wi-Fi bridge's switches, as xnuhost's config gives them.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Wifi {
    /// wifi-ring-wait (cbf97ea): a frame for the guest that finds its receive ring full waits for the guest's kick
    /// (20 ms data, 50 ms the station's own); off: it is dropped at once.
    pub ring_wait: bool,
    /// wifi-send-wait (b704922): a frame of the guest's that finds wlan0's send buffer full (EAGAIN) waits in the
    /// uplink list (20 ms at most); off: it fails at once (counted as "send failed, errno 11").
    pub send_wait: bool,
    /// wifi-buffers (cbf97ea, b704922): wlan0's AF_PACKET socket gets a 4 MiB receive and a 1 MiB send buffer;
    /// off: the kernel's defaults (rmem_default, wmem_default).
    pub buffers: bool,
    /// wifi-joined-scan (b704922, cbf97ea).
    pub joined_scan: JoinedScan,
}

impl Default for Wifi {
    fn default() -> Wifi {
        Wifi { ring_wait: true, send_wait: true, buffers: true, joined_scan: JoinedScan::Cache }
    }
}

static RING_WAIT: AtomicBool = AtomicBool::new(true);
static SEND_WAIT: AtomicBool = AtomicBool::new(true);
static BUFFERS: AtomicBool = AtomicBool::new(true);
static JOINED_SCAN: AtomicU8 = AtomicU8::new(JoinedScan::Cache as u8);

/// xnuhost, once, before the bridges start.
pub fn set_wifi(w: Wifi) {
    RING_WAIT.store(w.ring_wait, Ordering::Relaxed);
    SEND_WAIT.store(w.send_wait, Ordering::Relaxed);
    BUFFERS.store(w.buffers, Ordering::Relaxed);
    JOINED_SCAN.store(w.joined_scan as u8, Ordering::Relaxed);
}

/// The Wi-Fi switches in force.
pub fn wifi() -> Wifi {
    Wifi {
        ring_wait: RING_WAIT.load(Ordering::Relaxed),
        send_wait: SEND_WAIT.load(Ordering::Relaxed),
        buffers: BUFFERS.load(Ordering::Relaxed),
        joined_scan: JoinedScan::from_u8(JOINED_SCAN.load(Ordering::Relaxed)),
    }
}

impl Wifi {
    /// The start line's words for the switches that are not at their default ("" when all are).
    pub fn changed(&self) -> String {
        let d = Wifi::default();
        let mut words = Vec::new();
        if self.ring_wait != d.ring_wait {
            words.push("wifi-ring-wait=0".to_string());
        }
        if self.send_wait != d.send_wait {
            words.push("wifi-send-wait=0".to_string());
        }
        if self.buffers != d.buffers {
            words.push("wifi-buffers=0".to_string());
        }
        if self.joined_scan != d.joined_scan {
            words.push(format!("wifi-joined-scan={}", self.joined_scan.as_str()));
        }
        words.join(" ")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn words_and_defaults() {
        assert_eq!(Wifi::default().changed(), "");
        for w in ["cache", "low", "plain"] {
            assert_eq!(JoinedScan::parse(w).map(JoinedScan::as_str), Some(w));
            let j = JoinedScan::parse(w).unwrap();
            assert_eq!(JoinedScan::from_u8(j as u8), j);
        }
        assert_eq!(JoinedScan::parse("on"), None);
        let off = Wifi { ring_wait: false, send_wait: false, buffers: false, joined_scan: JoinedScan::Plain };
        assert_eq!(off.changed(), "wifi-ring-wait=0 wifi-send-wait=0 wifi-buffers=0 wifi-joined-scan=plain");
    }
}
