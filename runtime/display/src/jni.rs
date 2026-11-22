// SPDX-License-Identifier: MIT
//! Port of jni.c. liboneios-display.so: the native half of OneiOSDisplay (java/OneiOSDisplay.java). It owns the
//! socket, the mapped frame and the copy into the layer's buffers; the Java half owns the layers.
//!
//! ANativeWindow_setBuffersDataSpace and _get are API 28 and libnativewindow's (linked below: libandroid does not have
//! them; without it the library links but app_process cannot load it, the 2026-11-14 phone session: "cannot locate
//! symbol ANativeWindow_getBuffersDataSpace"); the library is built for 30, so they are there. build.rs links the
//! cdylib with -z defs, so a symbol no named library has is a link error here, not on the phone.
use crate::pixels;
use crate::sys::{self, Errno};
use crate::xhd::{
    self, Buffers, Direct, Frame, Rect, Snapshot, BUFFER_BGRA_8888, BUFFER_RGBA_FP16, BUFFER_RGBX_8888,
    DATASPACE_SCRGB, DATASPACE_SCRGB_LINEAR, DATASPACE_SRGB, DATASPACE_UNKNOWN, DONE_FAILED, DONE_UNCHANGED,
    FORMAT_BGRX, PANEL_DIRECT,
};
use std::ffi::{c_char, c_void, CStr};
use std::os::fd::{AsFd, BorrowedFd, IntoRawFd, OwnedFd};
use std::os::unix::ffi::OsStrExt;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicI32, Ordering};

// ---- JNI's types and the four functions of its table that are called --------------------------------------------

type Jint = i32;
type Jlong = i64;
type Jboolean = u8;
type Jobject = *mut c_void;
type Jstring = Jobject;
type JintArray = Jobject;
/// JNIEnv*: a pointer to the pointer to JNI's function table (struct JNINativeInterface).
type Env = *mut *const *const c_void;
const JNI_TRUE: Jboolean = 1;
const JNI_FALSE: Jboolean = 0;

// The table's slots (jni.h, struct JNINativeInterface: four reserved pointers, then the functions in order).
const GET_STRING_UTF_CHARS: usize = 169;
const RELEASE_STRING_UTF_CHARS: usize = 170;
const NEW_INT_ARRAY: usize = 179;
const SET_INT_ARRAY_REGION: usize = 211;

/// The function in slot `index` of env's table, as type F.
///
/// # Safety
/// env is the JNIEnv the VM passed to this call, and F is the slot's C signature.
unsafe fn jni_fn<F: Copy>(env: Env, index: usize) -> F {
    // SAFETY: (by the caller) env points at the VM's table, which has this slot; a function pointer and F (an extern
    // "C" fn pointer) have the same size.
    unsafe { std::mem::transmute_copy(&*(*env).add(index)) }
}

/// A Java string's (modified) UTF-8, copied out; None for null or no memory.
fn java_string(env: Env, s: Jstring) -> Option<Vec<u8>> {
    if s.is_null() {
        return None;
    }
    type Get = unsafe extern "C" fn(Env, Jstring, *mut Jboolean) -> *const c_char;
    type Release = unsafe extern "C" fn(Env, Jstring, *const c_char);
    // SAFETY: env is the VM's, s a live string; the chars are copied before they are released.
    unsafe {
        let chars = jni_fn::<Get>(env, GET_STRING_UTF_CHARS)(env, s, std::ptr::null_mut());
        if chars.is_null() {
            return None;
        }
        let out = CStr::from_ptr(chars).to_bytes().to_vec();
        jni_fn::<Release>(env, RELEASE_STRING_UTF_CHARS)(env, s, chars);
        Some(out)
    }
}

