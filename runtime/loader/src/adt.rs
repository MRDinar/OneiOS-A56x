// SPDX-License-Identifier: MIT
//! Apple device tree (ADT) in memory: build, parse, edit, flatten (adt.c).
//!
//! Format (as XNU's pexpert/gen/device_tree.c reads it; the same layout iOSPlus10's external/slop/tools/adt.py and
//! UniXNU's make-*-adt.py scripts write):
//! - node = { u32 nprops, u32 nchildren, props..., children... }
//! - prop = { char name[32], u32 length (bit 31: placeholder flag), value padded to 4 bytes }
//!
//! Every node's first property is "name". Names and values are bytes: a name is what precedes the NUL of its 32-byte
//! field, so `&str` and `&[u8]` both name a property.

use crate::Error;
use std::io::{self, Write};
use xh_core::bytes::le;

/// The property name field, NUL padded (31 characters at most).
pub const NAME_MAX: usize = 32;

/// One property: its name (without the NUL), its value, and the placeholder flag of the flat form.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Prop {
    pub name: Vec<u8>,
    pub value: Vec<u8>,
    pub placeholder: bool,
}

impl Prop {
    /// The value as a u32, when it is 4 bytes.
    pub fn u32(&self) -> Option<u32> {
        Some(u32::from_le_bytes(self.value.as_slice().try_into().ok()?))
    }

    /// The value as a u64, when it is 8 bytes.
    pub fn u64(&self) -> Option<u64> {
        Some(u64::from_le_bytes(self.value.as_slice().try_into().ok()?))
    }

    /// The value as a NUL-terminated string (without its NUL), when it is one: what a C reader of the value sees.
    pub fn str(&self) -> Option<&str> {
        let (last, text) = self.value.split_last()?;
        if *last != 0 {
            return None;
        }
        std::str::from_utf8(xh_core::text::until_nul(text)).ok()
    }

    /// The name as text (lossy: names are ASCII in every tree the loader meets).
    pub fn name_str(&self) -> std::borrow::Cow<'_, str> {
        String::from_utf8_lossy(&self.name)
    }
}

/// A node: its properties in order (the first is "name") and its children in order.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Node {
    pub props: Vec<Prop>,
    pub children: Vec<Node>,
}

const fn padded(length: usize) -> usize {
    (length + 3) & !3
}

impl Node {
    /// A node with its "name".
    pub fn new(name: &str) -> Node {
        let mut n = Node::default();
        n.set_str("name", name).expect("a node name fits a property");
        n
    }

    /// The node's name, "" for a node whose name is not a NUL-terminated string.
    pub fn name(&self) -> &[u8] {
        match self.get("name") {
            Some(p) if p.value.last() == Some(&0) => {
                let end = p.value.iter().position(|b| *b == 0).unwrap();
                &p.value[..end]
            }
            _ => b"",
        }
    }

    pub fn get(&self, name: impl AsRef<[u8]>) -> Option<&Prop> {
        let name = name.as_ref();
        self.props.iter().find(|p| p.name == name)
    }

    /// Sets (replaces or appends) a property. Fails for a name longer than 31 characters or a value of 2 GiB or more.
    pub fn set(&mut self, name: impl AsRef<[u8]>, value: impl AsRef<[u8]>) -> Result<(), Error> {
        let (name, value) = (name.as_ref(), value.as_ref());
        if name.len() >= NAME_MAX || name.contains(&0) {
            return Err(Error("device tree property name longer than 31 characters"));
        }
        if value.len() > 0x7fff_ffff {
            return Err(Error("device tree property value too long"));
        }
        match self.props.iter_mut().find(|p| p.name == name) {
            Some(p) => p.value = value.to_vec(),
            None => self.props.push(Prop { name: name.to_vec(), value: value.to_vec(), placeholder: false }),
        }
        Ok(())
    }

    pub fn set_u32(&mut self, name: impl AsRef<[u8]>, value: u32) -> Result<(), Error> {
        self.set(name, value.to_le_bytes())
    }

