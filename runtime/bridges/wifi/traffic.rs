// SPDX-License-Identifier: MIT
//! The Wi-Fi bridge's traffic counts and its `wifi: 60 s:` line (docs/hardware.md section 3, "The traffic line"):
//! frames and bytes each way, what was dropped where and why, the waits for wlan0's send buffer, the scans while
//! joined. One line a minute while anything moved, none while idle. A lasting line (2026-11-23): the session of 18:24
//! that day joined, got its DHCP lease, and then said nothing at all about the traffic that did not load web pages.

use std::sync::atomic::{AtomicI32, AtomicU64, Ordering};

/// The counters (the frames thread, the guest's sends and the control thread add to them).
#[derive(Default)]
pub struct Traffic {
    pub rx_frames: AtomicU64,    // frames given to the guest (a cut frame counts once per segment)
    pub rx_bytes: AtomicU64,     // their bytes
    pub rx_cut: AtomicU64,       // coalesced frames cut into segments (frames::segment)
    pub rx_too_long: AtomicU64,  // longer than the link and not TCP: dropped
    pub rx_ring_full: AtomicU64, // dropped: the guest's receive ring stayed full (devices::virtio_net's wait)
    pub rx_errors: AtomicU64,    // recv on the AF_PACKET socket failed (not EAGAIN or EINTR)
    pub rx_last_errno: AtomicI32,
    pub tx_frames: AtomicU64, // frames of iOS's sent on wlan0
    pub tx_bytes: AtomicU64,
    pub tx_refused: AtomicU64, // not a frame the radio may send (another source address, EAPOL, too long)
    pub tx_failed: AtomicU64,  // send on the AF_PACKET socket failed (ENOBUFS: wlan0's queue was full; not EAGAIN)
    pub tx_last_errno: AtomicI32,
    pub tx_waited: AtomicU64, // found the socket's send buffer full (EAGAIN) and waited (wifi::uplink)
    pub tx_wait_sent: AtomicU64, // of those, sent after the wait
    pub tx_wait_dropped: AtomicU64, // of those, dropped: the buffer stayed full for uplink::WAIT_MS
    pub tx_held_full: AtomicU64, // dropped at once: uplink::HELD_MAX frames waited already
    pub tx_wait_longest_us: AtomicU64, // the longest wait that ended in a send (taken by each line)
    pub joined_cached: AtomicU64, // scans while joined answered from the last scan's networks (the station's count)
    pub joined_scans: AtomicU64, // scans iOS asked while joined
    pub joined_scan_flags: AtomicU64, // the scan flags the driver took for them (NL80211_ATTR_SCAN_FLAGS)
}

/// One reading of [`Traffic`].
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Counts {
    pub rx_frames: u64,
    pub rx_bytes: u64,
    pub rx_cut: u64,
    pub rx_too_long: u64,
    pub rx_ring_full: u64,
    pub rx_errors: u64,
    pub rx_last_errno: i32,
    pub tx_frames: u64,
    pub tx_bytes: u64,
    pub tx_refused: u64,
    pub tx_failed: u64,
    pub tx_last_errno: i32,
    pub tx_waited: u64,
    pub tx_wait_sent: u64,
    pub tx_wait_dropped: u64,
    pub tx_held_full: u64,
    pub joined_cached: u64,
    pub joined_scans: u64,
    pub joined_scan_flags: u64,
}

impl Traffic {
    pub fn read(&self) -> Counts {
        let l = |a: &AtomicU64| a.load(Ordering::Relaxed);
        Counts {
            rx_frames: l(&self.rx_frames),
            rx_bytes: l(&self.rx_bytes),
            rx_cut: l(&self.rx_cut),
            rx_too_long: l(&self.rx_too_long),
            rx_ring_full: l(&self.rx_ring_full),
            rx_errors: l(&self.rx_errors),
            rx_last_errno: self.rx_last_errno.load(Ordering::Relaxed),
            tx_frames: l(&self.tx_frames),
            tx_bytes: l(&self.tx_bytes),
            tx_refused: l(&self.tx_refused),
            tx_failed: l(&self.tx_failed),
            tx_last_errno: self.tx_last_errno.load(Ordering::Relaxed),
            tx_waited: l(&self.tx_waited),
            tx_wait_sent: l(&self.tx_wait_sent),
            tx_wait_dropped: l(&self.tx_wait_dropped),
            tx_held_full: l(&self.tx_held_full),
            joined_cached: l(&self.joined_cached),
            joined_scans: l(&self.joined_scans),
            joined_scan_flags: l(&self.joined_scan_flags),
        }
    }

    pub fn add(a: &AtomicU64, n: u64) {
        a.fetch_add(n, Ordering::Relaxed);
    }
}

fn size(bytes: u64) -> String {
    if bytes >= 1 << 20 {
        format!("{:.1} MiB", bytes as f64 / f64::from(1u32 << 20))
    } else {
        format!("{:.1} KiB", bytes as f64 / 1024.0)
    }
}

