// SPDX-License-Identifier: MIT
//! evdev (linux/input.h): the event codes the runtime uses ([`codes`]), `struct input_event`, `input_absinfo` and
//! `ff_effect`, the EVIOC* ioctl numbers, and a safe call for each query xnuhost's input path, the haptics and
//! brightness bridges and bridgectl make. Bit arrays are byte arrays (little-endian longs are bytes in order).
//!
//! The calls take a [`RawFd`] the caller holds open, as [`crate::ioctl::ioctl`] does; a failed one leaves errno set
//! (callers log it).
use crate::ioctl::{ior, iow};
use std::io;
use std::os::fd::{FromRawFd, OwnedFd, RawFd};

/// Linux's event codes (linux/input-event-codes.h), the ones the runtime uses.
pub mod codes {
    pub const EV_SYN: u16 = 0;
    pub const EV_KEY: u16 = 1;
    pub const EV_REL: u16 = 2;
    pub const EV_ABS: u16 = 3;
    pub const EV_MSC: u16 = 4;
    pub const EV_SW: u16 = 5;
    pub const EV_FF: u16 = 0x15;
    pub const SYN_REPORT: u16 = 0;
    pub const SYN_DROPPED: u16 = 3;
    pub const MSC_TIMESTAMP: u16 = 5;
    pub const SW_PEN_INSERTED: u16 = 0x0f;
    pub const KEY_MUTE: u16 = 113;
    pub const KEY_VOLUMEDOWN: u16 = 114;
    pub const KEY_VOLUMEUP: u16 = 115;
    pub const KEY_POWER: u16 = 116;
    pub const KEY_HOMEPAGE: u16 = 172;
    pub const BTN_TOOL_PEN: u16 = 0x140;
    pub const BTN_TOOL_RUBBER: u16 = 0x141;
    pub const BTN_TOUCH: u16 = 0x14a;
    pub const BTN_STYLUS: u16 = 0x14b;
    pub const BTN_STYLUS2: u16 = 0x14c;
    pub const REL_X: u16 = 0;
    pub const REL_RX: u16 = 3;
    pub const REL_RY: u16 = 4;
    pub const ABS_X: u16 = 0;
    pub const ABS_Y: u16 = 1;
    pub const ABS_Z: u16 = 2;
    pub const ABS_RX: u16 = 3;
    pub const ABS_PRESSURE: u16 = 0x18;
    pub const ABS_DISTANCE: u16 = 0x19;
    pub const ABS_TILT_X: u16 = 0x1a;
    pub const ABS_TILT_Y: u16 = 0x1b;
    pub const ABS_MT_SLOT: u16 = 0x2f;
    pub const ABS_MT_TOUCH_MAJOR: u16 = 0x30;
    pub const ABS_MT_POSITION_X: u16 = 0x35;
    pub const ABS_MT_POSITION_Y: u16 = 0x36;
    pub const ABS_MT_TRACKING_ID: u16 = 0x39;
    pub const ABS_MT_PRESSURE: u16 = 0x3a;
    pub const FF_PERIODIC: u16 = 0x51;
    pub const FF_SINE: u16 = 0x5a;
    pub const FF_CUSTOM: u16 = 0x5d;
    pub const FF_GAIN: u16 = 0x60;
    pub const INPUT_PROP_DIRECT: u16 = 0x01;
    /// The highest code of each kind: a bit array of it is `MAX / 8 + 1` bytes.
    pub const ABS_MAX: usize = 0x3f;
    pub const KEY_MAX: usize = 0x2ff;
    pub const SW_MAX: usize = 0x10;
    pub const FF_MAX: usize = 0x7f;
}

/// `struct input_event` of a 64-bit process: a struct timeval, then type, code and value.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct InputEvent {
    pub sec: i64,
    pub usec: i64,
    pub kind: u16,
    pub code: u16,
    pub value: i32,
}

const _: () = assert!(std::mem::size_of::<InputEvent>() == std::mem::size_of::<libc::input_event>());
const _: () = assert!(std::mem::size_of::<InputEvent>() == 24);

/// `struct input_absinfo`
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct AbsInfo {
    pub value: i32,
    pub minimum: i32,
    pub maximum: i32,
    pub fuzz: i32,
    pub flat: i32,
    pub resolution: i32,
}

/// `struct ff_effect` with its union as `struct ff_periodic_effect`: 48 bytes on a 64-bit kernel.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct FfEffect {
    pub kind: u16,
    pub id: i16,
    pub direction: u16,
    pub trigger_button: u16,
    pub trigger_interval: u16,
    pub replay_length: u16,
    pub replay_delay: u16,
    pub _pad: u16,
    pub waveform: u16,
    pub period: u16,
    pub magnitude: i16,
    pub offset: i16,
    pub phase: u16,
    pub envelope: [u16; 4],
    pub custom_len: u32,
    pub custom_data: usize,
}

