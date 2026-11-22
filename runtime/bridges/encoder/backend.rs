// SPDX-License-Identifier: MIT
//! The backend interface (encoder.h): one encoder session per virtio-media session, fed NV12 frames, giving coded
//! access units back. Backends (both samsung and treble use the same path: Android's MediaCodec):
//! - [`crate::backend_ndk`]: the NDK's AMediaCodec (video/hevc -> c2.qti.hevc.encoder, video/avc); the phone and
//!   `encctl`.
//! - [`crate::backend_stub`]: the stand-in: no MediaCodec on the box, so it writes a real HEVC/H.264 Annex-B access
//!   unit of the frame's own samples (the host tests, xnuhost's encoder=sim and its PC boots).
//! - [`crate::backend_helper`]: the device-side backend that runs the real encoder in the oneios-encoder helper
//!   process.

/// What a poll gave: `bytes` of one coded access unit (0: none ready yet), whether it is a keyframe, its pts, and
/// whether the last buffer has come out after a drain.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Polled {
    pub bytes: usize,
    pub key: bool,
    pub pts_us: i64,
    pub eos: bool,
}

/// One encoder session on a backend (`br_enc_session`). Dropping it closes it.
pub trait Session: Send {
    /// Feeds one NV12 frame (w*h luma + w*h/2 chroma, tightly packed); pts in microseconds; `force_key` asks for an
    /// IDR. The backend copies what it needs: the caller may reuse the buffer at once. An errno on failure.
    fn feed(&mut self, nv12: &[u8], pts_us: i64, force_key: bool) -> Result<(), i32>;
    /// Asks the backend to finish every frame fed so far (end of stream).
    fn drain(&mut self) -> Result<(), i32>;
    /// One coded access unit, Annex-B (start codes), into `buf`, or an errno.
    fn poll(&mut self, buf: &mut [u8]) -> Result<Polled, i32>;
}

/// A backend (`br_encoder_backend`).
pub trait Backend: Send + Sync {
    fn name(&self) -> &'static str;
    /// A session for `codec` (MEDIA_PIXFMT_HEVC or MEDIA_PIXFMT_H264) at width x height, tightly packed NV12 input;
    /// None when it is refused.
    fn open(&self, codec: u32, width: u32, height: u32) -> Option<Box<dyn Session>>;
}
