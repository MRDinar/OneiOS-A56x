// SPDX-License-Identifier: MIT
//! xnuhost <-> the camera helper: one `struct cam_msg` per SOCK_SEQPACKET packet (camera.h's `CAM_MSG_*`), the
//! camera list after the CAMERAS one, the ring's descriptor with the HELLO one (SCM_RIGHTS).
use crate::pod::Pod;
use std::io;
use std::os::fd::{BorrowedFd, OwnedFd};

/// What a message is (`CAM_MSG_*`).
#[repr(u32)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    /// helper -> xnuhost, with the ring's descriptor (SCM_RIGHTS); a = protocol version
    Hello = 1,
    /// helper -> xnuhost: a = count, then `OcCamera[count]`
    Cameras = 2,
    /// xnuhost -> helper: a = role, b = width, c = height, d = fps
    Start = 3,
    /// xnuhost -> helper
    Stop = 4,
    /// helper -> xnuhost: status = errno (0 = running), a = running, b = width, c = height, d = fps
    State = 5,
    /// helper -> xnuhost: a frame is in the ring; seq = its sequence
    Frame = 6,
}

impl Kind {
    pub fn from_wire(kind: u32) -> Option<Kind> {
        [Kind::Hello, Kind::Cameras, Kind::Start, Kind::Stop, Kind::State, Kind::Frame]
            .into_iter()
            .find(|k| *k as u32 == kind)
    }
}

/// `struct cam_msg { uint32_t kind, status, a, b, c, d; uint64_t seq; }`
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Msg {
    pub kind: u32,
    pub status: u32,
    pub a: u32,
    pub b: u32,
    pub c: u32,
    pub d: u32,
    pub seq: u64,
}

// SAFETY: repr(C), six u32 and a u64 at offset 24: no padding.
unsafe impl Pod for Msg {}

impl Msg {
    pub fn new(kind: Kind) -> Msg {
        Msg { kind: kind as u32, ..Default::default() }
    }
}

const _: () = assert!(std::mem::size_of::<Msg>() == 32);

// ---- the socket ---------------------------------------------------------------------------------------------------
/// Sends one message, `payload` after it, and `pass` as SCM_RIGHTS, without blocking and without SIGPIPE (the
/// helper's `send_message`): an errno when the socket is full or gone.
pub fn send(socket: BorrowedFd<'_>, message: &Msg, payload: &[u8], pass: Option<BorrowedFd<'_>>) -> Result<(), i32> {
    let both = [io::IoSlice::new(message.as_bytes()), io::IoSlice::new(payload)];
    let iov = if payload.is_empty() { &both[..1] } else { &both[..] };
    let pass: &[BorrowedFd<'_>] = match &pass {
        Some(fd) => std::slice::from_ref(fd),
        None => &[],
    };
    match xh_core::fd::send_with_fds(socket, iov, pass, libc::MSG_NOSIGNAL | libc::MSG_DONTWAIT) {
        Ok(_) => Ok(()),
        Err(e) => Err(e.raw_os_error().unwrap_or(libc::EIO)),
    }
}

/// The descriptors a packet's control buffer has room for (64 bytes, as the C's): more than the one HELLO brings.
const ROOM_FDS: usize = 12;

/// Receives one packet into `buffer` (blocking; EINTR is retried): its length (0: the other end is gone) and the
/// descriptor it carried, if any (close-on-exec; of several, the last is kept and the others closed).
pub fn recv(socket: BorrowedFd<'_>, buffer: &mut [u8]) -> io::Result<(usize, Option<OwnedFd>)> {
    loop {
        match xh_core::fd::recv_with_fds(socket, buffer, ROOM_FDS, 0) {
            Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
            Err(e) => return Err(e),
            Ok(mut got) => return Ok((got.length, got.fds.pop())),
        }
    }
}
