// SPDX-License-Identifier: MIT
//! The provider-HAL back end (backend_hal.c): android.hardware.camera.provider.ICameraProvider/internal/0 (AIDL, over
//! /dev/binder) called the way cameraserver calls it, the way a GSI's framework reaches Samsung's vendor half. It does
//! not go through cameraserver, so nothing of system_server is needed: this is the route for a session (README.md,
//! "Which route").
//!
//! libbinder_ndk carries the transactions (its C API); the parcels are written by hand here to the AIDL definitions
//! of hardware/interfaces/camera/{provider,device}/aidl (android15-release, interface versions 1 to 3; a later field
//! is behind each parcelable's size and is skipped). Layout rules of the NDK AIDL back end: a parcelable is int32 1
//! (not null), int32 size (counting itself), the fields; a union is int32 1, int32 tag, the value; an array is int32
//! count, the elements; a method is transaction FIRST_CALL_TRANSACTION + its place in the interface.
//!
//! One stream: YCbCr_420_888 at the asked size into gralloc buffers of our own (AHardwareBuffer), a request per free
//! buffer, the picture read with AHardwareBuffer_lockPlanes when the HAL returns it.
//!
//! The parcels' layouts are functions over a [`Parcel`] (AParcel on Android, `tests::Fake` on a host, where they are
//! tested); the binder calls and the buffers are Android's only (`ffi`).

pub const PROVIDER: &str = "android.hardware.camera.provider.ICameraProvider";
pub const DEVICE: &str = "android.hardware.camera.device.ICameraDevice";
pub const SESSION: &str = "android.hardware.camera.device.ICameraDeviceSession";
pub const CALLBACK: &str = "android.hardware.camera.device.ICameraDeviceCallback";
/// The AIDL hash of android.hardware.camera.device version 3 (aidl_api/.../3/.hash).
pub const DEVICE_V3_HASH: &str = "ac7c7f74993871015f73daff1ea0c7f4d4f85ca7";

pub const PROVIDER_GET_ID_LIST: u32 = 2;
pub const PROVIDER_GET_DEVICE: u32 = 3;
pub const DEVICE_GET_CHARACTERISTICS: u32 = 0;
pub const DEVICE_OPEN: u32 = 4;
pub const SESSION_CLOSE: u32 = 0;
pub const SESSION_CONFIGURE_STREAMS: u32 = 1;
pub const SESSION_DEFAULT_SETTINGS: u32 = 2;
pub const SESSION_FLUSH: u32 = 3;
pub const SESSION_REQUEST_QUEUE: u32 = 4;
pub const SESSION_RESULT_QUEUE: u32 = 5;
pub const SESSION_PROCESS_REQUEST: u32 = 7;
pub const CALLBACK_NOTIFY: u32 = 0;
pub const CALLBACK_RESULT: u32 = 1;
pub const CALLBACK_REQUEST_BUFFERS: u32 = 2;
pub const CALLBACK_RETURN_BUFFERS: u32 = 3;
/// What every AIDL interface answers.
pub const METHOD_INTERFACE_VERSION: u32 = 16_777_214;
pub const METHOD_INTERFACE_HASH: u32 = 16_777_213;
pub const FIRST_CALL_TRANSACTION: u32 = 1;
pub const FORMAT_YCBCR_420_888: i32 = 0x23;
pub const DATASPACE_JFIF: i32 = 0x08c2_0000;
pub const MAX_BUFFERS: usize = 12;
const USAGE_CPU_READ_OFTEN: i64 = 3;

/// The parcel operations the layouts use (libbinder_ndk's AParcel_* of backend_hal.c). Reads give None on a failed
/// read; writes are not checked (as the C's).
pub trait Parcel {
    fn position(&mut self) -> i32;
    fn set_position(&mut self, at: i32) -> bool;
    fn write_i32(&mut self, v: i32);
    fn write_i64(&mut self, v: i64);
    fn write_bool(&mut self, v: bool);
    fn write_string(&mut self, s: &str);
    fn write_bytes(&mut self, b: &[u8]);
    fn write_i32_array(&mut self, a: &[i32]);
    fn write_fd(&mut self, fd: i32);
    fn read_i32(&mut self) -> Option<i32>;
    fn read_i64(&mut self) -> Option<i64>;
    fn read_bool(&mut self) -> Option<bool>;
    /// A string of at most 256 bytes; a null string is "".
    fn read_string(&mut self) -> Option<String>;
    fn read_bytes(&mut self) -> Option<Vec<u8>>;
    /// A file descriptor (ours to close), -1 for none.
    fn read_fd(&mut self) -> Option<i32>;
}

