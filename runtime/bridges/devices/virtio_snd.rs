// SPDX-License-Identifier: MIT
//! virtio-snd (VirtIO 1.2, 5.14) for the guest's speaker and microphone (C: devices/virtio_snd.c), as UniXNU-droidvm's
//! DroidVMVirtioSnd.c drives it: stream 0 is the output (48 kHz S16 stereo), stream 1 the input (48 kHz S16 mono),
//! periods of 240 frames. Queues: 0 control, 1 event (unused), 2 transmit, 3 receive. Output periods go to the backend
//! as they arrive (the guest's audio engine paces them); a capture thread fills the receive buffers the guest has
//! posted with what the backend recorded.

use super::virtio_net::config_word;
use crate::audio::AudioBackend;
use std::cell::{Cell, RefCell};
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
use std::sync::{Arc, Weak};
use std::time::Duration;
use xh_core::virtio::{self, Device, Info, Viov, Virtio};

const Q_CONTROL: u32 = 0;
const Q_EVENT: u32 = 1;
const Q_TX: u32 = 2;
const Q_RX: u32 = 3;
const R_PCM_INFO: u32 = 0x0100;
const R_PCM_SET_PARAMS: u32 = 0x0101;
const R_PCM_PREPARE: u32 = 0x0102;
const R_PCM_RELEASE: u32 = 0x0103;
const R_PCM_START: u32 = 0x0104;
const R_PCM_STOP: u32 = 0x0105;
const S_OK: u32 = 0x8000;
const S_BAD_MSG: u32 = 0x8001;
const S_NOT_SUPP: u32 = 0x8002;
const S_IO_ERR: u32 = 0x8003;
const FMT_S16: u8 = 5;
const RATE_48000: u8 = 7;
const D_OUTPUT: usize = 0;
const D_INPUT: usize = 1;
const PERIOD_FRAMES: usize = 240;
/// struct virtio_snd_pcm_info: hda_fn_nid, features (u32), formats, rates (u64), direction, channels_min/max, padding[5].
const PCM_INFO: u32 = 32;
/// struct virtio_snd_pcm_set_params: code, stream_id, buffer_bytes, period_bytes, features, channels, format, rate.
const SET_PARAMS: usize = 24;

/// The device's state.
pub struct SndDevice {
    backend: Arc<dyn AudioBackend>,
    running: [AtomicBool; 2],
    channels: [Cell<u8>; 2],
    /// Counts the capture threads: one sees a newer number and ends (a stop does not wait for it: the transport's
    /// lock, which the stop holds, is what it may be waiting on).
    capture_gen: Arc<AtomicU32>,
    capture: RefCell<Option<std::thread::JoinHandle<()>>>,
    me: RefCell<Weak<Virtio<SndDevice>>>,
    pub tx_periods: AtomicU64,
    pub rx_periods: AtomicU64,
    pub rx_dropped: AtomicU64,
}

fn u32_at(b: &[u8], at: usize) -> u32 {
    u32::from_le_bytes(b[at..at + 4].try_into().expect("4 bytes"))
}

/// The capture thread: a period from the backend into each receive buffer the guest posted.
fn capture(weak: Weak<Virtio<SndDevice>>, backend: Arc<dyn AudioBackend>, gen: Arc<AtomicU32>, mine: u32) {
    let mut pcm = [0i16; PERIOD_FRAMES];
    loop {
        // The backend blocks until a period is recorded (or returns less when it is stopped).
        let got = backend.read(&mut pcm);
        let Some(dev) = weak.upgrade() else { return };
        let g = dev.lock();
        if gen.load(Ordering::Acquire) != mine || !g.running[D_INPUT].load(Ordering::Acquire) {
            return;
        }
        if got <= 0 {
            drop(g);
            drop(dev);
            std::thread::sleep(Duration::from_millis(5));
            continue;
        }
        pcm[got as usize..].fill(0);
        let mut iov = [Viov::EMPTY; 4];
        let Some((head, n)) = dev.pop(Q_RX, &mut iov) else {
            g.rx_dropped.fetch_add(1, Ordering::Relaxed);
            continue;
        };
        // header (readable), data (writable), status (writable, 8 bytes: status, latency)
        let channels = usize::from(g.channels[D_INPUT].get().max(1));
        let mut written = 0u32;
        let parts = &iov[..n];
        for (i, part) in parts.iter().enumerate().filter(|(_, p)| p.writable()) {
            if i + 1 < n {
                // the data part
                let frames = (part.len() as usize / (2 * channels)).min(PERIOD_FRAMES);
                let mut out = Vec::with_capacity(frames * channels * 2);
                for &s in &pcm[..frames] {
                    for _ in 0..channels {
                        out.extend_from_slice(&s.to_le_bytes());
                    }
                }
                part.write_at(0, &out);
                written += out.len() as u32;
            } else if part.len() >= 8 {
                let mut status = [0u8; 8];
                status[..4].copy_from_slice(&S_OK.to_le_bytes());
                part.write_at(0, &status);
                written += 8;
            }
        }
        g.rx_periods.fetch_add(1, Ordering::Relaxed);
        dev.push(Q_RX, head, written);
    }
}

