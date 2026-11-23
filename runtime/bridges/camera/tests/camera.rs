// SPDX-License-Identifier: MIT
//! Host tests of the camera bridge's processes and device (test_camera.c's test_helper and test_device): the real
//! helper process (built for the host: the test back end) through xnuhost's end of it ([`onecamera::link`]), and the
//! `oneios,camera` virtio device on xh-core's transport against a guest of plain memory. No phone, no Android, no
//! Apple files. (The ring, conversion, PNG, metadata and core tests are the library's own unit tests.)
use onecamera::core::is_test_picture;
use onecamera::link::{Camera, Event};
use onecamera::pod::Pod;
use onecamera::proto::*;
use onecamera::ring::ReadError;
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};
use xh_core::mmio::MmioDevice;
use xh_core::virtio::testing::{self, Driver, GuestRam, RING_BYTES};

const HELPER: &str = env!("CARGO_BIN_EXE_oneios-camera");

fn nap(ms: u64) {
    std::thread::sleep(Duration::from_millis(ms));
}

/// The bridge's events, counted: frames, states, the last state's status.
#[derive(Default)]
struct Events {
    seen: Mutex<(u32, u32, i32)>,
    changed: Condvar,
}

impl Events {
    fn listen(camera: &Camera) -> Arc<Events> {
        let events = Arc::new(Events::default());
        let e = Arc::clone(&events);
        camera.listen(Some(Arc::new(move |event| {
            let mut seen = e.seen.lock().unwrap();
            match event {
                Event::Frame => seen.0 += 1,
                Event::State(status) => {
                    seen.1 += 1;
                    seen.2 = status;
                }
            }
            e.changed.notify_all();
        })));
        events
    }

    /// Waits (5 s at most) until `count` state events came; the last one's status.
    fn state(&self, count: u32) -> Option<i32> {
        let seen = self.seen.lock().unwrap();
        let (seen, _) = self.changed.wait_timeout_while(seen, Duration::from_secs(5), |s| s.1 < count).unwrap();
        (seen.1 >= count).then_some(seen.2)
    }

    fn frames(&self) -> u32 {
        self.seen.lock().unwrap().0
    }

    fn states(&self) -> u32 {
        self.seen.lock().unwrap().1
    }
}

/// Reads frames until `f` is the newest (5 s at most).
fn next_frame(camera: &Camera, f: &mut OcFrame, picture: &mut [u8]) -> bool {
    let until = Instant::now() + Duration::from_secs(5);
    while Instant::now() < until {
        if camera.frame(f, picture).is_ok() {
            return true;
        }
        nap(1);
    }
    false
}

#[test]
fn helper_process() {
    let mut picture = vec![0u8; OC_FRAME_BYTES_MAX as usize];
    let mut f = OcFrame::default();
    assert!(Camera::helper("/nonexistent/oneios-camera", Some("test")).is_none());
    let camera = Camera::helper(HELPER, Some("test")).expect("the helper starts");
    let events = Events::listen(&camera);
    let list = camera.list(OC_MAX_CAMERAS as usize, Duration::from_secs(5));
    assert_eq!(list.len(), 5);
    assert!(list[0].role == OC_ROLE_WIDE && list[1].role == OC_ROLE_FRONT && list[1].facing == OC_FACING_FRONT as u32);
    assert!(list[2].role == OC_ROLE_ULTRAWIDE && list[3].role == OC_ROLE_TELE3 && list[4].role == OC_ROLE_TELE5);
    assert!(list[0].max_width == 1920 && list[0].max_height == 1080 && list[0].max_fps == 60);
    assert_eq!(camera.frame(&mut f, &mut picture), Err(ReadError::Again));
    // Start: the state event, then frames with their notes.
    assert_eq!(camera.start(OC_ROLE_ULTRAWIDE, 640, 480, 60), Ok(()));
    assert_eq!(events.state(1), Some(0));
    let (mut got, mut good, mut sequence) = (0, 0, 0);
    for _ in 0..4000 {
        if got == 20 {
            break;
        }
        if camera.frame(&mut f, &mut picture).is_err() {
            nap(1);
            continue;
        }
        got += 1;
        if f.sequence > sequence
            && f.width == 640
            && f.height == 480
            && f.role == OC_ROLE_ULTRAWIDE
            && f.fps == 60
            && is_test_picture(&f, &picture)
        {
            good += 1;
        }
        sequence = f.sequence;
    }
    assert_eq!((got, good), (20, 20));
    assert!(events.frames() >= 20);
    // Too small a buffer, or nothing new: never a short copy.
    assert!(camera.frame(&mut f, &mut picture[..10]).is_err());
    // Stop: its state event, and no more frames.
    assert_eq!(camera.stop(), Ok(()));
    assert_eq!(events.state(2), Some(0));
    while camera.frame(&mut f, &mut picture).is_ok() {
        sequence = f.sequence;
    }
    nap(80);
    assert_eq!(camera.frame(&mut f, &mut picture), Err(ReadError::Again));
    // A camera that is not there: the error comes as a state event.
    assert_eq!(camera.start(OC_ROLE_OTHER, 640, 480, 30), Ok(()));
    assert_eq!(events.state(3), Some(libc::ENODEV));
    // A start after a stop does not hand out the old run's last frame.
    assert_eq!(camera.start(OC_ROLE_WIDE, 320, 240, 30), Ok(()));
    assert_eq!(events.state(4), Some(0));
    assert!(next_frame(&camera, &mut f, &mut picture));
    assert!(f.sequence > sequence && f.width == 320 && f.role == OC_ROLE_WIDE);
    drop(camera); // with the camera running: the helper stops it and exits

    // A helper that dies: the bridge says so and stays usable (no frames).
    let camera = Camera::helper(HELPER, Some("test")).expect("the helper starts");
    let events = Events::listen(&camera);
    assert_eq!(camera.list(OC_MAX_CAMERAS as usize, Duration::from_secs(5)).len(), 5);
    let states = events.states();
    // SAFETY: kill(2) of our own child.
    assert_eq!(unsafe { libc::kill(camera.pid(), libc::SIGKILL) }, 0);
    assert_eq!(events.state(states + 1), Some(libc::EPIPE));
    assert_eq!(camera.start(OC_ROLE_WIDE, 320, 240, 30), Err(libc::EPIPE));
    assert_eq!(camera.frame(&mut f, &mut picture), Err(ReadError::Again));
}