/// A gralloc buffer's handle as the HAL gets it (a NativeHandle: its descriptors and ints).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct NativeHandle {
    pub fds: Vec<i32>,
    pub ints: Vec<i32>,
}

/// Starts a parcelable: int32 1, then its size, written by [`put_end`].
pub fn put_begin(p: &mut impl Parcel) -> i32 {
    p.write_i32(1);
    let start = p.position();
    p.write_i32(0);
    start
}

pub fn put_end(p: &mut impl Parcel, start: i32) {
    let end = p.position();
    p.set_position(start);
    p.write_i32(end - start);
    p.set_position(end);
}

/// Enters a parcelable: where it ends; None when it is null or broken.
pub fn get_begin(p: &mut impl Parcel) -> Option<i32> {
    if p.read_i32()? == 0 {
        return None;
    }
    let start = p.position();
    let size = p.read_i32()?;
    (4..=1 << 28).contains(&size).then_some(start + size)
}

pub fn skip_parcelable(p: &mut impl Parcel) -> bool {
    get_begin(p).is_some_and(|end| p.set_position(end))
}

/// CameraMetadata { byte[] metadata }.
pub fn put_metadata(p: &mut impl Parcel, data: &[u8]) {
    let s = put_begin(p);
    p.write_bytes(data);
    put_end(p, s);
}

pub fn get_metadata(p: &mut impl Parcel) -> Option<Vec<u8>> {
    let end = get_begin(p)?;
    let bytes = p.read_bytes()?;
    p.set_position(end).then_some(bytes)
}

/// NativeHandle { ParcelFileDescriptor[] fds, int[] ints }.
pub fn put_handle(p: &mut impl Parcel, h: Option<&NativeHandle>) {
    let s = put_begin(p);
    let empty = NativeHandle::default();
    let h = h.unwrap_or(&empty);
    p.write_i32(h.fds.len() as i32);
    for &fd in &h.fds {
        p.write_fd(fd);
    }
    p.write_i32_array(&h.ints);
    put_end(p, s);
}

/// StreamBuffer { streamId, bufferId, buffer, status OK, no acquire fence, no release fence }.
pub fn put_stream_buffer(p: &mut impl Parcel, stream: i32, id: i64, h: Option<&NativeHandle>) {
    let s = put_begin(p);
    p.write_i32(stream);
    p.write_i64(id);
    put_handle(p, h);
    p.write_i32(0); // BufferStatus.OK
    put_handle(p, None); // no acquire fence
    put_handle(p, None); // no release fence
    put_end(p, s);
}

/// configureStreams' StreamConfiguration: one YCbCr_420_888 output stream of width x height that the CPU reads.
pub fn write_configuration(p: &mut impl Parcel, width: i32, height: i32) {
    let config = put_begin(p); // StreamConfiguration
    p.write_i32(1); //   streams: one
    let stream = put_begin(p); //   Stream
    p.write_i32(0); //     id
    p.write_i32(0); //     streamType OUTPUT
    p.write_i32(width);
    p.write_i32(height);
    p.write_i32(FORMAT_YCBCR_420_888);
    p.write_i64(USAGE_CPU_READ_OFTEN); //     usage: the consumer's
    p.write_i32(DATASPACE_JFIF);
    p.write_i32(0); //     rotation 0
    p.write_string(""); //     physicalCameraId
    p.write_i32(0); //     bufferSize
    p.write_i32(-1); //     groupId: none
    p.write_i32_array(&[0]); //     sensorPixelModesUsed: DEFAULT
    p.write_i64(1); //     dynamicRangeProfile STANDARD
    p.write_i64(0); //     useCase DEFAULT
    p.write_i32(-1); //     colorSpace: unspecified
    put_end(p, stream);
    p.write_i32(0); //   operationMode NORMAL_MODE
    put_metadata(p, &[]); //   sessionParams: none
    p.write_i32(0); //   streamConfigCounter
    p.write_bool(false); //   multiResolutionInputImage
    p.write_i64(0); //   logId
    put_end(p, config);
}

