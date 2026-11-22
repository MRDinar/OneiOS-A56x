// SPDX-License-Identifier: MIT
//! A reader for the guest-facing C headers the bridges mirror (runtime/RUST.md: "a test that reads the C header and
//! checks every constant, size and offset it mirrors"). It knows what those headers use: `#define NAME <integer
//! expression>`, `enum { A = expr, B, ... }`, and structs of fixed-width integers, bool, arrays and other structs, as
//! LP64 lays them out (natural alignment). Anything else in a struct is a test failure that names it.

use std::collections::BTreeMap;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Field {
    pub name: String,
    pub offset: usize,
    pub size: usize,
}

#[derive(Debug, Clone)]
pub struct Layout {
    pub size: usize,
    pub align: usize,
    pub fields: Vec<Field>,
}

#[derive(Debug, Default)]
pub struct Header {
    /// #defines with an integer value and every enumerator, as i64
    pub values: BTreeMap<String, i64>,
    pub structs: BTreeMap<String, Layout>,
}

fn strip_comments(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while !rest.is_empty() {
        if let Some(after) = rest.strip_prefix("//") {
            rest = after.find('\n').map_or("", |n| &after[n..]);
        } else if let Some(after) = rest.strip_prefix("/*") {
            rest = &after[after.find("*/").expect("an unterminated /* comment") + 2..];
            out.push(' ');
        } else {
            let c = rest.chars().next().unwrap();
            out.push(c);
            rest = &rest[c.len_utf8()..];
        }
    }
    out
}

/// An integer expression of numbers (with u/U/l/L suffixes), names already known, ( ), unary -, and + - * / << >> | &.
pub fn eval(expr: &str, names: &BTreeMap<String, i64>) -> Option<i64> {
    let tokens = tokenize(expr)?;
    let mut p = Parser { tokens, at: 0, names };
    let v = p.expr(0)?;
    (p.at == p.tokens.len()).then_some(v)
}

#[derive(Debug, Clone, PartialEq)]
enum Tok {
    Num(i64),
    Name(String),
    Op(&'static str),
}

fn tokenize(expr: &str) -> Option<Vec<Tok>> {
    const OPS: [&str; 10] = ["<<", ">>", "+", "-", "*", "/", "|", "&", "(", ")"];
    let mut out = Vec::new();
    let mut rest = expr.trim();
    while !rest.is_empty() {
        let c = rest.as_bytes()[0];
        if c.is_ascii_digit() {
            let end = rest.find(|ch: char| !ch.is_ascii_alphanumeric()).unwrap_or(rest.len());
            let lit = rest[..end].trim_end_matches(['u', 'U', 'l', 'L']);
            let v = match lit.strip_prefix("0x").or_else(|| lit.strip_prefix("0X")) {
                Some(hex) => i64::from_str_radix(hex, 16).ok()?,
                None => lit.parse().ok()?,
            };
            out.push(Tok::Num(v));
            rest = &rest[end..];
        } else if c.is_ascii_alphabetic() || c == b'_' {
            let end = rest.find(|ch: char| !(ch.is_ascii_alphanumeric() || ch == '_')).unwrap_or(rest.len());
            out.push(Tok::Name(rest[..end].to_owned()));
            rest = &rest[end..];
        } else {
            let op = OPS.iter().find(|o| rest.starts_with(**o))?;
            out.push(Tok::Op(op));
            rest = &rest[op.len()..];
        }
        rest = rest.trim_start();
    }
    Some(out)
}

struct Parser<'a> {
    tokens: Vec<Tok>,
    at: usize,
    names: &'a BTreeMap<String, i64>,
}