// ---- the virtio device against a guest of plain memory ----------------------------------------------------------
const QN: u16 = 16;

struct Guest {
    driver: Driver,
    _rings: GuestRam,
    io: GuestRam, // request at 0, reply at 0x100
}

impl Guest {
    fn command(&mut self, request: &OcRequest) -> (Option<u32>, OcReply) {
        self.io.write(0, request.as_bytes());
        self.io.fill(0x100, 0xee, std::mem::size_of::<OcReply>());
        let parts = [(self.io.pa(0), 32, false), (self.io.pa(0x100), std::mem::size_of::<OcReply>() as u32, true)];
        self.driver.post(OC_QUEUE_CONTROL, &parts);
        let len = self.driver.reap(OC_QUEUE_CONTROL).map(|(_, len)| len);
        (len, OcReply::read_from(&self.io.read(0x100, std::mem::size_of::<OcReply>())))
    }

    fn post_frame(&mut self, pa: u64, len: u32) {
        self.driver.post(OC_QUEUE_FRAME, &[(pa, len, true)]);
    }

    /// The next frame buffer the device returned (5 s at most): its guest address and length.
    fn wait_frame(&mut self) -> Option<(u64, u32)> {
        let until = Instant::now() + Duration::from_secs(5);
        while Instant::now() < until {
            if let Some((id, len)) = self.driver.reap(OC_QUEUE_FRAME) {
                return Some((self.driver.desc_addr(OC_QUEUE_FRAME, id % u32::from(QN)), len));
            }
            nap(1);
        }
        None
    }
}

fn frame_at(pa: u64) -> OcFrame {
    let mut f = OcFrame::default();
    // SAFETY: a guest buffer of this test's (identity RAM), at least 64 bytes.
    unsafe { std::ptr::copy_nonoverlapping(pa as *const u8, f.as_bytes_mut().as_mut_ptr(), 64) };
    f
}

fn bytes_at(pa: u64, len: usize) -> &'static [u8] {
    // SAFETY: a guest buffer of this test's (identity RAM) that lives to the end of the test.
    unsafe { std::slice::from_raw_parts(pa as *const u8, len) }
}