/// The line for `span_s` seconds from `before` to `now`; None when nothing moved. `ring`: the guest's receive ring
/// in that time (devices::virtio_net's counts: frames that waited for buffers, the longest wait that got one, in
/// microseconds). `sent_longest_us`: the longest wait for the send buffer that ended in a send (wifi::uplink).
pub fn line(span_s: u64, before: &Counts, now: &Counts, ring: (u64, u64), sent_longest_us: u64) -> Option<String> {
    let d = |a: u64, b: u64| b.wrapping_sub(a);
    let (rx, tx) = (d(before.rx_frames, now.rx_frames), d(before.tx_frames, now.tx_frames));
    let drops = [
        (d(before.rx_ring_full, now.rx_ring_full), "the guest's ring full"),
        (d(before.rx_too_long, now.rx_too_long), "too long"),
        (d(before.rx_errors, now.rx_errors), "receive errors"),
    ];
    let wait_dropped = format!("dropped after a {} ms wait", super::uplink::WAIT_MS);
    let held_full = format!("dropped with {} waiting", super::uplink::HELD_MAX);
    let tx_drops = [
        (d(before.tx_refused, now.tx_refused), "refused"),
        (d(before.tx_failed, now.tx_failed), "send failed"),
        (d(before.tx_wait_dropped, now.tx_wait_dropped), wait_dropped.as_str()),
        (d(before.tx_held_full, now.tx_held_full), held_full.as_str()),
    ];
    let (waited, wait_sent) = (d(before.tx_waited, now.tx_waited), d(before.tx_wait_sent, now.tx_wait_sent));
    let (scans, cached) = (d(before.joined_scans, now.joined_scans), d(before.joined_cached, now.joined_cached));
    if rx == 0
        && tx == 0
        && drops.iter().chain(&tx_drops).all(|(n, _)| *n == 0)
        && waited == 0
        && scans == 0
        && cached == 0
    {
        return None;
    }
    // each item with its count; the errno right after the failures it belongs to
    let list = |items: &[(u64, &str)], errno: i32, errno_of: &str| {
        let mut parts: Vec<String> = Vec::new();
        for (n, what) in items.iter().filter(|(n, _)| *n > 0) {
            parts.push(format!("{n} {what}"));
            if errno != 0 && *what == errno_of {
                parts.push(format!("last errno {errno}"));
            }
        }
        if parts.is_empty() {
            "none".to_owned()
        } else {
            parts.join(", ")
        }
    };
    let mut out = format!(
        "wifi: {span_s} s: to iOS {rx} frames ({}, {} cut from coalesced), from iOS {tx} frames ({}); dropped to iOS: {}; \
         from iOS: {}; ring full and waited {} (longest wait that got a buffer {} us); send buffer full and waited \
         {waited} ({wait_sent} sent, longest wait that sent {sent_longest_us} us)",
        size(d(before.rx_bytes, now.rx_bytes)),
        d(before.rx_cut, now.rx_cut),
        size(d(before.tx_bytes, now.tx_bytes)),
        list(&drops, now.rx_last_errno, "receive errors"),
        list(&tx_drops, now.tx_last_errno, "send failed"),
        ring.0,
        ring.1
    );
    if scans + cached > 0 {
        out.push_str(&format!(
            "; {} scans while joined ({cached} answered from the last scan's networks, {scans} on the radio with scan \
             flags 0x{:x})",
            scans + cached,
            now.joined_scan_flags
        ));
    }
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_line() {
        let t = Traffic::default();
        let zero = t.read();
        assert_eq!(line(60, &zero, &t.read(), (0, 0), 0), None); // idle: no line
        Traffic::add(&t.rx_frames, 1200);
        Traffic::add(&t.rx_bytes, 1_600_000);
        Traffic::add(&t.rx_cut, 3);
        Traffic::add(&t.tx_frames, 400);
        Traffic::add(&t.tx_bytes, 40_000);
        Traffic::add(&t.rx_ring_full, 2);
        let now = t.read();
        assert_eq!(
            line(60, &zero, &now, (9, 1500), 0).unwrap(),
            "wifi: 60 s: to iOS 1200 frames (1.5 MiB, 3 cut from coalesced), from iOS 400 frames (39.1 KiB); dropped to \
             iOS: 2 the guest's ring full; from iOS: none; ring full and waited 9 (longest wait that got a buffer 1500 us); \
             send buffer full and waited 0 (0 sent, longest wait that sent 0 us)"
        );
        // only what changed since the last line; errnos with the failures they belong to; the waits for the send
        // buffer; the scans while joined
        Traffic::add(&t.tx_failed, 5);
        t.tx_last_errno.store(105, Ordering::Relaxed);
        Traffic::add(&t.rx_errors, 1);
        t.rx_last_errno.store(22, Ordering::Relaxed);
        Traffic::add(&t.tx_waited, 12);
        Traffic::add(&t.tx_wait_sent, 9);
        Traffic::add(&t.tx_wait_dropped, 2);
        Traffic::add(&t.tx_held_full, 1);
        Traffic::add(&t.joined_scans, 1);
        Traffic::add(&t.joined_cached, 3);
        t.joined_scan_flags.store(1, Ordering::Relaxed);
        let l = line(60, &now, &t.read(), (0, 0), 850).unwrap();
        assert!(
            l.starts_with("wifi: 60 s: to iOS 0 frames (0.0 KiB, 0 cut from coalesced), from iOS 0 frames (0.0 KiB); "),
            "{l}"
        );
        assert!(
            l.contains(
                "dropped to iOS: 1 receive errors, last errno 22; from iOS: 5 send failed, last errno 105, 2 dropped \
                 after a 20 ms wait, 1 dropped with 256 waiting;"
            ),
            "{l}"
        );
        assert!(l.contains("; send buffer full and waited 12 (9 sent, longest wait that sent 850 us); "), "{l}");
        assert!(
            l.ends_with(
                "; 4 scans while joined (3 answered from the last scan's networks, 1 on the radio with scan flags 0x1)"
            ),
            "{l}"
        );
        // an errno is named only beside a failure of its kind in this span
        let later = t.read();
        Traffic::add(&t.tx_refused, 1);
        let l = line(60, &later, &t.read(), (0, 0), 0).unwrap();
        assert!(l.contains("from iOS: 1 refused;") && !l.contains("errno"), "{l}");
        // a minute of waits alone is a line
        let later = t.read();
        Traffic::add(&t.tx_waited, 1);
        assert!(line(60, &later, &t.read(), (0, 0), 0).is_some());
    }
}
