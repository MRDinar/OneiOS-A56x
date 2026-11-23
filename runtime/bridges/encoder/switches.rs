// SPDX-License-Identifier: MIT
//! The encoder's switches (docs/switches.md), set once by xnuhost from its config before the device is made
//! (`encoder-10bit=`, `encoder-warmup=` of the image's xnuhost.conf) and passed to the helper process on its command
//! line (`oneios-encoder 3 [no-main10] [no-warmup]`). One relaxed load where each is used. The defaults are main's
//! behaviour (b3da193); the other values are the behaviour before it, so a fault can be pinned by turning one off.
use std::sync::atomic::{AtomicBool, AtomicU8, Ordering};

/// What the device does with iOS's 10-bit pictures (`encoder-10bit=`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TenBit {
    /// main10 (b3da193): the device takes P010; HEVC Main10 where the encoder offers it, else 8-bit of the high bytes.
    Main10 = 0,
    /// 8bit: the device takes P010 and codes it 8-bit (the high bytes); Main10 is never tried.
    EightBit = 1,
    /// nv12: the device lists and takes no P010: the guest sends NV12 (its high bytes), as before b3da193 on the host.
    Nv12 = 2,
}

impl TenBit {
    pub fn parse(v: &str) -> Option<TenBit> {
        match v {
            "main10" => Some(TenBit::Main10),
            "8bit" => Some(TenBit::EightBit),
            "nv12" => Some(TenBit::Nv12),
            _ => None,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            TenBit::Main10 => "main10",
            TenBit::EightBit => "8bit",
            TenBit::Nv12 => "nv12",
        }
    }

    fn from_u8(v: u8) -> TenBit {
        match v {
            1 => TenBit::EightBit,
            2 => TenBit::Nv12,
            _ => TenBit::Main10,
        }
    }
}

static TEN_BIT: AtomicU8 = AtomicU8::new(TenBit::Main10 as u8);
static WARMUP: AtomicBool = AtomicBool::new(true);

/// xnuhost (before the device is made), or the helper from its command line.
pub fn set(ten: TenBit, warmup: bool) {
    TEN_BIT.store(ten as u8, Ordering::Relaxed);
    WARMUP.store(warmup, Ordering::Relaxed);
}

pub fn ten_bit() -> TenBit {
    TenBit::from_u8(TEN_BIT.load(Ordering::Relaxed))
}

/// encoder-warmup (b3da193): the helper starts at the guest's first virtio-media session and opens one HEVC encoder at
/// once; off: it starts at the first encode and opens nothing ahead (its binder threads start either way: a fix, not a
/// choice).
pub fn warmup() -> bool {
    WARMUP.load(Ordering::Relaxed)
}

/// The helper's command line words after its fd.
pub fn helper_args() -> Vec<&'static str> {
    let mut a = Vec::new();
    if ten_bit() != TenBit::Main10 {
        a.push("no-main10"); // the helper never sees P010 with nv12, so this word covers both
    }
    if !warmup() {
        a.push("no-warmup");
    }
    a
}

/// The helper: its command line words (after the fd).
pub fn from_helper_args<'a>(words: impl Iterator<Item = &'a str>) {
    let (mut ten, mut warm) = (TenBit::Main10, true);
    for w in words {
        match w {
            "no-main10" => ten = TenBit::EightBit,
            "no-warmup" => warm = false,
            _ => {}
        }
    }
    set(ten, warm);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn words() {
        for w in ["main10", "8bit", "nv12"] {
            let t = TenBit::parse(w).unwrap();
            assert_eq!((t.as_str(), TenBit::from_u8(t as u8)), (w, t));
        }
        assert_eq!(TenBit::parse("10"), None);
    }
}
