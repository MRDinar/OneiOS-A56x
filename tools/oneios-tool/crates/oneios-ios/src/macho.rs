//! A thin 64-bit Mach-O as the iOS steps read it: the header, the load commands, the symbol table.

use anyhow::{bail, Result};

pub const MH_MAGIC_64: u32 = 0xFEED_FACF;
pub const MH_EXECUTE: u32 = 2;
pub const MH_DYLIB: u32 = 6;
pub const MH_BUNDLE: u32 = 8;
pub const MH_TWOLEVEL: u32 = 0x80;
pub const LC_SYMTAB: u32 = 0x2;
pub const LC_LOAD_DYLIB: u32 = 0xC;
pub const LC_ID_DYLIB: u32 = 0xD;
pub const LC_SUB_FRAMEWORK: u32 = 0x12;
pub const LC_SEGMENT_64: u32 = 0x19;
pub const LC_CODE_SIGNATURE: u32 = 0x1D;
pub const LC_LAZY_LOAD_DYLIB: u32 = 0x20;
pub const LC_DYLD_INFO: u32 = 0x22;
pub const LC_DYLD_INFO_ONLY: u32 = 0x8000_0022;
pub const LC_LOAD_WEAK_DYLIB: u32 = 0x8000_0018;
pub const LC_REEXPORT_DYLIB: u32 = 0x8000_001F;
pub const LC_LOAD_UPWARD_DYLIB: u32 = 0x8000_0023;
pub const LC_BUILD_VERSION: u32 = 0x32;
pub const LC_DYLD_EXPORTS_TRIE: u32 = 0x8000_0033;

pub fn u32_at(d: &[u8], at: usize) -> Option<u32> {
    Some(u32::from_le_bytes(d.get(at..at.checked_add(4)?)?.try_into().ok()?))
}
pub fn u64_at(d: &[u8], at: usize) -> Option<u64> {
    Some(u64::from_le_bytes(d.get(at..at.checked_add(8)?)?.try_into().ok()?))
}
pub fn be32(d: &[u8], at: usize) -> Option<u32> {
    Some(u32::from_be_bytes(d.get(at..at.checked_add(4)?)?.try_into().ok()?))
}
/// The NUL-ended string at `at` (to the end when there is no NUL).
pub fn cstr(d: &[u8], at: usize) -> String {
    let tail = d.get(at..).unwrap_or(&[]);
    let end = tail.iter().position(|&b| b == 0).unwrap_or(tail.len());
    String::from_utf8_lossy(&tail[..end]).into_owned()
}

/// The load command's string at the offset its field `field` holds (a dylib's name, an umbrella's).
pub fn lc_str(cmd: &[u8], field: usize) -> String {
    match u32_at(cmd, field) {
        Some(off) => cstr(cmd, off as usize),
        None => String::new(),
    }
}

/// A dylib's version as tapi writes it: X, X.Y or X.Y.Z (the zeros at the end left out).
pub fn version_text(v: u32) -> String {
    let (x, y, z) = (v >> 16, (v >> 8) & 0xFF, v & 0xFF);
    match (y, z) {
        (0, 0) => format!("{x}"),
        (_, 0) => format!("{x}.{y}"),
        _ => format!("{x}.{y}.{z}"),
    }
}

pub struct MachO<'a> {
    pub data: &'a [u8],
    pub cputype: u32,
    pub cpusubtype: u32,
    pub filetype: u32,
    pub flags: u32,
    /// (cmd, the command's bytes)
    pub commands: Vec<(u32, &'a [u8])>,
}

/// One entry of the symbol table.
#[derive(Debug, Clone)]
pub struct Nlist {
    pub name: String,
    pub n_type: u8,
    pub n_desc: u16,
}

impl Nlist {
    pub fn is_stab(&self) -> bool {
        self.n_type & 0xE0 != 0
    }
    pub fn is_external(&self) -> bool {
        self.n_type & 0x01 != 0
    }
    pub fn is_undefined(&self) -> bool {
        self.n_type & 0x0E == 0
    }
    /// The library ordinal of an undefined symbol (two-level namespace): 1.. a dylib command, 0xFE looked up
    /// dynamically, 0xFF the executable, 0 this image.
    pub fn ordinal(&self) -> u8 {
        (self.n_desc >> 8) as u8
    }
    pub fn weak_ref(&self) -> bool {
        self.n_desc & 0x40 != 0
    }
}

impl<'a> MachO<'a> {
    pub fn parse(data: &'a [u8]) -> Result<MachO<'a>> {
        if u32_at(data, 0) != Some(MH_MAGIC_64) {
            bail!("not a thin 64-bit Mach-O");
        }
        let field = |i: usize| u32_at(data, 4 * i).unwrap_or(0);
        let (cputype, cpusubtype, filetype, ncmds, sizeofcmds, flags) = (field(1), field(2), field(3), field(4), field(5), field(6));
        let end = 32usize.saturating_add(sizeofcmds as usize).min(data.len());
        let mut at = 32usize;
        let mut commands = Vec::new();
        for _ in 0..ncmds {
            let (Some(cmd), Some(size)) = (u32_at(data, at), u32_at(data, at + 4)) else { bail!("the load commands are cut") };
            if size < 8 || at + size as usize > end {
                bail!("a load command is out of range");
            }
            commands.push((cmd, &data[at..at + size as usize]));
            at += size as usize;
        }
        Ok(MachO { data, cputype, cpusubtype, filetype, flags, commands })
    }

    pub fn arch(&self) -> String {
        match self.cputype {
            0x0100_000C if self.cpusubtype & 0xFF_FFFF == 2 => "arm64e".into(),
            0x0100_000C => "arm64".into(),
            0x0100_0007 => "x86_64".into(),
            other => format!("cpu-{other:#x}"),
        }
    }

    pub fn command(&self, cmd: u32) -> Option<&'a [u8]> {
        self.commands.iter().find(|(c, _)| *c == cmd).map(|(_, b)| *b)
    }

