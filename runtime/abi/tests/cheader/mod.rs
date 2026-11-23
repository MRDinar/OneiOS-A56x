// SPDX-License-Identifier: MIT
//! A reader for the plain C headers the Rust side mirrors (runtime/RUST.md: "a test that reads the C header and
//! checks every constant, size and offset it mirrors"). It knows what those headers use and no more: `#define NAME
//! <integer expression>`, one-parameter function-like macros, and structs of fixed-width integers, pointers,
//! function pointers, arrays and other structs, laid out as AAPCS64/LP64 lays them out (natural alignment). Anything
//! else it meets is a test failure that names it, never a silent guess.

use std::collections::BTreeMap;
use std::path::Path;

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

#[derive(Debug, Default, Clone)]
pub struct Header {
    /// object-like macros with an integer value, in the order the header has them
    pub defines: BTreeMap<String, u64>,
    /// function-like macros of one parameter: (parameter, body)
    pub macros: BTreeMap<String, (String, String)>,
    pub structs: BTreeMap<String, Layout>,
}

/// The header's text without its comments.
fn strip_comments(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while !rest.is_empty() {
        if let Some(after) = rest.strip_prefix("//") {
            rest = after.find('\n').map_or("", |n| &after[n..]);
        } else if let Some(after) = rest.strip_prefix("/*") {
            let end = after.find("*/").expect("an unterminated /* comment");
            out.push(' ');
            rest = &after[end + 2..];
        } else if rest.starts_with('"') {
            let end = rest[1..].find('"').expect("an unterminated string") + 2;
            out.push_str(&rest[..end]);
            rest = &rest[end..];
        } else {
            let c = rest.chars().next().unwrap();
            out.push(c);
            rest = &rest[c.len_utf8()..];
        }
    }
    out
}

// ---- integer expressions ---------------------------------------------------------------------------------------
#[derive(Debug, Clone, PartialEq)]
enum Token {
    Num(u64),
    Ident(String),
    Op(&'static str),
}

fn tokenize(expr: &str) -> Vec<Token> {
    const OPS: [&str; 11] = ["<<", ">>", "+", "-", "*", "/", "%", "|", "&", "(", ")"];
    let mut tokens = Vec::new();
    let mut rest = expr.trim_start();
    while !rest.is_empty() {
        let c = rest.as_bytes()[0];
        if c.is_ascii_digit() {
            let end = rest.find(|ch: char| !ch.is_ascii_alphanumeric()).unwrap_or(rest.len());
            let lit = rest[..end].trim_end_matches(['u', 'U', 'l', 'L']);
            let value = match lit.strip_prefix("0x").or_else(|| lit.strip_prefix("0X")) {
                Some(hex) => u64::from_str_radix(hex, 16),
                None => lit.parse(),
            };
            tokens.push(Token::Num(value.unwrap_or_else(|_| panic!("not a number: {}", &rest[..end]))));
            rest = &rest[end..];
        } else if c.is_ascii_alphabetic() || c == b'_' {
            let end = rest.find(|ch: char| !(ch.is_ascii_alphanumeric() || ch == '_')).unwrap_or(rest.len());
            tokens.push(Token::Ident(rest[..end].to_string()));
            rest = &rest[end..];
        } else {
            let op =
                OPS.iter().find(|op| rest.starts_with(**op)).unwrap_or_else(|| panic!("unknown operator in {expr:?}"));
            tokens.push(Token::Op(op));
            rest = &rest[op.len()..];
        }
        rest = rest.trim_start();
    }
    tokens
}

struct Eval<'a> {
    tokens: Vec<Token>,
    at: usize,
    names: &'a dyn Fn(&str) -> Option<u64>,
}

