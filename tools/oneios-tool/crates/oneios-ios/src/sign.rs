//! `ios sign`: the ad-hoc code signature of an iOS Mach-O, made on Linux or macOS, and its CDHash (the port of
//! scripts/ios/ios-sign.py, retired 2026-11-21; the same bytes: README "ios").
//!
//! What `codesign --force --sign - --pagesize 4096 [--entitlements PLIST] --identifier ID FILE` makes on a Mac: an
//! embedded signature with one SHA-256 code directory (version 0x20400, flags adhoc, 4 KiB pages: this guest's
//! kernel runs 4 KiB pages), an empty requirement set, the entitlements as XML and as DER when given, and an empty
//! CMS blob. The file is signed in place (cctools' codesign_allocate makes the room; a signature that is there is
//! replaced). Thin 64-bit files only.
//!
//! Not made: the bundle seal (_CodeSignature/CodeResources, the Info.plist slot). The kernel checks neither for a
//! library that dyld maps; only the CDHash has to be trusted.

use crate::macho::MachO;
use anyhow::{anyhow, bail, Context, Result};
use oneios_json::Value;
use sha2::{Digest, Sha256};
use std::path::{Path, PathBuf};

const PAGE_SHIFT: u8 = 12;
const PAGE: usize = 1 << PAGE_SHIFT;
const CS_ADHOC: u32 = 0x2;
const CS_EXECSEG_MAIN_BINARY: u64 = 0x1;
const SLOT_REQUIREMENTS: u32 = 2;
const SLOT_ENTITLEMENTS: u32 = 5;
const SLOT_DER: u32 = 7;

fn blob(magic: u32, body: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(8 + body.len());
    out.extend_from_slice(&magic.to_be_bytes());
    out.extend_from_slice(&((8 + body.len()) as u32).to_be_bytes());
    out.extend_from_slice(body);
    out
}

fn der_len(n: usize) -> Vec<u8> {
    if n < 0x80 {
        return vec![n as u8];
    }
    let raw: Vec<u8> = n.to_be_bytes().iter().copied().skip_while(|&b| b == 0).collect();
    let mut out = vec![0x80 | raw.len() as u8];
    out.extend(raw);
    out
}

fn der(tag: u8, body: &[u8]) -> Vec<u8> {
    let mut out = vec![tag];
    out.extend(der_len(body.len()));
    out.extend_from_slice(body);
    out
}

/// Python's `v.to_bytes(v.bit_length() // 8 + 1, 'big', signed=True)`.
fn int_bytes(v: i128) -> Vec<u8> {
    let bits = 128 - v.unsigned_abs().leading_zeros() as usize;
    let n = bits / 8 + 1;
    v.to_be_bytes()[16 - n..].to_vec()
}

/// The entitlements as CoreEntitlements' DER: a dictionary is [CONTEXT 16] of SEQUENCE { UTF8String key, value },
/// the keys sorted.
pub fn der_value(v: &plist::Value) -> Result<Vec<u8>> {
    Ok(match v {
        plist::Value::Boolean(b) => der(0x01, &[if *b { 0xFF } else { 0x00 }]),
        plist::Value::Integer(i) => {
            let n = i.as_signed().map(i128::from).or_else(|| i.as_unsigned().map(i128::from)).ok_or_else(|| anyhow!("an integer out of range"))?;
            der(0x02, &int_bytes(n))
        }
        plist::Value::String(s) => der(0x0C, s.as_bytes()),
        plist::Value::Array(a) => {
            let mut body = Vec::new();
            for x in a {
                body.extend(der_value(x)?);
            }
            der(0x30, &body)
        }
        plist::Value::Dictionary(d) => {
            let mut keys: Vec<&String> = d.keys().collect();
            keys.sort();
            let mut body = Vec::new();
            for k in keys {
                let mut pair = der(0x0C, k.as_bytes());
                pair.extend(der_value(&d[k])?);
                body.extend(der(0x30, &pair));
            }
            der(0xB0, &body)
        }
        other => bail!("entitlement value of type {}", kind(other)),
    })
}

fn kind(v: &plist::Value) -> &'static str {
    match v {
        plist::Value::Real(_) => "float",
        plist::Value::Data(_) => "bytes",
        plist::Value::Date(_) => "datetime",
        plist::Value::Uid(_) => "UID",
        _ => "unknown",
    }
}

