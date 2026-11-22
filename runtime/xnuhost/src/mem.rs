// SPDX-License-Identifier: MIT
//! Port of mem.c. Guest RAM: one memfd. xnuhost maps all of it once (the kernel's window, at the address the kernel
//! runs at); each iOS task maps the pages XNU's pmap gives it, at the addresses iOS expects, from the same memfd.
//! Shared memory between tasks is therefore two mappings of one offset.
//!
//! The state (`g_mem`) is set once at the start and read everywhere (signal handlers included): atomics, read with
//! [`state`].

use std::ptr::NonNull;
use std::sync::atomic::{AtomicBool, AtomicI32, AtomicU64, Ordering::Relaxed};
use xh_core::sys::{errno, strerror};
use xh_core::{loge, logi};
use xnuhost_abi::{XH_PROT_EXEC, XH_PROT_READ, XH_PROT_WRITE};

/// The memfd's first byte is physical address fd_phys = phys - head, head = phys mod 2 MiB (fix-ram-hugepages,
/// 2026-11-15): file offsets and addresses are then congruent modulo 2 MiB, which a 2 MiB host page needs to be
/// mapped with one entry. The memfd is HEAD_ROOM longer than the RAM for it; the head is mapped, never guest RAM.
pub const HEAD_ROOM: u64 = 2 << 20;

/// A set-up step failed (the RAM, the apertures, the vCPUs, the task factory, the prefault's thread); what went wrong
/// is in the log already.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SetupFailed;

impl std::fmt::Display for SetupFailed {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("set-up failed (the log says why)")
    }
}

impl std::error::Error for SetupFailed {}

/// `xh_mem`, as one copy of its fields.
#[derive(Debug, Clone, Copy)]
pub struct Mem {
    /// memfd of the RAM
    pub fd: i32,
    /// where the window is mapped in xnuhost (= what the kernel sees)
    pub host: *mut u8,
    pub phys: u64,
    pub size: u64,
    /// PROT_NONE reservation for the board's devices
    pub io_host: *mut u8,
    pub io_phys: u64,
    pub io_size: u64,
    /// reservation for the kernel's own mappings
    pub kva: u64,
    pub kva_size: u64,
    pub fd_phys: u64,
    pub head: u64,
    /// the window is mapped with MADV_HUGEPAGE (huge_choose said shmem THP)
    pub huge: bool,
}

struct MemCell {
    fd: AtomicI32,
    host: AtomicU64,
    phys: AtomicU64,
    size: AtomicU64,
    io_host: AtomicU64,
    io_phys: AtomicU64,
    io_size: AtomicU64,
    kva: AtomicU64,
    kva_size: AtomicU64,
    fd_phys: AtomicU64,
    head: AtomicU64,
    huge: AtomicBool,
}

static MEM: MemCell = MemCell {
    fd: AtomicI32::new(-1),
    host: AtomicU64::new(0),
    phys: AtomicU64::new(0),
    size: AtomicU64::new(0),
    io_host: AtomicU64::new(0),
    io_phys: AtomicU64::new(0),
    io_size: AtomicU64::new(0),
    kva: AtomicU64::new(0),
    kva_size: AtomicU64::new(0),
    fd_phys: AtomicU64::new(0),
    head: AtomicU64::new(0),
    huge: AtomicBool::new(false),
};

/// The RAM and the apertures as they are now.
pub fn state() -> Mem {
    let m = &MEM;
    Mem {
        fd: m.fd.load(Relaxed),
        host: m.host.load(Relaxed) as usize as *mut u8,
        phys: m.phys.load(Relaxed),
        size: m.size.load(Relaxed),
        io_host: m.io_host.load(Relaxed) as usize as *mut u8,
        io_phys: m.io_phys.load(Relaxed),
        io_size: m.io_size.load(Relaxed),
        kva: m.kva.load(Relaxed),
        kva_size: m.kva_size.load(Relaxed),
        fd_phys: m.fd_phys.load(Relaxed),
        head: m.head.load(Relaxed),
        huge: m.huge.load(Relaxed),
    }
}

/// Host pointer for guest physical [pa, pa+len), None if not inside RAM (`xh_ram`; the devices' RAM hook).
#[inline]
pub fn ram(pa: u64, len: u64) -> Option<NonNull<u8>> {
    let (host, phys, size) = (MEM.host.load(Relaxed), MEM.phys.load(Relaxed), MEM.size.load(Relaxed));
    if host == 0 || pa < phys || pa - phys > size || len > size - (pa - phys) {
        return None;
    }
    NonNull::new((host + (pa - phys)) as usize as *mut u8)
}

