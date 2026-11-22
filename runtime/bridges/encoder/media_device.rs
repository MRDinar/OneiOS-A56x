// SPDX-License-Identifier: MIT
//! "droidvm,virtio-media": xnuhost's stateful V4L2 video encoder device (docs/hardware.md, "Video encoder"). It is the
//! host end of the V4L2 codec UniXNU-droidvm's DroidVMVTHost.cpp drives for iOS's paravirtualized VideoToolbox
//! encoder: OUTPUT is tightly packed NV12 frames, CAPTURE is the HEVC/H.264 bitstream (Annex-B). The real encoding is
//! a backend ([`crate::backend`]): Android's MediaCodec on the phone, a stand-in on the PC.
//!
//! - commandq (0): OPEN/CLOSE/IOCTL/MMAP/MUNMAP, answered on the vCPU thread ([`Device::handle`], the device lock
//!   held).
//! - eventq (1, manual): the guest posts writable event buffers; a worker thread fills one per event (DQBUF of a
//!   returned OUTPUT buffer or a coded CAPTURE buffer, an EOS EVENT).
//!
//! The worker asks the backend for coded frames (a backend has no callback). It does so every millisecond only for
//! [`MEDIA_BUSY_MS`] after the guest's last command or the backend's last frame, every [`MEDIA_IDLE_MS`] while a
//! session is open and quiet, and not at all with no session and no event waiting: then it sleeps until a command or a
//! kick wakes it (a device nobody uses costs no wake-ups; before 2026-11-15 it woke 50 times a second for ever).
//!
//! Each virtio-media session gets a fixed slice of the shared media pool for its MMAP'd buffers ([`MEDIA_SLOTS`]
//! slots), so the allocator is a per-slot bump with no fragmentation. A session that will not fit is refused at
//! REQBUFS and VideoToolbox keeps its software encoder.
use crate::backend::{Backend, Session};
use crate::pod::Pod;
use crate::proto::*;
use std::cell::RefCell;
use std::collections::VecDeque;
use std::ptr::NonNull;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex, PoisonError, Weak};
use std::time::Duration;
use xh_core::bytes::le;
use xh_core::time::monotonic_ms;
use xh_core::virtio::{Device, Info, Viov, Virtio};
use xh_core::{loge, logi};

/// concurrent sessions (chronod snapshots are near-serial)
pub const MEDIA_SLOTS: usize = 2;
const MEDIA_OUTPUT_BUFS: u32 = 4;
const MEDIA_CAPTURE_BUFS: u32 = 8;
const MEDIA_CODED_HEADROOM: u64 = 65536;
const MEDIA_EVENT_FIFO: usize = 64;
/// the backend is asked every millisecond this long after the last activity
pub const MEDIA_BUSY_MS: u64 = 2000;
/// then this often, while a session is open
pub const MEDIA_IDLE_MS: u64 = 50;
const EVENT_BODY: usize = MEDIA_EVENT_BYTES as usize - 8;

/// Counters for the host rig and encctl (`br_encoder_stats`; monotonic).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Stats {
    pub sessions: u64,
    pub frames_in: u64,
    pub frames_out: u64,
    pub errors: u64,
}

#[derive(Clone, Copy, Debug, Default)]
struct Buf {
    /// into the pool (and the mmap handle)
    offset: u64,
    size: u64,
    /// on the device's side of the V4L2 queue
    queued: bool,
}

#[derive(Default)]
struct MediaSession {
    used: bool,
    id: u32,
    /// the session's pool slice
    slot_base: u64,
    slot_size: u64,
    bump: u64,
    /// codec from the CAPTURE format, size from OUTPUT
    codec: u32,
    width: u32,
    height: u32,
    output_on: bool,
    capture_on: bool,
    draining: bool,
    output: Vec<Buf>,
    capture: Vec<Buf>,
    enc: Option<Box<dyn Session>>,
}

impl MediaSession {
    /// A buffer out of the session's slot; None when the slot is full.
    fn slot_alloc(&mut self, size: u64) -> Option<u64> {
        let aligned = (size + 0xfff) & !0xfff;
        if self.bump + aligned > self.slot_size {
            return None;
        }
        let at = self.slot_base + self.bump;
        self.bump += aligned;
        Some(at)
    }

