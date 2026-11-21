//! `ios sign`'s signature, `ios macho-check` and the stubs' writer on made-up Mach-Os (no Apple file): a thin arm64e
//! file with __TEXT, __LINKEDIT, LC_BUILD_VERSION, two dylibs, a symbol table and room for the signature.

use oneios_ios::{check, sign, tbd};
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::path::PathBuf;

const MH_EXECUTE: u32 = 2;
const MH_DYLIB: u32 = 6;

struct Sym {
    name: &'static str,
    undefined: bool,
    ordinal: u8,
    weak: bool,
}

fn pad8(v: &mut Vec<u8>) {
    while v.len() % 8 != 0 {
        v.push(0);
    }
}

fn dylib_cmd(cmd: u32, name: &str) -> Vec<u8> {
    let mut c = Vec::new();
    c.extend(cmd.to_le_bytes());
    c.extend(0u32.to_le_bytes());
    c.extend(24u32.to_le_bytes());
    c.extend(2u32.to_le_bytes());
    c.extend((1u32 << 16).to_le_bytes());
    c.extend((1u32 << 16).to_le_bytes());
    c.extend(name.as_bytes());
    c.push(0);
    pad8(&mut c);
    let n = c.len() as u32;
    c[4..8].copy_from_slice(&n.to_le_bytes());
    c
}

fn segment(name: &str, vmaddr: u64, vmsize: u64, fileoff: u64, filesize: u64) -> Vec<u8> {
    let mut c = Vec::new();
    c.extend(0x19u32.to_le_bytes());
    c.extend(72u32.to_le_bytes());
    let mut n = [0u8; 16];
    n[..name.len()].copy_from_slice(name.as_bytes());
    c.extend(n);
    for v in [vmaddr, vmsize, fileoff, filesize] {
        c.extend(v.to_le_bytes());
    }
    c.extend([5u32, 5, 0, 0].iter().flat_map(|x| x.to_le_bytes()));
    c
}

/// The file, with LC_CODE_SIGNATURE's room (`room` bytes of zeros) at its end.
fn macho(filetype: u32, id: Option<&str>, dylibs: &[&str], syms: &[Sym], code: usize, room: usize) -> Vec<u8> {
    let mut cmds: Vec<Vec<u8>> = Vec::new();
    let header = 32usize;
    // placeholders, sized first: segments 2*72, build version 24, symtab 24, code signature 16, dylibs
    let mut dylib_cmds: Vec<Vec<u8>> = dylibs.iter().map(|d| dylib_cmd(0xC, d)).collect();
    if let Some(i) = id {
        dylib_cmds.insert(0, dylib_cmd(0xD, i));
    }
    let size_cmds = 72 * 2 + 24 + 24 + 16 + dylib_cmds.iter().map(Vec::len).sum::<usize>();
    let text_end = (header + size_cmds + code).next_multiple_of(4096);
    // the symbol table and its strings in __LINKEDIT
    let mut strings = vec![b' ', 0];
    let mut nlist = Vec::new();
    for s in syms {
        let strx = strings.len() as u32;
        strings.extend(s.name.as_bytes());
        strings.push(0);
        nlist.extend(strx.to_le_bytes());
        nlist.push(if s.undefined { 0x01 } else { 0x0F });
        nlist.push(if s.undefined { 0 } else { 1 });
        let desc: u16 = ((s.ordinal as u16) << 8) | if s.weak { 0x40 } else { 0 };
        nlist.extend(desc.to_le_bytes());
        nlist.extend(if s.undefined { 0u64 } else { 0x1000u64 }.to_le_bytes());
    }
    pad8(&mut strings);
    let symoff = text_end;
    let stroff = symoff + nlist.len();
    let sigoff = (stroff + strings.len()).next_multiple_of(16);
    let end = sigoff + room;
    cmds.push(segment("__TEXT", 0x1_0000_0000, text_end as u64, 0, text_end as u64));
    cmds.push(segment("__LINKEDIT", 0x1_0000_0000 + text_end as u64, ((end - text_end) as u64).next_multiple_of(0x4000), text_end as u64, (end - text_end) as u64));
    let mut bv = Vec::new();
    for v in [0x32u32, 24, 2, 27 << 16, 26 << 16 | 5 << 8, 0] {
        bv.extend(v.to_le_bytes());
    }
    cmds.push(bv);
    let mut st = Vec::new();
    for v in [0x2u32, 24, symoff as u32, syms.len() as u32, stroff as u32, strings.len() as u32] {
        st.extend(v.to_le_bytes());
    }
    cmds.push(st);
    cmds.extend(dylib_cmds);
    let mut cs = Vec::new();
    for v in [0x1Du32, 16, sigoff as u32, room as u32] {
        cs.extend(v.to_le_bytes());
    }
    cmds.push(cs);
    let sizeofcmds: usize = cmds.iter().map(Vec::len).sum();
    assert_eq!(sizeofcmds, size_cmds);
    let mut f = Vec::with_capacity(end);
    for v in [0xFEED_FACFu32, 0x0100_000C, 0x8000_0002, filetype, cmds.len() as u32, sizeofcmds as u32, 0x0020_0085, 0] {
        f.extend(v.to_le_bytes());
    }
    for c in &cmds {
        f.extend(c);
    }
    // the "code": a pattern, not zeros, so that every page hash differs
    while f.len() < text_end {
        let i = f.len();
        f.push((i * 7 % 251) as u8);
    }
    f.extend(nlist);
    f.extend(strings);
    f.resize(sigoff, 0);
    f.resize(end, 0);
    f
}

