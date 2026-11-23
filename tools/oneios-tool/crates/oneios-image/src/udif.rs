//! `image udif-raw IN OUT`: the APFS container of a UDIF disk image, as a raw image. iOS 16's IPSWs ship the OS
//! image as UDIF (20F66's `096-04088-077.dmg`: a `koly` trailer, LZFSE chunks, a GPT disk with one `Apple_APFS`
//! partition), where 24A446's images are raw APFS already; the image set holds raw APFS only (docs/image-format.md
//! section 1), so a UDIF member is converted once, like an AEA member is decrypted.
//!
//! Layout (Apple's, as `hdiutil` writes it; all big-endian): the 512-byte trailer at the file's end (`koly`, the
//! data fork's place and length at 0x18 and 0x20, the XML property list's at 0xd8 and 0xe0); the property list's
//! `resource-fork` `blkx` array, one entry per partition, each a `mish` block table: the partition's first sector
//! (8) and sector count (16), its data's place in the data fork (24), its checksum (type at 64, bits at 68, value at
//! 72: type 2 is the CRC-32 of the partition's plain bytes, its chunks of type 2 left out: 20F66's OS image, `3abcd8a4`
//! over its stored and LZFSE chunks, `60176318` with its 609,648 ignored sectors as zeros), the count of chunks (200) and the chunks from 204, 40
//! bytes each (type, comment, first sector and sector count from the partition's first, compressed place and length
//! from the partition's data). Sectors are 512 bytes. Chunk types: 0 and 2 zeros, 1 stored, 0x80000005 zlib,
//! 0x80000007 LZFSE, 0x7ffffffe a comment, 0xffffffff the end; ADC, bzip2 and LZMA (0x80000004, 6, 8) are refused.

use crate::{crash, err, Fail};
use oneios_common::job;
use rayon::prelude::*;
use sha2::Digest;
use std::fs::File;
use std::io::Read;
use std::os::unix::fs::FileExt;

const SECTOR: u64 = 512;

fn be32(d: &[u8], at: usize) -> Option<u32> {
    Some(u32::from_be_bytes(d.get(at..at + 4)?.try_into().ok()?))
}

fn be64(d: &[u8], at: usize) -> Option<u64> {
    Some(u64::from_be_bytes(d.get(at..at + 8)?.try_into().ok()?))
}

/// One chunk of a partition's block table.
#[derive(Clone, Debug, PartialEq)]
pub struct Chunk {
    pub kind: u32,
    /// first sector and sector count, from the partition's first sector
    pub sector: u64,
    pub sectors: u64,
    /// place and length in the file (the data fork's place and the partition's data place added)
    pub at: u64,
    pub len: u64,
}

/// One partition (`blkx` entry) of the image.
#[derive(Clone, Debug, PartialEq)]
pub struct Partition {
    pub name: String,
    pub first_sector: u64,
    pub sectors: u64,
    /// the CRC-32 of the partition's plain bytes, when the table carries one (checksum type 2)
    pub crc32: Option<u32>,
    pub chunks: Vec<Chunk>,
}

/// Whether `data` (a whole file) ends in a UDIF trailer.
pub fn is_udif(data: &[u8]) -> bool {
    data.len() >= 512 && &data[data.len() - 512..data.len() - 508] == b"koly"
}

