// SPDX-License-Identifier: MIT
//! For the tests: the values of C expressions as the build host's kernel headers give them (a small program built with
//! `cc`), so the numbers mirrored from UAPI headers (binder, nl80211, netlink) are checked against the headers
//! themselves. None, with the reason printed, where the host has no C compiler or not the headers.

/// Each expression's value (as i64), the headers included in order.
pub fn values(test: &str, includes: &[&str], exprs: &[&str]) -> Option<Vec<i64>> {
    if let Some(missing) = includes.iter().find(|h| !std::path::Path::new("/usr/include").join(h).exists()) {
        println!("{test}: skipped (no /usr/include/{missing})");
        return None;
    }
    let dir = std::env::temp_dir().join(format!("onebridges-{test}-{}", std::process::id()));
    std::fs::create_dir_all(&dir).ok()?;
    let mut c = String::from("#include <stdio.h>\n");
    for h in includes {
        c.push_str(&format!("#include <{h}>\n"));
    }
    c.push_str("int main(void) {\n");
    for e in exprs {
        c.push_str(&format!("  printf(\"%lld\\n\", (long long)({e}));\n"));
    }
    c.push_str("  return 0;\n}\n");
    std::fs::write(dir.join("v.c"), c).ok()?;
    let built = std::process::Command::new("cc")
        .arg(dir.join("v.c"))
        .arg("-o")
        .arg(dir.join("v"))
        .status()
        .is_ok_and(|s| s.success());
    if !built {
        println!("{test}: skipped (no C compiler, or the headers do not build)");
        let _ = std::fs::remove_dir_all(&dir);
        return None;
    }
    let out = std::process::Command::new(dir.join("v")).output().ok()?;
    let _ = std::fs::remove_dir_all(&dir);
    Some(String::from_utf8_lossy(&out.stdout).lines().map(|l| l.trim().parse().unwrap_or(i64::MIN)).collect())
}

/// Asserts each (expression, ours) against the headers.
pub fn check(test: &str, includes: &[&str], pairs: &[(&str, i64)]) {
    let exprs: Vec<&str> = pairs.iter().map(|p| p.0).collect();
    if let Some(theirs) = values(test, includes, &exprs) {
        for ((name, ours), v) in pairs.iter().zip(theirs) {
            assert_eq!(*ours, v, "{name}");
        }
    }
}
