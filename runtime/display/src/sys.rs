// SPDX-License-Identifier: MIT
//! The system calls the display helper makes (sockets with descriptors, maps, memfds, the clock), each wrapped in a
//! safe function. `Errno` is what the C returned as -errno; its text is strerror's, as the C logged it.
use std::ffi::CString;
use std::io;
use std::mem::{size_of, zeroed};
use std::os::fd::{AsRawFd, BorrowedFd, FromRawFd, OwnedFd, RawFd};
use std::path::Path;
use std::ptr;
use xh_core::fd::retry;

/// An errno value: the C functions' negative returns (xh-core's).
pub use xh_core::sys::Errno;

pub type Result<T> = std::result::Result<T, Errno>;

/// A system call's -1 as the errno.
fn check(r: libc::c_int) -> Result<libc::c_int> {
    if r < 0 {
        Err(Errno::last())
    } else {
        Ok(r)
    }
}

/// CLOCK_MONOTONIC in nanoseconds.
pub use xh_core::time::monotonic_ns;

/// CLOCK_MONOTONIC in microseconds.
pub fn now_us() -> i64 {
    xh_core::time::monotonic_us() as i64
}

/// CLOCK_MONOTONIC in milliseconds.
pub fn now_ms() -> i64 {
    xh_core::time::monotonic_ms() as i64
}

/// This process's user and system CPU time in microseconds.
pub fn cpu_us() -> i64 {
    // SAFETY: getrusage writes the rusage it is given.
    let mut u: libc::rusage = unsafe { zeroed() };
    // SAFETY: as above.
    unsafe { libc::getrusage(libc::RUSAGE_SELF, &mut u) };
    (u.ru_utime.tv_sec + u.ru_stime.tv_sec) as i64 * 1_000_000 + (u.ru_utime.tv_usec + u.ru_stime.tv_usec) as i64
}

/// A nap of `ms` milliseconds (the C's nanosleep in the connect wait and displayctl's pacing).
pub fn nap_ms(ms: u64) {
    std::thread::sleep(std::time::Duration::from_millis(ms));
}

// ---- unix sockets ----------------------------------------------------------------------------------------------

fn socket_address(path: &Path) -> Result<libc::sockaddr_un> {
    // SAFETY: sockaddr_un is plain data; all zeros is a valid value.
    let mut addr: libc::sockaddr_un = unsafe { zeroed() };
    addr.sun_family = libc::AF_UNIX as libc::sa_family_t;
    let bytes = path.as_os_str().as_encoded_bytes();
    if bytes.len() >= addr.sun_path.len() {
        return Err(Errno(libc::ENAMETOOLONG));
    }
    for (to, from) in addr.sun_path.iter_mut().zip(bytes) {
        *to = *from as libc::c_char;
    }
    Ok(addr)
}

fn seqpacket_socket() -> Result<OwnedFd> {
    // SAFETY: socket takes no pointers; a new descriptor is ours.
    let fd = check(unsafe { libc::socket(libc::AF_UNIX, libc::SOCK_SEQPACKET | libc::SOCK_CLOEXEC, 0) })?;
    // SAFETY: fd is a new descriptor nobody else owns.
    Ok(unsafe { OwnedFd::from_raw_fd(fd) })
}

/// One connect of a SOCK_SEQPACKET socket to `path`.
pub fn seqpacket_connect(path: &Path) -> Result<OwnedFd> {
    let addr = socket_address(path)?;
    let fd = seqpacket_socket()?;
    // SAFETY: addr is a valid sockaddr_un of the length given.
    check(unsafe {
        libc::connect(
            fd.as_raw_fd(),
            &addr as *const _ as *const libc::sockaddr,
            size_of::<libc::sockaddr_un>() as libc::socklen_t,
        )
    })?;
    Ok(fd)
}

/// A SOCK_SEQPACKET socket listening at `path` (whatever was there is removed first), backlog 2.
pub fn seqpacket_listen(path: &Path) -> Result<OwnedFd> {
    let addr = socket_address(path)?;
    let _ = std::fs::remove_file(path);
    let fd = seqpacket_socket()?;
    // SAFETY: addr is a valid sockaddr_un of the length given.
    check(unsafe {
        libc::bind(
            fd.as_raw_fd(),
            &addr as *const _ as *const libc::sockaddr,
            size_of::<libc::sockaddr_un>() as libc::socklen_t,
        )
    })?;
    // SAFETY: listen takes no pointers.
    check(unsafe { libc::listen(fd.as_raw_fd(), 2) })?;
    Ok(fd)
}

