// SPDX-License-Identifier: MIT
//! A stand-in for KGSL's memory half (objects, their GPU addresses, VBO binds), which is where the Galaxy S24 Ultra's
//! kernel differs from what drm2kgsl assumed, behind drm2kgsl-sys's open/ioctl hooks: requests of KGSL's ioctl type
//! go to the stand-in, the rest (the real /dev/udmabuf of this host) to the kernel. The stand-in keeps what Samsung's
//! graphics-kernel keeps:
//! - GPU addresses for a 64-bit process's objects come from 0x4000000000..0x8000000000 (KGSL_IOMMU_VA_BASE64/END64),
//!   first fit; a VBO takes its whole size there, and every imported or allocated object takes its own size there too
//!   (kgsl_mem_entry_attach_to_process -> kgsl_mmu_get_gpuaddr), ENOMEM when nothing is left;
//! - a VBO may be as large as the range (gpumem_max_va_size), not larger (EINVAL);
//! - BIND_RANGES takes children of the same process, whole pages, inside the child and inside the VBO.
//!
//! Without this host's /dev/udmabuf (root:kvm on a PC) a stand-in for it too, for the scenarios that never read pages
//! through it: UDMABUF_CREATE_LIST gives a memfd of the list's size, not the guest's pages. msm_kgsl.h's structures are
//! mirrored here; `layout_matches_msm_kgsl_h` checks them against the header.
use std::ffi::CStr;
use std::sync::{Mutex, MutexGuard};

use libc::{c_char, c_int, c_uint, c_ulong, c_void};

pub const VA_BASE: u64 = 0x40_0000_0000;
pub const VA_END: u64 = 0x80_0000_0000;

// msm_kgsl.h
pub const IOC_TYPE: u64 = 0x09;
pub const NR_GETPROPERTY: u64 = 0x2;
pub const NR_WAITTIMESTAMP_CTXTID: u64 = 0x7;
pub const NR_DRAWCTXT_CREATE: u64 = 0x13;
pub const NR_DRAWCTXT_DESTROY: u64 = 0x14;
pub const NR_SETPROPERTY: u64 = 0x32;
pub const NR_TIMESTAMP_EVENT: u64 = 0x33;
pub const NR_GPUOBJ_ALLOC: u64 = 0x45;
pub const NR_GPUOBJ_FREE: u64 = 0x46;
pub const NR_GPUOBJ_INFO: u64 = 0x47;
pub const NR_GPUOBJ_IMPORT: u64 = 0x48;
pub const NR_GPU_COMMAND: u64 = 0x4a;
pub const NR_GPUMEM_BIND_RANGES: u64 = 0x56;
pub const PROP_DEVICE_INFO: u32 = 0x1;
pub const PROP_PWR_CONSTRAINT: u32 = 0x12;
pub const MEMFLAGS_VBO: u64 = 1 << 34;
pub const MEMFLAGS_IOCOHERENT: u64 = 1 << 31;
pub const USER_MEM_TYPE_DMABUF: u32 = 3;
pub const RANGE_OP_BIND: u32 = 1;
pub const RANGE_OP_UNBIND: u32 = 2;
pub const CONTEXT_PRIORITY_MASK: u32 = 0xf000;
pub const CONTEXT_PRIORITY_SHIFT: u32 = 12;
pub const CONTEXT_PWR_CONSTRAINT: u32 = 0x800;
pub const TIMESTAMP_EVENT_FENCE: i32 = 2;
const UDMABUF_CREATE_LIST: c_ulong = 0x4008_7543;

