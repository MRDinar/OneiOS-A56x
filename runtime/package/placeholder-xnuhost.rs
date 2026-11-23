// SPDX-License-Identifier: MIT
//! Placeholder for bin/xnuhost until the booter's build exists (runtime/package.sh, when there is no runtime/build.sh):
//! prints one line and exits 2. No libc: one static arm64 executable that runs on any Android kernel, so the OneOS fork
//! can test download, verification, staging and the exec itself. Built by runtime/package.sh with rustc alone:
//!   rustc --edition 2021 --target aarch64-linux-android -C opt-level=s -C panic=abort -C linker=<NDK clang>
//!         -C link-arg=-nostdlib -C link-arg=-static -C link-arg=-Wl,-e,_start -C link-arg=-Wl,--build-id=none -C strip=symbols
#![no_std]
#![no_main]

const MSG: &[u8] = b"xnuhost: not built yet (placeholder in the OneiOS runtime package)\n";
const SYS_WRITE: usize = 64;
const SYS_EXIT_GROUP: usize = 94;

/// One Linux system call with three arguments.
///
/// # Safety
/// The caller passes a call number and arguments that are valid for it.
#[cfg(target_arch = "aarch64")]
unsafe fn syscall3(number: usize, a: usize, b: usize, c: usize) -> usize {
    let ret: usize;
    // SAFETY: `svc #0` with the AArch64 Linux convention: the number in x8, arguments in x0-x2, the result in x0.
    unsafe {
        core::arch::asm!("svc #0", inlateout("x0") a => ret, in("x1") b, in("x2") c, in("x8") number,
                         options(nostack));
    }
    ret
}

#[no_mangle]
#[cfg(target_arch = "aarch64")]
pub extern "C" fn _start() -> ! {
    // SAFETY: write(1, MSG, len) reads only MSG; exit_group takes no pointer.
    unsafe {
        syscall3(SYS_WRITE, 1, MSG.as_ptr() as usize, MSG.len());
        loop {
            syscall3(SYS_EXIT_GROUP, 2, 0, 0);
        }
    }
}

#[panic_handler]
fn panic(_: &core::panic::PanicInfo) -> ! {
    loop {}
}
