//! The tweaks' property lists: read and written by the `plist` crate (binary as Apple's CoreFoundation reads it,
//! or XML), held as `P` while a tweak changes them.
//!
//! `P` is the tweaks' own tree: a dictionary keeps the file's key order (the launchd cache's jobs are found in
//! that order; the crate's `Dictionary` keeps it too), and the comparisons the tweaks make (`py_eq`: a number
//! equals the same number of another kind, as the tweaks' rules were written) and the truth of a value are here.
//! The writers sort a dictionary's keys, so the same value always gives the same bytes. A date is kept in
//! microseconds from 2001-01-01; the crate keeps what the file holds (a binary date's seconds, an XML date's
//! text), and the microseconds are rounded from that.


/// A value of a property list.
#[derive(Clone, Debug)]
pub enum P {
    /// `None` (a binary list's 0x00; never in XML)
    Null,
    Bool(bool),
    /// `int`, from -2**63 to 2**64 - 1 when written
    Int(i128),
    Real(f64),
    /// `datetime.datetime` (naive), in microseconds from 2001-01-01 00:00:00
    Date(i64),
    Data(Vec<u8>),
    Str(String),
    /// a UID (binary only)
    Uid(u128),
    Array(Vec<P>),
    /// A dict: insertion order, as Python keeps it.
    Dict(Vec<(String, P)>),
}

impl P {
    pub fn dict() -> P {
        P::Dict(Vec::new())
    }
    pub fn get(&self, key: &str) -> Option<&P> {
        match self {
            P::Dict(items) => items.iter().find(|(k, _)| k == key).map(|(_, v)| v),
            _ => None,
        }
    }
    pub fn get_mut(&mut self, key: &str) -> Option<&mut P> {
        match self {
            P::Dict(items) => items.iter_mut().find(|(k, _)| k == key).map(|(_, v)| v),
            _ => None,
        }
    }
    /// `d[key] = value`: in place when the key is there, else at the end.
    pub fn set(&mut self, key: &str, value: P) {
        let P::Dict(items) = self else { panic!("plist: set on a non-dict") };
        match items.iter_mut().find(|(k, _)| k == key) {
            Some(slot) => slot.1 = value,
            None => items.push((key.to_string(), value)),
        }
    }
    /// `d.pop(key, None)` / `del d[key]`
    pub fn remove(&mut self, key: &str) -> Option<P> {
        let P::Dict(items) = self else { return None };
        let at = items.iter().position(|(k, _)| k == key)?;
        Some(items.remove(at).1)
    }
    pub fn contains(&self, key: &str) -> bool {
        self.get(key).is_some()
    }
    pub fn is_dict(&self) -> bool {
        matches!(self, P::Dict(_))
    }
    pub fn as_str(&self) -> Option<&str> {
        match self {
            P::Str(s) => Some(s),
            _ => None,
        }
    }
    pub fn items(&self) -> &[(String, P)] {
        match self {
            P::Dict(items) => items,
            _ => &[],
        }
    }
    pub fn items_mut(&mut self) -> Option<&mut Vec<(String, P)>> {
        match self {
            P::Dict(items) => Some(items),
            _ => None,
        }
    }
    /// Python's truth value (`if value:`).
    pub fn truthy(&self) -> bool {
        match self {
            P::Null => false,
            P::Bool(b) => *b,
            P::Int(i) => *i != 0,
            P::Real(f) => *f != 0.0,
            P::Date(_) | P::Uid(_) => true,
            P::Data(d) => !d.is_empty(),
            P::Str(s) => !s.is_empty(),
            P::Array(a) => !a.is_empty(),
            P::Dict(d) => !d.is_empty(),
        }
    }
    /// The Python type, for `type(a) is not type(b)`.
    pub fn kind(&self) -> u8 {
        match self {
            P::Null => 0,
            P::Bool(_) => 1,
            P::Int(_) => 2,
            P::Real(_) => 3,
            P::Date(_) => 4,
            P::Data(_) => 5,
            P::Str(_) => 6,
            P::Uid(_) => 7,
            P::Array(_) => 8,
            P::Dict(_) => 9,
        }
    }
    /// `str(value)` of Python, for the few messages that print a value read from a plist.
    pub fn py_str(&self) -> String {
        match self {
            P::Null => "None".into(),
            P::Bool(true) => "True".into(),
            P::Bool(false) => "False".into(),
            P::Int(i) => i.to_string(),
            P::Real(f) => format!("{f:?}"),
            P::Str(s) => s.clone(),
            other => format!("{other:?}"),
        }
    }
}