impl Parser<'_> {
    fn primary(&mut self) -> Option<i64> {
        let t = self.tokens.get(self.at)?.clone();
        self.at += 1;
        match t {
            Tok::Num(v) => Some(v),
            Tok::Name(n) => match n.as_str() {
                "INT32_MIN" => Some(i64::from(i32::MIN)),
                "INT32_MAX" => Some(i64::from(i32::MAX)),
                "UINT32_MAX" => Some(i64::from(u32::MAX)),
                _ => self.names.get(&n).copied(),
            },
            Tok::Op("(") => {
                let v = self.expr(0)?;
                (self.tokens.get(self.at) == Some(&Tok::Op(")"))).then(|| self.at += 1)?;
                Some(v)
            }
            Tok::Op("-") => Some(-self.primary()?),
            Tok::Op(_) => None,
        }
    }

    fn expr(&mut self, min: u8) -> Option<i64> {
        let mut left = self.primary()?;
        while let Some(Tok::Op(op)) = self.tokens.get(self.at).cloned() {
            let prec = match op {
                "*" | "/" => 5,
                "+" | "-" => 4,
                "<<" | ">>" => 3,
                "&" => 2,
                "|" => 1,
                _ => break,
            };
            if prec < min {
                break;
            }
            self.at += 1;
            let right = self.expr(prec + 1)?;
            left = match op {
                "*" => left * right,
                "/" => left / right,
                "+" => left + right,
                "-" => left - right,
                "<<" => left << right,
                ">>" => left >> right,
                "&" => left & right,
                _ => left | right,
            };
        }
        Some(left)
    }
}

fn primitive(name: &str) -> Option<usize> {
    Some(match name {
        "uint8_t" | "int8_t" | "bool" | "char" => 1,
        "uint16_t" | "int16_t" => 2,
        "uint32_t" | "int32_t" | "int" | "unsigned" => 4,
        "uint64_t" | "int64_t" => 8,
        _ => return None,
    })
}

impl Header {
    pub fn read(path: &std::path::Path) -> Header {
        let text = std::fs::read_to_string(path).unwrap_or_else(|e| panic!("{}: {e}", path.display()));
        Header::parse(&text)
    }

    pub fn parse(text: &str) -> Header {
        let text = strip_comments(text);
        let mut h = Header::default();
        let mut rest = text.as_str();
        loop {
            let next = ["#define", "enum", "struct"].iter().filter_map(|k| rest.find(k).map(|at| (at, *k))).min();
            let Some((at, kind)) = next else { break };
            let after = &rest[at + kind.len()..];
            match kind {
                "#define" => {
                    let line = &after[..after.find('\n').unwrap_or(after.len())];
                    let mut words = line.trim().splitn(2, char::is_whitespace);
                    let name = words.next().unwrap_or("").to_owned();
                    let body = words.next().unwrap_or("").trim();
                    if !name.contains('(') {
                        if let Some(v) = eval(body, &h.values) {
                            h.values.insert(name, v);
                        }
                    }
                    rest = &after[line.len()..];
                }
                "enum" if after.trim_start().starts_with('{') => {
                    let open = after.find('{').unwrap();
                    let close = after.find('}').expect("an enum's }");
                    let mut next = 0i64;
                    for item in after[open + 1..close].split(',').map(str::trim).filter(|s| !s.is_empty()) {
                        let (name, value) = match item.split_once('=') {
                            Some((n, e)) => {
                                (n.trim(), eval(e, &h.values).unwrap_or_else(|| panic!("enum value {item}")))
                            }
                            None => (item, next),
                        };
                        h.values.insert(name.to_owned(), value);
                        next = value + 1;
                    }
                    rest = &after[close + 1..];
                }
                "struct" => {
                    let head = after.trim_start();
                    let name_end = head.find(|c: char| !(c.is_ascii_alphanumeric() || c == '_')).unwrap_or(0);
                    let name = &head[..name_end];
                    let tail = head[name_end..].trim_start();
                    if !name.is_empty() && tail.starts_with('{') {
                        let close = tail.find('}').expect("a struct's }");
                        let layout = h.layout_of(&tail[1..close]);
                        h.structs.insert(name.to_owned(), layout);
                        rest = &tail[close + 1..];
                    } else {
                        rest = after;
                    }
                }
                _ => rest = after,
            }
        }
        h
    }