/// The signature for `data` (whose LC_CODE_SIGNATURE says where it goes) and its CDHash.
pub fn signature(data: &[u8], identifier: &str, entitlements: Option<&[u8]>) -> Result<(Vec<u8>, String)> {
    let m = MachO::parse(data)?;
    let (limit, _) = m.signature().ok_or_else(|| anyhow!("no LC_CODE_SIGNATURE"))?;
    let limit = limit as usize;
    let mut ident = identifier.as_bytes().to_vec();
    ident.push(0);
    let mut special: Vec<(u32, Vec<u8>)> = vec![(SLOT_REQUIREMENTS, blob(0xFADE_0C01, &0u32.to_be_bytes()))];
    if let Some(ent) = entitlements {
        special.push((SLOT_ENTITLEMENTS, blob(0xFADE_7171, ent)));
        let value = plist::Value::from_reader(std::io::Cursor::new(ent)).context("the entitlements")?;
        let mut body = der(0x02, &[0x01]);
        body.extend(der_value(&value)?);
        special.push((SLOT_DER, blob(0xFADE_7172, &der(0x70, &body))));
    }
    special.sort_by_key(|(n, _)| *n);
    let n_special = special.iter().map(|(n, _)| *n).max().unwrap_or(0);
    let n_code = limit.div_ceil(PAGE);
    let header = 88usize;
    let hash_offset = header + ident.len() + 32 * n_special as usize;
    let mut slots = Vec::new();
    for n in (1..=n_special).rev() {
        match special.iter().find(|(k, _)| *k == n) {
            Some((_, b)) => slots.extend_from_slice(&Sha256::digest(b)),
            None => slots.extend_from_slice(&[0u8; 32]),
        }
    }
    let code = data.get(..limit).ok_or_else(|| anyhow!("the signature's offset is past the end"))?;
    let mut pages = Vec::with_capacity(32 * n_code);
    for chunk in code.chunks(PAGE) {
        pages.extend_from_slice(&Sha256::digest(chunk));
    }
    let (text_off, text_size) = m.text().ok_or_else(|| anyhow!("no __TEXT segment"))?;
    let length = hash_offset + 32 * n_code;
    let mut d = Vec::with_capacity(length);
    for w in [0xFADE_0C02u32, length as u32, 0x20400, CS_ADHOC, hash_offset as u32, header as u32, n_special, n_code as u32, limit as u32] {
        d.extend_from_slice(&w.to_be_bytes());
    }
    d.extend_from_slice(&[32, 2, 0, PAGE_SHIFT]);
    for w in [0u32; 4] {
        d.extend_from_slice(&w.to_be_bytes());
    }
    let flags = if m.filetype == crate::macho::MH_EXECUTE { CS_EXECSEG_MAIN_BINARY } else { 0 };
    for w in [0u64, text_off, text_size, flags] {
        d.extend_from_slice(&w.to_be_bytes());
    }
    d.extend_from_slice(&ident);
    d.extend_from_slice(&slots);
    d.extend_from_slice(&pages);
    assert_eq!(d.len(), length);
    let cdhash: String = Sha256::digest(&d)[..20].iter().map(|b| format!("{b:02x}")).collect();
    let mut parts: Vec<(u32, Vec<u8>)> = vec![(0, d)];
    parts.extend(special);
    parts.push((0x10000, blob(0xFADE_0B01, b"")));
    let at = 12 + 8 * parts.len();
    let (mut index, mut body) = (Vec::new(), Vec::new());
    for (kind, part) in &parts {
        index.extend_from_slice(&kind.to_be_bytes());
        index.extend_from_slice(&((at + body.len()) as u32).to_be_bytes());
        body.extend_from_slice(part);
    }
    let mut sig = Vec::with_capacity(at + body.len());
    sig.extend_from_slice(&0xFADE_0CC0u32.to_be_bytes());
    sig.extend_from_slice(&((at + body.len()) as u32).to_be_bytes());
    sig.extend_from_slice(&(parts.len() as u32).to_be_bytes());
    sig.extend(index);
    sig.extend(body);
    Ok((sig, cdhash))
}

