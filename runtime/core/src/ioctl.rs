// SPDX-License-Identifier: MIT
//! ioctl numbers and the call: Linux's `_IOC`/`_IO`/`_IOR`/`_IOW`/`_IOWR` (asm-generic/ioctl.h, as on arm64 and
//! x86_64), and [`ioctl`], the one system call wrapper the bridges, the display and the GPU tools share.
use std::io;
use std::os::fd::RawFd;

/// `_IOC_NONE`, `_IOC_WRITE`, `_IOC_READ`: the direction bits (user space's view).
pub const NONE: u32 = 0;
pub const WRITE: u32 = 1;
pub const READ: u32 = 2;

/// `_IOC(dir, type, nr, size)`.
pub const fn ioc(dir: u32, ty: u8, nr: u8, size: usize) -> u32 {
    dir << 30 | (size as u32) << 16 | (ty as u32) << 8 | nr as u32
}

/// `_IO(type, nr)`.
pub const fn io(ty: u8, nr: u8) -> u32 {
    ioc(NONE, ty, nr, 0)
}

/// `_IOR(type, nr, size)`: the kernel writes `size` bytes.
pub const fn ior(ty: u8, nr: u8, size: usize) -> u32 {
    ioc(READ, ty, nr, size)
}

/// `_IOW(type, nr, size)`: the kernel reads `size` bytes.
pub const fn iow(ty: u8, nr: u8, size: usize) -> u32 {
    ioc(WRITE, ty, nr, size)
}

/// `_IOWR(type, nr, size)`.
pub const fn iowr(ty: u8, nr: u8, size: usize) -> u32 {
    ioc(READ | WRITE, ty, nr, size)
}

/// `_IOC_SIZE(request)`: the argument's size a number encodes.
pub const fn size(request: u32) -> usize {
    ((request >> 16) & 0x3fff) as usize
}

/// `ioctl(fd, request, arg)`: its result (0, or what the request returns: a new descriptor, a count), or the errno of
/// its -1.
///
/// # Safety
/// `arg` is what `request` takes: a pointer to memory of the size and layout the request encodes (and every pointer
/// inside it valid as the request uses it) that stays alive and unaliased for the call; or, for a request that takes
/// its argument by value, that value cast to a pointer.
pub unsafe fn ioctl<T>(fd: RawFd, request: u32, arg: *mut T) -> io::Result<libc::c_int> {
    // SAFETY: the caller's contract. (The request's C type is `unsigned long` on glibc, `int` on bionic.)
    let rc = unsafe { libc::ioctl(fd, request as _, arg) };
    if rc < 0 {
        Err(io::Error::last_os_error())
    } else {
        Ok(rc)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn numbers() {
        // Values from the kernel's headers.
        assert_eq!(iowr(b'b', 1, 48), 0xc030_6201); // BINDER_WRITE_READ
        assert_eq!(iow(b'E', 0x90, 4), 0x4004_4590); // EVIOCGRAB
        assert_eq!(ior(b'E', 0x06, 80), 0x8050_4506); // EVIOCGNAME(80)
        assert_eq!(io(b'd', 0x1e), 0x641e); // DRM_IOCTL_SET_MASTER
        assert_eq!(iow(b'u', 0x42, 24), 0x4018_7542); // UDMABUF_CREATE
        assert_eq!(size(iowr(b'b', 1, 48)), 48);
        let mut n = 0i32;
        // SAFETY: -1 is never a descriptor; the call fails with EBADF before it looks at the argument.
        let e = unsafe { ioctl(-1, io(b'T', 1), &mut n) }.unwrap_err();
        assert_eq!(e.raw_os_error(), Some(libc::EBADF));
    }
}
