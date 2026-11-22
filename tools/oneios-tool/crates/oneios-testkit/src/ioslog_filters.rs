//! `oneios-tool debug ioslog-filters TOOL SOURCE REFERENCE.csv WORKDIR` (was tests/ioslog/filters.py, step 8 of
//! tests/ioslog/compare.sh): `oneios-tool ioslog show`'s filters against the same selection made here from the
//! unfiltered CSV.
//!
//! REFERENCE.csv: unifiedlog_iterator's CSV of SOURCE's log (all entries). For each filter this runs
//! `TOOL ioslog show SOURCE -f csv FILTER...`, reads its CSV, and compares the rows, in order, with the rows of
//! REFERENCE.csv that the filter's rule (written here, from the CSV's columns) keeps. Prints one line per filter;
//! status 1 when one differs. WORKDIR holds `boots.txt` (`ioslog boots SOURCE`) and gets `filter.csv`.

use std::collections::HashMap;
use std::process::Command;

use anyhow::{bail, Context, Result};
use sha2::{Digest, Sha256};

const COLUMNS: [&str; 19] = [
    "Timestamp", "Event Type", "Log Type", "Subsystem", "Thread ID", "PID", "EUID", "Persona ID", "Library",
    "Library UUID", "Activity ID", "Parent Activity ID", "Category", "Process", "Process UUID", "Message",
    "Raw Message", "Boot UUID", "System Timezone Name",
];
const TIMESTAMP: usize = 0;
const LOG_TYPE: usize = 2;
const SUBSYSTEM: usize = 3;
const PID: usize = 5;
const CATEGORY: usize = 12;
const PROCESS: usize = 13;
const MESSAGE: usize = 15;
const BOOT: usize = 17;

type Row = Vec<String>;

/// The rows of a CSV with unifiedlog_iterator's header, one at a time.
fn rows(path: &str, mut each: impl FnMut(Row)) -> Result<()> {
    let mut reader = csv::ReaderBuilder::new().has_headers(false).flexible(true).from_path(path).with_context(|| path.to_string())?;
    let mut records = reader.records();
    let header = records.next().with_context(|| format!("{path}: no header"))?.with_context(|| path.to_string())?;
    if header.iter().ne(COLUMNS) {
        bail!("{path}: the header is not unifiedlog_iterator's: {:?}", header.iter().collect::<Vec<_>>());
    }
    for r in records {
        each(r.with_context(|| path.to_string())?.iter().map(str::to_string).collect());
    }
    Ok(())
}

fn all_rows(path: &str) -> Result<Vec<Row>> {
    let mut v = Vec::new();
    rows(path, |r| v.push(r))?;
    Ok(v)
}

fn proc_name(r: &Row) -> &str {
    r[PROCESS].rsplit('/').next().unwrap_or("")
}

fn joined(r: &Row) -> Vec<u8> {
    let mut b = r.join("\x1f").into_bytes();
    b.push(0x1e);
    b
}

type Rule = Box<dyn Fn(&Row) -> bool>;