    fn buffers(&mut self, kind: u32) -> Option<&mut Vec<Buf>> {
        match kind {
            MEDIA_BUF_TYPE_VIDEO_OUTPUT_MPLANE => Some(&mut self.output),
            MEDIA_BUF_TYPE_VIDEO_CAPTURE_MPLANE => Some(&mut self.capture),
            _ => None,
        }
    }
}

/// A pending event, filled into a guest event buffer by the worker.
struct Event {
    kind: u32,
    session: u32,
    body: [u8; EVENT_BODY],
    body_len: usize,
}

/// The host mapping of the media pool: guest memory the MMAP'd buffers live in.
struct Pool {
    host: NonNull<u8>,
    size: u64,
}

// SAFETY: the pool is guest RAM mapped for the process's life; it is touched only with the device lock held, on a
// buffer the guest has handed over (a queued OUTPUT buffer, a queued CAPTURE buffer).
unsafe impl Send for Pool {}

impl Pool {
    /// `[offset, offset + size)` of the pool, if it is inside it.
    fn at(&self, offset: u64, size: u64) -> Option<*mut u8> {
        // SAFETY: inside the pool (checked).
        (offset.checked_add(size)? <= self.size).then(|| unsafe { self.host.as_ptr().add(offset as usize) })
    }
}

struct State {
    sessions: [MediaSession; MEDIA_SLOTS],
    fifo: VecDeque<Event>,
    next_session: u32,
    /// the worker asks the backend every millisecond until then
    busy_until_ms: u64,
    stats: Stats,
}

/// The worker's sleep: `wake` under its own lock (not the device's).
#[derive(Default)]
struct Wake {
    wake: Mutex<bool>,
    cond: Condvar,
    stop: AtomicBool,
}

impl Wake {
    /// Something for the worker: a command or a kick of the guest's (the device lock held or not).
    fn wake(&self) {
        *self.wake.lock().unwrap_or_else(PoisonError::into_inner) = true;
        self.cond.notify_one();
    }

    /// The worker's sleep with nothing in flight: until it is woken, or `ms` at most (0: no limit).
    fn sleep(&self, ms: u64) {
        let mut woken = self.wake.lock().unwrap_or_else(PoisonError::into_inner);
        if !*woken {
            woken = if ms != 0 {
                self.cond.wait_timeout(woken, Duration::from_millis(ms)).unwrap_or_else(PoisonError::into_inner).0
            } else {
                self.cond.wait(woken).unwrap_or_else(PoisonError::into_inner)
            };
        }
        *woken = false;
    }
}

/// The device model (`media_state`).
pub struct MediaDevice {
    backend: Arc<dyn Backend>,
    pool: Pool,
    slot_size: u64,
    state: RefCell<State>,
    wake: Arc<Wake>,
}

pub type MediaVirtio = Virtio<MediaDevice>;

impl State {
    fn session(&mut self, id: u32) -> Option<&mut MediaSession> {
        self.sessions.iter_mut().find(|s| s.used && s.id == id)
    }

    fn push_event(&mut self, kind: u32, session: u32, body: &[u8]) {
        if self.fifo.len() >= MEDIA_EVENT_FIFO {
            self.stats.errors += 1;
            return;
        }
        let body_len = body.len().min(EVENT_BODY);
        let mut e = Event { kind, session, body: [0; EVENT_BODY], body_len };
        e.body[..body_len].copy_from_slice(&body[..body_len]);
        self.fifo.push_back(e);
    }
}

/// A DQBUF event: a struct v4l2_buffer (+ one plane) for the returned buffer.
fn push_dqbuf(st: &mut State, slot: usize, kind: u32, index: usize, bytesused: u32, flags: u32, sec: i64) {
    let s = &st.sessions[slot];
    let size = if kind == MEDIA_BUF_TYPE_VIDEO_CAPTURE_MPLANE { s.capture[index].size } else { s.output[index].size };
    let mut b = MediaV4l2Buffer {
        index: index as u32,
        type_: kind,
        memory: MEDIA_MEMORY_MMAP,
        flags,
        sec,
        length: 1,
        ..Default::default()
    };
    b.plane.bytesused = bytesused;
    b.plane.length = size as u32;
    let id = s.id;
    st.push_event(MEDIA_EVT_DQBUF, id, b.as_bytes());
}

