// SPDX-License-Identifier: MIT
//! test_xnuhost.c's test_pen: which devices are taken, the evdev state as the guest's sample, the frames the guest's
//! device gets, the coalescing, the guest's pen device's configuration, pen-rotate=auto and the script's pen lines.
use std::cell::{Cell, RefCell};

use xh_core::virtio::testing;

use super::*;
use crate::input::Event;
use crate::input_script::{self, Step};

/// The events a pen filter sent, in order.
#[derive(Default)]
struct Capture(RefCell<Vec<(u16, u16, i32)>>);

impl EventSink for Capture {
    fn event(&self, kind: u16, code: u16, value: i32) {
        self.0.borrow_mut().push((kind, code, value));
    }
}

impl Capture {
    fn take(&self) -> Vec<(u16, u16, i32)> {
        std::mem::take(&mut self.0.borrow_mut())
    }
}

fn find(got: &[(u16, u16, i32)], kind: u16, code: u16) -> Option<(usize, i32)> {
    got.iter().position(|e| e.0 == kind && e.1 == code).map(|i| (i, got[i].2))
}

fn value(got: &[(u16, u16, i32)], kind: u16, code: u16) -> Option<i32> {
    find(got, kind, code).map(|(_, v)| v)
}

fn syns(got: &[(u16, u16, i32)]) -> usize {
    got.iter().filter(|e| e.0 == 0).count()
}

const fn ev(t: i64, kind: u16, code: u16, value: i32) -> Event {
    Event::new(t, kind, code, value)
}

thread_local! {
    static FAKE: Cell<State> = Cell::new(State::default());
}

fn fake_read<S>(_: &Source<S>) -> Option<State> {
    Some(FAKE.with(|f| f.get()))
}

fn setbit(a: &mut [u8], n: u16) {
    a[n as usize / 8] |= 1 << (n % 8);
}

fn device(axes: [(i32, i32, i32); AXES]) -> Device {
    let mut d = Device { stylus: true, ..Device::default() };
    for (a, (min, max, res)) in d.axis.iter_mut().zip(axes) {
        *a = Axis::new(min, max, res);
    }
    d
}

#[test]
fn rank_of_a_device() {
    // The lookup's test: a pen's bits; never a device with a side key or multitouch positions.
    let (mut keys, mut abs) = ([0u8; 96], [0u8; 8]);
    assert!(rank("sec_e-pen", &keys, &abs).is_err_and(|w| w.contains("BTN_TOOL_PEN")));
    setbit(&mut keys, 0x140);
    setbit(&mut abs, 0);
    setbit(&mut abs, 1);
    assert!(rank("sec_e-pen", &keys, &abs).is_err_and(|w| w.contains("ABS_PRESSURE")));
    setbit(&mut abs, 0x18);
    let plain = rank("digitizer", &keys, &abs).unwrap();
    assert_eq!(plain, 1);
    setbit(&mut abs, 0x1a);
    setbit(&mut keys, 0x14b);
    let full = rank("sec_e-pen", &keys, &abs).unwrap();
    assert!(full > plain && full == 1 + 2 + 4); // tilt, the side button, a pen's name
    let mut mt = abs;
    setbit(&mut mt, 0x35);
    assert!(rank("sec_e-pen", &keys, &mt).is_err_and(|w| w.contains("multitouch")));
    setbit(&mut keys, 116); // KEY_POWER: never the pen's device
    assert!(rank("sec_e-pen", &keys, &abs).is_err_and(|w| w.contains("side keys")));
    assert!(lookup_is_default()); // the default lookup, replaceable
}

