// SPDX-License-Identifier: MIT
//! The XNU loader (runtime/loader in C until the port): it parses a kernel Mach-O or kernel collection
//! ([`macho`]), builds and edits Apple device trees ([`adt`]), describes xnuhost's machine as one ([`machine`]) and
//! places the kernel, the tree and the boot sidecars in the kernel's memory window with its boot_args ([`boot`]).
//! [`fixture`] writes the Mach-O files of the tests and of mkkernel (no Apple files). docs/booter.md.
//!
//! Bins: `xnuloader-inspect` (what the loader sees in a kernel collection or a device tree; the board's tree) and
//! `mkkernel` (the test kernel's ELF as a kernel.kc, and its test disk).

pub mod adt;
pub mod boot;
pub mod fixture;
pub mod machine;
pub mod macho;

/// Why the loader refused something: the C loader's error strings, word for word.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Error(pub &'static str);

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.0)
    }
}

impl std::error::Error for Error {}

pub use adt::{Node, Prop};
pub use boot::{BootLayout, BootRequest};
pub use machine::{DeviceKind, Machine};
pub use macho::Kernel;
