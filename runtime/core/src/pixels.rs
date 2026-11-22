// SPDX-License-Identifier: MIT
//! The guest frame's pixel formats between xnuhost and the display helper (display.sock's hello, docs/booter.md 2.8),
//! and the conversions between them: the Rust side of `runtime/xnuhost/include/xnuhost/pixels.h`, which stays C for
//! the kernel (a test here reads it and checks every constant mirrored).
//!
//! - [`BGRX8`] (1): bytes B,G,R,X, 8 bits a channel, sRGB-encoded, sRGB primaries. iOS's 'BGRA' framebuffer:
//!   QuartzCore composites this display in sRGB (CA::WindowServer::IOMFBDisplay::compositing_color_space is
//!   CAGetColorSpace(36), kCGColorSpaceSRGB, in 24A446), so colours outside sRGB (Display P3's reds and greens) are
//!   clipped before the frame leaves the guest.
//! - [`RGBA16F`] (2): R,G,B,A IEEE 754 half floats, little-endian: linear light, sRGB primaries, extended range:
//!   components below 0 and above 1 carry colours outside sRGB and EDR highlights. iOS's 'RGhA' framebuffer
//!   (Android's RGBA_FP16 with the dataspace SCRGB_LINEAR is the same thing).
use std::sync::OnceLock;

/// `XH_PIXELS_BGRX8`.
pub const BGRX8: u32 = 1;
/// `XH_PIXELS_RGBA16F`.
pub const RGBA16F: u32 = 2;
/// The scanout format a guest gives half-float surfaces in (RESOURCE_CREATE_2D, SET_SCANOUT_BLOB): xnuhost's own, not
/// virtio's (virtio-gpu has no half-float format; its codes are small numbers): IOSurface's FourCC 'RGhA'. The guest's
/// display (kernel/patches/oneios-hosted/display) sends it only when xnuhost passed oneios-display-wide=1
/// (`XH_VIRTIO_GPU_FORMAT_RGBA16F`).
pub const VIRTIO_GPU_FORMAT_RGBA16F: u32 = 0x5247_6841;
/// [`lit`]'s sampling step (`XH_LIT_STEP`).
pub const LIT_STEP: u32 = 16;
/// [`lit`]'s black level, an 8-bit sRGB code (`XH_LIT_LEVEL`).
pub const LIT_LEVEL: u8 = 16;

/// A pixel format of the display protocol.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Format {
    /// [`BGRX8`].
    Bgrx8,
    /// [`RGBA16F`].
    Rgba16f,
}

impl Format {
    /// The format of a protocol code, `None` for an unknown one.
    pub fn from_code(code: u32) -> Option<Format> {
        match code {
            BGRX8 => Some(Format::Bgrx8),
            RGBA16F => Some(Format::Rgba16f),
            _ => None,
        }
    }

    /// Its protocol code.
    pub fn code(self) -> u32 {
        match self {
            Format::Bgrx8 => BGRX8,
            Format::Rgba16f => RGBA16F,
        }
    }

    /// Bytes a pixel.
    pub fn bpp(self) -> u32 {
        match self {
            Format::Bgrx8 => 4,
            Format::Rgba16f => 8,
        }
    }

    /// Its name in the log.
    pub fn name(self) -> &'static str {
        match self {
            Format::Bgrx8 => "B,G,R,X 8-bit sRGB",
            Format::Rgba16f => "R,G,B,A half float, linear extended sRGB",
        }
    }
}

/// Bytes a pixel of format code `format`, 0 for an unknown one (`xh_pixels_bpp`).
pub fn bpp(format: u32) -> u32 {
    Format::from_code(format).map_or(0, Format::bpp)
}

/// The name of format code `format`, "unknown" for an unknown one (`xh_pixels_name`).
pub fn name(format: u32) -> &'static str {
    Format::from_code(format).map_or("unknown", Format::name)
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

