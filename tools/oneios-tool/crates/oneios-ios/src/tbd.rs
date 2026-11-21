//! Text-based stubs (.tbd, version 4: the YAML that tapi and `ios tbds` write): a reader for macho-check and the
//! writer of `ios tbds`.
//!
//! The reader takes the part of YAML that TBD v4 uses, as Apple's SDKs (705 files of the iPhoneOS 26.5 SDK) and
//! the stubs of `ios tbds` (and of the `ipsw` program before) write it: documents that start with `--- !tapi-tbd`
//! and may end with `...`; block mappings; block sequences of mappings (`- targets: [...]`); flow sequences of
//! scalars that may go over several lines; plain, 'single-quoted' and "double-quoted" scalars. A TBD v5 file (JSON)
//! is refused. Anything else in a file is an error, not a guess.

use anyhow::{anyhow, bail, Result};
use std::collections::BTreeSet;

/// A YAML value of the part TBD uses.
#[derive(Debug, Clone, PartialEq)]
pub enum Y {
    Str(String),
    List(Vec<Y>),
    Map(Vec<(String, Y)>),
}

impl Y {
    pub fn get(&self, key: &str) -> Option<&Y> {
        match self {
            Y::Map(m) => m.iter().find(|(k, _)| k == key).map(|(_, v)| v),
            _ => None,
        }
    }
    pub fn as_str(&self) -> Option<&str> {
        match self {
            Y::Str(s) => Some(s),
            _ => None,
        }
    }
    pub fn items(&self) -> &[Y] {
        match self {
            Y::List(l) => l,
            _ => &[],
        }
    }
    pub fn strings(&self) -> Vec<String> {
        self.items().iter().filter_map(|y| y.as_str().map(str::to_string)).collect()
    }
}

/// One logical line: its indentation, whether it is a sequence item (`- `), and its text after that.
#[derive(Debug)]
struct Line {
    indent: usize,
    dash: bool,
    text: String,
    number: usize,
}

/// Brackets still open at the end of `text` (quotes taken into account).
fn open_brackets(text: &str) -> i32 {
    let (mut depth, mut quote) = (0i32, None::<char>);
    let mut chars = text.chars().peekable();
    while let Some(c) = chars.next() {
        match quote {
            Some('\'') if c == '\'' => {
                if chars.peek() == Some(&'\'') {
                    chars.next();
                } else {
                    quote = None;
                }
            }
            Some('"') if c == '\\' => {
                chars.next();
            }
            Some(q) if c == q => quote = None,
            Some(_) => {}
            None => match c {
                '\'' | '"' => quote = Some(c),
                '[' => depth += 1,
                ']' => depth -= 1,
                '#' => break,
                _ => {}
            },
        }
    }
    depth
}

fn lines(doc: &str, first: usize) -> Result<Vec<Line>> {
    let mut out = Vec::new();
    let mut pending: Option<Line> = None;
    // the brackets still open in `pending` (a quote does not go over a line in a TBD, so each line is counted alone)
    let mut depth = 0i32;
    for (i, raw) in doc.lines().enumerate() {
        let number = first + i;
        if let Some(mut p) = pending.take() {
            p.text.push(' ');
            p.text.push_str(raw.trim());
            depth += open_brackets(raw);
            if depth > 0 {
                pending = Some(p);
            } else {
                out.push(p);
            }
            continue;
        }
        let trimmed = raw.trim_end();
        let body = trimmed.trim_start();
        if body.is_empty() || body.starts_with('#') {
            continue;
        }
        if raw.contains('\t') && raw.len() - raw.trim_start().len() > 0 && raw[..raw.len() - raw.trim_start().len()].contains('\t') {
            bail!("line {number}: a tab in the indentation");
        }
        let mut indent = trimmed.len() - body.len();
        let mut text = body.to_string();
        if text == "-" || text.starts_with("- ") {
            out.push(Line { indent, dash: true, text: String::new(), number });
            text = text[1..].trim_start().to_string();
            indent += body.len() - body[1..].trim_start().len();
            if text.is_empty() {
                continue;
            }
        }
        let line = Line { indent, dash: false, text, number };
        depth = open_brackets(&line.text);
        if depth > 0 {
            pending = Some(line);
        } else {
            out.push(line);
        }
    }
    if let Some(p) = pending {
        bail!("line {}: a '[' that is not closed", p.number);
    }
    Ok(out)
}

