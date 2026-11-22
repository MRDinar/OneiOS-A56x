// SPDX-License-Identifier: MIT
//! The S Pen (spen, 2026-11-15): the phone's pen digitizer as the guest's pen device, a virtio-input device of its own
//! (/arm-io/input-1) beside the touch screen's, which the guest publishes as iOS's pencil digitizer (Digitizer /
//! Stylus, kernel/patches/oneios-hosted/hid, staged/hid_pen.rs) when it is booted with oneios-pen=1.
//!
//! The host side has three parts, each replaceable on its own:
//! - the lookup: [`lookup`] finds the pen's input device by its capabilities and opens it read only. The default,
//!   [`lookup_evdev`], scans /dev/input/event0..63 by ioctl as input.rs does for the touch screen (the Samsung
//!   kernel's device is "sec_e-pen": docs/agents.md, 2026-11-10). Another hardware backend (a generic/Treble one) sets
//!   its own function with [`set_lookup`]; nothing else here names a Samsung path.
//! - the filter: [`Source::feed`] / [`sample_of`]: the device's evdev events, frame by frame, into one sample in the
//!   guest's units: in range (BTN_TOOL_PEN or BTN_TOOL_RUBBER), the tip down (BTN_TOUCH), the side button (BTN_STYLUS,
//!   BTN_STYLUS2), x and y 0..32767 turned to the panel's frame (pen-rotate, pen-mirror), pressure 0..4095 (only while
//!   the tip is down), the height above the panel 0..255 (ABS_DISTANCE; never 0 while hovering: UIKit makes no hover
//!   of a stylus event whose Z is 0), tilt x and y in degrees.
//! - the guest: [`guest_device`]: what the guest's pen device states (those fixed ranges), so the guest's transport
//!   (DroidVMVirtioInput.c, KIND_PEN) needs no knowledge of the phone's digitizer.
//!
//! A frame whose sample is the one last sent is not sent again (a pen held still in the air reports nothing new to
//! iOS), and only the fields that changed go out before the frame's SYN_REPORT. After SYN_DROPPED (the reader was too
//! slow) the device's state is read again (EVIOCGKEY, EVIOCGABS), so that a lift or a leave lost in the drop does not
//! leave the pen down or in range in iOS.
//!
//! The device is never grabbed (EVIOCGRAB): in a session nothing else reads it (zygote and system_server are gone, as
//! for the touch screen: oneos/interface.md), and One UI gets it back as it was. A device that has the side keys
//! (KEY_POWER, KEY_VOLUMEUP, KEY_VOLUMEDOWN) or multitouch positions is never taken as the pen.
//!
//! The S24 Ultra's device, from Samsung's Wacom driver (drivers/input/sec_input/wacom, docs/hardware.md 7f): one
//! device, "sec_e-pen", with BTN_TOOL_PEN, BTN_TOOL_RUBBER, BTN_TOUCH, BTN_STYLUS, ABS_X/Y (0..the IC's maximum, no
//! resolution), ABS_PRESSURE 0..4095, ABS_DISTANCE 0..255 (sent in every frame, also while touching: only a hovering
//! frame's is used), ABS_TILT_X/Y -63..63 (degrees, no resolution), SW_PEN_INSERT (the garage), and the screen-off keys
//! KEY_WAKEUP_UNLOCK and KEY_HOMEPAGE; no INPUT_PROP_DIRECT. Those keys come only in the IC's screen-off (AOP) mode,
//! which sends no positions: they are never passed on, and the first one is logged (the IC asleep for iOS: the
//! brightness bridge wakes it with the touch IC when xnuhost gives the guest this pen, ONEIOS_PEN_IC=1). input.rs never
//! takes this device (no side key, no multitouch), so its KEY_HOMEPAGE never presses Home. The leave is two frames:
//! the tip up, then the tool out.
//!
//! One line per time in range (the pen comes near, then leaves): `pen: in range N ms: F frames (H hovering, T
//! touching), S strokes, pressure up to P/4095, height a..b/255 hovering, tilt x a..b y c..d degrees, the side button
//! pressed B times; U unchanged frames not sent`.
use std::os::fd::{AsRawFd, OwnedFd};
use std::sync::atomic::{AtomicUsize, Ordering};

use xh_core::logi;

use crate::input::ev::*;
use crate::input::{evdev, Event, EventSink};
use crate::virtio_input::{self, InputDev};

/// What the guest's pen device states: x and y 0..32767, pressure 0..4095, the height above the panel 0..255.
pub const XY_MAX: i32 = 32767;
pub const PRESSURE_MAX: i32 = 4095;
pub const DISTANCE_MAX: i32 = 255;
/// At most one frame that is not an edge per this to the guest: the panel's 120 Hz (spen-lag, 2026-11-22).
pub const PERIOD_NS: i64 = 8_333_333;

