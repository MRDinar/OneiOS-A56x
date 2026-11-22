// SPDX-License-Identifier: MIT
//! The Android encoder backend (backend_ndk.c): the NDK's AMediaCodec. "video/hevc" reaches Codec2's
//! c2.qti.hevc.encoder on the S24 Ultra, "video/avc" the AVC encoder; both the samsung and treble hardware backends use
//! this same path, so one file serves both. ByteBuffer input (tightly packed NV12, COLOR_FormatYUV420SemiPlanar); the
//! output is an Annex-B byte stream (start codes), which is what DroidVMVTHost.cpp and CMPhoto expect.
//!
//! [I] NV12 as COLOR_FormatYUV420SemiPlanar (21) with ByteBuffer input: c2.qti.hevc.encoder accepts it; not yet run
//! on the phone. The still-image settings (one keyframe, no reordering, constant-quality) are in `ffi::open`. encctl
//! encodes a test pattern through this on the phone.
//!
//! The session's logic ([`CodecSession`]) is over a [`Codec`]: MediaCodec on Android (`ffi`), a fake in the host
//! tests.
use crate::backend::{Polled, Session};

/// AMediaCodecBufferInfo's flags.
pub const BUFFER_FLAG_KEY_FRAME: u32 = 1;
pub const BUFFER_FLAG_CODEC_CONFIG: u32 = 2;
pub const BUFFER_FLAG_END_OF_STREAM: u32 = 4;
/// dequeueOutputBuffer's answers that are not a buffer.
pub const INFO_TRY_AGAIN_LATER: isize = -1;
pub const INFO_OUTPUT_FORMAT_CHANGED: isize = -2;
pub const INFO_OUTPUT_BUFFERS_CHANGED: isize = -3;

/// An output buffer's description (AMediaCodecBufferInfo).
#[repr(C)]
#[derive(Clone, Copy, Debug, Default)]
pub struct BufferInfo {
    pub offset: i32,
    pub size: i32,
    pub presentation_time_us: i64,
    pub flags: u32,
}

/// What a session needs of a MediaCodec encoder (the AMediaCodec calls backend_ndk.c makes).
pub trait Codec: Send {
    /// An input buffer's index, or < 0 (after `timeout_us`).
    fn dequeue_input(&mut self, timeout_us: i64) -> isize;
    fn input_buffer(&mut self, index: usize) -> Option<&mut [u8]>;
    /// Queues `size` bytes of input buffer `index`; whether MediaCodec took it.
    fn queue_input(&mut self, index: usize, size: usize, pts_us: u64, flags: u32) -> bool;
    /// An output buffer's index (its description into `info`), or an INFO_* value or another error (< 0).
    fn dequeue_output(&mut self, info: &mut BufferInfo, timeout_us: i64) -> isize;
    fn output_buffer(&mut self, index: usize) -> Option<&[u8]>;
    fn release_output(&mut self, index: usize);
}

/// One encoder session over a codec (`br_enc_session` of backend_ndk.c).
pub struct CodecSession<C: Codec> {
    codec: C,
    eos_queued: bool,
}

impl<C: Codec> CodecSession<C> {
    pub fn new(codec: C) -> CodecSession<C> {
        CodecSession { codec, eos_queued: false }
    }
}

impl<C: Codec> Session for CodecSession<C> {
    fn feed(&mut self, nv12: &[u8], pts_us: i64, _force_key: bool) -> Result<(), i32> {
        let ix = self.codec.dequeue_input(10_000);
        if ix < 0 {
            return Err(libc::EAGAIN);
        }
        let ix = ix as usize;
        match self.codec.input_buffer(ix) {
            Some(input) if input.len() >= nv12.len() => input[..nv12.len()].copy_from_slice(nv12),
            _ => {
                self.codec.queue_input(ix, 0, 0, 0);
                return Err(libc::EINVAL);
            }
        }
        if self.codec.queue_input(ix, nv12.len(), pts_us as u64, 0) {
            Ok(())
        } else {
            Err(libc::EIO)
        }
    }

    fn drain(&mut self) -> Result<(), i32> {
        if self.eos_queued {
            return Ok(());
        }
        let ix = self.codec.dequeue_input(10_000);
        if ix < 0 {
            return Err(libc::EAGAIN);
        }
        self.eos_queued = self.codec.queue_input(ix as usize, 0, 0, BUFFER_FLAG_END_OF_STREAM);
        if self.eos_queued {
            Ok(())
        } else {
            Err(libc::EIO)
        }
    }