/// The output buffer of a request: the HAL's to ask for (HAL buffer management), or one of ours (id 1..; its handle
/// the first time it goes out).
#[derive(Clone, Copy, Debug)]
pub enum RequestBuffer<'a> {
    HalAsks,
    Ours { id: i64, handle: Option<&'a NativeHandle> },
}

/// processCaptureRequest's arguments: one CaptureRequest of `frame` with its settings (empty = as the request
/// before) and one output buffer, and no caches to remove.
pub fn write_request(p: &mut impl Parcel, frame: u32, settings: &[u8], buffer: RequestBuffer<'_>) {
    p.write_i32(1); // requests: one
    let s = put_begin(p); // CaptureRequest
    p.write_i32(frame as i32);
    p.write_i64(0); //   fmqSettingsSize: the settings are here
    put_metadata(p, settings);
    put_stream_buffer(p, -1, 0, None); //   inputBuffer: none
    p.write_i32(0); //   inputWidth
    p.write_i32(0); //   inputHeight
    p.write_i32(1); //   outputBuffers: one
    match buffer {
        RequestBuffer::HalAsks => put_stream_buffer(p, 0, 0, None), //   the HAL asks for it (requestStreamBuffers)
        RequestBuffer::Ours { id, handle } => put_stream_buffer(p, 0, id, handle),
    }
    p.write_i32(0); //   physicalCameraSettings: none
    put_end(p, s);
    p.write_i32(0); // cachesToRemove: none
}

/// Reads a NativeHandle that holds a fence and waits for it (at most a second).
fn wait_fence(p: &mut impl Parcel) {
    let Some(end) = get_begin(p) else { return };
    if let Some(count) = p.read_i32() {
        for _ in 0..count.clamp(0, 4) {
            let Some(fd) = p.read_fd() else { break };
            if fd >= 0 {
                let mut wait = libc::pollfd { fd, events: libc::POLLIN, revents: 0 };
                // SAFETY: a pollfd of a descriptor the parcel gave us, closed right after.
                unsafe {
                    libc::poll(&mut wait, 1, 1000);
                    libc::close(fd);
                }
            }
        }
    }
    p.set_position(end);
}

/// A buffer the HAL gave back: its id (1.. are ours), the frame, whether it holds a picture of a result.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Returned {
    pub id: i64,
    pub frame: u32,
    pub picture: bool,
    pub result: bool,
}

/// One StreamBuffer of a result (`with_picture`) or of returnStreamBuffers, handed to `done`. False when the parcel
/// is broken.
fn take_stream_buffer(p: &mut impl Parcel, frame: u32, with_picture: bool, done: &mut impl FnMut(Returned)) -> bool {
    let Some(end) = get_begin(p) else { return false };
    let mut id = 0;
    let mut status = 1;
    let ok = (|| {
        p.read_i32()?; // streamId
        id = p.read_i64()?;
        skip_parcelable(p).then_some(())?;
        status = p.read_i32()?;
        skip_parcelable(p).then_some(())
    })()
    .is_some();
    if ok {
        wait_fence(p);
    }
    done(Returned { id, frame, picture: ok && with_picture && status == 0, result: with_picture });
    p.set_position(end)
}

/// A message of notify(): a shutter's time, or an error.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Notify {
    /// ShutterMsg { frameNumber, timestamp, readoutTimestamp }
    Shutter { frame: u32, time: u64 },
    /// ErrorMsg { frameNumber, errorStreamId, errorCode }: 1 device, 2 request, 3 result, 4 buffer
    Error { frame: i32, code: i32 },
}

/// notify(NotifyMsg[]): the messages, as far as the parcel can be read.
pub fn read_notify(p: &mut impl Parcel) -> Vec<Notify> {
    let mut out = Vec::new();
    let Some(count) = p.read_i32() else { return out };
    for _ in 0..count.max(0) {
        let msg = (|| {
            if p.read_i32()? == 0 {
                return None;
            }
            let tag = p.read_i32()?;
            let end = get_begin(p)?;
            let frame = p.read_i32()?;
            let msg = match tag {
                1 => Some(Notify::Shutter { frame: frame as u32, time: p.read_i64()? as u64 }),
                0 => {
                    let _stream = p.read_i32();
                    let code = p.read_i32().unwrap_or(0);
                    Some(Notify::Error { frame, code })
                }
                _ => None,
            };
            Some((msg, end))
        })();
        let Some((msg, end)) = msg else { break };
        out.extend(msg);
        if !p.set_position(end) {
            break;
        }
    }
    out
}

