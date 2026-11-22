// SPDX-License-Identifier: MIT
//! main.c's host facts (the page size, the user address space, the counter, pointer authentication) and the selftest.

use xh_core::logi;
#[cfg(all(target_arch = "aarch64", any(target_os = "linux", target_os = "android")))]
use xnuhost_abi::XH_FEATURE_PAC_KEYS_OFF;
use xnuhost_abi::{XH_ABI_VERSION, XH_FEATURE_PAC};

use crate::config::Config;
use crate::{cpu, hostcalls, mem, task};

#[cfg(target_arch = "aarch64")]
mod pac {
    use std::arch::asm;

    /// pacia1716: a NOP without PAC.
    pub fn sign(value: u64) -> u64 {
        let mut x17 = value;
        // SAFETY: hint #8 (pacia1716) only changes x17, from x17 and x16; no memory.
        unsafe { asm!("hint #8", inout("x17") x17, in("x16") 0x1234u64, options(nomem, nostack, preserves_flags)) };
        x17
    }

    const PROBE: u64 = 0x0000_0012_3456_7000;

    /// One key set for the kernel and every task: a pointer signed here must come out the same in a forked child (it
    /// would not after an exec, which is why nothing in the tree of xnuhost execs).
    pub fn shared_by_fork() -> bool {
        let mut fds = [0i32; 2];
        // SAFETY: pipe fills the two descriptors.
        if unsafe { libc::pipe(fds.as_mut_ptr()) } != 0 {
            return false;
        }
        let mine = sign(PROBE);
        // SAFETY: the child does only async-signal-safe work (a signature, a write, _exit).
        let pid = unsafe { libc::fork() };
        if pid == 0 {
            let v = sign(PROBE);
            // SAFETY: 8 bytes from v to the pipe, then the child ends without running anything of the parent's.
            unsafe {
                libc::write(fds[1], &v as *const u64 as *const libc::c_void, 8);
                libc::_exit(0);
            }
        }
        let mut theirs = 0u64;
        // SAFETY: closing our write end; reading 8 bytes into theirs; reaping the child.
        let ok = unsafe {
            libc::close(fds[1]);
            let ok =
                pid > 0 && libc::read(fds[0], &mut theirs as *mut u64 as *mut libc::c_void, 8) == 8 && theirs == mine;
            libc::close(fds[0]);
            if pid > 0 {
                libc::waitpid(pid, std::ptr::null_mut(), 0);
            }
            ok
        };
        // Signing must also do something. One signature can be all zero bits by chance (1 in 128 with the 7 signature
        // bits of a 47-bit address space), so several values are tried.
        let signs = (0..16u64).any(|i| sign(PROBE + (i << 12)) != PROBE + (i << 12));
        ok && signs
    }

    /// The same must hold for xnuhost's own threads: the kernel's code runs on the vCPU threads and signs pointers that
    /// every other vCPU and every task authenticates. Linux copies the keys to a new thread, but bionic's pthread start
    /// resets the thread's APIA key (prctl PR_PAC_RESET_KEYS) for programs whose target SDK is 31 or later, which a
    /// static binary is by default. The session lowers the target SDK first; this checks that it worked.
    pub fn shared_by_threads() -> bool {
        std::thread::spawn(|| sign(PROBE)).join().is_ok_and(|theirs| theirs == sign(PROBE))
    }
}

/// The top of the user address space: the largest of the usual sizes with a free page just below it.
#[cfg(all(target_arch = "aarch64", any(target_os = "linux", target_os = "android")))]
fn user_va_bits() -> u32 {
    for bits in [52u32, 48, 47, 42, 39, 36] {
        let want = ((1u64 << bits) - 0x1000_0000) as *mut libc::c_void;
        // SAFETY: a PROT_NONE probe mapping that may not replace anything; unmapped at once.
        let p = unsafe {
            libc::mmap(
                want,
                4096,
                libc::PROT_NONE,
                libc::MAP_PRIVATE | libc::MAP_ANONYMOUS | crate::mem::MAP_FIXED_NOREPLACE,
                -1,
                0,
            )
        };
        if p != libc::MAP_FAILED {
            // SAFETY: the page just mapped.
            unsafe { libc::munmap(p, 4096) };
            if p == want {
                return bits;
            }
        }
    }
    0
}

