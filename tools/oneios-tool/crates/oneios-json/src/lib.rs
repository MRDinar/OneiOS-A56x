//! `oneios-tool json`: what the shell scripts read from and change in a JSON file (a manifest's field, a record's
//! list), in place of the inline `python3 -c 'import json ...'` lines they had until 2026-11-21.
//!
//! A PATH names values from the top: segments joined by `.`. A segment is an object's key, a number (an element of
//! an array, from 0), or `KEY[FIELD=TEXT]`: the elements of the array at KEY whose FIELD is TEXT (a string field
//! compares as it is, any other as its JSON). An empty PATH or `.` is the whole document. A PATH can name several
//! values (a selector); `get` prints each, `set` changes each.
//!
//! An edited file keeps its keys' order and is written as Python's `json.dump(..., indent=2)` wrote it: two spaces,
//! `": "` between a key and its value, a newline at the end.

use anyhow::{anyhow, bail, Context, Result};
use clap::Subcommand;
use indexmap::IndexMap;
use serde::{Deserialize, Serialize};
use std::io::{Read, Write};
use std::path::Path;

/// An object whose keys keep their order.
pub type Map = IndexMap<String, Value>;

/// A JSON value whose objects keep their keys' order.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum Value {
    Null,
    Bool(bool),
    Number(serde_json::Number),
    String(String),
    Array(Vec<Value>),
    Object(IndexMap<String, Value>),
}

impl Value {
    /// A string as it is; anything else as compact JSON.
    pub fn text(&self) -> String {
        match self {
            Value::String(s) => s.clone(),
            other => serde_json::to_string(other).unwrap_or_default(),
        }
    }
}

#[derive(Subcommand, Debug)]
pub enum Command {
    /// Print the value(s) at PATH: a string as it is (--json: quoted), anything else as compact JSON, one per line.
    /// Exit 1 when PATH names nothing (with --default the text is printed instead)
    Get {
        #[arg(long)]
        json: bool,
        #[arg(long)]
        default: Option<String>,
        /// The file, `-` for standard input
        file: String,
        path: String,
    },
    /// One line per element of the array at PATH: the FIELDs, separated by a tab (a field that is not there is an
    /// error, or --missing TEXT)
    Lines {
        #[arg(long)]
        missing: Option<String>,
        file: String,
        path: String,
        #[arg(required = true)]
        fields: Vec<String>,
    },
    /// The number of elements (an array), keys (an object) or bytes (a string) at PATH
    Len { file: String, path: String },
    /// Set the value(s) at PATH to VALUE (JSON; --string: VALUE is a string as it is). The last key of PATH is made
    /// when its object has none. The file is written again (keys in their order, indent 2)
    Set {
        #[arg(long)]
        string: bool,
        file: String,
        path: String,
        value: String,
    },
    /// Exit 1 (and say which) when one of the files is not JSON
    Check {
        #[arg(required = true)]
        files: Vec<String>,
    },
}

/// One segment of a PATH.
#[derive(Debug, Clone, PartialEq)]
enum Seg {
    Key(String),
    Index(usize),
    Select { key: String, field: String, text: String },
}

fn parse_path(path: &str) -> Result<Vec<Seg>> {
    let path = path.trim();
    if path.is_empty() || path == "." {
        return Ok(Vec::new());
    }
    let mut out = Vec::new();
    for raw in split_path(path) {
        if raw.is_empty() {
            bail!("path {path:?}: an empty segment");
        }
        if let Some(open) = raw.find('[') {
            let inner = raw[open + 1..].strip_suffix(']').ok_or_else(|| anyhow!("path {path:?}: {raw:?} has no ']' at its end"))?;
            let (field, text) = inner.split_once('=').ok_or_else(|| anyhow!("path {path:?}: {raw:?}: [FIELD=TEXT]"))?;
            out.push(Seg::Select { key: raw[..open].to_string(), field: field.to_string(), text: text.to_string() });
        } else if let Ok(n) = raw.parse::<usize>() {
            out.push(Seg::Index(n));
        } else {
            out.push(Seg::Key(raw.to_string()));
        }
    }
    Ok(out)
}

/// The segments of a path: split at the dots that are outside brackets (a selector's TEXT may hold dots).
fn split_path(path: &str) -> Vec<&str> {
    let (mut out, mut depth, mut start) = (Vec::new(), 0usize, 0usize);
    for (i, c) in path.char_indices() {
        match c {
            '[' => depth += 1,
            ']' => depth = depth.saturating_sub(1),
            '.' if depth == 0 => {
                out.push(&path[start..i]);
                start = i + 1;
            }
            _ => {}
        }
    }
    out.push(&path[start..]);
    out
}

fn matches(v: &Value, field: &str, text: &str) -> bool {
    match v {
        Value::Object(m) => m.get(field).is_some_and(|f| f.text() == text),
        _ => false,
    }
}

