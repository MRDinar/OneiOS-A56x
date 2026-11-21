//! `ios tbds` on a tiny dyld shared cache made here (no Apple bytes): a main file with two images' headers and a
//! subcache that maps both images' __LINKEDIT, where their export tries are (as in iOS 27's split caches). Image A
//! re-exports B, so A's file holds B's document too; B's name is versioned, so a link without the version is made.

use oneios_ios::tbds;
use std::path::{Path, PathBuf};

const BASE: u64 = 0x1_8000_0000;
const A: u64 = BASE + 0x1000;
const B: u64 = BASE + 0x2000;
const LINKEDIT: u64 = BASE + 0x10000; // the subcache's file offset 0x1000

fn put(buf: &mut Vec<u8>, at: usize, bytes: &[u8]) {
    if buf.len() < at + bytes.len() {
        buf.resize(at + bytes.len(), 0);
    }
    buf[at..at + bytes.len()].copy_from_slice(bytes);
}
fn put32(buf: &mut Vec<u8>, at: usize, v: u32) {
    put(buf, at, &v.to_le_bytes());
}
fn put64(buf: &mut Vec<u8>, at: usize, v: u64) {
    put(buf, at, &v.to_le_bytes());
}

fn header(buf: &mut Vec<u8>, uuid: u8, addr: u64, size: u64, foff: u64) {
    put(buf, 0, b"dyld_v1  arm64e\0");
    put32(buf, 0x10, 0x200);
    put32(buf, 0x14, 1);
    put(buf, 0x58, &[uuid; 16]);
    put64(buf, 0x200, addr);
    put64(buf, 0x208, size);
    put64(buf, 0x210, foff);
}

fn segment(name: &str, vmaddr: u64, vmsize: u64, fileoff: u64) -> Vec<u8> {
    let mut c = vec![0u8; 72];
    put32(&mut c, 0, 0x19);
    put32(&mut c, 4, 72);
    put(&mut c, 8, name.as_bytes());
    put64(&mut c, 24, vmaddr);
    put64(&mut c, 32, vmsize);
    put64(&mut c, 40, fileoff);
    put64(&mut c, 48, vmsize);
    c
}

/// A command with a string (a dylib's name at 24, an umbrella's at 12).
fn str_cmd(cmd: u32, at: usize, fields: &[u32], s: &str) -> Vec<u8> {
    let mut c = vec![0u8; at];
    put32(&mut c, 0, cmd);
    put32(&mut c, 8, at as u32);
    for (i, f) in fields.iter().enumerate() {
        put32(&mut c, 12 + 4 * i, *f);
    }
    c.extend(s.as_bytes());
    c.push(0);
    while c.len() % 8 != 0 {
        c.push(0);
    }
    let n = c.len() as u32;
    put32(&mut c, 4, n);
    c
}

fn trie_cmd(off: u32, size: u32) -> Vec<u8> {
    let mut c = vec![0u8; 16];
    put32(&mut c, 0, 0x8000_0033);
    put32(&mut c, 4, 16);
    put32(&mut c, 8, off);
    put32(&mut c, 12, size);
    c
}

fn macho(buf: &mut Vec<u8>, at: usize, commands: &[Vec<u8>]) {
    put32(buf, at, 0xfeed_facf);
    put32(buf, at + 12, 6);
    put32(buf, at + 16, commands.len() as u32);
    let mut off = at + 32;
    for c in commands {
        put(buf, off, c);
        off += c.len();
    }
}

fn uleb(mut v: usize) -> Vec<u8> {
    let mut out = Vec::new();
    loop {
        let b = (v & 0x7F) as u8;
        v >>= 7;
        if v == 0 {
            out.push(b);
            return out;
        }
        out.push(b | 0x80);
    }
}

/// An export trie whose root has one edge per name: (name, flags). The children's offsets are two-byte ULEB128s.
fn trie(names: &[(&str, u8)]) -> Vec<u8> {
    let mut root = vec![0x00, names.len() as u8];
    let edges: usize = names.iter().map(|(n, _)| n.len() + 3).sum();
    let mut children = Vec::new();
    let mut at = 2 + edges;
    for (n, flags) in names {
        root.extend(n.as_bytes());
        root.push(0);
        let mut off = uleb(at);
        if off.len() == 1 {
            off = vec![off[0] | 0x80, 0x00];
        }
        root.extend(off);
        let node = [0x02, *flags, 0x10, 0x00];
        children.extend(node);
        at += node.len();
    }
    root.extend(children);
    root
}

