//! `oneios-tool prelink port`: carry the records that name addresses of one kernelcache (FROM, e.g. iOS 27.0.1
//! 24A446's) over to another build of the same device and XNU branch (TO, e.g. iOS 27.2.1 24B156's), and check
//! every carried address against TO's own bytes.
//!
//!   oneios-tool prelink port --from KC_A --to KC_B [--map MAP.json]
//!       [--record IN.json=OUT.json ...] [--report REPORT.json] [--strict]
//!
//! Why: kernel.kc's reviewed records (`pairs-BUILD.json`, `stubs-BUILD.json`, `reviewed-BUILD-v54.json`, the lead's
//! and fable-user's), and the board archives' layouts, name Apple's functions and data by their address in one
//! kernelcache. A point release of the same XNU branch (24A446 is xnu-13432.2.10, 24B156 xnu-13432.42.3) moves almost
//! every address but keeps almost every function: the same code, other PC-relative immediates.
//!
//! How a FROM function finds its TO function (in this order; an earlier pair is never replaced, and a TO function
//! is taken once):
//!
//!   shape   per fileset entry, the function's relocation-masked shape (`image::scan`: branch, ADRP, ADD and
//!           load/store immediates to other code or data taken out) and length are unique in FROM and in TO
//!   walk    two paired functions of the same shape are the same instructions: their calls (BL/B out of the
//!           function) go to paired functions, call by call, and their ADRP-based references to paired data
//!   slot    two paired data words that are both chained-fixup pointers (an import slot of `__auth_got`, a
//!           vtable entry) point at paired targets
//!   order   between two paired functions of one fileset entry, FROM and TO have as many unpaired functions, of
//!           lengths within a quarter of each other: they pair in link order (the weakest rule; counted apart)
//!
//! A data address pairs when the walks agree on it (every vote for one TO address). Each round of `walk`/`slot`
//! runs again over the pairs the last round made, until nothing is added; then `order`, then the walks again.
//!
//! An address inside a function (a call site, an instruction a record checks) carries over at the same offset when
//! the two functions have the same shape, else at the instruction the longest common subsequence of the two shapes
//! puts it on (`same: false`: such a record is marked for review).
//!
//! `--record IN=OUT` rewrites a JSON record: every string that is an address of FROM's mapped segments (and every
//! `linked` one, FROM's address plus the collection slide 0x1f000000000) becomes TO's, FROM's kernelcache SHA-256
//! becomes TO's, and every check object (`{"address", "size", "sha256"}`, or `[address, size, sha256]`) is read again
//! from TO (a whole-function check takes TO's function length). An element of a top-level list (one record) any of
//! whose addresses does not carry over is left out of OUT and listed in the report with the address; one carried
//! over only through `order` or a changed shape is kept and gets `"port": "review: ..."`. Nothing is guessed: an
//! address carries over by one of the rules above or not at all. `--strict` fails when a record is left out.
//!
//! The map (`--map`) holds addresses only (no Apple code). It is a work file of out/, not committed.

use std::collections::{BTreeMap, HashMap, VecDeque};

use anyhow::{anyhow, bail, Context, Result};
use oneios_common::json::{self, Value};
use rayon::prelude::*;
use sha2::{Digest, Sha256};

use crate::image::{hex, scan, unhex, Fixups, Image};

/// The kernel collection's slide: an original address plus this is the address in kernel.kc (classes.rs "linked").
const LINKED_SLIDE: u64 = 0x1f0_0000_0000;

#[derive(clap::Args, Debug)]
pub struct Args {
    /// The kernelcache the records are of (decoded Mach-O)
    #[arg(long)]
    from: String,
    /// The kernelcache to carry them over to
    #[arg(long)]
    to: String,
    /// Write the function and data map here
    #[arg(long)]
    map: Option<String>,
    /// IN.json=OUT.json: a record to carry over (repeatable)
    #[arg(long = "record")]
    records: Vec<String>,
    /// What was carried, marked and left out, per record file
    #[arg(long)]
    report: Option<String>,
    /// Fail when a record is left out
    #[arg(long)]
    strict: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
enum How {
    Shape,
    Walk,
    Slot,
    Order,
}

impl How {
    fn name(self) -> &'static str {
        match self {
            How::Shape => "shape",
            How::Walk => "walk",
            How::Slot => "slot",
            How::Order => "order",
        }
    }
}

