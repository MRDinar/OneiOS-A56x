//! A raw APFS image made from folders, for tests of the tools that read one (no Apple tool, no root, no mount).
//! `oneios-tool debug mkapfs` (crate oneios-testkit, which includes this file) and the crates' own tests
//! (included with `#[path]`, std only). It was tests/ioslog/mkapfs.py until 2026-11-21; the same bytes for the
//! same folders (tools/oneios-tool/README.md "debug").
//!
//!   oneios-tool debug mkapfs OUT.img NAME=FOLDER [NAME=FOLDER ...] [--stale-block0]
//!
//! One volume per NAME=FOLDER, in this order (an empty FOLDER part, `NAME=`, makes an empty volume). The volumes
//! are laid out as the phone's Data store is: unencrypted, case-sensitive with hashed names, no role, a virtual
//! file-system tree behind the volume's object map, regular files, folders and symbolic links, files over 8
//! blocks in two extents. Checksums, object headers, the checkpoint areas and the name hashes are real, so
//! apfs-fuse reads the image too: tests/ioslog/compare.sh checks this writer against apfs-fuse before it trusts
//! the image as an input.
//!
//! --stale-block0: block 0 holds an older superblock (transaction 4) in which every volume is empty; the newest
//! checkpoint (transaction 5) holds the files. A store copied after use looks like this.
//!
//! Not written: the space manager's bitmaps, the reaper, snapshots, the extent reference tree, xattrs other than
//! a link's target, compression, encryption. Names must be ASCII (their hash needs no Unicode tables then).
#![allow(dead_code)]

use std::os::unix::fs::MetadataExt;
use std::path::Path;

pub const USAGE: &str = "usage: oneios-tool debug mkapfs OUT.img NAME=FOLDER [NAME=FOLDER ...] [--stale-block0]
  One volume per NAME=FOLDER, in this order (NAME= makes an empty volume). --stale-block0: block 0 holds an older
  superblock (transaction 4) in which every volume is empty; the newest checkpoint (transaction 5) holds the files.";

const BLOCK: usize = 4096;
const PHYSICAL: u32 = 0x4000_0000;
const EPHEMERAL: u32 = 0x8000_0000;
const T_NXSB: u32 = 1;
const T_BTREE: u32 = 2;
const T_NODE: u32 = 3;
const T_SPACEMAN: u32 = 5;
const T_OMAP: u32 = 0xB;
const T_CPMAP: u32 = 0xC;
const T_FS: u32 = 0xD;
const T_FSTREE: u32 = 0xE;

fn put(d: &mut [u8], at: usize, bytes: &[u8]) {
    d[at..at + bytes.len()].copy_from_slice(bytes);
}
fn p16(d: &mut [u8], at: usize, v: u16) {
    put(d, at, &v.to_le_bytes())
}
fn p32(d: &mut [u8], at: usize, v: u32) {
    put(d, at, &v.to_le_bytes())
}
fn p64(d: &mut [u8], at: usize, v: u64) {
    put(d, at, &v.to_le_bytes())
}

pub fn fletcher64(block: &[u8]) -> u64 {
    let (mut a, mut b) = (0u64, 0u64);
    for word in block[8..].chunks_exact(4) {
        a = (a + u32::from_le_bytes(word.try_into().unwrap()) as u64) % 0xFFFF_FFFF;
        b = (b + a) % 0xFFFF_FFFF;
    }
    let low = 0xFFFF_FFFF - (a + b) % 0xFFFF_FFFF;
    let high = 0xFFFF_FFFF - (a + low) % 0xFFFF_FFFF;
    high << 32 | low
}

/// j_drec_hashed_key_t's name_len_and_hash: CRC-32C (start 0xFFFFFFFF, not inverted) of the name's UTF-32 code
/// points, its low 22 bits above the 10 bits of the length (with the NUL).
pub fn name_hash(name: &str) -> u32 {
    let mut crc = 0xFFFF_FFFFu32;
    for c in name.chars() {
        for byte in (c as u32).to_le_bytes() {
            crc ^= byte as u32;
            for _ in 0..8 {
                crc = if crc & 1 != 0 { (crc >> 1) ^ 0x82F6_3B78 } else { crc >> 1 };
            }
        }
    }
    (crc & 0x3F_FFFF) << 10 | (name.len() as u32 + 1)
}

