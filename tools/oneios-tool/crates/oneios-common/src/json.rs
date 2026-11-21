//! The JSON of the tools' records, reports and the image set's manifest: read and written by serde_json.
//!
//! `Value` is the tools' own tree because an object keeps its keys in the order they were set (or read), which
//! serde_json's `Map` does only with its `preserve_order` feature, and that must stay off in this workspace
//! (Cargo.toml). Everything else is serde_json's: `dumps` writes with its `PrettyFormatter` (an indent of N
//! spaces) or its compact writer (no indent: no spaces at all), strings as UTF-8 with serde_json's escapes,
//! floats as serde_json writes them (`1.0`, `1e20`; a NaN or an infinity as `null`); `parse` is serde_json's
//! parser. An integer is kept as an integer (`1` and `1.0` stay apart); one beyond 64 bits reads as a float.

use serde::de::{Deserialize, Deserializer, MapAccess, SeqAccess, Visitor};
use serde::ser::{Serialize, SerializeMap, SerializeSeq, Serializer};

#[derive(Clone, Debug, PartialEq)]
pub enum Value {
    Null,
    Bool(bool),
    Int(i128),
    Float(f64),
    Str(String),
    List(Vec<Value>),
    Dict(Vec<(String, Value)>),
}

impl Value {
    pub fn dict() -> Value {
        Value::Dict(Vec::new())
    }
    pub fn list() -> Value {
        Value::List(Vec::new())
    }
    pub fn str(s: impl Into<String>) -> Value {
        Value::Str(s.into())
    }
    /// Set a key: replaces the value in place when the key exists, else appends.
    pub fn set(&mut self, key: impl Into<String>, value: impl Into<Value>) {
        let key = key.into();
        let Value::Dict(items) = self else { panic!("json: set on a non-object") };
        match items.iter_mut().find(|(k, _)| *k == key) {
            Some(slot) => slot.1 = value.into(),
            None => items.push((key, value.into())),
        }
    }
    pub fn push(&mut self, value: impl Into<Value>) {
        let Value::List(items) = self else { panic!("json: push on a non-list") };
        items.push(value.into());
    }
    pub fn get(&self, key: &str) -> Option<&Value> {
        match self {
            Value::Dict(items) => items.iter().find(|(k, _)| k == key).map(|(_, v)| v),
            _ => None,
        }
    }
    pub fn get_mut(&mut self, key: &str) -> Option<&mut Value> {
        match self {
            Value::Dict(items) => items.iter_mut().find(|(k, _)| k == key).map(|(_, v)| v),
            _ => None,
        }
    }
    pub fn as_str(&self) -> Option<&str> {
        match self {
            Value::Str(s) => Some(s),
            _ => None,
        }
    }
    pub fn as_int(&self) -> Option<i128> {
        match self {
            Value::Int(i) => Some(*i),
            _ => None,
        }
    }
    pub fn as_u64(&self) -> Option<u64> {
        self.as_int().and_then(|i| u64::try_from(i).ok())
    }
    pub fn as_f64(&self) -> Option<f64> {
        match self {
            Value::Float(f) => Some(*f),
            Value::Int(i) => Some(*i as f64),
            _ => None,
        }
    }
    pub fn as_bool(&self) -> Option<bool> {
        match self {
            Value::Bool(b) => Some(*b),
            _ => None,
        }
    }
    pub fn as_list(&self) -> Option<&[Value]> {
        match self {
            Value::List(items) => Some(items),
            _ => None,
        }
    }
    pub fn as_dict(&self) -> Option<&[(String, Value)]> {
        match self {
            Value::Dict(items) => Some(items),
            _ => None,
        }
    }
    pub fn is_null(&self) -> bool {
        matches!(self, Value::Null)
    }
}