/// A new int[] holding values (null when the VM has no memory: it has thrown then).
fn int_array(env: Env, values: &[Jint]) -> JintArray {
    type New = unsafe extern "C" fn(Env, Jint) -> JintArray;
    type Set = unsafe extern "C" fn(Env, JintArray, Jint, Jint, *const Jint);
    // SAFETY: env is the VM's; the region written is the new array's whole length.
    unsafe {
        let array = jni_fn::<New>(env, NEW_INT_ARRAY)(env, values.len() as Jint);
        if !array.is_null() {
            jni_fn::<Set>(env, SET_INT_ARRAY_REGION)(env, array, 0, values.len() as Jint, values.as_ptr());
        }
        array
    }
}

// ---- the NDK's native window -------------------------------------------------------------------------------------

#[repr(C)]
struct ANativeWindow {
    _opaque: [u8; 0],
}

#[repr(C)]
struct ANativeWindowBuffer {
    width: i32,
    height: i32,
    stride: i32,
    format: i32,
    bits: *mut c_void,
    reserved: [u32; 6],
}

#[repr(C)]
struct ARect {
    left: i32,
    top: i32,
    right: i32,
    bottom: i32,
}

#[link(name = "android")]
extern "C" {
    fn ANativeWindow_fromSurface(env: Env, surface: Jobject) -> *mut ANativeWindow;
    fn ANativeWindow_release(window: *mut ANativeWindow);
    fn ANativeWindow_setBuffersGeometry(window: *mut ANativeWindow, width: i32, height: i32, format: i32) -> i32;
    fn ANativeWindow_lock(window: *mut ANativeWindow, buffer: *mut ANativeWindowBuffer, dirty: *mut ARect) -> i32;
    fn ANativeWindow_unlockAndPost(window: *mut ANativeWindow) -> i32;
}

#[link(name = "nativewindow")]
extern "C" {
    fn ANativeWindow_setBuffersDataSpace(window: *mut ANativeWindow, dataspace: i32) -> i32;
    fn ANativeWindow_getBuffersDataSpace(window: *mut ANativeWindow) -> i32;
}

/// The layer's window, released when dropped.
struct Window(*mut ANativeWindow);

/// A locked buffer of the window.
struct Locked {
    width: i32,
    height: i32,
    stride: i32,
    format: i32,
    bits: *mut u8,
}

impl Window {
    fn from_surface(env: Env, surface: Jobject) -> Option<Window> {
        // SAFETY: env is the VM's and surface the Java half's android.view.Surface.
        let w = unsafe { ANativeWindow_fromSurface(env, surface) };
        (!w.is_null()).then_some(Window(w))
    }

    fn set_geometry(&self, width: u32, height: u32, format: i32) {
        // SAFETY: self.0 is a live window (released only in drop).
        unsafe { ANativeWindow_setBuffersGeometry(self.0, width as i32, height as i32, format) };
    }

    fn set_dataspace(&self, dataspace: i32) -> i32 {
        // SAFETY: as above.
        unsafe { ANativeWindow_setBuffersDataSpace(self.0, dataspace) }
    }

    fn dataspace(&self) -> i32 {
        // SAFETY: as above.
        unsafe { ANativeWindow_getBuffersDataSpace(self.0) }
    }

    /// Locks a buffer whole (dirty: the rectangle asked for). The error is the NDK's (negative) number.
    fn lock(&self, width: u32, height: u32) -> Result<Locked, i32> {
        let mut dirty = ARect { left: 0, top: 0, right: width as i32, bottom: height as i32 };
        let mut b = ANativeWindowBuffer {
            width: 0,
            height: 0,
            stride: 0,
            format: 0,
            bits: std::ptr::null_mut(),
            reserved: [0; 6],
        };
        // SAFETY: a live window; b and dirty are written by the call.
        let error = unsafe { ANativeWindow_lock(self.0, &mut b, &mut dirty) };
        if error != 0 {
            return Err(error);
        }
        Ok(Locked { width: b.width, height: b.height, stride: b.stride, format: b.format, bits: b.bits as *mut u8 })
    }

    fn unlock_and_post(&self) {
        // SAFETY: a live window with a locked buffer.
        unsafe { ANativeWindow_unlockAndPost(self.0) };
    }
}

