//! `oneios-tool debug mktar [--format pax|gnu] OUT < SPEC` (new, 2026-11-21): a tar for userland/tests/host.sh,
//! the host tests of the guest helper oneios-seed. It writes what that test's Python (`test_helpers.py`'s
//! `seed()`, Python's `tarfile`) wrote, byte for byte: the pax or GNU form, numeric owners and no owner names,
//! time 0, a folder's name with a `/` at its end, a long name as a pax `path` record (pax) or a `././@LongLink`
//! entry (GNU), extended attributes as `LIBARCHIVE.xattr.NAME` pax records, two empty blocks at the end and the
//! file padded to 10240 bytes. Unlike `tar`, it writes any name it is given (`../x`, `/abs`, a file under a
//! symbolic link): the test checks that oneios-seed refuses those.
//!
//! SPEC, one entry per line (empty lines and `#` lines are skipped), fields split at white space:
//!
//!   file NAME [mode=OCTAL] [uid=N] [gid=N] [data=PATH] [pax=KEY=VALUE]...
//!   dir NAME ...        symlink NAME target=TARGET ...        hard NAME target=TARGET ...
//!
//! mode defaults to 0644, uid and gid to 0; data=PATH is the file's bytes (none without it).

use anyhow::{bail, Context, Result};

const BLOCK: usize = 512;
const RECORD: usize = 20 * BLOCK;
const POSIX_MAGIC: &[u8; 8] = b"ustar\x0000";
const GNU_MAGIC: &[u8; 8] = b"ustar  \x00";

struct Header<'a> {
    name: &'a [u8],
    mode: u32,
    uid: u64,
    gid: u64,
    size: u64,
    kind: u8,
    linkname: &'a [u8],
    magic: &'a [u8; 8],
}

/// A field of `len` bytes: the text cut to `len`, NULs after it (tarfile's `stn`).
fn stn(out: &mut Vec<u8>, s: &[u8], len: usize) {
    let s = &s[..s.len().min(len)];
    out.extend_from_slice(s);
    out.resize(out.len() + len - s.len(), 0);
}

/// An octal field of `digits` bytes: digits - 1 octal digits and a NUL (tarfile's `itn`).
fn itn(out: &mut Vec<u8>, n: u64, digits: usize) -> Result<()> {
    if n >= 8u64.pow(digits as u32 - 1) {
        bail!("mktar: {n} does not fit a field of {digits} bytes");
    }
    out.extend_from_slice(format!("{n:0w$o}", w = digits - 1).as_bytes());
    out.push(0);
    Ok(())
}

/// tarfile's `_create_header`: one 512-byte block with its checksum.
fn header(h: &Header) -> Result<Vec<u8>> {
    let mut b = Vec::with_capacity(BLOCK);
    stn(&mut b, h.name, 100);
    itn(&mut b, (h.mode & 0o7777) as u64, 8)?;
    itn(&mut b, h.uid, 8)?;
    itn(&mut b, h.gid, 8)?;
    itn(&mut b, h.size, 12)?;
    itn(&mut b, 0, 12)?; // mtime
    b.extend_from_slice(b"        ");
    b.push(h.kind);
    stn(&mut b, h.linkname, 100);
    b.extend_from_slice(h.magic);
    stn(&mut b, b"", 32); // uname
    stn(&mut b, b"", 32); // gname
    stn(&mut b, b"", 8); // devmajor
    stn(&mut b, b"", 8); // devminor
    stn(&mut b, b"", 155); // prefix
    b.resize(BLOCK, 0);
    let sum: u32 = b.iter().map(|&c| c as u32).sum();
    b[148..156].copy_from_slice(format!("{sum:06o}\0 ").as_bytes());
    Ok(b)
}

fn payload(out: &mut Vec<u8>, data: &[u8]) {
    out.extend_from_slice(data);
    out.resize(out.len().div_ceil(BLOCK) * BLOCK, 0);
}

/// One pax record, `LEN KEY=VALUE\n`, LEN counting itself.
fn pax_record(key: &[u8], value: &[u8]) -> Vec<u8> {
    let l = key.len() + value.len() + 3;
    let mut p = 0;
    loop {
        let n = l + p.to_string().len();
        if n == p {
            break;
        }
        p = n;
    }
    let mut r = format!("{p} ").into_bytes();
    r.extend_from_slice(key);
    r.push(b'=');
    r.extend_from_slice(value);
    r.push(b'\n');
    r
}

pub struct Entry {
    pub kind: u8,
    pub name: String,
    pub mode: u32,
    pub uid: u64,
    pub gid: u64,
    pub target: String,
    pub data: Vec<u8>,
    pub pax: Vec<(String, String)>,
}

