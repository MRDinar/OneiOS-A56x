// SPDX-License-Identifier: MIT
//! The stand-in encoder alone (test_standin.c, then tests/decode.sh as `make test` ran them): for a list of frame
//! sizes and buffer sizes it feeds a test picture, takes the access unit, and checks that it fits the guest's coded
//! buffer (width * height / 2 + 64 KiB), that its NAL units are the ones a decoder needs in their order, that the
//! first block's arithmetic-coded bytes are the ones worked out by hand from the standard, and that the samples in
//! the HEVC slice are the frame's (within the PCM depth's step). Each stream is written with the picture a decoder
//! must give (NAME.hevc / NAME.h264, NAME.yuv: planar 4:2:0), and tests/decode.sh decodes them with ffmpeg when there
//! is one (it says so and passes when there is none).
mod standin_reader;

use oneencoder::backend::Backend;
use oneencoder::backend_stub::StubBackend;
use oneencoder::proto::{MEDIA_PIXFMT_H264, MEDIA_PIXFMT_HEVC, MEDIA_PIXFMT_NV12};
use std::path::Path;
use std::process::Command;

/// A picture with every level in it, edges, a black and a white corner block, and coloured chroma.
fn pattern(nv12: &mut [u8], w: u32, h: u32) {
    for y in 0..h {
        for x in 0..w {
            nv12[(y * w + x) as usize] = if x < 16 && y < 16 {
                0
            } else if x >= w - 16 && y >= h - 16 {
                255
            } else {
                ((x * 255 / (w - 1) + y * 255 / (h - 1)) / 2 + if (x ^ y) & 8 != 0 { 6 } else { 0 }) as u8
            };
        }
    }
    let uv = &mut nv12[(w * h) as usize..];
    for y in 0..h / 2 {
        for x in 0..w / 2 {
            let at = ((y * (w / 2) + x) * 2) as usize;
            uv[at] = (64 + x * 128 / (w / 2)) as u8;
            uv[at + 1] = (192 - y * 128 / (h / 2)) as u8;
        }
    }
}

/// One frame through the backend into `dir`; cap 0: the guest's coded buffer size. The failures, as lines.
fn one(dir: &Path, codec: u32, w: u32, h: u32, cap: u32, black: bool, failures: &mut Vec<String>) {
    let mut check = |ok: bool, what: &str| {
        if !ok {
            failures.push(format!("{codec:#x} {w}x{h} cap {cap}: {what}"));
        }
    };
    let hevc = codec == MEDIA_PIXFMT_HEVC;
    let (frame, budget) = ((w * h * 3 / 2) as usize, (w * h / 2 + 65536) as usize);
    let cap = if cap == 0 { budget } else { cap as usize };
    let mut nv12 = vec![0u8; frame];
    let mut au = vec![0u8; cap];
    let mut planar = vec![0u8; frame];
    let name = format!("{}-{w}x{h}-{cap}{}", if hevc { "hevc" } else { "h264" }, if black { "-black" } else { "" });
    if black {
        nv12[(w * h) as usize..].fill(128);
    } else {
        pattern(&mut nv12, w, h);
    }
    let Some(mut s) = StubBackend.open(codec, MEDIA_PIXFMT_NV12, w, h) else {
        check(false, "open");
        return;
    };
    check(s.feed(&nv12, 123456, true).is_ok(), "feed");
    let polled = s.poll(&mut au);
    let size = polled.map_or(0, |p| p.bytes);
    check(polled.is_ok_and(|p| p.bytes > 0 && p.bytes <= cap && p.bytes <= budget), "the access unit's size");
    check(polled.is_ok_and(|p| p.key && p.pts_us == 123456 && !p.eos), "key, pts, no eos");
    check(s.poll(&mut au).is_ok_and(|p| p.bytes == 0 && !p.eos), "nothing more");
    check(s.drain().is_ok(), "drain");
    check(s.poll(&mut []).is_ok_and(|p| p.bytes == 0 && p.eos), "the end of stream");
    drop(s);
    if size == 0 {
        check(false, &format!("no access unit ({polled:?})"));
        return;
    }
    let au = &au[..size];
    let nals = standin_reader::split(au, hevc, 8);
    let kinds: Vec<u8> = nals.iter().map(|n| n.kind).collect();
    let (mut luma, mut chroma, mut pcm) = (8, 8, true);
    if hevc {
        check(kinds == [32, 33, 34, 19], "VPS, SPS, PPS, IDR");
        // The profile, tier and level begin at the SPS's fourth byte (DroidVMVTHost.cpp copies them into the
        // hvcC): Main.
        check(nals.len() == 4 && nals[1].nal.len() > 15 && nals[1].nal[3] == 0x01 && nals[1].nal[4] == 0x60, "Main");
        let rbsp = if nals.len() == 4 { standin_reader::rbsp(&nals[3], true) } else { Vec::new() };
        let depths = standin_reader::hevc_picture(&rbsp, w, h, &mut planar);
        check(depths.is_some(), "the slice's samples");
        // The first block: part_mode 1 at state 0, then pcm_flag 1 and the flush, are the nine bits 100001101, then
        // seven alignment zeros (worked out by hand from 9.3.4.x: range 510 -> 270 -> 268, low 268, seven shifts).
        check(rbsp.len() > 3 && rbsp[1] == 0x86 && rbsp[2] == 0x80, "the first block's bytes 86 80");
        (luma, chroma) = depths.unwrap_or((8, 8));
        check(standin_reader::same_picture(&planar, &nv12, w, h, luma, chroma), "the picture");
    } else {
        check(kinds == [7, 8, 5], "SPS, PPS, IDR");
        check(nals.len() == 3 && nals[0].nal[1] == 66, "Baseline");
        pcm = size > frame; // raw samples, or the grey picture
        let (w, h) = (w as usize, h as usize);
        let quarter = (w / 2) * (h / 2);
        for i in 0..w * h {
            planar[i] = if pcm { nv12[i] } else { 128 };
        }
        for i in 0..quarter {
            planar[w * h + i] = if pcm { nv12[w * h + 2 * i] } else { 128 };
            planar[w * h + quarter + i] = if pcm { nv12[w * h + 2 * i + 1] } else { 128 };
        }
    }
    let kind = if hevc {
        match (luma, chroma) {
            (8, _) => "PCM 8+8 bits",
            (4, _) => "PCM 4+4 bits",
            (3, _) => "PCM 3+2 bits",
            (_, 2) => "PCM 2+2 bits",
            (2, _) => "PCM 2+1 bits",
            _ => "PCM 1+1 bits",
        }
    } else if pcm {
        "PCM"
    } else {
        "grey"
    };
    println!("{name}: {size} bytes of {cap}, {kind}");
    std::fs::write(dir.join(format!("{name}.{}", if hevc { "hevc" } else { "h264" })), au).unwrap();
    std::fs::write(dir.join(format!("{name}.yuv")), &planar).unwrap();
}