// ---- the ioctl numbers ----------------------------------------------------------------------------------------------
/// EVIOCGNAME(len)
pub const fn eviocgname(len: usize) -> u32 {
    ior(b'E', 0x06, len)
}
/// EVIOCGPROP(len)
pub const fn eviocgprop(len: usize) -> u32 {
    ior(b'E', 0x09, len)
}
/// EVIOCGMTSLOTS(len)
pub const fn eviocgmtslots(len: usize) -> u32 {
    ior(b'E', 0x0a, len)
}
/// EVIOCGKEY(len)
pub const fn eviocgkey(len: usize) -> u32 {
    ior(b'E', 0x18, len)
}
/// EVIOCGSW(len)
pub const fn eviocgsw(len: usize) -> u32 {
    ior(b'E', 0x1b, len)
}
/// EVIOCGBIT(ev, len)
pub const fn eviocgbit(ev: u16, len: usize) -> u32 {
    ior(b'E', 0x20 + ev as u8, len)
}
/// EVIOCGABS(axis)
pub const fn eviocgabs(axis: u16) -> u32 {
    ior(b'E', 0x40 + axis as u8, std::mem::size_of::<AbsInfo>())
}
pub const EVIOCSFF: u32 = iow(b'E', 0x80, std::mem::size_of::<FfEffect>());
pub const EVIOCGEFFECTS: u32 = ior(b'E', 0x84, std::mem::size_of::<libc::c_int>());
pub const EVIOCGRAB: u32 = iow(b'E', 0x90, std::mem::size_of::<libc::c_int>());
pub const EVIOCSCLOCKID: u32 = iow(b'E', 0xa0, std::mem::size_of::<libc::c_int>());

// ---- the calls ------------------------------------------------------------------------------------------------------
/// An ioctl that writes or reads at most the size its request encodes.
fn call<T>(fd: RawFd, request: u32, arg: *mut T) -> io::Result<libc::c_int> {
    // SAFETY: each caller passes an argument of the size and layout its request encodes, live across the call.
    unsafe { crate::ioctl::ioctl(fd, request, arg) }
}

/// Bit `n` of a bit array (false past its end).
pub fn bit(bits: &[u8], n: usize) -> bool {
    n / 8 < bits.len() && bits[n / 8] >> (n % 8) & 1 != 0
}

/// EVIOCGNAME with a buffer of `len` bytes (at most 256): the name up to its NUL, None when the call fails.
pub fn name(fd: RawFd, len: usize) -> Option<String> {
    let mut buf = [0u8; 256];
    let buf = &mut buf[..len];
    call(fd, eviocgname(len), buf.as_mut_ptr()).ok()?;
    Some(crate::text::nul_text(buf))
}

/// EVIOCGBIT(ev): the event type's code bits.
pub fn bits(fd: RawFd, ev: u16, out: &mut [u8]) -> bool {
    call(fd, eviocgbit(ev, out.len()), out.as_mut_ptr()).is_ok()
}

/// EVIOCGPROP: the device's property bits.
pub fn props(fd: RawFd, out: &mut [u8]) -> bool {
    call(fd, eviocgprop(out.len()), out.as_mut_ptr()).is_ok()
}

/// EVIOCGKEY: the keys held.
pub fn keys(fd: RawFd, out: &mut [u8]) -> bool {
    call(fd, eviocgkey(out.len()), out.as_mut_ptr()).is_ok()
}

/// EVIOCGSW: the switches' state.
pub fn switches(fd: RawFd, out: &mut [u8]) -> bool {
    call(fd, eviocgsw(out.len()), out.as_mut_ptr()).is_ok()
}

/// EVIOCGABS(axis)
pub fn abs(fd: RawFd, axis: u16) -> Option<AbsInfo> {
    let mut info = AbsInfo::default();
    call(fd, eviocgabs(axis), &mut info).ok().map(|_| info)
}

/// EVIOCGMTSLOTS for one code: the values of the first 16 slots.
pub fn mt_slots(fd: RawFd, code: u16) -> Option<[i32; 16]> {
    let mut slots = [0i32; 17];
    slots[0] = code as i32;
    call(fd, eviocgmtslots(std::mem::size_of_val(&slots)), slots.as_mut_ptr()).ok()?;
    let mut v = [0i32; 16];
    v.copy_from_slice(&slots[1..]);
    Some(v)
}

/// EVIOCGEFFECTS: how many effects the device holds at once.
pub fn effects(fd: RawFd) -> Option<i32> {
    let mut n: libc::c_int = -1;
    call(fd, EVIOCGEFFECTS, &mut n).ok().map(|_| n)
}

/// EVIOCSFF: uploads (or, with its id, updates) an effect; the kernel writes the id it got into `effect`.
pub fn upload(fd: RawFd, effect: &mut FfEffect) -> io::Result<()> {
    call(fd, EVIOCSFF, effect).map(drop)
}

