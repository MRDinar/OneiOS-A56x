//! Property lists as Python's `plistlib` (3.11 to 3.14) reads and writes them, byte for byte.
//!
//! The root tweaks write the Data seed's preference files, the launchd override and the feature flags with
//! `plistlib.dumps(value, fmt=..., sort_keys=True)`, and the seed's SHA-256 is pinned in the image's trust list,
//! so the writers here give the same bytes:
//! - binary (`dumps_binary`): plistlib's `_BinaryPlistWriter`. Objects in its order (depth first, a dictionary's
//!   keys before its values), scalars written once per (type, value) (`_scalars`: str, int, float, datetime, bytes;
//!   also `None`, one object), containers never shared; the smallest reference and offset sizes.
//! - XML (`dumps_xml`): plistlib's `_PlistWriter`: its header, tabs, `repr(float)`, the base64 lines of a `<data>`.
//!
//! The reader (`loads`) gives what plistlib gives: a dictionary keeps the file's key order (the launchd cache's
//! jobs are found in that order), a binary date is rounded to microseconds as `datetime.timedelta` does, an XML
//! one to seconds. A binary list whose containers are shared by two places is read, and said (`Loaded::shared`):
//! Python would write such a container once, this writer twice, so the tweaks refuse to write it back.

use std::fmt::Write as _;

/// A value of a property list: the Python types plistlib reads and writes.
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
    /// `plistlib.UID` (binary only)
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
            P::Real(f) => float_repr(*f),
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

/// `repr(float)` (plistlib's `<real>`): pyjson's, with Python's own words for the three special values.
pub fn float_repr(f: f64) -> String {
    if f.is_nan() {
        "nan".into()
    } else if f.is_infinite() {
        if f > 0.0 { "inf".into() } else { "-inf".into() }
    } else {
        oneios_common::pyjson::float_repr(f)
    }
}

// ---------------------------------------------------------------- dates

const US: i64 = 1_000_000;
/// datetime's range from 2001-01-01: 0001-01-01 00:00 to 9999-12-31 23:59:59.999999
const DATE_MIN_US: i64 = -730_485 * 86_400 * US;
const DATE_MAX_US: i64 = (3_652_059 - 730_486 + 1) * 86_400 * US - 1;

/// Days from 0001-01-01 (day 1) as Python's `date.toordinal()`.
fn ordinal(y: i64, m: i64, d: i64) -> i64 {
    let days_before_month = [0, 31, 59, 90, 120, 151, 181, 212, 243, 273, 304, 334];
    let leap = (y % 4 == 0 && y % 100 != 0) || y % 400 == 0;
    let yb = y - 1;
    yb * 365 + yb / 4 - yb / 100 + yb / 400 + days_before_month[(m - 1) as usize] + if m > 2 && leap { 1 } else { 0 } + d
}

fn days_in_month(y: i64, m: i64) -> i64 {
    let leap = (y % 4 == 0 && y % 100 != 0) || y % 400 == 0;
    match m {
        2 if leap => 29,
        2 => 28,
        4 | 6 | 9 | 11 => 30,
        _ => 31,
    }
}

/// (year, month, day, hour, minute, second) of a date, as Python's fields.
fn civil(us: i64) -> (i64, i64, i64, i64, i64, i64) {
    let secs = us.div_euclid(US);
    let days = secs.div_euclid(86_400) + ordinal(2001, 1, 1);
    let rem = secs.rem_euclid(86_400);
    // from an ordinal: count 400-, 100-, 4- and 1-year cycles (Python's _ord2ymd)
    let mut n = days - 1;
    let n400 = n.div_euclid(146_097);
    n = n.rem_euclid(146_097);
    let n100 = n / 36_524;
    n %= 36_524;
    let n4 = n / 1_461;
    n %= 1_461;
    let n1 = n / 365;
    n %= 365;
    let mut year = n400 * 400 + 1 + n100 * 100 + n4 * 4 + n1;
    if n1 == 4 || n100 == 4 {
        return (year - 1, 12, 31, rem / 3600, rem / 60 % 60, rem % 60);
    }
    let mut month = 1;
    loop {
        let dim = days_in_month(year, month);
        if n < dim {
            break;
        }
        n -= dim;
        month += 1;
        if month > 12 {
            month = 1;
            year += 1;
        }
    }
    (year, month, n + 1, rem / 3600, rem / 60 % 60, rem % 60)
}

/// `datetime(2001, 1, 1) + timedelta(seconds=f)`: timedelta's own rounding of a float to microseconds.
fn date_from_seconds(f: f64) -> Result<i64, String> {
    if !f.is_finite() {
        return Err("a date that is not a number".into());
    }
    let ip = f.trunc();
    let fp = f - ip;
    if ip.abs() > 1e15 {
        return Err("a date out of range".into());
    }
    let mut us = ip as i128 * US as i128;
    if fp != 0.0 {
        let d = 1e6 * fp;
        let ip2 = d.trunc();
        let left = d - ip2;
        us += ip2 as i128;
        if left != 0.0 {
            let mut whole = left.round();
            if (whole - left).abs() == 0.5 {
                let odd = (us & 1) as f64;
                whole = 2.0 * ((left + odd) * 0.5).round() - odd;
            }
            us += whole as i128;
        }
    }
    if us < DATE_MIN_US as i128 || us > DATE_MAX_US as i128 {
        return Err("a date out of range".into());
    }
    Ok(us as i64)
}