/// The partitions of a UDIF image held in `data`.
pub fn partitions(data: &[u8], path: &str) -> Result<Vec<Partition>, Fail> {
    if !is_udif(data) {
        return Err(err!("{path}: not a UDIF disk image (no koly trailer)"));
    }
    let koly = &data[data.len() - 512..];
    let bad = |what: &str| err!("{path}: the UDIF trailer's {what} is outside the file");
    let fork_at = be64(koly, 0x18).ok_or_else(|| bad("data fork"))?;
    let (xml_at, xml_len) = (be64(koly, 0xd8).ok_or_else(|| bad("plist"))?, be64(koly, 0xe0).ok_or_else(|| bad("plist"))?);
    let end = data.len() as u64 - 512;
    if xml_at.checked_add(xml_len).is_none_or(|e| e > end) || xml_len == 0 {
        return Err(bad("property list"));
    }
    let xml = &data[xml_at as usize..(xml_at + xml_len) as usize];
    let plist = plist::Value::from_reader_xml(xml).map_err(|e| err!("{path}: the UDIF property list: {e}"))?;
    let blkx = plist
        .as_dictionary()
        .and_then(|d| d.get("resource-fork"))
        .and_then(|r| r.as_dictionary())
        .and_then(|r| r.get("blkx"))
        .and_then(|b| b.as_array())
        .ok_or_else(|| err!("{path}: the UDIF property list has no resource-fork blkx"))?;
    let mut out = Vec::new();
    for entry in blkx {
        let d = entry.as_dictionary().ok_or_else(|| err!("{path}: a blkx entry is not a dictionary"))?;
        let name = d.get("Name").or_else(|| d.get("CFName")).and_then(|n| n.as_string()).unwrap_or("").to_string();
        let mish = d.get("Data").and_then(|v| v.as_data()).ok_or_else(|| err!("{path}: blkx {name:?} has no Data"))?;
        let short = || err!("{path}: blkx {name:?}: its block table is cut short");
        if mish.get(..4) != Some(b"mish") {
            return Err(err!("{path}: blkx {name:?}: its block table does not start with mish"));
        }
        let first_sector = be64(mish, 8).ok_or_else(short)?;
        let sectors = be64(mish, 16).ok_or_else(short)?;
        let data_at = be64(mish, 24).ok_or_else(short)?;
        let crc32 = (be32(mish, 64).ok_or_else(short)? == 2 && be32(mish, 68).ok_or_else(short)? == 32).then(|| be32(mish, 72)).flatten();
        let count = be32(mish, 200).ok_or_else(short)? as usize;
        let mut chunks = Vec::with_capacity(count);
        for i in 0..count {
            let c = 204 + 40 * i;
            let kind = be32(mish, c).ok_or_else(short)?;
            let chunk = Chunk {
                kind,
                sector: be64(mish, c + 8).ok_or_else(short)?,
                sectors: be64(mish, c + 16).ok_or_else(short)?,
                at: fork_at.checked_add(data_at).and_then(|a| a.checked_add(be64(mish, c + 24)?)).ok_or_else(short)?,
                len: be64(mish, c + 32).ok_or_else(short)?,
            };
            match kind {
                0xffff_fffe | 0x7fff_fffe => continue, // a comment
                0xffff_ffff => break,                  // the end
                0 | 1 | 2 | 0x8000_0005 | 0x8000_0007 => {}
                other => return Err(err!("{path}: blkx {name:?}: chunk {i} has type {other:#x} (ADC, bzip2, LZMA or unknown), which this tool does not decode")),
            }
            if chunk.sector.checked_add(chunk.sectors).is_none_or(|e| e > sectors) {
                return Err(err!("{path}: blkx {name:?}: chunk {i} (sectors {} + {}) is past the partition's {sectors}", chunk.sector, chunk.sectors));
            }
            if chunk.kind != 0 && chunk.kind != 2 && chunk.at.checked_add(chunk.len).is_none_or(|e| e > end) {
                return Err(err!("{path}: blkx {name:?}: chunk {i}'s data is outside the file"));
            }
            chunks.push(chunk);
        }
        out.push(Partition { name, first_sector, sectors, crc32, chunks });
    }
    Ok(out)
}

/// One chunk's plain bytes (`None`: zeros, which a sparse file already holds).
fn chunk_bytes(data: &[u8], c: &Chunk) -> Result<Option<Vec<u8>>, String> {
    let want = (c.sectors * SECTOR) as usize;
    let src = || &data[c.at as usize..(c.at + c.len) as usize];
    let out = match c.kind {
        0 | 2 => return Ok(None),
        1 => src().to_vec(),
        0x8000_0005 => {
            let mut out = Vec::with_capacity(want);
            flate2::read::ZlibDecoder::new(src()).take(want as u64 + 1).read_to_end(&mut out).map_err(|e| format!("zlib: {e}"))?;
            out
        }
        _ => {
            let mut out = Vec::with_capacity(want);
            lzfse_rust::decode_bytes(src(), &mut out).map_err(|e| format!("LZFSE: {e}"))?;
            out
        }
    };
    if out.len() != want {
        return Err(format!("decoded to {} bytes, its {} sectors are {want}", out.len(), c.sectors));
    }
    Ok(Some(out))
}

