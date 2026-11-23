// SPDX-License-Identifier: MIT
//! The Camera2 NDK back end (backend_ndk.c): ACameraManager through cameraserver ("media.camera"). The provider HAL
//! does the ISP work, so the pictures arrive finished (YUV_420_888).
//!
//! This is the way with One UI's framework running. In a session it is expected NOT to work (README.md, "Which
//! route"): cameraserver asks system_server's permission_checker service for android.permission.CAMERA for every
//! client outside its own process, waits 10 s for the service and then denies. backend_hal.rs is the way there.
//!
//! Binder threads (README.md, "The first phone run"): everything cameraserver sends back comes as a binder call INTO
//! this process: the device's callbacks (capture started, result, error) and the picture buffers themselves (the
//! AImageReader's buffer queue lives here; cameraserver's request thread calls its dequeueBuffer and queueBuffer). An
//! app has binder threads from zygote. A plain executable has none, and libcamera2ndk starts none: openCamera and
//! createCaptureSession still work (cameraserver's calls back during our own call are served on the calling thread),
//! and then cameraserver's first dequeueBuffer waits for ever. So the pool is started here, before anything.
//!
//! Threads: the NDK's device callbacks arrive on the camera manager's own looper thread, the pictures on the
//! AImageReader's own looper thread; neither is ours to make.
//!
//! The names, the error mapping and the diagnosis at the stop ([`summary`]) are plain functions, tested on a host; the
//! NDK calls are Android's only (`ffi`).
use crate::util::verbose;
use std::sync::atomic::{AtomicU32, Ordering};

// camera_status_t (NdkCameraError.h)
pub const ACAMERA_OK: i32 = 0;
pub const ACAMERA_ERROR_UNKNOWN: i32 = -10000;
pub const ACAMERA_ERROR_INVALID_PARAMETER: i32 = -10001;
pub const ACAMERA_ERROR_CAMERA_DISCONNECTED: i32 = -10002;
pub const ACAMERA_ERROR_NOT_ENOUGH_MEMORY: i32 = -10003;
pub const ACAMERA_ERROR_METADATA_NOT_FOUND: i32 = -10004;
pub const ACAMERA_ERROR_CAMERA_DEVICE: i32 = -10005;
pub const ACAMERA_ERROR_CAMERA_SERVICE: i32 = -10006;
pub const ACAMERA_ERROR_SESSION_CLOSED: i32 = -10007;
pub const ACAMERA_ERROR_INVALID_OPERATION: i32 = -10008;
pub const ACAMERA_ERROR_STREAM_CONFIGURE_FAIL: i32 = -10009;
pub const ACAMERA_ERROR_CAMERA_IN_USE: i32 = -10010;
pub const ACAMERA_ERROR_MAX_CAMERA_IN_USE: i32 = -10011;
pub const ACAMERA_ERROR_CAMERA_DISABLED: i32 = -10012;
pub const ACAMERA_ERROR_PERMISSION_DENIED: i32 = -10013;
pub const ACAMERA_ERROR_UNSUPPORTED_OPERATION: i32 = -10014;
// ACameraDevice_StateCallbacks.onError's codes (NdkCameraDevice.h)
pub const ERROR_CAMERA_IN_USE: i32 = 1;
pub const ERROR_MAX_CAMERAS_IN_USE: i32 = 2;
pub const ERROR_CAMERA_DISABLED: i32 = 3;
pub const ERROR_CAMERA_DEVICE: i32 = 4;
pub const ERROR_CAMERA_SERVICE: i32 = 5;
pub const TEMPLATE_PREVIEW: i32 = 1;
pub const CAPTURE_FAILURE_REASON_FLUSHED: i32 = 0;
// Tags meta.rs does not have (NdkCameraMetadataTags.h)
pub const ACAMERA_SENSOR_TIMESTAMP: u32 = 0xe0010;
pub const ACAMERA_CONTROL_ZOOM_RATIO: u32 = 0x1002f;
pub const AIMAGE_FORMAT_YUV_420_888: i32 = 0x23;
pub const AHARDWAREBUFFER_USAGE_CPU_READ_OFTEN: u64 = 3;

/// A camera_status_t's name.
pub fn status_name(status: i32) -> &'static str {
    match status {
        ACAMERA_OK => "ok",
        ACAMERA_ERROR_UNKNOWN => "ERROR_UNKNOWN",
        ACAMERA_ERROR_INVALID_PARAMETER => "ERROR_INVALID_PARAMETER",
        ACAMERA_ERROR_CAMERA_DISCONNECTED => "ERROR_CAMERA_DISCONNECTED",
        ACAMERA_ERROR_NOT_ENOUGH_MEMORY => "ERROR_NOT_ENOUGH_MEMORY",
        ACAMERA_ERROR_METADATA_NOT_FOUND => "ERROR_METADATA_NOT_FOUND",
        ACAMERA_ERROR_CAMERA_DEVICE => "ERROR_CAMERA_DEVICE",
        ACAMERA_ERROR_CAMERA_SERVICE => "ERROR_CAMERA_SERVICE",
        ACAMERA_ERROR_SESSION_CLOSED => "ERROR_SESSION_CLOSED",
        ACAMERA_ERROR_INVALID_OPERATION => "ERROR_INVALID_OPERATION",
        ACAMERA_ERROR_STREAM_CONFIGURE_FAIL => "ERROR_STREAM_CONFIGURE_FAIL",
        ACAMERA_ERROR_CAMERA_IN_USE => "ERROR_CAMERA_IN_USE",
        ACAMERA_ERROR_MAX_CAMERA_IN_USE => "ERROR_MAX_CAMERA_IN_USE",
        ACAMERA_ERROR_CAMERA_DISABLED => "ERROR_CAMERA_DISABLED",
        ACAMERA_ERROR_PERMISSION_DENIED => "ERROR_PERMISSION_DENIED",
        ACAMERA_ERROR_UNSUPPORTED_OPERATION => "ERROR_UNSUPPORTED_OPERATION",
        _ => "?",
    }
}

