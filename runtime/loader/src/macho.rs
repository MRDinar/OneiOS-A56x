// SPDX-License-Identifier: BSD-2-Clause-Patent
//! The kernel Mach-O / kernel collection parser of the OneiOS loader (macho.c).
//!
//! Ported from DarwinBoot (https://github.com/UniXNU/DarwinBoot, commit 8544957,
//! DarwinBootPkg/Driver/DarwinBoot.c: ParseKernelHeader, ParseKernel), Copyright (c) 2026 DarwinBoot contributors,
//! BSD-2-Clause-Patent. Changes: no EDK2, error strings, segment names, no 256 MiB window limit (the limit is the
//! caller's), a symbol lookup and a fileset entry list.

use crate::Error;
use xh_core::bytes::le;

pub const MAX_SEGMENTS: usize = 64;
pub const MAX_ENTRIES: usize = 1024;

pub const MH_MAGIC_64: u32 = 0xfeed_facf;
pub const CPU_TYPE_ARM64: u32 = 0x0100_000c;
pub const MH_EXECUTE: u32 = 2;
pub const MH_FILESET: u32 = 12;
pub const LC_SEGMENT_64: u32 = 0x19;
pub const LC_UNIXTHREAD: u32 = 0x5;
pub const LC_SYMTAB: u32 = 0x2;
pub const LC_FILESET_ENTRY: u32 = 0x8000_0035;
pub const KERNEL_ID: &str = "com.apple.kernel";
/// MH_DYLIB_IN_CACHE: a member of a kernel collection.
const MH_DYLIB_IN_CACHE: u32 = 0x8000_0000;

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Segment {
    /// up to 16 characters
    pub name: String,
    pub vmaddr: u64,
    pub vmsize: u64,
    pub fileoff: u64,
    pub filesize: u64,
    /// initprot: 1 read, 2 write, 4 execute
    pub prot: u32,
}

impl Segment {
    /// The segment covers [vmaddr, +vmsize).
    fn overlaps(&self, other: &Segment) -> bool {
        self.vmaddr < other.vmaddr + other.vmsize && other.vmaddr < self.vmaddr + self.vmsize
    }

