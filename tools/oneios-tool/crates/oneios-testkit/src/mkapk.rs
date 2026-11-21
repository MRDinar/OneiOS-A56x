//! `oneios-tool debug mkapk OUT.apk CERT.der [TEXT]` (was tests/app/mkapk.py): a stand-in APK for
//! tests/app/run.sh.
//!
//! A zip with one stored file, `stand-in.txt` holding TEXT (default `stand-in`), and an APK Signing Block
//! (scheme v2) whose one signer carries CERT.der. The signature and the digests are filler: oneios-tool reads the
//! certificate, it does not verify the APK (Android does, at install). The zip's records are the ones Python's
//! `zipfile.writestr` wrote, byte for byte, but for its time stamp: this writes 1980-01-01 00:00 (the script
//! wrote the time it ran), so two runs give the same file.

use anyhow::{Context, Result};

fn lp(b: &[u8]) -> Vec<u8> {
    let mut out = (b.len() as u32).to_le_bytes().to_vec();
    out.extend_from_slice(b);
    out
}

/// A zip of one stored file, as `zipfile.ZipFile(..., ZIP_STORED).writestr(name, text)` writes it: version 2.0,
/// made on Unix, mode 0600, no extra fields. Returns (zip, central directory's offset, end record's offset).
fn zip_one(name: &str, data: &[u8]) -> (Vec<u8>, usize, usize) {
    let crc = crc32fast::hash(data);
    let (time, date) = (0u16, (1 << 5) | 1u16); // 1980-01-01 00:00:00 in DOS form
    let mut z = Vec::new();
    z.extend_from_slice(b"PK\x03\x04");
    for v in [20u16, 0, 0, time, date] {
        z.extend_from_slice(&v.to_le_bytes());
    }
    for v in [crc, data.len() as u32, data.len() as u32] {
        z.extend_from_slice(&v.to_le_bytes());
    }
    z.extend_from_slice(&(name.len() as u16).to_le_bytes());
    z.extend_from_slice(&0u16.to_le_bytes());
    z.extend_from_slice(name.as_bytes());
    z.extend_from_slice(data);
    let cd = z.len();
    z.extend_from_slice(b"PK\x01\x02");
    z.extend_from_slice(&[20, 3, 20, 0]); // made by 2.0 on Unix, needs 2.0
    for v in [0u16, 0, time, date] {
        z.extend_from_slice(&v.to_le_bytes());
    }
    for v in [crc, data.len() as u32, data.len() as u32] {
        z.extend_from_slice(&v.to_le_bytes());
    }
    for v in [name.len() as u16, 0, 0, 0, 0] {
        z.extend_from_slice(&v.to_le_bytes());
    }
    z.extend_from_slice(&(0o600u32 << 16).to_le_bytes());
    z.extend_from_slice(&0u32.to_le_bytes());
    z.extend_from_slice(name.as_bytes());
    let eocd = z.len();
    z.extend_from_slice(b"PK\x05\x06");
    for v in [0u16, 0, 1, 1] {
        z.extend_from_slice(&v.to_le_bytes());
    }
    z.extend_from_slice(&((eocd - cd) as u32).to_le_bytes());
    z.extend_from_slice(&(cd as u32).to_le_bytes());
    z.extend_from_slice(&0u16.to_le_bytes());
    (z, cd, eocd)
}

/// The APK's bytes.
pub fn apk(cert: &[u8], text: &str) -> Vec<u8> {
    let (raw, cd_offset, eocd) = zip_one("stand-in.txt", text.as_bytes());
    let mut alg_digest = 0x0103u32.to_le_bytes().to_vec();
    alg_digest.extend(lp(&[0u8; 32]));
    let digests = lp(&lp(&alg_digest));
    let mut signed = lp(&digests);
    signed.extend(lp(&lp(cert)));
    signed.extend(lp(b""));
    let mut alg_sig = 0x0103u32.to_le_bytes().to_vec();
    alg_sig.extend(lp(&[0u8; 256]));
    let signatures = lp(&alg_sig);
    let mut signer = lp(&signed);
    signer.extend(lp(&signatures));
    signer.extend(lp(&[0u8; 32]));
    let value = lp(&lp(&signer));
    let mut pair = ((value.len() + 4) as u64).to_le_bytes().to_vec();
    pair.extend_from_slice(&0x7109_871Au32.to_le_bytes());
    pair.extend(value);
    let size = (pair.len() + 8 + 16) as u64;
    let mut block = size.to_le_bytes().to_vec();
    block.extend(pair);
    block.extend_from_slice(&size.to_le_bytes());
    block.extend_from_slice(b"APK Sig Block 42");

    let mut out = raw[..cd_offset].to_vec();
    out.extend_from_slice(&block);
    out.extend_from_slice(&raw[cd_offset..eocd]);
    out.extend_from_slice(&raw[eocd..eocd + 16]);
    out.extend_from_slice(&((cd_offset + block.len()) as u32).to_le_bytes());
    out.extend_from_slice(&raw[eocd + 20..]);
    out
}

pub fn run(out: &str, cert: &str, text: Option<&str>) -> Result<i32> {
    let cert = std::fs::read(cert).with_context(|| format!("mkapk: {cert}"))?;
    std::fs::write(out, apk(&cert, text.unwrap_or("stand-in"))).with_context(|| format!("mkapk: {out}"))?;
    Ok(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_block_sits_before_the_directory_and_the_end_record_points_past_it() {
        let a = apk(b"CERT", "apk x");
        let eocd = a.len() - 22;
        assert_eq!(&a[eocd..eocd + 4], b"PK\x05\x06");
        let cd = u32::from_le_bytes(a[eocd + 16..eocd + 20].try_into().unwrap()) as usize;
        assert_eq!(&a[cd..cd + 4], b"PK\x01\x02");
        assert_eq!(&a[cd - 16..cd], b"APK Sig Block 42");
        let size = u64::from_le_bytes(a[cd - 24..cd - 16].try_into().unwrap()) as usize;
        assert_eq!(u64::from_le_bytes(a[cd - size - 8..cd - size].try_into().unwrap()) as usize, size);
        assert!(a.windows(4).any(|w| w == b"CERT"));
        assert_eq!(apk(b"CERT", "apk x"), a, "two runs, the same bytes");
    }
}