fn unquote(s: &str) -> Result<String> {
    let s = s.trim();
    if let Some(inner) = s.strip_prefix('\'') {
        let inner = inner.strip_suffix('\'').ok_or_else(|| anyhow!("{s:?}: a quote that is not closed"))?;
        return Ok(inner.replace("''", "'"));
    }
    if s.starts_with('"') {
        return serde_json::from_str::<String>(s).map_err(|e| anyhow!("{s:?}: {e}"));
    }
    // a comment after a plain scalar
    let s = match s.find(" #") {
        Some(i) => s[..i].trim_end(),
        None => s,
    };
    Ok(s.to_string())
}

/// The scalars of a flow sequence `[ a, 'b', ... ]`.
fn flow(s: &str) -> Result<Y> {
    let inner = s.trim().strip_prefix('[').and_then(|x| x.trim_end().strip_suffix(']')).ok_or_else(|| anyhow!("{s:?}: not a flow sequence"))?;
    let mut items = Vec::new();
    let (mut cur, mut quote) = (String::new(), None::<char>);
    let mut chars = inner.chars().peekable();
    while let Some(c) = chars.next() {
        match quote {
            Some('\'') if c == '\'' && chars.peek() == Some(&'\'') => {
                cur.push_str("''");
                chars.next();
            }
            Some('"') if c == '\\' => {
                cur.push(c);
                if let Some(n) = chars.next() {
                    cur.push(n);
                }
            }
            Some(q) if c == q => {
                cur.push(c);
                quote = None;
            }
            Some(_) => cur.push(c),
            None if c == '\'' || c == '"' => {
                cur.push(c);
                quote = Some(c);
            }
            None if c == ',' => {
                let t = cur.trim();
                if !t.is_empty() {
                    items.push(Y::Str(unquote(t)?));
                }
                cur.clear();
            }
            None if c == '[' || c == '{' => bail!("{s:?}: a nested collection"),
            None => cur.push(c),
        }
    }
    let t = cur.trim();
    if !t.is_empty() {
        items.push(Y::Str(unquote(t)?));
    }
    Ok(Y::List(items))
}

fn value(text: &str) -> Result<Y> {
    let t = text.trim();
    if t.starts_with('[') {
        flow(t)
    } else if t.starts_with('{') {
        bail!("{t:?}: a flow mapping")
    } else {
        Ok(Y::Str(unquote(t)?))
    }
}

/// The key and the rest of a `key: value` line (the key may be quoted).
fn split_key(text: &str) -> Option<(String, &str)> {
    if let Some(rest) = text.strip_prefix('\'') {
        let end = rest.find("':")?;
        return Some((rest[..end].replace("''", "'"), rest[end + 2..].trim()));
    }
    if let Some(i) = text.find(": ") {
        return Some((text[..i].trim().to_string(), text[i + 2..].trim()));
    }
    text.strip_suffix(':').map(|k| (k.trim().to_string(), ""))
}

