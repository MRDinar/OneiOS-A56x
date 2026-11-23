// SPDX-License-Identifier: MIT
//! Generated Mach-O fixtures for the loader tests and mkkernel (tests/fixture.h): no Apple binaries. The layout
//! follows DarwinBoot's Test/make-fixtures.py (a __TEXT with the header and code, a zero-fill __DATA,
//! LC_UNIXTHREAD).

use xh_core::bytes::le;

pub const BASE: u64 = 0xffff_fff0_0700_4000;

/// An LC_SEGMENT_64 command (no sections) at the start of `p`; returns its length, 72.
pub fn segment(p: &mut [u8], name: &str, addr: u64, size: u64, off: u64, len: u64, prot: u32) -> usize {
    p[..72].fill(0);
    le::put_u32(p, 0, 0x19);
    le::put_u32(p, 4, 72);
    let n = name.len().min(16);
    p[8..8 + n].copy_from_slice(&name.as_bytes()[..n]);
    le::put_u64(p, 24, addr);
    le::put_u64(p, 32, size);
    le::put_u64(p, 40, off);
    le::put_u64(p, 48, len);
    le::put_u32(p, 56, prot);
    le::put_u32(p, 60, prot);
    72
}

/// An LC_UNIXTHREAD (ARM_THREAD_STATE64) with this pc at the start of `p`; returns its length, 288.
pub fn thread(p: &mut [u8], pc: u64) -> usize {
    p[..288].fill(0);
    le::put_u32(p, 0, 5);
    le::put_u32(p, 4, 288);
    le::put_u32(p, 8, 6);
    le::put_u32(p, 12, 68);
    le::put_u64(p, 272, pc);
    288
}

/// The Mach-O header: magic, arm64, the subtype, the file type, the command count and size.
pub fn header(p: &mut [u8], subtype: u32, filetype: u32, ncmds: u32, sizeofcmds: usize) {
    le::put_u32(p, 0, 0xfeed_facf);
    le::put_u32(p, 4, 0x0100_000c);
    le::put_u32(p, 8, subtype);
    le::put_u32(p, 12, filetype);
    le::put_u32(p, 16, ncmds);
    le::put_u32(p, 20, sizeofcmds as u32);
}

/// MH_EXECUTE at `base`: __TEXT (header, `code` at +0x1000; text_size bytes, at least 0x2000) and a zero-fill
/// __DATA of data_size after it. With `symbols`, an LC_SYMTAB with _fixture_symbol = base + 0x1008 (tables inside
/// __TEXT's file bytes at 0x800).
pub fn kernel_sized(
    base: u64,
    subtype: u32,
    code: &[u8],
    symbols: bool,
    text_size: usize,
    data_size: usize,
) -> Vec<u8> {
    let mut im = vec![0u8; text_size];
    let mut c = 32;
    c += segment(&mut im[c..], "__TEXT", base, text_size as u64, 0, text_size as u64, 5);
    c += segment(&mut im[c..], "__DATA", base + text_size as u64 + 0x2000, data_size as u64, 0, 0, 3);
    c += thread(&mut im[c..], base + 0x1000);
    let mut ncmds = 3;
    if symbols {
        for (i, v) in [2, 24, 0x800, 1, 0x810, 32].into_iter().enumerate() {
            le::put_u32(&mut im, c + 4 * i, v);
        }
        c += 24;
        ncmds += 1;
        le::put_u32(&mut im, 0x800, 1);
        im[0x804] = 0x0f;
        im[0x805] = 1;
        le::put_u64(&mut im, 0x808, base + 0x1008);
        im[0x811..0x811 + 16].copy_from_slice(b"_fixture_symbol\0");
    }
    header(&mut im, subtype, 2, ncmds, c - 32);
    assert!(code.len() <= text_size - 0x1000, "the code does not fit the fixture's text");
    im[0x1000..0x1000 + code.len()].copy_from_slice(code);
    im
}

pub fn kernel(base: u64, subtype: u32, code: &[u8], symbols: bool) -> Vec<u8> {
    kernel_sized(base, subtype, code, symbols, 0x2000, 0x1000)
}

/// MH_FILESET kernel collection: collection header page at `base`, then the kernel member (an MH_EXECUTE as above,
/// MH_DYLIB_IN_CACHE set, file offsets absolute) at +0x4000, then one kext member "com.example.kext" at +0xc000.
pub fn collection(base: u64, subtype: u32, code: &[u8]) -> Vec<u8> {
    let mut im = vec![0u8; 0x10000];
    let kbase = base + 0x4000;
    let k = kernel(kbase, subtype, code, true);
    im[0x4000..0x4000 + k.len()].copy_from_slice(&k);
    le::put_u32(&mut im, 0x4000 + 24, 0x8000_0000); // MH_DYLIB_IN_CACHE
    le::put_u64(&mut im, 0x4000 + 32 + 40, 0x4000); // __TEXT fileoff, absolute
    le::put_u32(&mut im, 0x4000 + 32 + 72 + 72 + 288 + 8, 0x4000 + 0x800); // symoff
    le::put_u32(&mut im, 0x4000 + 32 + 72 + 72 + 288 + 16, 0x4000 + 0x810); // stroff
    header(&mut im[0xc000..], subtype, 11, 0, 0); // the kext member: header only
    le::put_u32(&mut im, 0xc000 + 24, 0x8000_0000);
    let mut c = 32;
    c += segment(&mut im[c..], "__TEXT", base, 0x4000, 0, 0x4000, 1);
    c += segment(&mut im[c..], "__PRELINK_TEXT", kbase, 0x2000, 0x4000, 0x2000, 5);
    c += segment(&mut im[c..], "__DATA", kbase + 0x4000, 0x1000, 0, 0, 3);
    c += segment(&mut im[c..], "__KEXT", base + 0xc000, 0x4000, 0xc000, 0x4000, 5);
    c += thread(&mut im[c..], kbase + 0x1000);
    for (name, off) in [("com.apple.kernel", 0x4000u64), ("com.example.kext", 0xc000)] {
        im[c..c + 56].fill(0);
        le::put_u32(&mut im, c, 0x8000_0035);
        le::put_u32(&mut im, c + 4, 56);
        le::put_u64(&mut im, c + 8, base + off);
        le::put_u64(&mut im, c + 16, off);
        le::put_u32(&mut im, c + 24, 32);
        im[c + 32..c + 32 + name.len()].copy_from_slice(name.as_bytes());
        c += 56;
    }
    header(&mut im, subtype, 12, 7, c - 32);
    im
}

/// One version 2 trust cache module with `entries` sorted CDHashes, in the sidecar envelope.
pub fn trustcache(entries: usize) -> Vec<u8> {
    let mut im = vec![0u8; 8 + 24 + entries * 24];
    le::put_u32(&mut im, 0, 1);
    le::put_u32(&mut im, 4, 8);
    le::put_u32(&mut im, 8, 2);
    for i in 0..16 {
        im[12 + i] = i as u8;
    }
    le::put_u32(&mut im, 28, entries as u32);
    for e in 0..entries {
        let h = 32 + e * 24;
        im[h..h + 20].fill(0x10 + e as u8);
        im[h + 20] = 2;
    }
    im
}
