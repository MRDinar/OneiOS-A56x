//! The stand-in logs and the address sets of tools/oneios-tool/tests/sym/compare-golden.sh (was
//! tests/sym/make-log.py and an inline Python block of that script, until 2026-11-21).
//!
//!   debug sym-make-log DSC_DIR OUT.log [--slide HEX] [--stacks N] [--seed N]
//!
//! A stand-in xnuhost log for a host that has the dyld shared cache but no log from the phone: `oneios-ubt` thread
//! lines (12 to 14 return addresses each) and one `sheng-ubt sheng-exc` crash backtrace in the format xnuhost
//! prints. The addresses are real: each is the word after a `bl` in the cache's executable mappings, plus the
//! slide (default 0xedb4000, a multiple of 0x4000 as dyld's slides are), so the symbolizer finds the slide and
//! names them. The stacks are not real call chains: frames are drawn at random (Python's `random.Random(seed)`,
//! so the addresses are the ones make-log.py drew and the golden's argument hashes hold). Nothing of the cache is
//! written but those addresses.
//!
//!   debug sym-cases WORK LOG...
//!
//! The address sets, one file `WORK/NAME.args` per case (the arguments one per line), and `WORK/cases` (the
//! names of the `a` sets): whole backtraces (a), addresses outside the cache (c), two that tie (d), a large set
//! (e-large).

use std::fs::File;
use std::io::{Read, Seek, SeekFrom};

use anyhow::{bail, Context, Result};
use regex::Regex;

use crate::pyrandom::PyRandom;

/// Python's `int(text, 0)`.
fn int0(text: &str) -> Result<u64> {
    let t = text.trim().replace('_', "");
    let (digits, radix) = match t.get(..2).map(str::to_ascii_lowercase).as_deref() {
        Some("0x") => (&t[2..], 16),
        Some("0o") => (&t[2..], 8),
        Some("0b") => (&t[2..], 2),
        _ => (&t[..], 10),
    };
    u64::from_str_radix(digits, radix).with_context(|| format!("invalid literal for int() with base 0: '{text}'"))
}

pub const MAKE_LOG_USAGE: &str = "usage: oneios-tool debug sym-make-log DSC_DIR OUT.log [--slide HEX] [--stacks N] [--seed N]";

struct Text {
    address: u64,
    size: u64,
    path: String,
    fileoff: u64,
}

