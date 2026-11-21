//! `ios macho-check`: will an iOS Mach-O (executable, dylib or bundle) built here load in the guest? The port of
//! scripts/ios/macho-check.py (retired 2026-11-21); the same report, line for line (README "ios"). It needs no
//! outside program: the load commands and the symbol table are read here (the script ran llvm-objdump and llvm-nm),
//! the stubs with this crate's TBD reader (the script needed PyYAML).
//!
//! For each FILE:
//! 1. It is a thin Mach-O for arm64e (or --arch), platform iOS (LC_BUILD_VERSION), an executable, dylib or bundle.
//! 2. Every library it loads has a .tbd stub in the stubs' folder: out/ios27-tbd by default (`ios tbds` makes them
//!    from the guest's own shared cache); --sdk checks against the SDK of scripts/ios/env.sh instead. A library
//!    that travels with the file (the plug-in's Turnip) is named with --local and its symbol table read.
//! 3. Every undefined symbol is bound to one of those libraries (two-level namespace) and that library, or one it
//!    re-exports, exports it for arm64e-ios. A strong symbol with no library (-undefined dynamic_lookup) fails:
//!    dyld refuses the file at load. Weak ones are counted (NULL when absent).
//! 4. It is signed, with 4 KiB signature pages (`ios sign`).

use crate::macho::{code_directory, MachO, MH_BUNDLE, MH_DYLIB, MH_EXECUTE, MH_TWOLEVEL};
use crate::tbd;
use anyhow::{anyhow, bail, Context, Result};
use std::collections::{BTreeSet, HashMap};
use std::io::Write;
use std::path::Path;

fn filetype(t: u32) -> Option<&'static str> {
    match t {
        MH_EXECUTE => Some("EXECUTE"),
        MH_DYLIB => Some("DYLIB"),
        MH_BUNDLE => Some("BUNDLE"),
        _ => None,
    }
}

/// macho-check.py's `stub_for`: X.dylib -> X.tbd; a versioned framework's binary also as the framework's X.tbd.
pub fn stub_for(root: &str, install_name: &str) -> Option<String> {
    let base = install_name.strip_suffix(".dylib").unwrap_or(install_name);
    let mut cands = vec![format!("{root}{base}.tbd")];
    let re = regex::Regex::new(r"^(.*/([^/]+)\.framework)/Versions/[^/]+/([^/]+)$").expect("the pattern");
    if let Some(m) = re.captures(install_name) {
        cands.push(format!("{root}{}/{}.tbd", &m[1], &m[3]));
    }
    cands.into_iter().find(|p| Path::new(p).exists())
}

/// The defined external symbols of a dylib that travels with the file (`llvm-nm -gU`'s names).
fn local_exports(path: &str) -> Result<BTreeSet<String>> {
    let data = std::fs::read(path).with_context(|| path.to_string())?;
    let m = MachO::parse(&data).with_context(|| path.to_string())?;
    Ok(m.symbols()?.into_iter().filter(|s| !s.is_stab() && s.is_external() && !s.is_undefined()).map(|s| s.name).collect())
}

struct Libraries<'a> {
    root: &'a str,
    target: &'a str,
    local: &'a HashMap<String, String>,
    /// install name -> (symbols, re-exported libraries)
    exports: HashMap<String, (BTreeSet<String>, Vec<String>)>,
}

