// SPDX-License-Identifier: MIT
//! Small system helpers: errno and its C text.
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn texts() {
        assert_eq!(strerror(libc::ENOENT), "No such file or directory");
        assert_eq!(error_text(&io::Error::from_raw_os_error(libc::EACCES)), "Permission denied");
        assert_eq!(error_text(&io::Error::other("odd")), "odd");
    }
}
