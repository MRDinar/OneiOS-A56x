//! `oneios-tool phone wifi p2p|wonder`: the staged runtime's `wifictl` on the phone, as root, beside One UI
//! (docs/hardware.md 4a, runtime/bridges/README.md).
//! - `p2p`: read only, safe beside One UI and in a session: every radio and interface, Google's Wonder radio (its
//!   vendor subcommands under 00:1a:11, who holds it), the verdicts, Wonder's module and files.
//! - `wonder`: a session on Wonder's raw 802.11 path beside wlan0 (`wifictl wonder`): a monitor interface on Wonder's
//!   radio, set, up (the driver's PASSTHRU vdev opens), frames counted for SECONDS (AWDL's by peer), all of it closed
//!   in reverse, wlan0's link checked against its link before. wifictl refuses it when Wonder is missing or in use.
//!   Its lines are printed as they come (`adb exec-out`); wifictl ignores SIGPIPE, so a dropped adb never stops its
//!   teardown, and the next `wonder` removes an interface a killed one left.
//!
//! Only the named binary runs: nothing else is read or written by this tool. The arguments are checked here before
//! they reach root's shell. `--save FILE` keeps what the phone printed.
use crate::adb::{Adb, STAGE};
use anyhow::{bail, Context, Result};
use std::io::{BufRead, BufReader, Write};

/// The staged runtime's tool (runtime/build.sh puts it there).
pub fn wifictl() -> String {
    format!("{STAGE}/runtime/bin/wifictl")
}

/// The AWDL channels `wifictl wonder` takes: 6, 44 and 149 (Apple's devices meet on 6 and 149).
pub const FREQS: [u32; 3] = [2437, 5220, 5745];

pub struct Wonder {
    pub seconds: u32,
    pub freq: u32,
    pub country: Option<String>,
    pub save: Option<String>,
}

/// The `wifictl wonder` command line, its arguments checked (they go into root's shell).
pub fn wonder_command(w: &Wonder) -> Result<String> {
    if !(1..=600).contains(&w.seconds) {
        bail!("--seconds {} is not 1 to 600", w.seconds);
    }
    if !FREQS.contains(&w.freq) {
        bail!("--freq {} is no AWDL channel: 2437 (channel 6), 5220 (44) or 5745 (149)", w.freq);
    }
    let mut cmd = format!("{} wonder --seconds {} --freq {}", wifictl(), w.seconds, w.freq);
    if let Some(cc) = &w.country {
        if cc.len() != 2 || !cc.bytes().all(|b| b.is_ascii_alphabetic()) {
            bail!("--country \"{cc}\" is not two letters (ISO 3166-1, the phone's own)");
        }
        cmd += &format!(" --country {}", cc.to_ascii_uppercase());
    }
    Ok(cmd)
}

/// The `wifictl p2p` command line, the station's name checked.
pub fn p2p_command(station: &str) -> Result<String> {
    if station.is_empty() || station.len() > 15 || !station.bytes().all(|b| b.is_ascii_alphanumeric() || b"_-.".contains(&b)) || station.starts_with('-') {
        bail!("--station \"{station}\" is no interface name (letters, digits, _ - . and at most 15)");
    }
    Ok(format!("{} p2p {station}", wifictl()))
}

/// wifictl's last line: "ok" or "failed" (or "unknown command": an older wifictl).
pub fn outcome(out: &str) -> &str {
    out.lines().rev().map(str::trim).find(|l| !l.is_empty()).unwrap_or("")
}

/// The lines a reader looks for first, from `wifictl p2p`'s report: the Wonder block, the verdicts, Wonder's files,
/// the interfaces that are not wlan0 and any network device or process with a hinting name.
pub fn p2p_summary(out: &str) -> Vec<&str> {
    out.lines()
        .filter(|l| {
            l.starts_with("p2p: Wonder:") || l.starts_with("p2p: for AirDrop") || l.starts_with("radios:")
                || (l.starts_with("wonder: ") && l.ends_with("present")) || l.contains(": present: ")
                || (l.starts_with("interface: ") && !l.starts_with("interface: wlan0 ")) || l.starts_with("process: ")
        })
        .collect()
}

fn need_wifictl(adb: &Adb) -> Result<()> {
    let w = wifictl();
    let r = adb.root(&format!("[ -x {w} ] && echo yes || echo no\n"))?;
    if r.out.trim() != "yes" {
        bail!("the staged runtime has no {w}: stage a runtime that has it (oneios-tool phone stage runtime-asis DIR). Nothing was changed");
    }
    Ok(())
}

fn save(path: &Option<String>, out: &str) -> Result<()> {
    if let Some(p) = path {
        std::fs::write(p, out).with_context(|| format!("cannot write {p}"))?;
        println!("saved: {p}");
    }
    Ok(())
}