// Where the window goes in xnuhost's address space. Below every address a 39-bit kernel hands out by itself
// (libraries and stacks sit near the top), above the iOS shared region's addresses so a kernel pointer never looks
// like a user one.
const RAM_HINT: u64 = 0x20_0000_0000; // 128 GiB
const IO_HINT: u64 = 0x1f_0000_0000;
/// The RAM window and the kernel's own range share 0x20_0000_0000 .. 0x3c_0000_0000.
const KVA_TOP: u64 = 0x3c_0000_0000;
/// Linux 4.17 (not in every libc crate target, Android's included).
pub(crate) const MAP_FIXED_NOREPLACE: i32 = 0x10_0000;

/// Creates the RAM memfd (before the task factory forks): size + HEAD_ROOM bytes.
pub fn create(size: u64) -> Result<(), SetupFailed> {
    // Inherited by the task factory: no CLOEXEC. Sealable, and sealed against shrinking once it has its size: udmabuf,
    // which turns the guest's pages into the dma-bufs the GPU imports (gpu3d), takes no other memfd. HEAD_ROOM more
    // than the RAM: the window starts `head` bytes into the file (map); the rest of that room is never touched.
    // SAFETY: memfd_create with a static name.
    let fd = unsafe {
        libc::syscall(libc::SYS_memfd_create, c"xnuhost-ram".as_ptr(), 2u32 /* MFD_ALLOW_SEALING */)
    } as i32;
    if fd < 0 {
        loge!("memfd_create: {}", strerror(errno()));
        return Err(SetupFailed);
    }
    // SAFETY: our new descriptor.
    if unsafe { libc::ftruncate(fd, (size + HEAD_ROOM) as libc::off_t) } != 0 {
        loge!("RAM of {} MiB: {}", size >> 20, strerror(errno()));
        // SAFETY: as above.
        unsafe { libc::close(fd) };
        return Err(SetupFailed);
    }
    // SAFETY: as above.
    if unsafe {
        libc::fcntl(fd, 1033 /* F_ADD_SEALS */, 2 /* F_SEAL_SHRINK */)
    } != 0
    {
        loge!("RAM: no shrink seal ({}): the GPU cannot import guest pages", strerror(errno()));
    }
    MEM.fd.store(fd, Relaxed);
    MEM.size.store(size, Relaxed);
    Ok(())
}

/// Maps the window at an address congruent to `low21` modulo 2 MiB; that address is also its physical address
/// (phys == host). The file from offset 0 is mapped at the 2 MiB boundary below it. huge: madvise(MADV_HUGEPAGE) on
/// that mapping (shmem THP, ramfix::huge_choose). Installs the devices' RAM hook.
///
/// The window's physical address IS its address in xnuhost: one number names a byte of RAM for the kernel's pointers,
/// its "physical" addresses (page tables it may still write, DMA addresses in virtqueues, boot_args.physBase) and the
/// offsets tasks map (address - fd_phys). Code that runs before the kernel's MMU switch and computes physical
/// addresses therefore still touches the right bytes. The file is mapped from offset 0 at the 2 MiB boundary below
/// the window (fix-ram-hugepages, 2026-11-15). Before, offset 0 was the window's first byte, at 0x...4000 (the
/// kernel's link base modulo 2 MiB), so no 2 MiB host page could be mapped with one entry anywhere; now address and
/// offset agree modulo 2 MiB and, where shmem THP is on, Linux maps the window in 2 MiB entries.
pub fn map(low21: u64, huge: bool) -> Result<(), SetupFailed> {
    let size = MEM.size.load(Relaxed);
    let head = low21 & 0x1f_ffff;
    let length = (head + size) as usize;
    let mut want = RAM_HINT;
    let mut p = libc::MAP_FAILED;
    for _ in 0..16 {
        // SAFETY: a new shared mapping of the RAM memfd where nothing is mapped (MAP_FIXED_NOREPLACE).
        p = unsafe {
            libc::mmap(
                want as usize as *mut _,
                length,
                libc::PROT_READ | libc::PROT_WRITE | libc::PROT_EXEC,
                libc::MAP_SHARED | crate::mem::MAP_FIXED_NOREPLACE,
                MEM.fd.load(Relaxed),
                0,
            )
        };
        if p != libc::MAP_FAILED && p as u64 != want {
            // SAFETY: our mapping, at the wrong place (an old kernel that ignores the flag).
            unsafe { libc::munmap(p, length) };
            p = libc::MAP_FAILED;
        }
        if p != libc::MAP_FAILED {
            break;
        }
        want += 0x4_0000_0000;
    }
    if p == libc::MAP_FAILED {
        loge!("cannot map {} MiB of RAM executable: {}", size >> 20, strerror(errno()));
        return Err(SetupFailed);
    }
    let host = p as u64 + head;
    MEM.host.store(host, Relaxed);
    MEM.phys.store(host, Relaxed);
    MEM.head.store(head, Relaxed);
    MEM.fd_phys.store(host - head, Relaxed);
    // SAFETY: advice on our own mapping.
    unsafe { libc::madvise(p, length, libc::MADV_DONTDUMP) };
    // Before the first touch: shmem gives a 2 MiB folio to the first fault in a 2 MiB range through a mapping with
    // VM_HUGEPAGE (shmem_enabled "advise"), and 4 KiB pages to a range first touched through another mapping (a task's).
    // SAFETY: as above.
    let huge_ok = huge && unsafe { libc::madvise(p, length, libc::MADV_HUGEPAGE) } == 0;
    MEM.huge.store(huge_ok, Relaxed);
    if huge && !huge_ok {
        loge!("memory: madvise(MADV_HUGEPAGE) on the RAM: {}: 4 KiB host pages", strerror(errno()));
    }
    xh_core::hooks::install_ram(ram);
    Ok(())
}