impl SndDevice {
    /// Gives back the receive buffers the guest posted (a stopped stream returns what it holds).
    fn return_rx(dev: &Virtio<SndDevice>) {
        let mut iov = [Viov::EMPTY; 4];
        while let Some((head, _)) = dev.pop(Q_RX, &mut iov) {
            dev.push(Q_RX, head, 0);
        }
    }

    fn pcm_command(&self, dev: &Virtio<SndDevice>, code: u32, stream: u32) -> u32 {
        if stream > 1 {
            return S_BAD_MSG;
        }
        let output = stream == 0;
        let d = if output { D_OUTPUT } else { D_INPUT };
        match code {
            R_PCM_PREPARE | R_PCM_RELEASE => S_OK,
            R_PCM_START => {
                if self.running[d].load(Ordering::Acquire) {
                    return S_OK;
                }
                let channels = match self.channels[d].get() {
                    0 if output => 2,
                    0 => 1,
                    n => n,
                };
                if self.backend.start(output, 48000, channels.into()).is_err() {
                    return S_IO_ERR;
                }
                self.running[d].store(true, Ordering::Release);
                if !output && self.capture.borrow().is_none() {
                    let mine = self.capture_gen.fetch_add(1, Ordering::AcqRel) + 1;
                    let (weak, backend, gen) =
                        (self.me.borrow().clone(), Arc::clone(&self.backend), Arc::clone(&self.capture_gen));
                    let thread = xh_core::thread::spawn("snd-capture", move || capture(weak, backend, gen, mine));
                    *self.capture.borrow_mut() = thread.ok();
                }
                S_OK
            }
            R_PCM_STOP => {
                if !self.running[d].load(Ordering::Acquire) {
                    return S_OK;
                }
                self.running[d].store(false, Ordering::Release);
                let _ = self.backend.stop(output);
                if !output {
                    // The capture thread ends at its next period (it takes the device's lock, which this holds).
                    self.capture_gen.fetch_add(1, Ordering::AcqRel);
                    self.capture.borrow_mut().take();
                    SndDevice::return_rx(dev);
                }
                S_OK
            }
            _ => S_NOT_SUPP,
        }
    }

    fn control(&self, dev: &Virtio<SndDevice>, iov: &[Viov]) -> Option<u32> {
        let mut request = [0u8; 64];
        let size = virtio::read_chain(iov, 0, &mut request);
        let reply = iov.iter().find(|p| p.writable())?;
        if reply.len() < 4 || size < 4 {
            return Some(0);
        }
        let code = u32_at(&request, 0);
        let mut status = S_NOT_SUPP;
        let mut written = 4u32;
        if code == R_PCM_INFO && size >= 16 {
            let (first, count, each) = (u32_at(&request, 4), u32_at(&request, 8), u32_at(&request, 12));
            if u64::from(first) + u64::from(count) > 2
                || each < PCM_INFO
                || u64::from(reply.len()) < 4 + u64::from(count) * u64::from(each)
            {
                status = S_BAD_MSG;
            } else {
                reply.fill(0, 0, (4 + count * each) as usize);
                for i in 0..count {
                    let output = first + i == 0;
                    let mut info = [0u8; PCM_INFO as usize];
                    info[8..16].copy_from_slice(&(1u64 << FMT_S16).to_le_bytes());
                    info[16..24].copy_from_slice(&(1u64 << RATE_48000).to_le_bytes());
                    info[24] = if output { D_OUTPUT as u8 } else { D_INPUT as u8 };
                    info[25] = if output { 2 } else { 1 };
                    info[26] = info[25];
                    reply.write_at((4 + i * each) as usize, &info);
                }
                written = 4 + count * each;
                status = S_OK;
            }
        } else if code == R_PCM_SET_PARAMS && size >= SET_PARAMS {
            let stream = u32_at(&request, 4);
            let (channels, format, rate) = (request[20], request[21], request[22]);
            let output = stream == 0;
            status = if stream > 1 || format != FMT_S16 || rate != RATE_48000 || channels != if output { 2 } else { 1 }
            {
                S_NOT_SUPP
            } else {
                self.channels[if output { D_OUTPUT } else { D_INPUT }].set(channels);
                S_OK
            };
        } else if (R_PCM_PREPARE..=R_PCM_STOP).contains(&code) && size >= 8 {
            status = self.pcm_command(dev, code, u32_at(&request, 4));
        }
        reply.write_at(0, &status.to_le_bytes());
        Some(written)
    }

