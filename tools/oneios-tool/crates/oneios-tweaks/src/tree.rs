//! A directory that stands for a volume of the guest, and the record of what was changed in it
//! (roottweak/tree.py).
//!
//! Paths are the guest's, relative to the volume's top (`System/Library/xpc/launchd.plist`). A symbolic link inside
//! the tree is followed inside the tree: an absolute target (`/private/var`) is taken from the tree's top, never
//! from the host. A path that would leave the tree stays in it.
//!
//! The build usually does not run as root, so owners cannot be set on the host. Every change is recorded with the
//! owner and mode the guest needs; the image build applies them when it packs the volume (`tweaks.manifest.json`).
//! As root (`serve` on the phone), owners are also set on the files.
//!
//! A file is written as `NAME.part` and renamed over NAME; the part is a `job::temp`, so a stop or an error leaves
//! none behind. Every write asks `job::check()` first and counts its bytes (`job::count`).

use crate::plist::{self, P};
use crate::Refused;
use anyhow::{bail, Context as _, Result};
use oneios_common::job;
use oneios_common::json::Value;
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use std::fs;
use std::os::unix::fs::{MetadataExt, PermissionsExt};
use std::path::{Path, PathBuf};

pub type Owner = (u32, u32);
pub const ROOT: Owner = (0, 0);
pub const MOBILE: Owner = (501, 501);
const MAX_LINKS: usize = 40;

/// What the tweaks changed: one entry per (volume, path), the last change of a path wins. Entries are kept as the
/// JSON objects `tweaks.manifest.json` holds (an earlier run's are carried over as they were read).
#[derive(Default)]
pub struct Manifest {
    pub entries: BTreeMap<(String, String), Value>,
}

/// One change for `Manifest::record`.
#[derive(Default)]
pub struct Change<'a> {
    pub owner: Option<Owner>,
    pub mode: Option<u32>,
    pub sha256: Option<(String, usize)>,
    pub target: Option<&'a str>,
}

impl Manifest {
    pub fn record(&mut self, volume: &str, path: &str, kind: &str, c: Change, tweak: Option<&str>) {
        let mut e = Value::dict();
        e.set("volume", volume);
        e.set("path", path);
        e.set("type", kind);
        if let Some((uid, gid)) = c.owner {
            e.set("uid", uid);
            e.set("gid", gid);
        }
        if let Some(mode) = c.mode {
            e.set("mode", format!("{mode:04o}"));
        }
        if let Some((sha, size)) = c.sha256 {
            e.set("sha256", sha);
            e.set("size", size);
        }
        if let Some(t) = c.target {
            e.set("target", t);
        }
        if let Some(t) = tweak.filter(|t| !t.is_empty()) {
            e.set("tweak", t);
        }
        self.entries.insert((volume.to_string(), path.to_string()), e);
    }

    /// The entries in the order of their (volume, path): Python's sort of the tuples (code points, as UTF-8 bytes).
    pub fn sorted(&self) -> Vec<Value> {
        self.entries.values().cloned().collect()
    }
}

fn is_link(p: &Path) -> bool {
    fs::symlink_metadata(p).is_ok_and(|m| m.file_type().is_symlink())
}
fn lexists(p: &Path) -> bool {
    fs::symlink_metadata(p).is_ok()
}
pub(crate) fn is_dir(p: &Path) -> bool {
    fs::metadata(p).is_ok_and(|m| m.is_dir())
}
pub(crate) fn is_file(p: &Path) -> bool {
    fs::metadata(p).is_ok_and(|m| m.is_file())
}

/// Split a guest path as tree.py does (`p not in ('', '.')`).
fn parts(path: &str) -> Vec<String> {
    path.split('/').filter(|p| !p.is_empty() && *p != ".").map(str::to_string).collect()
}

/// `os.path.dirname` of a guest path.
pub fn dirname(path: &str) -> &str {
    match path.rfind('/') {
        Some(0) => "/",
        Some(at) => path[..at].trim_end_matches('/'),
        None => "",
    }
}

/// `os.path.basename`
pub fn basename(path: &str) -> &str {
    path.rsplit('/').next().unwrap_or(path)
}

