// SPDX-License-Identifier: MIT
//! Picture conversion for the camera helper: Android's three-plane YUV 4:2:0 to NV12 (what the guest gets), NV12 to
//! RGB and BGRA, and a PNG writer for camctl (stored blocks: no zlib on the phone side).
use std::io::{self, Write};

/// A YUV 4:2:0 picture as Android gives it: three planes with their own strides. `c_step` is the distance between
/// two samples of one chroma plane in a row (1 planar, 2 interleaved). The planes may overlap (NV12, NV21).
#[derive(Clone, Copy, Debug)]
pub struct Yuv<'a> {
    pub y: &'a [u8],
    pub cb: &'a [u8],
    pub cr: &'a [u8],
    pub y_stride: usize,
    pub c_stride: usize,
    pub c_step: usize,
    pub width: usize,
    pub height: usize,
}

impl<'a> Yuv<'a> {
    /// The bytes each plane spans for this geometry: (luma, chroma).
    pub fn spans(y_stride: usize, c_stride: usize, c_step: usize, width: usize, height: usize) -> (usize, usize) {
        let (cw, ch) = (width.div_ceil(2), height.div_ceil(2));
        let luma = if height == 0 { 0 } else { y_stride * (height - 1) + width };
        let chroma = if ch == 0 || cw == 0 { 0 } else { c_stride * (ch - 1) + (cw - 1) * c_step + 1 };
        (luma, chroma)
    }

    /// A picture from the planes' addresses, as Android's AImage and AHardwareBuffer give them.
    ///
    /// # Safety
    /// Each plane must be readable for the bytes [`Yuv::spans`] gives for this geometry, for as long as `'a`.
    #[allow(clippy::too_many_arguments)]
    pub unsafe fn from_raw(
        y: *const u8,
        cb: *const u8,
        cr: *const u8,
        y_stride: usize,
        c_stride: usize,
        c_step: usize,
        width: usize,
        height: usize,
    ) -> Yuv<'a> {
        let (luma, chroma) = Yuv::spans(y_stride, c_stride, c_step, width, height);
        // SAFETY: the caller's promise above.
        unsafe {
            Yuv {
                y: std::slice::from_raw_parts(y, luma),
                cb: std::slice::from_raw_parts(cb, chroma),
                cr: std::slice::from_raw_parts(cr, chroma),
                y_stride,
                c_stride,
                c_step,
                width,
                height,
            }
        }
    }
}

/// Into `dst` as NV12 with rows `dst_stride` apart (`cam_yuv_to_nv12`). The padding of a wider stride is not written.
pub fn yuv_to_nv12(src: &Yuv<'_>, dst: &mut [u8], dst_stride: usize) {
    let (w, h) = (src.width, src.height);
    let (cw, ch) = (w.div_ceil(2), h.div_ceil(2));
    for row in 0..h {
        let from = &src.y[row * src.y_stride..][..w];
        dst[row * dst_stride..][..w].copy_from_slice(from);
    }
    let out = &mut dst[dst_stride * h..];
    // Interleaved CbCr already (the usual gralloc layout): whole rows. The last sample of the last row of such a
    // plane may be one byte short of 2*cw, so that row is copied by samples.
    let nv12 = src.c_step == 2 && std::ptr::eq(src.cr.as_ptr(), src.cb.as_ptr().wrapping_add(1));
    for row in 0..ch {
        let line = &mut out[row * dst_stride..][..cw * 2];
        let at = row * src.c_stride;
        if nv12 && row + 1 < ch {
            line.copy_from_slice(&src.cb[at..at + cw * 2]);
            continue;
        }
        let (cb, cr) = (&src.cb[at..], &src.cr[at..]);
        for (x, pair) in line.chunks_exact_mut(2).enumerate() {
            pair[0] = cb[x * src.c_step];
            pair[1] = cr[x * src.c_step];
        }
    }
}

fn clamp8(v: i32) -> u8 {
    v.clamp(0, 255) as u8
}

/// BT.601 full range, 16.16 fixed point: R = Y + 1.402 Cr', G = Y - 0.344136 Cb' - 0.714136 Cr', B = Y + 1.772 Cb'.
fn pixel(y: u8, cb: u8, cr: u8) -> [u8; 3] {
    let (y, cb, cr) = (i32::from(y), i32::from(cb) - 128, i32::from(cr) - 128);
    [
        clamp8(y + ((91881 * cr + 32768) >> 16)),
        clamp8(y - ((22554 * cb + 46802 * cr + 32768) >> 16)),
        clamp8(y + ((116130 * cb + 32768) >> 16)),
    ]
}

