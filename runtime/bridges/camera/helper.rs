// SPDX-License-Identifier: MIT
//! bin/oneios-camera: the camera helper process. xnuhost is a static binary and the camera needs Android's shared
//! libraries (libcamera2ndk, libbinder_ndk, libnativewindow), so, like oneios-audio, a small dynamic executable does
//! it: it opens the camera xnuhost asks for and puts each frame, as NV12 with its time, into a memfd ring that
//! xnuhost maps.
//!
//! ```text
//!   oneios-camera FD [BACKEND]     FD: a SOCK_SEQPACKET to xnuhost (crate::msg)
//!                                  BACKEND: auto (default), ndk, hal, test; or $ONEIOS_CAMERA_BACKEND
//! ```
//!
//! It starts and ends with the session: it exits when its socket closes or on SIGTERM, and it dies with xnuhost
//! (PR_SET_PDEATHSIG). It reads no input device, asks for no realtime scheduling, touches nothing under /sys, and
//! holds the camera only between START and STOP. Between messages it sleeps in poll(2): a message, a stream that
//! ended by itself (an eventfd the back end's thread writes) or a signal wakes it (the C woke every 200 ms to look).
use crate::cam_log;
use crate::core::Core;
use crate::msg::{self, Kind, Msg};
use crate::pod::Pod;
use crate::proto::{OcCamera, Role, OC_MAX_CAMERAS};
use crate::ring::CAM_RING_VERSION;
use crate::util::{atoi, set_verbose, strerror};
use std::os::fd::{AsFd, AsRawFd, BorrowedFd, FromRawFd, OwnedFd};
use std::sync::atomic::{AtomicBool, AtomicI32, Ordering};
use std::sync::Arc;

static QUIT: AtomicBool = AtomicBool::new(false);

extern "C" fn on_signal(_: libc::c_int) {
    QUIT.store(true, Ordering::Relaxed);
}

/// SIGTERM, SIGINT and SIGHUP end the loop (no SA_RESTART: poll returns EINTR); SIGPIPE is ignored.
fn signals() {
    // SAFETY: an async-signal-safe handler (one atomic store); a zeroed sigaction with only the handler set.
    unsafe {
        let mut action: libc::sigaction = std::mem::zeroed();
        action.sa_sigaction = on_signal as extern "C" fn(libc::c_int) as libc::sighandler_t;
        for signal in [libc::SIGTERM, libc::SIGINT, libc::SIGHUP] {
            libc::sigaction(signal, &action, std::ptr::null_mut());
        }
        libc::signal(libc::SIGPIPE, libc::SIG_IGN);
    }
}

/// The stream's end, from a back end's thread to the main loop: the errno and an eventfd that wakes poll.
struct Ended {
    error: AtomicI32,
    wake: OwnedFd,
}

impl Ended {
    fn new() -> std::io::Result<Ended> {
        // SAFETY: a plain eventfd(2) call; the result is checked.
        let fd = unsafe { libc::eventfd(0, libc::EFD_CLOEXEC | libc::EFD_NONBLOCK) };
        if fd < 0 {
            return Err(std::io::Error::last_os_error());
        }
        // SAFETY: a fresh descriptor that nothing else owns.
        Ok(Ended { error: AtomicI32::new(0), wake: unsafe { OwnedFd::from_raw_fd(fd) } })
    }

    fn set(&self, error: i32) {
        self.error.store(if error != 0 { error } else { libc::EIO }, Ordering::Release);
        let one = 1u64.to_ne_bytes();
        // SAFETY: an 8-byte write to our eventfd.
        unsafe { libc::write(self.wake.as_raw_fd(), one.as_ptr().cast(), 8) };
    }

    fn take(&self) -> i32 {
        let mut count = [0u8; 8];
        // SAFETY: an 8-byte read from our non-blocking eventfd (EAGAIN when nothing is there).
        unsafe { libc::read(self.wake.as_raw_fd(), count.as_mut_ptr().cast(), 8) };
        self.error.swap(0, Ordering::AcqRel)
    }
}

fn send_state(channel: BorrowedFd<'_>, core: &Core, status: i32) {
    let state = Msg {
        status: status as u32,
        a: u32::from(core.running()),
        b: core.width() as u32,
        c: core.height() as u32,
        d: core.fps() as u32,
        ..Msg::new(Kind::State)
    };
    let _ = msg::send(channel, &state, &[], None);
}

