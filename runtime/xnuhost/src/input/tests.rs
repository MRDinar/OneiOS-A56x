// SPDX-License-Identifier: MIT
//! test_xnuhost.c's test_input_filter: which touchscreen is taken; one device's filter, what reaches the guest (through
//! the real virtio-input device and a guest driver), the scan time, SYN_DROPPED, the gesture's timing, the keys.
use std::cell::Cell;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::Arc;

use xh_core::mmio::MmioDevice;
use xh_core::virtio::testing::{self, Driver, GuestRam};

use super::*;
use crate::virtio_input::{self, InputDev};

/// A guest with the input device up and 8 event buffers posted.
pub(crate) struct Guest {
    pub ram: GuestRam,
    pub d: Driver,
    pub dev: InputDev,
}

impl Guest {
    pub fn new(spi: u32) -> Guest {
        testing::install();
        let ram = GuestRam::new(1 << 20);
        let dev = virtio_input::create(0, spi).unwrap();
        let mut d = Driver::up(dev.clone() as Arc<dyn MmioDevice>, 0, 2, 8, ram.pa(0x10000)).unwrap();
        for i in 0..8 {
            d.send(0, &[(ram.pa(0x50000 + i * 8), 8, true)], false);
        }
        d.kick(0);
        Guest { ram, d, dev }
    }

    /// The events the device wrote since the last call, in order; each buffer is posted again (8 in the queue).
    pub fn take(&mut self) -> Vec<(u16, u16, i32)> {
        let mut out = Vec::new();
        loop {
            let mut more = false;
            while let Some((id, _)) = self.d.reap(0) {
                let addr = self.d.desc_addr(0, id);
                let b = self.ram.read((addr - self.ram.pa(0)) as usize, 8);
                out.push((
                    u16::from_le_bytes([b[0], b[1]]),
                    u16::from_le_bytes([b[2], b[3]]),
                    i32::from_le_bytes([b[4], b[5], b[6], b[7]]),
                ));
                self.d.send(0, &[(addr, 8, true)], false);
                more = true;
            }
            self.d.kick(0); // the kick hands over what waited
            if !more {
                return out;
            }
        }
    }
}

thread_local! {
    static FAKE: Cell<State> = Cell::new(State::default());
}

fn fake_read_state<S>(_: &Source<S>) -> Option<State> {
    Some(FAKE.with(|f| f.get()))
}

static POWER_PRESSES: AtomicU32 = AtomicU32::new(0);
fn count_power_press() {
    POWER_PRESSES.fetch_add(1, Ordering::Relaxed);
}

const fn ev(t: i64, kind: u16, code: u16, value: i32) -> Event {
    Event::new(t, kind, code, value)
}

#[test]
fn which_touchscreen() {
    // S26 Ultra: both the internal sec_touchscreen and a connected HUAWEI Band advertise INPUT_PROP_DIRECT. Native input
    // wins even if event IDs change.
    assert!(accept_touch(Some("sec_touchscreen"), true, false, true));
    assert!(!accept_touch(Some("HUAWEI Band 11-F88"), true, false, true));
    assert!(!accept_touch(Some("sec_touchpad"), false, false, true));
    assert!(!accept_touch(Some("sec_touchscreen"), true, true, true));
    assert!(accept_touch(Some("generic touchscreen"), true, false, false));
    assert!(!accept_touch(Some("generic touchscreen"), false, false, false));
    // The bench SHM exclusive grab cannot silently select a wearable or pointer.
    assert!(exclusive_touch_allowed(Some("sec_touchscreen"), true, false));
    assert!(!exclusive_touch_allowed(Some("HUAWEI Band 11-F88"), true, false));
    assert!(!exclusive_touch_allowed(Some("sec_touchpad"), true, false));
    assert!(!exclusive_touch_allowed(Some("sec_touchscreen"), false, false));
    assert!(!exclusive_touch_allowed(Some("sec_touchscreen"), true, true));
    assert!(!exclusive_touch_allowed(None, true, false));
}

#[test]
fn the_grab_lines_and_one_release() {
    assert_eq!(
        grabbed_line(false),
        "input: touch-exclusive: sec_touchscreen grabbed for the bench preview; Android input hidden until release"
    );
    assert_eq!(
        grabbed_line(true),
        "input: touch-exclusive: sec_touchscreen grabbed (session-mode=takeover; the side keys are read, never grabbed); Android input hidden until release"
    );
    // The release runs once, whichever of stop() and the devices' drop comes first: a descriptor that is no evdev device
    // (the release fails and says so), then nothing more; a stop with no reader changes nothing.
    let (a, _b) = xh_core::fd::socket_pair(libc::SOCK_SEQPACKET).expect("a socket pair");
    EXCLUSIVE.store(a.as_raw_fd(), Ordering::Release);
    drop(Devices { sources: Vec::new(), pen: None });
    assert_eq!(EXCLUSIVE.load(Ordering::Acquire), -1);
    release_exclusive();
    stop();
    assert_eq!(EXCLUSIVE.load(Ordering::Acquire), -1);
}

