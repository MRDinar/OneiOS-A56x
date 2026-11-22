// SPDX-License-Identifier: MIT
//! Port of discard.c. The guest gives free pages back to Linux: ram_discard (hostcall.h), used by the hosted kernel's
//! free-page reporting (kernel patch 0058, docs/booter.md 2.2). Portable: the host tests run it on a memfd of their own.

use std::os::fd::RawFd;
use std::sync::atomic::{AtomicU64, Ordering::Relaxed};
use xh_core::sys::errno;

/// Counted for the log's `memory:` line: calls that punched, bytes given back, failures of fallocate, calls refused
/// (unaligned or outside the RAM), nanoseconds spent in fallocate.
#[derive(Debug, Default)]
pub struct DiscardStats {
    pub calls: AtomicU64,
    pub bytes: AtomicU64,
    pub failed: AtomicU64,
    pub refused: AtomicU64,
    pub ns: AtomicU64,
}

pub static DISCARD: DiscardStats = DiscardStats {
    calls: AtomicU64::new(0),
    bytes: AtomicU64::new(0),
    failed: AtomicU64::new(0),
    refused: AtomicU64::new(0),
    ns: AtomicU64::new(0),
};

/// While the RAM is held fixed (ram-giveback=0, XH_FEATURE_RAM_FIXED) nothing goes back: a hole punched in a RAM that
/// is held for the session would only be faulted in again. A kernel from before 0067 still asks; its first -EPERM
/// stops its reporting for the boot (0058: "free pages are no longer given back").
pub static FIXED_REFUSED: AtomicU64 = AtomicU64::new(0);

use xh_core::time::monotonic_ns as mono_ns;

/// [phys, phys + size) of the RAM memfd `fd`, whose first byte is physical address `fd_base`. Allowed only inside the
/// kernel's window [lo, lo + len) (never the boot framebuffer or the GPU's pool above it) and page-aligned (`page`,
/// the host's page size), or -EINVAL. A hole is punched in the memfd: Linux frees the pages and drops them from every
/// mapping (xnuhost's window, every task that maps them); the next access reads zeros, a fresh page. The size of the
/// memfd is kept (the GPU's udmabufs need its shrink seal; a punch is allowed under it). Returns 0 or -errno.
#[allow(clippy::too_many_arguments)]
pub fn ram_discard_fd(fd: RawFd, fd_base: u64, lo: u64, len: u64, phys: u64, size: u64, page: u64) -> i64 {
    if fd < 0
        || page == 0
        || (phys | size) & (page - 1) != 0
        || size == 0
        || lo < fd_base
        || phys < lo
        || phys - lo > len
        || size > len - (phys - lo)
    {
        DISCARD.refused.fetch_add(1, Relaxed);
        return -(libc::EINVAL as i64);
    }
    let t0 = mono_ns();
    // SAFETY: fallocate on a descriptor; a bad one is an error return.
    let r = unsafe {
        libc::fallocate(
            fd,
            libc::FALLOC_FL_PUNCH_HOLE | libc::FALLOC_FL_KEEP_SIZE,
            (phys - fd_base) as libc::off_t,
            size as libc::off_t,
        )
    };
    let r = if r != 0 { -(errno() as i64) } else { 0 };
    DISCARD.ns.fetch_add(mono_ns() - t0, Relaxed);
    DISCARD.calls.fetch_add(1, Relaxed);
    if r != 0 {
        DISCARD.failed.fetch_add(1, Relaxed);
    } else {
        DISCARD.bytes.fetch_add(size, Relaxed);
    }
    r
}

/// ram_discard as the host call answers it: refused with -EPERM (counted in [`FIXED_REFUSED`]) while the RAM is held
/// fixed (XH_FEATURE_RAM_FIXED, ram-giveback=0), else [`ram_discard_fd`].
#[allow(clippy::too_many_arguments)]
pub fn ram_discard_host(
    fixed: bool,
    fd: RawFd,
    fd_base: u64,
    lo: u64,
    len: u64,
    phys: u64,
    size: u64,
    page: u64,
) -> i64 {
    if fixed {
        FIXED_REFUSED.fetch_add(1, Relaxed);
        return -(libc::EPERM as i64);
    }
    ram_discard_fd(fd, fd_base, lo, len, phys, size, page)
}