/// processCaptureResult(CaptureResult[]): each result's output buffers, handed to `done` in order.
pub fn read_results(p: &mut impl Parcel, done: &mut impl FnMut(Returned)) {
    let Some(count) = p.read_i32() else { return };
    for _ in 0..count.max(0) {
        let Some(end) = get_begin(p) else { return };
        // CaptureResult { frameNumber, fmqResultSize, result, outputBuffers, ... }
        let head = (|| {
            let frame = p.read_i32()?;
            p.read_i64()?;
            skip_parcelable(p).then_some(())?;
            Some((frame, p.read_i32()?))
        })();
        let Some((frame, buffers)) = head else { return };
        for _ in 0..buffers.max(0) {
            if !take_stream_buffer(p, frame as u32, true, done) {
                return;
            }
        }
        if !p.set_position(end) {
            return;
        }
    }
}

/// returnStreamBuffers(StreamBuffer[]): unused buffers coming back.
pub fn read_returned(p: &mut impl Parcel, done: &mut impl FnMut(Returned)) {
    if let Some(count) = p.read_i32() {
        for _ in 0..count.max(0) {
            if !take_stream_buffer(p, 0, false, done) {
                break;
            }
        }
    }
}

/// requestStreamBuffers(BufferRequest[]): (stream, buffers wanted), at most 4.
pub fn read_buffer_requests(p: &mut impl Parcel) -> Vec<(i32, i32)> {
    let count = p.read_i32().filter(|c| (0..=4).contains(c)).unwrap_or(0);
    let mut out = Vec::new();
    for _ in 0..count {
        let req = (|| {
            let end = get_begin(p)?;
            let r = (p.read_i32()?, p.read_i32()?);
            p.set_position(end).then_some(r)
        })();
        match req {
            Some(r) => out.push(r),
            None => break,
        }
    }
    out
}

/// One of our buffers as the HAL holds it.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct BufferState {
    /// with the HAL (in a request, or given to it)
    pub held: bool,
    /// its handle went out once: from then on the HAL knows it by its id
    pub sent: bool,
}

/// requestStreamBuffers' answer (after the status header): BufferRequestStatus and StreamBufferRet[]. Every free
/// buffer of ours may go out (marked held and sent); `handle(i)` is buffer i's handle.
pub fn write_buffer_replies<'a>(
    out: &mut impl Parcel,
    requests: &[(i32, i32)],
    running: bool,
    buffers: &mut [BufferState],
    handle: impl Fn(usize) -> Option<&'a NativeHandle>,
) {
    let status_at = out.position();
    out.write_i32(0); // BufferRequestStatus, set below
    out.write_i32(requests.len() as i32);
    let mut all = true;
    for &(stream, wanted) in requests {
        let ret = put_begin(out); // StreamBufferRet { streamId, val }
        out.write_i32(stream);
        out.write_i32(1); // StreamBuffersVal: not null
        let have = buffers.iter().filter(|b| !b.held).count() as i32;
        if !running || stream != 0 || have < wanted {
            out.write_i32(0); // tag error
            out.write_i32(if !running || stream != 0 { 3 } else { 1 }); // STREAM_DISCONNECTED : NO_BUFFER_AVAILABLE
            all = false;
        } else {
            out.write_i32(1); // tag buffers
            out.write_i32(wanted);
            let mut given = 0;
            for (i, b) in buffers.iter_mut().enumerate() {
                if given >= wanted {
                    break;
                }
                if b.held {
                    continue;
                }
                put_stream_buffer(out, 0, i as i64 + 1, if b.sent { None } else { handle(i) });
                (b.held, b.sent) = (true, true);
                given += 1;
            }
        }
        put_end(out, ret);
    }
    let end = out.position();
    out.set_position(status_at);
    out.write_i32(if all { 0 } else { 1 }); // OK : FAILED_PARTIAL
    out.set_position(end);
}

/// What configureStreams answered of the stream (HalStream).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct HalStream {
    pub format: i32,
    pub producer: i64,
    pub consumer: i64,
    pub max_buffers: i32,
    pub hal_buffers: bool,
}

