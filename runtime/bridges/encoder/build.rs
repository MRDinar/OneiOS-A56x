// SPDX-License-Identifier: MIT
//! The two programs (oneios-encoder, encctl) are dynamic on Android: MediaCodec is libmediandk, a shared library (the
//! C Makefile's LIBS). Only the programs link it (`rustc-link-arg-bins`): xnuhost links this crate's library
//! statically and never reaches that code.
fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("android") {
        for lib in ["mediandk", "dl", "m"] {
            println!("cargo:rustc-link-arg-bins=-l{lib}");
        }
    }
}
