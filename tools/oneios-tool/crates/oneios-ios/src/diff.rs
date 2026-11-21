//! `ios tbd-diff A B`: two trees of stubs compared by what the linker and macho-check read from them: per file, the
//! documents' install names, re-exported libraries and exported names (the Objective-C lists expanded to the
//! symbols they stand for). A lasting check of `ios tbds` (README "ios": how 24A446's stubs were compared with
//! those `ipsw dyld tbd` wrote).

use crate::tbd::{self, Y};
use anyhow::{Context, Result};
use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

fn files(root: &Path, dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(rd) = std::fs::read_dir(dir) else { return };
    for e in rd.flatten() {
        let p = e.path();
        match e.file_type() {
            Ok(t) if t.is_dir() => files(root, &p, out),
            Ok(_) if p.extension().is_some_and(|x| x == "tbd") => out.push(p.strip_prefix(root).unwrap_or(&p).to_path_buf()),
            _ => {}
        }
    }
}

/// (install name, re-exports, names) per document. `listed`: only the names a document lists as symbols (its
/// `symbols`, `weak-symbols`, `thread-local-symbols`), not those its Objective-C lists stand for.
fn read(path: &Path, target: &str, listed: bool) -> Result<Vec<(String, Vec<String>, BTreeSet<String>)>> {
    let text = std::fs::read_to_string(path).with_context(|| path.display().to_string())?;
    if !listed {
        return Ok(tbd::exports(&text, target)?.into_iter().map(|d| (d.install_name, d.reexports, d.symbols)).collect());
    }
    let mut out = Vec::new();
    for (d, e) in tbd::parse(&text)?.into_iter().zip(tbd::exports(&text, target)?) {
        let mut names = BTreeSet::new();
        for x in d.get("exports").map(Y::items).unwrap_or(&[]) {
            for k in ["symbols", "weak-symbols", "thread-local-symbols"] {
                names.extend(x.get(k).map(Y::strings).unwrap_or_default());
            }
        }
        out.push((e.install_name, e.reexports, names));
    }
    Ok(out)
}

pub fn run(a: &Path, b: &Path, a_listed: bool, show: usize) -> Result<i32> {
    let target = crate::tbds::TARGET;
    let (mut fa, mut fb) = (Vec::new(), Vec::new());
    files(a, a, &mut fa);
    files(b, b, &mut fb);
    let (sa, sb): (BTreeSet<_>, BTreeSet<_>) = (fa.iter().cloned().collect(), fb.iter().cloned().collect());
    let (mut same, mut differ, mut only_a, mut only_b) = (0usize, 0usize, 0usize, 0usize);
    let mut shown = 0;
    for p in sa.difference(&sb) {
        only_a += 1;
        if shown < show {
            println!("only in A: {}", p.display());
            shown += 1;
        }
    }
    for p in sb.difference(&sa) {
        only_b += 1;
        if shown < show {
            println!("only in B: {}", p.display());
            shown += 1;
        }
    }
    let (mut names_a, mut names_b) = (0usize, 0usize);
    for p in sa.intersection(&sb) {
        let da = read(&a.join(p), target, a_listed)?;
        let db = read(&b.join(p), target, false)?;
        let mut why = Vec::new();
        if da.len() != db.len() {
            why.push(format!("{} documents, {}", da.len(), db.len()));
        }
        for (x, y) in da.iter().zip(&db) {
            if x.0 != y.0 {
                why.push(format!("install name {} / {}", x.0, y.0));
            }
            if x.1 != y.1 {
                why.push(format!("{}: re-exports {:?} / {:?}", x.0, x.1, y.1));
            }
            let (oa, ob): (Vec<_>, Vec<_>) = (x.2.difference(&y.2).collect(), y.2.difference(&x.2).collect());
            names_a += oa.len();
            names_b += ob.len();
            if !oa.is_empty() || !ob.is_empty() {
                why.push(format!("{}: {} names only in A (first {:?}), {} only in B (first {:?})", x.0, oa.len(), oa.first(), ob.len(), ob.first()));
            }
        }
        if why.is_empty() {
            same += 1;
        } else {
            differ += 1;
            if shown < show {
                println!("{}: {}", p.display(), why.join("; "));
                shown += 1;
            }
        }
    }
    println!("tbd-diff: {same} files the same, {differ} differ, {only_a} only in A, {only_b} only in B ({names_a} names only in A, {names_b} only in B)");
    Ok(if differ + only_a + only_b == 0 { 0 } else { 1 })
}
