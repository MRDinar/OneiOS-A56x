//! The Data seed's tree: the OS volume's `/private/var` copied out of the image with our APFS reader, and the meta
//! file `image tree-copy` writes beside it (what a copy as a user loses: owner, mode, time, xattrs).
//!
//! build.sh mounts the image (apfs-fuse on Linux, hdiutil on a Mac) and runs `tree-copy` on the mount. The phone
//! has neither, and `serve` starts no program, so this reads the volume's records itself and writes what tree-copy
//! writes from the mount: the same folders (mode 0700), files (mode 0600, both times the source's whole seconds),
//! links, and the same meta file, entry for entry and in tree-copy's order (`tests`: compared with tree-copy on an
//! apfs-fuse mount of 24A446's OS volume, tools/oneios-tool/README.md "image build").
//!
//! The xattrs: tree-copy leaves out the three that only say how APFS stores an entry (`com.apple.decmpfs`,
//! `com.apple.ResourceFork`, `com.apple.fs.symlink`); so does this. 24A446's `/private/var` has no other (its
//! records: 4,930 decmpfs, 2,889 resource forks, one link target), so its meta file has no `x` at all. Another
//! xattr would be recorded here with its value, as tree-copy records it on a Mac.

use oneios_common::job;
use oneios_common::json::{self, Value};
use oneios_image::apfs::{ApfsTree, Pick, DT_DIR, DT_REG};
use rayon::prelude::*;
use std::collections::HashMap;
use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt};
use std::os::unix::io::AsRawFd;
use std::sync::atomic::{AtomicBool, Ordering};

/// The xattrs tree-copy drops (`seed.rs` `DROP_XATTRS`).
const DROP_XATTRS: [&str; 3] = ["com.apple.decmpfs", "com.apple.ResourceFork", "com.apple.fs.symlink"];

const S_IFMT: u16 = 0o170000;
const S_IFDIR: u16 = 0o040000;
const S_IFREG: u16 = 0o100000;
const S_IFLNK: u16 = 0o120000;

/// File contents held in memory at one time, over all threads (a file is read whole before it is written).
const IN_MEMORY_MAX: u64 = 256 << 20;

/// One entry, as tree-copy records it.
struct Item {
    rel: String,
    id: u64,
    kind: char,
    mode: u32,
    uid: u32,
    gid: u32,
    mtime: i64,
    /// A file's size (a compressed file's plain size).
    size: u64,
    x: Vec<(String, String)>,
}

impl Item {
    /// tree-copy's entry: the top one has `t` first, the others last.
    fn value(&self, top: bool) -> Value {
        let mut d: Vec<(String, Value)> = Vec::with_capacity(6);
        let kind = ("t".to_string(), Value::Str(self.kind.to_string()));
        if top {
            d.push(kind.clone());
        }
        d.push(("m".into(), Value::from(self.mode)));
        d.push(("u".into(), Value::from(self.uid)));
        d.push(("g".into(), Value::from(self.gid)));
        d.push(("s".into(), Value::from(self.mtime)));
        if !self.x.is_empty() {
            d.push(("x".into(), Value::Dict(self.x.iter().map(|(k, v)| (k.clone(), Value::str(v))).collect())));
        }
        if !top {
            d.push(kind);
        }
        Value::Dict(d)
    }
}

/// `int(st.st_mtime)` of a time in nanoseconds since 1970, as tree-copy reads it from a mount: Python makes the
/// float `sec + 1e-9 * nsec` and int() cuts it towards zero (`py.rs` `int_mtime`).
fn int_mtime(ns: u64) -> i64 {
    let (sec, nsec) = ((ns / 1_000_000_000) as i64, (ns % 1_000_000_000) as i64);
    (sec as f64 + 1e-9 * nsec as f64) as i64
}