    pub fn set_u64(&mut self, name: impl AsRef<[u8]>, value: u64) -> Result<(), Error> {
        self.set(name, value.to_le_bytes())
    }

    /// A string property, with its NUL.
    pub fn set_str(&mut self, name: impl AsRef<[u8]>, value: &str) -> Result<(), Error> {
        let mut v = Vec::with_capacity(value.len() + 1);
        v.extend_from_slice(value.as_bytes());
        v.push(0);
        self.set(name, v)
    }

    /// Removes a property; false if there is none.
    pub fn unset(&mut self, name: impl AsRef<[u8]>) -> bool {
        let name = name.as_ref();
        match self.props.iter().position(|p| p.name == name) {
            Some(i) => {
                self.props.remove(i);
                true
            }
            None => false,
        }
    }

    /// Direct child by name.
    pub fn child(&self, name: impl AsRef<[u8]>) -> Option<&Node> {
        let name = name.as_ref();
        self.children.iter().find(|c| c.name() == name)
    }

    pub fn child_mut(&mut self, name: impl AsRef<[u8]>) -> Option<&mut Node> {
        let name = name.as_ref();
        self.children.iter_mut().find(|c| c.name() == name)
    }

    /// The direct child of that name, created (empty but for its name) if there is none.
    pub fn add(&mut self, name: &str) -> &mut Node {
        match self.children.iter().position(|c| c.name() == name.as_bytes()) {
            Some(i) => &mut self.children[i],
            None => {
                self.children.push(Node::new(name));
                self.children.last_mut().unwrap()
            }
        }
    }

    /// Removes a direct child and gives it back; None if there is none.
    pub fn remove(&mut self, name: impl AsRef<[u8]>) -> Option<Node> {
        let name = name.as_ref();
        let i = self.children.iter().position(|c| c.name() == name)?;
        Some(self.children.remove(i))
    }

    /// The parts of a '/'-separated path (empty parts skipped); None for a part of 64 bytes or more.
    fn parts(path: &str) -> Option<Vec<&str>> {
        let parts: Vec<&str> = path.split('/').filter(|p| !p.is_empty()).collect();
        parts.iter().all(|p| p.len() < 64).then_some(parts)
    }

    /// The node at a '/'-separated path below this one ("chosen/memory-map").
    pub fn find(&self, path: &str) -> Option<&Node> {
        Self::parts(path)?.into_iter().try_fold(self, |n, part| n.child(part))
    }

    pub fn find_mut(&mut self, path: &str) -> Option<&mut Node> {
        Self::parts(path)?.into_iter().try_fold(self, |n, part| n.child_mut(part))
    }

    /// The node at a path, every missing node on the way created. None only for a part of 64 bytes or more.
    pub fn path(&mut self, path: &str) -> Option<&mut Node> {
        Some(Self::parts(path)?.into_iter().fold(self, |n, part| n.add(part)))
    }

    pub fn flat_size(&self) -> usize {
        8 + self.props.iter().map(|p| 36 + padded(p.value.len())).sum::<usize>()
            + self.children.iter().map(Node::flat_size).sum::<usize>()
    }

    /// Writes the flat form, exactly [`Node::flat_size`] bytes, at the start of `out`; returns the bytes written.
    pub fn flatten_into(&self, out: &mut [u8]) -> usize {
        out[..4].copy_from_slice(&(self.props.len() as u32).to_le_bytes());
        out[4..8].copy_from_slice(&(self.children.len() as u32).to_le_bytes());
        let mut at = 8;
        for p in &self.props {
            let length = p.value.len() as u32 | if p.placeholder { 0x8000_0000 } else { 0 };
            let field = &mut out[at..at + 36 + padded(p.value.len())];
            field.fill(0);
            field[..p.name.len()].copy_from_slice(&p.name);
            field[32..36].copy_from_slice(&length.to_le_bytes());
            field[36..36 + p.value.len()].copy_from_slice(&p.value);
            at += field.len();
        }
        for c in &self.children {
            at += c.flatten_into(&mut out[at..]);
        }
        at
    }