/// EVIOCGRAB: this reader alone gets the device's events (`on`), or no longer.
pub fn grab(fd: RawFd, on: bool) -> bool {
    // SAFETY: EVIOCGRAB takes its argument by value.
    unsafe { crate::ioctl::ioctl(fd, EVIOCGRAB, usize::from(on) as *mut libc::c_void) }.is_ok()
}

/// EVIOCSCLOCKID: this reader's event times on CLOCK_MONOTONIC.
pub fn clock_monotonic(fd: RawFd) -> bool {
    let mut clock: libc::c_int = libc::CLOCK_MONOTONIC;
    call(fd, EVIOCSCLOCKID, &mut clock).is_ok()
}

/// /dev/input/eventN, opened read only and non-blocking: its path and descriptor.
pub fn open(n: u32) -> Option<(String, OwnedFd)> {
    let path = format!("/dev/input/event{n}");
    let c = std::ffi::CString::new(path.as_str()).ok()?;
    // SAFETY: a NUL-terminated path.
    let fd = unsafe { libc::open(c.as_ptr(), libc::O_RDONLY | libc::O_CLOEXEC | libc::O_NONBLOCK) };
    // SAFETY: a new descriptor of ours.
    (fd >= 0).then(|| (path, unsafe { OwnedFd::from_raw_fd(fd) }))
}

/// Up to `out.len()` events (one read), or None when there were none or the read failed.
pub fn read(fd: RawFd, out: &mut [InputEvent]) -> Option<usize> {
    // SAFETY: reads into our array of plain structs, at most its size.
    let n = unsafe { libc::read(fd, out.as_mut_ptr().cast(), std::mem::size_of_val(out)) };
    (n > 0).then(|| n as usize / std::mem::size_of::<InputEvent>())
}

/// Writes one event (an EV_FF play or gain); a short write is the errno that is left.
pub fn write(fd: RawFd, ev: &InputEvent) -> io::Result<()> {
    let size = std::mem::size_of::<InputEvent>();
    // SAFETY: `ev` is a valid input_event of `size` bytes.
    let n = unsafe { libc::write(fd, (ev as *const InputEvent).cast(), size) };
    if n == size as isize {
        Ok(())
    } else {
        Err(io::Error::last_os_error())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::fd::AsRawFd;

    #[test]
    fn numbers_and_layouts() {
        // Values from linux/input.h on a 64-bit kernel.
        assert_eq!(eviocgname(80), 0x8050_4506);
        assert_eq!(eviocgprop(8), 0x8008_4509);
        assert_eq!(eviocgmtslots(68), 0x8044_450a);
        assert_eq!(eviocgkey(96), 0x8060_4518);
        assert_eq!(eviocgsw(3), 0x8003_451b);
        assert_eq!(eviocgbit(codes::EV_FF, 16), 0x8010_4535);
        assert_eq!(eviocgabs(codes::ABS_MT_SLOT), 0x8018_456f);
        assert_eq!(EVIOCSFF, 0x4030_4580);
        assert_eq!(EVIOCGEFFECTS, 0x8004_4584);
        assert_eq!(EVIOCGRAB, 0x4004_4590);
        assert_eq!(EVIOCSCLOCKID, 0x4004_45a0);
        assert_eq!(std::mem::size_of::<FfEffect>(), 48);
        assert_eq!(std::mem::size_of::<AbsInfo>(), 24);
        assert!(bit(&[0, 0x80], 15) && !bit(&[0, 0x80], 14) && !bit(&[0xff], 8));
    }

    #[test]
    fn calls_on_a_file_that_is_not_a_device() {
        // /dev/null takes none of them: every query says so, none panics.
        let f = std::fs::File::open("/dev/null").unwrap();
        let fd = f.as_raw_fd();
        let mut b = [0u8; 8];
        assert_eq!(name(fd, 80), None);
        assert!(!bits(fd, codes::EV_ABS, &mut b) && !props(fd, &mut b) && !keys(fd, &mut b) && !switches(fd, &mut b));
        assert_eq!((abs(fd, 0), mt_slots(fd, 0), effects(fd)), (None, None, None));
        assert!(!grab(fd, true) && !clock_monotonic(fd));
        assert!(upload(fd, &mut FfEffect::default()).is_err());
        assert_eq!(read(fd, &mut [InputEvent::default(); 4]), None);
        // An event written to a pipe reads back the same.
        let mut p = [0; 2];
        // SAFETY: pipe writes two descriptors into the array.
        assert_eq!(unsafe { libc::pipe(p.as_mut_ptr()) }, 0);
        // SAFETY: two new descriptors of ours.
        let (r, w) = unsafe { (OwnedFd::from_raw_fd(p[0]), OwnedFd::from_raw_fd(p[1])) };
        let ev = InputEvent { sec: 1, usec: 2, kind: codes::EV_FF, code: codes::FF_GAIN, value: 0xffff };
        write(w.as_raw_fd(), &ev).unwrap();
        let mut out = [InputEvent::default(); 4];
        assert_eq!(read(r.as_raw_fd(), &mut out), Some(1));
        assert_eq!(out[0], ev);
    }
}