/// What `sign` did to one file.
pub struct Signed {
    pub file: String,
    pub cdhash: String,
    pub arch: String,
    pub identifier: String,
    pub bytes: usize,
    pub entitlements: bool,
}

impl Signed {
    pub fn line(&self) -> String {
        format!("{} {} pages={PAGE} {} {}", self.cdhash, self.arch, self.identifier, self.file)
    }
    pub fn to_value(&self) -> Value {
        let mut m = indexmap_of();
        m.insert("file".into(), Value::String(self.file.clone()));
        m.insert("cdhash".into(), Value::String(self.cdhash.clone()));
        m.insert("arch".into(), Value::String(self.arch.clone()));
        m.insert("page_size".into(), Value::Number(PAGE.into()));
        m.insert("identifier".into(), Value::String(self.identifier.clone()));
        m.insert("hash".into(), Value::String("sha256".into()));
        m.insert("bytes".into(), Value::Number(self.bytes.into()));
        m.insert("entitlements".into(), Value::Bool(self.entitlements));
        Value::Object(m)
    }
}

fn indexmap_of() -> oneios_json::Map {
    oneios_json::Map::new()
}

/// cctools' codesign_allocate: IOS_CODESIGN_ALLOCATE, else IOS_CCTOOLS/codesign_allocate, else the one
/// scripts/setup/ios-toolchain.sh --ld builds (out/ios-toolchain/cctools/bin).
pub fn allocator(repo: &Path) -> Result<PathBuf> {
    let path = match std::env::var_os("IOS_CODESIGN_ALLOCATE").filter(|v| !v.is_empty()) {
        Some(p) => PathBuf::from(p),
        None => std::env::var_os("IOS_CCTOOLS").filter(|v| !v.is_empty()).map(PathBuf::from).unwrap_or_else(|| repo.join("out/ios-toolchain/cctools/bin")).join("codesign_allocate"),
    };
    if !crate::is_executable(&path) {
        bail!("no codesign_allocate at {} (scripts/setup/ios-toolchain.sh --ld builds cctools)", path.display());
    }
    Ok(path)
}

/// Sign `path` in place.
pub fn sign(path: &str, identifier: &str, entitlements: Option<&[u8]>, allocate: &Path) -> Result<Signed> {
    use std::os::unix::fs::PermissionsExt;
    let data = std::fs::read(path)?;
    let m = MachO::parse(&data)?;
    let arch = m.arch();
    if arch.starts_with("cpu-") {
        bail!("cpu type {:#x}", m.cputype);
    }
    let end = m.signature().map(|(o, _)| o as usize).unwrap_or(data.len());
    let ent_room = entitlements.map(|e| 2 * e.len() + 256).unwrap_or(0);
    let mut room = 12 + 8 * 5 + 88 + identifier.len() + 1 + 32 * 7 + 32 * ((end + 16).div_ceil(PAGE) + 1) + 12 + 8 + ent_room;
    room = (room + 15) & !15;
    let mode = std::fs::metadata(path)?.permissions().mode() & 0o777;
    let abs = std::fs::canonicalize(path)?;
    let dir = abs.parent().unwrap_or(Path::new("."));
    let tmp = dir.join(format!(".{}.sign-{}", abs.file_name().and_then(|n| n.to_str()).unwrap_or("file"), std::process::id()));
    struct Gone<'a>(&'a Path);
    impl Drop for Gone<'_> {
        fn drop(&mut self) {
            let _ = std::fs::remove_file(self.0);
        }
    }
    let _gone = Gone(&tmp);
    // Twice: first with room to spare, to learn the signature's length (where it starts does not depend on the
    // room), then with the room Apple's codesign leaves: the length plus 8, rounded up to 16. The file is then byte
    // for byte what `codesign --force --sign - --pagesize 4096 [--entitlements]` writes (compared with Xcode 27.2's
    // codesign on 2026-11-12: 16 identifier lengths, 23 entitlement sets).
    let mut out;
    let mut cdhash;
    let mut tries = 0;
    loop {
        tries += 1;
        let status = std::process::Command::new(allocate)
            .args(["-i", path, "-a", &arch, &room.to_string(), "-o"])
            .arg(&tmp)
            .status()
            .with_context(|| allocate.display().to_string())?;
        if !status.success() {
            bail!("{} exited with {status}", allocate.display());
        }
        out = std::fs::read(&tmp)?;
        let m = MachO::parse(&out)?;
        let (offset, size) = m.signature().ok_or_else(|| anyhow!("codesign_allocate made no LC_CODE_SIGNATURE"))?;
        let (sig, cd) = signature(&out, identifier, entitlements)?;
        cdhash = cd;
        let (offset, size) = (offset as usize, size as usize);
        if sig.len() > size || offset + size != out.len() {
            bail!("signature of {} bytes for {size} bytes of room", sig.len());
        }
        room = (sig.len() + 8 + 15) & !15;
        if size == room || tries == 2 {
            out[offset..offset + sig.len()].copy_from_slice(&sig);
            out[offset + sig.len()..offset + size].fill(0);
            break;
        }
    }
    std::fs::write(&tmp, &out)?;
    std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(mode))?;
    std::fs::rename(&tmp, path)?;
    Ok(Signed { file: path.to_string(), cdhash, arch, identifier: identifier.to_string(), bytes: out.len(), entitlements: entitlements.is_some() })
}

