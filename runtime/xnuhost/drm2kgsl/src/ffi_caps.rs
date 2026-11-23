// SPDX-License-Identifier: MIT
//! glue/oneios_glue.h's struct oneios_d2k_caps.

/// What the probe found (the capset's msm part).
#[repr(C)]
#[derive(Clone, Copy, Debug, Default)]
pub struct Caps {
    pub chip_id: u64,
    pub va_start: u64,
    pub va_size: u64,
    pub gpu_id: u32,
    pub priorities: u32,
}
