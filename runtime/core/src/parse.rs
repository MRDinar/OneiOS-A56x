// SPDX-License-Identifier: MIT
//! Number and size parsing as the C runtime's `strtoull`/`strtoll` and `xh_parse_size` did it.

/// The radix of `strtoull(s, &end, 0)`: `0x`/`0X` hexadecimal, a leading `0` octal, else decimal. Returns the radix
/// and the digits after the prefix.
fn auto_radix(s: &str) -> (u32, &str) {
    if let Some(hex) = s.strip_prefix("0x").or_else(|| s.strip_prefix("0X")) {
        if hex.starts_with(|c: char| c.is_ascii_hexdigit()) {
            return (16, hex);
        }
        return (10, s); // "0x" alone: the 0, then "x"
    }
    if s.len() > 1 && s.starts_with('0') {
        return (8, &s[1..]);
    }
    (10, s)
}

/// The unsigned number at the start of `s`, as `strtoull(s, &end, radix)`: leading white space and a `+` skipped,
/// `radix` 0 for C's automatic prefixes. Returns the value and the rest after it, `None` when no digit is there or the
/// value overflows (C's ERANGE). Unlike strtoull a `-` is refused.
pub fn leading_u64(s: &str, radix: u32) -> Option<(u64, &str)> {
    let t = s.trim_start_matches([' ', '\t', '\n', '\r', '\x0b', '\x0c']);
    let t = t.strip_prefix('+').unwrap_or(t);
    let (radix, digits) = if radix == 0 { auto_radix(t) } else { (radix, t) };
    let end = digits.find(|c: char| !c.is_digit(radix)).unwrap_or(digits.len());
    if end == 0 {
        // "0" read as octal with no more digits is still 0
        return (radix == 8).then_some((0, digits));
    }
    let v = u64::from_str_radix(&digits[..end], radix).ok()?;
    Some((v, &digits[end..]))
}

/// The signed number at the start of `s`, as `strtoll(s, &end, radix)`; `None` when no digit or out of range.
pub fn leading_i64(s: &str, radix: u32) -> Option<(i64, &str)> {
    let t = s.trim_start_matches([' ', '\t', '\n', '\r', '\x0b', '\x0c']);
    let (neg, t) = match t.strip_prefix('-') {
        Some(rest) => (true, rest),
        None => (false, t),
    };
    if t.starts_with(['+', '-']) {
        return None;
    }
    let (v, rest) = leading_u64(t, radix)?;
    let v = if neg { 0i64.checked_sub_unsigned(v)? } else { i64::try_from(v).ok()? };
    Some((v, rest))
}

/// A whole unsigned number (`radix` 0: C's prefixes), white space around it allowed.
pub fn u64_text(s: &str, radix: u32) -> Option<u64> {
    let (v, rest) = leading_u64(s, radix)?;
    rest.trim().is_empty().then_some(v)
}

/// A whole signed number, white space around it allowed.
pub fn i64_text(s: &str, radix: u32) -> Option<i64> {
    let (v, rest) = leading_i64(s, radix)?;
    rest.trim().is_empty().then_some(v)
}

/// A size (`xh_parse_size`): a number (C's prefixes: 0x hexadecimal, 0 octal) and an optional `k`/`K`, `m`/`M` or
/// `g`/`G` (KiB, MiB, GiB); nothing after it. `None` for anything else.
pub fn size(text: &str) -> Option<u64> {
    let (v, rest) = leading_u64(text, 0)?;
    let (shift, rest) = match rest.as_bytes().first() {
        Some(b'k' | b'K') => (10, &rest[1..]),
        Some(b'm' | b'M') => (20, &rest[1..]),
        Some(b'g' | b'G') => (30, &rest[1..]),
        _ => (0, rest),
    };
    // (C shifted without a check; a size that overflows is refused here.)
    let bytes = v.checked_mul(1u64 << shift)?;
    rest.is_empty().then_some(bytes)
}

/// A cache size as sysfs gives it (topology.c's `cache_bytes`): "64K", "2048K", "12M" or bytes; 0 when no number.
pub fn cache_size(text: &str) -> u64 {
    match leading_u64(text, 10) {
        Some((v, rest)) if rest.starts_with('K') => v << 10,
        Some((v, rest)) if rest.starts_with('M') => v << 20,
        Some((v, _)) => v,
        None => 0,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn leading() {
        assert_eq!(leading_u64("  42 kB", 10), Some((42, " kB")));
        assert_eq!(leading_u64("0x1f!", 0), Some((31, "!")));
        assert_eq!(leading_u64("017", 0), Some((15, "")));
        assert_eq!(leading_u64("0", 0), Some((0, "")));
        assert_eq!(leading_u64("0x", 0), Some((0, "x")));
        assert_eq!(leading_u64("+9", 10), Some((9, "")));
        assert_eq!(leading_u64("x9", 10), None);
        assert_eq!(leading_u64("99999999999999999999", 10), None);
        assert_eq!(leading_i64("-273000", 10), Some((-273000, "")));
        assert_eq!(leading_i64("-9223372036854775808", 10), Some((i64::MIN, "")));
        assert_eq!(leading_i64("9223372036854775808", 10), None);
        assert_eq!(u64_text("123\n", 10), Some(123));
        assert_eq!(u64_text("12 3", 10), None);
        assert_eq!(i64_text(" -5 ", 0), Some(-5));
    }

    #[test]
    fn sizes() {
        assert_eq!(size("4096"), Some(4096));
        assert_eq!(size("8G"), Some(8 << 30));
        assert_eq!(size("512m"), Some(512 << 20));
        assert_eq!(size("64K"), Some(64 << 10));
        assert_eq!(size("0x1000"), Some(4096));
        assert_eq!(size("12MB"), None);
        assert_eq!(size(""), None);
        assert_eq!(size("G"), None);
        assert_eq!(cache_size("2048K"), 2 << 20);
        assert_eq!(cache_size("12M"), 12 << 20);
        assert_eq!(cache_size("512"), 512);
        assert_eq!(cache_size("?"), 0);
    }
}