#[test]
fn the_s26_ultras_pen() {
    // The Galaxy S26 Ultra's S Pen (docs/s26.md; YMGPwcca/OneiOS on its SM-S948B, port-sm8850-s26): sec_e-pen with key
    // 0x140 (getevent calls it BTN_DIGI, the kernel's BTN_TOOL_PEN), tip and side button, x/y, pressure, distance, tilt.
    let (mut keys, mut abs) = ([0u8; 96], [0u8; 8]);
    for k in [0x140, 0x14a, 0x14b] {
        setbit(&mut keys, k);
    }
    for a in [0, 1, 0x18, 0x19, 0x1a, 0x1b] {
        setbit(&mut abs, a);
    }
    assert!(rank("sec_e-pen", &keys, &abs).is_ok_and(|r| r >= 1 + 2 + 4)); // at least a full pen's rank
    setbit(&mut keys, 115); // with Volume Up: never the pen
    assert!(rank("sec_e-pen", &keys, &abs).is_err_and(|w| w.contains("side keys")));
    // Its digitizer in its own units (x 0..7330, y 0..15881, pressure 0..4095, distance 0..255, tilt +-63) and a
    // stroke the fork read on it: a hover at height 77, a contact at pressure 156, a hover at 28, then out of range.
    let d = device([(0, 7330, 0), (0, 15881, 0), (0, 4095, 0), (0, 255, 0), (-63, 63, 0), (-63, 63, 0)]);
    let mut st = State { tool: true, ..State::default() };
    st.value = [5924, 10584, 0, 77, -1, 13];
    let s = sample_of(&d, 0, false, &st);
    assert!(s.in_range && !s.touch && s.pressure == 0 && s.distance == 77);
    assert!(s.x == (5924 * 32767 / 7330) as _ && s.y == (10584 * 32767 / 15881) as _);
    st.touch = true;
    st.value[PRESSURE] = 156;
    st.value[DISTANCE] = 0;
    let s = sample_of(&d, 0, false, &st);
    assert!(s.in_range && s.touch && s.pressure == 156 && s.distance == 0);
    st.touch = false;
    st.value[PRESSURE] = 0;
    st.value[DISTANCE] = 28;
    let s = sample_of(&d, 0, false, &st);
    assert!(s.in_range && !s.touch && s.distance == 28);
    st.tool = false;
    let s = sample_of(&d, 0, false, &st);
    assert!(!s.in_range && !s.touch);
}
#[test]
fn samples() {
    // A digitizer of 0..16000 x 0..34000, pressure 0..8191, height 0..255, tilt +-63 without a resolution.
    let mut d = device([(0, 16000, 0), (0, 34000, 0), (0, 8191, 0), (0, 255, 0), (-63, 63, 0), (-63, 63, 0)]);
    let mut st = State { tool: true, ..State::default() };
    st.value = [8000, 34000, 500, 0, 30, -20];
    // Hovering: in range, no pressure (the device's 500 is not a contact), a height of 1 at least (UIKit's hover needs
    // Z > 0).
    let s = sample_of(&d, 0, false, &st);
    assert!(s.in_range && !s.touch && !s.eraser && s.pressure == 0 && s.distance == 1);
    assert!(s.x == 16383 && s.y == 32767 && s.tilt_x == 30 && s.tilt_y == -20);
    st.touch = true;
    st.value[PRESSURE] = 8191;
    let s = sample_of(&d, 0, false, &st);
    assert!(s.touch && s.pressure == 4095 && s.distance == 0);
    st.value[PRESSURE] = 4096;
    assert_eq!(sample_of(&d, 0, false, &st).pressure, 2047); // scaled to 0..4095
    st.stylus = true;
    assert!(sample_of(&d, 0, false, &st).barrel);
    // Turned a quarter clockwise: the digitizer's top left is the panel's top right; the tilt turns with it. Mirrored:
    // back.
    st.value[X] = 0;
    st.value[Y] = 0;
    let s = sample_of(&d, 90, false, &st);
    assert!(s.x == 32767 && s.y == 0 && s.tilt_x == 20 && s.tilt_y == 30);
    let s = sample_of(&d, 90, true, &st);
    assert!(s.x == 0 && s.y == 0 && s.tilt_x == -20 && s.tilt_y == 30);
    let s = sample_of(&d, 180, false, &st);
    assert!(s.x == 32767 && s.y == 32767 && s.tilt_x == -30 && s.tilt_y == 20);
    let s = sample_of(&d, 270, false, &st);
    assert!(s.x == 0 && s.y == 32767 && s.tilt_x == -20 && s.tilt_y == -30);
    // Tilt by resolution (units per radian: 57 is about a degree a unit), and a wide range without one (its end is 90).
    d.axis[TILT_X] = Axis::new(-5000, 5000, 57);
    st.value[TILT_X] = 57;
    assert_eq!(sample_of(&d, 0, false, &st).tilt_x, 57);
    d.axis[TILT_X] = Axis::new(-9000, 9000, 0);
    st.value[TILT_X] = 4500;
    assert_eq!(sample_of(&d, 0, false, &st).tilt_x, 45);
    st.value[TILT_X] = -9000;
    assert_eq!(sample_of(&d, 0, false, &st).tilt_x, -89); // kept short of flat
    d.axis[TILT_X] = Axis::new(-63, 63, 0);
    // The eraser end; a tip that touches without BTN_TOOL_PEN is in range; out of range is all zero.
    let s = sample_of(&d, 0, false, &State { rubber: true, ..State::default() });
    assert!(s.in_range && s.eraser);
    let s = sample_of(&d, 0, false, &State { touch: true, ..State::default() });
    assert!(s.in_range && s.touch);
    let mut gone = State::default();
    gone.value[X] = 500;
    let s = sample_of(&d, 0, false, &gone);
    assert!(!s.in_range && !s.touch && s.x == 0 && s.pressure == 0 && s.distance == 0);
    // A device with no height axis hovers half way up.
    d.axis[DISTANCE].present = false;
    assert_eq!(sample_of(&d, 0, false, &State { tool: true, ..State::default() }).distance, 127);
}