impl MediaDevice {
    /// A session's backend is opened when both queues are streaming and the codec and size are known.
    fn maybe_open_encoder(&self, st: &mut State, slot: usize) {
        let s = &mut st.sessions[slot];
        if s.enc.is_some() || !s.output_on || !s.capture_on || s.codec == 0 || s.width == 0 || s.height == 0 {
            return;
        }
        s.enc = self.backend.open(s.codec, s.width, s.height);
        let (id, codec, w, h) = (s.id, s.codec, s.width, s.height);
        if s.enc.is_none() {
            st.stats.errors += 1;
            loge!("encoder: backend {} refused {w}x{h}", self.backend.name());
        } else {
            st.stats.sessions += 1;
            let name = if codec == MEDIA_PIXFMT_HEVC { "HEVC" } else { "H.264" };
            logi!("encoder: session {id}: {name} {w}x{h} on {}", self.backend.name());
        }
    }

    /// IOCTL on a session: `nr` the _IOC_NR, `payload` the structure the guest sent (`len` valid bytes), changed in
    /// place for the bytes written back. A Linux errno (0 on success).
    fn ioctl(&self, st: &mut State, slot: usize, nr: u32, payload: &mut [u8], len: usize) -> u32 {
        let s = &mut st.sessions[slot];
        match nr {
            MEDIA_VIDIOC_S_FMT => {
                if len < std::mem::size_of::<MediaV4l2Format>() {
                    return libc::EINVAL as u32;
                }
                let mut f = MediaV4l2Format::read_from(payload);
                if f.type_ == MEDIA_BUF_TYPE_VIDEO_OUTPUT_MPLANE {
                    if f.pixelformat != MEDIA_PIXFMT_NV12 || f.width == 0 || f.height == 0 {
                        return libc::EINVAL as u32;
                    }
                    (s.width, s.height) = (f.width, f.height);
                    f.num_planes = 1;
                    f.planes[0].bytesperline = f.width;
                    f.planes[0].sizeimage = f.width.wrapping_mul(f.height).wrapping_mul(3) / 2;
                } else if f.type_ == MEDIA_BUF_TYPE_VIDEO_CAPTURE_MPLANE {
                    if f.pixelformat != MEDIA_PIXFMT_HEVC && f.pixelformat != MEDIA_PIXFMT_H264 {
                        return libc::EINVAL as u32;
                    }
                    s.codec = f.pixelformat;
                    f.num_planes = 1;
                    if f.planes[0].sizeimage == 0 {
                        f.planes[0].sizeimage = f.width.wrapping_mul(f.height) / 2 + MEDIA_CODED_HEADROOM as u32;
                    }
                } else {
                    return libc::EINVAL as u32;
                }
                f.write_to(payload);
                0
            }
            MEDIA_VIDIOC_REQBUFS => {
                if len < 20 {
                    return libc::EINVAL as u32;
                }
                let mut r = <[u32; 5]>::read_from(payload);
                let (want, kind) = (r[0], r[1]);
                let (cap, size) = match kind {
                    MEDIA_BUF_TYPE_VIDEO_OUTPUT_MPLANE => {
                        (MEDIA_OUTPUT_BUFS, u64::from(s.width) * u64::from(s.height) * 3 / 2)
                    }
                    MEDIA_BUF_TYPE_VIDEO_CAPTURE_MPLANE => {
                        (MEDIA_CAPTURE_BUFS, u64::from(s.width) * u64::from(s.height) / 2 + MEDIA_CODED_HEADROOM)
                    }
                    _ => return libc::EINVAL as u32,
                };
                let want = want.min(cap);
                let mut bufs = Vec::with_capacity(want as usize);
                for _ in 0..want {
                    let Some(offset) = s.slot_alloc(size) else {
                        st.stats.errors += 1;
                        return libc::ENOMEM as u32; // the slot is too small: VT keeps software
                    };
                    bufs.push(Buf { offset, size, queued: false });
                }
                *s.buffers(kind).unwrap() = bufs;
                r[0] = want;
                r.write_to(payload);
                0
            }
            MEDIA_VIDIOC_QUERYBUF => {
                if len < std::mem::size_of::<MediaV4l2Buffer>() {
                    return libc::EINVAL as u32;
                }
                let mut b = MediaV4l2Buffer::read_from(payload);
                let bufs = if b.type_ == MEDIA_BUF_TYPE_VIDEO_OUTPUT_MPLANE { &s.output } else { &s.capture };
                let Some(buf) = bufs.get(b.index as usize) else { return libc::EINVAL as u32 };
                b.length = 1;
                b.plane.length = buf.size as u32;
                b.plane.m = buf.offset; // the mmap handle (MMAP turns it into a place in the pool)
                b.write_to(payload);
                0
            }
            MEDIA_VIDIOC_QBUF => {
                if len < std::mem::size_of::<MediaV4l2Buffer>() {
                    return libc::EINVAL as u32;
                }
                let b = MediaV4l2Buffer::read_from(payload);
                let index = b.index as usize;
                if b.type_ == MEDIA_BUF_TYPE_VIDEO_OUTPUT_MPLANE {
                    let (Some(buf), Some(enc)) = (s.output.get(index), s.enc.as_mut()) else {
                        return libc::EINVAL as u32;
                    };
                    let used = u64::from(b.plane.bytesused);
                    let rc = match self.pool.at(buf.offset, used) {
                        Some(at) => {
                            // SAFETY: a queued OUTPUT buffer inside the pool: the guest does not touch it until DQBUF.
                            let nv12 = unsafe { std::slice::from_raw_parts(at, used as usize) };
                            enc.feed(nv12, b.sec * 1_000_000, false)
                        }
                        None => Err(libc::EINVAL),
                    };
                    if let Err(e) = rc {
                        st.stats.errors += 1;
                        return e as u32;
                    }
                    st.stats.frames_in += 1;
                    // MediaCodec copies the input; the OUTPUT buffer is free again at once.
                    push_dqbuf(st, slot, MEDIA_BUF_TYPE_VIDEO_OUTPUT_MPLANE, index, 0, 0, b.sec);
                } else if b.type_ == MEDIA_BUF_TYPE_VIDEO_CAPTURE_MPLANE {
                    let Some(buf) = s.capture.get_mut(index) else { return libc::EINVAL as u32 };
                    buf.queued = true; // free for the worker to fill
                } else {
                    return libc::EINVAL as u32;
                }
                b.write_to(payload);
                0
            }
            MEDIA_VIDIOC_STREAMON | MEDIA_VIDIOC_STREAMOFF => {
                if len < 4 {
                    return libc::EINVAL as u32;
                }
                let on = nr == MEDIA_VIDIOC_STREAMON;
                match le::u32(payload, 0) {
                    MEDIA_BUF_TYPE_VIDEO_OUTPUT_MPLANE => s.output_on = on,
                    MEDIA_BUF_TYPE_VIDEO_CAPTURE_MPLANE => s.capture_on = on,
                    _ => return libc::EINVAL as u32,
                }
                if on {
                    self.maybe_open_encoder(st, slot);
                }
                0
            }
            MEDIA_VIDIOC_SUBSCRIBE_EVENT => 0, // EOS / SOURCE_CHANGE: accepted, delivered on the eventq
            MEDIA_VIDIOC_ENCODER_CMD => {
                if len < 4 {
                    return libc::EINVAL as u32;
                }
                if le::u32(payload, 0) == MEDIA_ENC_CMD_STOP && !s.draining {
                    if let Some(enc) = s.enc.as_mut() {
                        s.draining = true;
                        let _ = enc.drain();
                    }
                }
                0
            }
            MEDIA_VIDIOC_ENUM_FRAMESIZES => libc::EINVAL as u32, // encoder: let VideoToolbox decide the size
            MEDIA_VIDIOC_ENUM_FMT => {
                // struct v4l2_fmtdesc: index, type, flags, description[32], pixelformat (at 44). The guest's
                // transport lists each queue's formats once at start (DroidVMVirtioMedia.c check(), up to 16 an
                // answer of 0 gives it): OUTPUT takes NV12, CAPTURE gives HEVC and H.264; past the list EINVAL ends
                // its loop, as V4L2 says.
                if len < 48 {
                    return libc::EINVAL as u32;
                }
                let format = match (le::u32(payload, 4), le::u32(payload, 0)) {
                    (MEDIA_BUF_TYPE_VIDEO_OUTPUT_MPLANE, 0) => MEDIA_PIXFMT_NV12,
                    (MEDIA_BUF_TYPE_VIDEO_CAPTURE_MPLANE, 0) => MEDIA_PIXFMT_HEVC,
                    (MEDIA_BUF_TYPE_VIDEO_CAPTURE_MPLANE, 1) => MEDIA_PIXFMT_H264,
                    _ => return libc::EINVAL as u32,
                };
                payload[44..48].copy_from_slice(&format.to_le_bytes());
                0
            }
            MEDIA_VIDIOC_G_FMT | MEDIA_VIDIOC_G_CTRL => 0, // not needed by the encoder path
            _ => libc::ENOTTY as u32,
        }
    }

