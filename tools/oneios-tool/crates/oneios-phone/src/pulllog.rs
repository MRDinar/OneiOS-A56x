//! `oneios-tool phone pull-log OUT_DIR`: iOS's own log of one boot, without copying the Data store (the owner,
//! 2026-11-22: "the data.img keeps getting pulled for logs which seems unrequired"). `pull-data` moves the whole store
//! (a 32 GiB sparse file, 1.7-2.8 GB written) to read a few hundred MB of log files. Here the runtime's own oneios-tool
//! (`STAGE/runtime/bin/oneios-tool`, aarch64 Android) reads the staged store on the phone (`ioslog show`, our APFS
//! reader: no mount), writes the boot's entries as text, busybox gzips it, and only that stream crosses adb. Then the
//! text gets `ioslog health`, `ioslog keyboard` and `ioslog haptics` here, beside it.
//!
//! Works beside an open session: the store is then being written, and `ioslog` reads its newest checkpoint that
//! passes the reader's checks (it says so); what iOS logged in the last seconds may not be in it yet.

use crate::adb::{plain, Adb, BUSYBOX, STAGE};
use crate::pull::ioslog_health;
use anyhow::{bail, Context, Result};
use std::io::Write;
use std::path::Path;
use std::time::Instant;

/// The text file, the keyboard and haptics reports, beside the health report (`pull::HEALTH_FILE`).
pub const TEXT_FILE: &str = "ioslog.txt";
pub const KEYBOARD_FILE: &str = "ioslog-keyboard.txt";
pub const HAPTICS_FILE: &str = "ioslog-haptics.txt";

/// The phone's side: refuse without a runtime tool or a store, write the boot's text, gzip it, print its size.
/// `tmp` is the text file's path on the phone (the `.gz` beside it is what is pulled).
pub fn phone_script(tool: &str, store: &str, tmp: &str, boot: &str) -> String {
    format!(
        "set -e\n\
         [ -x {tool} ] || {{ echo 'no {tool}: stage a runtime first (oneios-tool phone stage runtime DIR)'; exit 3; }}\n\
         [ -f {store} ] || {{ echo 'no {store}: no session has made a Data store yet'; exit 3; }}\n\
         mkdir -p {dir}\n\
         rm -f {tmp} {tmp}.gz\n\
         {tool} ioslog show {store} --boot {boot} -o {tmp}\n\
         {BUSYBOX} gzip -1 {tmp}\n\
         {BUSYBOX} stat -c %s {tmp}.gz\n",
        dir = Path::new(tmp).parent().map(|p| p.display().to_string()).unwrap_or_else(|| ".".into()),
    )
}

pub fn pull_log(adb: &Adb, out_dir: &str, boot: &str, keep: bool) -> Result<()> {
    adb.need_root()?;
    plain(boot, "--boot")?;
    let open = adb.session_open().unwrap_or(false);
    let tool = format!("{STAGE}/runtime/bin/oneios-tool");
    let store = format!("{STAGE}/data/data.img");
    let tmp = format!("{STAGE}/logs/pull-log.txt");
    std::fs::create_dir_all(out_dir).with_context(|| out_dir.to_string())?;
    if open {
        eprintln!("pull-log: a session is open: the store is being written; the newest checkpoint that passes the checks is read");
    }
    let started = Instant::now();
    let said = adb.root_ok(&phone_script(&tool, &store, &tmp, boot), "ioslog show on the phone")?;
    let gz: u64 = said.lines().last().unwrap_or_default().trim().parse().unwrap_or(0);
    println!("pull-log: the phone wrote the boot's log as text and gzipped it: {gz} bytes ({:.1} s)", started.elapsed().as_secs_f64());

    // Only the gzip stream crosses adb; it is unpacked as it comes.
    let text = Path::new(out_dir).join(TEXT_FILE);
    let pulled = Instant::now();
    let mut child = adb.exec_out(&format!("su -c 'cat {tmp}.gz'"))?;
    let from = child.stdout.take().expect("piped");
    let mut file = std::fs::File::create(&text).with_context(|| text.display().to_string())?;
    let copied = std::io::copy(&mut flate2::read::MultiGzDecoder::new(from), &mut file);
    let status = child.wait().context("waiting for adb")?;
    let bytes = copied.with_context(|| format!("unpacking {tmp}.gz from the phone"))?;
    file.flush()?;
    if !status.success() {
        bail!("adb exec-out cat {tmp}.gz ended with {status}");
    }
    if !keep {
        let _ = adb.root(&format!("rm -f {tmp} {tmp}.gz\n"));
    }
    println!(
        "pull-log: {} ({bytes} bytes of text from {gz} over adb, {:.1} s){}",
        text.display(),
        pulled.elapsed().as_secs_f64(),
        if keep { format!("; kept on the phone: {tmp}.gz") } else { String::new() }
    );

    let source = text.display().to_string();
    ioslog_health(&source, Path::new(out_dir))?;
    for (name, table, file) in [("keyboard", oneios_ioslog::keyboard::KEYBOARD, KEYBOARD_FILE), ("haptics", oneios_ioslog::haptics::HAPTICS, HAPTICS_FILE)] {
        let report =
            oneios_ioslog::health::read_with(table, &source, &Default::default(), "last", 3).map_err(|e| anyhow::anyhow!("ioslog {name}: {e:#}"))?;
        let path = Path::new(out_dir).join(file);
        std::fs::write(&path, report.text(oneios_ioslog::health::TOP, true)).with_context(|| path.display().to_string())?;
        println!("--- ioslog {name} {source}");
        print!("{}", report.text(oneios_ioslog::health::TOP, false));
        println!("{}: these lines, and under each kind its texts, every process and example lines", path.display());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_phone_script() {
        let s = phone_script("/s/runtime/bin/oneios-tool", "/s/data/data.img", "/s/logs/pull-log.txt", "last");
        assert!(s.starts_with("set -e\n"));
        assert!(s.contains("[ -x /s/runtime/bin/oneios-tool ] ||"));
        assert!(s.contains("[ -f /s/data/data.img ] ||"));
        assert!(s.contains("mkdir -p /s/logs\n"));
        assert!(s.contains("/s/runtime/bin/oneios-tool ioslog show /s/data/data.img --boot last -o /s/logs/pull-log.txt\n"));
        assert!(s.contains(&format!("{BUSYBOX} gzip -1 /s/logs/pull-log.txt\n")));
        assert!(s.ends_with(&format!("{BUSYBOX} stat -c %s /s/logs/pull-log.txt.gz\n")));
    }
}