/// `os.path.normpath` of a relative guest path (`a/./b/../c` -> `a/c`).
pub fn normpath(path: &str) -> String {
    let mut out: Vec<&str> = Vec::new();
    for p in path.split('/') {
        match p {
            "" | "." => {}
            ".." if out.last().is_some_and(|l| *l != "..") => {
                out.pop();
            }
            p => out.push(p),
        }
    }
    if out.is_empty() { ".".into() } else { out.join("/") }
}

pub struct Tree {
    pub top: PathBuf,
    pub volume: &'static str,
    pub readonly: bool,
    /// the tweak now running, for the manifest
    pub tweak: String,
    root_user: bool,
    /// A volume read from its image with our APFS reader (the phone mounts nothing): read only; `top` is the
    /// image's path then, for messages.
    image: Option<oneios_image::apfs::ApfsTree>,
}

const DT_DIR: u16 = oneios_image::apfs::DT_DIR;
const DT_REG: u16 = oneios_image::apfs::DT_REG;
const DT_LNK: u16 = oneios_image::apfs::DT_LNK;

fn fail(e: oneios_image::Fail) -> anyhow::Error {
    anyhow::anyhow!("{e}")
}

impl Tree {
    pub fn new(top: &Path, volume: &'static str, readonly: bool) -> Result<Tree> {
        let real = fs::canonicalize(top).unwrap_or_else(|_| top.to_path_buf());
        if !is_dir(&real) {
            bail!(Refused(format!("{}: not a directory", top.display())));
        }
        Ok(Tree { top: real, volume, readonly, tweak: String::new(), root_user: unsafe { libc::geteuid() } == 0, image: None })
    }

    /// The volume of an image (raw APFS: `os.img`), read in place; `volume` a number or a name (default: the first).
    /// Always read only.
    pub fn from_image(image: &Path, volume: &'static str, which: Option<&str>) -> Result<Tree> {
        let text = image.to_string_lossy();
        let tree = oneios_image::apfs_tools::open(&text, which, oneios_image::apfs::Pick::Block0).map_err(|e| Refused(format!("{text}: {e}")))?;
        Ok(Tree { top: image.to_path_buf(), volume, readonly: true, tweak: String::new(), root_user: false, image: Some(tree) })
    }

    pub fn is_image(&self) -> bool {
        self.image.is_some()
    }

    /// (object, DT_ kind) of a path of names in the image, no link followed; the top is a folder.
    fn entry(&self, names: &[String]) -> Result<Option<(u64, u16)>> {
        let img = self.image.as_ref().unwrap();
        let mut at = (img.find("").map_err(fail)?.unwrap_or(2), DT_DIR);
        for name in names {
            if at.1 != DT_DIR {
                return Ok(None);
            }
            match img.list(at.0).map_err(fail)?.into_iter().rev().find(|e| e.name == *name) {
                Some(e) => at = (e.id, e.kind),
                None => return Ok(None),
            }
        }
        Ok(Some(at))
    }

    /// The target of the link at these names, None when it is not a link.
    fn link_at(&self, names: &[String]) -> Result<Option<String>> {
        if let Some(img) = &self.image {
            return Ok(match self.entry(names)? {
                Some((id, DT_LNK)) => {
                    let raw = img.xattr(id, "com.apple.fs.symlink").map_err(fail)?.unwrap_or_default();
                    Some(String::from_utf8_lossy(&raw[..raw.iter().position(|b| *b == 0).unwrap_or(raw.len())]).into_owned())
                }
                _ => None,
            });
        }
        let mut here = self.top.clone();
        here.extend(names);
        Ok(if is_link(&here) { Some(fs::read_link(&here)?.to_string_lossy().into_owned()) } else { None })
    }

    /// The names of a guest path, links followed inside the tree (tree.py's `host`).
    fn resolve(&self, path: &str, follow_last: bool) -> Result<Vec<String>> {
        let mut todo: Vec<String> = parts(path);
        todo.reverse();
        let mut done: Vec<String> = Vec::new();
        let mut links = 0;
        while let Some(part) = todo.pop() {
            if part == ".." {
                done.pop();
                continue;
            }
            done.push(part);
            if !todo.is_empty() || follow_last {
                if let Some(target) = self.link_at(&done)? {
                    done.pop();
                    links += 1;
                    if links > MAX_LINKS {
                        bail!(Refused(format!("{path}: too many symbolic links")));
                    }
                    if target.starts_with('/') {
                        done.clear();
                    }
                    let mut more = parts(&target);
                    more.reverse();
                    todo.extend(more);
                }
            }
        }
        Ok(done)
    }

