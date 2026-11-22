// SPDX-License-Identifier: MIT
//! The renderer's calls (glue/oneios_glue.c), each under the front's one lock.
use std::ffi::CStr;
use std::os::fd::{AsRawFd, BorrowedFd, FromRawFd, IntoRawFd, OwnedFd, RawFd};
use std::ptr::NonNull;
use std::sync::{Mutex, MutexGuard, OnceLock};

use libc::{c_char, c_int, c_void};

use crate::ffi;
use crate::Errno;

/// What the probe found (the capset's msm part).
pub use ffi::Caps;

/// A log line of the renderer: level 0 error, 1 info, 2 debug; "drm2kgsl: " and the text.
pub type LineFn = fn(level: u8, line: &str);
/// A context's `ring` passed `fence` (and every earlier fence of the ring). From any thread.
pub type RetireFn = fn(ctx: u32, ring: u32, fence: u64);

static SINKS: OnceLock<(LineFn, RetireFn)> = OnceLock::new();
static LOCK: Mutex<()> = Mutex::new(());

fn lock() -> MutexGuard<'static, ()> {
    LOCK.lock().unwrap_or_else(|e| e.into_inner())
}

extern "C" fn on_line(level: c_int, line: *const c_char) {
    if let (Some(&(f, _)), false) = (SINKS.get(), line.is_null()) {
        // SAFETY: the glue passes a NUL-terminated buffer of its own.
        let text = unsafe { CStr::from_ptr(line) }.to_string_lossy();
        f(level.clamp(0, 2) as u8, &text);
    }
}

extern "C" fn on_retire(ctx: u32, ring: u32, fence: u64) {
    if let Some(&(_, f)) = SINKS.get() {
        f(ctx, ring, fence)
    }
}

/// The renderer's tables and log, once per process: `ENOMEM` when the tables could not be made, `EEXIST` when it was
/// already started.
pub fn init(line: LineFn, retire: RetireFn) -> Result<(), Errno> {
    SINKS.set((line, retire)).map_err(|_| Errno(libc::EEXIST))?;
    let _g = lock();
    // SAFETY: two extern "C" functions with the glue's signatures.
    Errno::check(unsafe { ffi::oneios_d2k_init(on_line, on_retire) })
}

/// The DRM capset a guest gets (`virgl_renderer_capset_drm`, its bytes) and its msm figures.
#[derive(Clone)]
pub struct Capset {
    bytes: [u8; 256],
    len: usize,
    /// The figures the log line names.
    pub caps: Caps,
}

impl Capset {
    /// The capset's bytes, as the guest reads them.
    pub fn bytes(&self) -> &[u8] {
        &self.bytes[..self.len]
    }
}

/// drm2kgsl_renderer_probe on a KGSL descriptor (the renderer keeps its own for the VBO).
pub fn probe(fd: BorrowedFd<'_>) -> Result<Capset, Errno> {
    let mut c = Capset { bytes: [0; 256], len: 0, caps: Caps::default() };
    let _g = lock();
    // SAFETY: the buffer and the summary are ours, of the sizes given.
    let r = unsafe { ffi::oneios_d2k_probe(fd.as_raw_fd(), c.bytes.as_mut_ptr().cast(), c.bytes.len(), &mut c.caps) };
    Errno::check(r)?;
    c.len = r as usize;
    Ok(c)
}

/// Whether context `id` exists.
pub fn context_exists(id: u32) -> bool {
    let _g = lock();
    // SAFETY: a lookup by id.
    unsafe { ffi::oneios_d2k_context_exists(id) }
}

/// A context on a KGSL descriptor (it owns `fd` from now on, also when this fails), named as the guest names it.
pub fn context_create(id: u32, fd: OwnedFd, name: &[u8]) -> Result<(), Errno> {
    let _g = lock();
    // SAFETY: the name's bytes and length; the fd is handed over.
    Errno::check(unsafe { ffi::oneios_d2k_context_create(id, fd.into_raw_fd(), name.as_ptr().cast(), name.len()) })
}

/// The context goes (virgl_context_remove).
pub fn context_destroy(id: u32) {
    let _g = lock();
    // SAFETY: a removal by id.
    unsafe { ffi::oneios_d2k_context_destroy(id) }
}

/// The context's commands (whole 32-bit words: the renderer reads them in place).
pub fn submit(id: u32, commands: &[u32]) -> Result<(), Errno> {
    let _g = lock();
    // SAFETY: the words and their byte length.
    Errno::check(unsafe { ffi::oneios_d2k_submit(id, commands.as_ptr().cast(), std::mem::size_of_val(commands)) })
}

/// A mergeable fence on `ring` of context `id`: retired later through the retire callback, or from inside this call.
pub fn submit_fence(id: u32, ring: u32, fence: u64) -> Result<(), Errno> {
    let _g = lock();
    // SAFETY: plain values.
    Errno::check(unsafe { ffi::oneios_d2k_submit_fence(id, ring, fence) })
}

/// patch 0008: the context's class (oneios_gpu_policy.h enum og_class) and whether its queues carry KGSL's lowest level.
pub fn set_class(id: u32, cls: u32, lowest: bool) {
    let _g = lock();
    // SAFETY: plain values.
    unsafe { ffi::oneios_d2k_set_class(id, cls, lowest) }
}

/// Whether resource `res` exists.
pub fn resource_exists(res: u32) -> bool {
    let _g = lock();
    // SAFETY: a lookup by id.
    unsafe { ffi::oneios_d2k_resource_exists(res) }
}