/// One function of one side: [start, end) in a fileset entry's __TEXT_EXEC, its masked shape and what it references.
struct Function {
    start: u64,
    end: u64,
    entry: usize,
    key: [u8; 32],
    shape: Vec<u32>,
    /// branch targets outside the function, in instruction order
    calls: Vec<u64>,
    /// ADRP-based reference targets, in instruction order
    refs: Vec<u64>,
}

struct Side<'a> {
    image: Image<'a>,
    fixups: Fixups,
    functions: Vec<Function>,
    /// start -> index in `functions`
    by_start: HashMap<u64, usize>,
    /// (entry, start) sorted, for "which function holds this address"
    sorted: Vec<(u64, usize)>,
    entries: Vec<String>,
    sha256: String,
}

impl<'a> Side<'a> {
    fn new(raw: &'a [u8], names: &[String]) -> Result<Side<'a>> {
        let image = Image::new(raw, 0)?;
        let fixups = image.fixups()?;
        let mut spans: Vec<(usize, u64, u64)> = Vec::new();
        for (index, name) in names.iter().enumerate() {
            let entry = image.entry(name)?;
            // a function ends at the next start or at the end of its section; each import stub (16 bytes in
            // __auth_stubs, 12 in __stubs) is a function of its own, so that calls through stubs pair too
            let mut texts: Vec<(u64, u64)> = entry.sections.iter().filter(|s| s.segment == "__TEXT_EXEC" && s.size != 0).map(|s| (s.va, s.va + s.size)).collect();
            if texts.is_empty() {
                texts = entry.segments.iter().filter(|s| s.name == "__TEXT_EXEC" && s.filesize != 0).map(|s| (s.va, s.va + s.filesize)).collect();
            }
            let mut starts = entry.function_starts()?;
            for s in entry.sections.iter().filter(|s| s.segment == "__TEXT_EXEC") {
                let step = match s.name.as_str() {
                    "__auth_stubs" => 16,
                    "__stubs" => 12,
                    _ => continue,
                };
                starts.extend((0..s.size / step).map(|i| s.va + i * step));
            }
            starts.sort_unstable();
            starts.dedup();
            for (low, high) in texts {
                let inside: Vec<u64> = starts.iter().copied().filter(|&a| low <= a && a < high).collect();
                for (i, &start) in inside.iter().enumerate() {
                    let end = inside.get(i + 1).copied().unwrap_or(high);
                    if end > start {
                        spans.push((index, start, end));
                    }
                }
            }
        }
        let functions: Vec<Function> = spans
            .par_iter()
            .filter_map(|&(entry, start, end)| {
                let code = image.read(start, end - start)?;
                let scanned = scan(code, start);
                let mut hasher = Sha256::new();
                hasher.update((entry as u64).to_le_bytes());
                for word in &scanned.shape {
                    hasher.update(word.to_le_bytes());
                }
                let calls = scanned.calls.iter().filter(|c| c.1 < start || c.1 >= end).map(|c| c.1).collect();
                let refs = scanned.refs.iter().map(|r| r.target).collect();
                Some(Function { start, end, entry, key: hasher.finalize().into(), shape: scanned.shape, calls, refs })
            })
            .collect();
        let by_start = functions.iter().enumerate().map(|(i, f)| (f.start, i)).collect();
        let mut sorted: Vec<(u64, usize)> = functions.iter().enumerate().map(|(i, f)| (f.start, i)).collect();
        sorted.sort_unstable();
        Ok(Side { image, fixups, functions, by_start, sorted, entries: names.to_vec(), sha256: oneios_common::sha256_hex(raw) })
    }

    /// The function holding `address`.
    fn holding(&self, address: u64) -> Option<usize> {
        let at = self.sorted.partition_point(|&(start, _)| start <= address);
        let (_, index) = *self.sorted.get(at.checked_sub(1)?)?;
        (address < self.functions[index].end).then_some(index)
    }

    fn mapped(&self, address: u64) -> bool {
        self.image.segments.iter().any(|s| s.name != "__LINKEDIT" && s.size != 0 && s.va <= address && address < s.va + s.size)
    }
}

/// FROM index -> TO index for functions; FROM address -> TO address for data.
struct Map {
    function: HashMap<usize, (usize, How)>,
    taken: HashMap<usize, usize>,
    data_votes: HashMap<u64, BTreeMap<u64, u32>>,
    conflicts: u64,
}

impl Map {
    fn pair(&mut self, a: usize, b: usize, how: How, queue: &mut VecDeque<usize>) -> bool {
        match (self.function.get(&a), self.taken.get(&b)) {
            (None, None) => {
                self.function.insert(a, (b, how));
                self.taken.insert(b, a);
                queue.push_back(a);
                true
            }
            (Some(&(had, _)), _) if had == b => false,
            _ => {
                self.conflicts += 1;
                false
            }
        }
    }

