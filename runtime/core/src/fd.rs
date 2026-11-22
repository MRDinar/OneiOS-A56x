// SPDX-License-Identifier: MIT
//! File descriptor helpers the C runtime repeated: the O_NONBLOCK and FD_CLOEXEC switches, and system calls retried
//! on EINTR.
use std::io;
use std::os::fd::{AsRawFd, BorrowedFd};

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

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::fd::AsFd;

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
    }
}
