// SPDX-License-Identifier: MIT
//! Small system helpers: errno, its C text and the [`Errno`] type, the page size.
use std::fmt;
use std::io;

/// The calling thread's errno.
pub fn errno() -> i32 {
    io::Error::last_os_error().raw_os_error().unwrap_or(0)
}

/// C's `strerror(errno)` text ("No such file or directory"), as the C runtime logged it (Rust's `io::Error` adds
/// " (os error N)").
pub fn strerror(errno: i32) -> String {
    let mut buf = [0 as libc::c_char; 128];
    // SAFETY: a valid buffer of its length; the POSIX strerror_r writes a NUL-terminated text into it.
    let rc = unsafe { libc::strerror_r(errno, buf.as_mut_ptr(), buf.len()) };
    if rc != 0 {
        return format!("Unknown error {errno}");
    }
    // SAFETY: strerror_r succeeded, so `buf` holds a NUL-terminated string.
    unsafe { std::ffi::CStr::from_ptr(buf.as_ptr()) }.to_string_lossy().into_owned()
}

/// The C text of an `io::Error`: [`strerror`] of its errno, else its own text.
pub fn error_text(e: &io::Error) -> String {
    match e.raw_os_error() {
        Some(n) => strerror(n),
        None => e.to_string(),
    }
}

/// Sets the calling thread's errno (a C caller reads it after a -1 of ours; the tests' stand-ins of a system call).
pub fn set_errno(errno: i32) {
    // SAFETY: the location of this thread's errno, always valid to write.
    unsafe {
        #[cfg(target_os = "android")]
        let at = libc::__errno();
        #[cfg(not(target_os = "android"))]
        let at = libc::__errno_location();
        *at = errno;
    }
}

/// An errno value (positive: `EINVAL`, `ENOMEM`, ...), what the C functions returned as -errno. Its text is
/// strerror's, as the C logged it.
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub struct Errno(pub i32);

impl Errno {
    /// The errno of a C call's negative return (`-EINVAL` -> `EINVAL`); 0 and positive values are `Ok`.
    pub fn check(r: i32) -> Result<(), Errno> {
        if r < 0 {
            Err(Errno(-r))
        } else {
            Ok(())
        }
    }

    /// The calling thread's errno as it is now (EIO if there is none).
    pub fn last() -> Errno {
        Errno(io::Error::last_os_error().raw_os_error().unwrap_or(libc::EIO))
    }
}

impl fmt::Display for Errno {
    /// strerror's text, the platform's own (the C's `%s` of strerror(errno)).
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&strerror(self.0))
    }
}

impl fmt::Debug for Errno {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Errno({}: {})", self.0, self)
    }
}

impl From<io::Error> for Errno {
    fn from(e: io::Error) -> Errno {
        Errno(e.raw_os_error().unwrap_or(libc::EIO))
    }
}

impl From<Errno> for io::Error {
    fn from(e: Errno) -> io::Error {
        io::Error::from_raw_os_error(e.0)
    }
}

impl std::error::Error for Errno {}

/// The host's page size (sysconf _SC_PAGESIZE).
pub fn page_size() -> u64 {
    // SAFETY: sysconf takes no pointers.
    unsafe { libc::sysconf(libc::_SC_PAGESIZE) as u64 }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn texts() {
        assert_eq!(strerror(libc::ENOENT), "No such file or directory");
        assert_eq!(error_text(&io::Error::from_raw_os_error(libc::EACCES)), "Permission denied");
        assert_eq!(error_text(&io::Error::other("odd")), "odd");
        assert_eq!(Errno(libc::ENOENT).to_string(), "No such file or directory");
        assert_eq!(format!("{:?}", Errno(libc::EACCES)), "Errno(13: Permission denied)");
        assert_eq!(Errno::check(-libc::EINVAL), Err(Errno(libc::EINVAL)));
        assert_eq!(Errno::check(3), Ok(()));
        set_errno(libc::EAGAIN);
        assert_eq!((errno(), Errno::last()), (libc::EAGAIN, Errno(libc::EAGAIN)));
        assert!(page_size().is_power_of_two() && page_size() >= 4096);
    }
}
