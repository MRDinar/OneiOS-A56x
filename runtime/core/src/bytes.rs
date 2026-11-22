// SPDX-License-Identifier: MIT
//! Fixed-width numbers in byte buffers: the wire layouts of virtio, binder, Mach-O, ELF, the device tree, the image
//! set and the display protocol. One reader and writer per byte order, in place of the `u32_at`/`le32`/`rd32` each
//! C file had.
//!
//! ```
//! use xh_core::bytes::{be, le};
//! let mut b = [0u8; 8];
//! le::put_u32(&mut b, 4, 0x1234_5678);
//! assert_eq!(le::u32(&b, 4), 0x1234_5678);
//! assert_eq!(be::u16(&b, 6), 0x3412);
//! assert_eq!(le::try_u64(&b, 1), None);
//! ```
//!
//! The plain readers and writers panic when the buffer is too short, as an index does (the callers checked the
//! length first); the `try_` readers give `None` instead.

macro_rules! order {
    ($module:ident, $from:ident, $to:ident, $doc:literal) => {
        #[doc = $doc]
        pub mod $module {
            order!(@one $from, $to, u16, try_u16, put_u16);
            order!(@one $from, $to, u32, try_u32, put_u32);
            order!(@one $from, $to, u64, try_u64, put_u64);
            order!(@one $from, $to, i16, try_i16, put_i16);
            order!(@one $from, $to, i32, try_i32, put_i32);
            order!(@one $from, $to, i64, try_i64, put_i64);
        }
    };
    (@one $from:ident, $to:ident, $t:ident, $try:ident, $put:ident) => {
        #[doc = concat!("The `", stringify!($t), "` at byte `at` of `b`; panics when `b` is too short.")]
        #[inline]
        pub fn $t(b: &[u8], at: usize) -> $t {
            $t::$from(b[at..at + size_of::<$t>()].try_into().expect("a sub-slice of the number's size"))
        }

        #[doc = concat!("The `", stringify!($t), "` at byte `at` of `b`; `None` when `b` is too short.")]
        #[inline]
        pub fn $try(b: &[u8], at: usize) -> Option<$t> {
            Some($t::$from(b.get(at..at.checked_add(size_of::<$t>())?)?.try_into().ok()?))
        }

        #[doc = concat!("Writes the `", stringify!($t), "` `v` at byte `at` of `b`; panics when `b` is too short.")]
        #[inline]
        pub fn $put(b: &mut [u8], at: usize, v: $t) {
            b[at..at + size_of::<$t>()].copy_from_slice(&v.$to());
        }
    };
}

order!(le, from_le_bytes, to_le_bytes, "Little-endian (virtio, Mach-O, ELF, the device tree, the image set).");
order!(be, from_be_bytes, to_be_bytes, "Big-endian (network byte order: Ethernet, IP, DHCP; PNG).");
order!(ne, from_ne_bytes, to_ne_bytes, "The host's own order (kernel structures: binder, netlink, input events).");

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn orders() {
        let mut b = [0u8; 16];
        le::put_u64(&mut b, 0, 0x0102_0304_0506_0708);
        assert_eq!(b[..8], [8, 7, 6, 5, 4, 3, 2, 1]);
        assert_eq!(le::u64(&b, 0), 0x0102_0304_0506_0708);
        assert_eq!(le::u32(&b, 4), 0x0102_0304);
        assert_eq!(le::u16(&b, 0), 0x0708);
        be::put_u32(&mut b, 8, 0xdead_beef);
        assert_eq!(b[8..12], [0xde, 0xad, 0xbe, 0xef]);
        assert_eq!(be::u32(&b, 8), 0xdead_beef);
        assert_eq!(be::u16(&b, 10), 0xbeef);
        ne::put_i32(&mut b, 12, -2);
        assert_eq!(ne::i32(&b, 12), -2);
        assert_eq!(ne::u32(&b, 12), u32::from_ne_bytes((-2i32).to_ne_bytes()));
        assert_eq!(le::try_u32(&b, 12), Some(le::u32(&b, 12)));
        assert_eq!(le::try_u32(&b, 13), None);
        assert_eq!(le::try_u64(&b, usize::MAX), None);
    }

    #[test]
    #[should_panic]
    fn short() {
        le::u32(&[0u8; 3], 0);
    }
}