/// `(d - datetime(2001, 1, 1)).total_seconds()`: microseconds / 10**6, correctly rounded (Python's int division).
fn date_seconds(us: i64) -> f64 {
    let n = us as i128;
    if n.unsigned_abs() < (1u128 << 53) {
        return n as f64 / 1e6;
    }
    let neg = n < 0;
    let n = n.unsigned_abs() as i128;
    let q = n as f64 / 1e6;
    // the nearest of q and its neighbours to n / 10**6, ties to an even mantissa
    let up = f64::from_bits(q.to_bits() + 1);
    let down = f64::from_bits(q.to_bits() - 1);
    let parts = |c: f64| {
        let bits = c.to_bits();
        let e = ((bits >> 52) & 0x7ff) as i32 - 1075;
        let m = (bits & ((1 << 52) - 1)) | (1 << 52);
        (m as i128, e)
    };
    let k = [down, q, up].iter().map(|c| -parts(*c).1).max().unwrap().max(0);
    let mut best: Option<(i128, f64, i128)> = None;
    for c in [down, q, up] {
        let (m, e) = parts(c);
        // c * 10**6 - n, times 2**k
        let lhs = m * US as i128;
        let lhs = if k + e >= 0 { lhs << (k + e) } else { lhs >> -(k + e) };
        let diff = (lhs - (n << k)).abs();
        let better = match best {
            None => true,
            Some((d, _, bm)) => diff < d || (diff == d && m & 1 == 0 && bm & 1 == 1),
        };
        if better {
            best = Some((diff, c, m));
        }
    }
    let c = best.unwrap().1;
    if neg { -c } else { c }
}

fn date_text(us: i64) -> String {
    let (y, m, d, hh, mm, ss) = civil(us);
    format!("{y:04}-{m:02}-{d:02}T{hh:02}:{mm:02}:{ss:02}Z")
}

/// plistlib's `_date_from_string`: `YYYY[-MM[-DD[THH[:MM[:SS]]]]]Z` at the start; datetime needs the day.
fn date_parse(s: &str) -> Result<i64, String> {
    let b = s.as_bytes();
    let digits = |at: usize, n: usize| -> Option<i64> {
        let part = b.get(at..at + n)?;
        part.iter().all(u8::is_ascii_digit).then(|| part.iter().fold(0i64, |a, c| a * 10 + (c - b'0') as i64))
    };
    let bad = || format!("{s:?}: not a date");
    let mut f = vec![digits(0, 4).ok_or_else(bad)?];
    let mut at = 4;
    for sep in [b'-', b'-', b'T', b':', b':'] {
        if b.get(at) == Some(&sep) {
            if let Some(v) = digits(at + 1, 2) {
                f.push(v);
                at += 3;
                continue;
            }
        }
        break;
    }
    if b.get(at) != Some(&b'Z') {
        return Err(bad());
    }
    if f.len() < 3 {
        return Err(format!("{s:?}: a date needs a day"));
    }
    f.resize(6, 0);
    let (y, m, d, hh, mm, ss) = (f[0], f[1], f[2], f[3], f[4], f[5]);
    if !(1..=9999).contains(&y) || !(1..=12).contains(&m) || d < 1 || d > days_in_month(y, m) || hh > 23 || mm > 59 || ss > 59 {
        return Err(format!("{s:?}: not a date"));
    }
    Ok(((ordinal(y, m, d) - ordinal(2001, 1, 1)) * 86_400 + hh * 3600 + mm * 60 + ss) * US)
}

// ---------------------------------------------------------------- reading

/// What `loads` read.
pub struct Loaded {
    /// The top object; `None` for an XML list with no value (plistlib returns None)
    pub value: Option<P>,
    /// A container of a binary list was referred to twice: Python reads it as one object and would write it once.
    pub shared: bool,
}

/// `plistlib.loads(data)`: XML or binary, by the first bytes.
pub fn loads(data: &[u8]) -> Result<Loaded, String> {
    let head = &data[..data.len().min(32)];
    let bom = b"\xef\xbb\xbf";
    let xml = |h: &[u8]| h.starts_with(b"<?xml") || h.starts_with(b"<plist");
    if xml(head) || (head.starts_with(bom) && xml(&head[3..])) {
        return xml_loads(data);
    }
    if head.starts_with(b"bplist00") {
        let mut r = Bin { d: data, offsets: Vec::new(), ref_size: 0, state: Vec::new(), cache: Vec::new(), shared: false };
        let top = r.parse().map_err(|e| if e.is_empty() { "Invalid file".to_string() } else { e })?;
        return Ok(Loaded { value: Some(top), shared: r.shared });
    }
    Err("Invalid file".into())
}

struct Bin<'a> {
    d: &'a [u8],
    offsets: Vec<u64>,
    ref_size: usize,
    /// per object: 0 not read, 1 being read (a container), 2 read
    state: Vec<u8>,
    cache: Vec<Option<P>>,
    shared: bool,
}

