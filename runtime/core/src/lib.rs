// SPDX-License-Identifier: MIT
//! xh-core: the base every crate of the runtime builds on (runtime/RUST.md).
//!
//! - [`log`]: the one log of xnuhost and the bridges (`loge!`, `logi!`, `logd!`, `xh_log!`), the guest console, the
//!   async-signal-safe line.
//! - [`hooks`]: what devices need from xnuhost: an interrupt line ([`hooks::set_spi`]) and guest RAM ([`hooks::ram`]).
//! - [`regs`], [`insn`], [`mmio`]: a trapped kernel context's registers, AArch64 load/store and mrs/msr decoding, the
//!   board's MMIO bus and the emulation of a trapped load or store.
//! - [`virtio`]: the virtio-mmio transport (split queues, the device trait, the queue health report) and its test
//!   driver ([`virtio::testing`]).
//! - [`pixels`]: the display protocol's pixel formats and conversions (pixels.h).
//! - Helpers several crates need: [`bytes`] (numbers in byte buffers, per byte order), [`fs`] (small sysfs and /proc
//!   files, meminfo-style values), [`parse`] (numbers and sizes as strtoull, strtol, atoi and xh_parse_size),
//!   [`text`] (C's fixed-size cuts, C strings in buffers), [`thread`] (named threads), [`fd`] (O_NONBLOCK,
//!   FD_CLOEXEC, EINTR), [`sys`] (errno, its texts, [`sys::Errno`], the page size), [`ioctl`] (the numbers and the
//!   call), [`time`] (the monotonic clock).
pub mod bytes;
pub mod fd;
pub mod fs;
pub mod hooks;
pub mod insn;
pub mod ioctl;
pub mod log;
pub mod mmio;
pub mod parse;
pub mod pixels;
pub mod regs;
pub mod sys;
pub mod text;
pub mod thread;
pub mod time;
pub mod virtio;
