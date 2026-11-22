// SPDX-License-Identifier: MIT
//! xnuhost's end of the camera helper (bin/oneios-camera, [`crate::helper`]): starts it, takes the frame ring's
//! descriptor and the camera list from it, and reads frames out of the ring (C: link.c, `br_camera_*` of
//! include/bridges/camera.h). Static and libc only, like the rest of the bridges.
use crate::msg::{self, Kind, Msg};
use crate::pod::Pod;
use crate::proto::{OcCamera, OcFrame, OC_MAX_CAMERAS};
use crate::ring::{ReadError, RingReader, CAM_RING_VERSION};
use crate::spawn::{reap_nowait, socket_pair, spawn_with_fd3};
use crate::util::strerror;
use std::os::fd::{AsFd, AsRawFd, OwnedFd};
use std::sync::{Arc, Condvar, Mutex, MutexGuard, PoisonError};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};
use xh_core::{loge, logi};

/// What the bridge says, on its own thread (`BR_CAMERA_FRAME`, `BR_CAMERA_STATE`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Event {
    /// A frame is ready.
    Frame,
    /// The stream's state changed: 0 = running or stopped as asked, else the errno it ended with (EPIPE: the helper
    /// is gone).
    State(i32),
}

pub type Listener = Arc<dyn Fn(Event) + Send + Sync>;

struct State {
    /// once the helper's hello came
    ring: Option<RingReader>,
    listed: bool,
    gone: bool,
    cameras: Vec<OcCamera>,
    /// the newest frame read
    last: u64,
    listener: Option<Listener>,
}

struct Shared {
    socket: OwnedFd,
    state: Mutex<State>,
    /// listed or gone became true ([`Camera::list`] waits on it)
    changed: Condvar,
}

impl Shared {
    fn lock(&self) -> MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }

    fn event(&self, event: Event) {
        let listener = self.lock().listener.clone();
        if let Some(f) = listener {
            f(event);
        }
    }

    fn send(&self, message: &Msg) -> Result<(), i32> {
        // Never block a vCPU on a helper that is behind.
        // SAFETY: send(2) of a live buffer on our socket.
        let sent = unsafe {
            libc::send(
                self.socket.as_raw_fd(),
                message.as_bytes().as_ptr().cast(),
                message.as_bytes().len(),
                libc::MSG_DONTWAIT | libc::MSG_NOSIGNAL,
            )
        };
        if sent < 0 {
            Err(xh_core::sys::errno())
        } else {
            Ok(())
        }
    }

    /// The receive loop: the ring, the list, the states and frame notes, until the helper ends or the socket is shut.
    fn receive(&self) {
        let mut packet = [0u8; std::mem::size_of::<Msg>() + OC_MAX_CAMERAS as usize * std::mem::size_of::<OcCamera>()];
        loop {
            let (got, passed) = match msg::recv(self.socket.as_fd(), &mut packet) {
                Ok((0, _)) | Err(_) => break, // the helper ended, or close() shut the socket
                Ok(r) => r,
            };
            if got < std::mem::size_of::<Msg>() {
                continue; // `passed` is closed with it
            }
            let message = Msg::read_from(&packet[..got]);
            match Kind::from_wire(message.kind) {
                Some(Kind::Hello) => {
                    let ring = match passed {
                        Some(fd) if message.a == CAM_RING_VERSION => {
                            RingReader::attach(fd).map_err(|e| crate::core::errno_of(&e))
                        }
                        _ => Err(libc::EPROTO),
                    };
                    match ring {
                        Ok(ring) => self.lock().ring = Some(ring),
                        Err(e) => loge!("camera: the helper's frame ring is not usable: {}", strerror(e)),
                    }
                }
                Some(Kind::Cameras) => {
                    let have = (got - std::mem::size_of::<Msg>()) / std::mem::size_of::<OcCamera>();
                    let count = (message.a as usize).min(have);
                    let list = &packet[std::mem::size_of::<Msg>()..];
                    let mut state = self.lock();
                    state.cameras = (0..count).map(|i| OcCamera::read_from(&list[i * 32..i * 32 + 32])).collect();
                    state.listed = true;
                    self.changed.notify_all();
                    drop(state);
                    logi!("camera: the helper has {count} cameras");
                }
                Some(Kind::State) => {
                    if message.status != 0 {
                        loge!("camera: the helper's stream ended: {}", strerror(message.status as i32));
                    } else if message.a != 0 {
                        logi!("camera: on: {}x{} at {} fps", message.b, message.c, message.d);
                    }
                    self.event(Event::State(message.status as i32));
                }
                Some(Kind::Frame) => self.event(Event::Frame),
                _ => {}
            }
        }
        let mut state = self.lock();
        state.gone = true;
        state.listed = true;
        self.changed.notify_all();
        drop(state);
        self.event(Event::State(libc::EPIPE));
    }
}

/// The camera bridge as xnuhost uses it (`br_camera`): the helper process and its frame ring.
pub struct Camera {
    shared: Arc<Shared>,
    pid: libc::pid_t,
    thread: Mutex<Option<JoinHandle<()>>>,
}