pub fn make_log(argv: &[String]) -> Result<i32> {
    let mut args: Vec<String> = argv.to_vec();
    let mut option = |name: &str, default: u64| -> Result<u64> {
        if let Some(i) = args.iter().position(|a| a == name) {
            let Some(value) = args.get(i + 1).cloned() else { bail!("{MAKE_LOG_USAGE}") };
            args.drain(i..i + 2);
            return int0(&value);
        }
        Ok(default)
    };
    let slide = option("--slide", 0xEDB_4000)?;
    let stacks = option("--stacks", 400)?;
    let seed = option("--seed", 1)?;
    if args.len() != 2 {
        bail!("{MAKE_LOG_USAGE}");
    }
    let (dsc, out) = (&args[0], &args[1]);
    let mut rng = PyRandom::new(u32::try_from(seed).context("--seed: below 2^32")?);

    // every file's executable mappings: (address, size, file, offset)
    let mut names: Vec<String> = std::fs::read_dir(dsc).with_context(|| dsc.clone())?.filter_map(|e| e.ok()).map(|e| e.file_name().to_string_lossy().into_owned()).collect();
    names.sort();
    let mut texts = Vec::new();
    for name in names {
        if !name.starts_with("dyld_shared_cache_arm64e") || [".symbols", ".dylddata", ".dyldreadonly", ".dyldlinkedit"].iter().any(|s| name.ends_with(s)) {
            continue;
        }
        let path = format!("{dsc}/{name}");
        let mut f = File::open(&path).with_context(|| path.clone())?;
        let mut head = Vec::new();
        f.by_ref().take(0x200).read_to_end(&mut head)?;
        if head.len() < 0x18 || &head[..4] != b"dyld" {
            continue;
        }
        let offset = u32::from_le_bytes(head[0x10..0x14].try_into().unwrap()) as u64;
        let count = u32::from_le_bytes(head[0x14..0x18].try_into().unwrap());
        f.seek(SeekFrom::Start(offset))?;
        for _ in 0..count {
            let mut m = [0u8; 32];
            f.read_exact(&mut m).with_context(|| path.clone())?;
            let q = |i: usize| u64::from_le_bytes(m[i..i + 8].try_into().unwrap());
            let initprot = u32::from_le_bytes(m[28..32].try_into().unwrap());
            if initprot & 4 != 0 && q(8) > 0x8000 {
                texts.push(Text { address: q(0), size: q(8), path: path.clone(), fileoff: q(16) });
            }
        }
    }
    if texts.is_empty() {
        bail!("make-log: no executable mapping in {dsc}");
    }

    // Addresses right after a call, from random places of the executable mappings.
    let return_addresses = |rng: &mut PyRandom, count: u64| -> Result<Vec<u64>> {
        let mut found = Vec::new();
        while (found.len() as u64) < count {
            let t = &texts[rng.below(texts.len() as u64) as usize];
            // randrange(0, size - 0x4000, 4)
            let n = (t.size - 0x4000).div_ceil(4);
            if n >= 1 << 32 {
                bail!("{}: a mapping of {:#x} bytes: randrange past 2^32 is not ported", t.path, t.size);
            }
            let at = 4 * rng.below(n);
            let mut f = File::open(&t.path)?;
            f.seek(SeekFrom::Start(t.fileoff + at))?;
            let mut block = Vec::new();
            f.take(0x4000).read_to_end(&mut block)?;
            for i in (0..block.len().saturating_sub(3)).step_by(4) {
                if u32::from_le_bytes(block[i..i + 4].try_into().unwrap()) & 0xFC00_0000 == 0x9400_0000 {
                    found.push(t.address + at + i as u64 + 4 + slide);
                    break;
                }
            }
        }
        Ok(found)
    };

    let mut lines = vec!["[    0.000001] xnuhost: stand-in log made by oneios-tool debug sym-make-log".to_string()];
    let mut clock = 1.0f64;
    let procs = ["backboardd", "SpringBoard", "launchd", "locationd"];
    for n in 0..stacks {
        clock += rng.random() / 10.0;
        let k = 12 + rng.below(3);
        let frames = return_addresses(&mut rng, k)?;
        let who = rng.choice(&procs);
        let list: String = frames.iter().map(|a| format!(" {a:#x}")).collect();
        lines.push(format!("[{clock:12.6}] guest  oneios-ubt: {who}[{}] thread {:#x} state 0x5:{list}", 30 + n % 40, 0x400 + n));
    }
    let crash = return_addresses(&mut rng, 7)?;
    lines.push(format!("[{:12.6}] guest  sheng-ubt sheng-exc: pc={:#x} lr={:#x} sp=0x16f1648f0", clock + 1.0, crash[0] - 4, crash[1]));
    for (i, address) in crash[2..].iter().enumerate() {
        lines.push(format!("[{:12.6}] guest  sheng-ubt sheng-exc: #{i} fp=0x16f164{:03x} ret={address:#x}", clock + 1.0, 0x900 + 16 * i));
    }
    lines.push(format!("[{:12.6}] xnuhost: stopping: stand-in log ends", clock + 2.0));
    std::fs::write(out, lines.join("\n") + "\n").with_context(|| out.clone())?;
    println!("{out}: {stacks} thread lines and one crash backtrace, slide {slide:#x}");
    Ok(0)
}

