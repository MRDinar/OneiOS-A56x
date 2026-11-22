// SPDX-License-Identifier: MIT
//! Netlink attributes both ways ({u16 length, u16 type} then the value, padded to 4), with no Linux header: what
//! the Wi-Fi bridge's nl80211 parsers (frames, p2p, wonder) and its requests (wlan, Wonder's vendor data) share.
//! C: the attribute loops of wifi/frames.c, p2p.c, wonder.c and wlan.c, once.

/// NLA_F_NESTED
pub const NESTED: u16 = 0x8000;

/// The attributes of a payload: (type without the flag bits, value). Ends at the payload's end or at a cut
/// (an attribute whose length is wrong); [`Attrs::cut`] tells which.
#[derive(Clone, Debug)]
pub struct Attrs<'a> {
    rest: &'a [u8],
    cut: bool,
}

pub fn attrs(payload: &[u8]) -> Attrs<'_> {
    Attrs { rest: payload, cut: false }
}

impl<'a> Attrs<'a> {
    /// The walk stopped at a malformed attribute.
    pub fn cut(&self) -> bool {
        self.cut
    }
}

impl<'a> Iterator for Attrs<'a> {
    type Item = (u16, &'a [u8]);
    fn next(&mut self) -> Option<Self::Item> {
        if self.rest.len() < 4 {
            return None;
        }
        let p = self.rest;
        let len = u16::from_le_bytes([p[0], p[1]]) as usize;
        let kind = u16::from_le_bytes([p[2], p[3]]) & 0x3fff;
        if len < 4 || len > p.len() {
            self.cut = true;
            self.rest = &[];
            return None;
        }
        let step = (len + 3) & !3;
        self.rest = if step >= p.len() { &[] } else { &p[step..] };
        Some((kind, &p[4..len]))
    }
}

pub fn u16_of(data: &[u8]) -> Option<u16> {
    let b: [u8; 2] = data.try_into().ok()?;
    Some(u16::from_le_bytes(b))
}
pub fn u32_of(data: &[u8]) -> Option<u32> {
    let b: [u8; 4] = data.try_into().ok()?;
    Some(u32::from_le_bytes(b))
}

/// A string attribute's text: without its NUL.
pub fn text_of(data: &[u8]) -> String {
    let n = if data.last() == Some(&0) { data.len() - 1 } else { data.len() };
    String::from_utf8_lossy(&data[..n]).into_owned()
}

/// Attributes written into a buffer of at most `max` bytes; one that does not fit spoils the whole (C's SIZE_MAX).
#[derive(Clone, Debug)]
pub struct Writer {
    bytes: Vec<u8>,
    max: usize,
    full: bool,
}

impl Writer {
    pub fn new(max: usize) -> Writer {
        Writer { bytes: Vec::new(), max, full: false }
    }
    pub fn attr(&mut self, kind: u16, data: &[u8]) -> &mut Writer {
        let total = 4 + data.len();
        let step = (total + 3) & !3;
        if self.full || self.bytes.len() + step > self.max {
            self.full = true;
            return self;
        }
        self.bytes.extend_from_slice(&(total as u16).to_le_bytes());
        self.bytes.extend_from_slice(&kind.to_le_bytes());
        self.bytes.extend_from_slice(data);
        self.bytes.resize(self.bytes.len() + step - total, 0);
        self
    }
    pub fn u32(&mut self, kind: u16, v: u32) -> &mut Writer {
        self.attr(kind, &v.to_le_bytes())
    }
    pub fn flag(&mut self, kind: u16) -> &mut Writer {
        self.attr(kind, &[])
    }
    /// The bytes, None when something did not fit.
    pub fn finish(&self) -> Option<&[u8]> {
        (!self.full).then_some(&self.bytes[..])
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn both_ways() {
        let mut w = Writer::new(64);
        w.u32(1, 7).attr(2, b"ab").flag(3);
        let b = w.finish().unwrap().to_vec();
        assert_eq!(b.len(), 8 + 8 + 4);
        let got: Vec<(u16, Vec<u8>)> = attrs(&b).map(|(t, d)| (t, d.to_vec())).collect();
        assert_eq!(got, vec![(1, 7u32.to_le_bytes().to_vec()), (2, b"ab".to_vec()), (3, vec![])]);
        let mut a = attrs(&b[..13]); // the second attribute says 6 bytes, 5 are there
        assert_eq!(a.next().map(|x| x.0), Some(1));
        assert!(a.next().is_none() && a.cut());
        assert!(Writer::new(8).u32(1, 1).u32(2, 2).finish().is_none());
    }
}