/// The axes of a pen, in this order.
pub const X: usize = 0;
pub const Y: usize = 1;
pub const PRESSURE: usize = 2;
pub const DISTANCE: usize = 3;
pub const TILT_X: usize = 4;
pub const TILT_Y: usize = 5;
pub const AXES: usize = 6;
const AXIS_CODE: [u16; AXES] = [ABS_X, ABS_Y, ABS_PRESSURE, ABS_DISTANCE, ABS_TILT_X, ABS_TILT_Y];

/// One axis of the host's device as EVIOCGABS states it.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Axis {
    pub min: i32,
    pub max: i32,
    pub res: i32,
    pub present: bool,
}

impl Axis {
    pub const fn new(min: i32, max: i32, res: i32) -> Axis {
        Axis { min, max, res, present: true }
    }
}

/// The host's pen device, as a lookup found it: its node (or what a backend names it), name, open descriptor (read
/// only, never grabbed; None for a scripted pen), axes, and which of the eraser end and the side button it has.
#[derive(Debug, Default)]
pub struct Device {
    pub path: String,
    pub name: String,
    pub fd: Option<OwnedFd>,
    pub axis: [Axis; AXES],
    pub rubber: bool,
    pub stylus: bool,
    /// the device has SW_PEN_INSERT
    pub garage: bool,
    /// its value at the lookup (Samsung's driver reports 1 with the pen out of its garage)
    pub pen_out: bool,
}

/// The device's state in its own units, as its events (or its ioctls) give it.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct State {
    /// BTN_TOOL_PEN, BTN_TOOL_RUBBER, BTN_TOUCH, either side button
    pub tool: bool,
    pub rubber: bool,
    pub touch: bool,
    pub stylus: bool,
    /// 1 BTN_STYLUS, 2 BTN_STYLUS2
    pub stylus_bits: u32,
    pub value: [i32; AXES],
}

/// One frame for the guest, in its pen device's units.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Sample {
    pub in_range: bool,
    pub eraser: bool,
    pub touch: bool,
    pub barrel: bool,
    pub x: i32,
    pub y: i32,
    pub pressure: i32,
    pub distance: i32,
    pub tilt_x: i32,
    pub tilt_y: i32,
}

/// One time in range, for its log line.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Stats {
    pub since_ns: i64,
    pub frames: u32,
    pub hover: u32,
    pub touching: u32,
    pub strokes: u32,
    pub presses: u32,
    pub unchanged: u32,
    pub pressure_max: i32,
    pub tilt_min: [i32; 2],
    pub tilt_max: [i32; 2],
    /// while hovering, in the guest's units (1..255)
    pub height_min: i32,
    pub height_max: i32,
    /// frames held then replaced by a newer one; hovering frames within a pixel; sent
    pub coalesced: u32,
    pub still: u32,
    pub sent: u32,
}

fn byte_bit(bits: &[u8], n: u16) -> bool {
    evdev::bit(bits, n as usize)
}

// ---- the lookup's test (portable) ------------------------------------------------------------------------------------
/// Whether a device with this name and these key and axis bits (EVIOCGBIT, byte arrays) is a pen: Err(why not), else
/// its rank (the best rank is taken). A device with a side key or multitouch positions is never one.
pub fn rank(name: &str, keys: &[u8], abs: &[u8]) -> Result<u32, &'static str> {
    if byte_bit(keys, KEY_POWER) || byte_bit(keys, KEY_VOLUMEUP) || byte_bit(keys, KEY_VOLUMEDOWN) {
        return Err("it has side keys (never taken)");
    }
    if byte_bit(abs, ABS_MT_POSITION_X) {
        return Err("a multitouch device");
    }
    if !byte_bit(keys, BTN_TOOL_PEN) {
        return Err("no BTN_TOOL_PEN");
    }
    if !byte_bit(abs, ABS_X) || !byte_bit(abs, ABS_Y) {
        return Err("no ABS_X/ABS_Y");
    }
    if !byte_bit(abs, ABS_PRESSURE) {
        return Err("no ABS_PRESSURE");
    }
    // A pen: more for what iOS can use (tilt, the side button), and for a name that says it is one (Samsung's
    // "sec_e-pen").
    let mut rank =
        1 + byte_bit(abs, ABS_TILT_X) as u32 + byte_bit(keys, BTN_STYLUS) as u32 + byte_bit(abs, ABS_DISTANCE) as u32;
    if ["pen", "Pen", "stylus", "Stylus"].iter().any(|w| name.contains(w)) {
        rank += 4;
    }
    Ok(rank)
}