    /// The kind of what a path names in an image (links followed or not), None where nothing is.
    fn kind(&self, path: &str, follow_last: bool) -> Result<Option<(u64, u16)>> {
        let names = self.resolve(path, follow_last)?;
        self.entry(&names)
    }

    fn no_host(&self) -> Result<()> {
        if self.image.is_some() {
            bail!(Refused(format!("{}: a volume read from its image has no host path (the sealed layout only reads it)", self.top.display())));
        }
        Ok(())
    }

    // ---- paths

    /// The host path of a guest path, links followed inside the tree.
    pub fn host_of(&self, path: &str, follow_last: bool) -> Result<PathBuf> {
        self.no_host()?;
        let mut out = self.top.clone();
        out.extend(self.resolve(path, follow_last)?);
        Ok(out)
    }
    pub fn host(&self, path: &str) -> Result<PathBuf> {
        self.host_of(path, true)
    }
    pub fn exists(&self, path: &str) -> Result<bool> {
        if self.image.is_some() {
            return Ok(self.kind(path, true)?.is_some_and(|(_, k)| k != DT_LNK));
        }
        Ok(fs::metadata(self.host(path)?).is_ok())
    }
    pub fn lexists(&self, path: &str) -> Result<bool> {
        if self.image.is_some() {
            return Ok(self.kind(path, false)?.is_some());
        }
        Ok(lexists(&self.host_of(path, false)?))
    }
    pub fn isdir(&self, path: &str) -> Result<bool> {
        if self.image.is_some() {
            return Ok(self.kind(path, true)?.is_some_and(|(_, k)| k == DT_DIR));
        }
        Ok(is_dir(&self.host(path)?))
    }
    pub fn isfile(&self, path: &str) -> Result<bool> {
        if self.image.is_some() {
            return Ok(self.kind(path, true)?.is_some_and(|(_, k)| k == DT_REG));
        }
        Ok(is_file(&self.host(path)?))
    }
    /// The names in a directory, sorted ([] where there is none).
    pub fn listdir(&self, path: &str) -> Result<Vec<String>> {
        if let Some(img) = &self.image {
            return Ok(match self.kind(path, true)? {
                Some((id, DT_DIR)) => {
                    let mut names: Vec<String> = img.list(id).map_err(fail)?.into_iter().map(|e| e.name).collect();
                    names.sort();
                    names.dedup();
                    names
                }
                _ => Vec::new(),
            });
        }
        let host = self.host(path)?;
        if !is_dir(&host) {
            return Ok(Vec::new());
        }
        listdir(&host)
    }

    // ---- reading

    pub fn read(&self, path: &str) -> Result<Vec<u8>> {
        if let Some(img) = &self.image {
            return match self.kind(path, true)? {
                Some((id, DT_REG)) => img.read_file(id).map_err(fail),
                _ => Err(anyhow::anyhow!("{}: {path}: not a file in the image", self.top.display())),
            };
        }
        let host = self.host(path)?;
        fs::read(&host).with_context(|| host.display().to_string())
    }

    /// The property list at `path`; `default` where there is no file. A file that is no plist is an error: it is
    /// never written over as if it were empty.
    pub fn read_plist(&self, path: &str, default: Option<P>) -> Result<Option<P>> {
        if !self.exists(path)? {
            return Ok(default);
        }
        match plist::loads(&self.read(path)?) {
            Ok(v) => Ok(Some(v)),
            Err(e) => bail!(Refused(format!("{}:{path}: not a property list ({e})", self.volume))),
        }
    }

    // ---- writing

    fn writable(&self, path: &str) -> Result<()> {
        if self.readonly {
            bail!(Refused(format!("{}:{path}: this volume is read-only in this layout", self.volume)));
        }
        Ok(())
    }