#[test]
fn filter() {
    // The filter: frames to the guest's device, only what changed, nothing for a frame like the last.
    let cap = Capture::default();
    let d = device([(0, 16000, 0), (0, 34000, 0), (0, 8191, 0), (0, 255, 0), (-63, 63, 0), (-63, 63, 0)]);
    let mut src = Source::new(&cap, d, 0, false);
    src.read_state = Some(fake_read);
    let t = 5_000_000_000;
    src.feed(&[ev(t, 1, 0x140, 1), ev(t, 3, 0, 8000), ev(t, 3, 1, 17000), ev(t, 3, 0x19, 128), ev(t, 0, 0, 0)], t);
    let got = cap.take();
    let (tool, v) = find(&got, 1, 0x140).unwrap();
    assert!(
        v == 1
            && value(&got, 3, 0) == Some(16383)
            && value(&got, 3, 1) == Some(16383)
            && value(&got, 3, 0x19) == Some(128)
    );
    assert!(got.last().unwrap().0 == 0 && find(&got, 3, 0).unwrap().0 < tool); // positions before the keys, then SYN
    let same = [ev(t + 1, 3, 0, 8000), ev(t + 1, 0, 0, 0)];
    src.feed(&same, t + 1);
    assert!(cap.take().is_empty() && src.stats.unchanged == 1); // a pen held still in the air: nothing new
    src.feed(&[ev(t + 2, 3, 0x18, 4096), ev(t + 2, 1, 0x14a, 1), ev(t + 2, 0, 0, 0)], t + 2);
    let got = cap.take();
    assert!(value(&got, 3, 0x18) == Some(2047) && value(&got, 3, 0x19) == Some(0) && value(&got, 1, 0x14a) == Some(1));
    assert!(find(&got, 1, 0x140).is_none() && got.len() == 4);
    src.feed(&[ev(t + 3, 1, 0x14b, 1), ev(t + 3, 0, 0, 0)], t + 3);
    let got = cap.take();
    assert!(got.len() == 2 && value(&got, 1, 0x14b) == Some(1) && src.stats.presses == 1 && src.stats.strokes == 1);
    // SYN_DROPPED: the rest of the frame is dropped, the state read again (out of range now): tip up and the pen gone.
    FAKE.with(|f| f.set(State::default()));
    src.feed(&[ev(t + 4, 0, 3, 0), ev(t + 4, 3, 0, 1), ev(t + 4, 0, 0, 0)], t + 4);
    let got = cap.take();
    assert!(
        src.drops == 1
            && value(&got, 1, 0x14a) == Some(0)
            && value(&got, 1, 0x140) == Some(0)
            && value(&got, 1, 0x14b) == Some(0)
    );
    // the in-range summary was logged and reset
    assert!(value(&got, 3, 0x18) == Some(0) && find(&got, 3, 0).is_none() && src.stats.frames == 0);
    src.feed(&same, t + 5); // out of range, still: nothing
    assert!(cap.take().is_empty());
}