// ---- the filter (portable) -------------------------------------------------------------------------------------------
fn scaled(v: i32, a: &Axis, top: i32) -> i32 {
    if !a.present || a.max <= a.min {
        return 0;
    }
    let v = v.clamp(a.min, a.max);
    ((v as i64 - a.min as i64) * top as i64 / (a.max as i64 - a.min as i64)) as i32
}

/// A tilt axis in whole degrees: by its resolution (evdev's for tilt: units per radian), else as degrees when its range
/// lies within +-90, else its range's end as 90 degrees. Kept within +-89 (the guest's altitude needs some lean left).
fn degrees(v: i32, a: &Axis) -> i32 {
    if !a.present {
        return 0;
    }
    let end = if a.max > -a.min { a.max } else { -a.min };
    let d: i64 = if a.res > 0 {
        v as i64 * 18_000_000 / (a.res as i64 * 314_159)
    } else if end <= 90 {
        v as i64
    } else if end != 0 {
        v as i64 * 90 / end as i64
    } else {
        0
    };
    d.clamp(-89, 89) as i32
}

/// The state's sample: x, y and the tilt turned clockwise by `rotate` (0, 90, 180, 270) onto the panel, then mirrored
/// left to right.
pub fn sample_of(d: &Device, rotate: u32, mirror: bool, st: &State) -> Sample {
    let mut out = Sample { touch: st.touch, in_range: st.tool || st.rubber || st.touch, ..Sample::default() };
    // In range: either end near the panel. A tip that touches is in range even if the driver never said so.
    if !out.in_range {
        return out;
    }
    out.eraser = st.rubber && !st.tool;
    out.barrel = st.stylus;
    let (x, y) = (scaled(st.value[X], &d.axis[X], XY_MAX), scaled(st.value[Y], &d.axis[Y], XY_MAX));
    let (tx, ty) = (degrees(st.value[TILT_X], &d.axis[TILT_X]), degrees(st.value[TILT_Y], &d.axis[TILT_Y]));
    // The digitizer's frame turned clockwise by `rotate` onto the panel's, then mirrored left to right; the tilt with it.
    let (mut px, py, mut ptx, pty) = match rotate {
        90 => (XY_MAX - y, x, -ty, tx),
        180 => (XY_MAX - x, XY_MAX - y, -tx, -ty),
        270 => (y, XY_MAX - x, ty, -tx),
        _ => (x, y, tx, ty),
    };
    if mirror {
        px = XY_MAX - px;
        ptx = -ptx;
    }
    (out.x, out.y, out.tilt_x, out.tilt_y) = (px, py, ptx, pty);
    if st.touch {
        out.pressure = if d.axis[PRESSURE].present {
            scaled(st.value[PRESSURE], &d.axis[PRESSURE], PRESSURE_MAX)
        } else {
            PRESSURE_MAX / 2
        };
    } else {
        // The height above the panel while hovering: never 0 (UIKit's hover needs the stylus event's Z above 0); half
        // way for a device that states none.
        let h = if d.axis[DISTANCE].present {
            scaled(st.value[DISTANCE], &d.axis[DISTANCE], DISTANCE_MAX)
        } else {
            DISTANCE_MAX / 2
        };
        out.distance = h.max(1);
    }
    out
}

/// A device's state read again after SYN_DROPPED (None: it could not be).
pub type ReadState<S> = fn(&Source<S>) -> Option<State>;

/// The pen's filter: its device's frames, coalesced, to the guest's pen device.
pub struct Source<S> {
    pub sink: S,
    pub device: Device,
    pub rotate: u32,
    pub mirror: bool,
    pub dropping: bool,
    pub sent_any: bool,
    pub read_state: Option<ReadState<S>>,
    pub state: State,
    /// the last sample the guest was sent
    pub sent: Sample,
    /// the last frame of the device (sent or not)
    pub prev: Sample,
    /// PERIOD_NS (0: every frame)
    pub period_ns: i64,
    /// one panel pixel in the guest's units (set_panel)
    pub pixel_x: i32,
    pub pixel_y: i32,
    /// a frame is held: tick sends it at sent_ns + period_ns
    pub pending: bool,
    pub held: Sample,
    /// the most pressure of the frames folded since the last send
    pub held_pressure: i32,
    pub sent_ns: i64,
    pub drops: u32,
    pub frames_sent: u32,
    pub events: u64,
    /// KEY_WAKEUP_UNLOCK / KEY_HOMEPAGE from the pen's IC (its screen-off mode)
    pub aop_keys: u32,
    pub stats: Stats,
}