fn block(ls: &[Line], i: &mut usize, indent: usize) -> Result<Y> {
    if ls.get(*i).is_some_and(|l| l.dash && l.indent == indent) {
        let mut items = Vec::new();
        while let Some(l) = ls.get(*i).filter(|l| l.dash && l.indent == indent) {
            *i += 1;
            match ls.get(*i) {
                Some(n) if !n.dash && n.indent > indent => {
                    let inner = n.indent;
                    if split_key(&n.text).is_some() {
                        items.push(block(ls, i, inner)?);
                    } else {
                        items.push(value(&n.text)?);
                        *i += 1;
                    }
                }
                _ => bail!("line {}: an empty sequence item", l.number),
            }
        }
        return Ok(Y::List(items));
    }
    let mut map = Vec::new();
    while let Some(l) = ls.get(*i) {
        if l.indent < indent || l.dash {
            break;
        }
        if l.indent > indent {
            bail!("line {}: indented more than its mapping", l.number);
        }
        let (key, rest) = split_key(&l.text).ok_or_else(|| anyhow!("line {}: not a 'key: value' line: {:?}", l.number, l.text))?;
        *i += 1;
        let v = if !rest.is_empty() {
            value(rest)?
        } else {
            match ls.get(*i) {
                Some(n) if n.indent > indent || (n.dash && n.indent == indent) => block(ls, i, n.indent)?,
                _ => Y::Str(String::new()),
            }
        };
        map.push((key, v));
    }
    Ok(Y::Map(map))
}

/// The documents of a TBD file.
pub fn parse(text: &str) -> Result<Vec<Y>> {
    if text.trim_start().starts_with('{') {
        bail!("TBD v5 (JSON) is not handled");
    }
    let mut docs = Vec::new();
    let mut cur = String::new();
    let mut first = 1;
    let mut start = 1;
    let flush = |cur: &mut String, start: usize, docs: &mut Vec<Y>| -> Result<()> {
        if !cur.trim().is_empty() {
            let ls = lines(cur, start)?;
            let mut i = 0;
            let y = block(&ls, &mut i, ls.first().map(|l| l.indent).unwrap_or(0))?;
            if let Some(l) = ls.get(i) {
                bail!("line {}: not part of the document", l.number);
            }
            docs.push(y);
        }
        cur.clear();
        Ok(())
    };
    for (n, line) in text.lines().enumerate() {
        if line.starts_with("---") || line.starts_with("...") {
            flush(&mut cur, start, &mut docs)?;
            first = n + 2;
            start = first;
            let rest = line.trim_start_matches(['-', '.']).trim();
            if !rest.is_empty() && !rest.starts_with('!') {
                bail!("line {}: content after '---'", n + 1);
            }
            continue;
        }
        cur.push_str(line);
        cur.push('\n');
    }
    flush(&mut cur, start, &mut docs)?;
    let _ = first;
    Ok(docs)
}

/// One document as macho-check reads it: (install name, the symbols it exports for `target`, the libraries it
/// re-exports for `target`). macho-check.py's `tbd_docs`.
pub struct Exports {
    pub install_name: String,
    pub symbols: BTreeSet<String>,
    pub reexports: Vec<String>,
}

pub fn exports(text: &str, target: &str) -> Result<Vec<Exports>> {
    let mut out = Vec::new();
    for d in parse(text)? {
        let mine = |e: &Y| e.get("targets").map(|t| t.strings().iter().any(|x| x == target)).unwrap_or(false);
        let mut symbols = BTreeSet::new();
        let mut reexports = Vec::new();
        if let Some(r) = d.get("reexported-libraries") {
            for e in r.items().iter().filter(|e| mine(e)) {
                reexports.extend(e.get("libraries").map(Y::strings).unwrap_or_default());
            }
        }
        for key in ["exports", "reexports"] {
            for e in d.get(key).map(Y::items).unwrap_or(&[]).iter().filter(|e| mine(e)) {
                for kind in ["symbols", "weak-symbols", "thread-local-symbols"] {
                    symbols.extend(e.get(kind).map(Y::strings).unwrap_or_default());
                }
                for c in e.get("objc-classes").map(Y::strings).unwrap_or_default() {
                    symbols.insert(format!("_OBJC_CLASS_$_{c}"));
                    symbols.insert(format!("_OBJC_METACLASS_$_{c}"));
                }
                for c in e.get("objc-eh-types").map(Y::strings).unwrap_or_default() {
                    symbols.insert(format!("_OBJC_EHTYPE_$_{c}"));
                }
                for c in e.get("objc-ivars").map(Y::strings).unwrap_or_default() {
                    symbols.insert(format!("_OBJC_IVAR_$_{c}"));
                }
            }
        }
        let install_name = d.get("install-name").and_then(Y::as_str).ok_or_else(|| anyhow!("a document has no install-name"))?.to_string();
        out.push(Exports { install_name, symbols, reexports });
    }
    Ok(out)
}

