//! `ios codesign ARGS...`: `codesign` for the kernel base's build scripts (macOS and Linux), over `ios sign`.
//! userland/metal/shim/codesign (a two-line sh script) runs it; until 2026-11-21 the shim was Python. The forms
//! those scripts use:
//!   codesign --force --sign - --pagesize 4096 --identifier ID [--entitlements PLIST] FILE|BUNDLE
//!   codesign --verify --strict FILE|BUNDLE
//!   codesign --display --verbose=4 FILE|BUNDLE          (stderr: Identifier=..., CDHash=..., Page size=...)
//!   codesign -d --entitlements :- FILE                  (stdout: the signed entitlements)
//! A bundle means its CFBundleExecutable; no bundle seal is made (sign.rs says why).

use crate::macho::{signature_blob, MachO};
use anyhow::{anyhow, bail, Context, Result};
use std::io::Write;
use std::path::{Path, PathBuf};

fn option<'a>(args: &'a [String], name: &str) -> Option<&'a str> {
    let i = args.iter().position(|a| a == name)?;
    args.get(i + 1).map(String::as_str)
}

fn target(args: &[String]) -> Result<PathBuf> {
    let last = args.last().ok_or_else(|| anyhow!("codesign shim: no file"))?;
    let p = PathBuf::from(last);
    if p.is_dir() {
        let info = p.join("Info.plist");
        let v = plist::Value::from_file(&info).with_context(|| info.display().to_string())?;
        let exe = v.as_dictionary().and_then(|d| d.get("CFBundleExecutable")).and_then(|x| x.as_string()).ok_or_else(|| anyhow!("{}: no CFBundleExecutable", info.display()))?;
        return Ok(p.join(exe));
    }
    Ok(p)
}

pub fn run(args: &[String]) -> Result<i32> {
    let file = target(args)?;
    let has = |a: &str| args.iter().any(|x| x == a);
    if has("--sign") {
        if option(args, "--pagesize") != Some("4096") {
            bail!("codesign shim: only --pagesize 4096");
        }
        let id = option(args, "--identifier").map(str::to_string).unwrap_or_else(|| file.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default());
        let ent = match option(args, "--entitlements") {
            Some(p) => Some(std::fs::read(p).with_context(|| p.to_string())?),
            None => None,
        };
        let allocate = crate::sign::allocator(&crate::repo())?;
        crate::sign::sign(&file.to_string_lossy(), &id, ent.as_deref(), &allocate)?;
        return Ok(0);
    }
    let data = std::fs::read(&file).with_context(|| file.display().to_string())?;
    let describe = || -> Result<oneios_tweaks::macho::Slice> {
        oneios_tweaks::macho::describe(&data).map_err(|e| anyhow!("{}: {e}", file.display()))?.into_iter().next().ok_or_else(|| anyhow!("{}: no slice", file.display()))
    };
    if has("--verify") {
        describe()?;
        return Ok(0);
    }
    if has("--entitlements") {
        // -d --entitlements :-
        let m = MachO::parse(&data)?;
        let sig = m.signature().and_then(|(o, l)| data.get(o as usize..(o as usize).checked_add(l as usize)?)).ok_or_else(|| anyhow!("codesign shim: not signed"))?;
        let blob = signature_blob(sig, 0xFADE_7171).ok_or_else(|| anyhow!("codesign shim: no entitlements in the signature"))?;
        std::io::stdout().write_all(&blob[8..])?;
        return Ok(0);
    }
    if has("--display") || has("-d") {
        let info = describe()?;
        eprint!("Identifier={}\nCDHash={}\nPage size={}\n", info.identifier, info.cdhash, info.page_size);
        return Ok(0);
    }
    bail!("codesign shim: form not handled: {}", args.join(" "))
}

/// For the tests: the bundle's executable.
pub fn bundle_executable(p: &Path) -> Result<PathBuf> {
    target(&[p.display().to_string()])
}