fn plural(n: u32) -> &'static str {
    if n == 1 {
        ""
    } else {
        "s"
    }
}

impl<S: EventSink> Source<S> {
    /// The filter of `device` into `sink`, turned by `rotate` and mirrored; the S24 Ultra's panel until
    /// [`set_panel`](Self::set_panel) says the guest's frame.
    pub fn new(sink: S, device: Device, rotate: u32, mirror: bool) -> Source<S> {
        let mut s = Source {
            sink,
            device,
            rotate,
            mirror,
            dropping: false,
            sent_any: false,
            read_state: None,
            state: State::default(),
            sent: Sample::default(),
            prev: Sample::default(),
            period_ns: PERIOD_NS,
            pixel_x: 1,
            pixel_y: 1,
            pending: false,
            held: Sample::default(),
            held_pressure: 0,
            sent_ns: 0,
            drops: 0,
            frames_sent: 0,
            events: 0,
            aop_keys: 0,
            stats: Stats::default(),
        };
        s.set_panel(1440, 3120);
        s
    }

    /// The guest's frame in pixels: a hovering frame that moves less than one is not sent.
    pub fn set_panel(&mut self, width: u32, height: u32) {
        // One panel pixel in the guest's units (0..32767 across the panel), at least 1: the hover's "moved" step.
        let px = |n: u32| if n != 0 { (XY_MAX as u32).div_ceil(n) as i32 } else { 1 };
        self.pixel_x = px(width);
        self.pixel_y = px(height);
    }

    fn log_proximity(&mut self, now_ns: i64) {
        let t = self.stats;
        let ms = if t.since_ns != 0 && now_ns > t.since_ns { (now_ns - t.since_ns) / 1_000_000 } else { 0 };
        let height = if t.hover != 0 {
            format!(", height {}..{}/255 hovering", t.height_min, t.height_max)
        } else {
            String::new()
        };
        let tilt = if self.device.axis[TILT_X].present {
            format!(", tilt x {}..{} y {}..{} degrees", t.tilt_min[0], t.tilt_max[0], t.tilt_min[1], t.tilt_max[1])
        } else {
            String::new()
        };
        logi!(
            "pen: in range {} ms: {} frame{} ({} hovering, {} touching), {} stroke{}, pressure up to {}/4095{}{}, the side \
             button pressed {} time{}; {} unchanged frame{} not sent, {} coalesced (at most one frame per {}.{} ms but \
             the edges), {} hovering frame{} moved under a pixel; {} sent",
            ms,
            t.frames,
            plural(t.frames),
            t.hover,
            t.touching,
            t.strokes,
            plural(t.strokes),
            t.pressure_max,
            height,
            tilt,
            t.presses,
            plural(t.presses),
            t.unchanged,
            plural(t.unchanged),
            t.coalesced,
            self.period_ns / 1_000_000,
            self.period_ns / 100_000 % 10,
            t.still,
            plural(t.still),
            t.sent
        );
        self.stats = Stats::default();
    }

    /// What one frame of the device adds to its time in range's line (every frame, sent or not).
    fn account(&mut self, n: &Sample, now_ns: i64) {
        let t = &mut self.stats;
        let o = self.prev;
        if n.in_range && t.since_ns == 0 {
            t.since_ns = if now_ns != 0 { now_ns } else { 1 };
            t.tilt_min = [n.tilt_x, n.tilt_y];
            t.tilt_max = [n.tilt_x, n.tilt_y];
        }
        if n.in_range {
            t.frames += 1;
            if n.touch {
                t.touching += 1;
            } else {
                if t.hover == 0 || n.distance < t.height_min {
                    t.height_min = n.distance;
                }
                if t.hover == 0 || n.distance > t.height_max {
                    t.height_max = n.distance;
                }
                t.hover += 1;
            }
            t.pressure_max = t.pressure_max.max(n.pressure);
            t.tilt_min = [t.tilt_min[0].min(n.tilt_x), t.tilt_min[1].min(n.tilt_y)];
            t.tilt_max = [t.tilt_max[0].max(n.tilt_x), t.tilt_max[1].max(n.tilt_y)];
            if n.touch && !o.touch {
                t.strokes += 1;
            }
            if n.barrel && !o.barrel {
                t.presses += 1;
            }
        }
        self.prev = *n;
    }

