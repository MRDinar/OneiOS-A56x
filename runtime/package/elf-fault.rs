// SPDX-License-Identifier: MIT
//! A test input of runtime/package/elf-check-test.sh and runtime/package/test.sh, never part of the runtime.
//! It does what liboneios-display.so of 1f7dd1f did: it calls the NDK's dataspace functions, which libnativewindow.so has
//! and libandroid.so has not. Linked as a cdylib against libandroid only it links (without -z defs) and Android cannot
//! load it: `cannot locate symbol "ANativeWindow_getBuffersDataSpace"` (the phone, 2026-11-14). Built with rustc alone
//! (`--crate-type cdylib`, or `--crate-type bin --cfg elf_fault_main` for the executable the test also links).
use std::ffi::c_void;

#[link(name = "android")]
extern "C" {
    fn ANativeWindow_getBuffersDataSpace(window: *mut c_void) -> i32;
    fn ANativeWindow_setBuffersDataSpace(window: *mut c_void, dataspace: i32) -> i32;
}

/// The two calls, as jni.rs made them.
#[no_mangle]
pub extern "C" fn oneios_elf_fault(window: *mut c_void) -> i32 {
    // SAFETY: a test input that is linked and read, never run.
    unsafe { ANativeWindow_setBuffersDataSpace(window, ANativeWindow_getBuffersDataSpace(window)) }
}

#[cfg(elf_fault_main)]
fn main() {
    std::process::exit(oneios_elf_fault(std::ptr::null_mut()));
}