fn base64(data: &[u8]) -> String {
    const ABC: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(data.len().div_ceil(3) * 4);
    for chunk in data.chunks(3) {
        let n = chunk.iter().enumerate().fold(0u32, |n, (i, b)| n | (*b as u32) << (16 - 8 * i));
        for i in 0..4 {
            out.push(if i <= chunk.len() { ABC[(n >> (18 - 6 * i) & 63) as usize] as char } else { '=' });
        }
    }
    out
}

/// What the copy did.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Copied {
    /// Entries below the top folder (tree-copy's "copied N entries").
    pub entries: usize,
    pub files: u64,
    pub bytes: u64,
    /// Sockets, FIFOs and devices: not copied, listed in the meta file's `skipped`.
    pub skipped: usize,
}

type Fail = String;

fn reader(e: oneios_image::Fail) -> Fail {
    e.to_string()
}

/// The entries below `subdir` that have an xattr tree-copy keeps, with those names (sorted): the records' walk.
fn kept_xattrs(tree: &ApfsTree, subdir: &str) -> Result<HashMap<String, Vec<String>>, Fail> {
    Ok(tree
        .walk(subdir)
        .map_err(reader)?
        .into_iter()
        .filter_map(|(rel, rec)| {
            let kept: Vec<String> = rec.x.into_iter().filter(|n| !DROP_XATTRS.contains(&n.as_str())).collect();
            (!kept.is_empty()).then_some((rel, kept))
        })
        .collect())
}

/// The entry `id` (named `rel`) as tree-copy records it, with the values of its xattrs that tree-copy keeps.
fn item_of(tree: &ApfsTree, rel: String, id: u64, xattrs: &HashMap<String, Vec<String>>) -> Result<Item, Fail> {
    let st = tree.stat(id).map_err(reader)?;
    let kind = match st.mode & S_IFMT {
        S_IFDIR => 'd',
        S_IFLNK => 'l',
        S_IFREG => 'f',
        _ => '?',
    };
    let mut x = Vec::new();
    for name in xattrs.get(&rel).into_iter().flatten() {
        let value = tree.xattr(id, name).map_err(reader)?.unwrap_or_default();
        x.push((name.clone(), base64(&value)));
    }
    Ok(Item { rel, id, kind, mode: (st.mode & 0o7777) as u32, uid: st.uid, gid: st.gid, mtime: int_mtime(st.mtime), size: st.size, x })
}

/// The sizes of the files below `subdir`, summed, and how many entries there are: the room the seed needs, asked
/// before anything is written (`image build`'s one `space` event).
pub fn tree_size(image: &str, subdir: &str) -> Result<(u64, u64), Fail> {
    let tree = oneios_image::apfs_tools::open(image, None, Pick::Block0).map_err(reader)?;
    let top = tree.find(subdir).map_err(reader)?.ok_or_else(|| format!("{image}: the OS volume has no /{subdir}"))?;
    let mut level = vec![top];
    let (mut bytes, mut entries) = (0u64, 0u64);
    while !level.is_empty() {
        let found: Vec<Result<(u64, u64, Vec<u64>), Fail>> = level
            .par_iter()
            .map(|dir| {
                let (mut bytes, mut count, mut dirs) = (0, 0, Vec::new());
                for e in tree.list(*dir).map_err(reader)? {
                    count += 1;
                    match e.kind {
                        DT_DIR => dirs.push(e.id),
                        DT_REG => bytes += tree.stat(e.id).map_err(reader)?.size,
                        _ => {}
                    }
                }
                Ok((bytes, count, dirs))
            })
            .collect();
        level = Vec::new();
        for f in found {
            let (b, c, dirs) = f?;
            bytes += b;
            entries += c;
            level.extend(dirs);
        }
    }
    Ok((bytes, entries))
}