    /// One sample to the guest: the fields that changed, then the frame's end. Nothing when nothing changed.
    fn send(&mut self, n: &Sample, now_ns: i64) {
        let o = self.sent;
        let all = !self.sent_any;
        let mut any = false;
        let mut field = |sink: &S, new: i32, old: i32, kind: u16, code: u16| {
            if all || new != old {
                sink.event(kind, code, new);
                any = true;
            }
        };
        // Positions and axes before the keys, so that a tip that comes down does so where its frame says.
        if n.in_range {
            field(&self.sink, n.x, o.x, EV_ABS, ABS_X);
            field(&self.sink, n.y, o.y, EV_ABS, ABS_Y);
            field(&self.sink, n.tilt_x, o.tilt_x, EV_ABS, ABS_TILT_X);
            field(&self.sink, n.tilt_y, o.tilt_y, EV_ABS, ABS_TILT_Y);
        }
        field(&self.sink, n.pressure, o.pressure, EV_ABS, ABS_PRESSURE);
        field(&self.sink, n.distance, o.distance, EV_ABS, ABS_DISTANCE);
        field(&self.sink, n.barrel as i32, o.barrel as i32, EV_KEY, BTN_STYLUS);
        field(&self.sink, n.touch as i32, o.touch as i32, EV_KEY, BTN_TOUCH);
        let (pen, rubber) = (n.in_range && !n.eraser, n.in_range && n.eraser);
        let (was_pen, was_rubber) = (o.in_range && !o.eraser, o.in_range && o.eraser);
        field(&self.sink, pen as i32, if all { !pen as i32 } else { was_pen as i32 }, EV_KEY, BTN_TOOL_PEN);
        field(&self.sink, rubber as i32, if all { !rubber as i32 } else { was_rubber as i32 }, EV_KEY, BTN_TOOL_RUBBER);
        if !any {
            if n.in_range {
                self.stats.unchanged += 1;
            }
            return;
        }
        self.sink.event(EV_SYN, SYN_REPORT, 0);
        self.frames_sent += 1;
        self.stats.sent += 1;
        let left = o.in_range && !n.in_range;
        self.sent = *n;
        self.sent_any = true;
        if left {
            self.log_proximity(now_ns);
        }
    }

    /// Send now: the sample, with the most pressure of the frames folded into it while the tip was down. The held
    /// frame (if any) is replaced by this one.
    fn deliver(&mut self, n: &Sample, now_ns: i64) {
        let mut m = *n;
        if m.touch && self.held_pressure > m.pressure {
            m.pressure = self.held_pressure;
        }
        self.held_pressure = 0;
        self.pending = false;
        self.sent_ns = now_ns;
        self.send(&m, now_ns);
    }

    /// One frame of the device (at its SYN_REPORT, its own time `at`), coalesced to the panel's rate (spen-lag,
    /// 2026-11-22: the S Pen's ~480 frames a second were each a HID report and a stylus event, and backboardd took
    /// 100-160 % of a CPU while drawing). An edge (in range or out, the tip down or up, the eraser end, the side
    /// button) goes at once, with the newest position. Any other frame goes when period_ns has passed since the last
    /// send; until then it is held, a newer one replaces it (counted as coalesced), and [`tick`](Self::tick) sends the
    /// held one when its time comes. The pressure sent is the most of the frames folded together while the tip was
    /// down. A hovering frame within a pixel of the one last sent is not sent at all. A frame like the last one sent
    /// is not sent (unchanged).
    fn frame(&mut self, n: &Sample, at: i64) {
        self.account(n, at);
        let o = self.sent;
        let edge = !self.sent_any
            || n.in_range != o.in_range
            || n.touch != o.touch
            || n.eraser != o.eraser
            || n.barrel != o.barrel;
        if self.pending {
            self.stats.coalesced += 1; // the held frame: this newer one takes its place
            self.pending = false;
        }
        if edge || self.period_ns == 0 {
            self.deliver(n, at);
            return;
        }
        if *n == o {
            if n.in_range {
                self.stats.unchanged += 1;
            }
            self.held_pressure = 0;
            return;
        }
        if !n.touch {
            let (dx, dy) = (n.x - o.x, n.y - o.y);
            if dx < self.pixel_x && dx > -self.pixel_x && dy < self.pixel_y && dy > -self.pixel_y {
                self.stats.still += 1;
                return;
            }
        }
        if n.touch && n.pressure > self.held_pressure {
            self.held_pressure = n.pressure;
        }
        if at - self.sent_ns >= self.period_ns {
            self.deliver(n, at);
            return;
        }
        self.held = *n;
        self.pending = true;
    }

    /// The coalescing's clock: the time the held frame is due (0: none).
    pub fn due(&self) -> i64 {
        if self.pending {
            self.sent_ns + self.period_ns
        } else {
            0
        }
    }

