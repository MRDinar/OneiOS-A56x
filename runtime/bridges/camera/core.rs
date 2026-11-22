// SPDX-License-Identifier: MIT
//! The camera helper's core: one back end, one running stream, its frames converted to NV12 straight into the ring.
//! Also the test back end (a moving picture without a camera).
use crate::cam_log;
use crate::convert::{yuv_to_nv12, Yuv};
use crate::meta::{assign_roles, CamInfo, CAM_MAX_LIST};
use crate::proto::{OcFrame, Role, OC_FACING_BACK, OC_FACING_FRONT, OC_FMT_NV12, OC_FRAME_BYTES_MAX, OC_FRAME_MAGIC};
use crate::proto::{OC_MAX_HEIGHT, OC_MAX_WIDTH};
use crate::ring::{RingWriter, CAM_RING_SLOTS};
use crate::util::{monotonic_ns, sleep_until, strerror};
use std::io;
use std::os::fd::OwnedFd;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::thread::JoinHandle;

/// Where a back end's pictures go: the core's stream.
pub trait FrameSink: Send + Sync {
    /// One picture of the stream and the sensor's time for it (a back end's thread).
    fn frame(&self, picture: &Yuv<'_>, timestamp_ns: u64);
    /// The stream ended by itself, with this errno. Called on a back end's thread: it must not call stop().
    fn error(&self, errno: i32);
}

/// A way to the cameras (`cam_backend`).
pub trait Backend: Send {
    fn name(&self) -> &'static str;
    /// The cameras, at most `max` (an errno on failure).
    fn list(&mut self, max: usize) -> Result<Vec<CamInfo>, i32>;
    /// Opens `camera` and streams `width` x `height` at `fps_lo`-`fps_hi` into `sink`.
    fn start(
        &mut self,
        camera: &CamInfo,
        width: i32,
        height: i32,
        fps_lo: i32,
        fps_hi: i32,
        sink: Arc<dyn FrameSink>,
    ) -> Result<(), i32>;
    /// Closes the camera; returns when no frame callback runs any more.
    fn stop(&mut self);
}

/// The back end of a name (`cam_core_init`): "auto" or none (the NDK while system_server runs, else the provider
/// HAL; on a host the test back end), "ndk", "hal", "test". None for another name.
pub fn backend_by_name(name: Option<&str>) -> Option<Box<dyn Backend>> {
    match name.unwrap_or("auto") {
        "auto" => Some(auto_backend()),
        "test" => Some(Box::new(TestBackend::default())),
        #[cfg(target_os = "android")]
        "ndk" => Some(Box::new(crate::backend_ndk::NdkBackend::default())),
        #[cfg(target_os = "android")]
        "hal" => Some(Box::new(crate::backend_hal::HalBackend::default())),
        _ => None,
    }
}

#[cfg(target_os = "android")]
fn auto_backend() -> Box<dyn Backend> {
    // With One UI's framework running, cameraserver's own way (it keeps its book of who has the camera). In a
    // session cameraserver refuses every client (no permission service): the HAL.
    if system_server_running() {
        Box::new(crate::backend_ndk::NdkBackend::default())
    } else {
        Box::new(crate::backend_hal::HalBackend::default())
    }
}

#[cfg(not(target_os = "android"))]
fn auto_backend() -> Box<dyn Backend> {
    Box::new(TestBackend::default())
}

/// Whether One UI's framework runs: a process whose first argument is "system_server" (`cam_system_server_running`).
pub fn system_server_running() -> bool {
    let Ok(proc) = std::fs::read_dir("/proc") else { return false };
    proc.flatten().any(|e| {
        let name = e.file_name();
        let name = name.as_encoded_bytes();
        if !(b'1'..=b'9').contains(&name.first().copied().unwrap_or(0)) {
            return false;
        }
        let Ok(cmdline) = std::fs::read(e.path().join("cmdline")) else { return false };
        // C read it with fgets into 32 bytes and compared up to the first NUL.
        let first = &cmdline[..cmdline.len().min(31)];
        first.split(|&b| b == 0 || b == b'\n').next() == Some(b"system_server".as_slice())
    })
}

// ---- the stream --------------------------------------------------------------------------------------------------
/// What the core tells its owner: a frame was committed (its sequence), the stream ended (an errno).
pub type FrameNote = Box<dyn Fn(u64) + Send + Sync>;
pub type ErrorNote = Box<dyn Fn(i32) + Send + Sync>;

struct StreamState {
    ring: RingWriter,
    running: bool,
    role: Role,
    orientation: u32,
    fps: u32,
    /// of the newest frame; goes on over a stop and start
    sequence: u64,
    /// since the start
    frames: u64,
    /// sensor time of the first and the newest frame
    first_ns: u64,
    last_ns: u64,
}

struct Stream {
    backend: &'static str,
    state: Mutex<StreamState>,
    on_frame: Option<FrameNote>,
    on_error: Option<ErrorNote>,
}

impl Stream {
    fn lock(&self) -> MutexGuard<'_, StreamState> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

impl FrameSink for Stream {
    fn frame(&self, picture: &Yuv<'_>, timestamp_ns: u64) {
        let sequence = {
            let mut s = self.lock();
            let (w, h) = (picture.width, picture.height);
            if !s.running || w == 0 || h == 0 || w & 1 != 0 || h & 1 != 0 {
                return;
            }
            let bytes = w * h * 3 / 2;
            if bytes > s.ring.slot_bytes() as usize {
                return;
            }
            let slot = s.ring.begin();
            yuv_to_nv12(picture, s.ring.picture(slot), w);
            s.sequence += 1;
            let frame = OcFrame {
                magic: OC_FRAME_MAGIC,
                sequence: s.sequence,
                timestamp_ns,
                width: w as u32,
                height: h as u32,
                stride: w as u32,
                format: OC_FMT_NV12,
                bytes: bytes as u32,
                role: s.role as u32,
                orientation: s.orientation,
                fps: s.fps,
                ..Default::default()
            };
            s.ring.commit(slot, &frame);
            s.frames += 1;
            if s.frames == 1 {
                s.first_ns = timestamp_ns;
            }
            s.last_ns = timestamp_ns;
            s.sequence
        };
        if let Some(note) = &self.on_frame {
            note(sequence);
        }
    }

