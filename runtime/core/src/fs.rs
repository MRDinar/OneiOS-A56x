// SPDX-License-Identifier: MIT
//! Small named files of sysfs and /proc: what the C runtime read and wrote with a dozen `read_line`, `read_text`,
//! `read_int`, `write_text` and `xh_proc_values` of its own (bridges' cpu, battery, thermal, brightness, wlan;
//! xnuhost's config, ramfix, usb, main). One read of at most [`SMALL`] bytes; nothing here walks a folder.
use crate::parse;
use std::fs::File;
use std::io::{self, Read, Write};
use std::os::unix::fs::OpenOptionsExt;
use std::path::Path;

/// The most a small file read takes (a sysfs value is one page at most).
pub const SMALL: usize = 4096;

/// Up to `max` bytes of the file at `path` (reads until the end or `max`).
pub fn read_bytes(path: impl AsRef<Path>, max: usize) -> io::Result<Vec<u8>> {
    let mut out = Vec::new();
    File::open(path)?.take(max as u64).read_to_end(&mut out)?;
    Ok(out)
}

/// The text of a small file (up to [`SMALL`] bytes), invalid UTF-8 replaced; `None` when it cannot be read.
pub fn read_raw(path: impl AsRef<Path>) -> Option<String> {
    read_bytes(path, SMALL).ok().map(|b| String::from_utf8_lossy(&b).into_owned())
}

/// A sysfs value: the file's text without the white space at its end (`\n`, spaces, `\r`, tabs); `None` when it
/// cannot be read or is empty.
pub fn read_text(path: impl AsRef<Path>) -> Option<String> {
    let mut t = read_raw(path)?;
    t.truncate(t.trim_end().len());
    (!t.is_empty()).then_some(t)
}

/// The first line of a small file without its line break (`\r` or `\n`); `None` when it cannot be read.
pub fn read_line(path: impl AsRef<Path>) -> Option<String> {
    let t = read_raw(path)?;
    let end = t.find(['\r', '\n']).unwrap_or(t.len());
    Some(t[..end].to_owned())
}

/// The value of the first line that starts with `key` (main.c's `read_line(path, key)`): what follows the key,
/// spaces and tabs before it skipped, without the line break. "Cpus_allowed_list:" of /proc/self/status (the
/// `cpu:` line of xnuhost's session).
pub fn key_line(path: impl AsRef<Path>, key: &str) -> Option<String> {
    let t = read_bytes(path, 1 << 16).ok()?;
    let t = String::from_utf8_lossy(&t);
    t.lines().find_map(|l| l.strip_prefix(key)).map(|v| v.trim_start_matches([' ', '\t']).to_owned())
}

/// A sysfs number: the file's text as a whole signed number (`radix` 0: C's 0x and 0 prefixes); `None` when it cannot
/// be read or is not a number.
pub fn read_i64(path: impl AsRef<Path>, radix: u32) -> Option<i64> {
    parse::i64_text(&read_text(path)?, radix)
}

/// A sysfs number, unsigned (`radix` 0: C's prefixes).
pub fn read_u64(path: impl AsRef<Path>, radix: u32) -> Option<u64> {
    parse::u64_text(&read_text(path)?, radix)
}

/// Writes `text` to an existing file (a sysfs or /proc node: no create, no truncate) in one write.
pub fn write_text(path: impl AsRef<Path>, text: &str) -> io::Result<()> {
    let mut f = std::fs::OpenOptions::new().write(true).custom_flags(libc::O_CLOEXEC).open(path)?;
    f.write_all(text.as_bytes())
}

/// The value of `key` in a /proc file of `Key: value [kB]` lines (one line of [`proc_values`]).
fn key_value(line: &str, key: &str) -> Option<u64> {
    let rest = line.strip_prefix(key)?;
    let rest = rest.strip_prefix(':').or_else(|| rest.strip_prefix(' '))?;
    let (v, after) = parse::leading_u64(rest, 10).unwrap_or((0, rest));
    Some(if after.trim_start_matches(' ').starts_with("kB") { v << 10 } else { v })
}