    /// Sends the held frame when it is due (input.rs's thread, after poll).
    pub fn tick(&mut self, now_ns: i64) {
        if self.pending && now_ns >= self.sent_ns + self.period_ns {
            let held = self.held;
            self.deliver(&held, now_ns);
        }
    }

    fn resync(&mut self, now_ns: i64) {
        self.drops += 1;
        let Some(st) = self.read_state.and_then(|f| f(self)) else {
            logi!("pen: events dropped by the kernel (the reader was too slow); the device's state could not be read again");
            return;
        };
        self.state = st;
        let n = sample_of(&self.device, self.rotate, self.mirror, &self.state);
        self.account(&n, now_ns);
        if self.pending {
            self.stats.coalesced += 1;
        }
        self.deliver(&n, now_ns); // at once: the state read again replaces whatever was held
        logi!(
            "pen: events dropped by the kernel (the reader was too slow, {} time{}); the state was read again: {}",
            self.drops,
            plural(self.drops),
            if n.touch {
                "touching"
            } else if n.in_range {
                "in range"
            } else {
                "out of range"
            }
        );
    }

    /// The device's events read at `now_ns`.
    pub fn feed(&mut self, events: &[Event], now_ns: i64) {
        for v in events {
            if self.events == 0 {
                let name = if self.device.name.is_empty() { "the pen's device" } else { &self.device.name };
                logi!("pen: the first event from {}", name);
            }
            self.events += 1;
            if self.dropping {
                if v.kind == EV_SYN && v.code == SYN_REPORT {
                    self.dropping = false;
                    self.resync(now_ns);
                }
                continue;
            }
            match v.kind {
                EV_SYN if v.code == SYN_DROPPED => self.dropping = true,
                EV_SYN if v.code == SYN_REPORT => {
                    let sample = sample_of(&self.device, self.rotate, self.mirror, &self.state);
                    // The frame's own time (CLOCK_MONOTONIC, EVIOCSCLOCKID), so that a batch read late is spaced as it
                    // was made; the read's time when it is not within the last second (another clock: the held
                    // frame's due time is on tick's).
                    let own = v.time_ns > 0 && v.time_ns <= now_ns && now_ns - v.time_ns < 1_000_000_000;
                    self.frame(&sample, if own { v.time_ns } else { now_ns });
                }
                EV_KEY => {
                    let down = v.value != 0;
                    let st = &mut self.state;
                    match v.code {
                        BTN_TOOL_PEN => st.tool = down,
                        BTN_TOOL_RUBBER => st.rubber = down,
                        BTN_TOUCH => st.touch = down,
                        BTN_STYLUS => st.stylus_bits = if down { st.stylus_bits | 1 } else { st.stylus_bits & !1 },
                        BTN_STYLUS2 => st.stylus_bits = if down { st.stylus_bits | 2 } else { st.stylus_bits & !2 },
                        _ => {
                            // Another key of the pen's IC: Samsung's driver sends KEY_WAKEUP_UNLOCK and KEY_HOMEPAGE
                            // (and nothing of a pen) only in its screen-off survey mode (AOP), which One UI's `1,0` to
                            // /sys/class/sec/sec_epen/enabled puts it in: the IC is asleep for iOS. Never sent to the
                            // guest (no Home press from the pen). Said once, then counted.
                            if v.value != 0 {
                                if self.aop_keys == 0 {
                                    logi!(
                                        "pen: key {} from the pen's IC, not a pen key: its screen-off (AOP) mode, which \
                                         sends no positions (the IC asleep: the brightness bridge's `brightness: pen:` \
                                         line says whether it was woken); ignored",
                                        v.code
                                    );
                                }
                                self.aop_keys += 1;
                            }
                        }
                    }
                    self.state.stylus = self.state.stylus_bits != 0;
                }
                EV_SW if v.code == SW_PEN_INSERTED => {
                    // The pen's garage (Samsung's driver: 1 with the pen out). Rare: one line each time.
                    logi!(
                        "pen: SW_PEN_INSERT {}: the pen {}",
                        v.value,
                        if v.value != 0 { "taken out of its garage" } else { "put back in its garage" }
                    );
                }
                EV_ABS => {
                    if let Some(a) = AXIS_CODE.iter().position(|&c| c == v.code) {
                        self.state.value[a] = v.value;
                    }
                }
                _ => {}
            }
        }
    }
}