/// The S24 Ultra's sec_e-pen as Samsung's Wacom driver states and reports it (drivers/input/sec_input/wacom,
/// docs/hardware.md 7f): x 0..7314, y 0..15686 (the device tree's max coords), no resolution, tilt -63..63, and the
/// screen-off keys KEY_HOMEPAGE and KEY_WAKEUP_UNLOCK beside the pen's.
fn s24_pen() -> Device {
    let mut d = device([(0, 7314, 0), (0, 15686, 0), (0, 4095, 0), (0, 255, 0), (-63, 63, 0), (-63, 63, 0)]);
    (d.rubber, d.garage, d.pen_out) = (true, true, true);
    d
}

#[test]
fn the_s24_ultras_pen() {
    let (mut keys, mut abs) = ([0u8; 96], [0u8; 8]);
    for k in [0x140, 0x141, 0x14a, 0x14b, 172, 0x2bf] {
        setbit(&mut keys, k);
    }
    for a in [0, 1, 0x18, 0x19, 0x1a, 0x1b] {
        setbit(&mut abs, a);
    }
    assert_eq!(rank("sec_e-pen", &keys, &abs), Ok(8)); // KEY_HOMEPAGE is no side key
    let sp = s24_pen();
    // pen-rotate=auto: the IC's frame is the 1440 x 3120 panel's shape, so it is taken as it is.
    let (r, how) = rotation(None, &sp, 1440, 3120);
    assert!(r == 0 && how.contains("both portrait"));
    let text = describe(&sp);
    assert!(text.contains("x 0..7314, y 0..15686, pressure 0..4095, distance 0..255, tilt x -63..63"), "{text}");
    assert!(text.contains("SW_PEN_INSERT 1 (Samsung: the pen out of its garage)"), "{text}");
    let cap = Capture::default();
    let mut sps = Source::new(&cap, s24_pen(), 0, false);
    let u = 9_000_000_000;
    // A hover frame in the driver's order: X, Y, BTN_STYLUS, BTN_TOUCH, pressure, distance, tilt, the tool, SYN.
    let near = [
        ev(u, 3, 0, 3657),
        ev(u, 3, 1, 15686),
        ev(u, 1, 0x14b, 0),
        ev(u, 1, 0x14a, 0),
        ev(u, 3, 0x18, 0),
        ev(u, 3, 0x19, 40),
        ev(u, 3, 0x1a, 10),
        ev(u, 3, 0x1b, -5),
        ev(u, 1, 0x140, 1),
        ev(u, 0, 0, 0),
    ];
    sps.feed(&near, u);
    let got = cap.take();
    assert!(value(&got, 3, 0) == Some(16383) && value(&got, 3, 1) == Some(32767) && value(&got, 3, 0x19) == Some(40));
    assert!(value(&got, 3, 0x1a) == Some(10) && value(&got, 3, 0x1b) == Some(-5) && value(&got, 1, 0x140) == Some(1));
    // Contact: the driver sends the distance while touching too; the guest gets 0 then, and the pressure.
    sps.feed(
        &[
            ev(u + 1, 3, 0, 3657),
            ev(u + 1, 1, 0x14a, 1),
            ev(u + 1, 3, 0x18, 2048),
            ev(u + 1, 3, 0x19, 12),
            ev(u + 1, 0, 0, 0),
        ],
        u + 1,
    );
    let got = cap.take();
    assert!(value(&got, 1, 0x14a) == Some(1) && value(&got, 3, 0x18) == Some(2048) && value(&got, 3, 0x19) == Some(0));
    // The leave, two frames: the tip up (a hover again, never at height 0), then the tool out.
    sps.feed(&[ev(u + 2, 3, 0x18, 0), ev(u + 2, 1, 0x14b, 0), ev(u + 2, 1, 0x14a, 0), ev(u + 2, 0, 0, 0)], u + 2);
    let got = cap.take();
    assert!(value(&got, 1, 0x14a) == Some(0) && value(&got, 3, 0x19) == Some(12) && find(&got, 1, 0x140).is_none());
    let s = sps.stats;
    assert!(s.hover == 2 && s.height_min == 12 && s.height_max == 40 && s.strokes == 1 && s.pressure_max == 2048);
    sps.feed(&[ev(u + 3, 3, 0x19, 0), ev(u + 3, 1, 0x141, 0), ev(u + 3, 1, 0x140, 0), ev(u + 3, 0, 0, 0)], u + 3);
    let got = cap.take();
    assert!(value(&got, 1, 0x140) == Some(0) && sps.stats.frames == 0); // out of range: logged and reset
                                                                        // The screen-off keys and the garage switch: never passed on.
    let aop = [
        ev(u + 4, 1, 0x2bf, 1),
        ev(u + 4, 0, 0, 0),
        ev(u + 4, 1, 0x2bf, 0),
        ev(u + 4, 1, 172, 1),
        ev(u + 4, 0, 0, 0),
        ev(u + 4, 5, 0x0f, 0),
        ev(u + 4, 0, 0, 0),
    ];
    sps.feed(&aop, u + 4);
    assert!(cap.take().is_empty() && sps.aop_keys == 2);
}