/// Values of /proc/meminfo-style lines (`xh_proc_values`): for each key, the number after `KEY:` (or `KEY `), in
/// bytes when the line says kB; `None` for a key not found (or the whole file unreadable).
pub fn proc_values(path: impl AsRef<Path>, keys: &[&str]) -> Vec<Option<u64>> {
    let mut out = vec![None; keys.len()];
    let Ok(bytes) = read_bytes(path, 1 << 20) else { return out };
    let text = String::from_utf8_lossy(&bytes);
    proc_values_in(&text, keys, &mut out);
    out
}

/// [`proc_values`] of text already read.
pub fn proc_values_in(text: &str, keys: &[&str], out: &mut [Option<u64>]) {
    let mut left = keys.len();
    for line in text.lines() {
        if left == 0 {
            break;
        }
        for (k, key) in keys.iter().enumerate() {
            if out[k].is_none() {
                if let Some(v) = key_value(line, key) {
                    out[k] = Some(v);
                    left -= 1;
                    break;
                }
            }
        }
    }
}

/// One /proc/meminfo value in bytes (`xh_meminfo_value`): the `KEY: N kB` line's N KiB; `None` when the key is not
/// there with kB.
pub fn meminfo_value(path: impl AsRef<Path>, key: &str) -> Option<u64> {
    let text = read_raw_large(path)?;
    text.lines().find_map(|l| {
        let rest = l.strip_prefix(key)?.strip_prefix(':')?;
        let (v, after) = parse::leading_u64(rest, 10)?;
        after.trim_start().starts_with("kB").then_some(v << 10)
    })
}

fn read_raw_large(path: impl AsRef<Path>) -> Option<String> {
    read_bytes(path, 1 << 20).ok().map(|b| String::from_utf8_lossy(&b).into_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp(name: &str, text: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!("xh-core-fs-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let p = dir.join(name);
        std::fs::write(&p, text).unwrap();
        p
    }

    #[test]
    fn small_files() {
        let p = tmp("value", "1440 \n");
        assert_eq!(read_text(&p).as_deref(), Some("1440"));
        assert_eq!(read_i64(&p, 10), Some(1440));
        assert_eq!(read_u64(&p, 0), Some(1440));
        assert_eq!(read_line(tmp("lines", "first\r\nsecond")).as_deref(), Some("first"));
        assert_eq!(read_text(tmp("empty", "\n")), None);
        assert_eq!(read_i64(tmp("word", "on\n"), 10), None);
        assert_eq!(read_text("/nonexistent/xh-core"), None);
        let st = tmp("status", "Name:\txnuhost\nCpus_allowed_list:\t0-7\n");
        assert_eq!(key_line(&st, "Cpus_allowed_list:").as_deref(), Some("0-7"));
        assert_eq!(key_line(&st, "Missing:"), None);
        let node = tmp("node", "0\n"); // a sysfs node: written in place, not truncated
        write_text(&node, "7").unwrap();
        assert_eq!(read_text(&node).as_deref(), Some("7"));
        assert!(write_text("/nonexistent/xh-core", "1").is_err());
    }

    #[test]
    fn proc_lines() {
        let m = tmp(
            "meminfo",
            "MemTotal:       11522148 kB\nMemAvailable:    4000000 kB\nHugePages_Free:        3\nHugepagesize:       2048 kB\n",
        );
        let v = proc_values(&m, &["MemTotal", "HugePages_Free", "Hugepagesize", "SwapTotal"]);
        assert_eq!(v, [Some(11522148 << 10), Some(3), Some(2048 << 10), None]);
        assert_eq!(meminfo_value(&m, "MemAvailable"), Some(4000000 << 10));
        assert_eq!(meminfo_value(&m, "HugePages_Free"), None); // no kB
        assert_eq!(meminfo_value(&m, "Mem"), None);
        let s = tmp("vmstat", "pswpin 12\npswpout 30\n");
        assert_eq!(proc_values(&s, &["pswpout", "pswpin"]), [Some(30), Some(12)]);
        assert_eq!(proc_values("/nonexistent", &["x"]), [None]);
    }
}