/// binary32 from IEEE 754 binary16 (exact).
pub fn half_to_float(h: u16) -> f32 {
    let h = u32::from(h);
    let sign = (h & 0x8000) << 16;
    let mut exp = (h >> 10) & 0x1f;
    let mut mant = h & 0x3ff;
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

/// sRGB's transfer function, encoded to linear.
pub fn srgb_decode(v: f32) -> f32 {
    if v <= 0.04045 {
        v / 12.92
    } else {
        ((v + 0.055) / 1.055).powf(2.4)
    }
}

/// sRGB's transfer function, linear to encoded.
pub fn srgb_encode(v: f32) -> f32 {
    if v <= 0.003_130_8 {
        v * 12.92
    } else {
        1.055 * v.powf(1.0 / 2.4) - 0.055
    }
}

/// 8-bit sRGB code -> linear half (`xh_srgb8_to_half`), made once.
pub fn srgb8_to_half() -> &'static [u16; 256] {
    static TABLE: OnceLock<[u16; 256]> = OnceLock::new();
    TABLE.get_or_init(|| std::array::from_fn(|i| half_from_float(srgb_decode(i as f32 / 255.0))))
}

/// Linear half (all 65536 patterns) -> 8-bit sRGB code, clamped to sRGB's range (`xh_half_to_srgb8`): negative
/// values and NaN give 0, 1 and above (and +infinity) 255. What a frame of extended range loses on an 8-bit buffer.
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

/// A row of B,G,R,X 8-bit pixels to R,G,B,A half floats, alpha 1 (`xh_pixels_bgrx8_to_rgba16f`): as many pixels as
/// both slices hold.
pub fn bgrx8_to_rgba16f(to: &mut [u8], from: &[u8]) {
    let t = srgb8_to_half();
    for (out, px) in to.chunks_exact_mut(8).zip(from.chunks_exact(4)) {
        let halves = [t[usize::from(px[2])], t[usize::from(px[1])], t[usize::from(px[0])], 0x3c00];
        for (o, h) in out.chunks_exact_mut(2).zip(halves) {
            o.copy_from_slice(&h.to_le_bytes());
        }
    }
}

/// A row of R,G,B,A half floats to 8-bit pixels (`xh_pixels_rgba16f_to_8`): `rgb` false gives B,G,R,X bytes, true
/// R,G,B,X (X written 0xff); as many pixels as both slices hold.
pub fn rgba16f_to_8(to: &mut [u8], from: &[u8], rgb: bool) {
    let t = half_to_srgb8();
    for (out, px) in to.chunks_exact_mut(4).zip(from.chunks_exact(8)) {
        let c = |i: usize| t[usize::from(u16::from_le_bytes([px[2 * i], px[2 * i + 1]]))];
        let (r, g, b) = (c(0), c(1), c(2));
        out.copy_from_slice(&if rgb { [r, g, b, 0xff] } else { [b, g, r, 0xff] });
    }
}

