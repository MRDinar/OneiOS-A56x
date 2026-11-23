// SPDX-License-Identifier: MIT
//! The two programs (oneios-camera, camctl) are dynamic on Android: the back ends call Android's camera, media,
//! binder and window libraries, which are shared libraries (the C Makefile's LIBS). Only the programs link them
//! (`rustc-link-arg-bins`): xnuhost links this crate's library statically and never reaches that code.
fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("android") {
        for lib in ["camera2ndk", "mediandk", "binder_ndk", "nativewindow", "dl", "m"] {
            println!("cargo:rustc-link-arg-bins=-l{lib}");
        }
    }
}
