// SPDX-License-Identifier: MIT
//! Links the stub with stub.ld (a flat blob at 0: position independent, entry at offset 0).
fn main() {
    let dir = std::env::var("CARGO_MANIFEST_DIR").unwrap();
    println!("cargo:rerun-if-changed={dir}/stub.ld");
    println!("cargo:rustc-link-arg-bins=-T{dir}/stub.ld");
    println!("cargo:rustc-link-arg-bins=-static");
    println!("cargo:rustc-link-arg-bins=--no-dynamic-linker");
}