    fn error(&self, errno: i32) {
        cam_log!("{}: the stream ended: {}", self.backend, strerror(errno));
        if let Some(note) = &self.on_error {
            note(errno);
        }
    }
}

/// A back end writing into a ring (`cam_core`).
pub struct Core {
    backend: Box<dyn Backend>,
    stream: Arc<Stream>,
    cameras: Vec<CamInfo>,
    running: bool,
    camera: Option<usize>,
    width: i32,
    height: i32,
    fps: i32,
}

/// What a stream has done since its start.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct StreamStats {
    pub sequence: u64,
    pub frames: u64,
    pub first_ns: u64,
    pub last_ns: u64,
}

impl Core {
    /// The back end of `backend` (see [`backend_by_name`]; EINVAL for another name) and the frame ring
    /// (`cam_core_init`, without the list: [`Core::list`]). `on_frame` runs after each commit, `on_error` when the
    /// stream ended by itself; both on a back end's thread.
    pub fn new(backend: Option<&str>, on_frame: Option<FrameNote>, on_error: Option<ErrorNote>) -> Result<Core, i32> {
        let backend = backend_by_name(backend).ok_or(libc::EINVAL)?;
        Core::with_backend(backend, on_frame, on_error)
    }

    /// The same with a back end of one's own.
    pub fn with_backend(
        backend: Box<dyn Backend>,
        on_frame: Option<FrameNote>,
        on_error: Option<ErrorNote>,
    ) -> Result<Core, i32> {
        let ring = RingWriter::create(CAM_RING_SLOTS, OC_FRAME_BYTES_MAX).map_err(|e| errno_of(&e))?;
        let state = StreamState {
            ring,
            running: false,
            role: Role::None,
            orientation: 0,
            fps: 0,
            sequence: 0,
            frames: 0,
            first_ns: 0,
            last_ns: 0,
        };
        let stream = Arc::new(Stream { backend: backend.name(), state: Mutex::new(state), on_frame, on_error });
        Ok(Core { backend, stream, cameras: Vec::new(), running: false, camera: None, width: 0, height: 0, fps: 0 })
    }

    /// Lists the cameras and gives them their roles. The count, or the back end's errno.
    pub fn list(&mut self) -> Result<usize, i32> {
        let mut cameras = self.backend.list(CAM_MAX_LIST)?;
        cameras.truncate(CAM_MAX_LIST);
        assign_roles(&mut cameras);
        self.cameras = cameras;
        Ok(self.cameras.len())
    }