/// Each pixel of an NV12 picture, row by row: (row, x, [r, g, b]).
fn each_pixel(nv12: &[u8], width: usize, height: usize, stride: usize, mut f: impl FnMut(usize, usize, [u8; 3])) {
    let chroma = &nv12[stride * height..];
    for row in 0..height {
        let y = &nv12[row * stride..][..width];
        let c = &chroma[(row / 2) * stride..];
        for (x, &luma) in y.iter().enumerate() {
            let even = x & !1;
            f(row, x, pixel(luma, c[even], c[even + 1]));
        }
    }
}

/// NV12 to packed RGB, `width * 3` bytes a row (BT.601 full range: Android's JFIF data space for camera YUV, as
/// iOS's 420f).
pub fn nv12_to_rgb(nv12: &[u8], width: usize, height: usize, stride: usize, rgb: &mut [u8]) {
    each_pixel(nv12, width, height, stride, |row, x, p| rgb[(row * width + x) * 3..][..3].copy_from_slice(&p));
}

/// NV12 to BGRA with rows `bgra_stride` apart.
pub fn nv12_to_bgra(nv12: &[u8], width: usize, height: usize, stride: usize, bgra: &mut [u8], bgra_stride: usize) {
    each_pixel(nv12, width, height, stride, |row, x, [r, g, b]| {
        bgra[row * bgra_stride + 4 * x..][..4].copy_from_slice(&[b, g, r, 255]);
    });
}

const CRC_TABLE: [u32; 256] = {
    let mut table = [0u32; 256];
    let mut i = 0;
    while i < 256 {
        let mut c = i as u32;
        let mut k = 0;
        while k < 8 {
            c = if c & 1 != 0 { 0xedb8_8320 ^ (c >> 1) } else { c >> 1 };
            k += 1;
        }
        table[i] = c;
        i += 1;
    }
    table
};

/// The CRC-32 of PNG and zlib, continued from `crc` (`cam_crc32`).
pub fn crc32(crc: u32, data: &[u8]) -> u32 {
    !data.iter().fold(!crc, |c, &b| CRC_TABLE[((c ^ u32::from(b)) & 0xff) as usize] ^ (c >> 8))
}

fn chunk(out: &mut impl Write, kind: &[u8; 4], data: &[u8]) -> io::Result<()> {
    let crc = crc32(crc32(0, kind), data);
    out.write_all(&(data.len() as u32).to_be_bytes())?;
    out.write_all(kind)?;
    out.write_all(data)?;
    out.write_all(&crc.to_be_bytes())
}

/// The zlib stream of the filtered rows (filter 0 before each row) in stored blocks of at most 65535 bytes.
fn stored_zlib(rgb: &[u8], width: usize, height: usize) -> Vec<u8> {
    let line = 1 + width * 3;
    let raw = line * height;
    let blocks = raw.div_ceil(65535);
    let mut z = Vec::with_capacity(2 + raw + blocks * 5 + 4);
    z.extend_from_slice(&[0x78, 0x01]);
    let (mut a, mut b) = (1u32, 0u32);
    let mut rows =
        (0..height).flat_map(|row| std::iter::once(0u8).chain(rgb[row * width * 3..][..width * 3].iter().copied()));
    let mut done = 0;
    while done < raw {
        let n = (raw - done).min(65535);
        z.push(u8::from(done + n == raw)); // BFINAL, BTYPE 00
        z.extend_from_slice(&(n as u16).to_le_bytes());
        z.extend_from_slice(&(!(n as u16)).to_le_bytes());
        for value in rows.by_ref().take(n) {
            z.push(value);
            a = (a + u32::from(value)) % 65521;
            b = (b + a) % 65521;
        }
        done += n;
    }
    z.extend_from_slice(&(b << 16 | a).to_be_bytes());
    z
}