    /// The data address `a` pairs with: the one TO address every vote names.
    fn data(&self, a: u64) -> Option<u64> {
        let votes = self.data_votes.get(&a)?;
        (votes.len() == 1).then(|| *votes.keys().next().unwrap())
    }
}

fn walk(from: &Side, to: &Side, map: &mut Map, queue: &mut VecDeque<usize>) {
    while let Some(a) = queue.pop_front() {
        let (b, _) = map.function[&a];
        let (fa, fb) = (&from.functions[a], &to.functions[b]);
        if fa.key != fb.key || fa.calls.len() != fb.calls.len() || fa.refs.len() != fb.refs.len() {
            continue;
        }
        for (&ta, &tb) in fa.calls.iter().zip(&fb.calls) {
            if let (Some(&ia), Some(&ib)) = (from.by_start.get(&ta), to.by_start.get(&tb)) {
                map.pair(ia, ib, How::Walk, queue);
            }
        }
        for (&ta, &tb) in fa.refs.iter().zip(&fb.refs) {
            match (from.by_start.get(&ta), to.by_start.get(&tb)) {
                (Some(&ia), Some(&ib)) => {
                    map.pair(ia, ib, How::Walk, queue);
                }
                _ => {
                    *map.data_votes.entry(ta).or_default().entry(tb).or_insert(0) += 1;
                    // a pointer slot (an import of __auth_got, a vtable entry): its target pairs too
                    if let (Some(pa), Some(pb)) = (from.fixups.get(&ta), to.fixups.get(&tb)) {
                        match (from.by_start.get(&pa.target), to.by_start.get(&pb.target)) {
                            (Some(&ia), Some(&ib)) => {
                                map.pair(ia, ib, How::Slot, queue);
                            }
                            _ => *map.data_votes.entry(pa.target).or_default().entry(pb.target).or_insert(0) += 1,
                        }
                    }
                }
            }
        }
    }
}

/// Between paired neighbours of one entry: as many unpaired on both sides, lengths close: pair them in order.
fn order(from: &Side, to: &Side, map: &mut Map, queue: &mut VecDeque<usize>) -> usize {
    let mut added = 0;
    for entry in 0..from.entries.len() {
        let list = |side: &Side| -> Vec<usize> {
            let mut v: Vec<usize> = side.functions.iter().enumerate().filter(|(_, f)| f.entry == entry).map(|(i, _)| i).collect();
            v.sort_by_key(|&i| side.functions[i].start);
            v
        };
        let (la, lb) = (list(from), list(to));
        let pos_b: HashMap<usize, usize> = lb.iter().enumerate().map(|(p, &i)| (i, p)).collect();
        // anchors: paired functions in increasing order on both sides
        let mut anchors: Vec<(usize, usize)> = vec![];
        let mut last_b: isize = -1;
        for (pa, &ia) in la.iter().enumerate() {
            if let Some(&(ib, _)) = map.function.get(&ia) {
                if let Some(&pb) = pos_b.get(&ib) {
                    if pb as isize > last_b {
                        anchors.push((pa, pb));
                        last_b = pb as isize;
                    }
                }
            }
        }
        let mut bounds = vec![(-1isize, -1isize)];
        bounds.extend(anchors.iter().map(|&(a, b)| (a as isize, b as isize)));
        bounds.push((la.len() as isize, lb.len() as isize));
        for w in bounds.windows(2) {
            let ((a0, b0), (a1, b1)) = (w[0], w[1]);
            let gap_a: Vec<usize> = ((a0 + 1)..a1).map(|p| la[p as usize]).filter(|i| !map.function.contains_key(i)).collect();
            let gap_b: Vec<usize> = ((b0 + 1)..b1).map(|p| lb[p as usize]).filter(|i| !map.taken.contains_key(i)).collect();
            if gap_a.is_empty() || gap_a.len() != gap_b.len() {
                continue;
            }
            let close = gap_a.iter().zip(&gap_b).all(|(&ia, &ib)| {
                let (x, y) = (from.functions[ia].end - from.functions[ia].start, to.functions[ib].end - to.functions[ib].start);
                4 * x.abs_diff(y) <= x.max(y)
            });
            if !close {
                continue;
            }
            for (&ia, &ib) in gap_a.iter().zip(&gap_b) {
                if map.pair(ia, ib, How::Order, queue) {
                    added += 1;
                }
            }
        }
    }
    added
}

/// Instruction index in FROM's function -> instruction index in TO's (the longest common subsequence of the shapes).
fn lcs(a: &[u32], b: &[u32]) -> Option<HashMap<usize, usize>> {
    let (n, m) = (a.len(), b.len());
    if n.saturating_mul(m) > 40_000_000 {
        return None;
    }
    let mut table = vec![0u32; (n + 1) * (m + 1)];
    for i in (0..n).rev() {
        for j in (0..m).rev() {
            table[i * (m + 1) + j] = if a[i] == b[j] { table[(i + 1) * (m + 1) + j + 1] + 1 } else { table[(i + 1) * (m + 1) + j].max(table[i * (m + 1) + j + 1]) };
        }
    }
    let (mut i, mut j, mut out) = (0, 0, HashMap::new());
    while i < n && j < m {
        if a[i] == b[j] {
            out.insert(i, j);
            i += 1;
            j += 1;
        } else if table[(i + 1) * (m + 1) + j] >= table[i * (m + 1) + j + 1] {
            i += 1;
        } else {
            j += 1;
        }
    }
    Some(out)
}

/// Where a FROM address is in TO, and whether by a rule a reviewer need not look at again.
#[derive(Clone, Debug)]
struct Carried {
    to: u64,
    sure: bool,
    how: String,
}

struct Porter<'a> {
    from: Side<'a>,
    to: Side<'a>,
    map: Map,
}