/// configureStreams' answer: HalStream[] { id, overrideFormat, producerUsage, consumerUsage, maxBuffers,
/// overrideDataSpace, physicalCameraId, supportOffline, enableHalBufferManager (version 2 on: read when the
/// parcelable has it) }. None when it is not one stream or cannot be read.
pub fn read_hal_stream(p: &mut impl Parcel) -> Option<HalStream> {
    if p.read_i32()? != 1 {
        return None;
    }
    let end = get_begin(p)?;
    let _id = p.read_i32()?;
    let mut s =
        HalStream { format: p.read_i32()?, producer: p.read_i64()?, consumer: p.read_i64()?, ..Default::default() };
    s.max_buffers = p.read_i32()?;
    let rest = (|| {
        p.read_i32()?; // overrideDataSpace
        p.read_string()?; // physicalCameraId
        p.read_bool()?; // supportOffline
        if p.position() < end {
            s.hal_buffers = p.read_bool().unwrap_or(false);
        }
        Some(())
    })();
    let _ = rest;
    Some(s)
}

/// A camera's id from its provider name: "device@1.1/internal/0" -> "0" (what follows the last slash).
pub fn camera_id(name: &str) -> &str {
    name.rsplit('/').next().unwrap_or(name)
}

#[cfg(target_os = "android")]
pub use ffi::HalBackend;

