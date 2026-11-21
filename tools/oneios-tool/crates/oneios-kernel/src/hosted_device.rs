//! `oneios-tool kernel hosted-series BOARD SERIES` and `kernel hosted-archives BOARD DEFCONFIG ENTRY...`: the two
//! edits kernel/build-hosted.sh makes to its copy of the device repository (out/hosted-device*), never to
//! kernel/device itself. They were inline Python blocks of build-hosted.sh until 2026-11-21 (Python is retired from
//! this repository); the edited text is the same byte for byte (tools/oneios-tool/README.md "kernel").
//!
//!   hosted-series    board.toml's [kernel] patch_series gets SERIES (the hosted series) as its last entry.
//!   hosted-archives  board.toml's [kernel] archives: the committed list ends with the board's own entry
//!                    (`"oneios-tool kernel gen board",`); the ENTRYs take its place, in their order. The defconfig's
//!                    CONFIG_XNU_EXTRA_LD_ARCHIVES gets the force-loads of the hosted archives (gpu, aes, keystore,
//!                    net, hid, audio: the link order) and CONFIG_XNU_NATIVE_LEGACY_DISPLAY=y is added at the end.
//!
//! A file whose text is not what the edit expects is refused with build-hosted's message, and nothing is written.
//! The files are read as Python's text mode read them (universal newlines: "\r\n" and "\r" become "\n").

use std::path::Path;

use anyhow::{bail, Context, Result};
use regex::{Captures, Regex};

/// The archives the hosted kernel force-loads beside the committed ones, in this order (the link order).
pub const FORCE_LOADED: [&str; 6] = ["gpu", "aes", "keystore", "net", "hid", "audio"];

fn read(path: &Path) -> Result<String> {
    let text = std::fs::read_to_string(path).with_context(|| path.display().to_string())?;
    Ok(text.replace("\r\n", "\n").replace('\r', "\n"))
}

fn write(path: &Path, text: &str) -> Result<()> {
    std::fs::write(path, text).with_context(|| path.display().to_string())
}

/// board.toml with SERIES appended to its (first) patch_series list.
pub fn add_series(board: &str, series: &str) -> Result<String> {
    let re = Regex::new(r"(patch_series\s*=\s*\[[^\]]*?)\]").unwrap();
    let Some(m) = re.captures(board) else { bail!("build-hosted: board.toml has no [kernel] patch_series") };
    let whole = m.get(0).unwrap();
    let head = m[1].trim_end().trim_end_matches(',');
    Ok(format!("{}{head}, \"{series}\"]{}", &board[..whole.start()], &board[whole.end()..]))
}