#[repr(C)]
pub struct GetProperty {
    pub kind: u32,
    pub value: *mut c_void,
    pub sizebytes: usize,
}
#[repr(C)]
#[derive(Default)]
pub struct DevInfo {
    pub device_id: u32,
    pub chip_id: u32,
    pub mmu_enabled: u32,
    pub gmem_gpubaseaddr: u64,
    pub gpu_id: u32,
    pub gmem_sizebytes: usize,
}
#[repr(C)]
pub struct GpuobjAlloc {
    pub size: u64,
    pub flags: u64,
    pub va_len: u64,
    pub mmapsize: u64,
    pub id: u32,
    pub metadata_len: u32,
    pub metadata: u64,
}
#[repr(C)]
pub struct GpuobjInfo {
    pub gpuaddr: u64,
    pub flags: u64,
    pub size: u64,
    pub va_len: u64,
    pub va_addr: u64,
    pub id: u32,
}
#[repr(C)]
pub struct GpuobjImport {
    pub private: u64,
    pub priv_len: u64,
    pub flags: u64,
    pub kind: u32,
    pub id: u32,
}
#[repr(C)]
pub struct ImportDmaBuf {
    pub fd: c_int,
}
#[repr(C)]
pub struct GpuobjFree {
    pub flags: u64,
    pub private: u64,
    pub id: u32,
    pub kind: u32,
    pub len: u32,
}
#[repr(C)]
pub struct BindRanges {
    pub ranges: u64,
    pub ranges_nents: u32,
    pub ranges_size: u32,
    pub id: u32,
    pub flags: u32,
    pub fence_id: c_int,
    pub padding: u32,
}
#[repr(C)]
pub struct BindRange {
    pub child_offset: u64,
    pub target_offset: u64,
    pub length: u64,
    pub child_id: u32,
    pub op: u32,
}
#[repr(C)]
pub struct DeviceConstraint {
    pub kind: u32,
    pub context_id: u32,
    pub data: *mut c_void,
    pub size: usize,
}
#[repr(C)]
pub struct ConstraintPwrlevel {
    pub level: u32,
}
#[repr(C)]
pub struct DrawctxtCreate {
    pub flags: u32,
    pub drawctxt_id: u32,
}
#[repr(C)]
pub struct TimestampEvent {
    pub kind: c_int,
    pub timestamp: u32,
    pub context_id: u32,
    pub private: *mut c_void,
    pub len: usize,
}
#[repr(C)]
pub struct TimestampEventFence {
    pub fence_fd: c_int,
}

/// What glue/oneios_kgsl_layout.c lists, in its order, from the mirror above.
pub fn layout() -> Vec<u64> {
    use std::mem::{offset_of, size_of};
    let v: Vec<usize> = vec![
        size_of::<GetProperty>(),
        size_of::<DevInfo>(),
        size_of::<GpuobjAlloc>(),
        size_of::<GpuobjInfo>(),
        size_of::<GpuobjImport>(),
        size_of::<ImportDmaBuf>(),
        size_of::<GpuobjFree>(),
        size_of::<BindRanges>(),
        size_of::<BindRange>(),
        size_of::<DeviceConstraint>(),
        size_of::<ConstraintPwrlevel>(),
        size_of::<DrawctxtCreate>(),
        size_of::<TimestampEvent>(),
        size_of::<TimestampEventFence>(),
        offset_of!(GetProperty, value),
        offset_of!(GetProperty, sizebytes),
        offset_of!(DevInfo, chip_id),
        offset_of!(DevInfo, mmu_enabled),
        offset_of!(DevInfo, gpu_id),
        offset_of!(DevInfo, gmem_sizebytes),
        offset_of!(GpuobjAlloc, id),
        offset_of!(GpuobjInfo, size),
        offset_of!(GpuobjInfo, id),
        offset_of!(GpuobjImport, kind),
        offset_of!(GpuobjImport, id),
        offset_of!(GpuobjFree, id),
        offset_of!(BindRanges, id),
        offset_of!(BindRange, child_id),
        offset_of!(DeviceConstraint, data),
        offset_of!(TimestampEvent, private),
    ];
    let mut out: Vec<u64> = v.into_iter().map(|x| x as u64).collect();
    out.extend([
        IOC_TYPE,
        NR_GETPROPERTY,
        NR_WAITTIMESTAMP_CTXTID,
        NR_DRAWCTXT_CREATE,
        NR_DRAWCTXT_DESTROY,
        NR_SETPROPERTY,
        NR_TIMESTAMP_EVENT,
        NR_GPUOBJ_ALLOC,
        NR_GPUOBJ_FREE,
        NR_GPUOBJ_INFO,
        NR_GPUOBJ_IMPORT,
        NR_GPU_COMMAND,
        NR_GPUMEM_BIND_RANGES,
        PROP_DEVICE_INFO as u64,
        PROP_PWR_CONSTRAINT as u64,
        MEMFLAGS_VBO,
        MEMFLAGS_IOCOHERENT,
        USER_MEM_TYPE_DMABUF as u64,
        RANGE_OP_BIND as u64,
        RANGE_OP_UNBIND as u64,
        CONTEXT_PRIORITY_MASK as u64,
        CONTEXT_PRIORITY_SHIFT as u64,
        CONTEXT_PWR_CONSTRAINT as u64,
        TIMESTAMP_EVENT_FENCE as u64,
    ]);
    out
}

/// The GPU the stand-in describes. The S25's Adreno 830 reports chip 0x44050000, gpu_id 6850, 12 MiB gmem and 192 GiB
/// of GPU VA (the S25 Ultra's gpuctl); its VA range is modelled as the same base, 192 GiB long.
#[derive(Clone, Copy)]
pub struct Profile {
    pub chip_id: u32,
    pub gpu_id: u32,
    pub gmem: u32,
    pub va_base: u64,
    pub va_end: u64,
}

