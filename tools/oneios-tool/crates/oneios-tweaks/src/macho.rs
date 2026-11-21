//! What the owner trust list needs from a signed Mach-O: its CDHash, identifier, architecture and signature page
//! size (roottweak/macho.py). Read only; signing is done by the helper build (ldid or codesign).

use sha2::Digest;

const MH_MAGIC_64: u32 = 0xFEED_FACF;
const FAT_MAGIC: u32 = 0xCAFE_BABE;
const FAT_MAGIC_64: u32 = 0xCAFE_BABF;
const CPU_TYPE_ARM64: u32 = 0x0100_000C;
const LC_CODE_SIGNATURE: u32 = 0x1D;
const LC_BUILD_VERSION: u32 = 0x32;
const CSMAGIC_EMBEDDED_SIGNATURE: u32 = 0xFADE_0CC0;
const CSMAGIC_CODEDIRECTORY: u32 = 0xFADE_0C02;
const CS_ADHOC: u32 = 0x2;

/// One architecture of a signed file.
#[derive(Clone, Debug, PartialEq)]
pub struct Slice {
    pub cdhash: String,
    pub hash: &'static str,
    pub identifier: String,
    pub arch: String,
    pub page_size: u64,
    pub adhoc: bool,
    pub platform: Option<String>,
    pub minos: Option<String>,
}

fn be32(d: &[u8], at: usize) -> Result<u32, String> {
    d.get(at..at + 4).map(|b| u32::from_be_bytes(b.try_into().unwrap())).ok_or_else(|| "unpack_from requires a buffer of more bytes".to_string())
}
fn le32(d: &[u8], at: usize) -> Result<u32, String> {
    d.get(at..at + 4).map(|b| u32::from_le_bytes(b.try_into().unwrap())).ok_or_else(|| "unpack_from requires a buffer of more bytes".to_string())
}
fn be64(d: &[u8], at: usize) -> Result<u64, String> {
    d.get(at..at + 8).map(|b| u64::from_be_bytes(b.try_into().unwrap())).ok_or_else(|| "unpack_from requires a buffer of more bytes".to_string())
}

/// `data[a:b]` of Python: clamped.
fn slice(d: &[u8], a: usize, b: usize) -> &[u8] {
    let b = b.min(d.len());
    &d[a.min(b)..b]
}

fn slices(data: &[u8]) -> Result<Vec<(usize, usize)>, String> {
    if data.len() < 8 {
        return Err("too short for a Mach-O".into());
    }
    let magic = be32(data, 0)?;
    if magic == FAT_MAGIC || magic == FAT_MAGIC_64 {
        let count = be32(data, 4)?;
        if count > 16 {
            return Err("not a fat Mach-O".into());
        }
        let mut out = Vec::new();
        for i in 0..count as usize {
            if magic == FAT_MAGIC {
                let at = 8 + 20 * i;
                be32(data, at + 16)?;
                out.push((be32(data, at + 8)? as usize, be32(data, at + 12)? as usize));
            } else {
                let at = 8 + 32 * i;
                be32(data, at + 28)?;
                out.push((be64(data, at + 8)? as usize, be64(data, at + 16)? as usize));
            }
        }
        return Ok(out);
    }
    Ok(vec![(0, data.len())])
}

fn code_directories(blob: &[u8]) -> Result<Vec<&[u8]>, String> {
    if blob.len() < 12 {
        return Err("signature too short".into());
    }
    let (magic, length, count) = (be32(blob, 0)?, be32(blob, 4)? as usize, be32(blob, 8)?);
    if magic != CSMAGIC_EMBEDDED_SIGNATURE || length > blob.len() {
        return Err("no embedded signature".into());
    }
    let mut out = Vec::new();
    for i in 0..count as usize {
        be32(blob, 12 + 8 * i)?;
        let offset = be32(blob, 12 + 8 * i + 4)? as usize;
        if offset + 8 > length {
            return Err("signature index out of range".into());
        }
        let (blob_magic, blob_length) = (be32(blob, offset)?, be32(blob, offset + 4)? as usize);
        if blob_magic == CSMAGIC_CODEDIRECTORY {
            if offset + blob_length > length || blob_length < 44 {
                return Err("code directory out of range".into());
            }
            out.push(&blob[offset..offset + blob_length]);
        }
    }
    Ok(out)
}

fn platform_name(p: u32) -> String {
    match p {
        1 => "macos".into(),
        2 => "ios".into(),
        3 => "tvos".into(),
        4 => "watchos".into(),
        6 => "maccatalyst".into(),
        7 => "iossimulator".into(),
        n => n.to_string(),
    }
}

