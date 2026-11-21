//! `oneios-tool kernel host-cc SETUP_TOOLCHAIN CLT CLANG`: xnu's HOST_CC on Linux, as UniXNU's
//! scripts/linux/setup-toolchain.py writes it, for kernel/linux/unixnu-layout.sh (which builds no LLVM and runs
//! none of that installer). Writes
//!   CLT/include/unixnu-host-prefix.h   the file's HOST_PREFIX_H
//!   CLT/include/copyfile.h             its COPYFILE_H
//!   CLT/bin/xnu-host-cc                its HOST_CC with {clang}, {arch_defs}, {include} and {lib} filled in (0755)
//! The three texts are taken from the pinned file's own text, so they stay in step with the pin; the file is read,
//! never run. Until 2026-11-21 an inline Python block of unixnu-layout.sh imported the file for them (Python is
//! retired from this repository); the three files are the same byte for byte (tools/oneios-tool/README.md "kernel").
//!
//! What is read: a module-level `NAME = <one string literal>` per constant (quotes ' " ''' """, prefixes r R u U,
//! Python's escapes). Anything else there (two assignments, a concatenation, an f-string, a call) is refused: the
//! pin changed in a way this reader does not know, and the old values must not be used silently.

use std::path::{Path, PathBuf};

use anyhow::{bail, Context, Result};
use regex::Regex;

/// The value of the module-level string constant NAME of a Python file's text.
pub fn constant(source: &str, name: &str) -> Result<String> {
    // Python reads a source file with universal newlines.
    let source = source.replace("\r\n", "\n").replace('\r', "\n");
    let re = Regex::new(&format!(r"(?m)^{}[ \t]*=[ \t]*", regex::escape(name))).unwrap();
    let starts: Vec<usize> = re
        .find_iter(&source)
        .filter(|m| !source[m.end()..].starts_with('='))
        .map(|m| m.end())
        .collect();
    let at = match starts[..] {
        [at] => at,
        [] => bail!("host-cc: no {name} = '...' in setup-toolchain.py"),
        _ => bail!("host-cc: {name} is assigned {} times in setup-toolchain.py", starts.len()),
    };
    let (value, end) = literal(&source[at..]).with_context(|| format!("host-cc: {name}"))?;
    let rest = source[at + end..].split('\n').next().unwrap_or("").trim_start_matches([' ', '\t']);
    if !(rest.is_empty() || rest.starts_with('#')) {
        bail!("host-cc: {name} is not one string literal (then: {})", rest.chars().take(40).collect::<String>());
    }
    Ok(value)
}

/// One Python string literal at the start of TEXT: its value and its length in TEXT.
fn literal(text: &str) -> Result<(String, usize)> {
    let bytes = text.as_bytes();
    let mut i = 0;
    let mut raw = false;
    while i < bytes.len() && bytes[i].is_ascii_alphabetic() {
        match bytes[i] {
            b'r' | b'R' => raw = true,
            b'u' | b'U' => {}
            other => bail!("a {}-string is not read", other as char),
        }
        i += 1;
    }
    let quote = match bytes.get(i) {
        Some(&q @ (b'\'' | b'"')) => q,
        _ => bail!("not a string literal"),
    };
    let triple = bytes[i..].starts_with(&[quote; 3]);
    let close: &[u8] = if triple { &bytes[i..i + 3] } else { &bytes[i..i + 1] };
    i += close.len();
    let body_start = i;
    loop {
        if i >= bytes.len() {
            bail!("the string literal does not end");
        }
        if bytes[i] == b'\\' {
            i += 2;
            continue;
        }
        if !triple && bytes[i] == b'\n' {
            bail!("the string literal does not end on its line");
        }
        if bytes[i..].starts_with(close) {
            break;
        }
        i += 1;
    }
    let body = &text[body_start..i];
    let value = if raw { body.to_string() } else { unescape(body)? };
    Ok((value, i + close.len()))
}