impl<'a> Porter<'a> {
    fn carry(&self, a: u64) -> Option<Carried> {
        if let Some(&ia) = self.from.by_start.get(&a) {
            let &(ib, how) = self.map.function.get(&ia)?;
            let same = self.from.functions[ia].key == self.to.functions[ib].key;
            return Some(Carried { to: self.to.functions[ib].start, sure: how != How::Order, how: format!("{}{}", how.name(), if same { "" } else { ", shape changed" }) });
        }
        if let Some(ia) = self.from.holding(a) {
            let &(ib, how) = self.map.function.get(&ia)?;
            let (fa, fb) = (&self.from.functions[ia], &self.to.functions[ib]);
            let offset = a - fa.start;
            if fa.key == fb.key {
                return Some(Carried { to: fb.start + offset, sure: how != How::Order, how: format!("{} (+{offset:#x})", how.name()) });
            }
            if offset % 4 != 0 {
                return None;
            }
            let at = lcs(&fa.shape, &fb.shape)?.get(&(offset as usize / 4)).copied()?;
            return Some(Carried { to: fb.start + 4 * at as u64, sure: false, how: format!("{} (+{offset:#x}), shape changed: the instruction the two shapes have in common", how.name()) });
        }
        let b = self.map.data(a)?;
        Some(Carried { to: b, sure: true, how: "data".into() })
    }
}

