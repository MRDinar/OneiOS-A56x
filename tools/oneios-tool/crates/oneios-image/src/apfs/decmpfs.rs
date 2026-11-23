//! Files kept compressed (decmpfs): the `com.apple.decmpfs` attribute holds a 16-byte header (`fpmc`, the kind,
//! the plain size) and, for the inline kinds, the data; the other kinds keep the data in the resource fork, in
//! chunks of 64 KiB of plain bytes. Kinds: 3 and 4 zlib, 7 and 8 LZVN, 9 and 10 stored, 11 and 12 LZFSE, 13 and 14
//! LZBITMAP (the odd one inline, the even one in the resource fork). iOS 27's OS volumes use LZBITMAP for most
//! files. The layouts are the ones apfs-fuse reads, and every compressed file of the 24A446 SystemOS and AppOS
//! images comes out as apfs-fuse gives it (tools/oneios-tool/README.md "The APFS reader").
//!
//! Decoders, all libraries: `flate2` (zlib), `lzfse_rust` (LZFSE), `zbitmap` (LZBITMAP). A decmpfs LZVN stream
//! is a bare LZVN payload; lzfse_rust decodes LZVN only as a `bvxn` block of an LZFSE stream, so the payload is
//! given that block's 12-byte header and the stream's end mark: no LZVN decoder of our own.

use std::io::Read;

const CHUNK: usize = 0x10000;

/// The resource map of a kind 4 fork, as Apple's writer makes it (`check_layout`).
const APPLE_MAP: [u8; 50] = [
    0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, // reserved
    0, 0, 0, 0, // the next map's handle
    0, 0, // the file's number
    0, 0, // attributes
    0x00, 0x1c, // the type list's place
    0x00, 0x32, // the name list's place (the map's end: no names)
    0x00, 0x00, // types - 1
    b'c', b'm', b'p', b'f', 0x00, 0x00, // cmpf, resources - 1
    0x00, 0x0a, // its reference list's place, from the type list
    0x00, 0x01, 0xff, 0xff, // resource 1, no name
    0x00, 0x00, 0x00, 0x00, // attributes, its data at 0
    0x00, 0x00, 0x00, 0x00, // handle
];

fn u32_le(d: &[u8], at: usize) -> Option<usize> {
    Some(u32::from_le_bytes(d.get(at..at + 4)?.try_into().ok()?) as usize)
}

/// The kind and the plain size of a decmpfs attribute.
pub fn header(attr: &[u8]) -> Option<(u32, u64)> {
    if attr.get(..4)? != b"fpmc" {
        return None;
    }
    Some((u32::from_le_bytes(attr.get(4..8)?.try_into().ok()?), u64::from_le_bytes(attr.get(8..16)?.try_into().ok()?)))
}

pub fn in_resource_fork(kind: u32) -> bool {
    matches!(kind, 4 | 8 | 10 | 12 | 14)
}

fn zlib(src: &[u8], want: usize) -> Result<Vec<u8>, String> {
    let mut out = Vec::with_capacity(want);
    flate2::read::ZlibDecoder::new(src).take(want as u64 + 1).read_to_end(&mut out).map_err(|e| format!("zlib: {e}"))?;
    Ok(out)
}

fn lzfse(src: &[u8], want: usize) -> Result<Vec<u8>, String> {
    let mut out = Vec::with_capacity(want);
    lzfse_rust::decode_bytes(src, &mut out).map_err(|e| format!("LZFSE: {e}"))?;
    Ok(out)
}

/// A bare LZVN payload, as one `bvxn` block of an LZFSE stream.
fn lzvn(src: &[u8], want: usize) -> Result<Vec<u8>, String> {
    let mut stream = Vec::with_capacity(src.len() + 16);
    stream.extend_from_slice(b"bvxn");
    stream.extend_from_slice(&(want as u32).to_le_bytes());
    stream.extend_from_slice(&(src.len() as u32).to_le_bytes());
    stream.extend_from_slice(src);
    stream.extend_from_slice(b"bvx$");
    let mut out = Vec::with_capacity(want);
    lzfse_rust::decode_bytes(&stream, &mut out).map_err(|e| format!("LZVN: {e}"))?;
    Ok(out)
}