/// Python's escapes of a str literal.
fn unescape(body: &str) -> Result<String> {
    let mut out = String::with_capacity(body.len());
    let mut chars = body.chars().peekable();
    while let Some(c) = chars.next() {
        if c != '\\' {
            out.push(c);
            continue;
        }
        let Some(e) = chars.next() else { bail!("a lone backslash at the end") };
        match e {
            '\n' => {}
            '\\' | '\'' | '"' => out.push(e),
            'a' => out.push('\x07'),
            'b' => out.push('\x08'),
            'f' => out.push('\x0c'),
            'n' => out.push('\n'),
            'r' => out.push('\r'),
            't' => out.push('\t'),
            'v' => out.push('\x0b'),
            '0'..='7' => {
                let mut v = e.to_digit(8).unwrap();
                for _ in 0..2 {
                    match chars.peek().and_then(|d| d.to_digit(8)) {
                        Some(d) => {
                            v = v * 8 + d;
                            chars.next();
                        }
                        None => break,
                    }
                }
                out.push(char::from_u32(v).context("an octal escape")?);
            }
            'x' | 'u' | 'U' => {
                let n = match e { 'x' => 2, 'u' => 4, _ => 8 };
                let digits: String = (0..n).filter_map(|_| chars.next()).collect();
                let v = u32::from_str_radix(&digits, 16).ok().filter(|_| digits.len() == n).with_context(|| format!("a \\{e} escape"))?;
                out.push(char::from_u32(v).with_context(|| format!("a \\{e} escape"))?);
            }
            'N' => bail!("a \\N{{...}} escape is not read"),
            other => {
                // Python keeps an unknown escape as it is (with a warning).
                out.push('\\');
                out.push(other);
            }
        }
    }
    Ok(out)
}

/// Python's str.format(**fields) for plain {name} fields and {{ }}.
pub fn format(template: &str, fields: &[(&str, &str)]) -> Result<String> {
    let mut out = String::with_capacity(template.len());
    let mut chars = template.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '{' if chars.peek() == Some(&'{') => {
                chars.next();
                out.push('{');
            }
            '}' if chars.peek() == Some(&'}') => {
                chars.next();
                out.push('}');
            }
            '{' => {
                let mut name = String::new();
                loop {
                    match chars.next() {
                        Some('}') => break,
                        Some(c) => name.push(c),
                        None => bail!("host-cc: HOST_CC has a {{ that does not end"),
                    }
                }
                let Some((_, value)) = fields.iter().find(|(key, _)| *key == name) else {
                    bail!("host-cc: HOST_CC has a field {{{name}}} that is not one of clang, arch_defs, include, lib")
                };
                out.push_str(value);
            }
            '}' => bail!("host-cc: HOST_CC has a single }}"),
            c => out.push(c),
        }
    }
    Ok(out)
}

/// A path as Python's pathlib.Path prints it ('a//b/./c/' -> 'a/b/c').
fn pathlib(path: &Path) -> PathBuf {
    path.components().collect()
}

/// The three files' (place below CLT, text, mode).
pub fn files(source: &str, clt: &Path, clang: &str, machine: &str) -> Result<Vec<(PathBuf, String, u32)>> {
    let clt = pathlib(clt);
    let include = clt.join("include");
    let arch = if machine == "aarch64" { "-D__arm64__ -fsigned-char" } else { "" };
    let (include_s, lib_s) = (include.display().to_string(), clt.join("lib").display().to_string());
    let cc = format(&constant(source, "HOST_CC")?, &[("clang", clang), ("arch_defs", arch), ("include", &include_s), ("lib", &lib_s)])?;
    Ok(vec![
        (include.join("unixnu-host-prefix.h"), constant(source, "HOST_PREFIX_H")?, 0o644),
        (include.join("copyfile.h"), constant(source, "COPYFILE_H")?, 0o644),
        (clt.join("bin/xnu-host-cc"), cc, 0o755),
    ])
}

/// platform.machine(): uname's machine.
fn machine() -> String {
    // SAFETY: uname fills the struct it is given; its fields are NUL-terminated.
    unsafe {
        let mut u: libc::utsname = std::mem::zeroed();
        if libc::uname(&mut u) != 0 {
            return String::new();
        }
        std::ffi::CStr::from_ptr(u.machine.as_ptr()).to_string_lossy().into_owned()
    }
}