macro_rules! from_int {
    ($($t:ty),*) => {$(impl From<$t> for Value { fn from(v: $t) -> Value { Value::Int(v as i128) } })*};
}
from_int!(i8, i16, i32, i64, i128, isize, u8, u16, u32, u64, usize);
impl From<bool> for Value {
    fn from(v: bool) -> Value {
        Value::Bool(v)
    }
}
impl From<f64> for Value {
    fn from(v: f64) -> Value {
        Value::Float(v)
    }
}
impl From<&str> for Value {
    fn from(v: &str) -> Value {
        Value::Str(v.to_string())
    }
}
impl From<String> for Value {
    fn from(v: String) -> Value {
        Value::Str(v)
    }
}
impl From<&String> for Value {
    fn from(v: &String) -> Value {
        Value::Str(v.clone())
    }
}
impl<T: Into<Value>> From<Vec<T>> for Value {
    fn from(v: Vec<T>) -> Value {
        Value::List(v.into_iter().map(Into::into).collect())
    }
}
impl<T: Into<Value>> From<Option<T>> for Value {
    fn from(v: Option<T>) -> Value {
        v.map_or(Value::Null, Into::into)
    }
}

/// A value as serde sees it: an object's entries in their own order, or sorted by key.
struct Ordered<'a> {
    value: &'a Value,
    sort_keys: bool,
}

impl Serialize for Ordered<'_> {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        let inner = |value| Ordered { value, sort_keys: self.sort_keys };
        match self.value {
            Value::Null => s.serialize_unit(),
            Value::Bool(b) => s.serialize_bool(*b),
            Value::Int(i) => s.serialize_i128(*i),
            Value::Float(f) => s.serialize_f64(*f),
            Value::Str(text) => s.serialize_str(text),
            Value::List(items) => {
                let mut seq = s.serialize_seq(Some(items.len()))?;
                for item in items {
                    seq.serialize_element(&inner(item))?;
                }
                seq.end()
            }
            Value::Dict(items) => {
                let mut order: Vec<&(String, Value)> = items.iter().collect();
                if self.sort_keys {
                    order.sort_by(|a, b| a.0.cmp(&b.0));
                }
                let mut map = s.serialize_map(Some(order.len()))?;
                for (key, value) in order {
                    map.serialize_entry(key, &inner(value))?;
                }
                map.end()
            }
        }
    }
}

impl Serialize for Value {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        Ordered { value: self, sort_keys: false }.serialize(s)
    }
}

impl<'de> Deserialize<'de> for Value {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Value, D::Error> {
        struct V;
        impl<'de> Visitor<'de> for V {
            type Value = Value;
            fn expecting(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
                f.write_str("a JSON value")
            }
            fn visit_unit<E>(self) -> Result<Value, E> {
                Ok(Value::Null)
            }
            fn visit_none<E>(self) -> Result<Value, E> {
                Ok(Value::Null)
            }
            fn visit_some<D: Deserializer<'de>>(self, d: D) -> Result<Value, D::Error> {
                Value::deserialize(d)
            }
            fn visit_bool<E>(self, b: bool) -> Result<Value, E> {
                Ok(Value::Bool(b))
            }
            fn visit_i64<E>(self, i: i64) -> Result<Value, E> {
                Ok(Value::Int(i as i128))
            }
            fn visit_u64<E>(self, i: u64) -> Result<Value, E> {
                Ok(Value::Int(i as i128))
            }
            fn visit_i128<E>(self, i: i128) -> Result<Value, E> {
                Ok(Value::Int(i))
            }
            fn visit_f64<E>(self, f: f64) -> Result<Value, E> {
                Ok(Value::Float(f))
            }
            fn visit_str<E>(self, s: &str) -> Result<Value, E> {
                Ok(Value::Str(s.to_string()))
            }
            fn visit_string<E>(self, s: String) -> Result<Value, E> {
                Ok(Value::Str(s))
            }
            fn visit_seq<A: SeqAccess<'de>>(self, mut seq: A) -> Result<Value, A::Error> {
                let mut items = Vec::with_capacity(seq.size_hint().unwrap_or(0));
                while let Some(item) = seq.next_element()? {
                    items.push(item);
                }
                Ok(Value::List(items))
            }
            fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<Value, A::Error> {
                // a key given twice keeps its first place and its last value
                let mut dict = Value::dict();
                while let Some((key, value)) = map.next_entry::<String, Value>()? {
                    dict.set(key, value);
                }
                Ok(dict)
            }
        }
        d.deserialize_any(V)
    }
}

