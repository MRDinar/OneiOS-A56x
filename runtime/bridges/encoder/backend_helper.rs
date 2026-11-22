// SPDX-License-Identifier: MIT
//! The device-side encoder backend that runs the real encoder in the oneios-encoder helper process
//! ([`crate::helper`]), because MediaCodec is a shared library and xnuhost is static (backend_helper.c). It spawns the
//! helper once (at the first session), multiplexes the encoder sessions over a UNIX socket ([`crate::link`]), and
//! buffers the coded access units the helper returns for poll(). Static, libc only, like the rest of the bridges.
//!
//! Writes to the helper go under a lock of their own, not the state's: in the C one lock covered both, so a device
//! writing a frame while the helper wrote coded units back could stop the receive thread at the lock with the socket
//! full both ways.
use crate::backend::{Backend, Polled, Session};
use crate::link::*;
use onecamera::spawn::{reap_nowait, socket_pair, spawn_with_fd3};
use std::collections::VecDeque;
use std::io::Read;
use std::os::fd::{AsFd, OwnedFd};
use std::os::unix::net::UnixStream;
use std::sync::{Arc, Condvar, Mutex, MutexGuard, PoisonError};

const HELPER_MAX_SESSIONS: usize = 8;
const HELPER_CODED_RING: usize = 32;

struct Coded {
    bytes: Vec<u8>,
    pts: i64,
    key: bool,
    eos: bool,
}

#[derive(Default)]
struct Slot {
    used: bool,
    open_ok: bool,
    open_done: bool,
    ring: VecDeque<Coded>,
    got_eos: bool,
}

#[derive(Default)]
struct State {
    started: bool,
    gone: bool,
    pid: libc::pid_t,
    slots: [Slot; HELPER_MAX_SESSIONS],
}

struct Shared {
    path: String,
    state: Mutex<State>,
    /// open replies and new coded buffers
    cond: Condvar,
    /// the device's end of the socket, once the helper said hello; writes under this lock
    writer: Mutex<Option<UnixStream>>,
}

impl Shared {
    fn lock(&self) -> MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }

    fn send(&self, m: EncMsg, payload: &[u8]) -> Result<(), i32> {
        let mut writer = self.writer.lock().unwrap_or_else(PoisonError::into_inner);
        match writer.as_mut() {
            Some(w) => m.send(w, payload).map_err(|_| libc::EIO),
            None => Err(libc::EPIPE),
        }
    }

    /// The receive loop: open replies, coded buffers and EOS from the helper.
    fn receive(&self, mut from: UnixStream) {
        while let Ok(m) = EncMsg::receive(&mut from) {
            let mut payload = vec![0u8; m.len as usize];
            if from.read_exact(&mut payload).is_err() {
                break;
            }
            let mut st = self.lock();
            let slot = (m.session as usize).checked_sub(1).filter(|&i| i < HELPER_MAX_SESSIONS);
            if let Some(s) = slot.map(|i| &mut st.slots[i]).filter(|s| s.used) {
                match m.type_ {
                    ENC_MSG_OPEN_REPLY => (s.open_ok, s.open_done) = (m.session != 0 && m.a != 0, true),
                    ENC_MSG_CODED if !payload.is_empty() && s.ring.len() < HELPER_CODED_RING => s
                        .ring
                        .push_back(Coded { bytes: payload, pts: m.pts, key: m.a & 1 != 0, eos: (m.a >> 1) & 1 != 0 }),
                    ENC_MSG_EOS => s.got_eos = true,
                    _ => {}
                }
            }
            self.cond.notify_all();
        }
        self.lock().gone = true;
        self.cond.notify_all();
    }

    /// Spawns the helper and waits for its HELLO (the state locked by the caller). An errno on failure.
    fn start(self: &Arc<Self>, st: &mut State) -> Result<(), i32> {
        if st.started {
            return if st.gone { Err(libc::EPIPE) } else { Ok(()) };
        }
        let (ours, theirs): (OwnedFd, OwnedFd) =
            socket_pair(libc::SOCK_STREAM).map_err(|e| e.raw_os_error().unwrap_or(libc::EIO))?;
        let pid = spawn_with_fd3(&self.path, &["3"], theirs.as_fd());
        drop(theirs);
        st.pid = pid?;
        let mut stream = UnixStream::from(ours);
        let hello = EncMsg::receive(&mut stream);
        if !hello.is_ok_and(|h| h.type_ == ENC_MSG_HELLO && h.a == ENC_LINK_VERSION) {
            reap_nowait(st.pid);
            return Err(libc::EPROTO);
        }
        let reader = stream.try_clone().map_err(|e| e.raw_os_error().unwrap_or(libc::EIO))?;
        *self.writer.lock().unwrap_or_else(PoisonError::into_inner) = Some(stream);
        st.started = true;
        let shared = Arc::clone(self);
        let _ = std::thread::Builder::new().name("encoder-link".into()).spawn(move || shared.receive(reader));
        Ok(())
    }
}