/// One piece (the inline data, or a chunk of the resource fork) of `want` plain bytes.
fn piece(kind: u32, src: &[u8], want: usize) -> Result<Vec<u8>, String> {
    let first = *src.first().ok_or("an empty piece")?;
    let stored = || src[1..].to_vec();
    let out = match kind {
        3 | 4 if first == 0x78 => zlib(src, want)?,
        3 if first == 0xff => stored(),
        4 if first & 0x0f == 0x0f => stored(),
        3 | 4 => return Err(format!("zlib data starts with {first:#04x}")),
        7 | 8 if first == 0x06 => stored(),
        7 | 8 => lzvn(src, want)?,
        9 | 10 => stored(),
        11 | 12 => lzfse(src, want)?,
        13 | 14 if first == 0xff => stored(),
        13 | 14 => {
            let mut out = Vec::with_capacity(want);
            zbitmap::decompress(src, &mut out, want).map_err(|e| format!("LZBITMAP: {e}"))?;
            out
        }
        _ => return Err(format!("decmpfs kind {kind} is not known")),
    };
    if out.len() != want {
        return Err(format!("a piece decoded to {} bytes, {want} were expected", out.len()));
    }
    Ok(out)
}

/// The plain contents of a compressed file: `attr` is its `com.apple.decmpfs` attribute, `fork` reads its
/// resource fork when the kind keeps the data there.
pub fn decode(attr: &[u8], fork: impl FnOnce() -> Result<Vec<u8>, String>) -> Result<Vec<u8>, String> {
    let (kind, size) = header(attr).ok_or("the decmpfs attribute has no fpmc header")?;
    let size = usize::try_from(size).map_err(|_| "the plain size does not fit")?;
    if !in_resource_fork(kind) {
        return if size == 0 { Ok(Vec::new()) } else { piece(kind, &attr[16..], size) };
    }
    let fork = fork()?;
    let chunks = size.div_ceil(CHUNK);
    // where chunk k is in the fork: (start, length)
    let place = |k: usize| -> Option<(usize, usize)> {
        if kind == 4 {
            // a resource fork: big-endian header, the data's place first; there a length, then the table
            let base = u32::from_be_bytes(fork.get(..4)?.try_into().ok()?) as usize + 4;
            if k >= u32_le(&fork, base)? {
                return None;
            }
            Some((base + u32_le(&fork, base + 4 + 8 * k)?, u32_le(&fork, base + 8 + 8 * k)?))
        } else {
            // a list of chunks + 1 offsets from the fork's start
            let start = u32_le(&fork, 4 * k)?;
            Some((start, u32_le(&fork, 4 * k + 4)?.checked_sub(start)?))
        }
    };
    let mut out = Vec::with_capacity(size);
    for k in 0..chunks {
        let want = (size - k * CHUNK).min(CHUNK);
        let (start, len) = place(k).ok_or_else(|| format!("the resource fork has no chunk {k} of {chunks}"))?;
        let src = fork.get(start..start + len).ok_or_else(|| format!("chunk {k} is outside the resource fork"))?;
        out.extend_from_slice(&piece(kind, src, want).map_err(|e| format!("chunk {k}: {e}"))?);
    }
    Ok(out)
}

fn u32_be(d: &[u8], at: usize) -> Option<usize> {
    Some(u32::from_be_bytes(d.get(at..at + 4)?.try_into().ok()?) as usize)
}