/// An RGB picture as a PNG file (`cam_png_write`): EINVAL for a size outside 1..16384, the file's own errors else.
pub fn png_write(path: &str, rgb: &[u8], width: usize, height: usize) -> io::Result<()> {
    if !(1..=16384).contains(&width) || !(1..=16384).contains(&height) {
        return Err(io::Error::from_raw_os_error(libc::EINVAL));
    }
    let z = stored_zlib(rgb, width, height);
    let mut file = io::BufWriter::new(std::fs::File::create(path)?);
    let mut ihdr = [0u8; 13];
    ihdr[0..4].copy_from_slice(&(width as u32).to_be_bytes());
    ihdr[4..8].copy_from_slice(&(height as u32).to_be_bytes());
    ihdr[8] = 8; // 8 bits
    ihdr[9] = 2; // RGB
    file.write_all(b"\x89PNG\r\n\x1a\n")?;
    chunk(&mut file, b"IHDR", &ihdr)?;
    chunk(&mut file, b"IDAT", &z)?;
    chunk(&mut file, b"IEND", &[])?;
    file.flush()
}

#[cfg(test)]
mod tests {
    use super::*;
    use xh_core::bytes::be;

    #[test]
    fn to_nv12() {
        const W: usize = 6;
        const H: usize = 4;
        let mut y = [[0u8; 8]; H];
        let (mut cb, mut cr) = ([[0u8; 4]; 2], [[0u8; 4]; 2]);
        for (r, row) in y.iter_mut().enumerate() {
            for (x, v) in row.iter_mut().enumerate() {
                *v = if x < W { (10 * r + x) as u8 } else { 0xee };
            }
        }
        for r in 0..2 {
            for x in 0..4 {
                cb[r][x] = if x < 3 { (100 + 10 * r + x) as u8 } else { 0xee };
                cr[r][x] = if x < 3 { (200 + 10 * r + x) as u8 } else { 0xee };
            }
        }
        let flat = |p: &[[u8; 4]; 2]| p.concat();
        let (yf, cbf, crf) = (y.concat(), flat(&cb), flat(&cr));
        // Planar (I420) with row padding.
        let planar = Yuv { y: &yf, cb: &cbf, cr: &crf, y_stride: 8, c_stride: 4, c_step: 1, width: W, height: H };
        let mut out = [0u8; W * H * 3 / 2];
        yuv_to_nv12(&planar, &mut out, W);
        let mut ok = true;
        for r in 0..H {
            for x in 0..W {
                ok &= out[r * W + x] == (10 * r + x) as u8;
            }
        }
        for r in 0..2 {
            for x in 0..3 {
                ok &= out[W * H + r * W + 2 * x] == (100 + 10 * r + x) as u8
                    && out[W * H + r * W + 2 * x + 1] == (200 + 10 * r + x) as u8;
            }
        }
        assert!(ok);
        // Into a wider stride: the padding is not written.
        let mut padded = [0x55u8; 8 * H * 3 / 2];
        yuv_to_nv12(&planar, &mut padded, 8);
        assert!(padded[5] == 5 && padded[6] == 0x55);
        assert!(padded[8 * H + 4] == 102 && padded[8 * H + 5] == 202 && padded[8 * H + 6] == 0x55);
        // Interleaved CbCr (gralloc's NV12): the chroma plane's last byte does not exist in the buffer.
        let mut semi = [[0u8; 8]; 2];
        for (r, row) in semi.iter_mut().enumerate() {
            for x in 0..3 {
                row[2 * x] = (100 + 10 * r + x) as u8;
                row[2 * x + 1] = (200 + 10 * r + x) as u8;
            }
        }
        let s = semi.concat();
        let chroma_len = 8 + 5; // a plane of two rows 8 apart, three samples 2 apart: 13 bytes, Cr one after Cb
        let nv12 = Yuv {
            y: &yf,
            cb: &s[..chroma_len],
            cr: &s[1..1 + chroma_len],
            y_stride: 8,
            c_stride: 8,
            c_step: 2,
            width: W,
            height: H,
        };
        let mut again = [0u8; W * H * 3 / 2];
        yuv_to_nv12(&nv12, &mut again, W);
        assert_eq!(out, again);
        // Interleaved CrCb (NV21): swapped back.
        for (r, row) in semi.iter_mut().enumerate() {
            for x in 0..3 {
                row[2 * x] = (200 + 10 * r + x) as u8;
                row[2 * x + 1] = (100 + 10 * r + x) as u8;
            }
        }
        let s = semi.concat();
        let nv21 = Yuv {
            y: &yf,
            cb: &s[1..1 + chroma_len],
            cr: &s[..chroma_len],
            y_stride: 8,
            c_stride: 8,
            c_step: 2,
            width: W,
            height: H,
        };
        let mut again = [0u8; W * H * 3 / 2];
        yuv_to_nv12(&nv21, &mut again, W);
        assert_eq!(out, again);
    }