/// pen-rotate: `setting` None ("auto") from the digitizer's and the panel's aspect, else the number given; with why.
pub fn rotation(setting: Option<u32>, d: &Device, width: u32, height: u32) -> (u32, &'static str) {
    if let Some(r) = setting {
        return (r, "pen-rotate");
    }
    // auto: a digitizer whose frame has the panel's shape is taken as it is (0); one whose long side lies across the
    // panel's is turned a quarter (90, a guess for such a device: pen-rotate=270 and pen-mirror cover the others).
    // The S24 Ultra (SM-S928): Samsung's Wacom driver (drivers/input/sec_input/wacom, wacom_coord_handler) reports the
    // IC's own coordinates with no swap and no flip (it has no xy_switch or invert), and its device tree gives the IC
    // 7315 x 15687 (sec,max_coords; the IC's query at probe may replace them): the short side on x, as the 1440 x 3120
    // panel's (0.466 and 0.462). So auto is 0 there [F the driver's source and device tree; I that the IC's origin is
    // the panel's top left: the first phone session's stroke from the top left corner checks it].
    let (x, y) = (&d.axis[X], &d.axis[Y]);
    let (mut w, mut h) = (x.max as i64 - x.min as i64, y.max as i64 - y.min as i64);
    if x.res > 0 && y.res > 0 {
        w = w * 1000 / x.res as i64;
        h = h * 1000 / y.res as i64;
    }
    if (h >= w) == (height >= width) {
        (0, "auto: the digitizer and the panel are both portrait or both landscape")
    } else {
        (90, "auto: the digitizer's long side is across the panel's (90 is a guess: pen-rotate=270 if strokes come out turned)")
    }
}

/// The guest's pen device: its name, keys and fixed ranges.
pub fn guest_device(dev: &InputDev) {
    virtio_input::set_identity(
        dev,
        "OneiOS pen",
        "xnuhost-pen",
        &[BTN_TOOL_PEN, BTN_TOOL_RUBBER, BTN_TOUCH, BTN_STYLUS],
    );
    virtio_input::set_abs(dev, ABS_X as u32, 0, XY_MAX, 0);
    virtio_input::set_abs(dev, ABS_Y as u32, 0, XY_MAX, 0);
    virtio_input::set_abs(dev, ABS_PRESSURE as u32, 0, PRESSURE_MAX, 0);
    virtio_input::set_abs(dev, ABS_DISTANCE as u32, 0, DISTANCE_MAX, 0);
    // Degrees: no resolution, the guest's transport takes the values as degrees then.
    virtio_input::set_abs(dev, ABS_TILT_X as u32, -90, 90, 0);
    virtio_input::set_abs(dev, ABS_TILT_Y as u32, -90, 90, 0);
}

/// "x 0..N, y 0..N, pressure 0..N, ...; keys: ..." for the log.
pub fn describe(d: &Device) -> String {
    const NAMES: [&str; AXES] = ["x", "y", "pressure", "distance", "tilt x", "tilt y"];
    let axes: Vec<String> = d
        .axis
        .iter()
        .zip(NAMES)
        .map(|(a, name)| {
            if !a.present {
                return format!("no {name}");
            }
            let res = if a.res != 0 { format!(" (res {})", a.res) } else { String::new() };
            format!("{name} {}..{}{res}", a.min, a.max)
        })
        .collect();
    let mut text = format!(
        "{}; keys: tool pen{}, touch{}",
        axes.join(", "),
        if d.rubber { ", rubber" } else { "" },
        if d.stylus { ", stylus (the side button)" } else { ", no side button" }
    );
    // Samsung's driver: SW_PEN_INSERT 1 with the pen out of its garage [F its source; the first session's line checks it].
    if d.garage {
        text += &format!(
            "; SW_PEN_INSERT {} (Samsung: {})",
            d.pen_out as i32,
            if d.pen_out { "the pen out of its garage" } else { "the pen in its garage" }
        );
    }
    text
}

/// The device's state now, by ioctl on its descriptor (after SYN_DROPPED).
pub fn read_state<S>(s: &Source<S>) -> Option<State> {
    let fd = s.device.fd.as_ref()?.as_raw_fd();
    let mut key = [0u8; evdev::KEY_MAX / 8 + 1];
    if !evdev::keys(fd, &mut key) {
        return None;
    }
    let mut st = State {
        tool: byte_bit(&key, BTN_TOOL_PEN),
        rubber: byte_bit(&key, BTN_TOOL_RUBBER),
        touch: byte_bit(&key, BTN_TOUCH),
        stylus_bits: byte_bit(&key, BTN_STYLUS) as u32 | (byte_bit(&key, BTN_STYLUS2) as u32) << 1,
        ..State::default()
    };
    st.stylus = st.stylus_bits != 0;
    for (a, &code) in AXIS_CODE.iter().enumerate() {
        if s.device.axis[a].present {
            if let Some(info) = evdev::abs(fd, code) {
                st.value[a] = info.value;
            }
        }
    }
    Some(st)
}