impl Eval<'_> {
    fn binary_precedence(op: &str) -> Option<u8> {
        Some(match op {
            "*" | "/" | "%" => 5,
            "+" | "-" => 4,
            "<<" | ">>" => 3,
            "&" => 2,
            "|" => 1,
            _ => return None,
        })
    }

    fn primary(&mut self) -> u64 {
        let token = self.tokens.get(self.at).cloned().expect("an expression ends early");
        self.at += 1;
        match token {
            Token::Num(n) => n,
            Token::Ident(name) => (self.names)(&name).unwrap_or_else(|| panic!("unknown name {name}")),
            Token::Op("(") => {
                let v = self.expr(0);
                assert_eq!(self.tokens.get(self.at), Some(&Token::Op(")")), "unbalanced parentheses");
                self.at += 1;
                v
            }
            Token::Op("-") => self.primary().wrapping_neg(),
            Token::Op(op) => panic!("unexpected {op}"),
        }
    }

    fn expr(&mut self, min: u8) -> u64 {
        let mut lhs = self.primary();
        while let Some(Token::Op(op)) = self.tokens.get(self.at) {
            let op = *op;
            let Some(prec) = Self::binary_precedence(op).filter(|p| *p > min) else { break };
            self.at += 1;
            let rhs = self.expr(prec);
            lhs = match op {
                "*" => lhs.wrapping_mul(rhs),
                "/" => lhs / rhs,
                "%" => lhs % rhs,
                "+" => lhs.wrapping_add(rhs),
                "-" => lhs.wrapping_sub(rhs),
                "<<" => lhs << rhs,
                ">>" => lhs >> rhs,
                "&" => lhs & rhs,
                _ => lhs | rhs,
            };
        }
        lhs
    }
}

/// Evaluates a C integer expression; names are looked up in `names`.
pub fn eval(expr: &str, names: &dyn Fn(&str) -> Option<u64>) -> u64 {
    let mut e = Eval { tokens: tokenize(expr), at: 0, names };
    let v = e.expr(0);
    assert_eq!(e.at, e.tokens.len(), "trailing tokens in {expr:?}");
    v
}

// ---- structs ---------------------------------------------------------------------------------------------------
fn primitive(name: &str) -> Option<usize> {
    Some(match name {
        "uint8_t" | "int8_t" | "char" | "bool" | "_Bool" => 1,
        "uint16_t" | "int16_t" | "short" => 2,
        "uint32_t" | "int32_t" | "int" | "unsigned" => 4,
        "uint64_t" | "int64_t" | "long" | "size_t" => 8,
        _ => return None,
    })
}

const fn align_up(v: usize, a: usize) -> usize {
    v.div_ceil(a) * a
}

impl Header {
    pub fn read(path: &Path, includes: &[&Header]) -> Header {
        let text = std::fs::read_to_string(path).unwrap_or_else(|e| panic!("{}: {e}", path.display()));
        Header::parse(&text, includes)
    }

    pub fn parse(text: &str, includes: &[&Header]) -> Header {
        let text = strip_comments(text);
        let mut h = Header::default();
        for inc in includes {
            // what an included header defines is known here, not checked again
            h.structs.extend(inc.structs.clone());
        }
        let known = |h: &Header, name: &str| -> Option<u64> {
            h.defines.get(name).copied().or_else(|| includes.iter().find_map(|i| i.defines.get(name).copied()))
        };
        for line in text.lines() {
            let Some(def) = line.trim_start().strip_prefix("#define") else { continue };
            let def = def.trim();
            let name_end = def.find(|c: char| !(c.is_ascii_alphanumeric() || c == '_')).unwrap_or(def.len());
            let (name, rest) = def.split_at(name_end);
            if let Some(params) = rest.strip_prefix('(') {
                let (param, body) = params.split_once(')').expect("a macro's parameters");
                h.macros.insert(name.to_string(), (param.trim().to_string(), body.trim().to_string()));
            } else if !rest.trim().is_empty() {
                let v = eval(rest, &|n| known(&h, n));
                h.defines.insert(name.to_string(), v);
            }
        }
        let mut rest = text.as_str();
        while let Some(at) = rest.find("struct ") {
            rest = &rest[at + 7..];
            let name_end = rest.find(|c: char| !(c.is_ascii_alphanumeric() || c == '_')).unwrap_or(rest.len());
            let name = &rest[..name_end];
            let after = rest[name_end..].trim_start();
            let Some(body) = after.strip_prefix('{') else { continue };
            let end = body.find('}').expect("an unterminated struct");
            let layout = h.layout_of(&body[..end], &|n| known(&h, n));
            h.structs.insert(name.to_string(), layout);
            rest = &body[end..];
        }
        h
    }