    /// (offset, size) of LC_CODE_SIGNATURE's data.
    pub fn signature(&self) -> Option<(u32, u32)> {
        let c = self.command(LC_CODE_SIGNATURE)?;
        Some((u32_at(c, 8)?, u32_at(c, 12)?))
    }

    /// (fileoff, filesize) of the __TEXT segment.
    pub fn text(&self) -> Option<(u64, u64)> {
        self.segment("__TEXT").map(|s| (s.fileoff, s.filesize))
    }

    pub fn segment(&self, name: &str) -> Option<Segment> {
        self.commands.iter().filter(|(c, _)| *c == LC_SEGMENT_64).find_map(|(_, b)| {
            let seg = Segment::read(b)?;
            (seg.name == name).then_some(seg)
        })
    }

    /// (platform, minos, sdk) of LC_BUILD_VERSION.
    pub fn build_version(&self) -> Option<(u32, u32, u32)> {
        let c = self.command(LC_BUILD_VERSION)?;
        Some((u32_at(c, 8)?, u32_at(c, 12)?, u32_at(c, 16)?))
    }

    pub fn install_name(&self) -> Option<String> {
        self.command(LC_ID_DYLIB).map(|c| lc_str(c, 8))
    }

    /// Every dylib command in order (their place is the library ordinal, from 1): (cmd, name).
    pub fn dylibs(&self) -> Vec<(u32, String)> {
        self.commands
            .iter()
            .filter(|(c, _)| matches!(*c, LC_LOAD_DYLIB | LC_LOAD_WEAK_DYLIB | LC_REEXPORT_DYLIB | LC_LOAD_UPWARD_DYLIB | LC_LAZY_LOAD_DYLIB))
            .map(|(c, b)| (*c, lc_str(b, 8)))
            .collect()
    }

    pub fn symbols(&self) -> Result<Vec<Nlist>> {
        let Some(c) = self.command(LC_SYMTAB) else { return Ok(Vec::new()) };
        let field = |i: usize| u32_at(c, 8 + 4 * i).unwrap_or(0) as usize;
        let (symoff, nsyms, stroff, strsize) = (field(0), field(1), field(2), field(3));
        let strings = self.data.get(stroff..stroff.saturating_add(strsize)).unwrap_or(&[]);
        let mut out = Vec::with_capacity(nsyms);
        for i in 0..nsyms {
            let at = symoff + 16 * i;
            let Some(entry) = self.data.get(at..at + 16) else { bail!("the symbol table is cut") };
            let strx = u32_at(entry, 0).unwrap_or(0) as usize;
            out.push(Nlist { name: cstr(strings, strx), n_type: entry[4], n_desc: u16::from_le_bytes([entry[6], entry[7]]) });
        }
        Ok(out)
    }
}

#[derive(Debug, Clone)]
pub struct Segment {
    pub name: String,
    pub vmaddr: u64,
    pub vmsize: u64,
    pub fileoff: u64,
    pub filesize: u64,
}

impl Segment {
    pub fn read(cmd: &[u8]) -> Option<Segment> {
        let raw = cmd.get(8..24)?;
        let end = raw.iter().position(|&b| b == 0).unwrap_or(16);
        Some(Segment {
            name: String::from_utf8_lossy(&raw[..end]).into_owned(),
            vmaddr: u64_at(cmd, 24)?,
            vmsize: u64_at(cmd, 32)?,
            fileoff: u64_at(cmd, 40)?,
            filesize: u64_at(cmd, 48)?,
        })
    }
}

/// (hash type, page size) of the first code directory of an embedded signature (macho-check's question).
pub fn code_directory(sig: &[u8]) -> Option<(u8, u32)> {
    if be32(sig, 0)? != 0xFADE_0CC0 {
        return None;
    }
    for i in 0..be32(sig, 8)? as usize {
        let offset = be32(sig, 12 + 8 * i + 4)? as usize;
        if be32(sig, offset)? == 0xFADE_0C02 {
            return Some((*sig.get(offset + 37)?, 1u32.checked_shl(*sig.get(offset + 39)? as u32)?));
        }
    }
    None
}

/// The blob of `magic` in an embedded signature (the first of that kind), with its 8-byte header.
pub fn signature_blob(sig: &[u8], magic: u32) -> Option<&[u8]> {
    if be32(sig, 0)? != 0xFADE_0CC0 {
        return None;
    }
    for i in 0..be32(sig, 8)? as usize {
        let offset = be32(sig, 12 + 8 * i + 4)? as usize;
        if be32(sig, offset)? == magic {
            let len = be32(sig, offset + 4)? as usize;
            return sig.get(offset..offset.checked_add(len)?);
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn versions_as_tapi_writes_them() {
        assert_eq!(version_text(1359 << 16), "1359");
        assert_eq!(version_text((382 << 16) | (5 << 8) | 3), "382.5.3");
        assert_eq!(version_text((2 << 16) | (1 << 8)), "2.1");
        assert_eq!(version_text(1 << 16), "1");
        assert_eq!(version_text(0), "0");
        assert_eq!(version_text((1 << 16) | 4), "1.0.4");
    }
}