#[cfg(target_os = "android")]
mod ffi;

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    /// A parcel of 4-byte words in memory: i32 one word, i64 two, a bool a word, a string or byte array its length
    /// and its bytes padded to a word, a descriptor a word.
    #[derive(Default)]
    pub struct Fake {
        pub data: Vec<u8>,
        pub at: usize,
    }

    impl Fake {
        fn put(&mut self, bytes: &[u8]) {
            let padded = bytes.len().div_ceil(4) * 4;
            if self.data.len() < self.at + padded {
                self.data.resize(self.at + padded, 0);
            }
            self.data[self.at..self.at + bytes.len()].copy_from_slice(bytes);
            self.at += padded;
        }
        fn take(&mut self, n: usize) -> Option<&[u8]> {
            let padded = n.div_ceil(4) * 4;
            if self.at + padded > self.data.len() {
                return None;
            }
            let at = self.at;
            self.at += padded;
            Some(&self.data[at..at + n])
        }
        /// Back to the start, for reading what was written.
        pub fn rewind(&mut self) -> &mut Self {
            self.at = 0;
            self
        }
    }

    impl Parcel for Fake {
        fn position(&mut self) -> i32 {
            self.at as i32
        }
        fn set_position(&mut self, at: i32) -> bool {
            if at < 0 || at as usize > self.data.len() {
                return false;
            }
            self.at = at as usize;
            true
        }
        fn write_i32(&mut self, v: i32) {
            self.put(&v.to_le_bytes())
        }
        fn write_i64(&mut self, v: i64) {
            self.put(&v.to_le_bytes())
        }
        fn write_bool(&mut self, v: bool) {
            self.write_i32(i32::from(v))
        }
        fn write_string(&mut self, s: &str) {
            self.write_i32(s.len() as i32);
            self.put(s.as_bytes())
        }
        fn write_bytes(&mut self, b: &[u8]) {
            self.write_i32(b.len() as i32);
            self.put(b)
        }
        fn write_i32_array(&mut self, a: &[i32]) {
            self.write_i32(a.len() as i32);
            for &v in a {
                self.write_i32(v);
            }
        }
        fn write_fd(&mut self, fd: i32) {
            self.write_i32(fd)
        }
        fn read_i32(&mut self) -> Option<i32> {
            self.take(4).map(|b| i32::from_le_bytes(b.try_into().unwrap()))
        }
        fn read_i64(&mut self) -> Option<i64> {
            self.take(8).map(|b| i64::from_le_bytes(b.try_into().unwrap()))
        }
        fn read_bool(&mut self) -> Option<bool> {
            self.read_i32().map(|v| v != 0)
        }
        fn read_string(&mut self) -> Option<String> {
            let n = self.read_i32()?;
            if n < 0 {
                return Some(String::new());
            }
            if n > 256 {
                return None;
            }
            self.take(n as usize).map(|b| String::from_utf8_lossy(b).into_owned())
        }
        fn read_bytes(&mut self) -> Option<Vec<u8>> {
            let n = usize::try_from(self.read_i32()?).ok()?;
            self.take(n).map(<[u8]>::to_vec)
        }
        fn read_fd(&mut self) -> Option<i32> {
            self.read_i32()
        }
    }

    #[test]
    fn parcelables_carry_their_size() {
        let mut p = Fake::default();
        let s = put_begin(&mut p);
        p.write_i64(7);
        put_end(&mut p, s);
        p.write_i32(99);
        p.rewind();
        let end = get_begin(&mut p).unwrap();
        assert_eq!(end, 4 + 4 + 8); // the size counts itself
        assert!(p.set_position(end));
        assert_eq!(p.read_i32(), Some(99));
        // null, and a size that is no size
        let mut p = Fake::default();
        p.write_i32(0);
        p.write_i32(1);
        p.write_i32(2);
        p.rewind();
        assert_eq!(get_begin(&mut p), None);
        assert_eq!(get_begin(&mut p), None);
    }

    #[test]
    fn the_configuration() {
        let mut p = Fake::default();
        write_configuration(&mut p, 1280, 720);
        p.rewind();
        let config_end = get_begin(&mut p).unwrap();
        assert_eq!(p.read_i32(), Some(1)); // one stream
        let stream_end = get_begin(&mut p).unwrap();
        let ints: Vec<_> = (0..5).map(|_| p.read_i32().unwrap()).collect();
        assert_eq!(ints, [0, 0, 1280, 720, 0x23]);
        assert_eq!(p.read_i64(), Some(3)); // CPU_READ_OFTEN
        assert_eq!(p.read_i32(), Some(DATASPACE_JFIF));
        assert!(p.set_position(stream_end));
        assert_eq!(p.read_i32(), Some(0)); // NORMAL_MODE
        assert_eq!(get_metadata(&mut p), Some(Vec::new()));
        assert_eq!((p.read_i32(), p.read_bool(), p.read_i64()), (Some(0), Some(false), Some(0)));
        assert_eq!(p.position(), config_end);
    }

    #[test]
    fn a_request() {
        let handle = NativeHandle { fds: vec![5, 6], ints: vec![1, 2, 3] };
        let mut p = Fake::default();
        write_request(&mut p, 9, &[1, 2, 3, 4, 5], RequestBuffer::Ours { id: 3, handle: Some(&handle) });
        p.rewind();
        assert_eq!(p.read_i32(), Some(1));
        let end = get_begin(&mut p).unwrap();
        assert_eq!((p.read_i32(), p.read_i64()), (Some(9), Some(0)));
        assert_eq!(get_metadata(&mut p), Some(vec![1, 2, 3, 4, 5]));
        assert!(skip_parcelable(&mut p)); // no input buffer
        assert_eq!((p.read_i32(), p.read_i32(), p.read_i32()), (Some(0), Some(0), Some(1)));
        // the output buffer: stream 0, id 3, its handle
        get_begin(&mut p).unwrap();
        assert_eq!((p.read_i32(), p.read_i64()), (Some(0), Some(3)));
        get_begin(&mut p).unwrap();
        assert_eq!((p.read_i32(), p.read_fd(), p.read_fd()), (Some(2), Some(5), Some(6)));
        assert_eq!((p.read_i32(), p.read_i32(), p.read_i32(), p.read_i32()), (Some(3), Some(1), Some(2), Some(3)));
        assert!(p.set_position(end));
        assert_eq!(p.read_i32(), Some(0)); // no caches to remove
    }

    /// A StreamBuffer as the HAL writes one back: stream 0, `id`, no handle, `status`, no fences.
    fn returned_buffer(p: &mut Fake, id: i64, status: i32) {
        let s = put_begin(p);
        p.write_i32(0);
        p.write_i64(id);
        put_handle(p, None);
        p.write_i32(status);
        put_handle(p, None);
        put_handle(p, None);
        put_end(p, s);
    }

    #[test]
    fn results_and_returns() {
        let mut p = Fake::default();
        p.write_i32(2); // two results
        for (frame, ids) in [(4, vec![(1, 0), (2, 1)]), (5, vec![(3, 0)])] {
            let s = put_begin(&mut p);
            p.write_i32(frame);
            p.write_i64(0);
            put_metadata(&mut p, &[0; 6]);
            p.write_i32(ids.len() as i32);
            for (id, status) in ids {
                returned_buffer(&mut p, id, status);
            }
            p.write_i32(77); // a later field, skipped by the size
            put_end(&mut p, s);
        }
        let mut got = Vec::new();
        read_results(p.rewind(), &mut |r| got.push(r));
        let r = |id, frame, picture| Returned { id, frame, picture, result: true };
        assert_eq!(got, [r(1, 4, true), r(2, 4, false), r(3, 5, true)]);
        let mut p = Fake::default();
        p.write_i32(1);
        returned_buffer(&mut p, 2, 0);
        let mut got = Vec::new();
        read_returned(p.rewind(), &mut |r| got.push(r));
        assert_eq!(got, [Returned { id: 2, frame: 0, picture: false, result: false }]);
    }

    #[test]
    fn notify_messages() {
        let mut p = Fake::default();
        p.write_i32(3);
        // a shutter, an error, an unknown tag
        for (tag, fields) in [(1, vec![12i64]), (0, vec![]), (7, vec![])] {
            p.write_i32(1);
            p.write_i32(tag);
            let s = put_begin(&mut p);
            p.write_i32(12); // frame
            if tag == 1 {
                p.write_i64(fields[0] * 1000);
                p.write_i64(0);
            } else {
                p.write_i32(0);
                p.write_i32(1); // a device error
            }
            put_end(&mut p, s);
        }
        let got = read_notify(p.rewind());
        assert_eq!(got, [Notify::Shutter { frame: 12, time: 12000 }, Notify::Error { frame: 12, code: 1 }]);
    }

    #[test]
    fn buffers_asked_for() {
        let mut p = Fake::default();
        p.write_i32(2);
        for (stream, wanted) in [(0, 2), (1, 1)] {
            let s = put_begin(&mut p);
            p.write_i32(stream);
            p.write_i32(wanted);
            put_end(&mut p, s);
        }
        let requests = read_buffer_requests(p.rewind());
        assert_eq!(requests, [(0, 2), (1, 1)]);
        let handle = NativeHandle { fds: vec![9], ints: vec![] };
        let mut buffers =
            [BufferState { held: true, sent: true }, BufferState::default(), BufferState { held: false, sent: true }];
        let mut out = Fake::default();
        write_buffer_replies(&mut out, &requests, true, &mut buffers, |_| Some(&handle));
        assert!(buffers.iter().all(|b| b.held && b.sent));
        out.rewind();
        assert_eq!((out.read_i32(), out.read_i32()), (Some(1), Some(2))); // FAILED_PARTIAL, two answers
        get_begin(&mut out).unwrap();
        assert_eq!(
            (out.read_i32(), out.read_i32(), out.read_i32(), out.read_i32()),
            (Some(0), Some(1), Some(1), Some(2))
        );
        // buffer 2 (index 1) with its handle, buffer 3 known already
        get_begin(&mut out).unwrap();
        assert_eq!((out.read_i32(), out.read_i64()), (Some(0), Some(2)));
        get_begin(&mut out).unwrap();
        assert_eq!((out.read_i32(), out.read_fd()), (Some(1), Some(9)));
        // Not running: every answer is STREAM_DISCONNECTED.
        let mut out = Fake::default();
        write_buffer_replies(&mut out, &[(0, 1)], false, &mut [BufferState::default()], |_| None);
        out.rewind();
        out.read_i32();
        out.read_i32();
        get_begin(&mut out).unwrap();
        assert_eq!(
            (out.read_i32(), out.read_i32(), out.read_i32(), out.read_i32()),
            (Some(0), Some(1), Some(0), Some(3))
        );
    }

    #[test]
    fn the_hal_stream() {
        let hal_stream = |with_manager: bool| {
            let mut p = Fake::default();
            p.write_i32(1);
            let s = put_begin(&mut p);
            p.write_i32(0);
            p.write_i32(0x23);
            p.write_i64(0x100);
            p.write_i64(0x3);
            p.write_i32(6);
            p.write_i32(0);
            p.write_string("");
            p.write_bool(false);
            if with_manager {
                p.write_bool(true);
            }
            put_end(&mut p, s);
            p.rewind();
            read_hal_stream(&mut p).unwrap()
        };
        assert_eq!(
            hal_stream(false),
            HalStream { format: 0x23, producer: 0x100, consumer: 3, max_buffers: 6, hal_buffers: false }
        );
        assert!(hal_stream(true).hal_buffers);
        assert_eq!(camera_id("device@1.1/internal/0"), "0");
        assert_eq!(camera_id("7"), "7");
    }
}