impl Libraries<'_> {
    fn load(&mut self, name: &str) -> Result<()> {
        if self.exports.contains_key(name) {
            return Ok(());
        }
        self.exports.insert(name.to_string(), Default::default());
        if let Some(file) = self.local.get(name) {
            let syms = local_exports(file)?;
            self.exports.insert(name.to_string(), (syms, Vec::new()));
            return Ok(());
        }
        if let Some(stub) = stub_for(self.root, name) {
            let text = std::fs::read_to_string(&stub).with_context(|| stub.clone())?;
            for d in tbd::exports(&text, self.target).with_context(|| stub.clone())? {
                self.exports.insert(d.install_name, (d.symbols, d.reexports));
            }
        }
        Ok(())
    }

    fn provides(&mut self, name: &str, sym: &str, seen: &mut Vec<String>) -> Result<bool> {
        if seen.iter().any(|s| s == name) {
            return Ok(false);
        }
        self.load(name)?;
        let (has, reex) = match self.exports.get(name) {
            Some((s, r)) => (s.contains(sym), r.clone()),
            None => (false, Vec::new()),
        };
        if has {
            return Ok(true);
        }
        seen.push(name.to_string());
        for r in reex {
            if self.provides(&r, sym, seen)? {
                seen.pop();
                return Ok(true);
            }
        }
        seen.pop();
        Ok(false)
    }
}

fn platform_text(p: Option<u32>) -> String {
    p.map(|v| v.to_string()).unwrap_or_else(|| "None".into())
}

fn ver(v: u32) -> String {
    format!("{}.{}", v >> 16, (v >> 8) & 0xFF)
}

/// Check one file; the report goes to `out`.
pub fn check(out: &mut dyn Write, path: &str, root: &str, target: &str, arch: &str, local: &HashMap<String, String>) -> Result<bool> {
    let mut ok = true;
    writeln!(out, "file: {path}")?;
    let data = std::fs::read(path).with_context(|| path.to_string())?;
    let m = match MachO::parse(&data) {
        Ok(m) if filetype(m.filetype).is_some() => m,
        _ => {
            writeln!(out, "FAIL not a thin 64-bit Mach-O executable, dylib or bundle")?;
            return Ok(false);
        }
    };
    let file_arch = match (m.cputype, m.cpusubtype & 0xFF_FFFF) {
        (0x0100_000C, 2) => "arm64e".to_string(),
        (0x0100_000C, 0) => "arm64".to_string(),
        _ => format!("cpu-{:#x}", m.cputype),
    };
    let bv = m.build_version();
    let (platform, minos, sdk) = match bv {
        Some((p, mi, s)) => (Some(p), ver(mi), ver(s)),
        None => (None, "None".into(), "None".into()),
    };
    writeln!(out, "  Mach-O {file_arch} {}, {} bytes, platform {} minos {minos} sdk {sdk}", filetype(m.filetype).unwrap_or("?"), data.len(), platform_text(platform))?;
    if file_arch != arch {
        writeln!(out, "FAIL {file_arch}, not {arch}")?;
        ok = false;
    }
    if platform != Some(2) {
        writeln!(out, "FAIL not built for iOS (LC_BUILD_VERSION platform 2)")?;
        ok = false;
    }
    let cd = m.signature().and_then(|(o, l)| data.get(o as usize..(o as usize).checked_add(l as usize)?)).and_then(code_directory);
    match cd {
        None => {
            writeln!(out, "FAIL not signed (oneios-tool ios sign)")?;
            ok = false;
        }
        Some((2, 4096)) => writeln!(out, "  signed: SHA-256 code directory, 4096-byte pages")?,
        Some((t, p)) => {
            writeln!(out, "FAIL signature hash type {t}, pages of {p} bytes (want SHA-256, 4096)")?;
            ok = false;
        }
    }
    if let Some(own) = m.install_name() {
        writeln!(out, "  install name {own}")?;
    }
    let dylibs = m.dylibs();
    let mut libs = Libraries { root, target, local, exports: HashMap::new() };
    for (_, u) in &dylibs {
        libs.load(u)?;
        if let Some(f) = local.get(u) {
            writeln!(out, "  links {u} (travels with it: {f})")?;
        } else if stub_for(root, u).is_none() {
            writeln!(out, "FAIL {u}: no stub in {root}")?;
            ok = false;
        } else {
            writeln!(out, "  links {u}")?;
        }
    }
    let two_level = m.flags & MH_TWOLEVEL != 0;
    let (mut bad, mut flat, mut weakflat, mut bound, mut n) = (Vec::new(), Vec::new(), 0usize, 0usize, 0usize);
    for s in m.symbols()?.into_iter().filter(|s| !s.is_stab() && s.is_undefined() && s.is_external()) {
        n += 1;
        let weak = s.weak_ref();
        let ord = s.ordinal() as usize;
        let lib = if two_level && (1..=dylibs.len()).contains(&ord) { Some(dylibs[ord - 1].1.clone()) } else { None };
        match lib {
            None if two_level && ord == 0xFF => {
                // (from executable): no library of the guest has it
                if weak {
                    weakflat += 1;
                } else {
                    bad.push(format!("{} (from executable) is not exported by its stub", s.name));
                }
            }
            None => {
                if weak {
                    weakflat += 1;
                } else {
                    flat.push(s.name.clone());
                }
            }
            Some(l) => {
                if libs.provides(&l, &s.name, &mut Vec::new())? {
                    bound += 1;
                } else if weak {
                    weakflat += 1;
                } else {
                    let short = Path::new(&l).file_name().map(|f| f.to_string_lossy().split('.').next().unwrap_or("").to_string()).unwrap_or_default();
                    bad.push(format!("{} (from {short}) is not exported by its stub", s.name));
                }
            }
        }
    }
    writeln!(out, "  {n} undefined symbols: {bound} bound to a library whose stub exports them ({target}), {weakflat} weak and absent or looked up dynamically (NULL when absent)")?;
    for s in &flat {
        writeln!(out, "FAIL strong symbol with no library (dynamic_lookup): {s}")?;
        ok = false;
    }
    for b in &bad {
        writeln!(out, "FAIL {b}")?;
        ok = false;
    }
    writeln!(out, "{}", if ok { "PASS" } else { "FAILED" })?;
    Ok(ok)
}