#[test]
fn coalescing() {
    // The coalescing (spen-lag, 2026-11-22: ~480 frames a second, each a HID report, made backboardd take 100-160 % of
    // a CPU): at most one frame per 8.33 ms but the edges, which go at once; the held frame goes at its time (tick); the
    // pressure sent is the most of the folded frames; a hover within a pixel is not sent.
    let cap = Capture::default();
    let mut c = Source::new(&cap, s24_pen(), 0, false);
    assert!(c.period_ns == 8_333_333 && c.pixel_x == 23 && c.pixel_y == 11); // 1440 x 3120 by default
    let (ms, w) = (1_000_000i64, 20_000_000_000i64);
    c.feed(&[ev(w, 3, 0, 1000), ev(w, 3, 1, 1000), ev(w, 3, 0x19, 50), ev(w, 1, 0x140, 1), ev(w, 0, 0, 0)], w);
    assert!(syns(&cap.take()) == 1 && c.due() == 0); // in range: an edge, at once
                                                     // Hover moves 2 ms later: held, not sent; a newer one replaces it.
    c.feed(&[ev(w + 2 * ms, 3, 0, 1100), ev(w + 2 * ms, 0, 0, 0)], w + 2 * ms);
    assert!(cap.take().is_empty() && c.pending && c.due() == w + 8_333_333);
    // Back within a pixel of what was sent: nothing to send, the held frame is dropped.
    c.feed(&[ev(w + 3 * ms, 3, 0, 1000), ev(w + 3 * ms, 3, 1, 1003), ev(w + 3 * ms, 0, 0, 0)], w + 3 * ms);
    assert!(cap.take().is_empty() && !c.pending && c.stats.still == 1 && c.stats.coalesced == 1);
    c.feed(&[ev(w + 4 * ms, 3, 0, 1400), ev(w + 4 * ms, 0, 0, 0)], w + 4 * ms);
    c.tick(w + 5 * ms);
    assert!(cap.take().is_empty() && c.pending); // not due yet
    c.tick(w + 9 * ms); // due: the held hover goes, nothing more came
    let got = cap.take();
    assert!(syns(&got) == 1 && value(&got, 3, 0) == Some((1400i64 * 32767 / 7314) as i32) && !c.pending);
    // The tip down 1 ms after that send: an edge, at once.
    c.feed(&[ev(w + 10 * ms, 3, 0x18, 500), ev(w + 10 * ms, 1, 0x14a, 1), ev(w + 10 * ms, 0, 0, 0)], w + 10 * ms);
    let got = cap.take();
    assert!(syns(&got) == 1 && value(&got, 1, 0x14a) == Some(1) && value(&got, 3, 0x18) == Some(500));
    // Three touching frames within 8.33 ms: held, each replacing the last; the next one, 9 ms after the send, goes with
    // the newest position and the most pressure of the four (3000).
    let m = [
        ev(w + 11 * ms, 3, 0, 1500),
        ev(w + 11 * ms, 3, 0x18, 1000),
        ev(w + 11 * ms, 0, 0, 0),
        ev(w + 12 * ms, 3, 0, 1600),
        ev(w + 12 * ms, 3, 0x18, 3000),
        ev(w + 12 * ms, 0, 0, 0),
        ev(w + 13 * ms, 3, 0, 1700),
        ev(w + 13 * ms, 3, 0x18, 2000),
        ev(w + 13 * ms, 0, 0, 0),
        ev(w + 19 * ms, 3, 0, 1800),
        ev(w + 19 * ms, 3, 0x18, 1500),
        ev(w + 19 * ms, 0, 0, 0),
    ];
    c.feed(&m, w + 19 * ms);
    let got = cap.take();
    assert!(
        syns(&got) == 1
            && value(&got, 3, 0) == Some((1800i64 * 32767 / 7314) as i32)
            && value(&got, 3, 0x18) == Some(3000)
    );
    assert!(c.stats.coalesced == 4 && !c.pending);
    // The tip up 1 ms later and the leave 1 ms after it: both edges, both at once, whatever the rate.
    let up = [
        ev(w + 20 * ms, 3, 0x18, 0),
        ev(w + 20 * ms, 1, 0x14a, 0),
        ev(w + 20 * ms, 0, 0, 0),
        ev(w + 21 * ms, 1, 0x140, 0),
        ev(w + 21 * ms, 0, 0, 0),
    ];
    c.feed(&up, w + 21 * ms);
    let got = cap.take();
    assert!(
        syns(&got) == 2 && value(&got, 1, 0x14a) == Some(0) && value(&got, 1, 0x140) == Some(0) && c.stats.frames == 0
    );
    // A batch read late (the frames' own times 1 ms apart, read at once): coalesced by their times, not the read's.
    let late: Vec<Event> = (0..40)
        .flat_map(|i| {
            let at = w + 100 * ms + i * ms;
            [ev(at, 3, 0, (1000 + 50 * i) as i32), ev(at, 1, 0x140, 1), ev(at, 0, 0, 0)]
        })
        .collect();
    let mut b = Source::new(&cap, s24_pen(), 0, false);
    b.feed(&late, w + 200 * ms);
    // 40 ms of frames: the first (an edge), then one per 8.33 ms (at 9, 18, 27, 36 ms), the newest still held.
    assert!(syns(&cap.take()) == 5 && b.pending && b.stats.coalesced == 34);
    // With no period, every frame that changed goes (the old behaviour).
    let mut all = Source::new(&cap, s24_pen(), 0, false);
    all.period_ns = 0;
    all.feed(&late[..60], w + 200 * ms); // 20 frames
    assert!(syns(&cap.take()) == 20 && !all.pending);
    // Frame times on another clock (EVIOCSCLOCKID refused): the read's time is taken, so the held frame is still due on
    // tick's clock.
    let mut r = Source::new(&cap, s24_pen(), 0, false);
    let other = 500_000_000_000_000_000i64;
    r.feed(&[ev(other, 3, 0, 1000), ev(other, 1, 0x140, 1), ev(other, 0, 0, 0)], w);
    r.feed(&[ev(other + 2 * ms, 3, 0, 1500), ev(other + 2 * ms, 0, 0, 0)], w + 2 * ms);
    assert!(syns(&cap.take()) == 1 && r.pending && r.due() == w + 8_333_333);
    r.tick(w + 9 * ms);
    assert!(syns(&cap.take()) == 1 && !r.pending);
    all.set_panel(720, 1560);
    assert!(all.pixel_x == 46 && all.pixel_y == 22);
}