/// Whether a frame shows anything (`xh_pixels_lit`): a sparse grid of its pixels (every [`LIT_STEP`]-th pixel of
/// every [`LIT_STEP`]-th row, starting half a step in) against black. A pixel is lit when one of R, G, B is above
/// [`LIT_LEVEL`] as an 8-bit sRGB code (a half float through the clamp to sRGB). At 1440x3120 that is 90 x 195 =
/// 17,550 pixels, one cache line each, not the 18 MB of the frame; a lit area of LIT_STEP x LIT_STEP pixels or more is
/// always seen (iOS's boot logo is hundreds of pixels across). xnuhost's ready rule (session/ready: the first frame of
/// the guest's own that is lit, docs/booter.md 2.8) uses it. Stops at the first lit pixel; an unknown format, or
/// samples beyond `pixels`, are not lit.
pub fn lit(pixels: &[u8], stride: usize, width: u32, height: u32, format: u32) -> bool {
    let Some(format) = Format::from_code(format) else { return false };
    let half = (format == Format::Rgba16f).then(half_to_srgb8);
    let need = if half.is_some() { 6 } else { 3 }; // the R, G, B bytes a sample reads
                                                   // (A frame smaller than a step is sampled in its middle: the tests' frames.)
    let x0 = if width >= LIT_STEP { LIT_STEP / 2 } else { width / 2 };
    let y0 = if height >= LIT_STEP { LIT_STEP / 2 } else { height / 2 };
    for y in (y0..height).step_by(LIT_STEP as usize) {
        let row = y as usize * stride;
        for x in (x0..width).step_by(LIT_STEP as usize) {
            let at = row + x as usize * format.bpp() as usize;
            let Some(p) = pixels.get(at..at + need) else { return false };
            let lit = match half {
                None => p[..3].iter().any(|&c| c > LIT_LEVEL),
                Some(t) => (0..3).any(|i| t[usize::from(u16::from_le_bytes([p[2 * i], p[2 * i + 1]]))] > LIT_LEVEL),
            };
            if lit {
                return true;
            }
        }
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    // The header stays C (the kernel and the display helper's C include it): every constant mirrored here must be its.
    #[test]
    fn mirrors_the_c_header() {
        let path = concat!(env!("CARGO_MANIFEST_DIR"), "/../xnuhost/include/xnuhost/pixels.h");
        let header = std::fs::read_to_string(path).expect("pixels.h");
        let define = |name: &str| -> u32 {
            let line = header
                .lines()
                .find(|l| l.split_whitespace().nth(1) == Some(name) && l.starts_with("#define"))
                .unwrap_or_else(|| panic!("{name} not in pixels.h"));
            let v = line.split_whitespace().nth(2).unwrap().trim_end_matches('u');
            match v.strip_prefix("0x") {
                Some(hex) => u32::from_str_radix(hex, 16).unwrap(),
                None => v.parse().unwrap(),
            }
        };
        assert_eq!(define("XH_PIXELS_BGRX8"), BGRX8);
        assert_eq!(define("XH_PIXELS_RGBA16F"), RGBA16F);
        assert_eq!(define("XH_VIRTIO_GPU_FORMAT_RGBA16F"), VIRTIO_GPU_FORMAT_RGBA16F);
        assert_eq!(define("XH_LIT_STEP"), LIT_STEP);
        assert_eq!(define("XH_LIT_LEVEL"), u32::from(LIT_LEVEL));
        assert!(header.contains("format == XH_PIXELS_BGRX8 ? 4u : format == XH_PIXELS_RGBA16F ? 8u : 0u"));
        for f in [Format::Bgrx8, Format::Rgba16f] {
            assert!(header.contains(&format!("\"{}\"", f.name())), "{f:?}");
        }
        assert!(header.contains("\"unknown\""));
        assert!(header.contains("0x3c00u"));
        assert_eq!(VIRTIO_GPU_FORMAT_RGBA16F.to_be_bytes(), *b"RGhA");
    }

    #[test]
    fn formats() {
        assert_eq!((bpp(BGRX8), bpp(RGBA16F), bpp(7)), (4, 8, 0));
        assert_eq!(name(3), "unknown");
        assert_eq!(Format::from_code(2).map(Format::code), Some(2));
    }

    #[test]
    fn halves() {
        assert_eq!(half_from_float(1.0), 0x3c00);
        assert_eq!(half_from_float(-1.0), 0xbc00);
        assert_eq!(half_from_float(0.5), 0x3800);
        assert_eq!(half_from_float(65504.0), 0x7bff);
        assert_eq!(half_from_float(65520.0), 0x7c00); // rounds up to infinity
        assert_eq!(half_from_float(1e9), 0x7c00);
        assert_eq!(half_from_float(f32::NAN) & 0x7e00, 0x7e00);
        assert_eq!(half_from_float(5.960_464_5e-8), 0x0001); // the smallest subnormal
        assert_eq!(half_from_float(1e-9), 0);
        assert_eq!(half_from_float(1.0 + 1.0 / 2048.0), 0x3c00); // a tie: to even
        assert_eq!(half_from_float(1.0 + 3.0 / 2048.0), 0x3c02);
        // every half that is not a NaN goes to a float and back unchanged
        for h in 0..=u16::MAX {
            let f = half_to_float(h);
            if !f.is_nan() {
                assert_eq!(half_from_float(f), h, "{h:#06x}");
            }
        }
    }

    #[test]
    fn tables_and_rows() {
        let to_half = srgb8_to_half();
        let to_8 = half_to_srgb8();
        assert_eq!((to_half[0], to_half[255]), (0, 0x3c00));
        for i in 0..=255u8 {
            assert_eq!(to_8[usize::from(to_half[usize::from(i)])], i, "{i}");
        }
        assert_eq!((to_8[0xbc00], to_8[0x7c00], to_8[0x7e00], to_8[0x4000]), (0, 255, 0, 255));
        let bgrx = [10u8, 128, 250, 0, 0, 0, 255, 7];
        let mut half = [0u8; 16];
        bgrx8_to_rgba16f(&mut half, &bgrx);
        assert_eq!(u16::from_le_bytes([half[6], half[7]]), 0x3c00);
        assert_eq!(u16::from_le_bytes([half[0], half[1]]), to_half[250]);
        let mut back = [0u8; 8];
        rgba16f_to_8(&mut back, &half, false);
        assert_eq!(back, [10, 128, 250, 0xff, 0, 0, 255, 0xff]);
        rgba16f_to_8(&mut back, &half, true);
        assert_eq!(back, [250, 128, 10, 0xff, 255, 0, 0, 0xff]);
    }

    // test_xnuhost.c test_pixels_lit: an all-black 1440x3120 frame (the X byte set or not), the boot console's kind of
    // frame (a few rows of text), a few lit pixels on the sampled grid, a lit 16x16 block anywhere, a lit pixel off the
    // grid (not seen: the check is sparse), the level at the threshold, and the same in half floats.
    #[test]
    fn lit_frames() {
        let (w, h) = (1440u32, 3120u32);
        let stride = w as usize * 4;
        let mut f = vec![0u8; h as usize * stride];
        assert!(!lit(&f, stride, w, h, BGRX8)); // black
        f.iter_mut().skip(3).step_by(4).for_each(|b| *b = 0xff);
        assert!(!lit(&f, stride, w, h, BGRX8)); // black, X 0xff: not alpha, not content
        assert!(!lit(&f, stride, w, h, 99) && !lit(&[], stride, w, h, BGRX8));
        // A console's picture: grey text in the top rows of a black frame (glyphs 8 px wide, 16 rows each).
        for y in 0..48usize {
            for x in 0..640usize {
                if (x / 2 + y) % 3 == 0 {
                    f[y * stride + x * 4..][..3].fill(0xaa);
                }
            }
        }
        assert!(lit(&f, stride, w, h, BGRX8));
        f.fill(0);
        f[8 * stride + 8 * 4 + 2] = 0x80; // one red pixel on the grid
        assert!(lit(&f, stride, w, h, BGRX8));
        f.fill(0);
        f[9 * stride + 9 * 4 + 1] = 0xff; // one pixel off the grid: missed
        assert!(!lit(&f, stride, w, h, BGRX8));
        f.fill(0);
        f[3000 * stride + 1432 * 4] = LIT_LEVEL; // at the level: still black
        assert!(!lit(&f, stride, w, h, BGRX8));
        f[3000 * stride + 1432 * 4] = LIT_LEVEL + 1; // above it: lit
        assert!(lit(&f, stride, w, h, BGRX8));
        // A lit 16x16 block (a logo is far larger) is seen wherever it lies.
        let mut all = true;
        'blocks: for by in (0..=h - 16).step_by(37) {
            for bx in (0..=w - 16).step_by(29) {
                f.fill(0);
                for y in by..by + 16 {
                    f[y as usize * stride + bx as usize * 4..][..16 * 4].fill(0xc0);
                }
                all = lit(&f, stride, w, h, BGRX8);
                if !all {
                    break 'blocks;
                }
            }
        }
        assert!(all);
        // Half floats: black (and negative values, which clamp to black), then one lit pixel on the grid.
        let hstride = w as usize * 8;
        let mut hf = vec![0u8; h as usize * hstride];
        assert!(!lit(&hf, hstride, w, h, RGBA16F));
        let set =
            |hf: &mut [u8], px: usize, c: usize, v: u16| hf[px * 8 + c * 2..][..2].copy_from_slice(&v.to_le_bytes());
        for i in 0..(w * h) as usize {
            set(&mut hf, i, 0, 0xbc00); // R -1
            set(&mut hf, i, 3, 0x3c00); // alpha 1
        }
        assert!(!lit(&hf, hstride, w, h, RGBA16F));
        set(&mut hf, 24 * w as usize + 40, 2, half_from_float(0.5)); // B 0.5 at (40,24)
        assert!(lit(&hf, hstride, w, h, RGBA16F));
    }
}