/// Python's `==` of two values: numbers across bool, int and float (exactly), containers element by element
/// (a dict's order does not count).
pub fn py_eq(a: &P, b: &P) -> bool {
    fn num(v: &P) -> Option<Num> {
        match v {
            P::Bool(b) => Some(Num::I(*b as i128)),
            P::Int(i) => Some(Num::I(*i)),
            P::Real(f) => Some(Num::F(*f)),
            _ => None,
        }
    }
    enum Num {
        I(i128),
        F(f64),
    }
    if let (Some(x), Some(y)) = (num(a), num(b)) {
        return match (x, y) {
            (Num::I(x), Num::I(y)) => x == y,
            (Num::F(x), Num::F(y)) => x == y,
            (Num::I(i), Num::F(f)) | (Num::F(f), Num::I(i)) => f.is_finite() && f.fract() == 0.0 && f.abs() < 1.7e38 && f as i128 == i,
        };
    }
    match (a, b) {
        (P::Null, P::Null) => true,
        (P::Date(x), P::Date(y)) => x == y,
        (P::Data(x), P::Data(y)) => x == y,
        (P::Str(x), P::Str(y)) => x == y,
        (P::Uid(x), P::Uid(y)) => x == y,
        (P::Array(x), P::Array(y)) => x.len() == y.len() && x.iter().zip(y).all(|(x, y)| py_eq(x, y)),
        (P::Dict(x), P::Dict(y)) => x.len() == y.len() && x.iter().all(|(k, v)| y.iter().any(|(k2, v2)| k == k2 && py_eq(v, v2))),
        _ => false,
    }
}

// ---------------------------------------------------------------- the crate's values

/// 2001-01-01 00:00:00 UTC, the epoch of a property list's dates, as Unix seconds.
const EPOCH_2001: u64 = 978_307_200;

fn from_crate(v: ::plist::Value) -> Result<P, String> {
    use ::plist::Value as V;
    Ok(match v {
        V::Boolean(b) => P::Bool(b),
        V::Integer(i) => match (i.as_signed(), i.as_unsigned()) {
            (Some(s), _) => P::Int(s as i128),
            (None, Some(u)) => P::Int(u as i128),
            (None, None) => return Err(format!("an integer out of range: {i:?}")),
        },
        V::Real(f) => P::Real(f),
        V::Date(d) => {
            let t: std::time::SystemTime = d.into();
            let epoch = std::time::UNIX_EPOCH + std::time::Duration::from_secs(EPOCH_2001);
            let us = match t.duration_since(epoch) {
                // a binary date is a double of seconds: to the nearest microsecond
                Ok(after) => ((after.as_nanos() + 500) / 1000) as i64,
                Err(before) => -(((before.duration().as_nanos() + 500) / 1000) as i64),
            };
            P::Date(us)
        }
        V::Data(d) => P::Data(d),
        V::String(s) => P::Str(s),
        V::Uid(u) => P::Uid(u.get() as u128),
        V::Array(a) => P::Array(a.into_iter().map(from_crate).collect::<Result<_, _>>()?),
        V::Dictionary(d) => P::Dict(d.into_iter().map(|(k, v)| Ok((k, from_crate(v)?))).collect::<Result<_, String>>()?),
        other => return Err(format!("a value this reader does not know: {other:?}")),
    })
}

/// The crate's value of `v`, a dictionary's keys sorted. For XML (`whole`) a date is cut to its second: an XML
/// date of Apple's format has no fraction (the crate would write one).
fn to_crate(v: &P, whole: bool) -> Result<::plist::Value, String> {
    use ::plist::Value as V;
    Ok(match v {
        P::Null => return Err("a property list holds no None".into()),
        P::Bool(b) => V::Boolean(*b),
        P::Int(i) => {
            if let Ok(s) = i64::try_from(*i) {
                V::Integer(s.into())
            } else if let Ok(u) = u64::try_from(*i) {
                V::Integer(u.into())
            } else {
                return Err(format!("an integer out of range: {i}"));
            }
        }
        P::Real(f) => V::Real(*f),
        P::Date(us) => {
            let us = &if whole { us.div_euclid(1_000_000) * 1_000_000 } else { *us };
            let epoch = std::time::UNIX_EPOCH + std::time::Duration::from_secs(EPOCH_2001);
            let t = if *us >= 0 {
                epoch.checked_add(std::time::Duration::from_micros(*us as u64))
            } else {
                epoch.checked_sub(std::time::Duration::from_micros(us.unsigned_abs()))
            };
            V::Date(t.ok_or_else(|| format!("a date out of range: {us}"))?.into())
        }
        P::Data(d) => V::Data(d.clone()),
        P::Str(s) => V::String(s.clone()),
        P::Uid(u) => V::Uid(::plist::Uid::new(u64::try_from(*u).map_err(|_| format!("a UID out of range: {u}"))?)),
        P::Array(a) => V::Array(a.iter().map(|v| to_crate(v, whole)).collect::<Result<_, _>>()?),
        P::Dict(items) => {
            let mut sorted: Vec<&(String, P)> = items.iter().collect();
            sorted.sort_by(|a, b| a.0.cmp(&b.0));
            let mut d = ::plist::Dictionary::new();
            for (k, v) in sorted {
                d.insert(k.clone(), to_crate(v, whole)?);
            }
            V::Dictionary(d)
        }
    })
}