fn reserve(at: u64, size: u64, fixed: bool) -> *mut libc::c_void {
    let flags = libc::MAP_PRIVATE
        | libc::MAP_ANONYMOUS
        | libc::MAP_NORESERVE
        | if fixed { crate::mem::MAP_FIXED_NOREPLACE } else { 0 };
    // SAFETY: a new inaccessible reservation; with MAP_FIXED_NOREPLACE never over another mapping.
    unsafe { libc::mmap(at as usize as *mut _, size as usize, libc::PROT_NONE, flags, -1, 0) }
}

/// Reserves the PROT_NONE aperture the board's device windows live in: at their physical addresses when those are
/// free in this process (so a device's physical address is also its pointer), else elsewhere; the io_map host call
/// and xh_info.io_virt say where.
pub fn io(phys: u64, size: u64) -> Result<(), SetupFailed> {
    let mut p = reserve(phys, size, true);
    if p != libc::MAP_FAILED && p as u64 != phys {
        // SAFETY: our reservation, at the wrong place.
        unsafe { libc::munmap(p, size as usize) };
        p = libc::MAP_FAILED;
    }
    if p == libc::MAP_FAILED {
        p = reserve(IO_HINT, size, false);
    }
    if p == libc::MAP_FAILED {
        loge!("cannot reserve the I/O aperture: {}", strerror(errno()));
        return Err(SetupFailed);
    }
    MEM.io_host.store(p as u64, Relaxed);
    MEM.io_phys.store(phys, Relaxed);
    MEM.io_size.store(size, Relaxed);
    Ok(())
}

/// The kernel's own address space outside the window (hostcall.h, kernel_map): reserved, no access.
pub fn kva() -> Result<(), SetupFailed> {
    let base = (MEM.host.load(Relaxed) + MEM.size.load(Relaxed) + 0x3fff_ffff) & !0x3fff_ffff;
    if base >= KVA_TOP {
        loge!("no room for the kernel's address space below 0x{:x}", KVA_TOP);
        return Err(SetupFailed);
    }
    let p = reserve(base, KVA_TOP - base, true);
    if p == libc::MAP_FAILED || p as u64 != base {
        let e = errno();
        if p != libc::MAP_FAILED {
            // SAFETY: our reservation, at the wrong place.
            unsafe { libc::munmap(p, (KVA_TOP - base) as usize) };
        }
        loge!("cannot reserve the kernel's address space at 0x{:x}: {}", base, strerror(e));
        return Err(SetupFailed);
    }
    MEM.kva.store(base, Relaxed);
    MEM.kva_size.store(KVA_TOP - base, Relaxed);
    Ok(())
}

fn page_mask() -> u64 {
    xh_core::sys::page_size() - 1
}