    fn poll(&mut self, buf: &mut [u8]) -> Result<Polled, i32> {
        let mut info = BufferInfo::default();
        let ix = self.codec.dequeue_output(&mut info, 0);
        if matches!(ix, INFO_TRY_AGAIN_LATER | INFO_OUTPUT_FORMAT_CHANGED | INFO_OUTPUT_BUFFERS_CHANGED) {
            return Ok(Polled::default());
        }
        if ix < 0 {
            return Err(libc::EIO);
        }
        let ix = ix as usize;
        let mut p = Polled::default();
        let mut result = Ok(());
        let size = info.size.max(0) as usize;
        let at = info.offset.max(0) as usize;
        match self.codec.output_buffer(ix) {
            Some(out) if size <= buf.len() && at + size <= out.len() => {
                buf[..size].copy_from_slice(&out[at..at + size]);
                p.bytes = size;
                p.pts_us = info.presentation_time_us;
                p.key = info.flags & (BUFFER_FLAG_KEY_FRAME | BUFFER_FLAG_CODEC_CONFIG) != 0;
            }
            _ if size > buf.len() => result = Err(libc::ENOSPC),
            _ => {}
        }
        p.eos = info.flags & BUFFER_FLAG_END_OF_STREAM != 0;
        self.codec.release_output(ix);
        result.map(|()| p)
    }
}

#[cfg(target_os = "android")]
pub use ffi::NdkBackend;

/// AMediaCodec of libmediandk (linked into the programs only: build.rs).
#[cfg(target_os = "android")]
mod ffi {
    use super::{BufferInfo, Codec, CodecSession};
    use crate::backend::{Backend, Session};
    use crate::proto::MEDIA_PIXFMT_HEVC;
    use std::ffi::{c_char, c_void, CStr};

    #[repr(C)]
    struct AMediaCodec {
        _p: [u8; 0],
    }
    #[repr(C)]
    struct AMediaFormat {
        _p: [u8; 0],
    }
    const AMEDIA_OK: i32 = 0;
    const CONFIGURE_FLAG_ENCODE: u32 = 1;
    const COLOR_FORMAT_NV12: i32 = 21; // COLOR_FormatYUV420SemiPlanar
    const BITRATE_MODE_CQ: i32 = 0;

    extern "C" {
        static AMEDIAFORMAT_KEY_MIME: *const c_char;
        static AMEDIAFORMAT_KEY_WIDTH: *const c_char;
        static AMEDIAFORMAT_KEY_HEIGHT: *const c_char;
        static AMEDIAFORMAT_KEY_COLOR_FORMAT: *const c_char;
        static AMEDIAFORMAT_KEY_FRAME_RATE: *const c_char;
        static AMEDIAFORMAT_KEY_BIT_RATE: *const c_char;
        fn AMediaCodec_createEncoderByType(mime: *const c_char) -> *mut AMediaCodec;
        fn AMediaCodec_configure(
            c: *mut AMediaCodec,
            f: *const AMediaFormat,
            w: *mut c_void,
            crypto: *mut c_void,
            flags: u32,
        ) -> i32;
        fn AMediaCodec_start(c: *mut AMediaCodec) -> i32;
        fn AMediaCodec_stop(c: *mut AMediaCodec) -> i32;
        fn AMediaCodec_delete(c: *mut AMediaCodec) -> i32;
        fn AMediaCodec_dequeueInputBuffer(c: *mut AMediaCodec, timeout_us: i64) -> isize;
        fn AMediaCodec_getInputBuffer(c: *mut AMediaCodec, idx: usize, size: *mut usize) -> *mut u8;
        fn AMediaCodec_queueInputBuffer(
            c: *mut AMediaCodec,
            idx: usize,
            offset: i64,
            size: usize,
            pts: u64,
            flags: u32,
        ) -> i32;
        fn AMediaCodec_dequeueOutputBuffer(c: *mut AMediaCodec, info: *mut BufferInfo, timeout_us: i64) -> isize;
        fn AMediaCodec_getOutputBuffer(c: *mut AMediaCodec, idx: usize, size: *mut usize) -> *mut u8;
        fn AMediaCodec_releaseOutputBuffer(c: *mut AMediaCodec, idx: usize, render: bool) -> i32;
        fn AMediaFormat_new() -> *mut AMediaFormat;
        fn AMediaFormat_delete(f: *mut AMediaFormat) -> i32;
        fn AMediaFormat_setString(f: *mut AMediaFormat, name: *const c_char, value: *const c_char);
        fn AMediaFormat_setInt32(f: *mut AMediaFormat, name: *const c_char, value: i32);
    }