impl Drop for Window {
    fn drop(&mut self) {
        // SAFETY: the reference fromSurface gave, released once.
        unsafe { ANativeWindow_release(self.0) };
    }
}

// ---- the signals ------------------------------------------------------------------------------------------------

// SIGTERM, SIGINT and SIGHUP end the run loop through a pipe, so the Java half removes the layers.
static SIGNAL_READ: AtomicI32 = AtomicI32::new(-1);
static SIGNAL_WRITE: AtomicI32 = AtomicI32::new(-1);
static STOPPING: AtomicBool = AtomicBool::new(false);

extern "C" fn on_signal(number: libc::c_int) {
    // SAFETY: errno is saved and restored around a write(2), all async-signal-safe.
    unsafe {
        let errno = libc::__errno();
        let saved = *errno;
        STOPPING.store(true, Ordering::SeqCst);
        let byte = number as u8;
        let fd = SIGNAL_WRITE.load(Ordering::SeqCst);
        if fd >= 0 {
            let _ = libc::write(fd, &byte as *const u8 as *const c_void, 1);
        }
        *errno = saved;
    }
}

/// The pipe and the handlers, once (a pipe that cannot be made: no handlers, as in C).
fn install_signals() {
    if SIGNAL_READ.load(Ordering::SeqCst) >= 0 {
        return;
    }
    let Ok((r, w)) = sys::pipe() else { return };
    // The pipe lives as long as the process: app_process loads this library once.
    SIGNAL_WRITE.store(w.into_raw_fd(), Ordering::SeqCst);
    SIGNAL_READ.store(r.into_raw_fd(), Ordering::SeqCst);
    // SAFETY: a zeroed sigaction with a handler that only does async-signal-safe things.
    unsafe {
        let mut action: libc::sigaction = std::mem::zeroed();
        action.sa_sigaction = on_signal as extern "C" fn(libc::c_int) as libc::sighandler_t;
        action.sa_flags = libc::SA_RESTART;
        libc::sigemptyset(&mut action.sa_mask);
        for signal in [libc::SIGTERM, libc::SIGINT, libc::SIGHUP] {
            libc::sigaction(signal, &action, std::ptr::null_mut());
        }
    }
}

fn stopping() -> bool {
    STOPPING.load(Ordering::SeqCst)
}

// ---- the helper's state, the Java half's handle -----------------------------------------------------------------

struct Context {
    fd: OwnedFd,
    frame: Option<Frame>,
    direct: Direct,
}

/// The handle's context.
///
/// # Safety
/// handle is one nOpen returned and nClose has not freed.
unsafe fn context<'a>(handle: Jlong) -> &'a mut Context {
    // SAFETY: (by the caller) a live Box<Context> leaked by nOpen; the Java half calls one native at a time.
    unsafe { &mut *(handle as usize as *mut Context) }
}

#[no_mangle]
pub extern "C" fn Java_OneiOSDisplay_nOpen(env: Env, _type: Jobject, socket_path: Jstring, wait_ms: Jint) -> Jlong {
    install_signals();
    let Some(path) = java_string(env, socket_path) else { return 0 };
    let path = PathBuf::from(std::ffi::OsStr::from_bytes(&path));
    match xhd::connect(&path, wait_ms, &stopping) {
        Ok(fd) => Box::into_raw(Box::new(Context { fd, frame: None, direct: Direct::new() })) as usize as Jlong,
        Err(e) => {
            let why = if stopping() { "stopped while waiting".to_string() } else { e.to_string() };
            eprintln!("oneios-display: {}: {why}", path.display());
            0
        }
    }
}