/// The helper's main: its exit code.
pub fn main() -> i32 {
    let args: Vec<String> = std::env::args().collect();
    if args.len() < 2 {
        eprintln!("usage: oneios-camera FD [auto|ndk|hal|test]   (started by xnuhost; camctl tries it by hand)");
        return 2;
    }
    let fd = atoi(&args[1]);
    // SAFETY: the descriptor xnuhost (or camctl) gave this process as its socket; it lives as long as the process.
    let channel = unsafe { BorrowedFd::borrow_raw(fd) };
    let env_backend = std::env::var("ONEIOS_CAMERA_BACKEND").ok();
    let backend = args.get(2).cloned().or(env_backend);
    if let Ok(v) = std::env::var("ONEIOS_CAMERA_VERBOSE") {
        set_verbose(if atoi(&v) > 1 { 2 } else { 1 }); // 2: every callback and frame
    }
    // SAFETY: plain prctl(2): this process gets SIGTERM when xnuhost ends.
    unsafe { libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGTERM) };
    signals();

    let ended = match Ended::new() {
        Ok(e) => Arc::new(e),
        Err(e) => {
            cam_log!("no wake descriptor: {}", xh_core::sys::error_text(&e));
            return 1;
        }
    };
    let on_frame = Box::new(move |sequence: u64| {
        // A frame is in the ring. A full socket drops the note, not the frame: the reader takes the newest.
        // SAFETY: as `channel` above.
        let channel = unsafe { BorrowedFd::borrow_raw(fd) };
        let _ = msg::send(channel, &Msg { seq: sequence, ..Msg::new(Kind::Frame) }, &[], None);
    });
    let stream_end = Arc::clone(&ended);
    let on_error = Box::new(move |error: i32| stream_end.set(error));
    let mut core = match Core::new(backend.as_deref(), Some(on_frame), Some(on_error)) {
        Ok(core) => core,
        Err(e) => {
            cam_log!("no frame ring: {}", strerror(e));
            return 1;
        }
    };
    match core.list() {
        Ok(n) => cam_log!("{}: {n} cameras", core.backend_name()),
        Err(e) => cam_log!("{}: no cameras: {}", core.backend_name(), strerror(e)),
    }

    let ring = core.ring_fd();
    let hello = Msg { a: CAM_RING_VERSION, ..Msg::new(Kind::Hello) };
    let sent = match &ring {
        Ok(ring) => msg::send(channel, &hello, &[], Some(ring.as_fd())),
        Err(e) => Err(crate::core::errno_of(e)),
    };
    drop(ring);
    if let Err(e) = sent {
        cam_log!("no xnuhost on descriptor {fd}: {}", strerror(e));
        return 1;
    }
    // One camera per role goes to the guest; the cameras without a role stay the helper's.
    let list: Vec<OcCamera> = core
        .cameras()
        .iter()
        .filter(|c| c.role != Role::Other && c.role != Role::None)
        .take(OC_MAX_CAMERAS as usize)
        .map(|c| c.to_wire())
        .collect();
    let payload: Vec<u8> = list.iter().flat_map(|c| c.as_bytes().to_vec()).collect();
    let _ = msg::send(channel, &Msg { a: list.len() as u32, ..Msg::new(Kind::Cameras) }, &payload, None);

    while !QUIT.load(Ordering::Relaxed) {
        let mut wait = [
            libc::pollfd { fd, events: libc::POLLIN, revents: 0 },
            libc::pollfd { fd: ended.wake.as_raw_fd(), events: libc::POLLIN, revents: 0 },
        ];
        // SAFETY: two valid pollfd entries.
        let ready = unsafe { libc::poll(wait.as_mut_ptr(), 2, -1) };
        let poll_errno = xh_core::sys::errno();
        let error = ended.take();
        if error != 0 && core.running() {
            core.stop();
            send_state(channel, &core, error);
        }
        if ready < 0 && poll_errno != libc::EINTR {
            break;
        }
        if ready <= 0 || wait[0].revents == 0 {
            continue;
        }
        let mut message = Msg::default();
        // SAFETY: recv(2) into a live buffer of the message's size.
        let got = unsafe {
            libc::recv(fd, message.as_bytes_mut().as_mut_ptr().cast(), std::mem::size_of::<Msg>(), libc::MSG_DONTWAIT)
        };
        let recv_errno = xh_core::sys::errno();
        if got == 0 || (got < 0 && recv_errno != libc::EAGAIN && recv_errno != libc::EINTR) {
            break; // xnuhost is gone
        }
        if got < std::mem::size_of::<Msg>() as isize {
            continue;
        }
        match Kind::from_wire(message.kind) {
            Some(Kind::Start) => {
                let rc = match core.by_role(message.a) {
                    Some(i) => core.start(i, message.b as i32, message.c as i32, message.d as i32),
                    None => Err(libc::ENODEV),
                };
                send_state(channel, &core, rc.err().unwrap_or(0));
            }
            Some(Kind::Stop) => {
                core.stop();
                send_state(channel, &core, 0);
            }
            _ => {}
        }
    }
    core.stop(); // closes the camera: the HAL keeps nothing of ours
    0
}