/// What iOS's own reader (AppleFSCompression) needs of a compressed file's layout, beyond what `decode` reads: the
/// resource fork of kind 4 is a whole Mac resource fork (its header, its resource map with the `cmpf` resource the
/// header's data names), and the other resource-fork kinds have a chunk table of rising offsets inside the fork.
/// `decode` reads only the data's place, so a fork with a broken map decodes here and not on iOS (OneiOS issue 73:
/// `ChunkCompression.cpp:415: /usr/lib/dyld: invalid resource map` on another fork's hand-made os.img). Layout: the
/// Resource Manager's (Inside Macintosh: More Macintosh Toolbox, 1-121), all big-endian; the `cmpf` data is
/// little-endian (a count, then an offset and a length per chunk).
pub fn check_layout(attr: &[u8], fork: Option<&[u8]>) -> Result<(), String> {
    let (kind, size) = header(attr).ok_or("the decmpfs attribute has no fpmc header")?;
    if attr.len() < 16 {
        return Err("the decmpfs attribute is shorter than its 16-byte header".into());
    }
    if !in_resource_fork(kind) {
        return Ok(());
    }
    let fork = fork.ok_or_else(|| format!("kind {kind} keeps its data in the resource fork, and the file has none"))?;
    let size = usize::try_from(size).map_err(|_| "the plain size does not fit")?;
    let chunks = size.div_ceil(CHUNK);
    let n = fork.len();
    if kind == 4 {
        let bad = |what: String| Err(format!("invalid resource map: {what} (fork of {n} bytes)"));
        let (Some(data_off), Some(map_off), Some(data_len), Some(map_len)) = (u32_be(fork, 0), u32_be(fork, 4), u32_be(fork, 8), u32_be(fork, 12)) else {
            return bad("no 16-byte header".into());
        };
        if data_off < 16 || data_off.checked_add(data_len).is_none_or(|end| end > n) {
            return bad(format!("data at {data_off:#x} of {data_len:#x} bytes is outside the fork"));
        }
        if map_len < 30 || map_off.checked_add(map_len).is_none_or(|end| end > n) {
            return bad(format!("map at {map_off:#x} of {map_len:#x} bytes is outside the fork"));
        }
        if map_off < data_off + data_len && data_off < map_off + map_len {
            return bad(format!("map at {map_off:#x} overlaps the data at {data_off:#x}"));
        }
        // The map: the one Apple's writer puts in every kind 4 fork, byte for byte (16 reserved bytes, the next
        // map's handle, the file's number and the attributes all zero; the type list at 0x1c, the name list at 0x32;
        // one type, cmpf, with one resource, number 1, no name, its data at 0). Every kind 4 file of 24A446's OS,
        // SystemOS and AppOS images has it, and 20F66's. The other fork's dyld differed only in the 16 reserved bytes
        // (a copy of the fork's header, as the classic Resource Manager writes it) and iOS refused it with "invalid
        // resource map" (OneiOS issue 73); that iOS compares the whole map is inferred from that, not read in its code.
        let map = &fork[map_off..map_off + map_len];
        if map != APPLE_MAP {
            let differ: Vec<usize> = (0..map.len().max(APPLE_MAP.len())).filter(|&i| map.get(i) != APPLE_MAP.get(i)).collect();
            let shown: String = map.iter().take(16).map(|b| format!("{b:02x}")).collect();
            return bad(format!(
                "the map ({map_len} bytes) is not Apple's: {} bytes differ, from byte {} (its first 16 bytes {shown}; Apple's are zeros)",
                differ.len(),
                differ.first().copied().unwrap_or(0)
            ));
        }
        let res_off = 0;
        let Some(res_len) = u32_be(&fork[data_off..data_off + data_len], res_off) else {
            return bad(format!("cmpf's data at {res_off:#x} is outside the data of {data_len:#x} bytes"));
        };
        if res_off + 4 + res_len > data_len {
            return bad(format!("cmpf's {res_len} bytes at {res_off:#x} run past the data's {data_len:#x}"));
        }
        let res = &fork[data_off + res_off + 4..data_off + res_off + 4 + res_len];
        let listed = u32_le(res, 0).ok_or_else(|| "cmpf's data has no chunk count".to_string())?;
        if listed != chunks {
            return Err(format!("cmpf lists {listed} chunks, the plain size {size} needs {chunks}"));
        }
        for k in 0..chunks {
            let (Some(off), Some(len)) = (u32_le(res, 4 + 8 * k), u32_le(res, 8 + 8 * k)) else {
                return Err(format!("cmpf's chunk table ends before chunk {k} of {chunks}"));
            };
            if off.checked_add(len).is_none_or(|end| end > res_len) {
                return Err(format!("chunk {k} ({len} bytes at {off:#x}) runs past cmpf's {res_len} bytes"));
            }
        }
        return Ok(());
    }
    // the other kinds: chunks + 1 offsets from the fork's start, the first just after the table, rising, the last
    // not past the fork's end
    let mut last = 4 * (chunks + 1);
    if u32_le(fork, 0) != Some(last) {
        return Err(format!("the chunk table's first offset is {:?}, {last} for {chunks} chunks", u32_le(fork, 0)));
    }
    for k in 1..=chunks {
        let off = u32_le(fork, 4 * k).ok_or_else(|| format!("the chunk table ends before entry {k} of {}", chunks + 1))?;
        if off < last || off > n {
            return Err(format!("chunk {} ends at {off}: before its start {last} or past the fork's {n} bytes", k - 1));
        }
        last = off;
    }
    Ok(())
}