fn describe_slice(data: &[u8]) -> Result<Slice, String> {
    if data.len() < 32 {
        return Err("too short for a Mach-O".into());
    }
    let (magic, cputype, cpusubtype) = (le32(data, 0)?, le32(data, 4)?, le32(data, 8)?);
    let (ncmds, sizeofcmds) = (le32(data, 16)?, le32(data, 20)? as usize);
    if magic != MH_MAGIC_64 {
        return Err("not a 64-bit little-endian Mach-O".into());
    }
    let arch = if cputype == CPU_TYPE_ARM64 {
        if cpusubtype & 0x00FF_FFFF == 2 { "arm64e".to_string() } else { "arm64".to_string() }
    } else {
        format!("cpu-{cputype:#x}")
    };
    let (mut platform, mut minos) = (None, None);
    let mut signature = None;
    let mut at = 32usize;
    for _ in 0..ncmds {
        if at + 8 > data.len().min(32 + sizeofcmds) {
            return Err("load commands out of range".into());
        }
        let (cmd, size) = (le32(data, at)?, le32(data, at + 4)? as usize);
        if size < 8 {
            return Err("bad load command".into());
        }
        if cmd == LC_CODE_SIGNATURE {
            let (offset, length) = (le32(data, at + 8)? as usize, le32(data, at + 12)? as usize);
            if offset + length > data.len() {
                return Err("signature out of range".into());
            }
            signature = Some(&data[offset..offset + length]);
        } else if cmd == LC_BUILD_VERSION {
            let (p, m) = (le32(data, at + 8)?, le32(data, at + 12)?);
            platform = Some(platform_name(p));
            minos = Some(format!("{}.{}", m >> 16, (m >> 8) & 0xFF));
        }
        at += size;
    }
    let signature = signature.ok_or("not signed (no LC_CODE_SIGNATURE)")?;
    let mut best: Option<(u8, Slice)> = None;
    for directory in code_directories(signature)? {
        let flags = be32(directory, 12)?;
        let ident_offset = be32(directory, 20)? as usize;
        let (hash_type, page_shift) = (directory[37], directory[39]);
        // hashType -> (hashlib name, rank); the highest rank is the directory AMFI identifies the file by
        let (name, rank) = match hash_type {
            1 => ("sha1", 1),
            3 => ("sha256", 2),
            2 => ("sha256", 3),
            4 => ("sha384", 4),
            _ => continue,
        };
        if best.as_ref().is_none_or(|(r, _)| rank > *r) {
            let digest: Vec<u8> = match name {
                "sha1" => <sha1::Sha1 as sha1::Digest>::digest(directory).to_vec(),
                "sha256" => sha2::Sha256::digest(directory).to_vec(),
                _ => sha2::Sha384::digest(directory).to_vec(),
            };
            let cdhash: String = digest[..20].iter().map(|b| format!("{b:02x}")).collect();
            // directory.find(b'\0', ident_offset); an identifier only when the zero is past the start
            let end = directory.get(ident_offset..).and_then(|rest| rest.iter().position(|b| *b == 0)).map(|p| p + ident_offset);
            let identifier = match end {
                Some(end) if end > 0 => String::from_utf8_lossy(slice(directory, ident_offset, end)).into_owned(),
                _ => String::new(),
            };
            best = Some((
                rank,
                Slice {
                    cdhash,
                    hash: name,
                    identifier,
                    arch: arch.clone(),
                    page_size: if page_shift != 0 { 1u64 << page_shift } else { 0 },
                    adhoc: flags & CS_ADHOC != 0,
                    platform: platform.clone(),
                    minos: minos.clone(),
                },
            ));
        }
    }
    best.map(|(_, s)| s).ok_or_else(|| "signature has no usable code directory".to_string())
}

/// One record per architecture of a signed Mach-O file.
pub fn describe(data: &[u8]) -> Result<Vec<Slice>, String> {
    slices(data)?.into_iter().map(|(offset, size)| describe_slice(slice(data, offset, offset.saturating_add(size)))).collect()
}

pub fn is_macho(data: &[u8]) -> bool {
    if data.len() < 4 {
        return false;
    }
    let le = u32::from_le_bytes(data[..4].try_into().unwrap());
    let be = u32::from_be_bytes(data[..4].try_into().unwrap());
    le == MH_MAGIC_64 || be == FAT_MAGIC || be == FAT_MAGIC_64
}