/// The values a path names (none when it names nothing).
pub fn select<'a>(root: &'a Value, path: &str) -> Result<Vec<&'a Value>> {
    let mut now = vec![root];
    for seg in parse_path(path)? {
        let mut next = Vec::new();
        for v in now {
            match (&seg, v) {
                (Seg::Key(k), Value::Object(m)) => next.extend(m.get(k)),
                (Seg::Index(i), Value::Array(a)) => next.extend(a.get(*i)),
                // a number is also an object's key ("0")
                (Seg::Index(i), Value::Object(m)) => next.extend(m.get(&i.to_string())),
                (Seg::Select { key, field, text }, Value::Object(m)) => {
                    if let Some(Value::Array(a)) = m.get(key) {
                        next.extend(a.iter().filter(|e| matches(e, field, text)));
                    }
                }
                _ => {}
            }
        }
        now = next;
    }
    Ok(now)
}

/// Set every value the path names to `new`; the last segment, a key, is made where its object lacks it.
/// Returns how many values were set.
pub fn set(root: &mut Value, path: &str, new: &Value) -> Result<usize> {
    let segs = parse_path(path)?;
    let Some((last, head)) = segs.split_last() else {
        *root = new.clone();
        return Ok(1);
    };
    fn walk<'a>(v: &'a mut Value, segs: &[Seg], out: &mut Vec<&'a mut Value>) {
        let Some((seg, rest)) = segs.split_first() else {
            out.push(v);
            return;
        };
        match (seg, v) {
            (Seg::Key(k), Value::Object(m)) => {
                if let Some(c) = m.get_mut(k) {
                    walk(c, rest, out)
                }
            }
            (Seg::Index(i), Value::Array(a)) => {
                if let Some(c) = a.get_mut(*i) {
                    walk(c, rest, out)
                }
            }
            (Seg::Index(i), Value::Object(m)) => {
                if let Some(c) = m.get_mut(&i.to_string()) {
                    walk(c, rest, out)
                }
            }
            (Seg::Select { key, field, text }, Value::Object(m)) => {
                if let Some(Value::Array(a)) = m.get_mut(key) {
                    for e in a.iter_mut().filter(|e| matches(e, field, text)) {
                        walk(e, rest, out)
                    }
                }
            }
            _ => {}
        }
    }
    let mut parents = Vec::new();
    walk(root, head, &mut parents);
    let mut n = 0;
    for p in parents {
        match (last, p) {
            (Seg::Key(k), Value::Object(m)) => {
                m.insert(k.clone(), new.clone());
                n += 1;
            }
            (Seg::Index(i), Value::Array(a)) if *i < a.len() => {
                a[*i] = new.clone();
                n += 1;
            }
            (Seg::Select { key, field, text }, Value::Object(m)) => {
                if let Some(Value::Array(a)) = m.get_mut(key) {
                    for e in a.iter_mut().filter(|e| matches(e, field, text)) {
                        *e = new.clone();
                        n += 1;
                    }
                }
            }
            _ => {}
        }
    }
    Ok(n)
}

/// Python's `json.dumps(v, indent=2)` for what our files hold (it escapes non-ASCII as \uXXXX: so does this).
pub fn to_pretty(v: &Value) -> String {
    let text = serde_json::to_string_pretty(v).unwrap_or_default();
    let mut out = String::with_capacity(text.len());
    for c in text.chars() {
        if c.is_ascii() {
            out.push(c);
        } else {
            let mut buf = [0u16; 2];
            for unit in c.encode_utf16(&mut buf) {
                out.push_str(&format!("\\u{unit:04x}"));
            }
        }
    }
    out
}

pub fn read(file: &str) -> Result<Value> {
    let mut text = String::new();
    if file == "-" {
        std::io::stdin().read_to_string(&mut text).context("standard input")?;
    } else {
        text = std::fs::read_to_string(file).with_context(|| file.to_string())?;
    }
    serde_json::from_str(&text).with_context(|| format!("{}: not JSON", if file == "-" { "standard input" } else { file }))
}

fn write(file: &str, v: &Value) -> Result<()> {
    let path = Path::new(file);
    let part = path.with_file_name(format!(".{}.part", path.file_name().and_then(|n| n.to_str()).unwrap_or("json")));
    std::fs::write(&part, to_pretty(v) + "\n").with_context(|| part.display().to_string())?;
    std::fs::rename(&part, path).with_context(|| file.to_string())
}