    fn layout_of(&self, body: &str, names: &dyn Fn(&str) -> Option<u64>) -> Layout {
        let (mut size, mut align, mut fields) = (0usize, 1usize, Vec::new());
        for decl in body.split(';').map(|d| d.split_whitespace().collect::<Vec<_>>().join(" ")) {
            if decl.is_empty() {
                continue;
            }
            // (name, element size, element align, count)
            let mut members: Vec<(String, usize, usize, usize)> = Vec::new();
            if let Some(at) = decl.find("(*") {
                let name = decl[at + 2..].split(')').next().unwrap().trim();
                members.push((name.to_string(), 8, 8, 1));
            } else {
                let tokens: Vec<&str> = decl.split(' ').filter(|t| *t != "const").collect();
                let (ty, rest) =
                    if tokens[0] == "struct" { (tokens[1], &tokens[2..]) } else { (tokens[0], &tokens[1..]) };
                for declarator in rest.join(" ").split(',') {
                    let declarator = declarator.trim();
                    let pointer = declarator.starts_with('*');
                    let declarator = declarator.trim_start_matches(['*', ' ']);
                    let (name, count) = match declarator.split_once('[') {
                        Some((name, dim)) => (name.trim(), eval(dim.trim_end_matches(']'), names) as usize),
                        None => (declarator, 1),
                    };
                    let (esize, ealign) = if pointer {
                        (8, 8)
                    } else if let Some(s) = primitive(ty) {
                        (s, s)
                    } else {
                        let l = self.structs.get(ty).unwrap_or_else(|| panic!("unknown type {ty} in {decl:?}"));
                        (l.size, l.align)
                    };
                    members.push((name.to_string(), esize, ealign, count));
                }
            }
            for (name, esize, ealign, count) in members {
                size = align_up(size, ealign);
                fields.push(Field { name, offset: size, size: esize * count });
                size += esize * count;
                align = align.max(ealign);
            }
        }
        Layout { size: align_up(size, align), align, fields }
    }

    pub fn layout(&self, name: &str) -> &Layout {
        self.structs.get(name).unwrap_or_else(|| panic!("no struct {name} in the header"))
    }

    /// A one-parameter macro's value for `arg`.
    pub fn apply(&self, name: &str, arg: u64) -> u64 {
        let (param, body) = self.macros.get(name).unwrap_or_else(|| panic!("no macro {name}"));
        eval(body, &|n| if n == param { Some(arg) } else { self.defines.get(n).copied() })
    }
}

#[test]
fn reader_lays_out_like_c() {
    let h = Header::parse(
        "#define N (2u << 3) /* 16 */\n#define M (N - 1) | 0x20ull\nstruct a { uint8_t b; uint64_t c, d[N]; };\n\
         struct e { uint16_t f; struct a g[2]; void (*h)(struct a *x,\n int y); const char *s; uint32_t t; };",
        &[],
    );
    assert_eq!(h.defines["N"], 16);
    assert_eq!(h.defines["M"], 0x2f);
    let a = h.layout("a");
    assert_eq!((a.size, a.align), (8 + 8 + 128, 8));
    let e = h.layout("e");
    let offsets: Vec<_> = e.fields.iter().map(|f| (f.name.as_str(), f.offset, f.size)).collect();
    assert_eq!(offsets, [("f", 0, 2), ("g", 8, 288), ("h", 296, 8), ("s", 304, 8), ("t", 312, 4)]);
    assert_eq!(e.size, 320);
}