/// SHA-1 (FIPS 180-4), for uuid5 only.
fn sha1(message: &[u8]) -> [u8; 20] {
    let mut h: [u32; 5] = [0x6745_2301, 0xEFCD_AB89, 0x98BA_DCFE, 0x1032_5476, 0xC3D2_E1F0];
    let mut m = message.to_vec();
    m.push(0x80);
    while m.len() % 64 != 56 {
        m.push(0);
    }
    m.extend_from_slice(&((message.len() as u64) * 8).to_be_bytes());
    for chunk in m.chunks_exact(64) {
        let mut w = [0u32; 80];
        for i in 0..16 {
            w[i] = u32::from_be_bytes(chunk[4 * i..4 * i + 4].try_into().unwrap());
        }
        for i in 16..80 {
            w[i] = (w[i - 3] ^ w[i - 8] ^ w[i - 14] ^ w[i - 16]).rotate_left(1);
        }
        let [mut a, mut b, mut c, mut d, mut e] = h;
        for (i, wi) in w.iter().enumerate() {
            let (f, k) = match i {
                0..=19 => ((b & c) | (!b & d), 0x5A82_7999),
                20..=39 => (b ^ c ^ d, 0x6ED9_EBA1),
                40..=59 => ((b & c) | (b & d) | (c & d), 0x8F1B_BCDC),
                _ => (b ^ c ^ d, 0xCA62_C1D6),
            };
            let t = a.rotate_left(5).wrapping_add(f).wrapping_add(e).wrapping_add(k).wrapping_add(*wi);
            e = d;
            d = c;
            c = b.rotate_left(30);
            b = a;
            a = t;
        }
        for (x, y) in h.iter_mut().zip([a, b, c, d, e]) {
            *x = x.wrapping_add(y);
        }
    }
    let mut out = [0u8; 20];
    for (i, x) in h.iter().enumerate() {
        out[4 * i..4 * i + 4].copy_from_slice(&x.to_be_bytes());
    }
    out
}

/// Python's `uuid.uuid5(uuid.NAMESPACE_DNS, name).bytes`.
pub fn uuid5_dns(name: &str) -> [u8; 16] {
    let mut input = vec![0x6b, 0xa7, 0xb8, 0x10, 0x9d, 0xad, 0x11, 0xd1, 0x80, 0xb4, 0x00, 0xc0, 0x4f, 0xd4, 0x30, 0xc8];
    input.extend_from_slice(name.as_bytes());
    let digest = sha1(&input);
    let mut u: [u8; 16] = digest[..16].try_into().unwrap();
    u[6] = (u[6] & 0x0F) | 0x50;
    u[8] = (u[8] & 0x3F) | 0x80;
    u
}

struct Image {
    data: Vec<u8>,
    /// blocks that carry an object header (checksummed at the end)
    objects: Vec<usize>,
}

impl Image {
    fn alloc(&mut self, count: usize) -> usize {
        let first = self.data.len() / BLOCK;
        self.data.resize(self.data.len() + BLOCK * count, 0);
        first
    }
    fn obj(&mut self, block: usize, oid: u64, xid: u64, kind: u32, subtype: u32) -> usize {
        let at = block * BLOCK;
        p64(&mut self.data, at + 8, oid);
        p64(&mut self.data, at + 16, xid);
        p32(&mut self.data, at + 24, kind);
        p32(&mut self.data, at + 28, subtype);
        self.objects.push(block);
        at
    }
    fn finish(&mut self) {
        for &block in &self.objects {
            let at = block * BLOCK;
            let sum = fletcher64(&self.data[at..at + BLOCK]);
            p64(&mut self.data, at, sum);
        }
    }
}

