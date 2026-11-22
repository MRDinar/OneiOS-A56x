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
use std::fmt;

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

/// A failure as an errno (positive: `EINVAL`, `ENOMEM`, ...).
#[derive(Clone, Copy, PartialEq, Eq)]
pub struct Errno(pub i32);

impl Errno {
    /// The errno of a C call's negative return (`-EINVAL` -> `EINVAL`); 0 and positive values are `Ok`.
    pub fn check(r: i32) -> Result<(), Errno> {
        if r < 0 {
            Err(Errno(-r))
        } else {
            Ok(())
        }
    }

    /// The last OS error's errno.
    pub fn last() -> Errno {
        Errno(std::io::Error::last_os_error().raw_os_error().unwrap_or(libc::EIO))
    }
}

impl fmt::Debug for Errno {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Errno({}: {})", self.0, self)
    }
}

impl fmt::Display for Errno {
    /// strerror's text, as the C's `%s` of strerror(errno).
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        // SAFETY: strerror returns a valid NUL-terminated string for any value (an "Unknown error" one at worst); it
        // is copied before anything else can call it.
        let text = unsafe { std::ffi::CStr::from_ptr(libc::strerror(self.0)) };
        f.write_str(&text.to_string_lossy())
    }
}