pub const A750: Profile =
    Profile { chip_id: 0x4305_1400, gpu_id: 6770, gmem: 3072 << 10, va_base: VA_BASE, va_end: VA_END };
pub const A830: Profile =
    Profile { chip_id: 0x4405_0000, gpu_id: 6850, gmem: 12288 << 10, va_base: VA_BASE, va_end: VA_BASE + (192 << 30) };

#[derive(Clone, Copy, Debug)]
pub struct Object {
    pub id: u32,
    pub base: u64,
    pub size: u64,
    pub flags: u64,
    pub vbo: bool,
}

// Every field the C stand-in recorded is kept, read or not.
#[allow(dead_code)]
#[derive(Clone, Copy, Debug)]
pub struct Bound {
    pub vbo: u32,
    pub child: u32,
    pub op: u32,
    pub target: u64,
    pub length: u64,
    pub child_offset: u64,
    pub call: u32,
}

#[allow(dead_code)]
#[derive(Clone, Copy, Debug)]
pub struct Constraint {
    pub context: u32,
    pub kind: u32,
    pub level: u32,
}

/// The stand-in's state: objects, binds, what the renderer asked.
pub struct Stand {
    pub profile: Profile,
    pub objects: Vec<Object>,
    pub next_id: u32,
    pub imports: u32,
    pub frees: u32,
    /// As KGSL on an MMU without IO coherence (kgsl_memdesc_init): an import keeps every flag it asked but that one.
    pub no_io_coherence: bool,
    pub binds: Vec<Bound>,
    pub bind_calls: u32,
    /// GPU_COMMANDs the renderer sent and TIMESTAMP_EVENT(FENCE)s it asked for (patch 0007's two paths).
    pub gpu_commands: u32,
    pub fence_events: u32,
    /// Queues made (DRAWCTXT_CREATE's flags; queue i + 1 is id i + 1) and power constraints set (patch 0008).
    pub queue_flags: Vec<u32>,
    pub constraints: Vec<Constraint>,
    /// /dev/udmabuf's stand-in too
    pub fake_udmabuf: bool,
}

static STAND: Mutex<Stand> = Mutex::new(Stand {
    profile: A750,
    objects: Vec::new(),
    next_id: 1,
    imports: 0,
    frees: 0,
    no_io_coherence: false,
    binds: Vec::new(),
    bind_calls: 0,
    gpu_commands: 0,
    fence_events: 0,
    queue_flags: Vec::new(),
    constraints: Vec::new(),
    fake_udmabuf: false,
});

pub fn stand() -> MutexGuard<'static, Stand> {
    STAND.lock().unwrap_or_else(|e| e.into_inner())
}

fn fail(errno: c_int) -> c_int {
    xh_core::sys::set_errno(errno);
    -1
}

impl Stand {
    pub fn find(&self, id: u32) -> Option<&Object> {
        self.objects.iter().find(|o| o.id == id)
    }

    /// KGSL objects other than VBOs.
    pub fn live(&self) -> usize {
        self.objects.iter().filter(|o| !o.vbo).count()
    }

    pub fn binds_of(&self, op: u32) -> usize {
        self.binds.iter().filter(|b| b.op == op).count()
    }

    /// A queue's KGSL priority (99: no such queue).
    pub fn priority_of(&self, id: u32) -> u32 {
        match id {
            1.. if id as usize <= self.queue_flags.len() => {
                (self.queue_flags[id as usize - 1] & CONTEXT_PRIORITY_MASK) >> CONTEXT_PRIORITY_SHIFT
            }
            _ => 99,
        }
    }

    pub fn constrained(&self, id: u32) -> bool {
        id != 0 && self.queue_flags[id as usize - 1] & CONTEXT_PWR_CONSTRAINT != 0
    }

    fn object_new(&mut self, size: u64, vbo: bool) -> Result<u32, c_int> {
        let mut at = self.profile.va_base;
        loop {
            // first fit
            let moved =
                self.objects.iter().find(|o| at < o.base + o.size && o.base < at + size).map(|o| o.base + o.size);
            match moved {
                Some(next) => at = next,
                None => break,
            }
        }
        if size == 0 || at + size > self.profile.va_end || self.objects.len() == 256 {
            return Err(libc::ENOMEM);
        }
        let id = self.next_id;
        self.next_id += 1;
        self.objects.push(Object { id, base: at, size, flags: 0, vbo });
        Ok(id)
    }

