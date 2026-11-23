// SPDX-License-Identifier: MIT
//! The camera bridge (README.md here; docs/hardware.md section 7): the Galaxy S24 Ultra's cameras for iOS.
//!
//! ```text
//!  camera HAL (Samsung's provider, One UI's kernel)                      guest (XNU in xnuhost)
//!         |  binder                                                              ^
//!  oneios-camera  -- NV12 frames -->  memfd ring  -->  xnuhost: oneios,camera  --+-- drivers/camera
//!  (helper process)  <-- START/STOP -- SOCK_SEQPACKET --  (virtio-mmio device)
//! ```
//!
//! The helper process (`bin/oneios-camera`, [`helper`]) runs a [`core::Core`]: one back end ([`core::TestBackend`];
//! on Android also the Camera2 NDK, `backend_ndk`, and the provider HAL over binder, `backend_hal`), its pictures
//! converted to NV12 ([`convert`]) straight into the frame [`ring`]. xnuhost's end is [`link`] (starts the helper,
//! maps the ring read-only) and [`virtio_camera`] (the `oneios,camera` device of [`proto`]). `camctl` ([`camctl`]) is
//! the phone test.
//!
//! The Android back ends call libcamera2ndk, libmediandk, libbinder_ndk and libnativewindow (shared libraries): only
//! the two programs link them (build.rs); xnuhost, static, links this library without them and never reaches that
//! code (it uses [`link`] and [`virtio_camera`] only).

pub mod pod;
pub mod util;

pub mod convert;
pub mod core;
pub mod meta;
pub mod msg;
pub mod proto;
pub mod ring;
pub mod spawn;

pub mod camctl;
pub mod helper;
pub mod link;
pub mod virtio_camera;

#[cfg(target_os = "android")]
pub mod android;
pub mod backend_hal;
pub mod backend_ndk;