type Item = (Vec<u8>, Vec<u8>);

/// Write a B-tree of (key, value) items (sorted). new_node() -> (block, oid) for a node; a child is named by its
/// oid. fixed: (key size, value size) of an object map, None for variable entries. Returns the root's (block, oid).
fn btree(
    image: &mut Image,
    xid: u64,
    subtype: u32,
    items: Vec<Item>,
    fixed: Option<(u32, u32)>,
    new_node: &mut dyn FnMut(&mut Image) -> (usize, u64),
) -> (usize, u64) {
    let toc_size = if fixed.is_some() { 4 } else { 8 };
    // Split one level's items into nodes. Every node is filled as if it were the root (40 bytes less): the root
    // is not known yet.
    let pack = |level_items: Vec<Item>| -> Vec<Vec<Item>> {
        let room = BLOCK - 56 - 40;
        let (mut nodes, mut current, mut used): (Vec<Vec<Item>>, Vec<Item>, usize) = (Vec::new(), Vec::new(), 0);
        for (key, val) in level_items {
            let need = toc_size + key.len() + val.len();
            if !current.is_empty() && used + need > room {
                nodes.push(std::mem::take(&mut current));
                used = 0;
            }
            used += need;
            current.push((key, val));
        }
        nodes.push(current);
        nodes
    };
    let write = |image: &mut Image, node_items: &[Item], level: u16, root: bool, block: usize, oid: u64| {
        let kind = (if root { T_BTREE } else { T_NODE }) | if fixed.is_some() { PHYSICAL } else { 0 };
        let at = image.obj(block, oid, xid, kind, subtype);
        let flags = (if root { 1 } else { 0 }) | (if level == 0 { 2 } else { 0 }) | if fixed.is_some() { 4 } else { 0 };
        let table = toc_size * node_items.len();
        let d = &mut image.data;
        p16(d, at + 32, flags);
        p16(d, at + 34, level);
        p32(d, at + 36, node_items.len() as u32);
        p16(d, at + 40, 0);
        p16(d, at + 42, table as u16);
        let (keys, vals) = (at + 56 + table, at + BLOCK - if root { 40 } else { 0 });
        let (mut k_at, mut v_at) = (0usize, 0usize);
        for (i, (key, val)) in node_items.iter().enumerate() {
            v_at += val.len();
            if fixed.is_some() {
                p16(d, at + 56 + 4 * i, k_at as u16);
                p16(d, at + 56 + 4 * i + 2, v_at as u16);
            } else {
                p16(d, at + 56 + 8 * i, k_at as u16);
                p16(d, at + 56 + 8 * i + 2, key.len() as u16);
                p16(d, at + 56 + 8 * i + 4, v_at as u16);
                p16(d, at + 56 + 8 * i + 6, val.len() as u16);
            }
            put(d, keys + k_at, key);
            put(d, vals - v_at, val);
            k_at += key.len();
        }
        // free space: between the keys and the values
        p16(d, at + 44, k_at as u16);
        p16(d, at + 46, ((vals - v_at) - (keys + k_at)) as u16);
        p16(d, at + 48, 0xFFFF);
        p16(d, at + 50, 0);
        p16(d, at + 52, 0xFFFF);
        p16(d, at + 54, 0);
    };

    let count = items.len() as u64;
    let longest_key = items.iter().map(|(k, _)| k.len()).max().unwrap_or(0) as u32;
    let longest_val = items.iter().map(|(_, v)| v.len()).max().unwrap_or(0) as u32;
    let (mut level, mut level_items, mut total_nodes) = (0u16, items, 0u64);
    loop {
        let nodes = pack(level_items);
        total_nodes += nodes.len() as u64;
        if nodes.len() == 1 {
            let (block, oid) = new_node(image);
            write(image, &nodes[0], level, true, block, oid);
            let at = block * BLOCK + BLOCK - 40;
            let (key_size, val_size) = fixed.unwrap_or((0, 0));
            let d = &mut image.data;
            for (i, v) in [0, BLOCK as u32, key_size, val_size, longest_key, longest_val].into_iter().enumerate() {
                p32(d, at + 4 * i, v);
            }
            p64(d, at + 24, count);
            p64(d, at + 32, total_nodes);
            return (block, oid);
        }
        let mut parents = Vec::new();
        for node_items in &nodes {
            let (block, oid) = new_node(image);
            write(image, node_items, level, false, block, oid);
            parents.push((node_items[0].0.clone(), oid.to_le_bytes().to_vec()));
        }
        level += 1;
        level_items = parents;
    }
}