    /// One KGSL request.
    ///
    /// # Safety
    /// `arg` is the request's structure, as the renderer passes it.
    unsafe fn kgsl(&mut self, request: u64, arg: *mut c_void) -> c_int {
        let r = |r: Result<(), c_int>| r.map_or_else(fail, |()| 0);
        // SAFETY: (each arm) `arg` is the structure the request's number names (the caller's contract), and the
        // pointers inside it the renderer's own buffers.
        unsafe {
            match request & 0xff {
                NR_GETPROPERTY => {
                    let p = &mut *(arg as *mut GetProperty);
                    if p.kind != PROP_DEVICE_INFO || p.sizebytes < std::mem::size_of::<DevInfo>() {
                        return fail(libc::EINVAL);
                    }
                    let pr = self.profile;
                    let info = DevInfo {
                        device_id: 1,
                        chip_id: pr.chip_id,
                        mmu_enabled: 1,
                        gpu_id: pr.gpu_id,
                        gmem_sizebytes: pr.gmem as usize,
                        ..DevInfo::default()
                    };
                    (p.value as *mut DevInfo).write_unaligned(info);
                    0
                }
                NR_GPUOBJ_ALLOC => {
                    let a = &mut *(arg as *mut GpuobjAlloc);
                    if a.flags & MEMFLAGS_VBO != 0 && a.size > VA_END - VA_BASE {
                        return fail(libc::EINVAL);
                    }
                    r(self.object_new(a.size, a.flags & MEMFLAGS_VBO != 0).map(|id| a.id = id))
                }
                NR_GPUOBJ_INFO => {
                    let i = &mut *(arg as *mut GpuobjInfo);
                    let Some(o) = self.find(i.id) else { return fail(libc::ENOENT) };
                    (i.gpuaddr, i.size, i.flags) = (o.base, o.size, o.flags);
                    0
                }
                NR_GPUOBJ_IMPORT => {
                    let i = &mut *(arg as *mut GpuobjImport);
                    let d = &*(i.private as usize as *const ImportDmaBuf);
                    let size = if i.kind == USER_MEM_TYPE_DMABUF { libc::lseek(d.fd, 0, libc::SEEK_END) } else { -1 };
                    if size <= 0 || i.flags & MEMFLAGS_VBO != 0 {
                        return fail(libc::EINVAL);
                    }
                    let id = match self.object_new(size as u64, false) {
                        Ok(id) => id,
                        Err(e) => return fail(e),
                    };
                    i.id = id;
                    let keep = if self.no_io_coherence { i.flags & !MEMFLAGS_IOCOHERENT } else { i.flags };
                    self.objects.last_mut().unwrap().flags = keep;
                    self.imports += 1;
                    0
                }
                NR_GPUOBJ_FREE => {
                    let f = &*(arg as *const GpuobjFree);
                    let Some(i) = self.objects.iter().position(|o| o.id == f.id) else { return fail(libc::ENOENT) };
                    self.objects.swap_remove(i);
                    self.frees += 1;
                    0
                }
                NR_GPUMEM_BIND_RANGES => {
                    let r = &*(arg as *const BindRanges);
                    let ranges =
                        std::slice::from_raw_parts(r.ranges as usize as *const BindRange, r.ranges_nents as usize);
                    let Some(vbo) = self.find(r.id).filter(|v| v.vbo).copied() else { return fail(libc::EINVAL) };
                    if r.ranges_size as usize != std::mem::size_of::<BindRange>() {
                        return fail(libc::EINVAL);
                    }
                    for b in ranges {
                        let child = if b.child_id != 0 { self.find(b.child_id) } else { None };
                        let bind = b.op == RANGE_OP_BIND;
                        if (b.target_offset | b.length | b.child_offset) & 0xfff != 0
                            || b.length == 0
                            || b.target_offset + b.length > vbo.size
                            || (bind && child.is_none_or(|c| c.vbo || b.child_offset + b.length > c.size))
                        {
                            return fail(libc::EINVAL);
                        }
                    }
                    for b in ranges {
                        if self.binds.len() < 256 {
                            self.binds.push(Bound {
                                vbo: r.id,
                                child: b.child_id,
                                op: b.op,
                                target: b.target_offset,
                                length: b.length,
                                child_offset: b.child_offset,
                                call: self.bind_calls,
                            });
                        }
                    }
                    self.bind_calls += 1;
                    0
                }
                NR_SETPROPERTY => {
                    let p = &*(arg as *const GetProperty);
                    if p.kind == PROP_PWR_CONSTRAINT
                        && p.sizebytes == std::mem::size_of::<DeviceConstraint>()
                        && self.constraints.len() < 64
                    {
                        let c = &*(p.value as *const DeviceConstraint);
                        let l = c.data as *const ConstraintPwrlevel;
                        let level = if l.is_null() { !0 } else { (*l).level };
                        self.constraints.push(Constraint { context: c.context_id, kind: c.kind, level });
                    }
                    0
                }
                // Queues (patch 0008): their flags as asked, a priority over 15 refused as KGSL does.
                NR_DRAWCTXT_CREATE => {
                    let d = &mut *(arg as *mut DrawctxtCreate);
                    if self.queue_flags.len() == 64 {
                        return fail(libc::ENOMEM);
                    }
                    self.queue_flags.push(d.flags);
                    d.drawctxt_id = self.queue_flags.len() as u32;
                    0
                }
                NR_DRAWCTXT_DESTROY => 0,
                // The submit path (patch 0007): GPU_COMMAND always; the FENCE path then asks for a sync_file. The
                // timestamp path asks for none, and its wait ioctl reports the timestamp passed.
                NR_GPU_COMMAND => {
                    self.gpu_commands += 1;
                    0
                }
                NR_WAITTIMESTAMP_CTXTID => 0,
                NR_TIMESTAMP_EVENT => {
                    let e = &*(arg as *const TimestampEvent);
                    if e.kind != TIMESTAMP_EVENT_FENCE || e.private.is_null() {
                        return fail(libc::EINVAL);
                    }
                    let fd = libc::eventfd(0, 0);
                    if fd < 0 {
                        return -1;
                    }
                    (*(e.private as *mut TimestampEventFence)).fence_fd = fd;
                    self.fence_events += 1;
                    0
                }
                _ => fail(libc::ENOTTY),
            }
        }
    }
}

