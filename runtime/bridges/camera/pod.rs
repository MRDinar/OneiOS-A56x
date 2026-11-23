// SPDX-License-Identifier: MIT
//! Plain wire structures as bytes: the `#[repr(C)]` structures of the camera's protocols (proto.rs, msg.rs), copied
//! to and from guest buffers, the ring and the socket as the C code copied them with memcpy.

/// A `#[repr(C)]` structure of integers only, with no padding: every byte pattern is a value.
///
/// # Safety
/// The implementing type must be `#[repr(C)]`, contain only integer fields (or arrays of them, or other `Pod` types),
/// and have no padding bytes.
pub unsafe trait Pod: Copy + Default + 'static {
    /// The structure's bytes.
    fn as_bytes(&self) -> &[u8] {
        // SAFETY: Pod types have no padding, so all size_of::<Self>() bytes are initialized.
        unsafe { std::slice::from_raw_parts((self as *const Self).cast::<u8>(), std::mem::size_of::<Self>()) }
    }

    /// The structure's bytes, writable.
    fn as_bytes_mut(&mut self) -> &mut [u8] {
        // SAFETY: as above, and any byte pattern is a valid value of a Pod type.
        unsafe { std::slice::from_raw_parts_mut((self as *mut Self).cast::<u8>(), std::mem::size_of::<Self>()) }
    }

    /// A structure from the first bytes of `bytes`; what `bytes` does not have stays zero (a short read, as the C
    /// code's memset-then-memcpy of the part that came).
    fn read_from(bytes: &[u8]) -> Self {
        let mut value = Self::default();
        let n = bytes.len().min(std::mem::size_of::<Self>());
        value.as_bytes_mut()[..n].copy_from_slice(&bytes[..n]);
        value
    }

    /// Writes the structure into the start of `out` (as much of it as fits). Returns the bytes written.
    fn write_to(&self, out: &mut [u8]) -> usize {
        let n = out.len().min(std::mem::size_of::<Self>());
        out[..n].copy_from_slice(&self.as_bytes()[..n]);
        n
    }
}

// SAFETY: integers, arrays of them: no padding, every pattern valid.
unsafe impl Pod for u32 {}
// SAFETY: as above.
unsafe impl Pod for u64 {}
// SAFETY: as above.
unsafe impl<const N: usize> Pod for [u32; N] where [u32; N]: Default {}
