// SPDX-License-Identifier: MIT
//! Port of kctx.c: code that runs in the kernel's context (the kernel's TPIDR_EL0 and x18, the kernel's stack).
//!
//! The C was built with -fno-stack-protector (its guard lives in bionic's TLS). Rust has no stack protector, but the
//! same rules hold for what is written here: no thread-local (TPIDR_EL0 is the kernel's), nothing that can panic
//! (the panic path takes locks and thread-locals), no allocation, no lock, no logging; only loads and stores of
//! statics and the vCPU block, and the kernel's own upcall. x18 is reserved on Android (LLVM does not allocate it),
//! so the kernel's x18 survives.

use crate::cpu::UPCALLS;
use std::sync::atomic::Ordering::Relaxed;
use xnuhost_abi::{XhCpu, XhFrame, XH_FRAME_IRQ};

/// A host call is returning and an interrupt was noted while it ran. Deliver it as the kernel's interrupt upcall,
/// called like a function: the frame says where the kernel "was" (`pc`, `sp`: the thunk's return path, arch.rs).
///
/// # Safety
/// Called only by the host-call thunks (arch.rs), in kernel context, with `cpu` the calling vCPU's block.
#[inline(never)]
pub unsafe extern "C" fn xh_kctx_irq_poll(cpu: *mut XhCpu, pc: u64, sp: u64) {
    let Some(irq) = UPCALLS.irq() else { return };
    let mut frame = XhFrame { kind: XH_FRAME_IRQ, pc, sp, ..XhFrame::default() };
    // SAFETY: `cpu` is a vCPU block (the thunk's x19), whose fields are atomics (cpu.rs CpuBlock).
    let block = unsafe { &*(cpu as *const crate::cpu::CpuBlock) };
    block.irq_masked.store(1, Relaxed);
    // SAFETY: the kernel's interrupt upcall with its own vCPU and a frame on this stack, as hostcall.h has it.
    unsafe { irq(cpu, &mut frame) };
    block.irq_masked.store(0, Relaxed);
}
