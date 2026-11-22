// SPDX-License-Identifier: MIT
//! Text cut as the C runtime's fixed `char` arrays cut it, and C strings in byte buffers: what the log, the thread
//! names, the virtio labels, the disks' labels, the bridges' lines and the device names (EVIOCGNAME, ifreq, Mach-O
//! segment names) did each with a loop of their own.

/// `text` cut to at most `max` bytes, at a character boundary (C's snprintf into a buffer of `max + 1`).
pub fn truncate(text: &str, max: usize) -> &str {
    if text.len() <= max {
        return text;
    }
    let mut end = max;
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    &text[..end]
}

/// [`truncate`] of a `String` in place.
pub fn truncate_string(text: &mut String, max: usize) {
    let end = truncate(text, max).len();
    text.truncate(end);
}

/// The bytes before the first NUL (all of them when there is none): a C string in a fixed-size field.
pub fn until_nul(bytes: &[u8]) -> &[u8] {
    let end = bytes.iter().position(|&b| b == 0).unwrap_or(bytes.len());
    &bytes[..end]
}

/// The text of a C string in a fixed-size field ([`until_nul`]), invalid UTF-8 replaced.
pub fn nul_text(bytes: &[u8]) -> String {
    String::from_utf8_lossy(until_nul(bytes)).into_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cuts() {
        assert_eq!(truncate("héllo", 2), "h");
        assert_eq!(truncate("abc", 5), "abc");
        assert_eq!(truncate("abcdef", 3), "abc");
        let mut s = String::from("aé");
        truncate_string(&mut s, 2);
        assert_eq!(s, "a");
        assert_eq!(until_nul(b"eth0\0\0x"), b"eth0");
        assert_eq!(until_nul(b"full"), b"full");
        assert_eq!(nul_text(b"__TEXT\0\0\0"), "__TEXT");
    }
}