/// board.toml with the board's own archive entry (the last of the committed list) replaced by ENTRIES.
pub fn replace_archives(board: &str, entries: &[String]) -> Result<String> {
    let re = Regex::new(r#"(?m)^(\s*)"oneios-tool kernel gen board",?\s*\n(\s*\])"#).unwrap();
    if !re.is_match(board) {
        bail!("build-hosted: board.toml archives do not end with the board's own entry");
    }
    Ok(re
        .replacen(board, 1, |m: &Captures| {
            let mut out: String = entries.iter().map(|entry| format!("{}\"{entry}\",\n", &m[1])).collect();
            out.push_str(&m[2]);
            out
        })
        .into_owned())
}

/// The hosted defconfig with the force-loads and the legacy display.
pub fn defconfig(config: &str) -> Result<String> {
    let re = Regex::new(r#"(?m)^(CONFIG_XNU_EXTRA_LD_ARCHIVES="[^"\n]*)""#).unwrap();
    let mut text = config.to_string();
    for name in FORCE_LOADED {
        if !re.is_match(&text) {
            bail!("build-hosted: the hosted defconfig has no CONFIG_XNU_EXTRA_LD_ARCHIVES");
        }
        text = re
            .replacen(&text, 1, |m: &Captures| format!("{} -Wl,-force_load,{{tree}}/build/oneios-{name}/liboneios{name}.a\"", &m[1]))
            .into_owned();
    }
    if text.contains("CONFIG_XNU_NATIVE_LEGACY_DISPLAY") {
        bail!("build-hosted: the hosted defconfig already sets CONFIG_XNU_NATIVE_LEGACY_DISPLAY");
    }
    Ok(format!("{}\nCONFIG_XNU_NATIVE_LEGACY_DISPLAY=y\n", text.trim_end_matches('\n')))
}

pub fn series_main(board: &Path, series: &str) -> Result<()> {
    let text = add_series(&read(board)?, series)?;
    write(board, &text)
}

pub fn archives_main(board: &Path, config: &Path, entries: &[String]) -> Result<()> {
    // Both edits are made before either file is written: a refused defconfig leaves board.toml as it was.
    let board_text = replace_archives(&read(board)?, entries)?;
    let config_text = defconfig(&read(config)?)?;
    write(board, &board_text)?;
    write(config, &config_text)
}

#[cfg(test)]
mod tests {
    use super::*;

    const BOARD: &str = "[board]\nname = \"x\"\n\n[kernel]\npatch_series = [\n  \"patches/xnu-12377/series\",\n]\narchives = [\n  \"build-corecrypto-14.py\",\n  \"oneios-tool kernel gen board\",\n]\n";

    #[test]
    fn series_is_appended_to_the_list() {
        let out = add_series(BOARD, "/r/kernel/patches/oneios-hosted/series").unwrap();
        assert_eq!(
            out,
            "[board]\nname = \"x\"\n\n[kernel]\npatch_series = [\n  \"patches/xnu-12377/series\", \"/r/kernel/patches/oneios-hosted/series\"]\narchives = [\n  \"build-corecrypto-14.py\",\n  \"oneios-tool kernel gen board\",\n]\n"
        );
        // one line, no trailing comma; only the first list
        assert_eq!(add_series("patch_series = [\"a\"]\npatch_series = []\n", "s").unwrap(), "patch_series = [\"a\", \"s\"]\npatch_series = []\n");
        assert_eq!(add_series("patch_series=[]", "s").unwrap(), "patch_series=[, \"s\"]");
        assert_eq!(add_series("archives = []\n", "s").unwrap_err().to_string(), "build-hosted: board.toml has no [kernel] patch_series");
    }

    #[test]
    fn archives_take_the_board_entry_s_place() {
        let entries: Vec<String> = ["storage", "board", "display"].iter().map(|n| format!("oneios-tool kernel gen {n}")).collect();
        let out = replace_archives(BOARD, &entries).unwrap();
        assert!(out.ends_with(
            "archives = [\n  \"build-corecrypto-14.py\",\n  \"oneios-tool kernel gen storage\",\n  \"oneios-tool kernel gen board\",\n  \"oneios-tool kernel gen display\",\n]\n"
        ), "{out}");
        // without the trailing comma, and with a blank line before the ] (the match takes it, as Python's did)
        let out = replace_archives("archives = [\n    \"oneios-tool kernel gen board\"\n\n  ]\n", &entries[..1]).unwrap();
        assert_eq!(out, "archives = [\n    \"oneios-tool kernel gen storage\",\n  ]\n");
        let refused = "build-hosted: board.toml archives do not end with the board's own entry";
        assert_eq!(replace_archives("archives = [\n  \"oneios-tool kernel gen board\", \"x\",\n]\n", &entries).unwrap_err().to_string(), refused);
        assert_eq!(replace_archives("archives = []\n", &entries).unwrap_err().to_string(), refused);
    }

    #[test]
    fn defconfig_gets_the_force_loads_and_the_display() {
        let out = defconfig("CONFIG_A=y\nCONFIG_XNU_EXTRA_LD_ARCHIVES=\"-Wl,-force_load,{tree}/build/x.a\"\nCONFIG_B=y\n\n\n").unwrap();
        let loads: String = FORCE_LOADED.iter().map(|n| format!(" -Wl,-force_load,{{tree}}/build/oneios-{n}/liboneios{n}.a")).collect();
        assert_eq!(out, format!("CONFIG_A=y\nCONFIG_XNU_EXTRA_LD_ARCHIVES=\"-Wl,-force_load,{{tree}}/build/x.a{loads}\"\nCONFIG_B=y\nCONFIG_XNU_NATIVE_LEGACY_DISPLAY=y\n"));
        assert!(loads.starts_with(" -Wl,-force_load,{tree}/build/oneios-gpu/liboneiosgpu.a -Wl,-force_load,{tree}/build/oneios-aes/"));
        assert_eq!(defconfig("CONFIG_A=y\n").unwrap_err().to_string(), "build-hosted: the hosted defconfig has no CONFIG_XNU_EXTRA_LD_ARCHIVES");
        assert_eq!(
            defconfig("CONFIG_XNU_EXTRA_LD_ARCHIVES=\"\"\n# CONFIG_XNU_NATIVE_LEGACY_DISPLAY is not set\n").unwrap_err().to_string(),
            "build-hosted: the hosted defconfig already sets CONFIG_XNU_NATIVE_LEGACY_DISPLAY"
        );
    }

    #[test]
    fn a_refused_defconfig_writes_nothing() {
        let dir = std::env::temp_dir().join(format!("oneios-hosted-device-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let (board, config) = (dir.join("board.toml"), dir.join("defconfig"));
        std::fs::write(&board, BOARD.replace('\n', "\r\n")).unwrap();
        std::fs::write(&config, "CONFIG_A=y\n").unwrap();
        assert!(archives_main(&board, &config, &["e".into()]).is_err());
        assert_eq!(std::fs::read_to_string(&board).unwrap(), BOARD.replace('\n', "\r\n"));
        std::fs::write(&config, "CONFIG_XNU_EXTRA_LD_ARCHIVES=\"\"\n").unwrap();
        archives_main(&board, &config, &["e".into()]).unwrap();
        // read as Python's text mode reads: the CRLF file is written back with LF
        assert!(std::fs::read_to_string(&board).unwrap().ends_with("  \"build-corecrypto-14.py\",\n  \"e\",\n]\n"));
        series_main(&board, "s").unwrap();
        assert!(std::fs::read_to_string(&board).unwrap().contains("\"patches/xnu-12377/series\", \"s\"]"));
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