/// An object map of {oid: block} (sorted by oid); returns its block.
fn omap(image: &mut Image, xid: u64, mappings: &std::collections::BTreeMap<u64, usize>) -> usize {
    let block = image.alloc(1);
    let items = mappings
        .iter()
        .map(|(&oid, &paddr)| {
            let mut key = oid.to_le_bytes().to_vec();
            key.extend_from_slice(&xid.to_le_bytes());
            let mut val = 0u32.to_le_bytes().to_vec();
            val.extend_from_slice(&(BLOCK as u32).to_le_bytes());
            val.extend_from_slice(&(paddr as u64).to_le_bytes());
            (key, val)
        })
        .collect();
    let mut new_node = |image: &mut Image| {
        let b = image.alloc(1);
        (b, b as u64)
    };
    let (root, _) = btree(image, xid, T_OMAP, items, Some((16, 16)), &mut new_node);
    let at = image.obj(block, block as u64, xid, T_OMAP | PHYSICAL, 0);
    let d = &mut image.data;
    p32(d, at + 32, 0);
    p32(d, at + 36, 0);
    p32(d, at + 40, T_BTREE | PHYSICAL);
    p32(d, at + 44, T_BTREE | PHYSICAL);
    p64(d, at + 48, root as u64);
    block
}

/// A j_inode_val_t with the name and, for a file, the data stream as extended fields.
fn inode(parent: u64, oid: u64, mode: u16, name: &str, size: Option<u64>, children: i32, mtime: u64) -> Vec<u8> {
    let mut name_field = name.as_bytes().to_vec();
    name_field.push(0);
    let mut fields: Vec<(u8, u8, Vec<u8>)> = vec![(4, 2, name_field)];
    if let Some(size) = size {
        let mut d = Vec::new();
        for v in [size, size.div_ceil(BLOCK as u64) * BLOCK as u64, 0, size, 0] {
            d.extend_from_slice(&v.to_le_bytes());
        }
        fields.push((8, 0x20, d));
    }
    let mut table = Vec::new();
    let mut values = Vec::new();
    for (t, f, d) in &fields {
        table.extend_from_slice(&[*t, *f]);
        table.extend_from_slice(&(d.len() as u16).to_le_bytes());
        values.extend_from_slice(d);
        values.resize(values.len() + (8 - d.len() % 8) % 8, 0);
    }
    let mut out = Vec::new();
    for v in [parent, oid, mtime, mtime, mtime, mtime, 0x8000] {
        out.extend_from_slice(&v.to_le_bytes());
    }
    let is_dir = mode & 0o170000 == 0o040000;
    out.extend_from_slice(&(if is_dir { children } else { 1 }).to_le_bytes());
    for v in [0u32, 0, 0, 0, 0] {
        // protection class, write generation, BSD flags, uid, gid
        out.extend_from_slice(&v.to_le_bytes());
    }
    out.extend_from_slice(&mode.to_le_bytes());
    out.extend_from_slice(&0u16.to_le_bytes());
    out.extend_from_slice(&0u64.to_le_bytes());
    out.extend_from_slice(&(fields.len() as u16).to_le_bytes());
    out.extend_from_slice(&(values.len() as u16).to_le_bytes());
    out.extend_from_slice(&table);
    out.extend_from_slice(&values);
    out
}

/// (object, type, sort bytes, key bytes after the header, value)
type Record = (u64, u64, Vec<u8>, Vec<u8>, Vec<u8>);

