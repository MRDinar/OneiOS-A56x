// SPDX-License-Identifier: MIT
//! The ABI xnuhost shares with the hosted kernel and with the task stub: Rust mirrors of the C headers that stay C
//! because the kernel's patches include them (runtime/RUST.md). `tests/headers.rs` reads each header and checks every
//! constant, struct size and field offset mirrored here, and that the kernel patches carry the free-report headers
//! line for line.
//!
//! - [`hostcall`][]: runtime/xnuhost/include/xnuhost/hostcall.h (the host-call vector, the vCPU block, frames, traps)
//! - [`stub`][]: the stub's region, trap pages and service page (C: stub_abi.h, gone with the C stub; the kernel never
//!   included it, so this mirror and runtime/xnuhost/stub are its only copies)
//! - [`free_report`][]: runtime/xnuhost/include/xnuhost/free_report.h and free_report_runs.h (kernel 0058, 0066)
#![no_std]

pub mod free_report;
pub mod hostcall;
pub mod stub;

pub use hostcall::*;
pub use stub::*;