/// The bytes of [base, base + size) of a shared mapping of the RAM memfd that are in the host's RAM (mincore: a page
/// the host swapped out, to zram on the phone, is not; nor one never touched or given back). `page` is the host's
/// page size. A range mincore refuses (not mapped) counts nothing. The `memory:` line's swap figure is the memfd's
/// held pages (st_blocks, which counts swapped ones too) less this.
pub fn ram_resident(base: *const u8, size: u64, page: u64) -> u64 {
    // One call per 16384 pages (64 MiB of 4 KiB pages); the main loop's only caller.
    const VEC: u64 = 16384;
    if base.is_null() || page == 0 || (base as u64) & (page - 1) != 0 {
        return 0;
    }
    let mut vec = vec![0u8; VEC as usize];
    let mut resident = 0u64;
    let mut at = 0u64;
    while at < size {
        let length = (size - at).min(VEC * page);
        // SAFETY: mincore only reads the page tables of [base + at, +length) and writes one byte per page into vec,
        // which has room for VEC pages; an unmapped range is an error return.
        let r = unsafe {
            libc::mincore(base.wrapping_add(at as usize) as *mut libc::c_void, length as usize, vec.as_mut_ptr())
        };
        if r == 0 {
            let pages = length.div_ceil(page) as usize;
            resident += vec[..pages].iter().map(|&b| u64::from(b & 1)).sum::<u64>();
        }
        at += VEC * page;
    }
    resident * page
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use std::os::fd::AsRawFd;

    /// The bytes a memfd holds (st_blocks).
    pub(crate) fn memfd_held(fd: RawFd) -> u64 {
        // SAFETY: fstat fills the stat it is given.
        let mut st: libc::stat = unsafe { std::mem::zeroed() };
        // SAFETY: as above.
        if unsafe { libc::fstat(fd, &mut st) } == 0 {
            st.st_blocks as u64 * 512
        } else {
            u64::MAX
        }
    }

    /// A RAM memfd with xnuhost's shrink seal, `size` bytes, mapped shared read-write.
    pub(crate) struct TestRam {
        pub fd: std::os::fd::OwnedFd,
        pub base: *mut u8,
        pub size: u64,
    }

    impl TestRam {
        pub(crate) fn new(size: u64) -> Option<TestRam> {
            let fd = xh_core::fd::memfd(c"xnuhost-test-ram", libc::MFD_ALLOW_SEALING).ok()?;
            // SAFETY: ftruncate and fcntl on our descriptor.
            let sized = unsafe { libc::ftruncate(fd.as_raw_fd(), size as libc::off_t) } == 0;
            // SAFETY: as above.
            if !sized || unsafe { libc::fcntl(fd.as_raw_fd(), libc::F_ADD_SEALS, libc::F_SEAL_SHRINK) } != 0 {
                return None;
            }
            let base = map(fd.as_raw_fd(), size, libc::PROT_READ | libc::PROT_WRITE)?;
            Some(TestRam { fd, base, size })
        }

        pub(crate) fn raw(&self) -> RawFd {
            self.fd.as_raw_fd()
        }

        #[allow(clippy::mut_from_ref)] // a test's view of its own shared mapping
        pub(crate) fn bytes(&self) -> &mut [u8] {
            // SAFETY: the mapping is `size` bytes long and lives as long as self; the tests are single-threaded on it.
            unsafe { std::slice::from_raw_parts_mut(self.base, self.size as usize) }
        }
    }

    impl Drop for TestRam {
        fn drop(&mut self) {
            // SAFETY: our own mapping.
            unsafe { libc::munmap(self.base as *mut libc::c_void, self.size as usize) };
        }
    }

    pub(crate) fn map(fd: RawFd, size: u64, prot: i32) -> Option<*mut u8> {
        // SAFETY: a new shared mapping of the descriptor, anywhere.
        let p = unsafe { libc::mmap(std::ptr::null_mut(), size as usize, prot, libc::MAP_SHARED, fd, 0) };
        (p != libc::MAP_FAILED).then_some(p as *mut u8)
    }

    #[test]
    fn discard() {
        // A RAM of 64 pages of 16 KiB at physical 0x40000000, as a memfd with xnuhost's shrink seal, mapped twice: the
        // window and "a task" (MAP_SHARED, as task_map maps it).
        let (page, gp, base) = (4096u64, 16384u64, 0x4000_0000u64);
        let size = 64 * gp;
        let ram = TestRam::new(size).expect("memfd");
        let fd = ram.raw();
        let task = map(fd, size, libc::PROT_READ).expect("second mapping");
        // SAFETY: both are `size`-byte mappings of the memfd.
        let w = unsafe { std::slice::from_raw_parts_mut(ram.base as *mut u64, (size / 8) as usize) };
        // SAFETY: as above, read only.
        let t = unsafe { std::slice::from_raw_parts(task as *const u64, (size / 8) as usize) };
        for (i, word) in w.iter_mut().enumerate() {
            *word = i as u64 | 1;
        }
        assert_eq!(memfd_held(fd), size);
        let s = &DISCARD;
        let before = |c: &AtomicU64| c.load(Relaxed);
        let (calls0, bytes0, failed0, refused0) =
            (before(&s.calls), before(&s.bytes), before(&s.failed), before(&s.refused));
        // Pages 8..15 given back: the memfd holds 8 pages less, both mappings read zero there and the rest is kept.
        assert_eq!(ram_discard_fd(fd, base, base, size, base + 8 * gp, 8 * gp, page), 0);
        assert_eq!(memfd_held(fd), size - 8 * gp);
        let (mut zero, mut kept) = (true, true);
        for i in 0..(size / 8) as usize {
            let at = i as u64 * 8;
            if at >= 8 * gp && at < 16 * gp {
                zero &= w[i] == 0 && t[i] == 0;
            } else {
                kept &= w[i] == (i as u64 | 1) && t[i] == (i as u64 | 1);
            }
        }
        assert!(zero && kept);
        // A read of a hole through a shared mapping fills it (shmem has no shared zero page): the pages read above are
        // held again. Given back once more, then used again: a write makes one host page held, the task sees it.
        assert_eq!(memfd_held(fd), size);
        assert_eq!(ram_discard_fd(fd, base, base, size, base + 8 * gp, 8 * gp, page), 0);
        assert_eq!(memfd_held(fd), size - 8 * gp);
        let bytes = ram.bytes();
        bytes[9 * gp as usize] = 0x5a;
        // SAFETY: inside the task mapping.
        assert_eq!(unsafe { *task.add(9 * gp as usize) }, 0x5a);
        assert_eq!(memfd_held(fd), size - 8 * gp + page);
        // A page that is a hole already: nothing more to give, the call succeeds.
        assert_eq!(ram_discard_fd(fd, base, base, size, base + 10 * gp, gp, page), 0);
        assert_eq!(memfd_held(fd), size - 8 * gp + page);
        // Refused, nothing changed: unaligned, empty, outside the window, past its end, a window below the memfd.
        let e = -(libc::EINVAL as i64);
        assert_eq!(ram_discard_fd(fd, base, base, size, base + 100, gp, page), e);
        assert_eq!(ram_discard_fd(fd, base, base, size, base, 100, page), e);
        assert_eq!(ram_discard_fd(fd, base, base, size, base, 0, page), e);
        assert_eq!(ram_discard_fd(fd, base, base, size, base - gp, gp, page), e);
        assert_eq!(ram_discard_fd(fd, base, base, size, base + 63 * gp, 2 * gp, page), e);
        assert_eq!(ram_discard_fd(fd, base, base + gp, size - gp, base, gp, page), e); // below the window's start
        assert_eq!(ram_discard_fd(fd, base, base, size - gp, base + 63 * gp, gp, page), e); // above it (the framebuffer)
        assert_eq!(ram_discard_fd(fd, base + gp, base, size, base + gp, gp, page), e); // window below the memfd
        assert_eq!(w[0], 1);
        assert_eq!(w[(63 * gp / 8) as usize], (63 * gp / 8) | 1);
        // The other tests of this binary discard too (in parallel): the counts are checked as at least ours.
        assert!(before(&s.calls) - calls0 >= 3 && before(&s.bytes) - bytes0 >= 17 * gp);
        assert!(before(&s.failed) >= failed0 && before(&s.refused) - refused0 >= 8);
        // fallocate's own failure is counted and returned (a closed descriptor).
        let failed1 = before(&s.failed);
        assert!(ram_discard_fd(1000, base, base, size, base, gp, page) < 0);
        assert!(before(&s.failed) > failed1);
        // In the host's RAM (mincore): every page but the holes of pages 8..15.
        assert_eq!(ram_discard_fd(fd, base, base, size, base + 8 * gp, 8 * gp, page), 0);
        assert_eq!(ram_resident(ram.base, size, page), memfd_held(fd));
        assert_eq!(memfd_held(fd), size - 8 * gp);
        assert_eq!(ram_resident(ram.base, size, 0), 0);
        assert_eq!(ram_resident(std::ptr::null(), size, page), 0);
        // SAFETY: our own second mapping.
        unsafe { libc::munmap(task as *mut libc::c_void, size as usize) };
    }

    #[test]
    fn discard_while_fixed() {
        // ram_discard while the RAM is held: refused, counted, nothing punched; with ram-giveback=1 as before.
        const M: u64 = 1 << 20;
        let size = 32 * M;
        let ram = TestRam::new(size).expect("memfd");
        ram.bytes().fill(1);
        let was = FIXED_REFUSED.load(Relaxed);
        assert_eq!(ram_discard_host(true, ram.raw(), 0, 0, size, 0, M, 4096), -(libc::EPERM as i64));
        assert_eq!(FIXED_REFUSED.load(Relaxed), was + 1);
        assert_eq!(memfd_held(ram.raw()), size);
        assert_eq!(ram.bytes()[0], 1);
        assert_eq!(ram_discard_host(false, ram.raw(), 0, 0, size, 0, M, 4096), 0);
        assert_eq!(memfd_held(ram.raw()), size - M);
        assert_eq!(ram.bytes()[0], 0);
    }
}