/// accept4 with SOCK_CLOEXEC.
pub fn accept(listener: BorrowedFd<'_>) -> Result<OwnedFd> {
    // SAFETY: accept4 may take null address pointers.
    let fd =
        retry(|| unsafe { libc::accept4(listener.as_raw_fd(), ptr::null_mut(), ptr::null_mut(), libc::SOCK_CLOEXEC) })?;
    // SAFETY: a new descriptor nobody else owns.
    Ok(unsafe { OwnedFd::from_raw_fd(fd) })
}

/// A connected pair of SOCK_SEQPACKET sockets (the tests' xnuhost and helper).
pub fn seqpacket_pair() -> Result<(OwnedFd, OwnedFd)> {
    let mut fds = [0 as RawFd; 2];
    // SAFETY: socketpair writes two descriptors into the array.
    check(unsafe { libc::socketpair(libc::AF_UNIX, libc::SOCK_SEQPACKET | libc::SOCK_CLOEXEC, 0, fds.as_mut_ptr()) })?;
    // SAFETY: two new descriptors nobody else owns.
    Ok(unsafe { (OwnedFd::from_raw_fd(fds[0]), OwnedFd::from_raw_fd(fds[1])) })
}

/// The most descriptors one message brings that are kept; any more are closed.
pub const MAX_FDS: usize = 4;

/// send (or sendmsg with SCM_RIGHTS when `fds` is not empty) of one packet, with MSG_NOSIGNAL; EINTR is retried.
pub fn send_packet(fd: BorrowedFd<'_>, bytes: &[u8], fds: &[BorrowedFd<'_>], flags: libc::c_int) -> Result<()> {
    assert!(fds.len() <= MAX_FDS);
    loop {
        match xh_core::fd::send_with_fds(fd, &[io::IoSlice::new(bytes)], fds, flags | libc::MSG_NOSIGNAL) {
            Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
            r => return r.map(drop).map_err(Errno::from),
        }
    }
}

/// What one recvmsg brought: its length (0: the peer closed), every descriptor that came up to MAX_FDS (the rest
/// were closed; ours to close), and whether MSG_TRUNC or MSG_CTRUNC was set.
pub use xh_core::fd::Received;

/// One recvmsg into `buf` with MSG_CMSG_CLOEXEC and `flags`, room for MAX_FDS descriptors as the C asked for (EINTR
/// is the caller's to retry).
pub fn recv_packet(fd: BorrowedFd<'_>, buf: &mut [u8], flags: libc::c_int) -> Result<Received> {
    Ok(xh_core::fd::recv_with_fds(fd, buf, MAX_FDS, flags)?)
}