/// Reports the panel's mode, then takes xnuhost's hello. JNI_FALSE: no usable hello (nClose is still the caller's).
#[no_mangle]
pub extern "C" fn Java_OneiOSDisplay_nHello(
    _env: Env,
    _type: Jobject,
    handle: Jlong,
    width: Jint,
    height: Jint,
    hz: Jint,
    direct: Jboolean,
) -> Jboolean {
    // SAFETY: the Java half passes the handle nOpen gave it.
    let c = unsafe { context(handle) };
    let flags = if direct != 0 { PANEL_DIRECT } else { 0 };
    if let Err(e) = xhd::send_panel(c.fd.as_fd(), width as u32, height as u32, hz as u32, flags) {
        eprintln!("oneios-display: the panel's mode could not be sent: {e}");
    }
    match xhd::recv_hello(c.fd.as_fd(), 10000) {
        Ok(frame) => {
            c.frame = Some(frame);
            JNI_TRUE
        }
        Err(e) => {
            eprintln!("oneios-display: no usable hello from xnuhost: {e}");
            c.frame = None;
            JNI_FALSE
        }
    }
}

#[no_mangle]
pub extern "C" fn Java_OneiOSDisplay_nInfo(env: Env, _type: Jobject, handle: Jlong) -> JintArray {
    // SAFETY: the Java half passes the handle nOpen gave it.
    let c = unsafe { context(handle) };
    let v =
        c.frame.as_ref().map_or([0; 4], |f| [f.width as Jint, f.height as Jint, f.stride as Jint, f.format as Jint]);
    int_array(env, &v)
}

#[no_mangle]
pub extern "C" fn Java_OneiOSDisplay_nFit(
    env: Env,
    _type: Jobject,
    sw: Jint,
    sh: Jint,
    dw: Jint,
    dh: Jint,
) -> JintArray {
    let f = xhd::fit(sw as u32, sh as u32, dw as u32, dh as u32);
    int_array(env, &[f.x as Jint, f.y as Jint, f.w as Jint, f.h as Jint])
}

/// The buffer layout and dataspace a frame format gets (xhd::buffer_for): {buffer, dataspace}.
#[no_mangle]
pub extern "C" fn Java_OneiOSDisplay_nBuffer(env: Env, _type: Jobject, format: Jint) -> JintArray {
    let (buffer, dataspace) = xhd::buffer_for(format as u32).unwrap_or((0, DATASPACE_UNKNOWN));
    int_array(env, &[buffer, dataspace])
}

#[no_mangle]
pub extern "C" fn Java_OneiOSDisplay_nClose(_env: Env, _type: Jobject, handle: Jlong) {
    if handle == 0 {
        return;
    }
    // SAFETY: the handle nOpen leaked, freed once: the socket, the frame and guest RAM's maps are closed with it.
    drop(unsafe { Box::from_raw(handle as usize as *mut Context) });
}

// ---- the run loop -----------------------------------------------------------------------------------------------

/// printf's %#x: 0 is "0", anything else 0x and its hex digits.
fn alt_hex(v: u32) -> String {
    if v == 0 {
        "0".into()
    } else {
        format!("{v:#x}")
    }
}

fn format_name(format: i32) -> &'static str {
    match format {
        BUFFER_BGRA_8888 => "BGRA_8888",
        BUFFER_RGBX_8888 => "RGBX_8888",
        BUFFER_RGBA_FP16 => "RGBA_FP16",
        _ => "other",
    }
}

/// Tags the window's buffers with the dataspace: what the numbers in them mean, so that surfaceflinger turns them into
/// the panel's colours (One UI's colour mode decides how; nothing of it is changed here). Logs what the window has.
fn set_dataspace(window: &Window, dataspace: i32) {
    let error = window.set_dataspace(dataspace);
    let now = window.dataspace();
    if error != 0 {
        eprintln!(
            "oneios-display: the dataspace {} ({}) was refused ({error}); the buffers say {} ({})",
            xhd::dataspace_name(dataspace),
            alt_hex(dataspace as u32),
            xhd::dataspace_name(now),
            alt_hex(now as u32)
        );
    } else {
        eprintln!("oneios-display: buffers tagged {} (dataspace {})", xhd::dataspace_name(now), alt_hex(now as u32));
    }
}