fn uint(bytes: &[u8]) -> u128 {
    bytes.iter().fold(0u128, |a, b| (a << 8) | *b as u128)
}

impl Bin<'_> {
    fn take(&self, at: usize, n: usize) -> Result<&[u8], String> {
        self.d.get(at..at.checked_add(n).ok_or("")?).ok_or_else(String::new)
    }
    /// `fp.read(n)` after a seek: what is there, short at the end
    fn some(&self, at: usize, n: usize) -> &[u8] {
        let end = at.saturating_add(n).min(self.d.len());
        self.d.get(at.min(end)..end).unwrap_or(&[])
    }
    fn ints(&self, at: usize, n: usize, size: usize) -> Result<Vec<u64>, String> {
        if size == 0 || n.checked_mul(size).is_none_or(|t| t > self.d.len()) {
            return Err(String::new());
        }
        let raw = self.take(at, n * size)?;
        raw.chunks(size).map(|c| u64::try_from(uint(c)).map_err(|_| String::new())).collect()
    }
    fn parse(&mut self) -> Result<P, String> {
        if self.d.len() < 32 {
            return Err(String::new());
        }
        let t = &self.d[self.d.len() - 32..];
        let offset_size = t[6] as usize;
        self.ref_size = t[7] as usize;
        let num = u64::from_be_bytes(t[8..16].try_into().unwrap());
        let top = u64::from_be_bytes(t[16..24].try_into().unwrap());
        let table = u64::from_be_bytes(t[24..32].try_into().unwrap());
        let num = usize::try_from(num).map_err(|_| String::new())?;
        let table = usize::try_from(table).map_err(|_| String::new())?;
        self.offsets = self.ints(table, num, offset_size)?;
        self.state = vec![0; num];
        self.cache = vec![None; num];
        self.object(usize::try_from(top).map_err(|_| String::new())?)
    }
    fn size(&self, low: u8, at: &mut usize) -> Result<usize, String> {
        if low != 0xF {
            return Ok(low as usize);
        }
        let m = *self.d.get(*at).ok_or("")? & 0x3;
        let s = 1usize << m;
        let v = uint(self.take(*at + 1, s)?);
        *at += 1 + s;
        usize::try_from(v).map_err(|_| String::new())
    }
    fn object(&mut self, r: usize) -> Result<P, String> {
        let offset = *self.offsets.get(r).ok_or("")? as usize;
        match self.state[r] {
            2 => {
                if matches!(self.cache[r], Some(P::Array(_)) | Some(P::Dict(_))) {
                    self.shared = true;
                }
                return Ok(self.cache[r].clone().unwrap());
            }
            1 => return Err("a container that holds itself".into()),
            _ => {}
        }
        let token = *self.d.get(offset).ok_or("")?;
        let (high, low) = (token & 0xF0, token & 0x0F);
        let at = offset + 1;
        let value = match token {
            0x00 => P::Null,
            0x08 => P::Bool(false),
            0x09 => P::Bool(true),
            0x0f => P::Data(Vec::new()),
            0x22 => P::Real(f32::from_be_bytes(self.take(at, 4)?.try_into().unwrap()) as f64),
            0x23 => P::Real(f64::from_be_bytes(self.take(at, 8)?.try_into().unwrap())),
            0x33 => P::Date(date_from_seconds(f64::from_be_bytes(self.take(at, 8)?.try_into().unwrap()))?),
            _ if high == 0x10 => {
                if low > 4 {
                    return Err("an integer of more than 128 bits".into());
                }
                let raw = self.some(at, 1 << low);
                let v = uint(raw);
                let bits = raw.len() * 8;
                // signed for 8 and 16 bytes (`signed=tokenL >= 3`)
                P::Int(if low >= 3 && bits > 0 && v >> (bits - 1) & 1 == 1 {
                    if bits == 128 { v as i128 } else { v as i128 - (1i128 << bits) }
                } else {
                    if v > i128::MAX as u128 {
                        return Err(String::new());
                    }
                    v as i128
                })
            }
            _ if high == 0x40 => {
                let mut at = at;
                let s = self.size(low, &mut at)?;
                P::Data(self.take(at, s)?.to_vec())
            }
            _ if high == 0x50 => {
                let mut at = at;
                let s = self.size(low, &mut at)?;
                let raw = self.take(at, s)?;
                if !raw.is_ascii() {
                    return Err(String::new());
                }
                P::Str(String::from_utf8(raw.to_vec()).unwrap())
            }
            _ if high == 0x60 => {
                let mut at = at;
                let s = self.size(low, &mut at)?;
                let raw = self.take(at, s.checked_mul(2).ok_or("")?)?;
                let units: Vec<u16> = raw.chunks(2).map(|c| u16::from_be_bytes([c[0], c[1]])).collect();
                P::Str(String::from_utf16(&units).map_err(|_| String::new())?)
            }
            _ if high == 0x80 => P::Uid(uint(self.some(at, 1 + low as usize))),
            _ if high == 0xA0 || high == 0xD0 => {
                let mut at = at;
                let s = self.size(low, &mut at)?;
                let n = if high == 0xD0 { 2 * s } else { s };
                let refs = self.ints(at, n, self.ref_size)?;
                self.state[r] = 1;
                let value = if high == 0xA0 {
                    let mut items = Vec::with_capacity(s);
                    for x in refs {
                        items.push(self.object(x as usize)?);
                    }
                    P::Array(items)
                } else {
                    let mut d = P::dict();
                    for i in 0..s {
                        let key = match self.object(refs[i] as usize)? {
                            P::Str(k) => k,
                            _ => return Err("a dictionary key that is not a string".into()),
                        };
                        let v = self.object(refs[s + i] as usize)?;
                        d.set(&key, v);
                    }
                    d
                };
                value
            }
            _ => return Err(String::new()),
        };
        self.state[r] = 2;
        self.cache[r] = Some(value.clone());
        Ok(value)
    }
}