fn is_address(text: &str) -> Option<u64> {
    let t = text.strip_prefix("0x")?;
    if t.len() != 16 || !t.starts_with("fffffe") && !t.starts_with("fffff0") {
        return None;
    }
    u64::from_str_radix(t, 16).ok()
}

/// One record's walk: every address string rewritten; what could not be carried, and what is not sure.
#[derive(Default)]
struct Walked {
    missing: Vec<String>,
    review: Vec<String>,
    checks: u64,
}

impl<'a> Porter<'a> {
    fn rewrite(&self, value: &mut Value, out: &mut Walked) -> Result<()> {
        match value {
            Value::Str(text) => {
                if *text == self.from.sha256 {
                    *text = self.to.sha256.clone();
                    return Ok(());
                }
                let Some(address) = is_address(text) else { return Ok(()) };
                let (original, linked) = if !self.from.mapped(address) && self.from.mapped(address.wrapping_sub(LINKED_SLIDE)) {
                    (address - LINKED_SLIDE, true)
                } else {
                    (address, false)
                };
                if !self.from.mapped(original) {
                    return Ok(());
                }
                match self.carry(original) {
                    Some(c) => {
                        if !c.sure {
                            out.review.push(format!("{} -> {}: {}", hex(original), hex(c.to), c.how));
                        }
                        *text = hex(if linked { c.to + LINKED_SLIDE } else { c.to });
                    }
                    None => out.missing.push(hex(original)),
                }
            }
            Value::List(items) => {
                for item in items.iter_mut() {
                    self.rewrite(item, out)?;
                }
                self.recheck_list(items, out)?;
            }
            Value::Dict(_) => {
                if let Value::Dict(items) = value {
                    for (_, item) in items.iter_mut() {
                        self.rewrite(item, out)?;
                    }
                }
                self.recheck_dict(value, out)?;
            }
            _ => {}
        }
        Ok(())
    }

    /// A check's size in TO: the TO function's length when the check covered the whole FROM function.
    fn check_size(&self, old_address: Option<u64>, new_address: u64, size: u64) -> u64 {
        if let (Some(a), Some(&ib)) = (old_address, self.to.by_start.get(&new_address)) {
            if let Some(&ia) = self.from.by_start.get(&a) {
                let f = &self.from.functions[ia];
                if f.end - f.start == size {
                    let g = &self.to.functions[ib];
                    return g.end - g.start;
                }
            }
        }
        size
    }

    fn digest(&self, address: u64, size: u64) -> Result<String> {
        let bytes = self.to.image.read(address, size).ok_or_else(|| anyhow!("{} +{size}: not in the TO kernelcache", hex(address)))?;
        Ok(oneios_common::sha256_hex(bytes))
    }

    fn recheck_dict(&self, value: &mut Value, out: &mut Walked) -> Result<()> {
        let (Some(address), Some(size), Some(_)) = (value.get("address").and_then(Value::as_str), value.get("size"), value.get("sha256")) else { return Ok(()) };
        let new = unhex(address)?;
        let size = match size {
            Value::Int(n) => *n as u64,
            Value::Str(s) => unhex(s)?,
            _ => return Ok(()),
        };
        let old = self.map.taken.get(&self.to.by_start.get(&new).copied().unwrap_or(usize::MAX)).map(|&ia| self.from.functions[ia].start);
        let size = self.check_size(old, new, size);
        value.set("size", size as i128);
        value.set("sha256", self.digest(new, size)?);
        out.checks += 1;
        Ok(())
    }

    fn recheck_list(&self, items: &mut [Value], out: &mut Walked) -> Result<()> {
        // [address, size, sha256] (the display record's "checks")
        if let [Value::Str(address), Value::Int(size), Value::Str(sum)] = items {
            if is_address(address).is_some() && sum.len() == 64 {
                let new = unhex(address)?;
                *sum = self.digest(new, *size as u64)?;
                out.checks += 1;
            }
        }
        Ok(())
    }
}