    pub fn flatten(&self) -> Vec<u8> {
        let mut out = vec![0; self.flat_size()];
        self.flatten_into(&mut out);
        out
    }

    /// Parses a flat tree that must fill `data` exactly.
    pub fn parse(data: &[u8]) -> Result<Node, Error> {
        let mut offset = 0;
        let root = parse_node(data, &mut offset, 0)?;
        if offset != data.len() {
            return Err(Error("bytes after the device tree"));
        }
        Ok(root)
    }

    /// The tree as text (iOSPlus10's external/slop/tools/adt.py dump).
    pub fn dump(&self, out: &mut dyn Write, depth: usize) -> io::Result<()> {
        let indent = depth * 2;
        write!(out, "{:indent$}", "")?;
        out.write_all(self.name())?;
        out.write_all(b" {\n")?;
        for p in self.props.iter().filter(|p| p.name != b"name") {
            out.write_all(format!("{:w$}", "", w = indent + 2).as_bytes())?;
            out.write_all(&p.name)?;
            write!(out, "{} = ", if p.placeholder { " [placeholder]" } else { "" })?;
            let v = &p.value;
            let text = v.len() > 1
                && v[v.len() - 1] == 0
                && v[..v.len() - 1].iter().all(|b| *b == 0 || (32..=126).contains(b));
            if text {
                let shown: Vec<u8> = v[..v.len() - 1].iter().map(|b| if *b == 0 { b'|' } else { *b }).collect();
                out.write_all(b"\"")?;
                out.write_all(&shown)?;
                out.write_all(b"\"\n")?;
            } else if let Some(word) = p.u32() {
                writeln!(out, "{word:#x}")?;
            } else if !v.is_empty() && v.len() % 8 == 0 && v.len() <= 64 {
                let words: Vec<String> =
                    v.chunks(8).map(|c| format!("{:#x}", u64::from_le_bytes(c.try_into().unwrap()))).collect();
                writeln!(out, "<{}>", words.join(" "))?;
            } else {
                for b in v.iter().take(32) {
                    write!(out, "{b:02x}")?;
                }
                writeln!(out, "{} ({} bytes)", if v.len() > 32 { "..." } else { "" }, v.len())?;
            }
        }
        for c in &self.children {
            c.dump(out, depth + 1)?;
        }
        writeln!(out, "{:indent$}}}", "")
    }
}

fn parse_node(data: &[u8], offset: &mut usize, depth: u32) -> Result<Node, Error> {
    let size = data.len();
    if depth > 64 || size - *offset < 8 {
        return Err(Error("device tree node truncated or too deep"));
    }
    let nprops = le::u32(data, *offset) as usize;
    let nchildren = le::u32(data, *offset + 4) as usize;
    *offset += 8;
    if nprops > (size - *offset) / 36 {
        return Err(Error("device tree property count runs past the tree"));
    }
    let mut n = Node::default();
    for _ in 0..nprops {
        let field = &data[*offset..];
        let name_end = field.get(..32).and_then(|f| f.iter().position(|b| *b == 0));
        let Some(name_end) = name_end.filter(|_| field.len() >= 36) else {
            return Err(Error("bad device tree property header"));
        };
        let raw = le::u32(field, 32);
        let length = (raw & 0x7fff_ffff) as usize;
        *offset += 36;
        if padded(length) > size - *offset {
            return Err(Error("device tree property value runs past the tree"));
        }
        let name = &field[..name_end];
        if n.get(name).is_some() {
            return Err(Error("device tree property repeated in a node"));
        }
        n.props.push(Prop {
            name: name.to_vec(),
            value: data[*offset..*offset + length].to_vec(),
            placeholder: raw >> 31 != 0,
        });
        *offset += padded(length);
    }
    if nchildren > (size - *offset) / 8 {
        return Err(Error("device tree child count runs past the tree"));
    }
    for _ in 0..nchildren {
        n.children.push(parse_node(data, offset, depth + 1)?);
    }
    Ok(n)
}