    /// Flushes the event FIFO into the guest's posted event buffers (the device lock held).
    fn flush_events(&self, dev: &MediaVirtio, st: &mut State) {
        while let Some(e) = st.fifo.front() {
            let mut iov = [Viov::EMPTY; 2];
            let Some((head, _)) = dev.pop(MEDIA_QUEUE_EVENT, &mut iov) else { break }; // no guest buffer: keep it
            let part = &iov[0];
            let mut written = 0;
            if part.writable() && part.len() >= 8 {
                part.write_at(0, &e.kind.to_le_bytes());
                part.write_at(4, &e.session.to_le_bytes());
                let body = e.body_len.min(part.len() as usize - 8);
                part.write_at(8, &e.body[..body]);
                written = 8 + body as u32;
            }
            dev.push(MEDIA_QUEUE_EVENT, head, written);
            st.fifo.pop_front();
        }
    }

    /// Pulls coded access units out of each streaming session's backend and turns them into CAPTURE DQBUF events.
    fn pump(&self, st: &mut State) {
        for slot in 0..MEDIA_SLOTS {
            loop {
                let s = &mut st.sessions[slot];
                if !s.used || !s.capture_on || s.enc.is_none() {
                    break;
                }
                let Some(index) = s.capture.iter().position(|b| b.queued) else { break }; // none free; next poll
                let buf = s.capture[index];
                let Some(dst) = self.pool.at(buf.offset, buf.size) else { break };
                // SAFETY: a queued CAPTURE buffer inside the pool: the guest does not touch it until DQBUF.
                let dst = unsafe { std::slice::from_raw_parts_mut(dst, buf.size as usize) };
                let polled = match s.enc.as_mut().unwrap().poll(dst) {
                    Ok(p) => p,
                    Err(_) => {
                        st.stats.errors += 1;
                        break;
                    }
                };
                if polled.bytes == 0 && !polled.eos {
                    break; // nothing ready yet
                }
                let flags = if polled.key { MEDIA_BUF_FLAG_KEYFRAME } else { 0 }
                    | if polled.eos { MEDIA_BUF_FLAG_LAST } else { 0 };
                s.capture[index].queued = false;
                let id = s.id;
                push_dqbuf(
                    st,
                    slot,
                    MEDIA_BUF_TYPE_VIDEO_CAPTURE_MPLANE,
                    index,
                    polled.bytes as u32,
                    flags,
                    polled.pts_us / 1_000_000,
                );
                st.busy_until_ms = monotonic_ms() + MEDIA_BUSY_MS; // the backend is giving: the next may follow at once
                if polled.bytes > 0 {
                    st.stats.frames_out += 1;
                }
                if polled.eos {
                    let mut ev = [0u32; 16];
                    ev[0] = MEDIA_EVENT_EOS;
                    st.push_event(MEDIA_EVT_EVENT, id, ev.as_bytes());
                    st.sessions[slot].draining = false;
                    break;
                }
            }
        }
    }