fn port_record(porter: &Porter, input: &str, output: &str) -> Result<Value> {
    let mut record = json::load(input)?;
    let mut summary = Value::dict();
    summary.set("in", input);
    summary.set("out", output);
    let (mut kept, mut left, mut marked, mut checks) = (0u64, Vec::new(), Vec::new(), 0u64);
    // each element of a top-level list is one record; anything else is walked whole
    if let Value::Dict(items) = &mut record {
        for (key, item) in items.iter_mut() {
            if let Value::List(list) = item {
                let mut carried = Vec::new();
                for (index, mut element) in std::mem::take(list).into_iter().enumerate() {
                    let mut walked = Walked::default();
                    porter.rewrite(&mut element, &mut walked)?;
                    checks += walked.checks;
                    if !walked.missing.is_empty() {
                        left.push(Value::str(format!("{key}[{index}]: {} not carried over", walked.missing.join(", "))));
                        continue;
                    }
                    if !walked.review.is_empty() {
                        let text = format!("review: {}", walked.review.join("; "));
                        if let Value::Dict(_) = element {
                            element.set("port", text.as_str());
                        }
                        marked.push(Value::str(format!("{key}[{index}]: {text}")));
                    }
                    kept += 1;
                    carried.push(element);
                }
                *list = carried;
            } else {
                let mut walked = Walked::default();
                porter.rewrite(item, &mut walked)?;
                checks += walked.checks;
                if !walked.missing.is_empty() {
                    bail!("{input}: \"{key}\" names {} which do not carry over (not a list of records: nothing to leave out)", walked.missing.join(", "));
                }
                for text in walked.review {
                    marked.push(Value::str(format!("{key}: review: {text}")));
                }
            }
        }
    }
    let mut ported = Value::dict();
    ported.set("from_kernelcache_sha256", porter.from.sha256.as_str());
    ported.set("by", "oneios-tool prelink port (kernel/prelink/README.md, \"Another build\")");
    ported.set("kept", kept as i128);
    ported.set("left_out", left.len() as i128);
    ported.set("review", marked.len() as i128);
    if let Value::Dict(_) = record {
        record.set("ported", ported);
    }
    std::fs::write(output, json::dumps(&record, Some(1), false) + "\n").with_context(|| output.to_string())?;
    summary.set("kept", kept as i128);
    summary.set("checks_read_again", checks as i128);
    summary.set("left_out", Value::List(left));
    summary.set("review", Value::List(marked));
    Ok(summary)
}