pub fn run(tool: &str, source: &str, reference: &str, work: &str) -> Result<i32> {
    // One pass for what the filters are made of: the commonest process, subsystem, category and pid, a time in
    // the middle, the boots.
    let mut count: HashMap<(&'static str, String), u64> = HashMap::new();
    let mut stamps: Vec<String> = Vec::new();
    let mut boots: Vec<String> = Vec::new();
    let mut total = 0u64;
    rows(reference, |r| {
        *count.entry(("proc", proc_name(&r).to_string())).or_default() += 1;
        *count.entry(("sub", r[SUBSYSTEM].clone())).or_default() += 1;
        *count.entry(("cat", r[CATEGORY].clone())).or_default() += 1;
        *count.entry(("pid", r[PID].clone())).or_default() += 1;
        if stamps.len() < 2_000_000 {
            stamps.push(r[TIMESTAMP].clone());
        }
        if !boots.contains(&r[BOOT]) {
            boots.push(r[BOOT].clone());
        }
        total += 1;
    })?;
    // the commonest non-empty name; of two as common, the greater (Python's max of (count, name))
    let top = |kind: &str| -> Option<String> {
        count.iter().filter(|((k, name), _)| *k == kind && !name.is_empty()).map(|((_, name), v)| (*v, name.clone())).max().map(|(_, n)| n)
    };
    stamps.sort();
    if stamps.is_empty() {
        bail!("{reference}: no rows");
    }
    let middle = stamps[stamps.len() / 2].clone();
    let later = stamps[(stamps.len() - 1).min(stamps.len() / 2 + stamps.len() / 10)].clone();
    let (proc_, sub, cat, pid) = (top("proc"), top("sub"), top("cat"), top("pid"));
    let (Some(proc_), Some(pid)) = (proc_, pid) else { bail!("{reference}: no process or pid to filter by") };
    // the boot `ioslog boots` lists last is what --boot last must choose
    let boot_line = regex::Regex::new(r"^[0-9A-F]{32}\s").unwrap();
    let boots_txt = std::fs::read_to_string(format!("{work}/boots.txt")).with_context(|| format!("{work}/boots.txt"))?;
    let last_boot = boots_txt.lines().filter(|l| boot_line.is_match(l)).filter_map(|l| l.split_whitespace().next()).last().map(str::to_string);

    let grep = r"(?i)fail(ed|ure)?\b.*\d";
    let grep_re = regex::Regex::new(grep).unwrap();
    let mut cases: Vec<(String, Vec<String>, Rule)> = Vec::new();
    let s = |v: &[&str]| v.iter().map(|x| x.to_string()).collect::<Vec<_>>();
    let p = proc_.clone();
    cases.push((format!("--proc {proc_}"), s(&["--proc", &proc_]), Box::new(move |r| proc_name(r) == p)));
    cases.push(("--type error,fault".into(), s(&["--type", "error,fault"]), Box::new(|r| r[LOG_TYPE] == "Error" || r[LOG_TYPE] == "Fault")));
    cases.push(("--grep".into(), s(&["--grep", grep]), Box::new(move |r| grep_re.is_match(&r[MESSAGE]))));
    let (m, l) = (middle.clone(), later.clone());
    cases.push(("--since/--until".into(), s(&["--since", &middle, "--until", &later]), Box::new(move |r| m <= r[TIMESTAMP] && r[TIMESTAMP] < l)));
    let pd = pid.clone();
    cases.push((format!("--pid {pid}"), s(&["--pid", &pid]), Box::new(move |r| r[PID] == pd)));
    if let Some(sub) = sub {
        let chars: Vec<char> = sub.chars().collect();
        let prefix: String = chars[..1.max(chars.len() * 2 / 3)].iter().collect();
        let pre = prefix.clone();
        cases.push((format!("--subsystem {prefix}"), s(&["--subsystem", &prefix]), Box::new(move |r| r[SUBSYSTEM].starts_with(&pre))));
    }
    if let Some(cat) = cat {
        let c = cat.clone();
        cases.push((format!("--category {cat}"), s(&["--category", &cat]), Box::new(move |r| r[CATEGORY] == c)));
    }
    if let Some(last) = &last_boot {
        let (b1, b2, b3, pp) = (last.clone(), last.clone(), last.clone(), proc_.clone());
        let short: String = last.chars().take(8).collect();
        cases.push(("--boot last".into(), s(&["--boot", "last"]), Box::new(move |r| r[BOOT] == b1)));
        cases.push((format!("--boot {short}"), s(&["--boot", &short]), Box::new(move |r| r[BOOT] == b2)));
        cases.push((
            "--boot last --proc --type".into(),
            s(&["--boot", "last", "--proc", &proc_, "--type", "default"]),
            Box::new(move |r| r[BOOT] == b3 && proc_name(r) == pp && r[LOG_TYPE] == "Default"),
        ));
    }

    // what each rule keeps of the reference, in one more pass
    let mut want: Vec<(u64, Sha256)> = cases.iter().map(|_| (0, Sha256::new())).collect();
    rows(reference, |r| {
        let j = joined(&r);
        for (i, (_, _, rule)) in cases.iter().enumerate() {
            if rule(&r) {
                want[i].0 += 1;
                want[i].1.update(&j);
            }
        }
    })?;

    let mut status = 0;
    println!("{total} entries, {} boots in the reference CSV", boots.len());
    let out = format!("{work}/filter.csv");
    let show = |extra: &[String], to_file: bool| {
        let mut c = Command::new(tool);
        // the library's times, as in the reference CSV (`ioslog show --library-times`)
        c.args(["ioslog", "show", source, "--library-times"]);
        if to_file {
            c.args(["-f", "csv", "-o", &out]);
        }
        c.args(extra).output().with_context(|| format!("starting {tool}"))
    };
    let stderr_head = |e: &[u8]| String::from_utf8_lossy(e).trim().chars().take(200).collect::<String>();
    for (i, (name, args, _)) in cases.iter().enumerate() {
        let run = show(args, true)?;
        if !run.status.success() {
            println!("FAIL   {name}: status {}: {}", run.status.code().unwrap_or(-1), stderr_head(&run.stderr));
            status = 1;
            continue;
        }
        let (mut n, mut h) = (0u64, Sha256::new());
        rows(&out, |r| {
            n += 1;
            h.update(joined(&r));
        })?;
        let (wn, wh) = (want[i].0, want[i].1.clone().finalize());
        if n == wn && h.finalize() == wh {
            println!("ok     {name}: {n} rows, the same as the rule keeps");
        } else {
            println!("FAIL   {name}: ioslog wrote {n} rows, the rule keeps {wn}");
            status = 1;
        }
    }

    // --sort: the same rows as without it, in time order; text: one entry per kept row
    let run = show(&s(&["--sort", "--proc", &proc_]), true)?;
    let got = all_rows(&out)?;
    let mut plain = Vec::new();
    rows(reference, |r| {
        if proc_name(&r) == proc_ {
            plain.push(r)
        }
    })?;
    let (mut a, mut b) = (got.clone(), plain.clone());
    a.sort();
    b.sort();
    if run.status.success() && a == b && got.windows(2).all(|w| w[0][0] <= w[1][0]) {
        println!("ok     --sort: {} rows, the unsorted set, times ascending", got.len());
    } else {
        println!("FAIL   --sort: status {}, {} rows against {}", run.status.code().unwrap_or(-1), got.len(), plain.len());
        status = 1;
    }
    let run = show(&s(&["--proc", &proc_, "--type", "error,fault"]), false)?;
    let want_n = plain.iter().filter(|r| r[LOG_TYPE] == "Error" || r[LOG_TYPE] == "Fault").count();
    let entry = regex::bytes::Regex::new(r"(?m-u)^\d{4}-\d\d-\d\d \d\d:\d\d:\d\d\.\d{6}Z \S").unwrap();
    let starts = entry.find_iter(&run.stdout).count();
    if run.status.success() && starts >= want_n && (want_n == 0 || starts > 0) {
        println!("ok     text: {want_n} entries, {starts} lines that start with a time (a message may hold such a line)");
    } else {
        println!("FAIL   text: status {}, {starts} entry lines for {want_n} entries", run.status.code().unwrap_or(-1));
        status = 1;
    }
    let run = Command::new(tool).args(["ioslog", "checks", source, "--boot", "all"]).output().with_context(|| format!("starting {tool}"))?;
    let stdout = String::from_utf8_lossy(&run.stdout);
    if run.status.success() && stdout.contains("entries") {
        println!("ok     checks: {}", stdout.lines().next().unwrap_or(""));
    } else {
        println!("FAIL   checks: status {}: {}", run.status.code().unwrap_or(-1), stderr_head(&run.stderr));
        status = 1;
    }
    Ok(status)
}