    fn own(&self, host: &Path, owner: Option<Owner>, mode: Option<u32>) -> Result<()> {
        if let Some(mode) = mode {
            if !is_link(host) {
                fs::set_permissions(host, fs::Permissions::from_mode(mode)).with_context(|| host.display().to_string())?;
            }
        }
        if let Some((uid, gid)) = owner {
            if self.root_user {
                std::os::unix::fs::lchown(host, Some(uid), Some(gid)).with_context(|| host.display().to_string())?;
            }
        }
        Ok(())
    }

    fn record(&self, m: &mut Manifest, path: &str, kind: &str, c: Change) {
        m.record(self.volume, path, kind, c, Some(&self.tweak));
    }

    /// Makes `path` and its missing parents. Only what is made here is recorded, with `owner` and `mode`; a
    /// directory that exists keeps what it has unless it is `path` itself.
    pub fn mkdir(&self, m: &mut Manifest, path: &str, owner: Owner, mode: u32) -> Result<()> {
        self.writable(path)?;
        let parts: Vec<&str> = path.split('/').filter(|p| !p.is_empty()).collect();
        for i in 1..=parts.len() {
            let sub = parts[..i].join("/");
            let host = self.host(&sub)?;
            let last = i == parts.len();
            if !is_dir(&host) {
                job::check()?;
                // a link's target inside the tree may lack parents of its own
                fs::create_dir_all(&host).with_context(|| host.display().to_string())?;
                let (o, md) = if last { (owner, mode) } else { (ROOT, 0o755) };
                self.own(&host, Some(o), Some(md))?;
                self.record(m, &sub, "dir", Change { owner: Some(o), mode: Some(md), ..Default::default() });
            } else if last {
                self.own(&host, Some(owner), Some(mode))?;
                self.record(m, &sub, "dir", Change { owner: Some(owner), mode: Some(mode), ..Default::default() });
            }
        }
        Ok(())
    }

    /// Writes a file. True when the content changed.
    pub fn write(&self, m: &mut Manifest, path: &str, data: &[u8], owner: Owner, mode: u32, parent_owner: Option<Owner>) -> Result<bool> {
        self.writable(path)?;
        job::check()?;
        let parent = dirname(path);
        if !parent.is_empty() && !self.isdir(parent)? {
            self.mkdir(m, parent, parent_owner.unwrap_or(owner), 0o755)?;
        }
        let host = self.host(path)?;
        let mut changed = true;
        if is_file(&host) && !is_link(&host) {
            changed = fs::read(&host).with_context(|| host.display().to_string())? != data;
        }
        if changed {
            let mut name = host.file_name().unwrap_or_default().to_os_string();
            name.push(".part");
            let part = job::temp(host.with_file_name(name));
            fs::write(part.path(), data).with_context(|| part.path().display().to_string())?;
            fs::rename(part.path(), &host).with_context(|| host.display().to_string())?;
            part.keep();
            job::count(data.len() as u64);
        }
        self.own(&host, Some(owner), Some(mode))?;
        let sha = format!("{:x}", Sha256::digest(data));
        self.record(m, path, "file", Change { owner: Some(owner), mode: Some(mode), sha256: Some((sha, data.len())), ..Default::default() });
        Ok(changed)
    }

    pub fn write_plist(&self, m: &mut Manifest, path: &str, value: &P, owner: Owner, mode: u32, binary: bool, parent_owner: Option<Owner>) -> Result<bool> {
        let data = if binary { plist::dumps_binary(value) } else { plist::dumps_xml(value) };
        let data = data.map_err(|e| anyhow::anyhow!("{}:{path}: {e}", self.volume))?;
        self.write(m, path, &data, owner, mode, parent_owner)
    }

    /// Sets `values` in the dictionary at `path`, keeping its other keys. The keys that changed.
    pub fn merge_plist(&self, m: &mut Manifest, path: &str, values: &[(String, P)], owner: Owner, mode: u32, binary: bool, parent_owner: Option<Owner>) -> Result<Vec<String>> {
        let have = self.read_plist(path, Some(P::dict()))?;
        let Some(mut have) = have.filter(P::is_dict) else {
            bail!(Refused(format!("{}:{path}: not a dictionary", self.volume)));
        };
        let mut changed: Vec<String> = values
            .iter()
            .filter(|(k, v)| match have.get(k) {
                None => true,
                Some(h) => !plist::py_eq(h, v) || h.kind() != v.kind(),
            })
            .map(|(k, _)| k.clone())
            .collect();
        changed.sort();
        for (k, v) in values {
            have.set(k, v.clone());
        }
        self.write_plist(m, path, &have, owner, mode, binary, parent_owner)?;
        Ok(changed)
    }