/// What `to_raw` wrote.
pub struct Raw {
    pub bytes: u64,
    pub sha256: String,
    pub partition: String,
}

/// The `Apple_APFS` partition of UDIF image `input` into `out` (via `out.part`, renamed last): its chunks decoded in
/// every thread of the pool and written in place (zeros left as holes), then read back once for its CRC-32 (checked
/// against the table's, when it has one) and its SHA-256 (checked against `sha256`, when given). The result must be a
/// raw APFS container (`NXSB` at byte 32).
pub fn to_raw(input: &str, out: &str, sha256: Option<&str>) -> Result<Raw, Fail> {
    let file = File::open(input).map_err(|e| err!("{input}: {e}"))?;
    let data = unsafe { memmap2::Mmap::map(&file) }.map_err(|e| crash!("{input}: {e}"))?;
    let parts = partitions(&data, input)?;
    let apfs: Vec<&Partition> = parts.iter().filter(|p| p.name.contains("Apple_APFS")).collect();
    let [part] = apfs.as_slice() else {
        let names: Vec<&str> = parts.iter().map(|p| p.name.as_str()).collect();
        return Err(err!("{input}: {} Apple_APFS partitions, one was expected (partitions: {})", apfs.len(), names.join(", ")));
    };
    let bytes = part.sectors * SECTOR;
    let part_path = format!("{out}.part");
    let dst = File::create(&part_path).map_err(|e| err!("{part_path}: {e}"))?;
    dst.set_len(bytes).map_err(|e| err!("{part_path}: {e}"))?;
    let wrote = part.chunks.par_iter().enumerate().try_for_each(|(i, c)| -> Result<(), Fail> {
        job::check().map_err(|stop| crash!("{stop}"))?;
        if let Some(plain) = chunk_bytes(&data, c).map_err(|e| err!("{input}: {}: chunk {i}: {e}", part.name))? {
            dst.write_all_at(&plain, c.sector * SECTOR).map_err(|e| err!("{part_path}: {e}"))?;
        }
        Ok(())
    });
    if let Err(e) = wrote {
        let _ = std::fs::remove_file(&part_path);
        return Err(e);
    }
    drop(dst);
    // read back once: the CRC-32 of the table, the SHA-256 of the pin, the APFS magic
    let check = || -> Result<Raw, Fail> {
        let mut f = File::open(&part_path).map_err(|e| err!("{part_path}: {e}"))?;
        let (mut crc, mut sha) = (crc32fast::Hasher::new(), sha2::Sha256::new());
        let mut buf = vec![0u8; 8 << 20];
        let mut head = Vec::new();
        // the byte ranges the table's CRC-32 covers: every chunk but the ignored ones (type 2), in the partition's order
        let mut counted: Vec<(u64, u64)> = part.chunks.iter().filter(|c| c.kind != 2).map(|c| (c.sector * SECTOR, (c.sector + c.sectors) * SECTOR)).collect();
        counted.sort_unstable();
        let (mut pos, mut next) = (0u64, 0usize);
        loop {
            job::check().map_err(|stop| crash!("{stop}"))?;
            let n = f.read(&mut buf).map_err(|e| err!("{part_path}: {e}"))?;
            if n == 0 {
                break;
            }
            if head.len() < 64 {
                head.extend_from_slice(&buf[..n.min(64)]);
            }
            let end = pos + n as u64;
            while next < counted.len() {
                let (a, b) = counted[next];
                if a >= end {
                    break;
                }
                let (from, to) = (a.max(pos), b.min(end));
                if from < to {
                    crc.update(&buf[(from - pos) as usize..(to - pos) as usize]);
                }
                if b > end {
                    break;
                }
                next += 1;
            }
            pos = end;
            sha.update(&buf[..n]);
        }
        if head.get(32..36) != Some(b"NXSB") {
            return Err(err!("{input}: its {} partition is not an APFS container (no NXSB at byte 32)", part.name));
        }
        let crc = crc.finalize();
        if let Some(want) = part.crc32.filter(|w| *w != crc) {
            return Err(err!("{input}: {}: CRC-32 {crc:08x} of the plain bytes, the image's table says {want:08x}", part.name));
        }
        let digest = format!("{:x}", sha.finalize());
        if let Some(want) = sha256.filter(|w| !w.eq_ignore_ascii_case(&digest)) {
            return Err(err!("{input}: the raw APFS image has SHA-256 {digest}, the profile pins {want}"));
        }
        Ok(Raw { bytes, sha256: digest, partition: part.name.clone() })
    };
    match check() {
        Ok(raw) => {
            std::fs::rename(&part_path, out).map_err(|e| err!("{out}: {e}"))?;
            Ok(raw)
        }
        Err(e) => {
            let _ = std::fs::remove_file(&part_path);
            Err(e)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    /// A UDIF image of `plain` (whole sectors) as one Apple_APFS partition after a stand-in GPT partition: chunks
    /// of `per` sectors, stored, zlib, LZFSE or zeros in turn; the table's CRC-32 is `crc` (None: none).
    pub(crate) fn make(plain: &[u8], per: u64, crc: Option<u32>) -> Vec<u8> {
        assert_eq!(plain.len() as u64 % SECTOR, 0);
        let mut fork = b"GPT stand-in".to_vec();
        let gpt_len = fork.len() as u64;
        let mut chunks = Vec::new();
        let sectors = plain.len() as u64 / SECTOR;
        let mut s = 0;
        let mut i = 0;
        while s < sectors {
            let n = per.min(sectors - s);
            let bytes = &plain[(s * SECTOR) as usize..((s + n) * SECTOR) as usize];
            let (kind, packed) = match i % 4 {
                _ if bytes.iter().all(|b| *b == 0) => (2u32, Vec::new()),
                0 => (1, bytes.to_vec()),
                1 => {
                    let mut e = flate2::write::ZlibEncoder::new(Vec::new(), flate2::Compression::default());
                    e.write_all(bytes).unwrap();
                    (0x8000_0005, e.finish().unwrap())
                }
                _ => {
                    let mut out = Vec::new();
                    lzfse_rust::encode_bytes(bytes, &mut out).unwrap();
                    (0x8000_0007, out)
                }
            };
            chunks.push((kind, s, n, fork.len() as u64 - gpt_len, packed.len() as u64));
            fork.extend_from_slice(&packed);
            s += n;
            i += 1;
        }
        let mish = |first: u64, sectors: u64, data_at: u64, crc: Option<u32>, chunks: &[(u32, u64, u64, u64, u64)]| {
            let mut m = b"mish".to_vec();
            m.extend_from_slice(&1u32.to_be_bytes());
            m.extend_from_slice(&first.to_be_bytes());
            m.extend_from_slice(&sectors.to_be_bytes());
            m.extend_from_slice(&data_at.to_be_bytes());
            m.resize(64, 0);
            m.extend_from_slice(&(if crc.is_some() { 2u32 } else { 0 }).to_be_bytes());
            m.extend_from_slice(&32u32.to_be_bytes());
            m.extend_from_slice(&crc.unwrap_or(0).to_be_bytes());
            m.resize(200, 0);
            m.extend_from_slice(&(chunks.len() as u32 + 1).to_be_bytes());
            for (kind, s, n, at, len) in chunks.iter().chain([(0xffff_ffffu32, 0, 0, 0, 0)].iter()) {
                m.extend_from_slice(&kind.to_be_bytes());
                m.extend_from_slice(&0u32.to_be_bytes());
                m.extend_from_slice(&s.to_be_bytes());
                m.extend_from_slice(&n.to_be_bytes());
                m.extend_from_slice(&at.to_be_bytes());
                m.extend_from_slice(&len.to_be_bytes());
            }
            m
        };
        let gpt = mish(0, 1, 0, None, &[(1, 0, 1, 0, 0)]);
        let apfs = mish(1, sectors, gpt_len, crc, &chunks);
        let b64 = |d: &[u8]| base64::Engine::encode(&base64::engine::general_purpose::STANDARD, d);
        let xml = format!(
            "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<plist version=\"1.0\"><dict><key>resource-fork</key><dict><key>blkx</key><array>\
             <dict><key>Name</key><string>Primary GPT Header</string><key>Data</key><data>{}</data></dict>\
             <dict><key>Name</key><string>Apple_APFS : 4</string><key>Data</key><data>{}</data></dict>\
             </array></dict></dict></plist>\n",
            b64(&gpt),
            b64(&apfs)
        );
        let mut file = vec![0u8; 0];
        file.extend_from_slice(&fork);
        let xml_at = file.len() as u64;
        file.extend_from_slice(xml.as_bytes());
        let mut koly = b"koly".to_vec();
        koly.resize(0x18, 0);
        koly.extend_from_slice(&0u64.to_be_bytes()); // data fork at 0
        koly.extend_from_slice(&(fork.len() as u64).to_be_bytes());
        koly.resize(0xd8, 0);
        koly.extend_from_slice(&xml_at.to_be_bytes());
        koly.extend_from_slice(&(xml.len() as u64).to_be_bytes());
        koly.resize(512, 0);
        file.extend_from_slice(&koly);
        file
    }

    fn apfs_like(sectors: usize) -> Vec<u8> {
        let mut plain: Vec<u8> = (0..sectors * 512).map(|i| ((i * 7) % 253) as u8).collect();
        plain[32..36].copy_from_slice(b"NXSB");
        plain[8 * 512..12 * 512].fill(0); // a run of zeros (the third chunk of 4 sectors): a hole
        plain
    }

    #[test]
    fn converts_each_chunk_kind() {
        let tmp = crate::fixtures::Tmp::new("udif");
        let plain = apfs_like(40);
        // the table's CRC-32 leaves out the ignored chunk (the zeros of sectors 8 to 11)
        let crc = crc32fast::hash(&[&plain[..8 * 512], &plain[12 * 512..]].concat());
        let img = tmp.write("os.dmg", &make(&plain, 4, Some(crc)));
        let out = tmp.path("os.raw");
        let sha = oneios_common::sha256_hex(&plain);
        let raw = to_raw(&img, &out, Some(&sha)).unwrap();
        assert_eq!((raw.bytes, raw.sha256.as_str(), raw.partition.as_str()), (plain.len() as u64, sha.as_str(), "Apple_APFS : 4"));
        assert!(std::fs::read(&out).unwrap() == plain);
        assert!(!std::path::Path::new(&format!("{out}.part")).exists());
        let parts = partitions(&std::fs::read(&img).unwrap(), &img).unwrap();
        let kinds: Vec<u32> = parts[1].chunks.iter().map(|c| c.kind).collect();
        assert!(kinds.contains(&1) && kinds.contains(&2) && kinds.contains(&0x8000_0005) && kinds.contains(&0x8000_0007), "{kinds:x?}");
    }

    #[test]
    fn refuses_what_it_cannot_trust() {
        let tmp = crate::fixtures::Tmp::new("udif-refuse");
        let plain = apfs_like(16);
        let out = tmp.path("os.raw");
        // a wrong pin, a wrong CRC-32: nothing is left behind
        let good = crc32fast::hash(&[&plain[..8 * 512], &plain[12 * 512..]].concat());
        let img = tmp.write("a.dmg", &make(&plain, 4, Some(good)));
        let e = to_raw(&img, &out, Some(&"0".repeat(64))).err().map(|e| e.to_string()).unwrap();
        assert!(e.contains("the profile pins 0000"), "{e}");
        let img = tmp.write("b.dmg", &make(&plain, 4, Some(1)));
        let e = to_raw(&img, &out, None).err().map(|e| e.to_string()).unwrap();
        assert!(e.contains("the image's table says 00000001"), "{e}");
        assert!(!std::path::Path::new(&out).exists() && !std::path::Path::new(&format!("{out}.part")).exists());
        // not APFS inside, not UDIF at all
        let mut other = plain.clone();
        other[32..36].copy_from_slice(b"XXXX");
        let img = tmp.write("c.dmg", &make(&other, 4, None));
        assert!(to_raw(&img, &out, None).err().map(|e| e.to_string()).unwrap().contains("not an APFS container"));
        let img = tmp.write("d.dmg", &plain);
        assert!(to_raw(&img, &out, None).err().map(|e| e.to_string()).unwrap().contains("no koly trailer"));
    }
}