    /// A started MediaCodec encoder; stopped and deleted on drop.
    struct MediaCodec(*mut AMediaCodec);

    // SAFETY: an AMediaCodec may be used from any thread, one at a time (the session is behind the device's lock or
    // the helper's one thread).
    unsafe impl Send for MediaCodec {}

    impl MediaCodec {
        /// The encoder of `codec` at width x height with the still-image settings (`ndk_open`).
        fn open(codec: u32, width: u32, height: u32) -> Option<MediaCodec> {
            let mime: &CStr = if codec == MEDIA_PIXFMT_HEVC { c"video/hevc" } else { c"video/avc" };
            // SAFETY: NDK calls with valid C strings; every object made here is deleted here or by Drop.
            unsafe {
                let c = AMediaCodec_createEncoderByType(mime.as_ptr());
                if c.is_null() {
                    return None;
                }
                let f = AMediaFormat_new();
                AMediaFormat_setString(f, AMEDIAFORMAT_KEY_MIME, mime.as_ptr());
                AMediaFormat_setInt32(f, AMEDIAFORMAT_KEY_WIDTH, width as i32);
                AMediaFormat_setInt32(f, AMEDIAFORMAT_KEY_HEIGHT, height as i32);
                AMediaFormat_setInt32(f, AMEDIAFORMAT_KEY_COLOR_FORMAT, COLOR_FORMAT_NV12);
                AMediaFormat_setInt32(f, AMEDIAFORMAT_KEY_FRAME_RATE, 30);
                AMediaFormat_setInt32(f, c"i-frame-interval".as_ptr(), 0); // still: every frame a keyframe
                AMediaFormat_setInt32(f, c"bitrate-mode".as_ptr(), BITRATE_MODE_CQ); // constant quality
                AMediaFormat_setInt32(f, c"quality".as_ptr(), 90);
                AMediaFormat_setInt32(f, AMEDIAFORMAT_KEY_BIT_RATE, (width * height * 4) as i32);
                let rc = AMediaCodec_configure(c, f, std::ptr::null_mut(), std::ptr::null_mut(), CONFIGURE_FLAG_ENCODE);
                AMediaFormat_delete(f);
                if rc != AMEDIA_OK || AMediaCodec_start(c) != AMEDIA_OK {
                    AMediaCodec_delete(c);
                    return None;
                }
                Some(MediaCodec(c))
            }
        }
    }

    impl Drop for MediaCodec {
        fn drop(&mut self) {
            // SAFETY: a started codec of ours, stopped and deleted once.
            unsafe {
                AMediaCodec_stop(self.0);
                AMediaCodec_delete(self.0);
            }
        }
    }

    impl Codec for MediaCodec {
        fn dequeue_input(&mut self, timeout_us: i64) -> isize {
            // SAFETY: our codec.
            unsafe { AMediaCodec_dequeueInputBuffer(self.0, timeout_us) }
        }

        fn input_buffer(&mut self, index: usize) -> Option<&mut [u8]> {
            let mut cap = 0;
            // SAFETY: a dequeued input buffer of our codec: `cap` bytes, ours until it is queued.
            unsafe {
                let p = AMediaCodec_getInputBuffer(self.0, index, &mut cap);
                (!p.is_null()).then(|| std::slice::from_raw_parts_mut(p, cap))
            }
        }

        fn queue_input(&mut self, index: usize, size: usize, pts_us: u64, flags: u32) -> bool {
            // SAFETY: a dequeued input buffer of our codec.
            unsafe { AMediaCodec_queueInputBuffer(self.0, index, 0, size, pts_us, flags) == AMEDIA_OK }
        }

        fn dequeue_output(&mut self, info: &mut BufferInfo, timeout_us: i64) -> isize {
            // SAFETY: our codec and a BufferInfo of the NDK's layout.
            unsafe { AMediaCodec_dequeueOutputBuffer(self.0, info, timeout_us) }
        }

        fn output_buffer(&mut self, index: usize) -> Option<&[u8]> {
            let mut cap = 0;
            // SAFETY: a dequeued output buffer of our codec: `cap` bytes until it is released.
            unsafe {
                let p = AMediaCodec_getOutputBuffer(self.0, index, &mut cap);
                (!p.is_null()).then(|| std::slice::from_raw_parts(p, cap))
            }
        }

        fn release_output(&mut self, index: usize) {
            // SAFETY: a dequeued output buffer of our codec.
            unsafe { AMediaCodec_releaseOutputBuffer(self.0, index, false) };
        }
    }

