//! After a boot: did launchd apply the daemon policy? Read from the Data volume the boot left
//! (roottweak/bootcheck.py; its header has what launchd 27.0.1 does with the override and what its log says).

use crate::launchd;
use crate::plist::{self, P};
use regex::Regex;
use std::collections::{BTreeMap, BTreeSet};
use std::os::unix::fs::MetadataExt;
use std::path::Path;
use std::sync::OnceLock;

pub const OVERRIDE: &str = "db/com.apple.xpc.launchd/disabled.plist";
pub const MIGRATED: &str = "db/com.apple.xpc.launchd/disabled.migrated";
pub const LOG: &str = "log/com.apple.xpc.launchd/launchd.log";

const LABEL: &str = r"(?:system/|user/\d+/|gui/\d+/)?([A-Za-z0-9_.\-]+)";

fn res() -> &'static [Regex; 4] {
    static R: OnceLock<[Regex; 4]> = OnceLock::new();
    R.get_or_init(|| {
        [
            Regex::new(&format!(r"\({LABEL}(?: \[\d+\])?\) <Notice>: service state: spawning")).unwrap(),
            Regex::new(r"pending spawn, domain in on-demand-only mode: (\S+)").unwrap(),
            Regex::new(r"Ignoring disabled state because DisableAllowed=false: (\S+)").unwrap(),
            Regex::new(r"failed lookup: name = (\S+?), requestor = ([^,]+), error = 3\b").unwrap(),
        ]
    })
}

/// The fields of a stat result launchd looks at.
#[derive(Clone, Copy, Debug)]
pub struct Stat {
    pub mode: u32,
    pub uid: u32,
}

/// Why launchd would not read an override file with this stat result; None when it would.
pub fn accepted(st: Stat) -> Option<String> {
    if st.mode & 0o170000 != 0o100000 {
        return Some("not a regular file".into());
    }
    if st.uid != 0 {
        return Some(format!("owner is uid {}: launchd reads it only when root owns it", st.uid));
    }
    if st.mode & 0o022 != 0 {
        return Some(format!("mode {:04o}: launchd reads it only when group and others cannot write it", st.mode & 0o7777));
    }
    None
}

pub struct Log {
    pub spawned: BTreeSet<String>,
    pub pending: BTreeSet<String>,
    pub forced: BTreeSet<String>,
    /// {name: requestor} of the Mach service lookups that found no job
    pub no_such_service: BTreeMap<String, String>,
}

/// What a launchd.log says.
pub fn read_log(text: &str) -> Log {
    let r = res();
    let all = |re: &Regex| re.captures_iter(text).map(|c| c[1].to_string()).collect::<BTreeSet<_>>();
    Log {
        spawned: all(&r[0]),
        pending: all(&r[1]),
        forced: all(&r[2]),
        no_such_service: r[3].captures_iter(text).map(|c| (c[1].to_string(), c[2].trim().to_string())).collect(),
    }
}

/// Labels of the compiled cache that run without being asked: RunAtLoad or KeepAlive.
pub fn at_load(cache: &P) -> BTreeSet<String> {
    launchd::jobs(cache)
        .into_iter()
        .filter(|(_, g, k)| {
            let job = cache.get(g).and_then(|e| e.get(k)).unwrap();
            job.get("RunAtLoad").is_some_and(P::truthy) || job.get("KeepAlive").is_some_and(P::truthy)
        })
        .map(|(l, _, _)| l)
        .collect()
}

/// Python's text of an OSError.
pub fn os_error(e: &std::io::Error, path: &Path) -> String {
    match e.raw_os_error() {
        Some(n) => {
            let text = unsafe { std::ffi::CStr::from_ptr(libc::strerror(n)) }.to_string_lossy().into_owned();
            format!("[Errno {n}] {text}: {}", oneios_common::pyrepr::repr_str(&path.to_string_lossy()))
        }
        None => e.to_string(),
    }
}

fn load_plist(path: &Path) -> Result<P, String> {
    let data = std::fs::read(path).map_err(|e| os_error(&e, path))?;
    plist::loads(&data).map_err(|e| e.to_string())?.value.ok_or_else(|| "an empty property list".to_string())
}