    #[test]
    fn colours() {
        // BT.601 full range: grey, white, black, and the three primaries within 2 steps.
        let colours: [[u8; 6]; 6] = [
            [128, 128, 128, 128, 128, 128],
            [255, 128, 128, 255, 255, 255],
            [0, 128, 128, 0, 0, 0],
            [76, 85, 255, 254, 0, 0],
            [150, 44, 21, 0, 255, 1],
            [29, 255, 107, 0, 0, 254],
        ];
        for [y, cb, cr, r, g, b] in colours {
            let picture = [y, y, y, y, cb, cr];
            let (mut rgb, mut bgra) = ([0u8; 12], [0u8; 16]);
            nv12_to_rgb(&picture, 2, 2, 2, &mut rgb);
            nv12_to_bgra(&picture, 2, 2, 2, &mut bgra, 8);
            let near = |a: u8, b: u8| a.abs_diff(b) <= 2;
            assert!(near(rgb[9], r) && near(rgb[10], g) && near(rgb[11], b));
            assert!(bgra[12] == rgb[11] && bgra[13] == rgb[10] && bgra[14] == rgb[9] && bgra[15] == 255);
        }
    }

    #[test]
    fn png() {
        const W: usize = 301;
        const H: usize = 217; // 196 kB of rows: more than one stored block
        let dir = std::env::temp_dir().join(format!("oneios-camera-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("picture.png");
        let path = path.to_str().unwrap();
        let rgb: Vec<u8> = (0..W * H * 3).map(|i| (i * 7 + i / 251) as u8).collect();
        png_write(path, &rgb, W, H).unwrap();
        let file = std::fs::read(path).unwrap();
        let size = file.len();
        assert!(size > rgb.len() && &file[..8] == b"\x89PNG\r\n\x1a\n");
        // IHDR, one IDAT, IEND: lengths, types and CRCs.
        assert!(be::u32(&file, 8) == 13 && &file[12..16] == b"IHDR" && be::u32(&file, 16) == W as u32);
        assert!(be::u32(&file, 20) == H as u32 && file[24] == 8 && file[25] == 2);
        assert_eq!(be::u32(&file, 29), crc32(0, &file[12..29]));
        let idat = be::u32(&file, 33) as usize;
        assert!(&file[37..41] == b"IDAT" && 41 + idat + 4 + 12 == size);
        assert_eq!(be::u32(&file, 41 + idat), crc32(0, &file[37..41 + idat]));
        assert!(be::u32(&file, size - 12) == 0 && &file[size - 8..size - 4] == b"IEND");
        assert_eq!(be::u32(&file, size - 4), crc32(0, &file[size - 8..size - 4]));
        // The zlib stream: stored blocks that add up to the filtered rows, and its Adler-32.
        let z = &file[41..41 + idat];
        let mut raw = Vec::new();
        let (mut at, mut last) = (2, false);
        let mut sane = z[0] == 0x78 && (u32::from(z[0]) * 256 + u32::from(z[1])) % 31 == 0;
        while sane && !last && at + 5 <= idat {
            let n = usize::from(z[at + 1]) | usize::from(z[at + 2]) << 8;
            let inverse = usize::from(z[at + 3]) | usize::from(z[at + 4]) << 8;
            last = z[at] == 1;
            sane = z[at] <= 1 && (n ^ inverse) == 0xffff && at + 5 + n <= idat;
            if sane {
                raw.extend_from_slice(&z[at + 5..at + 5 + n]);
            }
            at += 5 + n;
        }
        assert!(sane && last && raw.len() == (W * 3 + 1) * H && at + 4 == idat);
        let (mut a, mut b) = (1u32, 0u32);
        for &v in &raw {
            a = (a + u32::from(v)) % 65521;
            b = (b + a) % 65521;
        }
        assert_eq!(be::u32(z, at), b << 16 | a);
        for r in 0..H {
            let line = &raw[r * (W * 3 + 1)..][..W * 3 + 1];
            assert!(line[0] == 0 && line[1..] == rgb[r * W * 3..][..W * 3]);
        }
        assert_eq!(png_write("/nonexistent-dir/x.png", &rgb, W, H).unwrap_err().raw_os_error(), Some(libc::ENOENT));
        assert_eq!(png_write(path, &rgb, 0, H).unwrap_err().raw_os_error(), Some(libc::EINVAL));
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
