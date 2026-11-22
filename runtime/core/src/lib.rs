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
//! - [`sys`], [`time`]: small system helpers (errno texts, the monotonic clock).
pub mod hooks;
pub mod insn;
pub mod log;
pub mod mmio;
pub mod pixels;
pub mod regs;
pub mod sys;
pub mod time;
pub mod virtio;