/// ACameraDevice_StateCallbacks.onError's codes, with what cameraserver means by each.
pub fn device_error_name(error: i32) -> &'static str {
    match error {
        ERROR_CAMERA_IN_USE => "ERROR_CAMERA_IN_USE (another client has it)",
        ERROR_MAX_CAMERAS_IN_USE => "ERROR_MAX_CAMERAS_IN_USE",
        ERROR_CAMERA_DISABLED => "ERROR_CAMERA_DISABLED (a policy took it: device policy, the camera privacy switch, or this uid counted as idle)",
        ERROR_CAMERA_DEVICE => "ERROR_CAMERA_DEVICE (the camera device failed: the HAL, or a stream's buffers)",
        ERROR_CAMERA_SERVICE => "ERROR_CAMERA_SERVICE (cameraserver failed or died)",
        _ => "?",
    }
}

/// The errno of a camera_status_t.
pub fn error_of(status: i32) -> i32 {
    match status {
        ACAMERA_OK => 0,
        ACAMERA_ERROR_PERMISSION_DENIED => libc::EACCES,
        ACAMERA_ERROR_CAMERA_IN_USE | ACAMERA_ERROR_MAX_CAMERA_IN_USE => libc::EBUSY,
        ACAMERA_ERROR_CAMERA_DISABLED => libc::EPERM,
        ACAMERA_ERROR_CAMERA_DISCONNECTED => libc::ENODEV,
        ACAMERA_ERROR_INVALID_PARAMETER => libc::EINVAL,
        _ => libc::EIO,
    }
}

/// The errno a device error ends the stream with.
pub fn device_error_errno(error: i32) -> i32 {
    match error {
        ERROR_CAMERA_DISABLED => libc::EPERM,
        ERROR_CAMERA_IN_USE | ERROR_MAX_CAMERAS_IN_USE => libc::EBUSY,
        _ => libc::EIO,
    }
}

/// Whether the n-th callback of a kind is printed: all with -vv, the first three and every 30th with -v.
pub fn shown(n: u32) -> bool {
    let v = verbose();
    v >= 2 || (v == 1 && (n <= 3 || n % 30 == 0))
}

/// What came back since the start (printed at the stop and by the callbacks).
#[derive(Debug)]
pub struct Counters {
    pub active: AtomicU32,
    pub ready: AtomicU32,
    pub closed: AtomicU32,
    pub started: AtomicU32,
    pub completed: AtomicU32,
    pub failed: AtomicU32,
    pub lost: AtomicU32,
    pub sequences: AtomicU32,
    pub images: AtomicU32,
    pub bad_images: AtomicU32,
    pub device_errors: AtomicU32,
}

/// The counters' values at one moment.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Counts {
    pub active: u32,
    pub ready: u32,
    pub closed: u32,
    pub started: u32,
    pub completed: u32,
    pub failed: u32,
    pub lost: u32,
    pub images: u32,
    pub bad_images: u32,
    pub device_errors: u32,
}

impl Counters {
    pub const fn new() -> Counters {
        Counters {
            active: AtomicU32::new(0),
            ready: AtomicU32::new(0),
            closed: AtomicU32::new(0),
            started: AtomicU32::new(0),
            completed: AtomicU32::new(0),
            failed: AtomicU32::new(0),
            lost: AtomicU32::new(0),
            sequences: AtomicU32::new(0),
            images: AtomicU32::new(0),
            bad_images: AtomicU32::new(0),
            device_errors: AtomicU32::new(0),
        }
    }

    /// One more; the new count.
    pub fn bump(counter: &AtomicU32) -> u32 {
        counter.fetch_add(1, Ordering::Relaxed) + 1
    }

    pub fn reset(&self) {
        for c in [
            &self.active,
            &self.ready,
            &self.closed,
            &self.started,
            &self.completed,
            &self.failed,
            &self.lost,
            &self.sequences,
            &self.images,
            &self.bad_images,
            &self.device_errors,
        ] {
            c.store(0, Ordering::Relaxed);
        }
    }

    pub fn counts(&self) -> Counts {
        let r = |c: &AtomicU32| c.load(Ordering::Relaxed);
        Counts {
            active: r(&self.active),
            ready: r(&self.ready),
            closed: r(&self.closed),
            started: r(&self.started),
            completed: r(&self.completed),
            failed: r(&self.failed),
            lost: r(&self.lost),
            images: r(&self.images),
            bad_images: r(&self.bad_images),
            device_errors: r(&self.device_errors),
        }
    }
}

impl Default for Counters {
    fn default() -> Counters {
        Counters::new()
    }
}

/// Where the stream got to, from the counters (`summary`): the counters' line, and when no picture came the line that
/// says where it stopped. Nothing when pictures came and `verbose` is 0.
pub fn summary(c: &Counts, verbose: u8) -> Vec<String> {
    if c.images != 0 && verbose == 0 {
        return Vec::new();
    }
    let mut lines = vec![format!(
        "ndk: callbacks: session active {}, ready {}, closed {}; captures started {}, completed {}, failed {}; buffers lost {}; device errors {}; images {} (unreadable {})",
        c.active, c.ready, c.closed, c.started, c.completed, c.failed, c.lost, c.device_errors, c.images, c.bad_images
    )];
    if c.images != 0 {
        return lines;
    }
    let at = if c.device_errors != 0 {
        "the device error above"
    } else if c.started == 0 && c.failed == 0 && c.active == 0 {
        "no callback at all after setRepeatingRequest. cameraserver never began the request, or its calls into this process do not arrive (binder threads); see `dumpsys media.camera` and logcat (README.md, \"When no frame arrives\")"
    } else if c.started == 0 && c.failed == 0 {
        "the session went active but the sensor started no capture (the HAL holds the request)"
    } else if c.failed != 0 || c.lost != 0 {
        "captures fail or lose their buffer (the lines above have the reason)"
    } else if c.started != 0 && c.completed == 0 {
        "captures start but no result and no picture comes back (the HAL, or the buffer queue)"
    } else {
        "results arrive without pictures: the reader's buffer queue (format, usage or size the HAL does not fill)"
    };
    lines.push(format!("ndk: stopped at: {at}"));
    lines
}

