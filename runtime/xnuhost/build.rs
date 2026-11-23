// SPDX-License-Identifier: MIT
//! xnuhost's build script.
//!
//! - `cfg(xh_machine)`: the target can run the kernel (AArch64 Linux or Android): the vCPUs, the host-call thunks
//!   (arch.rs) and the task layer are built; elsewhere (the x86_64 PC's host tests) their stand-ins are.
//! - On such a target the task stub (runtime/xnuhost/stub, its own no_std crate for aarch64-unknown-none) is built by
//!   stub/build.sh into OUT_DIR/stub.bin, which arch.rs embeds (`STUB`).
use std::path::PathBuf;
use std::process::Command;

fn main() {
    println!("cargo::rustc-check-cfg=cfg(xh_machine)");
    let arch = std::env::var("CARGO_CFG_TARGET_ARCH").unwrap_or_default();
    let os = std::env::var("CARGO_CFG_TARGET_OS").unwrap_or_default();
    if arch != "aarch64" || !(os == "linux" || os == "android") {
        return;
    }
    println!("cargo::rustc-cfg=xh_machine");
    let here = PathBuf::from(std::env::var("CARGO_MANIFEST_DIR").expect("CARGO_MANIFEST_DIR"));
    let out = PathBuf::from(std::env::var("OUT_DIR").expect("OUT_DIR"));
    let stub = here.join("stub");
    for watched in ["build.sh", "Cargo.toml", "Cargo.lock", "build.rs", "stub.ld", "src"] {
        println!("cargo::rerun-if-changed={}", stub.join(watched).display());
    }
    println!("cargo::rerun-if-changed=../abi/src");
    println!("cargo::rerun-if-env-changed=NDK");
    println!("cargo::rerun-if-env-changed=ANDROID_NDK_HOME");
    let status = Command::new("sh")
        .arg(stub.join("build.sh"))
        .arg(out.join("stub.bin"))
        .env("STUB_TARGET_DIR", out.join("stub-target"))
        .status()
        .expect("sh: stub/build.sh");
    assert!(status.success(), "stub/build.sh failed: the task stub is not built");
}
