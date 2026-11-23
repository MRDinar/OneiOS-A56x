// SPDX-License-Identifier: MIT
//! "oneios,camera": the virtio-mmio device that hands the camera helper's frames to the guest driver (drivers/camera,
//! protocol in [`crate::proto`]). Queue 0 takes the guest's commands and answers them at once; queue 1 holds the frame
//! buffers the guest posted: each frame is copied once, from the helper's ring into the guest's buffer, on the
//! bridge's thread. Device tree: /arm-io/camera, compatible "oneios,camera", one window and SPI, as the bridge
//! channel.
use crate::link::{Camera, Event};
use crate::pod::Pod;
use crate::proto::*;
use crate::ring::ReadError;
use std::cell::RefCell;
use std::sync::{Arc, Weak};
use std::time::Duration;
use xh_core::virtio::{Device, Info, Viov, Virtio};

#[derive(Default)]
struct State {
    /// of the stream the guest asked for
    role: u32,
    fps: u32,
    started: bool,
    /// handed to the guest
    frames: u64,
    /// made while the guest had no buffer posted
    dropped: u64,
}

/// The device model (`cam_state`).
pub struct CameraDevice {
    camera: Arc<Camera>,
    state: RefCell<State>,
}

pub type CameraVirtio = Virtio<CameraDevice>;

impl CameraDevice {
    /// Completes one posted frame buffer: with the newest frame, or (status != 0) with only a header.
    fn deliver(&self, dev: &CameraVirtio, status: i32) {
        let mut peek = OcFrame::default();
        if status == 0 && self.camera.frame(&mut peek, &mut []) == Err(ReadError::Again) {
            return; // nothing new: keep the guest's buffer
        }
        let mut iov = [Viov::EMPTY; 4];
        let Some((head, n)) = dev.pop(OC_QUEUE_FRAME, &mut iov) else {
            if status == 0 {
                self.state.borrow_mut().dropped += 1;
            }
            return;
        };
        let (role, fps) = {
            let s = self.state.borrow();
            (s.role, s.fps)
        };
        let empty = OcFrame { magic: OC_FRAME_MAGIC, role, fps, ..Default::default() };
        let mut frame = OcFrame { status: status as u32, ..empty };
        let mut written = 0;
        let part = &iov[0];
        if n > 0 && part.writable() && part.len() >= OC_FRAME_DATA {
            if status == 0 {
                // SAFETY: a writable part of a chain this device holds: the guest keeps off it until it is pushed.
                let picture = unsafe { &mut part.as_mut_slice()[OC_FRAME_DATA as usize..] };
                match self.camera.frame(&mut frame, picture) {
                    // Written again during the copy, four times over. The transport has no way to put a chain back:
                    // the guest gets it empty and posts it again.
                    Err(ReadError::Again) => frame = empty,
                    Err(e) => frame = OcFrame { status: e.errno() as u32, ..empty },
                    Ok(()) => self.state.borrow_mut().frames += 1,
                }
            }
            part.write_at(0, frame.as_bytes());
            written = OC_FRAME_DATA + frame.bytes;
        }
        dev.push(OC_QUEUE_FRAME, head, written);
    }

    /// The bridge's events, on its thread.
    fn on_event(&self, dev: &CameraVirtio, event: Event) {
        let started = self.state.borrow().started;
        match event {
            Event::Frame if started => self.deliver(dev, 0),
            Event::State(status) if status != 0 => {
                // the stream ended by itself
                if started {
                    self.deliver(dev, status);
                }
                self.state.borrow_mut().started = false;
            }
            _ => {}
        }
    }

    fn command(&self, request: &OcRequest, size: usize, room: usize) -> (OcReply, usize) {
        let mut reply = OcReply::default();
        let mut written = 8;
        match request.code {
            OC_CMD_INFO => {
                // The helper lists the cameras when it starts; the guest's first question may come before it has.
                let mut list = self.camera.list(OC_MAX_CAMERAS as usize, Duration::ZERO);
                while !list.is_empty() && 8 + list.len() * std::mem::size_of::<OcCamera>() > room {
                    list.pop();
                }
                reply.count = list.len() as u32;
                reply.cameras[..list.len()].copy_from_slice(&list);
                written = 8 + list.len() * std::mem::size_of::<OcCamera>();
            }
            OC_CMD_START => {
                if size < std::mem::size_of::<OcRequest>()
                    || request.width > OC_MAX_WIDTH
                    || request.height > OC_MAX_HEIGHT
                {
                    reply.status = libc::EINVAL as u32;
                } else {
                    let fps = if request.fps != 0 { request.fps } else { 30 };
                    let rc = self.camera.start(request.role, request.width, request.height, fps);
                    reply.status = rc.err().unwrap_or(0) as u32;
                    let mut s = self.state.borrow_mut();
                    (s.role, s.fps, s.started) = (request.role, fps, reply.status == 0);
                }
            }
            OC_CMD_STOP => {
                self.state.borrow_mut().started = false;
                reply.status = self.camera.stop().err().unwrap_or(0) as u32;
            }
            _ => reply.status = libc::ENOTSUP as u32,
        }
        (reply, written)
    }

    /// Frames handed to the guest, and frames made while it had no buffer posted.
    pub fn counts(&self) -> (u64, u64) {
        let s = self.state.borrow();
        (s.frames, s.dropped)
    }
}

impl Device for CameraDevice {
    fn handle(&self, _dev: &CameraVirtio, queue: u32, _head: u16, iov: &[Viov]) -> Option<u32> {
        if queue != OC_QUEUE_CONTROL {
            return Some(0);
        }
        let mut request = [0u8; std::mem::size_of::<OcRequest>()];
        let mut size = 0;
        let mut out: Option<&Viov> = None;
        for part in iov {
            if part.writable() {
                out = out.or(Some(part));
                continue;
            }
            size += part.read_at(0, &mut request[size..]);
        }
        let out = match out {
            Some(out) if out.len() >= 8 && size >= 4 => out,
            _ => return Some(0),
        };
        let (reply, written) = self.command(&OcRequest::read_from(&request), size, out.len() as usize);
        out.write_at(0, &reply.as_bytes()[..written]);
        Some(written as u32)
    }

    fn config_read(&self, _dev: &CameraVirtio, offset: u32, _size: u32) -> u32 {
        if offset == 0 {
            OC_PROTO_VERSION
        } else {
            0
        }
    }

    fn reset(&self, _dev: &CameraVirtio) {
        let mut s = self.state.borrow_mut();
        if s.started {
            let _ = self.camera.stop(); // a guest that restarts leaves no camera open
        }
        s.started = false;
    }
}

/// The device on xnuhost's transport (`br_camera_create`), at `base` with interrupt `spi`, fed by `camera`. None
/// without a camera bridge, or when its window cannot be had.
pub fn create(base: u64, spi: u32, camera: Option<Arc<Camera>>) -> Option<Arc<CameraVirtio>> {
    let camera = camera?;
    let info = Info { device_id: OC_VIRTIO_DEVICE_ID, features: 0, nqueues: 2, manual: 1 << OC_QUEUE_FRAME };
    let device = CameraDevice { camera: Arc::clone(&camera), state: RefCell::new(State::default()) };
    let dev = Virtio::create(base, spi, info, device, "camera").ok()?;
    // Weak: the bridge's listener must not keep the device alive (the device holds the bridge).
    let weak: Weak<CameraVirtio> = Arc::downgrade(&dev);
    camera.listen(Some(Arc::new(move |event| {
        if let Some(dev) = weak.upgrade() {
            let guard = dev.lock();
            guard.on_event(&dev, event);
        }
    })));
    Some(dev)
}