pub fn run(args: Args) -> Result<()> {
    let (raw_a, raw_b) = (oneios_common::Map::open(&args.from)?, oneios_common::Map::open(&args.to)?);
    let (ia, ib) = (Image::new(&raw_a, 0)?, Image::new(&raw_b, 0)?);
    let names: Vec<String> = ia.entries.keys().filter(|n| ib.entries.contains_key(*n)).cloned().collect();
    let only: Vec<&String> = ia.entries.keys().filter(|n| !ib.entries.contains_key(*n)).chain(ib.entries.keys().filter(|n| !ia.entries.contains_key(*n))).collect();
    let (from, to) = (Side::new(&raw_a, &names)?, Side::new(&raw_b, &names)?);
    let mut map = Map { function: HashMap::new(), taken: HashMap::new(), data_votes: HashMap::new(), conflicts: 0 };
    let mut queue = VecDeque::new();
    // shape: unique per entry on both sides
    let count = |side: &Side| {
        let mut c: HashMap<[u8; 32], Vec<usize>> = HashMap::new();
        for (i, f) in side.functions.iter().enumerate() {
            c.entry(f.key).or_default().push(i);
        }
        c
    };
    let (ka, kb) = (count(&from), count(&to));
    let mut unique: Vec<(usize, usize)> = ka.iter().filter_map(|(k, v)| match (v.as_slice(), kb.get(k).map(Vec::as_slice)) {
        ([a], Some([b])) => Some((*a, *b)),
        _ => None,
    }).collect();
    unique.sort_unstable();
    for (a, b) in unique {
        map.pair(a, b, How::Shape, &mut queue);
    }
    walk(&from, &to, &mut map, &mut queue);
    loop {
        let added = order(&from, &to, &mut map, &mut queue);
        walk(&from, &to, &mut map, &mut queue);
        if added == 0 {
            break;
        }
    }
    let porter = Porter { from, to, map };
    let mut how_counts: BTreeMap<&str, u64> = BTreeMap::new();
    let mut same = 0u64;
    for (&a, &(b, how)) in &porter.map.function {
        *how_counts.entry(how.name()).or_default() += 1;
        if porter.from.functions[a].key == porter.to.functions[b].key {
            same += 1;
        }
    }
    let data_paired = porter.map.data_votes.values().filter(|v| v.len() == 1).count();
    let data_split = porter.map.data_votes.len() - data_paired;
    println!(
        "port: {} -> {}: {} entries in both ({} in one only); functions {} / {}, paired {} ({}), {} of them the same shape; data paired {data_paired}, {data_split} with votes for several; {} conflicts",
        &porter.from.sha256[..12],
        &porter.to.sha256[..12],
        names.len(),
        only.len(),
        porter.from.functions.len(),
        porter.to.functions.len(),
        porter.map.function.len(),
        how_counts.iter().map(|(k, v)| format!("{k} {v}")).collect::<Vec<_>>().join(", "),
        same,
        porter.map.conflicts
    );
    if let Some(path) = &args.map {
        let mut functions = Value::dict();
        let mut pairs: Vec<(&usize, &(usize, How))> = porter.map.function.iter().collect();
        pairs.sort_by_key(|(a, _)| porter.from.functions[**a].start);
        for (&a, &(b, how)) in pairs {
            let (fa, fb) = (&porter.from.functions[a], &porter.to.functions[b]);
            let mut row = Value::dict();
            row.set("to", hex(fb.start));
            row.set("how", how.name());
            row.set("same", fa.key == fb.key);
            row.set("size", (fa.end - fa.start) as i128);
            row.set("size_to", (fb.end - fb.start) as i128);
            row.set("entry", porter.from.entries[fa.entry].as_str());
            functions.set(hex(fa.start), row);
        }
        let mut data = Value::dict();
        let mut keys: Vec<&u64> = porter.map.data_votes.keys().collect();
        keys.sort_unstable();
        for a in keys {
            if let Some(b) = porter.map.data(*a) {
                data.set(hex(*a), hex(b));
            }
        }
        let mut out = Value::dict();
        out.set("from_kernelcache_sha256", porter.from.sha256.as_str());
        out.set("to_kernelcache_sha256", porter.to.sha256.as_str());
        out.set("functions", functions);
        out.set("data", data);
        std::fs::write(path, json::dumps(&out, Some(1), false) + "\n").with_context(|| path.clone())?;
    }
    let mut report = Vec::new();
    let mut left_total = 0;
    for spec in &args.records {
        let (input, output) = spec.split_once('=').ok_or_else(|| anyhow!("--record IN=OUT, not {spec}"))?;
        let summary = port_record(&porter, input, output)?;
        let left = summary.get("left_out").and_then(Value::as_list).map_or(0, |l| l.len());
        let review = summary.get("review").and_then(Value::as_list).map_or(0, |l| l.len());
        println!("port: {input} -> {output}: {} kept ({review} to review), {left} left out, {} checks read again", summary.get("kept").and_then(Value::as_int).unwrap_or(0), summary.get("checks_read_again").and_then(Value::as_int).unwrap_or(0));
        left_total += left;
        report.push(summary);
    }
    if let Some(path) = &args.report {
        std::fs::write(path, json::dumps(&Value::List(report), Some(1), false) + "\n").with_context(|| path.clone())?;
    }
    if args.strict && left_total > 0 {
        bail!("port: {left_total} record(s) left out (--strict)");
    }
    Ok(())
}