    /// One turn of the worker: pump and flush. Whether a session is open or an event waits, and whether the busy
    /// time runs.
    fn work(&self, dev: &MediaVirtio) -> (bool, bool) {
        let mut st = self.state.borrow_mut();
        self.pump(&mut st);
        self.flush_events(dev, &mut st);
        // Something may still come without a command: a session's backend may hold a frame, an event may wait for a
        // buffer.
        let open = !st.fifo.is_empty() || st.sessions.iter().any(|s| s.used);
        (open, open && monotonic_ms() < st.busy_until_ms)
    }

    fn command(&self, reads: &[&Viov], writes: &[&Viov]) -> Option<u32> {
        let mut cmd = [0u8; 20];
        let cmd_len = reads[0].len() as usize;
        reads[0].read_at(0, &mut cmd);
        let op = le::u32(&cmd, 0);
        let mut st = self.state.borrow_mut();
        st.busy_until_ms = monotonic_ms() + MEDIA_BUSY_MS; // a command: the worker looks at once, and often for a while
        self.wake.wake();
        // CLOSE comes with no buffer for an answer (DroidVMVirtioMedia.c droidvm_media_close sends the command
        // alone), so it is done before the answer's buffer is asked for. Until 2026-11-15 a command without one was
        // dropped: no session was ever closed, and the device's MEDIA_SLOTS sessions were used up by the guest's
        // start-up check and the first encode.
        if op == MEDIA_CMD_CLOSE {
            if cmd_len >= 12 {
                if let Some(s) = st.session(le::u32(&cmd, 8)) {
                    s.enc = None; // closes the backend's session
                    s.used = false;
                }
            }
            let resp = writes.first().filter(|w| w.len() >= 8)?;
            resp.fill(0, 0, 8);
            return Some(8);
        }
        let resp = *writes.first()?;
        let cap = resp.len() as usize;
        if cap < 8 {
            return None;
        }
        resp.fill(0, 0, cap);
        let status = |code: u32| {
            resp.write_at(0, &code.to_le_bytes());
        };
        let written = match op {
            MEDIA_CMD_OPEN => {
                if cap < 16 {
                    8
                } else if let Some(slot) = st.sessions.iter().position(|s| !s.used) {
                    st.next_session += 1;
                    let id = st.next_session;
                    st.sessions[slot] = MediaSession {
                        used: true,
                        id,
                        slot_base: slot as u64 * self.slot_size,
                        slot_size: self.slot_size,
                        ..Default::default()
                    };
                    resp.write_at(8, &id.to_le_bytes());
                    16
                } else {
                    status(libc::ENOMEM as u32);
                    16
                }
            }
            MEDIA_CMD_IOCTL if cmd_len >= 16 => {
                let slot = st.sessions.iter().position(|s| s.used && s.id == le::u32(&cmd, 8));
                let nr = le::u32(&cmd, 12);
                let mut scratch = [0u8; std::mem::size_of::<MediaV4l2Format>()];
                let len = reads.get(1).map_or(0, |out| out.read_at(0, &mut scratch));
                let back = writes.get(1).map_or(0, |inp| (inp.len() as usize).min(scratch.len()));
                let code = match slot {
                    Some(slot) => self.ioctl(&mut st, slot, nr, &mut scratch, len),
                    None => libc::ENODEV as u32,
                };
                status(code);
                if let Some(inp) = writes.get(1) {
                    if code == 0 {
                        inp.write_at(0, &scratch[..back]);
                    }
                }
                8 + back as u32
            }
            MEDIA_CMD_MMAP if cmd_len >= 20 && cap >= 24 => {
                let offset = u64::from(le::u32(&cmd, 16));
                let found = st.session(le::u32(&cmd, 8)).and_then(|s| {
                    s.output.iter().chain(s.capture.iter()).find(|b| b.offset == offset).map(|b| (offset, b.size))
                });
                // The answer is the buffer's place in the pool, not its address: the guest adds its media-host-pool
                // base (DroidVMVirtioMedia.c droidvm_media_mmap: `at > pool_size` is refused, then gpa = pool_base
                // + at). Until 2026-11-15 the pool's guest address was added here too, which the guest's bound
                // refused for every buffer.
                let (at, size) = found.unwrap_or((0, 0));
                status(if size != 0 { 0 } else { libc::EINVAL as u32 });
                resp.write_at(8, &at.to_le_bytes());
                resp.write_at(16, &size.to_le_bytes());
                24
            }
            MEDIA_CMD_IOCTL | MEDIA_CMD_MMAP | MEDIA_CMD_MUNMAP => 8,
            _ => {
                status(libc::ENOTTY as u32);
                8
            }
        };
        Some(written)
    }