impl Camera {
    /// Starts the helper at `path` with `backend` (None = auto; `br_camera_helper`). Returns at once: the helper's
    /// camera list arrives later. None (with a log line) when it cannot be started.
    pub fn helper(path: &str, backend: Option<&str>) -> Option<Camera> {
        let (ours, theirs) = match socket_pair(libc::SOCK_SEQPACKET) {
            Ok(pair) => pair,
            Err(e) => {
                loge!("camera: off: socketpair: {}", xh_core::sys::error_text(&e));
                return None;
            }
        };
        // The helper's end is its descriptor 3.
        let args: Vec<&str> = std::iter::once("3").chain(backend).collect();
        let spawned = spawn_with_fd3(path, &args, theirs.as_fd());
        drop(theirs);
        let pid = match spawned {
            Ok(pid) => pid,
            Err(e) => {
                loge!("camera: off: {path}: {}", strerror(e));
                return None;
            }
        };
        let state = State { ring: None, listed: false, gone: false, cameras: Vec::new(), last: 0, listener: None };
        let shared = Arc::new(Shared { socket: ours, state: Mutex::new(state), changed: Condvar::new() });
        let receiver = Arc::clone(&shared);
        let thread = std::thread::Builder::new().name("camera-link".into()).spawn(move || receiver.receive());
        let Ok(thread) = thread else {
            loge!("camera: off: no thread for the helper");
            return None; // the helper exits when its socket closes
        };
        logi!("camera: helper {path} (pid {pid})");
        Some(Camera { shared, pid, thread: Mutex::new(Some(thread)) })
    }

    /// The helper's process id.
    pub fn pid(&self) -> libc::pid_t {
        self.pid
    }

    /// Who hears of frames and state changes (`br_camera_listen`); None: nobody.
    pub fn listen(&self, listener: Option<Listener>) {
        self.shared.lock().listener = listener;
    }

    /// The cameras the helper found, one per role (`br_camera_list`), at most `max`: waits up to `wait` for the
    /// helper's list, asleep on the receive thread's signal (xnuhost waits here once, before it builds the device
    /// tree, and wakes the moment the list comes). Empty when none came.
    pub fn list(&self, max: usize, wait: Duration) -> Vec<OcCamera> {
        let deadline = Instant::now() + wait;
        let mut state = self.shared.lock();
        while !state.listed {
            let left = deadline.saturating_duration_since(Instant::now());
            if left.is_zero() {
                break;
            }
            state = self.shared.changed.wait_timeout(state, left).unwrap_or_else(PoisonError::into_inner).0;
        }
        if !state.listed {
            return Vec::new();
        }
        state.cameras.iter().take(max).copied().collect()
    }

    /// Asks the helper to open a camera (`br_camera_start`). It does not wait: the result is an [`Event::State`].
    pub fn start(&self, role: u32, width: u32, height: u32, fps: u32) -> Result<(), i32> {
        let gone = {
            let mut state = self.shared.lock();
            // What is in the ring now is from before this start.
            if let Some(newest) = state.ring.as_ref().map(RingReader::newest) {
                state.last = newest;
            }
            state.gone
        };
        if gone {
            return Err(libc::EPIPE);
        }
        self.shared.send(&Msg { a: role, b: width, c: height, d: fps, ..Msg::new(Kind::Start) })
    }

    /// Asks the helper to close the camera (`br_camera_stop`); the result is an [`Event::State`].
    pub fn stop(&self) -> Result<(), i32> {
        self.shared.send(&Msg::new(Kind::Stop))
    }

    /// Copies the newest frame not read yet: its header to `frame`, the picture to `dst` (`br_camera_frame`).
    /// [`ReadError::Again`] when there is none (or no ring yet).
    pub fn frame(&self, frame: &mut OcFrame, dst: &mut [u8]) -> Result<(), ReadError> {
        let mut state = self.shared.lock();
        let state = &mut *state;
        match &state.ring {
            Some(ring) => ring.read(&mut state.last, frame, dst),
            None => Err(ReadError::Again),
        }
    }
}

impl Camera {
    /// Closes the camera and ends the helper (`br_camera_close`; again: nothing). Does not wait for the helper longer
    /// than it takes to close its socket. After it the bridge has no frames, and a start is EPIPE.
    pub fn close(&self) {
        let Some(thread) = self.thread.lock().unwrap_or_else(PoisonError::into_inner).take() else { return };
        self.listen(None);
        // SAFETY: shutdown(2) of our own socket: the helper sees the end, closes the camera and exits.
        unsafe { libc::shutdown(self.shared.socket.as_raw_fd(), libc::SHUT_RDWR) };
        let _ = thread.join();
        self.shared.lock().ring = None;
        // The helper needs a moment to close the camera; it is not waited for (PR_SET_PDEATHSIG and the session's
        // cgroup end it in any case).
        reap_nowait(self.pid);
    }
}

impl Drop for Camera {
    fn drop(&mut self) {
        self.close();
    }
}