/// Compares the override and launchd's log of a Data tree. `root` (the OS volume's files) adds how many of the
/// jobs want to run at load and which are marked DisableAllowed = false. (lines, problems): problems is empty when
/// the policy was applied.
pub fn check(data: &Path, root: Option<&Path>, ignore_owner: bool) -> (Vec<String>, Vec<String>) {
    let mut lines = Vec::new();
    let mut problems = Vec::new();
    let path = data.join(OVERRIDE);
    let st = match std::fs::metadata(&path) {
        Ok(m) => Stat { mode: m.mode(), uid: m.uid() },
        Err(e) => return (lines, vec![format!("{OVERRIDE}: {}", os_error(&e, &path))]),
    };
    let disabled: BTreeSet<String> = match load_plist(&path) {
        Ok(v) => v.items().iter().filter(|(_, off)| matches!(off, P::Bool(true))).map(|(l, _)| l.clone()).collect(),
        Err(e) => return (lines, vec![format!("{OVERRIDE}: {e}")]),
    };
    match accepted(st) {
        Some(why) if ignore_owner => lines.push(format!("override: {} jobs disabled; owner and mode not checked here ({why})", disabled.len())),
        Some(why) => problems.push(format!("{OVERRIDE}: {why}")),
        None => lines.push(format!("override: {} jobs disabled; root owns it, mode {:04o}: launchd reads it", disabled.len(), st.mode & 0o7777)),
    }
    if !data.join(MIGRATED).exists() {
        lines.push(format!("{MIGRATED} is not there: launchd looks for the older launchd.db overrides first"));
    }
    let log_path = data.join(LOG);
    let log = match std::fs::read(&log_path) {
        Ok(raw) => read_log(&String::from_utf8_lossy(&raw).replace("\r\n", "\n").replace('\r', "\n")),
        Err(e) => {
            problems.push(format!("{LOG}: {} (launchd writes it once the Data volume is mounted)", os_error(&e, &log_path)));
            return (lines, problems);
        }
    };
    lines.push(format!("launchd.log: {} jobs spawned, {} pending at load", log.spawned.len(), log.pending.len()));

    let mut forced = BTreeSet::new();
    let ran: BTreeSet<String> = log.spawned.union(&log.pending).cloned().collect();
    if let Some(root) = root {
        let daemons = root.join(launchd::DAEMONS_DIR);
        if daemons.is_dir() {
            for name in crate::tree::listdir(&daemons).unwrap_or_default() {
                if name.starts_with("com.apple.jetsamproperties.") && name.ends_with(".plist") {
                    if let Ok(j) = load_plist(&daemons.join(&name)) {
                        forced.extend(launchd::forced(&j));
                    }
                }
            }
        }
        let wants = load_plist(&root.join(launchd::CACHE)).map(|c| at_load(&c)).unwrap_or_default();
        let wd: BTreeSet<&String> = wants.intersection(&disabled).collect();
        let other: BTreeSet<&String> = wants.difference(&disabled).collect();
        lines.push(format!(
            "cache: {} of the disabled jobs want to run at load, {} of them pending or spawned; of the {} other such jobs, {}",
            wd.len(),
            wd.iter().filter(|l| ran.contains(**l)).count(),
            other.len(),
            other.iter().filter(|l| ran.contains(**l)).count()
        ));
    }
    let ran_disabled: Vec<&String> = ran.intersection(&disabled).collect();
    let allowed: Vec<&String> = ran_disabled.iter().copied().filter(|l| forced.contains(*l) || log.forced.contains(*l)).collect();
    let wrong: Vec<&String> = ran_disabled.iter().copied().filter(|l| !allowed.contains(l)).collect();
    if !allowed.is_empty() {
        lines.push(format!("run although disabled (DisableAllowed = false): {}", allowed.iter().map(|s| s.as_str()).collect::<Vec<_>>().join(", ")));
    }
    let refused: Vec<&String> = log.no_such_service.keys().filter(|n| disabled.contains(*n)).collect();
    if !refused.is_empty() {
        lines.push(format!(
            "lookups of disabled jobs that found no service: {}",
            refused.iter().map(|n| format!("{n} (asked by {})", log.no_such_service[*n])).collect::<Vec<_>>().join(", ")
        ));
    }
    if !wrong.is_empty() {
        problems.push(format!("{} disabled jobs were started: {}", wrong.len(), wrong.iter().map(|s| s.as_str()).collect::<Vec<_>>().join(", ")));
    } else {
        lines.push("no disabled job was started: the policy is in force".into());
    }
    (lines, problems)
}