    /// The counters (`br_encoder_get_stats`).
    pub fn stats(&self) -> Stats {
        self.state.borrow().stats
    }
}

impl Device for MediaDevice {
    fn handle(&self, _dev: &MediaVirtio, queue: u32, _head: u16, iov: &[Viov]) -> Option<u32> {
        if queue != MEDIA_QUEUE_COMMAND {
            return Some(0);
        }
        let reads: Vec<&Viov> = iov.iter().filter(|v| !v.writable()).take(4).collect();
        let writes: Vec<&Viov> = iov.iter().filter(|v| v.writable()).take(4).collect();
        if reads.first().is_none_or(|r| r.len() < 8) {
            return Some(0);
        }
        Some(self.command(&reads, &writes).unwrap_or(0))
    }

    fn config_read(&self, _dev: &MediaVirtio, offset: u32, _size: u32) -> u32 {
        match offset {
            0 => MEDIA_CAP_VIDEO_M2M_MPLANE | MEDIA_CAP_STREAMING, // device_caps
            4 => 1,                                                // device_type: 1 = encoder (host rig)
            8..40 => {
                // card: "oneios-encoder"
                let mut card = [0u8; 36];
                card[..14].copy_from_slice(b"oneios-encoder");
                let at = (offset - 8) as usize;
                le::u32(&card, at)
            }
            _ => 0,
        }
    }