/// The text of a value: serde_json's pretty layout with `indent` spaces a level (also 0: one line per item), or
/// its compact one without an indent; keys in their order, or sorted with `sort_keys`.
pub fn dumps(value: &Value, indent: Option<usize>, sort_keys: bool) -> String {
    let value = Ordered { value, sort_keys };
    let mut out = Vec::new();
    let done = match indent {
        Some(n) => {
            let spaces = " ".repeat(n);
            let mut ser = serde_json::Serializer::with_formatter(&mut out, serde_json::ser::PrettyFormatter::with_indent(spaces.as_bytes()));
            value.serialize(&mut ser)
        }
        None => value.serialize(&mut serde_json::Serializer::new(&mut out)),
    };
    done.expect("a Vec takes every write, and every key is a string");
    String::from_utf8(out).expect("serde_json writes UTF-8")
}

/// Read a JSON text.
pub fn parse(text: &str) -> Result<Value, String> {
    serde_json::from_str(text).map_err(|e| format!("json: {e}"))
}

/// Read a JSON file.
pub fn load(path: impl AsRef<std::path::Path>) -> anyhow::Result<Value> {
    let path = path.as_ref();
    let text = std::fs::read_to_string(path).map_err(|e| anyhow::anyhow!("{}: {e}", path.display()))?;
    parse(&text).map_err(|e| anyhow::anyhow!("{}: {e}", path.display()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dumps_compact_and_pretty() {
        let mut d = Value::dict();
        d.set("b", 1);
        d.set("a", vec![Value::Int(1), Value::Null, Value::Bool(true), Value::Float(1.0), Value::Float(f64::NAN)]);
        d.set("e", Value::list());
        d.set("s", "a\"\\\n\u{e9}\u{7f}");
        assert_eq!(dumps(&d, None, false), "{\"b\":1,\"a\":[1,null,true,1.0,null],\"e\":[],\"s\":\"a\\\"\\\\\\n\u{e9}\u{7f}\"}");
        assert!(dumps(&d, None, true).starts_with("{\"a\":"));
        let mut small = Value::dict();
        small.set("k", vec![Value::Int(1), Value::Int(2)]);
        small.set("d", Value::dict());
        small.set("big", Value::Int(1 << 70));
        assert_eq!(dumps(&small, Some(1), false), "{\n \"k\": [\n  1,\n  2\n ],\n \"d\": {},\n \"big\": 1180591620717411303424\n}");
        assert_eq!(dumps(&small, Some(0), false), "{\n\"k\": [\n1,\n2\n],\n\"d\": {},\n\"big\": 1180591620717411303424\n}");
    }

    #[test]
    fn parse_keeps_order_and_kinds() {
        let v = parse(r#"{"z": 1, "a": [1.0, -2, 3.5e-07, "x"], "n": null, "t": true, "z": 2}"#).unwrap();
        assert_eq!(v.get("z").unwrap().as_int(), Some(2));
        assert_eq!(dumps(&v, None, false), r#"{"z":2,"a":[1.0,-2,3.5e-7,"x"],"n":null,"t":true}"#);
        // what Python's json module wrote before (", " and ": ", \u escapes) reads as the same values
        let old = parse(&format!("{{\"s\": \"caf{}u00e9\", \"n\": 18446744073709551615}}", '\\')).unwrap();
        assert_eq!(old.get("s").and_then(Value::as_str), Some("caf\u{e9}"));
        assert_eq!(old.get("n").and_then(Value::as_u64), Some(u64::MAX));
        assert!(parse("{\"a\": 1} x").is_err());
    }
}