/// The archive's bytes; `gnu`: GNU_FORMAT, else PAX_FORMAT.
pub fn archive(entries: &[Entry], gnu: bool) -> Result<Vec<u8>> {
    let mut out = Vec::new();
    for e in entries {
        let mut name = e.name.clone();
        if e.kind == b'5' && !name.ends_with('/') {
            name.push('/');
        }
        let size = if e.kind == b'0' { e.data.len() as u64 } else { 0 };
        let main = |magic| Header { name: name.as_bytes(), mode: e.mode, uid: e.uid, gid: e.gid, size, kind: e.kind, linkname: e.target.as_bytes(), magic };
        if gnu {
            for (long, kind) in [(e.target.as_bytes(), b'K'), (name.as_bytes(), b'L')] {
                if long.len() > 100 {
                    let mut text = long.to_vec();
                    text.push(0);
                    out.extend(header(&Header { name: b"././@LongLink", mode: 0, uid: 0, gid: 0, size: text.len() as u64, kind, linkname: b"", magic: GNU_MAGIC })?);
                    payload(&mut out, &text);
                }
            }
            out.extend(header(&main(GNU_MAGIC))?);
        } else {
            let mut pax: Vec<(String, String)> = e.pax.clone();
            for (key, value) in [("path", &name), ("linkpath", &e.target)] {
                if !pax.iter().any(|(k, _)| k == key) && value.len() > 100 {
                    pax.push((key.to_string(), value.to_string()));
                }
            }
            if !pax.is_empty() {
                let records: Vec<u8> = pax.iter().flat_map(|(k, v)| pax_record(k.as_bytes(), v.as_bytes())).collect();
                out.extend(header(&Header { name: b"././@PaxHeader", mode: 0, uid: 0, gid: 0, size: records.len() as u64, kind: b'x', linkname: b"", magic: POSIX_MAGIC })?);
                payload(&mut out, &records);
            }
            out.extend(header(&main(POSIX_MAGIC))?);
        }
        if e.kind == b'0' {
            payload(&mut out, &e.data);
        }
    }
    out.resize(out.len() + 2 * BLOCK, 0);
    out.resize(out.len().div_ceil(RECORD) * RECORD, 0);
    Ok(out)
}

pub fn parse(spec: &str) -> Result<Vec<Entry>> {
    let mut entries = Vec::new();
    for (n, line) in spec.lines().enumerate() {
        let words: Vec<&str> = line.split_whitespace().collect();
        if words.is_empty() || words[0].starts_with('#') {
            continue;
        }
        let kind = match words[0] {
            "file" => b'0',
            "dir" => b'5',
            "symlink" => b'2',
            "hard" => b'1',
            other => bail!("mktar: line {}: {other} is not file, dir, symlink or hard", n + 1),
        };
        let Some(name) = words.get(1) else { bail!("mktar: line {}: no name", n + 1) };
        let mut e = Entry { kind, name: name.to_string(), mode: 0o644, uid: 0, gid: 0, target: String::new(), data: Vec::new(), pax: Vec::new() };
        for w in &words[2..] {
            let Some((key, value)) = w.split_once('=') else { bail!("mktar: line {}: {w} is not KEY=VALUE", n + 1) };
            let num = |v: &str, radix| u64::from_str_radix(v, radix).with_context(|| format!("mktar: line {}: {w}", n + 1));
            match key {
                "mode" => e.mode = num(value, 8)? as u32,
                "uid" => e.uid = num(value, 10)?,
                "gid" => e.gid = num(value, 10)?,
                "target" => e.target = value.to_string(),
                "data" => e.data = std::fs::read(value).with_context(|| format!("mktar: line {}: {value}", n + 1))?,
                "pax" => {
                    let Some((k, v)) = value.split_once('=') else { bail!("mktar: line {}: pax={value} is not pax=KEY=VALUE", n + 1) };
                    e.pax.push((k.to_string(), v.to_string()));
                }
                _ => bail!("mktar: line {}: unknown field {key}", n + 1),
            }
        }
        if matches!(kind, b'1' | b'2') && e.target.is_empty() {
            bail!("mktar: line {}: a link needs target=", n + 1);
        }
        entries.push(e);
    }
    Ok(entries)
}

pub fn run(format: &str, out: &str) -> Result<i32> {
    let gnu = match format {
        "pax" => false,
        "gnu" => true,
        other => bail!("mktar: --format is pax or gnu, not {other}"),
    };
    let mut spec = String::new();
    std::io::Read::read_to_string(&mut std::io::stdin(), &mut spec).context("mktar: reading the spec")?;
    let bytes = archive(&parse(&spec)?, gnu)?;
    std::fs::write(out, bytes).with_context(|| format!("mktar: {out}"))?;
    Ok(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pax_records_count_their_own_length() {
        assert_eq!(pax_record(b"path", b"a"), b"9 path=a\n");
        // 4 + 93 + 3 = 100 bytes without the length; its 3 digits make 103 (Python's tarfile: "103 path=...")
        let v = "v".repeat(93);
        let r = pax_record(b"path", v.as_bytes());
        assert_eq!(r.len(), 103);
        assert!(r.starts_with(b"103 "));
    }

    #[test]
    fn a_long_name_and_a_folder() {
        let long = "d/".repeat(60) + "f";
        let spec = format!("dir d\nfile {long} mode=600\n");
        let pax = archive(&parse(&spec).unwrap(), false).unwrap();
        assert_eq!(pax.len() % RECORD, 0);
        assert_eq!(&pax[..2], b"d/");
        assert_eq!(pax[BLOCK + 156], b'x');
        let gnu = archive(&parse(&spec).unwrap(), true).unwrap();
        assert_eq!(&gnu[BLOCK..BLOCK + 13], b"././@LongLink");
        assert_eq!(gnu[BLOCK + 156], b'L');
        assert!(parse("pipe x").is_err() && parse("symlink x").is_err());
    }
}
