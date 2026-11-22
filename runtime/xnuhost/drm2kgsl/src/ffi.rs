// SPDX-License-Identifier: MIT
//! glue/oneios_glue.h and oneios_drm2kgsl.h (the patched renderer's own additions), declared by hand.
use libc::{c_char, c_int, c_uint, c_void, size_t};

pub type LineFn = extern "C" fn(level: c_int, line: *const c_char);
pub type RetireFn = extern "C" fn(ctx: u32, ring: u32, fence: u64);

pub use crate::ffi_caps::Caps;

extern "C" {
    pub fn oneios_d2k_init(on_line: LineFn, on_retire: RetireFn) -> c_int;
    pub fn oneios_d2k_probe(fd: c_int, capset: *mut c_void, max: size_t, caps: *mut Caps) -> c_int;
    pub fn oneios_d2k_context_exists(id: u32) -> bool;
    pub fn oneios_d2k_context_create(id: u32, fd: c_int, name: *const c_char, name_length: size_t) -> c_int;
    pub fn oneios_d2k_context_destroy(id: u32);
    pub fn oneios_d2k_submit(id: u32, commands: *const c_void, size: size_t) -> c_int;
    pub fn oneios_d2k_submit_fence(id: u32, ring: u32, fence: u64) -> c_int;
    pub fn oneios_d2k_set_class(id: u32, cls: c_uint, lowest: bool);
    pub fn oneios_d2k_resource_exists(res: u32) -> bool;
    pub fn oneios_d2k_resource_dmabuf(res: u32, fd: c_int, size: u64) -> c_int;
    pub fn oneios_d2k_resource_remove(res: u32);
    pub fn oneios_d2k_set_guest_blob_chunks(
        id: u32,
        blob_id: u64,
        fds: *const c_int,
        sizes: *const u64,
        n: c_uint,
    ) -> c_int;
    pub fn oneios_d2k_get_blob(id: u32, res: u32, blob_id: u64, size: u64, flags: u32) -> c_int;
    pub fn oneios_d2k_resource_map(res: u32, ptr: *mut *mut c_void, size: *mut u64, map_info: *mut u32) -> c_int;
    pub fn oneios_d2k_attach(id: u32, res: u32, on: bool, fds: *const c_int, sizes: *const u64, n: c_uint) -> c_int;
    pub fn oneios_d2k_unref(res: u32);
    pub fn oneios_d2k_policy_priority(cls: c_uint) -> c_uint;
    pub fn oneios_d2k_policy_class_name(cls: c_uint) -> *const c_char;

    pub fn drm2kgsl_renderer_hold_stats(holds: *mut u64, keepalives: *mut u64);
    pub fn drm2kgsl_renderer_set_level(top: c_int);
    pub fn drm2kgsl_renderer_import_stats(
        imports: *mut u64,
        checked: *mut u64,
        kept: *mut u64,
        dropped: *mut u64,
        plain: *mut u64,
        unreadable: *mut u64,
    );
}