/// A property list, binary or XML (the crate tells them apart by their first bytes).
pub fn loads(data: &[u8]) -> Result<P, String> {
    let value = ::plist::Value::from_reader(std::io::Cursor::new(data)).map_err(|e| e.to_string())?;
    from_crate(value)
}

/// The binary property list of `value`.
pub fn dumps_binary(value: &P) -> Result<Vec<u8>, String> {
    let mut out = Vec::new();
    to_crate(value, false)?.to_writer_binary(&mut out).map_err(|e| e.to_string())?;
    Ok(out)
}

/// The XML property list of `value` (the crate's layout: a tab a level), with a line end after `</plist>`.
pub fn dumps_xml(value: &P) -> Result<Vec<u8>, String> {
    let mut out = Vec::new();
    to_crate(value, true)?.to_writer_xml(&mut out).map_err(|e| e.to_string())?;
    if !out.ends_with(b"\n") {
        out.push(b'\n');
    }
    Ok(out)
}

/// A value as text for a test's message.
pub fn show(v: &P) -> String {
    format!("{v:?}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn apples_global_preferences_read_and_written_again() {
        // mobile's .GlobalPreferences.plist of 24A446's OS volume, 89 bytes (two locale words in CF's binary
        // layout; not a file of Apple's code)
        let raw = b"bplist00\xd2\x01\x02\x03\x04[AppleLocale^AppleLanguagesUen_US\xa1\x05Ren\x08\x0d\x19\x28\x2e\x30\x00\x00\x00\x00\x00\x00\x01\x01\x00\x00\x00\x00\x00\x00\x00\x06\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x33";
        let v = loads(raw).unwrap();
        assert_eq!(v.get("AppleLocale").and_then(P::as_str), Some("en_US"));
        let again = dumps_binary(&v).unwrap();
        assert!(again.starts_with(b"bplist00"));
        assert!(py_eq(&loads(&again).unwrap(), &v));
    }

    fn sample(uid: bool) -> P {
        let mut v = P::dict();
        // 2026-11-21T10:20:30Z and 123457 microseconds
        let date = (1_795_256_430 - EPOCH_2001 as i64) * 1_000_000 + 123_457;
        let mut inner = P::dict();
        inner.set("z", P::Int(1));
        inner.set("y", P::Array(vec![]));
        v.set("a", P::Array(vec![P::Int(1), P::Int(1), P::Bool(true), P::Real(1.0), P::Real(-0.5), P::Str("x".into()), P::Str("\u{e9}".into()), P::Data(vec![0; 100]), P::Date(date), inner, P::dict()]));
        v.set("big", P::Int((1i128 << 64) - 1));
        v.set("neg", P::Int(-5));
        if uid {
            v.set("u", P::Uid(300));
        }
        v
    }

    #[test]
    fn written_and_read_back() {
        let bin = dumps_binary(&sample(true)).unwrap();
        assert!(bin.starts_with(b"bplist00"));
        assert!(py_eq(&loads(&bin).unwrap(), &sample(true)));
        let xml = String::from_utf8(dumps_xml(&sample(false)).unwrap()).unwrap();
        assert!(xml.starts_with("<?xml version=\"1.0\" encoding=\"UTF-8\"?>"), "{xml}");
        assert!(xml.contains("<date>2026-11-21T10:20:30Z</date>"), "{xml}");
        assert!(xml.find("<key>big</key>").unwrap() < xml.find("<key>neg</key>").unwrap());
        assert!(xml.ends_with("</plist>\n"));
        // the XML date keeps whole seconds
        let mut want = sample(false);
        let P::Array(a) = want.get_mut("a").unwrap() else { unreachable!() };
        a[8] = P::Date((1_795_256_430 - EPOCH_2001 as i64) * 1_000_000);
        assert!(py_eq(&loads(xml.as_bytes()).unwrap(), &want));
        // the same value, the same bytes, whatever the order of its keys
        let mut other = P::dict();
        for (k, v) in sample(true).items().iter().rev() {
            other.set(k, v.clone());
        }
        assert_eq!(dumps_binary(&other).unwrap(), bin);
        assert!(dumps_binary(&P::Null).is_err());
        assert!(loads(b"not a plist").is_err());
    }
}