/// The lookup: the pen found (its descriptor open) and a note on devices skipped (may be empty), or why none was.
pub type LookupFn = fn() -> Result<(Device, String), String>;

/// The default lookup: /dev/input/event0..63 by their bits, the best rank taken.
pub fn lookup_evdev() -> Result<(Device, String), String> {
    let mut best: Option<(u32, String, String, OwnedFd)> = None;
    let mut skipped = String::new();
    for i in 0..64 {
        let Some((path, fd)) = evdev::open(i) else { continue };
        let raw = fd.as_raw_fd();
        let (mut key, mut abs) = ([0u8; evdev::KEY_MAX / 8 + 1], [0u8; evdev::ABS_MAX / 8 + 1]);
        let name = evdev::name(raw);
        evdev::bits(raw, EV_KEY, &mut key);
        evdev::bits(raw, EV_ABS, &mut abs);
        match rank(&name, &key, &abs) {
            Ok(r) if best.as_ref().is_none_or(|b| r > b.0) => best = Some((r, path, name, fd)),
            Ok(_) => {}
            // Say why a device that looks like a pen by its bits or its name was not taken.
            Err(no) => {
                if (byte_bit(&key, BTN_TOOL_PEN) || name.contains("pen")) && skipped.is_empty() {
                    skipped = crate::input::clip(&format!("; {path} \"{name}\" not taken: {no}"), 159);
                }
            }
        }
    }
    let Some((_, path, name, fd)) = best else {
        return Err(format!(
            "no input device has a pen's bits (BTN_TOOL_PEN, ABS_X/Y, ABS_PRESSURE, no side keys){skipped}"
        ));
    };
    let raw = fd.as_raw_fd();
    let (mut key, mut abs) = ([0u8; evdev::KEY_MAX / 8 + 1], [0u8; evdev::ABS_MAX / 8 + 1]);
    evdev::bits(raw, EV_KEY, &mut key);
    evdev::bits(raw, EV_ABS, &mut abs);
    let mut d = Device {
        path,
        name,
        rubber: byte_bit(&key, BTN_TOOL_RUBBER),
        stylus: byte_bit(&key, BTN_STYLUS) || byte_bit(&key, BTN_STYLUS2),
        ..Device::default()
    };
    // The pen's garage switch, when it has one (Samsung's sec_e-pen: SW_PEN_INSERT, 1 with the pen out): for the log.
    let (mut sw, mut sw_now) = ([0u8; evdev::SW_MAX / 8 + 1], [0u8; evdev::SW_MAX / 8 + 1]);
    if evdev::bits(raw, EV_SW, &mut sw) && byte_bit(&sw, SW_PEN_INSERTED) && evdev::switches(raw, &mut sw_now) {
        d.garage = true;
        d.pen_out = byte_bit(&sw_now, SW_PEN_INSERTED);
    }
    for (a, &code) in AXIS_CODE.iter().enumerate() {
        if !byte_bit(&abs, code) {
            continue;
        }
        if let Some(info) = evdev::abs(raw, code) {
            d.axis[a] = Axis {
                min: info.minimum,
                max: info.maximum,
                res: info.resolution,
                present: info.maximum > info.minimum,
            };
        }
    }
    // Event times on CLOCK_MONOTONIC for this reader (a property of the open file): the log's in-range times.
    evdev::clock_monotonic(raw);
    d.fd = Some(fd);
    Ok((d, skipped.strip_prefix("; ").unwrap_or("").to_owned()))
}

static LOOKUP: AtomicUsize = AtomicUsize::new(0);

/// Replaces the lookup (another hardware backend's); None: the default, [`lookup_evdev`].
pub fn set_lookup(f: Option<LookupFn>) {
    LOOKUP.store(f.map_or(0, |f| f as usize), Ordering::Release);
}

/// Whether the lookup is the default one.
pub fn lookup_is_default() -> bool {
    LOOKUP.load(Ordering::Acquire) == 0
}

/// Finds the pen with the lookup in force.
pub fn lookup() -> Result<(Device, String), String> {
    let p = LOOKUP.load(Ordering::Acquire);
    if p == 0 {
        return lookup_evdev();
    }
    // SAFETY: LOOKUP only ever holds 0 or a `LookupFn` stored by set_lookup.
    let f: LookupFn = unsafe { std::mem::transmute::<usize, LookupFn>(p) };
    f()
}

#[cfg(test)]
mod tests;