pub fn cases(work: &str, logs: &[String]) -> Result<i32> {
    let thread = Regex::new(r"^\[[^\]]*\] guest\s+oneios-ubt: \S+\[\d+\] thread 0x[0-9a-f]+ state 0x[0-9a-f]+:((?: 0x[0-9a-f]+)+)$").unwrap();
    let frame = Regex::new(r"sheng-ubt sheng-exc: #\d+ fp=0x[0-9a-f]+ ret=(0x[0-9a-f]+)$").unwrap();
    let first_lr = Regex::new(r"sheng-ubt sheng-exc: pc=0x[0-9a-f]+ lr=(0x[0-9a-f]+) ").unwrap();
    let write = |name: &str, args: &[String]| -> Result<()> {
        let text: String = args.iter().map(|a| format!("{a}\n")).collect();
        std::fs::write(format!("{work}/{name}.args"), text).with_context(|| format!("{work}/{name}.args"))
    };
    let (mut whole, mut every, mut crashes): (Vec<Vec<String>>, Vec<String>, Vec<Vec<String>>) = (Vec::new(), Vec::new(), Vec::new());
    for path in logs {
        let bytes = std::fs::read(path).with_context(|| path.clone())?;
        // text mode: invalid UTF-8 replaced, universal newlines
        let text = String::from_utf8_lossy(&bytes).replace("\r\n", "\n").replace('\r', "\n");
        let (mut stacks, mut crash, mut got): (Vec<Vec<String>>, Option<Vec<String>>, bool) = (Vec::new(), None, false);
        for line in text.split_inclusive('\n') {
            let line = line.strip_suffix('\n').unwrap_or(line);
            if let Some(m) = thread.captures(line) {
                stacks.push(m[1].split_whitespace().map(str::to_string).collect());
                continue;
            }
            if let Some(m) = first_lr.captures(line) {
                crash = Some(vec![m[1].to_string()]);
                continue;
            }
            let m = frame.captures(line);
            match (&m, &mut crash) {
                (Some(m), Some(c)) => c.push(m[1].to_string()),
                (_, Some(c)) if c.len() >= 4 && !got => {
                    crashes.push(std::mem::take(c));
                    crash = None;
                    got = true;
                }
                _ => {}
            }
        }
        whole.extend(stacks.iter().filter(|s| s.len() >= 12).take(2).cloned());
        if every.is_empty() && stacks.len() > 300 {
            every = stacks.concat();
        }
    }
    if whole.is_empty() && crashes.is_empty() {
        bail!("no oneios-ubt thread line of 12 addresses and no sheng-ubt backtrace in the logs");
    }
    let mut names = Vec::new();
    for (i, s) in whole.iter().take(4).chain(crashes.iter().take(2)).enumerate() {
        let name = format!("a{}", i + 1);
        write(&name, s)?;
        names.push(name);
    }
    let first = whole.first().or(crashes.first()).unwrap();
    let owned = |v: &[&str]| v.iter().map(|s| s.to_string()).collect::<Vec<_>>();
    write("c1", &owned(&["0x1007c9084", "0x16f1648f8", "0x0", "0x7fffffffffff", "0xffffffffffffffff"]))?;
    write("c2", &owned(&["--slide", "0xedb4000", "0x1007c9084", "0x16f1648f8", "0x0", "0x7fffffffffff", "0x180000000"]))?;
    write("d1", &first[1.min(first.len())..3.min(first.len())])?;
    write("d2", &first[..1])?;
    if !every.is_empty() {
        write("e-large", &every[..every.len().min(4000)])?; // --slide is added by the script
    }
    std::fs::write(format!("{work}/cases"), names.join(" ") + "\n").with_context(|| format!("{work}/cases"))?;
    Ok(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn int_with_base_0() {
        assert_eq!(int0("0x124d8000").unwrap(), 0x124d8000);
        assert_eq!(int0("350").unwrap(), 350);
        assert!(int0("zz").is_err());
    }

    #[test]
    fn the_sets_of_a_made_log() {
        let dir = std::env::temp_dir().join(format!("oneios-symcases-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let mut log = String::from("[    0.000001] xnuhost: start\n");
        for n in 0..3 {
            let frames: String = (0..12).map(|i| format!(" 0x{:x}", 0x1000 + 16 * i + n)).collect();
            log += &format!("[    1.{n:06}] guest  oneios-ubt: launchd[30] thread 0x400 state 0x5:{frames}\n");
        }
        log += "[    2.000000] guest  sheng-ubt sheng-exc: pc=0x10 lr=0x20 sp=0x30\n";
        for i in 0..4 {
            log += &format!("[    2.000000] guest  sheng-ubt sheng-exc: #{i} fp=0x16f164900 ret=0x{:x}\n", 0x40 + i);
        }
        log += "[    3.000000] xnuhost: stopping\n";
        let path = dir.join("a.log");
        std::fs::write(&path, log).unwrap();
        let work = dir.to_str().unwrap();
        assert_eq!(cases(work, &[path.to_str().unwrap().to_string()]).unwrap(), 0);
        assert_eq!(std::fs::read_to_string(dir.join("cases")).unwrap(), "a1 a2 a3\n");
        assert_eq!(std::fs::read_to_string(dir.join("a3.args")).unwrap(), "0x20\n0x40\n0x41\n0x42\n0x43\n");
        assert_eq!(std::fs::read_to_string(dir.join("d2.args")).unwrap(), "0x1000\n");
        assert!(!dir.join("e-large.args").exists());
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