fn drec(parent: u64, name: &str, child: u64, added: u64, kind: u16) -> Record {
    let h = name_hash(name);
    let mut raw = name.as_bytes().to_vec();
    raw.push(0);
    let mut rest = h.to_le_bytes().to_vec();
    rest.extend_from_slice(&raw);
    let mut val = child.to_le_bytes().to_vec();
    val.extend_from_slice(&added.to_le_bytes());
    val.extend_from_slice(&kind.to_le_bytes());
    let mut order = (h & 0xFFFF_FC00).to_be_bytes().to_vec();
    order.extend_from_slice(&raw);
    (parent, 9, order, rest, val)
}

fn walk(image: &mut Image, records: &mut Vec<Record>, ids: &mut u64, path: &Path, parent: u64, oid: u64, entry_name: &str) -> Result<(), String> {
    let st = std::fs::symlink_metadata(path).map_err(|e| format!("mkapfs: {}: {e}", path.display()))?;
    let mtime = (st.mtime() as i128 * 1_000_000_000 + st.mtime_nsec() as i128) as u64;
    let ft = st.file_type();
    if ft.is_dir() {
        let mut names: Vec<String> = Vec::new();
        for e in std::fs::read_dir(path).map_err(|e| format!("mkapfs: {}: {e}", path.display()))? {
            let e = e.map_err(|e| format!("mkapfs: {}: {e}", path.display()))?;
            names.push(e.file_name().to_string_lossy().into_owned());
        }
        names.sort();
        records.push((oid, 3, Vec::new(), Vec::new(), inode(parent, oid, 0o40755, entry_name, None, names.len() as i32, mtime)));
        for child in &names {
            if child.chars().any(|c| (c as u32) > 126 || (c as u32) < 32) {
                return Err(format!("mkapfs: {}/{child}: only ASCII names", path.display()));
            }
            let cid = *ids;
            *ids += 1;
            let cpath = path.join(child);
            let cft = std::fs::symlink_metadata(&cpath).map_err(|e| format!("mkapfs: {}: {e}", cpath.display()))?.file_type();
            let kind = if cft.is_dir() { 4 } else if cft.is_symlink() { 10 } else { 8 };
            records.push(drec(oid, child, cid, mtime, kind));
            walk(image, records, ids, &cpath, oid, cid, child)?;
        }
    } else if ft.is_symlink() {
        let mut target = std::fs::read_link(path).map_err(|e| format!("mkapfs: {}: {e}", path.display()))?.into_os_string().into_encoded_bytes();
        target.push(0);
        records.push((oid, 3, Vec::new(), Vec::new(), inode(parent, oid, 0o120755, entry_name, None, 0, mtime)));
        let xname = b"com.apple.fs.symlink\0".to_vec();
        let mut rest = (xname.len() as u16).to_le_bytes().to_vec();
        rest.extend_from_slice(&xname);
        let mut val = 2u16.to_le_bytes().to_vec();
        val.extend_from_slice(&(target.len() as u16).to_le_bytes());
        val.extend_from_slice(&target);
        records.push((oid, 4, xname, rest, val));
    } else if ft.is_file() {
        let data = std::fs::read(path).map_err(|e| format!("mkapfs: {}: {e}", path.display()))?;
        records.push((oid, 3, Vec::new(), Vec::new(), inode(parent, oid, 0o100644, entry_name, Some(data.len() as u64), 0, mtime)));
        let blocks = data.len().div_ceil(BLOCK);
        // over 8 blocks: two extents with a block of something else between them
        let parts = if blocks <= 8 { vec![(0, blocks)] } else { vec![(0, blocks / 2), (blocks / 2, blocks - blocks / 2)] };
        for (n, (first, count)) in parts.into_iter().enumerate() {
            if n > 0 {
                image.alloc(1);
            }
            let at = image.alloc(count);
            let chunk = &data[(first * BLOCK).min(data.len())..((first + count) * BLOCK).min(data.len())];
            put(&mut image.data, at * BLOCK, chunk);
            let offset = (first * BLOCK) as u64;
            let mut val = ((count * BLOCK) as u64).to_le_bytes().to_vec();
            val.extend_from_slice(&(at as u64).to_le_bytes());
            val.extend_from_slice(&0u64.to_le_bytes());
            records.push((oid, 8, offset.to_be_bytes().to_vec(), offset.to_le_bytes().to_vec(), val));
        }
    }
    Ok(())
}

