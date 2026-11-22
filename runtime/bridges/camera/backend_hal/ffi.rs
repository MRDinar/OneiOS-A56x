// SPDX-License-Identifier: MIT
//! The provider HAL over libbinder_ndk (Android only): the binder calls, the callback object the HAL calls, our gralloc
//! buffers. The parcels' layouts are the parent module's.
//!
//! Locks: the set-up (classes, provider, devices) is the back end's own (one thread: the helper's or camctl's). The
//! stream's state is one static lock shared with the HAL's calls into the callback (on this process's binder
//! threads) and the request thread; it is never held across a binder call of ours, since the HAL may call back while
//! it serves one.
use super::*;
use crate::cam_log;
use crate::convert::Yuv;
use crate::core::{Backend, FrameSink};
use crate::meta::{set_i32, CamInfo, CAM_MAX_LIST, CAM_TAG_AE_TARGET_FPS_RANGE};
use crate::util::verbose;
use std::ffi::{c_char, c_int, c_void, CString};
use std::ptr::null_mut;
use std::sync::{Arc, Condvar, Mutex, MutexGuard, PoisonError};
use std::thread::JoinHandle;
use std::time::Duration;

macro_rules! opaque {
    ($($name:ident),*) => { $( #[repr(C)] pub struct $name { _p: [u8; 0] } )* };
}
opaque!(AIBinder, AIBinderClass, AParcel, AStatus, AHardwareBuffer);

const STATUS_OK: i32 = 0;
const STATUS_UNKNOWN_TRANSACTION: i32 = -libc::EBADMSG;

type OnCreate = extern "C" fn(*mut c_void) -> *mut c_void;
type OnDestroy = extern "C" fn(*mut c_void);
type OnTransact = extern "C" fn(*mut AIBinder, u32, *const AParcel, *mut AParcel) -> i32;
type StringAllocator = extern "C" fn(*mut c_void, i32, *mut *mut c_char) -> bool;
type ByteAllocator = extern "C" fn(*mut c_void, i32, *mut *mut i8) -> bool;
type ArrayAllocator = extern "C" fn(*mut c_void, i32) -> bool;
type ElementAllocator = extern "C" fn(*mut c_void, usize, i32, *mut *mut c_char) -> bool;

#[repr(C)]
struct HardwareBufferDesc {
    width: u32,
    height: u32,
    layers: u32,
    format: u32,
    usage: u64,
    stride: u32,
    rfu0: u32,
    rfu1: u64,
}

#[repr(C)]
#[derive(Clone, Copy)]
struct HardwareBufferPlane {
    data: *mut u8,
    pixel_stride: u32,
    row_stride: u32,
}

#[repr(C)]
struct HardwareBufferPlanes {
    count: u32,
    planes: [HardwareBufferPlane; 4],
}

/// native_handle_t: version, numFds, numInts, then the fds and the ints.
#[repr(C)]
struct RawNativeHandle {
    version: c_int,
    num_fds: c_int,
    num_ints: c_int,
    data: [c_int; 0],
}

extern "C" {
    fn AIBinder_Class_define(
        descriptor: *const c_char,
        create: OnCreate,
        destroy: OnDestroy,
        transact: OnTransact,
    ) -> *mut AIBinderClass;
    fn AIBinder_associateClass(binder: *mut AIBinder, class: *const AIBinderClass) -> bool;
    fn AIBinder_new(class: *const AIBinderClass, args: *mut c_void) -> *mut AIBinder;
    fn AIBinder_decStrong(binder: *mut AIBinder);
    fn AIBinder_prepareTransaction(binder: *mut AIBinder, input: *mut *mut AParcel) -> i32;
    fn AIBinder_transact(
        binder: *mut AIBinder,
        code: u32,
        input: *mut *mut AParcel,
        output: *mut *mut AParcel,
        flags: u32,
    ) -> i32;
    fn AParcel_delete(parcel: *mut AParcel);
    fn AParcel_readStatusHeader(parcel: *const AParcel, status: *mut *mut AStatus) -> i32;
    fn AParcel_writeStatusHeader(parcel: *mut AParcel, status: *const AStatus) -> i32;
    fn AStatus_isOk(status: *const AStatus) -> bool;
    fn AStatus_getExceptionCode(status: *const AStatus) -> i32;
    fn AStatus_getServiceSpecificError(status: *const AStatus) -> i32;
    fn AStatus_newOk() -> *mut AStatus;
    fn AStatus_delete(status: *mut AStatus);
    fn AParcel_getDataPosition(parcel: *const AParcel) -> i32;
    fn AParcel_setDataPosition(parcel: *const AParcel, position: i32) -> i32;
    fn AParcel_writeInt32(parcel: *mut AParcel, v: i32) -> i32;
    fn AParcel_writeInt64(parcel: *mut AParcel, v: i64) -> i32;
    fn AParcel_writeBool(parcel: *mut AParcel, v: bool) -> i32;
    fn AParcel_writeString(parcel: *mut AParcel, s: *const c_char, length: i32) -> i32;
    fn AParcel_writeByteArray(parcel: *mut AParcel, data: *const i8, length: i32) -> i32;
    fn AParcel_writeInt32Array(parcel: *mut AParcel, data: *const i32, length: i32) -> i32;
    fn AParcel_writeParcelFileDescriptor(parcel: *mut AParcel, fd: c_int) -> i32;
    fn AParcel_writeStrongBinder(parcel: *mut AParcel, binder: *mut AIBinder) -> i32;
    fn AParcel_readInt32(parcel: *const AParcel, v: *mut i32) -> i32;
    fn AParcel_readInt64(parcel: *const AParcel, v: *mut i64) -> i32;
    fn AParcel_readBool(parcel: *const AParcel, v: *mut bool) -> i32;
    fn AParcel_readString(parcel: *const AParcel, data: *mut c_void, allocator: StringAllocator) -> i32;
    fn AParcel_readByteArray(parcel: *const AParcel, data: *mut c_void, allocator: ByteAllocator) -> i32;
    fn AParcel_readStringArray(
        parcel: *const AParcel,
        data: *mut c_void,
        array: ArrayAllocator,
        element: ElementAllocator,
    ) -> i32;
    fn AParcel_readParcelFileDescriptor(parcel: *const AParcel, fd: *mut c_int) -> i32;
    fn AParcel_readStrongBinder(parcel: *const AParcel, binder: *mut *mut AIBinder) -> i32;
    fn AHardwareBuffer_allocate(desc: *const HardwareBufferDesc, out: *mut *mut AHardwareBuffer) -> c_int;
    fn AHardwareBuffer_release(buffer: *mut AHardwareBuffer);
    fn AHardwareBuffer_lockPlanes(
        buffer: *mut AHardwareBuffer,
        usage: u64,
        fence: i32,
        rect: *const c_void,
        out: *mut HardwareBufferPlanes,
    ) -> c_int;
    fn AHardwareBuffer_unlock(buffer: *mut AHardwareBuffer, fence: *mut i32) -> c_int;
}

// ---- parcels -----------------------------------------------------------------------------------------------------
/// An AParcel of libbinder_ndk as a [`Parcel`] (not owned: the reply type below deletes its own).
struct AParcelRef(*mut AParcel);

extern "C" fn string_allocator(ctx: *mut c_void, length: i32, out: *mut *mut c_char) -> bool {
    // SAFETY: ctx is the Option<Vec<u8>> read_string passed; out is the NDK's to fill.
    let into = unsafe { &mut *ctx.cast::<Option<Vec<u8>>>() };
    // length counts the terminator; -1 is a null string.
    if length < 0 {
        *into = Some(Vec::new());
        return true;
    }
    if length > 256 {
        return false;
    }
    let v = into.insert(vec![0; length as usize]);
    // SAFETY: as above.
    unsafe { *out = v.as_mut_ptr().cast() };
    true
}

extern "C" fn byte_allocator(ctx: *mut c_void, length: i32, out: *mut *mut i8) -> bool {
    // SAFETY: ctx is the Option<Vec<u8>> read_bytes passed; out is the NDK's to fill.
    let into = unsafe { &mut *ctx.cast::<Option<Vec<u8>>>() };
    if length < 0 {
        return false;
    }
    let v = into.insert(vec![0; (length as usize).max(1)]);
    v.truncate(length as usize);
    // SAFETY: as above (the capacity is at least one byte).
    unsafe { *out = v.as_mut_ptr().cast() };
    true
}

fn ok(rc: i32) -> Option<()> {
    (rc == STATUS_OK).then_some(())
}

impl Parcel for AParcelRef {
    fn position(&mut self) -> i32 {
        // SAFETY: a live parcel.
        unsafe { AParcel_getDataPosition(self.0) }
    }
    fn set_position(&mut self, at: i32) -> bool {
        // SAFETY: a live parcel.
        unsafe { AParcel_setDataPosition(self.0, at) == STATUS_OK }
    }
    fn write_i32(&mut self, v: i32) {
        // SAFETY: a live parcel.
        unsafe { AParcel_writeInt32(self.0, v) };
    }
    fn write_i64(&mut self, v: i64) {
        // SAFETY: a live parcel.
        unsafe { AParcel_writeInt64(self.0, v) };
    }
    fn write_bool(&mut self, v: bool) {
        // SAFETY: a live parcel.
        unsafe { AParcel_writeBool(self.0, v) };
    }
    fn write_string(&mut self, s: &str) {
        // SAFETY: a live parcel and `s.len()` bytes of UTF-8.
        unsafe { AParcel_writeString(self.0, s.as_ptr().cast(), s.len() as i32) };
    }
    fn write_bytes(&mut self, b: &[u8]) {
        let p = if b.is_empty() { std::ptr::null() } else { b.as_ptr().cast() };
        // SAFETY: a live parcel and `b.len()` bytes (NULL and 0 for none, as the C).
        unsafe { AParcel_writeByteArray(self.0, p, b.len() as i32) };
    }
    fn write_i32_array(&mut self, a: &[i32]) {
        let p = if a.is_empty() { std::ptr::null() } else { a.as_ptr() };
        // SAFETY: a live parcel and `a.len()` ints.
        unsafe { AParcel_writeInt32Array(self.0, p, a.len() as i32) };
    }
    fn write_fd(&mut self, fd: i32) {
        // SAFETY: a live parcel; binder duplicates the descriptor.
        unsafe { AParcel_writeParcelFileDescriptor(self.0, fd) };
    }
    fn read_i32(&mut self) -> Option<i32> {
        let mut v = 0;
        // SAFETY: a live parcel.
        ok(unsafe { AParcel_readInt32(self.0, &mut v) }).map(|()| v)
    }
    fn read_i64(&mut self) -> Option<i64> {
        let mut v = 0;
        // SAFETY: a live parcel.
        ok(unsafe { AParcel_readInt64(self.0, &mut v) }).map(|()| v)
    }
    fn read_bool(&mut self) -> Option<bool> {
        let mut v = false;
        // SAFETY: a live parcel.
        ok(unsafe { AParcel_readBool(self.0, &mut v) }).map(|()| v)
    }
    fn read_string(&mut self) -> Option<String> {
        let mut into: Option<Vec<u8>> = None;
        // SAFETY: a live parcel; the allocator fills `into`.
        ok(unsafe { AParcel_readString(self.0, (&mut into as *mut Option<Vec<u8>>).cast(), string_allocator) })?;
        let mut v = into.unwrap_or_default();
        if v.last() == Some(&0) {
            v.pop();
        }
        Some(String::from_utf8_lossy(&v).into_owned())
    }
    fn read_bytes(&mut self) -> Option<Vec<u8>> {
        let mut into: Option<Vec<u8>> = None;
        // SAFETY: a live parcel; the allocator fills `into`.
        ok(unsafe { AParcel_readByteArray(self.0, (&mut into as *mut Option<Vec<u8>>).cast(), byte_allocator) })?;
        Some(into.unwrap_or_default())
    }
    fn read_fd(&mut self) -> Option<i32> {
        let mut fd = -1;
        // SAFETY: a live parcel; the descriptor is ours.
        ok(unsafe { AParcel_readParcelFileDescriptor(self.0, &mut fd) }).map(|()| fd)
    }
}

impl AParcelRef {
    fn read_binder(&mut self) -> Option<*mut AIBinder> {
        let mut b = null_mut();
        // SAFETY: a live parcel; the binder's reference is ours.
        ok(unsafe { AParcel_readStrongBinder(self.0, &mut b) })?;
        (!b.is_null()).then_some(b)
    }

    fn write_binder(&mut self, b: *mut AIBinder) {
        // SAFETY: a live parcel and binder.
        unsafe { AParcel_writeStrongBinder(self.0, b) };
    }
}

/// A reply parcel, deleted on drop.
struct Reply(AParcelRef);

impl Drop for Reply {
    fn drop(&mut self) {
        // SAFETY: the reply transact gave us, deleted once.
        unsafe { AParcel_delete(self.0 .0) };
    }
}

/// A call (`call`): `fill` writes the arguments; the reply after its status was read (None on failure, said).
fn call(binder: *mut AIBinder, method: u32, what: &str, fill: impl FnOnce(&mut AParcelRef)) -> Option<Reply> {
    let (mut input, mut output) = (null_mut(), null_mut());
    // SAFETY: a live binder; the parcels are the NDK's (transact takes `input`; `output` is ours to delete).
    let mut rc = unsafe { AIBinder_prepareTransaction(binder, &mut input) };
    if rc == STATUS_OK {
        fill(&mut AParcelRef(input));
        // SAFETY: as above.
        rc = unsafe { AIBinder_transact(binder, FIRST_CALL_TRANSACTION + method, &mut input, &mut output, 0) };
    }
    if rc != STATUS_OK {
        cam_log!("hal: {what}: binder status {rc}");
        if !input.is_null() {
            // SAFETY: a parcel transact did not take.
            unsafe { AParcel_delete(input) };
        }
        return None;
    }
    let reply = Reply(AParcelRef(output));
    let mut status = null_mut();
    // SAFETY: the reply parcel; the status is deleted below.
    if unsafe { AParcel_readStatusHeader(output, &mut status) } != STATUS_OK || status.is_null() {
        cam_log!("hal: {what}: no status in the reply");
        return None;
    }
    // SAFETY: the status read above, deleted once.
    unsafe {
        let good = AStatus_isOk(status);
        if !good {
            // The HAL's errors are service-specific (camera/common Status): 1 illegal argument, 2 camera in use, 3 too
            // many cameras in use, 4 operation not supported, 5 camera disconnected, 6 internal error.
            cam_log!(
                "hal: {what}: exception {}, camera status {}",
                AStatus_getExceptionCode(status),
                AStatus_getServiceSpecificError(status)
            );
        }
        AStatus_delete(status);
        good.then_some(reply)
    }
}

// ---- the stream's state ------------------------------------------------------------------------------------------
struct Buffer {
    buffer: *mut AHardwareBuffer,
    state: BufferState,
}

struct Stream {
    session: *mut AIBinder,
    running: bool,
    submitting: bool,
    hal_buffers: bool,
    width: usize,
    height: usize,
    max_inflight: usize,
    inflight: usize,
    buffers: Vec<Buffer>,
    /// the preview template with our frame rate, sent with frame 0
    settings: Vec<u8>,
    next_frame: u32,
    /// the shutter's time of each frame (frame % 64)
    stamps: [(u32, u64); 64],
    sink: Option<Arc<dyn FrameSink>>,
}

// SAFETY: the binder and buffer pointers are libbinder_ndk's and libnativewindow's objects, usable from any thread;
// they are reached only with STREAM's lock held.
unsafe impl Send for Stream {}

static STREAM: Mutex<Stream> = Mutex::new(Stream {
    session: null_mut(),
    running: false,
    submitting: false,
    hal_buffers: false,
    width: 0,
    height: 0,
    max_inflight: 0,
    inflight: 0,
    buffers: Vec::new(),
    settings: Vec::new(),
    next_frame: 0,
    stamps: [(u32::MAX, u64::MAX); 64],
    sink: None,
});
/// open replies of the buffers and new free buffers (the request thread waits on it)
static WAKE: Condvar = Condvar::new();

fn stream() -> MutexGuard<'static, Stream> {
    STREAM.lock().unwrap_or_else(PoisonError::into_inner)
}

/// The platform's `AHardwareBuffer_getNativeHandle` (dlsym'd at the set-up).
static BUFFER_HANDLE: std::sync::OnceLock<usize> = std::sync::OnceLock::new();

/// A buffer's handle, copied (for its first trip to the HAL).
fn handle_of(buffer: *mut AHardwareBuffer) -> Option<NativeHandle> {
    let f = *BUFFER_HANDLE.get()?;
    // SAFETY: the platform function by its C signature; the handle belongs to the buffer and is read at once.
    unsafe {
        let f: extern "C" fn(*const AHardwareBuffer) -> *const RawNativeHandle = std::mem::transmute(f);
        let h = f(buffer);
        if h.is_null() {
            return None;
        }
        let (nf, ni) = ((*h).num_fds.max(0) as usize, (*h).num_ints.max(0) as usize);
        let data = std::slice::from_raw_parts((*h).data.as_ptr(), nf + ni);
        Some(NativeHandle { fds: data[..nf].to_vec(), ints: data[nf..].to_vec() })
    }
}

/// A buffer came back: in a result (the request it belonged to is done) or unused (`buffer_done`).
fn buffer_done(st: &mut Stream, r: Returned) {
    if r.result && st.inflight > 0 {
        st.inflight -= 1;
    }
    let Some(index) = usize::try_from(r.id).ok().filter(|&i| i >= 1 && i <= st.buffers.len()).map(|i| i - 1) else {
        return;
    };
    let b = &st.buffers[index];
    if r.picture && st.running && b.state.held {
        let stamp = st.stamps[(r.frame % 64) as usize];
        let time = if stamp.0 == r.frame { stamp.1 } else { 0 };
        // SAFETY: an all-zero planes record to fill.
        let mut planes: HardwareBufferPlanes = unsafe { std::mem::zeroed() };
        // SAFETY: our buffer, locked for reading and unlocked below.
        if unsafe {
            AHardwareBuffer_lockPlanes(
                b.buffer,
                AHARDWAREBUFFER_USAGE_CPU_READ_OFTEN,
                -1,
                std::ptr::null(),
                &mut planes,
            )
        } == 0
        {
            let p = planes.planes;
            if planes.count >= 3 && !p[0].data.is_null() && !p[1].data.is_null() && !p[2].data.is_null() {
                // SAFETY: the locked buffer's planes, as gralloc describes them, valid until the unlock.
                let picture = unsafe {
                    Yuv::from_raw(
                        p[0].data,
                        p[1].data,
                        p[2].data,
                        p[0].row_stride as usize,
                        p[1].row_stride as usize,
                        p[1].pixel_stride as usize,
                        st.width,
                        st.height,
                    )
                };
                if let Some(sink) = &st.sink {
                    sink.frame(&picture, time);
                }
            }
            // SAFETY: as above.
            unsafe { AHardwareBuffer_unlock(b.buffer, null_mut()) };
        }
    }
    st.buffers[index].state.held = false;
}

use crate::backend_ndk::AHARDWAREBUFFER_USAGE_CPU_READ_OFTEN;

fn reply_ok(out: *mut AParcel) {
    // SAFETY: a fresh OK status written into the reply and deleted.
    unsafe {
        let status = AStatus_newOk();
        AParcel_writeStatusHeader(out, status);
        AStatus_delete(status);
    }
}

/// The stream's end by itself: the sink's error, outside the lock.
fn stream_error(st: MutexGuard<'_, Stream>, errno: i32) {
    let sink = if st.running { st.sink.clone() } else { None };
    drop(st);
    if let Some(s) = sink {
        s.error(errno);
    }
}

/// The callback the HAL calls (ICameraDeviceCallback), on this process's binder threads.
extern "C" fn callback_transact(_: *mut AIBinder, code: u32, input: *const AParcel, out: *mut AParcel) -> i32 {
    let mut p = AParcelRef(input.cast_mut());
    match code.wrapping_sub(FIRST_CALL_TRANSACTION) {
        CALLBACK_NOTIFY => {
            for m in read_notify(&mut p) {
                match m {
                    Notify::Shutter { frame, time } => stream().stamps[(frame % 64) as usize] = (frame, time),
                    Notify::Error { frame, code } => {
                        cam_log!("hal: error {code} for frame {frame} (1 device, 2 request, 3 result, 4 buffer)");
                        if code == 1 {
                            stream_error(stream(), libc::EIO);
                        }
                    }
                }
            }
            reply_ok(out);
        }
        CALLBACK_RESULT => {
            read_results(&mut p, &mut |r| {
                let mut st = stream();
                buffer_done(&mut st, r);
                WAKE.notify_all();
            });
            reply_ok(out);
        }
        CALLBACK_REQUEST_BUFFERS => {
            // The HAL asks for buffers itself (HAL buffer management): every free buffer of ours may go out.
            let requests = read_buffer_requests(&mut p);
            reply_ok(out);
            let mut st = stream();
            let handles: Vec<Option<NativeHandle>> =
                st.buffers.iter().map(|b| if b.state.sent { None } else { handle_of(b.buffer) }).collect();
            let running = st.running;
            let mut states: Vec<BufferState> = st.buffers.iter().map(|b| b.state).collect();
            write_buffer_replies(&mut AParcelRef(out), &requests, running, &mut states, |i| handles[i].as_ref());
            for (b, s) in st.buffers.iter_mut().zip(states) {
                b.state = s;
            }
        }
        CALLBACK_RETURN_BUFFERS => {
            read_returned(&mut p, &mut |r| {
                let mut st = stream();
                buffer_done(&mut st, r);
                WAKE.notify_all();
            });
            reply_ok(out);
        }
        METHOD_INTERFACE_VERSION => {
            reply_ok(out);
            AParcelRef(out).write_i32(3);
        }
        METHOD_INTERFACE_HASH => {
            reply_ok(out);
            AParcelRef(out).write_string(DEVICE_V3_HASH);
        }
        _ => return STATUS_UNKNOWN_TRANSACTION,
    }
    STATUS_OK
}

extern "C" fn class_create(args: *mut c_void) -> *mut c_void {
    args
}
extern "C" fn class_destroy(_: *mut c_void) {}
extern "C" fn no_transact(_: *mut AIBinder, _: u32, _: *const AParcel, _: *mut AParcel) -> i32 {
    STATUS_UNKNOWN_TRANSACTION
}

/// The request thread: a request per free buffer (or, with the HAL's own buffer management, up to its count in
/// flight), until the stop (`submit`).
fn submit() {
    let mut st = stream();
    while st.submitting {
        let buffer = if st.hal_buffers { None } else { st.buffers.iter().position(|b| !b.state.held) };
        if st.inflight >= st.max_inflight || (!st.hal_buffers && buffer.is_none()) {
            let (guard, timeout) =
                WAKE.wait_timeout(st, Duration::from_secs(1)).unwrap_or_else(PoisonError::into_inner);
            st = guard;
            // With the HAL's own buffer management a failed request returns no buffer: after a second without one,
            // the count of requests in flight starts again.
            if timeout.timed_out() && st.hal_buffers {
                st.inflight = 0;
            }
            continue;
        }
        let frame = st.next_frame;
        st.next_frame += 1;
        st.inflight += 1;
        // The settings go with the first request only (empty = as the request before); a buffer's handle only on
        // its first trip.
        let settings = if frame == 0 { std::mem::take(&mut st.settings) } else { Vec::new() };
        let handle = buffer.and_then(|b| {
            st.buffers[b].state.held = true;
            (!st.buffers[b].state.sent).then(|| handle_of(st.buffers[b].buffer)).flatten()
        });
        let session = st.session;
        drop(st);
        let request = match buffer {
            Some(b) => RequestBuffer::Ours { id: b as i64 + 1, handle: handle.as_ref() },
            None => RequestBuffer::HalAsks,
        };
        let reply = call(session, SESSION_PROCESS_REQUEST, "processCaptureRequest", |p| {
            write_request(p, frame, &settings, request)
        });
        st = stream();
        if let Some(b) = buffer {
            st.buffers[b].state.sent = true;
        }
        if reply.is_none() {
            if let Some(b) = buffer {
                st.buffers[b].state.held = false;
            }
            st.submitting = false;
            stream_error(st, libc::EIO);
            return;
        }
    }
}

// ---- the back end ------------------------------------------------------------------------------------------------
type CheckService = extern "C" fn(*const c_char) -> *mut AIBinder;
type MarkVintf = extern "C" fn(*mut AIBinder);

/// The provider-HAL back end: its binder objects (the set-up of `hal_init`) and the request thread.
#[derive(Default)]
pub struct HalBackend {
    ready: bool,
    device_class: usize,
    session_class: usize,
    provider: usize,
    callback: usize,
    devices: Vec<(String, usize)>,
    submitter: Option<JoinHandle<()>>,
}

impl HalBackend {
    fn init(&mut self) -> Result<(), i32> {
        if self.ready {
            return Ok(());
        }
        let check = crate::android::platform_symbol(c"AServiceManager_checkService");
        let vintf = crate::android::platform_symbol(c"AIBinder_markVintfStability");
        let handle = crate::android::platform_symbol(c"AHardwareBuffer_getNativeHandle");
        let (Some(check), Some(vintf), Some(handle), Ok(())) = (check, vintf, handle, crate::android::binder_threads())
        else {
            cam_log!(
                "hal: this Android's libbinder_ndk or libnativewindow lacks a platform symbol (service {}, vintf {}, handle {}, or the thread pool's)",
                i32::from(check.is_some()),
                i32::from(vintf.is_some()),
                i32::from(handle.is_some())
            );
            return Err(libc::ENOSYS);
        };
        let _ = BUFFER_HANDLE.set(handle as usize);
        // SAFETY: the platform functions by their C signatures; the classes live for the process's life.
        unsafe {
            let check: CheckService = std::mem::transmute(check);
            let vintf: MarkVintf = std::mem::transmute(vintf);
            let define = |name: &str, transact: OnTransact| {
                let name = CString::new(name).unwrap();
                AIBinder_Class_define(name.as_ptr(), class_create, class_destroy, transact)
            };
            let provider_class = define(PROVIDER, no_transact);
            let device_class = define(DEVICE, no_transact);
            let session_class = define(SESSION, no_transact);
            let callback_class = define(CALLBACK, callback_transact);
            if provider_class.is_null() || device_class.is_null() || session_class.is_null() || callback_class.is_null()
            {
                return Err(libc::ENOMEM);
            }
            // binder_threads() above: the HAL's calls into the callback arrive on those threads.
            let name = CString::new(format!("{PROVIDER}/internal/0")).unwrap();
            let provider = check(name.as_ptr());
            if provider.is_null() {
                cam_log!("hal: no service {PROVIDER}/internal/0");
                return Err(libc::ENOENT);
            }
            if !AIBinder_associateClass(provider, provider_class) {
                cam_log!("hal: the provider is not an {PROVIDER}");
                return Err(libc::EPROTO);
            }
            let callback = AIBinder_new(callback_class, null_mut());
            if callback.is_null() {
                return Err(libc::ENOMEM);
            }
            vintf(callback); // a vendor process may call it only with this
            (self.device_class, self.session_class) = (device_class as usize, session_class as usize);
            (self.provider, self.callback) = (provider as usize, callback as usize);
        }
        self.ready = true;
        Ok(())
    }

    fn teardown(&mut self) {
        let session = std::mem::replace(&mut stream().session, null_mut());
        if !session.is_null() {
            drop(call(session, SESSION_FLUSH, "flush", |_| {}));
            drop(call(session, SESSION_CLOSE, "close", |_| {})); // returns when the HAL holds nothing of ours
                                                                 // SAFETY: our reference to the session, given back once.
            unsafe { AIBinder_decStrong(session) };
        }
        let mut st = stream();
        for b in st.buffers.drain(..) {
            // SAFETY: our buffer, released once.
            unsafe { AHardwareBuffer_release(b.buffer) };
        }
        st.inflight = 0;
        st.settings = Vec::new();
    }

    /// The steps of a start up to the request thread (`hal_start` before its `failed:` label): the failed step.
    fn open(&mut self, camera: &CamInfo, device: *mut AIBinder, fps: [i32; 2]) -> Result<(), &'static str> {
        let callback = self.callback as *mut AIBinder;
        let reply = call(device, DEVICE_OPEN, "open", |p| p.write_binder(callback)).ok_or("")?;
        let session = AParcelRef(reply.0 .0).read_binder().ok_or("open")?;
        stream().session = session;
        drop(reply);
        // SAFETY: a live binder and our class.
        if !unsafe { AIBinder_associateClass(session, self.session_class as *const AIBinderClass) } {
            return Err("open");
        }
        // cameraserver takes both metadata queues before anything else; a HAL may expect them made. They are not
        // used: settings go in the request, and a result's metadata is not needed.
        drop(call(session, SESSION_REQUEST_QUEUE, "getCaptureRequestMetadataQueue", |_| {}));
        drop(call(session, SESSION_RESULT_QUEUE, "getCaptureResultMetadataQueue", |_| {}));

        let step = "constructDefaultRequestSettings";
        let reply = call(session, SESSION_DEFAULT_SETTINGS, step, |p| p.write_i32(1)).ok_or(step)?; // PREVIEW
        let mut settings = get_metadata(&mut AParcelRef(reply.0 .0)).filter(|s| s.len() >= 48).ok_or(step)?;
        drop(reply);
        if set_i32(&mut settings, CAM_TAG_AE_TARGET_FPS_RANGE, &fps).is_err() {
            cam_log!("hal: the preview template has no frame rate range: the camera's own rate is used");
        }
        stream().settings = settings;

        let step = "configureStreams";
        let (width, height) = {
            let st = stream();
            (st.width as i32, st.height as i32)
        };
        let reply =
            call(session, SESSION_CONFIGURE_STREAMS, step, |p| write_configuration(p, width, height)).ok_or(step)?;
        let Some(hal) = read_hal_stream(&mut AParcelRef(reply.0 .0)) else {
            cam_log!("hal: configureStreams: the answer could not be read");
            return Err(step);
        };
        drop(reply);
        let mut st = stream();
        st.hal_buffers = hal.hal_buffers || camera.hal_buffers;
        st.max_inflight = (hal.max_buffers.max(1) as usize).min(MAX_BUFFERS - 1);
        let count = st.max_inflight + 1;
        if verbose() != 0 {
            let format = if hal.format == 0 { "0".into() } else { format!("{:#x}", hal.format) };
            let producer = if hal.producer == 0 { "0".into() } else { format!("{:#x}", hal.producer) };
            cam_log!(
                "hal: stream format {format}, producer usage {producer}, at most {} buffers, HAL buffer management {}",
                hal.max_buffers,
                i32::from(st.hal_buffers)
            );
        }
        let desc = HardwareBufferDesc {
            width: width as u32,
            height: height as u32,
            layers: 1,
            format: hal.format as u32,
            usage: hal.producer as u64 | hal.consumer as u64 | AHARDWAREBUFFER_USAGE_CPU_READ_OFTEN,
            stride: 0,
            rfu0: 0,
            rfu1: 0,
        };
        for _ in 0..count {
            let mut buffer = null_mut();
            // SAFETY: a buffer description; the buffer is released at the teardown.
            if unsafe { AHardwareBuffer_allocate(&desc, &mut buffer) } != 0 || buffer.is_null() {
                return Err("AHardwareBuffer_allocate");
            }
            st.buffers.push(Buffer { buffer, state: BufferState::default() });
            if handle_of(buffer).is_none() {
                return Err("AHardwareBuffer_allocate");
            }
        }
        Ok(())
    }
}