pub fn main(setup: &Path, clt: &Path, clang: &str) -> Result<()> {
    let source = std::fs::read_to_string(setup).with_context(|| setup.display().to_string())?;
    for (path, text, mode) in files(&source, clt, clang, &machine())? {
        std::fs::write(&path, text).with_context(|| path.display().to_string())?;
        if mode == 0o755 {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(mode)).with_context(|| path.display().to_string())?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A stand-in setup-toolchain.py: the shapes of the pinned file (''' literals, \\ line ends, {fields}).
    const SETUP: &str = r#"#!/usr/bin/env python3
import os
HOST_PREFIX_H = '''/* prefix */
#define X "a\tb"
'''

# one call
COPYFILE_H = """/* copyfile */ \
static int n = '\x41';
"""

HOST_CC = '''#!/bin/sh
exec "{clang}" \\
    --ld-path=/usr/bin/ld {arch_defs} \\
    -include "{include}/unixnu-host-prefix.h" -L{lib} "$@" {{}}
'''  # the wrapper


def install_host_cc(clt):
    if HOST_CC == 1:
        pass
"#;

    #[test]
    fn the_three_files_of_a_stand_in() {
        let files = files(SETUP, Path::new("/t//unixnu/./CommandLineTools/usr/"), "/usr/bin/clang", "aarch64").unwrap();
        let names: Vec<String> = files.iter().map(|f| f.0.display().to_string()).collect();
        assert_eq!(names, [
            "/t/unixnu/CommandLineTools/usr/include/unixnu-host-prefix.h",
            "/t/unixnu/CommandLineTools/usr/include/copyfile.h",
            "/t/unixnu/CommandLineTools/usr/bin/xnu-host-cc"
        ]);
        assert_eq!(files[0].1, "/* prefix */\n#define X \"a\tb\"\n");
        assert_eq!(files[1].1, "/* copyfile */ static int n = 'A';\n");
        assert_eq!(
            files[2].1,
            "#!/bin/sh\nexec \"/usr/bin/clang\" \\\n    --ld-path=/usr/bin/ld -D__arm64__ -fsigned-char \\\n    -include \"/t/unixnu/CommandLineTools/usr/include/unixnu-host-prefix.h\" -L/t/unixnu/CommandLineTools/usr/lib \"$@\" {}\n"
        );
        assert_eq!(files[2].2, 0o755);
        let x86 = super::files(SETUP, Path::new("/c"), "clang", "x86_64").unwrap();
        assert!(x86[2].1.contains("--ld-path=/usr/bin/ld  \\\n"));
    }

    #[test]
    fn what_is_refused() {
        let twice = format!("{SETUP}\nHOST_CC = 'x'\n");
        assert_eq!(constant(&twice, "HOST_CC").unwrap_err().to_string(), "host-cc: HOST_CC is assigned 2 times in setup-toolchain.py");
        assert_eq!(constant("X = 1\n", "HOST_CC").unwrap_err().to_string(), "host-cc: no HOST_CC = '...' in setup-toolchain.py");
        assert!(constant("HOST_CC = 'a' 'b'\n", "HOST_CC").unwrap_err().to_string().starts_with("host-cc: HOST_CC is not one string literal"));
        assert!(constant("HOST_CC = '''a'''.strip()\n", "HOST_CC").is_err());
        assert!(format!("{:#}", constant("HOST_CC = f'{x}'\n", "HOST_CC").unwrap_err()).contains("f-string"));
        assert!(constant("HOST_CC = '''a\n", "HOST_CC").is_err());
        assert_eq!(constant("HOST_CC = r'a\\n' # c\n", "HOST_CC").unwrap(), "a\\n");
        assert_eq!(constant("HOST_CC = 'it\\'s'\r\n", "HOST_CC").unwrap(), "it's");
        assert_eq!(constant("HOST_CC == 1\nHOST_CC = \"\\101\\q\"\n", "HOST_CC").unwrap(), "A\\q");
        assert!(format("{clang:>4}", &[("clang", "c")]).is_err());
        assert!(format("a } b", &[]).is_err());
        assert!(format("{other}", &[("clang", "c")]).is_err());
    }
}