// ------------------------------------------------------------------------------------------------ the writer

/// What one stub document says.
#[derive(Debug, Default, Clone)]
pub struct Stub {
    pub install_name: String,
    pub current_version: Option<String>,
    pub umbrella: Option<String>,
    pub reexported: Vec<String>,
    pub symbols: Vec<String>,
    pub weak: Vec<String>,
    pub thread_local: Vec<String>,
    pub objc_classes: Vec<String>,
    pub objc_eh_types: Vec<String>,
    pub objc_ivars: Vec<String>,
}

/// A scalar as plain text when YAML reads it back as the same string, else single-quoted.
fn scalar(s: &str) -> String {
    let plain = !s.is_empty()
        && s.bytes().all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'$' | b'.' | b'/' | b'-' | b'+' | b'@' | b'<' | b'>' | b'(' | b')' | b'=' | b'~' | b'^' | b';'))
        && !s.starts_with(['-', '@', '<', '>', '(', ')', '=', '~', '^', ';', '.', '+'])
        && !matches!(s, "true" | "false" | "null" | "yes" | "no" | "on" | "off" | "True" | "False" | "Null" | "Yes" | "No" | "On" | "Off" | "TRUE" | "FALSE" | "NULL" | "YES" | "NO" | "ON" | "OFF")
        && s.parse::<f64>().is_err();
    if plain {
        s.to_string()
    } else {
        format!("'{}'", s.replace('\'', "''"))
    }
}

const COL: usize = 22;

fn key_line(out: &mut String, indent: usize, key: &str, value: &str) {
    let head = format!("{}{key}:", " ".repeat(indent));
    let pad = if head.len() < COL { COL - head.len() } else { 1 };
    out.push_str(&head);
    out.push_str(&" ".repeat(pad));
    out.push_str(value);
    out.push('\n');
}

/// One item per line, as the stubs of 24A446 were written (`ipsw dyld tbd`'s layout: the `[` of `symbols` in
/// column 22, of the other lists in column 21, the items after the first in column 23).
fn list_lines(out: &mut String, key: &str, items: &[String]) {
    let head = format!("    {key}:");
    let col = if key == "symbols" { COL } else { COL - 1 };
    out.push_str(&head);
    out.push_str(&" ".repeat(col.saturating_sub(head.len()).max(1)));
    out.push_str("[ ");
    let cont = " ".repeat(COL + 1);
    for (i, s) in items.iter().enumerate() {
        if i > 0 {
            out.push_str(",\n");
            out.push_str(&cont);
        }
        out.push_str(&scalar(s));
    }
    out.push_str(" ]\n");
}