/// `phone wifi p2p`.
pub fn p2p(adb: &Adb, station: &str, save_to: &Option<String>) -> Result<()> {
    let cmd = p2p_command(station)?;
    adb.need_root()?;
    need_wifictl(adb)?;
    let r = adb.root(&format!("{cmd}\n"))?;
    print!("{}", r.out);
    save(save_to, &r.out)?;
    let summary = p2p_summary(&r.out);
    if !summary.is_empty() {
        println!("---- summary");
        for line in summary {
            println!("{line}");
        }
    }
    if outcome(&r.out) != "ok" {
        bail!("wifictl p2p failed on the phone (status {}): {}", r.status, crate::adb::last_lines(&format!("{}{}", r.out, r.err), 3));
    }
    Ok(())
}

/// `phone wifi wonder`: wifictl's lines as they come.
pub fn wonder(adb: &Adb, w: &Wonder) -> Result<()> {
    let cmd = wonder_command(w)?;
    adb.need_root()?;
    need_wifictl(adb)?;
    if adb.session_open()? {
        println!("note: a session is open: Wonder's vdev shares the radio with the wlan0 iOS uses (wifictl checks wlan0's link after)");
    }
    println!("wonder: {} s on {} MHz, on the phone: {cmd}", w.seconds, w.freq);
    let mut child = adb.exec_out(&format!("su -c '{cmd}'"))?;
    let mut out = String::new();
    let reader = BufReader::new(child.stdout.take().context("no output from adb")?);
    let stdout = std::io::stdout();
    for line in reader.lines() {
        let line = line.context("reading adb")?.replace('\r', "");
        writeln!(stdout.lock(), "{line}").ok();
        out += &line;
        out.push('\n');
    }
    child.wait().context("waiting for adb")?;
    save(&w.save, &out)?;
    match outcome(&out) {
        "ok" => Ok(()),
        "" => bail!("wifictl wonder gave nothing (adb dropped?): if it was cut, the next `phone wifi wonder` removes what it left"),
        last => bail!("wifictl wonder ended \"{last}\": the lines above say which step"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_commands_and_their_checks() {
        let w = Wonder { seconds: 30, freq: 5745, country: Some("gb".into()), save: None };
        assert_eq!(wonder_command(&w).unwrap(), "/data/local/tmp/oneios/runtime/bin/wifictl wonder --seconds 30 --freq 5745 --country GB");
        assert_eq!(wonder_command(&Wonder { country: None, ..w }).unwrap(), "/data/local/tmp/oneios/runtime/bin/wifictl wonder --seconds 30 --freq 5745");
        for bad in [Wonder { seconds: 0, freq: 2437, country: None, save: None }, Wonder { seconds: 601, freq: 2437, country: None, save: None },
                    Wonder { seconds: 5, freq: 5180, country: None, save: None }, Wonder { seconds: 5, freq: 2437, country: Some("G;".into()), save: None },
                    Wonder { seconds: 5, freq: 2437, country: Some("GBR".into()), save: None }] {
            assert!(wonder_command(&bad).is_err());
        }
        assert_eq!(p2p_command("wlan0").unwrap(), "/data/local/tmp/oneios/runtime/bin/wifictl p2p wlan0");
        for bad in ["", "-x", "wlan0;id", "a b", "0123456789abcdef"] {
            assert!(p2p_command(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn the_outcome_and_the_summary() {
        assert_eq!(outcome("  wonder: done\nok\n\n"), "ok");
        assert_eq!(outcome("failed\n"), "failed");
        assert_eq!(outcome(""), "");
        let report = "radios: 2\nradio: phy0 (wiphy 0)\ninterface: wlan0 (index 44) station, wiphy 0\ninterface: wondertap0 (index 51) monitor, wiphy 0\n\
p2p: Wonder: wiphy 1 (wonder), interface types monitor\np2p: monitor mode: yes\np2p: for AirDrop iOS needs AWDL: Wonder's raw 802.11 path\n\
wonder: /sys/module/wonder: present\nwonder: /system_ext/bin/mosey_server: none\nwonder: /sys/module/wonder/parameters/physical_name: present: wondertap0\n\
process: 812 /system_ext/bin/mosey_server\nok\n";
        let s = p2p_summary(report);
        assert_eq!(s, vec!["radios: 2", "interface: wondertap0 (index 51) monitor, wiphy 0", "p2p: Wonder: wiphy 1 (wonder), interface types monitor",
                           "p2p: for AirDrop iOS needs AWDL: Wonder's raw 802.11 path", "wonder: /sys/module/wonder: present",
                           "wonder: /sys/module/wonder/parameters/physical_name: present: wondertap0", "process: 812 /system_ext/bin/mosey_server"]);
    }
}