    fn transmit(&self, iov: &[Viov]) -> Option<u32> {
        // header { stream_id } then PCM (readable), then the status (writable).
        let mut stream = u32::MAX;
        let mut skip = 4usize;
        for part in iov {
            if part.writable() {
                if part.len() >= 8 {
                    let mut status = [0u8; 8];
                    status[..4].copy_from_slice(&S_OK.to_le_bytes());
                    part.write_at(0, &status);
                    return Some(8);
                }
                continue;
            }
            let mut data = vec![0u8; part.len() as usize];
            part.read_at(0, &mut data);
            let mut data = &data[..];
            if skip != 0 {
                if data.len() < skip {
                    return Some(0);
                }
                stream = u32_at(data, 0);
                data = &data[skip..];
                skip = 0;
            }
            if stream == 0 && self.running[D_OUTPUT].load(Ordering::Acquire) && data.len() >= 4 {
                let pcm: Vec<i16> = crate::audio::proto::samples(&data[..data.len() / 4 * 4]).collect();
                let _ = self.backend.write(&pcm);
                self.tx_periods.fetch_add(1, Ordering::Relaxed);
            }
        }
        Some(0)
    }
}

impl Device for SndDevice {
    fn config_read(&self, _dev: &Virtio<Self>, offset: u32, size: u32) -> u32 {
        let config: Vec<u8> = [0u32, 2, 0].iter().flat_map(|w| w.to_le_bytes()).collect(); // jacks, streams, chmaps
        config_word(&config, offset, size)
    }

    fn handle(&self, dev: &Virtio<Self>, queue: u32, _head: u16, iov: &[Viov]) -> Option<u32> {
        match queue {
            Q_CONTROL => self.control(dev, iov),
            Q_TX => self.transmit(iov),
            _ => Some(0),
        }
    }

    fn reset(&self, dev: &Virtio<Self>) {
        let _ = self.pcm_command(dev, R_PCM_STOP, 0);
        let _ = self.pcm_command(dev, R_PCM_STOP, 1);
    }
}

/// "droidvm,virtio-snd": one output stream (48 kHz S16 stereo) and one input stream (48 kHz S16 mono), 240-frame
/// periods, as DroidVMVirtioSnd.c asks. None when the device cannot be made.
pub fn create(base: u64, spi: u32, backend: Arc<dyn AudioBackend>) -> Option<Arc<Virtio<SndDevice>>> {
    let info = Info { device_id: 25, nqueues: 4, manual: 1 << Q_EVENT | 1 << Q_RX, ..Info::default() };
    let device = SndDevice {
        backend,
        running: Default::default(),
        channels: Default::default(),
        capture_gen: Arc::new(AtomicU32::new(0)),
        capture: RefCell::new(None),
        me: RefCell::new(Weak::new()),
        tx_periods: AtomicU64::new(0),
        rx_periods: AtomicU64::new(0),
        rx_dropped: AtomicU64::new(0),
    };
    let dev = Virtio::create(base, spi, info, device, "virtio-snd").ok()?;
    *dev.lock().me.borrow_mut() = Arc::downgrade(&dev);
    Some(dev)
}