/// binascii.a2b_base64 (not strict): characters outside the alphabet are skipped, a pad ends the data.
fn base64_decode(text: &str) -> Result<Vec<u8>, String> {
    let mut out = Vec::new();
    let (mut quad, mut left, mut pads) = (0u8, 0u32, 0u32);
    for c in text.bytes() {
        if c == b'=' {
            if quad >= 2 {
                pads += 1;
                if quad as u32 + pads >= 4 {
                    return Ok(out);
                }
            }
            continue;
        }
        let v = match c {
            b'A'..=b'Z' => c - b'A',
            b'a'..=b'z' => c - b'a' + 26,
            b'0'..=b'9' => c - b'0' + 52,
            b'+' => 62,
            b'/' => 63,
            _ => continue,
        } as u32;
        pads = 0;
        match quad {
            0 => {
                quad = 1;
                left = v;
            }
            1 => {
                quad = 2;
                out.push(((left << 2) | (v >> 4)) as u8);
                left = v & 0x0f;
            }
            2 => {
                quad = 3;
                out.push(((left << 4) | (v >> 2)) as u8);
                left = v & 0x03;
            }
            _ => {
                quad = 0;
                out.push(((left << 6) | v) as u8);
                left = 0;
            }
        }
    }
    match quad {
        0 => Ok(out),
        1 => Err("Invalid base64-encoded string".into()),
        _ => Err("Incorrect padding".into()),
    }
}

/// Python's `int(text)` / `int(text, 16)` of plistlib's `<integer>`.
fn int_parse(raw: &str) -> Result<i128, String> {
    let bad = || format!("invalid literal for int(): {raw:?}");
    let (text, radix) = if raw.starts_with("0x") || raw.starts_with("0X") { (&raw[2..], 16) } else { (raw.trim(), 10) };
    let (neg, body) = match text.as_bytes().first() {
        Some(b'-') if radix == 10 => (true, &text[1..]),
        Some(b'+') if radix == 10 => (false, &text[1..]),
        _ => (false, text),
    };
    if body.is_empty() || body.starts_with('_') || body.ends_with('_') || body.contains("__") {
        return Err(bad());
    }
    let digits: String = body.chars().filter(|c| *c != '_').collect();
    let v = i128::from_str_radix(&digits, radix).map_err(|_| bad())?;
    Ok(if neg { -v } else { v })
}

/// Python's `float(text)` of plistlib's `<real>`.
fn real_parse(raw: &str) -> Result<f64, String> {
    let t = raw.trim();
    let bad = || format!("could not convert string to float: {raw:?}");
    let lower = t.to_ascii_lowercase();
    let body = lower.trim_start_matches(['+', '-']);
    if !matches!(body, "inf" | "infinity" | "nan") && !t.bytes().all(|c| c.is_ascii_digit() || b"+-.eE".contains(&c)) {
        return Err(bad());
    }
    t.parse::<f64>().map_err(|_| bad())
}

enum Slot {
    Root,
    Key(String),
    Append,
}

struct Xml {
    stack: Vec<(P, Slot)>,
    key: Option<String>,
    root: Option<P>,
    data: String,
}