/// `ios sign [--identifier ID] [--entitlements PLIST] [--json] FILE...`
pub fn run(identifier: Option<&str>, entitlements: Option<&str>, json: bool, files: &[String]) -> Result<()> {
    let allocate = allocator(&crate::repo())?;
    let ent = match entitlements {
        Some(p) => {
            let raw = std::fs::read(p).with_context(|| p.to_string())?;
            plist::Value::from_reader(std::io::Cursor::new(&raw)).with_context(|| format!("{p}: not a property list"))?;
            Some(raw)
        }
        None => None,
    };
    let mut records = Vec::new();
    for path in files {
        let id = identifier.map(str::to_string).unwrap_or_else(|| Path::new(path).file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default());
        records.push(sign(path, &id, ent.as_deref(), &allocate).map_err(|e| anyhow!("{path}: {e:#}"))?);
    }
    if json {
        println!("{}", oneios_json::to_pretty(&Value::Array(records.iter().map(Signed::to_value).collect())));
    } else {
        for r in &records {
            println!("{}", r.line());
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn integers_as_python_writes_them() {
        assert_eq!(int_bytes(0), vec![0]);
        assert_eq!(int_bytes(1), vec![1]);
        assert_eq!(int_bytes(127), vec![0x7F]);
        assert_eq!(int_bytes(128), vec![0x00, 0x80]);
        assert_eq!(int_bytes(200), vec![0x00, 0xC8]);
        assert_eq!(int_bytes(255), vec![0x00, 0xFF]);
        assert_eq!(int_bytes(256), vec![0x01, 0x00]);
        assert_eq!(int_bytes(-1), vec![0xFF]);
        // (-128).bit_length() is 8: two bytes, as Python
        assert_eq!(int_bytes(-128), vec![0xFF, 0x80]);
        assert_eq!(int_bytes(-129), vec![0xFF, 0x7F]);
    }

    #[test]
    fn der_lengths() {
        assert_eq!(der_len(5), vec![5]);
        assert_eq!(der_len(0x80), vec![0x81, 0x80]);
        assert_eq!(der_len(0x1234), vec![0x82, 0x12, 0x34]);
    }

    #[test]
    fn entitlements_der_sorts_the_keys() {
        let xml = br#"<?xml version="1.0" encoding="UTF-8"?><plist version="1.0"><dict><key>b</key><true/><key>a</key><array><string>x</string><integer>-128</integer></array></dict></plist>"#;
        let v = plist::Value::from_reader(std::io::Cursor::new(&xml[..])).unwrap();
        let got = der_value(&v).unwrap();
        // b0 { 30 { 0c "a" 30 { 0c "x" 02 ff80 } } 30 { 0c "b" 01 ff } }
        let want = [0xB0, 0x16, 0x30, 0x0C, 0x0C, 0x01, b'a', 0x30, 0x07, 0x0C, 0x01, b'x', 0x02, 0x02, 0xFF, 0x80, 0x30, 0x06, 0x0C, 0x01, b'b', 0x01, 0x01, 0xFF];
        assert_eq!(got, want);
        let real = plist::Value::Real(1.5);
        assert!(der_value(&real).unwrap_err().to_string().contains("float"));
    }
}