#[test]
fn the_guests_pen_device() {
    testing::install();
    // The guest's pen device: its name, keys and fixed ranges in its configuration.
    let g = virtio_input::create(0, 46).unwrap();
    guest_device(&g);
    g.mmio_write(0x100, 1, 1);
    assert!(g.mmio_read(0x102, 1) == 10 && g.mmio_read(0x108 + 7, 1) == b'p' as u64); // "OneiOS pen"
    g.mmio_write(0x100, 1, 0x11);
    g.mmio_write(0x101, 1, 1); // EV_KEY: BTN_TOOL_PEN 0x140, BTN_STYLUS 0x14b
    assert!(g.mmio_read(0x108 + 40, 1) & 3 == 3 && g.mmio_read(0x108 + 41, 1) & 0x0c == 0x0c);
    assert_eq!(g.mmio_read(0x108 + 14, 1) & 0x1c, 0); // no side keys
    g.mmio_write(0x101, 1, 3); // EV_ABS: X, Y, pressure, distance, tilt
    assert!(g.mmio_read(0x108, 1) & 3 == 3 && g.mmio_read(0x108 + 3, 1) & 0x0f == 0x0f);
    g.mmio_write(0x100, 1, 0x12);
    g.mmio_write(0x101, 1, 0x1a);
    assert!(g.mmio_read(0x108, 4) as u32 as i32 == -90 && g.mmio_read(0x108 + 4, 4) == 90);
    g.mmio_write(0x100, 1, 0x10); // INPUT_PROP_DIRECT, as the guest's transport needs
    assert_eq!(g.mmio_read(0x108, 1), 2);
    // The touch screen's device keeps its own name and keys.
    let touch = virtio_input::create(0, 47).unwrap();
    touch.mmio_write(0x100, 1, 1);
    assert_eq!(touch.mmio_read(0x102, 1), 18);
}