/// The backend ("mediacodec-helper", `br_encoder_backend_helper`): the helper at `path`.
pub struct HelperBackend {
    shared: Arc<Shared>,
}

impl HelperBackend {
    /// `path` is the helper binary (runtime/bin/oneios-encoder); it is started at the first session.
    pub fn new(path: &str) -> Arc<HelperBackend> {
        let shared =
            Shared { path: path.to_owned(), state: Mutex::default(), cond: Condvar::new(), writer: Mutex::new(None) };
        Arc::new(HelperBackend { shared: Arc::new(shared) })
    }
}

struct HelperSession {
    shared: Arc<Shared>,
    id: u32,
}

impl Backend for HelperBackend {
    fn name(&self) -> &'static str {
        "mediacodec-helper"
    }

    fn open(&self, codec: u32, width: u32, height: u32) -> Option<Box<dyn Session>> {
        let shared = &self.shared;
        let mut st = shared.lock();
        shared.start(&mut st).ok()?;
        let index = st.slots.iter().position(|s| !s.used)?;
        st.slots[index] = Slot { used: true, ..Slot::default() };
        let id = index as u32 + 1;
        drop(st);
        let open = EncMsg { a: codec, b: width, c: height, ..EncMsg::new(ENC_MSG_OPEN, id) };
        let sent = shared.send(open, &[]);
        let mut st = shared.lock();
        if sent.is_ok() {
            st = shared
                .cond
                .wait_while(st, |st| !st.slots[index].open_done && !st.gone)
                .unwrap_or_else(PoisonError::into_inner);
        }
        let ok = sent.is_ok() && st.slots[index].open_ok;
        if !ok {
            st.slots[index].used = false;
            return None;
        }
        Some(Box::new(HelperSession { shared: Arc::clone(shared), id }))
    }
}

impl HelperSession {
    fn slot<'a>(&self, st: &'a mut State) -> &'a mut Slot {
        &mut st.slots[self.id as usize - 1]
    }
}

impl Session for HelperSession {
    fn feed(&mut self, nv12: &[u8], pts_us: i64, force_key: bool) -> Result<(), i32> {
        let m = EncMsg { pts: pts_us, a: u32::from(force_key), ..EncMsg::new(ENC_MSG_FEED, self.id) };
        self.shared.send(m, nv12).map_err(|_| libc::EIO)
    }

    fn drain(&mut self) -> Result<(), i32> {
        self.shared.send(EncMsg::new(ENC_MSG_DRAIN, self.id), &[]).map_err(|_| libc::EIO)
    }

    fn poll(&mut self, buf: &mut [u8]) -> Result<Polled, i32> {
        let mut st = self.shared.lock();
        let s = self.slot(&mut st);
        if let Some(c) = s.ring.pop_front() {
            // A unit larger than the guest's coded buffer is dropped and said once: left at the ring's head it came
            // back as an error at every poll of the device's worker, for as long as the session lived, and no later
            // frame got out.
            if c.bytes.len() > buf.len() {
                return Err(libc::ENOSPC);
            }
            buf[..c.bytes.len()].copy_from_slice(&c.bytes);
            return Ok(Polled { bytes: c.bytes.len(), key: c.key, pts_us: c.pts, eos: c.eos });
        }
        if s.got_eos {
            s.got_eos = false;
            return Ok(Polled { eos: true, ..Polled::default() });
        }
        Ok(Polled::default())
    }
}

impl Drop for HelperSession {
    fn drop(&mut self) {
        let alive = {
            let st = self.shared.lock();
            st.started && !st.gone
        };
        if alive {
            let _ = self.shared.send(EncMsg::new(ENC_MSG_CLOSE, self.id), &[]);
        }
        let mut st = self.shared.lock();
        *self.slot(&mut st) = Slot::default();
    }
}