impl Backend for HalBackend {
    fn name(&self) -> &'static str {
        "hal"
    }

    fn list(&mut self, max: usize) -> Result<Vec<CamInfo>, i32> {
        self.init()?;
        let reply =
            call(self.provider as *mut AIBinder, PROVIDER_GET_ID_LIST, "getCameraIdList", |_| {}).ok_or(libc::EIO)?;
        let mut names = Names::default();
        // SAFETY: the reply parcel; the allocators fill `names`.
        let status = unsafe {
            AParcel_readStringArray(reply.0 .0, (&mut names as *mut Names).cast(), names_allocator, name_allocator)
        };
        drop(reply);
        if status != STATUS_OK {
            cam_log!("hal: getCameraIdList: the list could not be read ({status})");
            return Err(libc::EIO);
        }
        for (_, d) in self.devices.drain(..) {
            // SAFETY: our references to the devices of the last list.
            unsafe { AIBinder_decStrong(d as *mut AIBinder) };
        }
        let mut out = Vec::new();
        for name in names.names.iter().take(names.count) {
            if out.len() >= max.min(CAM_MAX_LIST) {
                break;
            }
            let name = xh_core::text::nul_text(name);
            let id = camera_id(&name).to_owned();
            let Some(reply) =
                call(self.provider as *mut AIBinder, PROVIDER_GET_DEVICE, "getCameraDeviceInterface", |p| {
                    p.write_string(&name)
                })
            else {
                continue;
            };
            let device = AParcelRef(reply.0 .0).read_binder();
            drop(reply);
            let Some(device) = device else { continue };
            // SAFETY: a live binder and our class.
            if !unsafe { AIBinder_associateClass(device, self.device_class as *const AIBinderClass) } {
                // SAFETY: our reference, given back.
                unsafe { AIBinder_decStrong(device) };
                continue;
            }
            let meta = call(device, DEVICE_GET_CHARACTERISTICS, "getCameraCharacteristics", |_| {})
                .and_then(|reply| get_metadata(&mut AParcelRef(reply.0 .0)));
            let Some(meta) = meta else {
                // SAFETY: as above.
                unsafe { AIBinder_decStrong(device) };
                continue;
            };
            let mut c = CamInfo::named(&id, "");
            c.read(&meta[..]);
            out.push(c);
            self.devices.push((crate::util::truncate(&id, 31).to_owned(), device as usize));
        }
        Ok(out)
    }

    fn start(
        &mut self,
        camera: &CamInfo,
        width: i32,
        height: i32,
        fps_lo: i32,
        fps_hi: i32,
        sink: Arc<dyn FrameSink>,
    ) -> Result<(), i32> {
        self.init()?;
        let device =
            self.devices.iter().find(|(id, _)| *id == camera.id).map(|d| d.1 as *mut AIBinder).ok_or(libc::ENODEV)?;
        {
            let mut st = stream();
            (st.width, st.height, st.next_frame) = (width as usize, height as usize, 0);
            st.sink = Some(sink);
            st.stamps = [(u32::MAX, u64::MAX); 64];
        }
        let failed = match self.open(camera, device, [fps_lo, fps_hi]) {
            Err("") => {
                stream().sink = None;
                return Err(libc::EBUSY); // open refused: in use, or the HAL said no
            }
            Err(step) => step,
            Ok(()) => {
                {
                    let mut st = stream();
                    (st.running, st.submitting) = (true, true);
                }
                match std::thread::Builder::new().name("camera-hal".into()).spawn(submit) {
                    Ok(t) => {
                        self.submitter = Some(t);
                        return Ok(());
                    }
                    Err(_) => {
                        let mut st = stream();
                        (st.running, st.submitting) = (false, false);
                        "the request thread"
                    }
                }
            }
        };
        cam_log!("hal: camera {}: {failed} failed", camera.id);
        stream().sink = None;
        self.teardown();
        Err(libc::EIO)
    }

    fn stop(&mut self) {
        let was = {
            let mut st = stream();
            let was = st.running;
            (st.running, st.submitting) = (false, false); // from here no frame is handed on
            WAKE.notify_all();
            was
        };
        if !was {
            return;
        }
        if let Some(t) = self.submitter.take() {
            let _ = t.join();
        }
        stream().sink = None;
        self.teardown();
    }
}