/// A memfd of the list's size (not the guest's pages).
///
/// # Safety
/// `arg` is a struct udmabuf_create_list.
unsafe fn fake_udmabuf_create(arg: *const c_void) -> c_int {
    // SAFETY: the list's header, then `count` items of 24 bytes (memfd, pad, offset, size).
    let bytes: u64 = unsafe {
        let count = *(arg as *const u32).add(1) as usize;
        let items = (arg as *const u8).add(8) as *const u64;
        (0..count).map(|i| *items.add(i * 3 + 2)).sum()
    };
    // A dma-buf's stand-in, handed to the renderer as the ioctl's new descriptor (closed when it fails).
    let Ok(fd) = xh_core::fd::memfd(c"fake-udmabuf", libc::MFD_CLOEXEC) else { return -1 };
    let file = std::fs::File::from(fd);
    if file.set_len(bytes).is_err() {
        return -1;
    }
    std::os::fd::IntoRawFd::into_raw_fd(file)
}

extern "C" fn open_hook(path: *const c_char, flags: c_int, mode: c_uint) -> c_int {
    // SAFETY: the renderer's (or gpu3d's) NUL-terminated path.
    let p = unsafe { CStr::from_ptr(path) };
    let fake = stand().fake_udmabuf;
    if p == c"/dev/kgsl-3d0" || p == c"/dev/dma_heap/system" || (fake && p == c"/dev/udmabuf") {
        // SAFETY: a NUL-terminated path.
        return unsafe { libc::open(c"/dev/null".as_ptr(), libc::O_RDWR | libc::O_CLOEXEC) };
    }
    // SAFETY: as given.
    unsafe { libc::open(path, flags, mode) }
}

extern "C" fn ioctl_hook(fd: c_int, request: c_ulong, arg: *mut c_void) -> c_int {
    let mut s = stand();
    if s.fake_udmabuf && request == UDMABUF_CREATE_LIST {
        drop(s);
        // SAFETY: the request's structure.
        return unsafe { fake_udmabuf_create(arg) };
    }
    if (request >> 8) & 0xff == IOC_TYPE {
        // SAFETY: the request's structure, as the renderer passes it.
        return unsafe { s.kgsl(request, arg) };
    }
    drop(s);
    // SAFETY: the caller's request, passed on to the kernel.
    unsafe { libc::ioctl(fd, request as _, arg) }
}

/// The stand-in in place of KGSL (and with `fake_udmabuf`, of /dev/udmabuf), with the GPU `profile`.
pub fn install(profile: Profile, fake_udmabuf: bool) {
    {
        let mut s = stand();
        s.profile = profile;
        s.fake_udmabuf = fake_udmabuf;
    }
    drm2kgsl_sys::sys::set_hooks(Some(open_hook), Some(ioctl_hook));
}