    fn layout_of(&self, body: &str) -> Layout {
        let (mut size, mut align, mut fields) = (0usize, 1usize, Vec::new());
        for decl in body.split(';').map(str::trim).filter(|d| !d.is_empty()) {
            let (ty, names) = match decl.strip_prefix("struct ") {
                Some(r) => {
                    let (t, n) = r.split_once(char::is_whitespace).expect("struct NAME field");
                    (format!("struct {t}"), n)
                }
                None => {
                    let (t, n) = decl.split_once(char::is_whitespace).unwrap_or_else(|| panic!("a field: {decl}"));
                    (t.to_owned(), n)
                }
            };
            let (elem, elem_align) = match ty.strip_prefix("struct ") {
                Some(s) => {
                    let l = self.structs.get(s).unwrap_or_else(|| panic!("struct {s} is not known"));
                    (l.size, l.align)
                }
                None => {
                    let s = primitive(&ty).unwrap_or_else(|| panic!("a field type this reader does not know: {ty}"));
                    (s, s)
                }
            };
            for name in names.split(',').map(str::trim) {
                let (name, count) = match name.split_once('[') {
                    Some((n, dim)) => {
                        let dim = dim.trim_end_matches(']');
                        (n.trim(), eval(dim, &self.values).unwrap_or_else(|| panic!("an array size: {dim}")) as usize)
                    }
                    None => (name, 1),
                };
                size = size.div_ceil(elem_align) * elem_align;
                fields.push(Field { name: name.to_owned(), offset: size, size: elem * count });
                size += elem * count;
                align = align.max(elem_align);
            }
        }
        Layout { size: size.div_ceil(align) * align, align, fields }
    }

    /// A value of the header (define or enumerator).
    pub fn value(&self, name: &str) -> i64 {
        *self.values.get(name).unwrap_or_else(|| panic!("{name} is not in the header"))
    }

    /// A struct's layout.
    pub fn layout(&self, name: &str) -> &Layout {
        self.structs.get(name).unwrap_or_else(|| panic!("struct {name} is not in the header"))
    }
}

/// The checkout's file `rel` (from the repository's root: the crate's grandparent, or above this source file when
/// the module is built from elsewhere).
pub fn repo_file(rel: &str) -> std::path::PathBuf {
    let crate_root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let source_root = std::path::Path::new(file!()).ancestors().nth(4).map(std::path::Path::to_path_buf);
    [Some(crate_root), source_root]
        .into_iter()
        .flatten()
        .map(|r| r.join(rel))
        .find(|p| p.exists())
        .unwrap_or_else(|| rel.into())
}

/// Each (name, value) against the header's; and every value of the header whose name starts with one of `prefixes`
/// is among them (a constant added to the header and not to the Rust fails).
pub fn check_values(h: &Header, prefixes: &[&str], ours: &[(&str, i64)]) {
    for (name, v) in ours {
        assert_eq!(h.value(name), *v, "{name}");
    }
    let missing: Vec<&String> = h
        .values
        .keys()
        .filter(|k| prefixes.iter().any(|p| k.starts_with(p)) && !ours.iter().any(|(n, _)| n == k))
        .collect();
    assert!(missing.is_empty(), "constants of the header without a Rust mirror: {missing:?}");
}

/// A wire struct's fields (names may differ: C's and ours, in order) and size against the header's struct.
pub fn check_struct<T: super::wire::Wire>(h: &Header, c_name: &str, names: &[&str]) {
    let c = h.layout(c_name);
    let ours = T::layout();
    assert_eq!(c.size, T::SIZE, "sizeof(struct {c_name})");
    assert_eq!(c.fields.len(), ours.len(), "struct {c_name}'s fields");
    for ((cf, (_, offset, size)), want) in c.fields.iter().zip(&ours).zip(names) {
        assert_eq!((cf.name.as_str(), cf.offset, cf.size), (*want, *offset, *size), "struct {c_name}");
    }
}

#[test]
fn reader_lays_out_like_c() {
    let h = Header::parse(
        "#define A 4u\n#define B (A - 1u)\nenum { X = 1, Y, Z = (1u << 3) }; struct s { uint8_t a; uint32_t b; \
         uint16_t c[3]; }; struct t { struct s s; uint8_t d; };",
    );
    assert!(h.value("A") == 4 && h.value("B") == 3 && h.value("Y") == 2 && h.value("Z") == 8);
    let s = h.layout("s");
    assert_eq!((s.size, s.fields[1].offset, s.fields[2].offset, s.fields[2].size), (16, 4, 8, 6));
    assert_eq!(h.layout("t").size, 20);
}