/// camctl --zoom: a zoom ratio put into the NDK request (0 = none). On a logical camera cameraserver then picks the
/// lens; a second way to the telephotos when a physical stream is refused.
static ZOOM: AtomicU32 = AtomicU32::new(0);

pub fn set_zoom(ratio: f32) {
    ZOOM.store(ratio.to_bits(), Ordering::Relaxed);
}

pub fn zoom() -> f32 {
    f32::from_bits(ZOOM.load(Ordering::Relaxed))
}

#[cfg(target_os = "android")]
pub use ffi::NdkBackend;

#[cfg(target_os = "android")]
mod ffi {
    use super::*;
    use crate::cam_log;
    use crate::convert::Yuv;
    use crate::core::{Backend, FrameSink};
    use crate::meta::{CamInfo, Characteristics, Entry};
    use crate::util::strerror;
    use std::ffi::{c_char, c_int, c_void, CStr, CString};
    use std::ptr::null_mut;
    use std::sync::{Arc, Mutex, PoisonError};

    macro_rules! opaque {
        ($($name:ident),*) => { $( #[repr(C)] pub struct $name { _p: [u8; 0] } )* };
    }
    opaque!(
        ACameraManager,
        ACameraMetadata,
        ACameraDevice,
        ACaptureSessionOutputContainer,
        ACaptureSessionOutput,
        ACameraOutputTarget,
        ACaptureRequest,
        ACameraCaptureSession,
        AImageReader,
        AImage,
        ANativeWindow
    );

    #[repr(C)]
    struct ACameraIdList {
        num_cameras: c_int,
        camera_ids: *const *const c_char,
    }

    #[repr(C)]
    struct ConstEntry {
        tag: u32,
        ty: u8,
        count: u32,
        data: *const c_void,
    }

    #[repr(C)]
    struct DeviceCallbacks {
        context: *mut c_void,
        on_disconnected: extern "C" fn(*mut c_void, *mut ACameraDevice),
        on_error: extern "C" fn(*mut c_void, *mut ACameraDevice, c_int),
    }

    #[repr(C)]
    struct SessionCallbacks {
        context: *mut c_void,
        on_closed: extern "C" fn(*mut c_void, *mut ACameraCaptureSession),
        on_ready: extern "C" fn(*mut c_void, *mut ACameraCaptureSession),
        on_active: extern "C" fn(*mut c_void, *mut ACameraCaptureSession),
    }

    #[repr(C)]
    pub struct CaptureFailure {
        frame_number: i64,
        reason: c_int,
        sequence_id: c_int,
        was_image_captured: bool,
    }

    type ResultFn =
        extern "C" fn(*mut c_void, *mut ACameraCaptureSession, *mut ACaptureRequest, *const ACameraMetadata);

    #[repr(C)]
    struct CaptureCallbacks {
        context: *mut c_void,
        on_capture_started: extern "C" fn(*mut c_void, *mut ACameraCaptureSession, *const ACaptureRequest, i64),
        on_capture_progressed: Option<ResultFn>,
        on_capture_completed: ResultFn,
        on_capture_failed:
            extern "C" fn(*mut c_void, *mut ACameraCaptureSession, *mut ACaptureRequest, *mut CaptureFailure),
        on_capture_sequence_completed: extern "C" fn(*mut c_void, *mut ACameraCaptureSession, c_int, i64),
        on_capture_sequence_aborted: extern "C" fn(*mut c_void, *mut ACameraCaptureSession, c_int),
        on_capture_buffer_lost:
            extern "C" fn(*mut c_void, *mut ACameraCaptureSession, *mut ACaptureRequest, *mut ANativeWindow, i64),
    }

    #[repr(C)]
    struct ImageListener {
        context: *mut c_void,
        on_image_available: extern "C" fn(*mut c_void, *mut AImageReader),
    }

    extern "C" {
        fn ACameraManager_create() -> *mut ACameraManager;
        fn ACameraManager_getCameraIdList(m: *mut ACameraManager, list: *mut *mut ACameraIdList) -> i32;
        fn ACameraManager_deleteCameraIdList(list: *mut ACameraIdList);
        fn ACameraManager_getCameraCharacteristics(
            m: *mut ACameraManager,
            id: *const c_char,
            out: *mut *mut ACameraMetadata,
        ) -> i32;
        fn ACameraManager_openCamera(
            m: *mut ACameraManager,
            id: *const c_char,
            cb: *const DeviceCallbacks,
            dev: *mut *mut ACameraDevice,
        ) -> i32;
        fn ACameraMetadata_free(m: *mut ACameraMetadata);
        fn ACameraMetadata_getConstEntry(m: *const ACameraMetadata, tag: u32, e: *mut ConstEntry) -> i32;
        fn ACameraMetadata_isLogicalMultiCamera(
            m: *const ACameraMetadata,
            n: *mut usize,
            ids: *mut *const *const c_char,
        ) -> bool;
        fn ACameraDevice_close(d: *mut ACameraDevice) -> i32;
        fn ACameraDevice_getId(d: *const ACameraDevice) -> *const c_char;
        fn ACameraDevice_createCaptureRequest(
            d: *const ACameraDevice,
            template: c_int,
            r: *mut *mut ACaptureRequest,
        ) -> i32;
        fn ACameraDevice_createCaptureSession(
            d: *mut ACameraDevice,
            outputs: *const ACaptureSessionOutputContainer,
            cb: *const SessionCallbacks,
            s: *mut *mut ACameraCaptureSession,
        ) -> i32;
        fn ACaptureSessionOutputContainer_create(c: *mut *mut ACaptureSessionOutputContainer) -> i32;
        fn ACaptureSessionOutputContainer_free(c: *mut ACaptureSessionOutputContainer);
        fn ACaptureSessionOutputContainer_add(
            c: *mut ACaptureSessionOutputContainer,
            o: *const ACaptureSessionOutput,
        ) -> i32;
        fn ACaptureSessionOutput_create(w: *mut ANativeWindow, o: *mut *mut ACaptureSessionOutput) -> i32;
        fn ACaptureSessionPhysicalOutput_create(
            w: *mut ANativeWindow,
            id: *const c_char,
            o: *mut *mut ACaptureSessionOutput,
        ) -> i32;
        fn ACaptureSessionOutput_free(o: *mut ACaptureSessionOutput);
        fn ACameraOutputTarget_create(w: *mut ANativeWindow, t: *mut *mut ACameraOutputTarget) -> i32;
        fn ACameraOutputTarget_free(t: *mut ACameraOutputTarget);
        fn ACaptureRequest_addTarget(r: *mut ACaptureRequest, t: *const ACameraOutputTarget) -> i32;
        fn ACaptureRequest_setEntry_i32(r: *mut ACaptureRequest, tag: u32, n: u32, data: *const i32) -> i32;
        fn ACaptureRequest_setEntry_float(r: *mut ACaptureRequest, tag: u32, n: u32, data: *const f32) -> i32;
        fn ACaptureRequest_free(r: *mut ACaptureRequest);
        fn ACameraCaptureSession_setRepeatingRequest(
            s: *mut ACameraCaptureSession,
            cb: *const CaptureCallbacks,
            n: c_int,
            requests: *mut *mut ACaptureRequest,
            sequence: *mut c_int,
        ) -> i32;
        fn ACameraCaptureSession_stopRepeating(s: *mut ACameraCaptureSession) -> i32;
        fn ACameraCaptureSession_close(s: *mut ACameraCaptureSession);
        fn AImageReader_newWithUsage(
            w: i32,
            h: i32,
            format: i32,
            usage: u64,
            max: i32,
            r: *mut *mut AImageReader,
        ) -> i32;
        fn AImageReader_setImageListener(r: *mut AImageReader, l: *const ImageListener) -> i32;
        fn AImageReader_getWindow(r: *mut AImageReader, w: *mut *mut ANativeWindow) -> i32;
        fn AImageReader_delete(r: *mut AImageReader);
        fn AImageReader_acquireLatestImage(r: *mut AImageReader, i: *mut *mut AImage) -> i32;
        fn AImage_getPlaneData(i: *const AImage, plane: c_int, data: *mut *mut u8, len: *mut c_int) -> i32;
        fn AImage_getPlaneRowStride(i: *const AImage, plane: c_int, stride: *mut i32) -> i32;
        fn AImage_getPlanePixelStride(i: *const AImage, plane: c_int, stride: *mut i32) -> i32;
        fn AImage_getWidth(i: *const AImage, w: *mut i32) -> i32;
        fn AImage_getHeight(i: *const AImage, h: *mut i32) -> i32;
        fn AImage_getTimestamp(i: *const AImage, t: *mut i64) -> i32;
        fn AImage_getFormat(i: *const AImage, f: *mut i32) -> i32;
        fn AImage_delete(i: *mut AImage);
    }

    /// What the callbacks share with the back end (C's static `ndk`): the stream's sink while it runs, the counters.
    struct Shared {
        /// Some while running: from the stop on no frame is handed on
        sink: Mutex<Option<Arc<dyn FrameSink>>>,
        counters: Counters,
    }

    static SHARED: Shared = Shared { sink: Mutex::new(None), counters: Counters::new() };

    fn sink() -> std::sync::MutexGuard<'static, Option<Arc<dyn FrameSink>>> {
        SHARED.sink.lock().unwrap_or_else(PoisonError::into_inner)
    }

    fn counters() -> &'static Counters {
        &SHARED.counters
    }