#[test]
fn filter() {
    let mut g = Guest::new(45);
    let (hz, counter, now) = (19_200_000u64, 1_000_000_000u64, 100_000_000_000i64);
    let mut s = Source::new(g.dev.clone(), "touch", true);
    s.read_state = Some(fake_read_state);
    // A touch-down, read 2 ms after its scan: the frame and then its scan time, 38400 ticks before the counter now.
    let t = now - 2_000_000;
    let down = [
        ev(t, 3, 0x2f, 0),
        ev(t, 3, 0x39, 5),
        ev(t, 3, 0x35, 700),
        ev(t, 3, 0x36, 1500),
        ev(t, 1, 0x14a, 1),
        ev(t, 4, 5, 77),
        ev(t, 0, 0, 0),
    ];
    s.feed(&down, now, counter, hz);
    let got = g.take();
    assert_eq!(got.len(), 7);
    assert!(got[0].1 == 0x2f && got[1].1 == 0x39 && got[1].2 == 5 && got[2].2 == 700 && got[3].2 == 1500);
    assert_eq!(got[4], (1, 0x14a, 1)); // the device's own MSC is not passed on
    assert!(got[5].0 == 4 && got[5].1 == 5 && got[5].2 as u32 == (counter - 38400) as u32 && got[6] == (0, 0, 0));
    assert!(s.timing.frames == 1 && s.timing.began == 1 && s.sent_id[0] == 5 && s.timing.read_min_ns == 2_000_000);
    // A move 8 ms later: only what changed.
    s.feed(&[ev(now + 6_000_000, 3, 0x35, 710), ev(now + 6_000_000, 0, 0, 0)], now + 6_500_000, counter, hz);
    let got = g.take();
    assert!(got.len() == 3 && got[0].2 == 710 && got[1].0 == 4 && got[2].0 == 0);
    assert!(s.timing.frames == 2 && s.timing.gap_min_ns == 8_000_000);
    // Dropped events: the rest of that frame is ignored, the state is read again. Slot 0's finger lifted in the drop,
    // slot 1 holds a new one: a frame of the lift (the gesture ends there), then a frame with the new contact.
    let mut st = State::default();
    (st.id[1], st.x[1], st.y[1], st.slot) = (9, 100, 200, 1);
    FAKE.with(|f| f.set(st));
    s.feed(
        &[ev(now, 0, 3, 0), ev(now, 3, 0x35, 720), ev(now, 3, 0x39, -1), ev(now, 0, 0, 0)],
        now + 20_000_000,
        counter,
        hz,
    );
    let got = g.take();
    assert_eq!(got.len(), 5 + 8);
    assert!(
        got[0] == (3, 0x2f, 0) && got[1] == (3, 0x39, -1) && got[2] == (1, 0x14a, 0) && got[3].0 == 4 && got[4].0 == 0
    );
    assert!(got[5] == (3, 0x2f, 1) && got[6] == (3, 0x39, 9) && got[7].2 == 100 && got[8].2 == 200);
    assert!(got[9] == (3, 0x2f, 1) && got[10] == (1, 0x14a, 1) && got[11].0 == 4 && got[12].0 == 0);
    assert!(
        s.drops == 1
            && !s.dropping
            && s.sent_id[0] == -1
            && s.sent_id[1] == 9
            && s.timing.frames == 1
            && s.timing.began == 1
    );
    // The lift: the gesture is logged and the counts start again; a frame with nothing down is no gesture.
    let lift = [
        ev(now + 30_000_000, 3, 0x39, -1),
        ev(now + 30_000_000, 1, 0x14a, 0),
        ev(now + 30_000_000, 0, 0, 0),
        ev(now + 40_000_000, 1, 0x14a, 0),
        ev(now + 40_000_000, 0, 0, 0),
    ];
    s.feed(&lift, now + 41_000_000, counter, hz);
    let got = g.take();
    assert!(got.len() == 4 + 3 && got[0] == (3, 0x39, -1) && s.timing.frames == 0 && s.sent_id[1] == -1);
    // A touch-down on a still screen (no guest frame for 120 ms) is timed to the next frame shown.
    frame_shown();
    std::thread::sleep(std::time::Duration::from_millis(120));
    let real = monotonic_ns();
    s.feed(&[ev(real, 3, 0x39, 11), ev(real, 3, 0x35, 5), ev(real, 0, 0, 0)], real, counter, hz);
    assert!(s.timing.still && s.timing.frames == 1);
    frame_shown();
    s.feed(&[ev(real + 1, 3, 0x39, -1), ev(real + 1, 0, 0, 0)], real + 2, counter, hz);
    assert_eq!(s.timing.frames, 0);
    g.take();
    // A key device: the side keys pass, other keys and touch codes do not; after a drop the keys are read again.
    let mut k = Source::new(g.dev.clone(), "keys", false);
    k.read_state = Some(fake_read_state);
    POWER_PRESSES.store(0, Ordering::Relaxed);
    set_power_key(Some(count_power_press)); // the brightness bridge's touch wake
    k.feed(
        &[ev(now, 1, 116, 1), ev(now, 1, 30, 1), ev(now, 3, 0x35, 3), ev(now, 1, 172, 1), ev(now, 0, 0, 0)],
        now,
        counter,
        hz,
    );
    assert_eq!(POWER_PRESSES.load(Ordering::Relaxed), 1); // the press, not the release below
    let got = g.take();
    assert!(got.len() == 3 && got[0] == (1, 116, 1) && got[1].1 == 172 && got[2].0 == 0 && k.sent_keys == 4);
    FAKE.with(|f| f.set(State { keys: 2, ..State::default() })); // power up, volume up down: both changed
    k.feed(&[ev(now, 0, 3, 0), ev(now, 1, 116, 0), ev(now, 0, 0, 0)], now, counter, hz);
    let got = g.take();
    assert!(got.len() == 3 && got[0] == (1, 115, 1) && got[1] == (1, 116, 0) && got[2].0 == 0 && k.sent_keys == 2);
    assert_eq!(POWER_PRESSES.load(Ordering::Relaxed), 1); // a release (and a resync) is no press
    set_power_key(None);
}
