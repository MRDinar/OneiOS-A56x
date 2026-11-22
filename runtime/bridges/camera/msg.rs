// SPDX-License-Identifier: MIT
//! xnuhost <-> the camera helper: one `struct cam_msg` per SOCK_SEQPACKET packet (camera.h's `CAM_MSG_*`), the
//! camera list after the CAMERAS one, the ring's descriptor with the HELLO one (SCM_RIGHTS).
use crate::pod::Pod;
use std::io;
use std::os::fd::{AsRawFd, BorrowedFd, FromRawFd, OwnedFd};

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
    let mut iov = [
        libc::iovec { iov_base: message.as_bytes().as_ptr() as *mut libc::c_void, iov_len: message.as_bytes().len() },
        libc::iovec { iov_base: payload.as_ptr() as *mut libc::c_void, iov_len: payload.len() },
    ];
    // SAFETY: an all-zero msghdr is a valid empty one.
    let mut packet: libc::msghdr = unsafe { std::mem::zeroed() };
    packet.msg_iov = iov.as_mut_ptr();
    packet.msg_iovlen = if payload.is_empty() { 1 } else { 2 };
    // A control buffer for one descriptor, aligned for cmsghdr.
    let mut control = [0u64; 4];
    if let Some(fd) = pass {
        // SAFETY: CMSG_SPACE is a pure size computation.
        let space = unsafe { libc::CMSG_SPACE(std::mem::size_of::<libc::c_int>() as u32) } as usize;
        assert!(space <= std::mem::size_of_val(&control));
        packet.msg_control = control.as_mut_ptr().cast();
        packet.msg_controllen = space as _;
        // SAFETY: the control buffer is `space` bytes, enough for one cmsghdr with an int.
        unsafe {
            let header = libc::CMSG_FIRSTHDR(&packet);
            (*header).cmsg_level = libc::SOL_SOCKET;
            (*header).cmsg_type = libc::SCM_RIGHTS;
            (*header).cmsg_len = libc::CMSG_LEN(std::mem::size_of::<libc::c_int>() as u32) as _;
            std::ptr::write_unaligned(libc::CMSG_DATA(header).cast::<libc::c_int>(), fd.as_raw_fd());
        }
    }
    // SAFETY: the msghdr points at live buffers for the call.
    let sent = unsafe { libc::sendmsg(socket.as_raw_fd(), &packet, libc::MSG_NOSIGNAL | libc::MSG_DONTWAIT) };
    if sent < 0 {
        Err(xh_core::sys::errno())
    } else {
        Ok(())
    }
}

/// Receives one packet into `buffer` (blocking; EINTR is retried): its length (0: the other end is gone) and the
/// descriptor it carried, if any (close-on-exec).
pub fn recv(socket: BorrowedFd<'_>, buffer: &mut [u8]) -> io::Result<(usize, Option<OwnedFd>)> {
    loop {
        let mut iov = libc::iovec { iov_base: buffer.as_mut_ptr().cast(), iov_len: buffer.len() };
        let mut control = [0u64; 8];
        // SAFETY: an all-zero msghdr is a valid empty one.
        let mut header: libc::msghdr = unsafe { std::mem::zeroed() };
        header.msg_iov = &mut iov;
        header.msg_iovlen = 1;
        header.msg_control = control.as_mut_ptr().cast();
        header.msg_controllen = std::mem::size_of_val(&control) as _;
        // SAFETY: the msghdr points at live buffers for the call.
        let got = unsafe { libc::recvmsg(socket.as_raw_fd(), &mut header, libc::MSG_CMSG_CLOEXEC) };
        if got < 0 {
            let e = io::Error::last_os_error();
            if e.raw_os_error() == Some(libc::EINTR) {
                continue;
            }
            return Err(e);
        }
        let mut passed: Option<OwnedFd> = None;
        // SAFETY: walking the control messages the kernel wrote into our buffer, with the CMSG macros.
        unsafe {
            let mut m = libc::CMSG_FIRSTHDR(&header);
            while !m.is_null() {
                let len = libc::CMSG_LEN(std::mem::size_of::<libc::c_int>() as u32) as usize;
                if (*m).cmsg_level == libc::SOL_SOCKET
                    && (*m).cmsg_type == libc::SCM_RIGHTS
                    && (*m).cmsg_len as usize >= len
                {
                    let fd = std::ptr::read_unaligned(libc::CMSG_DATA(m).cast::<libc::c_int>());
                    // A descriptor the kernel just gave us: ours to own (a second one replaces and closes the first).
                    passed = Some(OwnedFd::from_raw_fd(fd));
                }
                m = libc::CMSG_NXTHDR(&header, m);
            }
        }
        return Ok((got as usize, passed));
    }
}