/// Copy `/subdir` of the image's first volume to `dst` (made; it must be absent or empty) and write tree-copy's meta
/// file to `meta_path`.
pub fn copy(image: &str, subdir: &str, dst: &str, meta_path: &str) -> Result<Copied, Fail> {
    let tree = oneios_image::apfs_tools::open(image, None, Pick::Block0).map_err(reader)?;
    let top_id = tree.find(subdir).map_err(reader)?.ok_or_else(|| format!("{image}: the OS volume has no /{subdir}"))?;
    if std::fs::read_dir(dst).is_ok_and(|mut d| d.next().is_some()) {
        return Err(format!("{dst}: not empty"));
    }
    std::fs::create_dir_all(dst).map_err(|e| format!("{dst}: {e}"))?;
    let xattrs = kept_xattrs(&tree, subdir)?;
    let top = item_of(&tree, ".".into(), top_id, &xattrs)?;
    if top.kind != 'd' {
        return Err(format!("{image}: /{subdir} is not a folder"));
    }

    // tree-copy's order: a stack of folders (the last folder of a folder comes next), each folder's entries sorted
    // by name. The folders of a level are read in parallel; the order does not depend on that.
    struct Folder {
        rel: String,
        id: u64,
        held: Vec<Item>,
        below: Vec<usize>,
    }
    let mut folders = vec![Folder { rel: String::new(), id: top_id, held: Vec::new(), below: Vec::new() }];
    let mut level = vec![0usize];
    while !level.is_empty() {
        job::check().map_err(|stop| stop.to_string())?;
        let read: Vec<Result<Vec<Item>, Fail>> = level
            .par_iter()
            .map(|i| {
                let f = &folders[*i];
                let mut entries = tree.list(f.id).map_err(reader)?;
                if let Some(e) = entries.iter().find(|e| e.name.contains('/') || e.name == "." || e.name == ".." || e.name.is_empty()) {
                    return Err(format!("{image}: a name in the volume's records is not a file name: {:?}", e.name));
                }
                entries.sort_by(|a, b| a.name.as_bytes().cmp(b.name.as_bytes()));
                entries.dedup_by(|a, b| a.name == b.name);
                entries.par_iter().map(|e| item_of(&tree, if f.rel.is_empty() { e.name.clone() } else { format!("{}/{}", f.rel, e.name) }, e.id, &xattrs)).collect()
            })
            .collect();
        let mut next = Vec::new();
        for (i, held) in level.iter().zip(read) {
            let held = held?;
            for item in held.iter().filter(|item| item.kind == 'd') {
                let at = folders.len();
                next.push(at);
                folders[*i].below.push(at);
                folders.push(Folder { rel: item.rel.clone(), id: item.id, held: Vec::new(), below: Vec::new() });
            }
            folders[*i].held = held;
        }
        level = next;
    }
    let mut items: Vec<Item> = Vec::new();
    let mut skipped: Vec<String> = Vec::new();
    let mut stack = vec![0usize];
    while let Some(i) = stack.pop() {
        for item in std::mem::take(&mut folders[i].held) {
            if item.kind == '?' {
                skipped.push(item.rel);
            } else {
                items.push(item);
            }
        }
        stack.extend(folders[i].below.iter().copied());
    }

    // The folders (a folder before what it holds: the stack order has that), then the files in every thread of the
    // pool, then the links.
    let join = |rel: &str| format!("{}/{rel}", dst.trim_end_matches('/'));
    for item in items.iter().filter(|i| i.kind == 'd') {
        std::fs::DirBuilder::new().mode(0o700).create(join(&item.rel)).map_err(|e| format!("{}: {e}", join(&item.rel)))?;
    }
    let files: Vec<&Item> = items.iter().filter(|i| i.kind == 'f').collect();
    let total: u64 = files.iter().map(|i| i.size).sum();
    // (the caller said the step: its numbers only, no second `step` event)
    let progress = job::Progress::new("data", "bytes", total).items(files.len() as u64);
    let budget = Budget { left: std::sync::Mutex::new(IN_MEMORY_MAX), freed: std::sync::Condvar::new() };
    let failed = AtomicBool::new(false);
    let written: Result<Vec<u64>, Fail> = files
        .par_iter()
        .map(|item| {
            if failed.load(Ordering::Relaxed) {
                return Ok(0);
            }
            let one = || -> Result<u64, Fail> {
                job::check().map_err(|stop| stop.to_string())?;
                let held = budget.take(item.size);
                let data = tree.read_file(item.id);
                let result = data.map_err(reader).and_then(|data| write_file(&join(&item.rel), &data, item.mtime).map(|()| data.len() as u64));
                budget.give(held);
                progress.add(item.size);
                progress.item();
                result
            };
            one().inspect_err(|_| failed.store(true, Ordering::Relaxed))
        })
        .collect();
    progress.finish();
    let bytes: u64 = written?.iter().sum();
    for item in items.iter().filter(|i| i.kind == 'l') {
        let target = tree.xattr(item.id, "com.apple.fs.symlink").map_err(reader)?.ok_or_else(|| format!("{image}: /{subdir}/{}: a link without its target", item.rel))?;
        let target = &target[..target.iter().position(|b| *b == 0).unwrap_or(target.len())];
        let target = std::str::from_utf8(target).map_err(|_| format!("{image}: /{subdir}/{}: the link's target is not UTF-8", item.rel))?;
        std::os::unix::fs::symlink(target, join(&item.rel)).map_err(|e| format!("{}: {e}", join(&item.rel)))?;
    }

    let mut entries: Vec<(String, Value)> = Vec::with_capacity(items.len() + 1);
    entries.push((".".into(), top.value(true)));
    entries.extend(items.iter().map(|item| (item.rel.clone(), item.value(false))));
    let meta = Value::Dict(vec![("entries".into(), Value::Dict(entries)), ("skipped".into(), Value::List(skipped.iter().map(Value::str).collect()))]);
    std::fs::write(meta_path, json::dumps(&meta, None, false)).map_err(|e| format!("{meta_path}: {e}"))?;
    Ok(Copied { entries: items.len(), files: files.len() as u64, bytes, skipped: skipped.len() })
}