/// The text of one document (without the `...` that ends a file).
pub fn write_doc(stub: &Stub, target: &str) -> String {
    let mut out = String::from("--- !tapi-tbd\n");
    let targets = format!("[ {target} ]");
    key_line(&mut out, 0, "tbd-version", "4");
    key_line(&mut out, 0, "targets", &targets);
    key_line(&mut out, 0, "install-name", &format!("'{}'", stub.install_name.replace('\'', "''")));
    if let Some(v) = &stub.current_version {
        key_line(&mut out, 0, "current-version", v);
    }
    if let Some(u) = &stub.umbrella {
        out.push_str("parent-umbrella:\n");
        key_line(&mut out, 2, "- targets", &targets);
        key_line(&mut out, 4, "umbrella", &scalar(u));
    }
    if !stub.reexported.is_empty() {
        out.push_str("reexported-libraries:\n");
        key_line(&mut out, 2, "- targets", &targets);
        let libs: Vec<String> = stub.reexported.iter().map(|l| format!("'{}'", l.replace('\'', "''"))).collect();
        key_line(&mut out, 4, "libraries", &format!("[ {} ]", libs.join(", ")));
    }
    let lists: [(&str, &Vec<String>); 6] = [
        ("symbols", &stub.symbols),
        ("objc-classes", &stub.objc_classes),
        ("objc-eh-types", &stub.objc_eh_types),
        ("objc-ivars", &stub.objc_ivars),
        ("weak-symbols", &stub.weak),
        ("thread-local-symbols", &stub.thread_local),
    ];
    if lists.iter().any(|(_, l)| !l.is_empty()) {
        out.push_str("exports:\n");
        key_line(&mut out, 2, "- targets", &targets);
        for (key, items) in lists {
            if !items.is_empty() {
                list_lines(&mut out, key, items);
            }
        }
    }
    out
}

