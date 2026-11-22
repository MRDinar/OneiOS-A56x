// SPDX-License-Identifier: MIT
//! open(2) and ioctl(2) of the GPU's device nodes (/dev/kgsl-3d0, /dev/udmabuf, /dev/dma_heap/system), for the Rust
//! side (xnuhost's gpu3d, gpu3d_kgsl) and, in host builds, also for the renderer's C (glue/oneios_sys.h). On the build
//! host a test points both at its stand-in for KGSL with [`set_hooks`], as test_drm2kgsl.c did with
//! -Wl,--wrap=open,--wrap=ioctl; on Android they are the system calls, with no table between.
use std::ffi::CStr;
use std::io;
use std::os::fd::{FromRawFd, OwnedFd, RawFd};

use libc::{c_char, c_int, c_uint, c_ulong, c_void};

/// A test's open: (path, flags, mode) -> a descriptor or -1 with errno.
pub type OpenHook = extern "C" fn(path: *const c_char, flags: c_int, mode: c_uint) -> c_int;
/// A test's ioctl: (fd, request, arg) -> the result, -1 with errno.
pub type IoctlHook = extern "C" fn(fd: c_int, request: c_ulong, arg: *mut c_void) -> c_int;

#[cfg(not(target_os = "android"))]
extern "C" {
    fn oneios_sys_set_hooks(open: Option<OpenHook>, ioctl: Option<IoctlHook>);
    fn oneios_sys_open(path: *const c_char, flags: c_int, ...) -> c_int;
    fn oneios_sys_ioctl(fd: c_int, request: c_ulong, ...) -> c_int;
}

/// Points every open and ioctl of the GPU path at these (None: the system's). Host builds only.
#[cfg(not(target_os = "android"))]
pub fn set_hooks(open: Option<OpenHook>, ioctl: Option<IoctlHook>) {
    // SAFETY: stores two function pointers (or nulls) in the C side's table.
    unsafe { oneios_sys_set_hooks(open, ioctl) }
}

/// open(2) through the hooks: an owned descriptor, or the OS error.
pub fn open(path: &CStr, flags: c_int) -> io::Result<OwnedFd> {
    // SAFETY: path is NUL-terminated; no O_CREAT, so no mode argument is read.
    #[cfg(not(target_os = "android"))]
    let fd = unsafe { oneios_sys_open(path.as_ptr(), flags, 0 as c_uint) };
    // SAFETY: as above.
    #[cfg(target_os = "android")]
    let fd = unsafe { libc::open(path.as_ptr(), flags) };
    if fd < 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: a new descriptor that nothing else owns.
    Ok(unsafe { OwnedFd::from_raw_fd(fd) })
}

/// ioctl(2) through the hooks.
///
/// # Safety
/// `arg` must be what `request` expects of it (the kernel's or the hook's reads and writes through it).
pub unsafe fn ioctl(fd: RawFd, request: c_ulong, arg: *mut c_void) -> c_int {
    // SAFETY: the caller's contract.
    #[cfg(not(target_os = "android"))]
    let r = unsafe { oneios_sys_ioctl(fd, request, arg) };
    // SAFETY: the caller's contract. (bionic's request type is c_int.)
    #[cfg(target_os = "android")]
    let r = unsafe { libc::ioctl(fd, request as _, arg) };
    r
}