/// The 10 s statistics.
#[derive(Default)]
struct Stats {
    posted: u64,
    direct: u64,
    unchanged: u64,
    reads: u64,
    delays: u64,
    lock_us: i64,
    copy_us: i64,
    copy_max: i64,
    rows_changed: i64,
    rows_filled: i64,
    read_us: i64,
    read_max: i64,
    delay_us: i64,
    delay_max: i64,
    since: i64,
    cpu: i64,
}

impl Stats {
    fn new(since: i64, cpu: i64) -> Stats {
        Stats { since, cpu, ..Stats::default() }
    }

    /// Reported when a frame comes and 10 s have passed since the last report: no timer, so nothing at all when idle.
    fn report(&mut self, frame_height: u32, buffers: &Buffers) {
        let now = sys::now_us();
        if now - self.since < 10_000_000 {
            return;
        }
        let cpu = sys::cpu_us();
        let seen = self.posted + self.unchanged;
        let mean = |sum: i64, n: u64| if n > 0 { sum as f64 / 1000.0 / n as f64 } else { 0.0 };
        let delay = if self.delays > 0 {
            format!(
                "from {:.1} ms after xnuhost sent the frame (mean; max {:.1} ms)",
                self.delay_us as f64 / 1000.0 / self.delays as f64,
                self.delay_max as f64 / 1000.0
            )
        } else {
            "the send times not known (an xnuhost before 2026-11-22)".to_string()
        };
        let share =
            |rows: i64, n: u64| if n > 0 { rows as f64 * 100.0 / (n as f64 * frame_height as f64) } else { 0.0 };
        eprintln!(
            "oneios-display: {:.0} s: {seen} frames from xnuhost, {} unchanged (nothing posted), {} posted ({} from guest \
             RAM, {:.0}% of the rows changed, {:.0}% filled); guest pages read in one pass for mean {:.1} ms, max {:.1} \
             ms, {delay}; buffer wait mean {:.1} ms; copy mean {:.1} ms, max {:.1} ms; {} buffers seen ({} forgotten); \
             this process {:.0}% of a core",
            (now - self.since) as f64 / 1e6,
            self.unchanged,
            self.posted,
            self.direct,
            share(self.rows_changed, self.reads),
            share(self.rows_filled, self.posted),
            mean(self.read_us, self.reads),
            self.read_max as f64 / 1000.0,
            mean(self.lock_us, self.posted),
            mean(self.copy_us, self.posted),
            self.copy_max as f64 / 1000.0,
            buffers.len(),
            buffers.forgotten,
            (cpu - self.cpu) as f64 * 100.0 / (now - self.since) as f64
        );
        *self = Stats::new(now, cpu);
    }
}

/// How the run loop ended.
#[derive(Clone, Copy, PartialEq, Eq)]
enum End {
    Closed = 0,
    Stopped = 1,
    LockFailed = -2,
    PollFailed = -3,
}

/// The direct frame is answered: done (flags), and no longer pending.
fn answer(fd: BorrowedFd<'_>, direct: &mut Direct, flags: u32) {
    if let Some(p) = direct.pending.take() {
        let _ = xhd::send_done(fd, p.seq, flags);
    }
}