/// A resource that is this dma-buf (a display surface: cached, `size` bytes). It owns `fd` when this succeeds; when it
/// fails the fd is closed.
pub fn resource_dmabuf(res: u32, fd: OwnedFd, size: u64) -> Result<(), Errno> {
    let _g = lock();
    let raw = fd.into_raw_fd();
    // SAFETY: the fd is handed over on success.
    let r = unsafe { ffi::oneios_d2k_resource_dmabuf(res, raw, size) };
    if r < 0 {
        // SAFETY: the glue left it the caller's.
        drop(unsafe { OwnedFd::from_raw_fd(raw) });
    }
    Errno::check(r)
}

/// The resource goes from the table (no detach from the contexts: [`unref`] does that).
pub fn resource_remove(res: u32) {
    let _g = lock();
    // SAFETY: a removal by id.
    unsafe { ffi::oneios_d2k_resource_remove(res) }
}

fn split(chunks: &[(RawFd, u64)]) -> (Vec<c_int>, Vec<u64>) {
    chunks.iter().copied().unzip()
}

/// patch 0001: a guest BO's pages as dma-bufs in the blob's order, parked for the [`get_blob`] that follows. Takes
/// every fd, also when it fails.
pub fn set_guest_blob_chunks(id: u32, blob_id: u64, chunks: Vec<(OwnedFd, u64)>) -> Result<(), Errno> {
    let (fds, sizes): (Vec<c_int>, Vec<u64>) = chunks.into_iter().map(|(f, s)| (f.into_raw_fd(), s)).unzip();
    let _g = lock();
    // SAFETY: n fds (handed over) and n sizes.
    Errno::check(unsafe {
        ffi::oneios_d2k_set_guest_blob_chunks(id, blob_id, fds.as_ptr(), sizes.as_ptr(), fds.len() as u32)
    })
}

/// get_blob and the resource made from it (its fd, mapping, cache mode).
pub fn get_blob(id: u32, res: u32, blob_id: u64, size: u64, flags: u32) -> Result<(), Errno> {
    let _g = lock();
    // SAFETY: plain values.
    Errno::check(unsafe { ffi::oneios_d2k_get_blob(id, res, blob_id, size, flags) })
}

/// A resource's host mapping: where it is (None: no mapping), its size and its cache mode (map_info).
pub fn resource_map(res: u32) -> Option<(Option<NonNull<u8>>, u64, u32)> {
    let (mut ptr, mut size, mut info) = (std::ptr::null_mut::<c_void>(), 0u64, 0u32);
    let _g = lock();
    // SAFETY: three outputs of ours.
    let r = unsafe { ffi::oneios_d2k_resource_map(res, &mut ptr, &mut size, &mut info) };
    (r == 0).then(|| (NonNull::new(ptr.cast()), size, info))
}

/// Attaches resource `res` to context `id` (or detaches it). `chunks`: the dma-bufs of a resource of several, which
/// the context imports each (they stay the caller's).
pub fn attach(id: u32, res: u32, on: bool, chunks: &[(BorrowedFd<'_>, u64)]) -> Result<(), Errno> {
    let raw: Vec<(RawFd, u64)> = chunks.iter().map(|(f, s)| (f.as_raw_fd(), *s)).collect();
    let (fds, sizes) = split(&raw);
    let _g = lock();
    // SAFETY: n fds (borrowed) and n sizes.
    Errno::check(unsafe { ffi::oneios_d2k_attach(id, res, on, fds.as_ptr(), sizes.as_ptr(), fds.len() as u32) })
}

/// The resource is detached from every context and goes.
pub fn unref(res: u32) {
    let _g = lock();
    // SAFETY: by id.
    unsafe { ffi::oneios_d2k_unref(res) }
}

/// patch 0005: how often the render server's clock hold started, and its keep-alive submissions, since the start.
pub fn hold_stats() -> (u64, u64) {
    let (mut holds, mut keepalives) = (0, 0);
    // SAFETY: two outputs of ours. The renderer reads them under its own lock (no front lock: a retire callback calls this).
    unsafe { ffi::drm2kgsl_renderer_hold_stats(&mut holds, &mut keepalives) };
    (holds, keepalives)
}

/// patch 0008: the GPU's level from xnuhost's policy: the top (true) or the governor's.
pub fn set_level(top: bool) {
    // SAFETY: a plain value (atomics and its own lock in the renderer).
    unsafe { ffi::drm2kgsl_renderer_set_level(top as c_int) }
}

/// patch 0007: KGSL's imports and the coherence read back of them.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ImportStats {
    pub imports: u64,
    pub checked: u64,
    pub kept: u64,
    pub dropped: u64,
    pub plain: u64,
    pub unreadable: u64,
}

/// patch 0007's counters since the start.
pub fn import_stats() -> ImportStats {
    let mut s = ImportStats::default();
    // SAFETY: six outputs of ours (read under the renderer's own lock).
    unsafe {
        ffi::drm2kgsl_renderer_import_stats(
            &mut s.imports,
            &mut s.checked,
            &mut s.kept,
            &mut s.dropped,
            &mut s.plain,
            &mut s.unreadable,
        )
    };
    s
}

/// oneios_gpu_policy.h's og_kgsl_priority, as the patched renderer has it (for the test that Rust's policy agrees).
pub fn policy_priority(cls: u32) -> u32 {
    // SAFETY: a pure function.
    unsafe { ffi::oneios_d2k_policy_priority(cls) }
}

/// oneios_gpu_policy.h's og_class_name, as the patched renderer has it.
pub fn policy_class_name(cls: u32) -> String {
    // SAFETY: a static string of the header's.
    unsafe { CStr::from_ptr(ffi::oneios_d2k_policy_class_name(cls)) }.to_string_lossy().into_owned()
}