fn run(command: Command) -> Result<i32> {
    let mut out = std::io::stdout().lock();
    match command {
        Command::Get { json, default, file, path } => {
            let doc = read(&file)?;
            let found = select(&doc, &path)?;
            if found.is_empty() {
                return match default {
                    Some(d) => {
                        writeln!(out, "{d}")?;
                        Ok(0)
                    }
                    None => {
                        eprintln!("oneios-tool json: {file}: nothing at {path:?}");
                        Ok(1)
                    }
                };
            }
            for v in found {
                let line = if json { serde_json::to_string(v)? } else { v.text() };
                writeln!(out, "{line}")?;
            }
        }
        Command::Lines { missing, file, path, fields } => {
            let doc = read(&file)?;
            for v in select(&doc, &path)? {
                let Value::Array(items) = v else { bail!("{file}: {path:?} is not an array") };
                for item in items {
                    let mut cols = Vec::new();
                    for f in &fields {
                        match select(item, f)?.first() {
                            Some(x) => cols.push(x.text()),
                            None => match &missing {
                                Some(m) => cols.push(m.clone()),
                                None => bail!("{file}: an element of {path:?} has no {f:?}"),
                            },
                        }
                    }
                    writeln!(out, "{}", cols.join("\t"))?;
                }
            }
        }
        Command::Len { file, path } => {
            let doc = read(&file)?;
            let found = select(&doc, &path)?;
            let Some(v) = found.first() else {
                eprintln!("oneios-tool json: {file}: nothing at {path:?}");
                return Ok(1);
            };
            let n = match v {
                Value::Array(a) => a.len(),
                Value::Object(m) => m.len(),
                Value::String(s) => s.len(),
                _ => bail!("{file}: {path:?} has no length"),
            };
            writeln!(out, "{n}")?;
        }
        Command::Set { string, file, path, value } => {
            let mut doc = read(&file)?;
            let new = if string { Value::String(value) } else { serde_json::from_str(&value).with_context(|| format!("VALUE {value:?}: not JSON (--string for a string)"))? };
            if set(&mut doc, &path, &new)? == 0 {
                eprintln!("oneios-tool json: {file}: nothing at {path:?} to set");
                return Ok(1);
            }
            write(&file, &doc)?;
        }
        Command::Check { files } => {
            let mut status = 0;
            for f in files {
                if let Err(e) = read(&f) {
                    eprintln!("oneios-tool json: {e:#}");
                    status = 1;
                }
            }
            return Ok(status);
        }
    }
    Ok(0)
}

pub fn main(command: Command) -> i32 {
    match run(command) {
        Ok(n) => n,
        Err(e) => {
            eprintln!("oneios-tool json: {e:#}");
            1
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn doc() -> Value {
        serde_json::from_str(r#"{"z": 1, "a": {"b": "x"}, "files": [{"path": "p/1.txt", "size": 3}, {"path": "q", "size": 4, "mode": "0755"}], "n": null, "ok": true}"#).unwrap()
    }

    #[test]
    fn get_keys_indexes_and_selectors() {
        let d = doc();
        assert_eq!(select(&d, "a.b").unwrap()[0].text(), "x");
        assert_eq!(select(&d, "files.1.size").unwrap()[0].text(), "4");
        assert_eq!(select(&d, "files[path=p/1.txt].size").unwrap()[0].text(), "3");
        assert_eq!(select(&d, "files[size=4].path").unwrap()[0].text(), "q");
        assert!(select(&d, "files[path=none]").unwrap().is_empty());
        assert!(select(&d, "a.c").unwrap().is_empty());
        assert_eq!(select(&d, "n").unwrap()[0].text(), "null");
        assert_eq!(select(&d, "ok").unwrap()[0].text(), "true");
        assert_eq!(select(&d, "files.0").unwrap()[0].text(), r#"{"path":"p/1.txt","size":3}"#);
        assert_eq!(select(&d, ".").unwrap().len(), 1);
    }

    #[test]
    fn set_keeps_the_order_and_writes_as_python() {
        let mut d = doc();
        assert_eq!(set(&mut d, "files[path=q].sha256", &Value::String("ab".into())).unwrap(), 1);
        assert_eq!(set(&mut d, "a.b", &Value::String("y".into())).unwrap(), 1);
        assert_eq!(set(&mut d, "new", &serde_json::from_str("[1, 2]").unwrap()).unwrap(), 1);
        assert_eq!(set(&mut d, "missing.x", &Value::Null).unwrap(), 0);
        let text = to_pretty(&d);
        // Python: json.dumps(d, indent=2) of the same edits
        assert_eq!(text, "{\n  \"z\": 1,\n  \"a\": {\n    \"b\": \"y\"\n  },\n  \"files\": [\n    {\n      \"path\": \"p/1.txt\",\n      \"size\": 3\n    },\n    {\n      \"path\": \"q\",\n      \"size\": 4,\n      \"mode\": \"0755\",\n      \"sha256\": \"ab\"\n    }\n  ],\n  \"n\": null,\n  \"ok\": true,\n  \"new\": [\n    1,\n    2\n  ]\n}");
    }

    #[test]
    fn non_ascii_is_escaped_as_python_does() {
        let v: Value = serde_json::from_str("{\"k\": \"caf\u{e9} \u{1F600}\"}").unwrap();
        let back = to_pretty(&v);
        assert!(back.is_ascii());
        assert_eq!(serde_json::from_str::<Value>(&back).unwrap(), v);
        assert!(back.contains("\\u00e9") && back.contains("\\ud83d\\ude00"));
    }

    #[test]
    fn dotted_text_in_a_selector() {
        let d: Value = serde_json::from_str(r#"{"patches": [{"path": "/a/b.patch", "sha256": "0"}]}"#).unwrap();
        assert_eq!(select(&d, "patches[path=/a/b.patch].sha256").unwrap()[0].text(), "0");
        assert!(parse_path("a[b").is_err());
    }
}
