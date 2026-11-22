// SPDX-License-Identifier: MIT
//! ffi.rs's functions in a build without external/virglrenderer: each says ENODEV (or gives nothing), so the front and
//! xnuhost's gpu3d_kgsl build the same either way and `gpu3d::kgsl_open` refuses before it calls any of them.
#![allow(clippy::missing_safety_doc)]
use libc::{c_char, c_int, c_uint, c_void, size_t};

pub type LineFn = extern "C" fn(level: c_int, line: *const c_char);
pub type RetireFn = extern "C" fn(ctx: u32, ring: u32, fence: u64);
pub use crate::ffi_caps::Caps;

const NO: c_int = -libc::ENODEV;
pub unsafe fn oneios_d2k_init(_: LineFn, _: RetireFn) -> c_int {
    NO
}
pub unsafe fn oneios_d2k_probe(_: c_int, _: *mut c_void, _: size_t, _: *mut Caps) -> c_int {
    NO
}
pub unsafe fn oneios_d2k_context_exists(_: u32) -> bool {
    false
}
pub unsafe fn oneios_d2k_context_create(_: u32, fd: c_int, _: *const c_char, _: size_t) -> c_int {
    // SAFETY: the descriptor was handed over (as to the renderer, which owns it also when it fails).
    unsafe { libc::close(fd) };
    NO
}
pub unsafe fn oneios_d2k_context_destroy(_: u32) {}
pub unsafe fn oneios_d2k_submit(_: u32, _: *const c_void, _: size_t) -> c_int {
    NO
}
pub unsafe fn oneios_d2k_submit_fence(_: u32, _: u32, _: u64) -> c_int {
    NO
}
pub unsafe fn oneios_d2k_set_class(_: u32, _: c_uint, _: bool) {}
pub unsafe fn oneios_d2k_resource_exists(_: u32) -> bool {
    false
}
pub unsafe fn oneios_d2k_resource_dmabuf(_: u32, _: c_int, _: u64) -> c_int {
    NO
}
pub unsafe fn oneios_d2k_resource_remove(_: u32) {}
pub unsafe fn oneios_d2k_set_guest_blob_chunks(_: u32, _: u64, fds: *const c_int, _: *const u64, n: c_uint) -> c_int {
    for i in 0..n as usize {
        // SAFETY: n descriptors handed over.
        unsafe { libc::close(*fds.add(i)) };
    }
    NO
}
pub unsafe fn oneios_d2k_get_blob(_: u32, _: u32, _: u64, _: u64, _: u32) -> c_int {
    NO
}
pub unsafe fn oneios_d2k_resource_map(_: u32, _: *mut *mut c_void, _: *mut u64, _: *mut u32) -> c_int {
    NO
}
pub unsafe fn oneios_d2k_attach(_: u32, _: u32, _: bool, _: *const c_int, _: *const u64, _: c_uint) -> c_int {
    NO
}
pub unsafe fn oneios_d2k_unref(_: u32) {}
pub unsafe fn oneios_d2k_policy_priority(_: c_uint) -> c_uint {
    0
}
pub unsafe fn oneios_d2k_policy_class_name(_: c_uint) -> *const c_char {
    c"".as_ptr()
}
pub unsafe fn drm2kgsl_renderer_hold_stats(_: *mut u64, _: *mut u64) {}
pub unsafe fn drm2kgsl_renderer_set_level(_: c_int) {}
pub unsafe fn drm2kgsl_renderer_import_stats(
    _: *mut u64,
    _: *mut u64,
    _: *mut u64,
    _: *mut u64,
    _: *mut u64,
    _: *mut u64,
) {
}
