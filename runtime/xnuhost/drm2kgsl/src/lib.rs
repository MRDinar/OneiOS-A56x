// SPDX-License-Identifier: MIT
//! Droid-VM's drm2kgsl (external/virglrenderer src/drm/drm2kgsl, MIT): virglrenderer's DRM native context for the msm
//! protocol, served on /dev/kgsl-3d0, built from its C by build.rs with drm2kgsl/patches applied, and this safe front
//! over it. xnuhost's `gpu3d_kgsl` (runtime/xnuhost/src/gpu3d_kgsl.rs) is the renderer around it; gpuctl drives it
//! alone. The renderer's tables (contexts, resources) are the process's: every call here takes one lock, so the
//! front is safe from any thread. The two callbacks (a log line, a retired fence) come from inside the calls or from
//! drm2kgsl's own threads, and must not call back into the front.
//!
//! Without external/virglrenderer the C is not built ([`AVAILABLE`] is false): the same functions are there, and say
//! ENODEV.

pub mod sys;

#[cfg(drm2kgsl)]
mod ffi;
#[cfg(not(drm2kgsl))]
#[path = "absent.rs"]
mod ffi;
mod ffi_caps;
mod renderer;
pub use renderer::*;

/// Whether this build has the renderer (external/virglrenderer was there).
pub const AVAILABLE: bool = cfg!(drm2kgsl);

/// KGSL's device node (drm2kgsl_renderer.h KGSL_DEVICE_NODE).
pub const DEVICE_NODE: &str = "/dev/kgsl-3d0";

/// A failure as an errno (positive: `EINVAL`, `ENOMEM`, ...): xh-core's.
pub use xh_core::sys::Errno;
