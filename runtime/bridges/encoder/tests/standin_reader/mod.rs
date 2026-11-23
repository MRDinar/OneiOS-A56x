// SPDX-License-Identifier: MIT
//! The tests' reader of the stand-in encoder's streams (tests/standin_reader.h): it splits an Annex-B access unit,
//! takes the emulation prevention bytes out, and reads the I_PCM samples of the HEVC slice back into a picture, so a
//! test can say that the frame it fed is the frame in the stream. It reads only what the stand-in writes (one slice,
//! 16x16 blocks, the PCM depths of its list); a real decoder's reading is tests/decode.sh's ffmpeg comparison.
#![allow(dead_code)]

/// One NAL unit of an access unit: its bytes (with its header) and its type.
#[derive(Clone, Copy, Debug)]
pub struct Nal<'a> {
    pub nal: &'a [u8],
    pub kind: u8,
}

/// The NAL units of an access unit (4-byte start codes), up to `max`.
pub fn split(au: &[u8], hevc: bool, max: usize) -> Vec<Nal<'_>> {
    let size = au.len();
    let start_code = |i: usize| au[i] == 0 && au[i + 1] == 0 && au[i + 2] == 0 && au[i + 3] == 1;
    let mut out = Vec::new();
    let mut i = 0;
    while i + 4 < size && out.len() < max {
        if !start_code(i) {
            i += 1;
            continue;
        }
        let start = i + 4;
        let mut end = start;
        while end + 3 < size && !start_code(end) {
            end += 1;
        }
        if end + 3 >= size {
            end = size;
        }
        let kind = if hevc { (au[start] >> 1) & 0x3f } else { au[start] & 0x1f };
        out.push(Nal { nal: &au[start..end], kind });
        i = end;
    }
    out
}

/// The payload of a NAL unit without its header and its emulation prevention bytes.
pub fn rbsp(nal: &Nal<'_>, hevc: bool) -> Vec<u8> {
    let header = if hevc { 2 } else { 1 };
    let mut out = Vec::with_capacity(nal.nal.len());
    let mut zeros = 0;
    for &b in nal.nal.iter().skip(header) {
        if zeros >= 2 && b == 3 {
            zeros = 0;
            continue;
        }
        out.push(b);
        zeros = if b != 0 { 0 } else { zeros + 1 };
    }
    out
}

const DEPTHS: [(u32, u32); 6] = [(8, 8), (4, 4), (3, 2), (2, 2), (2, 1), (1, 1)];

/// The HEVC slice's samples as planar 4:2:0 (w x h luma, then Cb, then Cr), cropped from the 16x16 blocks, and the
/// PCM depths found (luma, chroma). `out` holds w * h + 2 * (w/2) * (h/2) bytes (w, h even). None when the slice is
/// not the stand-in's.
pub fn hevc_picture(rbsp: &[u8], w: u32, h: u32, out: &mut [u8]) -> Option<(u32, u32)> {
    let (columns, rows) = (w.div_ceil(16), h.div_ceil(16));
    let blocks = (columns * rows) as usize;
    // The slice header is one byte; each block: two arithmetic-coded bytes (the end of the block before, part_mode,
    // pcm_flag) and its samples; the last end_of_slice_segment_flag: two bytes more.
    let size = rbsp.len();
    if size < 3 || rbsp[0] != 0xaf || (size - 3) % blocks != 0 {
        return None;
    }
    let each = (size - 3) / blocks;
    let (dl, dc) = DEPTHS.iter().copied().rfind(|&(l, c)| each == (2 + 32 * l + 16 * c) as usize)?;
    let (w, h) = (w as usize, h as usize);
    let (luma, chroma) = out.split_at_mut(w * h);
    let (cb, cr) = chroma.split_at_mut((w / 2) * (h / 2));
    for b in 0..blocks {
        let p = &rbsp[1 + b * each + 2..]; // after the block's two arithmetic-coded bytes
        let (bx, by) = (b % columns as usize, b / columns as usize);
        let mut bit = 0usize;
        let mut read = |depth: u32| {
            let mut v = 0u32;
            for _ in 0..depth {
                v = (v << 1) | u32::from((p[bit >> 3] >> (7 - (bit & 7))) & 1);
                bit += 1;
            }
            v
        };
        for y in 0..16 {
            for x in 0..16 {
                let v = read(dl);
                if bx * 16 + x < w && by * 16 + y < h {
                    luma[(by * 16 + y) * w + bx * 16 + x] = (v << (8 - dl)) as u8;
                }
            }
        }
        for plane in [&mut *cb, &mut *cr] {
            for y in 0..8 {
                for x in 0..8 {
                    let v = read(dc);
                    if bx * 8 + x < w / 2 && by * 8 + y < h / 2 {
                        plane[(by * 8 + y) * (w / 2) + bx * 8 + x] = (v << (8 - dc)) as u8;
                    }
                }
            }
        }
    }
    Some((dl, dc))
}

/// Whether the planar picture `got` is the NV12 frame `nv12` within the PCM depths' steps (the dither rounds up by
/// less than a step, and the top code saturates).
pub fn same_picture(got: &[u8], nv12: &[u8], w: u32, h: u32, luma_depth: u32, chroma_depth: u32) -> bool {
    let (w, h) = (w as usize, h as usize);
    let (luma_step, chroma_step) = (1i32 << (8 - luma_depth), 1i32 << (8 - chroma_depth));
    let near = |a: u8, b: u8, step: i32| (i32::from(a) - i32::from(b)).abs() < step;
    if !(0..w * h).all(|i| near(got[i], nv12[i], luma_step)) {
        return false;
    }
    let quarter = (w / 2) * (h / 2);
    let (cb, cr, uv) = (&got[w * h..], &got[w * h + quarter..], &nv12[w * h..]);
    (0..quarter).all(|i| near(cb[i], uv[2 * i], chroma_step) && near(cr[i], uv[2 * i + 1], chroma_step))
}