/// A file as tree-copy writes it: mode 0600, both times the source's whole seconds.
fn write_file(path: &str, data: &[u8], mtime: i64) -> Result<(), Fail> {
    use std::io::Write;
    let bad = |e: std::io::Error| format!("{path}: {e}");
    let mut f = std::fs::OpenOptions::new().write(true).create_new(true).mode(0o600).open(path).map_err(bad)?;
    f.write_all(data).map_err(bad)?;
    let times = [libc::timespec { tv_sec: mtime as libc::time_t, tv_nsec: 0 }; 2];
    // Safety: the descriptor is open and `times` holds two timespecs.
    if unsafe { libc::futimens(f.as_raw_fd(), times.as_ptr()) } != 0 {
        return Err(bad(std::io::Error::last_os_error()));
    }
    Ok(())
}

/// A count of bytes that threads take from and give back to.
struct Budget {
    left: std::sync::Mutex<u64>,
    freed: std::sync::Condvar,
}

impl Budget {
    fn take(&self, bytes: u64) -> u64 {
        let want = bytes.min(IN_MEMORY_MAX);
        let mut left = self.left.lock().unwrap();
        while *left < want {
            left = self.freed.wait(left).unwrap();
        }
        *left -= want;
        want
    }
    fn give(&self, bytes: u64) {
        *self.left.lock().unwrap() += bytes;
        self.freed.notify_all();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn base64_as_python() {
        assert_eq!(base64(b""), "");
        assert_eq!(base64(b"v"), "dg==");
        assert_eq!(base64(&[0, 1, 2]), "AAEC");
        assert_eq!(base64(b"ab"), "YWI=");
    }

    #[test]
    fn mtime_as_python() {
        assert_eq!(int_mtime(1_600_000_000_000_000_000), 1_600_000_000);
        assert_eq!(int_mtime(1_600_000_000_500_000_000), 1_600_000_000);
        assert_eq!(int_mtime(1_700_000_000_999_999_999), 1_700_000_001);
    }
}