    /// A camera's characteristics as ACameraMetadata gives them. The buffer-management tag is not asked for: the
    /// NDK's cameras never said hal-buffers in the C (only the HAL's metadata did).
    struct Meta(*const ACameraMetadata);

    impl Characteristics for Meta {
        fn entry(&self, tag: u32) -> Option<Entry<'_>> {
            if tag == crate::meta::CAM_TAG_BUFFER_MANAGEMENT {
                return None;
            }
            let mut e = ConstEntry { tag: 0, ty: 0, count: 0, data: std::ptr::null() };
            // SAFETY: a live metadata object and an entry to fill.
            if unsafe { ACameraMetadata_getConstEntry(self.0, tag, &mut e) } != ACAMERA_OK
                || e.data.is_null()
                || e.ty > 5
            {
                return None;
            }
            let size = [1usize, 4, 4, 8, 8, 8][e.ty as usize] * e.count as usize;
            // SAFETY: the entry's values, `count` of its type, owned by the metadata while it lives.
            let data = unsafe { std::slice::from_raw_parts(e.data.cast::<u8>(), size) };
            Some(Entry { data, count: e.count, ty: e.ty })
        }
    }

    fn info(id: &str, parent: &str, meta: *const ACameraMetadata) -> CamInfo {
        let mut c = CamInfo::named(id, parent);
        c.read(&Meta(meta));
        c
    }

    fn device_id(device: *mut ACameraDevice) -> String {
        // SAFETY: the NDK's id of a live device, a C string.
        unsafe { CStr::from_ptr(ACameraDevice_getId(device)) }.to_string_lossy().into_owned()
    }

    fn report(error: i32) {
        let s = sink().clone();
        if let Some(s) = s {
            s.error(error);
        }
    }

    // ---- the callbacks: every one is counted, and printed (session and device ones always) -------------------
    extern "C" fn on_image(_: *mut c_void, reader: *mut AImageReader) {
        let c = counters();
        let mut image: *mut AImage = null_mut();
        // SAFETY: the reader the NDK called us for; the image is deleted below.
        let got = unsafe { AImageReader_acquireLatestImage(reader, &mut image) };
        if got != 0 || image.is_null() {
            if shown(Counters::bump(&c.bad_images)) || c.bad_images.load(Ordering::Relaxed) <= 3 {
                cam_log!("ndk: image available, but acquireLatestImage: media status {got}");
            }
            return;
        }
        let (mut y, mut cb, mut cr) = (null_mut(), null_mut(), null_mut());
        let (mut ylen, mut cblen, mut crlen, mut step) = (0, 0, 0, 0);
        let (mut width, mut height, mut y_stride, mut c_stride, mut format, mut time) = (0, 0, 0, 0, 0, 0i64);
        // SAFETY: getters of a live image into locals.
        let ok = unsafe {
            AImage_getPlaneData(image, 0, &mut y, &mut ylen) == 0
                && AImage_getPlaneData(image, 1, &mut cb, &mut cblen) == 0
                && AImage_getPlaneData(image, 2, &mut cr, &mut crlen) == 0
                && AImage_getPlaneRowStride(image, 0, &mut y_stride) == 0
                && AImage_getPlaneRowStride(image, 1, &mut c_stride) == 0
                && AImage_getPlanePixelStride(image, 1, &mut step) == 0
                && AImage_getWidth(image, &mut width) == 0
                && AImage_getHeight(image, &mut height) == 0
        };
        // SAFETY: as above.
        unsafe {
            AImage_getTimestamp(image, &mut time);
            AImage_getFormat(image, &mut format);
        }
        let n = Counters::bump(&c.images);
        if shown(n) {
            cam_log!(
                "ndk: image {n}: {width}x{height} format {format:#x}, rows {y_stride}/{c_stride} apart, chroma step {step}, time {time} ns{}",
                if ok { "" } else { " (its planes could not be read)" }
            );
        }
        let (w, h) = (width.max(0) as usize, height.max(0) as usize);
        let (ys, cs, step_u) = (y_stride.max(0) as usize, c_stride.max(0) as usize, step.max(0) as usize);
        let (luma, chroma) = Yuv::spans(ys, cs, step_u, w, h);
        let fits = luma <= ylen as usize && chroma <= cblen as usize && chroma <= crlen as usize;
        if ok && fits {
            // SAFETY: the planes are the image's, at least as long as the geometry needs (checked), alive until
            // AImage_delete below.
            let picture = unsafe { Yuv::from_raw(y, cb, cr, ys, cs, step_u, w, h) };
            let s = sink();
            if let Some(s) = s.as_ref() {
                s.frame(&picture, time as u64);
            }
        } else if Counters::bump(&c.bad_images) <= 3 {
            cam_log!("ndk: an image's planes could not be read ({width}x{height} format {format:#x})");
        }
        // SAFETY: the image acquired above, deleted once.
        unsafe { AImage_delete(image) };
    }

    extern "C" fn on_disconnected(_: *mut c_void, device: *mut ACameraDevice) {
        cam_log!(
            "ndk: device {}: disconnected (cameraserver took the camera away: another client with a higher priority, a policy, or cameraserver restarted)",
            device_id(device)
        );
        report(libc::ENODEV);
    }

    extern "C" fn on_device_error(_: *mut c_void, device: *mut ACameraDevice, error: c_int) {
        Counters::bump(&counters().device_errors);
        cam_log!("ndk: device {}: error {error} {}", device_id(device), device_error_name(error));
        report(device_error_errno(error));
    }

    extern "C" fn on_session_closed(_: *mut c_void, _: *mut ACameraCaptureSession) {
        Counters::bump(&counters().closed);
        if verbose() != 0 {
            cam_log!("ndk: session: closed");
        }
    }

    extern "C" fn on_session_ready(_: *mut c_void, _: *mut ACameraCaptureSession) {
        let n = Counters::bump(&counters().ready);
        if verbose() != 0 {
            cam_log!("ndk: session: ready (idle: no request is being processed){}", if n == 1 { "" } else { " again" });
        }
    }

    extern "C" fn on_session_active(_: *mut c_void, _: *mut ACameraCaptureSession) {
        Counters::bump(&counters().active);
        if verbose() != 0 {
            cam_log!("ndk: session: active (cameraserver began processing requests)");
        }
    }

    extern "C" fn on_capture_started(
        _: *mut c_void,
        _: *mut ACameraCaptureSession,
        _: *const ACaptureRequest,
        timestamp: i64,
    ) {
        let n = Counters::bump(&counters().started);
        if shown(n) {
            cam_log!("ndk: capture {n} started: sensor time {timestamp} ns");
        }
    }

    extern "C" fn on_capture_completed(
        _: *mut c_void,
        _: *mut ACameraCaptureSession,
        _: *mut ACaptureRequest,
        result: *const ACameraMetadata,
    ) {
        let n = Counters::bump(&counters().completed);
        if !shown(n) {
            return;
        }
        let time = Meta(result)
            .entry(ACAMERA_SENSOR_TIMESTAMP)
            .filter(|e| e.count >= 1)
            .map_or(-1, |e| i64::from_ne_bytes(e.data[..8].try_into().unwrap()));
        cam_log!("ndk: capture {n} completed: result's sensor time {time} ns");
    }

    extern "C" fn on_capture_failed(
        _: *mut c_void,
        _: *mut ACameraCaptureSession,
        _: *mut ACaptureRequest,
        failure: *mut CaptureFailure,
    ) {
        let n = Counters::bump(&counters().failed);
        if n <= 5 || shown(n) {
            // SAFETY: the NDK's failure description for the call.
            let f = unsafe { &*failure };
            cam_log!(
                "ndk: capture failed ({n}): frame {}, reason {} {}, sequence {}, picture {}",
                f.frame_number,
                f.reason,
                if f.reason == CAPTURE_FAILURE_REASON_FLUSHED {
                    "FLUSHED (dropped by an abort or a stop)"
                } else {
                    "ERROR (the camera or cameraserver dropped the frame)"
                },
                f.sequence_id,
                if f.was_image_captured { "was made" } else { "was not made" }
            );
        }
    }

    extern "C" fn on_sequence_completed(_: *mut c_void, _: *mut ACameraCaptureSession, sequence: c_int, frame: i64) {
        Counters::bump(&counters().sequences);
        if verbose() != 0 {
            cam_log!("ndk: capture sequence {sequence} ended at frame {frame}");
        }
    }

    extern "C" fn on_sequence_aborted(_: *mut c_void, _: *mut ACameraCaptureSession, sequence: c_int) {
        Counters::bump(&counters().sequences);
        cam_log!("ndk: capture sequence {sequence} aborted before any frame");
    }

    extern "C" fn on_buffer_lost(
        _: *mut c_void,
        _: *mut ACameraCaptureSession,
        _: *mut ACaptureRequest,
        _: *mut ANativeWindow,
        frame: i64,
    ) {
        let n = Counters::bump(&counters().lost);
        if n <= 5 || shown(n) {
            cam_log!("ndk: picture buffer lost ({n}): frame {frame} (the camera made no picture for our reader)");
        }
    }

    /// The NDK's objects of one stream; the camera manager lives as long as the back end.
    pub struct NdkBackend {
        manager: *mut ACameraManager,
        device: *mut ACameraDevice,
        reader: *mut AImageReader,
        outputs: *mut ACaptureSessionOutputContainer,
        output: *mut ACaptureSessionOutput,
        target: *mut ACameraOutputTarget,
        request: *mut ACaptureRequest,
        session: *mut ACameraCaptureSession,
    }

    // SAFETY: the NDK's camera objects may be used from any thread; the back end is used by one thread at a time
    // (the helper's main loop, camctl's).
    unsafe impl Send for NdkBackend {}

    impl Default for NdkBackend {
        fn default() -> NdkBackend {
            NdkBackend {
                manager: null_mut(),
                device: null_mut(),
                reader: null_mut(),
                outputs: null_mut(),
                output: null_mut(),
                target: null_mut(),
                request: null_mut(),
                session: null_mut(),
            }
        }
    }

    /// Where a start failed: the step, the camera status and the media status.
    struct Failed(&'static str, i32, i32);

    fn cstring(text: &str) -> CString {
        CString::new(text).unwrap_or_default()
    }

    impl NdkBackend {
        fn init(&mut self) -> Result<(), i32> {
            match crate::android::binder_threads() {
                Err(e) => cam_log!(
                    "ndk: no binder threads ({}): cameraserver's callbacks and picture buffers cannot arrive",
                    strerror(e)
                ),
                Ok(()) if verbose() != 0 => {
                    cam_log!(
                        "ndk: binder threads started (cameraserver's callbacks and buffer queue calls arrive on them)"
                    )
                }
                Ok(()) => {}
            }
            if self.manager.is_null() {
                // SAFETY: a plain NDK constructor.
                self.manager = unsafe { ACameraManager_create() };
            }
            if self.manager.is_null() {
                Err(libc::EIO)
            } else {
                Ok(())
            }
        }

        fn characteristics(&self, id: &CStr) -> Result<*mut ACameraMetadata, i32> {
            let mut meta = null_mut();
            // SAFETY: our manager and a C string; the metadata is freed by the caller.
            let status = unsafe { ACameraManager_getCameraCharacteristics(self.manager, id.as_ptr(), &mut meta) };
            if status != ACAMERA_OK || meta.is_null() {
                Err(status)
            } else {
                Ok(meta)
            }
        }

        fn teardown(&mut self) {
            // SAFETY: each object is ours, freed once and set to null.
            unsafe {
                if !self.session.is_null() {
                    let status = ACameraCaptureSession_stopRepeating(self.session);
                    if status != ACAMERA_OK && verbose() != 0 {
                        cam_log!("ndk: stopRepeating: status {status} {}", status_name(status));
                    }
                    ACameraCaptureSession_close(self.session);
                    self.session = null_mut();
                }
                if !self.request.is_null() {
                    ACaptureRequest_free(self.request);
                    self.request = null_mut();
                }
                if !self.target.is_null() {
                    ACameraOutputTarget_free(self.target);
                    self.target = null_mut();
                }
                if !self.device.is_null() {
                    let status = ACameraDevice_close(self.device);
                    if status != ACAMERA_OK {
                        cam_log!("ndk: closing the camera: status {status} {}", status_name(status));
                    }
                    self.device = null_mut();
                }
                if !self.outputs.is_null() {
                    ACaptureSessionOutputContainer_free(self.outputs);
                    self.outputs = null_mut();
                }
                if !self.output.is_null() {
                    ACaptureSessionOutput_free(self.output);
                    self.output = null_mut();
                }
                if !self.reader.is_null() {
                    AImageReader_delete(self.reader);
                    self.reader = null_mut();
                }
            }
        }

        /// The steps of a start (`ndk_start` up to its `failed:` label).
        fn open_stream(
            &mut self,
            camera: &CamInfo,
            width: i32,
            height: i32,
            range: [i32; 2],
            stream: Arc<dyn FrameSink>,
        ) -> Result<(), Failed> {
            static DEVICE: DeviceCallbacks =
                DeviceCallbacks { context: null_mut(), on_disconnected, on_error: on_device_error };
            static SESSION: SessionCallbacks = SessionCallbacks {
                context: null_mut(),
                on_closed: on_session_closed,
                on_ready: on_session_ready,
                on_active: on_session_active,
            };
            static CAPTURE: CaptureCallbacks = CaptureCallbacks {
                context: null_mut(),
                on_capture_started,
                on_capture_progressed: None,
                on_capture_completed,
                on_capture_failed,
                on_capture_sequence_completed: on_sequence_completed,
                on_capture_sequence_aborted: on_sequence_aborted,
                on_capture_buffer_lost: on_buffer_lost,
            };
            let physical = !camera.parent.is_empty();
            // A physical camera behind a logical one: the logical camera is opened, the stream is the physical one's.
            let open_id = if physical { &camera.parent } else { &camera.id };
            let (open_c, id_c) = (cstring(open_id), cstring(&camera.id));
            let check = |step: &'static str, status: i32| {
                if status == ACAMERA_OK {
                    Ok(())
                } else {
                    Err(Failed(step, status, 0))
                }
            };
            let media = |step: &'static str, status: i32| {
                if status == 0 {
                    Ok(())
                } else {
                    Err(Failed(step, ACAMERA_ERROR_UNKNOWN, status))
                }
            };
            // SAFETY: NDK calls on objects this back end owns (teardown frees them); the callback tables are statics.
            unsafe {
                check(
                    "openCamera",
                    ACameraManager_openCamera(self.manager, open_c.as_ptr(), &DEVICE, &mut self.device),
                )?;
                if verbose() != 0 {
                    let of = if physical { " for its physical camera " } else { "" };
                    cam_log!("ndk: camera {open_id} opened{of}{}", if physical { camera.id.as_str() } else { "" });
                }
                // The reader: YUV_420_888 that the CPU reads (the usage the buffer queue tells cameraserver), 4 deep.
                let usage = AHARDWAREBUFFER_USAGE_CPU_READ_OFTEN;
                let step = "AImageReader_newWithUsage";
                media(
                    step,
                    AImageReader_newWithUsage(width, height, AIMAGE_FORMAT_YUV_420_888, usage, 4, &mut self.reader),
                )?;
                let listener = ImageListener { context: null_mut(), on_image_available: on_image };
                media("AImageReader_setImageListener", AImageReader_setImageListener(self.reader, &listener))?;
                let mut window = null_mut();
                media("AImageReader_getWindow", AImageReader_getWindow(self.reader, &mut window))?;
                if window.is_null() {
                    return Err(Failed("AImageReader_getWindow", ACAMERA_ERROR_UNKNOWN, 0));
                }
                let step =
                    if physical { "ACaptureSessionPhysicalOutput_create" } else { "ACaptureSessionOutput_create" };
                check(step, ACaptureSessionOutputContainer_create(&mut self.outputs))?;
                let status = if physical {
                    ACaptureSessionPhysicalOutput_create(window, id_c.as_ptr(), &mut self.output)
                } else {
                    ACaptureSessionOutput_create(window, &mut self.output)
                };
                check(step, status)?;
                check(step, ACaptureSessionOutputContainer_add(self.outputs, self.output))?;
                // The repeating request: the preview template, its one target the reader's window.
                let step = "createCaptureRequest(TEMPLATE_PREVIEW)";
                check(step, ACameraDevice_createCaptureRequest(self.device, TEMPLATE_PREVIEW, &mut self.request))?;
                check("request target", ACameraOutputTarget_create(window, &mut self.target))?;
                check("request target", ACaptureRequest_addTarget(self.request, self.target))?;
                let status = ACaptureRequest_setEntry_i32(
                    self.request,
                    crate::meta::CAM_TAG_AE_TARGET_FPS_RANGE,
                    2,
                    range.as_ptr(),
                );
                if status != ACAMERA_OK {
                    cam_log!(
                        "ndk: the request did not take the frame rate {}-{}: status {status} {} (the template's rate is used)",
                        range[0],
                        range[1],
                        status_name(status)
                    );
                }
                let zoom = zoom();
                if zoom > 0.0 {
                    let status = ACaptureRequest_setEntry_float(self.request, ACAMERA_CONTROL_ZOOM_RATIO, 1, &zoom);
                    cam_log!("ndk: zoom ratio {:.2} in the request: status {status} {}", zoom, status_name(status));
                }
                *sink() = Some(stream); // running: from here frames and errors are handed on
                check(
                    "createCaptureSession",
                    ACameraDevice_createCaptureSession(self.device, self.outputs, &SESSION, &mut self.session),
                )?;
                if verbose() != 0 {
                    let kind = if physical { "physical" } else { "plain" };
                    cam_log!("ndk: session made: one {kind} YUV_420_888 stream {width}x{height}, CPU-read usage");
                }
                let mut sequence = -1;
                let step = "setRepeatingRequest";
                check(
                    step,
                    ACameraCaptureSession_setRepeatingRequest(
                        self.session,
                        &CAPTURE,
                        1,
                        &mut self.request,
                        &mut sequence,
                    ),
                )?;
                if verbose() != 0 {
                    cam_log!(
                        "ndk: repeating request set: sequence {sequence}, preview template, {}-{} fps",
                        range[0],
                        range[1]
                    );
                }
            }
            Ok(())
        }
    }

    // SAFETY: the callback tables hold function pointers and a null context only.
    unsafe impl Sync for DeviceCallbacks {}
    // SAFETY: as above.
    unsafe impl Sync for SessionCallbacks {}
    // SAFETY: as above.
    unsafe impl Sync for CaptureCallbacks {}

    impl Backend for NdkBackend {
        fn name(&self) -> &'static str {
            "ndk"
        }

        /// The cameras cameraserver lists, then the physical cameras behind each logical one that are not in the
        /// list themselves (the S24 Ultra's 3x and 5x sit behind logical camera 0): those are opened through their
        /// logical camera with a physical stream.
        fn list(&mut self, max: usize) -> Result<Vec<CamInfo>, i32> {
            self.init()?;
            let mut ids: *mut ACameraIdList = null_mut();
            // SAFETY: our manager; the list is deleted below.
            let status = unsafe { ACameraManager_getCameraIdList(self.manager, &mut ids) };
            if status != ACAMERA_OK || ids.is_null() {
                cam_log!("ndk: no camera list from cameraserver: status {status} {}", status_name(status));
                return Err(error_of(status));
            }
            // SAFETY: the NDK's list: num_cameras C strings.
            let names: Vec<CString> = unsafe {
                let l = &*ids;
                (0..l.num_cameras.max(0) as usize).map(|i| CStr::from_ptr(*l.camera_ids.add(i)).to_owned()).collect()
            };
            // SAFETY: the list we got above, deleted once.
            unsafe { ACameraManager_deleteCameraIdList(ids) };
            let mut out: Vec<CamInfo> = Vec::new();
            for name in names.iter().take(max) {
                let id = name.to_string_lossy();
                match self.characteristics(name) {
                    Ok(meta) => {
                        out.push(info(&id, "", meta));
                        // SAFETY: freed once.
                        unsafe { ACameraMetadata_free(meta) };
                    }
                    Err(status) => {
                        cam_log!("ndk: camera {id}: no characteristics: status {status} {}", status_name(status))
                    }
                }
            }
            let listed = out.len();
            for i in 0..listed {
                if !out[i].logical {
                    continue;
                }
                let parent = out[i].id.clone();
                let Ok(meta) = self.characteristics(&cstring(&parent)) else { continue };
                let (mut n, mut member) = (0usize, std::ptr::null());
                // SAFETY: the metadata's member names live while it does (freed after the loop).
                let members: Vec<CString> = unsafe {
                    if !ACameraMetadata_isLogicalMultiCamera(meta, &mut n, &mut member) {
                        n = 0;
                    }
                    (0..n).map(|m| CStr::from_ptr(*member.add(m)).to_owned()).collect()
                };
                if verbose() != 0 {
                    cam_log!("ndk: logical camera {parent} has {} physical cameras", members.len());
                }
                for m in &members {
                    if out.len() >= max {
                        break;
                    }
                    let id = m.to_string_lossy();
                    let known = out.iter().any(|c| c.id == id);
                    if verbose() != 0 {
                        cam_log!("ndk:   physical camera {id}{}", if known { " (listed by itself too)" } else { "" });
                    }
                    if known {
                        continue;
                    }
                    match self.characteristics(m) {
                        Ok(physical) => {
                            out.push(info(&id, &parent, physical));
                            // SAFETY: freed once.
                            unsafe { ACameraMetadata_free(physical) };
                        }
                        Err(status) => cam_log!(
                            "ndk: physical camera {id} of {parent}: no characteristics: status {status} {}",
                            status_name(status)
                        ),
                    }
                }
                // SAFETY: freed once (the member names were the metadata's).
                unsafe { ACameraMetadata_free(meta) };
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
            let result = match self.init() {
                Err(_) => Err(Failed("ACameraManager_create", ACAMERA_ERROR_UNKNOWN, 0)),
                Ok(()) => {
                    counters().reset();
                    self.open_stream(camera, width, height, [fps_lo, fps_hi], sink)
                }
            };
            let Err(Failed(step, status, media)) = result else { return Ok(()) };
            cam_log!(
                "ndk: {step} failed for camera {}: status {status} {}, media status {media}{}",
                camera.id,
                status_name(status),
                if status == ACAMERA_ERROR_PERMISSION_DENIED {
                    " (permission denied: cameraserver could not ask system_server; use the hal back end)"
                } else {
                    ""
                }
            );
            *self::sink() = None;
            self.teardown();
            Err(error_of(status))
        }

        fn stop(&mut self) {
            *sink() = None; // from here no frame is handed on
            for line in summary(&counters().counts(), verbose()) {
                cam_log!("{line}");
            }
            self.teardown();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_and_errnos() {
        assert_eq!(status_name(ACAMERA_ERROR_PERMISSION_DENIED), "ERROR_PERMISSION_DENIED");
        assert_eq!(status_name(-1), "?");
        assert_eq!(error_of(ACAMERA_ERROR_MAX_CAMERA_IN_USE), libc::EBUSY);
        assert_eq!(error_of(ACAMERA_ERROR_STREAM_CONFIGURE_FAIL), libc::EIO);
        assert_eq!(device_error_errno(ERROR_CAMERA_DISABLED), libc::EPERM);
        assert!(device_error_name(ERROR_CAMERA_SERVICE).starts_with("ERROR_CAMERA_SERVICE ("));
    }

    #[test]
    fn where_it_stopped() {
        let none = Counts::default();
        let lines = summary(&none, 0);
        assert_eq!(lines.len(), 2);
        assert!(lines[0].starts_with("ndk: callbacks: session active 0, ready 0, closed 0; captures started 0"));
        assert!(lines[1].starts_with("ndk: stopped at: no callback at all after setRepeatingRequest."));
        let at = |c: Counts| summary(&c, 0).pop().unwrap();
        assert!(at(Counts { active: 1, ..none }).ends_with("the sensor started no capture (the HAL holds the request)"));
        assert!(at(Counts { device_errors: 1, ..none }).ends_with("the device error above"));
        assert!(at(Counts { started: 3, lost: 1, ..none }).contains("captures fail or lose their buffer"));
        assert!(at(Counts { started: 3, ..none }).contains("captures start but no result"));
        assert!(at(Counts { started: 3, completed: 3, ..none }).contains("results arrive without pictures"));
        // Pictures came: nothing, or (verbose) the counters alone.
        assert!(summary(&Counts { images: 5, ..none }, 0).is_empty());
        assert_eq!(summary(&Counts { images: 5, ..none }, 1).len(), 1);
    }

    #[test]
    fn counters() {
        let c = Counters::new();
        assert_eq!(Counters::bump(&c.images), 1);
        assert_eq!(Counters::bump(&c.images), 2);
        assert_eq!(c.counts().images, 2);
        c.reset();
        assert_eq!(c.counts(), Counts::default());
    }
}
