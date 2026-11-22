// SPDX-License-Identifier: MIT
//! The guest frame's pixel formats (runtime/xnuhost/include/xnuhost/pixels.h): the parts of it the display helper
//! uses. TEMPORARY: xh-core's `pixels` module (owner: core) is the port of pixels.h that both sides of display.sock
//! are to share; it had not landed when this crate was ported (2026-11-23). When it lands this file becomes
//! `pub use xh_core::pixels::*;` (the names here are chosen to match) and its tests move there.
//!
//!   BGRX8    1  bytes B,G,R,X: 8 bits a channel, sRGB-encoded, sRGB primaries. iOS's 'BGRA' framebuffer.
//!   RGBA16F  2  R,G,B,A IEEE 754 half floats, little-endian: linear light, sRGB primaries, extended range.
use std::sync::OnceLock;

pub const BGRX8: u32 = 1;
pub const RGBA16F: u32 = 2;

/// Bytes a pixel (0: a format it does not know).
pub const fn bpp(format: u32) -> u32 {
    match format {
        BGRX8 => 4,
        RGBA16F => 8,
        _ => 0,
    }
}

pub const fn name(format: u32) -> &'static str {
    match format {
        BGRX8 => "B,G,R,X 8-bit sRGB",
        RGBA16F => "R,G,B,A half float, linear extended sRGB",
        _ => "unknown",
    }
}

/// IEEE 754 binary16 from binary32: round to nearest even, overflow to infinity, NaN kept a NaN.
pub fn half_from_float(value: f32) -> u16 {
    let f = value.to_bits();
    let sign = (f >> 16) & 0x8000;
    let exp = (f >> 23) & 0xff;
    let mut mant = f & 0x7f_ffff;
    if exp == 0xff {
        return (sign | 0x7c00 | if mant != 0 { 0x200 } else { 0 }) as u16;
    }
    let e = exp as i32 - 127 + 15;
    if e >= 31 {
        return (sign | 0x7c00) as u16;
    }
    if e <= 0 {
        // a subnormal half, or zero
        if e < -10 {
            return sign as u16;
        }
        mant |= 0x80_0000;
        let shift = (14 - e) as u32;
        let mut half = mant >> shift;
        let rest = mant & ((1 << shift) - 1);
        let mid = 1 << (shift - 1);
        if rest > mid || (rest == mid && half & 1 != 0) {
            half += 1;
        }
        return (sign | half) as u16;
    }
    let mut half = ((e as u32) << 10) | (mant >> 13);
    let rest = mant & 0x1fff;
    if rest > 0x1000 || (rest == 0x1000 && half & 1 != 0) {
        half += 1; // may carry into the exponent: still right
    }
    (sign | half) as u16
}

pub fn half_to_float(h: u16) -> f32 {
    let sign = ((h & 0x8000) as u32) << 16;
    let mut exp = ((h >> 10) & 0x1f) as u32;
    let mut mant = (h & 0x3ff) as u32;
    let f = if exp == 0x1f {
        sign | 0x7f80_0000 | (mant << 13)
    } else if exp != 0 {
        sign | ((exp + 112) << 23) | (mant << 13)
    } else if mant == 0 {
        sign
    } else {
        // subnormal: normalise
        exp = 113;
        while mant & 0x400 == 0 {
            mant <<= 1;
            exp -= 1;
        }
        sign | (exp << 23) | ((mant & 0x3ff) << 13)
    };
    f32::from_bits(f)
}

pub fn srgb_decode(v: f32) -> f32 {
    if v <= 0.04045 {
        v / 12.92
    } else {
        ((v + 0.055) / 1.055).powf(2.4)
    }
}

pub fn srgb_encode(v: f32) -> f32 {
    if v <= 0.0031308 {
        v * 12.92
    } else {
        1.055 * v.powf(1.0 / 2.4) - 0.055
    }
}

/// 8-bit sRGB code -> linear half.
pub fn srgb8_to_half() -> &'static [u16; 256] {
    static TABLE: OnceLock<[u16; 256]> = OnceLock::new();
    TABLE.get_or_init(|| std::array::from_fn(|i| half_from_float(srgb_decode(i as f32 / 255.0))))
}

/// Linear half (all 65536 patterns) -> 8-bit sRGB code, clamped to sRGB's range: negative values and NaN give 0,
/// 1 and above (and +infinity) 255. What a frame of extended range loses on an 8-bit buffer.
pub fn half_to_srgb8() -> &'static [u8; 65536] {
    static TABLE: OnceLock<Box<[u8; 65536]>> = OnceLock::new();
    TABLE.get_or_init(|| {
        let mut t = Box::new([0u8; 65536]);
        for (i, out) in t.iter_mut().enumerate() {
            let v = half_to_float(i as u16);
            *out = if v.is_nan() || v <= 0.0 {
                0 // negative, zero, NaN
            } else if v >= 1.0 {
                255
            } else {
                (srgb_encode(v) * 255.0 + 0.5) as u8
            };
        }
        t
    })
}

/// A row of B,G,R,X 8-bit pixels to R,G,B,A half floats (alpha 1).
pub fn bgrx8_to_rgba16f(to: &mut [u8], from: &[u8], pixels: usize) {
    let t = srgb8_to_half();
    for (d, s) in to.chunks_exact_mut(8).zip(from.chunks_exact(4)).take(pixels) {
        for (i, v) in [t[s[2] as usize], t[s[1] as usize], t[s[0] as usize], 0x3c00].into_iter().enumerate() {
            d[i * 2..i * 2 + 2].copy_from_slice(&v.to_le_bytes());
        }
    }
}

/// A row of R,G,B,A half floats to 8-bit pixels: rgb false gives B,G,R,X bytes, true R,G,B,X (X written 0xff).
pub fn rgba16f_to_8(to: &mut [u8], from: &[u8], pixels: usize, rgb: bool) {
    let t = half_to_srgb8();
    for (d, s) in to.chunks_exact_mut(4).zip(from.chunks_exact(8)).take(pixels) {
        let c = |i: usize| t[u16::from_le_bytes([s[i * 2], s[i * 2 + 1]]) as usize];
        let (r, g, b) = (c(0), c(1), c(2));
        d.copy_from_slice(&if rgb { [r, g, b, 0xff] } else { [b, g, r, 0xff] });
    }
}