    pub fn symlink(&self, m: &mut Manifest, path: &str, target: &str, owner: Owner) -> Result<bool> {
        self.writable(path)?;
        let parent = dirname(path);
        if !parent.is_empty() && !self.isdir(parent)? {
            self.mkdir(m, parent, owner, 0o755)?;
        }
        let host = self.host_of(path, false)?;
        let changed = if is_link(&host) && fs::read_link(&host)?.as_os_str() == std::ffi::OsStr::new(target) {
            false
        } else {
            if lexists(&host) {
                bail!(Refused(format!("{}:{path}: in the way of a link to {target}", self.volume)));
            }
            std::os::unix::fs::symlink(target, &host).with_context(|| host.display().to_string())?;
            true
        };
        self.own(&host, Some(owner), None)?;
        self.record(m, path, "link", Change { owner: Some(owner), target: Some(target), ..Default::default() });
        Ok(changed)
    }

    /// Copies a host file or directory to `path`. Modes come from the source unless `mode` is given (files only):
    /// executable stays executable. Links inside a copied directory are refused.
    pub fn copy_in(&self, m: &mut Manifest, source: &Path, path: &str, owner: Owner, mode: Option<u32>) -> Result<()> {
        self.writable(path)?;
        if is_link(source) {
            bail!(Refused(format!("{}: a symbolic link is not copied", source.display())));
        }
        if is_dir(source) {
            self.mkdir(m, path, owner, 0o755)?;
            for name in listdir(source)? {
                self.copy_in(m, &source.join(&name), &format!("{path}/{name}"), owner, None)?;
            }
            return Ok(());
        }
        if !is_file(source) {
            bail!(Refused(format!("{}: not a regular file", source.display())));
        }
        let data = fs::read(source).with_context(|| source.display().to_string())?;
        let mode = mode.unwrap_or_else(|| if fs::metadata(source).map(|m| m.mode() & 0o100 != 0).unwrap_or(false) { 0o755 } else { 0o644 });
        self.write(m, path, &data, owner, mode, None)?;
        Ok(())
    }

    /// Removes a file or link. True when there was one.
    pub fn remove(&self, m: &mut Manifest, path: &str) -> Result<bool> {
        self.writable(path)?;
        let host = self.host_of(path, false)?;
        if !lexists(&host) {
            return Ok(false);
        }
        if is_dir(&host) && !is_link(&host) {
            bail!(Refused(format!("{}:{path}: a directory is not removed", self.volume)));
        }
        fs::remove_file(&host).with_context(|| host.display().to_string())?;
        self.record(m, path, "removed", Change::default());
        Ok(true)
    }
}

/// `sorted(os.listdir(dir))`: names that are not UTF-8 are refused (Python would sort their escapes elsewhere).
pub fn listdir(dir: &Path) -> Result<Vec<String>> {
    let mut names = Vec::new();
    for e in fs::read_dir(dir).with_context(|| dir.display().to_string())? {
        let name = e?.file_name();
        match name.into_string() {
            Ok(n) => names.push(n),
            Err(n) => bail!(Refused(format!("{}: a name that is not UTF-8 ({n:?})", dir.display()))),
        }
    }
    names.sort();
    Ok(names)
}

/// One folder of `os.walk(top)` (top-down, links not followed into): its sub-folders (a link to a folder is one)
/// and its other names, each sorted.
pub fn walk_level(dir: &Path) -> Result<(Vec<String>, Vec<String>)> {
    let (mut dirs, mut files) = (Vec::new(), Vec::new());
    for name in listdir(dir)? {
        if is_dir(&dir.join(&name)) {
            dirs.push(name);
        } else {
            files.push(name);
        }
    }
    Ok((dirs, files))
}

/// Whether os.walk goes into this folder: not a link.
pub fn walk_into(p: &Path) -> bool {
    !is_link(p)
}
