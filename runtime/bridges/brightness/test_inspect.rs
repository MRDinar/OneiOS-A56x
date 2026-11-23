// SPDX-License-Identifier: MIT
//! Passive backlight inspection regression (test_inspect.sh), no phone and no root: `brightctl inspect` on a fake
//! sysfs picks the node, warns at a zero brightness, refuses all-zero maxima, and changes no file.

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::process::Command;

fn inspect(dir: &Path) -> (bool, String) {
    let out =
        Command::new(env!("CARGO_BIN_EXE_brightctl")).arg("inspect").env("ONEIOS_BRIGHT_DIR", dir).output().unwrap();
    (out.status.success(), String::from_utf8_lossy(&out.stdout).into_owned())
}

fn put(dir: &Path, rel: &str, text: &str) {
    let path = dir.join(rel);
    if path.exists() {
        fs::set_permissions(&path, fs::Permissions::from_mode(0o644)).unwrap();
    }
    fs::write(path, text).unwrap();
}

fn snapshot(dir: &Path) -> Vec<(String, Vec<u8>)> {
    let mut all = Vec::new();
    for node in ["panel", "panel0-backlight"] {
        for e in fs::read_dir(dir.join(node)).unwrap().flatten() {
            all.push((e.path().display().to_string(), fs::read(e.path()).unwrap()));
        }
    }
    all.sort();
    all
}

#[test]
fn inspect_is_read_only() {
    let dir = std::env::temp_dir().join(format!("oneios-inspect-{}", std::process::id()));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(dir.join("panel")).unwrap();
    fs::create_dir_all(dir.join("panel0-backlight")).unwrap();
    for (rel, text) in [
        ("panel/max_brightness", "0\n"),
        ("panel/brightness", "0\n"),
        ("panel/actual_brightness", "0\n"),
        ("panel0-backlight/max_brightness", "12750\n"),
        ("panel0-backlight/brightness", "0\n"),
        ("panel0-backlight/actual_brightness", "0\n"),
    ] {
        put(&dir, rel, text);
        fs::set_permissions(dir.join(rel), fs::Permissions::from_mode(0o444)).unwrap();
    }
    let before = snapshot(&dir);
    let (ok, out) = inspect(&dir);
    assert_eq!(before, snapshot(&dir), "inspect modified the fake sysfs files");
    assert!(ok);
    assert!(out.contains("node panel: max 0; brightness 0; actual 0; bl_power unavailable; skipped (max=0)"), "{out}");
    assert!(out.contains("node panel0-backlight: max 12750; brightness 0; actual 0; bl_power unavailable; selected"));
    assert!(out.contains("WARNING: selected node brightness is 0"));
    assert!(out.contains("inspect ok (no state changes)"));

    // A nonzero native panel is chosen first, with no warning.
    put(&dir, "panel/max_brightness", "255\n");
    put(&dir, "panel/brightness", "80\n");
    let (_, out) = inspect(&dir);
    assert!(out.contains("node panel: max 255; brightness 80; actual 0; bl_power unavailable; selected"), "{out}");
    assert!(!out.contains("WARNING:"), "nonzero selected brightness unexpectedly warned");

    // A directory with only zero-max nodes must not claim success. Reset BOTH max values: earlier cases deliberately
    // left panel0-backlight usable.
    put(&dir, "panel/max_brightness", "0\n");
    put(&dir, "panel0-backlight/max_brightness", "0\n");
    let (ok, out) = inspect(&dir);
    assert!(!ok, "inspect accepted all-zero maxima");
    assert!(out.contains("no usable positive-max backlight node"));
    let _ = fs::remove_dir_all(&dir);
}