/// One volume holding folder's tree (None: an empty volume). Returns its superblock's block.
fn volume(image: &mut Image, xid: u64, oid: u64, index: u32, name: &str, folder: Option<&Path>, next_oid: &mut u64) -> Result<usize, String> {
    let mut records: Vec<Record> = Vec::new();
    let mut ids = 16u64;
    match folder {
        Some(folder) => walk(image, &mut records, &mut ids, folder, 1, 2, "root")?,
        None => records.push((2, 3, Vec::new(), Vec::new(), inode(1, 2, 0o40755, "root", None, 0, 0))),
    }
    records.push((3, 3, Vec::new(), Vec::new(), inode(1, 3, 0o40755, "private-dir", None, 0, 0)));
    // the root folder and the private folder are entries of object 1 (the root's parent)
    records.push(drec(1, "root", 2, 0, 4));
    records.push(drec(1, "private-dir", 3, 0, 4));
    records.sort_by(|a, b| (a.0, a.1, &a.2).cmp(&(b.0, b.1, &b.2)));
    let items: Vec<Item> = records
        .into_iter()
        .map(|(obj, kind, _, rest, val)| {
            let mut key = (obj | kind << 60).to_le_bytes().to_vec();
            key.extend_from_slice(&rest);
            (key, val)
        })
        .collect();

    let mut nodes = std::collections::BTreeMap::new();
    let mut new_node = |image: &mut Image| {
        let block = image.alloc(1);
        let node_oid = *next_oid;
        *next_oid += 1;
        nodes.insert(node_oid, block);
        (block, node_oid)
    };
    let (_, root_oid) = btree(image, xid, T_FSTREE, items, None, &mut new_node);
    let vol_omap = omap(image, xid, &nodes);

    let block = image.alloc(1);
    let at = image.obj(block, oid, xid, T_FS, 0);
    let d = &mut image.data;
    put(d, at + 32, b"APSB");
    p32(d, at + 36, index);
    p64(d, at + 56, 0x8); // names hashed, normalization-insensitive
    p32(d, at + 116, T_BTREE);
    p32(d, at + 120, T_BTREE | PHYSICAL);
    p32(d, at + 124, T_BTREE | PHYSICAL);
    p64(d, at + 128, vol_omap as u64);
    p64(d, at + 136, root_oid);
    p64(d, at + 176, ids);
    put(d, at + 240, &uuid5_dns(&format!("oneios-test-{name}")));
    p64(d, at + 264, 1); // unencrypted
    put(d, at + 704, name.as_bytes());
    Ok(block)
}