fn in_kva(m: &Mem, va: u64, size: u64) -> bool {
    size != 0 && va >= m.kva && va - m.kva <= m.kva_size && size <= m.kva_size - (va - m.kva)
}

/// kernel_map: RAM pages at kernel virtual addresses inside the reservation. 0 or -errno.
pub fn kernel_map(va: u64, size: u64, prot: u32, phys: u64) -> i64 {
    let m = state();
    if (va | size | phys) & page_mask() != 0 || !in_kva(&m, va, size) || ram(phys, size).is_none() {
        return -(libc::EINVAL as i64);
    }
    let p = [(XH_PROT_READ, libc::PROT_READ), (XH_PROT_WRITE, libc::PROT_WRITE), (XH_PROT_EXEC, libc::PROT_EXEC)]
        .iter()
        .filter(|(x, _)| prot & x != 0)
        .fold(0, |p, (_, l)| p | l);
    // Populated in the same call: the kernel touches what it maps (a stack, a zone page), and each first touch was a
    // Linux fault of its own on the vCPU (host-overhead, 2026-11-15).
    // SAFETY: replaces pages inside the kernel's own reservation (checked above) with the RAM memfd's.
    let at = unsafe {
        libc::mmap(
            va as usize as *mut _,
            size as usize,
            p,
            libc::MAP_SHARED | libc::MAP_FIXED | libc::MAP_POPULATE,
            m.fd,
            (phys - m.fd_phys) as libc::off_t,
        )
    };
    if at == libc::MAP_FAILED {
        -(errno() as i64)
    } else {
        0
    }
}

/// kernel_unmap: the range faults again. 0 or -errno.
pub fn kernel_unmap(va: u64, size: u64) -> i64 {
    let m = state();
    if (va | size) & page_mask() != 0 || !in_kva(&m, va, size) {
        return -(libc::EINVAL as i64);
    }
    // SAFETY: replaces pages inside the kernel's own reservation (checked above) with an inaccessible reservation.
    let at = unsafe {
        libc::mmap(
            va as usize as *mut _,
            size as usize,
            libc::PROT_NONE,
            libc::MAP_PRIVATE | libc::MAP_ANONYMOUS | libc::MAP_NORESERVE | libc::MAP_FIXED,
            -1,
            0,
        )
    };
    if at == libc::MAP_FAILED {
        -(errno() as i64)
    } else {
        0
    }
}

/// The kernel's heap and every task's map are page-granular mappings of one memfd: far more areas than Linux's
/// default limit of 65530 per process. Root may raise it.
pub fn map_limit() {
    const PATH: &str = "/proc/sys/vm/max_map_count";
    const WANT: i64 = 1_048_576;
    let have = std::fs::read_to_string(PATH).ok().and_then(|s| s.trim().parse::<i64>().ok()).unwrap_or(0);
    if have < WANT && std::fs::write(PATH, format!("{WANT}\n")).is_ok() {
        logi!("memory: vm.max_map_count raised from {} to {}", have, WANT);
        return;
    }
    if have < WANT {
        loge!(
            "memory: vm.max_map_count is {} and could not be raised to {} (not root?): the kernel's mappings may run out",
            have,
            WANT
        );
    } else {
        logi!("memory: vm.max_map_count is {}", have);
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    /// Puts a test buffer in as the RAM window (host, phys, size) until the guard goes.
    pub(crate) struct Window;

    pub(crate) fn window(host: *mut u8, phys: u64, size: u64) -> Window {
        MEM.host.store(host as u64, Relaxed);
        MEM.phys.store(phys, Relaxed);
        MEM.size.store(size, Relaxed);
        Window
    }

    impl Drop for Window {
        fn drop(&mut self) {
            MEM.host.store(0, Relaxed);
            MEM.phys.store(0, Relaxed);
            MEM.size.store(0, Relaxed);
        }
    }

    #[test]
    fn ram_bounds() {
        let _m = crate::cpu::tests::machine();
        let mut buf = vec![0u8; 0x4000];
        let _w = window(buf.as_mut_ptr(), 0x8000_0000, 0x4000);
        assert_eq!(ram(0x8000_0000, 0x4000).map(|p| p.as_ptr()), Some(buf.as_mut_ptr()));
        assert!(ram(0x8000_3fff, 1).is_some() && ram(0x8000_3fff, 2).is_none());
        assert!(ram(0x7fff_ffff, 1).is_none() && ram(0x8000_4000, 0).is_some() && ram(0x8000_4001, 0).is_none());
    }
}