fn sym(name: &'static str, ordinal: u8, weak: bool) -> Sym {
    Sym { name, undefined: true, ordinal, weak }
}

fn tmp(name: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("oneios-ios-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

const ENT: &[u8] = br#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
	<key>platform-application</key><true/>
	<key>com.apple.private.list</key><array><string>a</string><integer>-128</integer></array>
</dict>
</plist>
"#;

fn hex(b: &[u8]) -> String {
    b.iter().map(|x| format!("{x:02x}")).collect()
}

/// The signature of a made-up executable, without and with entitlements: the bytes scripts/ios/ios-sign.py's
/// `signature()` gave for the same files (2026-11-21, before it was retired).
#[test]
fn the_signature_is_the_retired_scripts() {
    let f = macho(MH_EXECUTE, None, &["/usr/lib/libSystem.B.dylib"], &[sym("_printf", 1, false)], 9000, 4096);
    let (sig, cdhash) = sign::signature(&f, "com.oneios.test", None).unwrap();
    let (sig_e, cdhash_e) = sign::signature(&f, "com.oneios.test", Some(ENT)).unwrap();
    assert_eq!(hex(&Sha256::digest(&f)), INPUT_SHA256);
    assert_eq!((hex(&Sha256::digest(&sig)), cdhash.as_str()), (PLAIN_SHA256.to_string(), PLAIN_CDHASH));
    assert_eq!((hex(&Sha256::digest(&sig_e)), cdhash_e.as_str()), (ENT_SHA256.to_string(), ENT_CDHASH));
}

const INPUT_SHA256: &str = "fd2d323990a1e3c16e205c18e719ba66d49f817b40b60da1fada864edebf3cfe";
const PLAIN_SHA256: &str = "fe91873ecdaf78cbe3c490c3f2791a9c1411d369d3a2dd508dc6a992a26c95e7";
const PLAIN_CDHASH: &str = "a18b4d797309e730e7a55ba0231e2182cb52b981";
const ENT_SHA256: &str = "de6228d48fd732f29798aefb8b468f2b535063d517f128a2f9a25adedc990db1";
const ENT_CDHASH: &str = "5515101bd83a3614d348a9f88574af765c951438";

/// Sign the made-up file in place of codesign_allocate (its room is already there) and write it.
fn signed(f: &mut Vec<u8>, id: &str) {
    let (sig, _) = sign::signature(f, id, None).unwrap();
    let m = oneios_ios::macho::MachO::parse(f).unwrap();
    let (off, _) = m.signature().unwrap();
    f[off as usize..off as usize + sig.len()].copy_from_slice(&sig);
}

#[test]
fn macho_check_binds_against_written_stubs() {
    let d = tmp("check");
    let stubs = d.join("tbd");
    // the stubs: libSystem re-exports libsystem_c, which has _printf; Foundation has a class
    let system = tbd::Stub { install_name: "/usr/lib/libSystem.B.dylib".into(), current_version: Some("1359".into()), reexported: vec!["/usr/lib/system/libsystem_c.dylib".into()], symbols: vec!["_mach_init_routine".into()], ..Default::default() };
    let libc = tbd::Stub { install_name: "/usr/lib/system/libsystem_c.dylib".into(), umbrella: Some("System".into()), symbols: vec!["_printf".into()], weak: vec!["_weak_def".into()], ..Default::default() };
    let foundation = tbd::Stub { install_name: "/System/Library/Frameworks/Foundation.framework/Foundation".into(), objc_classes: vec!["NSObject".into()], ..Default::default() };
    for (path, docs) in [("usr/lib/libSystem.B.tbd", vec![&system, &libc]), ("usr/lib/system/libsystem_c.tbd", vec![&libc]), ("System/Library/Frameworks/Foundation.framework/Foundation.tbd", vec![&foundation])] {
        let p = stubs.join(path);
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(p, tbd::write_file(&docs, "arm64e-ios")).unwrap();
    }
    let root = stubs.display().to_string();
    let libs = ["/usr/lib/libSystem.B.dylib", "/System/Library/Frameworks/Foundation.framework/Foundation", "/usr/lib/libmine.dylib"];
    let run = |name: &str, f: &[u8], local: &HashMap<String, String>| -> (bool, String) {
        let p = d.join(name);
        std::fs::write(&p, f).unwrap();
        let mut out = Vec::new();
        let ok = check::check(&mut out, &p.display().to_string(), &root, "arm64e-ios", "arm64e", local).unwrap();
        (ok, String::from_utf8(out).unwrap())
    };
    // a dylib that travels with it
    let mut mine = macho(MH_DYLIB, Some("/usr/lib/libmine.dylib"), &[], &[Sym { name: "_mine", undefined: false, ordinal: 0, weak: false }], 100, 2048);
    signed(&mut mine, "mine");
    let mine_path = d.join("libmine.dylib");
    std::fs::write(&mine_path, &mine).unwrap();
    let local: HashMap<String, String> = [("/usr/lib/libmine.dylib".to_string(), mine_path.display().to_string())].into();

    let good_syms = [sym("_printf", 1, false), sym("_OBJC_CLASS_$_NSObject", 2, false), sym("_OBJC_METACLASS_$_NSObject", 2, false), sym("_mine", 3, false), sym("_absent", 1, true), sym("_weak_def", 1, false)];
    let mut good = macho(MH_EXECUTE, None, &libs, &good_syms, 5000, 4096);
    signed(&mut good, "good");
    let (ok, text) = run("good", &good, &local);
    assert!(ok, "{text}");
    assert!(text.contains("  Mach-O arm64e EXECUTE, ") && text.contains("platform 2 minos 27.0 sdk 26.5"), "{text}");
    assert!(text.contains("  signed: SHA-256 code directory, 4096-byte pages"), "{text}");
    assert!(text.contains("  links /usr/lib/libmine.dylib (travels with it: "), "{text}");
    assert!(text.contains("  6 undefined symbols: 5 bound to a library whose stub exports them (arm64e-ios), 1 weak and absent"), "{text}");
    assert!(text.ends_with("PASS\n"), "{text}");

    // unsigned, a strong flat symbol, a symbol its library does not export, and a library without a stub
    let bad_syms = [sym("_printf", 1, false), sym("_flat", 0xFE, false), sym("_nope", 2, false)];
    let bad = macho(MH_EXECUTE, None, &["/usr/lib/libSystem.B.dylib", "/System/Library/Frameworks/Foundation.framework/Foundation", "/usr/lib/libnostub.dylib"], &bad_syms, 5000, 4096);
    let (ok, text) = run("bad", &bad, &HashMap::new());
    assert!(!ok);
    for want in ["FAIL not signed (oneios-tool ios sign)", "FAIL /usr/lib/libnostub.dylib: no stub in ", "FAIL strong symbol with no library (dynamic_lookup): _flat", "FAIL _nope (from Foundation) is not exported by its stub", "FAILED"] {
        assert!(text.contains(want), "{want}\n{text}");
    }
    std::fs::remove_dir_all(&d).unwrap();
}