/// A compressed file's layout in one line, for `image apfs-check --verbose`: the kind, the plain size, the
/// attribute's and the fork's sizes; for kind 4 the fork's header and the map's type list; for the other fork
/// kinds the chunk table's first and last offsets; the first bytes of the first chunk.
pub fn describe(attr: &[u8], fork: Option<&[u8]>) -> String {
    let Some((kind, size)) = header(attr) else { return format!("no fpmc header ({} bytes)", attr.len()) };
    let mut text = format!("kind {kind}, plain {size} bytes, attribute {} bytes", attr.len());
    let hex = |d: &[u8]| d.iter().take(16).map(|b| format!("{b:02x}")).collect::<String>();
    let Some(fork) = fork.filter(|_| in_resource_fork(kind)) else {
        if attr.len() > 16 {
            text.push_str(&format!(", inline data {}", hex(&attr[16..])));
        }
        return text;
    };
    text.push_str(&format!(", fork {} bytes", fork.len()));
    if kind == 4 {
        let f = |at| u32_be(fork, at).map_or("-".to_string(), |v| format!("{v:#x}"));
        text.push_str(&format!(", header data {} map {} data-len {} map-len {}", f(0), f(4), f(8), f(12)));
        if let (Some(map_off), Some(map_len)) = (u32_be(fork, 4), u32_be(fork, 12)) {
            if let Some(map) = fork.get(map_off..map_off.saturating_add(map_len)) {
                text.push_str(&format!(", map {}", map.iter().map(|b| format!("{b:02x}")).collect::<String>()));
            }
            if let Some(data_off) = u32_be(fork, 0) {
                if let Some(res) = fork.get(data_off..) {
                    text.push_str(&format!(", data {}", hex(res)));
                }
            }
        }
    } else {
        let chunks = usize::try_from(size).unwrap_or(0).div_ceil(CHUNK);
        text.push_str(&format!(", chunk table {:?} .. {:?} ({chunks} chunks)", u32_le(fork, 0), u32_le(fork, 4 * chunks)));
        if let Some(start) = u32_le(fork, 0) {
            if let Some(first) = fork.get(start..) {
                text.push_str(&format!(", first chunk {}", hex(first)));
            }
        }
    }
    text
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    fn attr(kind: u32, size: usize, data: &[u8]) -> Vec<u8> {
        let mut a = b"fpmc".to_vec();
        a.extend_from_slice(&kind.to_le_bytes());
        a.extend_from_slice(&(size as u64).to_le_bytes());
        a.extend_from_slice(data);
        a
    }

    fn deflate(data: &[u8]) -> Vec<u8> {
        let mut e = flate2::write::ZlibEncoder::new(Vec::new(), flate2::Compression::default());
        e.write_all(data).unwrap();
        e.finish().unwrap()
    }

    fn none() -> Result<Vec<u8>, String> {
        Err("no fork".into())
    }

    #[test]
    fn inline_kinds() {
        let plain = b"hello hello hello hello hello hello".to_vec();
        assert_eq!(decode(&attr(3, plain.len(), &deflate(&plain)), none).unwrap(), plain);
        let mut stored = vec![0xff];
        stored.extend_from_slice(&plain);
        assert_eq!(decode(&attr(3, plain.len(), &stored), none).unwrap(), plain);
        stored[0] = 0x06;
        assert_eq!(decode(&attr(7, plain.len(), &stored), none).unwrap(), plain);
        stored[0] = 0xcc;
        assert_eq!(decode(&attr(9, plain.len(), &stored), none).unwrap(), plain);
        let mut packed = Vec::new();
        lzfse_rust::encode_bytes(&plain, &mut packed).unwrap();
        assert_eq!(decode(&attr(11, plain.len(), &packed), none).unwrap(), plain);
        assert_eq!(decode(&attr(3, 0, &[]), none).unwrap(), b"");
        // a bare LZVN payload: a literal of 5 bytes (opcode 0xe5), then the end of the stream
        let mut vn = vec![0xe5];
        vn.extend_from_slice(b"abcde");
        vn.extend_from_slice(&[0x06, 0, 0, 0, 0, 0, 0, 0]);
        assert_eq!(decode(&attr(7, 5, &vn), none).unwrap(), b"abcde");
        // LZBITMAP: "hello" as one stored chunk and the end chunk (the zbitmap crate's own example), and stored whole
        assert_eq!(decode(&attr(13, 5, b"ZBM\x09\x0b\x00\x00\x05\x00\x00hello\x06\x00\x00\x00\x00\x00"), none).unwrap(), b"hello");
        assert_eq!(decode(&attr(13, 5, b"\xffhello"), none).unwrap(), b"hello");
        assert!(decode(&attr(13, 5, &[1, 2]), none).unwrap_err().contains("LZBITMAP"));
        assert!(decode(&attr(99, 5, &[1, 2]), none).unwrap_err().contains("not known"));
        assert!(decode(&attr(3, plain.len() + 1, &deflate(&plain)), none).unwrap_err().contains("were expected"));
        assert!(decode(b"nope", none).is_err());
    }

    #[test]
    fn resource_fork_kinds() {
        // 64 KiB + 100 bytes: two chunks
        let plain: Vec<u8> = (0..CHUNK + 100).map(|i| (i % 251) as u8).collect();
        let (a, b) = plain.split_at(CHUNK);
        // kind 4: a resource fork header, then a length, the count, (offset, length) pairs from the count's place
        let (za, zb) = (deflate(a), { let mut s = vec![0x0f]; s.extend_from_slice(b); s });
        let mut fork = Vec::new();
        fork.extend_from_slice(&0x100u32.to_be_bytes());
        fork.resize(0x100, 0);
        fork.extend_from_slice(&[0; 4]);
        fork.extend_from_slice(&2u32.to_le_bytes());
        let first = 4 + 16;
        for (off, len) in [(first, za.len()), (first + za.len(), zb.len())] {
            fork.extend_from_slice(&(off as u32).to_le_bytes());
            fork.extend_from_slice(&(len as u32).to_le_bytes());
        }
        fork.extend_from_slice(&za);
        fork.extend_from_slice(&zb);
        assert!(decode(&attr(4, plain.len(), &[]), || Ok(fork.clone())).unwrap() == plain);
        // kind 12: offsets from the fork's start
        let (mut la, mut lb) = (Vec::new(), Vec::new());
        lzfse_rust::encode_bytes(a, &mut la).unwrap();
        lzfse_rust::encode_bytes(b, &mut lb).unwrap();
        let mut fork = Vec::new();
        for off in [12, 12 + la.len(), 12 + la.len() + lb.len()] {
            fork.extend_from_slice(&(off as u32).to_le_bytes());
        }
        fork.extend_from_slice(&la);
        fork.extend_from_slice(&lb);
        assert!(decode(&attr(12, plain.len(), &[]), || Ok(fork.clone())).unwrap() == plain);
        // kind 8 with stored chunks
        let mut fork = Vec::new();
        for off in [12, 13 + CHUNK, 14 + CHUNK + 100] {
            fork.extend_from_slice(&(off as u32).to_le_bytes());
        }
        fork.push(0x06);
        fork.extend_from_slice(a);
        fork.push(0x06);
        fork.extend_from_slice(b);
        assert!(decode(&attr(8, plain.len(), &[]), || Ok(fork.clone())).unwrap() == plain);
        fork.truncate(40);
        assert!(decode(&attr(8, plain.len(), &[]), || Ok(fork.clone())).unwrap_err().contains("outside the resource fork"));
    }

    /// A kind 4 fork as Apple's writer lays it out: the header, the data (its length, then cmpf: the count and an
    /// (offset, length) per chunk from the count, then the chunks), then Apple's map.
    fn apple_fork(chunks: &[Vec<u8>]) -> Vec<u8> {
        let mut cmpf = (chunks.len() as u32).to_le_bytes().to_vec();
        let mut at = 4 + 8 * chunks.len();
        for c in chunks {
            cmpf.extend_from_slice(&(at as u32).to_le_bytes());
            cmpf.extend_from_slice(&(c.len() as u32).to_le_bytes());
            at += c.len();
        }
        for c in chunks {
            cmpf.extend_from_slice(c);
        }
        let data_len = 4 + cmpf.len();
        let mut fork = Vec::new();
        for v in [0x100, 0x100 + data_len, data_len, APPLE_MAP.len()] {
            fork.extend_from_slice(&(v as u32).to_be_bytes());
        }
        fork.resize(0x100, 0);
        fork.extend_from_slice(&(cmpf.len() as u32).to_be_bytes());
        fork.extend_from_slice(&cmpf);
        fork.extend_from_slice(&APPLE_MAP);
        fork
    }

    #[test]
    fn layout_as_ios_reads_it() {
        let plain: Vec<u8> = (0..CHUNK + 100).map(|i| (i % 251) as u8).collect();
        let (a, b) = plain.split_at(CHUNK);
        let fork = apple_fork(&[deflate(a), deflate(b)]);
        let attr4 = attr(4, plain.len(), &[]);
        check_layout(&attr4, Some(&fork)).unwrap();
        assert!(decode(&attr4, || Ok(fork.clone())).unwrap() == plain);
        // OneiOS issue 73: the map's 16 reserved bytes hold a copy of the header (the other A56 fork's os.img, whose
        // dyld iOS refused): `decode` reads it, iOS does not
        let mut copy = fork.clone();
        let map = copy.len() - APPLE_MAP.len();
        let header: Vec<u8> = copy[..16].to_vec();
        copy[map..map + 16].copy_from_slice(&header);
        assert!(decode(&attr4, || Ok(copy.clone())).unwrap() == plain);
        let e = check_layout(&attr4, Some(&copy)).unwrap_err();
        assert!(e.starts_with("invalid resource map: the map (50 bytes) is not Apple's: 6 bytes differ, from byte 2 (its first 16 bytes 00000100"), "{e}");
        // a map outside the fork, no fork, a chunk count that does not fit the plain size, a chunk past cmpf's end
        let mut cut = fork.clone();
        cut.truncate(cut.len() - 1);
        assert!(check_layout(&attr4, Some(&cut)).unwrap_err().contains("map at 0x"));
        assert!(check_layout(&attr4, None).unwrap_err().contains("has none"));
        assert!(check_layout(&attr(4, CHUNK, &[]), Some(&fork)).unwrap_err().contains("cmpf lists 2 chunks, the plain size 65536 needs 1"));
        let mut long = fork.clone();
        long[0x100 + 4 + 4 + 4..0x100 + 4 + 4 + 8].copy_from_slice(&0x7fff_0000u32.to_le_bytes());
        assert!(check_layout(&attr4, Some(&long)).unwrap_err().contains("chunk 0"));
        // the other fork kinds: the table's first offset just after it, rising, not past the fork
        let mut fork12 = Vec::new();
        for off in [12u32, 20, 30] {
            fork12.extend_from_slice(&off.to_le_bytes());
        }
        fork12.resize(30, 0);
        let attr12 = attr(12, CHUNK + 1, &[]);
        check_layout(&attr12, Some(&fork12)).unwrap();
        fork12[4..8].copy_from_slice(&40u32.to_le_bytes());
        assert!(check_layout(&attr12, Some(&fork12)).unwrap_err().contains("chunk 0 ends at 40"));
        fork12[0..4].copy_from_slice(&8u32.to_le_bytes());
        assert!(check_layout(&attr12, Some(&fork12)).unwrap_err().contains("first offset"));
        // the inline kinds have nothing more to check
        check_layout(&attr(3, 5, b"\xffhello"), None).unwrap();
        // describe: the kind, sizes, the header and the map
        let text = describe(&attr4, Some(&copy));
        assert!(text.starts_with("kind 4, plain 65636 bytes, attribute 16 bytes, fork "), "{text}");
        assert!(text.contains(", header data 0x100 map 0x"), "{text}");
    }
}