// SAFETY: the back end's binder objects are libbinder_ndk's, usable from any thread; it is used by one at a time.
unsafe impl Send for HalBackend {}

/// getCameraIdList's names: 32 of 96 bytes at most.
struct Names {
    names: [[u8; 96]; CAM_MAX_LIST],
    count: usize,
}

impl Default for Names {
    fn default() -> Names {
        Names { names: [[0; 96]; CAM_MAX_LIST], count: 0 }
    }
}

extern "C" fn names_allocator(ctx: *mut c_void, length: i32) -> bool {
    // SAFETY: ctx is the Names list() passed.
    let l = unsafe { &mut *ctx.cast::<Names>() };
    l.count = length.clamp(0, CAM_MAX_LIST as i32) as usize;
    length <= CAM_MAX_LIST as i32
}

extern "C" fn name_allocator(ctx: *mut c_void, index: usize, length: i32, out: *mut *mut c_char) -> bool {
    // SAFETY: ctx is the Names list() passed; out is the NDK's to fill.
    let l = unsafe { &mut *ctx.cast::<Names>() };
    if index >= CAM_MAX_LIST || length > 96 {
        return false;
    }
    if length < 0 {
        l.names[index][0] = 0;
        return true;
    }
    // SAFETY: as above.
    unsafe { *out = l.names[index].as_mut_ptr().cast() };
    true
}