fn build(dir: &Path) {
    std::fs::create_dir_all(dir).unwrap();
    let mut main = vec![0u8; 0x4000];
    header(&mut main, 0x5a, BASE, 0x4000, 0);
    put32(&mut main, 0x188, 0x300);
    put32(&mut main, 0x18C, 1);
    put(&mut main, 0x300 + 24, b".01\0");
    put32(&mut main, 0x1C0, 0x400);
    put32(&mut main, 0x1C4, 2);
    let paths: [(u64, &str, usize); 2] = [(A, "/usr/lib/liba.dylib", 0x500), (B, "/usr/lib/libb.1.dylib", 0x540)];
    for (i, (addr, path, off)) in paths.iter().enumerate() {
        put64(&mut main, 0x400 + 32 * i, *addr);
        put32(&mut main, 0x400 + 32 * i + 24, *off as u32);
        put(&mut main, *off, path.as_bytes());
    }
    let ta = trie(&[
        ("_a_fn", 0x00),
        ("_a_weak", 0x04),
        ("_a_tls", 0x01),
        ("_OBJC_CLASS_$_AThing", 0x00),
        ("_OBJC_METACLASS_$_AThing", 0x00),
        ("_OBJC_CLASS_$_OnlyClass", 0x00),
        ("_OBJC_IVAR_$_AThing._x", 0x00),
    ]);
    let tb = trie(&[("_b_fn", 0x00)]);
    // the tries in the subcache's __LINKEDIT: file offsets 0x1000 + 0x100 and + 0x400; the load commands give them
    // as offsets of the file that maps __LINKEDIT, whose segment says fileoff 0x1000
    macho(
        &mut main,
        0x1000,
        &[
            segment("__TEXT", A, 0x1000, 0x1000),
            segment("__LINKEDIT", LINKEDIT, 0x1000, 0x1000),
            str_cmd(0xD, 24, &[0, (2 << 16) | (1 << 8) | 3, 1 << 16], "/usr/lib/liba.dylib"),
            str_cmd(0x8000_001F, 24, &[0, 1 << 16, 1 << 16], "/usr/lib/libb.1.dylib"),
            trie_cmd(0x1100, ta.len() as u32),
        ],
    );
    macho(
        &mut main,
        0x2000,
        &[
            segment("__TEXT", B, 0x1000, 0x2000),
            segment("__LINKEDIT", LINKEDIT, 0x1000, 0x1000),
            str_cmd(0xD, 24, &[0, 7 << 16, 1 << 16], "/usr/lib/libb.1.dylib"),
            str_cmd(0x12, 12, &[], "Umbrella"),
            trie_cmd(0x1400, tb.len() as u32),
        ],
    );
    std::fs::write(dir.join("dyld_shared_cache_arm64e"), &main).unwrap();
    let mut sub = vec![0u8; 0x2000];
    header(&mut sub, 0x6b, LINKEDIT, 0x1000, 0x1000);
    put(&mut sub, 0x1100, &ta);
    put(&mut sub, 0x1400, &tb);
    std::fs::write(dir.join("dyld_shared_cache_arm64e.01"), &sub).unwrap();
}

fn tmp(name: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("oneios-ios-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    d
}

#[test]
fn stubs_of_a_made_up_cache() {
    let d = tmp("tbds");
    build(&d.join("cache"));
    let out = d.join("tbd");
    let o = tbds::Options { cache: Some(d.join("cache")), image: d.join("none.img"), out: out.clone(), names: vec![] };
    assert_eq!(tbds::run(&d, &o).unwrap(), 0);
    let a = std::fs::read_to_string(out.join("usr/lib/liba.tbd")).unwrap();
    assert_eq!(
        a,
        "--- !tapi-tbd
tbd-version:          4
targets:              [ arm64e-ios ]
install-name:         '/usr/lib/liba.dylib'
current-version:      2.1.3
reexported-libraries:
  - targets:          [ arm64e-ios ]
    libraries:        [ '/usr/lib/libb.1.dylib' ]
exports:
  - targets:          [ arm64e-ios ]
    symbols:          [ _OBJC_CLASS_$_OnlyClass,
                       _a_fn ]
    objc-classes:    [ AThing ]
    objc-ivars:      [ AThing._x ]
    weak-symbols:    [ _a_weak ]
    thread-local-symbols: [ _a_tls ]
--- !tapi-tbd
tbd-version:          4
targets:              [ arm64e-ios ]
install-name:         '/usr/lib/libb.1.dylib'
current-version:      7
parent-umbrella:
  - targets:          [ arm64e-ios ]
    umbrella:         Umbrella
exports:
  - targets:          [ arm64e-ios ]
    symbols:          [ _b_fn ]
...
"
    );
    let b = std::fs::read_to_string(out.join("usr/lib/libb.1.tbd")).unwrap();
    assert!(b.starts_with("--- !tapi-tbd\n") && b.contains("    symbols:          [ _b_fn ]\n...\n") && b.matches("--- ").count() == 1);
    assert_eq!(std::fs::read_link(out.join("usr/lib/libb.tbd")).unwrap(), PathBuf::from("libb.1.tbd"));
    let record = std::fs::read_to_string(out.join("tbd.json")).unwrap();
    assert!(record.contains("\"uuid\": \"5A5A5A5A-5A5A-5A5A-5A5A-5A5A5A5A5A5A\""), "{record}");
    assert!(record.contains("\"images_in_cache\": 2") && record.contains("\"stubs\": 3"), "{record}");
    // a second run keeps what is there; a name picks one image
    std::fs::write(out.join("usr/lib/liba.tbd"), "kept").unwrap();
    let o = tbds::Options { names: vec!["libb.1.dylib".into()], ..o };
    assert_eq!(tbds::run(&d, &o).unwrap(), 0);
    assert_eq!(std::fs::read_to_string(out.join("usr/lib/liba.tbd")).unwrap(), "kept");
    let o = tbds::Options { names: vec!["nothing.dylib".into()], ..o };
    assert!(tbds::run(&d, &o).unwrap_err().to_string().contains("0 images of the cache have that name"));
    std::fs::remove_dir_all(&d).unwrap();
}