impl Xml {
    /// Where a new value goes (plistlib's add_object, at the start of a container).
    fn slot(&mut self) -> Result<Slot, String> {
        if let Some(k) = self.key.take() {
            if !matches!(self.stack.last(), Some((P::Dict(_), _))) {
                return Err("unexpected element".into());
            }
            Ok(Slot::Key(k))
        } else if self.stack.is_empty() {
            Ok(Slot::Root)
        } else {
            if !matches!(self.stack.last(), Some((P::Array(_), _))) {
                return Err("unexpected element".into());
            }
            Ok(Slot::Append)
        }
    }
    fn put(&mut self, slot: Slot, value: P) {
        match slot {
            Slot::Root => self.root = Some(value),
            Slot::Key(k) => self.stack.last_mut().unwrap().0.set(&k, value),
            Slot::Append => {
                if let Some((P::Array(items), _)) = self.stack.last_mut() {
                    items.push(value)
                }
            }
        }
    }
    fn add(&mut self, value: P) -> Result<(), String> {
        let slot = self.slot()?;
        self.put(slot, value);
        Ok(())
    }
    fn begin(&mut self, name: &str) -> Result<(), String> {
        self.data.clear();
        match name {
            "dict" | "array" => {
                let slot = self.slot()?;
                self.stack.push((if name == "dict" { P::dict() } else { P::Array(Vec::new()) }, slot));
            }
            _ => {}
        }
        Ok(())
    }
    fn end(&mut self, name: &str) -> Result<(), String> {
        let data = std::mem::take(&mut self.data);
        match name {
            "dict" | "array" => {
                if name == "dict" && self.key.as_deref().is_some_and(|k| !k.is_empty()) {
                    return Err(format!("missing value for key '{}'", self.key.as_deref().unwrap()));
                }
                let (value, slot) = self.stack.pop().ok_or("unexpected end")?;
                self.put(slot, value);
            }
            "key" => {
                if self.key.as_deref().is_some_and(|k| !k.is_empty()) || !matches!(self.stack.last(), Some((P::Dict(_), _))) {
                    return Err("unexpected key".into());
                }
                self.key = Some(data);
            }
            "true" => self.add(P::Bool(true))?,
            "false" => self.add(P::Bool(false))?,
            "integer" => self.add(P::Int(int_parse(&data)?))?,
            "real" => self.add(P::Real(real_parse(&data)?))?,
            "string" => self.add(P::Str(data))?,
            "data" => self.add(P::Data(base64_decode(&data)?))?,
            "date" => self.add(P::Date(date_parse(&data)?))?,
            // (no handler: the text is kept until the next element starts, as plistlib's)
            _ => self.data = data,
        }
        Ok(())
    }
    fn visit(&mut self, node: roxmltree::Node) -> Result<(), String> {
        if node.is_element() {
            let name = node.tag_name().name();
            self.begin(name)?;
            for child in node.children() {
                self.visit(child)?;
            }
            self.end(name)?;
        } else if node.is_text() {
            self.data.push_str(node.text().unwrap_or(""));
        }
        Ok(())
    }
}

fn xml_loads(data: &[u8]) -> Result<Loaded, String> {
    let data = data.strip_prefix(b"\xef\xbb\xbf").unwrap_or(data);
    let text = std::str::from_utf8(data).map_err(|e| format!("not UTF-8 ({e})"))?;
    let options = roxmltree::ParsingOptions { allow_dtd: true, ..Default::default() };
    let doc = roxmltree::Document::parse_with_options(text, options).map_err(|e| e.to_string())?;
    let mut x = Xml { stack: Vec::new(), key: None, root: None, data: String::new() };
    x.visit(doc.root_element())?;
    Ok(Loaded { value: x.root, shared: false })
}

// ---------------------------------------------------------------- writing

fn sorted_items(items: &[(String, P)]) -> Vec<&(String, P)> {
    let mut out: Vec<&(String, P)> = items.iter().collect();
    // Python sorts str by code point: the same order as UTF-8's bytes
    out.sort_by(|a, b| a.0.as_bytes().cmp(b.0.as_bytes()));
    out
}

#[derive(PartialEq, Eq, Hash)]
enum Key {
    Null,
    Bool(bool),
    Int(i128),
    /// the bits of a float, -0.0 as 0.0 (they are equal in Python)
    Real(u64),
    Date(i64),
    Data(Vec<u8>),
    Str(String),
}

fn key_of(v: &P) -> Option<Key> {
    Some(match v {
        P::Null => Key::Null,
        P::Bool(b) => Key::Bool(*b),
        P::Int(i) => Key::Int(*i),
        P::Real(f) if f.is_nan() => return None,
        P::Real(f) => Key::Real(if *f == 0.0 { 0 } else { f.to_bits() }),
        P::Date(d) => Key::Date(*d),
        P::Data(d) => Key::Data(d.clone()),
        P::Str(s) => Key::Str(s.clone()),
        _ => return None,
    })
}

/// The binary writer's state: the objects in their order, their references, the keys written as strings.
struct Writer<'a> {
    objects: Vec<Obj<'a>>,
    table: std::collections::HashMap<Key, usize>,
}

