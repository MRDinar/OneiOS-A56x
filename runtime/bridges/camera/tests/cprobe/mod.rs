// SPDX-License-Identifier: MIT
//! The C side of a header-mirror test (runtime/RUST.md: "a test that reads the C header and checks every constant,
//! size and offset it mirrors"), for the camera's and the encoder's headers: their enums, nested anonymous structs and
//! multi-argument macros are answered by the C compiler itself. A probe program includes the header and prints each
//! expression asked for; [`Probe::names`] lists every `#define` with a value and every enum member, so that a name the
//! Rust side does not mirror fails the test. Without a C compiler (`$CC`, else `cc`) the test is skipped with a line
//! that says so.
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::process::Command;

pub struct Probe {
    header: PathBuf,
    includes: Vec<PathBuf>,
    exprs: Vec<(String, String)>,
}

impl Probe {
    pub fn new(header: &Path, includes: &[&Path]) -> Probe {
        Probe {
            header: header.to_path_buf(),
            includes: includes.iter().map(|p| p.to_path_buf()).collect(),
            exprs: Vec::new(),
        }
    }

    /// Asks for the value of the C expression `expr` under the key `key`.
    pub fn ask(&mut self, key: &str, expr: &str) -> &mut Self {
        self.exprs.push((key.to_owned(), expr.to_owned()));
        self
    }

    /// Asks for a struct's size and each named field's offset and size (keys `sizeof S`, `S.f`, `sizeof S.f`).
    pub fn layout(&mut self, ty: &str, fields: &[&str]) -> &mut Self {
        self.ask(&format!("sizeof {ty}"), &format!("sizeof({ty})"));
        for f in fields {
            self.ask(&format!("{ty}.{f}"), &format!("offsetof({ty}, {f})"));
            self.ask(&format!("sizeof {ty}.{f}"), &format!("sizeof((({ty} *)0)->{f})"));
        }
        self
    }

    /// The values, from the compiled probe; None (said on stderr) when there is no C compiler.
    pub fn run(&self) -> Option<BTreeMap<String, u64>> {
        let cc = std::env::var("CC").unwrap_or_else(|_| "cc".into());
        if Command::new(&cc).arg("--version").output().is_err() {
            eprintln!("header test skipped: no C compiler ({cc}) to read {}", self.header.display());
            return None;
        }
        let dir = std::env::temp_dir().join(format!(
            "oneios-cprobe-{}-{:x}",
            std::process::id(),
            self.exprs.len() ^ self.header.as_os_str().len()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let mut c = format!(
            "#include <stdio.h>\n#include <stddef.h>\n#include \"{}\"\nint main(void) {{\n",
            self.header.display()
        );
        for (key, expr) in &self.exprs {
            c += &format!("  printf(\"%s=%llu\\n\", \"{key}\", (unsigned long long)({expr}));\n");
        }
        c += "  return 0;\n}\n";
        let source = dir.join("probe.c");
        let program = dir.join("probe");
        std::fs::write(&source, c).unwrap();
        let mut compile = Command::new(&cc);
        compile.arg("-std=gnu11").arg("-o").arg(&program).arg(&source);
        for inc in &self.includes {
            compile.arg("-I").arg(inc);
        }
        let out = compile.output().expect("the C compiler runs");
        assert!(
            out.status.success(),
            "the probe of {} did not compile:\n{}",
            self.header.display(),
            String::from_utf8_lossy(&out.stderr)
        );
        let out = Command::new(&program).output().expect("the probe runs");
        std::fs::remove_dir_all(&dir).unwrap();
        Some(
            String::from_utf8(out.stdout)
                .unwrap()
                .lines()
                .map(|l| {
                    let (k, v) = l.split_once('=').unwrap();
                    (k.to_owned(), v.parse().unwrap())
                })
                .collect(),
        )
    }

    /// Every name the header gives a value: object-like `#define NAME VALUE` (not the include guard, not a macro with
    /// parameters) and every member of an `enum { ... }`.
    pub fn names(header: &Path) -> BTreeSet<String> {
        let text = std::fs::read_to_string(header).unwrap();
        let text: String = text
            .lines()
            .map(|l| match l.find("//") {
                Some(at) => &l[..at],
                None => l,
            })
            .collect::<Vec<_>>()
            .join("\n");
        let text = strip_block_comments(&text);
        let mut names = BTreeSet::new();
        for line in text.lines() {
            let Some(def) = line.trim_start().strip_prefix("#define") else { continue };
            let def = def.trim_start();
            let end = def.find(|c: char| !(c.is_ascii_alphanumeric() || c == '_')).unwrap_or(def.len());
            let (name, rest) = def.split_at(end);
            if !rest.starts_with('(') && !rest.trim().is_empty() {
                names.insert(name.to_owned());
            }
        }
        let mut rest = text.as_str();
        while let Some(at) = rest.find("enum") {
            rest = &rest[at + 4..];
            let Some(body) = rest.trim_start().strip_prefix('{') else { continue };
            let end = body.find('}').expect("an unterminated enum");
            for member in body[..end].split(',') {
                let name = member.split('=').next().unwrap().trim();
                if !name.is_empty() {
                    names.insert(name.to_owned());
                }
            }
            rest = &body[end..];
        }
        names
    }
}

fn strip_block_comments(text: &str) -> String {
    let mut out = String::new();
    let mut rest = text;
    while let Some(at) = rest.find("/*") {
        out += &rest[..at];
        let end = rest[at..].find("*/").expect("an unterminated comment");
        rest = &rest[at + end + 2..];
    }
    out + rest
}

/// Each `(name, rust value)` against the probe's value of the same name, and the header's every name among them.
pub fn check_consts(header: &Path, values: &BTreeMap<String, u64>, rust: &[(&str, u64)], not_values: &[&str]) {
    for (name, value) in rust {
        assert_eq!(values.get(*name), Some(value), "{name} of {}", header.display());
    }
    let mirrored: BTreeSet<&str> = rust.iter().map(|r| r.0).chain(not_values.iter().copied()).collect();
    let missing: Vec<_> = Probe::names(header).into_iter().filter(|n| !mirrored.contains(n.as_str())).collect();
    assert!(missing.is_empty(), "names of {} without a Rust mirror: {missing:?}", header.display());
}

/// A struct's size and fields against the probe's: `fields` are (C name, Rust offset, Rust size).
pub fn check_layout(values: &BTreeMap<String, u64>, ty: &str, size: usize, fields: &[(&str, usize, usize)]) {
    assert_eq!(values[&format!("sizeof {ty}")], size as u64, "sizeof({ty})");
    for (f, offset, fsize) in fields {
        assert_eq!(values[&format!("{ty}.{f}")], *offset as u64, "offsetof({ty}, {f})");
        assert_eq!(values[&format!("sizeof {ty}.{f}")], *fsize as u64, "sizeof({ty}.{f})");
    }
}

/// `(C field name, Rust offset, Rust size)` of field `$f` of `$t` (a nested path for nested structs).
#[macro_export]
macro_rules! field {
    ($t:ty, $($f:ident).+) => {
        (
            stringify!($($f).+),
            std::mem::offset_of!($t, $($f).+),
            {
                fn size<T>(_: *const T) -> usize { std::mem::size_of::<T>() }
                let v = std::mem::MaybeUninit::<$t>::uninit();
                // SAFETY: only the field's address is taken (addr_of!), never read.
                size(unsafe { std::ptr::addr_of!((*v.as_ptr()).$($f).+) })
            },
        )
    };
}