/// A whole file: the image's document, then those of the libraries it re-exports, then `...`.
pub fn write_file(docs: &[&Stub], target: &str) -> String {
    let mut out = String::new();
    for d in docs {
        out.push_str(&write_doc(d, target));
    }
    out.push_str("...\n");
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    const SDK_STYLE: &str = "--- !tapi-tbd
tbd-version:     4
targets:         [ arm64e-ios, arm64-ios ]
install-name:    '/usr/lib/libSystem.B.dylib'
current-version: 1356
reexported-libraries:
  - targets:         [ arm64e-ios ]
    libraries:       [ '/usr/lib/system/libcache.dylib',
                       '/usr/lib/system/libxpc.dylib' ]
exports:
  - targets:         [ arm64e-ios ]
    symbols:         [ ___crashreporter_info__, _libSystem_init_after_boot_tasks_4launchd,
                       _mach_init_routine, 'weird symbol, with a comma', 'it''s' ]
    objc-classes:    [ NSThing ]
    objc-ivars:      [ NSThing._x ]
    weak-symbols:    [ _w ]
  - targets:         [ arm64-ios ]
    symbols:         [ _only_arm64 ]
reexports:
  - targets:         [ arm64e-ios ]
    symbols:         [ _re ]
--- !tapi-tbd
tbd-version:     4
targets:         [ arm64e-ios ]
install-name:    '/usr/lib/system/libcache.dylib'
parent-umbrella:
  - targets:         [ arm64e-ios ]
    umbrella:        System
exports:
  - targets:         [ arm64e-ios ]
    symbols:         [ _cache_create ]
...
";

    #[test]
    fn reads_the_sdks_layout() {
        let docs = exports(SDK_STYLE, "arm64e-ios").unwrap();
        assert_eq!(docs.len(), 2);
        assert_eq!(docs[0].install_name, "/usr/lib/libSystem.B.dylib");
        assert_eq!(docs[0].reexports, vec!["/usr/lib/system/libcache.dylib", "/usr/lib/system/libxpc.dylib"]);
        let want: BTreeSet<String> = [
            "___crashreporter_info__",
            "_libSystem_init_after_boot_tasks_4launchd",
            "_mach_init_routine",
            "weird symbol, with a comma",
            "it's",
            "_OBJC_CLASS_$_NSThing",
            "_OBJC_METACLASS_$_NSThing",
            "_OBJC_IVAR_$_NSThing._x",
            "_w",
            "_re",
        ]
        .iter()
        .map(|s| s.to_string())
        .collect();
        assert_eq!(docs[0].symbols, want);
        assert_eq!(docs[1].install_name, "/usr/lib/system/libcache.dylib");
        assert!(docs[1].symbols.contains("_cache_create"));
        let other = exports(SDK_STYLE, "arm64-ios").unwrap();
        assert_eq!(other[0].symbols.iter().cloned().collect::<Vec<_>>(), vec!["_only_arm64"]);
    }

    #[test]
    fn refuses_what_it_does_not_read() {
        assert!(parse("{ \"tapi_tbd_version\": 5 }").unwrap_err().to_string().contains("v5"));
        assert!(parse("--- !tapi-tbd\nsymbols: [ a, b\n").is_err());
        assert!(parse("--- !tapi-tbd\nexports: { a: b }\n").is_err());
    }

    #[test]
    fn writes_what_it_reads() {
        let stub = Stub {
            install_name: "/System/Library/Frameworks/X.framework/X".into(),
            current_version: Some("382.5.3".into()),
            umbrella: Some("System".into()),
            reexported: vec!["/usr/lib/a.dylib".into(), "/usr/lib/b.dylib".into()],
            symbols: vec!["_a".into(), "_$s3Foo".into(), "-[Weird name]".into(), "true".into()],
            weak: vec!["_w".into()],
            thread_local: vec!["_tls".into()],
            objc_classes: vec!["Cls".into()],
            objc_eh_types: vec!["Eh".into()],
            objc_ivars: vec!["Cls._i".into()],
        };
        let lib = Stub { install_name: "/usr/lib/a.dylib".into(), symbols: vec!["_in_a".into()], ..Default::default() };
        let text = write_file(&[&stub, &lib], "arm64e-ios");
        let docs = exports(&text, "arm64e-ios").unwrap();
        assert_eq!(docs.len(), 2);
        assert_eq!(docs[0].reexports, vec!["/usr/lib/a.dylib", "/usr/lib/b.dylib"]);
        for s in ["_a", "_$s3Foo", "-[Weird name]", "true", "_w", "_tls", "_OBJC_CLASS_$_Cls", "_OBJC_METACLASS_$_Cls", "_OBJC_EHTYPE_$_Eh", "_OBJC_IVAR_$_Cls._i"] {
            assert!(docs[0].symbols.contains(s), "{s}");
        }
        assert_eq!(docs[0].symbols.len(), 10);
        assert!(docs[1].symbols.contains("_in_a"));
        let parsed = parse(&text).unwrap();
        assert_eq!(parsed[0].get("current-version").and_then(Y::as_str), Some("382.5.3"));
        assert_eq!(parsed[0].get("parent-umbrella").unwrap().items()[0].get("umbrella").and_then(Y::as_str), Some("System"));
    }

    #[test]
    fn the_layout_of_the_24a446_stubs() {
        // usr/lib/libAmber.tbd as `ipsw dyld tbd` wrote it for 24A446 (the text is not Apple's: names only)
        let stub = Stub { install_name: "/usr/lib/libAmber.dylib".into(), current_version: Some("1".into()), symbols: vec!["_AmberDiskImagePluginCreate".into()], ..Default::default() };
        assert_eq!(
            write_file(&[&stub], "arm64e-ios"),
            "--- !tapi-tbd\ntbd-version:          4\ntargets:              [ arm64e-ios ]\ninstall-name:         '/usr/lib/libAmber.dylib'\ncurrent-version:      1\nexports:\n  - targets:          [ arm64e-ios ]\n    symbols:          [ _AmberDiskImagePluginCreate ]\n...\n"
        );
        let two = Stub { install_name: "/usr/lib/dyld".into(), symbols: vec!["__dyld_start".into(), "_lldb_image_notifier".into()], objc_classes: vec!["A".into()], ..Default::default() };
        assert_eq!(
            write_doc(&two, "arm64e-ios"),
            "--- !tapi-tbd\ntbd-version:          4\ntargets:              [ arm64e-ios ]\ninstall-name:         '/usr/lib/dyld'\nexports:\n  - targets:          [ arm64e-ios ]\n    symbols:          [ __dyld_start,\n                       _lldb_image_notifier ]\n    objc-classes:    [ A ]\n"
        );
    }
}