pub struct Options {
    pub tbd: Option<String>,
    pub sdk: bool,
    pub local: Vec<String>,
    pub arch: String,
    pub files: Vec<String>,
}

/// The SDK's folder as scripts/ios/env.sh finds it.
fn sdk_root(repo: &Path) -> Result<String> {
    let env = repo.join("scripts/ios/env.sh");
    let out = std::process::Command::new("sh").arg("-c").arg(". \"$1\" && printf %s \"$IOS_SDK\"").arg("sh").arg(&env).output().with_context(|| env.display().to_string())?;
    if !out.status.success() {
        bail!("{}: {}", env.display(), String::from_utf8_lossy(&out.stderr).trim());
    }
    Ok(String::from_utf8_lossy(&out.stdout).into_owned())
}

pub fn run(repo: &Path, o: &Options) -> Result<i32> {
    let root = if o.sdk {
        sdk_root(repo)?
    } else {
        o.tbd.clone().or_else(|| std::env::var("IOS_TBD").ok().filter(|v| !v.is_empty())).unwrap_or_else(|| repo.join("out/ios27-tbd").display().to_string())
    };
    if !Path::new(&root).join("usr/lib/libSystem.B.tbd").exists() {
        bail!("no stubs in {root} (oneios-tool ios tbds makes the iOS 27 ones; --sdk uses the SDK's)");
    }
    let mut out = std::io::stdout().lock();
    writeln!(out, "stubs: {root}")?;
    let mut local = HashMap::new();
    for f in &o.local {
        let data = std::fs::read(f).with_context(|| f.clone())?;
        let own = MachO::parse(&data).ok().and_then(|m| m.install_name()).ok_or_else(|| anyhow!("--local {f}: not a dylib"))?;
        local.insert(own, f.clone());
    }
    let target = format!("{}-ios", o.arch);
    let mut all = true;
    for f in &o.files {
        all &= check(&mut out, f, &root, &target, &o.arch, &local)?;
    }
    Ok(if all { 0 } else { 1 })
}