    /// The backend ("mediacodec").
    #[derive(Clone, Copy, Debug, Default)]
    pub struct NdkBackend;

    impl Backend for NdkBackend {
        fn name(&self) -> &'static str {
            "mediacodec"
        }

        fn open(&self, codec: u32, width: u32, height: u32) -> Option<Box<dyn Session>> {
            Some(Box::new(CodecSession::new(MediaCodec::open(codec, width, height)?)))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A codec of four input buffers of `cap` bytes whose output is what was queued, as it was given.
    #[derive(Default)]
    struct Fake {
        cap: usize,
        inputs: Vec<Vec<u8>>,
        queued: Vec<(usize, usize, u64, u32)>,
        outputs: Vec<(Vec<u8>, BufferInfo)>,
        released: Vec<usize>,
        busy: bool,
        refuse: bool,
    }

    impl Codec for Fake {
        fn dequeue_input(&mut self, _timeout_us: i64) -> isize {
            if self.busy {
                return -1;
            }
            self.inputs.push(vec![0; self.cap]);
            self.inputs.len() as isize - 1
        }
        fn input_buffer(&mut self, index: usize) -> Option<&mut [u8]> {
            self.inputs.get_mut(index).map(|v| v.as_mut_slice())
        }
        fn queue_input(&mut self, index: usize, size: usize, pts_us: u64, flags: u32) -> bool {
            self.queued.push((index, size, pts_us, flags));
            if size > 0 {
                let info = BufferInfo {
                    offset: 2,
                    size: size as i32,
                    presentation_time_us: pts_us as i64,
                    flags: BUFFER_FLAG_KEY_FRAME,
                };
                let mut data = vec![0xee, 0xee];
                data.extend_from_slice(&self.inputs[index][..size]);
                self.outputs.push((data, info));
            }
            if flags & BUFFER_FLAG_END_OF_STREAM != 0 {
                self.outputs.push((Vec::new(), BufferInfo { flags, ..Default::default() }));
            }
            !self.refuse
        }
        fn dequeue_output(&mut self, info: &mut BufferInfo, _timeout_us: i64) -> isize {
            match self.outputs.first() {
                Some((_, i)) => {
                    *info = *i;
                    0
                }
                None => INFO_TRY_AGAIN_LATER,
            }
        }
        fn output_buffer(&mut self, _index: usize) -> Option<&[u8]> {
            self.outputs.first().map(|o| o.0.as_slice())
        }
        fn release_output(&mut self, index: usize) {
            self.released.push(index);
            self.outputs.remove(0);
        }
    }

    #[test]
    fn a_frame_and_the_end() {
        let mut s = CodecSession::new(Fake { cap: 16, ..Default::default() });
        let mut buf = [0u8; 32];
        assert_eq!(s.poll(&mut buf), Ok(Polled::default())); // try again later
        assert_eq!(s.feed(&[1, 2, 3], 42, false), Ok(()));
        assert_eq!(s.poll(&mut buf), Ok(Polled { bytes: 3, key: true, pts_us: 42, eos: false }));
        assert_eq!(&buf[..3], &[1, 2, 3]); // from the buffer's offset
        assert_eq!(s.drain(), Ok(()));
        assert_eq!(s.drain(), Ok(())); // once
        assert_eq!(s.codec.queued.len(), 2);
        assert_eq!(s.poll(&mut buf), Ok(Polled { eos: true, ..Polled::default() }));
        assert_eq!(s.codec.released, [0, 0]);
    }

    #[test]
    fn refusals() {
        let mut s = CodecSession::new(Fake { cap: 2, ..Default::default() });
        assert_eq!(s.feed(&[1, 2, 3], 0, false), Err(libc::EINVAL)); // larger than the input buffer: given back empty
        assert_eq!(s.codec.queued, [(0, 0, 0, 0)]);
        s.codec.busy = true;
        assert_eq!(s.feed(&[1], 0, false), Err(libc::EAGAIN));
        assert_eq!(s.drain(), Err(libc::EAGAIN));
        s.codec.busy = false;
        s.codec.cap = 64;
        assert_eq!(s.feed(&[7; 40], 0, false), Ok(()));
        assert_eq!(s.poll(&mut [0u8; 8]), Err(libc::ENOSPC)); // a unit larger than the buffer: released, an error
        assert!(s.codec.outputs.is_empty());
        s.codec.refuse = true;
        assert_eq!(s.feed(&[1], 0, false), Err(libc::EIO));
    }
}
