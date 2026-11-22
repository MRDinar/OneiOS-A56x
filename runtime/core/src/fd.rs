// SPDX-License-Identifier: MIT
//! File descriptor helpers the C runtime repeated: the O_NONBLOCK and FD_CLOEXEC switches, system calls retried on
//! EINTR, memfds, and SCM_RIGHTS packets (descriptors sent and received over a socket).
use std::ffi::CStr;
use std::io;
use std::os::fd::{AsRawFd, BorrowedFd, FromRawFd, OwnedFd, RawFd};

fn flags(fd: BorrowedFd<'_>, get: i32, set: i32, bit: i32, on: bool) -> io::Result<()> {
    let raw = fd.as_raw_fd();
    // SAFETY: fcntl on a descriptor the caller holds open (BorrowedFd), with integer arguments only.
    let old = unsafe { libc::fcntl(raw, get) };
    if old < 0 {
        return Err(io::Error::last_os_error());
    }
    let new = if on { old | bit } else { old & !bit };
    // SAFETY: as above.
    if new != old && unsafe { libc::fcntl(raw, set, new) } < 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

/// Sets or clears O_NONBLOCK.
pub fn set_nonblocking(fd: BorrowedFd<'_>, on: bool) -> io::Result<()> {
    flags(fd, libc::F_GETFL, libc::F_SETFL, libc::O_NONBLOCK, on)
}

/// Sets or clears FD_CLOEXEC.
pub fn set_cloexec(fd: BorrowedFd<'_>, on: bool) -> io::Result<()> {
    flags(fd, libc::F_GETFD, libc::F_SETFD, libc::FD_CLOEXEC, on)
}

/// Runs a system call wrapper until it does not fail with EINTR: `retry(|| unsafe { libc::read(..) })`.
pub fn retry<T: Copy + PartialOrd + Default>(mut call: impl FnMut() -> T) -> io::Result<T> {
    loop {
        let r = call();
        if r >= T::default() {
            return Ok(r);
        }
        let e = io::Error::last_os_error();
        if e.kind() != io::ErrorKind::Interrupted {
            return Err(e);
        }
    }
}

/// `memfd_create(name, flags)` (flags: `MFD_CLOEXEC`, `MFD_ALLOW_SEALING`): a new anonymous file, empty.
pub fn memfd(name: &CStr, flags: libc::c_uint) -> io::Result<OwnedFd> {
    // SAFETY: a NUL-terminated name and a flag word; the descriptor it returns is checked.
    let fd = unsafe { libc::syscall(libc::SYS_memfd_create, name.as_ptr(), flags) };
    if fd < 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: a new descriptor that nothing else owns.
    Ok(unsafe { OwnedFd::from_raw_fd(fd as RawFd) })
}

/// The control buffer's size for [`send_with_fds`] and [`recv_with_fds`]: CMSG_SPACE of up to 56 descriptors.
const CONTROL_BYTES: usize = 256;

/// A control buffer aligned for `cmsghdr`.
#[repr(C, align(8))]
struct Control([u8; CONTROL_BYTES]);

/// CMSG_SPACE of `count` descriptors.
fn fds_space(count: usize) -> usize {
    // SAFETY: CMSG_SPACE only computes a length.
    let space = unsafe { libc::CMSG_SPACE((count * std::mem::size_of::<RawFd>()) as u32) } as usize;
    assert!(space <= CONTROL_BYTES, "{count} descriptors do not fit one control buffer");
    space
}

/// One `sendmsg` of the buffers `iov` (one packet on a SOCK_SEQPACKET socket), with `fds` as SCM_RIGHTS when it is not
/// empty, and `flags` (MSG_NOSIGNAL, MSG_DONTWAIT, ...: the caller's). The bytes sent, or the errno; EINTR is the
/// caller's to retry. The display's xhd protocol and the camera helper's socket send through it.
pub fn send_with_fds(
    socket: BorrowedFd<'_>,
    iov: &[io::IoSlice<'_>],
    fds: &[BorrowedFd<'_>],
    flags: libc::c_int,
) -> io::Result<usize> {
    let mut control = Control([0; CONTROL_BYTES]);
    // SAFETY: msghdr is plain data; all zeros is a valid empty one.
    let mut msg: libc::msghdr = unsafe { std::mem::zeroed() };
    // IoSlice is ABI-compatible with iovec on Unix (std's guarantee); sendmsg only reads them.
    msg.msg_iov = iov.as_ptr() as *mut libc::iovec;
    msg.msg_iovlen = iov.len() as _;
    if !fds.is_empty() {
        let data = (fds.len() * std::mem::size_of::<RawFd>()) as u32;
        msg.msg_control = control.0.as_mut_ptr().cast();
        msg.msg_controllen = fds_space(fds.len()) as _;
        // SAFETY: the control buffer is aligned and at least CMSG_SPACE(data) bytes, so the first header and its data
        // fit in it.
        unsafe {
            let cm = libc::CMSG_FIRSTHDR(&msg);
            (*cm).cmsg_level = libc::SOL_SOCKET;
            (*cm).cmsg_type = libc::SCM_RIGHTS;
            (*cm).cmsg_len = libc::CMSG_LEN(data) as _;
            let out = libc::CMSG_DATA(cm) as *mut RawFd;
            for (i, f) in fds.iter().enumerate() {
                std::ptr::write_unaligned(out.add(i), f.as_raw_fd());
            }
        }
    }
    // SAFETY: msg points at the caller's buffers and the control buffer above, which live across the call.
    let n = unsafe { libc::sendmsg(socket.as_raw_fd(), &msg, flags) };
    if n < 0 {
        Err(io::Error::last_os_error())
    } else {
        Ok(n as usize)
    }
}

/// What one [`recv_with_fds`] brought.
#[derive(Debug)]
pub struct Received {
    /// The packet's length (0: the peer closed).
    pub length: usize,
    /// Every descriptor that came (close-on-exec), in order, up to the call's `max_fds`; ours to close.
    pub fds: Vec<OwnedFd>,
    /// MSG_TRUNC or MSG_CTRUNC: the packet or its descriptors did not fit.
    pub truncated: bool,
}

/// One `recvmsg` into `buf` with MSG_CMSG_CLOEXEC and `flags`, with room for `max_fds` descriptors (CMSG_SPACE of
/// them: what the kernel sends beyond that it closes, and sets MSG_CTRUNC). Every SCM_RIGHTS descriptor that came is
/// owned at once: kept up to `max_fds`, any others closed. EINTR is the caller's to retry.
pub fn recv_with_fds(
    socket: BorrowedFd<'_>,
    buf: &mut [u8],
    max_fds: usize,
    flags: libc::c_int,
) -> io::Result<Received> {
    let mut iov = libc::iovec { iov_base: buf.as_mut_ptr().cast(), iov_len: buf.len() };
    let mut control = Control([0; CONTROL_BYTES]);
    // SAFETY: msghdr is plain data; all zeros is a valid empty one.
    let mut msg: libc::msghdr = unsafe { std::mem::zeroed() };
    msg.msg_iov = &mut iov;
    msg.msg_iovlen = 1;
    msg.msg_control = control.0.as_mut_ptr().cast();
    msg.msg_controllen = fds_space(max_fds) as _;
    // SAFETY: msg points at buf and the control buffer, which live across the call.
    let n = unsafe { libc::recvmsg(socket.as_raw_fd(), &mut msg, flags | libc::MSG_CMSG_CLOEXEC) };
    if n < 0 {
        return Err(io::Error::last_os_error());
    }
    let mut fds = Vec::new();
    // SAFETY: the kernel filled msg's control buffer; the CMSG_* macros walk it within msg_controllen.
    unsafe {
        let mut cm = libc::CMSG_FIRSTHDR(&msg);
        while !cm.is_null() {
            if (*cm).cmsg_level == libc::SOL_SOCKET && (*cm).cmsg_type == libc::SCM_RIGHTS {
                let count = ((*cm).cmsg_len as usize - libc::CMSG_LEN(0) as usize) / std::mem::size_of::<RawFd>();
                let data = libc::CMSG_DATA(cm) as *const RawFd;
                for i in 0..count {
                    // Each descriptor that came is ours: kept up to max_fds, the rest closed when dropped here.
                    let got = OwnedFd::from_raw_fd(std::ptr::read_unaligned(data.add(i)));
                    if fds.len() < max_fds {
                        fds.push(got);
                    }
                }
            }
            cm = libc::CMSG_NXTHDR(&msg, cm);
        }
    }
    Ok(Received { length: n as usize, fds, truncated: msg.msg_flags & (libc::MSG_TRUNC | libc::MSG_CTRUNC) != 0 })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::fd::AsFd;

    fn pair() -> (OwnedFd, OwnedFd) {
        let mut fds = [0 as RawFd; 2];
        // SAFETY: socketpair writes two descriptors into the array.
        let rc =
            unsafe { libc::socketpair(libc::AF_UNIX, libc::SOCK_SEQPACKET | libc::SOCK_CLOEXEC, 0, fds.as_mut_ptr()) };
        assert_eq!(rc, 0);
        // SAFETY: two new descriptors nobody else owns.
        unsafe { (OwnedFd::from_raw_fd(fds[0]), OwnedFd::from_raw_fd(fds[1])) }
    }

    #[test]
    fn descriptors_over_a_socket() {
        let (a, b) = pair();
        let (m1, m2) = (memfd(c"xh-a", 0).unwrap(), memfd(c"xh-b", 0).unwrap());
        std::fs::File::from(m1.try_clone().unwrap()).set_len(4096).unwrap();
        // Two buffers, two descriptors: one packet.
        let iov = [io::IoSlice::new(b"he"), io::IoSlice::new(b"llo")];
        assert_eq!(send_with_fds(a.as_fd(), &iov, &[m1.as_fd(), m2.as_fd()], libc::MSG_NOSIGNAL).unwrap(), 5);
        let mut buf = [0u8; 16];
        let got = recv_with_fds(b.as_fd(), &mut buf, 4, 0).unwrap();
        assert_eq!((&buf[..got.length], got.fds.len(), got.truncated), (&b"hello"[..], 2, false));
        assert_eq!(std::fs::File::from(got.fds.into_iter().next().unwrap()).metadata().unwrap().len(), 4096);
        // SAFETY: fcntl F_GETFD on an open descriptor.
        let cloexec = |fd: &OwnedFd| unsafe { libc::fcntl(fd.as_raw_fd(), libc::F_GETFD) } & libc::FD_CLOEXEC != 0;
        // Room for one (CMSG_SPACE rounds it up to two on 64-bit): three sent, the kernel brings two and says it cut
        // the rest, the second is closed here. Then a packet with no descriptors that does not fit.
        send_with_fds(a.as_fd(), &[io::IoSlice::new(b"x")], &[m1.as_fd(), m2.as_fd(), m1.as_fd()], 0).unwrap();
        let got = recv_with_fds(b.as_fd(), &mut buf, 1, 0).unwrap();
        assert_eq!((got.length, got.fds.len(), got.truncated), (1, 1, true));
        assert!(cloexec(&got.fds[0]));
        send_with_fds(a.as_fd(), &[io::IoSlice::new(b"plain")], &[], 0).unwrap();
        let got = recv_with_fds(b.as_fd(), &mut buf[..2], 4, 0).unwrap();
        assert_eq!((got.length, got.fds.len(), got.truncated), (2, 0, true)); // the packet did not fit
        drop(a);
        assert_eq!(recv_with_fds(b.as_fd(), &mut buf, 4, libc::MSG_DONTWAIT).unwrap().length, 0);
        assert_eq!(
            send_with_fds(b.as_fd(), &[io::IoSlice::new(b"x")], &[], libc::MSG_NOSIGNAL).unwrap_err().raw_os_error(),
            Some(libc::EPIPE)
        );
    }

    #[test]
    fn switches() {
        let f = std::fs::File::open("/dev/null").unwrap();
        set_nonblocking(f.as_fd(), true).unwrap();
        // SAFETY: fcntl F_GETFL on an open descriptor.
        assert_ne!(unsafe { libc::fcntl(f.as_raw_fd(), libc::F_GETFL) } & libc::O_NONBLOCK, 0);
        set_cloexec(f.as_fd(), false).unwrap();
        // SAFETY: as above.
        assert_eq!(unsafe { libc::fcntl(f.as_raw_fd(), libc::F_GETFD) } & libc::FD_CLOEXEC, 0);
        let mut b = [0u8; 4];
        // SAFETY: a valid buffer of its length and an open descriptor.
        let n = retry(|| unsafe { libc::read(f.as_raw_fd(), b.as_mut_ptr().cast(), b.len()) }).unwrap();
        assert_eq!(n, 0);
        // SAFETY: -1 is never a descriptor; read fails with EBADF.
        let e = retry(|| unsafe { libc::read(-1, b.as_mut_ptr().cast(), 1) }).unwrap_err();
        assert_eq!(e.raw_os_error(), Some(libc::EBADF));
        let m = memfd(c"xh-core-test", libc::MFD_CLOEXEC).unwrap();
        let f = std::fs::File::from(m);
        f.set_len(8192).unwrap();
        assert_eq!(f.metadata().unwrap().len(), 8192);
    }
}