/// main.c's host_facts: what the hosted kernel is told of this host (XhInfo), and its line.
pub fn host_facts() {
    #[cfg(all(target_arch = "aarch64", any(target_os = "linux", target_os = "android")))]
    {
        const HWCAP_PACA: u64 = 1 << 30;
        const PR_PAC_SET_ENABLED_KEYS: i32 = 60;
        let va_bits = user_va_bits();
        // SAFETY: getauxval has no preconditions.
        let paca = unsafe { libc::getauxval(libc::AT_HWCAP) } & HWCAP_PACA != 0;
        let by_fork = paca && pac::shared_by_fork();
        let by_thread = paca && pac::shared_by_threads();
        // SAFETY: a no-op call (no keys named): it only asks whether per-thread key control exists.
        let keys_off = unsafe { libc::prctl(PR_PAC_SET_ENABLED_KEYS, 0, 0, 0, 0) } == 0;
        hostcalls::update_info(|i| {
            i.abi = XH_ABI_VERSION;
            i.host_page_size = xh_core::sys::page_size() as u32;
            i.user_va_bits = va_bits;
            i.timebase_hz = cpu::counter_hz();
            if by_fork && by_thread {
                i.features |= XH_FEATURE_PAC;
            }
            if keys_off {
                i.features |= XH_FEATURE_PAC_KEYS_OFF;
            }
        });
        let info = hostcalls::info();
        let pac = if !paca {
            "no"
        } else if info.features & XH_FEATURE_PAC != 0 {
            "yes, keys shared across fork and threads"
        } else if !by_fork {
            "present but keys differ after fork"
        } else {
            "present but keys differ between threads"
        };
        logi!(
            "host: page {}, user VA {} bits, counter {} Hz, PAC {}{}",
            info.host_page_size,
            info.user_va_bits,
            info.timebase_hz,
            pac,
            if keys_off { ", per-thread key control" } else { "" }
        );
    }
    #[cfg(not(all(target_arch = "aarch64", any(target_os = "linux", target_os = "android"))))]
    {
        hostcalls::update_info(|i| {
            i.abi = XH_ABI_VERSION;
            i.host_page_size = xh_core::sys::page_size() as u32;
            i.timebase_hz = cpu::counter_hz();
        });
        logi!("host: not AArch64 Linux: the kernel cannot be entered here");
    }
}

/// --selftest: what must hold on a phone before anything else is worth trying. The process's exit code: 0 or 1.
pub fn selftest(config: &Config) -> i32 {
    let info = hostcalls::info();
    let mut ok = info.host_page_size <= 16384 && info.user_va_bits >= 39 && info.features & XH_FEATURE_PAC != 0;
    let name = c"xnuhost-selftest";
    // SAFETY: memfd_create with a NUL-terminated name; ftruncate and mmap of the descriptor it gave (kept to the exit).
    let mapped = unsafe {
        let fd = libc::memfd_create(name.as_ptr(), 0);
        fd >= 0 && libc::ftruncate(fd, 16384) == 0 && {
            let x = libc::mmap(
                std::ptr::null_mut(),
                16384,
                libc::PROT_READ | libc::PROT_WRITE | libc::PROT_EXEC,
                libc::MAP_SHARED,
                fd,
                0,
            );
            x != libc::MAP_FAILED
        }
    };
    let error = if mapped { "yes".to_owned() } else { xh_core::sys::strerror(xh_core::sys::errno()) };
    logi!("selftest: executable shared memfd mapping: {}", error);
    mem::map_limit();
    ok = ok && mapped;
    if mem::create(64 << 20).is_ok() && task::init(config).is_ok() {
        logi!(
            "selftest: task factory: started; system-call trapping (seccomp): {}",
            if task::seccomp() { "yes" } else { "NO" }
        );
        ok = ok && task::seccomp();
        task::shutdown();
    } else {
        ok = false;
    }
    logi!(
        "selftest: {}",
        if ok { "this host can run xnuhost" } else { "this host lacks something xnuhost needs (see above)" }
    );
    if ok {
        0
    } else {
        1
    }
}