#[test]
fn standin_streams() {
    let dir = std::env::temp_dir().join(format!("oneios-encoder-streams-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let mut failures = Vec::new();
    for (w, h) in [(64, 64), (256, 256), (364, 382), (338, 158), (640, 480), (1440, 3120)] {
        one(&dir, MEDIA_PIXFMT_HEVC, w, h, 0, false, &mut failures);
        one(&dir, MEDIA_PIXFMT_H264, w, h, 0, false, &mut failures);
    }
    one(&dir, MEDIA_PIXFMT_HEVC, 256, 256, 8 << 20, false, &mut failures); // a large buffer changes nothing
    one(&dir, MEDIA_PIXFMT_HEVC, 640, 480, 0, true, &mut failures); // black: the most emulation prevention bytes
    one(&dir, MEDIA_PIXFMT_HEVC, 640, 480, 100000, false, &mut failures); // a small buffer: fewer levels
    one(&dir, MEDIA_PIXFMT_H264, 64, 64, 0, true, &mut failures);
    assert!(failures.is_empty(), "{failures:#?}");
    // The streams through a real decoder (tests/decode.sh: ffmpeg, or a line that there is none).
    let script = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/decode.sh");
    let out = Command::new("sh").arg(&script).arg(&dir).output().expect("sh runs");
    print!("{}", String::from_utf8_lossy(&out.stdout));
    assert!(
        out.status.success(),
        "decode.sh: {}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    std::fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn standin_refusals() {
    // A buffer nothing fits in is an error, not an overrun; a short frame is refused.
    let mut s = StubBackend.open(MEDIA_PIXFMT_HEVC, MEDIA_PIXFMT_NV12, 640, 480).unwrap();
    let nv12 = vec![0u8; 640 * 480 * 3 / 2];
    let mut small = [0u8; 64];
    assert!(s.feed(&nv12, 0, true).is_ok());
    assert!(s.poll(&mut small).is_err());
    assert!(s.feed(&nv12[..100], 0, true).is_err());
    assert!(StubBackend.open(MEDIA_PIXFMT_HEVC, MEDIA_PIXFMT_NV12, 0, 480).is_none());
}