#[test]
fn rotation_and_the_scripts_pen() {
    // pen-rotate=auto: a landscape digitizer under a portrait panel is turned a quarter; a portrait one is not.
    let d = device([(0, 16000, 0), (0, 34000, 0), (0, 8191, 0), (0, 255, 0), (-63, 63, 0), (-63, 63, 0)]);
    let (r, how) = rotation(None, &d, 1440, 3120);
    assert!(r == 0 && !how.is_empty());
    let wide = device([(0, 34000, 0), (0, 16000, 0), (0, 8191, 0), (0, 255, 0), (-63, 63, 0), (-63, 63, 0)]);
    assert!(rotation(None, &wide, 1440, 3120).0 == 90 && rotation(Some(270), &wide, 1440, 3120).0 == 270);
    // The script's pen lines: their steps are the pen's; a stroke is down, moves and up.
    let steps = input_script::parse(
        "pen-abs 0 0 16000\npen-hover 100 200 50 40\npen-stroke 0 0 160 0 32 100 900 10 -5\npen-button 1\npen-out\n",
    )
    .unwrap();
    let pens = steps.iter().filter(|s| s.is_pen()).count();
    let tip = |v: bool| {
        steps.iter().filter(|s| matches!(s, Step::Event { code: 0x14a, value, .. } if (*value != 0) == v)).count()
    };
    assert!(matches!(steps[0], Step::Abs { pen: true, max: 16000, .. }));
    assert!(pens == steps.len() - 3 && tip(true) == 1 && tip(false) == 2); // 3 waits; pen-out lifts too
    assert_eq!(input_script::parse("pen-stroke 1 2 3\n"), Err(1));
    assert!(input_script::parse("pen-button 2\n").is_err());
    // The demo script (the PC boot's) parses and has its pen.
    let demo = concat!(env!("CARGO_MANIFEST_DIR"), "/tools/input-demo.script");
    if std::path::Path::new(demo).exists() {
        assert_eq!(input_script::load(demo), Ok(true));
    }
}