    /// The file offset of `len` bytes at link address `va`, when this segment's file bytes hold them.
    fn file_offset(&self, va: u64, len: u64) -> Option<u64> {
        let delta = va.checked_sub(self.vmaddr)?;
        (delta <= self.filesize && len <= self.filesize - delta).then_some(self.fileoff + delta)
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct FilesetEntry {
    /// bundle identifier (63 characters at most)
    pub name: String,
    /// of the member's Mach-O header
    pub vmaddr: u64,
    pub fileoff: u64,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Kernel {
    pub segments: Vec<Segment>,
    /// lowest segment address, highest segment end (link addresses)
    pub base: u64,
    pub end: u64,
    /// LC_UNIXTHREAD pc (link address)
    pub entry: u64,
    /// low 24 bits: 0 arm64, 2 arm64e
    pub cpusubtype: u32,
    /// MH_FILESET kernel collection
    pub fileset: bool,
    /// file offset of the kernel's own header (0 for MH_EXECUTE)
    pub kernel_header: u64,
    pub entries: Vec<FilesetEntry>,
}

macro_rules! fail {
    ($text:literal) => {
        return Err(Error($text))
    };
}

/// One Mach-O header at file offset `header`: the collection's own (header 0) or a member's.
fn parse_header(data: &[u8], header: usize) -> Result<Kernel, Error> {
    let size = data.len();
    if header > size || size - header < 32 {
        fail!("file too small for a Mach-O header");
    }
    if le::u32(data, header) != MH_MAGIC_64 {
        fail!("not a 64-bit Mach-O");
    }
    if le::u32(data, header + 4) != CPU_TYPE_ARM64 {
        fail!("not an arm64 Mach-O");
    }
    let subtype = le::u32(data, header + 8) & 0x00ff_ffff;
    if subtype != 0 && subtype != 2 {
        fail!("CPU subtype is neither arm64 nor arm64e");
    }
    let kind = le::u32(data, header + 12);
    if kind != MH_EXECUTE && !(header == 0 && kind == MH_FILESET) {
        fail!("not MH_EXECUTE or a top-level MH_FILESET");
    }
    let commands_size = le::u32(data, header + 20) as usize;
    if commands_size > size - header - 32 {
        fail!("load commands run past the file");
    }

    let mut k = Kernel { cpusubtype: subtype, fileset: kind == MH_FILESET, ..Kernel::default() };
    let count = le::u32(data, header + 16);
    let end = header + 32 + commands_size;
    let mut offset = header + 32;
    let mut has_entry = false;
    for _ in 0..count {
        if end - offset < 8 {
            fail!("truncated load command");
        }
        let (command, length) = (le::u32(data, offset), le::u32(data, offset + 4) as usize);
        if length < 8 || length & 7 != 0 || length > end - offset {
            fail!("bad load command length");
        }
        if command == LC_SEGMENT_64 {
            if length < 72 || le::u32(data, offset + 64) as usize > (length - 72) / 80 {
                fail!("bad segment command");
            }
            if le::u64(data, offset + 32) != 0 {
                if k.segments.len() == MAX_SEGMENTS {
                    fail!("too many segments");
                }
                let s = Segment {
                    name: xh_core::text::nul_text(&data[offset + 8..offset + 24]),
                    vmaddr: le::u64(data, offset + 24),
                    vmsize: le::u64(data, offset + 32),
                    fileoff: le::u64(data, offset + 40),
                    filesize: le::u64(data, offset + 48),
                    prot: le::u32(data, offset + 60),
                };
                if s.vmaddr > u64::MAX - s.vmsize
                    || s.filesize > s.vmsize
                    || s.fileoff > size as u64
                    || s.filesize > size as u64 - s.fileoff
                {
                    fail!("segment outside the file or wrapping");
                }
                k.segments.push(s);
            }
        } else if command == LC_UNIXTHREAD {
            // ARM_THREAD_STATE64 (flavor 6, 68 words): pc is the 33rd 64-bit register.
            if has_entry || length != 288 || le::u32(data, offset + 8) != 6 || le::u32(data, offset + 12) != 68 {
                fail!("bad or repeated LC_UNIXTHREAD");
            }
            k.entry = le::u64(data, offset + 272);
            has_entry = true;
        }
        offset += length;
    }
    if offset != end {
        fail!("load commands do not fill their size");
    }
    if !has_entry {
        fail!("no LC_UNIXTHREAD entry point");
    }
    if k.segments.is_empty() {
        fail!("no segments");
    }
    k.base = k.segments.iter().map(|s| s.vmaddr).min().unwrap();
    k.end = k.segments.iter().map(|s| s.vmaddr + s.vmsize).max().unwrap();
    for (i, s) in k.segments.iter().enumerate() {
        if k.segments[..i].iter().any(|o| s.overlaps(o)) {
            fail!("segments overlap");
        }
    }
    let executable =
        k.segments.iter().any(|s| s.prot & 4 != 0 && k.entry >= s.vmaddr && k.entry - s.vmaddr < s.filesize);
    if !executable {
        fail!("entry point is not in an executable segment's file bytes");
    }
    if k.entry & 3 != 0 {
        fail!("entry point is not instruction aligned");
    }
    if k.base & 0xfff != 0 {
        fail!("kernel base is not page aligned");
    }
    Ok(k)
}

impl Kernel {
    /// Parses an arm64/arm64e MH_EXECUTE kernel or an MH_FILESET kernel collection. Nothing in the file is trusted:
    /// every offset and size is checked against the data. Fileset segment offsets are absolute in the collection.
    /// XNU rebases and signs its own fixup chains after entry; the loader only copies the collection's mappings.
    pub fn parse(data: &[u8]) -> Result<Kernel, Error> {
        let mut k = parse_header(data, 0)?;
        if !k.fileset {
            return Ok(k);
        }
        let size = data.len();
        let end = 32 + le::u32(data, 20) as usize;
        let mut offset = 32;
        let mut found = false;
        while offset < end {
            let length = le::u32(data, offset + 4) as usize;
            if le::u32(data, offset) == LC_FILESET_ENTRY {
                if length < 40 {
                    fail!("bad fileset entry");
                }
                let name = le::u32(data, offset + 24) as usize;
                if name < 32 || name >= length {
                    fail!("bad fileset entry name offset");
                }
                let Some(name_len) = data[offset + name..offset + length].iter().position(|b| *b == 0) else {
                    fail!("unterminated fileset entry name");
                };
                let name_bytes = &data[offset + name..offset + name + name_len];
                let (vmaddr, header) = (le::u64(data, offset + 8), le::u64(data, offset + 16));
                if header > size as u64 || size as u64 - header < 32 || header & 7 != 0 {
                    fail!("fileset member header outside the file");
                }
                // Every indexed header must actually be loaded at its advertised address.
                let covered = k.segments.iter().any(|p| {
                    header >= p.fileoff
                        && header - p.fileoff <= p.filesize
                        && 32 <= p.filesize - (header - p.fileoff)
                        && vmaddr == p.vmaddr.wrapping_add(header - p.fileoff)
                });
                if !covered {
                    fail!("fileset member header is not mapped at its address");
                }
                if k.entries.len() == MAX_ENTRIES {
                    fail!("too many fileset entries");
                }
                k.entries.push(FilesetEntry {
                    name: String::from_utf8_lossy(&name_bytes[..name_bytes.len().min(63)]).into_owned(),
                    vmaddr,
                    fileoff: header,
                });
                if name_bytes == KERNEL_ID.as_bytes() {
                    if found {
                        fail!("two com.apple.kernel entries");
                    }
                    let h = header as usize;
                    if le::u32(data, h + 12) != MH_EXECUTE
                        || le::u32(data, h + 8) != le::u32(data, 8)
                        || le::u32(data, h + 24) & MH_DYLIB_IN_CACHE == 0
                    {
                        fail!("com.apple.kernel is not an MH_EXECUTE member of this collection");
                    }
                    found = true;
                    let member = parse_header(data, h)?;
                    if member.entry != k.entry {
                        fail!("the collection's entry is not the kernel's");
                    }
                    if !member.segments.iter().all(|ch| k.segments.iter().any(|p| inside(ch, p))) {
                        fail!("a kernel segment is not inside a collection segment");
                    }
                    k.kernel_header = header;
                }
            }
            offset += length;
        }
        if !found {
            fail!("the collection has no com.apple.kernel");
        }
        Ok(k)
    }

    /// File offset of link address `va`, when a segment's file bytes cover [va, va+len).
    pub fn offset(&self, va: u64, len: u64) -> Option<u64> {
        self.segments.iter().find_map(|s| s.file_offset(va, len))
    }
}

/// A member's segment `ch` lies inside the collection's segment `p`: its addresses, no more protection, and its file
/// bytes at the same place (or none).
fn inside(ch: &Segment, p: &Segment) -> bool {
    let Some(delta) = ch.vmaddr.checked_sub(p.vmaddr) else { return false };
    delta <= p.vmsize
        && ch.vmsize <= p.vmsize - delta
        && ch.prot & !p.prot == 0
        && (ch.filesize == 0
            || (delta <= p.filesize && ch.filesize <= p.filesize - delta && ch.fileoff == p.fileoff + delta))
}

/// The value of an external or local symbol of the Mach-O whose header is at `header` (the kernel's own header:
/// [`Kernel::kernel_header`]): its link address, or None if it is absent or the tables are bad.
pub fn symbol(data: &[u8], header: u64, name: &str) -> Option<u64> {
    let size = data.len();
    let header = usize::try_from(header).ok()?;
    if header > size || size - header < 32 || le::u32(data, header) != MH_MAGIC_64 {
        return None;
    }
    let (count, bytes) = (le::u32(data, header + 16), le::u32(data, header + 20) as usize);
    if bytes > size - header - 32 {
        return None;
    }
    let mut offset = header + 32;
    let end = offset + bytes;
    for _ in 0..count {
        if end - offset < 8 {
            break;
        }
        let (command, length) = (le::u32(data, offset), le::u32(data, offset + 4) as usize);
        if length < 8 || length > end - offset {
            return None;
        }
        if command == LC_SYMTAB && length >= 24 {
            let symoff = le::u32(data, offset + 8) as usize;
            let nsyms = le::u32(data, offset + 12) as usize;
            let stroff = le::u32(data, offset + 16) as usize;
            let strsize = le::u32(data, offset + 20) as usize;
            if symoff > size || nsyms > (size - symoff) / 16 || stroff > size || strsize > size - stroff {
                return None;
            }
            let want = name.as_bytes();
            let strings = &data[stroff..stroff + strsize];
            return data[symoff..symoff + nsyms * 16].chunks_exact(16).find_map(|n| {
                let strx = le::u32(n, 0) as usize;
                // not a stab, defined in a section
                if n[4] & 0xe0 != 0 || n[4] & 0x0e != 0x0e || strx >= strsize || strsize - strx <= want.len() {
                    return None;
                }
                let s = &strings[strx..strx + want.len() + 1];
                (s[..want.len()] == *want && s[want.len()] == 0).then(|| le::u64(n, 8))
            });
        }
        offset += length;
    }
    None
}