enum Obj<'a> {
    Value(&'a P, Vec<usize>),
    Key(&'a str),
}

impl<'a> Writer<'a> {
    fn scalar(&mut self, key: Option<Key>, obj: Obj<'a>) -> (usize, bool) {
        if let Some(k) = &key {
            if let Some(r) = self.table.get(k) {
                return (*r, false);
            }
        }
        let r = self.objects.len();
        self.objects.push(obj);
        if let Some(k) = key {
            self.table.insert(k, r);
        }
        (r, true)
    }
    fn flatten(&mut self, v: &'a P) -> usize {
        let (r, new) = self.scalar(key_of(v), Obj::Value(v, Vec::new()));
        if !new {
            return r;
        }
        let refs = match v {
            P::Dict(items) => {
                let items = sorted_items(items);
                let mut refs: Vec<usize> = items.iter().map(|(k, _)| self.scalar(Some(Key::Str(k.clone())), Obj::Key(k)).0).collect();
                for (_, value) in &items {
                    refs.push(self.flatten(value));
                }
                refs
            }
            P::Array(items) => items.iter().map(|value| self.flatten(value)).collect(),
            _ => return r,
        };
        if let Obj::Value(_, slot) = &mut self.objects[r] {
            *slot = refs;
        }
        r
    }
}

fn count_size(n: u64) -> usize {
    if n < 1 << 8 {
        1
    } else if n < 1 << 16 {
        2
    } else if n < 1 << 32 {
        4
    } else {
        8
    }
}

fn put_uint(out: &mut Vec<u8>, v: u64, size: usize) {
    out.extend_from_slice(&v.to_be_bytes()[8 - size..]);
}

fn put_size(out: &mut Vec<u8>, token: u8, size: usize) {
    if size < 15 {
        out.push(token | size as u8);
    } else if size < 1 << 8 {
        out.extend_from_slice(&[token | 0xF, 0x10, size as u8]);
    } else if size < 1 << 16 {
        out.extend_from_slice(&[token | 0xF, 0x11]);
        put_uint(out, size as u64, 2);
    } else if size < 1 << 32 {
        out.extend_from_slice(&[token | 0xF, 0x12]);
        put_uint(out, size as u64, 4);
    } else {
        out.extend_from_slice(&[token | 0xF, 0x13]);
        put_uint(out, size as u64, 8);
    }
}

fn put_str(out: &mut Vec<u8>, s: &str) {
    if s.is_ascii() {
        put_size(out, 0x50, s.len());
        out.extend_from_slice(s.as_bytes());
    } else {
        let units: Vec<u16> = s.encode_utf16().collect();
        put_size(out, 0x60, units.len());
        for u in units {
            out.extend_from_slice(&u.to_be_bytes());
        }
    }
}

/// `plistlib.dumps(value, fmt=plistlib.FMT_BINARY, sort_keys=True)`
pub fn dumps_binary(value: &P) -> Result<Vec<u8>, String> {
    let mut w = Writer { objects: Vec::new(), table: std::collections::HashMap::new() };
    let top = w.flatten(value);
    let n = w.objects.len();
    let ref_size = count_size(n as u64);
    let mut out = b"bplist00".to_vec();
    let mut offsets = Vec::with_capacity(n);
    for obj in &w.objects {
        offsets.push(out.len() as u64);
        match obj {
            Obj::Key(k) => put_str(&mut out, k),
            Obj::Value(v, refs) => match v {
                P::Null => out.push(0x00),
                P::Bool(false) => out.push(0x08),
                P::Bool(true) => out.push(0x09),
                P::Int(i) => {
                    let i = *i;
                    if i < 0 {
                        if i < i64::MIN as i128 {
                            return Err(format!("OverflowError: {i}"));
                        }
                        out.push(0x13);
                        out.extend_from_slice(&(i as i64).to_be_bytes());
                    } else if i < 1 << 8 {
                        out.extend_from_slice(&[0x10, i as u8]);
                    } else if i < 1 << 16 {
                        out.push(0x11);
                        put_uint(&mut out, i as u64, 2);
                    } else if i < 1 << 32 {
                        out.push(0x12);
                        put_uint(&mut out, i as u64, 4);
                    } else if i < 1 << 63 {
                        out.push(0x13);
                        put_uint(&mut out, i as u64, 8);
                    } else if i < 1 << 64 {
                        out.push(0x14);
                        out.extend_from_slice(&i.to_be_bytes());
                    } else {
                        return Err(format!("OverflowError: {i}"));
                    }
                }
                P::Real(f) => {
                    out.push(0x23);
                    out.extend_from_slice(&f.to_be_bytes());
                }
                P::Date(d) => {
                    out.push(0x33);
                    out.extend_from_slice(&date_seconds(*d).to_be_bytes());
                }
                P::Data(d) => {
                    put_size(&mut out, 0x40, d.len());
                    out.extend_from_slice(d);
                }
                P::Str(s) => put_str(&mut out, s),
                P::Uid(u) => {
                    let u = *u;
                    let (token, size) = if u < 1 << 8 {
                        (0x80, 1)
                    } else if u < 1 << 16 {
                        (0x81, 2)
                    } else if u < 1 << 32 {
                        (0x83, 4)
                    } else if u < 1 << 64 {
                        (0x87, 8)
                    } else {
                        return Err(format!("OverflowError: UID {u}"));
                    };
                    out.push(token);
                    put_uint(&mut out, u as u64, size);
                }
                P::Array(_) => {
                    put_size(&mut out, 0xA0, refs.len());
                    for r in refs {
                        put_uint(&mut out, *r as u64, ref_size);
                    }
                }
                P::Dict(_) => {
                    put_size(&mut out, 0xD0, refs.len() / 2);
                    for r in refs {
                        put_uint(&mut out, *r as u64, ref_size);
                    }
                }
            },
        }
    }
    let table = out.len() as u64;
    let offset_size = count_size(table);
    for o in &offsets {
        put_uint(&mut out, *o, offset_size);
    }
    out.extend_from_slice(&[0; 5]);
    out.extend_from_slice(&[0, offset_size as u8, ref_size as u8]);
    out.extend_from_slice(&(n as u64).to_be_bytes());
    out.extend_from_slice(&(top as u64).to_be_bytes());
    out.extend_from_slice(&table.to_be_bytes());
    Ok(out)
}

const HEADER: &str = "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<!DOCTYPE plist PUBLIC \"-//Apple//DTD PLIST 1.0//EN\" \"http://www.apple.com/DTDs/PropertyList-1.0.dtd\">\n";

fn escape(text: &str) -> Result<String, String> {
    if text.chars().any(|c| (c as u32) < 0x20 && !matches!(c, '\t' | '\n' | '\r')) {
        return Err("strings can't contain control characters; use bytes instead".into());
    }
    Ok(text.replace("\r\n", "\n").replace('\r', "\n").replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;"))
}

fn base64_lines(data: &[u8], max_line: usize) -> Vec<String> {
    const A: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let bin = (max_line / 4) * 3;
    let mut lines = Vec::new();
    for chunk in data.chunks(bin.max(1)) {
        let mut line = String::new();
        for g in chunk.chunks(3) {
            let n = (g[0] as u32) << 16 | (*g.get(1).unwrap_or(&0) as u32) << 8 | *g.get(2).unwrap_or(&0) as u32;
            line.push(A[(n >> 18) as usize & 63] as char);
            line.push(A[(n >> 12) as usize & 63] as char);
            line.push(if g.len() > 1 { A[(n >> 6) as usize & 63] as char } else { '=' });
            line.push(if g.len() > 2 { A[n as usize & 63] as char } else { '=' });
        }
        lines.push(line);
    }
    lines
}

struct XmlOut {
    out: String,
    level: usize,
}

impl XmlOut {
    fn line(&mut self, text: &str) {
        if !text.is_empty() {
            for _ in 0..self.level {
                self.out.push('\t');
            }
            self.out.push_str(text);
        }
        self.out.push('\n');
    }
    fn simple(&mut self, element: &str, value: Option<&str>) -> Result<(), String> {
        match value {
            Some(v) => {
                let v = escape(v)?;
                self.line(&format!("<{element}>{v}</{element}>"));
            }
            None => self.line(&format!("<{element}/>")),
        }
        Ok(())
    }
    fn value(&mut self, v: &P) -> Result<(), String> {
        match v {
            P::Str(s) => self.simple("string", Some(s))?,
            P::Bool(true) => self.simple("true", None)?,
            P::Bool(false) => self.simple("false", None)?,
            P::Int(i) => {
                if *i < i64::MIN as i128 || *i >= 1i128 << 64 {
                    return Err(format!("OverflowError: {i}"));
                }
                self.simple("integer", Some(&i.to_string()))?
            }
            P::Real(f) => self.simple("real", Some(&float_repr(*f)))?,
            P::Dict(items) if items.is_empty() => self.simple("dict", None)?,
            P::Dict(items) => {
                self.line("<dict>");
                self.level += 1;
                for (k, value) in sorted_items(items) {
                    self.simple("key", Some(k))?;
                    self.value(value)?;
                }
                self.level -= 1;
                self.line("</dict>");
            }
            P::Data(d) => {
                self.line("<data>");
                let max = 76usize.saturating_sub(8 * self.level).max(16);
                for l in base64_lines(d, max) {
                    self.line(&l);
                }
                self.line("</data>");
            }
            P::Date(d) => self.simple("date", Some(&date_text(*d)))?,
            P::Array(items) if items.is_empty() => self.simple("array", None)?,
            P::Array(items) => {
                self.line("<array>");
                self.level += 1;
                for value in items {
                    self.value(value)?;
                }
                self.level -= 1;
                self.line("</array>");
            }
            P::Null | P::Uid(_) => return Err(format!("unsupported type: {}", if matches!(v, P::Null) { "<class 'NoneType'>" } else { "<class 'plistlib.UID'>" })),
        }
        Ok(())
    }
}

/// `plistlib.dumps(value, fmt=plistlib.FMT_XML, sort_keys=True)`
pub fn dumps_xml(value: &P) -> Result<Vec<u8>, String> {
    let mut x = XmlOut { out: String::from(HEADER), level: 0 };
    x.line("<plist version=\"1.0\">");
    x.value(value)?;
    x.line("</plist>");
    Ok(x.out.into_bytes())
}

/// A value as text for a test's message.
pub fn show(v: &P) -> String {
    let mut s = String::new();
    let _ = write!(s, "{v:?}");
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hex(b: &[u8]) -> String {
        b.iter().map(|x| format!("{x:02x}")).collect()
    }

    #[test]
    fn apples_global_preferences_read_and_written_again() {
        // mobile's .GlobalPreferences.plist of 24A446's OS volume, 89 bytes (two locale words in CF's binary
        // layout; not a file of Apple's code)
        let raw = b"bplist00\xd2\x01\x02\x03\x04[AppleLocale^AppleLanguagesUen_US\xa1\x05Ren\x08\x0d\x19\x28\x2e\x30\x00\x00\x00\x00\x00\x00\x01\x01\x00\x00\x00\x00\x00\x00\x00\x06\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x33";
        let v = loads(raw).unwrap().value.unwrap();
        assert_eq!(v.get("AppleLocale").and_then(P::as_str), Some("en_US"));
        // plistlib.dumps(that, fmt=FMT_BINARY, sort_keys=True), Python 3.14
        assert_eq!(hex(&dumps_binary(&v).unwrap()), "62706c6973743030d2010203055e4170706c654c616e6775616765735b4170706c654c6f63616c65a10452656e55656e5f5553080d1c282a2d0000000000000101000000000000000600000000000000000000000000000033");
    }

    /// {'a': [1, 1, True, 1.0, -0.0, 0.0, 'x', 'é', bytes(100), datetime(2026, 11, 21, 10, 20, 30, 123457),
    ///  {'z': 1, 'y': []}, {}], 'big': 2**64 - 1, 'neg': -5, 'u': UID(300)}
    fn sample(uid: bool) -> P {
        let mut v = P::dict();
        let date = date_parse("2026-11-21T10:20:30Z").unwrap() + 123_457;
        let mut inner = P::dict();
        inner.set("z", P::Int(1));
        inner.set("y", P::Array(vec![]));
        v.set("a", P::Array(vec![P::Int(1), P::Int(1), P::Bool(true), P::Real(1.0), P::Real(-0.0), P::Real(0.0), P::Str("x".into()), P::Str("\u{e9}".into()), P::Data(vec![0; 100]), P::Date(date), inner, P::dict()]));
        v.set("big", P::Int((1i128 << 64) - 1));
        v.set("neg", P::Int(-5));
        if uid {
            v.set("u", P::Uid(300));
        }
        v
    }

    #[test]
    fn the_writers_are_plistlibs() {
        assert_eq!(hex(&dumps_binary(&sample(true)).unwrap()), "62706c6973743030d40102030405131415516153626967536e65675175ac0606070809090a0b0c0d0e12100109233ff000000000000023800000000000000051786100e94f1064000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000003341c858d3f70fcd70d20f1011065179517aa0d0140000000000000000ffffffffffffffff13fffffffffffffffb81012c081113171b1d2a2c2d363f4144abb4b9bbbdbebfd0d900000000000001010000000000000016000000000000000000000000000000dc");
        let xml = "<?xml version=\"1.0\" encoding=\"UTF-8\"?>|<!DOCTYPE plist PUBLIC \"-//Apple//DTD PLIST 1.0//EN\" \"http://www.apple.com/DTDs/PropertyList-1.0.dtd\">|<plist version=\"1.0\">|<dict>|~<key>a</key>|~<array>|~~<integer>1</integer>|~~<integer>1</integer>|~~<true/>|~~<real>1.0</real>|~~<real>-0.0</real>|~~<real>0.0</real>|~~<string>x</string>|~~<string>\u{e9}</string>|~~<data>|~~AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA|~~AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA|~~AAAAAAAAAAAAAA==|~~</data>|~~<date>2026-11-21T10:20:30Z</date>|~~<dict>|~~~<key>y</key>|~~~<array/>|~~~<key>z</key>|~~~<integer>1</integer>|~~</dict>|~~<dict/>|~</array>|~<key>big</key>|~<integer>18446744073709551615</integer>|~<key>neg</key>|~<integer>-5</integer>|</dict>|</plist>|";
        assert_eq!(String::from_utf8(dumps_xml(&sample(false)).unwrap()).unwrap().replace('\n', "|").replace('\t', "~"), xml);
        // read back: the same values, the XML date to the second
        let again = loads(&dumps_binary(&sample(true)).unwrap()).unwrap();
        assert!(!again.shared);
        assert!(py_eq(&again.value.unwrap(), &sample(true)));
        let from_xml = loads(&dumps_xml(&sample(false)).unwrap()).unwrap().value.unwrap();
        assert_eq!(dumps_xml(&from_xml).unwrap(), dumps_xml(&sample(false)).unwrap());
    }

    #[test]
    fn dates_round_as_timedelta_does() {
        for f in [0.0, 1.5, -1.5, 0.0000005, 0.0000015, 784_111_777.123_456_7, -978_307_200.0, 63_113_904_000.25] {
            let us = date_from_seconds(f).unwrap();
            let back = date_seconds(us);
            assert!((back - f).abs() < 1e-6, "{f} {us} {back}");
        }
        assert_eq!(date_text(0), "2001-01-01T00:00:00Z");
        assert_eq!(date_parse("2026-11-21T10:20:30Z").map(date_text).unwrap(), "2026-11-21T10:20:30Z");
        assert_eq!(date_text(date_parse("1600-02-29T00:00:00Z").unwrap()), "1600-02-29T00:00:00Z");
        assert!(date_parse("2026Z").is_err());
    }

    #[test]
    fn base64_is_binascii() {
        assert_eq!(base64_decode("U1dF\nRVQ=").unwrap(), b"SWEET");
        assert_eq!(base64_decode("QQ==QUFB").unwrap(), b"A");
        assert!(base64_decode("QUF").is_err());
        assert_eq!(base64_lines(b"SWEET", 76), vec!["U1dFRVQ=".to_string()]);
    }
}