/// Posts frames until the socket closes (0) or a signal asks to stop (1); negative on an error.
/// The picture the layer is to show is the helper's snapshot for 8-bit frames (xhd::snapshot): a direct frame
/// (xnuhost's "frame") is read into it in one pass as soon as it comes, and xnuhost's own pixels (the memfd's flushes)
/// are put over it. Unchanged, the frame is answered at once and nothing is locked or posted, so surfaceflinger has
/// nothing to compose and the panel's refresh rate can drop by itself. Changed, the layer's buffer is locked whole and
/// the helper fills what that buffer lacks from the snapshot (xhd::buffers: Surface's own copy-back loses changes once
/// the queue has three buffers or more). The answer ("done") goes after the post: xnuhost then answers the guest's
/// flush, and the guest's display driver posts its next frame only after that answer (its frame in flight lands first:
/// DroidVMVirtioGpuDisplay.cpp). Half-float frames have no snapshot: the memfd is the picture, and a direct frame is
/// refused (xnuhost then copies the frames itself).
///
/// Pacing: the next buffer is only handed out when surfaceflinger has taken the last one, so ANativeWindow_lock blocks
/// to the panel's vsync, and every flush that came meanwhile is one post.
///
/// ready_path: SESSION/ready (--session), or null (--socket): the first post waits for it (xhd::hold_first). Until the
/// first post the layer has no buffer and the OneOS boot screen under it shows.
#[no_mangle]
pub extern "C" fn Java_OneiOSDisplay_nRun(
    env: Env,
    _type: Jobject,
    handle: Jlong,
    surface: Jobject,
    format: Jint,
    dataspace: Jint,
    ready_path: Jstring,
) -> Jint {
    // SAFETY: the Java half passes the handle nOpen gave it.
    let c = unsafe { context(handle) };
    let Some(f) = c.frame.as_ref() else { return -1 };
    let Some(window) = Window::from_surface(env, surface) else {
        eprintln!("oneios-display: no native window for the layer");
        return -1;
    };
    let ready = java_string(env, ready_path).map(|p| PathBuf::from(std::ffi::OsStr::from_bytes(&p)));
    let end = run(c.fd.as_fd(), f, &mut c.direct, &window, format, dataspace, ready);
    drop(window);
    end as Jint
}