#[test]
fn virtio_device() {
    testing::install();
    let camera = Arc::new(Camera::helper(HELPER, Some("test")).expect("the helper starts"));
    assert!(onecamera::virtio_camera::create(0x0a00_0000, 40, None).is_none());
    assert_eq!(camera.list(OC_MAX_CAMERAS as usize, Duration::from_secs(5)).len(), 5);
    let dev = onecamera::virtio_camera::create(0x0a00_0000, 40, Some(Arc::clone(&camera))).expect("the device");
    assert_eq!(dev.mmio_read(0x08, 4), u64::from(OC_VIRTIO_DEVICE_ID));
    assert_eq!(dev.mmio_read(0x100, 4), u64::from(OC_PROTO_VERSION));
    let rings = GuestRam::new(2 * RING_BYTES as usize);
    let driver = Driver::up(dev.clone() as Arc<dyn MmioDevice>, 0, 2, QN, rings.pa(0)).expect("the guest's driver");
    let mut g = Guest { driver, _rings: rings, io: GuestRam::new(0x1000) };
    let buffers = GuestRam::new(3 * OC_BUFFER_BYTES as usize + 0x1000);
    let buffer = |i: usize| buffers.pa(i * OC_BUFFER_BYTES as usize);
    let small = buffers.pa(3 * OC_BUFFER_BYTES as usize);

    let (len, reply) = g.command(&OcRequest { code: OC_CMD_INFO, ..Default::default() });
    assert!(len == Some(8 + 5 * 32) && reply.status == 0 && reply.count == 5);
    assert!(reply.cameras[1].role == OC_ROLE_FRONT && reply.cameras[4].role == OC_ROLE_TELE5);
    assert_eq!(reply.cameras[0].max_width, 1920);
    let (len, reply) = g.command(&OcRequest { code: 99, ..Default::default() });
    assert!(len == Some(8) && reply.status == libc::ENOTSUP as u32);
    let start =
        |role, width, height, fps| OcRequest { code: OC_CMD_START, role, width, height, fps, ..Default::default() };
    let (len, reply) = g.command(&start(OC_ROLE_WIDE, 4000, 3000, 0));
    assert!(len == Some(8) && reply.status == libc::EINVAL as u32);

    // Start with three buffers posted: frames arrive in them, in order, each the test picture.
    for i in 0..3 {
        g.post_frame(buffer(i), OC_BUFFER_BYTES);
    }
    let (len, reply) = g.command(&start(OC_ROLE_TELE3, 1280, 720, 30));
    assert!(len == Some(8) && reply.status == 0);
    let (mut good, mut sequence) = (0, 0);
    for _ in 0..10 {
        let Some((pa, n)) = g.wait_frame() else { break };
        if n < OC_FRAME_DATA {
            break;
        }
        let f = frame_at(pa);
        let picture = bytes_at(pa + u64::from(OC_FRAME_DATA), f.bytes.min(OC_FRAME_BYTES_MAX) as usize);
        if f.magic == OC_FRAME_MAGIC
            && f.status == 0
            && f.sequence > sequence
            && f.width == 1280
            && f.height == 720
            && f.stride == 1280
            && f.format == OC_FMT_NV12
            && f.bytes == 1280 * 720 * 3 / 2
            && n == OC_FRAME_DATA + f.bytes
            && f.role == OC_ROLE_TELE3
            && f.fps == 30
            && f.timestamp_ns != 0
            && is_test_picture(&f, picture)
        {
            good += 1;
        }
        sequence = f.sequence;
        g.post_frame(pa, OC_BUFFER_BYTES); // the guest gives the buffer back
    }
    assert_eq!(good, 10);
    // A buffer too small for a picture comes back with the error, not a cut picture.
    for _ in 0..3 {
        g.wait_frame();
    }
    buffers.fill(3 * OC_BUFFER_BYTES as usize, 0, 100);
    g.post_frame(small, 100);
    assert_eq!(g.wait_frame().map(|w| w.1), Some(OC_FRAME_DATA));
    let f = frame_at(small);
    assert!(f.magic == OC_FRAME_MAGIC && f.status == libc::EMSGSIZE as u32 && f.bytes == 0);
    assert_eq!(bytes_at(small + 64, 1)[0], 0);
    // Stop: buffers posted afterwards stay with the device.
    let (len, reply) = g.command(&OcRequest { code: OC_CMD_STOP, ..Default::default() });
    assert!(len == Some(8) && reply.status == 0);
    nap(150);
    while g.driver.reap(OC_QUEUE_FRAME).is_some() {}
    g.post_frame(buffer(0), OC_BUFFER_BYTES);
    nap(150);
    assert!(g.driver.reap(OC_QUEUE_FRAME).is_none());
    // A camera that is not there: the posted buffer comes back with the errno.
    let (len, reply) = g.command(&start(OC_ROLE_OTHER, 640, 480, 0));
    assert!(len == Some(8) && reply.status == 0);
    assert_eq!(g.wait_frame().map(|w| w.1), Some(OC_FRAME_DATA));
    let f = frame_at(buffer(0));
    assert!(f.status == libc::ENODEV as u32 && f.bytes == 0);
    // A guest that resets the device with the camera on: the camera is stopped.
    g.post_frame(buffer(1), OC_BUFFER_BYTES);
    let (len, reply) = g.command(&start(OC_ROLE_FRONT, 640, 480, 60));
    assert!(len == Some(8) && reply.status == 0);
    assert!(g.wait_frame().is_some_and(|w| w.1 > OC_FRAME_DATA));
    g.driver.write(0x70, 0);
    nap(150);
    let mut none = OcFrame::default();
    let mut picture = vec![0u8; OC_FRAME_BYTES_MAX as usize];
    while camera.frame(&mut none, &mut picture).is_ok() {}
    nap(100);
    assert_eq!(camera.frame(&mut none, &mut picture), Err(ReadError::Again));
    camera.close(); // the device keeps its Arc; the helper goes with its socket
}