/// poll of `fds` (fd, events) for `timeout_ms` (-1: no end); EINTR is retried when `retry`. The revents, or the error.
pub fn poll(fds: &[(BorrowedFd<'_>, i16)], timeout_ms: i32, retry: bool) -> Result<(usize, Vec<i16>)> {
    let mut p: Vec<libc::pollfd> =
        fds.iter().map(|(fd, events)| libc::pollfd { fd: fd.as_raw_fd(), events: *events, revents: 0 }).collect();
    loop {
        // SAFETY: p is a valid array of p.len() pollfds.
        let n = unsafe { libc::poll(p.as_mut_ptr(), p.len() as libc::nfds_t, timeout_ms) };
        if n >= 0 {
            return Ok((n as usize, p.iter().map(|x| x.revents).collect()));
        }
        let e = Errno::last();
        if !retry || e.0 != libc::EINTR {
            return Err(e);
        }
    }
}

// ---- memory ----------------------------------------------------------------------------------------------------

/// A shared map of a descriptor (MAP_SHARED), unmapped when dropped. The other side (xnuhost, the guest) may write the
/// memory while it is mapped here: the helper only copies bytes out, and what it reads at such a moment is guarded by
/// the protocol (the slot's seq, the flushes), as it was in C.
pub struct Map {
    ptr: *mut u8,
    len: usize,
    writable: bool,
}

// SAFETY: the map is plain memory owned by this value; nothing in it is tied to a thread.
unsafe impl Send for Map {}
// SAFETY: shared references only read it.
unsafe impl Sync for Map {}

impl Map {
    /// Maps `len` bytes of `fd` from offset 0, read-only or read-write.
    pub fn new(fd: BorrowedFd<'_>, len: usize, writable: bool) -> Result<Map> {
        if len == 0 {
            return Err(Errno(libc::EINVAL));
        }
        let prot = if writable { libc::PROT_READ | libc::PROT_WRITE } else { libc::PROT_READ };
        // SAFETY: a new mapping at an address the kernel picks; nothing else is touched.
        let p = unsafe { libc::mmap(ptr::null_mut(), len, prot, libc::MAP_SHARED, fd.as_raw_fd(), 0) };
        if p == libc::MAP_FAILED {
            return Err(Errno::last());
        }
        Ok(Map { ptr: p as *mut u8, len, writable })
    }

    pub fn len(&self) -> usize {
        self.len
    }

    pub fn is_empty(&self) -> bool {
        self.len == 0
    }

    pub fn as_ptr(&self) -> *const u8 {
        self.ptr
    }

    pub fn bytes(&self) -> &[u8] {
        // SAFETY: ptr..ptr+len is mapped for as long as self lives (see the type's comment on concurrent writers).
        unsafe { std::slice::from_raw_parts(self.ptr, self.len) }
    }

    pub fn bytes_mut(&mut self) -> &mut [u8] {
        assert!(self.writable, "a read-only map");
        // SAFETY: mapped read-write for as long as self lives, and &mut self makes this the only slice of it here.
        unsafe { std::slice::from_raw_parts_mut(self.ptr, self.len) }
    }
}

impl Drop for Map {
    fn drop(&mut self) {
        // SAFETY: the mapping made in new, not used after this.
        unsafe { libc::munmap(self.ptr as *mut libc::c_void, self.len) };
    }
}

/// fstat's size and whether it is a regular file.
pub fn file_size(fd: BorrowedFd<'_>) -> Result<(i64, bool)> {
    // SAFETY: fstat writes the stat it is given.
    let mut st: libc::stat = unsafe { zeroed() };
    // SAFETY: as above.
    check(unsafe { libc::fstat(fd.as_raw_fd(), &mut st) })?;
    Ok((st.st_size as i64, st.st_mode & libc::S_IFMT == libc::S_IFREG))
}

/// A memfd of `bytes` (MFD_CLOEXEC).
pub fn memfd(name: &str, bytes: usize) -> Result<OwnedFd> {
    let name = CString::new(name).map_err(|_| Errno(libc::EINVAL))?;
    let fd = xh_core::fd::memfd(&name, libc::MFD_CLOEXEC)?;
    // SAFETY: ftruncate takes no pointers.
    check(unsafe { libc::ftruncate(fd.as_raw_fd(), bytes as libc::off_t) })?;
    Ok(fd)
}

/// A pipe with O_CLOEXEC | O_NONBLOCK: (read end, write end).
pub fn pipe() -> Result<(OwnedFd, OwnedFd)> {
    let mut fds = [0 as RawFd; 2];
    // SAFETY: pipe2 writes two descriptors into the array.
    check(unsafe { libc::pipe2(fds.as_mut_ptr(), libc::O_CLOEXEC | libc::O_NONBLOCK) })?;
    // SAFETY: two new descriptors nobody else owns.
    Ok(unsafe { (OwnedFd::from_raw_fd(fds[0]), OwnedFd::from_raw_fd(fds[1])) })
}

/// The program's own path (/proc/self/exe).
pub fn self_exe() -> io::Result<std::path::PathBuf> {
    std::fs::read_link("/proc/self/exe")
}

/// access(path, mode) == 0.
pub fn access(path: &Path, mode: libc::c_int) -> Result<()> {
    let c = CString::new(path.as_os_str().as_encoded_bytes()).map_err(|_| Errno(libc::EINVAL))?;
    // SAFETY: c is a NUL-terminated path.
    check(unsafe { libc::access(c.as_ptr(), mode) }).map(|_| ())
}