fn run(
    fd: BorrowedFd<'_>,
    f: &Frame,
    direct: &mut Direct,
    window: &Window,
    mut format: i32,
    mut dataspace: i32,
    ready: Option<PathBuf>,
) -> End {
    window.set_geometry(f.width, f.height, format);
    set_dataspace(window, dataspace);
    let ready_name = ready.as_ref().map_or(String::new(), |p| p.display().to_string());
    let whole = Rect::whole(f.width, f.height);
    let mut pending = whole;
    // The picture of 8-bit frames (18 MB at 1440x3120, and its tiles' hashes), made before the first post.
    let mut snapshot = if f.format == FORMAT_BGRX { Snapshot::new(f.width, f.height).ok() } else { None };
    if f.format == FORMAT_BGRX && snapshot.is_none() {
        eprintln!(
            "oneios-display: no memory for the snapshot: direct frames are refused (xnuhost copies them), the memfd is \
             posted"
        );
    }
    let mut buffers = Buffers::new(f.width, f.height);
    let mut unposted = Rect::EMPTY; // in the snapshot, not in the layer yet
    let (mut frames, mut direct_frames, mut refused, mut held_flushes) = (0u64, 0u64, 0u64, 0u64);
    let (mut announced, mut held) = (false, false);
    let mut hold_start = 0i64;
    let mut st = Stats::new(sys::now_us(), sys::cpu_us());
    let signal = SIGNAL_READ.load(Ordering::SeqCst);
    // SAFETY: the signal pipe's read end is open for the whole process (install_signals); with no pipe (-1) only the
    // socket is polled.
    let signal_fd = (signal >= 0).then(|| unsafe { BorrowedFd::borrow_raw(signal) });
    let result = loop {
        // Held: the guest's frame is not shown yet, the damage adds up, and `ready` is looked for at every flush and
        // every HOLD_POLL_MS between them (a guest that drew its frame with content and then nothing more).
        let hold = frames == 0 && xhd::hold_first(ready.as_deref());
        if hold && !held {
            held = true;
            hold_start = sys::now_us();
            eprintln!(
                "oneios-display: the first frame waits for {ready_name} (xnuhost's first frame of the guest's own with \
                 content); the boot screen stays"
            );
        }
        let timeout = if hold {
            xhd::HOLD_POLL_MS
        } else if pending.w != 0 || direct.pending.is_some() {
            0
        } else {
            -1
        };
        let mut watched = vec![(fd, libc::POLLIN)];
        watched.extend(signal_fd.map(|s| (s, libc::POLLIN)));
        let (ready_count, revents) = match sys::poll(&watched, timeout, false) {
            Ok((n, r)) => (n as i32, r),
            Err(Errno(libc::EINTR)) => (-1, vec![0, 0]),
            Err(_) => break End::PollFailed,
        };
        if stopping() || (ready_count > 0 && revents.get(1).is_some_and(|&r| r != 0)) {
            break End::Stopped;
        }
        if !xhd::drain_direct(fd, f.width, f.height, &mut pending, Some(direct)) {
            break End::Closed;
        }
        if hold {
            if ready_count > 0 {
                held_flushes += 1;
            }
            continue;
        }
        if pending.w == 0 && direct.pending.is_none() {
            continue;
        }
        // What this post changes against the last one: xnuhost's own damage, the direct frame's changed tiles, and what
        // an earlier round put into the snapshot without posting it.
        let mut changed = std::mem::take(&mut unposted);
        if pending.w != 0 {
            match snapshot.as_mut() {
                Some(s) => {
                    // Until the snapshot is whole the memfd is the whole picture. Put first: a direct frame of the same
                    // round is the guest's scanout, newer than xnuhost's copy, and the put tiles have no hash, so the
                    // read below takes them.
                    let put = if s.full { pending } else { whole };
                    let _ = s.put(&f.view(), put);
                    changed.union(&put);
                }
                None => changed.union(&pending),
            }
        }
        if let Some(p) = direct.pending {
            // The guest's pages, read once and at once (the time since xnuhost sent the frame is logged: a late start
            // is as bad as a long read).
            let t0 = sys::now_us();
            if p.sent_us != 0 {
                let delay = (t0 as u32).wrapping_sub(p.sent_us);
                if delay < 10_000_000 {
                    st.delays += 1;
                    st.delay_us += delay as i64;
                    st.delay_max = st.delay_max.max(delay as i64);
                }
            }
            let (got, error) = match snapshot.as_mut() {
                Some(s) => s.update(direct),
                None => (Rect::EMPTY, Err(Errno(libc::ENOMEM))),
            };
            let read = sys::now_us() - t0;
            st.reads += 1;
            st.read_us += read;
            st.read_max = st.read_max.max(read);
            match error {
                Err(e) => {
                    // Refused: xnuhost copies the frames itself from now on (they come as flushes of the memfd). What
                    // was read is in the snapshot and goes to the layer with the next post that has xnuhost's whole
                    // frame.
                    refused += 1;
                    if refused == 1 {
                        eprintln!("oneios-display: a direct frame could not be read from guest RAM ({e})");
                    }
                    answer(fd, direct, DONE_FAILED);
                    unposted.union(&got);
                }
                Ok(()) => {
                    changed.union(&got);
                    st.rows_changed += got.h as i64;
                }
            }
        }
        if changed.w == 0 && frames > 0 {
            if direct.pending.is_some() {
                answer(fd, direct, DONE_UNCHANGED); // the layer shows it already
                st.unchanged += 1;
            }
            pending = Rect::EMPTY;
            st.report(f.height, &buffers);
            continue;
        }
        // Locked whole: Surface then copies nothing back from the last buffer (xhd::buffers), and the helper fills
        // what this buffer lacks itself.
        let t0 = sys::now_us();
        let locked = window.lock(f.width, f.height);
        let t1 = sys::now_us();
        let buffer = match locked {
            Ok(b) => b,
            Err(error) if frames == 0 && format != BUFFER_RGBX_8888 => {
                // No B,G,R,A buffers here: R,G,B,X and a swap. No RGBA_FP16 ones: B,G,R,A, the half floats encoded to
                // sRGB and clamped (what is outside sRGB is lost), tagged sRGB.
                let next = if format == BUFFER_RGBA_FP16 { BUFFER_BGRA_8888 } else { BUFFER_RGBX_8888 };
                eprintln!(
                    "oneios-display: no buffer in format {format} {} ({error}), using {}",
                    format_name(format),
                    format_name(next)
                );
                format = next;
                window.set_geometry(f.width, f.height, format);
                if dataspace == DATASPACE_SCRGB_LINEAR || dataspace == DATASPACE_SCRGB {
                    dataspace = DATASPACE_SRGB;
                    set_dataspace(window, dataspace);
                }
                buffers.reset(f.width, f.height); // new buffers
                unposted = changed; // the next round posts it
                continue;
            }
            Err(error) => {
                eprintln!("oneios-display: the layer's buffer could not be locked ({error})");
                answer(fd, direct, DONE_FAILED);
                break End::LockFailed;
            }
        };
        let width = (buffer.width as u32).min(f.width);
        let height = (buffer.height as u32).min(f.height);
        let usable = buffer.width > 0 && buffer.height > 0 && buffer.stride >= buffer.width && !buffer.bits.is_null();
        // Only a buffer of the frame's own size is filled to the last pixel and so known afterwards.
        let exact = usable && buffer.width as u32 == f.width && buffer.height as u32 == f.height;
        let bits = buffer.bits as usize;
        // What this buffer lacks and what this post changes (the first four buffers: whole, as a buffer not seen yet
        // is).
        let mut fill = if frames < 4 { whole } else { buffers.fill(bits, &changed) };
        // The buffer's own format decides the copy (RGBA_8888 and RGBX_8888 are both R,G,B bytes).
        let layout = if buffer.format == BUFFER_BGRA_8888 || buffer.format == BUFFER_RGBA_FP16 {
            buffer.format
        } else {
            BUFFER_RGBX_8888
        };
        let bytes = if layout == BUFFER_RGBA_FP16 { 8 } else { 4 };
        if usable && fill.clip(width, height) {
            let dst_stride = buffer.stride as usize * bytes;
            // SAFETY: the locked buffer is stride x height pixels of `bytes` each, ours until unlockAndPost.
            let dst = unsafe { std::slice::from_raw_parts_mut(buffer.bits, dst_stride * buffer.height as usize) };
            let _ = match snapshot.as_ref() {
                Some(s) => xhd::copy(dst, dst_stride, layout, s.pixels(), s.stride, FORMAT_BGRX, fill),
                None => xhd::copy(dst, dst_stride, layout, f.pixels(), f.stride as usize, f.format, fill),
            };
            st.rows_filled += fill.h as i64;
        }
        let t2 = sys::now_us();
        window.unlock_and_post();
        if exact {
            buffers.posted(bits, &changed);
        } else {
            buffers.reset(f.width, f.height); // none of them is known then
        }
        if direct.pending.is_some() {
            answer(fd, direct, 0);
            direct_frames += 1;
            st.direct += 1;
        }
        if frames == 0 {
            let waited = if held {
                format!(
                    "; held {:.1} s for session/ready, {held_flushes} wakes for flushes not shown",
                    (sys::now_us() - hold_start) as f64 / 1e6
                )
            } else {
                String::new()
            };
            let now = window.dataspace();
            eprintln!(
                "oneios-display: first frame posted ({}x{} buffer, stride {}, format {} {}, dataspace {} {}; the guest's \
                 frame: {}{waited})",
                buffer.width,
                buffer.height,
                buffer.stride,
                buffer.format,
                format_name(buffer.format),
                alt_hex(now as u32),
                xhd::dataspace_name(now),
                pixels::name(f.format)
            );
        }
        frames += 1;
        if direct_frames > 0 && !announced {
            announced = true;
            eprintln!(
                "oneios-display: frames come straight from guest RAM now, read once into the snapshot; each buffer gets \
                 what it lacks"
            );
        }
        pending = Rect::EMPTY;
        st.posted += 1;
        st.lock_us += t1 - t0;
        st.copy_us += t2 - t1;
        st.copy_max = st.copy_max.max(t2 - t1);
        st.report(f.height, &buffers);
    };
    drop(snapshot);
    let what = match result {
        End::Closed => "the socket closed",
        End::Stopped => "asked to stop",
        _ => "error",
    };
    eprintln!("oneios-display: {what} after {frames} frames");
    result
}
