// SPDX-License-Identifier: MIT
//! Links the test kernel with testkernel.ld: linked high, like XNU, so that running it proves the loader's slide.
fn main() {
    let dir = std::env::var("CARGO_MANIFEST_DIR").unwrap();
    println!("cargo:rerun-if-changed={dir}/testkernel.ld");
    println!("cargo:rustc-link-arg-bins=-T{dir}/testkernel.ld");
    println!("cargo:rustc-link-arg-bins=-static");
}