    /// eventq: the guest gave event buffers back; the worker fills them (an event may have waited for one).
    fn kick(&self, _dev: &MediaVirtio, _queue: u32) {
        self.wake.wake();
    }

    fn reset(&self, _dev: &MediaVirtio) {
        let mut st = self.state.borrow_mut();
        for s in st.sessions.iter_mut().filter(|s| s.used) {
            s.enc = None;
            s.used = false;
        }
        st.fifo.clear();
    }
}

impl Drop for MediaDevice {
    fn drop(&mut self) {
        // The worker holds the device weakly: it ends at its next wake-up.
        self.wake.stop.store(true, Ordering::Release);
        self.wake.wake();
    }
}

fn worker(dev: Weak<MediaVirtio>, wake: Arc<Wake>) {
    while !wake.stop.load(Ordering::Acquire) {
        let Some(dev) = dev.upgrade() else { break };
        let (open, busy) = {
            let guard = dev.lock();
            guard.work(&dev)
        };
        drop(dev);
        if busy {
            std::thread::sleep(Duration::from_millis(1));
        } else {
            wake.sleep(if open { MEDIA_IDLE_MS } else { 0 });
        }
    }
}

/// The "droidvm,virtio-media" encoder device on xnuhost's transport (`br_encoder_create`). `pool_host` is the host
/// mapping of the media pool (guest physical `pool_gpa`, `pool_size` bytes) in which the MMAP'd V4L2 buffers live;
/// the guest maps the same pool through its device-tree media-host-pool property. None when the device could not be
/// made.
///
/// # Safety
/// `pool_host` must point at `pool_size` bytes of guest RAM, mapped for as long as the device lives.
pub unsafe fn create(
    base: u64,
    spi: u32,
    backend: Option<Arc<dyn Backend>>,
    pool_host: *mut u8,
    pool_gpa: u64,
    pool_size: u64,
) -> Option<Arc<MediaVirtio>> {
    let (Some(backend), Some(host)) = (backend, NonNull::new(pool_host)) else {
        logi!("encoder: off: no media pool or backend");
        return None;
    };
    if pool_size < MEDIA_SLOTS as u64 * (1 << 20) {
        logi!("encoder: off: no media pool or backend");
        return None;
    }
    let name = backend.name();
    let wake = Arc::new(Wake::default());
    let state = State {
        sessions: Default::default(),
        fifo: VecDeque::with_capacity(MEDIA_EVENT_FIFO),
        next_session: 0,
        busy_until_ms: 0,
        stats: Stats::default(),
    };
    let device = MediaDevice {
        backend,
        pool: Pool { host, size: pool_size },
        slot_size: pool_size / MEDIA_SLOTS as u64,
        state: RefCell::new(state),
        wake: Arc::clone(&wake),
    };
    let info =
        Info { device_id: MEDIA_VIRTIO_DEVICE_ID, features: 0, nqueues: MEDIA_NQUEUES, manual: 1 << MEDIA_QUEUE_EVENT };
    let dev = Virtio::create(base, spi, info, device, "virtio-media").ok()?;
    let weak = Arc::downgrade(&dev);
    if std::thread::Builder::new().name("encoder".into()).spawn(move || worker(weak, wake)).is_err() {
        loge!("encoder: no worker thread");
    }
    logi!(
        "encoder: on: {name} backend, media pool {} MiB at {pool_gpa:#x} ({MEDIA_SLOTS} session slots)",
        pool_size >> 20
    );
    Some(dev)
}

/// The counters of a device (`br_encoder_get_stats`).
pub fn stats(dev: &MediaVirtio) -> Stats {
    dev.lock().stats()
}