    pub fn backend_name(&self) -> &'static str {
        self.backend.name()
    }

    pub fn cameras(&self) -> &[CamInfo] {
        &self.cameras
    }

    /// A copy of the ring's descriptor (for xnuhost, or a reader of one's own).
    pub fn ring_fd(&self) -> io::Result<OwnedFd> {
        self.stream.lock().ring.fd().try_clone_to_owned()
    }

    /// A camera by a role name, "id:<id>" or an index in the list (`cam_core_find`).
    pub fn find(&self, which: &str) -> Option<usize> {
        if let Some(id) = which.strip_prefix("id:") {
            return self.cameras.iter().position(|c| c.id == id);
        }
        let role = Role::from_name(which);
        if role != Role::None {
            return self.cameras.iter().position(|c| c.role == role);
        }
        let index: usize = which.parse().ok()?;
        (index < self.cameras.len()).then_some(index)
    }

    /// The first camera of `role`.
    pub fn by_role(&self, role: u32) -> Option<usize> {
        self.cameras.iter().position(|c| c.role as u32 == role)
    }

    /// Starts camera `index` at the size and rate nearest what is asked (`cam_core_start`); fps 0 is 30.
    pub fn start(&mut self, index: usize, width: i32, height: i32, fps: i32) -> Result<(), i32> {
        if self.running {
            self.stop();
        }
        let fps = if fps == 0 { 30 } else { fps };
        let camera = self.cameras[index].clone();
        let Some((w, h)) = camera.pick_size(width, height) else {
            cam_log!("camera {} has no picture size within {}x{}", camera.id, OC_MAX_WIDTH, OC_MAX_HEIGHT);
            return Err(libc::ENOENT);
        };
        let (lo, hi) = camera.pick_fps(fps).unwrap_or((fps, fps));
        (self.camera, self.width, self.height, self.fps) = (Some(index), w, h, hi);
        {
            let mut s = self.stream.lock();
            (s.role, s.orientation, s.fps) = (camera.role, camera.orientation as u32, hi as u32);
            (s.frames, s.first_ns, s.last_ns) = (0, 0, 0);
            s.running = true;
        }
        self.running = true;
        let sink: Arc<dyn FrameSink> = self.stream.clone();
        if let Err(e) = self.backend.start(&camera, w, h, lo, hi, sink) {
            self.stream.lock().running = false;
            self.running = false;
            return Err(e);
        }
        let name = self.backend.name();
        cam_log!("{name}: camera {} ({}) {w}x{h} at {lo}-{hi} fps", camera.id, camera.role.name());
        Ok(())
    }

    /// Closes the camera (`cam_core_stop`); harmless when none runs.
    pub fn stop(&mut self) {
        if !self.running {
            return;
        }
        self.backend.stop(); // returns when no frame callback runs any more
        self.stream.lock().running = false;
        self.running = false;
    }

    pub fn running(&self) -> bool {
        self.running
    }

    /// The running (or last) stream's camera.
    pub fn camera(&self) -> Option<&CamInfo> {
        self.camera.map(|i| &self.cameras[i])
    }

    pub fn width(&self) -> i32 {
        self.width
    }

    pub fn height(&self) -> i32 {
        self.height
    }

    pub fn fps(&self) -> i32 {
        self.fps
    }

    pub fn stats(&self) -> StreamStats {
        let s = self.stream.lock();
        StreamStats { sequence: s.sequence, frames: s.frames, first_ns: s.first_ns, last_ns: s.last_ns }
    }
}

impl Drop for Core {
    fn drop(&mut self) {
        self.stop();
    }
}

/// The errno of an io::Error (EIO when it has none).
pub fn errno_of(e: &io::Error) -> i32 {
    e.raw_os_error().unwrap_or(libc::EIO)
}

// ---- the test back end -------------------------------------------------------------------------------------------
/// Eight bars (white, yellow, cyan, green, magenta, red, blue, black) in Y, Cb, Cr.
const BARS: [[u8; 3]; 8] = [
    [235, 128, 128],
    [210, 16, 146],
    [170, 166, 16],
    [145, 54, 34],
    [106, 202, 222],
    [81, 90, 240],
    [41, 240, 110],
    [16, 128, 128],
];

