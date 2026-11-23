// SPDX-License-Identifier: MIT
//! Messages between xnuhost's audio backend (audio/backend.rs) and the AAudio helper process (audio/aaudio_helper.rs)
//! on their socket pair (SOCK_SEQPACKET: one message per packet). C: audio/audio_proto.h.
//!
//! AU_PROBE (xnuhost -> helper, before the guest boots): open the speaker in silence and measure its latency;
//! AU_LATENCY (helper -> xnuhost): its answer, value = frames at 48 kHz (0: not measured); AU_TOLD (xnuhost -> helper):
//! the output latency the guest publishes to iOS, value = frames (for the helper's end-to-end line).

use crate::wire_struct;

pub const AU_START: u8 = 1;
pub const AU_STOP: u8 = 2;
pub const AU_PCM: u8 = 3;
pub const AU_STATE: u8 = 4;
pub const AU_PROBE: u8 = 5;
pub const AU_LATENCY: u8 = 6;
pub const AU_TOLD: u8 = 7;
pub const AU_MAX_FRAMES: u32 = 960;

wire_struct! {
    /// struct au_header: every message's start (the PCM frames, S16LE interleaved, follow an AU_PCM).
    pub struct AuHeader {
        /// AU_*
        pub kind: u8,
        /// 1 = the speaker stream, 0 = the microphone stream
        pub output: u8,
        pub channels: u8,
        pub reserved: u8,
        /// START: sample rate; PCM: frames that follow; STATE: 0 ok, else errno
        pub value: u32,
    }
}

/// The header's size.
pub const AU_HEADER: usize = 8;

/// A message: the header and, for PCM, the samples as S16LE.
pub fn au_message(header: AuHeader, pcm: &[i16]) -> Vec<u8> {
    use crate::proto::Wire;
    let mut out = header.to_bytes();
    out.reserve(pcm.len() * 2);
    pcm.iter().for_each(|s| out.extend_from_slice(&s.to_le_bytes()));
    out
}

/// The samples of a message's body (S16LE).
pub fn samples(body: &[u8]) -> impl Iterator<Item = i16> + '_ {
    (0..body.len() / 2).map(|i| i16::from_le_bytes([body[2 * i], body[2 * i + 1]]))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The wire layout and the message numbers: xnuhost and the helper are both this crate, so this test is the
    /// protocol's only check (audio_proto.h went with the C helper, 2026-11-23; its values are these).
    #[test]
    fn wire_layout() {
        assert_eq!([AU_START, AU_STOP, AU_PCM, AU_STATE, AU_PROBE, AU_LATENCY, AU_TOLD], [1, 2, 3, 4, 5, 6, 7]);
        assert_eq!(AU_MAX_FRAMES, 960);
        assert_eq!(<AuHeader as crate::proto::Wire>::SIZE, AU_HEADER);
        let m = au_message(AuHeader { kind: AU_PCM, output: 1, channels: 2, value: 1, ..Default::default() }, &[-2, 3]);
        assert_eq!(m, [3, 1, 2, 0, 1, 0, 0, 0, 0xfe, 0xff, 3, 0]);
        assert_eq!(samples(&m[8..]).collect::<Vec<_>>(), [-2, 3]);
    }
}