/// Write the image; volumes: (NAME, FOLDER), FOLDER "" for an empty volume. Returns the line the command prints.
pub fn make(out: &Path, volumes: &[(String, String)], stale: bool) -> Result<String, String> {
    let mut image = Image { data: Vec::new(), objects: Vec::new() };
    image.alloc(1); // block 0
    let desc = image.alloc(4); // two checkpoints: (map, superblock) each
    let data_area = image.alloc(4);
    let spaceman = data_area;
    image.obj(spaceman, 1024, 5, T_SPACEMAN | EPHEMERAL, 0);
    let mut next_oid = 1100u64;

    let mut checkpoint = |image: &mut Image, slot: usize, xid: u64, with_files: bool| -> Result<Vec<u8>, String> {
        let mut mappings = std::collections::BTreeMap::new();
        for (index, (name, folder)) in volumes.iter().enumerate() {
            let vol_oid = 1026 + index as u64;
            let folder = (with_files && !folder.is_empty()).then(|| Path::new(folder.as_str()));
            mappings.insert(vol_oid, volume(image, xid, vol_oid, index as u32, name, folder, &mut next_oid)?);
        }
        let container_omap = omap(image, xid, &mappings);
        let cpm = desc + 2 * slot;
        let at = image.obj(cpm, cpm as u64, xid, T_CPMAP | PHYSICAL, 0);
        let d = &mut image.data;
        p32(d, at + 32, 1);
        p32(d, at + 36, 1);
        for (i, v) in [T_SPACEMAN | EPHEMERAL, 0, BLOCK as u32, 0].into_iter().enumerate() {
            p32(d, at + 40 + 4 * i, v);
        }
        p64(d, at + 56, 0);
        p64(d, at + 64, 1024);
        p64(d, at + 72, spaceman as u64);
        let mut sb = vec![0u8; BLOCK];
        put(&mut sb, 32, b"NXSB");
        p32(&mut sb, 36, BLOCK as u32);
        p64(&mut sb, 40, 0); // the block count is set at the end
        p64(&mut sb, 64, 2);
        put(&mut sb, 72, &uuid5_dns("oneios-test-container"));
        p64(&mut sb, 88, next_oid + 1000);
        p64(&mut sb, 96, xid + 1);
        p32(&mut sb, 104, 4);
        p32(&mut sb, 108, 4);
        p64(&mut sb, 112, desc as u64);
        p64(&mut sb, 120, data_area as u64);
        for (i, v) in [0u32, 0, 2 * slot as u32, 2, 0, 1].into_iter().enumerate() {
            p32(&mut sb, 128 + 4 * i, v);
        }
        p64(&mut sb, 152, 1024);
        p64(&mut sb, 160, container_omap as u64);
        p64(&mut sb, 168, 0);
        p32(&mut sb, 176, 0);
        p32(&mut sb, 180, 100);
        for index in 0..volumes.len() {
            p64(&mut sb, 184 + 8 * index, 1026 + index as u64);
        }
        Ok(sb)
    };

    let old = if stale { Some(checkpoint(&mut image, 0, 4, false)?) } else { None };
    let new_slot = if stale { 1 } else { 0 };
    let new = checkpoint(&mut image, new_slot, 5, true)?;
    let count = image.data.len() / BLOCK;
    let mut first = None;
    for (slot, sb, xid) in [(0, old, 4u64), (new_slot, Some(new), 5)] {
        let Some(mut sb) = sb else { continue };
        p64(&mut sb, 40, count as u64);
        let block = desc + 2 * slot + 1;
        put(&mut image.data, block * BLOCK, &sb);
        image.obj(block, 1, xid, T_NXSB | EPHEMERAL, 0);
        if first.is_none() {
            first = Some(sb);
        }
    }
    put(&mut image.data, 0, &first.unwrap());
    image.obj(0, 1, if stale { 4 } else { 5 }, T_NXSB | EPHEMERAL, 0);
    image.finish();
    std::fs::write(out, &image.data).map_err(|e| format!("mkapfs: {}: {e}", out.display()))?;
    Ok(format!("{}: {count} blocks, {} volumes{}", out.display(), volumes.len(), if stale { ", block 0 one transaction behind" } else { "" }))
}

/// The command line: OUT.img NAME=FOLDER... [--stale-block0]. Ok(line) or Err(message for stderr).
pub fn run(argv: &[String]) -> Result<String, String> {
    let stale = argv.iter().any(|a| a == "--stale-block0");
    let args: Vec<&String> = argv.iter().filter(|a| *a != "--stale-block0").collect();
    if args.len() < 2 || args[1..].iter().any(|a| !a.contains('=')) {
        return Err(USAGE.to_string());
    }
    let volumes: Vec<(String, String)> = args[1..]
        .iter()
        .map(|a| {
            let (n, f) = a.split_once('=').unwrap();
            (n.to_string(), f.to_string())
        })
        .collect();
    make(Path::new(args[0].as_str()), &volumes, stale)
}