/// The test back end's picture of frame `n` (`cam_test_picture`): colour bars with a white bar of 8 pixels that
/// moves 8 pixels a frame, planar: `y` width x height, `cb` and `cr` (width/2) x (height/2).
pub fn test_picture(y: &mut [u8], cb: &mut [u8], cr: &mut [u8], width: usize, height: usize, n: u64) {
    let marker = (n * 8 % width as u64) as usize;
    for row in 0..height {
        for x in 0..width {
            let bar = &BARS[x * 8 / width];
            let mark = x >= marker && x < marker + 8;
            y[row * width + x] = if mark { 255 } else { bar[0] };
            if row & 1 == 0 && x & 1 == 0 {
                let at = (row / 2) * (width / 2) + x / 2;
                cb[at] = if mark { 128 } else { bar[1] };
                cr[at] = if mark { 128 } else { bar[2] };
            }
        }
    }
}

/// Five made-up cameras with the S24 Ultra's lenses; the picture is [`test_picture`], so a reader can tell frames
/// apart and check their order.
#[derive(Default)]
pub struct TestBackend {
    run: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
}

impl Backend for TestBackend {
    fn name(&self) -> &'static str {
        "test"
    }

    fn list(&mut self, max: usize) -> Result<Vec<CamInfo>, i32> {
        // id, facing, focal length, sensor size, orientation
        const LENSES: [(&str, i32, f32, f32, f32, i32); 5] = [
            ("0", OC_FACING_BACK, 6.3, 9.83, 7.37, 90),
            ("1", OC_FACING_FRONT, 3.3, 4.5, 3.4, 270),
            ("2", OC_FACING_BACK, 2.2, 5.9, 4.4, 90),
            ("3", OC_FACING_BACK, 7.9, 4.1, 3.1, 90),
            ("4", OC_FACING_BACK, 18.6, 5.8, 4.35, 90),
        ];
        let sizes = vec![(4000, 3000), (1920, 1080), (1440, 1080), (1280, 720), (640, 480), (320, 240)];
        Ok(LENSES
            .iter()
            .take(max)
            .map(|&(id, facing, focal_mm, physical_w, physical_h, orientation)| {
                let mut c = CamInfo {
                    facing,
                    orientation,
                    focal_mm,
                    physical_w,
                    physical_h,
                    pixel_w: 4000,
                    pixel_h: 3000,
                    sizes: sizes.clone(),
                    fps: vec![(15, 30), (30, 30), (60, 60)],
                    ..CamInfo::named(id, "")
                };
                c.finish();
                c
            })
            .collect())
    }

    fn start(
        &mut self,
        _camera: &CamInfo,
        width: i32,
        height: i32,
        _fps_lo: i32,
        fps_hi: i32,
        sink: Arc<dyn FrameSink>,
    ) -> Result<(), i32> {
        let (width, height) = (width as usize, height as usize);
        let fps = if fps_hi > 0 { fps_hi as u64 } else { 30 };
        self.run.store(true, Ordering::Release);
        let run = Arc::clone(&self.run);
        let thread = std::thread::Builder::new().name("camera-test".into()).spawn(move || {
            let luma = width * height;
            let mut planes = vec![0u8; luma * 3 / 2];
            let mut next = monotonic_ns();
            let mut n = 0u64;
            while run.load(Ordering::Acquire) {
                let (y, chroma) = planes.split_at_mut(luma);
                let (cb, cr) = chroma.split_at_mut(luma / 4);
                test_picture(y, cb, cr, width, height, n);
                let picture = Yuv {
                    y: &planes[..luma],
                    cb: &planes[luma..luma + luma / 4],
                    cr: &planes[luma + luma / 4..],
                    y_stride: width,
                    c_stride: width / 2,
                    c_step: 1,
                    width,
                    height,
                };
                sink.frame(&picture, next);
                next += 1_000_000_000 / fps;
                sleep_until(next);
                n += 1;
            }
        });
        match thread {
            Ok(t) => {
                self.thread = Some(t);
                Ok(())
            }
            Err(_) => {
                self.run.store(false, Ordering::Release);
                Err(libc::EAGAIN)
            }
        }
    }

    fn stop(&mut self) {
        self.run.store(false, Ordering::Release);
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

/// True when the picture is the test back end's for some frame number (its bar moves 8 pixels a frame): what
/// the tests check a frame with.
pub fn is_test_picture(f: &OcFrame, nv12: &[u8]) -> bool {
    let (w, h) = (f.width as usize, f.height as usize);
    let luma = w * h;
    let Some(marker) = nv12[..w].iter().position(|&v| v == 255) else { return false };
    if marker % 8 != 0 || f.bytes as usize != luma * 3 / 2 {
        return false;
    }
    let mut want = vec![0u8; luma * 3 / 2];
    let (y, chroma) = want.split_at_mut(luma);
    let (cb, cr) = chroma.split_at_mut(luma / 4);
    test_picture(y, cb, cr, w, h, marker as u64 / 8);
    nv12[..luma] == *y && (0..luma / 4).all(|i| nv12[luma + 2 * i] == cb[i] && nv12[luma + 2 * i + 1] == cr[i])
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ring::RingReader;
    use std::sync::atomic::AtomicU64;

    fn nap(ms: u64) {
        std::thread::sleep(std::time::Duration::from_millis(ms));
    }

    #[test]
    fn core_with_the_test_backend() {
        let notes = Arc::new(AtomicU64::new(0));
        let n = Arc::clone(&notes);
        assert_eq!(Core::new(Some("nonsense"), None, None).err(), Some(libc::EINVAL));
        let mut core = Core::new(Some("test"), Some(Box::new(move |s| n.store(s, Ordering::Release))), None).unwrap();
        assert_eq!(core.list(), Ok(5));
        assert_eq!(core.find("wide"), Some(0));
        assert_eq!(core.find("front"), Some(1));
        assert_eq!(core.find("ultrawide"), Some(2));
        assert_eq!(core.find("tele3"), Some(3));
        assert_eq!(core.find("tele5"), Some(4));
        assert_eq!(core.find("id:2"), Some(2));
        assert_eq!(core.find("3"), Some(3));
        assert!(core.find("9").is_none() && core.find("id:x").is_none() && core.find("").is_none());
        let reader = RingReader::attach(core.ring_fd().unwrap()).unwrap();
        assert!(core.start(core.find("wide").unwrap(), 1280, 720, 60).is_ok());
        assert!(core.width() == 1280 && core.height() == 720 && core.fps() == 60);
        let mut picture = vec![0u8; OC_FRAME_BYTES_MAX as usize];
        let (mut f, mut before) = (OcFrame::default(), OcFrame::default());
        let mut last = 0;
        let (mut got, mut in_order, mut pictures) = (0, 0, 0);
        for _ in 0..3000 {
            if got == 12 {
                break;
            }
            if reader.read(&mut last, &mut f, &mut picture).is_err() {
                nap(1);
                continue;
            }
            got += 1;
            if f.sequence > before.sequence && f.timestamp_ns > before.timestamp_ns {
                in_order += 1;
            }
            if f.width == 1280
                && f.height == 720
                && f.stride == 1280
                && f.format == OC_FMT_NV12
                && f.role == Role::Wide as u32
                && f.orientation == 90
                && f.fps == 60
                && is_test_picture(&f, &picture)
            {
                pictures += 1;
            }
            before = f;
        }
        assert_eq!((got, in_order, pictures), (12, 12, 12));
        // 60 frames a second by the frames' own times.
        let st = core.stats();
        assert!(st.frames >= 12 && st.last_ns > st.first_ns);
        let fps = (st.frames - 1) as f64 * 1e9 / (st.last_ns - st.first_ns) as f64;
        assert!(fps > 59.0 && fps < 61.0, "{fps}");
        core.stop();
        let stopped = core.stats().sequence;
        nap(60);
        assert_eq!(core.stats().sequence, stopped);
        assert_eq!(notes.load(Ordering::Acquire), stopped);
        // Another camera, a size it does not have: the nearest; the numbering goes on. What the ring holds from
        // before the start is passed over, as xnuhost's end does.
        last = reader.newest();
        assert!(core.start(core.find("front").unwrap(), 1000, 750, 30).is_ok());
        assert!(core.width() == 640 && core.height() == 480 && core.fps() == 30);
        for _ in 0..3000 {
            if reader.read(&mut last, &mut f, &mut picture).is_ok() {
                break;
            }
            nap(1);
        }
        assert!(f.sequence > stopped && f.width == 640 && f.height == 480);
        assert!(f.role == Role::Front as u32 && f.orientation == 270 && is_test_picture(&f, &picture));
        core.stop();
        core.stop(); // twice is harmless
    }
}
